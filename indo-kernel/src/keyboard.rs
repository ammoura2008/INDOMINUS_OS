//! # PS/2 Keyboard Driver
//!
//! Handles IRQ1 (vector 33) from the PS/2 keyboard controller.
//! Reads scancodes from port 0x60, translates to ASCII, and provides
//! line-buffered input with echo and backspace support.
//!
//! Architecture:
//! ```text
//! Keyboard IRQ → handler() → line_discipline_input() → line buffer
//!                                                      ↓
//! sys_read(stdin) ← pop_line_bytes() ← line buffer
//! ```

use core::sync::atomic::{AtomicUsize, AtomicBool, Ordering};

/// Ring buffer size for raw keyboard input (used by handler).
const KBDBUF_SIZE: usize = 256;

/// Line buffer size — maximum characters per line.
const LINE_BUF_SIZE: usize = 4096;

/// Static ring buffer for raw keyboard input (unused now, kept for potential raw mode).
static KBD_BUF: crate::sync_cell::SyncUnsafeCell<[u8; KBDBUF_SIZE]> = crate::sync_cell::SyncUnsafeCell::new([0u8; KBDBUF_SIZE]);
static KBD_HEAD: AtomicUsize = AtomicUsize::new(0);
static KBD_TAIL: AtomicUsize = AtomicUsize::new(0);
static KBD_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Line buffer — three-pointer design (xv6-inspired, but larger).
///
/// `r` = read index — consumed by sys_read (bytes delivered to userspace)
/// `w` = write index — line boundary (committed when Enter is pressed)
/// `e` = edit index — current typing position (characters accumulate here)
///
/// Invariant: r <= w <= e. During typing, w == e. When Enter is pressed,
/// w advances to e, delivering a complete line to readers.
static LINE_BUF: crate::sync_cell::SyncUnsafeCell<[u8; LINE_BUF_SIZE]> = crate::sync_cell::SyncUnsafeCell::new([0u8; LINE_BUF_SIZE]);
static LINE_R: AtomicUsize = AtomicUsize::new(0);
static LINE_W: AtomicUsize = AtomicUsize::new(0);
static LINE_E: AtomicUsize = AtomicUsize::new(0);

/// Scancode set 1 → ASCII translation table (make codes only, no E0 prefix).
/// Index = scancode, value = ASCII (0 = non-printable, skip).
static SCANCODE_TO_ASCII: [u8; 128] = [
    0,    0,   b'1', b'2', b'3', b'4', b'5', b'6',
    b'7', b'8', b'9', b'0', b'-', b'=', 0,    0,
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i',
    b'o', b'p', b'[', b']', 0,    0,   b'a', b's',
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';',
    b'\'', b'`', 0,   b'\\', b'z', b'x', b'c', b'v',
    b'b', b'n', b'm', b',', b'.', b'/', 0,    b'*',
    0,    b' ', 0,    0,    0,    0,    0,    0,
    0,    0,    0,    0,    0,    0,    0,    0,
    0,    0,    b'-', 0,    0,    0,    b'+', 0,
    0,    0,    0,    0,    0,    0,    0,    0,
    0,    0,    0,    0,    0,    0,    0,    0,
    0,    0,    0,    0,    0,    0,    0,    0,
    0,    0,    0,    0,    0,    0,    0,    0,
    0,    0,    0,    0,    0,    0,    0,    0,
    0,    0,    0,    0,    0,    0,    0,    0,
];

/// Current shift state for scancode translation.
static SHIFT_HELD: AtomicBool = AtomicBool::new(false);

/// Push a byte into the raw ring buffer (unused by line discipline).
#[allow(dead_code)]
fn push_byte(b: u8) {
    let count = KBD_COUNT.load(Ordering::Relaxed);
    if count >= KBDBUF_SIZE {
        return;
    }
    let head = KBD_HEAD.load(Ordering::Relaxed);
    unsafe { core::ptr::write_volatile((*KBD_BUF.get()).as_mut_ptr().add(head), b); }
    KBD_HEAD.store((head + 1) % KBDBUF_SIZE, Ordering::Relaxed);
    KBD_COUNT.store(count + 1, Ordering::Relaxed);
}

/// Pop a byte from the raw ring buffer. Returns None if empty.
#[allow(dead_code)]
pub fn pop_byte() -> Option<u8> {
    let count = KBD_COUNT.load(Ordering::Relaxed);
    if count == 0 { return None; }
    let tail = KBD_TAIL.load(Ordering::Relaxed);
    let b = unsafe { core::ptr::read_volatile((*KBD_BUF.get()).as_mut_ptr().add(tail)) };
    KBD_TAIL.store((tail + 1) % KBDBUF_SIZE, Ordering::Relaxed);
    KBD_COUNT.store(count - 1, Ordering::Relaxed);
    Some(b)
}

/// Check if the raw keyboard buffer has data.
#[allow(dead_code)]
pub fn has_data() -> bool {
    KBD_COUNT.load(Ordering::Relaxed) > 0
}

