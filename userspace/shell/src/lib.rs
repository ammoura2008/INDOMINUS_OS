#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::exit(1);
}

/// Write a prompt and read a line from stdin
fn read_line(buf: &mut [u8]) -> usize {
    sys::write(1, b"$ ");
    let n = sys::read(0, buf);
    if sys::is_error(n) {
        return 0;
    }
    n as usize
}

/// Simple string comparison
fn str_eq(a: &str, b: &[u8]) -> bool {
    a.as_bytes() == b
}

/// Trim trailing newline/carriage return
fn trim_end(buf: &[u8]) -> &[u8] {
    let mut len = buf.len();
    while len > 0 && (buf[len - 1] == b'\n' || buf[len - 1] == b'\r' || buf[len - 1] == b' ') {
        len -= 1;
    }
    &buf[..len]
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write(1, b"Indominus OS Shell v0.1\n");
    sys::write(1, b"Type 'help' for commands, 'exit' to quit.\n\n");

    let mut buf = [0u8; 256];

    loop {
        let n = read_line(&mut buf);
        if n == 0 {
            continue;
        }

        let line = trim_end(&buf[..n]);

        if line.is_empty() {
            continue;
        }

        // Built-in: help
        if str_eq("help", line) {
            sys::write(1, b"Commands:\n");
            sys::write(1, b"  help     - show this help\n");
            sys::write(1, b"  exit     - exit shell\n");
            sys::write(1, b"  echo     - echo text\n");
            sys::write(1, b"  clear    - clear screen\n");
            continue;
        }

        // Built-in: exit
        if str_eq("exit", line) {
            sys::write(1, b"Goodbye!\n");
            sys::exit(0);
        }

        // Built-in: echo
        if line.starts_with(b"echo ") {
            sys::write(1, &line[5..]);
            sys::write(1, b"\n");
            continue;
        }

        // Built-in: clear (send ANSI escape)
        if str_eq("clear", line) {
            sys::write(1, b"\x1b[2J\x1b[H");
            continue;
        }

        // Unknown command
        sys::write(1, b"Unknown command: ");
        sys::write(1, line);
        sys::write(1, b"\n");
    }
}
