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
/// Protected by `spin::Mutex` to eliminate `static mut` unsoundness.
/// Lock ordering: SCHEDULER → PIPES (never reversed).
pub static PIPES: spin::Mutex<[Option<pipe::Pipe>; MAX_PIPES]> = spin::Mutex::new({
    const NONE: Option<pipe::Pipe> = None;
    [NONE; MAX_PIPES]
});

/// Allocate a pipe from the global table. Returns its index.
pub fn alloc_pipe() -> Option<usize> {
    let mut pipes = PIPES.lock();
    for i in 0..MAX_PIPES {
        if pipes[i].is_none() {
            pipes[i] = Some(pipe::Pipe::new());
            return Some(i);
        }
    }
    None
}

/// Free a pipe from the global table.
pub fn free_pipe(idx: usize) {
    if idx < MAX_PIPES {
        PIPES.lock()[idx] = None;
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
    use crate::memory::{self, vmm};
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

    // 4. Map user stack with guard page below (ASLR randomized stack top)
    let stack_top = crate::aslr::randomize_stack_base();
    let guard_page_frame = vmm::PmmFrameAllocator.allocate_frame()
        .expect("PMM: out of memory for user stack guard page");
    let guard_page_virt = VirtAddr::new(stack_top - 5 * crate::memory::PAGE_SIZE);
    let guard_flags = PageTableFlags::PRESENT; // No USER_ACCESSIBLE, no WRITABLE
    vmm::map_page(user_pml4, guard_page_virt, memory::PhysAddr::new(guard_page_frame.start_address().as_u64()), guard_flags);

    // Map 4 stack pages (16 KiB)
    for i in 0..4 {
        let frame = vmm::PmmFrameAllocator.allocate_frame()
            .expect("PMM: out of memory for user stack page");
        let offset = (4 - i) * crate::memory::PAGE_SIZE;
        let stack_virt = VirtAddr::new(stack_top - offset);
        let stack_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        vmm::map_page(user_pml4, stack_virt, memory::PhysAddr::new(frame.start_address().as_u64()), stack_flags);
    }

    // User RSP starts at the top of the stack region (ASLR randomized)
    let user_rsp = stack_top - 8;

    crate::serial::write_str("[PROC] ELF loaded: entry=");
    crate::serial::write_hex(elf_image.entry);
    crate::serial::write_str(", stack_top=");
    crate::serial::write_hex(stack_top);
    crate::serial::write_str(", stack_guard=");
    crate::serial::write_hex(stack_top - 5 * crate::memory::PAGE_SIZE);
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
    let pipes = PIPES.lock();

    // Phase 1: Collect which processes need waking (avoid borrow conflicts)
    let mut wake_list: alloc::vec::Vec<usize> = alloc::vec::Vec::new();

    for i in 0..process::MAX_PROCESSES {
        if let Some(ref proc) = sched.processes()[i] {
            if proc.state != process::ProcessState::Blocked {
                continue;
            }
            match proc.wake_reason {
                process::WakeReason::Keyboard => {
                    wake_list.push(i);
                }
                process::WakeReason::PipeRead { pipe_idx } => {
                    let idx = pipe_idx as usize;
                    if idx < MAX_PIPES {
                        if let Some(ref p) = pipes[idx] {
                            let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed);
                            let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed);
                            if nread < nwrite || !p.write_open.load(core::sync::atomic::Ordering::Relaxed) {
                                wake_list.push(i);
                            }
                        }
                    }
                }
                process::WakeReason::PipeWrite { pipe_idx } => {
                    let idx = pipe_idx as usize;
                    if idx < MAX_PIPES {
                        if let Some(ref p) = pipes[idx] {
                            let nread = p.nread.load(core::sync::atomic::Ordering::Relaxed);
                            let nwrite = p.nwrite.load(core::sync::atomic::Ordering::Relaxed);
                            if nwrite < nread.wrapping_add(pipe::PIPE_SIZE as u32) {
                                wake_list.push(i);
                            }
                        }
                    }
                }
                process::WakeReason::WaitForChild { child_pid } => {
                    let cp = child_pid as usize;
                    if cp < process::MAX_PROCESSES {
                        let child_is_zombie = sched.processes()[cp]
                            .as_ref()
                            .map_or(false, |c| c.state == process::ProcessState::Zombie);
                        if child_is_zombie {
                            wake_list.push(i);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Phase 2: Wake the collected processes
    for pid in wake_list {
        if let Some(Some(ref mut proc)) = sched.processes_mut().get_mut(pid) {
            proc.state = process::ProcessState::Ready;
            proc.wake_reason = process::WakeReason::None;
        }
    }
}

/// Yield the CPU to the next process.
///
/// Sets the force_switch flag so the syscall_entry handler performs a context
/// switch via the force_switch path (which correctly does swapgs before iretq).
///
/// yield_now() is called from INSIDE a syscall handler, after swapgs has set
/// KERNEL_GS_BASE = 0. If we do `sti; hlt`, the timer interrupt fires while
/// KERNEL_GS_BASE is 0. The timer handler does NOT do swapgs, so when it
/// context-switches to a new process via iretq, KERNEL_GS_BASE remains 0.
/// The new process then does `syscall` → `swapgs` → GS_BASE = 0 → gs:0
/// faults at address 0 (CR2=0x0).
///
/// Instead, we just set force_switch and return. The caller MUST break out of
/// any loop and return from syscall_dispatch. The force_switch path in
/// syscall_entry will then do the proper context switch with swapgs.
pub fn yield_now() {
    unsafe { crate::syscall::set_force_switch(); }
}

/// Send a signal to all processes in the foreground process group.
///
/// Finds the running user process with the lowest PID (the foreground process)
/// and sends the signal to all processes sharing its PGID.
/// SIGINT (2) and SIGQUIT (3) kill the process. SIGTSTP (20) stops it.
pub fn send_signal_to_fg(signal: u8) {
    use core::sync::atomic::Ordering;

    let mut sched = SCHEDULER.lock();

    // Find the foreground PGID: the PGID of the lowest-PID running user process
    let fg_pgid = {
        let mut found_pgid = None;
        for i in 1..crate::process::process::MAX_PROCESSES {
            if let Some(ref proc) = sched.processes()[i] {
                if proc.is_user && proc.state == crate::process::process::ProcessState::Running {
                    found_pgid = Some(proc.pgid);
                    break;
                }
            }
        }
        // If no running process, find the lowest-PID ready user process
        if found_pgid.is_none() {
            for i in 1..crate::process::process::MAX_PROCESSES {
                if let Some(ref proc) = sched.processes()[i] {
                    if proc.is_user && proc.state == crate::process::process::ProcessState::Ready {
                        found_pgid = Some(proc.pgid);
                        break;
                    }
                }
            }
        }
        found_pgid
    };

    if let Some(fg_pgid) = fg_pgid {
        for i in 1..crate::process::process::MAX_PROCESSES {
            if let Some(ref mut proc) = sched.processes_mut()[i] {
                if proc.is_user && proc.pgid == fg_pgid
                    && proc.state != crate::process::process::ProcessState::Zombie
                {
                    match signal {
                        2 | 3 => {
                            // SIGINT or SIGQUIT: kill the process
                            proc.state = crate::process::process::ProcessState::Zombie;
                            proc.exit_code = signal as u64;
                            proc.terminated_by_signal = true;
                        }
                        20 => {
                            // SIGTSTP: stop the process (for WUNTRACED)
                            proc.state = crate::process::process::ProcessState::Blocked;
                            proc.wake_reason = crate::process::process::WakeReason::None;
                            proc.stop_signal = signal;
                        }
                        18 => {
                            // SIGCONT: resume stopped processes
                            if proc.state == crate::process::process::ProcessState::Blocked
                                && proc.wake_reason == crate::process::process::WakeReason::None
                            {
                                proc.state = crate::process::process::ProcessState::Ready;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    drop(sched);
    crate::process::keyboard_wake();
}
