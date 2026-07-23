//! # Process Management Module
//!
//! Implements the process abstraction and round-robin scheduler.
//!
//! ## Context Switch Flow
//!
//! ```text
//! Timer fires (vector 32)
//!   → Naked handler saves registers on current stack
//!   → schedule() picks next process
//!   → Load new process's stack pointer
//!   → Restore registers from new stack
//!   → iretq resumes new process
//! ```

pub mod context_switch;
pub mod idle;
pub mod init;
pub mod pipe;
pub mod process;
pub mod scheduler;
pub mod tasks;

pub use process::{ProcessState, Pid, MAX_PROCESSES, FdType, WakeReason, MAX_FDS};
pub use scheduler::SCHEDULER;

/// Maximum number of pipes in the system.
pub const MAX_PIPES: usize = 16;

/// Global pipe table. Allocated on demand by sys_pipe.
pub static mut PIPES: [Option<pipe::Pipe>; MAX_PIPES] = {
    const NONE: Option<pipe::Pipe> = None;
    [NONE; MAX_PIPES]
};

/// Allocate a pipe from the global table. Returns its index.
pub fn alloc_pipe() -> Option<usize> {
    unsafe {
        for i in 0..MAX_PIPES {
            if PIPES[i].is_none() {
                PIPES[i] = Some(pipe::Pipe::new());
                return Some(i);
            }
        }
    }
    None
}

/// Free a pipe from the global table.
///
/// # Safety
/// `idx` must be a valid pipe index returned by `alloc_pipe` that hasn't been freed yet.
pub unsafe fn free_pipe(idx: usize) {
    if idx < MAX_PIPES {
        PIPES[idx] = None;
    }
}

/// Initialize the process subsystem.
///
/// # Safety
/// Must be called with interrupts DISABLED. Returns with interrupts DISABLED.
/// Caller is responsible for enabling interrupts after all processes are spawned.
pub fn init() {
    crate::serial::write_str("[PROC] Initializing process subsystem...\n");

    unsafe { core::arch::asm!("cli", options(nostack, nomem)); }
    let mut sched = SCHEDULER.lock();
    sched.init();
    sched.spawn_idle(idle::idle_main as *const () as u64);

    // Spawn PID 1 (init/reaper) — adopts all orphaned processes
    sched.spawn_kernel(init::init_main as *const () as u64);

    drop(sched);
    // NOTE: interrupts remain DISABLED — caller must enable them later.

    crate::serial::write_str("[PROC] Process subsystem initialized\n");
}

/// Spawn a new user-mode process from an ELF binary.
///
/// Creates a per-process PML4, loads ELF segments via the ELF loader,
/// maps a user stack page, and creates the process.
pub fn spawn_user(elf_data: &[u8], parent: Option<Pid>) -> Option<Pid> {
    use crate::memory::{self, vmm, USER_STACK_TOP};
    use x86_64::structures::paging::{FrameAllocator, PageTableFlags};
    use x86_64::VirtAddr;

    // 1. Get the current kernel PML4 (to copy kernel entries from)
    let kernel_pml4 = {
        let (frame, _) = x86_64::registers::control::Cr3::read();
        frame.start_address().as_u64()
    };

    // 2. Create a new PML4 with kernel entries shared
    let user_pml4 = vmm::create_user_pml4(memory::PhysAddr::new(kernel_pml4));

    // 3. Load ELF segments into the process's address space
    let elf_image = match crate::elf::load_elf(elf_data, user_pml4) {
        Ok(img) => img,
        Err(e) => {
            crate::serial::write_str("[PROC] ELF load failed: ");
            crate::serial::write_str(e.description());
            crate::serial::write_nl();
            return None;
        }
    };

    // 4. Map user stack with guard page below
    //    Layout (grows downward):
    //    USER_STACK_TOP                          = stack top (RSP starts here)
    //    USER_STACK_TOP - PAGE_SIZE              = stack page 4
    //    USER_STACK_TOP - 2*PAGE_SIZE            = stack page 3
    //    USER_STACK_TOP - 3*PAGE_SIZE            = stack page 2
    //    USER_STACK_TOP - 4*PAGE_SIZE            = stack page 1 (bottom)
    //    USER_STACK_TOP - 5*PAGE_SIZE            = guard page (not user-accessible)
    let guard_page_frame = vmm::PmmFrameAllocator.allocate_frame()
        .expect("PMM: out of memory for user stack guard page");
    let guard_page_virt = VirtAddr::new(crate::memory::USER_STACK_TOP - 5 * crate::memory::PAGE_SIZE);
    let guard_flags = PageTableFlags::PRESENT; // No USER_ACCESSIBLE, no WRITABLE
    vmm::map_page(user_pml4, guard_page_virt, memory::PhysAddr::new(guard_page_frame.start_address().as_u64()), guard_flags);

    // Map 4 stack pages (16 KiB)
    for i in 0..4 {
        let frame = vmm::PmmFrameAllocator.allocate_frame()
            .expect("PMM: out of memory for user stack page");
        let offset = (4 - i) * crate::memory::PAGE_SIZE; // pages 4,3,2,1 from top
        let stack_virt = VirtAddr::new(crate::memory::USER_STACK_TOP - offset);
        let stack_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        vmm::map_page(user_pml4, stack_virt, memory::PhysAddr::new(frame.start_address().as_u64()), stack_flags);
    }

    // User RSP starts at the top of the stack region
    let user_rsp = USER_STACK_TOP;

    crate::serial::write_str("[PROC] ELF loaded: entry=");
    crate::serial::write_hex(elf_image.entry);
    crate::serial::write_str(", stack_top=");
    crate::serial::write_hex(USER_STACK_TOP);
    crate::serial::write_str(", stack_guard=");
    crate::serial::write_hex(crate::memory::USER_STACK_TOP - 5 * crate::memory::PAGE_SIZE);
    crate::serial::write_str(", pml4=");
    crate::serial::write_hex(user_pml4.as_u64());
    crate::serial::write_nl();

    // 5. Spawn via the scheduler
    let result = SCHEDULER.lock().spawn_user(elf_image.entry, user_rsp, user_pml4.as_u64(), parent);
    result
}

