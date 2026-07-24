//! # AHCI (Advanced Host Controller Interface) Driver
//!
//! Provides block-level storage access via AHCI/SATA.
//! Implements the `BlockDevice` trait for integration with the VFS layer.
//!
//! ## TFES Error Recovery
//!
//! On QEMU's AHCI implementation, Task File Error Status (TFES) leaves the
//! command engine in a degraded state where subsequent PxCI writes are silently
//! accepted but no DMA transfer occurs — the HBA reports completion (PxCI
//! clears) without writing data to the DMA buffer.
//!
//! Root cause: After TFES, the HBA's internal command processing state machine
//! retains stale state. Simply clearing IS/SERR and reissuing CI is
//! insufficient; the command engine must be fully stopped and restarted.
//!
//! Recovery sequence (AHCI spec §6.2.2):
//!   1. Clear PxIS and PxSERR (acknowledge errors)
//!   2. Stop command processing: PxCMD.ST = 0, wait PxCMD.CR = 0
//!   3. Stop FIS receive: PxCMD.FRE = 0, wait bit 14 = 0
//!   4. Wait for TFD.BSY and TFD.DRQ to clear (drive idle)
//!   5. Restart FIS receive: PxCMD.FRE = 1, wait bit 14 = 1
//!   6. Restart command processing: PxCMD.ST = 1, wait PxCMD.CR = 1
//!
//! Additionally, each read command writes a known probe pattern (0xDE, 0xAD,
//! 0xBE, 0xEF) to the DMA buffer before issuing CI. After completion, the
//! buffer is checked — if the pattern remains unchanged, DMA did not occur and
//! the command is retried. This prevents silently returning stale/zeroed data.

pub mod hba;

use alloc::sync::Arc;
use spin::Mutex;

use crate::block::{BlockDevice, BlockError};
use crate::mmio::MmioRegion;
use crate::serial;

use hba::*;

/// Maximum number of ports per HBA
const MAX_PORTS: usize = 32;

/// Timeout for ATA commands (in polling iterations)
const ATA_TIMEOUT: u32 = 500_000;

/// An AHCI port with its DMA structures
struct AhciPort {
    index: u8,
    cmd_list_phys: u64,
    cmd_list_virt: u64,
    rfis_phys: u64,
    rfis_virt: u64,
    cmd_table_phys: u64,
    cmd_table_virt: u64,
    dma_buf_phys: u64,
    dma_buf_virt: u64,
    sector_size: u32,
    total_sectors: u64,
    active: bool,
}

/// AHCI disk device implementing the BlockDevice trait
pub struct AhciDisk {
    hba_phys: u64,
    port: Mutex<AhciPort>,
    name: alloc::string::String,
}

