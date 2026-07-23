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

pub mod errno;

use alloc::vec::Vec;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Msr};
use x86_64::VirtAddr;
use x86_64::structures::paging::FrameAllocator;

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

/// Check if every page in a user-space buffer range is present and user-accessible.
///
/// Walks the process page tables (PML4 → PDPT → PD → PT) for each 4 KiB page
/// in [addr, addr+len). Returns `true` only if ALL pages are present and have
/// the USER_ACCESSIBLE bit set.
///
/// Temporarily switches CR3 to the kernel PML4 (which has the identity map) so
/// we can access arbitrary physical page table frames via `phys_to_virt`.
/// User PML4s don't have the identity map, and `phys_to_kernel_virt` only works
/// for the kernel's own physical memory — not for PMM-allocated page tables.
fn is_user_buffer_mapped(pml4_phys: u64, addr: u64, len: u64) -> bool {
    use x86_64::structures::paging::{PageTable, PageTableIndex, PageTableFlags};

    if len == 0 || addr == 0 {
        return false;
    }

    let page_size = 4096u64;
    let start_page = addr / page_size;
    let end_page = (addr + len - 1) / page_size;

    // Switch to kernel PML4 which has the identity map (PML4[0]).
    // Interrupts are disabled (SFMASK clears IF on syscall entry), so this is safe.
    let kernel_pml4 = crate::memory::kernel_pml4_phys();
    let old_cr3: u64;
    unsafe {
        core::arch::asm!("mov {0}, cr3", out(reg) old_cr3);
        core::arch::asm!("mov cr3, {0}", in(reg) kernel_pml4);
    }

    // Now we're running with the kernel PML4. The identity map is active,
    // so phys_to_virt (which is identity: virt == phys) works for any physical address.
    let result = unsafe {
        let pml4_virt = crate::memory::vmm::phys_to_virt(pml4_phys);
        let pml4 = &*(pml4_virt.as_ptr() as *const PageTable);

        let mut ok = true;
        for page_num in start_page..=end_page {
            let virt = page_num * page_size;

            let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
            let pml4_entry = &pml4[PageTableIndex::new(pml4_idx as u16)];
            if !pml4_entry.flags().contains(PageTableFlags::PRESENT) {
                ok = false;
                break;
            }

            let pdpt_virt = crate::memory::vmm::phys_to_virt(pml4_entry.addr().as_u64());
            let pdpt = &*(pdpt_virt.as_ptr() as *const PageTable);

            let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
            let pdpt_entry = &pdpt[PageTableIndex::new(pdpt_idx as u16)];
            if !pdpt_entry.flags().contains(PageTableFlags::PRESENT) {
                ok = false;
                break;
            }
            if pdpt_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                ok = pdpt_entry.flags().contains(PageTableFlags::USER_ACCESSIBLE);
                break;
            }

            let pd_virt = crate::memory::vmm::phys_to_virt(pdpt_entry.addr().as_u64());
            let pd = &*(pd_virt.as_ptr() as *const PageTable);

            let pd_idx = ((virt >> 21) & 0x1FF) as usize;
            let pd_entry = &pd[PageTableIndex::new(pd_idx as u16)];
            if !pd_entry.flags().contains(PageTableFlags::PRESENT) {
                ok = false;
                break;
            }
            if pd_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                ok = pd_entry.flags().contains(PageTableFlags::USER_ACCESSIBLE);
                break;
            }

            let pt_virt = crate::memory::vmm::phys_to_virt(pd_entry.addr().as_u64());
            let pt = &*(pt_virt.as_ptr() as *const PageTable);

            let pt_idx = ((virt >> 12) & 0x1FF) as usize;
            let pt_entry = &pt[PageTableIndex::new(pt_idx as u16)];
            if !pt_entry.flags().contains(PageTableFlags::PRESENT) {
                ok = false;
                break;
            }
            if !pt_entry.flags().contains(PageTableFlags::USER_ACCESSIBLE) {
                ok = false;
                break;
            }
        }
        ok
    };

    // Restore original CR3 (user PML4)
    unsafe {
        core::arch::asm!("mov cr3, {0}", in(reg) old_cr3);
    }

    result
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
    SFMask::write(x86_64::registers::rflags::RFlags::INTERRUPT_FLAG);

    // ── Enable SCE + NX in EFER ─────────────────────────────────────────
    // SCE: enables `syscall`/`sysret` instructions.
    // NXE: enables No-Execute bit in page tables (NX protection).
    //      Must be set BEFORE mapping any pages with NO_EXECUTE flag.
    unsafe {
        let mut efer = Efer::read();
        efer |= EferFlags::SYSTEM_CALL_EXTENSIONS;
        efer |= EferFlags::NO_EXECUTE_ENABLE;
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

        // Construct IRET frame FIRST (below GP regs in memory),
        // THEN push GP regs on top. This produces the same layout the timer
        // handler expects: GP regs at [RSP+0..112], IRET at [RSP+120..160].
        //
        // Push IRET frame (5 qwords) — these end up at HIGHER addresses
        // because the subsequent GP pushes go to LOWER addresses.
        "push 0x23",                            // SS  = user data selector (Ring 3)
        "push rcx",                             // RSP = user RSP
        "push rbx",                             // RFLAGS = user RFLAGS
        "push 0x1B",                            // CS  = user code selector (Ring 3)
        "push rax",                             // RIP = user RIP

        // Push 15 GP regs (R15 first → RAX last). These go to LOWER addresses,
        // placing them BELOW the IRET frame — matching the timer handler layout.
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
        // RSP now points at the GP frame base, with IRET frame immediately
        // above it — identical to the timer interrupt layout.

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

        // ── Normal return path: iretq (replaces sysretq for CVE-2012-0217) ──
        ".normal_return:",
        "pop rax",
        "pop rbx",
        "pop rcx",                              // RCX = user RIP (saved by CPU on syscall)
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop rbp",
        "pop r8",
        "pop r9",
        "pop r10",
        "pop r11",                              // R11 = user RFLAGS (saved by CPU on syscall)
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",

        // Read user RSP from per-CPU data BEFORE swapgs (while GS still points to kernel per-CPU)
        "mov r12, gs:[0]",                      // r12 = user RSP

        "swapgs",                               // Restore user GSBase

        // Construct IRET frame and return via iretq (safe on all Intel CPUs).
        // sysretq is vulnerable to CVE-2012-0217 on some Intel CPUs.
        "push 0x23",                            // SS  = user data selector (Ring 3)
        "push r12",                             // RSP = user stack
        "push r11",                             // RFLAGS = user RFLAGS
        "push 0x1B",                            // CS  = user code selector (Ring 3)
        "push rcx",                             // RIP = user instruction pointer
        "iretq",

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
        4 => sys_waitpid(arg0),
        5 => sys_sleep(arg0),
        6 => sys_read(arg0, arg1, arg2),
        7 => sys_pipe(),
        8 => sys_fork(),
        9 => sys_exec(arg0),
        10 => sys_close(arg0),
        11 => sys_dup(arg0),
        12 => sys_open(arg0),
        13 => sys_lseek(arg0, arg1),
        14 => sys_dup2(arg0, arg1),
        15 => sys_readdir(arg0, arg1, arg2),
        _ => {
            crate::serial::write_str("[SYSCALL] Unknown syscall: ");
            crate::serial::write_u64(syscall_num);
            crate::serial::write_nl();
            errno::ENOSYS as u64
        }
    };

    // Store return value in RAX slot
    *frame.add(0) = result;

    result
}

