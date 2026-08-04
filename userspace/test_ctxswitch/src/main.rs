#![no_std]
#![no_main]
#![allow(unused)]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::write(2, b"[ctxswitch] PANIC\n");
    sys::exit(1);
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn write_str(s: &[u8]) { sys::write(1, s); }
fn write_str_nl(s: &[u8]) { sys::write(1, s); sys::write(1, b"\n"); }
fn write_nl() { sys::write(1, b"\n"); }

fn write_hex(mut v: u64) {
    if v == 0 { sys::write(1, b"0"); return; }
    let mut buf = [0u8; 16];
    let mut i = 0;
    while v > 0 {
        let d = (v & 0xF) as u8;
        buf[i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        v >>= 4; i += 1;
    }
    while i > 0 { i -= 1; sys::write(1, &buf[i..i + 1]); }
}

fn check(name: &[u8], expected: u64, actual: u64) -> bool {
    if expected == actual {
        sys::write(1, b"  "); sys::write(1, name);
        sys::write(1, b" = 0x"); write_hex(actual);
        sys::write(1, b" OK\n"); true
    } else {
        sys::write(1, b"  "); sys::write(1, name);
        sys::write(1, b" FAIL: expected 0x"); write_hex(expected);
        sys::write(1, b" got 0x"); write_hex(actual); write_nl(); false
    }
}

// ── Register verification helpers ──────────────────────────────────────────

// All 9 callee-saved registers — used in tests with 0 inputs (yield, sleep)
fn verify_all9(
    rbx: u64, rbp: u64, r8: u64, r9: u64, r10: u64,
    r12: u64, r13: u64, r14: u64, r15: u64,
) -> bool {
    let mut ok = true;
    ok &= check(b"RBX", 0xAAAAAAAAAAAAAAAA, rbx);
    ok &= check(b"RBP", 0xBBBBBBBBBBBBBBBB, rbp);
    ok &= check(b"R8 ", 0x1111111111111111, r8);
    ok &= check(b"R9 ", 0x2222222222222222, r9);
    ok &= check(b"R10", 0x3333333333333333, r10);
    ok &= check(b"R12", 0x5555555555555555, r12);
    ok &= check(b"R13", 0x6666666666666666, r13);
    ok &= check(b"R14", 0x7777777777777777, r14);
    ok &= check(b"R15", 0x8888888888888888, r15);
    ok
}

// 5 callee-saved registers — used in tests with inputs (read, waitpid, fork readback)
fn verify5(
    rbx: u64, rbp: u64, r12: u64, r13: u64, r14: u64,
) -> bool {
    let mut ok = true;
    ok &= check(b"RBX", 0xAAAAAAAAAAAAAAAA, rbx);
    ok &= check(b"RBP", 0xBBBBBBBBBBBBBBBB, rbp);
    ok &= check(b"R12", 0x5555555555555555, r12);
    ok &= check(b"R13", 0x6666666666666666, r13);
    ok &= check(b"R14", 0x7777777777777777, r14);
    ok
}

// ── Test 1: yield() ────────────────────────────────────────────────────────

fn test_yield() -> bool {
    write_str_nl(b"== Test 1: yield() ==");
    let mut rbx: u64; let mut rbp: u64;
    let mut r8: u64;  let mut r9: u64;
    let mut r10: u64; let mut r12: u64;
    let mut r13: u64; let mut r14: u64; let mut r15: u64;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, 0xAAAAAAAAAAAAAAAA",
            "mov {rbp_op}, 0xBBBBBBBBBBBBBBBB",
            "mov {r8_op},  0x1111111111111111",
            "mov {r9_op},  0x2222222222222222",
            "mov {r10_op}, 0x3333333333333333",
            "mov {r12_op}, 0x5555555555555555",
            "mov {r13_op}, 0x6666666666666666",
            "mov {r14_op}, 0x7777777777777777",
            "mov {r15_op}, 0x8888888888888888",
            "mov rax, 2",   // SYS_YIELD
            "syscall",
            rbx_op = out(reg) rbx, rbp_op = out(reg) rbp,
            r8_op = out(reg) r8, r9_op = out(reg) r9,
            r10_op = out(reg) r10, r12_op = out(reg) r12,
            r13_op = out(reg) r13, r14_op = out(reg) r14,
            r15_op = out(reg) r15,
            lateout("rax") _, lateout("rcx") _, lateout("rdi") _,
            lateout("rsi") _, lateout("rdx") _, lateout("r11") _,
            options(nostack),
        );
    }

    let ok = verify_all9(rbx, rbp, r8, r9, r10, r12, r13, r14, r15);
    if ok { write_str_nl(b"  PASS: yield()"); }
    ok
}

