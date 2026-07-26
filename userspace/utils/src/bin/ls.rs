#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { sys::exit(1); }

#[no_mangle]
pub extern "C" fn _start(argc: u64, argv: u64) -> ! {
    let dir_path = if argc > 1 {
        let args = unsafe { core::slice::from_raw_parts(argv as *const u64, argc as usize) };
        let ptr = args[1] as *const u8;
        let mut len = 0;
        while len < 4096 && unsafe { *ptr.add(len) } != 0 { len += 1; }
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(ptr, len)) }
    } else {
        "/"
    };
    let fd = sys::open(dir_path, sys::O_RDONLY);
    if sys::is_error(fd) {
        sys::write(2, b"ls: directory not found\n");
        sys::exit(1);
    }
    let mut buf = [0u8; 512];
    let mut first = true;
    loop {
        let n = sys::readdir(fd as u64, &mut buf);
        if sys::is_error(n) || n == 0 { break; }
        let data = &buf[..n as usize];
        let mut i = 0;
        while i < data.len() {
            let name_len = data[i] as usize;
            if name_len == 0 || i + 1 + name_len > data.len() { break; }
            if !first { sys::write(1, b"  "); }
            first = false;
            sys::write(1, &data[i + 1..i + 1 + name_len]);
            i += 1 + name_len;
        }
    }
    sys::write(1, b"\n");
    sys::close(fd as u64);
    sys::exit(0);
}
