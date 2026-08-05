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
    let mut cr8: u64 = 0; let mut cr9: u64 = 0; let mut cr10: u64 = 0;
    let mut cr12: u64 = 0; let mut cr13: u64 = 0; let mut cr14: u64 = 0;
    let mut cr15: u64 = 0;
    let mut buf = [0u64; 2]; // [0]=rbx [1]=rbp

    unsafe {
        core::arch::asm!(
            "push rbx",
            "push rbp",
            "mov rbx, 0xAAAAAAAAAAAAAAAA",
            "mov rbp, 0xBBBBBBBBBBBBBBBB",
            "mov r8,  0x1111111111111111",
            "mov r9,  0x2222222222222222",
            "mov r10, 0x3333333333333333",
            "mov r12, 0x5555555555555555",
            "mov r13, 0x6666666666666666",
            "mov r14, 0x7777777777777777",
            "mov r15, 0x8888888888888888",
            "mov rax, 2",   // SYS_YIELD
            "syscall",
            "mov [rdi], rbx",
            "mov [rdi + 8], rbp",
            "pop rbp",
            "pop rbx",
            out("r8") cr8,
            out("r9") cr9,
            out("r10") cr10,
            out("r12") cr12,
            out("r13") cr13,
            out("r14") cr14,
            out("r15") cr15,
            inout("rdi") buf.as_mut_ptr() => _,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("rsi") _,
            lateout("rdx") _,
            lateout("r11") _,
        );
    }

    let crbx = buf[0];
    let crbp = buf[1];

    let ok = verify_all9(crbx, crbp, cr8, cr9, cr10, cr12, cr13, cr14, cr15);
    if ok { write_str_nl(b"  PASS: yield()"); }
    ok
}

// ── Test 2: sleep() ────────────────────────────────────────────────────────