/// Line discipline input — processes a keystroke through the line buffer.
///
/// Handles:
/// - Printable characters: echo to serial, append to edit buffer
/// - Backspace (0x08): remove last character from edit buffer, echo `\b \b`
/// - Enter (0x0A): commit line (advance w to e), wake blocked readers
/// - Other: ignore
fn line_discipline_input(byte: u8) {
    match byte {
        b'\n' => {
            // Enter — commit the line
            // Echo newline to serial
            crate::serial::write_byte(b'\n');
            // Advance write index to edit index (committed line)
            LINE_W.store(LINE_E.load(Ordering::Relaxed), Ordering::Relaxed);
            // Wake blocked readers
            crate::process::keyboard_wake();
        }
        0x08 => {
            // Backspace — remove last character if available
            let r = LINE_R.load(Ordering::Relaxed);
            let e = LINE_E.load(Ordering::Relaxed);
            if e > r {
                // Decrement edit index
                LINE_E.store(e - 1, Ordering::Relaxed);
                // Echo backspace sequence: move cursor back, overwrite with space, move back again
                crate::serial::write_byte(0x08);
                crate::serial::write_byte(b' ');
                crate::serial::write_byte(0x08);
            }
        }
        _ => {
            // Printable character — append to edit buffer if space available
            let e = LINE_E.load(Ordering::Relaxed);
            // Only append if there's space (leave room for at least one more line)
            if e < LINE_BUF_SIZE {
                unsafe { core::ptr::write_volatile((*LINE_BUF.get()).as_mut_ptr().add(e), byte); }
                LINE_E.store(e + 1, Ordering::Relaxed);
                // Echo the character to serial
                crate::serial::write_byte(byte);
            }
        }
    }
}

/// Read bytes from the line buffer into the provided slice.
/// Returns the number of bytes read. Blocks if no complete line is available.
///
/// Called by sys_read for stdin. Reads from committed lines (r..w).
pub fn read_line(buf: &mut [u8]) -> usize {
    loop {
        let r = LINE_R.load(Ordering::Relaxed);
        let w = LINE_W.load(Ordering::Relaxed);

        if r < w {
            // Data available — copy from line buffer to user buffer
            let available = w - r;
            let to_read = core::cmp::min(buf.len(), available);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (*LINE_BUF.get()).as_mut_ptr().add(r),
                    buf.as_mut_ptr(),
                    to_read,
                );
            }
            LINE_R.store(r + to_read, Ordering::Relaxed);

            // If we've consumed all committed data, reset the buffer (circular)
            if LINE_R.load(Ordering::Relaxed) == LINE_W.load(Ordering::Relaxed) {
                LINE_R.store(0, Ordering::Relaxed);
                LINE_W.store(0, Ordering::Relaxed);
                LINE_E.store(0, Ordering::Relaxed);
            }

            return to_read;
        }

        // No committed data — block this process until a line is committed.
        // keyboard_wake() (called by line_discipline_input on Enter) will
        // transition us back to Ready.
        {
            let mut sched = crate::process::scheduler::SCHEDULER.lock();
            if let Some(pid) = sched.current_pid() {
                if let Some(ref mut proc) = sched.processes_mut()[pid as usize] {
                    proc.state = crate::process::ProcessState::Blocked;
                    proc.wake_reason = crate::process::WakeReason::Keyboard;
                }
            }
        }
        crate::process::yield_now();
    }
}

/// PS/2 keyboard IRQ1 handler.
///
/// Reads scancode from port 0x60, translates to ASCII, and feeds to line discipline.
pub fn handler() {
    let scancode: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") scancode, in("dx") 0x60u16, options(nostack, nomem));
    }

    // Ignore break codes (bit 7 set)
    if scancode & 0x80 != 0 {
        let make = scancode & 0x7F;
        if make == 0x2A || make == 0x36 {
            SHIFT_HELD.store(false, Ordering::Relaxed);
        }
        return;
    }

    // Track shift state
    if scancode == 0x2A || scancode == 0x36 {
        SHIFT_HELD.store(true, Ordering::Relaxed);
        return;
    }

    // Handle special keys
    match scancode {
        0x0E => { line_discipline_input(0x08); return; } // Backspace
        0x1C => { line_discipline_input(b'\n'); return; } // Enter
        0x39 => { line_discipline_input(b' '); return; } // Space
        _ => {}
    }

    // Translate scancode to ASCII
    if (scancode as usize) < SCANCODE_TO_ASCII.len() {
        let mut ascii = SCANCODE_TO_ASCII[scancode as usize];
        if ascii != 0 {
            if SHIFT_HELD.load(Ordering::Relaxed) {
                if ascii >= b'a' && ascii <= b'z' {
                    ascii = ascii - b'a' + b'A';
                } else {
                    ascii = match ascii {
                        b'1' => b'!', b'2' => b'@', b'3' => b'#', b'4' => b'$', b'5' => b'%',
                        b'6' => b'^', b'7' => b'&', b'8' => b'*', b'9' => b'(', b'0' => b')',
                        b'-' => b'_', b'=' => b'+', b'[' => b'{', b']' => b'}', b'\\' => b'|',
                        b';' => b':', b'\'' => b'"', b'`' => b'~', b',' => b'<', b'.' => b'>',
                        b'/' => b'?',
                        _ => ascii,
                    };
                }
            }
            line_discipline_input(ascii);
        }
    }
}

/// Initialize the keyboard driver.
pub fn init() {
    crate::serial::write_str("[KBD] Initializing PS/2 keyboard driver\n");
    crate::interrupts::dispatch::register(1, handler);
    crate::serial::write_str("[KBD] Keyboard driver initialized\n");
}
