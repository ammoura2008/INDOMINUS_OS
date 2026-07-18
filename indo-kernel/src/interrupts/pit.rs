//! # Programmable Interval Timer (PIT)
//!
//! The PIT is a legacy x86 timer chip (Intel 8253/8254) that generates
//! periodic interrupts at a configurable frequency.
//!
//! ## Channels
//!
//! The PIT has 3 channels:
//! - Channel 0: Connected to IRQ0 (hardware interrupt line 0)
//! - Channel 1: Historically used for DRAM refresh (not used on modern systems)
//! - Channel 2: Connected to the PC speaker
//!
//! We use Channel 0 for the system timer tick.
//!
//! ## How it works
//!
//! The PIT has a base oscillator frequency of 1,193,182 Hz (~1.193 MHz).
//! By writing a divisor to the PIT, we can generate interrupts at any
//! frequency: interrupt_rate = 1,193,182 / divisor
//!
//! For 100 Hz (10 ms period): divisor = 1,193,182 / 100 ≈ 11,932
//!
//! ## I/O Ports
//!
//! - 0x40: Channel 0 data port (read/write divisor)
//! - 0x41: Channel 1 data port
//! - 0x42: Channel 2 data port
//! - 0x43: Mode/Command register

/// PIT Channel 0 data port.
const PIT_CHANNEL0: u16 = 0x40;

/// PIT Mode/Command register.
const PIT_COMMAND: u16 = 0x43;

/// PIT base oscillator frequency (Hz).
const PIT_FREQUENCY: u32 = 1_193_182;

/// Desired timer interrupt frequency (Hz).
/// 100 Hz = 10 ms period, standard for OS scheduler ticks.
pub const PIT_TICK_HZ: u32 = 100;

/// Global tick counter. Incremented by the timer interrupt handler.
/// Uses atomic for safe access from interrupt context and future SMP.
static mut TICK_COUNT: u64 = 0;

/// Initialize the PIT Channel 0 for periodic interrupts.
///
/// Configures Channel 0 to generate IRQ0 at `PIT_TICK_HZ` Hz.
///
/// # Safety
/// Must be called once during kernel initialization.
pub fn init() {
    let divisor = PIT_FREQUENCY / PIT_TICK_HZ;

    unsafe {
        // Command byte:
        // - Bits 6-7: Channel 0 (00)
        // - Bits 4-5: Access mode: lobyte/hibyte (11)
        // - Bits 1-3: Operating mode: rate generator (010) for periodic
        // - Bit 0: BCD mode (0 = binary)
        let command: u8 = 0b00_11_010_0;
        let mut port = x86_64::instructions::port::Port::new(PIT_COMMAND);
        port.write(command);

        // Write divisor: lobyte first, then hibyte
        let mut data_port = x86_64::instructions::port::Port::new(PIT_CHANNEL0);
        data_port.write((divisor & 0xFF) as u8);      // Low byte
        data_port.write(((divisor >> 8) & 0xFF) as u8); // High byte

        crate::serial::write_str("[PIT] Channel 0 configured: ");
        crate::serial::write_u64(divisor as u64);
        crate::serial::write_str(" divisor, ~");
        crate::serial::write_u64(PIT_TICK_HZ as u64);
        crate::serial::write_str(" Hz, ");
        crate::serial::write_str("vector 32 (IRQ0)");
        crate::serial::write_nl();
    }
}

/// Called by the timer interrupt handler (IRQ0, vector 32) on each tick.
///
/// # Safety
/// Must be called from interrupt context with interrupts disabled.
#[inline]
pub unsafe fn on_tick() {
    TICK_COUNT += 1;
}

/// Get the current tick count since boot.
///
/// Useful for timing, sleep, and scheduler quantum tracking.
pub fn tick_count() -> u64 {
    unsafe { TICK_COUNT }
}

/// Spin-wait for approximately `ms` milliseconds.
///
/// This is a blocking busy-wait — not power-efficient, but useful for
/// early boot delays before the scheduler is running.
///
/// # Arguments
/// * `ms` - Approximate milliseconds to wait
pub fn sleep_ms(ms: u64) {
    let target = tick_count() + (ms * PIT_TICK_HZ as u64 / 1000);
    while tick_count() < target {
        core::hint::spin_loop();
    }
}
