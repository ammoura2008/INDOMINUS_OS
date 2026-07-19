//! # System Call Interface
//!
//! Implements the `syscall`/`sysret` mechanism for user → kernel transitions.
//!
//! ## How `syscall` works
//!
//! 1. User code loads syscall number into RAX, arguments into RDI/RSI/RDX/R8/R9
//! 2. User code executes `syscall`
//! 3. CPU saves RIP → RCX, RFLAGS → R11
//! 4. CPU loads CS from STAR (kernel code), RIP from LSTAR (entry point)
//! 5. CPU clears RFLAGS bits per SFMASK (disables interrupts)
//! 6. CPU does NOT switch stacks — RSP still points to user stack
//!
//! ## Our approach
//!
//! We use `swapgs` to switch to a kernel GSBase that points to a per-CPU
//! structure containing the current process's kernel stack pointer. The
//! syscall handler reads this and switches RSP before saving user context.
//!
//! ## MSR layout
//!
//! ```text
//! STAR  = (kernel_ss << 48) | (kernel_cs << 32) | (user_cs << 16) | user_ss
//! LSTAR = address of syscall_entry
//! SFMASK = 0x200 (clear IF bit to disable interrupts during syscall)
//! ```

use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Msr};
use x86_64::VirtAddr;

// ─────────────────────────────────────────────────────────────────────────────
// User address validation
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum valid user-space virtual address (lower half, canonical).
/// x86-64 canonical lower half: 0x0000_0000_0000_0000 .. 0x0000_7FFF_FFFF_FFFF
const USER_ADDR_MAX: u64 = 0x0000_7FFF_FFFF_FFFF;

/// Check if a user-space address range is valid.
///
/// Returns `true` if:
/// - `addr` is in user space (below USER_ADDR_MAX)
/// - `addr + len` does not overflow
/// - `addr + len` is still in user space
///
/// This prevents user processes from tricking the kernel into
/// reading/writing kernel memory via syscall arguments.
fn is_valid_user_range(addr: u64, len: u64) -> bool {
    if addr == 0 || len == 0 {
        return false;
    }
    let end = addr.wrapping_add(len);
    end > addr && end <= USER_ADDR_MAX
}

/// Per-CPU data structure pointed to by GSBase.
///
/// Layout matches the naked handler's `gs:[offset]` accesses:
/// - offset 0:  user_rsp   (saved on syscall entry)
/// - offset 8:  kernel_rsp (top of kernel stack)
/// - offset 16: force_switch (1 = context switch after syscall, 0 = normal sysret)
#[repr(C)]
pub struct PerCpuData {
    /// User RSP saved on syscall entry (written by the naked handler).
    pub user_rsp: u64,
    /// Top of the current process's kernel stack (written during context switch).
    pub kernel_rsp: u64,
    /// Force context switch flag. Set by sys_exit/sys_yield. Checked by naked handler.
    pub force_switch: u64,
}

/// Static per-CPU data for the boot CPU.
///
/// # Safety
/// Accessed only from the syscall entry handler (single-CPU system).
///
/// # IMPORTANT: Identity map dependency
/// The GS base is set to the physical address of this static (via `&raw const PER_CPU`).
/// This works ONLY because the identity map (phys == virt for first 4 GiB) is active.
/// Before removing the identity map in Phase 5.4, GS base MUST be changed to the
/// higher-half virtual address: `phys_to_kernel_virt(&raw const PER_CPU as u64)`.
static mut PER_CPU: PerCpuData = PerCpuData { user_rsp: 0, kernel_rsp: 0, force_switch: 0 };

/// Update the kernel stack pointer in the per-CPU data.
///
/// Called during context switch so the next syscall uses the correct kernel stack.
///
/// # Safety
/// Must be called with interrupts disabled (from the timer handler or
/// with interrupts globally disabled).
pub unsafe fn set_kernel_rsp(rsp: u64) {
    PER_CPU.kernel_rsp = rsp;
}

