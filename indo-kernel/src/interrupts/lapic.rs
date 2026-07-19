//! # Local APIC (LAPIC)
//!
//! The LAPIC is a per-CPU interrupt controller built into every x86 core.
//! It handles:
//! - Receiving interrupts from the IO-APIC or other CPUs
//! - Delivering timer interrupts (LVT)
//! - Sending End-of-Interrupt (EOI) to acknowledge interrupt handling is done
//! - Inter-Processor Interrupts (IPI) for future SMP
//!
//! ## MMIO Layout
//!
//! LAPIC registers are memory-mapped, typically at `0xFEE00000`.
//! Each register is 32 bits, 16-byte aligned (offset = register_id * 0x10).
//!
//! Key registers:
//! - ID (0x020): LAPIC ID
//! - TPR (0x080): Task Priority (set to 0 to accept all interrupts)
//! - EOI (0x0B0): End of Interrupt (write 0 to acknowledge)
//! - SVR (0x0F0): Spurious Interrupt Vector Register
//! - LVT TIMER (0x320): Local Vector Table timer entry
//! - INIT COUNT (0x380): Timer initial count
//! - CURRENT COUNT (0x390): Timer current count
//! - DIVIDE CONFIG (0x3E0): Timer divide configuration

use core::ptr;

/// LAPIC base virtual address (mapped in the upper half so it survives
/// CR3 switches to user PML4s that lack the identity map).
const LAPIC_BASE: u64 = 0xFFFF_FFFF_FEE0_0000;

/// LAPIC register offsets (in bytes, each register is 16-byte aligned).
const LAPIC_ID: u32 = 0x020;
const LAPIC_TPR: u32 = 0x080;
const LAPIC_EOI: u32 = 0x0B0;
const LAPIC_SVR: u32 = 0x0F0;
const LAPIC_LVT_TIMER: u32 = 0x320;
const LAPIC_INIT_COUNT: u32 = 0x380;
const LAPIC_CURRENT_COUNT: u32 = 0x390;
const LAPIC_DIVIDE_CONFIG: u32 = 0x3E0;

/// Spurious interrupt vector number (common convention).
const SPURIOUS_VECTOR: u8 = 39;

/// LAPIC SVR bit 8: APIC Software Enable.
const SVR_ENABLE: u32 = 1 << 8;

/// LVT Timer mode bits.
const LVT_TIMER_PERIODIC: u32 = 1 << 17;

/// LVT Timer mask bit (bit 16). When set, the timer interrupt is masked.
const LVT_TIMER_MASK: u32 = 1 << 16;

/// LVT Timer vector for the APIC timer interrupt.
const TIMER_VECTOR: u8 = 32;

/// Read a 32-bit LAPIC register.
///
/// # Safety
/// The caller must ensure `offset` is a valid LAPIC register offset.
#[inline]
unsafe fn lapic_read(offset: u32) -> u32 {
    ptr::read_volatile((LAPIC_BASE + offset as u64) as *const u32)
}

/// Write a 32-bit value to a LAPIC register.
///
/// # Safety
/// The caller must ensure `offset` is a valid LAPIC register offset.
#[inline]
unsafe fn lapic_write(offset: u32, value: u32) {
    ptr::write_volatile((LAPIC_BASE + offset as u64) as *mut u32, value);
}

/// Initialize the Local APIC.
///
/// This function:
/// 1. Reads the LAPIC ID to verify the LAPIC is accessible
/// 2. Sets the Task Priority Register to 0 (accept all interrupts)
/// 3. Enables the LAPIC via the Spurious Interrupt Vector Register
/// 4. Configures the LAPIC timer in periodic mode
///
/// # Safety
/// Must be called once during kernel initialization, after page tables
/// are set up and the LAPIC MMIO region is identity-mapped.
pub fn init() {
    unsafe {
        // Read LAPIC ID to verify MMIO access works
        let id = lapic_read(LAPIC_ID);
        crate::serial::write_str("[LAPIC] ID: 0x");
        crate::serial::write_hex(id as u64);
        crate::serial::write_nl();

        // Set Task Priority Register to 0 — accept all interrupts
        // TPR bits 0-7 = priority threshold; 0 means accept everything
        lapic_write(LAPIC_TPR, 0);

        // Enable the LAPIC via the Spurious Interrupt Vector Register
        // SVR bit 8 = APIC Software Enable
        // SVR bits 0-7 = spurious interrupt vector number
        lapic_write(LAPIC_SVR, SVR_ENABLE | (SPURIOUS_VECTOR as u32));
        crate::serial::write_str("[LAPIC] Enabled, spurious vector: ");
        crate::serial::write_u64(SPURIOUS_VECTOR as u64);
        crate::serial::write_nl();

        // Configure the LAPIC timer for periodic interrupts
        // Divide configuration: divide by 16 (bits 0-2 = 011, bit 3 = 0)
        lapic_write(LAPIC_DIVIDE_CONFIG, 0x03);

        // Set the timer vector and mode
        // Bits 0-7: vector number (32 = timer)
        // Bit 17: periodic mode (1 = periodic, 0 = one-shot)
        lapic_write(LAPIC_LVT_TIMER, (TIMER_VECTOR as u32) | LVT_TIMER_PERIODIC);

        // Set initial count: determines the tick rate
        // With divide-by-16, base clock = 1,193,182 / 16 ≈ 74,574 Hz
        // For ~100 Hz: count = 74,574 / 100 ≈ 746
        lapic_write(LAPIC_INIT_COUNT, 746);

        crate::serial::write_str("[LAPIC] Timer configured: ~100 Hz periodic, vector ");
        crate::serial::write_u64(TIMER_VECTOR as u64);
        crate::serial::write_nl();
    }
}

/// Send End-of-Interrupt (EOI) to the LAPIC.
///
/// Must be called at the end of every hardware interrupt handler.
/// Failure to send EOI will prevent the LAPIC from delivering further interrupts.
///
/// # Safety
/// Must be called from an interrupt context.
#[inline]
pub unsafe fn send_eoi() {
    lapic_write(LAPIC_EOI, 0);
}

/// Mask (disable) the LAPIC timer.
///
/// Sets the mask bit (bit 16) in the LVT Timer register.
/// This prevents the LAPIC timer from delivering interrupts,
/// independent of the IO-APIC mask.
///
/// # Safety
/// Must be called after LAPIC init.
pub unsafe fn mask_lapic_timer() {
    let current = lapic_read(LAPIC_LVT_TIMER);
    lapic_write(LAPIC_LVT_TIMER, current | LVT_TIMER_MASK);
    crate::serial::write_str_nl("[LAPIC] Timer masked");
}
