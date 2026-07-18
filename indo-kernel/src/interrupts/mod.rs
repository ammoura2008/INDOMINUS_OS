//! # Interrupts Module
//!
//! This module manages hardware and software interrupts on x86_64.
//!
//! ## Interrupt Flow
//!
//! ```text
//! CPU Exception (0-31)          Hardware IRQ (32-47)
//!        │                            │
//!        ▼                            ▼
//!   IDT[n] handler              IDT[n] handler
//!        │                            │
//!        ▼                            ▼
//!   Exception-specific         dispatch::dispatch(vector)
//!   handler code                      │
//!                                     ▼
//!                              registered IrqHandler
//!                                     │
//!                                     ▼
//!                              LAPIC send_eoi()
//! ```
//!
//! ## Components
//!
//! - **LAPIC**: Local APIC for per-CPU interrupt delivery
//! - **IO-APIC**: I/O APIC for routing hardware IRQs to the LAPIC
//! - **PIT**: Programmable Interval Timer for periodic ticks
//! - **Dispatch**: IRQ handler registration and dispatch

pub mod dispatch;
pub mod ioapic;
pub mod lapic;
pub mod pit;

/// Initialize the entire interrupt subsystem.
///
/// This function:
/// 1. Initializes the LAPIC (local interrupt controller)
/// 2. Initializes the IO-APIC (hardware IRQ routing)
/// 3. Configures the PIT (periodic timer)
/// 4. Routes IRQ0 to vector 32 (timer) and unmasks it
///
/// # Safety
/// Must be called once during kernel initialization, after page tables
/// are set up and the MMIO regions are accessible.
pub fn init() {
    crate::serial::write_str_nl("[INT] Initializing interrupt subsystem...");

    // Step 1: Initialize LAPIC
    lapic::init();

    // Step 2: Initialize IO-APIC
    ioapic::init();

    // Step 3: Initialize PIT
    pit::init();

    // Step 4: Route hardware IRQs to LAPIC vectors
    unsafe {
        // IRQ0 (PIT timer) -> vector 32
        ioapic::set_irq(0, 32, 0);
        ioapic::unmask_irq(0);

        // IRQ1 (PS/2 keyboard) -> vector 33
        ioapic::set_irq(1, 33, 0);
        ioapic::unmask_irq(1);

        // Mask all other IRQs (they're already masked from ioapic::init)
    }

    crate::serial::write_str_nl("[INT] Interrupt subsystem initialized");
}