/// Get the current kernel stack pointer from per-CPU data.
///
/// # Safety
/// Must be called from the syscall entry handler with interrupts disabled.
pub unsafe fn get_kernel_rsp() -> u64 {
    PER_CPU.kernel_rsp
}

/// Set the force_switch flag in per-CPU data.
///
/// Called by sys_exit and sys_yield to request a context switch after the
/// syscall dispatch returns. The naked handler checks this flag and branches
/// to the context switch path instead of doing `sysretq`.
///
/// # Safety
/// Must be called with interrupts disabled (during syscall dispatch).
pub unsafe fn set_force_switch() {
    PER_CPU.force_switch = 1;
}

/// Clear the force_switch flag in per-CPU data.
///
/// # Safety
/// Must be called with interrupts disabled (from the naked handler).
pub unsafe fn clear_force_switch() {
    PER_CPU.force_switch = 0;
}

// ─────────────────────────────────────────────────────────────────────────────
// MSR setup
// ─────────────────────────────────────────────────────────────────────────────

/// Initialize the `syscall`/`sysret` MSRs.
///
/// Sets up:
/// - STAR: segment selectors for kernel/user mode
/// - LSTAR: syscall entry point address
/// - SFMASK: clears IF during syscall (disables interrupts)
/// - EFER: enables the `syscall`/`sysret` feature (SCE bit)
pub fn init() {
    crate::serial::write_str("[SYSCALL] Setting up MSRs...\n");

    // ── STAR MSR ─────────────────────────────────────────────────────────
    // GDT layout:
    //   Index 1: Kernel code (0x08)
    //   Index 2: Kernel data (0x10)
    //   Index 3: User code   (0x18)
    //   Index 4: User data   (0x20)
    //
    // STAR format:
    //   Bits 0-15:   User SS (for sysret)  = 0x20 | 3 = 0x23
    //   Bits 16-31:  User CS (for sysret)  = 0x18 | 3 = 0x1B
    //   Bits 32-47:  Kernel CS (for syscall) = 0x08
    //   Bits 48-63:  Kernel SS (for syscall) = 0x10
    unsafe {
        let star_val: u64 = (0x10u64 << 48) | (0x08u64 << 32) | (0x1Bu64 << 16) | 0x23u64;
        Msr::new(0xC000_0081).write(star_val);
    }

    // ── LSTAR MSR ────────────────────────────────────────────────────────
    // The CPU jumps here on `syscall`.
    // With PIC, fn pointers contain physical addresses after relocation — convert to virtual.
    unsafe {
        let entry_phys = syscall_entry as *const () as u64;
        let entry_virt = crate::memory::phys_to_kernel_virt(entry_phys);
        crate::serial::write_str("[SYSCALL] LSTAR entry phys=");
        crate::serial::write_hex(entry_phys);
        crate::serial::write_str(" virt=");
        crate::serial::write_hex(entry_virt);
        crate::serial::write_nl();
        LStar::write(VirtAddr::new(entry_virt));
    }

    // ── SFMASK MSR ───────────────────────────────────────────────────────
    // Bits set here will be CLEARED in RFLAGS when `syscall` executes.
    // Bit 9 = IF (Interrupt Flag). Clearing it disables interrupts.
    unsafe {
        SFMask::write(x86_64::registers::rflags::RFlags::INTERRUPT_FLAG);
    }

    // ── Enable SCE in EFER ───────────────────────────────────────────────
    // The SCE (System Call Extensions) bit enables the `syscall`/`sysret`
    // instructions.
    unsafe {
        let mut efer = Efer::read();
        efer |= EferFlags::SYSTEM_CALL_EXTENSIONS;
        Efer::write(efer);
    }

    // Set KERNEL_GS_BASE to point to our per-CPU data using the kernel virtual address.
    // The syscall_entry handler does `swapgs` which swaps GS_BASE and KERNEL_GS_BASE.
    // After swapgs, the kernel uses KERNEL_GS_BASE for GS-relative accesses.
    // So KERNEL_GS_BASE must point to PER_CPU, while GS_BASE (used in user mode) can be 0.
    // The user PML4 does NOT have the identity map, so we must use the higher-half
    // virtual address which IS mapped in all PML4s.
    unsafe {
        let gs_phys = &raw const PER_CPU as u64;
        let gs_virt = crate::memory::phys_to_kernel_virt(gs_phys);
        // KERNEL_GS_BASE (MSR 0xC0000102) — used in kernel mode after swapgs
        Msr::new(0xC000_0102).write(gs_virt);
        // GS_BASE (MSR 0xC0000101) — not used, but clear it for sanity
        Msr::new(0xC000_0101).write(0u64);
    }

    crate::serial::write_str("[SYSCALL] MSRs configured\n");
}

