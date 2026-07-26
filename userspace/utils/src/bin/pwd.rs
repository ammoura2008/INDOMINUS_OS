#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { sys::exit(1); }

#[no_mangle]
pub extern "C" fn _start(_argc: u64, _argv: u64) -> ! {
    let mut buf = [0u8; 256];
    let n = sys::getcwd(&mut buf);
    if !sys::is_error(n) && n > 0 {
        sys::write(1, &buf[..n as usize]);
    }
    sys::write(1, b"\n");
    sys::exit(0);
}