// ─────────────────────────────────────────────────────────────────────────────
// System call implementations
// ─────────────────────────────────────────────────────────────────────────────

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
            // Re-parent all live children to PID 1 (init/reaper) before becoming zombie.
            // This prevents orphaned processes from being lost when this process is reaped.
            sched.reparent_orphans_to_init(pid);
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

/// SYS_WAITPID (4) — Wait for a child process to exit.
///
/// Non-blocking implementation (WNOHANG):
/// - If `child_pid == 0`: wait for any child
/// - If `child_pid > 0`: wait for that specific child
/// - If child is Zombie: reap it (free slot) and return its exit_code
/// - If child is still running: return 0 (WNOHANG)
/// - If no matching child found: return -1 (u64::MAX)
///
/// Arguments: child_pid
/// Returns: exit_code of reaped child, 0 if still running, -1 on error
fn sys_waitpid(child_pid: u64) -> u64 {
    use crate::process::ProcessState;

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    let parent_pid = match sched.current_pid() {
        Some(pid) => pid,
        None => return errno::ESRCH as u64,
    };
    let parent_gen = sched.get_generation(parent_pid);

    // Step 1: Find the target child and its state
    let (found_pid, is_zombie, exit_code) = if child_pid == 0 {
        // Wait for any child
        match sched.find_any_zombie_child() {
            Some((c_pid, _, exit)) => (c_pid, true, exit),
            None => return errno::ESRCH as u64, // No children at all
        }
    } else {
        // Wait for specific child
        if !sched.is_child_of(child_pid, parent_pid, parent_gen) {
            return errno::ESRCH as u64; // Not our child
        }
        match sched.processes().get(child_pid as usize) {
            Some(Some(proc)) => {
                let is_z = proc.state == ProcessState::Zombie;
                let exit = if is_z { proc.exit_code } else { 0 };
                (child_pid, is_z, exit)
            }
            _ => return errno::ESRCH as u64, // Child slot empty
        }
    };

    // Step 2: Act on the child's state
    if is_zombie {
        // Found a zombie — reap it (free slot)
        sched.reap_zombie(found_pid);
        exit_code
    } else {
        // Child still running (WNOHANG)
        0
    }
}

