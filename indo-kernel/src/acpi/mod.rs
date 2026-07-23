pub mod rsdp;
pub mod madt;

extern crate alloc;
use alloc::vec::Vec;

/// ACPI table header (common to all tables)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AcpiTableHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

impl AcpiTableHeader {
    pub fn signature_str(&self) -> &str {
        core::str::from_utf8(&self.signature).unwrap_or("????")
    }

    pub fn oem_id_str(&self) -> &str {
        core::str::from_utf8(&self.oem_id).unwrap_or("??????")
    }
}

/// ACPI table reference (physical address + header)
pub struct AcpiTable {
    pub phys_addr: u64,
    pub header: AcpiTableHeader,
}

/// Global ACPI state
pub struct AcpiState {
    pub tables: Vec<AcpiTable>,
    pub rsdp_phys: u64,
    pub madt: Option<madt::MadtInfo>,
}

impl AcpiState {
    pub const fn new() -> Self {
        AcpiState {
            tables: Vec::new(),
            rsdp_phys: 0,
            madt: None,
        }
    }
}

static ACPI_STATE: crate::sync_cell::SyncUnsafeCell<Option<AcpiState>> = crate::sync_cell::SyncUnsafeCell::new(None);

/// Initialize ACPI with RSDP address from bootloader or memory scan
pub fn init(bootloader_rsdp: Option<u64>) {
    crate::serial::write_str("[ACPI] Searching for RSDP...\n");

    // Prefer RSDP from bootloader (UEFI config tables), fallback to memory scan
    let rsdp = if let Some(addr) = bootloader_rsdp {
        if addr != 0 {
            crate::serial::write_str("[ACPI] Using RSDP from bootloader\n");
            Some(addr)
        } else {
            None
        }
    } else {
        None
    }.or_else(|| rsdp::find_rsdp());

    let rsdp = match rsdp {
        Some(addr) => addr,
        None => {
            crate::serial::write_str("[ACPI] RSDP not found\n");
            return;
        }
    };

    crate::serial::write_str("[ACPI] RSDP at ");
    crate::serial::write_hex(rsdp);
    crate::serial::write_nl();

    // Parse RSDP to get XSDT/RSDT address
    let (xsdt_addr, rsdt_addr) = rsdp::parse_rsdp(rsdp);

    crate::serial::write_str("[ACPI] XSDT=");
    crate::serial::write_hex(xsdt_addr);
    crate::serial::write_str(" RSDT=");
    crate::serial::write_hex(rsdt_addr);
    crate::serial::write_nl();

    let mut state = AcpiState::new();
    state.rsdp_phys = rsdp;

    // Parse the XSDT (or RSDT if XSDT not available)
    let table_addrs = if xsdt_addr != 0 {
        parse_xsdt(xsdt_addr)
    } else if rsdt_addr != 0 {
        parse_rsdt(rsdt_addr)
    } else {
        Vec::new()
    };

    crate::serial::write_str("[ACPI] Found ");
    crate::serial::write_hex(table_addrs.len() as u64);
    crate::serial::write_str(" tables\n");

    // Read and validate each table
    for &addr in &table_addrs {
        if let Some(table) = read_table(addr) {
            crate::serial::write_str("[ACPI]   ");
            crate::serial::write_str(table.header.signature_str());
            crate::serial::write_str(" (rev=");
            crate::serial::write_hex(table.header.revision as u64);
            crate::serial::write_str(" len=");
            crate::serial::write_hex(table.header.length as u64);
            crate::serial::write_nl();
            state.tables.push(table);
        }
    }

    // Parse MADT if present
    if let Some(madt_table) = state.tables.iter().find(|t| t.header.signature == *b"MADT") {
        let madt_info = madt::parse_madt(madt_table.phys_addr);
        crate::serial::write_str("[ACPI] MADT: local_apic=");
        crate::serial::write_hex(madt_info.local_apic_addr);
        crate::serial::write_str(" processors=");
        crate::serial::write_hex(madt_info.processors.len() as u64);
        crate::serial::write_nl();
        state.madt = Some(madt_info);
    }

    unsafe {
        *ACPI_STATE.get() = Some(state);
    }
}

/// Get the ACPI state
pub fn acpi_state() -> &'static AcpiState {
    unsafe { (*ACPI_STATE.get()).as_ref().expect("ACPI not initialized") }
}

/// Get MADT info (CPU topology, APIC addresses)
pub fn madt_info() -> Option<&'static madt::MadtInfo> {
    unsafe { (*ACPI_STATE.get()).as_ref()?.madt.as_ref() }
}

/// Parse XSDT (64-bit table pointers)
///
/// # Safety
/// Identity map must be active (virt == phys for low memory).
fn parse_xsdt(xsdt_phys: u64) -> Vec<u64> {
    // Use identity-mapped address (identity map still active during ACPI init)
    let header = unsafe { &*(xsdt_phys as *const AcpiTableHeader) };
    let total_len = header.length as usize;
    if total_len < 44 { return Vec::new(); } // 36 header + at least 1 entry (8 bytes)
    let num_entries = (total_len - 36) / 8;
    // XSDT entries start at byte 36 (after the 36-byte header)
    let entries_start = xsdt_phys + 36;

    let mut addrs = Vec::new();
    for i in 0..num_entries {
        let entry_ptr = (entries_start + (i as u64) * 8) as *const u64;
        let addr = unsafe { *entry_ptr };
        if addr != 0 {
            addrs.push(addr);
        }
    }
    addrs
}

/// Parse RSDT (32-bit table pointers)
///
/// # Safety
/// Identity map must be active (virt == phys for low memory).
fn parse_rsdt(rsdt_phys: u64) -> Vec<u64> {
    // Use identity-mapped address (identity map still active during ACPI init)
    let header = unsafe { &*(rsdt_phys as *const AcpiTableHeader) };
    let total_len = header.length as usize;
    if total_len < 40 { return Vec::new(); } // 36 header + at least 1 entry (4 bytes)
    let num_entries = (total_len - 36) / 4;
    // RSDT entries start at byte 36 (after the 36-byte header)
    let entries_start = rsdt_phys + 36;

    let mut addrs = Vec::new();
    for i in 0..num_entries {
        let entry_ptr = (entries_start + (i as u64) * 4) as *const u32;
        let addr = unsafe { *entry_ptr } as u64;
        if addr != 0 {
            addrs.push(addr);
        }
    }
    addrs
}

/// Read and validate an ACPI table
///
/// # Safety
/// Identity map must be active (virt == phys for low memory).
fn read_table(phys_addr: u64) -> Option<AcpiTable> {
    // Use identity-mapped address (identity map still active during ACPI init)
    let header = unsafe { core::ptr::read(phys_addr as *const AcpiTableHeader) };

    // Validate signature (4 ASCII uppercase letters)
    for &b in &header.signature {
        if !b.is_ascii_uppercase() && !b.is_ascii_digit() && b != b' ' {
            return None;
        }
    }

    // Validate length (must be at least 36 bytes for header)
    if (header.length as usize) < 36 {
        return None;
    }

    // Validate checksum
    let table_data = unsafe {
        core::slice::from_raw_parts(phys_addr as *const u8, header.length as usize)
    };
    let sum: u8 = table_data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if sum != 0 {
        return None;
    }

    Some(AcpiTable { phys_addr, header })
}
