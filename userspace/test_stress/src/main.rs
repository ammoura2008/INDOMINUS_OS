#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::write(2, b"PANIC in test!\n");
    sys::exit(1);
}

static mut PASSED: u32 = 0;
static mut FAILED: u32 = 0;
static mut TOTAL: u32 = 0;

fn write_num(n: u32) {
    let mut buf = [0u8; 12];
    let mut i = buf.len();
    let mut val = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        if val == 0 { break; }
    }
    sys::write(1, &buf[i..]);
}

fn ok(name: &[u8], passed: bool) {
    unsafe { TOTAL += 1; }
    sys::write(1, b"  ");
    sys::write(1, name);
    if passed {
        unsafe { PASSED += 1; }
        sys::write(1, b" ... OK\n");
    } else {
        unsafe { FAILED += 1; }
        sys::write(1, b" ... FAIL\n");
    }
}

fn cleanup_files() {
    let files: &[&[u8]] = &[
        b"/stress_a\0", b"/stress_b\0", b"/test_stress.tmp\0",
        b"/lseek_test.txt\0", b"/chdir_test.txt\0", b"/rw_cycle.txt\0",
        b"/append_test.txt\0", b"/dup2_test.txt\0", b"/dup2_test2.txt\0",
    ];
    for f in files {
        sys::unlink(f.as_ptr() as u64);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test categories
// ═══════════════════════════════════════════════════════════════════════════════

fn test_invalid_fd_read() {
    sys::write(1, b"\n== Invalid FD reads ==\n");
    let mut buf = [0u8; 64];

    let r = sys::read(255, &mut buf);
    ok(b"read(fd=255) -> EBADF", sys::is_error(r));

    let r = sys::read(100, &mut buf);
    ok(b"read(fd=100) never-opened -> EBADF", sys::is_error(r));
}

fn test_invalid_fd_write() {
    sys::write(1, b"\n== Invalid FD writes ==\n");
    let data = b"test";

    let r = sys::write(255, data);
    ok(b"write(fd=255) -> EBADF", sys::is_error(r));

    let r = sys::write(99, data);
    ok(b"write(fd=99) -> EBADF", sys::is_error(r));
}

fn test_double_close() {
    sys::write(1, b"\n== Double close ==\n");

    let fd = sys::open("/test_stress.tmp\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        sys::close(fd as u64);
        let r = sys::close(fd as u64);
        ok(b"double close -> EBADF", sys::is_error(r));
    } else {
        ok(b"double close -> EBADF (skip: open failed)", false);
    }
}

fn test_close_bad_fd() {
    sys::write(1, b"\n== Close bad FDs ==\n");

    ok(b"close(fd=255) -> EBADF", sys::is_error(sys::close(255)));
    ok(b"close(fd=99) -> EBADF", sys::is_error(sys::close(99)));
}

fn test_invalid_flags() {
    sys::write(1, b"\n== Invalid open flags ==\n");

    ok(b"open(O_RDONLY|O_WRONLY=0x03) -> EINVAL",
       sys::is_error(sys::open("/tmp/bad\0", 0x03)));

    ok(b"open(0xFF00) -> EINVAL",
       sys::is_error(sys::open("/tmp/bad\0", 0xFF00)));
}

fn test_missing_files() {
    sys::write(1, b"\n== Missing files ==\n");

    ok(b"open(/nonexistent) -> ENOENT",
       sys::is_error(sys::open("/nonexistent_file.txt\0", sys::O_RDONLY)));

    ok(b"open(/a/b/c/d/e/f.txt) -> ENOENT",
       sys::is_error(sys::open("/a/b/c/d/e/f.txt\0", sys::O_RDONLY)));
}

fn test_nested_directories() {
    sys::write(1, b"\n== Nested directories ==\n");

    ok(b"mkdir /stress", !sys::is_error(sys::mkdir("/stress\0")));
    ok(b"mkdir /stress/a", !sys::is_error(sys::mkdir("/stress/a\0")));
    ok(b"mkdir /stress/a/b", !sys::is_error(sys::mkdir("/stress/a/b\0")));
    ok(b"mkdir /stress/a/b/c", !sys::is_error(sys::mkdir("/stress/a/b/c\0")));

    let fd = sys::open("/stress\0", sys::O_RDONLY);
    if !sys::is_error(fd) {
        let mut buf = [0u8; 512];
        let n = sys::readdir(fd as u64, &mut buf);
        sys::close(fd as u64);
        ok(b"ls /stress has entries", !sys::is_error(n) && n > 0);
    } else {
        ok(b"ls /stress has entries", false);
    }

    let fd = sys::open("/stress/a/b/c/file.txt\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        sys::close(fd as u64);
        ok(b"touch /stress/a/b/c/file.txt", true);
    } else {
        ok(b"touch /stress/a/b/c/file.txt", false);
    }

    ok(b"cd /stress/a/b/c", !sys::is_error(sys::chdir("/stress/a/b/c\0")));

    let mut buf = [0u8; 256];
    let n = sys::getcwd(&mut buf);
    ok(b"pwd = /stress/a/b/c",
       !sys::is_error(n) && n > 0 && buf[..n as usize] == *b"/stress/a/b/c");

    ok(b"cd /", !sys::is_error(sys::chdir("/\0")));
}

fn test_fork_stress() {
    sys::write(1, b"\n== Fork stress ==\n");

    // Fork 5 children
    let mut pids = [0i64; 5];
    let mut fork_count = 0u32;
    for i in 0..5usize {
        let pid = sys::fork();
        if !sys::is_error(pid) {
            if pid == 0 {
                sys::exit(0);
            }
            pids[i] = pid;
            fork_count += 1;
        }
    }
    ok(b"fork 5 children", fork_count == 5);

    for &p in &pids {
        if p > 0 { sys::waitpid(p as u64, 0); }
    }

    // Fork + waitpid blocking
    let pid = sys::fork();
    if !sys::is_error(pid) {
        if pid == 0 { sys::exit(42); }
        let status = sys::waitpid(pid as u64, 0);
        ok(b"fork+waitpid exit_code=42", status == 42);
    } else {
        ok(b"fork+waitpid exit_code=42", false);
    }

    // Fork + waitpid WNOHANG
    let pid = sys::fork();
    if !sys::is_error(pid) {
        if pid == 0 {
            for _ in 0..50 { sys::yield_now(); }
            sys::exit(0);
        }
        // WNOHANG - child may still be running
        let _ = sys::waitpid(pid as u64, 1);
        ok(b"fork+waitpid WNOHANG (no hang)", true);
        sys::waitpid(pid as u64, 0);
    } else {
        ok(b"fork+waitpid WNOHANG (no hang)", false);
    }
}

fn test_pipe_edge_cases() {
    sys::write(1, b"\n== Pipe edge cases ==\n");

    // Create + close
    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        sys::close(rd);
        sys::close(wr);
        ok(b"pipe create + close", true);
    } else {
        ok(b"pipe create + close", false);
    }

    // Read from write end
    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        let mut buf = [0u8; 16];
        let result = sys::read(wr, &mut buf);
        sys::close(rd);
        sys::close(wr);
        ok(b"read from write end -> EBADF", sys::is_error(result));
    } else {
        ok(b"read from write end -> EBADF (skip)", false);
    }

    // Write to read end
    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        let result = sys::write(rd, b"test");
        sys::close(rd);
        sys::close(wr);
        ok(b"write to read end -> EBADF", sys::is_error(result));
    } else {
        ok(b"write to read end -> EBADF (skip)", false);
    }

    // Pipe EOF
    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        sys::close(wr);
        let mut buf = [0u8; 16];
        let n = sys::read(rd, &mut buf);
        sys::close(rd);
        ok(b"pipe EOF -> read returns 0", n == 0);
    } else {
        ok(b"pipe EOF -> read returns 0 (skip)", false);
    }

    // Write after reader closed
    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        sys::close(rd);
        let result = sys::write(wr, b"test");
        sys::close(wr);
        ok(b"write after reader close (no panic)", true);
    } else {
        ok(b"write after reader close (no panic) (skip)", false);
    }

    // Fill pipe
    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        let data = [0xABu8; 256];
        let mut total: u64 = 0;
        for _ in 0..32 {
            let n = sys::write(wr, &data);
            if sys::is_error(n) || n == 0 { break; }
            total += n as u64;
        }
        sys::close(rd);
        sys::close(wr);
        ok(b"fill pipe (wrote bytes)", total > 0);
    } else {
        ok(b"fill pipe (wrote bytes)", false);
    }
}