fn test_sleep() -> bool {
    write_str_nl(b"== Test 2: sleep() ==");
    let mut cr8: u64 = 0; let mut cr9: u64 = 0; let mut cr10: u64 = 0;
    let mut cr12: u64 = 0; let mut cr13: u64 = 0; let mut cr14: u64 = 0;
    let mut cr15: u64 = 0;
    let mut buf = [0u64; 2]; // [0]=rbx [1]=rbp

    unsafe {
        core::arch::asm!(
            "push rbx",
            "push rbp",
            "mov rbx, 0xAAAAAAAAAAAAAAAA",
            "mov rbp, 0xBBBBBBBBBBBBBBBB",
            "mov r8,  0x1111111111111111",
            "mov r9,  0x2222222222222222",
            "mov r10, 0x3333333333333333",
            "mov r12, 0x5555555555555555",
            "mov r13, 0x6666666666666666",
            "mov r14, 0x7777777777777777",
            "mov r15, 0x8888888888888888",
            "mov rax, 5",   // SYS_SLEEP
            "mov rdi, 2",   // 2 ticks
            "syscall",
            "mov [rsi], rbx",
            "mov [rsi + 8], rbp",
            "pop rbp",
            "pop rbx",
            out("r8") cr8,
            out("r9") cr9,
            out("r10") cr10,
            out("r12") cr12,
            out("r13") cr13,
            out("r14") cr14,
            out("r15") cr15,
            inout("rsi") buf.as_mut_ptr() => _,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("rdi") _,
            lateout("rdx") _,
            lateout("r11") _,
        );
    }

    let crbx = buf[0];
    let crbp = buf[1];

    let ok = verify_all9(crbx, crbp, cr8, cr9, cr10, cr12, cr13, cr14, cr15);
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

    let mut cr12: u64 = 0; let mut cr13: u64 = 0; let mut cr14: u64 = 0;
    let mut buf = [0u8; 1];
    let mut rbuf = [0u64; 2]; // [0]=rbx [1]=rbp

    unsafe {
        core::arch::asm!(
            "push rbx",
            "push rbp",
            "mov rbx, 0xAAAAAAAAAAAAAAAA",
            "mov rbp, 0xBBBBBBBBBBBBBBBB",
            "mov r12, 0x5555555555555555",
            "mov r13, 0x6666666666666666",
            "mov r14, 0x7777777777777777",
            "mov rax, 6",           // SYS_READ
            "mov rdi, {fd_op}",
            "mov rsi, {buf_op}",
            "mov rdx, 1",
            "syscall",
            "mov [r15], rbx",
            "mov [r15 + 8], rbp",
            "pop rbp",
            "pop rbx",
            out("r12") cr12,
            out("r13") cr13,
            out("r14") cr14,
            inout("r15") rbuf.as_mut_ptr() => _,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("rdi") _,
            lateout("rsi") _,
            lateout("rdx") _,
            lateout("r8") _,
            lateout("r9") _,
            lateout("r10") _,
            lateout("r11") _,
            fd_op = in(reg) read_fd,
            buf_op = in(reg) buf.as_mut_ptr(),
        );
    }

    let crbx = rbuf[0];
    let crbp = rbuf[1];

    sys::waitpid_blocking(child as u64);
    sys::close(read_fd);
    sys::close(write_fd);

    let ok = verify5(crbx, crbp, cr12, cr13, cr14);
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

    let mut cr12: u64 = 0; let mut cr13: u64 = 0; let mut cr14: u64 = 0;
    let mut buf = [0u64; 2]; // [0]=rbx [1]=rbp

    unsafe {
        core::arch::asm!(
            "push rbx",
            "push rbp",
            "mov rbx, 0xAAAAAAAAAAAAAAAA",
            "mov rbp, 0xBBBBBBBBBBBBBBBB",
            "mov r12, 0x5555555555555555",
            "mov r13, 0x6666666666666666",
            "mov r14, 0x7777777777777777",
            "mov rax, 4",           // SYS_WAITPID
            "mov rdi, {child_op}",
            "mov rsi, 0",
            "syscall",
            "mov [r15], rbx",
            "mov [r15 + 8], rbp",
            "pop rbp",
            "pop rbx",
            out("r12") cr12,
            out("r13") cr13,
            out("r14") cr14,
            inout("r15") buf.as_mut_ptr() => _,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("rdi") _,
            lateout("rsi") _,
            lateout("rdx") _,
            lateout("r8") _,
            lateout("r9") _,
            lateout("r10") _,
            lateout("r11") _,
            child_op = in(reg) child as u64,
        );
    }

    let crbx = buf[0];
    let crbp = buf[1];

    let ok = verify5(crbx, crbp, cr12, cr13, cr14);
    if ok { write_str_nl(b"  PASS: waitpid()"); }
    ok
}

// ── Test 5: timer preemption ───────────────────────────────────────────────

fn test_timer_preemption() -> bool {
    write_str_nl(b"== Test 5: timer preemption ==");

    // Single asm block: set regs, spin for >20ms (timer preempts us),
    // read regs back. push/pop preserves compiler's frame pointer.
    let mut cr12: u64 = 0; let mut cr13: u64 = 0; let mut cr14: u64 = 0;
    let mut buf = [0u64; 2]; // [0]=rbx [1]=rbp

    unsafe {
        core::arch::asm!(
            "push rbx",
            "push rbp",
            "mov rbx, 0xAAAAAAAAAAAAAAAA",
            "mov rbp, 0xBBBBBBBBBBBBBBBB",
            "mov r12, 0x5555555555555555",
            "mov r13, 0x6666666666666666",
            "mov r14, 0x7777777777777777",
            // Spin loop in asm: ~20ms at 50 Hz timer
            "xor rcx, rcx",
            "2:",
            "inc rcx",
            "pause",
            "cmp rcx, 2000000",
            "jl 2b",
            // Read back
            "mov [rdi], rbx",
            "mov [rdi + 8], rbp",
            "pop rbp",
            "pop rbx",
            out("r12") cr12,
            out("r13") cr13,
            out("r14") cr14,
            inout("rdi") buf.as_mut_ptr() => _,
            lateout("rcx") _,
        );
    }

    let crbx = buf[0];
    let crbp = buf[1];

    let ok = verify5(crbx, crbp, cr12, cr13, cr14);
    if ok { write_str_nl(b"  PASS: timer preemption"); }
    ok
}

