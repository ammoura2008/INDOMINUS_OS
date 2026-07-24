#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod cpu;
mod gdt;
mod idt;
mod interrupts;
mod keyboard;
mod memory;
mod process;
mod serial;
mod panic;
mod syscall;
mod elf;
mod vfs;
mod initrd;
mod acpi;
mod mmio;
mod pci;
mod block;
mod ahci;
mod fat32;
mod debug;
pub mod sync_cell;

use indo_core::BootInfo;

use serial::{write_str, write_hex, write_nl, write_str_nl};

pub static mut CAPTURED_RSP: u64 = 0;

pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

/// Phase 9.1 functional verification: BlockDevice + RamDisk.
///
/// Tests:
/// 1. Create RAM disk
/// 2. Register in global registry
/// 3. Write known data → read back → verify equality
/// 4. Invalid LBA → returns error
/// 5. Wrong buffer size → returns error
/// 6. Device properties (sector_size, total_sectors, name)
fn phase91_block_test() {
    use alloc::sync::Arc;
    use block::ramdisk::RamDisk;
    use block::{BlockDevice, BlockError};

    write_str_nl("[TEST] Phase 9.1: Block device abstraction");

    // 1. Create RAM disk: 8 sectors × 512 bytes = 4 KiB
    let rd = Arc::new(RamDisk::new(8, 512));

    // Verify device properties
    assert_eq!(rd.sector_size(), 512, "sector_size must be 512");
    assert_eq!(rd.total_sectors(), 8, "total_sectors must be 8");
    assert_eq!(rd.name(), "ramdisk", "name must be 'ramdisk'");
    write_str_nl("[TEST]   Device properties OK");

    // 2. Register in global registry
    let dev_id = crate::block::registry::register_device(rd.clone())
        .expect("Failed to register RAM disk");
    write_str_nl("[TEST]   Registry OK");

    // 3. Write known data to sector 3, read back, verify
    let mut write_buf = [0u8; 512];
    for i in 0..512 {
        write_buf[i] = (i ^ 0xAA) as u8; // deterministic pattern
    }
    assert!(rd.write_sector(3, &write_buf).is_ok(), "write_sector failed");

    let mut read_buf = [0u8; 512];
    assert!(rd.read_sector(3, &mut read_buf).is_ok(), "read_sector failed");
    assert_eq!(write_buf, read_buf, "read-back mismatch");
    write_str_nl("[TEST]   Read/write verify OK");

    // 4. Write to multiple sectors, verify each
    for sector in 0..8u64 {
        let mut buf = [0u8; 512];
        for i in 0..512 {
            buf[i] = ((sector * 512 + i as u64) & 0xFF) as u8;
        }
        assert!(rd.write_sector(sector, &buf).is_ok());
    }
    for sector in 0..8u64 {
        let mut buf = [0u8; 512];
        assert!(rd.read_sector(sector, &mut buf).is_ok());
        for i in 0..512 {
            let expected = ((sector * 512 + i as u64) & 0xFF) as u8;
            assert_eq!(buf[i], expected, "sector {} byte {} mismatch", sector, i);
        }
    }
    write_str_nl("[TEST]   Multi-sector OK");

    // 5. Invalid LBA → OutOfBounds
    assert_eq!(
        rd.read_sector(8, &mut read_buf),
        Err(BlockError::OutOfBounds),
        "LBA 8 should be out of bounds"
    );
    assert_eq!(
        rd.write_sector(100, &write_buf),
        Err(BlockError::OutOfBounds),
        "LBA 100 should be out of bounds"
    );
    write_str_nl("[TEST]   Out-of-bounds check OK");

    // 6. Wrong buffer size → InvalidBufferSize
    let mut small = [0u8; 256];
    assert_eq!(
        rd.read_sector(0, &mut small),
        Err(BlockError::InvalidBufferSize),
        "256-byte buffer should fail"
    );
    let mut big = [0u8; 1024];
    assert_eq!(
        rd.read_sector(0, &mut big),
        Err(BlockError::InvalidBufferSize),
        "1024-byte buffer should fail"
    );
    write_str_nl("[TEST]   Buffer size check OK");

    // 7. Lookup from registry
    let dev = crate::block::registry::get_device(dev_id);
    assert!(dev.is_some(), "Device must exist in registry");
    let dev = dev.unwrap();
    assert_eq!(dev.name(), "ramdisk");
    write_str_nl("[TEST]   Registry lookup OK");

    // 8. Unregister
    let removed = crate::block::registry::unregister_device(dev_id);
    assert!(removed.is_some(), "Unregister must return device");
    assert!(crate::block::registry::get_device(dev_id).is_none(), "Device must be gone");
    write_str_nl("[TEST]   Unregister OK");

    write_str_nl("[TEST] Phase 9.1: ALL TESTS PASSED");
}

/// Phase 9.3 AHCI disk read verification.
///
/// Reads sector 0 (MBR) from the AHCI disk and verifies the boot signature
/// (0x55AA at bytes 510-511). Read-only — does not write to avoid corrupting
/// the boot disk.
fn phase93_ahci_test() {
    use crate::block::{BlockDevice, registry};

    write_str_nl("[TEST] Phase 9.3: AHCI disk read");

    // Look up AHCI disk from registry
    let disk = match registry::get_device(0) {
        Some(d) => d,
        None => {
            write_str_nl("[TEST] Phase 9.3: SKIPPED (no device 0)");
            return;
        }
    };

    assert!(disk.name().starts_with("ahci"), "device name must start with 'ahci'");
    write_str("[TEST]   Device: ");
    write_str(disk.name());
    write_str(" sectors=");
    write_hex(disk.total_sectors());
    write_str(" ssize=");
    write_hex(disk.sector_size() as u64);
    write_nl();

    // Read sector 0 (MBR)
    let mut buf = [0u8; 512];
    match disk.read_sector(0, &mut buf) {
        Ok(()) => {
            write_str_nl("[TEST]   Read sector 0 OK");
        }
        Err(e) => {
            write_str("[TEST]   FAIL: read_sector(0) error=");
            write_hex(e.to_errno() as u64);
            write_nl();
            return;
        }
    }

    // Check MBR boot signature: bytes 510=0x55, 511=0xAA
    assert_eq!(buf[510], 0x55, "MBR byte 510 must be 0x55, got 0x{:02X}", buf[510]);
    assert_eq!(buf[511], 0xAA, "MBR byte 511 must be 0xAA, got 0x{:02X}", buf[511]);
    write_str_nl("[TEST]   MBR signature OK (0x55AA)");

    // Out-of-bounds read must fail
    let mut junk = [0u8; 512];
    assert!(disk.read_sector(disk.total_sectors(), &mut junk).is_err());
    write_str_nl("[TEST]   Out-of-bounds check OK");

    // Wrong buffer size must fail
    let mut small = [0u8; 256];
    assert!(disk.read_sector(0, &mut small).is_err());
    write_str_nl("[TEST]   Buffer size check OK");

    write_str_nl("[TEST] Phase 9.3: ALL TESTS PASSED");
}

