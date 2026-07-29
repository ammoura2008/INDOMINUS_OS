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

/// Return the kernel virtual address of the per-CPU data structure.
///
/// This is the value stored in KERNEL_GS_BASE MSR, used by the regression
/// test to verify KERNEL_GS_BASE matches.
pub fn per_cpu_base_addr() -> u64 {
    let raw = &raw const PER_CPU as u64;
    unsafe { crate::memory::phys_to_kernel_virt(raw) }
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

    // Set KERNEL_GS_BASE to point to our per-CPU data.
    // The syscall_entry handler does `swapgs` which swaps GS_BASE and KERNEL_GS_BASE.
    // After swapgs, the kernel uses KERNEL_GS_BASE for GS-relative accesses.
    // So KERNEL_GS_BASE must point to PER_CPU, while GS_BASE (used in user mode) can be 0.
    // &raw const PER_CPU gives the kernel virtual address directly (it's a static in .bss,
    // already mapped in all PML4s via the higher-half mapping). No phys→virt conversion needed.
    unsafe {
        let raw_addr = &raw const PER_CPU as u64;
        let gs_virt = crate::memory::phys_to_kernel_virt(raw_addr);
        crate::serial::write_str("[SYSCALL] PER_CPU raw=0x");
        crate::serial::write_hex(raw_addr);
        crate::serial::write_str(" virt=0x");
        crate::serial::write_hex(gs_virt);
        crate::serial::write_nl();
        // KERNEL_GS_BASE (MSR 0xC0000102) — used in kernel mode after swapgs
        Msr::new(0xC000_0102).write(gs_virt);
        // GS_BASE (MSR 0xC0000101) — not used, but clear it for sanity
        Msr::new(0xC000_0101).write(0u64);
        // Verify
        let readback: u64 = Msr::new(0xC000_0102).read();
        crate::serial::write_str("[SYSCALL] KERNEL_GS_BASE readback=0x");
        crate::serial::write_hex(readback);
        crate::serial::write_nl();
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
        4 => sys_waitpid(arg0, arg1),
        5 => sys_sleep(arg0),
        6 => sys_read(arg0, arg1, arg2),
        7 => sys_pipe(),
        8 => sys_fork(),
        9 => sys_exec(arg0),
        10 => sys_close(arg0),
        11 => sys_dup(arg0),
        12 => sys_open(arg0, arg1),
        13 => sys_lseek(arg0, arg1),
        14 => sys_dup2(arg0, arg1),
        15 => sys_readdir(arg0, arg1, arg2),
        16 => sys_unlink(arg0),
        17 => sys_brk(arg0),
        18 => sys_execve(arg0, arg1, arg2),
        19 => sys_chdir(arg0),
        20 => sys_getcwd(arg0, arg1),
        21 => sys_mkdir(arg0),
        22 => sys_mmap(arg0, arg1),
        23 => sys_munmap(arg0, arg1),
        24 => sys_tcgetattr(arg0, arg1),
        25 => sys_tcsetattr(arg0, arg1, arg2),
        26 => sys_sigaction(arg0, arg1, arg2),
        27 => sys_kill(arg0, arg1),
        28 => sys_setpgid(arg0, arg1),
        29 => sys_getpgid(arg0),
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
    let current_pid = {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            // Re-parent all live children to PID 1 (init/reaper) before becoming zombie.
            sched.reparent_orphans_to_init(pid);
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                proc.state = crate::process::ProcessState::Zombie;
                proc.exit_code = exit_code;
            }
            Some(pid)
        } else {
            None
        }
    };

    // Wake parent processes blocked in waitpid for this child.
    // Must drop SCHEDULER before calling keyboard_wake (lock ordering).
    // keyboard_wake already handles WaitForChild wake reasons.
    drop(current_pid); // just drop the Option, not a lock
    crate::process::keyboard_wake();

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
/// flags & 1 = WNOHANG (non-blocking). If clear, blocks until child exits.
///
/// Arguments: child_pid, flags
/// Returns: exit_code of reaped child, 0 if still running (WNOHANG), or -errno.
fn sys_waitpid(child_pid: u64, flags: u64) -> u64 {
    use crate::process::ProcessState;

    const WNOHANG: u64 = 1;

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
            None => {
                if flags & WNOHANG != 0 {
                    return 0; // WNOHANG: no zombie yet
                }
                // Check if we have any children at all
                let has_children = sched.processes().iter().enumerate().any(|(i, p)| {
                    i > 0 && i < crate::process::MAX_PROCESSES
                        && p.as_ref().map_or(false, |proc| {
                            proc.parent_pid == Some(parent_pid)
                                && proc.parent_generation == parent_gen
                        })
                });
                if !has_children {
                    return errno::ESRCH as u64;
                }
                // Block: set wake reason and context switch
                {
                    let pid = parent_pid;
                    if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                        proc.state = ProcessState::Blocked;
                        proc.wake_reason = crate::process::WakeReason::WaitForChild { child_pid: 0 };
                    }
                }
                drop(sched);
                unsafe { set_force_switch(); }
                return 0;
            }
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
    } else if flags & WNOHANG != 0 {
        0 // WNOHANG: child still running
    } else {
        // Block until child exits
        {
            if let Some(ref mut proc) = sched.processes_mut()[parent_pid as usize] {
                proc.state = ProcessState::Blocked;
                proc.wake_reason = crate::process::WakeReason::WaitForChild { child_pid: found_pid };
            }
        }
        drop(sched);
        unsafe { set_force_switch(); }
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
            if pipe_idx >= crate::process::MAX_PIPES {
                return errno::EBADF as u64;
            }
            let buf = buf_ptr as *mut u8;
                    let mut total_read = 0u64;

            // Read available data from pipe (non-blocking check first)
            let pipe_result = {
                let pipes = crate::process::PIPES.lock();
                if let Some(ref p) = pipes[pipe_idx] {
                    let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed) as u64;
                    let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed) as u64;
                    while total_read < count && nread + total_read < nwrite {
                        let idx = ((nread + total_read) as usize) % crate::process::pipe::PIPE_SIZE;
                        unsafe { *buf.add(total_read as usize) = p.data[idx]; }
                        total_read += 1;
                    }
                    if total_read > 0 {
                        p.nread.store(((nread + total_read) & 0xFFFF_FFFF) as u32, core::sync::atomic::Ordering::Relaxed);
                        Some(total_read)
                    } else if !p.write_open.load(core::sync::atomic::Ordering::Relaxed) {
                        Some(0u64) // EOF
                    } else {
                        None // Need to block
                    }
                } else {
                    Some(errno::EBADF as u64) // Pipe gone
                }
                // pipes Mutex dropped here
            };
            match pipe_result {
                Some(n) => {
                    if n > 0 { crate::process::keyboard_wake(); }
                    return n;
                }
                None => {} // Fall through to block
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
            if index >= crate::process::process::MAX_FILE_HANDLES {
                return errno::EBADF as u64;
            }
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
            if pipe_idx >= crate::process::MAX_PIPES {
                return errno::EBADF as u64;
            }
            let buf = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, count as usize) };

            let write_result = {
                let mut pipes = crate::process::PIPES.lock();
                if let Some(ref mut p) = pipes[pipe_idx] {
                    let mut written = 0u64;
                    for &byte in buf {
                        let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed);
                        let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed);
                        if nwrite == u32::MAX || nwrite >= nread.wrapping_add(crate::process::pipe::PIPE_SIZE as u32) {
                            // Pipe full — if no reader, return EPIPE
                            if !p.read_open.load(core::sync::atomic::Ordering::Relaxed) {
                                if written > 0 {
                                    drop(pipes);
                                    crate::process::keyboard_wake();
                                }
                                return written; // partial write, or 0 for EPIPE
                            }
                            // If we wrote some bytes, return them
                            if written > 0 {
                                drop(pipes);
                                crate::process::keyboard_wake();
                                return written;
                            }
                            // No bytes written yet — block the process properly
                            // instead of busy-spinning
                            drop(pipes);
                            {
                                let mut sched = crate::process::scheduler::SCHEDULER.lock();
                                if let Some(pid) = sched.current_pid() {
                                    if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                                        proc.state = crate::process::ProcessState::Blocked;
                                        proc.wake_reason = crate::process::WakeReason::PipeWrite {
                                            pipe_idx: pipe_idx as u8,
                                        };
                                    }
                                }
                            }
                            unsafe { crate::syscall::set_force_switch(); }
                            return 0;
                        }
                        let idx = (nwrite as usize) % crate::process::pipe::PIPE_SIZE;
                        p.data[idx] = byte;
                        p.nwrite.store(nwrite.wrapping_add(1), core::sync::atomic::Ordering::Relaxed);
                        written += 1;
                    }
                    written
                } else {
                    return errno::EBADF as u64;
                }
            };
            crate::process::keyboard_wake();
            return write_result;
        }
        crate::process::FdType::FsFile { index } => {
            let index = index as usize;
            if index >= crate::process::process::MAX_FILE_HANDLES {
                return errno::EBADF as u64;
            }
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
                let mut child = match Process::new_kernel(pid, 0) {
                    Some(c) => c,
                    None => return errno::ENOMEM as u64,
                };
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
                    // Inherit process group from parent
                    child.pgid = parent_proc.pgid;

                    // CRITICAL: Increment pipe refcounts for inherited pipe FDs.
                    // Without this, when the child closes its pipe FDs, the refcount
                    // underflows or the pipe is freed while the parent still holds a reference.
                    {
                        let mut pipes = crate::process::PIPES.lock();
                        for fd in child.fd_types.iter() {
                            if let crate::process::FdType::Pipe { pipe_idx, .. } = fd {
                                let idx = *pipe_idx as usize;
                                if idx < crate::process::MAX_PIPES {
                                    if let Some(ref mut p) = pipes[idx] {
                                        p.refcount.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
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

    // Close FDs marked close-on-exec before loading.
    // POSIX: only FDs with FD_CLOEXEC flag are closed on exec.
    // FDs 0, 1, 2 are never closed by exec (stdin/stdout/stderr).
    // Collect pipe close operations first, then execute after releasing SCHEDULER
    // to avoid lock ordering issues (SCHEDULER → PIPES).
    let mut cloexec_pipes: alloc::vec::Vec<(usize, bool)> = alloc::vec::Vec::new();
    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                const CLOEXEC_BIT: u8 = 1;
                for i in 3..crate::process::MAX_FDS {
                    if proc.fd_flags[i] & CLOEXEC_BIT == 0 {
                        continue;
                    }
                    let fd_type = proc.fd_types[i];
                    if fd_type == crate::process::FdType::None {
                        continue;
                    }
                    match fd_type {
                        crate::process::FdType::Pipe { pipe_idx, writable } => {
                            let pipe_idx = pipe_idx as usize;
                            if pipe_idx < crate::process::MAX_PIPES {
                                cloexec_pipes.push((pipe_idx, writable));
                            }
                        }
                        crate::process::FdType::FsFile { index } => {
                            let index = index as usize;
                            if index >= crate::process::process::MAX_FILE_HANDLES {
                                continue;
                            }
                            let still_referenced = proc.fd_types.iter().enumerate().any(|(j, f)| {
                                j != i && matches!(f, crate::process::FdType::FsFile { index: idx } if *idx as usize == index)
                            });
                            if !still_referenced {
                                proc.file_handles[index] = None;
                            }
                        }
                        _ => {}
                    }
                    proc.fd_types[i] = crate::process::FdType::None;
                }
            }
        }
    }
    // Execute pipe closes after SCHEDULER is dropped
    if !cloexec_pipes.is_empty() {
        let mut pipes = crate::process::PIPES.lock();
        for (pipe_idx, writable) in cloexec_pipes {
            if let Some(ref mut p) = pipes[pipe_idx] {
                crate::process::pipe::pipe_close(p, writable);
                let old = p.refcount.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
                if old == 1 {
                    pipes[pipe_idx] = None;
                }
            }
        }
    }

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
            if sp.is_null() {
                return errno::EINVAL as u64;
            }
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

    let mut pipe_ops: alloc::vec::Vec<(usize, bool)> = alloc::vec::Vec::new();
    let mut fs_close_indices: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                let fd_type = proc.fd_types[fd as usize];
                match fd_type {
                    crate::process::FdType::None => {
                        return errno::EBADF as u64; // Double-close
                    }
                    crate::process::FdType::Pipe { pipe_idx, writable } => {
                        let pipe_idx = pipe_idx as usize;
                        if pipe_idx < crate::process::MAX_PIPES {
                            pipe_ops.push((pipe_idx, writable));
                        }
                    }
                    crate::process::FdType::FsFile { index } => {
                        let index = index as usize;
                        let still_referenced = proc.fd_types.iter().enumerate().any(|(i, f)| {
                            i != fd as usize
                                && matches!(f, crate::process::FdType::FsFile { index: idx } if *idx as usize == index)
                        });
                        if !still_referenced {
                            fs_close_indices.push(index);
                        }
                    }
                    _ => {}
                }
                proc.fd_types[fd as usize] = crate::process::FdType::None;
                proc.fd_flags[fd as usize] = 0;
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    }
    // Execute pipe closes after SCHEDULER is dropped
    for &(pipe_idx, writable) in &pipe_ops {
        let mut pipes = crate::process::PIPES.lock();
        if let Some(ref mut p) = pipes[pipe_idx] {
            crate::process::pipe::pipe_close(p, writable);
            let old = p.refcount.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
            if old == 1 {
                pipes[pipe_idx] = None;
            }
        }
    }
    // Wake blocked processes
    if !pipe_ops.is_empty() {
        crate::process::keyboard_wake();
    }
    0
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
                if index >= crate::process::process::MAX_FILE_HANDLES {
                    return errno::EBADF as u64;
                }
                if proc.file_handles[index].is_none() {
                    return errno::EBADF as u64;
                }
                // Find free FD slot and point it to the same handle index
                for i in 0..crate::process::MAX_FDS {
                    if proc.fd_types[i] == crate::process::FdType::None {
                        proc.fd_types[i] = crate::process::FdType::FsFile { index: index as u8 };
                        proc.fd_flags[i] = 0; // POSIX: dup clears close-on-exec
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
                if pipe_idx < crate::process::MAX_PIPES {
                    let mut pipes = crate::process::PIPES.lock();
                    if let Some(ref mut p) = pipes[pipe_idx] {
                        p.refcount.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
                    }
                }
            }
            for i in 0..crate::process::MAX_FDS {
                if proc.fd_types[i] == crate::process::FdType::None {
                    proc.fd_types[i] = fd_type;
                    proc.fd_flags[i] = 0; // POSIX: dup clears close-on-exec
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

    let mut dup2_close_pipes: alloc::vec::Vec<(usize, bool)> = alloc::vec::Vec::new();
    let mut dup2_inc_pipes: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    let result = {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                let old_type = proc.fd_types[oldfd as usize];
                if old_type == crate::process::FdType::None {
                    return errno::EBADF as u64;
                }
                if oldfd == newfd {
                    return newfd;
                }
                let new_type = proc.fd_types[newfd as usize];
                if new_type != crate::process::FdType::None {
                    match new_type {
                        crate::process::FdType::Pipe { pipe_idx, writable } => {
                            let pipe_idx = pipe_idx as usize;
                            if pipe_idx < crate::process::MAX_PIPES {
                                dup2_close_pipes.push((pipe_idx, writable));
                            }
                        }
                        crate::process::FdType::FsFile { index } => {
                            let index = index as usize;
                            if index < crate::process::process::MAX_FILE_HANDLES {
                                let still_referenced = proc.fd_types.iter().enumerate().any(|(i, f)| {
                                    i != newfd as usize
                                        && matches!(f, crate::process::FdType::FsFile { index: idx } if *idx as usize == index)
                                });
                                if !still_referenced {
                                    proc.file_handles[index] = None;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let crate::process::FdType::Pipe { pipe_idx, writable: _ } = old_type {
                    let pipe_idx = pipe_idx as usize;
                    if pipe_idx < crate::process::MAX_PIPES {
                        dup2_inc_pipes.push(pipe_idx);
                    }
                }
                proc.fd_types[newfd as usize] = old_type;
                proc.fd_flags[newfd as usize] = 0;
                newfd
            } else {
                errno::ESRCH as u64
            }
        } else {
            errno::ESRCH as u64
        }
    };
    if (result as i64) < 0 {
        return result;
    }
    for &(pipe_idx, writable) in &dup2_close_pipes {
        let mut pipes = crate::process::PIPES.lock();
        if let Some(ref mut p) = pipes[pipe_idx] {
            crate::process::pipe::pipe_close(p, writable);
            let old_ref = p.refcount.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
            if old_ref == 1 {
                pipes[pipe_idx] = None;
            }
        }
    }
    for &pipe_idx in &dup2_inc_pipes {
        let mut pipes = crate::process::PIPES.lock();
        if let Some(ref mut p) = pipes[pipe_idx] {
            p.refcount.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        }
    }
    if !dup2_close_pipes.is_empty() {
        crate::process::keyboard_wake();
    }
    result
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
/// Arguments: path_ptr (user pointer to null-terminated path string), flags
/// Returns: fd number on success, or negative errno on error
///
/// Flags (POSIX-compatible):
///   0x0000 = O_RDONLY (default)
///   0x0001 = O_WRONLY
///   0x0002 = O_RDWR
///   0x0040 = O_CREAT (create file if it doesn't exist)
///   0x0200 = O_TRUNC (truncate to zero length)
fn sys_open(path_ptr: u64, flags: u64) -> u64 {
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

    // Validate flags: must have exactly one of O_RDONLY, O_WRONLY, O_RDWR
    let access_mode = flags & 0x3; // Lower 2 bits
    if access_mode == 0x3 || access_mode == 0 {
        return errno::EINVAL as u64; // Invalid or missing access mode
    }
    // Reject unknown high bits
    const VALID_FLAGS: u64 = 0x0000_00FF; // O_RDONLY|O_WRONLY|O_RDWR|O_CREAT|O_TRUNC|O_APPEND|O_CLOEXEC
    if flags & !VALID_FLAGS != 0 {
        return errno::EINVAL as u64;
    }

    // Open the file via VFS, creating if O_CREAT is set
    let vfs = crate::vfs::vfs();
    let mut file = if flags & crate::process::process::O_CREAT != 0 {
        // O_CREAT: create file if it doesn't exist
        match vfs.open(&path) {
            Ok(f) => {
                if flags & crate::process::process::O_TRUNC != 0 {
                    // O_TRUNC: truncate existing file — create new empty file
                    drop(f);
                    match vfs.create_file(&path) {
                        Ok(f) => f,
                        Err(e) => return e.to_errno() as u64,
                    }
                } else {
                    f
                }
            }
            Err(_) => {
                // File doesn't exist — create it
                match vfs.create_file(&path) {
                    Ok(f) => f,
                    Err(e) => return e.to_errno() as u64,
                }
            }
        }
    } else {
        match vfs.open(&path) {
            Ok(f) => {
                if flags & crate::process::process::O_TRUNC != 0 {
                    // O_TRUNC on existing file — truncate
                    drop(f);
                    match vfs.create_file(&path) {
                        Ok(f) => f,
                        Err(e) => return e.to_errno() as u64,
                    }
                } else {
                    f
                }
            }
            Err(e) => return e.to_errno() as u64,
        }
    };

    // O_APPEND: seek to end of file so writes append
    if flags & crate::process::process::O_APPEND != 0 {
        // Get file size from VFS inode
        if let Ok(inode) = vfs.resolve(&path) {
            let size = inode.size();
            let _ = file.seek(size);
        }
    }

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
                    // Set close-on-exec flag if O_CLOEXEC was in the flags
                    const CLOEXEC_BIT: u8 = 1;
                    if flags & crate::process::process::O_CLOEXEC != 0 {
                        proc.fd_flags[fd] |= CLOEXEC_BIT;
                    }
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
                    if index >= crate::process::process::MAX_FILE_HANDLES {
                        return errno::EBADF as u64;
                    }
                    if let Some(ref file_handle) = proc.file_handles[index] {
                        let mut file = file_handle.lock();
                        match file.seek(offset) {
                            Ok(()) => offset, // Return new position
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

/// SYS_UNLINK (16) — Delete a file by path.
///
/// Arguments: path_ptr (user pointer to null-terminated path string)
/// Returns: 0 on success, or negative errno on error.
///
/// Currently limited to ramfs files. FAT filesystem is read-only.
fn sys_unlink(path_ptr: u64) -> u64 {
    use alloc::string::String;

    if path_ptr == 0 {
        return errno::EFAULT as u64;
    }

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

    match crate::vfs::vfs().delete_file(&path) {
        Ok(()) => 0,
        Err(e) => e.to_errno() as u64,
    }
}

/// SYS_BRK (17) — Change the data segment size (heap).
///
/// Arguments: new_brk (0 = query current, >0 = set new break)
/// Returns: new break address on success, or negative errno on error.
fn sys_brk(new_brk: u64) -> u64 {
    use crate::memory::{self, vmm, PhysAddr, PAGE_SIZE};
    use x86_64::VirtAddr;

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            // Initialize heap_start on first call
            if proc.heap_start == 0 {
                proc.heap_start = crate::memory::USER_HEAP_BASE;
                proc.heap_end = crate::memory::USER_HEAP_BASE;
            }

            if new_brk == 0 {
                // Query: return current heap end
                return proc.heap_end;
            }

            if new_brk < proc.heap_start {
                // Below heap start — invalid
                return proc.heap_end;
            }

            let old_end = proc.heap_end;
            let new_end = new_brk;

            if new_end > old_end {
                // Growing: map new pages
                let pml4 = proc.pml4_phys;
                let mut addr = old_end;
                while addr < new_end {
                    let page_virt = VirtAddr::new(addr);
                    // Check if already mapped
                    if vmm::translate_addr(PhysAddr::new(pml4), page_virt).is_none() {
                        // Allocate a new physical frame and map it
                        let frame = crate::memory::pmm::alloc_frame();
                        if let Some(frame) = frame {
                            vmm::map_page(
                                PhysAddr::new(pml4),
                                page_virt,
                                frame,
                                x86_64::structures::paging::PageTableFlags::PRESENT
                                    | x86_64::structures::paging::PageTableFlags::WRITABLE
                                    | x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE,
                            );
                        } else {
                            // OOM: can't grow
                            break;
                        }
                    }
                    addr += PAGE_SIZE;
                }
                proc.heap_end = addr;
            } else if new_end < old_end {
                // Shrinking: unmap pages
                let pml4 = proc.pml4_phys;
                let mut addr = new_end;
                while addr < old_end {
                    let page_virt = VirtAddr::new(addr);
                    // Unmap and free the physical frame
                    if let Some(phys) = vmm::translate_addr(PhysAddr::new(pml4), page_virt) {
                        vmm::unmap_page(PhysAddr::new(pml4), page_virt);
                        crate::memory::pmm::free_frame(phys);
                    }
                    addr += PAGE_SIZE;
                }
                proc.heap_end = new_end;
            }

            return proc.heap_end;
        }
    }
    errno::ESRCH as u64
}

/// SYS_EXECVE (18) — Replace process with new ELF, passing argc/argv.
///
/// Arguments: path_ptr, argc, argv_ptr
/// Returns: 0 on success, or negative errno on error.
///
/// argv_ptr points to an array of `argc` u64 user pointers, each pointing
/// to a null-terminated string. After exec, the new process receives
/// argc in RDI and a pointer to the argv array in RSI.
fn sys_execve(path_ptr: u64, argc: u64, argv_ptr: u64) -> u64 {
    use alloc::string::String;
    use alloc::vec::Vec;
    use crate::memory::{self, vmm, PhysAddr};

    if path_ptr == 0 {
        return errno::EFAULT as u64;
    }

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

    // Read the path string from user space
    let path = {
        let mut buf = Vec::new();
        let user_ptr = path_ptr as *const u8;
        for i in 0..4096u64 {
            if !is_valid_user_range(user_ptr as u64 + i, 1) {
                return errno::EFAULT as u64;
            }
            if !is_user_buffer_mapped(pml4, user_ptr as u64 + i, 1) {
                return errno::EFAULT as u64;
            }
            let byte = unsafe { *user_ptr.add(i as usize) };
            if byte == 0 { break; }
            buf.push(byte);
        }
        String::from_utf8(buf).unwrap_or_default()
    };

    if path.is_empty() {
        return errno::EINVAL as u64;
    }

    // Read argv strings from user space (before replacing address space)
    let argv_strings: Vec<String> = if argc > 0 && argv_ptr != 0 {
        let mut strings = Vec::new();
        let argv_array = argv_ptr as *const u64;
        for i in 0..argc.min(64) {
            if !is_valid_user_range(argv_ptr + i * 8, 8) {
                break;
            }
            if !is_user_buffer_mapped(pml4, argv_ptr + i * 8, 8) {
                break;
            }
            let str_ptr = unsafe { *argv_array.add(i as usize) };
            if str_ptr == 0 {
                strings.push(String::new());
                continue;
            }
            let mut sbuf = Vec::new();
            for j in 0..4096u64 {
                if !is_valid_user_range(str_ptr + j, 1) { break; }
                if !is_user_buffer_mapped(pml4, str_ptr + j, 1) { break; }
                let byte = unsafe { *((str_ptr + j) as *const u8) };
                if byte == 0 { break; }
                sbuf.push(byte);
            }
            strings.push(String::from_utf8(sbuf).unwrap_or_default());
        }
        strings
    } else {
        Vec::new()
    };

    crate::serial::write_str("[SYSCALL] execve: ");
    crate::serial::write_str(&path);
    crate::serial::write_str(" argc=");
    crate::serial::write_u64(argc);
    crate::serial::write_nl();

    // Close FDs marked close-on-exec
    let mut cloexec_pipes: alloc::vec::Vec<(usize, bool)> = alloc::vec::Vec::new();
    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                const CLOEXEC_BIT: u8 = 1;
                for i in 3..crate::process::MAX_FDS {
                    if proc.fd_flags[i] & CLOEXEC_BIT == 0 { continue; }
                    let fd_type = proc.fd_types[i];
                    if fd_type == crate::process::FdType::None { continue; }
                    match fd_type {
                        crate::process::FdType::Pipe { pipe_idx, writable } => {
                            let pipe_idx = pipe_idx as usize;
                            if pipe_idx < crate::process::MAX_PIPES {
                                cloexec_pipes.push((pipe_idx, writable));
                            }
                        }
                        crate::process::FdType::FsFile { index } => {
                            let index = index as usize;
                            if index >= crate::process::process::MAX_FILE_HANDLES { continue; }
                            let still_referenced = proc.fd_types.iter().enumerate().any(|(j, f)| {
                                j != i && matches!(f, crate::process::FdType::FsFile { index: idx } if *idx as usize == index)
                            });
                            if !still_referenced {
                                proc.file_handles[index] = None;
                            }
                        }
                        _ => {}
                    }
                    proc.fd_types[i] = crate::process::FdType::None;
                }
            }
        }
    }
    if !cloexec_pipes.is_empty() {
        let mut pipes = crate::process::PIPES.lock();
        for (pipe_idx, writable) in cloexec_pipes {
            if let Some(ref mut p) = pipes[pipe_idx] {
                crate::process::pipe::pipe_close(p, writable);
                let old = p.refcount.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
                if old == 1 { pipes[pipe_idx] = None; }
            }
        }
    }

    // Read the ELF from VFS
    let elf_data = match crate::vfs::vfs().read_file(&path) {
        Ok(data) => data,
        Err(e) => return e.to_errno() as u64,
    };
    if elf_data.is_empty() {
        return errno::ENOENT as u64;
    }

    let (current_pid, old_pml4_phys) = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                (pid, proc.pml4_phys)
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };

    let kernel_pml4 = memory::kernel_pml4_phys();
    let new_pml4 = vmm::create_user_pml4(PhysAddr::new(kernel_pml4));

    let elf_image = match crate::elf::load_elf(&elf_data, new_pml4) {
        Ok(img) => img,
        Err(e) => {
            crate::serial::write_str("[SYSCALL] execve: ELF load failed: ");
            crate::serial::write_str(e.description());
            crate::serial::write_nl();
            unsafe { vmm::free_user_address_space(new_pml4); }
            return errno::ENOEXEC as u64;
        }
    };

    let user_stack_top = crate::memory::USER_STACK_TOP;
    let user_stack_bottom = user_stack_top - 4 * crate::memory::PAGE_SIZE;

    // Map stack pages
    for i in 0..4u64 {
        let page_virt = x86_64::VirtAddr::new(user_stack_bottom + i * crate::memory::PAGE_SIZE);
        let frame = match vmm::PmmFrameAllocator.allocate_frame() {
            Some(f) => f,
            None => { unsafe { vmm::free_user_address_space(new_pml4); } return errno::ENOMEM as u64; }
        };
        vmm::map_page(new_pml4, page_virt, PhysAddr::new(frame.start_address().as_u64()),
            x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE);
        let frame_ptr = unsafe { vmm::phys_to_virt(frame.start_address().as_u64()).as_mut_ptr::<u8>() };
        unsafe { core::ptr::write_bytes(frame_ptr, 0, 4096); }
    }

    // Map guard page
    let guard_virt = x86_64::VirtAddr::new(user_stack_bottom - crate::memory::PAGE_SIZE);
    let guard_frame = match vmm::PmmFrameAllocator.allocate_frame() {
        Some(f) => f,
        None => { unsafe { vmm::free_user_address_space(new_pml4); } return errno::ENOMEM as u64; }
    };
    vmm::map_page(new_pml4, guard_virt, PhysAddr::new(guard_frame.start_address().as_u64()),
        x86_64::structures::paging::PageTableFlags::PRESENT);

    // Free old address space
    unsafe { vmm::free_user_address_space(PhysAddr::new(old_pml4_phys)); }

    // Set up user stack with argc/argv data
    let mut sp = user_stack_top - 8; // 16-byte aligned before CALL

    // Write argv strings and pointers to user stack
    // Layout (high to low):
    //   argv string data
    //   argv pointers (u64)
    //   NULL terminator
    //   argc (u64) <-- RSP
    let actual_argc = argv_strings.len() as u64;

    // First, write all strings at the top of the usable stack area
    let mut string_offsets: Vec<u64> = Vec::new();
    let string_data_start = sp - 4096; // Use the page below RSP for string data
    let mut string_pos = string_data_start;

    // Safety check: total argv string data must not exceed 3072 bytes (75% of page)
    // to leave room for argv pointers, argc, and alignment
    let total_string_bytes: usize = argv_strings.iter().map(|s| s.len() + 1).sum();
    if total_string_bytes > 3072 {
        return errno::E2BIG as u64;
    }

    for s in &argv_strings {
        string_offsets.push(string_pos);
        let bytes = s.as_bytes();
        for (j, &b) in bytes.iter().enumerate() {
            let addr = string_pos + j as u64;
            if let Some(phys) = vmm::translate_addr(new_pml4, x86_64::VirtAddr::new(addr)) {
                let ptr = unsafe { vmm::phys_to_virt(phys.as_u64()).as_mut_ptr::<u8>() };
                unsafe { *ptr = b; }
            }
        }
        // Null terminator
        let null_addr = string_pos + bytes.len() as u64;
        if let Some(phys) = vmm::translate_addr(new_pml4, x86_64::VirtAddr::new(null_addr)) {
            let ptr = unsafe { vmm::phys_to_virt(phys.as_u64()).as_mut_ptr::<u8>() };
            unsafe { *ptr = 0; }
        }
        string_pos += (bytes.len() + 1) as u64;
    }

    // Write NULL terminator for argv
    sp -= 8;
    if let Some(phys) = vmm::translate_addr(new_pml4, x86_64::VirtAddr::new(sp)) {
        let ptr = unsafe { vmm::phys_to_virt(phys.as_u64()).as_mut_ptr::<u64>() };
        unsafe { ptr.write(0); }
    }

    // Write argv pointers (reversed order since stack grows down)
    for i in (0..argv_strings.len()).rev() {
        sp -= 8;
        let ptr_val = string_offsets[i];
        if let Some(phys) = vmm::translate_addr(new_pml4, x86_64::VirtAddr::new(sp)) {
            let ptr = unsafe { vmm::phys_to_virt(phys.as_u64()).as_mut_ptr::<u64>() };
            unsafe { ptr.write(ptr_val); }
        }
    }
    let argv_user_ptr = sp; // User-space pointer to the argv array

    // Write argc
    sp -= 8;
    if let Some(phys) = vmm::translate_addr(new_pml4, x86_64::VirtAddr::new(sp)) {
        let ptr = unsafe { vmm::phys_to_virt(phys.as_u64()).as_mut_ptr::<u64>() };
        unsafe { ptr.write(actual_argc); }
    }

    let user_rip = elf_image.entry;
    let user_rsp = sp; // RSP points to argc

    // Update the process
    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(ref mut proc) = sched.processes_mut()[current_pid as usize] {
            proc.pml4_phys = new_pml4.as_u64();
            proc.user_rip = Some(user_rip);
            proc.user_rsp = Some(user_rsp);
            proc.is_user = true;
            // Initialize heap at USER_HEAP_BASE (brk(0) will return this until first brk call)
            proc.heap_start = crate::memory::USER_HEAP_BASE;
            proc.heap_end = crate::memory::USER_HEAP_BASE;

            let sp_ptr = proc.stack_pointer as *mut u64;
            if sp_ptr.is_null() {
                return errno::EINVAL as u64;
            }
            unsafe {
                sp_ptr.add(15).write(user_rip);  // RIP
                sp_ptr.add(16).write(crate::gdt::user_code_selector().0 as u64); // CS
                sp_ptr.add(17).write(0x202u64);  // RFLAGS (IF=1)
                sp_ptr.add(18).write(user_rsp);   // RSP
                sp_ptr.add(19).write(crate::gdt::user_data_selector().0 as u64); // SS
                // Set argc (RDI) and argv pointer (RSI) for _start
                sp_ptr.add(5).write(actual_argc); // RDI = argc
                sp_ptr.add(4).write(argv_user_ptr); // RSI = argv pointer
            }
        }
    }

    crate::serial::write_str("[SYSCALL] execve: entry=");
    crate::serial::write_hex(user_rip);
    crate::serial::write_str(" rsp=");
    crate::serial::write_hex(user_rsp);
    crate::serial::write_str(" argc=");
    crate::serial::write_u64(actual_argc);
    crate::serial::write_nl();

    0
}

/// SYS_CHDIR (19) — Change the current working directory.
///
/// Arguments: path_ptr (user pointer to null-terminated path string)
/// Returns: 0 on success, or negative errno on error.
fn sys_chdir(path_ptr: u64) -> u64 {
    use alloc::string::String;
    use alloc::vec::Vec;

    if path_ptr == 0 {
        return errno::EFAULT as u64;
    }

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

    let path = {
        let mut buf = Vec::new();
        let user_ptr = path_ptr as *const u8;
        for i in 0..4096u64 {
            if !is_valid_user_range(user_ptr as u64 + i, 1) { return errno::EFAULT as u64; }
            if !is_user_buffer_mapped(pml4, user_ptr as u64 + i, 1) { return errno::EFAULT as u64; }
            let byte = unsafe { *user_ptr.add(i as usize) };
            if byte == 0 { break; }
            buf.push(byte);
        }
        String::from_utf8(buf).unwrap_or_default()
    };

    if path.is_empty() {
        return errno::EINVAL as u64;
    }

    // Resolve the path against CWD if relative
    let resolved = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                resolve_cwd(proc.cwd_str(), &path)
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };

    // Verify the path exists and is a directory
    match crate::vfs::vfs().resolve(&resolved) {
        Ok(inode) => {
            if !inode.is_dir() {
                return errno::ENOTDIR as u64;
            }
        }
        Err(e) => return e.to_errno() as u64,
    }

    // Set the CWD
    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            proc.set_cwd(&resolved);
        }
    }

    0
}

/// SYS_GETCWD (20) — Get the current working directory.
///
/// Arguments: buf_ptr (user buffer), buf_size (buffer size)
/// Returns: number of bytes written (excluding null), or negative errno on error.
fn sys_getcwd(buf_ptr: u64, buf_size: u64) -> u64 {
    if buf_ptr == 0 || buf_size == 0 {
        return errno::EINVAL as u64;
    }

    if !is_valid_user_range(buf_ptr, buf_size) {
        return errno::EFAULT as u64;
    }

    let cwd = {
        let sched = crate::process::scheduler::SCHEDULER.lock();
        if let Some(pid) = sched.current_pid() {
            if let Some(ref proc) = sched.processes()[pid as usize] {
                let len = proc.cwd.iter().position(|&b| b == 0).unwrap_or(256);
                let mut result = alloc::vec![0u8; len + 1];
                result[..len].copy_from_slice(&proc.cwd[..len]);
                result[len] = 0;
                result
            } else {
                return errno::ESRCH as u64;
            }
        } else {
            return errno::ESRCH as u64;
        }
    };

    let copy_len = core::cmp::min(cwd.len() as u64, buf_size);
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
    if !is_user_buffer_mapped(pml4, buf_ptr, copy_len) {
        return errno::EFAULT as u64;
    }

    unsafe {
        core::ptr::copy_nonoverlapping(cwd.as_ptr(), buf_ptr as *mut u8, copy_len as usize);
    }

    // Return the length of the string (excluding null terminator) per POSIX getcwd
    let len = cwd.iter().position(|&b| b == 0).unwrap_or(cwd.len());
    len as u64
}

/// SYS_MKDIR (21) — Create a directory.
///
/// Arguments: path_ptr (user pointer to null-terminated path string)
/// Returns: 0 on success, or negative errno on error.
fn sys_mkdir(path_ptr: u64) -> u64 {
    use alloc::string::String;
    use alloc::vec::Vec;

    if path_ptr == 0 {
        return errno::EFAULT as u64;
    }

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

    let path = {
        let mut buf = Vec::new();
        let user_ptr = path_ptr as *const u8;
        for i in 0..4096u64 {
            if !is_valid_user_range(user_ptr as u64 + i, 1) { return errno::EFAULT as u64; }
            if !is_user_buffer_mapped(pml4, user_ptr as u64 + i, 1) { return errno::EFAULT as u64; }
            let byte = unsafe { *user_ptr.add(i as usize) };
            if byte == 0 { break; }
            buf.push(byte);
        }
        String::from_utf8(buf).unwrap_or_default()
    };

    if path.is_empty() {
        return errno::EINVAL as u64;
    }

    match crate::vfs::vfs().create_dir(&path) {
        Ok(()) => 0,
        Err(e) => e.to_errno() as u64,
    }
}

/// SYS_MMAP (22) — Map anonymous memory.
///
/// Arguments: addr (hint), length
/// Returns: mapped address on success, or negative errno on error.
///
/// For now, only supports MAP_ANONYMOUS | MAP_PRIVATE (no file backing).
/// The `addr` parameter is a hint; the kernel ignores it and picks an address.
fn sys_mmap(addr: u64, length: u64) -> u64 {
    use crate::memory::{self, vmm, PhysAddr, PAGE_SIZE};
    use x86_64::VirtAddr;

    if length == 0 {
        return errno::EINVAL as u64;
    }

    // Round up to page boundary
    let num_pages = (length + PAGE_SIZE - 1) / PAGE_SIZE;

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            if proc.pml4_phys == 0 {
                return errno::ESRCH as u64;
            }

            if proc.mmap_count as usize >= crate::process::process::MAX_MMAP_REGIONS {
                return errno::ENOMEM as u64;
            }

            // Find the next free virtual address region
            // Start from USER_MMAP_BASE and scan forward
            let mut base = crate::memory::USER_MMAP_BASE;
            let pml4 = proc.pml4_phys;
            'outer: loop {
                // Check if this region overlaps with any existing mmap
                let mut overlap = false;
                for i in 0..proc.mmap_count as usize {
                    let (existing_base, existing_pages) = proc.mmap_regions[i];
                    let existing_end = existing_base + existing_pages * PAGE_SIZE;
                    if base < existing_end && base + num_pages * PAGE_SIZE > existing_base {
                        base = existing_end;
                        overlap = true;
                        break;
                    }
                }
                if overlap { continue 'outer; }

                // Also check against stack (leave a guard page)
                let stack_bottom = crate::memory::USER_STACK_TOP - 5 * PAGE_SIZE;
                if base + num_pages * PAGE_SIZE > stack_bottom {
                    return errno::ENOMEM as u64;
                }

                // Check if all pages in the region are unmapped
                for i in 0..num_pages {
                    let page_virt = VirtAddr::new(base + i * PAGE_SIZE);
                    if vmm::translate_addr(PhysAddr::new(pml4), page_virt).is_some() {
                        base = base + (i + 1) * PAGE_SIZE;
                        continue 'outer;
                    }
                }
                break;
            }

            // Map the pages
            for i in 0..num_pages {
                let page_virt = VirtAddr::new(base + i * PAGE_SIZE);
                let frame = crate::memory::pmm::alloc_frame();
                if let Some(frame) = frame {
                    vmm::map_page(
                        PhysAddr::new(pml4),
                        page_virt,
                        frame,
                        x86_64::structures::paging::PageTableFlags::PRESENT
                            | x86_64::structures::paging::PageTableFlags::WRITABLE
                            | x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE,
                    );
                } else {
                    // OOM: unmap already-mapped pages
                    for j in 0..i {
                        let page_virt = VirtAddr::new(base + j * PAGE_SIZE);
                        if let Some(phys) = vmm::translate_addr(PhysAddr::new(pml4), page_virt) {
                            vmm::unmap_page(PhysAddr::new(pml4), page_virt);
                            crate::memory::pmm::free_frame(phys);
                        }
                    }
                    return errno::ENOMEM as u64;
                }
            }

            // Track the region
            proc.mmap_regions[proc.mmap_count as usize] = (base, num_pages);
            proc.mmap_count += 1;

            return base;
        }
    }
    errno::ESRCH as u64
}

