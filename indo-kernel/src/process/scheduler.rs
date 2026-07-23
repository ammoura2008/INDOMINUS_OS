//! # Round-Robin Scheduler
//!
//! Implements preemptive round-robin scheduling with timer-driven preemption.
//!
//! ## Stack transition (boot → task)
//!
//! ```text
//! 1. kernel_main runs on UEFI boot stack
//! 2. start_scheduler enables interrupts, enters HLT loop
//! 3. First timer fires → CPU pushes IRQ frame on boot stack
//! 4. Naked handler pushes 15 GP regs on boot stack
//! 5. schedule(boot_rsp): current_pid is None → find first Ready task
//! 6. Returns task's initial frame SP (on its allocated kernel stack)
//! 7. Handler: mov rsp, r12 → load initial frame → iretq
//! 8. Task runs on its OWN kernel stack — boot stack abandoned
//! ```
//!
//! ## Idle policy
//!
//! Idle (PID 0) is NEVER selected while any normal Ready task exists.
//! It only runs when all other tasks are Zombie or not Ready.

use super::process::{Process, ProcessState, Pid, MAX_PROCESSES, WakeReason};

/// Time quantum in ticks (5 ticks = 50ms at 100 Hz).
const DEFAULT_QUANTUM: u64 = 5;

/// Global scheduler state.
pub static SCHEDULER: spin::Lazy<spin::Mutex<Scheduler>> = spin::Lazy::new(|| {
    spin::Mutex::new(Scheduler::new())
});

pub struct Scheduler {
    processes: [Option<Process>; MAX_PROCESSES],
    /// Per-slot generation counters. Incremented each time a slot is reused.
    /// Prevents cross-family reaping when PIDs are recycled.
    generations: [u32; MAX_PROCESSES],
    current_pid: Option<Pid>,
    tick_counter: u64,
    quantum: u64,
    idle_pid: Pid,
}

impl Scheduler {
    fn new() -> Self {
        const NONE: Option<Process> = None;
        Scheduler {
            processes: [NONE; MAX_PROCESSES],
            generations: [0; MAX_PROCESSES],
            current_pid: None,
            tick_counter: 0,
            quantum: DEFAULT_QUANTUM,
            idle_pid: 0,
        }
    }

    pub fn init(&mut self) {
        for slot in self.processes.iter_mut() {
            *slot = None;
        }
        self.current_pid = None;
        self.tick_counter = 0;
    }

    pub fn spawn_idle(&mut self, entry_phys: u64) -> Pid {
        let pid = 0;
        let mut process = Process::new_kernel(pid, entry_phys);
        process.state = ProcessState::Ready;
        process.generation = 1; // PID 0 always has generation 1
        self.processes[pid as usize] = Some(process);
        self.idle_pid = pid;

        crate::serial::write_str("[SCHED] Idle process at PID 0\n");
        pid
    }

    /// Spawn a kernel-mode process at the first free PID (skipping 0=idle).
    /// Returns the PID on success, or None if the process table is full.
    pub fn spawn_kernel(&mut self, entry_phys: u64) -> Option<Pid> {
        let pid = (1..MAX_PROCESSES as u64).find(|&i| self.processes[i as usize].is_none())?;
        let slot = pid as usize;
        self.generations[slot] += 1;
        let mut process = Process::new_kernel(pid, entry_phys);
        process.state = ProcessState::Ready;
        process.generation = self.generations[slot];
        self.processes[slot] = Some(process);

        crate::serial::write_str("[SCHED] Spawned kernel process PID=");
        crate::serial::write_u64(pid);
        crate::serial::write_nl();
        Some(pid)
    }

    pub fn spawn_user(&mut self, user_rip: u64, user_rsp: u64, pml4: u64, parent: Option<Pid>) -> Option<Pid> {
        // Find a free slot (PID 0 is reserved for idle)
        let pid = (1..MAX_PROCESSES as u64).find(|&i| self.processes[i as usize].is_none())?;
        let slot = pid as usize;

        // Get parent's generation for cross-family reaping protection
        let parent_gen = parent.and_then(|p| {
            self.processes.get(p as usize)
                .and_then(|s| s.as_ref())
                .map(|proc| proc.generation)
        }).unwrap_or(0);

        // Increment slot generation (prevents new process from reaping old orphans)
        self.generations[slot] += 1;
        let gen = self.generations[slot];

        let mut process = Process::new_user(pid, user_rip, user_rsp, pml4, parent, parent_gen);
        process.generation = gen;
        self.processes[slot] = Some(process);

        crate::serial::write_str("[SCHED] Spawned user process ");
        crate::serial::write_u64(pid);
        crate::serial::write_nl();

        Some(pid)
    }