/// SYS_SLEEP (5) — Sleep for a specified number of timer ticks.
///
/// The process enters Blocked state and will not be scheduled until
/// the specified number of ticks have elapsed. Other processes continue
/// to run during this time.
///
/// Arguments: ticks (number of 10 ms ticks to sleep)
/// Returns: always 0
fn sys_sleep(ticks: u64) -> u64 {
    let deadline = crate::interrupts::pit::tick_count() + ticks;

    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                proc.state = crate::process::ProcessState::Blocked;
                proc.wake_reason = crate::process::WakeReason::Sleep { deadline };
                #[cfg(DEBUG_KERNEL)]
                {
                    crate::serial::write_str("[SYSCALL] sleep PID=");
                    crate::serial::write_u64(pid);
                    crate::serial::write_str(" ticks=");
                    crate::serial::write_u64(ticks);
                    crate::serial::write_str(" deadline=");
                    crate::serial::write_u64(deadline);
                    crate::serial::write_nl();
                }
            }
        }
    }

    // Force context switch — process is now Blocked, scheduler picks next Ready
    unsafe { set_force_switch(); }

    0
}

/// SYS_READ (6) — Read data from a file descriptor.
///
/// For fd=0 (stdin): reads from keyboard buffer. If buffer is empty,
/// blocks the process until data arrives.
/// For fd=1,2: returns error (can't read from stdout/stderr).
///
/// Arguments: fd, buf_ptr, count
/// Returns: number of bytes read, or u64::MAX on error
fn sys_read(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    if fd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }

    if count == 0 {
        return 0;
    }

    // Validate the user buffer address range
    if !is_valid_user_range(buf_ptr, count) {
        return errno::EFAULT as u64;
    }

    // Validate the user buffer is actually mapped before dereferencing
    let pml4 = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                proc.pml4_phys
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };
    if !is_user_buffer_mapped(pml4, buf_ptr, count) {
        return errno::EFAULT as u64;
    }

    // Get the current process's FD type
    let fd_type = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                proc.fd_types[fd as usize]
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };

    match fd_type {
        crate::process::FdType::Stdin | crate::process::FdType::Tty => {
            // Read from the line discipline buffer (blocks until a line is available)
            let buf = buf_ptr as *mut u8;
            let slice = unsafe { core::slice::from_raw_parts_mut(buf, count as usize) };
            let nread = crate::keyboard::read_line(slice);
            nread as u64
        }
        crate::process::FdType::Stdout | crate::process::FdType::Stderr => {
            errno::EBADF as u64 // Can't read from stdout/stderr
        }
        crate::process::FdType::Null => {
            0
        }
        crate::process::FdType::Pipe { pipe_idx, writable } => {
            if writable {
                return errno::EBADF as u64; // Can't read from write end
            }
            let pipe_idx = pipe_idx as usize;
            let buf = buf_ptr as *mut u8;
                    let mut total_read = 0u64;

            // Read available data from pipe (non-blocking check first)
            unsafe {
                if let Some(ref mut p) = crate::process::PIPES[pipe_idx] {
                    let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed) as u64;
                    let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed) as u64;
                    while total_read < count && nread + total_read < nwrite {
                        let idx = ((nread + total_read) as usize) % crate::process::pipe::PIPE_SIZE;
                        *buf.add(total_read as usize) = p.data[idx];
                        total_read += 1;
                    }
                    if total_read > 0 {
                        p.nread.store((nread + total_read) as u32, core::sync::atomic::Ordering::Relaxed);
                        return total_read;
                    }
                    // Check if writer is closed (EOF)
                    if !p.write_open.load(core::sync::atomic::Ordering::Relaxed) {
                        return 0; // EOF
                    }
                }
            }

            // Buffer empty — block until data arrives
            {
                let mut sched = crate::process::scheduler::SCHEDULER.lock();
                if let Some(pid) = sched.current_pid() {
                    if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                        proc.state = crate::process::ProcessState::Blocked;
                        proc.wake_reason = crate::process::WakeReason::PipeRead { pipe_idx: pipe_idx as u8 };
                    }
                }
            }
            unsafe { set_force_switch(); }
            0
        }
        crate::process::FdType::FsFile { index } => {
            let index = index as usize;
            let sched = crate::process::scheduler::SCHEDULER.lock();            if let Some(pid) = sched.current_pid() {
                if let Some(ref proc) = sched.processes()[pid as usize] {
                    if let Some(ref file_handle) = proc.file_handles[index] {
                        let buf = buf_ptr as *mut u8;
                        let slice = unsafe { core::slice::from_raw_parts_mut(buf, count as usize) };
                        // Lock the mutex for interior mutability (File trait requires &mut self)
                        let mut file = file_handle.lock();
                        match file.read(slice) {
                            Ok(n) => n as u64,
                            Err(e) => e.to_errno() as u64,
                        }
                    } else {
                        errno::EBADF as u64
                    }
                } else {
                    errno::ESRCH as u64
                }
            } else {
                errno::ESRCH as u64
            }
        }
        crate::process::FdType::None => {
            errno::EBADF as u64
        }
    }
}