/// SYS_MUNMAP (23) — Unmap anonymous memory.
///
/// Arguments: addr, length
/// Returns: 0 on success, or negative errno on error.
fn sys_munmap(addr: u64, length: u64) -> u64 {
    use crate::memory::{self, vmm, PhysAddr, PAGE_SIZE};
    use x86_64::VirtAddr;

    if length == 0 || addr == 0 {
        return errno::EINVAL as u64;
    }

    let num_pages = (length + PAGE_SIZE - 1) / PAGE_SIZE;
    let end = addr + num_pages * PAGE_SIZE;

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    if let Some(pid) = sched.current_pid() {
        if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
            let pml4 = proc.pml4_phys;

            // Find and remove the mmap region
            let mut found = false;
            for i in 0..proc.mmap_count as usize {
                let (region_base, region_pages) = proc.mmap_regions[i];
                let region_end = region_base + region_pages * PAGE_SIZE;
                if addr >= region_base && end <= region_end {
                    // Found it — unmap the pages
                    for j in 0..region_pages {
                        let page_virt = VirtAddr::new(region_base + j * PAGE_SIZE);
                        if let Some(phys) = vmm::translate_addr(PhysAddr::new(pml4), page_virt) {
                            vmm::unmap_page(PhysAddr::new(pml4), page_virt);
                            crate::memory::pmm::free_frame(phys);
                        }
                    }
                    // Remove from tracking array
                    for j in i..(proc.mmap_count as usize - 1) {
                        proc.mmap_regions[j] = proc.mmap_regions[j + 1];
                    }
                    proc.mmap_regions[proc.mmap_count as usize - 1] = (0, 0);
                    proc.mmap_count -= 1;
                    found = true;
                    break;
                }
            }

            if found { 0 } else { errno::EINVAL as u64 }
        } else {
            errno::ESRCH as u64
        }
    } else {
        errno::ESRCH as u64
    }
}