    /// Get the generation of a process slot.
    pub fn get_generation(&self, pid: Pid) -> u32 {
        self.generations.get(pid as usize).copied().unwrap_or(0)
    }

    pub fn on_tick(&mut self) -> u64 {
        self.tick_counter += 1;

        // Wake any blocked processes whose deadline has passed
        let now = crate::interrupts::pit::tick_count();
        for i in 0..MAX_PROCESSES as u64 {
            if let Some(Some(ref mut proc)) = self.processes.get_mut(i as usize) {
                if proc.state == ProcessState::Blocked {
                    if let WakeReason::Sleep { deadline } = proc.wake_reason {
                        if now >= deadline {
                            proc.state = ProcessState::Ready;
                            proc.wake_reason = WakeReason::None;
                            #[cfg(DEBUG_KERNEL)]
                            {
                                crate::serial::write_str("[SCHED] Woke PID=");
                                crate::serial::write_u64(i);
                                crate::serial::write_nl();
                            }
                        }
                    }
                }
            }
        }

        if self.tick_counter >= self.quantum {
            self.tick_counter = 0;
            #[cfg(DEBUG_KERNEL)]
            {
                crate::serial::write_str("[SCHED] Q=");
                crate::serial::write_u64(self.current_pid.unwrap_or(99));
                crate::serial::write_nl();
            }
            return self.switch_next();
        }
        self.current_stack_pointer()
    }

    fn current_stack_pointer(&self) -> u64 {
        if let Some(pid) = self.current_pid {
            self.processes[pid as usize]
                .as_ref()
                .map(|p| p.stack_pointer)
                .unwrap_or(0)
        } else {
            0
        }
    }

    /// Find the next Ready process after `after_pid` in round-robin order.
    ///
    /// Idle (PID 0) is SKIPPED during the first pass. It is only returned
    /// as a fallback when no other Ready process exists.
    pub(crate) fn find_next_ready(&self, after_pid: Pid) -> Option<Pid> {
        let start = (after_pid + 1) % MAX_PROCESSES as u64;

        // Pass 1: find any non-idle Ready process
        for i in 0..MAX_PROCESSES as u64 {
            let check_pid = (start + i) % MAX_PROCESSES as u64;
            if check_pid == self.idle_pid {
                continue;
            }
            if let Some(ref proc) = self.processes[check_pid as usize] {
                if proc.state == ProcessState::Ready {
                    return Some(check_pid);
                }
            }
        }

        // Pass 2: only idle might be ready — use it as fallback
        if let Some(ref proc) = self.processes[self.idle_pid as usize] {
            if proc.state == ProcessState::Ready {
                return Some(self.idle_pid);
            }
        }

        None
    }

    fn switch_next(&mut self) -> u64 {
        let old_pid = self.current_pid;

        if let Some(old) = old_pid {
            if let Some(ref mut proc) = self.processes[old as usize] {
                if proc.state == ProcessState::Running {
                    proc.state = ProcessState::Ready;
                }
            }
        }

        let next_pid = if let Some(old) = old_pid {
            self.find_next_ready(old)
        } else {
            self.find_next_ready(0)
        };

        let new_pid = next_pid.unwrap_or(self.idle_pid);

        if let Some(ref mut proc) = self.processes[new_pid as usize] {
            proc.state = ProcessState::Running;
        }

        self.current_pid = Some(new_pid);

        self.processes[new_pid as usize]
            .as_ref()
            .map(|p| p.stack_pointer)
            .unwrap_or(0)
    }

