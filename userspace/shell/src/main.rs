#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    sys::write(1, b"[SHELL PANIC] ");
    if let Some(loc) = info.location() {
        sys::write(1, loc.file().as_bytes());
        sys::write(1, b":");
        let line = loc.line();
        let mut buf = [0u8; 10];
        let mut i = buf.len();
        let mut val = line;
        if val == 0 { buf[0] = b'0'; i = 0; }
        while val > 0 { i -= 1; buf[i] = b'0' + (val % 10) as u8; val /= 10; }
        sys::write(1, &buf[i..]);
    }
    sys::write(1, b" msg=");
    panic_msg(info.message());
    sys::write(1, b"\n");
    sys::exit(1);
}

fn panic_msg(msg: core::panic::PanicMessage<'_>) {
    use core::fmt::Write;
    struct PanicWriter;
    impl Write for PanicWriter {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            sys::write(1, s.as_bytes());
            Ok(())
        }
    }
    let _ = write!(PanicWriter, "{}", &msg);
}

#[no_mangle]
pub extern "C" fn _start(argc: u64, argv: u64) -> ! {
    indosh_lib::shell_main(argc, argv)
}