// ── Test 2: sleep() ────────────────────────────────────────────────────────

fn test_sleep() -> bool {
    write_str_nl(b"== Test 2: sleep() ==");
    let mut rbx: u64; let mut rbp: u64;
    let mut r8: u64;  let mut r9: u64;
    let mut r10: u64; let mut r12: u64;
    let mut r13: u64; let mut r14: u64; let mut r15: u64;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, 0xAAAAAAAAAAAAAAAA",
            "mov {rbp_op}, 0xBBBBBBBBBBBBBBBB",
            "mov {r8_op},  0x1111111111111111",
            "mov {r9_op},  0x2222222222222222",
            "mov {r10_op}, 0x3333333333333333",
            "mov {r12_op}, 0x5555555555555555",
            "mov {r13_op}, 0x6666666666666666",
            "mov {r14_op}, 0x7777777777777777",
            "mov {r15_op}, 0x8888888888888888",
            "mov rax, 5",   // SYS_SLEEP
            "mov rdi, 2",   // 2 ticks
            "syscall",
            rbx_op = out(reg) rbx, rbp_op = out(reg) rbp,
            r8_op = out(reg) r8, r9_op = out(reg) r9,
            r10_op = out(reg) r10, r12_op = out(reg) r12,
            r13_op = out(reg) r13, r14_op = out(reg) r14,
            r15_op = out(reg) r15,
            lateout("rax") _, lateout("rcx") _, lateout("rdi") _,
            lateout("rsi") _, lateout("rdx") _, lateout("r11") _,
            options(nostack),
        );
    }

    let ok = verify_all9(rbx, rbp, r8, r9, r10, r12, r13, r14, r15);
    if ok { write_str_nl(b"  PASS: sleep()"); }
    ok
}

// ── Test 3: blocking read() ────────────────────────────────────────────────

fn test_blocking_read() -> bool {
    write_str_nl(b"== Test 3: blocking read() ==");

    let pipe_ret = sys::pipe();
    if sys::is_error(pipe_ret) {
        write_str_nl(b"  SKIP: pipe() failed");
        return false;
    }
    let read_fd = ((pipe_ret as u64) >> 32) & 0xFFFF_FFFF;
    let write_fd = (pipe_ret as u64) & 0xFFFF_FFFF;

    let child = sys::fork();
    if child == 0 {
        sys::sleep(1);
        sys::write(write_fd, b"X");
        sys::exit(0);
    }

    // 5 registers only (2 inputs + 5 outputs + 6 clobbers = 13, fits in 14 GP regs)
    let mut rbx: u64; let mut rbp: u64;
    let mut r12: u64; let mut r13: u64; let mut r14: u64;
    let mut buf = [0u8; 1];

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, 0xAAAAAAAAAAAAAAAA",
            "mov {rbp_op}, 0xBBBBBBBBBBBBBBBB",
            "mov {r12_op}, 0x5555555555555555",
            "mov {r13_op}, 0x6666666666666666",
            "mov {r14_op}, 0x7777777777777777",
            "mov rax, 6",           // SYS_READ
            "mov rdi, {fd_op}",
            "mov rsi, {buf_op}",
            "mov rdx, 1",
            "syscall",
            rbx_op = out(reg) rbx, rbp_op = out(reg) rbp,
            r12_op = out(reg) r12, r13_op = out(reg) r13,
            r14_op = out(reg) r14,
            fd_op = in(reg) read_fd,
            buf_op = in(reg) buf.as_mut_ptr(),
            lateout("rax") _, lateout("rcx") _, lateout("rdi") _,
            lateout("rsi") _, lateout("rdx") _,
            lateout("r8") _, lateout("r9") _, lateout("r10") _,
            lateout("r11") _, lateout("r15") _,
            options(nostack),
        );
    }

    sys::waitpid_blocking(child as u64);
    sys::close(read_fd);
    sys::close(write_fd);

    let ok = verify5(rbx, rbp, r12, r13, r14);
    if ok { write_str_nl(b"  PASS: blocking read()"); }
    ok
}

