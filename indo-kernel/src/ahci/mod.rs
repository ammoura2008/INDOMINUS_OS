//! # AHCI (Advanced Host Controller Interface) Driver
//!
//! Provides block-level storage access via AHCI/SATA.
//! Implements the `BlockDevice` trait for integration with the VFS layer.

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

    // Clear interrupt status
    unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }

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
    let (sector_size, total_sectors) = identify_device(hba, port_idx, cmd_table_phys, dma_buf_phys, dma_buf_virt);

    if total_sectors == 0 {
        return Err("no sectors detected");
    }

    serial::write_str("[AHCI] Port ");
    serial::write_hex(port_idx as u64);
    serial::write_str(" sectors=");
    serial::write_hex(total_sectors);
    serial::write_str(" ssize=");
    serial::write_hex(sector_size as u64);
    serial::write_nl();

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

fn identify_device(hba: &MmioRegion, port_idx: usize, cmd_table_phys: u64, dma_buf_phys: u64, dma_buf_virt: u64) -> (u32, u64) {
    let pr = HBA_PORT_REGS + (port_idx as u32) * 0x80;

    // Set up command header 0
    let cmd_list_virt = unsafe {
        let lo = hba.read_reg::<u32>(pr + PORT_CLB) as u64;
        let hi = hba.read_reg::<u32>(pr + PORT_CLBU) as u64;
        lo | (hi << 32)
    };

    // Command header: FIS-based command, 1 PRD, no write
    unsafe {
        let hdr = &mut *(cmd_list_virt as *mut [HbaCmdHeader; 32]);
        hdr[0].set_cfl(5);
        hdr[0].set_write(false);
        hdr[0].set_prdt_len(1);
        hdr[0].set_cmd_table_addr(cmd_table_phys);
    }

    // CFIS for IDENTIFY DEVICE (0xEC)
    unsafe {
        let cfis = cmd_table_phys as *mut u8;
        let v = cfis as *mut u8;
        core::ptr::write_bytes(v, 0, 64);
        v.add(0).write(0x27);  // FIS type: H2D Register
        v.add(1).write(0x80);  // C=1
        v.add(2).write(0xEC);  // IDENTIFY DEVICE
        v.add(7).write(0x40);  // LBA mode
        v.add(12).write(1);    // Sector count = 1
    }

    // PRDT entry
    unsafe {
        let prdt = (cmd_table_phys + 0x80) as *mut HbaPrdtEntry;
        (*prdt).base_addr = dma_buf_phys;
        (*prdt).set_byte_count(512);
        (*prdt).set_interrupt_on_complete(true);
    }

    // Clear interrupt and issue
    unsafe {
        hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF);
        hba.write_reg::<u32>(pr + PORT_CI, 1);
    }

    // Wait
    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let ci = unsafe { hba.read_reg::<u32>(pr + PORT_CI) };
        if ci & 1 == 0 { break; }
        let is = unsafe { hba.read_reg::<u32>(pr + PORT_IS) };
        if is & PORT_IS_TFES != 0 {
            unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }
            return (512, 0);
        }
        timeout -= 1;
        core::hint::spin_loop();
    }

    if timeout == 0 { return (512, 0); }

    unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }

    // Parse IDENTIFY data
    let data = unsafe { core::slice::from_raw_parts(dma_buf_virt as *const u16, 256) };

    let word83 = data[83];
    let total_sectors = if word83 & (1 << 10) != 0 {
        // LBA48
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

    (sector_size, if total_sectors == 0 { 0xFFFFFFFF } else { total_sectors })
}

// ─────────────────────────────────────────────────────────────────────────────
// ATA Command Issuing
// ─────────────────────────────────────────────────────────────────────────────

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

    // Clear interrupt and check ready
    unsafe {
        hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF);
        let tfd = hba.read_reg::<u32>(pr + PORT_TFD);
        if tfd & TFD_BSY != 0 {
            return Err(BlockError::DeviceNotReady);
        }
        hba.write_reg::<u32>(pr + PORT_CI, 1);
    }

    // Wait for completion
    let mut timeout = ATA_TIMEOUT;
    while timeout > 0 {
        let ci = unsafe { hba.read_reg::<u32>(pr + PORT_CI) };
        if ci & 1 == 0 { break; }
        let is = unsafe { hba.read_reg::<u32>(pr + PORT_IS) };
        if is & PORT_IS_TFES != 0 {
            unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }
            return Err(BlockError::IoError);
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    if timeout == 0 {
        return Err(BlockError::IoError);
    }

    unsafe { hba.write_reg::<u32>(pr + PORT_IS, 0xFFFF_FFFF); }

    // If reading, copy from DMA buffer to user buffer
    if is_read {
        unsafe {
            let dma = core::slice::from_raw_parts(port.dma_buf_virt as *const u8, buf_len);
            let dst = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len);
            dst.copy_from_slice(dma);
        }
    }

    Ok(())
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
