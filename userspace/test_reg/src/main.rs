#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::write(2, b"[test_reg] PANIC\n");
    sys::exit(1);
}

fn write_str(s: &[u8]) {
    sys::write(1, s);
}

fn write_hex(mut v: u64) {
    let mut buf = [0u8; 18];
    buf[0] = b'0';
    buf[1] = b'x';
    for i in (2..18).rev() {
        buf[i] = b"0123456789abcdef"[(v & 0xF) as usize];
        v >>= 4;
    }
    sys::write(1, &buf);
}

fn check(name: &[u8], expected: u64, actual: u64) -> bool {
    if expected == actual {
        write_str(b"  ");
        sys::write(1, name);
        write_str(b" = ");
        write_hex(actual);
        write_str(b" OK\n");
        true
    } else {
        write_str(b"  ");
        sys::write(1, name);
        write_str(b" expected=");
        write_hex(expected);
        write_str(b" got=");
        write_hex(actual);
        write_str(b" FAIL\n");
        false
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 1: Verify registers survive a blocking pipe read (context switch)
// ═══════════════════════════════════════════════════════════════════════════════
fn test_regs_pipe_read() -> bool {
    write_str(b"\n== Test 1: Register preservation across pipe read context switch ==\n");

    let r = sys::pipe();
    if sys::is_error(r) {
        write_str(b"  FAIL: pipe() returned error\n");
        return false;
    }
    let rd = (r >> 32) as u64;
    let wr = (r & 0xFFFF_FFFF) as u64;

    let pid = sys::fork();
    if sys::is_error(pid) {
        write_str(b"  FAIL: fork() returned error\n");
        return false;
    }

    if pid == 0 {
        // Child: sleep briefly then write to wake parent
        sys::sleep(3);
        sys::write(wr, b"X");
        sys::close(wr);
        sys::exit(0);
    }

    // Parent: close write end, then do blocking read on pipe
    sys::close(wr);

    let mut buf = [0u8; 4];
    let buf_ptr = buf.as_mut_ptr() as u64;

    // Set unique values in all GP registers, then do blocking read.
    // The syscall will block (pipe empty), context-switch to child,
    // child writes + exits, parent wakes, restores registers.
    //
    // We test: RBX, RBP, R8-R10, R12-R15 (not used as syscall args).
    // RCX and R11 are clobbered by the CPU on syscall entry (saves
    // user RIP to RCX, user RFLAGS to R11). The kernel saves these
    // clobbered values and restores them via iretq, so after return
    // RCX = address after syscall instruction, R11 = user RFLAGS.
    // RAX is the return value, always overwritten.

    let mut rbx_v: u64 = 0;
    let mut rbp_v: u64 = 0;
    let mut r8_v: u64 = 0;
    let mut r9_v: u64 = 0;
    let mut r10_v: u64 = 0;
    let mut r12_v: u64 = 0;
    let mut r13_v: u64 = 0;
    let mut r14_v: u64 = 0;
    let mut r15_v: u64 = 0;

    unsafe {
        core::arch::asm!(
            // Set known values in compiler-allocated registers
            "mov {out0}, 0xB0B0B0B0B0B0B0B0",
            "mov {out1}, 0x0000DEAD0000BEEF",
            "mov {out2}, 0x0000000000000008",
            "mov {out3}, 0x0000000000000009",
            "mov {out4}, 0x0000000000000010",
            "mov {out5}, 0x0000000000000012",
            "mov {out6}, 0x0000000000000013",
            "mov {out7}, 0x0000000000000014",
            "mov {out8}, 0x0000000000000015",

            // Syscall: read(rd, buf, 1) — will BLOCK on empty pipe
            "mov rax, 6",           // SYS_READ
            "mov rdi, {rd}",        // fd = pipe read end
            "mov rsi, {buf_ptr}",   // buf
            "mov rdx, 1",           // count = 1
            "syscall",

            rd = in(reg) rd,
            buf_ptr = in(reg) buf_ptr,
            out0 = out(reg) rbx_v,
            out1 = out(reg) rbp_v,
            out2 = out(reg) r8_v,
            out3 = out(reg) r9_v,
            out4 = out(reg) r10_v,
            out5 = out(reg) r12_v,
            out6 = out(reg) r13_v,
            out7 = out(reg) r14_v,
            out8 = out(reg) r15_v,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }

    // Wait for child
    sys::waitpid(pid as u64, 0);
    sys::close(rd);

    let mut all_ok = true;
    all_ok &= check(b"REG0", 0xB0B0B0B0B0B0B0B0, rbx_v);
    all_ok &= check(b"REG1", 0x0000DEAD0000BEEF, rbp_v);
    all_ok &= check(b"REG2", 0x0000000000000008, r8_v);
    all_ok &= check(b"REG3", 0x0000000000000009, r9_v);
    all_ok &= check(b"REG4", 0x0000000000000010, r10_v);
    all_ok &= check(b"REG5", 0x0000000000000012, r12_v);
    all_ok &= check(b"REG6", 0x0000000000000013, r13_v);
    all_ok &= check(b"REG7", 0x0000000000000014, r14_v);
    all_ok &= check(b"REG8", 0x0000000000000015, r15_v);

    all_ok
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 2: Verify registers survive sleep (timer-based context switch)
// ═══════════════════════════════════════════════════════════════════════════════
fn test_regs_sleep() -> bool {
    write_str(b"\n== Test 2: Register preservation across sleep context switch ==\n");

    let mut rbx_v: u64 = 0;
    let mut rbp_v: u64 = 0;
    let mut r8_v: u64 = 0;
    let mut r9_v: u64 = 0;
    let mut r10_v: u64 = 0;
    let mut r12_v: u64 = 0;
    let mut r13_v: u64 = 0;
    let mut r14_v: u64 = 0;
    let mut r15_v: u64 = 0;

    unsafe {
        core::arch::asm!(
            // Set known values in compiler-allocated registers
            "mov {out0}, 0xAAAAAAAAAAAAAAAA",
            "mov {out1}, 0xBBBBBBBBBBBBBBBB",
            "mov {out2}, 0x1111111111111111",
            "mov {out3}, 0x2222222222222222",
            "mov {out4}, 0x3333333333333333",
            "mov {out5}, 0x4444444444444444",
            "mov {out6}, 0x5555555555555555",
            "mov {out7}, 0x6666666666666666",
            "mov {out8}, 0x7777777777777777",

            // Syscall: sleep(2) — blocks for 2 ticks, context-switches
            "mov rax, 5",   // SYS_SLEEP
            "mov rdi, 2",   // 2 ticks
            "syscall",

            out0 = out(reg) rbx_v,
            out1 = out(reg) rbp_v,
            out2 = out(reg) r8_v,
            out3 = out(reg) r9_v,
            out4 = out(reg) r10_v,
            out5 = out(reg) r12_v,
            out6 = out(reg) r13_v,
            out7 = out(reg) r14_v,
            out8 = out(reg) r15_v,
            // Declare syscall-clobbered registers so compiler avoids them
            lateout("rax") _,
            lateout("rcx") _,
            lateout("rdi") _,
            lateout("r11") _,
            options(nostack),
        );
    }

    let mut all_ok = true;
    all_ok &= check(b"REG0", 0xAAAAAAAAAAAAAAAA, rbx_v);
    all_ok &= check(b"REG1", 0xBBBBBBBBBBBBBBBB, rbp_v);
    all_ok &= check(b"REG2", 0x1111111111111111, r8_v);
    all_ok &= check(b"REG3", 0x2222222222222222, r9_v);
    all_ok &= check(b"REG4", 0x3333333333333333, r10_v);
    all_ok &= check(b"REG5", 0x4444444444444444, r12_v);
    all_ok &= check(b"REG6", 0x5555555555555555, r13_v);
    all_ok &= check(b"REG7", 0x6666666666666666, r14_v);
    all_ok &= check(b"REG8", 0x7777777777777777, r15_v);

    all_ok
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 3: Verify registers survive fork + exec (full process creation)
// ═══════════════════════════════════════════════════════════════════════════════
fn test_regs_after_fork() -> bool {
    write_str(b"\n== Test 3: Register preservation across fork ==\n");

    let mut rbx_v: u64 = 0;
    let mut rbp_v: u64 = 0;
    let mut r12_v: u64 = 0;
    let mut r13_v: u64 = 0;
    let mut r14_v: u64 = 0;
    let mut r15_v: u64 = 0;

    unsafe {
        core::arch::asm!(
            "mov {out0}, 0xF1F1F1F1F1F1F1F1",
            "mov {out1}, 0xF2F2F2F2F2F2F2F2",
            "mov {out2}, 0xF3F3F3F3F3F3F3F3",
            "mov {out3}, 0xF4F4F4F4F4F4F4F4",
            "mov {out4}, 0xF5F5F5F5F5F5F5F5",
            "mov {out5}, 0xF6F6F6F6F6F6F6F6",

            // Syscall: fork()
            "mov rax, 8",   // SYS_FORK
            "syscall",

            // Parent gets child PID in RAX, child gets 0.
            // We're testing the PARENT's registers.
            out0 = out(reg) rbx_v,
            out1 = out(reg) rbp_v,
            out2 = out(reg) r12_v,
            out3 = out(reg) r13_v,
            out4 = out(reg) r14_v,
            out5 = out(reg) r15_v,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("rdi") _,
            lateout("rsi") _,
            lateout("rdx") _,
            lateout("r10") _,
            lateout("r11") _,
            options(nostack),
        );
    }

    // Reap any child
    sys::waitpid(0, 1);

    let mut all_ok = true;
    all_ok &= check(b"REG0", 0xF1F1F1F1F1F1F1F1, rbx_v);
    all_ok &= check(b"REG1", 0xF2F2F2F2F2F2F2F2, rbp_v);
    all_ok &= check(b"REG2", 0xF3F3F3F3F3F3F3F3, r12_v);
    all_ok &= check(b"REG3", 0xF4F4F4F4F4F4F4F4, r13_v);
    all_ok &= check(b"REG4", 0xF5F5F5F5F5F5F5F5, r14_v);
    all_ok &= check(b"REG5", 0xF6F6F6F6F6F6F6F6, r15_v);

    all_ok
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 4: Multiple sleep/wake cycles
// ═══════════════════════════════════════════════════════════════════════════════
fn test_regs_multi_sleep() -> bool {
    write_str(b"\n== Test 4: Registers survive multiple sleep/wake cycles ==\n");

    let mut rbx_v: u64 = 0;
    let mut r12_v: u64 = 0;
    let mut r13_v: u64 = 0;

    let mut all_ok = true;

    for i in 0..5u64 {
        let pattern = 0xA0A0_0000_0000_0000 | i;

        unsafe {
            core::arch::asm!(
                "mov {out0}, {pat}",
                "mov {out1}, {pat}",
                "mov {out2}, {pat}",

                "mov rax, 5",   // SYS_SLEEP
                "mov rdi, 1",   // 1 tick
                "syscall",

                pat = in(reg) pattern,
                out0 = out(reg) rbx_v,
                out1 = out(reg) r12_v,
                out2 = out(reg) r13_v,
                lateout("rax") _,
                lateout("rcx") _,
                lateout("rdi") _,
                lateout("rsi") _,
                lateout("rdx") _,
                lateout("r10") _,
                lateout("r11") _,
                options(nostack),
            );
        }

        all_ok &= check(b"REG0", pattern, rbx_v);
        all_ok &= check(b"REG1", pattern, r12_v);
        all_ok &= check(b"REG2", pattern, r13_v);
    }

    all_ok
}

// ═══════════════════════════════════════════════════════════════════════════════
// Entry point
// ═══════════════════════════════════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn _start(_argc: u64, _argv: u64) -> ! {
    write_str(b"\n========================================\n");
    write_str(b"  INDOMINUS OS - Register Preservation Test\n");
    write_str(b"========================================\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    if test_regs_sleep()     { passed += 1; } else { failed += 1; }
    if test_regs_after_fork(){ passed += 1; } else { failed += 1; }
    if test_regs_multi_sleep(){ passed += 1; } else { failed += 1; }

    write_str(b"\n========================================\n");
    write_str(b"  RESULTS: ");
    // Simple number output
    if passed == 0 { write_str(b"0"); }
    else {
        let mut v = passed;
        let mut buf = [0u8; 10];
        let mut i = buf.len();
        while v > 0 { i -= 1; buf[i] = b'0' + (v % 10) as u8; v /= 10; }
        sys::write(1, &buf[i..]);
    }
    write_str(b" passed, ");
    if failed == 0 { write_str(b"0"); }
    else {
        let mut v = failed;
        let mut buf = [0u8; 10];
        let mut i = buf.len();
        while v > 0 { i -= 1; buf[i] = b'0' + (v % 10) as u8; v /= 10; }
        sys::write(1, &buf[i..]);
    }
    write_str(b" failed\n");

    if failed == 0 {
        write_str(b"  >>> ALL REGISTER TESTS PASSED <<<\n");
    } else {
        write_str(b"  >>> SOME REGISTER TESTS FAILED <<<\n");
        write_str(b"  >>> KERNEL HAS REGISTER CORRUPTION BUG <<<\n");
    }
    write_str(b"========================================\n\n");

    sys::exit(if failed == 0 { 0 } else { 1 });
}