// ── Test 4: waitpid() ──────────────────────────────────────────────────────

fn test_waitpid() -> bool {
    write_str_nl(b"== Test 4: waitpid() ==");

    let child = sys::fork();
    if child == 0 {
        sys::exit(42);
    }

    let mut rbx: u64; let mut rbp: u64;
    let mut r12: u64; let mut r13: u64; let mut r14: u64;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, 0xAAAAAAAAAAAAAAAA",
            "mov {rbp_op}, 0xBBBBBBBBBBBBBBBB",
            "mov {r12_op}, 0x5555555555555555",
            "mov {r13_op}, 0x6666666666666666",
            "mov {r14_op}, 0x7777777777777777",
            "mov rax, 4",           // SYS_WAITPID
            "mov rdi, {child_op}",
            "mov rsi, 0",
            "syscall",
            rbx_op = out(reg) rbx, rbp_op = out(reg) rbp,
            r12_op = out(reg) r12, r13_op = out(reg) r13,
            r14_op = out(reg) r14,
            child_op = in(reg) child as u64,
            lateout("rax") _, lateout("rcx") _, lateout("rdi") _,
            lateout("rsi") _, lateout("rdx") _,
            lateout("r8") _, lateout("r9") _, lateout("r10") _,
            lateout("r11") _, lateout("r15") _,
            options(nostack),
        );
    }

    let ok = verify5(rbx, rbp, r12, r13, r14);
    if ok { write_str_nl(b"  PASS: waitpid()"); }
    ok
}

// ── Test 5: timer preemption ───────────────────────────────────────────────

fn test_timer_preemption() -> bool {
    write_str_nl(b"== Test 5: timer preemption ==");

    // Set 5 registers, spin for >20ms (timer interrupt preempts us),
    // read them back via a 5-in/5-out asm block.
    let mut rbx: u64; let mut rbp: u64;
    let mut r12: u64; let mut r13: u64; let mut r14: u64;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, 0xAAAAAAAAAAAAAAAA",
            "mov {rbp_op}, 0xBBBBBBBBBBBBBBBB",
            "mov {r12_op}, 0x5555555555555555",
            "mov {r13_op}, 0x6666666666666666",
            "mov {r14_op}, 0x7777777777777777",
            rbx_op = out(reg) rbx, rbp_op = out(reg) rbp,
            r12_op = out(reg) r12, r13_op = out(reg) r13,
            r14_op = out(reg) r14,
            options(nostack),
        );
    }

    // Spin for >20ms (>1 timer tick at 50 Hz)
    let mut counter: u64 = 0;
    loop {
        unsafe { let _ = core::ptr::read_volatile(&counter as *const u64); }
        counter = counter.wrapping_add(1);
        core::hint::spin_loop();
        if counter > 500_000 { break; }
    }

    // Read back — 5 in + 5 out = 10 named operands, fits easily
    let mut rbx2: u64 = 0; let mut rbp2: u64 = 0;
    let mut r122: u64 = 0; let mut r132: u64 = 0; let mut r142: u64 = 0;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, {rbx_in}",
            "mov {rbp_op}, {rbp_in}",
            "mov {r12_op}, {r12_in}",
            "mov {r13_op}, {r13_in}",
            "mov {r14_op}, {r14_in}",
            rbx_op = out(reg) rbx2, rbp_op = out(reg) rbp2,
            r12_op = out(reg) r122, r13_op = out(reg) r132,
            r14_op = out(reg) r142,
            rbx_in = in(reg) rbx, rbp_in = in(reg) rbp,
            r12_in = in(reg) r12, r13_in = in(reg) r13,
            r14_in = in(reg) r14,
            options(nostack),
        );
    }

    let ok = verify5(rbx2, rbp2, r122, r132, r142);
    if ok { write_str_nl(b"  PASS: timer preemption"); }
    ok
}