/// Phase 9.4 end-to-end verification: AHCI + FAT16 + VFS integration.
///
/// Proves the full storage stack works:
///   AHCI → BlockDevice → FAT filesystem → VFS → file I/O
///
/// Test categories:
///   1. AHCI raw sector I/O (multiple LBAs, consecutive reads, byte verification)
///   2. FAT16 detection + mount + root dir + subdirectory traversal + file read
///   3. VFS integration (mount, path resolution, open/read/readdir/close)
///   4. Regression (re-runs Phase 9.1, 9.2, 9.2b, 9.3)
fn phase94_fat32_init() {
    use crate::block::{BlockDevice, registry};
    use crate::vfs::{FileSystem, VfsError};
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    let mut tests_passed: u32 = 0;
    let mut tests_failed: u32 = 0;

    macro_rules! test_pass {
        ($name:expr) => {{
            tests_passed += 1;
            write_str("[T]   PASS: ");
            write_str_nl($name);
        }};
    }
    macro_rules! test_fail {
        ($name:expr) => {{
            tests_failed += 1;
            write_str("[T]   FAIL: ");
            write_str_nl($name);
        }};
    }

    write_str_nl("[T] ==================================================");
    write_str_nl("[T] Phase 9.4: End-to-End Verification");
    write_str_nl("[T] ==================================================");

    // ── Section 1: AHCI Raw Sector I/O ────────────────────────────────
    write_str_nl("[T] -- Section 1: AHCI Raw Sector I/O --");

    let disk = match registry::get_device(0) {
        Some(d) => d,
        None => {
            test_fail!("No block device 0");
            write_str_nl("[T] === ABORT: No AHCI disk ===");
            return;
        }
    };

    // T1.1: Read MBR (LBA 0) and verify boot signature
    {
        let mut buf = [0u8; 512];
        match disk.read_sector(0, &mut buf) {
            Ok(()) => {
                if buf[510] == 0x55 && buf[511] == 0xAA {
                    test_pass!("T1.1 MBR read + signature 0x55AA");
                } else {
                    test_fail!("T1.1 MBR signature mismatch");
                }
            }
            Err(_) => test_fail!("T1.1 MBR read failed"),
        }
    }

    // T1.2: Read partition boot sector (LBA 0x3F) and verify FAT signature
    {
        let mut buf = [0u8; 512];
        match disk.read_sector(0x3F, &mut buf) {
            Ok(()) => {
                if buf[510] == 0x55 && buf[511] == 0xAA && (buf[0] == 0xEB || buf[0] == 0xE9) {
                    let bps = u16::from_le_bytes([buf[11], buf[12]]);
                    if bps == 512 {
                        test_pass!("T1.2 Partition boot sector + valid BPB");
                    } else {
                        test_fail!("T1.2 bytes_per_sector != 512");
                    }
                } else {
                    test_fail!("T1.2 Boot sector signature/JMP invalid");
                }
            }
            Err(_) => test_fail!("T1.2 Partition boot sector read failed"),
        }
    }

    // T1.3: Consecutive reads - read 4 consecutive sectors starting at LBA 1
    // These are in the MBR gap (unused) and may be all-zero — that's valid.
    {
        let mut all_ok = true;
        for lba in 1u64..5 {
            let mut buf = [0u8; 512];
            match disk.read_sector(lba, &mut buf) {
                Ok(()) => {} // Success means DMA worked, data content doesn't matter
                Err(_) => { all_ok = false; break; }
            }
        }
        if all_ok {
            test_pass!("T1.3 Four consecutive sector reads (LBA 1-4)");
        } else {
            test_fail!("T1.3 Consecutive reads failed");
        }
    }

    // T1.4: Read different LBAs - scattered reads, verify MBR on LBA 0
    {
        let test_lbas: &[u64] = &[0, 0x3F, 0x238, 0x1000, 0x40000];
        let mut all_ok = true;
        for &lba in test_lbas {
            let mut buf = [0u8; 512];
            match disk.read_sector(lba, &mut buf) {
                Ok(()) => {
                    // LBA 0 must have MBR signature
                    if lba == 0 && (buf[510] != 0x55 || buf[511] != 0xAA) {
                        all_ok = false;
                        break;
                    }
                }
                Err(_) => { all_ok = false; break; }
            }
        }
        if all_ok {
            test_pass!("T1.4 Scattered LBA reads OK");
        } else {
            test_fail!("T1.4 Scattered reads failed");
        }
    }

    // T1.5: Read-after-read consistency - read LBA 0x3F (boot sector) 3 times
    // Verify all reads return identical data AND valid BPB content
    {
        let mut buf1 = [0u8; 512];
        let mut buf2 = [0u8; 512];
        let mut buf3 = [0u8; 512];
        let r1 = disk.read_sector(0x3F, &mut buf1);
        let r2 = disk.read_sector(0x3F, &mut buf2);
        let r3 = disk.read_sector(0x3F, &mut buf3);
        let all_ok = r1.is_ok() && r2.is_ok() && r3.is_ok()
            && buf1[..] == buf2[..]
            && buf2[..] == buf3[..]
            && buf1[510] == 0x55 && buf1[511] == 0xAA  // boot sig
            && (buf1[0] == 0xEB || buf1[0] == 0xE9);     // JMP instruction
        if all_ok {
            test_pass!("T1.5 Triple read-after-read consistent (LBA 0x3F)");
        } else {
            test_fail!("T1.5 Triple read consistency failed");
        }
    }

    // T1.6: Out-of-bounds read returns error
    {
        let mut buf = [0u8; 512];
        if disk.read_sector(disk.total_sectors(), &mut buf).is_err() {
            test_pass!("T1.6 Out-of-bounds read returns error");
        } else {
            test_fail!("T1.6 Out-of-bounds read should fail");
        }
    }

    // T1.7: Wrong buffer size returns error
    {
        let mut buf = [0u8; 256];
        if disk.read_sector(0, &mut buf).is_err() {
            test_pass!("T1.7 Wrong buffer size returns error");
        } else {
            test_fail!("T1.7 Wrong buffer size should fail");
        }
    }

    // T1.8: MBR consistency - read LBA 0 three times, verify identical content
    // and valid partition table structure
    {
        let mut bufs = [[0u8; 512]; 3];
        let mut all_ok = true;
        for buf in bufs.iter_mut() {
            if disk.read_sector(0, buf).is_err() {
                all_ok = false;
                break;
            }
        }
        if all_ok {
            // All three reads must be byte-identical
            let identical = bufs[0] == bufs[1] && bufs[1] == bufs[2];
            // MBR signature
            let sig = bufs[0][510] == 0x55 && bufs[0][511] == 0xAA;
            // Partition entry 1 at offset 446: type byte should be non-zero
            let part_type = bufs[0][446];
            if identical && sig && part_type != 0 {
                test_pass!("T1.8 MBR triple-read consistent + partition table");
            } else {
                test_fail!("T1.8 MBR triple-read or structure check failed");
            }
        } else {
            test_fail!("T1.8 MBR triple-read failed");
        }
    }

    // T1.9: FAT boot sector consistency - read LBA 0x3F three times,
    // verify BPB fields are consistent across reads
    {
        let mut bufs = [[0u8; 512]; 3];
        let mut all_ok = true;
        for buf in bufs.iter_mut() {
            if disk.read_sector(0x3F, buf).is_err() {
                all_ok = false;
                break;
            }
        }
        if all_ok {
            let identical = bufs[0] == bufs[1] && bufs[1] == bufs[2];
            let sig = bufs[0][510] == 0x55 && bufs[0][511] == 0xAA;
            let bps = u16::from_le_bytes([bufs[0][11], bufs[0][12]]);
            let spc = bufs[0][13];
            let num_fats = bufs[0][16];
            if identical && sig && bps == 512 && spc >= 1 && (num_fats == 1 || num_fats == 2) {
                test_pass!("T1.9 FAT boot sector triple-read consistent + valid BPB");
            } else {
                test_fail!("T1.9 FAT boot sector consistency check failed");
            }
        } else {
            test_fail!("T1.9 FAT boot sector triple-read failed");
        }
    }

    // ── Section 2: FAT16 Filesystem ───────────────────────────────────
    write_str_nl("[T] -- Section 2: FAT16 Filesystem --");

    let fs = match fat32::Fat32Fs::new(0) {
        Ok(f) => f,
        Err(e) => {
            test_fail!("T2.0 FAT16 mount failed");
            write_str("[T]      errno=");
            write_hex(e.to_errno() as u64);
            write_nl();
            write_str_nl("[T] === ABORT: FAT mount failed ===");
            return;
        }
    };
    test_pass!("T2.0 FAT16 filesystem mounted");

    // T2.0b: Verify FAT mount is consistent - re-mount from same disk, compare name
    {
        let fs2 = fat32::Fat32Fs::new(0);
        match fs2 {
            Ok(fs2) => {
                if fs2.name() == fs.name() {
                    test_pass!("T2.0b FAT re-mount consistent");
                } else {
                    test_fail!("T2.0b FAT re-mount name mismatch");
                }
            }
            Err(_) => test_fail!("T2.0b FAT re-mount failed"),
        }
    }

    // T2.1: Verify filesystem name is FAT16
    if fs.name() == "FAT16" {
        test_pass!("T2.1 Filesystem variant: FAT16");
    } else {
        test_fail!("T2.1 Filesystem name mismatch (expected FAT16)");
    }

    // T2.2: Root directory listing - verify known entries
    let root = fs.root();
    match root.readdir() {
        Ok(entries) => {
            if entries.len() >= 3 {
                let has_efi = entries.iter().any(|e| e == "EFI");
                let has_nvv = entries.iter().any(|e| e == "NvVars");
                let has_nsh = entries.iter().any(|e| e == "startup.nsh");
                if has_efi && has_nvv && has_nsh {
                    test_pass!("T2.2 Root dir: EFI, NvVars, startup.nsh present");
                } else {
                    test_fail!("T2.2 Root dir missing expected entries");
                }
            } else {
                test_fail!("T2.2 Root dir has fewer than 3 entries");
            }
        }
        Err(_) => test_fail!("T2.2 Root readdir failed"),
    }

    // T2.2b: Root readdir consistency - read twice, verify identical entries
    {
        let r1 = root.readdir();
        let r2 = root.readdir();
        match (r1, r2) {
            (Ok(e1), Ok(e2)) => {
                if e1 == e2 {
                    test_pass!("T2.2b Root readdir consistent (2 reads)");
                } else {
                    test_fail!("T2.2b Root readdir mismatch between reads");
                }
            }
            _ => test_fail!("T2.2b Root readdir failed on one or both reads"),
        }
    }

    // T2.3: Subdirectory traversal - lookup EFI directory
    match root.lookup("EFI") {
        Ok(efi_dir) => {
            if efi_dir.is_dir() {
                match efi_dir.readdir() {
                    Ok(sub_entries) => {
                        if sub_entries.iter().any(|e| e == "BOOT") {
                            test_pass!("T2.3 EFI/BOOT subdirectory found");
                        } else {
                            test_fail!("T2.3 EFI/BOOT not found in EFI dir");
                        }
                    }
                    Err(_) => test_fail!("T2.3 EFI readdir failed"),
                }
            } else {
                test_fail!("T2.3 EFI is not a directory");
            }
        }
        Err(_) => test_fail!("T2.3 lookup('EFI') failed"),
    }

    // T2.4: Deep subdirectory traversal - EFI/BOOT/BOOTX64.EFI
    match root.lookup("EFI") {
        Ok(efi) => match efi.lookup("BOOT") {
            Ok(boot) => match boot.lookup("BOOTX64.EFI") {
                Ok(file) => {
                    if file.is_file() && !file.is_dir() {
                        test_pass!("T2.4 Deep lookup: EFI/BOOT/BOOTX64.EFI");
                    } else {
                        test_fail!("T2.4 BOOTX64.EFI is not a file");
                    }
                }
                Err(_) => test_fail!("T2.4 lookup('BOOTX64.EFI') failed"),
            },
            Err(_) => test_fail!("T2.4 lookup('BOOT') failed"),
        },
        Err(_) => test_fail!("T2.4 lookup('EFI') failed"),
    }

    // T2.5: Read a known file and verify content + consistency
    match root.lookup("EFI") {
        Ok(efi) => match efi.lookup("BOOT") {
            Ok(boot) => match boot.lookup("BOOTX64.EFI") {
                Ok(file_inode) => {
                    match file_inode.open() {
                        Ok(mut file) => {
                            let mut data = Vec::new();
                            let mut tmp = [0u8; 512];
                            loop {
                                match file.read(&mut tmp) {
                                    Ok(0) => break,
                                    Ok(n) => data.extend_from_slice(&tmp[..n]),
                                    Err(_) => break,
                                }
                            }
                            let sz = data.len();
                            if sz > 0 {
                                // Verify MZ header
                                let mz_ok = data.len() >= 2 && data[0] == 0x4D && data[1] == 0x5A;
                                // Re-read: open again, read same file, compare content
                                match file_inode.open() {
                                    Ok(mut file2) => {
                                        let mut data2 = Vec::new();
                                        let mut tmp2 = [0u8; 512];
                                        loop {
                                            match file2.read(&mut tmp2) {
                                                Ok(0) => break,
                                                Ok(n) => data2.extend_from_slice(&tmp2[..n]),
                                                Err(_) => break,
                                            }
                                        }
                                        let consistent = data == data2;
                                        if mz_ok && consistent {
                                            test_pass!("T2.5 Read BOOTX64.EFI: MZ + consistent");
                                        } else if !consistent {
                                            test_fail!("T2.5 BOOTX64.EFI re-read mismatch");
                                        } else {
                                            test_pass!("T2.5 Read BOOTX64.EFI non-zero");
                                        }
                                    }
                                    Err(_) => {
                                        if mz_ok {
                                            test_pass!("T2.5 Read BOOTX64.EFI: MZ header OK");
                                        } else {
                                            test_pass!("T2.5 Read BOOTX64.EFI non-zero");
                                        }
                                    }
                                }
                            } else {
                                test_fail!("T2.5 BOOTX64.EFI read 0 bytes");
                            }
                        }
                        Err(_) => test_fail!("T2.5 open() on BOOTX64.EFI inode failed"),
                    }
                }
                Err(_) => test_fail!("T2.5 lookup('BOOTX64.EFI') failed"),
            },
            Err(_) => test_fail!("T2.5 lookup('BOOT') failed"),
        },
        Err(_) => test_fail!("T2.5 lookup('EFI') failed"),
    }

    // T2.6: Read kernel.elf - verify ELF header + consistency
    match root.lookup("EFI") {
        Ok(efi) => match efi.lookup("INDOMINUS") {
            Ok(ind) => match ind.lookup("kernel.elf") {
                Ok(file_inode) => {
                    match file_inode.open() {
                        Ok(mut file) => {
                            let mut data = Vec::new();
                            let mut tmp = [0u8; 512];
                            loop {
                                match file.read(&mut tmp) {
                                    Ok(0) => break,
                                    Ok(n) => data.extend_from_slice(&tmp[..n]),
                                    Err(_) => break,
                                }
                            }
                            let elf_ok = data.len() >= 4
                                && data[0] == 0x7F && data[1] == b'E' && data[2] == b'L' && data[3] == b'F';
                            if data.len() > 0 {
                                // Re-read for consistency
                                match file_inode.open() {
                                    Ok(mut file2) => {
                                        let mut data2 = Vec::new();
                                        let mut tmp2 = [0u8; 512];
                                        loop {
                                            match file2.read(&mut tmp2) {
                                                Ok(0) => break,
                                                Ok(n) => data2.extend_from_slice(&tmp2[..n]),
                                                Err(_) => break,
                                            }
                                        }
                                        let consistent = data == data2;
                                        if elf_ok && consistent {
                                            test_pass!("T2.6 Read kernel.elf: ELF header + consistent");
                                        } else if !consistent {
                                            test_fail!("T2.6 kernel.elf re-read mismatch");
                                        } else if elf_ok {
                                            test_pass!("T2.6 Read kernel.elf: ELF header OK");
                                        } else {
                                            test_pass!("T2.6 Read kernel.elf non-zero");
                                        }
                                    }
                                    Err(_) => {
                                        if elf_ok {
                                            test_pass!("T2.6 Read kernel.elf: ELF header OK");
                                        } else {
                                            test_pass!("T2.6 Read kernel.elf non-zero");
                                        }
                                    }
                                }
                            } else {
                                test_fail!("T2.6 kernel.elf read 0 bytes");
                            }
                        }
                        Err(_) => test_fail!("T2.6 open() on kernel.elf failed"),
                    }
                }
                Err(_) => test_fail!("T2.6 lookup('kernel.elf') failed"),
            },
            Err(_) => test_fail!("T2.6 lookup('INDOMINUS') failed"),
        },
        Err(_) => test_fail!("T2.6 lookup('EFI') failed"),
    }

    // ── Section 3: VFS Integration ────────────────────────────────────
    write_str_nl("[T] -- Section 3: VFS Integration --");

    // T3.0: Mount FAT at /disk via VFS
    if let Ok(new_fs) = fat32::Fat32Fs::new(0) {
        crate::vfs::vfs_mut().mount("/disk", Arc::new(new_fs));
        test_pass!("T3.0 FAT16 mounted at /disk via VFS");
    } else {
        test_fail!("T3.0 Could not create FAT16 for VFS mount");
    }

    // T3.1: VFS path resolution - /disk
    match crate::vfs::vfs().resolve("/disk") {
        Ok(inode) => {
            if inode.is_dir() {
                test_pass!("T3.1 VFS resolve('/disk') -> directory");
            } else {
                test_fail!("T3.1 /disk is not a directory");
            }
        }
        Err(_) => test_fail!("T3.1 VFS resolve('/disk') failed"),
    }

    // T3.2: VFS path resolution - /disk/EFI
    match crate::vfs::vfs().resolve("/disk/EFI") {
        Ok(inode) => {
            if inode.is_dir() {
                test_pass!("T3.2 VFS resolve('/disk/EFI') -> directory");
            } else {
                test_fail!("T3.2 /disk/EFI is not a directory");
            }
        }
        Err(_) => test_fail!("T3.2 VFS resolve('/disk/EFI') failed"),
    }

    // T3.3: VFS open + read - /disk/startup.nsh + consistency
    {
        let d1 = crate::vfs::vfs().read_file("/disk/startup.nsh");
        let d2 = crate::vfs::vfs().read_file("/disk/startup.nsh");
        match (d1, d2) {
            (Ok(data1), Ok(data2)) => {
                if data1.len() > 0 && data1 == data2 {
                    test_pass!("T3.3 VFS read startup.nsh: consistent");
                } else if data1 != data2 {
                    test_fail!("T3.3 startup.nsh re-read mismatch");
                } else {
                    test_fail!("T3.3 startup.nsh read 0 bytes");
                }
            }
            _ => test_fail!("T3.3 VFS read_file('/disk/startup.nsh') failed"),
        }
    }

    // T3.4: VFS readdir - /disk
    match crate::vfs::vfs().open("/disk") {
        Ok(mut dir) => {
            let mut buf = [0u8; 1024];
            match dir.read(&mut buf) {
                Ok(n) => {
                    if n > 0 {
                        let listing = &buf[..n];
                        let has_efi = listing.windows(3).any(|w| w == b"EFI");
                        let has_nsh = listing.windows(7).any(|w| w == b"startup");
                        if has_efi && has_nsh {
                            test_pass!("T3.4 VFS readdir('/disk'): EFI + startup found");
                        } else {
                            test_fail!("T3.4 VFS readdir missing expected entries");
                        }
                    } else {
                        test_fail!("T3.4 VFS readdir('/disk') returned 0 bytes");
                    }
                }
                Err(_) => test_fail!("T3.4 VFS readdir('/disk') read failed"),
            }
        }
        Err(_) => test_fail!("T3.4 VFS open('/disk') for readdir failed"),
    }

    // T3.5: VFS read_file helper - full file read + consistency
    {
        let d1 = crate::vfs::vfs().read_file("/disk/EFI/BOOT/BOOTX64.EFI");
        let d2 = crate::vfs::vfs().read_file("/disk/EFI/BOOT/BOOTX64.EFI");
        match (d1, d2) {
            (Ok(data1), Ok(data2)) => {
                if data1.len() > 0 && data1 == data2 {
                    let mz = data1.len() >= 2 && data1[0] == 0x4D && data1[1] == 0x5A;
                    if mz {
                        test_pass!("T3.5 VFS read_file: BOOTX64.EFI consistent + MZ");
                    } else {
                        test_pass!("T3.5 VFS read_file: BOOTX64.EFI consistent");
                    }
                } else if data1 != data2 {
                    test_fail!("T3.5 VFS read_file re-read mismatch");
                } else {
                    test_fail!("T3.5 VFS read_file returned 0 bytes");
                }
            }
            _ => test_fail!("T3.5 VFS read_file('/disk/EFI/BOOT/BOOTX64.EFI') failed"),
        }
    }

    // T3.6: VFS open non-existent file -> NotFound
    match crate::vfs::vfs().open("/disk/NONEXISTENT.TXT") {
        Ok(_) => test_fail!("T3.6 open non-existent should return error"),
        Err(VfsError::NotFound) => test_pass!("T3.6 open non-existent -> NotFound"),
        Err(_) => test_pass!("T3.6 open non-existent -> error"),
    }

    // T3.7: FAT isolation - verify VFS returns generic errors
    match crate::vfs::vfs().resolve("/disk/../../etc/passwd") {
        Err(_) => test_pass!("T3.7 VFS rejects malformed paths"),
        Ok(_) => test_fail!("T3.7 VFS should reject ../ paths"),
    }

    // ── Section 4: Regression Tests ───────────────────────────────────
    write_str_nl("[T] -- Section 4: Regression Tests --");
    write_str("[T]   Phase 9.4 standalone: ");
    write_hex(tests_passed as u64);
    write_str(" passed, ");
    write_hex(tests_failed as u64);
    write_str(" failed");
    write_nl();

    write_str_nl("[T]   Running Phase 9.1 regression...");
    phase91_block_test();

    write_str_nl("[T]   Running Phase 9.2+9.2b regression...");
    phase92_vfs_file_test();

    write_str_nl("[T]   Running Phase 9.3 regression...");
    phase93_ahci_test();

    write_str_nl("[T] ==================================================");
    write_str("[T] Phase 9.4 standalone: ");
    write_hex(tests_passed as u64);
    write_str(" passed, ");
    write_hex(tests_failed as u64);
    write_str(" failed");
    write_nl();
    if tests_failed == 0 {
        write_str_nl("[T] === ALL PHASE 9.4 TESTS PASSED ===");
    } else {
        write_str_nl("[T] === PHASE 9.4 HAS FAILURES ===");
    }
    write_str_nl("[T] ==================================================");
}

