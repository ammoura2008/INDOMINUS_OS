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
        write_out("[FATW] PASS: ");
    } else {
        write_out("[FATW] FAIL: ");
    }
    write_out(test);
    write_out("\n");
}

fn write_file(path: &str, data: &[u8]) -> bool {
    let fd = sys::open(path, sys::O_CREAT | sys::O_WRONLY | sys::O_TRUNC);
    if sys::is_error(fd) {
        write_out("[FATW]   open(O_CREAT|O_WRONLY|O_TRUNC) failed: ");
        return false;
    }
    let n = sys::write(fd as u64, data);
    sys::close(fd as u64);
    if sys::is_error(n) || n as usize != data.len() {
        write_out("[FATW]   write failed or short write\n");
        return false;
    }
    true
}

fn read_file(path: &str, buf: &mut [u8]) -> Result<usize, ()> {
    let fd = sys::open(path, sys::O_RDONLY);
    if sys::is_error(fd) {
        return Err(());
    }
    let n = sys::read(fd as u64, buf);
    sys::close(fd as u64);
    if sys::is_error(n) {
        return Err(());
    }
    Ok(n as usize)
}

fn verify_file(path: &str, expected: &[u8], test_name: &str) -> bool {
    let mut buf = [0u8; 4096];
    match read_file(path, &mut buf) {
        Ok(n) => {
            if n != expected.len() {
                write_out("[FATW]   size mismatch: got ");
                // Write decimal size
                let mut tmp = [0u8; 16];
                let mut v = n;
                let mut i = tmp.len();
                if v == 0 { i -= 1; tmp[i] = b'0'; }
                else { while v > 0 { i -= 1; tmp[i] = b'0' + (v % 10) as u8; v /= 10; } }
                write_out(core::str::from_utf8(&tmp[i..]).unwrap_or("?"));
                write_out(" expected ");
                v = expected.len();
                i = tmp.len();
                if v == 0 { i -= 1; tmp[i] = b'0'; }
                else { while v > 0 { i -= 1; tmp[i] = b'0' + (v % 10) as u8; v /= 10; } }
                write_out(core::str::from_utf8(&tmp[i..]).unwrap_or("?"));
                write_out("\n");
                return false;
            }
            if &buf[..n] != expected {
                write_out("[FATW]   content mismatch\n");
                return false;
            }
            true
        }
        Err(()) => {
            write_out("[FATW]   read failed for: ");
            write_out(test_name);
            write_out("\n");
            false
        }
    }
}

fn delete_file(path: &str) -> bool {
    let ret = sys::unlink(path.as_ptr() as u64);
    !sys::is_error(ret)
}

#[no_mangle]
pub extern "C" fn _start(_argc: u64, _argv: u64) -> ! {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;

    write_out("[FATW] === FAT Write Test Suite ===\n");

    // ── Test 1: Write and read back a small file on /disk/ ──
    {
        let path = "/disk/TESTRW.TXT";
        let data = b"Hello, FAT write world!";
        let ok = write_file(path, data) && verify_file(path, data, "T1 small write");
        report("T1: write + read round-trip (small file)", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 2: Overwrite existing file ──
    {
        let path = "/disk/TESTOW.TXT";
        let data1 = b"AAAA";
        let data2 = b"BBBB";
        let ok = write_file(path, data1)
            && write_file(path, data2)
            && verify_file(path, data2, "T2 overwrite");
        report("T2: overwrite existing file", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 3: Write larger data (512 bytes = 1 sector) ──
    {
        let path = "/disk/TEST512.TXT";
        let mut data = [0u8; 512];
        for i in 0..512 {
            data[i] = (i % 256) as u8;
        }
        let ok = write_file(path, &data) && verify_file(path, &data, "T3 512 bytes");
        report("T3: write + read 512 bytes (1 sector)", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 4: Write larger data (1024 bytes = 2 sectors) ──
    {
        let path = "/disk/TEST1K.TXT";
        let mut data = [0u8; 1024];
        for i in 0..1024 {
            data[i] = ((i * 7 + 13) % 256) as u8;
        }
        let ok = write_file(path, &data) && verify_file(path, &data, "T4 1KB");
        report("T4: write + read 1024 bytes (2 sectors)", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 5: Multiple files ──
    {
        let paths = ["/disk/F1.TXT", "/disk/F2.TXT", "/disk/F3.TXT"];
        let datas: [&[u8]; 3] = [b"File one", b"File two content", b"File three data here"];
        let mut ok = true;
        for i in 0..3 {
            if !write_file(paths[i], datas[i]) {
                ok = false;
                break;
            }
        }
        if ok {
            for i in 0..3 {
                if !verify_file(paths[i], datas[i], "T5 multi-file") {
                    ok = false;
                    break;
                }
            }
        }
        report("T5: create + verify 3 separate files", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 6: Delete file then verify it's gone ──
    {
        let path = "/disk/DELTEST.TXT";
        let data = b"Delete me";
        let ok = write_file(path, data)
            && delete_file(path);
        // Try to open deleted file — should fail
        let fd = sys::open(path, sys::O_RDONLY);
        let deleted_ok = sys::is_error(fd);
        let final_ok = ok && deleted_ok;
        report("T6: delete file, verify gone", final_ok);
        if final_ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 7: O_TRUNC — open existing, truncate, write new ──
    {
        let path = "/disk/TRUNCTST.TXT";
        let data1 = b"Original long content";
        let data2 = b"New";
        let ok = write_file(path, data1)
            && write_file(path, data2)
            && verify_file(path, data2, "T7 truncate");
        report("T7: O_TRUNC truncates existing file", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Test 8: Write empty file ──
    {
        let path = "/disk/EMPTY.TXT";
        let ok = write_file(path, b"")
            && verify_file(path, b"", "T8 empty");
        report("T8: create and read empty file", ok);
        if ok { passed += 1; } else { failed += 1; }
    }

    // ── Summary ──
    write_out("\n[FATW] === Results: ");
    let mut tmp = [0u8; 16];
    let mut v = passed;
    let mut i = tmp.len();
    if v == 0 { i -= 1; tmp[i] = b'0'; }
    else { while v > 0 { i -= 1; tmp[i] = b'0' + (v % 10) as u8; v /= 10; } }
    write_out(core::str::from_utf8(&tmp[i..]).unwrap_or("?"));
    write_out(" passed, ");
    v = failed;
    i = tmp.len();
    if v == 0 { i -= 1; tmp[i] = b'0'; }
    else { while v > 0 { i -= 1; tmp[i] = b'0' + (v % 10) as u8; v /= 10; } }
    write_out(core::str::from_utf8(&tmp[i..]).unwrap_or("?"));
    write_out(" failed ===\n");

    if failed == 0 {
        write_out("[FATW] ALL TESTS PASSED\n");
    } else {
        write_out("[FATW] SOME TESTS FAILED\n");
    }

    sys::exit(0);
}
