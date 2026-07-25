#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::exit(1);
}

fn write_out(s: &str) {
    sys::write(1, s.as_bytes());
}

fn report(test: &str, passed: bool) {
    if passed {
        write_out("[STRESS] PASS: ");
    } else {
        write_out("[STRESS] FAIL: ");
    }
    write_out(test);
    write_out("\n");
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;

    // ── Test 1: Invalid syscall numbers return errors ──
    {
        let mut ok = true;
        // Use an invalid operation: dup2 with bad fds
        let ret = sys::dup2(255, 255);
        if sys::is_error(ret) {
            // Expected: error for invalid fd
        } else {
            ok = false;
        }
        report("dup2 invalid fd returns error", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 2: Write to invalid fd returns error ──
    {
        let ret = sys::write(255, b"test");
        let ok = sys::is_error(ret);
        report("write to invalid fd returns error", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 3: Read from invalid fd returns error ──
    {
        let mut buf = [0u8; 4];
        let ret = sys::read(255, &mut buf);
        let ok = sys::is_error(ret);
        report("read from invalid fd returns error", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 4: Close invalid fd returns error ──
    {
        let ret = sys::close(255);
        let ok = sys::is_error(ret);
        report("close invalid fd returns error", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 5: Open non-existent file returns error ──
    {
        let ret = sys::open("/nonexistent", sys::O_RDONLY);
        let ok = sys::is_error(ret);
        report("open non-existent file returns error", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 6: Fork/exec/exit cycle (10 iterations) ──
    {
        let mut ok = true;
        for _ in 0..10 {
            let ret = sys::fork();
            if ret == 0 {
                // Child: exit immediately
                sys::exit(0);
            } else if sys::is_error(ret) {
                ok = false;
                break;
            } else {
                // Parent: wait for child
                let wait_ret = sys::waitpid(ret as u64);
                if sys::is_error(wait_ret) {
                    ok = false;
                    break;
                }
            }
        }
        report("fork/exec/exit cycle (10x)", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 7: Pipe create + close ──
    {
        let ret = sys::pipe();
        let ok = if !sys::is_error(ret) {
            let read_fd = (ret >> 32) as u64;
            let write_fd = (ret & 0xFFFF_FFFF) as u64;
            let c1 = sys::close(read_fd);
            let c2 = sys::close(write_fd);
            !sys::is_error(c1) && !sys::is_error(c2)
        } else {
            false
        };
        report("pipe create + close", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 8: Pipe write + read ──
    {
        let ret = sys::pipe();
        let ok = if !sys::is_error(ret) {
            let read_fd = (ret >> 32) as u64;
            let write_fd = (ret & 0xFFFF_FFFF) as u64;
            let msg = b"hello pipe";
            let w = sys::write(write_fd, msg);
            sys::close(write_fd);
            if sys::is_error(w) {
                sys::close(read_fd);
                false
            } else {
                let mut buf = [0u8; 32];
                let r = sys::read(read_fd, &mut buf);
                sys::close(read_fd);
                !sys::is_error(r) && r as usize == msg.len() && &buf[..r as usize] == msg
            }
        } else {
            false
        };
        report("pipe write + read", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 9: Dup2 to stdout, write, then restore ──
    {
        let ret = sys::pipe();
        let ok = if !sys::is_error(ret) {
            let read_fd = (ret >> 32) as u64;
            let write_fd = (ret & 0xFFFF_FFFF) as u64;
            // dup2 write_fd to fd 10 (safe fd)
            let d = sys::dup2(write_fd, 10);
            sys::close(write_fd);
            if sys::is_error(d) {
                sys::close(read_fd);
                false
            } else {
                let msg = b"dup2test";
                let w = sys::write(10, msg);
                sys::close(10);
                let mut buf = [0u8; 32];
                let r = sys::read(read_fd, &mut buf);
                sys::close(read_fd);
                !sys::is_error(w) && !sys::is_error(r) && r as usize == msg.len()
            }
        } else {
            false
        };
        report("dup2 + write + read", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 10: Lseek on pipe returns error ──
    {
        let ret = sys::pipe();
        let ok = if !sys::is_error(ret) {
            let read_fd = (ret >> 32) as u64;
            let write_fd = (ret & 0xFFFF_FFFF) as u64;
            let s = sys::lseek(read_fd, 0);
            sys::close(read_fd);
            sys::close(write_fd);
            sys::is_error(s) // lseek on pipe should fail
        } else {
            false
        };
        report("lseek on pipe returns error", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 11: Multiple fork children concurrently ──
    {
        let mut ok = true;
        let mut pids = [0i64; 5];
        for i in 0..5usize {
            let ret = sys::fork();
            if ret == 0 {
                // Child: yield a few times then exit
                for _ in 0..5 { sys::yield_now(); }
                sys::exit(i as u64);
            } else if sys::is_error(ret) {
                ok = false;
                break;
            } else {
                pids[i] = ret;
            }
        }
        // Wait for all children
        if ok {
            for i in 0..5usize {
                if pids[i] != 0 {
                    let w = sys::waitpid(pids[i] as u64);
                    if sys::is_error(w) {
                        ok = false;
                        break;
                    }
                }
            }
        }
        report("5 concurrent fork children", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 12: Yield stress (1000 yields) ──
    {
        for _ in 0..1000 {
            sys::yield_now();
        }
        report("1000 yields without crash", true);
        passed += 1;
    }

    // ── Test 13: Getpid consistency ──
    {
        let pid1 = sys::getpid();
        let pid2 = sys::getpid();
        let ok = pid1 != 0 && pid1 == pid2;
        report("getpid consistent", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 14: Close fd 0 (stdin) then write ──
    {
        let ret = sys::pipe();
        let ok = if !sys::is_error(ret) {
            let read_fd = (ret >> 32) as u64;
            let write_fd = (ret & 0xFFFF_FFFF) as u64;
            // Close write end, then read should get EOF (0 bytes)
            sys::close(write_fd);
            let mut buf = [0u8; 4];
            let r = sys::read(read_fd, &mut buf);
            sys::close(read_fd);
            r == 0 // EOF
        } else {
            false
        };
        report("pipe EOF on close write end", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 15: Readdir on root ──
    {
        let fd = sys::open("/disk", sys::O_RDONLY);
        let ok = if !sys::is_error(fd) {
            let mut buf = [0u8; 256];
            let r = sys::readdir(fd as u64, &mut buf);
            sys::close(fd as u64);
            !sys::is_error(r) && r > 0
        } else {
            false
        };
        report("readdir /disk", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Summary ──
    write_out("\n[STRESS] ====== SUMMARY ======\n");
    write_out("[STRESS] passed=");
    // Simple integer to string
    let mut n = passed;
    let mut digits = [0u8; 10];
    let mut i = 10;
    if n == 0 {
        i = 9;
        digits[9] = b'0';
    }
    while n > 0 {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    write_out(core::str::from_utf8(&digits[i..]).unwrap());
    write_out(" failed=");
    n = failed;
    i = 10;
    if n == 0 {
        i = 9;
        digits[9] = b'0';
    }
    while n > 0 {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    write_out(core::str::from_utf8(&digits[i..]).unwrap());
    write_out("\n[STRESS] ========================\n");

    if failed == 0 {
        write_out("[STRESS] ALL TESTS PASSED\n");
    } else {
        write_out("[STRESS] SOME TESTS FAILED\n");
    }

    sys::exit(0);
}