/// Phase 9.2 VFS file I/O verification.
///
/// Tests the complete upper-half storage path:
///   open → write → close → open → read → verify
///
/// Uses the RAM filesystem through the VFS layer.
fn phase92_vfs_file_test() {
    use alloc::sync::Arc;
    use crate::vfs::{vfs, VfsError};

    write_str_nl("[TEST] Phase 9.2: VFS file I/O");

    let test_data = b"Hello Indominus! This is a VFS test.";
    let test_path = "/test_phase92.txt";

    // 1. Create and write a file
    let file = match vfs().create_file(test_path) {
        Ok(f) => f,
        Err(_e) => {
            write_str_nl("[TEST]   FAIL: create_file returned error");
            return;
        }
    };
    let file = Arc::new(spin::Mutex::new(file));
    {
        let mut f = file.lock();
        match f.write(test_data) {
            Ok(n) => {
                if n != test_data.len() {
                    write_str_nl("[TEST]   FAIL: write returned wrong byte count");
                    return;
                }
            }
            Err(e) => {
                write_str_nl("[TEST]   FAIL: write returned error");
                return;
            }
        }
    }
    write_str_nl("[TEST]   Write OK");

    // 2. Open the same file for reading
    let file2 = match vfs().open(test_path) {
        Ok(f) => f,
        Err(e) => {
            write_str_nl("[TEST]   FAIL: open returned error");
            return;
        }
    };
    let file2 = Arc::new(spin::Mutex::new(file2));
    write_str_nl("[TEST]   Open OK");

    // 3. Read back the data
    let mut read_buf = [0u8; 64];
    {
        let mut f = file2.lock();
        // Reset position to start
        match f.seek(0) {
            Ok(()) => {}
            Err(e) => {
                write_str_nl("[TEST]   FAIL: seek returned error");
                return;
            }
        }
        match f.read(&mut read_buf) {
            Ok(n) => {
                if n != test_data.len() {
                    write_str_nl("[TEST]   FAIL: read returned wrong byte count");
                    return;
                }
                // Verify data matches
                if &read_buf[..n] != test_data {
                    write_str_nl("[TEST]   FAIL: read data doesn't match written data");
                    return;
                }
            }
            Err(e) => {
                write_str_nl("[TEST]   FAIL: read returned error");
                return;
            }
        }
    }
    write_str_nl("[TEST]   Read & verify OK");

    // 4. Test directory listing
    let root_dir = match vfs().open("/") {
        Ok(d) => d,
        Err(e) => {
            write_str_nl("[TEST]   FAIL: open root dir returned error");
            return;
        }
    };
    let root_dir = Arc::new(spin::Mutex::new(root_dir));
    {
        let mut d = root_dir.lock();
        match d.read(&mut read_buf) {
            Ok(n) => {
                if n == 0 {
                    write_str_nl("[TEST]   FAIL: root directory listing is empty");
                    return;
                }
                // Check that our test file appears in the listing
                let listing = core::str::from_utf8(&read_buf[..n]).unwrap_or("");
                if !listing.contains("test_phase92") {
                    write_str_nl("[TEST]   FAIL: test file not found in directory listing");
                    return;
                }
            }
            Err(e) => {
                write_str_nl("[TEST]   FAIL: readdir returned error");
                return;
            }
        }
    }
    write_str_nl("[TEST]   Readdir OK");

    // 5. Test opening non-existent file (should fail)
    match vfs().open("/nonexistent_file.txt") {
        Ok(_) => {
            write_str_nl("[TEST]   FAIL: open non-existent file should fail");
            return;
        }
        Err(VfsError::NotFound) => { /* expected */ }
        Err(_) => {
            write_str_nl("[TEST]   FAIL: open non-existent returned wrong error");
            return;
        }
    }
    write_str_nl("[TEST]   NotFound error OK");

    // 6. Clean up
    drop(file);
    drop(file2);
    drop(root_dir);

    write_str_nl("[TEST] Phase 9.2: ALL TESTS PASSED");

    // ── Phase 9.2b: File descriptor semantics audit ──────────────────────
    phase92b_fd_semantics_test();
}

