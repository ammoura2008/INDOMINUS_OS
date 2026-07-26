#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { sys::exit(1); }

#[no_mangle]
pub extern "C" fn _start(argc: u64, argv: u64) -> ! {
    let args = unsafe { core::slice::from_raw_parts(argv as *const u64, argc as usize) };
    let mut first = true;
    for i in 0..argc as usize {
        let ptr = args[i] as *const u8;
        if ptr.is_null() { continue; }
        if !first { sys::write(1, b" "); }
        first = false;
        let mut len = 0;
        while len < 4096 && unsafe { *ptr.add(len) } != 0 { len += 1; }
        if len > 0 {
            let s = unsafe { core::slice::from_raw_parts(ptr, len) };
            sys::write(1, s);
        }
    }
    sys::write(1, b"\n");
    sys::exit(0);
}
