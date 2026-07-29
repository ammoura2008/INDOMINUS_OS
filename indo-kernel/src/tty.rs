//! # TTY — Terminal Discipline
//!
//! Provides terminal settings (termios) and signal character handling.
//! Ctrl+C sends SIGINT, Ctrl+Z sends SIGTSTP, Ctrl+\ sends SIGQUIT.
//!
//! The TTY has two modes:
//! - Canonical (cooked): line-buffered, signal chars processed, backspace handled
//! - Raw: character-at-a-time, no processing, no echo

use core::sync::atomic::{AtomicU8, Ordering};

/// Signal character codes (Ctrl+key = key & 0x1F)
pub const SIGINT_CHAR: u8 = 0x03;   // Ctrl+C
pub const SIGTSTP_CHAR: u8 = 0x1A;  // Ctrl+Z
pub const SIGQUIT_CHAR: u8 = 0x1C;  // Ctrl+\

/// Termios flags
const ICANON: u8 = 0x01;
const ECHO: u8 = 0x02;
const ISIG: u8 = 0x04;
const IEXTEN: u8 = 0x08;

/// Global TTY settings (single console for now)
static TTY_LFLAGS: AtomicU8 = AtomicU8::new(ICANON | ECHO | ISIG);
static TTY_IFLAG: AtomicU8 = AtomicU8::new(0);
static TTY_OFLAG: AtomicU8 = AtomicU8::new(0);

/// Get the current terminal flags
pub fn tty_get_lflags() -> u8 {
    TTY_LFLAGS.load(Ordering::Relaxed)
}

/// Set the terminal local flags
pub fn tty_set_lflags(flags: u8) {
    TTY_LFLAGS.store(flags, Ordering::Relaxed);
}

/// Check if canonical mode is enabled
pub fn is_canonical() -> bool {
    TTY_LFLAGS.load(Ordering::Relaxed) & ICANON != 0
}

/// Check if echo is enabled
pub fn is_echo() -> bool {
    TTY_LFLAGS.load(Ordering::Relaxed) & ECHO != 0
}

/// Check if signal processing is enabled
pub fn is_sig_enabled() -> bool {
    TTY_LFLAGS.load(Ordering::Relaxed) & ISIG != 0
}

/// Termios structure for tcgetattr/tcsetattr
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    /// Input flags
    pub iflag: u32,
    /// Output flags
    pub oflag: u32,
    /// Control flags (baud rate, etc.)
    pub cflag: u32,
    /// Local flags (canonical, echo, signals)
    pub lflag: u32,
    /// Control characters array (19 bytes, padded to 32)
    pub cc: [u8; 32],
}

impl Termios {
    /// Create a default termios (cooked mode)
    pub fn default_cooked() -> Self {
        let mut cc = [0u8; 32];
        // Standard control characters
        cc[0] = 1;   // VINTR (Ctrl+C)
        cc[1] = 28;  // VQUIT (Ctrl+\)
        cc[2] = 127; // VERASE (Backspace)
        cc[3] = 3;   // VEOF (Ctrl+D)
        cc[4] = 26;  // VSUSP (Ctrl+Z)
        Termios {
            iflag: 0,
            oflag: 0,
            cflag: 0,
            lflag: (ICANON | ECHO | ISIG) as u32,
            cc,
        }
    }

    /// Create a raw termios
    pub fn default_raw() -> Self {
        let mut cc = [0u8; 32];
        cc[0] = 1;
        cc[1] = 28;
        cc[2] = 127;
        cc[3] = 3;
        cc[4] = 26;
        Termios {
            iflag: 0,
            oflag: 0,
            cflag: 0,
            lflag: 0, // No canonical, no echo, no signals
            cc,
        }
    }
}

/// Apply a termios structure to the global TTY settings
pub fn tty_apply_termios(termios: &Termios) {
    let flags = termios.lflag as u8;
    TTY_LFLAGS.store(flags, Ordering::Relaxed);
    TTY_IFLAG.store(termios.iflag as u8, Ordering::Relaxed);
    TTY_OFLAG.store(termios.oflag as u8, Ordering::Relaxed);
}

/// Check if a byte is a signal character and send the appropriate signal.
///
/// Returns:
/// - 0: not a signal char (process normally)
/// - SIGINT (2): Ctrl+C
/// - SIGTSTP (20): Ctrl+Z
/// - SIGQUIT (3): Ctrl+\
pub fn check_signal_char(byte: u8) -> u8 {
    if !is_sig_enabled() {
        return 0;
    }
    match byte {
        SIGINT_CHAR => 2,   // SIGINT
        SIGTSTP_CHAR => 20, // SIGTSTP
        SIGQUIT_CHAR => 3,  // SIGQUIT
        _ => 0,
    }
}