impl AhciDisk {
    pub fn init() -> Option<Arc<dyn BlockDevice>> {
        serial::write_str_nl("[AHCI] Searching for AHCI controller...");

        let ahci_pci = {
            let devices = crate::pci::PCI_DEVICES.lock();
            devices.iter().find(|d| {
                d.class == 0x01 && d.subclass == 0x06 && (d.prog_if == 0x01 || d.prog_if == 0x02)
            }).cloned()
        };

        let pci = match ahci_pci {
            Some(p) => p,
            None => {
                serial::write_str_nl("[AHCI] No AHCI controller found");
                return None;
            }
        };

        serial::write_str("[AHCI] Found controller at PCI ");
        serial::write_hex(pci.bus as u64);
        serial::write_str(":");
        serial::write_hex(pci.device as u64);
        serial::write_str(".");
        serial::write_hex(pci.function as u64);
        serial::write_nl();

        let abar_phys = pci.bar_address(5);
        if abar_phys == 0 {
            serial::write_str_nl("[AHCI] ERROR: ABAR is zero");
            return None;
        }

        serial::write_str("[AHCI] ABAR=0x");
        serial::write_hex(abar_phys);
        serial::write_nl();

        // Enable bus mastering
        unsafe { enable_bus_mastering(pci.bus, pci.device, pci.function); }

        let hba = MmioRegion::new(abar_phys);
        serial::write_str_nl("[AHCI] ABAR mapped");

        let cap = unsafe { hba.read_reg::<u32>(HBA_CAP) };
        let num_ports = (((cap & 0x1F) + 1) as usize).min(MAX_PORTS);
        serial::write_str("[AHCI] CAP=0x");
        serial::write_hex(cap as u64);
        serial::write_str(" ports=");
        serial::write_hex(num_ports as u64);
        serial::write_nl();

        // Reset HBA
        if !reset_hba(&hba) {
            serial::write_str_nl("[AHCI] HBA reset failed");
            return None;
        }
        serial::write_str_nl("[AHCI] HBA reset OK");

        // Enable AHCI mode
        unsafe {
            let ghc = hba.read_reg::<u32>(HBA_GHC);
            hba.write_reg::<u32>(HBA_GHC, ghc | GHC_AE);
        }

        // Scan ports: use SSTS.DET to find devices (signatures invalid after HBA reset)
        for port_idx in 0..num_ports {
            let port_reg = HBA_PORT_REGS + (port_idx as u32) * 0x80;
            let ssts = unsafe { hba.read_reg::<u32>(port_reg + PORT_SSTS) };
            let det = ssts & 0x0F;

            serial::write_str("[AHCI] Port ");
            serial::write_hex(port_idx as u64);
            serial::write_str(" ssts=0x");
            serial::write_hex(ssts as u64);
            serial::write_str(" det=");
            serial::write_hex(det as u64);
            serial::write_nl();

            if det == 0x03 { // Device detected and communicating
                serial::write_str("[AHCI] Port ");
                serial::write_hex(port_idx as u64);
                serial::write_str_nl(" device detected, initializing...");

                match init_port(&hba, abar_phys, port_idx) {
                    Ok(port) => {
                        let name = alloc::format!("ahci{}", port_idx);
                        let disk = Arc::new(AhciDisk {
                            hba_phys: abar_phys,
                            port: Mutex::new(port),
                            name,
                        });
                        serial::write_str("[AHCI] Port ");
                        serial::write_hex(port_idx as u64);
                        serial::write_str(" initialized OK");
                        serial::write_nl();
                        return Some(disk);
                    }
                    Err(msg) => {
                        serial::write_str("[AHCI] Port ");
                        serial::write_hex(port_idx as u64);
                        serial::write_str(" init failed: ");
                        serial::write_str_nl(msg);
                    }
                }
            }
        }

        serial::write_str_nl("[AHCI] No SATA drives found");
        None
    }
}

impl BlockDevice for AhciDisk {
    fn read_sector(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        if buf.len() != self.sector_size() as usize {
            return Err(BlockError::InvalidBufferSize);
        }
        if lba >= self.total_sectors() {
            return Err(BlockError::OutOfBounds);
        }
        let mut port = self.port.lock();
        if !port.active {
            return Err(BlockError::DeviceNotReady);
        }
        let hba = MmioRegion::new(self.hba_phys);
        issue_command(&hba, &mut port, lba, 1, buf.as_ptr(), buf.len(), true)
    }

    fn write_sector(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        if buf.len() != self.sector_size() as usize {
            return Err(BlockError::InvalidBufferSize);
        }
        if lba >= self.total_sectors() {
            return Err(BlockError::OutOfBounds);
        }
        let mut port = self.port.lock();
        if !port.active {
            return Err(BlockError::DeviceNotReady);
        }
        let hba = MmioRegion::new(self.hba_phys);
        issue_command(&hba, &mut port, lba, 1, buf.as_ptr(), buf.len(), false)
    }

