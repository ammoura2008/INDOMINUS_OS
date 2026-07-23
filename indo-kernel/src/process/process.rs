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

use alloc::boxed::Box;
use alloc::sync::Arc;
use crate::vfs::File;

/// Maximum number of concurrent processes.
pub const MAX_PROCESSES: usize = 32;

/// Size of each process's kernel stack (8 KiB = 2 pages).
/// Must be large enough for syscall/interrupt handling.
pub const KERNEL_STACK_SIZE: usize = 8192;

/// Maximum number of file descriptors per process.
pub const MAX_FDS: usize = 8;

/// Process identifier.
pub type Pid = u64;

/// File descriptor types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdType {
    /// Not open.
    None,
    /// Stdin (keyboard input).
    Stdin,
    /// Stdout (serial output).
    Stdout,
    /// Stderr (serial output).
    Stderr,
    /// Null device (discards writes, returns EOF on read).
    Null,
    /// TTY (console input/output).
    Tty,
    /// Pipe endpoint. `writable` indicates write end (true) or read end (false).
    Pipe { pipe_idx: u8, writable: bool },
    /// VFS file. Index into process's `file_handles` array.
    FsFile { index: u8 },
}

/// Reason a process is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// Not blocked.
    None,
    /// Sleeping — wake when tick_count >= deadline.
    Sleep { deadline: u64 },
    /// Waiting for keyboard input.
    Keyboard,
    /// Waiting for pipe data (read end).
    PipeRead { pipe_idx: u8 },
    /// Waiting for pipe space (write end).
    PipeWrite { pipe_idx: u8 },
}

/// Process states.
///
/// Valid transitions:
///   Ready    → Running  (scheduler picks process)
///   Running  → Ready    (preempted by timer)
///   Running  → Blocked  (process calls sys_sleep or blocking waitpid)
///   Running  → Zombie   (process calls sys_exit or is killed)
///   Blocked  → Ready    (woken by timer/event)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Zombie,
}