fn test_chdir_edge_cases() {
    sys::write(1, b"\n== chdir edge cases ==\n");

    let fd = sys::open("/chdir_test.txt\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        sys::close(fd as u64);
        let r = sys::chdir("/chdir_test.txt\0");
        ok(b"chdir to file -> ENOTDIR", sys::is_error(r));
    } else {
        ok(b"chdir to file -> ENOTDIR (skip)", false);
    }

    ok(b"chdir to nonexistent -> ENOENT",
       sys::is_error(sys::chdir("/nonexistent_dir\0")));

    let _ = sys::chdir("/\0");
    let _ = sys::chdir("/..\0");
    let mut buf = [0u8; 256];
    let n = sys::getcwd(&mut buf);
    ok(b"cd .. from / stays at /",
       !sys::is_error(n) && n <= 1);
}

fn test_exec_edge_cases() {
    sys::write(1, b"\n== exec edge cases ==\n");

    ok(b"exec(/nonexistent) -> ENOENT",
       sys::is_error(sys::execve("/nonexistent.elf\0", 0, core::ptr::null())));

    ok(b"exec(null) -> EFAULT",
       sys::is_error(sys::execve("\0", 0, core::ptr::null())));
}

fn test_lseek_edge_cases() {
    sys::write(1, b"\n== lseek edge cases ==\n");

    let fd = sys::open("/lseek_test.txt\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        let n = sys::write(fd as u64, b"hello world");
        let pos = sys::lseek(fd as u64, 0);
        sys::close(fd as u64);
        ok(b"lseek(0) returns 0", pos == 0);
    } else {
        ok(b"lseek(0) returns 0 (skip)", false);
    }

    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        let result = sys::lseek(rd, 0);
        sys::close(rd);
        sys::close(wr);
        ok(b"lseek on pipe -> ESPIPE", sys::is_error(result));
    } else {
        ok(b"lseek on pipe -> ESPIPE (skip)", false);
    }
}