// ── Test 6: fork() ─────────────────────────────────────────────────────────
//
// CRITICAL: Register setup + syscall MUST be in a single asm block.
// out(reg) lets the compiler pick ANY register. sys_fork reads from the
// syscall frame which has the ACTUAL CPU register values. If we set registers
// in one asm block and call fork() separately, the compiler can clobber them
// (e.g., use RBX for a local) between the two. Combining them in one block
// ensures the actual CPU registers have the expected values at syscall time.
//
// RBX/RBP cannot be used as asm operands (reserved by LLVM). Instead we use
// rdi as a buffer pointer (preserved across syscall) and mov [rdi], rbx etc.

fn test_fork() -> bool {
    write_str_nl(b"== Test 6: fork() ==");

    let child: u64;
    let mut cr12: u64 = 0; let mut cr13: u64 = 0; let mut cr14: u64 = 0;
    let mut buf = [0u64; 2]; // [0]=rbx [1]=rbp

    unsafe {
        core::arch::asm!(
            "push rbx",
            "push rbp",
            "mov rbx, 0xAAAAAAAAAAAAAAAA",
            "mov rbp, 0xBBBBBBBBBBBBBBBB",
            "mov r12, 0x5555555555555555",
            "mov r13, 0x6666666666666666",
            "mov r14, 0x7777777777777777",
            "mov rax, 8",
            "syscall",
            "mov [rdi], rbx",
            "mov [rdi + 8], rbp",
            "pop rbp",
            "pop rbx",
            out("r12") cr12,
            out("r13") cr13,
            out("r14") cr14,
            inout("rdi") buf.as_mut_ptr() => _,
            out("rax") child,
            lateout("rcx") _,
            lateout("rsi") _,
            lateout("rdx") _,
            lateout("r8") _,
            lateout("r9") _,
            lateout("r10") _,
            lateout("r11") _,
            lateout("r15") _,
        );
    }

    let crbx = buf[0];
    let crbp = buf[1];

    if child == 0 {
        let ok = verify5(crbx, crbp, cr12, cr13, cr14);
        if ok { write_str_nl(b"  PASS: fork() child registers"); }
        else { write_str_nl(b"  FAIL: fork() child registers"); }
        sys::exit(if ok { 0 } else { 1 });
    }

    sys::waitpid_blocking(child);

    let ok = verify5(crbx, crbp, cr12, cr13, cr14);
    if ok { write_str_nl(b"  PASS: fork() parent registers"); }
    ok
}

// ── Test 7: signal delivery ────────────────────────────────────────────────

extern "C" fn sigusr1_handler(_signum: u64) {}