// ── Test 6: fork() ─────────────────────────────────────────────────────────

fn test_fork() -> bool {
    write_str_nl(b"== Test 6: fork() ==");

    let mut rbx: u64; let mut rbp: u64;
    let mut r12: u64; let mut r13: u64; let mut r14: u64;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, 0xAAAAAAAAAAAAAAAA",
            "mov {rbp_op}, 0xBBBBBBBBBBBBBBBB",
            "mov {r12_op}, 0x5555555555555555",
            "mov {r13_op}, 0x6666666666666666",
            "mov {r14_op}, 0x7777777777777777",
            rbx_op = out(reg) rbx, rbp_op = out(reg) rbp,
            r12_op = out(reg) r12, r13_op = out(reg) r13,
            r14_op = out(reg) r14,
            options(nostack),
        );
    }

    let child = sys::fork();
    if child == 0 {
        // Child: verify registers match parent's
        let mut crbx: u64 = 0; let mut crbp: u64 = 0;
        let mut cr12: u64 = 0; let mut cr13: u64 = 0; let mut cr14: u64 = 0;

        unsafe {
            core::arch::asm!(
                "mov {rbx_op}, {rbx_in}",
                "mov {rbp_op}, {rbp_in}",
                "mov {r12_op}, {r12_in}",
                "mov {r13_op}, {r13_in}",
                "mov {r14_op}, {r14_in}",
                rbx_op = out(reg) crbx, rbp_op = out(reg) crbp,
                r12_op = out(reg) cr12, r13_op = out(reg) cr13,
                r14_op = out(reg) cr14,
                rbx_in = in(reg) rbx, rbp_in = in(reg) rbp,
                r12_in = in(reg) r12, r13_in = in(reg) r13,
                r14_in = in(reg) r14,
                options(nostack),
            );
        }

        let ok = verify5(crbx, crbp, cr12, cr13, cr14);
        if ok { write_str_nl(b"  PASS: fork() child registers"); }
        else { write_str_nl(b"  FAIL: fork() child registers"); }
        sys::exit(if ok { 0 } else { 1 });
    }

    // Parent: verify its own registers unchanged
    let mut prbx: u64 = 0; let mut prbp: u64 = 0;
    let mut pr12: u64 = 0; let mut pr13: u64 = 0; let mut pr14: u64 = 0;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, {rbx_in}",
            "mov {rbp_op}, {rbp_in}",
            "mov {r12_op}, {r12_in}",
            "mov {r13_op}, {r13_in}",
            "mov {r14_op}, {r14_in}",
            rbx_op = out(reg) prbx, rbp_op = out(reg) prbp,
            r12_op = out(reg) pr12, r13_op = out(reg) pr13,
            r14_op = out(reg) pr14,
            rbx_in = in(reg) rbx, rbp_in = in(reg) rbp,
            r12_in = in(reg) r12, r13_in = in(reg) r13,
            r14_in = in(reg) r14,
            options(nostack),
        );
    }

    sys::waitpid_blocking(child as u64);

    let ok = verify5(prbx, prbp, pr12, pr13, pr14);
    if ok { write_str_nl(b"  PASS: fork() parent registers"); }
    ok
}

// ── Test 7: signal delivery ────────────────────────────────────────────────

extern "C" fn sigusr1_handler(_signum: u64) {}