fn test_multi_process_pipes() {
    sys::write(1, b"\n== Multi-process pipes ==\n");

    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        let pid = sys::fork();
        if !sys::is_error(pid) {
            if pid == 0 {
                sys::close(rd);
                sys::write(wr, b"hello from child");
                sys::close(wr);
                sys::exit(0);
            }
            sys::close(wr);
            let mut buf = [0u8; 64];
            let n = sys::read(rd, &mut buf);
            sys::close(rd);
            sys::waitpid(pid as u64, 0);
            ok(b"fork+pipe+write+read",
               n > 0 && buf[..n as usize] == *b"hello from child");
        } else {
            ok(b"fork+pipe+write+read", false);
        }
    } else {
        ok(b"fork+pipe+write+read", false);
    }

    // Two children write to same pipe
    let r = sys::pipe();
    if !sys::is_error(r) {
        let rd = (r >> 32) as u64;
        let wr = (r & 0xFFFF_FFFF) as u64;
        let pid1 = sys::fork();
        if !sys::is_error(pid1) {
            if pid1 == 0 { sys::close(rd); sys::write(wr, b"A"); sys::close(wr); sys::exit(0); }
        }
        let pid2 = sys::fork();
        if !sys::is_error(pid2) {
            if pid2 == 0 { sys::close(rd); sys::write(wr, b"B"); sys::close(wr); sys::exit(0); }
        }
        sys::close(wr);
        let mut buf = [0u8; 64];
        let mut total: i64 = 0;
        loop {
            let n = sys::read(rd, &mut buf[total as usize..]);
            if sys::is_error(n) || n == 0 { break; }
            total += n;
            if total >= 2 { break; }
        }
        sys::close(rd);
        if pid1 > 0 { sys::waitpid(pid1 as u64, 0); }
        if pid2 > 0 { sys::waitpid(pid2 as u64, 0); }
        ok(b"2 children write to same pipe", total == 2);
    } else {
        ok(b"2 children write to same pipe (skip)", false);
    }
}

fn test_dup2_edge_cases() {
    sys::write(1, b"\n== dup2 edge cases ==\n");

    let fd = sys::open("/dup2_test.txt\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        let r = sys::dup2(fd as u64, fd as u64);
        sys::close(fd as u64);
        ok(b"dup2(fd, fd) same -> returns fd", r as u64 == fd as u64);
    } else {
        ok(b"dup2(fd, fd) same -> returns fd (skip)", false);
    }

    let fd = sys::open("/dup2_test2.txt\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        let r = sys::dup2(fd as u64, 255);
        sys::close(fd as u64);
        ok(b"dup2(fd, 255) -> EBADF", sys::is_error(r));
    } else {
        ok(b"dup2(fd, 255) -> EBADF (skip)", false);
    }
}