/// SYS_TCGETATTR (24) — Get terminal attributes.
///
/// Arguments: fd, termios_ptr
/// Returns: 0 on success, or negative errno.
fn sys_tcgetattr(fd: u64, termios_ptr: u64) -> u64 {
    if fd > 2 || termios_ptr == 0 {
        return errno::EINVAL as u64;
    }

    let termios = crate::tty::Termios::default_cooked();

    // Write termios to user buffer
    let dst = termios_ptr as *mut crate::tty::Termios;
    if !is_valid_user_range(termios_ptr, core::mem::size_of::<crate::tty::Termios>() as u64) {
        return errno::EFAULT as u64;
    }
    unsafe { core::ptr::write_volatile(dst, termios); }
    0
}

/// SYS_TCSETATTR (25) — Set terminal attributes.
///
/// Arguments: fd, optional_actions, termios_ptr
/// Returns: 0 on success, or negative errno.
fn sys_tcsetattr(fd: u64, _optional_actions: u64, termios_ptr: u64) -> u64 {
    if fd > 2 || termios_ptr == 0 {
        return errno::EINVAL as u64;
    }

    let src = termios_ptr as *const crate::tty::Termios;
    if !is_valid_user_range(termios_ptr, core::mem::size_of::<crate::tty::Termios>() as u64) {
        return errno::EFAULT as u64;
    }
    let termios = unsafe { core::ptr::read_volatile(src) };

    crate::tty::tty_apply_termios(&termios);
    0
}

