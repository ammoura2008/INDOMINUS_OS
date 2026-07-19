//! # Process Structure
//!
//! Defines the process abstraction for INDOMINUS.
//!
//! ## Ring 0 vs Ring 3 processes
//!
//! - **Kernel-mode (Ring 0) processes:** run entirely in kernel space. The initial
//!   IRET frame has 3 entries (RIP, CS, RFLAGS). Used for kernel tasks.
//! - **User-mode (Ring 3) processes:** run user code in the lower half. The initial
//!   IRET frame has 5 entries (RIP, CS, RFLAGS, RSP, SS) — the CPU switches to
//!   Ring 3 automatically when executing `iretq`.

use crate::memory::vmm;

/// Maximum number of concurrent processes.
pub const MAX_PROCESSES: usize = 8;

/// Size of each process's kernel stack (8 KiB = 2 pages).
/// Must be large enough for syscall/interrupt handling.
pub const KERNEL_STACK_SIZE: usize = 8192;

/// Process identifier.
pub type Pid = u64;

/// Process states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Zombie,
}

/// A kernel-mode or user-mode process.
pub struct Process {
    pub pid: Pid,
    pub state: ProcessState,
    /// Current stack pointer — points to saved registers on the kernel stack.
    pub stack_pointer: u64,
    /// Pointer to the allocated kernel stack base (for deallocation).
    pub kernel_stack_base: u64,
    /// Per-process page table (phys address). Kernel processes share the boot PML4.
    pub pml4_phys: u64,
    /// User-mode entry point (virtual address in user space). None for kernel processes.
    pub user_rip: Option<u64>,
    /// User-mode initial stack pointer. None for kernel processes.
    pub user_rsp: Option<u64>,
    /// Kernel-mode entry function address (raw physical). Used for diagnostics.
    pub entry_addr: u64,
    pub exit_code: u64,
    /// Whether this is a user-mode process.
    pub is_user: bool,
}

impl Process {
    /// Create a new kernel-mode process (Ring 0).
    ///
    /// `entry_phys` is the physical address of the entry function
    /// (obtained by casting the fn pointer to u64).
    pub fn new_kernel(pid: Pid, entry_phys: u64) -> Self {
        let stack_base = alloc_kernel_stack();
        let stack_top = stack_base + KERNEL_STACK_SIZE as u64;
        let sp = setup_initial_stack_frame_kernel(stack_top, entry_phys);

        // Use the boot page tables (kernel processes share the kernel's PML4)
        let pml4 = unsafe {
            let (frame, _flags) = x86_64::registers::control::Cr3::read();
            frame.start_address().as_u64()
        };

        Process {
            pid,
            state: ProcessState::Ready,
            stack_pointer: sp,
            kernel_stack_base: stack_base,
            pml4_phys: pml4,
            user_rip: None,
            user_rsp: None,
            entry_addr: entry_phys,
            exit_code: 0,
            is_user: false,
        }
    }

    /// Create a new user-mode process (Ring 3).
    ///
    /// - `user_rip`: virtual address of the user entry point
    /// - `user_rsp`: initial user stack pointer
    /// - `pml4`: per-process page table (from `create_user_pml4`)
    pub fn new_user(pid: Pid, user_rip: u64, user_rsp: u64, pml4: u64) -> Self {
        let stack_base = alloc_kernel_stack();
        let stack_top = stack_base + KERNEL_STACK_SIZE as u64;
        let sp = setup_initial_stack_frame_user(stack_top, user_rip, user_rsp);

        Process {
            pid,
            state: ProcessState::Ready,
            stack_pointer: sp,
            kernel_stack_base: stack_base,
            pml4_phys: pml4,
            user_rip: Some(user_rip),
            user_rsp: Some(user_rsp),
            entry_addr: 0,
            exit_code: 0,
            is_user: true,
        }
    }
}

/// Allocate a kernel stack from the heap and zero it.
fn alloc_kernel_stack() -> u64 {
    let layout = core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE, 16)
        .expect("Invalid kernel stack layout");
    unsafe {
        let ptr = alloc::alloc::alloc(layout);
        if ptr.is_null() {
            panic!("Process: out of memory for kernel stack");
        }
        core::ptr::write_bytes(ptr, 0, KERNEL_STACK_SIZE);
        ptr as u64
    }
}