    /// Force-switch: always picks the next Ready process, ignoring quantum.
    /// Used by sys_exit/sys_yield where the current process must yield now.
    pub(crate) fn switch_next_force(&mut self) -> u64 {
        let old_pid = self.current_pid;

        // Mark old process: if Running → Ready, if Zombie → stay Zombie
        if let Some(old) = old_pid {
            if let Some(ref mut proc) = self.processes[old as usize] {
                if proc.state == ProcessState::Running {
                    proc.state = ProcessState::Ready;
                }
            }
        }

        let next_pid = if let Some(old) = old_pid {
            self.find_next_ready(old)
        } else {
            self.find_next_ready(0)
        };

        let new_pid = next_pid.unwrap_or(self.idle_pid);

        if let Some(ref mut proc) = self.processes[new_pid as usize] {
            proc.state = ProcessState::Running;
        }

        self.current_pid = Some(new_pid);

        self.processes[new_pid as usize]
            .as_ref()
            .map(|p| p.stack_pointer)
            .unwrap_or(0)
    }

    pub fn dump_table(&self) {
        crate::serial::write_str("[SCHED] === PROCESS TABLE ===\n");
        crate::serial::write_str("[SCHED] idle_pid=");
        crate::serial::write_u64(self.idle_pid);
        crate::serial::write_str(" current=");
        crate::serial::write_u64(self.current_pid.unwrap_or(99));
        crate::serial::write_str(" tick=");
        crate::serial::write_u64(self.tick_counter);
        crate::serial::write_str("/");
        crate::serial::write_u64(self.quantum);
        crate::serial::write_nl();

        let mut active = 0u64;
        for i in 0..MAX_PROCESSES as u64 {
            if let Some(ref proc) = self.processes[i as usize] {
                active += 1;
                crate::serial::write_str("[SCHED] PID=");
                crate::serial::write_u64(proc.pid);
                crate::serial::write_str(" state=");
                match proc.state {
                    ProcessState::Ready => crate::serial::write_str("Ready"),
                    ProcessState::Running => crate::serial::write_str("Running"),
                    ProcessState::Blocked => crate::serial::write_str("Blocked"),
                    ProcessState::Zombie => crate::serial::write_str("Zombie"),
                }
                if proc.state == ProcessState::Blocked {
                    crate::serial::write_str("(");
                    match proc.wake_reason {
                        WakeReason::Sleep { deadline } => {
                            crate::serial::write_str("sleep=");
                            crate::serial::write_u64(deadline);
                        }
                        WakeReason::Keyboard => crate::serial::write_str("kbd"),
                        WakeReason::PipeRead { pipe_idx } => {
                            crate::serial::write_str("pipe_read=");
                            crate::serial::write_u64(pipe_idx as u64);
                        }
                        WakeReason::PipeWrite { pipe_idx } => {
                            crate::serial::write_str("pipe_write=");
                            crate::serial::write_u64(pipe_idx as u64);
                        }
                        WakeReason::None => crate::serial::write_str("none"),
                    }
                    crate::serial::write_str(")");
                }
                crate::serial::write_str(" sp=");
                crate::serial::write_hex(proc.stack_pointer);
                crate::serial::write_str(" kstack=");
                crate::serial::write_hex(proc.kernel_stack_base);
                crate::serial::write_nl();
            }
        }
        crate::serial::write_str("[SCHED] active=");
        crate::serial::write_u64(active);
        crate::serial::write_str("/");
        crate::serial::write_u64(MAX_PROCESSES as u64);
        crate::serial::write_str("\n[SCHED] === END TABLE ===\n");
    }

    pub fn save_current_sp(&mut self, sp: u64) {
        if let Some(pid) = self.current_pid {
            if let Some(ref mut proc) = self.processes[pid as usize] {
                proc.stack_pointer = sp;
            }
        }
    }

    /// Dispatch the first process. Called when current_pid is None.
    /// Sets the process as Running and returns its initial frame SP.
    pub(crate) fn dispatch_first(&mut self, pid: Pid) -> u64 {
        if let Some(ref mut proc) = self.processes[pid as usize] {
            proc.state = ProcessState::Running;
        }
        self.current_pid = Some(pid);
        self.tick_counter = 0;
        self.processes[pid as usize]
            .as_ref()
            .map(|p| p.stack_pointer)
            .unwrap_or(0)
    }