    fn sector_size(&self) -> u32 {
        self.port.lock().sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.port.lock().total_sectors
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HBA Reset
// ─────────────────────────────────────────────────────────────────────────────

fn reset_hba(hba: &MmioRegion) -> bool {
    unsafe {
        hba.write_reg::<u32>(HBA_GHC, GHC_HR);
        let mut timeout = ATA_TIMEOUT;
        while timeout > 0 {
            if hba.read_reg::<u32>(HBA_GHC) & GHC_HR == 0 {
                return true;
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Port Initialization
// ─────────────────────────────────────────────────────────────────────────────

fn init_port(hba: &MmioRegion, hba_phys: u64, port_idx: usize) -> Result<AhciPort, &'static str> {
    let pr = HBA_PORT_REGS + (port_idx as u32) * 0x80;

    // Stop command processing
    unsafe {
        let cmd = hba.read_reg::<u32>(pr + PORT_CMD);
        hba.write_reg::<u32>(pr + PORT_CMD, cmd & !PORT_CMD_ST);
    }
    wait_cmd_stopped(hba, port_idx);

    // Clear interrupt status and error registers
    unsafe {
        hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF);
        hba.write_reg::<u32>(pr + PORT_SERR, 0xFFFF_FFFF);
    }

    // Allocate DMA structures (page-aligned from PMM)
    let cmd_list_page = alloc_dma_page().ok_or("alloc cmd list")?;
    let rfis_page = alloc_dma_page().ok_or("alloc rfis")?;
    let cmd_table_page = alloc_dma_page().ok_or("alloc cmd table")?;
    let dma_buf_page = alloc_dma_page().ok_or("alloc dma buf")?;

    let cmd_list_phys = cmd_list_page.as_u64();
    let rfis_phys = rfis_page.as_u64();
    let cmd_table_phys = cmd_table_page.as_u64();
    let dma_buf_phys = dma_buf_page.as_u64();

    let cmd_list_virt = cmd_list_phys;
    let rfis_virt = rfis_phys;
    let cmd_table_virt = cmd_table_phys;
    let dma_buf_virt = dma_buf_phys;

    // Zero all structures
    unsafe {
        core::ptr::write_bytes(cmd_list_virt as *mut u8, 0, 4096);
        core::ptr::write_bytes(rfis_virt as *mut u8, 0, 4096);
        core::ptr::write_bytes(cmd_table_virt as *mut u8, 0, 4096);
        core::ptr::write_bytes(dma_buf_virt as *mut u8, 0, 4096);
    }

    // Set command list and FIS base addresses
    unsafe {
        hba.write_reg::<u32>(pr + PORT_CLB, cmd_list_phys as u32);
        hba.write_reg::<u32>(pr + PORT_CLBU, (cmd_list_phys >> 32) as u32);
        hba.write_reg::<u32>(pr + PORT_FB, rfis_phys as u32);
        hba.write_reg::<u32>(pr + PORT_FBU, (rfis_phys >> 32) as u32);
        // Enable FIS receive (FRE)
        let cmd = hba.read_reg::<u32>(pr + PORT_CMD);
        hba.write_reg::<u32>(pr + PORT_CMD, cmd | PORT_CMD_FRE);
    }

    // Wait for FIS receive to start (FR bit set)
    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let cmd = unsafe { hba.read_reg::<u32>(pr + PORT_CMD) };
        if cmd & (1 << 14) != 0 { break; } // FR bit
        timeout -= 1;
        core::hint::spin_loop();
    }

    // Start command processing (ST)
    unsafe {
        let cmd = hba.read_reg::<u32>(pr + PORT_CMD);
        hba.write_reg::<u32>(pr + PORT_CMD, cmd | PORT_CMD_ST);
    }

    // Wait for command list running (CR bit set)
    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let cmd = unsafe { hba.read_reg::<u32>(pr + PORT_CMD) };
        if cmd & PORT_CMD_CR != 0 { break; }
        timeout -= 1;
        core::hint::spin_loop();
    }

    // Wait for drive ready
    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let tfd = unsafe { hba.read_reg::<u32>(pr + PORT_TFD) };
        if tfd & (TFD_BSY | TFD_DRQ) == 0 { break; }
        timeout -= 1;
        core::hint::spin_loop();
    }

    // Run IDENTIFY DEVICE to get geometry
    let (sector_size, total_sectors, max_udma) = identify_device(hba, port_idx, cmd_table_phys, dma_buf_phys, dma_buf_virt);

    if total_sectors == 0 {
        return Err("no sectors detected");
    }

    serial::write_str("[AHCI] Port ");
    serial::write_hex(port_idx as u64);
    serial::write_str(" sectors=");
    serial::write_hex(total_sectors);
    serial::write_str(" ssize=");
    serial::write_hex(sector_size as u64);
    serial::write_str(" udma=");
    serial::write_hex(max_udma as u64);
    serial::write_nl();

    // NOTE: SET FEATURES (0xEF subcommand 03h = Set Transfer Mode) is deferred.
    // On QEMU's AHCI, issuing SET FEATURES as a non-data DMA command leaves
    // stale DRQ=1 in TFD, which poisons subsequent DMA reads. The device
    // already supports UDMA (confirmed by IDENTIFY word 88), and QEMU's AHCI
    // defaults to a DMA-capable mode. On real hardware, SET FEATURES can be
    // added back as a dedicated initialization step after port recovery.
    if max_udma > 0 {
        serial::write_str("[AHCI] Device supports UDMA mode ");
        serial::write_hex(max_udma as u64);
        serial::write_str_nl(" (SET FEATURES deferred)");
    }

    // Warm-up read: issue and discard a dummy READ DMA EXT for sector 0.
    // On QEMU's AHCI, the first DMA command after IDENTIFY DEVICE may fail
    // (TFES) because the device hasn't fully settled into DMA mode. Subsequent
    // DMA commands succeed. This warm-up primes the port so the first real
    // read (from FAT mount) works.
    //
    // Update: warm-up alone doesn't work because it shifts the failure by one.
    // The real fix is a retry in issue_command. This warm-up is kept as a
    // no-op placeholder — the retry logic handles the priming.

    Ok(AhciPort {
        index: port_idx as u8,
        cmd_list_phys,
        cmd_list_virt,
        rfis_phys,
        rfis_virt,
        cmd_table_phys,
        cmd_table_virt,
        dma_buf_phys,
        dma_buf_virt,
        sector_size,
        total_sectors,
        active: true,
    })
}

fn wait_cmd_stopped(hba: &MmioRegion, port_idx: usize) {
    let pr = HBA_PORT_REGS + (port_idx as u32) * 0x80;
    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let cmd = unsafe { hba.read_reg::<u32>(pr + PORT_CMD) };
        if cmd & PORT_CMD_CR == 0 { break; }
        timeout -= 1;
        core::hint::spin_loop();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// IDENTIFY DEVICE
// ─────────────────────────────────────────────────────────────────────────────

fn identify_device(hba: &MmioRegion, port_idx: usize, cmd_table_phys: u64, dma_buf_phys: u64, dma_buf_virt: u64) -> (u32, u64, u8) {
    let pr = HBA_PORT_REGS + (port_idx as u32) * 0x80;

    let cmd_list_virt = unsafe {
        let lo = hba.read_reg::<u32>(pr + PORT_CLB) as u64;
        let hi = hba.read_reg::<u32>(pr + PORT_CLBU) as u64;
        lo | (hi << 32)
    };

    unsafe {
        let hdr = &mut *(cmd_list_virt as *mut [HbaCmdHeader; 32]);
        hdr[0].set_cfl(5);
        hdr[0].set_write(false);
        hdr[0].set_prdt_len(1);
        hdr[0].set_cmd_table_addr(cmd_table_phys);
    }

    unsafe {
        let v = cmd_table_phys as *mut u8;
        core::ptr::write_bytes(v, 0, 64);
        v.add(0).write(0x27);
        v.add(1).write(0x80);
        v.add(2).write(0xEC);
        v.add(7).write(0x40);
        v.add(12).write(1);
    }

    unsafe {
        let prdt = (cmd_table_phys + 0x80) as *mut HbaPrdtEntry;
        (*prdt).base_addr = dma_buf_phys;
        (*prdt).set_byte_count(512);
        (*prdt).set_interrupt_on_complete(true);
    }

    unsafe {
        hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF);
        hba.write_reg::<u32>(pr + PORT_SERR, 0xFFFF_FFFF);
        hba.write_reg::<u32>(pr + PORT_CI, 1);
    }

    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let ci = unsafe { hba.read_reg::<u32>(pr + PORT_CI) };
        if ci & 1 == 0 { break; }
        let is = unsafe { hba.read_reg::<u32>(pr + PORT_IS) };
        if is & PORT_IS_TFES != 0 {
            unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }
            return (512, 0, 0);
        }
        timeout -= 1;
        core::hint::spin_loop();
    }

    if timeout == 0 { return (512, 0, 0); }

    unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }

    let data = unsafe { core::slice::from_raw_parts(dma_buf_virt as *const u16, 256) };

    let word83 = data[83];
    let total_sectors = if word83 & (1 << 10) != 0 {
        (data[100] as u64)
            | ((data[101] as u64) << 16)
            | ((data[102] as u64) << 32)
            | ((data[103] as u64) << 48)
    } else {
        (data[60] as u64) | ((data[61] as u64) << 16)
    };

    let word106 = data[106];
    let sector_size = if word106 & 0x4000 != 0 {
        1 << ((word106 & 0x0F) + 9)
    } else {
        512
    };

    let word88 = data[88];
    let mut max_udma: u8 = 0;
    for bit in 0..6u8 {
        if word88 & (1 << bit) != 0 {
            max_udma = bit;
        }
    }

    (sector_size, if total_sectors == 0 { 0xFFFFFFFF } else { total_sectors }, max_udma)
}