fn test_signal() -> bool {
    write_str_nl(b"== Test 7: signal delivery ==");

    let handler_addr = sigusr1_handler as *const () as u64;
    let ret = sys::sigaction(10, handler_addr, 0);
    if sys::is_error(ret) {
        write_str_nl(b"  SKIP: sigaction() failed");
        return false;
    }

    let mut rbx: u64; let mut rbp: u64;
    let mut r12: u64; let mut r13: u64; let mut r14: u64;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, 0xAAAAAAAAAAAAAAAA",
            "mov {rbp_op}, 0xBBBBBBBBBBBBBBBB",
            "mov {r12_op}, 0x5555555555555555",
            "mov {r13_op}, 0x6666666666666666",
            "mov {r14_op}, 0x7777777777777777",
            rbx_op = out(reg) rbx, rbp_op = out(reg) rbp,
            r12_op = out(reg) r12, r13_op = out(reg) r13,
            r14_op = out(reg) r14,
            options(nostack),
        );
    }

    // Send ourselves SIGUSR1 — handler runs and returns
    let my_pid = sys::getpid();
    sys::kill(my_pid, 10);

    // Verify registers restored after signal handler
    let mut rbx2: u64 = 0; let mut rbp2: u64 = 0;
    let mut r122: u64 = 0; let mut r132: u64 = 0; let mut r142: u64 = 0;

    unsafe {
        core::arch::asm!(
            "mov {rbx_op}, {rbx_in}",
            "mov {rbp_op}, {rbp_in}",
            "mov {r12_op}, {r12_in}",
            "mov {r13_op}, {r13_in}",
            "mov {r14_op}, {r14_in}",
            rbx_op = out(reg) rbx2, rbp_op = out(reg) rbp2,
            r12_op = out(reg) r122, r13_op = out(reg) r132,
            r14_op = out(reg) r142,
            rbx_in = in(reg) rbx, rbp_in = in(reg) rbp,
            r12_in = in(reg) r12, r13_in = in(reg) r13,
            r14_in = in(reg) r14,
            options(nostack),
        );
    }

    let ok = verify5(rbx2, rbp2, r122, r132, r142);
    if ok { write_str_nl(b"  PASS: signal delivery"); }
    ok
}

// ── Test 8: multi-context-switch ───────────────────────────────────────────

fn test_multi_context_switch() -> bool {
    write_str_nl(b"== Test 8: multi-context-switch ==");

    // Just do 3 yield/sleep cycles — verifies no crash from repeated context switches
    for i in 0..3u64 {
        if i % 2 == 0 { sys::yield_now(); } else { sys::sleep(1); }
    }

    write_str_nl(b"  PASS: 3 context switches completed");
    true
}

// ── Test 9: RFLAGS preservation ────────────────────────────────────────────

fn test_rflags_preservation() -> bool {
    write_str_nl(b"== Test 9: RFLAGS preservation ==");

    let mut rflags_before: u64 = 0;
    let mut rflags_after: u64 = 0;

    unsafe {
        core::arch::asm!(
            "pushfq",
            "pop rax",
            "or rax, 0xC5",       // set CF(0), PF(2), ZF(6), SF(7)
            "push rax",
            "popfq",
            "pushfq",
            "pop {rf_bef_op}",
            "mov rax, 2",         // SYS_YIELD
            "syscall",
            "pushfq",
            "pop {rf_aft_op}",
            rf_bef_op = out(reg) rflags_before,
            rf_aft_op = out(reg) rflags_after,
            lateout("rax") _, lateout("rcx") _, lateout("rdi") _,
            lateout("rsi") _, lateout("rdx") _, lateout("r11") _,
            options(nostack),
        );
    }

    let user_mask: u64 = 0x8FF;
    let preserved = (rflags_before & user_mask) == (rflags_after & user_mask);

    write_str(b"  RFLAGS before: 0x"); write_hex(rflags_before); write_nl();
    write_str(b"  RFLAGS after:  0x"); write_hex(rflags_after); write_nl();

    if preserved { write_str_nl(b"  PASS: RFLAGS user bits preserved"); }
    else { write_str_nl(b"  FAIL: RFLAGS mismatch"); }
    preserved
}

