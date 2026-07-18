//! # Kernel Panic Handler
//!
//! ## What is a panic handler?
//!
//! In `no_std` Rust, you MUST define what happens when the program panics
//! (e.g., array out of bounds, explicit `panic!()`, failed `unwrap()`).
//!
//! In userland Rust, the standard library provides this and unwinds the stack.
//! We have NO standard library and NO stack unwinder — so we must define our own.
//!
//! ## Our panic strategy: fail loudly, halt immediately
//!
//! A kernel panic is an unrecoverable error. Our goal is:
//! 1. Print as much debug info as possible to serial BEFORE we halt
//! 2. Disable interrupts (prevent re-entrancy into broken state)
//! 3. Halt permanently
//!
//! In Phase 1+ we will add:
//! - Stack trace walking (using DWARF debug info)
//! - Register dump
//! - Memory state snapshot
//! - Optional reboot after timeout

use core::panic::PanicInfo;

/// The `#[panic_handler]` attribute marks this as THE function Rust calls
/// when a panic occurs anywhere in the kernel. There can be exactly one.
///
/// `PanicInfo` contains:
/// - The panic message (from `panic!("message")`)
/// - The source file and line number where the panic occurred
/// - Whether it was an explicit `panic!` or an implicit assertion failure
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Disable interrupts immediately. We don't want a timer interrupt
    // firing in the middle of our panic output and corrupting the serial state.
    unsafe {
        core::arch::asm!("cli", options(nostack, nomem));
    }

    crate::kprintln!();
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("  INDOMINUS KERNEL PANIC");
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Print the panic message if one was provided.
    if let Some(message) = info.message().as_str() {
        crate::kprintln!("  Message  : {}", message);
    } else {
        crate::kprintln!("  Message  : (no message)");
    }

    // Print source location (file + line number).
    if let Some(location) = info.location() {
        crate::kprintln!("  Location : {}:{}", location.file(), location.line());
    }

    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("  System halted. Connect GDB or restart.");

    // Halt permanently.
    crate::halt()
}