/// SYS_WRITE (0) — Write data to a file descriptor (updated for pipes).
fn sys_write(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    if fd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }

    if count == 0 {
        return 0;
    }

    if !is_valid_user_range(buf_ptr, count) {
        return errno::EFAULT as u64;
    }

    let fd_type = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                proc.fd_types[fd as usize]
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };

    // Validate the user buffer is actually mapped before dereferencing
    let pml4 = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                proc.pml4_phys
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };
    if !is_user_buffer_mapped(pml4, buf_ptr, count) {
        return errno::EFAULT as u64;
    }

    match fd_type {
        crate::process::FdType::Stdout | crate::process::FdType::Stderr | crate::process::FdType::Tty => {
            let slice = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, count as usize) };
            for &byte in slice {
                crate::serial::write_byte(byte);
            }
            count
        }
        crate::process::FdType::Stdin => {
            errno::EBADF as u64 // Can't write to stdin
        }
        crate::process::FdType::Null => {
            count
        }
        crate::process::FdType::Pipe { pipe_idx, writable } => {
            if !writable {
                return errno::EBADF as u64; // Can't write to read end
            }
            let pipe_idx = pipe_idx as usize;
            let buf = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, count as usize) };

            unsafe {
                if let Some(ref mut p) = crate::process::PIPES[pipe_idx] {
                    let mut written = 0u64;
                    for &byte in buf {
                        loop {
                            let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed);
                            let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed);
                            if nwrite < nread + crate::process::pipe::PIPE_SIZE as u32 {
                                break;
                            }
                            if !p.read_open.load(core::sync::atomic::Ordering::Relaxed) {
                                return written;
                            }
                            crate::process::yield_now();
                        }
                        let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed);
                        let idx = (nwrite as usize) % crate::process::pipe::PIPE_SIZE;
                        p.data[idx] = byte;
                        p.nwrite.store(nwrite + 1, core::sync::atomic::Ordering::Relaxed);
                        written += 1;
                    }
                    crate::process::keyboard_wake();
                    return written;
                }
            }
            errno::EBADF as u64
        }
        crate::process::FdType::FsFile { index } => {
            let index = index as usize;
            let sched = crate::process::scheduler::SCHEDULER.lock();            if let Some(pid) = sched.current_pid() {
                if let Some(ref proc) = sched.processes()[pid as usize] {
                    if let Some(ref file_handle) = proc.file_handles[index] {
                        let slice = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, count as usize) };
                        let mut file = file_handle.lock();
                        match file.write(slice) {
                            Ok(n) => n as u64,
                            Err(e) => e.to_errno() as u64,
                        }
                    } else {
                        errno::EBADF as u64
                    }
                } else {
                    errno::ESRCH as u64
                }
            } else {
                errno::ESRCH as u64
            }
        }
        crate::process::FdType::None => {
            errno::EBADF as u64
        }
    }
}

/// SYS_PIPE (7) — Create a pipe pair.
///
/// Returns: (read_fd << 32) | write_fd, or negative errno on error
fn sys_pipe() -> u64 {
    let pipe_idx = match crate::process::alloc_pipe() {
        Some(idx) => idx,
        None => return errno::ENOMEM as u64,
    };

    let (read_fd, write_fd) = {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                let mut first_free = None;
                let mut second_free = None;
                for i in 0..crate::process::MAX_FDS {
                    if proc.fd_types[i] == crate::process::FdType::None {
                        if first_free.is_none() {
                            first_free = Some(i);
                        } else {
                            second_free = Some(i);
                            break;
                        }
                    }
                }
                match (first_free, second_free) {
                    (Some(r), Some(w)) => {
                        proc.fd_types[r] = crate::process::FdType::Pipe { pipe_idx: pipe_idx as u8, writable: false };
                        proc.fd_types[w] = crate::process::FdType::Pipe { pipe_idx: pipe_idx as u8, writable: true };
                        (r as u64, w as u64)
                    }
                    _ => {
                        // FD allocation failed — free the pipe to prevent global leak
                        unsafe { crate::process::free_pipe(pipe_idx); }
                        return errno::EMFILE as u64;
                    }
                }
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };

    crate::serial::write_str("[SYSCALL] pipe read_fd=");
    crate::serial::write_u64(read_fd);
    crate::serial::write_str(" write_fd=");
    crate::serial::write_u64(write_fd);
    crate::serial::write_nl();

    (read_fd << 32) | write_fd
}