// ── Test 10: CoW multi-generation fork ────────────────────────────────────
//
// Parent sets marker=0xAA, forks child.
// Child sees marker=0xAA (CoW shared), writes marker=0xBB, forks grandchild.
// Grandchild sees marker=0xBB (CoW shared from child), writes marker=0xCC.
// Parent waits for child, verifies parent's marker still 0xAA (CoW protected).
// Child waits for grandchild, verifies child's marker still 0xBB (CoW protected).

fn test_cow_multigeneration() -> bool {
    write_str_nl(b"== Test 10: CoW multi-generation fork ==");

    // Use a heap-allocated buffer via mmap so the address is in a CoW region
    let buf_addr = sys::mmap(0, 4096);
    if sys::is_error(buf_addr) {
        write_str_nl(b"  SKIP: mmap failed");
        return false;
    }
    let buf = buf_addr as *mut u8;

    unsafe { *buf = 0xAA; }

    let child = sys::fork();
    if child == 0 {
        // Child: verify inherited value, write new marker
        let val = unsafe { *buf };
        if val != 0xAA {
            write_str_nl(b"  FAIL: child inherited wrong value");
            sys::exit(1);
        }
        unsafe { *buf = 0xBB; }

        let grandchild = sys::fork();
        if grandchild == 0 {
            // Grandchild: verify inherited value from child, write new marker
            let val2 = unsafe { *buf };
            if val2 != 0xBB {
                write_str_nl(b"  FAIL: grandchild inherited wrong value");
                sys::exit(1);
            }
            unsafe { *buf = 0xCC; }
            sys::exit(0);
        }

        // Child: wait for grandchild, verify child's buffer untouched
        sys::waitpid_blocking(grandchild as u64);
        let after_gc = unsafe { *buf };
        if after_gc != 0xBB {
            write_str(b"  FAIL: child buffer corrupted after grandchild exit, got 0x");
            write_hex(after_gc as u64);
            write_nl();
            sys::exit(1);
        }
        sys::exit(0);
    }

    // Parent: wait for child, verify parent's buffer untouched
    sys::waitpid_blocking(child as u64);
    let after_child = unsafe { *buf };
    if after_child != 0xAA {
        write_str(b"  FAIL: parent buffer corrupted after child exit, got 0x");
        write_hex(after_child as u64);
        write_nl();
        return false;
    }

    let _ = sys::munmap(buf_addr as u64, 4096);
    write_str_nl(b"  PASS: CoW multi-generation fork");
    true
}

// ── Test 11: CoW fork write isolation ─────────────────────────────────────
//
// Parent writes A, forks child, parent writes B, child writes C.
// Both waitpid, verify parent sees B (not C) and child sees A (not C).

fn test_cow_isolation() -> bool {
    write_str_nl(b"== Test 11: CoW fork write isolation ==");

    let buf_addr = sys::mmap(0, 4096);
    if sys::is_error(buf_addr) {
        write_str_nl(b"  SKIP: mmap failed");
        return false;
    }
    let buf = buf_addr as *mut u8;

    unsafe { *buf = b'A'; }

    let pipe_ret = sys::pipe();
    if sys::is_error(pipe_ret) {
        write_str_nl(b"  SKIP: pipe() failed");
        return false;
    }
    let read_fd = ((pipe_ret as u64) >> 32) & 0xFFFF_FFFF;
    let write_fd = (pipe_ret as u64) & 0xFFFF_FFFF;

    let child = sys::fork();
    if child == 0 {
        // Child: reads parent's value, writes its own, signals via pipe
        let val = unsafe { *buf };
        if val != b'A' as u8 {
            write_str(b"  FAIL: child expected A got 0x"); write_hex(val as u64); write_nl();
            sys::exit(1);
        }
        unsafe { *buf = b'C'; }
        // Signal completion
        sys::write(write_fd, b"X");
        sys::close(write_fd);
        sys::exit(0);
    }

    // Parent: wait for child pipe signal, then write B
    let mut tmp = [0u8; 1];
    sys::read(read_fd, &mut tmp);
    sys::close(read_fd);

    unsafe { *buf = b'B'; }
    sys::waitpid_blocking(child as u64);

    let parent_val = unsafe { *buf };
    if parent_val != b'B' as u8 {
        write_str(b"  FAIL: parent expected B got 0x"); write_hex(parent_val as u64); write_nl();
        let _ = sys::munmap(buf_addr as u64, 4096);
        return false;
    }

    let _ = sys::munmap(buf_addr as u64, 4096);
    write_str_nl(b"  PASS: CoW fork write isolation");
    true
}