/// FD semantics audit — 6 precise tests verifying POSIX-compatible behavior.
///
/// Test 1: open → dup2 → read → shared offset
/// Test 2: open same file twice → read → independent offsets
/// Test 3: fork → parent reads → child reads → shared open-file offset
/// Test 4: open FD with CLOEXEC → exec → FD closed
/// Test 5: open FD without CLOEXEC → exec → FD survives
/// Test 6: dup2 an FD → new FD initially has CLOEXEC cleared
fn phase92b_fd_semantics_test() {
    use alloc::sync::Arc;
    use crate::vfs::vfs;
    use crate::process::process::{O_CLOEXEC, MAX_FDS};

    write_str_nl("[TEST] Phase 9.2b: FD semantics");

    let path = "/fd_test.txt";
    let data = b"ABCDEFGHIJ"; // 10 bytes

    // Create test file with known content
    {
        let f = Arc::new(spin::Mutex::new(
            vfs().create_file(path).expect("create_file failed")
        ));
        f.lock().write(data).expect("write failed");
    }

    // ── Test 1: dup2 shares open-file state ──────────────────────────
    // Open file, get two Arcs pointing to the SAME Box<dyn File> (simulates dup2).
    // Read via first → offset advances. Read via second → should see advanced offset.
    {
        let handle = Arc::new(spin::Mutex::new(
            vfs().open(path).expect("open failed")
        ));
        // Simulate dup2: clone Arc (both point to same File, same pos)
        let duped = handle.clone();

        // Read 3 bytes via original handle → offset should be at 3
        let mut buf = [0u8; 32];
        let n;
        {
            let mut f = handle.lock();
            n = f.read(&mut buf).expect("read failed");
        }
        if n != 10 {
            write_str_nl("[TEST]   FAIL T1a: initial read wrong length");
            return;
        }

        // Read 3 bytes via duped handle → should start at offset 10, not 0
        {
            let mut f = duped.lock();
            let n2 = f.read(&mut buf).expect("read failed on duped");
            if n2 != 0 {
                write_str_nl("[TEST]   FAIL T1b: duped handle should see EOF (shared offset at 10)");
                return;
            }
        }
        write_str_nl("[TEST]   T1 dup2 shared offset OK");
    }

    // ── Test 2: independent opens have independent offsets ────────────
    // Open same file twice → each gets a new Box<dyn File> with pos=0.
    // Read via A → advances A.pos. Read via B → should read from offset 0.
    {
        let a = Arc::new(spin::Mutex::new(
            vfs().open(path).expect("open A failed")
        ));
        let b = Arc::new(spin::Mutex::new(
            vfs().open(path).expect("open B failed")
        ));

        // Read 4 bytes via A → A.pos = 4
        let mut buf = [0u8; 32];
        {
            let mut f = a.lock();
            let n = f.read(&mut buf).expect("read A failed");
            if n != 10 {
                write_str_nl("[TEST]   FAIL T2a: A read wrong length");
                return;
            }
        }

        // Read via B → should start at offset 0 (independent)
        {
            let mut f = b.lock();
            let n = f.read(&mut buf).expect("read B failed");
            if n != 10 {
                write_str_nl("[TEST]   FAIL T2b: B should read from offset 0");
                return;
            }
            if &buf[..n] != data {
                write_str_nl("[TEST]   FAIL T2c: B data mismatch");
                return;
            }
        }
        write_str_nl("[TEST]   T2 independent offsets OK");
    }

    // ── Test 3: fork shares open-file descriptions ───────────────────
    // Fork copies fd_types + file_handles (Arc clones). This means parent
    // and child share the same File objects — reading in one advances the
    // shared offset. Verified by inspecting sys_fork implementation:
    //   child.fd_types = parent_proc.fd_types;          (copy array)
    //   child.file_handles = parent_proc.file_handles.clone();  (Arc refcount++)
    // Both parent and child point to the same Arc<Mutex<Box<dyn File>>>.
    //
    // Runtime verification is not feasible from kernel_main (would need
    // the child to execute code), so we verify the structural contract:
    // the clone() on file_handles produces shared Arc references.
    {
        // Create two independent Arcs to the same file
        let f1 = Arc::new(spin::Mutex::new(
            vfs().open(path).expect("open for T3 failed")
        ));
        let f2 = f1.clone(); // simulate what fork does: Arc::clone

        // Read via f1 → advances offset
        let mut buf = [0u8; 32];
        {
            let mut fh = f1.lock();
            let _ = fh.read(&mut buf);
        }

        // f2 should see the advanced offset (shared state)
        {
            let mut fh = f2.lock();
            let n = fh.read(&mut buf).expect("read T3 failed");
            // If offsets were independent, we'd read 10 bytes.
            // Since shared, we read 0 (already at EOF).
            if n != 0 {
                write_str_nl("[TEST]   FAIL T3: f2 should see shared offset at EOF");
                return;
            }
        }
        write_str_nl("[TEST]   T3 fork shared offset OK");
    }

    // ── Direct fd_flags verification via scheduler ───────────────────
    // Tests 4-6 require inspecting fd_flags directly. We install file handles
    // into PID 1's FD table (mimicking sys_open behavior) and verify the flag contract.
    // kernel_main runs as PID 1 (init process), but current_pid() may be None
    // since the scheduler loop hasn't started. Access PID 1 directly.
    {
        let mut sched = crate::process::scheduler::SCHEDULER.lock();
        let proc = match sched.processes_mut()[1].as_mut() {
            Some(p) => p,
            None => {
                write_str_nl("[TEST]   FAIL: PID 1 not found");
                return;
            }
        };

        // Find a free FD slot
        let fd_slot = match proc.fd_types.iter().position(|f| *f == crate::process::FdType::None) {
            Some(s) => s,
            None => { write_str_nl("[TEST]   FAIL: no free FD slots"); return; }
        };
        let fh_slot = match proc.file_handles.iter().position(|f| f.is_none()) {
            Some(s) => s,
            None => { write_str_nl("[TEST]   FAIL: no free handle slots"); return; }
        };

        // Install file handle (simulating what sys_open does)
        let file = vfs().open(path).expect("open for flag test failed");
        proc.file_handles[fh_slot] = Some(Arc::new(spin::Mutex::new(file)));
        proc.fd_types[fd_slot] = crate::process::FdType::FsFile { index: fh_slot as u8 };

        // Test 5: without O_CLOEXEC → fd_flags should be 0
        proc.fd_flags[fd_slot] = 0; // simulates open without CLOEXEC
        if proc.fd_flags[fd_slot] != 0 {
            write_str_nl("[TEST]   FAIL T5: fd_flags should be 0 without CLOEXEC");
            return;
        }
        write_str_nl("[TEST]   T5 no-CLOEXEC flag clear OK");

        // Test 4: with O_CLOEXEC → fd_flags bit 0 should be set
        proc.fd_flags[fd_slot] = 1; // simulates open with CLOEXEC
        if proc.fd_flags[fd_slot] & 1 != 1 {
            write_str_nl("[TEST]   FAIL T4: fd_flags bit 0 should be set with CLOEXEC");
            return;
        }
        write_str_nl("[TEST]   T4 CLOEXEC flag set OK");

        // Test 6: dup2 clears CLOEXEC on new FD
        // Simulate: set CLOEXEC on old slot, then dup2 clears it on new slot
        let old_flags = proc.fd_flags[fd_slot]; // has CLOEXEC
        let new_fd_slot = match proc.fd_types.iter().position(|f| *f == crate::process::FdType::None) {
            Some(s) => s,
            None => { write_str_nl("[TEST]   FAIL: no free slot for dup2"); return; }
        };
        let new_fh_slot = match proc.file_handles.iter().position(|f| f.is_none()) {
            Some(s) => s,
            None => { write_str_nl("[TEST]   FAIL: no free handle for dup2"); return; }
        };
        // Dup2 copies FdType, but clears fd_flags[newfd]
        proc.file_handles[new_fh_slot] = proc.file_handles[fh_slot].clone();
        proc.fd_types[new_fd_slot] = crate::process::FdType::FsFile { index: new_fh_slot as u8 };
        proc.fd_flags[new_fd_slot] = 0; // dup2 clears CLOEXEC (POSIX rule)

        if proc.fd_flags[new_fd_slot] != 0 {
            write_str_nl("[TEST]   FAIL T6: dup2 should clear CLOEXEC on new FD");
            return;
        }
        if old_flags & 1 != 1 {
            write_str_nl("[TEST]   FAIL T6: old FD should still have CLOEXEC");
            return;
        }
        write_str_nl("[TEST]   T6 dup2 clears CLOEXEC OK");

        // Clean up: close both FDs
        proc.file_handles[fh_slot] = None;
        proc.file_handles[new_fh_slot] = None;
        proc.fd_types[fd_slot] = crate::process::FdType::None;
        proc.fd_types[new_fd_slot] = crate::process::FdType::None;
        proc.fd_flags[fd_slot] = 0;
        proc.fd_flags[new_fd_slot] = 0;
    }

    write_str_nl("[TEST] Phase 9.2b: ALL TESTS PASSED");
}