/// SYS_FORK (8) — Fork the current process.
///
/// Creates a copy using CoW. Child gets RAX=0, parent gets child PID.
fn sys_fork() -> u64 {
    use crate::memory::{self, vmm, PhysAddr};
    use crate::process::process::Process;

    let (parent_pid, parent_pml4, parent_sp, parent_is_user) = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        match sched.current_pid() {
            Some(pid) => {
                if let Some(ref proc) = sched.processes()[pid as usize] {
                    (pid, proc.pml4_phys, proc.stack_pointer, proc.is_user)
                } else {
                    return errno::ESRCH as u64;
                }
            }
            None => return errno::ESRCH as u64,
        }
    };

    let kernel_pml4 = memory::kernel_pml4_phys();
    let child_pml4 = vmm::create_user_pml4(PhysAddr::new(kernel_pml4));

    match unsafe { vmm::copy_user_pages(PhysAddr::new(parent_pml4), child_pml4) } {
        Ok(()) => {}
        Err(()) => return errno::ENOMEM as u64,
    }

    let stack_base = {
        let layout = core::alloc::Layout::from_size_align(crate::process::process::KERNEL_STACK_SIZE, 16)
            .expect("Invalid kernel stack layout");
        unsafe {
            let ptr = alloc::alloc::alloc(layout);
            if ptr.is_null() {
                return errno::ENOMEM as u64;
            }
            core::ptr::write_bytes(ptr, 0, crate::process::process::KERNEL_STACK_SIZE);
            ptr as u64
        }
    };
    let stack_top = stack_base + crate::process::process::KERNEL_STACK_SIZE as u64;

    let child_sp = {
        let frame_size = 20 * 8;
        let child_frame_base = stack_top - frame_size as u64;
        unsafe {
            let src = parent_sp as *const u64;
            let dst = child_frame_base as *mut u64;
            core::ptr::copy_nonoverlapping(src, dst, 20);
            (child_frame_base as *mut u64).write(0);
        }
        child_frame_base
    };

    let child_pid = {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        let pid = (1..crate::process::MAX_PROCESSES as u64)
            .find(|&i| sched.processes()[i as usize].is_none());

        match pid {
            Some(pid) => {
                let mut child = Process::new_kernel(pid, 0);
                child.state = crate::process::ProcessState::Ready;
                child.stack_pointer = child_sp;
                child.kernel_stack_base = stack_base;
                child.pml4_phys = child_pml4.as_u64();
                child.is_user = parent_is_user;
                child.parent_pid = Some(parent_pid);

                if let Some(parent_proc) = sched.processes()[parent_pid as usize].as_ref() {
                    child.fd_types = parent_proc.fd_types;
                    child.file_handles = parent_proc.file_handles.clone();
                    child.parent_generation = parent_proc.generation;
                }

                sched.processes_mut()[pid as usize] = Some(child);
                pid
            }
            None => return errno::ENOMEM as u64,
        }
    };

    crate::serial::write_str("[SYSCALL] fork parent=");
    crate::serial::write_u64(parent_pid);
    crate::serial::write_str(" child=");
    crate::serial::write_u64(child_pid);
    crate::serial::write_nl();

    child_pid
}