/// SYS_SIGACTION (26) — Register a signal handler.
///
/// Arguments: signum, handler_ptr (user function addr), old_handler_ptr (out)
/// Returns: 0 on success, or negative errno.
fn sys_sigaction(signum: u64, handler_ptr: u64, old_handler_ptr: u64) -> u64 {
    if signum == 0 || signum > 31 {
        return errno::EINVAL as u64;
    }

    let sig_idx = signum as usize - 1;

    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    let pid = sched.current_pid().unwrap_or(0) as usize;
    if let Some(Some(ref mut proc)) = sched.processes_mut().get_mut(pid) {
        // Return old handler if requested
        if old_handler_ptr != 0 {
            let old_val = proc.signal_handlers[sig_idx];
            let dst = old_handler_ptr as *mut u64;
            if is_valid_user_range(old_handler_ptr, 8) {
                unsafe { core::ptr::write_volatile(dst, old_val); }
            }
        }
        // Set new handler
        proc.signal_handlers[sig_idx] = handler_ptr;
        0
    } else {
        errno::ESRCH as u64
    }
}

/// SYS_KILL (27) — Send a signal to a process.
///
/// Arguments: pid, signum
/// Returns: 0 on success, or negative errno.
fn sys_kill(pid: u64, signum: u64) -> u64 {
    if signum == 0 || signum > 31 {
        return errno::EINVAL as u64;
    }

    let target = pid as usize;
    let mut sched = crate::process::scheduler::SCHEDULER.lock();

    if let Some(Some(ref mut proc)) = sched.processes_mut().get_mut(target) {
        if proc.state == crate::process::process::ProcessState::Zombie {
            return errno::ESRCH as u64;
        }
        proc.send_signal(signum as u8);
        0
    } else {
        errno::ESRCH as u64
    }
}

