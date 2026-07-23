#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::exit(1);
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
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