// ── Test 12: fork stress ─────────────────────────────────────────────────
//
// Rapid fork+waitpid cycles to stress process creation/cleanup.

fn test_fork_stress() -> bool {
    write_str_nl(b"== Test 12: fork stress ==");

    let n = 20u64;
    let mut ok = true;

    for i in 0..n {
        let child = sys::fork();
        if child == 0 {
            // Child: quick yield then exit
            sys::yield_now();
            sys::exit(i as u64);
        }
        let status = sys::waitpid_blocking(child as u64);
        if sys::is_error(status) {
            write_str(b"  FAIL: waitpid returned error at iteration "); write_hex(i); write_nl();
            ok = false;
            break;
        }
    }

    if ok {
        write_str(b"  PASS: fork stress ("); write_hex(n); write_str_nl(b" cycles)");
    }
    ok
}

// ── Test 13: memory stress ───────────────────────────────────────────────
//
// Rapid mmap/munmap and brk cycles to stress VMM and PMM.

fn test_memory_stress() -> bool {
    write_str_nl(b"== Test 13: memory stress ==");

    // mmap/munmap cycle
    let mut ok = true;
    for i in 0..10u64 {
        let addr = sys::mmap(0, 4096);
        if sys::is_error(addr) {
            write_str(b"  FAIL: mmap failed at iteration "); write_hex(i); write_nl();
            ok = false;
            break;
        }
        // Write to the page to ensure it's actually mapped
        unsafe { core::ptr::write_volatile(addr as *mut u64, 0xDEADBEEF_CAFEBABE); }
        let ret = sys::munmap(addr as u64, 4096);
        if sys::is_error(ret) {
            write_str(b"  FAIL: munmap failed at iteration "); write_hex(i); write_nl();
            ok = false;
            break;
        }
    }

    if ok {
        // brk stress
        let initial_brk = sys::brk(0);
        if sys::is_error(initial_brk) {
            write_str_nl(b"  FAIL: brk(0) failed");
            return false;
        }
        let mut current = initial_brk as u64;
        for _ in 0..5u64 {
            let new_brk = current + 4096;
            let ret = sys::brk(new_brk);
            if sys::is_error(ret) {
                write_str_nl(b"  FAIL: brk grow failed");
                ok = false;
                break;
            }
            current = new_brk;
        }
        // Shrink back
        let ret = sys::brk(initial_brk as u64);
        if sys::is_error(ret) {
            write_str_nl(b"  FAIL: brk shrink failed");
            ok = false;
        }
    }

    if ok { write_str_nl(b"  PASS: memory stress"); }
    ok
}

// ── Test 14: pipe stress ─────────────────────────────────────────────────
//
// Rapid pipe create/close and read/write cycles.