fn test_file_write_read_cycle() {
    sys::write(1, b"\n== File write/read cycle ==\n");

    let fd = sys::open("/rw_cycle.txt\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        let data = b"Indominus OS stress test data!";
        let _ = sys::write(fd as u64, data);
        sys::close(fd as u64);

        let fd2 = sys::open("/rw_cycle.txt\0", sys::O_RDONLY);
        if !sys::is_error(fd2) {
            let mut buf = [0u8; 64];
            let n = sys::read(fd2 as u64, &mut buf);
            sys::close(fd2 as u64);
            ok(b"write+read back",
               n as usize == data.len() && buf[..n as usize] == *data);
        } else {
            ok(b"write+read back (reopen failed)", false);
        }
    } else {
        ok(b"write+read back (open failed)", false);
    }

    // Append mode
    let fd = sys::open("/append_test.txt\0", sys::O_CREAT | sys::O_TRUNC);
    if !sys::is_error(fd) {
        let _ = sys::write(fd as u64, b"hello");
        sys::close(fd as u64);

        let fd2 = sys::open("/append_test.txt\0", sys::O_WRONLY | sys::O_APPEND);
        if !sys::is_error(fd2) {
            let _ = sys::write(fd2 as u64, b" world");
            sys::close(fd2 as u64);

            let fd3 = sys::open("/append_test.txt\0", sys::O_RDONLY);
            if !sys::is_error(fd3) {
                let mut buf = [0u8; 64];
                let n = sys::read(fd3 as u64, &mut buf);
                sys::close(fd3 as u64);
                ok(b"append mode -> 'hello world'", n == 11 && buf[..11] == *b"hello world");
            } else {
                ok(b"append mode -> 'hello world' (reopen3 failed)", false);
            }
        } else {
            ok(b"append mode -> 'hello world' (append open failed)", false);
        }
    } else {
        ok(b"append mode -> 'hello world' (open failed)", false);
    }

    // Unlink
    let r = sys::unlink(b"/rw_cycle.txt\0".as_ptr() as u64);
    if !sys::is_error(r) {
        let fd = sys::open("/rw_cycle.txt\0", sys::O_RDONLY);
        ok(b"unlink file -> gone", sys::is_error(fd));
        if !sys::is_error(fd) { sys::close(fd as u64); }
    } else {
        ok(b"unlink file -> gone (unlink failed)", false);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Entry point
// ═══════════════════════════════════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn _start(_argc: u64, _argv: u64) -> ! {
    sys::write(1, b"\n");
    sys::write(1, b"========================================\n");
    sys::write(1, b"  INDOMINUS OS - Phase 12.12 Stress Test\n");
    sys::write(1, b"  Deliberately trying to break kernel\n");
    sys::write(1, b"========================================\n");

    cleanup_files();

    test_invalid_fd_read();
    test_invalid_fd_write();
    test_double_close();
    test_close_bad_fd();
    test_invalid_flags();
    test_missing_files();
    test_nested_directories();
    test_fork_stress();
    test_pipe_edge_cases();
    test_chdir_edge_cases();
    test_exec_edge_cases();
    test_lseek_edge_cases();
    test_multi_process_pipes();
    test_dup2_edge_cases();
    test_file_write_read_cycle();

    sys::write(1, b"\n========================================\n");
    sys::write(1, b"  RESULTS: ");
    write_num(unsafe { PASSED });
    sys::write(1, b" passed, ");
    write_num(unsafe { FAILED });
    sys::write(1, b" failed, ");
    write_num(unsafe { TOTAL });
    sys::write(1, b" total\n");

    if unsafe { FAILED } == 0 {
        sys::write(1, b"  >>> ALL TESTS PASSED <<<\n");
    } else {
        sys::write(1, b"  >>> SOME TESTS FAILED <<<\n");
    }
    sys::write(1, b"========================================\n\n");

    sys::exit(if unsafe { FAILED } == 0 { 0 } else { 1 });
}
