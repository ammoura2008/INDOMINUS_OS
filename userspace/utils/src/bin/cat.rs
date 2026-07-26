#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { sys::exit(1); }

#[no_mangle]
pub extern "C" fn _start(argc: u64, argv: u64) -> ! {
    if argc < 2 {
        sys::write(2, b"cat: missing operand\n");
        sys::exit(1);
    }
    let args = unsafe { core::slice::from_raw_parts(argv as *const u64, argc as usize) };
    for i in 1..argc as usize {
        let ptr = args[i] as *const u8;
        if ptr.is_null() { continue; }
        let mut len = 0;
        while len < 4096 && unsafe { *ptr.add(len) } != 0 { len += 1; }
        if len == 0 { continue; }
        let path = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(ptr, len)) };
        let fd = sys::open(path, sys::O_RDONLY);
        if sys::is_error(fd) {
            sys::write(2, b"cat: ");
            sys::write(2, unsafe { core::slice::from_raw_parts(ptr, len) });
            sys::write(2, b": file not found\n");
            continue;
        }
        let mut buf = [0u8; 4096];
        loop {
            let n = sys::read(fd as u64, &mut buf);
            if sys::is_error(n) || n == 0 { break; }
            sys::write(1, &buf[..n as usize]);
        }
        sys::close(fd as u64);
    }
    sys::exit(0);
}