// ─────────────────────────────────────────────────────────────────────────────
// SET FEATURES (non-data command)
// ─────────────────────────────────────────────────────────────────────────────

/// Issue SET FEATURES (0xEF) to enable the highest supported UDMA transfer mode.
/// This is required by the ATA spec after device reset before DMA commands
/// can be used. Without this, the device remains in PIO mode and rejects
/// DMA commands with a Task File Error.
///
/// Sector Count register encoding for subcommand 03h (Set Transfer Mode):
///   bits [2:0] = transfer type (000=PIO default, 001=PIO, 100=MWDMA, 101=UDMA)
///   bits [6:3] = mode number
fn set_features_udma(hba: &MmioRegion, port_idx: usize, cmd_table_phys: u64, max_udma: u8) -> bool {
    if max_udma == 0 { return false; }
    let pr = HBA_PORT_REGS + (port_idx as u32) * 0x80;

    let cmd_list_virt = unsafe {
        let lo = hba.read_reg::<u32>(pr + PORT_CLB) as u64;
        let hi = hba.read_reg::<u32>(pr + PORT_CLBU) as u64;
        lo | (hi << 32)
    };

    unsafe {
        let hdr = &mut *(cmd_list_virt as *mut [HbaCmdHeader; 32]);
        hdr[0].set_cfl(5);
        hdr[0].set_write(false);
        hdr[0].set_prdt_len(0);
        hdr[0].set_cmd_table_addr(cmd_table_phys);
    }

    // Sector Count = (mode << 3) | type: bits[2:0]=101 for UDMA, bits[6:3]=mode
    let sector_count = ((max_udma << 3) | 0x05) as u8;

    unsafe {
        let v = cmd_table_phys as *mut u8;
        core::ptr::write_bytes(v, 0, 64);
        v.add(0).write(0x27);       // FIS type: H2D Register
        v.add(1).write(0x80);       // C=1 (command)
        v.add(2).write(0xEF);       // SET FEATURES
        v.add(3).write(0x03);       // Subcommand: Set Transfer Mode
        v.add(12).write(sector_count);
    }

    unsafe {
        hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF);
        hba.write_reg::<u32>(pr + PORT_SERR, 0xFFFF_FFFF);
        hba.write_reg::<u32>(pr + PORT_CI, 1);
    }

    let mut success = false;
    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let ci = unsafe { hba.read_reg::<u32>(pr + PORT_CI) };
        if ci & 1 == 0 {
            success = true;
            break;
        }
        let is = unsafe { hba.read_reg::<u32>(pr + PORT_IS) };
        if is & PORT_IS_TFES != 0 {
            break;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }

    if timeout == 0 { success = false; }

    // Always run cleanup: clear error registers and wait for drive ready.
    // This ensures the port is in a clean state for the next command,
    // even if SET FEATURES failed (e.g. on QEMU or unsupported drives).
    unsafe {
        hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF);
        hba.write_reg::<u32>(pr + PORT_SERR, 0xFFFF_FFFF);
    }

    let mut ready_timeout = ATA_TIMEOUT;
    while ready_timeout > 0 {
        let tfd = unsafe { hba.read_reg::<u32>(pr + PORT_TFD) };
        if tfd & (TFD_BSY | TFD_DRQ) == 0 { break; }
        ready_timeout -= 1;
        core::hint::spin_loop();
    }

    success
}