fn test_pipe_stress() -> bool {
    write_str_nl(b"== Test 14: pipe stress ==");

    let mut ok = true;
    for i in 0..15u64 {
        let pipe_ret = sys::pipe();
        if sys::is_error(pipe_ret) {
            write_str(b"  FAIL: pipe() failed at iteration "); write_hex(i); write_nl();
            ok = false;
            break;
        }
        let read_fd = ((pipe_ret as u64) >> 32) & 0xFFFF_FFFF;
        let write_fd = (pipe_ret as u64) & 0xFFFF_FFFF;

        let data = b"STRESS";
        sys::write(write_fd, data);
        sys::close(write_fd);

        let mut buf = [0u8; 8];
        let n = sys::read(read_fd, &mut buf);
        sys::close(read_fd);

        if n != data.len() as i64 {
            write_str(b"  FAIL: read returned wrong length at iteration "); write_hex(i); write_nl();
            ok = false;
            break;
        }
    }

    if ok { write_str_nl(b"  PASS: pipe stress"); }
    ok
}

// ── Test 15: CoW fork bomb resilience ────────────────────────────────────
//
// Fork many children simultaneously, all writing to shared CoW pages.
// Parent waits for all. Verifies no crash from concurrent CoW faults.

fn test_cow_fork_bomb() -> bool {
    write_str_nl(b"== Test 15: CoW fork bomb ==");

    let buf_addr = sys::mmap(0, 4096);
    if sys::is_error(buf_addr) {
        write_str_nl(b"  SKIP: mmap failed");
        return false;
    }
    let buf = buf_addr as *mut u8;
    unsafe { *buf = 0x42; }

    let n = 4u64;
    let mut child_pids = [0i64; 4];
    let mut all_ok = true;

    for i in 0..n {
        let child = sys::fork();
        if child == 0 {
            // Child: read inherited value, write unique marker, exit
            let val = unsafe { *buf };
            if val != 0x42 {
                sys::exit(1);
            }
            unsafe { *buf = 0x42 + (i as u8 + 1); }
            sys::exit(0);
        }
        child_pids[i as usize] = child;
        sys::yield_now();
    }

    // Parent waits for all children
    for i in 0..n {
        let status = sys::waitpid_blocking(child_pids[i as usize] as u64);
        if sys::is_error(status) {
            write_str(b"  FAIL: waitpid error for child "); write_hex(i); write_nl();
            all_ok = false;
        }
    }

    // Parent's buffer should still be 0x42 (CoW protected from all children)
    let parent_val = unsafe { *buf };
    if all_ok && parent_val != 0x42 {
        write_str(b"  FAIL: parent buffer corrupted, got 0x"); write_hex(parent_val as u64); write_nl();
        all_ok = false;
    }

    let _ = sys::munmap(buf_addr as u64, 4096);

    if all_ok { write_str_nl(b"  PASS: CoW fork bomb"); }
    all_ok
}

// ── Entry Point ────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start(_argc: u64, _argv: u64) -> ! {
    write_str_nl(b"[CTXSW] Context-Switch Validation Suite");
    write_str_nl(b"[CTXSW] ====================================");

    let mut passed = 0u32;
    let mut failed = 0u32;

    macro_rules! run_test {
        ($func:expr) => { if $func() { passed += 1; } else { failed += 1; } };
    }

    run_test!(test_yield);
    run_test!(test_sleep);
    run_test!(test_blocking_read);
    run_test!(test_waitpid);
    run_test!(test_timer_preemption);
    run_test!(test_fork);
    run_test!(test_signal);
    run_test!(test_multi_context_switch);
    run_test!(test_rflags_preservation);
    run_test!(test_cow_multigeneration);
    run_test!(test_cow_isolation);
    run_test!(test_fork_stress);
    run_test!(test_memory_stress);
    run_test!(test_pipe_stress);
    run_test!(test_cow_fork_bomb);

    write_str_nl(b"[CTXSW] ====================================");
    write_str(b"[CTXSW] Results: "); write_hex(passed as u64);
    write_str(b" passed, "); write_hex(failed as u64); write_str(b" failed\n");

    if failed == 0 { write_str_nl(b"[CTXSW] ALL TESTS PASSED"); }
    else { write_str_nl(b"[CTXSW] SOME TESTS FAILED"); }

    sys::exit(if failed == 0 { 0 } else { 1 });
}