/// SYS_EXEC (9) — Replace process address space with a new ELF binary.
///
/// Reads the path from user space, loads the ELF from VFS, replaces the
/// process's address space, and resets the instruction/stack pointers.
fn sys_exec(path_ptr: u64) -> u64 {
    use alloc::string::String;
    use alloc::vec::Vec;
    use crate::memory::{self, vmm, PhysAddr};

    if path_ptr == 0 {
        return errno::EFAULT as u64;
    }

    // Read the path string from user space
    let path = {
        let mut buf = Vec::new();
        let user_ptr = path_ptr as *const u8;
        let pml4 = {
            let sched = crate::process::scheduler::SCHEDULER.lock();
            if let Some(pid) = sched.current_pid() {
                if let Some(ref proc) = sched.processes()[pid as usize] {
                    proc.pml4_phys
                } else {
                    return errno::ESRCH as u64;
                }
            } else {
                return errno::ESRCH as u64;
            }
        };

        for i in 0..4096u64 {
            if !is_valid_user_range(user_ptr as u64 + i, 1) {
                return errno::EFAULT as u64;
            }
            if !is_user_buffer_mapped(pml4, user_ptr as u64 + i, 1) {
                return errno::EFAULT as u64;
            }
            let byte = unsafe { *user_ptr.add(i as usize) };
            if byte == 0 {
                break;
            }
            buf.push(byte);
        }
        String::from_utf8(buf).unwrap_or_default()
    };

    if path.is_empty() {
        return errno::EINVAL as u64;
    }

    crate::serial::write_str("[SYSCALL] exec: ");
    crate::serial::write_str(&path);
    crate::serial::write_nl();

    // Read the ELF from VFS
    let elf_data = match crate::vfs::vfs().read_file(&path) {
        Ok(data) => data,
        Err(e) => return e.to_errno() as u64,
    };

    if elf_data.is_empty() {
        return errno::ENOENT as u64;
    }

    // Get current process info
    let (current_pid, old_pml4_phys, _old_kernel_stack_base) = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                (pid, proc.pml4_phys, proc.kernel_stack_base)
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };

    // Create a new user PML4 (don't free old one yet — atomic transition)
    let kernel_pml4 = memory::kernel_pml4_phys();
    let new_pml4 = vmm::create_user_pml4(PhysAddr::new(kernel_pml4));

    // Load the ELF into the new address space
    let elf_image = match crate::elf::load_elf(&elf_data, new_pml4) {
        Ok(img) => img,
        Err(e) => {
            crate::serial::write_str("[SYSCALL] exec: ELF load failed: ");
            crate::serial::write_str(e.description());
            crate::serial::write_nl();
            // Free the new PML4 (no user pages were loaded)
            unsafe { vmm::free_user_address_space(new_pml4); }
            return errno::ENOEXEC as u64;
        }
    };

    // Map a new user stack: 4 pages (16 KiB) + 1 guard page
    let user_stack_top = crate::memory::USER_STACK_TOP;
    let user_stack_bottom = user_stack_top - 4 * crate::memory::PAGE_SIZE;

    // Map the stack pages
    for i in 0..4u64 {
        let page_virt = x86_64::VirtAddr::new(user_stack_bottom + i * crate::memory::PAGE_SIZE);
        let frame = match vmm::PmmFrameAllocator.allocate_frame() {
            Some(f) => f,
            None => {
                unsafe { vmm::free_user_address_space(new_pml4); }
                return errno::ENOMEM as u64;
            }
        };
        vmm::map_page(
            new_pml4,
            page_virt,
            PhysAddr::new(frame.start_address().as_u64()),
            x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE,
        );
        // Zero the page
        let frame_ptr = unsafe {
            vmm::phys_to_virt(frame.start_address().as_u64()).as_mut_ptr::<u8>()
        };
        unsafe { core::ptr::write_bytes(frame_ptr, 0, 4096); }
    }

    // Map guard page (present but not writable — stack overflow hits this → page fault)
    // No USER_ACCESSIBLE: kernel-only page, any user access triggers a fault.
    let guard_virt = x86_64::VirtAddr::new(user_stack_bottom - crate::memory::PAGE_SIZE);
    let guard_frame = match vmm::PmmFrameAllocator.allocate_frame() {
        Some(f) => f,
        None => {
            unsafe { vmm::free_user_address_space(new_pml4); }
            return errno::ENOMEM as u64;
        }
    };
    vmm::map_page(
        new_pml4,
        guard_virt,
        PhysAddr::new(guard_frame.start_address().as_u64()),
        x86_64::structures::paging::PageTableFlags::PRESENT,
    );

    // NEW address space is fully built. NOW safe to free the old one.
    unsafe {
        vmm::free_user_address_space(PhysAddr::new(old_pml4_phys));
    }

    // Update the process
    let user_rip = elf_image.entry;
    let user_rsp = user_stack_top - 8; // ABI: RSP must be 16-byte aligned before CALL

    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(ref mut proc) = sched.processes_mut()[current_pid as usize] {
            proc.pml4_phys = new_pml4.as_u64();
            proc.user_rip = Some(user_rip);
            proc.user_rsp = Some(user_rsp);
            proc.is_user = true;

            // Set up a new initial stack frame for this process
            // We need to modify the saved RSP on the kernel stack to point to
            // a new user-mode IRET frame that will return to the new entry point.
            let sp = proc.stack_pointer as *mut u64;
            unsafe {
                // [rsp+2] = RCX = user RIP (for IRET)
                sp.add(2).write(user_rip);
                // [rsp+5] = RDI = user RSP (for IRET) — no, that's wrong.
                // The frame layout is: [RAX][RBX][RCX][RDX][RSI][RDI][RBP][R8][R9][R10][R11][R12][R13][R14][R15]
                // followed by IRET: [RIP][CS][RFLAGS][RSP][SS]
                // RCX (offset 2) = user RIP saved by CPU on syscall
                // R11 (offset 10) = user RFLAGS saved by CPU on syscall
                // For the IRET frame (offset 15-19):
                //   [15] = RIP
                //   [16] = CS
                //   [17] = RFLAGS
                //   [18] = RSP
                //   [19] = SS
                sp.add(15).write(user_rip);  // RIP
                sp.add(16).write(crate::gdt::user_code_selector().0 as u64); // CS
                sp.add(17).write(0x202u64);  // RFLAGS (IF=1)
                sp.add(18).write(user_rsp);   // RSP
                sp.add(19).write(crate::gdt::user_data_selector().0 as u64); // SS
            }
        }
    }

    crate::serial::write_str("[SYSCALL] exec: entry=");
    crate::serial::write_hex(user_rip);
    crate::serial::write_str(" stack=");
    crate::serial::write_hex(user_rsp);
    crate::serial::write_nl();

    0
}

/// SYS_CLOSE (10) — Close a file descriptor.
fn sys_close(fd: u64) -> u64 {
    if fd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            let fd_type = proc.fd_types[fd as usize];
            match fd_type {
                crate::process::FdType::Pipe { pipe_idx, writable } => {
                    let pipe_idx = pipe_idx as usize;
                    unsafe {
                        if let Some(ref mut p) = crate::process::PIPES[pipe_idx] {
                            crate::process::pipe::pipe_close(p, writable);
                            // Decrement refcount. If last reference, free the pipe slot.
                            let old = p.refcount.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
                            if old == 1 {
                                crate::process::free_pipe(pipe_idx);
                            }
                        }
                    }
                }
                crate::process::FdType::FsFile { index } => {
                    let index = index as usize;
                    // Only clear the file handle slot if no other FDs share it.
                    // After dup, multiple FDs may reference the same handle slot.
                    let still_referenced = proc.fd_types.iter().enumerate().any(|(i, f)| {
                        i != fd as usize
                            && matches!(f, crate::process::FdType::FsFile { index: idx } if *idx as usize == index)
                    });
                    if !still_referenced {
                        proc.file_handles[index] = None;
                    }
                }
                _ => {}
            }
            proc.fd_types[fd as usize] = crate::process::FdType::None;
            return 0;
        }
    }
    errno::EBADF as u64
}

