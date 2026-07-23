//! # Pipe Implementation
//!
//! Inter-process communication via pipe pairs.
//! Uses a 512-byte ring buffer with blocking read/write.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

/// Size of the pipe ring buffer.
pub const PIPE_SIZE: usize = 512;

/// A pipe for inter-process communication.
///
/// Uses monotonic counters for the ring buffer (nread, nwrite).
/// Buffer full = nwrite == nread + PIPE_SIZE.
/// Buffer empty = nread == nwrite.
pub struct Pipe {
    /// Ring buffer data.
    pub data: [u8; PIPE_SIZE],
    /// Monotonic read counter (bytes read total).
    pub nread: AtomicU32,
    /// Monotonic write counter (bytes written total).
    pub nwrite: AtomicU32,
    /// Is the read end still open?
    pub read_open: AtomicBool,
    /// Is the write end still open?
    pub write_open: AtomicBool,
    /// Reference count — number of FDs (read + write) referencing this pipe.
    /// Starts at 2 (one read + one write). Decremented on each sys_close.
    /// When refcount reaches 0, the pipe slot is freed.
    pub refcount: AtomicU8,
}

impl Pipe {
    /// Create a new pipe with both ends open.
    pub fn new() -> Self {
        Pipe {
            data: [0u8; PIPE_SIZE],
            nread: AtomicU32::new(0),
            nwrite: AtomicU32::new(0),
            read_open: AtomicBool::new(true),
            write_open: AtomicBool::new(true),
            refcount: AtomicU8::new(2), // One read end + one write end
        }
    }
}

/// Close one end of a pipe.
///
/// `writable` indicates which end is being closed.
/// Wakes the other end so it can detect the close.
pub fn pipe_close(pipe: &mut Pipe, writable: bool) {
    if writable {
        pipe.write_open.store(false, Ordering::Relaxed);
        // Wake readers so they can detect EOF
        crate::process::keyboard_wake();
    } else {
        pipe.read_open.store(false, Ordering::Relaxed);
        // Wake writers so they can detect broken pipe
        crate::process::keyboard_wake();
    }
}
