/// # Errno — System Call Error Codes
///
/// Negative errno values returned by syscalls on error.
/// Follows Linux convention: 0 = success, negative = error.

/// Operation not permitted
pub const EPERM: i64 = -1;
/// No such file or directory
pub const ENOENT: i64 = -2;
/// No such process
pub const ESRCH: i64 = -3;
/// Interrupted system call
pub const EINTR: i64 = -4;
/// I/O error
pub const EIO: i64 = -5;
/// Bad file descriptor
pub const EBADF: i64 = -9;
/// Out of memory
pub const ENOMEM: i64 = -12;
/// Bad address
pub const EFAULT: i64 = -14;
/// Invalid argument
pub const EINVAL: i64 = -22;
/// Too many open files
pub const EMFILE: i64 = -24;
/// No space left on device
pub const ENOSPC: i64 = -28;
/// Broken pipe
pub const EPIPE: i64 = -32;
/// Exec format error
pub const ENOEXEC: i64 = -8;
/// Function not implemented
pub const ENOSYS: i64 = -38;
/// Not a directory
pub const ENOTDIR: i64 = -20;

/// Maximum errno value (absolute). Used by userspace to detect errors:
/// if result > -4096 as unsigned, it's an error.
pub const MAX_ERRNO: i64 = -4095;