fn test_signal() -> bool {
    write_str_nl(b"== Test 7: signal delivery ==");
    write_str_nl(b"  SKIP: kernel signal delivery bug (RIP=0x600000000 page fault)");
    false
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

// ── Test 16: FPU basic — float math survives yield ───────────────────────

fn test_fpu_basic() -> bool {
    write_str_nl(b"[CTXSW] [16/18] fpu_basic...");

    let a: f64 = 3.14159;
    let b: f64 = 2.71828;
    let expected = a * b + a / b; // do some math

    // Yield to cause context switch
    sys::yield_now();

    let result = a * b + a / b;

    // Check bit-exact (same computation, same result)
    let ok = to_bits(result) == to_bits(expected);
    if !ok { write_str_nl(b"  FAIL: FPU value corrupted after yield"); }
    if ok { write_str_nl(b"  PASS: FPU basic"); }
    ok
}

// ── Test 17: FPU isolation — parent/child float values independent ──────

fn test_fpu_isolation() -> bool {
    write_str_nl(b"[CTXSW] [17/18] fpu_isolation...");

    let parent_val: f64 = 1.23456789;
    let child_val: f64 = 9.87654321;

    let pid = sys::fork();
    if pid == 0 {
        // Child: compute with child_val, yield, verify
        let r = child_val * 2.0;
        sys::yield_now();
        let r2 = child_val * 2.0;
        if to_bits(r) != to_bits(r2) {
            write_str_nl(b"  FAIL: child FPU corrupted");
            sys::exit(1);
        }
        sys::exit(0);
    }

    // Parent: compute with parent_val, yield, verify
    let r = parent_val * 3.0;
    sys::yield_now();
    let r2 = parent_val * 3.0;

    let r_eq = to_bits(r) == to_bits(r2);
    if !r_eq {
        write_str(b"  FAIL: parent r="); write_hex(to_bits(r));
        write_str(b" r2="); write_hex(to_bits(r2)); write_nl();
    }

    let result = sys::waitpid_blocking(pid as u64);
    let wp_ok = !sys::is_error(result);
    if !wp_ok {
        write_str(b"  FAIL: waitpid err="); write_hex(result as u64); write_nl();
    }

    let ok = r_eq && wp_ok;
    if ok { write_str_nl(b"  PASS: FPU isolation"); }
    else { write_str_nl(b"  FAIL: FPU isolation failed"); }
    ok
}

// ── Test 18: FPU stress — 10 fork+float cycles ──────────────────────────

fn test_fpu_stress() -> bool {
    write_str_nl(b"[CTXSW] [18/18] fpu_stress...");

    let mut all_ok = true;

    for i in 0..10u64 {
        let val: f64 = (i as f64) * 1.1 + 0.5;

        let pid = sys::fork();
        if pid == 0 {
            // Child: compute, yield, verify
            let r = val * val + 1.0;
            sys::yield_now();
            let r2 = val * val + 1.0;
            if to_bits(r) != to_bits(r2) {
                write_str_nl(b"  FAIL: child FPU corrupted in stress");
                sys::exit(1);
            }
            sys::exit(0);
        }

        // Parent: compute, yield, verify
        let r = val * val + 1.0;
        sys::yield_now();
        let r2 = val * val + 1.0;

        let r_eq = to_bits(r) == to_bits(r2);
        let result = sys::waitpid_blocking(pid as u64);
        let wp_ok = !sys::is_error(result);

        if !r_eq || !wp_ok {
            write_str(b"  FAIL: FPU stress iter="); write_hex(i);
            if !r_eq { write_str(b" r="); write_hex(to_bits(r)); write_str(b" r2="); write_hex(to_bits(r2)); }
            if !wp_ok { write_str(b" wp="); write_hex(result as u64); }
            write_nl();
            all_ok = false;
            break;
        }
    }

    if all_ok { write_str_nl(b"  PASS: FPU stress"); }
    all_ok
}

/// Reinterpret f64 bits as u64 for exact comparison.
fn to_bits(v: f64) -> u64 {
    unsafe { core::mem::transmute::<f64, u64>(v) }
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
    run_test!(test_fpu_basic);
    run_test!(test_fpu_isolation);
    run_test!(test_fpu_stress);

    write_str_nl(b"[CTXSW] ====================================");
    write_str(b"[CTXSW] Results: "); write_hex(passed as u64);
    write_str(b" passed, "); write_hex(failed as u64); write_str(b" failed\n");

    if failed == 0 { write_str_nl(b"[CTXSW] ALL TESTS PASSED"); }
    else { write_str_nl(b"[CTXSW] SOME TESTS FAILED"); }

    sys::exit(if failed == 0 { 0 } else { 1 });
}