/// Maximum number of open file handles per process.
pub const MAX_FILE_HANDLES: usize = 16;

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
    pub exit_code: u64,
    /// Whether this is a user-mode process.
    pub is_user: bool,
    /// Parent process PID. None for the idle process and kernel tasks spawned at boot.
    pub parent_pid: Option<Pid>,
    /// Generation of the parent at spawn time. Used to prevent cross-family reaping
    /// when PIDs are reused. Matches against the parent's `generation` field.
    pub parent_generation: u32,
    /// Monotonic generation counter for this process slot. Incremented each time the
    /// slot is reused. Prevents a new process at PID N from reaping orphans of the
    /// previous process at PID N.
    pub generation: u32,
    /// Reason this process is blocked (sleep deadline, keyboard, etc.).
    pub wake_reason: WakeReason,
    /// File descriptor table. Index = FD number, value = FD type.
    /// FDs 0, 1, 2 are pre-assigned to stdin/stdout/stderr.
    pub fd_types: [FdType; MAX_FDS],
    /// VFS file handles. Ref-counted via Arc — multiple FDs can share one handle
    /// (e.g. after dup). The inner Box<dyn File> is wrapped in spin::Mutex for
    /// interior mutability (File trait requires &mut self for read/write/seek).
    pub file_handles: [Option<Arc<spin::Mutex<Box<dyn File>>>>; MAX_FILE_HANDLES],
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
        let pml4 = {
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
            exit_code: 0,
            is_user: false,
            parent_pid: None,
            parent_generation: 0,
            generation: 0,
            wake_reason: WakeReason::None,
            fd_types: [FdType::None; MAX_FDS],
            file_handles: Default::default(),
        }
    }

    /// Create a new user-mode process (Ring 3).
    ///
    /// - `user_rip`: virtual address of the user entry point
    /// - `user_rsp`: initial user stack pointer
    /// - `pml4`: per-process page table (from `create_user_pml4`)
    /// - `parent_pid`: PID of the parent process (the spawner)
    /// - `parent_generation`: generation of the parent at spawn time
    pub fn new_user(pid: Pid, user_rip: u64, user_rsp: u64, pml4: u64, parent_pid: Option<Pid>, parent_generation: u32) -> Self {
        let stack_base = alloc_kernel_stack();
        let stack_top = stack_base + KERNEL_STACK_SIZE as u64;
        let sp = setup_initial_stack_frame_user(stack_top, user_rip, user_rsp);

        let mut fd_types = [FdType::None; MAX_FDS];
        fd_types[0] = FdType::Stdin;
        fd_types[1] = FdType::Stdout;
        fd_types[2] = FdType::Stderr;

        Process {
            pid,
            state: ProcessState::Ready,
            stack_pointer: sp,
            kernel_stack_base: stack_base,
            pml4_phys: pml4,
            user_rip: Some(user_rip),
            user_rsp: Some(user_rsp),
            exit_code: 0,
            is_user: true,
            parent_pid,
            parent_generation,
            generation: 0,
            wake_reason: WakeReason::None,
            fd_types,
            file_handles: Default::default(),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Close all open file handles — drop the Arc refs
        for i in 0..MAX_FILE_HANDLES {
            self.file_handles[i] = None;
        }

        // Close any pipe FDs — decrement refcounts so pipes aren't leaked.
        // This runs when a process is reaped (waitpid) or killed (exception handler).
        for i in 0..MAX_FDS {
            if let FdType::Pipe { pipe_idx, writable } = self.fd_types[i] {
                let pipe_idx = pipe_idx as usize;
                unsafe {
                    if let Some(ref mut p) = crate::process::PIPES[pipe_idx] {
                        crate::process::pipe::pipe_close(p, writable);
                        let old = p.refcount.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
                        if old == 1 {
                            crate::process::free_pipe(pipe_idx);
                        }
                    }
                }
            }
            self.fd_types[i] = FdType::None;
        }

        // Free user address space if this was a user process with a valid PML4.
        // This prevents permanent page table + physical frame leaks when zombies
        // are reaped via waitpid. The context_switch paths (force_switch, kill_process)
        // free resources for processes that die while running, but reaped zombies
        // only get cleaned up here.
        //
        // Safety: we must be on the kernel PML4 when calling free_user_address_space.
        // Drop runs from reap_zombie (syscall context with scheduler lock held),
        // where the kernel PML4 is guaranteed active.
        if self.is_user && self.pml4_phys != 0 {
            unsafe {
                crate::memory::vmm::free_user_address_space(
                    crate::memory::PhysAddr::new(self.pml4_phys)
                );
            }
            self.pml4_phys = 0;
        }

        // Free the kernel stack (heap allocation)
        if self.kernel_stack_base != 0 {
            free_kernel_stack(self.kernel_stack_base);
            self.kernel_stack_base = 0;
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

/// Free a previously allocated kernel stack back to the heap.
///
/// # Safety
/// - `stack_base` must have been returned by `alloc_kernel_stack()`
/// - Must not have been freed already (double-free is UB)
/// - Must only be called when the stack is no longer in use
pub fn free_kernel_stack(stack_base: u64) {
    if stack_base == 0 {
        return;
    }
    let layout = core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE, 16)
        .expect("Invalid kernel stack layout");
    unsafe {
        alloc::alloc::dealloc(stack_base as *mut u8, layout);
    }
}

/// Set up the initial stack frame for a **kernel-mode** process (Ring 0).
///
/// Creates a fake interrupt frame on the kernel stack that, when restored
/// by the context switch `iretq`, will start the process at `entry_point`
/// with interrupts enabled.
///
/// Register order matches the timer handler's push convention:
/// R15 pushed first (highest address), RAX last (lowest = RSP).
///
/// Canonical SyscallFrame layout (15 GP regs):
/// ```text
/// [RAX] [RBX] [RCX] ... [R15] [RIP] [CS] [RFLAGS] [RSP] [SS]
///  ^                                              ^
///  sp (RSP after 15 pushes)                     sp + 20*8
/// ```
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
        // Canonical SyscallFrame: [sp+0]=RAX, [sp+8]=RBX, ..., [sp+112]=R15
        frame.add(0).write(0);   // RAX (syscall number)
        frame.add(1).write(0);   // RBX
        frame.add(2).write(0);   // RCX (user RIP)
        frame.add(3).write(0);   // RDX
        frame.add(4).write(0);   // RSI
        frame.add(5).write(0);   // RDI
        frame.add(6).write(0);   // RBP
        frame.add(7).write(0);   // R8
        frame.add(8).write(0);   // R9
        frame.add(9).write(0);   // R10
        frame.add(10).write(0);  // R11 (user RFLAGS)
        frame.add(11).write(0);  // R12
        frame.add(12).write(0);  // R13
        frame.add(13).write(0);  // R14
        frame.add(14).write(0);  // R15
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
/// Canonical SyscallFrame layout:
/// ```text
/// [RAX] [RBX] [RCX] ... [R15] [RIP] [CS] [RFLAGS] [RSP] [SS]
///  ^                                              ^
///  sp                                            sp + 20*8
/// ```
fn setup_initial_stack_frame_user(stack_top: u64, user_rip: u64, user_rsp: u64) -> u64 {
    let mut sp = stack_top;

    // Reserve 20 qwords: 15 GP regs + RIP + CS + RFLAGS + RSP + SS
    sp -= 20 * 8;

    let frame = sp as *mut u64;

    unsafe {
        // Canonical SyscallFrame: [sp+0]=RAX, [sp+8]=RBX, ..., [sp+112]=R15
        frame.add(0).write(0);   // RAX (syscall number)
        frame.add(1).write(0);   // RBX
        frame.add(2).write(0);   // RCX (user RIP)
        frame.add(3).write(0);   // RDX
        frame.add(4).write(0);   // RSI
        frame.add(5).write(0);   // RDI
        frame.add(6).write(0);   // RBP
        frame.add(7).write(0);   // R8
        frame.add(8).write(0);   // R9
        frame.add(9).write(0);   // R10
        frame.add(10).write(0);  // R11 (user RFLAGS)
        frame.add(11).write(0);  // R12
        frame.add(12).write(0);  // R13
        frame.add(13).write(0);  // R14
        frame.add(14).write(0);  // R15
        // Ring 3 IRET frame
        frame.add(15).write(user_rip);                    // RIP
        frame.add(16).write(crate::gdt::user_code_selector().0 as u64); // CS (Ring 3)
        frame.add(17).write(0x202);                       // RFLAGS (IF=1)
        frame.add(18).write(user_rsp);                    // RSP (Ring 3)
        frame.add(19).write(crate::gdt::user_data_selector().0 as u64); // SS (Ring 3)
    }

    sp
}
