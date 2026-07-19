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

use super::process::{Process, ProcessState, Pid, MAX_PROCESSES};

/// Time quantum in ticks (5 ticks = 50ms at 100 Hz).
const DEFAULT_QUANTUM: u64 = 5;

/// Global scheduler state.
pub static SCHEDULER: spin::Lazy<spin::Mutex<Scheduler>> = spin::Lazy::new(|| {
    spin::Mutex::new(Scheduler::new())
});

pub struct Scheduler {
    processes: [Option<Process>; MAX_PROCESSES],
    current_pid: Option<Pid>,
    tick_counter: u64,
    quantum: u64,
    idle_pid: Pid,
    next_pid: Pid,
}

impl Scheduler {
    fn new() -> Self {
        const NONE: Option<Process> = None;
        Scheduler {
            processes: [NONE; MAX_PROCESSES],
            current_pid: None,
            tick_counter: 0,
            quantum: DEFAULT_QUANTUM,
            idle_pid: 0,
            next_pid: 1,
        }
    }

    pub fn init(&mut self) {
        for slot in self.processes.iter_mut() {
            *slot = None;
        }
        self.current_pid = None;
        self.tick_counter = 0;
        self.next_pid = 1;
    }

    pub fn spawn(&mut self, entry_phys: u64) -> Option<Pid> {
        let pid = self.next_pid;
        if pid >= MAX_PROCESSES as u64 {
            return None;
        }
        self.next_pid += 1;

        let process = Process::new_kernel(pid, entry_phys);
        self.processes[pid as usize] = Some(process);

        crate::serial::write_str("[SCHED] Spawned process ");
        crate::serial::write_u64(pid);
        crate::serial::write_nl();

        Some(pid)
    }

    pub fn spawn_idle(&mut self, entry_phys: u64) -> Pid {
        let pid = 0;
        let mut process = Process::new_kernel(pid, entry_phys);
        process.state = ProcessState::Ready;
        self.processes[pid as usize] = Some(process);
        self.idle_pid = pid;

        crate::serial::write_str("[SCHED] Idle process at PID 0\n");
        pid
    }

    pub fn spawn_user(&mut self, user_rip: u64, user_rsp: u64, pml4: u64) -> Option<Pid> {
        let pid = self.next_pid;
        if pid >= MAX_PROCESSES as u64 {
            return None;
        }
        self.next_pid += 1;

        let process = Process::new_user(pid, user_rip, user_rsp, pml4);
        self.processes[pid as usize] = Some(process);

        crate::serial::write_str("[SCHED] Spawned user process ");
        crate::serial::write_u64(pid);
        crate::serial::write_nl();

        Some(pid)
    }

    pub fn on_tick(&mut self) -> u64 {
        self.tick_counter += 1;
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
                    ProcessState::Zombie => crate::serial::write_str("Zombie"),
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

    pub fn current_pid(&self) -> Option<Pid> {
        self.current_pid
    }

    pub fn get_entry_addr(&self) -> Option<u64> {
        if let Some(pid) = self.current_pid {
            if let Some(ref proc) = self.processes[pid as usize] {
                return Some(proc.entry_addr);
            }
        }
        None
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
}
