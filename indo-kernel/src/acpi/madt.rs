/// MADT (Multiple APIC Description Table) parser
///
/// The MADT describes the interrupt controller topology:
/// - Local APIC addresses
/// - I/O APIC addresses
/// - Interrupt source overrides
/// - NMI sources
/// - Processor local APIC entries (which CPUs are present)

extern crate alloc;
use alloc::vec::Vec;

/// Information extracted from the MADT
pub struct MadtInfo {
    /// Physical address of the Local APIC registers
    pub local_apic_addr: u64,
    /// Physical address of the I/O APIC registers (if present)
    pub io_apic_addr: u64,
    /// I/O APIC global interrupt base
    pub io_apic_gsi_base: u32,
    /// List of active processor IDs
    pub processors: Vec<u8>,
    /// Interrupt source overrides
    pub overrides: Vec<InterruptOverride>,
    /// NMIs
    pub nmis: Vec<NmiSource>,
}

/// Interrupt source override
pub struct InterruptOverride {
    pub bus: u8,         // 0 = ISA
    pub source: u8,      // Bus-relative interrupt number
    pub global: u32,     // Global system interrupt number
    pub polarity: u8,    // 0=conforms, 1=active-high, 3=active-low
    pub trigger: u8,     // 0=conforms, 1=edge, 3=level
}

/// NMI source
pub struct NmiSource {
    pub processor: u8,   // 0xFF = all processors
    pub pin: u8,         // LINT pin (0 or 1)
    pub polarity: u8,
    pub trigger: u8,
}

/// MADT record types
const LOCAL_APIC: u8 = 0;
const IO_APIC: u8 = 1;
const INTERRUPT_OVERRIDE: u8 = 2;
const NMI: u8 = 3;
const LOCAL_APIC_NMI: u8 = 4;
const LOCAL_APIC_ADDRESS_OVERRIDE: u8 = 5;

/// Parse the MADT table
///
/// # Safety
/// Identity map must be active (virt == phys for low memory).
pub fn parse_madt(madt_phys: u64) -> MadtInfo {
    // Use identity-mapped address (identity map still active during ACPI init)
    let madt_virt = madt_phys;

    // MADT header is 44 bytes (36 for standard header + 4 for local_apic_addr + 4 for flags)
    let local_apic_addr = unsafe {
        let ptr = madt_virt as *const u8;
        let addr = core::ptr::read_unaligned(ptr.add(36) as *const u32) as u64;
        if addr == 0 { 0xFEE00000 } else { addr } // Default: 0xFEE00000
    };

    let mut info = MadtInfo {
        local_apic_addr,
        io_apic_addr: 0,
        io_apic_gsi_base: 0,
        processors: Vec::new(),
        overrides: Vec::new(),
        nmis: Vec::new(),
    };

    // Parse records (start after 44-byte header)
    let table_len = unsafe { core::ptr::read(madt_virt as *const u32) } as usize;
    let mut offset = 44usize;

    while offset + 2 <= table_len {
        let record_type = unsafe { *(madt_virt as *const u8).add(offset) };
        let record_len = unsafe { *(madt_virt as *const u8).add(offset + 1) } as usize;

        if record_len < 2 || offset + record_len > table_len {
            break;
        }

        let record_data = unsafe {
            core::slice::from_raw_parts((madt_virt as *const u8).add(offset + 2), record_len - 2)
        };

        match record_type {
            LOCAL_APIC => {
                if record_len >= 4 {
                    let _acpi_id = record_data[0];
                    let apic_id = record_data[1];
                    let flags = unsafe { core::ptr::read_unaligned(record_data[2..].as_ptr() as *const u32) };
                    if flags & 1 != 0 { // Enabled bit
                        info.processors.push(apic_id);
                    }
                    crate::serial::write_str("[ACPI]   LAPIC: id=");
                    crate::serial::write_hex(apic_id as u64);
                    crate::serial::write_str(" enabled=");
                    crate::serial::write_hex((flags & 1) as u64);
                    crate::serial::write_nl();
                }
            }
            IO_APIC => {
                if record_len >= 6 {
                    let ioapic_id = record_data[0];
                    info.io_apic_addr = unsafe { core::ptr::read_unaligned(record_data[2..].as_ptr() as *const u32) } as u64;
                    info.io_apic_gsi_base = unsafe { core::ptr::read_unaligned(record_data[4..].as_ptr() as *const u32) };
                    crate::serial::write_str("[ACPI]   IOAPIC: id=");
                    crate::serial::write_hex(ioapic_id as u64);
                    crate::serial::write_str(" addr=");
                    crate::serial::write_hex(info.io_apic_addr);
                    crate::serial::write_str(" gsi_base=");
                    crate::serial::write_hex(info.io_apic_gsi_base as u64);
                    crate::serial::write_nl();
                }
            }
            INTERRUPT_OVERRIDE => {
                if record_len >= 8 {
                    let bus = record_data[0];
                    let source = record_data[1];
                    let global = unsafe { core::ptr::read_unaligned(record_data[2..].as_ptr() as *const u32) };
                    let flags = unsafe { core::ptr::read_unaligned(record_data[4..].as_ptr() as *const u32) };
                    info.overrides.push(InterruptOverride {
                        bus,
                        source,
                        global,
                        polarity: (flags & 0x3) as u8,
                        trigger: ((flags >> 2) & 0x3) as u8,
                    });
                    crate::serial::write_str("[ACPI]   Override: bus=");
                    crate::serial::write_hex(bus as u64);
                    crate::serial::write_str(" source=");
                    crate::serial::write_hex(source as u64);
                    crate::serial::write_str(" global=");
                    crate::serial::write_hex(global as u64);
                    crate::serial::write_nl();
                }
            }
            NMI | LOCAL_APIC_NMI => {
                if record_len >= 4 {
                    let processor = record_data[0];
                    let pin = if record_type == LOCAL_APIC_NMI { record_data[1] } else { 0xFF };
                    info.nmis.push(NmiSource {
                        processor,
                        pin,
                        polarity: 0,
                        trigger: 0,
                    });
                }
            }
            LOCAL_APIC_ADDRESS_OVERRIDE => {
                if record_len >= 6 {
                    let addr = unsafe { core::ptr::read_unaligned(record_data[0..].as_ptr() as *const u64) };
                    info.local_apic_addr = addr;
                    crate::serial::write_str("[ACPI]   LAPIC addr override: ");
                    crate::serial::write_hex(addr);
                    crate::serial::write_nl();
                }
            }
            _ => {}
        }

        offset += record_len;
    }

    info
}
