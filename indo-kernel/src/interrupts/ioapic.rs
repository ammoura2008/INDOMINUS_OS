//! # I/O APIC (IO-APIC)
//!
//! The IO-APIC is a separate chip (or integrated into the chipset) that
//! receives hardware IRQs from devices and routes them to the LAPIC.
//!
//! ## How it works
//!
//! 1. A device (keyboard, timer, disk) asserts an IRQ line
//! 2. The IO-APIC receives the IRQ
//! 3. The IO-APIC looks up the IRQ in its redirection table
//! 4. The IO-APIC sends an interrupt to the LAPIC with the configured vector
//! 5. The LAPIC delivers the interrupt to the CPU
//!
//! ## MMIO Layout
//!
//! The IO-APIC is memory-mapped at `0xFEC00000` (standard x86).
//! Registers are accessed via an I/O Register Select (IOREGSEL) and
//! a data window (IOWIN).
//!
//! Key registers:
//! - ID (0x00): IO-APIC ID
//! - VER (0x01): IO-APIC version (max redirection entries)
//! - REDTBL (0x10 + 2*n): Redirection table entry for IRQ n (low dword)
//! - REDTBL+1 (0x11 + 2*n): Redirection table entry for IRQ n (high dword)

use crate::mmio::MmioRegion;
use crate::sync_cell::SyncUnsafeCell;

/// IO-APIC MMIO region (mapped via mmio framework)
static IOAPIC: SyncUnsafeCell<Option<MmioRegion>> = SyncUnsafeCell::new(None);

/// IO-APIC register select port (offset from base).
const IOREGSEL: u32 = 0x00;

/// IO-APIC data window port (offset from base).
const IOWIN: u32 = 0x10;

/// IO-APIC register IDs.
const IOAPICID: u32 = 0x00;
const IOAPICVER: u32 = 0x01;

/// Base offset for redirection table entries.
/// IRQ n uses registers (0x10 + 2*n) and (0x11 + 2*n).
const REDTBL_BASE: u32 = 0x10;

/// Read a 32-bit IO-APIC register.
///
/// # Safety
/// The caller must ensure `reg` is a valid IO-APIC register ID.
#[inline]
unsafe fn ioapic_read(reg: u32) -> u32 {
    match (*IOAPIC.get()).as_ref() {
        Some(ioapic) => {
            ioapic.write_reg(IOREGSEL, reg);
            ioapic.read_reg(IOWIN)
        }
        None => {
            crate::serial::write_str("[IOAPIC] ERROR: read before init\n");
            0
        }
    }
}

/// Write a 32-bit value to an IO-APIC register.
///
/// # Safety
/// The caller must ensure `reg` is a valid IO-APIC register ID.
#[inline]
unsafe fn ioapic_write(reg: u32, value: u32) {
    if let Some(ioapic) = (*IOAPIC.get()).as_ref() {
        ioapic.write_reg(IOREGSEL, reg);
        ioapic.write_reg(IOWIN, value);
    } else {
        crate::serial::write_str("[IOAPIC] ERROR: write before init\n");
    }
}

/// Initialize the I/O APIC.
///
/// This function:
/// 1. Maps the IO-APIC MMIO region
/// 2. Reads the IO-APIC ID and version to verify MMIO access
/// 3. Masks all redirection entries (disables all IRQs)
/// 4. Reports the number of redirection entries available
///
/// # Safety
/// Must be called once during kernel initialization, after page tables
/// are set up and ACPI has been parsed.
pub fn init(ioapic_phys: u64) {
    unsafe {
        *IOAPIC.get() = Some(MmioRegion::new(ioapic_phys));
        crate::serial::write_str("[IOAPIC] Mapped at phys=");
        crate::serial::write_hex(ioapic_phys);
        crate::serial::write_nl();

        let id = ioapic_read(IOAPICID);
        let version = ioapic_read(IOAPICVER);

        crate::serial::write_str("[IOAPIC] ID: 0x");
        crate::serial::write_hex(id as u64);
        crate::serial::write_nl();

        let max_entries = ((version >> 16) & 0xFF) as u16;
        let version_lower = (version & 0xFF) as u8;
        crate::serial::write_str("[IOAPIC] Version: ");
        crate::serial::write_u64(version_lower as u64);
        crate::serial::write_str(", max redirection entries: ");
        crate::serial::write_u64(max_entries as u64 + 1);
        crate::serial::write_nl();

        // Mask all redirection entries
        // For each IRQ, set the mask bit (bit 16 of the low dword)
        for i in 0..=max_entries {
            mask_irq(i);
        }

        crate::serial::write_str("[IOAPIC] All IRQs masked\n");
    }
}

/// Set the redirection table entry for a hardware IRQ.
///
/// This configures the IO-APIC to route the given hardware IRQ to the
/// specified LAPIC vector.
///
/// # Arguments
/// * `irq` - The hardware IRQ number (0-23 for standard AT IRQs)
/// * `vector` - The LAPIC vector number to deliver (32-255)
/// * `destination_apic_id` - The LAPIC ID of the target CPU (0 for BSP)
///
/// # Safety
/// The caller must ensure `irq` is within range and the IO-APIC is initialized.
pub unsafe fn set_irq(irq: u16, vector: u8, destination_apic_id: u8) {
    let low_dword = REDTBL_BASE + (irq as u32) * 2;
    let high_dword = low_dword + 1;

    // Low dword:
    // - Bits 0-7: vector number
    // - Bits 8-10: delivery mode (000 = fixed)
    // - Bit 11: destination mode (0 = physical)
    // - Bit 13: polarity (0 = active high)
    // - Bit 14: trigger mode (0 = edge triggered)
    // - Bit 16: mask (0 = not masked)
    let low = vector as u32;

    // High dword:
    // - Bits 24-27: destination APIC ID (for physical delivery mode)
    let high = (destination_apic_id as u32) << 24;

    ioapic_write(low_dword, low);
    ioapic_write(high_dword, high);

    crate::serial::write_str("[IOAPIC] IRQ ");
    crate::serial::write_u64(irq as u64);
    crate::serial::write_str(" -> vector ");
    crate::serial::write_u64(vector as u64);
    crate::serial::write_nl();
}

/// Mask (disable) a hardware IRQ in the IO-APIC.
///
/// Sets the mask bit (bit 16) in the redirection table entry.
///
/// # Safety
/// The caller must ensure `irq` is within range.
pub unsafe fn mask_irq(irq: u16) {
    let low_dword = REDTBL_BASE + (irq as u32) * 2;
    let current = ioapic_read(low_dword);
    ioapic_write(low_dword, current | (1 << 16)); // Set mask bit
}

/// Unmask (enable) a hardware IRQ in the IO-APIC.
///
/// Clears the mask bit (bit 16) in the redirection table entry.
///
/// # Safety
/// The caller must ensure `irq` is within range and the redirection
/// table entry has been properly configured via `set_irq`.
pub unsafe fn unmask_irq(irq: u16) {
    let low_dword = REDTBL_BASE + (irq as u32) * 2;
    let current = ioapic_read(low_dword);
    ioapic_write(low_dword, current & !(1 << 16)); // Clear mask bit
}