/// SYS_SETPGID (28) — Set the process group ID of a process.
///
/// Arguments: pid, pgid
/// If pid == 0, use current process. If pgid == 0, create new group (PGID = PID).
/// Returns: 0 on success, or negative errno.
fn sys_setpgid(pid: u64, pgid: u64) -> u64 {
    let mut sched = crate::process::scheduler::SCHEDULER.lock();
    let target_pid = if pid == 0 {
        sched.current_pid().unwrap_or(0)
    } else {
        pid
    };

    let new_pgid = if pgid == 0 {
        target_pid
    } else {
        pgid
    };

    if let Some(Some(ref mut proc)) = sched.processes_mut().get_mut(target_pid as usize) {
        proc.pgid = new_pgid;
        0
    } else {
        errno::ESRCH as u64
    }
}

/// SYS_GETPGID (29) — Get the process group ID of a process.
///
/// Arguments: pid (0 = current process)
/// Returns: PGID, or negative errno.
fn sys_getpgid(pid: u64) -> u64 {
    let sched = crate::process::scheduler::SCHEDULER.lock();
    let target = if pid == 0 {
        sched.current_pid().unwrap_or(0)
    } else {
        pid
    };

    if let Some(Some(ref proc)) = sched.processes().get(target as usize) {
        proc.pgid
    } else {
        errno::ESRCH as u64
    }
}