use x86_64::structures::gdt::SegmentSelector;

// ─────────────────────────────────────────────────────────────────────────────
// Syscall entry handler
// ─────────────────────────────────────────────────────────────────────────────

/// Naked syscall entry point (called via LSTAR on `syscall` instruction).
///
/// When this handler starts:
/// - RSP = user stack (we must switch to kernel stack)
/// - RCX = user RIP (saved by CPU)
/// - R11 = user RFLAGS (saved by CPU)
/// - RAX = syscall number
/// - RDI, RSI, RDX, R8, R9 = arguments (Linux convention)
///
/// Flow:
/// 1. `swapgs` → switch to kernel GSBase (per-CPU data)
/// 2. Load kernel RSP from per-CPU data
/// 3. Save all user registers on kernel stack
/// 4. Call Rust dispatch function
/// 5. Check force_switch flag (gs:[16])
/// 6a. If clear: restore registers, `swapgs`, `sysretq` (normal return)
/// 6b. If set: construct IRET frame, call schedule(), context switch via `iretq`
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // ═══════════════════════════════════════════════════════════════════
        // DIAGNOSTIC: dump RAX at syscall entry (before any register changes)
        // ═══════════════════════════════════════════════════════════════════
        // Save all caller-saved registers so the diagnostic call doesn't
        // corrupt anything the normal flow needs.
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "mov rdi, 0x53",            // 'S' marker
        "mov rsi, [rsp + 64]",      // RAX is at [rsp+64] (first pushed)
        "call {dump_rax}",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 1: Switch to kernel stack and save user context
        // ═══════════════════════════════════════════════════════════════════
        "swapgs",                                // Switch to kernel GSBase
        "mov gs:[0], rsp",                       // Save user RSP to per-CPU
        "mov rsp, gs:[8]",                       // Load kernel RSP from per-CPU

        // Save user context on kernel stack (15 GP regs)
        // Push R15 first (highest addr) → RAX last (lowest addr = RSP).
        // Canonical SyscallFrame layout:
        //   [rsp+0]   = RAX  (syscall number / return value)
        //   [rsp+8]   = RBX
        //   [rsp+16]  = RCX  (user RIP, saved by CPU)
        //   [rsp+24]  = RDX
        //   [rsp+32]  = RSI  (arg1)
        //   [rsp+40]  = RDI  (arg0)
        //   [rsp+48]  = RBP
        //   [rsp+56]  = R8   (arg4)
        //   [rsp+64]  = R9   (arg5)
        //   [rsp+72]  = R10  (arg3)
        //   [rsp+80]  = R11  (user RFLAGS, saved by CPU)
        //   [rsp+88]  = R12
        //   [rsp+96]  = R13
        //   [rsp+104] = R14
        //   [rsp+112] = R15
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push r11",
        "push r10",
        "push r9",
        "push r8",
        "push rbp",
        "push rdi",
        "push rsi",
        "push rdx",
        "push rcx",
        "push rbx",
        "push rax",

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 2: Call Rust dispatch function
        // ═══════════════════════════════════════════════════════════════════
        "mov rdi, rsp",  // arg0 = pointer to saved register frame
        "call {dispatch}",
        // RAX = syscall return value (stored in frame[0] by dispatch)

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 3: Check force_switch flag (gs:[16])
        // ═══════════════════════════════════════════════════════════════════
        "cmp qword ptr gs:[16], 0",
        "je .normal_return",

        // ── force_switch path: context switch ─────────────────────────────
        // sys_exit or sys_yield requested a context switch.
        // We need to:
        // 1. Construct an IRET frame so the timer handler can restore us later
        // 2. Call schedule() to switch to the next process
        // 3. Load the new process's stack and iretq

        "mov qword ptr gs:[16], 0",             // Clear force_switch

        // Read user state from saved GP register frame
        // New canonical layout: [rsp+16]=RCX (user RIP), [rsp+80]=R11 (user RFLAGS)
        "mov rax, [rsp + 16]",                  // RCX = user RIP
        "mov rbx, [rsp + 80]",                  // R11 = user RFLAGS
        "mov rcx, gs:[0]",                      // user RSP (saved at syscall entry)

        // Construct 5-qword IRET frame above the GP regs
        // iretq pops: RIP, CS, RFLAGS, and if CS.RPL > CPL: RSP, SS
        // Push in reverse order: SS, RSP, RFLAGS, CS, RIP
        "push 0x23",                            // SS  = user data selector (Ring 3)
        "push rcx",                             // RSP = user RSP
        "push rbx",                             // RFLAGS = user RFLAGS
        "push 0x1B",                            // CS  = user code selector (Ring 3)
        "push rax",                             // RIP = user RIP

        // Adjust RSP back to point at the GP regs (skip over 5 IRET qwords)
        // schedule() needs RSP → GP regs so the timer handler can later
        // pop the GP regs and iretq will find the IRET frame immediately after.
        "sub rsp, 40",                          // 5 * 8 = 40 bytes

        // Call schedule_force(GP_regs_ptr) → always switches, returns new SP in RAX
        "mov rdi, rsp",
        "call {schedule_force}",
        // RAX = new process's saved RSP

        // ── Checkpoint P: first instruction after schedule_force returns ──
        "mov r12, rax",
        "push rax",
        "push rdi",
        "mov dil, 0x50",
        "call {ddbg}",
        "pop rdi",
        "pop rax",

        // Send EOI to LAPIC (upper-half virtual address)
        "mov rax, 0xFFFFFFFFFEE000B0",
        "mov dword ptr [rax], 0",

        // Switch to new process's stack
        "mov rsp, r12",

        // Restore new process's GP registers (canonical order: RAX first, R15 last)
        "pop rax",
        "pop rbx",
        "pop rcx",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop rbp",
        "pop r8",
        "pop r9",
        "pop r10",
        "pop r11",
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",

        // ── Checkpoint I: about to iretq ────────────────────────────────
        "push rax",
        "push rdi",
        "mov dil, 0x49",
        "call {ddbg}",
        "pop rdi",
        "pop rax",

        // Restore user GS before returning to Ring 3.
        // The syscall_entry swapgs'd at entry (GS_BASE ↔ KERNEL_GS_BASE).
        // We must swap back so Ring 3 sees GS_BASE=0 (user value).
        "swapgs",

        // Return from interrupt (pops IRET frame: RIP, CS, RFLAGS, [RSP, SS])
        "iretq",

        // ── Normal return path: sysretq ───────────────────────────────────
        ".normal_return:",
        "pop rax",
        "pop rbx",
        "pop rcx",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop rbp",
        "pop r8",
        "pop r9",
        "pop r10",
        "pop r11",
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",

        "swapgs",                                // Switch back to user GSBase
        "sysretq",                               // Return to Ring 3

        dispatch = sym syscall_dispatch,
        schedule_force = sym crate::process::context_switch::schedule_force,
        dump_rax = sym crate::serial::dump_rax,
        ddbg = sym crate::serial::ddbg,
    );
}