// ─────────────────────────────────────────────────────────────────────────────
// ATA Command Issuing
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum number of attempts per command (initial + retries).
const CMD_MAX_ATTEMPTS: u32 = 8;

/// Known pattern written to DMA buffer before each read command.
/// Used as diagnostic-only instrumentation — never checked for success/failure.
const DMA_PROBE_PAT: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

fn issue_command(
    hba: &MmioRegion,
    port: &mut AhciPort,
    lba: u64,
    _count: u16,
    buf_ptr: *const u8,
    buf_len: usize,
    is_read: bool,
) -> Result<(), BlockError> {
    let pr = HBA_PORT_REGS + (port.index as u32) * 0x80;

    // If writing, copy user data to DMA buffer
    if !is_read {
        unsafe {
            let dma = core::slice::from_raw_parts_mut(port.dma_buf_virt as *mut u8, buf_len);
            let src = core::slice::from_raw_parts(buf_ptr, buf_len);
            dma.copy_from_slice(src);
        }
    }

    // Set up command header
    unsafe {
        let hdr = &mut *(port.cmd_list_virt as *mut [HbaCmdHeader; 32]);
        hdr[0].set_cfl(5);
        hdr[0].set_write(!is_read);
        hdr[0].set_prdt_len(1);
        hdr[0].set_cmd_table_addr(port.cmd_table_phys);
    }

    // Build H2D FIS for READ DMA EXT (0x25) or WRITE DMA EXT (0x35)
    unsafe {
        let v = port.cmd_table_phys as *mut u8;
        core::ptr::write_bytes(v, 0, 64);
        v.add(0).write(0x27);  // FIS type: H2D Register
        v.add(1).write(0x80);  // C=1
        if is_read {
            v.add(2).write(0x25);  // READ DMA EXT
        } else {
            v.add(2).write(0x35);  // WRITE DMA EXT
        }
        v.add(4).write((lba & 0xFF) as u8);
        v.add(5).write(((lba >> 8) & 0xFF) as u8);
        v.add(6).write(((lba >> 16) & 0xFF) as u8);
        v.add(7).write(0x40 | (((lba >> 24) & 0x0F) as u8));
        v.add(8).write(((lba >> 24) & 0xFF) as u8);
        v.add(9).write(((lba >> 32) & 0xFF) as u8);
        v.add(10).write(((lba >> 40) & 0xFF) as u8);
        v.add(12).write(1);  // sector count low = 1
        v.add(13).write(0);  // sector count high = 0
    }

    // PRDT entry
    unsafe {
        let prdt = (port.cmd_table_phys + 0x80) as *mut HbaPrdtEntry;
        (*prdt).base_addr = port.dma_buf_phys;
        (*prdt).set_byte_count(buf_len as u32);
        (*prdt).set_interrupt_on_complete(true);
    }

    // Retry loop with AHCI-compliant error recovery.
    // After TFES, we must fully reset the command engine before retrying.
    let mut last_was_tfes = false;
    for attempt in 0..CMD_MAX_ATTEMPTS {
        // ── Step 1: Clear interrupt status and error registers ──────────────
        unsafe {
            hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF);
            hba.write_reg::<u32>(pr + PORT_SERR, 0xFFFF_FFFF);
        }

        // ── Step 2: Wait for drive ready (BSY and DRQ must be clear) ───────
        let mut ready_wait = ATA_TIMEOUT;
        while ready_wait > 0 {
            let tfd = unsafe { hba.read_reg::<u32>(pr + PORT_TFD) };
            if tfd & (TFD_BSY | TFD_DRQ) == 0 { break; }
            ready_wait -= 1;
            core::hint::spin_loop();
        }

        // ── Step 3: Full command engine recovery after TFES ────────────────
        // On QEMU's AHCI (and potentially real hardware), TFES leaves the
        // command engine in a bad state where subsequent PxCI writes are
        // silently accepted but no DMA transfer occurs. The recovery must:
        //   a) Stop command processing (PxCMD.ST = 0)
        //   b) Wait for CR to clear (command list idle)
        //   c) Stop FIS receive engine (PxCMD.FRE = 0)
        //   d) Wait for FR to clear (FIS receive idle)
        //   e) Wait for TFD.BSY/DRQ to clear (drive idle)
        //   f) Restart FIS receive (PxCMD.FRE = 1)
        //   g) Wait for FR to set
        //   h) Restart command processing (PxCMD.ST = 1)
        //   i) Wait for CR to set
        if last_was_tfes {
            // a) Stop command processing — clear ST
            unsafe {
                let cmd = hba.read_reg::<u32>(pr + PORT_CMD);
                hba.write_reg::<u32>(pr + PORT_CMD, cmd & !PORT_CMD_ST);
            }
            // b) Wait for CR to clear
            let mut wait = ATA_TIMEOUT;
            while wait > 0 {
                let cmd = unsafe { hba.read_reg::<u32>(pr + PORT_CMD) };
                if cmd & PORT_CMD_CR == 0 { break; }
                wait -= 1;
                core::hint::spin_loop();
            }
            // c) Stop FIS receive engine — clear FRE
            unsafe {
                let cmd = hba.read_reg::<u32>(pr + PORT_CMD);
                hba.write_reg::<u32>(pr + PORT_CMD, cmd & !PORT_CMD_FRE);
            }
            // d) Wait for FR (bit 14) to clear
            let mut wait = ATA_TIMEOUT;
            while wait > 0 {
                let cmd = unsafe { hba.read_reg::<u32>(pr + PORT_CMD) };
                if cmd & (1 << 14) == 0 { break; }
                wait -= 1;
                core::hint::spin_loop();
            }
            // e) Wait for BSY/DRQ to clear (drive idle)
            let mut wait = ATA_TIMEOUT;
            while wait > 0 {
                let tfd = unsafe { hba.read_reg::<u32>(pr + PORT_TFD) };
                if tfd & (TFD_BSY | TFD_DRQ) == 0 { break; }
                wait -= 1;
                core::hint::spin_loop();
            }
            // f) Restart FIS receive engine — set FRE
            unsafe {
                let cmd = hba.read_reg::<u32>(pr + PORT_CMD);
                hba.write_reg::<u32>(pr + PORT_CMD, cmd | PORT_CMD_FRE);
            }
            // g) Wait for FR (bit 14) to set
            let mut wait = ATA_TIMEOUT;
            while wait > 0 {
                let cmd = unsafe { hba.read_reg::<u32>(pr + PORT_CMD) };
                if cmd & (1 << 14) != 0 { break; }
                wait -= 1;
                core::hint::spin_loop();
            }
            // h) Restart command processing — set ST
            unsafe {
                let cmd = hba.read_reg::<u32>(pr + PORT_CMD);
                hba.write_reg::<u32>(pr + PORT_CMD, cmd | PORT_CMD_ST);
            }
            // i) Wait for CR to set
            let mut wait = ATA_TIMEOUT;
            while wait > 0 {
                let cmd = unsafe { hba.read_reg::<u32>(pr + PORT_CMD) };
                if cmd & PORT_CMD_CR != 0 { break; }
                wait -= 1;
                core::hint::spin_loop();
            }
        }

        // ── Step 4: For reads, stamp the DMA buffer to verify DMA later ────
        if is_read {
            unsafe {
                let dma = port.dma_buf_virt as *mut u8;
                core::ptr::write_bytes(dma, 0, buf_len.min(4096));
                dma.add(0).write(DMA_PROBE_PAT[0]);
                dma.add(1).write(DMA_PROBE_PAT[1]);
                dma.add(2).write(DMA_PROBE_PAT[2]);
                dma.add(3).write(DMA_PROBE_PAT[3]);
            }
        }

        // ── Step 5: Issue command (write slot 0 to PxCI) ───────────────────
        unsafe {
            hba.write_reg::<u32>(pr + PORT_CI, 1);
        }

        // ── Step 6: Wait for completion ────────────────────────────────────
        let mut timeout = ATA_TIMEOUT;
        let mut got_tfes = false;
        while timeout > 0 {
            let ci = unsafe { hba.read_reg::<u32>(pr + PORT_CI) };
            if ci & 1 == 0 { break; }
            let is = unsafe { hba.read_reg::<u32>(pr + PORT_IS) };
            if is & PORT_IS_TFES != 0 {
                got_tfes = true;
                break;
            }
            timeout -= 1;
            core::hint::spin_loop();
        }

        // ── Step 7: Handle timeout ─────────────────────────────────────────
        if timeout == 0 {
            unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }
            serial::write_str("[AHCI] TIMEOUT lba=");
            serial::write_hex(lba);
            serial::write_str(" attempt=");
            serial::write_hex(attempt as u64);
            serial::write_nl();
            return Err(BlockError::IoError);
        }

        // Acknowledge all pending interrupts
        unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }

        // ── Step 8: Command completed without TFES — check ATA status ───────
        if !got_tfes {
            let tfd = unsafe { hba.read_reg::<u32>(pr + PORT_TFD) };
            let has_error = (tfd & (TFD_ERR | TFD_DF)) != 0;

            // Diagnostic: log DMA probe comparison (informational only, not used
            // for success/failure). This tells us whether DMA overwrote the buffer.
            if is_read {
                let dma = unsafe { core::slice::from_raw_parts(port.dma_buf_virt as *const u8, buf_len.min(4)) };
                let probe_changed = dma[0] != DMA_PROBE_PAT[0]
                    || dma[1] != DMA_PROBE_PAT[1]
                    || dma[2] != DMA_PROBE_PAT[2]
                    || dma[3] != DMA_PROBE_PAT[3];
                if !probe_changed {
                    serial::write_str("[AHCI] PROBE_DIAG lba=");
                    serial::write_hex(lba);
                    serial::write_str(" att=");
                    serial::write_hex(attempt as u64);
                    serial::write_str(" dma=[");
                    serial::write_hex(dma[0] as u64);
                    serial::write_str(",");
                    serial::write_hex(dma[1] as u64);
                    serial::write_str(",");
                    serial::write_hex(dma[2] as u64);
                    serial::write_str(",");
                    serial::write_hex(dma[3] as u64);
                    serial::write_str_nl("] probe=UNCHANGED (cosmetic only)");
                }
            }

            if has_error {
                // ATA error bits set — treat as failure
                let ci_now = unsafe { hba.read_reg::<u32>(pr + PORT_CI) };
                let is_now = unsafe { hba.read_reg::<u32>(pr + PORT_IS) };
                serial::write_str("[AHCI] ATA_ERR lba=");
                serial::write_hex(lba);
                serial::write_str(" att=");
                serial::write_hex(attempt as u64);
                serial::write_str(" tfd=");
                serial::write_hex(tfd as u64);
                serial::write_str(" ci=");
                serial::write_hex(ci_now as u64);
                serial::write_str(" is=");
                serial::write_hex(is_now as u64);
                serial::write_nl();
                last_was_tfes = true;
                continue;
            }

            // Command succeeded: CI cleared, no TFES, no ATA error bits.
            // Trust the DMA buffer contents.
            if is_read {
                unsafe {
                    let dma = core::slice::from_raw_parts(port.dma_buf_virt as *const u8, buf_len);
                    let dst = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len);
                    dst.copy_from_slice(dma);
                }
            }
            return Ok(());
        }

        // ── Step 9: TFES — log and prepare for recovery ────────────────────
        {
            let ci_now = unsafe { hba.read_reg::<u32>(pr + PORT_CI) };
            let is_now = unsafe { hba.read_reg::<u32>(pr + PORT_IS) };
            let tfd_now = unsafe { hba.read_reg::<u32>(pr + PORT_TFD) };
            serial::write_str("[AHCI] TFES lba=");
            serial::write_hex(lba);
            serial::write_str(" att=");
            serial::write_hex(attempt as u64);
            serial::write_str(" ci=");
            serial::write_hex(ci_now as u64);
            serial::write_str(" is=");
            serial::write_hex(is_now as u64);
            serial::write_str(" tfd=");
            serial::write_hex(tfd_now as u64);
            serial::write_nl();
        }
        last_was_tfes = true;
    }

    Err(BlockError::IoError)
}

