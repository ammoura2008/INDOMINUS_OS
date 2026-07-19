//! # Test Tasks
//!
//! Simple kernel-mode tasks for validating the scheduler and context switch.
//! Each task prints a marker on entry and loops forever.

/// Task A: prints marker, then HLT-loops.
pub fn task_a() -> ! {
    crate::serial::write_str("[TASK A] running\n");
    loop {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem)); }
    }
}

/// Task B: prints marker, then HLT-loops.
pub fn task_b() -> ! {
    crate::serial::write_str("[TASK B] running\n");
    loop {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem)); }
    }
}