/// Rust-side syscall dispatch function.
///
/// Called from the naked `syscall_entry` handler with RSP pointing to the
/// saved register frame on the kernel stack.
///
/// # Arguments
/// * `regs` — pointer to the saved register frame
///
/// # Returns
/// Syscall return value (placed in RAX for `sysret`).
#[no_mangle]
pub unsafe extern "C" fn syscall_dispatch(regs: *mut u64) -> u64 {
    // Canonical SyscallFrame layout (15 qwords, pushed R15→RAX):
    //   [0]  RAX  = syscall number (also return value)
    //   [1]  RBX
    //   [2]  RCX  = user RIP (saved by CPU)
    //   [3]  RDX  = arg2
    //   [4]  RSI  = arg1
    //   [5]  RDI  = arg0
    //   [6]  RBP
    //   [7]  R8   = arg4
    //   [8]  R9   = arg5
    //   [9]  R10  = arg3
    //   [10] R11  = user RFLAGS (saved by CPU)
    //   [11] R12
    //   [12] R13
    //   [13] R14
    //   [14] R15

    let frame = regs as *mut u64;
    let syscall_num = *frame.add(0);
    let arg0 = *frame.add(5);  // RDI
    let arg1 = *frame.add(4);  // RSI
    let arg2 = *frame.add(3);  // RDX
    let _arg3 = *frame.add(9); // R10

    let result = match syscall_num {
        0 => sys_write(arg0, arg1, arg2),
        1 => sys_exit(arg0),
        2 => sys_yield(),
        3 => sys_getpid(),
        _ => {
            crate::serial::write_str("[SYSCALL] Unknown syscall: ");
            crate::serial::write_u64(syscall_num);
            crate::serial::write_nl();
            u64::MAX // -ENOSYS
        }
    };

    // Store return value in RAX slot
    *frame.add(0) = result;

    result
}

