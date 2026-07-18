//! # Idle Process
//!
//! The idle process runs when no other process is Ready.
//! It executes `hlt` in a loop, putting the CPU into a low-power state.

pub fn idle_main() -> ! {
    crate::serial::write_str("[IDLE] Idle process running\n");

    loop {
        unsafe {
            core::arch::asm!("hlt", options(nostack, nomem));
        }
    }
}