/// Resolve a relative path against a CWD.
///
/// Handles ".", "..", and multiple slashes. Returns an absolute path.
fn resolve_cwd(cwd: &str, path: &str) -> alloc::string::String {
    use alloc::string::String;
    use alloc::vec::Vec;

    // If path starts with '/', it's absolute
    if path.starts_with('/') {
        return normalize_path(path);
    }

    // Relative path: start from CWD
    let mut parts: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    for part in path.split('/').filter(|s| !s.is_empty()) {
        match part {
            "." => {}
            ".." => { parts.pop(); }
            other => parts.push(other),
        }
    }

    let mut result = String::from("/");
    for (i, part) in parts.iter().enumerate() {
        if i > 0 { result.push('/'); }
        result.push_str(part);
    }
    if result.is_empty() { result.push('/'); }
    result
}

/// Normalize a path by resolving ".", ".." and collapsing multiple slashes.
fn normalize_path(path: &str) -> alloc::string::String {
    use alloc::string::String;
    use alloc::vec::Vec;

    let mut parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut resolved = Vec::new();
    for part in parts.drain(..) {
        match part {
            "." => {}
            ".." => { resolved.pop(); }
            other => resolved.push(other),
        }
    }

    let mut result = String::from("/");
    for (i, part) in resolved.iter().enumerate() {
        if i > 0 { result.push('/'); }
        result.push_str(part);
    }
    if result.is_empty() { result.push('/'); }
    result
}