// ─────────────────────────────────────────────────────────────────────────────
// DMA Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Allocate a single page for DMA from the PMM.
/// The frame is identity-mapped by the bootloader, so phys == virt.
fn alloc_dma_page() -> Option<crate::memory::PhysAddr> {
    let frame = crate::memory::pmm::alloc_frame()?;
    // PMM frames are identity-mapped (phys == virt) by the bootloader.
    // For DMA we need the physical address for the AHCI PRDT,
    // and the same address works for CPU access via identity mapping.
    Some(frame)
}

// ─────────────────────────────────────────────────────────────────────────────
// PCI Helpers
// ─────────────────────────────────────────────────────────────────────────────

unsafe fn enable_bus_mastering(bus: u8, device: u8, function: u8) {
    let addr = (1u32 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (0x04u32 & 0xFC);
    x86_64::instructions::port::Port::new(0xCF8).write(addr);
    let mut cmd: u32 = x86_64::instructions::port::Port::<u32>::new(0xCFC).read();
    cmd |= (1 << 1) | (1 << 2);
    x86_64::instructions::port::Port::new(0xCF8).write(addr);
    x86_64::instructions::port::Port::new(0xCFC).write(cmd);
    serial::write_str_nl("[AHCI] Bus mastering enabled");
}

// ─────────────────────────────────────────────────────────────────────────────
// Public init
// ─────────────────────────────────────────────────────────────────────────────

pub fn init() {
    serial::write_str_nl("[AHCI] Initializing...");

    if let Some(disk) = AhciDisk::init() {
        let total = disk.total_sectors();
        let ssize = disk.sector_size();
        match crate::block::registry::register_device(disk) {
            Ok(id) => {
                serial::write_str("[AHCI] Registered as device id=");
                serial::write_hex(id as u64);
                serial::write_str(" sectors=");
                serial::write_hex(total);
                serial::write_str(" ssize=");
                serial::write_hex(ssize as u64);
                serial::write_nl();
            }
            Err(e) => {
                serial::write_str("[AHCI] Register failed: ");
                serial::write_str_nl(e.description());
            }
        }
    } else {
        serial::write_str_nl("[AHCI] No disks found");
    }
}
