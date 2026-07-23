//! # Init Process (PID 1)
//!
//! The init process is the ancestor of all user processes.
//! Its primary job is to reap orphaned zombie processes — children whose
//! parent exited before calling waitpid. Without init, these zombies would
//! permanently leak their kernel stack, PML4, user pages, and process table slot.
//!
//! ## Design
//!
//! Init is a **kernel-mode** process (Ring 0). It cannot make syscalls directly.
//! Instead, it directly calls scheduler functions to find and reap zombies.
//!
//! ```text
//! loop:
//!   lock scheduler
//!   find zombie children (parent_pid == 1)
//!   reap each zombie (set slot to None → Drop frees resources)
//!   unlock scheduler
//!   hlt (yield until next timer tick)
//! ```

/// Init process main function. Runs forever reaping orphaned zombies.
///
/// # Safety
/// Called from kernel context only. Accesses the global scheduler.
pub fn init_main() -> ! {
    crate::serial::write_str("[INIT] Init process (PID 1) running — reaper active\n");

    loop {
        // Lock the scheduler and reap all zombie children.
        // Releasing the lock before HLT allows the timer interrupt to
        // schedule other processes while we sleep.
        {
            let mut sched = crate::process::scheduler::SCHEDULER.lock();
            let mut reaped = 0u64;

            // Collect zombie PIDs first (can't modify while iterating)
            let mut zombies: crate::alloc::vec::Vec<crate::process::Pid> =
                crate::alloc::vec::Vec::new();

            for i in 0..crate::process::MAX_PROCESSES as u64 {
                if let Some(Some(proc)) = sched.processes().get(i as usize) {
                    if proc.parent_pid == Some(1)
                        && proc.state == crate::process::ProcessState::Zombie
                    {
                        zombies.push(proc.pid);
                    }
                }
            }

            for pid in zombies {
                sched.reap_zombie(pid);
                reaped += 1;
            }

            if reaped > 0 {
                crate::serial::write_str("[INIT] Reaped ");
                crate::serial::write_u64(reaped);
                crate::serial::write_str(" zombie(s)\n");
            }
        }

        // Yield until next timer tick — the scheduler will pick us again
        // when no other Ready process is available.
        unsafe {
            core::arch::asm!("hlt", options(nostack, nomem));
        }
    }
}
