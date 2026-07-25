#![no_std]

use indo_syscall as sys;

pub fn init_main() -> ! {
    sys::write(1, b"[INIT] Indominus OS init started\n");

    // Main loop: reap orphaned children
    loop {
        // Try to reap any zombie children (non-blocking)
        let result = sys::waitpid(0);
        if sys::is_error(result) {
            // No children to reap — yield and try again
            sys::yield_now();
        }
    }
}