// ─────────────────────────────────────────────────────────────────────────────
// System call implementations
// ─────────────────────────────────────────────────────────────────────────────

/// SYS_WRITE (0) — Write data to a file descriptor.
///
/// For now, only fd=1 (serial/stdout) is supported.
///
/// Arguments: fd, buf_ptr, count
/// Returns: number of bytes written, or u64::MAX on error (-EINVAL)
fn sys_write(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    if fd != 1 {
        return u64::MAX; // Only stdout supported
    }

    // Validate the user buffer address range
    if !is_valid_user_range(buf_ptr, count) {
        crate::serial::write_str("[SYSCALL] sys_write: invalid user buffer addr=");
        crate::serial::write_hex(buf_ptr);
        crate::serial::write_str(" len=");
        crate::serial::write_hex(count);
        crate::serial::write_nl();
        return u64::MAX; // -EINVAL
    }

    let slice = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, count as usize) };

    for &byte in slice {
        crate::serial::write_byte(byte);
    }

    count
}

/// SYS_EXIT (1) — Exit the current process.
///
/// Marks the process as Zombie and requests a context switch.
/// The naked handler will switch to the next process after we return.
///
/// Arguments: exit_code
/// Returns: never (naked handler context-switches before returning to user)
fn sys_exit(exit_code: u64) -> u64 {
    crate::serial::write_str("[SYSCALL] exit(");
    crate::serial::write_u64(exit_code);
    crate::serial::write_str(")\n");

    // Mark current process as Zombie
    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                proc.state = crate::process::ProcessState::Zombie;
                proc.exit_code = exit_code;
            }
        }
    }

    // Request context switch — the naked handler will call schedule() and
    // switch to the next process instead of doing sysretq back to user mode.
    unsafe { set_force_switch(); }

    0 // Return value (ignored — naked handler switches before sysret)
}

/// SYS_YIELD (2) — Yield the CPU to the next process.
///
/// Requests a context switch. The naked handler will call schedule() and
/// switch to the next ready process, then resume this process when it's
/// picked again.
///
/// Returns: always 0
fn sys_yield() -> u64 {
    unsafe { set_force_switch(); }
    0
}

/// SYS_GETPID (3) — Get the current process ID.
///
/// Returns: current process PID
fn sys_getpid() -> u64 {
    let sched = crate::process::scheduler::SCHEDULER.lock();
    sched.current_pid().unwrap_or(0)
}