/// SYS_DUP (11) — Duplicate a file descriptor to the lowest available slot.
///
/// File handles are ref-counted via Arc. dup clones the Arc, so multiple FDs
/// can safely share one underlying file. When the last FD is closed, the Arc
/// refcount drops to 0 and the File is dropped.
fn sys_dup(fd: u64) -> u64 {
    if fd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            let fd_type = proc.fd_types[fd as usize];
            if fd_type == crate::process::FdType::None {
                return errno::EBADF as u64;
            }
            // For FsFile, share the same handle slot via Arc refcount.
            // No new handle slot needed — multiple FDs point to the same index.
            if let crate::process::FdType::FsFile { index } = fd_type {
                let index = index as usize;
                if proc.file_handles[index].is_none() {
                    return errno::EBADF as u64;
                }
                // Find free FD slot and point it to the same handle index
                for i in 0..crate::process::MAX_FDS {
                    if proc.fd_types[i] == crate::process::FdType::None {
                        proc.fd_types[i] = crate::process::FdType::FsFile { index: index as u8 };
                        return i as u64;
                    }
                }
                return errno::EMFILE as u64; // No free FD slots
            }
            // For non-FsFile types (Pipe, Stdin, etc.), copy the FD type directly.
            // For Pipe, also increment the refcount so the pipe isn't freed when only
            // the original FD is closed.
            if let crate::process::FdType::Pipe { pipe_idx, writable: _ } = fd_type {
                let pipe_idx = pipe_idx as usize;
                unsafe {
                    if let Some(ref mut p) = crate::process::PIPES[pipe_idx] {
                        p.refcount.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
                    }
                }
            }
            for i in 0..crate::process::MAX_FDS {
                if proc.fd_types[i] == crate::process::FdType::None {
                    proc.fd_types[i] = fd_type;
                    return i as u64;
                }
            }
        }
    }
    errno::EMFILE as u64
}

/// SYS_DUP2 (14) — Duplicate a file descriptor to a specific target number.
///
/// Arguments: oldfd (source), newfd (target)
/// Returns: newfd on success, or negative errno on error.
///
/// Semantics (POSIX-compatible):
/// 1. Validate oldfd — must be open
/// 2. Validate newfd — must be in range 0..MAX_FDS
/// 3. If oldfd == newfd, return newfd (no-op)
/// 4. Close newfd if open (reuse sys_close logic inline to avoid double-locking)
/// 5. Copy the FD type from oldfd to newfd
/// 6. For Pipe: increment refcount
/// 7. Return newfd
fn sys_dup2(oldfd: u64, newfd: u64) -> u64 {
    if oldfd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }
    if newfd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            // Validate oldfd is open
            let old_type = proc.fd_types[oldfd as usize];
            if old_type == crate::process::FdType::None {
                return errno::EBADF as u64;
            }

            // If oldfd == newfd, return newfd (POSIX no-op)
            if oldfd == newfd {
                return newfd;
            }

            // Close newfd if open (inline close logic to avoid double-locking scheduler)
            let new_type = proc.fd_types[newfd as usize];
            if new_type != crate::process::FdType::None {
                match new_type {
                    crate::process::FdType::Pipe { pipe_idx, writable } => {
                        let pipe_idx = pipe_idx as usize;
                        unsafe {
                            if let Some(ref mut p) = crate::process::PIPES[pipe_idx] {
                                crate::process::pipe::pipe_close(p, writable);
                                let old_ref = p.refcount.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
                                if old_ref == 1 {
                                    crate::process::free_pipe(pipe_idx);
                                }
                            }
                        }
                    }
                    crate::process::FdType::FsFile { index } => {
                        let index = index as usize;
                        // Only clear the handle slot if no other FDs share it
                        let still_referenced = proc.fd_types.iter().enumerate().any(|(i, f)| {
                            i != newfd as usize
                                && matches!(f, crate::process::FdType::FsFile { index: idx } if *idx as usize == index)
                        });
                        if !still_referenced {
                            proc.file_handles[index] = None;
                        }
                    }
                    _ => {}
                }
            }

            // Copy oldfd type to newfd
            // For Pipe, increment refcount
            if let crate::process::FdType::Pipe { pipe_idx, writable: _ } = old_type {
                let pipe_idx = pipe_idx as usize;
                unsafe {
                    if let Some(ref mut p) = crate::process::PIPES[pipe_idx] {
                        p.refcount.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
                    }
                }
            }
            proc.fd_types[newfd as usize] = old_type;
            return newfd;
        }
    }
    errno::EBADF as u64
}

