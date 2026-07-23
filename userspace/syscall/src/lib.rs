//! # Indo Syscall
//!
//! Userspace syscall wrappers for Indominus OS.
//!
//! Provides safe Rust wrappers around all kernel syscalls using
//! inline assembly. Each syscall follows the Linux ABI:
//! - RAX = syscall number
//! - RDI, RSI, RDX, R10, R8, R9 = arguments
//! - Return value in RAX (0+ = success, negative = error)

#![no_std]

/// Syscall numbers
pub const SYS_WRITE: u64 = 0;
pub const SYS_EXIT: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_GETPID: u64 = 3;
pub const SYS_WAITPID: u64 = 4;
pub const SYS_SLEEP: u64 = 5;
pub const SYS_READ: u64 = 6;
pub const SYS_PIPE: u64 = 7;
pub const SYS_FORK: u64 = 8;
pub const SYS_EXEC: u64 = 9;
pub const SYS_CLOSE: u64 = 10;
pub const SYS_DUP: u64 = 11;
pub const SYS_OPEN: u64 = 12;
pub const SYS_LSEEK: u64 = 13;
pub const SYS_DUP2: u64 = 14;
pub const SYS_READDIR: u64 = 15;

/// Open flags (POSIX-compatible).
pub const O_RDONLY: u64 = 0x0000;
pub const O_WRONLY: u64 = 0x0001;
pub const O_RDWR: u64 = 0x0002;
pub const O_CREAT: u64 = 0x0040;
pub const O_TRUNC: u64 = 0x0200;
/// Close this FD on exec(). POSIX default is inherit (flag clear).
pub const O_CLOEXEC: u64 = 0x80000;

/// Error threshold: if result > -4096 (unsigned), it's an error.
const ERR_THRESHOLD: u64 = (-4096i64) as u64;

/// Raw syscall with 0 arguments
unsafe fn syscall0(num: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num as i64 => ret,
        out("rcx") _,
        out("r11") _,
    );
    ret
}

/// Raw syscall with 1 argument
unsafe fn syscall1(num: u64, arg1: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num as i64 => ret,
        in("rdi") arg1 as i64,
        out("rcx") _,
        out("r11") _,
    );
    ret
}

/// Raw syscall with 2 arguments
unsafe fn syscall2(num: u64, arg1: u64, arg2: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num as i64 => ret,
        in("rdi") arg1 as i64,
        in("rsi") arg2 as i64,
        out("rcx") _,
        out("r11") _,
    );
    ret
}

/// Raw syscall with 3 arguments
unsafe fn syscall3(num: u64, arg1: u64, arg2: u64, arg3: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num as i64 => ret,
        in("rdi") arg1 as i64,
        in("rsi") arg2 as i64,
        in("rdx") arg3 as i64,
        out("rcx") _,
        out("r11") _,
    );
    ret
}

// ─── File I/O ───────────────────────────────────────────────────────────────

/// Write bytes to a file descriptor.
/// Returns number of bytes written, or negative errno on error.
pub fn write(fd: u64, buf: &[u8]) -> i64 {
    unsafe { syscall3(SYS_WRITE, fd, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Read bytes from a file descriptor.
/// Returns number of bytes read, or negative errno on error.
pub fn read(fd: u64, buf: &mut [u8]) -> i64 {
    unsafe { syscall3(SYS_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Open a file by path with flags.
/// Returns fd number, or negative errno on error.
pub fn open(path: &str, flags: u64) -> i64 {
    unsafe { syscall2(SYS_OPEN, path.as_ptr() as u64, flags) }
}

/// Close a file descriptor.
/// Returns 0 on success, or negative errno on error.
pub fn close(fd: u64) -> i64 {
    unsafe { syscall1(SYS_CLOSE, fd) }
}

/// Seek to a position in a file.
/// Returns 0 on success, or negative errno on error.
pub fn lseek(fd: u64, offset: u64) -> i64 {
    unsafe { syscall2(SYS_LSEEK, fd, offset) }
}

// ─── Process Control ────────────────────────────────────────────────────────

/// Exit the current process.
pub fn exit(code: u64) -> ! {
    unsafe { syscall1(SYS_EXIT, code); }
    loop {}
}

/// Yield the CPU to the next process.
pub fn yield_now() {
    unsafe { syscall0(SYS_YIELD); }
}

/// Get the current process PID.
pub fn getpid() -> u64 {
    unsafe { syscall0(SYS_GETPID) as u64 }
}

/// Wait for a child process to exit.
/// Returns child's exit code, or negative errno on error.
pub fn waitpid(child_pid: u64) -> i64 {
    unsafe { syscall1(SYS_WAITPID, child_pid) }
}

/// Sleep for a specified number of timer ticks (10ms each).
pub fn sleep(ticks: u64) {
    unsafe { syscall1(SYS_SLEEP, ticks); }
}

/// Fork the current process.
/// Returns 0 in child, child PID in parent, or negative errno on error.
pub fn fork() -> i64 {
    unsafe { syscall0(SYS_FORK) }
}

/// Execute a new program, replacing the current address space.
/// Returns 0 on success, or negative errno on error.
pub fn exec(path: &str) -> i64 {
    unsafe { syscall1(SYS_EXEC, path.as_ptr() as u64) }
}

// ─── Pipes ──────────────────────────────────────────────────────────────────

/// Create a pipe pair.
/// Returns (read_fd << 32) | write_fd, or negative errno on error.
pub fn pipe() -> i64 {
    unsafe { syscall0(SYS_PIPE) }
}

/// Duplicate a file descriptor.
/// Returns new fd number, or negative errno on error.
pub fn dup(fd: u64) -> i64 {
    unsafe { syscall1(SYS_DUP, fd) }
}

/// Duplicate a file descriptor to a specific target number.
/// Returns newfd on success, or negative errno on error.
pub fn dup2(oldfd: u64, newfd: u64) -> i64 {
    unsafe { syscall2(SYS_DUP2, oldfd, newfd) }
}

/// Read directory entries into a buffer.
/// Returns bytes written, or negative errno on error.
pub fn readdir(fd: u64, buf: &mut [u8]) -> i64 {
    unsafe { syscall3(SYS_READDIR, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

// ─── Convenience Functions ──────────────────────────────────────────────────

/// Write a string to stdout.
pub fn println(s: &str) {
    write(1, s.as_bytes());
    write(1, b"\n");
}

/// Write a string to stderr.
pub fn eprintln(s: &str) {
    write(2, s.as_bytes());
    write(2, b"\n");
}

/// Check if a syscall result indicates an error.
pub fn is_error(result: i64) -> bool {
    (result as u64) > ERR_THRESHOLD
}

/// Get the errno from a syscall result.
pub fn get_errno(result: i64) -> i64 {
    -result
}