/// Set up the initial stack frame for a **kernel-mode** process (Ring 0).
///
/// Creates a fake interrupt frame on the kernel stack that, when restored
/// by the context switch `iretq`, will start the process at `entry_point`
/// with interrupts enabled.
///
/// Register order matches the timer handler's push/pop convention.
/// The timer handler pushes: rax, rbx, ..., r15 (rax first = lowest addr).
/// After all pushes, RSP points to R15 (lowest addr of saved regs).
/// So stack_pointer must point to the R15 slot = frame base.
///
/// Stack layout (grows downward from `stack_top`):
///
/// ```text
/// [R15] [R14] ... [RAX] [RIP] [CS] [RFLAGS] [RSP] [SS]
///  ^                                            ^
///  sp (RSP after 15 pushes)                  sp + 20*8
/// ```
///
/// NOTE: All 5 IRET entries (RIP/CS/RFLAGS/RSP/SS) are always written,
/// even for same-privilege (CPL→CPL) returns. On x86-64, iretq may pop
/// RSP/SS regardless of CPL change when the saved CS RPL equals CPL.
/// Writing valid RSP and SS prevents the CPU from reading garbage.
fn setup_initial_stack_frame_kernel(stack_top: u64, entry_point: u64) -> u64 {
    let mut sp = stack_top;

    // Reserve 20 qwords: 15 GP regs + RIP + CS + RFLAGS + RSP + SS
    sp -= 20 * 8;

    let frame = sp as *mut u64;

    // With PIC, function pointers contain physical addresses after
    // R_X86_64_RELATIVE relocation by the bootloader. Convert to virtual.
    let entry_virt = unsafe { crate::memory::phys_to_kernel_virt(entry_point) };

    crate::serial::write_str("[PROC] stack frame: entry_phys=");
    crate::serial::write_hex(entry_point);
    crate::serial::write_str(" virt=");
    crate::serial::write_hex(entry_virt);
    crate::serial::write_str(" sp=");
    crate::serial::write_hex(sp);
    crate::serial::write_nl();

    unsafe {
        // After push rax..push r15, the stack from bottom to top is:
        // [RSP+0]=R15, [RSP+8]=R14, ..., [RSP+14*8]=RAX
        // But we need the frame to match what the pops expect.
        // The handler pops: r15, r14, ..., rax
        // So [sp+0]=R15, [sp+8]=R14, ..., [sp+14*8]=RAX
        frame.add(0).write(0);   // R15
        frame.add(1).write(0);   // R14
        frame.add(2).write(0);   // R13
        frame.add(3).write(0);   // R12
        frame.add(4).write(0);   // R11
        frame.add(5).write(0);   // R10
        frame.add(6).write(0);   // R9
        frame.add(7).write(0);   // R8
        frame.add(8).write(0);   // RBP
        frame.add(9).write(0);   // RDI
        frame.add(10).write(0);  // RSI
        frame.add(11).write(0);  // RDX
        frame.add(12).write(0);  // RCX
        frame.add(13).write(0);  // RBX
        frame.add(14).write(0);  // RAX
        // Interrupt frame (restored by iretq — 5 entries)
        frame.add(15).write(entry_virt);  // RIP (virtual address)
        frame.add(16).write(0x08);         // CS = kernel code selector (DPL=0, RPL=0)
        frame.add(17).write(0x202);        // RFLAGS = IF=1
        frame.add(18).write(stack_top);    // RSP = top of kernel stack
        frame.add(19).write(0x10);         // SS = kernel data selector (DPL=0, RPL=0)
    }

    sp
}

/// Set up the initial stack frame for a **user-mode** process (Ring 3).
///
/// Creates a fake interrupt frame that will cause `iretq` to:
/// 1. Switch to Ring 3 (CS = user code selector, SS = user data selector)
/// 2. Jump to `user_rip` with the user stack at `user_rsp`
///
/// Register order matches the timer handler's push/pop convention:
/// R15 at lowest address, RAX at highest (of the GP regs).
///
/// Stack layout (grows downward from `stack_top`):
///
/// ```text
/// [R15] [R14] ... [RAX] [RIP] [CS] [RFLAGS] [RSP] [SS]
///  ^                                            ^
///  sp                                          sp + 20*8
/// ```
fn setup_initial_stack_frame_user(stack_top: u64, user_rip: u64, user_rsp: u64) -> u64 {
    let mut sp = stack_top;

    // Reserve 20 qwords: 15 GP regs + RIP + CS + RFLAGS + RSP + SS
    sp -= 20 * 8;

    let frame = sp as *mut u64;

    unsafe {
        // 15 GP registers (all zeroed) — stored in reverse order to match
        // the timer handler's push convention (R15 at lowest address, RAX at highest)
        frame.add(0).write(0);   // R15
        frame.add(1).write(0);   // R14
        frame.add(2).write(0);   // R13
        frame.add(3).write(0);   // R12
        frame.add(4).write(0);   // R11
        frame.add(5).write(0);   // R10
        frame.add(6).write(0);   // R9
        frame.add(7).write(0);   // R8
        frame.add(8).write(0);   // RBP
        frame.add(9).write(0);   // RDI
        frame.add(10).write(0);  // RSI
        frame.add(11).write(0);  // RDX
        frame.add(12).write(0);  // RCX
        frame.add(13).write(0);  // RBX
        frame.add(14).write(0);  // RAX
        // Ring 3 IRET frame
        frame.add(15).write(user_rip);                    // RIP
        frame.add(16).write(crate::gdt::user_code_selector().0 as u64); // CS (Ring 3)
        frame.add(17).write(0x202);                       // RFLAGS (IF=1)
        frame.add(18).write(user_rsp);                    // RSP (Ring 3)
        frame.add(19).write(crate::gdt::user_data_selector().0 as u64); // SS (Ring 3)
    }

    sp
}