/// SYS_READDIR (15) — Read directory entries into a buffer.
///
/// Arguments: fd (directory fd), buf_ptr (user buffer), count (buffer size)
/// Returns: bytes written on success, 0 on end of directory, or negative errno.
///
/// Writes null-terminated filenames sequentially into buf.
/// Returns 0 when all entries have been listed.
fn sys_readdir(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    if fd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }
    if count == 0 {
        return 0;
    }
    if !is_valid_user_range(buf_ptr, count) {
        return errno::EFAULT as u64;
    }

    let sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref proc) = sched.processes()[pid as usize] {
            let fd_type = proc.fd_types[fd as usize];
            match fd_type {
                crate::process::FdType::FsFile { index } => {
                    let index = index as usize;
                    if let Some(ref file_handle) = proc.file_handles[index] {
                        let mut file = file_handle.lock();
                        // Read directory entries — ramfs files store entries as
                        // null-terminated strings packed sequentially
                        let buf = buf_ptr as *mut u8;
                        let mut total = 0u64;
                        let mut tmp = [0u8; 256];
                        loop {
                            if total >= count {
                                break;
                            }
                            match file.read(&mut tmp) {
                                Ok(0) => break,
                                Ok(n) => {
                                    let to_copy = core::cmp::min(n as u64, count - total);
                                    unsafe {
                                        core::ptr::copy_nonoverlapping(
                                            tmp.as_ptr(),
                                            buf.add(total as usize),
                                            to_copy as usize,
                                        );
                                    }
                                    total += to_copy;
                                }
                                Err(_) => break,
                            }
                        }
                        return total;
                    }
                    errno::EBADF as u64
                }
                _ => errno::ENOTDIR as u64,
            }
        } else {
            errno::ESRCH as u64
        }
    } else {
        errno::ESRCH as u64
    }
}

/// SYS_OPEN (12) — Open a file by path.
///
/// Arguments: path_ptr (user pointer to null-terminated path string)
/// Returns: fd number on success, or negative errno on error
fn sys_open(path_ptr: u64) -> u64 {
    use alloc::string::String;

    if path_ptr == 0 {
        return errno::EFAULT as u64;
    }

    // Read the path string from user space
    let path = {
        let mut buf = Vec::new();
        let user_ptr = path_ptr as *const u8;
        let pml4 = {
            let sched = crate::process::scheduler::SCHEDULER.lock();
            if let Some(pid) = sched.current_pid() {
                if let Some(ref proc) = sched.processes()[pid as usize] {
                    proc.pml4_phys
                } else {
                    return errno::ESRCH as u64;
                }
            } else {
                return errno::ESRCH as u64;
            }
        };

        // Read byte by byte until null terminator
        for i in 0..4096u64 {
            if !is_valid_user_range(user_ptr as u64 + i, 1) {
                return errno::EFAULT as u64;
            }
            if !is_user_buffer_mapped(pml4, user_ptr as u64 + i, 1) {
                return errno::EFAULT as u64;
            }
            let byte = unsafe { *user_ptr.add(i as usize) };
            if byte == 0 {
                break;
            }
            buf.push(byte);
        }
        String::from_utf8(buf).unwrap_or_default()
    };

    if path.is_empty() {
        return errno::EINVAL as u64;
    }

    // Open the file via VFS
    let file = match crate::vfs::vfs().open(&path) {
        Ok(f) => f,
        Err(e) => return e.to_errno() as u64,
    };

    // Find a free FD slot and file handle slot
    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            // Find free FD
            let fd_slot = proc.fd_types.iter().position(|f| *f == crate::process::FdType::None);
            let fh_slot = proc.file_handles.iter().position(|f| f.is_none());

            match (fd_slot, fh_slot) {
                (Some(fd), Some(fh)) => {
                    // Wrap in Arc<Mutex<...>> for ref-counted sharing + interior mutability
                    proc.file_handles[fh] = Some(alloc::sync::Arc::new(spin::Mutex::new(file)));
                    proc.fd_types[fd] = crate::process::FdType::FsFile { index: fh as u8 };
                    fd as u64
                }
                _ => errno::EMFILE as u64,
            }
        } else {
            errno::ESRCH as u64
        }
    } else {
        errno::ESRCH as u64
    }
}

/// SYS_LSEEK (13) — Seek to a position in a file.
///
/// Arguments: fd, offset
/// Returns: 0 on success, or negative errno on error
fn sys_lseek(fd: u64, offset: u64) -> u64 {
    if fd >= crate::process::MAX_FDS as u64 {
        return errno::EBADF as u64;
    }

    let sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref proc) = sched.processes()[pid as usize] {
            let fd_type = proc.fd_types[fd as usize];
            match fd_type {
                crate::process::FdType::FsFile { index } => {
                    let index = index as usize;
                    if let Some(ref file_handle) = proc.file_handles[index] {
                        let mut file = file_handle.lock();
                        match file.seek(offset) {
                            Ok(()) => 0,
                            Err(e) => e.to_errno() as u64,
                        }
                    } else {
                        errno::EBADF as u64
                    }
                }
                _ => errno::EINVAL as u64,
            }
        } else {
            errno::ESRCH as u64
        }
    } else {
        errno::ESRCH as u64
    }
}
