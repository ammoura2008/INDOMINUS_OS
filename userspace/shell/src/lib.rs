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

/// Skip leading whitespace
fn skip_space(buf: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < buf.len() && buf[i] == b' ' {
        i += 1;
    }
    &buf[i..]
}

/// Find first space (argument separator)
fn split_args(line: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i < line.len() && line[i] != b' ' {
        i += 1;
    }
    if i >= line.len() {
        (line, &[])
    } else {
        (&line[..i], &line[i..])
    }
}

/// Print a number in decimal
fn print_decimal(mut n: u64) {
    if n == 0 {
        sys::write(1, b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::write(1, &buf[i..]);
}

/// Handle 'cat <path>' — read file and write to stdout
fn cmd_cat(path: &str) {
    let fd = sys::open(path, sys::O_RDONLY);
    if sys::is_error(fd) {
        sys::write(1, b"cat: ");
        sys::write(1, path.as_bytes());
        sys::write(1, b": file not found\n");
        return;
    }
    let mut buf = [0u8; 4096];
    loop {
        let n = sys::read(fd as u64, &mut buf);
        if sys::is_error(n) || n == 0 {
            break;
        }
        sys::write(1, &buf[..n as usize]);
    }
    sys::close(fd as u64);
}

/// Handle 'ls [path]' — list directory entries
fn cmd_ls(path: &str) {
    let dir_path = if path.is_empty() { "/" } else { path };
    let fd = sys::open(dir_path, sys::O_RDONLY);
    if sys::is_error(fd) {
        sys::write(1, b"ls: ");
        sys::write(1, dir_path.as_bytes());
        sys::write(1, b": directory not found\n");
        return;
    }
    let mut buf = [0u8; 512];
    loop {
        let n = sys::readdir(fd as u64, &mut buf);
        if sys::is_error(n) || n == 0 {
            break;
        }
        let data = &buf[..n as usize];
        let mut i = 0;
        while i < data.len() {
            // Each entry: 1 byte len, then name bytes
            let name_len = data[i] as usize;
            if name_len == 0 || i + 1 + name_len > data.len() {
                break;
            }
            let name = &data[i + 1..i + 1 + name_len];
            sys::write(1, name);
            sys::write(1, b"  ");
            i += 1 + name_len;
        }
        sys::write(1, b"\n");
    }
    sys::close(fd as u64);
}

/// Handle 'exec <path>' — fork and exec a program
fn cmd_exec(path: &str) {
    let pid = sys::fork();
    if sys::is_error(pid) {
        sys::write(1, b"exec: fork failed\n");
        return;
    }
    if pid == 0 {
        // Child: exec the program
        let ret = sys::exec(path);
        // If exec returns, it failed
        sys::write(1, b"exec: ");
        sys::write(1, path.as_bytes());
        sys::write(1, b": exec failed\n");
        sys::exit(1);
    } else {
        // Parent: wait for child
        sys::waitpid(pid as u64);
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write(1, b"Indominus OS Shell v0.3\n");
    sys::write(1, b"Type 'help' for commands, 'exit' to quit.\n\n");

    let mut buf = [0u8; 256];

    loop {
        let n = read_line(&mut buf);
        if n == 0 {
            continue;
        }

        let line = trim_end(&buf[..n]);
        let line = skip_space(line);

        if line.is_empty() {
            continue;
        }

        let (cmd, args) = split_args(line);
        let args = skip_space(args);

        // Built-in: help
        if str_eq("help", cmd) {
            sys::write(1, b"Commands:\n");
            sys::write(1, b"  help          - show this help\n");
            sys::write(1, b"  exit          - exit shell\n");
            sys::write(1, b"  echo <text>   - echo text\n");
            sys::write(1, b"  clear         - clear screen\n");
            sys::write(1, b"  cat <file>    - read and display a file\n");
            sys::write(1, b"  ls [dir]      - list directory contents\n");
            sys::write(1, b"  exec <file>   - execute a program\n");
            sys::write(1, b"  pid           - show current PID\n");
            continue;
        }

        // Built-in: exit
        if str_eq("exit", cmd) {
            sys::write(1, b"Goodbye!\n");
            sys::exit(0);
        }

        // Built-in: echo
        if str_eq("echo", cmd) {
            sys::write(1, args);
            sys::write(1, b"\n");
            continue;
        }

        // Built-in: clear (send ANSI escape)
        if str_eq("clear", cmd) {
            sys::write(1, b"\x1b[2J\x1b[H");
            continue;
        }

        // Built-in: cat
        if str_eq("cat", cmd) {
            if args.is_empty() {
                sys::write(1, b"cat: missing operand\n");
            } else {
                // Convert args bytes to str
                if let Ok(path) = core::str::from_utf8(args) {
                    cmd_cat(path);
                } else {
                    sys::write(1, b"cat: invalid path\n");
                }
            }
            continue;
        }

        // Built-in: ls
        if str_eq("ls", cmd) {
            if let Ok(path) = core::str::from_utf8(args) {
                cmd_ls(path);
            } else {
                sys::write(1, b"ls: invalid path\n");
            }
            continue;
        }

        // Built-in: exec
        if str_eq("exec", cmd) {
            if args.is_empty() {
                sys::write(1, b"exec: missing operand\n");
            } else {
                if let Ok(path) = core::str::from_utf8(args) {
                    cmd_exec(path);
                } else {
                    sys::write(1, b"exec: invalid path\n");
                }
            }
            continue;
        }

        // Built-in: pid
        if str_eq("pid", cmd) {
            sys::write(1, b"PID: ");
            print_decimal(sys::getpid());
            sys::write(1, b"\n");
            continue;
        }

        // Unknown command
        sys::write(1, b"Unknown command: ");
        sys::write(1, cmd);
        sys::write(1, b"\n");
    }
}