/// Start the scheduler. Never returns.
///
/// Enables interrupts and enters a HLT loop. The first timer interrupt
/// triggers `schedule()` which, seeing `current_pid == None`, finds the
/// first Ready task and returns its initial frame SP. The naked handler
/// then context-switches to that task via iretq — the same path used
/// for every subsequent context switch.
///
/// ## Stack transition
///
/// ```text
/// boot stack (kernel_main)  →  timer fires  →  schedule()
///   finds first Ready task  →  returns its initial frame SP
///   handler: mov rsp, r12   →  pop 15 GP  →  iretq
///   task runs on its OWN allocated kernel stack
///   boot stack is abandoned (never returned to)
/// ```
pub fn start_scheduler() -> ! {
    crate::serial::write_str("[PROC] Starting scheduler — first tick will dispatch\n");

    // Enable interrupts. The first timer IRQ will trigger the initial dispatch.
    // current_pid is None (set by init/sched.start not being called),
    // so schedule() will find the first Ready task and iretq to it.
    unsafe { core::arch::asm!("sti", options(nostack, nomem)); }

    // HLT loop — we never return to kernel_main.
    // The first timer interrupt context-switches to the first task.
    loop {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem)); }
    }
}

/// Wake all processes blocked on keyboard input or pipe I/O.
///
/// Called by the keyboard IRQ handler and pipe write/read operations.
pub fn keyboard_wake() {
    let mut sched = SCHEDULER.lock();
    for i in 0..process::MAX_PROCESSES as u64 {
        if let Some(Some(ref mut proc)) = sched.processes_mut().get_mut(i as usize) {
            if proc.state == process::ProcessState::Blocked {
                match proc.wake_reason {
                    process::WakeReason::Keyboard => {
                        proc.state = process::ProcessState::Ready;
                        proc.wake_reason = process::WakeReason::None;
                    }
                    process::WakeReason::PipeRead { pipe_idx } => {
                        // Check if pipe has data
                        unsafe {
                            if let Some(ref p) = PIPES[pipe_idx as usize] {
                                let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed);
                                let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed);
                                if nread < nwrite || !p.write_open.load(core::sync::atomic::Ordering::Relaxed) {
                                    proc.state = process::ProcessState::Ready;
                                    proc.wake_reason = process::WakeReason::None;
                                }
                            }
                        }
                    }
                    process::WakeReason::PipeWrite { pipe_idx } => {
                        // Check if pipe has space
                        unsafe {
                            if let Some(ref p) = PIPES[pipe_idx as usize] {
                                let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed);
                                let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed);
                                if nwrite < nread + pipe::PIPE_SIZE as u32 {
                                    proc.state = process::ProcessState::Ready;
                                    proc.wake_reason = process::WakeReason::None;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Yield the CPU to the next process.
///
/// Used by pipe read/write to block while waiting for data/space.
pub fn yield_now() {
    unsafe { crate::syscall::set_force_switch(); }
    // After force_switch, we need to re-enable interrupts since syscall clears IF
    unsafe { core::arch::asm!("sti", options(nostack, nomem)); }
    // HLT until next timer interrupt reschedules us
    unsafe { core::arch::asm!("hlt", options(nostack, nomem)); }
}
