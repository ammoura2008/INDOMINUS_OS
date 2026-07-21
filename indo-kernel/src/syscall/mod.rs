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
        'outer: for page_num in start_page..=end_page {
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
/// Validates that every page in the user buffer is mapped and user-accessible
/// before reading any data, preventing kernel page faults from bad user pointers.
///
/// Arguments: fd, buf_ptr, count
/// Returns: number of bytes written, or u64::MAX on error (-EINVAL)
fn sys_write(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    if fd != 1 {
        return u64::MAX; // Only stdout supported
    }

    // Validate the user buffer address range (bounds check)
    if !is_valid_user_range(buf_ptr, count) {
        crate::serial::write_str("[SYSCALL] sys_write: invalid user buffer addr=");
        crate::serial::write_hex(buf_ptr);
        crate::serial::write_str(" len=");
        crate::serial::write_hex(count);
        crate::serial::write_nl();
        return u64::MAX; // -EINVAL
    }

    // Validate that every page in the buffer is actually mapped and user-accessible.
    // This prevents a kernel page fault from an unmapped or kernel-only pointer.
    let pml4_phys = {
        let (frame, _) = x86_64::registers::control::Cr3::read();
        frame.start_address().as_u64()
    };
    if !is_user_buffer_mapped(pml4_phys, buf_ptr, count) {
        crate::serial::write_str("[SYSCALL] sys_write: unmapped user buffer addr=");
        crate::serial::write_hex(buf_ptr);
        crate::serial::write_str(" len=");
        crate::serial::write_hex(count);
        crate::serial::write_nl();
        return u64::MAX; // -EFAULT
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
        None => return u64::MAX,
    };

    // Step 1: Find the target child and its state
    let (found_pid, is_zombie, exit_code) = if child_pid == 0 {
        // Wait for any child
        match sched.find_any_zombie_child() {
            Some((c_pid, _, exit)) => (c_pid, true, exit),
            None => return u64::MAX, // No children at all
        }
    } else {
        // Wait for specific child
        if !sched.is_child_of(child_pid, parent_pid) {
            return u64::MAX; // Not our child
        }
        match sched.processes().get(child_pid as usize) {
            Some(Some(proc)) => {
                let is_z = proc.state == ProcessState::Zombie;
                let exit = if is_z { proc.exit_code } else { 0 };
                (child_pid, is_z, exit)
            }
            _ => return u64::MAX, // Child slot empty
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
