/// RSDP (Root System Description Pointer) detection
///
/// The RSDP is located by scanning:
/// 1. The BIOS EBDA (Extended BIOS Data Area) — address from 0x40E
/// 2. Physical memory range 0x000E0000..0x000FFFFF (BIOS read-only area)
/// 3. Physical memory range 0x00080000..0x0009FFFF (typically EBDA region)
/// 4. Physical memory range 0x000A0000..0x000BFFFF (legacy video/ROM area)
///
/// QEMU/OVMF (UEFI) places RSDP in the standard BIOS area (0xE0000..0xFFFFF).
/// Some firmware may place it elsewhere, so we scan aggressively.

/// RSDP structure (first 36 bytes for ACPI 1.0, extended to 66 bytes for ACPI 2.0)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Rsdp {
    signature: [u8; 8],     // "RSD PTR "
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,           // 0 = ACPI 1.0, 2 = ACPI 2.0+
    rsdt_addr: u32,         // Physical address of RSDT (ACPI 1.0)
    // Extended fields (ACPI 2.0+)
    length: u32,            // Total length of RSDP
    xsdt_addr: u64,         // Physical address of XSDT
    extended_checksum: u8,
    reserved: [u8; 3],
}

/// Find the RSDP by scanning memory
pub fn find_rsdp() -> Option<u64> {
    // Method 1: Scan the EBDA (address from BIOS data area at 0x40E)
    let ebda = get_ebda();
    if ebda >= 0x8000 && ebda < 0xA0000 {
        let ebda_end = core::cmp::min(ebda + 0x8000, 0xA0000);
        if let Some(addr) = scan_range(ebda, ebda_end) {
            return Some(addr);
        }
    }

    // Method 2: Scan BIOS read-only memory area (0xE0000..0xFFFFF)
    // This is where SeaBIOS/QEMU typically places the RSDP
    if let Some(addr) = scan_range(0xE0000, 0x100000) {
        return Some(addr);
    }

    // Method 3: Scan extended EBDA range (0x80000..0x9FFFF)
    if let Some(addr) = scan_range(0x80000, 0xA0000) {
        return Some(addr);
    }

    // Method 4: Scan lower BIOS area (0xA0000..0xBFFFF)
    if let Some(addr) = scan_range(0xA0000, 0xC0000) {
        return Some(addr);
    }

    // Method 5: Scan all of low memory (0x1000..0x80000)
    // OVMF/UEFI may place RSDP anywhere in low memory
    if let Some(addr) = scan_range(0x1000, 0x80000) {
        return Some(addr);
    }

    // Method 6: Scan upper BIOS area (0xC0000..0xE0000)
    // Video BIOS and other option ROMs live here, RSDP might be nearby
    if let Some(addr) = scan_range(0xC0000, 0xE0000) {
        return Some(addr);
    }

    None
}

/// Parse the RSDP to get XSDT and RSDT addresses
///
/// # Safety
/// Identity map must be active (virt == phys for low memory).
pub fn parse_rsdp(rsdp_phys: u64) -> (u64, u64) {
    // Use identity-mapped address (identity map still active during ACPI init)
    let rsdp = unsafe { &*(rsdp_phys as *const Rsdp) };
    let xsdt = if rsdp.revision >= 2 { rsdp.xsdt_addr } else { 0 };
    let rsdt = rsdp.rsdt_addr as u64;
    (xsdt, rsdt)
}

/// Get the EBDA address from the BIOS data area at 0x40E
fn get_ebda() -> u64 {
    // The EBDA segment is stored at physical address 0x40E (2 bytes, paragraph number)
    let ebda_segment = unsafe { *(0x40E as *const u16) } as u64;
    ebda_segment * 16 // Convert paragraphs to bytes
}

/// Scan a physical memory range for the RSDP signature
fn scan_range(start: u64, end: u64) -> Option<u64> {
    // RSDP must be on an 8-byte boundary
    let mut addr = start & !7;
    while addr + 8 <= end {
        let sig = unsafe { core::ptr::read(addr as *const u64) };
        if sig == 0x2052545020445352 { // "RSD PTR " in little-endian
            if validate_rsdp(addr) {
                return Some(addr);
            }
        }
        addr += 8;
    }
    None
}

/// Validate an RSDP by checking its checksum
fn validate_rsdp(addr: u64) -> bool {
    let rsdp = unsafe { &*(addr as *const Rsdp) };

    // First 20 bytes must sum to 0 (ACPI 1.0 checksum)
    let data = unsafe { core::slice::from_raw_parts(addr as *const u8, 20) };
    let sum1: u8 = data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if sum1 != 0 {
        return false;
    }

    // If ACPI 2.0+, validate extended checksum (first 36 bytes)
    if rsdp.revision >= 2 {
        let data = unsafe { core::slice::from_raw_parts(addr as *const u8, 36) };
        let sum2: u8 = data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        if sum2 != 0 {
            return false;
        }
    }

    // Validate signature characters
    for &b in &rsdp.signature {
        if !b.is_ascii_uppercase() && b != b' ' {
            return false;
        }
    }

    true
}