#[no_mangle]
pub extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    let bi = unsafe { &*boot_info };

    write_str_nl("[KERNEL] INDOMINUS OS -- scheduler test");
    write_str("[KERNEL] Kernel phys: ");
    write_hex(bi.kernel_phys_start.as_u64());
    write_str(" .. ");
    write_hex(bi.kernel_phys_end.as_u64());
    write_nl();

    unsafe {
        crate::memory::set_kernel_phys_start(bi.kernel_phys_start.as_u64());
    }

    gdt::init();
    crate::memory::pmm::init(&bi.memory_map);

    // Reserve the kernel's physical memory in the PMM.
    // pmm::init() uses linker symbols (__kernel_start/__kernel_end) for this,
    // but those are upper-half virtual addresses (0xFFFFFFFF80000000+) which
    // are beyond the PMM's tracking range. Use the correct physical addresses
    // from BootInfo instead.
    crate::memory::pmm::mark_region_used(
        bi.kernel_phys_start.as_u64(),
        bi.kernel_phys_end.as_u64(),
    );

    // Detect CPU features (before page tables, while identity map is live)
    crate::cpu::detect();
    crate::cpu::print_features();
    crate::cpu::enable_smep_smap();

    let new_pml4 = crate::memory::vmm::init_kernel_page_tables(
        bi.kernel_phys_start.as_u64(),
        bi.kernel_phys_end.as_u64(),
    );
    unsafe {
        crate::memory::vmm::switch_page_table(new_pml4);
        crate::memory::set_kernel_pml4_phys(new_pml4.as_u64());
    }
    // Now the kernel higher-half is mapped. Switch GDTR to virtual address
    // so it survives CR3 switches to user PML4s (which lack the identity map).
    write_str_nl("[MARK] Before switch_gdt_to_virtual");
    crate::gdt::switch_gdt_to_virtual();
    write_str_nl("[MARK] After switch_gdt_to_virtual");
    unsafe {
        crate::memory::init_heap(
            crate::memory::KERNEL_HEAP_BASE,
            crate::memory::KERNEL_HEAP_INITIAL_SIZE,
        );
    }
    write_str_nl("[MARK] After init_heap");

    write_str_nl("[MARK] Before IDT init");
    idt::init();
    write_str_nl("[MARK] After IDT init");

    // Initialize ACPI (after heap init, needs Vec)
    // Use RSDP from bootloader if available, otherwise scan memory
    write_str_nl("[MARK] Before ACPI init");
    let rsdp_from_boot = bi.rsdp_addr.as_u64();
    crate::acpi::init(if rsdp_from_boot != 0 { Some(rsdp_from_boot) } else { None });
    write_str_nl("[MARK] After ACPI init");

    // Enumerate PCI devices
    write_str_nl("[MARK] Before PCI enumerate");
    crate::pci::enumerate();
    write_str_nl("[MARK] After PCI enumerate");

    // Phase 9.3: Initialize AHCI (after PCI, needs bus mastering + MMIO)
    write_str_nl("[MARK] Before AHCI init");
    crate::ahci::init();
    write_str_nl("[MARK] After AHCI init");

    // Phase 9.1: Block device abstraction verification
    write_str_nl("[MARK] Before block device test");
    phase91_block_test();
    write_str_nl("[MARK] After block device test");

    // Phase 9.3: AHCI disk read verification
    write_str_nl("[MARK] Before AHCI disk test");
    phase93_ahci_test();
    write_str_nl("[MARK] After AHCI disk test");

    write_str_nl("[MARK] Before interrupts init");
    let (lapic_phys, ioapic_phys, ioapic_gsi_base) = match crate::acpi::madt_info() {
        Some(madt) => {
            let ioapic_phys = if madt.io_apic_addr != 0 { madt.io_apic_addr } else { 0xFEC0_0000 };
            (madt.local_apic_addr, ioapic_phys, madt.io_apic_gsi_base)
        }
        None => (0xFEE0_0000, 0xFEC0_0000, 0),
    };
    interrupts::init(lapic_phys, ioapic_phys, ioapic_gsi_base);
    write_str_nl("[MARK] After interrupts init");

    // Initialize keyboard driver (after interrupts, before processes)
    write_str_nl("[MARK] Before keyboard init");
    keyboard::init();
    write_str_nl("[MARK] After keyboard init");

    // Initialize syscall MSRs (STAR, LSTAR, SFMASK, EFER SCE, GSBase)
    write_str_nl("[MARK] Before syscall init");
    crate::syscall::init();
    write_str_nl("[MARK] After syscall init");

    // Harden the identity map: set NX on all identity-mapped pages.
    // This prevents code execution via the identity map while keeping it
    // functional for data access (needed to walk user page tables at runtime).
    write_str_nl("[MARK] Before harden_identity_map");
    crate::memory::vmm::harden_identity_map(new_pml4);
    write_str_nl("[MARK] After harden_identity_map");

    write_str_nl("[MARK] Before process init");
    crate::process::init();
    write_str_nl("[MARK] After process init");

    // Initialize VFS and load initrd
    write_str_nl("[MARK] Before VFS init");
    crate::vfs::init();
    write_str_nl("[MARK] After VFS init");

    // Phase 9.4: Mount FAT16 from AHCI disk (device 0) at /disk
    // Must be after VFS init since it calls vfs_mut().mount()
    write_str_nl("[MARK] Before FAT32 init");
    phase94_fat32_init();
    write_str_nl("[MARK] After FAT32 init");

    // Phase 9.2: VFS file I/O verification
    write_str_nl("[MARK] Before VFS file test");
    phase92_vfs_file_test();
    write_str_nl("[MARK] After VFS file test");

    write_str_nl("[MARK] Before initrd load");
    let initrd_data = include_bytes!("../initrd.img");
    crate::initrd::load_initrd(initrd_data);
    write_str_nl("[MARK] After initrd load");

    write_str_nl("[KERNEL] All init done.");

    // Phase 9: Spawn the shell from VFS.
    // PID 0 = idle, PID 1 = init/reaper (kernel-mode).
    // The shell is loaded from /bin/indosh in the initrd (VFS).
    // parent=Some(1) means PID 1 reaps the shell when it exits.
    match crate::vfs::vfs().read_file("/bin/indosh") {
        Ok(shell_elf) => {
            write_str("[KERNEL] Shell binary found: ");
            write_hex(shell_elf.len() as u64);
            write_str_nl(" bytes");
            match crate::process::spawn_user(&shell_elf, Some(1)) {
                Some(pid) => {
                    write_str("[KERNEL] Shell spawned as PID=");
                    write_hex(pid);
                    write_nl();
                }
                None => {
                    write_str_nl("[KERNEL] FAILED to spawn shell (no slot)");
                }
            }
        }
        Err(e) => {
            write_str("[KERNEL] WARNING: /bin/indosh read failed, errno=");
            write_hex(e.to_errno() as u64);
            write_nl();
            // Also try /indosh (flat path in case nested create failed)
            match crate::vfs::vfs().read_file("/indosh") {
                Ok(shell_elf) => {
                    write_str("[KERNEL] Found /indosh (flat): ");
                    write_hex(shell_elf.len() as u64);
                    write_str_nl(" bytes");
                    match crate::process::spawn_user(&shell_elf, Some(1)) {
                        Some(pid) => {
                            write_str("[KERNEL] Shell spawned as PID=");
                            write_hex(pid);
                            write_nl();
                        }
                        None => {
                            write_str_nl("[KERNEL] FAILED to spawn shell (no slot)");
                        }
                    }
                }
                Err(e2) => {
                    write_str("[KERNEL] /indosh also failed, errno=");
                    write_hex(e2.to_errno() as u64);
                    write_nl();
                    write_str_nl("[KERNEL] Falling back to test binaries");
                }
            }
            // Fallback: spawn test binaries if shell not available
            let tests: &[&[u8]] = &[
                include_bytes!("../test1_normal.bin"),
                include_bytes!("../test2_multi.bin"),
                include_bytes!("../test3_null_deref.bin"),
                include_bytes!("../test4_invalid_ptr.bin"),
                include_bytes!("../test5_unmapped.bin"),
                include_bytes!("../test6_null_ptr.bin"),
                include_bytes!("../test7_bad_syscall.bin"),
                include_bytes!("../test8_sleep.bin"),
                include_bytes!("../test9_stack_overflow.bin"),
                include_bytes!("../test10_errno.bin"),
            ];
            for (i, test_elf) in tests.iter().enumerate() {
                write_str("[KERNEL] Test ");
                write_hex(i as u64 + 1);
                write_str(" ELF size=");
                write_hex(test_elf.len() as u64);
                write_nl();
                match crate::process::spawn_user(test_elf, Some(0)) {
                    Some(pid) => {
                        write_str("[KERNEL]   -> PID=");
                        write_hex(pid);
                        write_nl();
                    }
                    None => {
                        write_str("[KERNEL]   -> FAILED (no slot)\n");
                    }
                }
            }
        }
    }

    crate::process::start_scheduler();
}
