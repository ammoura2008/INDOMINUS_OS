#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::exit(1);
}

#[no_mangle]
pub extern "C" fn _start(argc: u64, argv: u64) -> ! {
    indosh_lib::shell_main(argc, argv)
}
