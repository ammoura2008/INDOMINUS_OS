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
/// 4. Routes IRQs using ACPI MADT interrupt source overrides
///
/// # Safety
/// Must be called once during kernel initialization, after page tables
/// are set up, MMIO regions are accessible, and ACPI has been parsed.
pub fn init(lapic_phys: u64, ioapic_phys: u64, ioapic_gsi_base: u32) {
    crate::serial::write_str_nl("[INT] Initializing interrupt subsystem...");

    // Step 1: Initialize LAPIC
    lapic::init(lapic_phys);

    // Step 2: Initialize IO-APIC
    ioapic::init(ioapic_phys);

    // Step 3: Initialize PIT
    pit::init();

    // Step 4: Route hardware IRQs using MADT overrides
    // Default: ISA IRQs map 1:1 to GSI (GSI base = 0)
    // With overrides, ISA IRQ source N may map to a different GSI number.
    // The IOAPIC redirection table is indexed by GSI - ioapic_gsi_base.
    //
    // Default mapping (no MADT overrides):
    //   ISA IRQ0 (PIT timer)  -> GSI 0  -> IOAPIC entry 0  -> vector 32
    //   ISA IRQ1 (keyboard)   -> GSI 1  -> IOAPIC entry 1  -> vector 33
    //
    // With typical Q35 MADT overrides:
    //   IRQ2 (ISA) -> GSI 0 (PIC ExtINT) — we skip this
    //   IRQ0 (ISA) -> GSI 2 (PIT timer)
    //   IRQ1 (ISA) -> GSI 1 (keyboard)

    let mut gsi_for_irq = [0u32; 16]; // ISA IRQ -> GSI mapping
    for i in 0..16 {
        gsi_for_irq[i] = i as u32; // Default 1:1
    }

    // Apply MADT interrupt source overrides
    if let Some(madt) = crate::acpi::madt_info() {
        for irq_override in &madt.overrides {
            if irq_override.bus == 0 && (irq_override.source as usize) < 16 {
                crate::serial::write_str("[INT] IRQ override: ISA IRQ ");
                crate::serial::write_hex(irq_override.source as u64);
                crate::serial::write_str(" -> GSI ");
                crate::serial::write_hex(irq_override.global as u64);
                crate::serial::write_nl();
                gsi_for_irq[irq_override.source as usize] = irq_override.global;
            }
        }
    }

    unsafe {
        // Route PIT timer: find which GSI IRQ0 maps to
        let pit_gsi = gsi_for_irq[0];
        let pit_entry = pit_gsi.wrapping_sub(ioapic_gsi_base);
        crate::serial::write_str("[INT] PIT IRQ0 -> GSI ");
        crate::serial::write_hex(pit_gsi as u64);
        crate::serial::write_str(" -> IOAPIC entry ");
        crate::serial::write_hex(pit_entry as u64);
        crate::serial::write_nl();
        ioapic::set_irq(pit_entry as u16, 32, 0);
        ioapic::unmask_irq(pit_entry as u16);

        // Route keyboard: find which GSI IRQ1 maps to
        let kbd_gsi = gsi_for_irq[1];
        let kbd_entry = kbd_gsi.wrapping_sub(ioapic_gsi_base);
        crate::serial::write_str("[INT] KBD IRQ1 -> GSI ");
        crate::serial::write_hex(kbd_gsi as u64);
        crate::serial::write_str(" -> IOAPIC entry ");
        crate::serial::write_hex(kbd_entry as u64);
        crate::serial::write_nl();
        ioapic::set_irq(kbd_entry as u16, 33, 0);
        ioapic::unmask_irq(kbd_entry as u16);

        // Mask all other IOAPIC entries (already masked from ioapic::init)
    }

    crate::serial::write_str_nl("[INT] Interrupt subsystem initialized");
}