    /// Kill the current process (mark as Zombie) and return the next process's
    /// saved stack pointer. Used by the page fault handler when a user process
    /// faults — the kernel continues with the next process instead of halting.
    ///
    /// Unlike `switch_next_force`, this ALWAYS marks the current process as
    /// Zombie (not Ready), regardless of its current state.
    ///
    /// Returns the stack pointer of the next Ready process, or the idle process.
    pub fn kill_process(&mut self) -> u64 {
        let old_pid = self.current_pid;

        // Re-parent all live children to PID 1 before becoming zombie.
        if let Some(old) = old_pid {
            self.reparent_orphans_to_init(old);
        }

        // Mark current process as Zombie (even if it was Running)
        if let Some(old) = old_pid {
            if let Some(ref mut proc) = self.processes[old as usize] {
                proc.state = ProcessState::Zombie;
                #[cfg(DEBUG_KERNEL)]
                {
                    crate::serial::write_str("[SCHED] KILLED PID=");
                    crate::serial::write_u64(old);
                    crate::serial::write_nl();
                }
            }
        }

        // Find next Ready process (skips idle, skips zombies)
        let next_pid = if let Some(old) = old_pid {
            self.find_next_ready(old)
        } else {
            self.find_next_ready(0)
        };

        let new_pid = next_pid.unwrap_or(self.idle_pid);

        if let Some(ref mut proc) = self.processes[new_pid as usize] {
            proc.state = ProcessState::Running;
        }

        self.current_pid = Some(new_pid);

        self.processes[new_pid as usize]
            .as_ref()
            .map(|p| p.stack_pointer)
            .unwrap_or(0)
    }

    pub fn current_pid(&self) -> Option<Pid> {
        self.current_pid
    }

    pub fn current_process(&self) -> Option<&Process> {
        if let Some(pid) = self.current_pid {
            self.processes[pid as usize].as_ref()
        } else {
            None
        }
    }

    pub fn processes(&self) -> &[Option<Process>; MAX_PROCESSES] {
        &self.processes
    }

    pub fn processes_mut(&mut self) -> &mut [Option<Process>; MAX_PROCESSES] {
        &mut self.processes
    }

    /// Check if `child_pid` is a child of `parent_pid`.
    pub fn is_child_of(&self, child_pid: Pid, parent_pid: Pid, parent_gen: u32) -> bool {
        if let Some(Some(proc)) = self.processes.get(child_pid as usize) {
            proc.parent_pid == Some(parent_pid) && proc.parent_generation == parent_gen
        } else {
            false
        }
    }

    /// Find any Zombie child (for waitpid(-1)).
    /// Returns (child_pid, parent_pid, exit_code) if found.
    /// Matches both PID and generation to prevent cross-family reaping.
    pub fn find_any_zombie_child(&self) -> Option<(Pid, Pid, u64)> {
        let parent = self.current_pid?;
        let parent_gen = self.generations.get(parent as usize).copied().unwrap_or(0);
        for i in 0..MAX_PROCESSES as u64 {
            if let Some(Some(proc)) = self.processes.get(i as usize) {
                if proc.parent_pid == Some(parent)
                    && proc.parent_generation == parent_gen
                    && proc.state == ProcessState::Zombie
                {
                    return Some((proc.pid, parent, proc.exit_code));
                }
            }
        }
        None
    }

    /// Remove a process slot (set to None).
    /// Used after waitpid collects a zombie's exit code.
    pub fn reap_zombie(&mut self, pid: Pid) {
        crate::serial::write_str("[SCHED] Reaped PID=");
        crate::serial::write_u64(pid);
        crate::serial::write_nl();
        self.processes[pid as usize] = None;
    }

    /// Re-parent all live children of `old_parent` to PID 1 (init/reaper).
    /// Called before marking a process as Zombie, so orphans are not leaked.
    /// Also sets the children's `parent_generation` to PID 1's generation (always 1).
    pub fn reparent_orphans_to_init(&mut self, old_parent: Pid) {
        let init_gen = 1; // PID 1 generation is always 1
        for i in 0..MAX_PROCESSES as u64 {
            if let Some(Some(proc)) = self.processes.get_mut(i as usize) {
                if proc.parent_pid == Some(old_parent) {
                    proc.parent_pid = Some(1);
                    proc.parent_generation = init_gen;
                    crate::serial::write_str("[SCHED] Re-parented PID ");
                    crate::serial::write_u64(i);
                    crate::serial::write_str(" to init (PID 1)");
                    crate::serial::write_nl();
                }
            }
        }
    }
}
