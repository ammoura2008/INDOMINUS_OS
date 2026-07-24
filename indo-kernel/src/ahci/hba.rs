//! # AHCI HBA (Host Bus Adapter) Register Definitions
//!
//! Defines the memory-mapped register layout for AHCI controllers.
//! Based on the AHCI specification revision 1.3.1.

// ─────────────────────────────────────────────────────────────────────────────
// HBA Global Registers
// ─────────────────────────────────────────────────────────────────────────────

/// Host Capabilities (RO)
pub const HBA_CAP: u32 = 0x00;

/// Global HBA Control
pub const HBA_GHC: u32 = 0x04;

/// Global HBA Control: HBA Reset
pub const GHC_HR: u32 = 1 << 0;

/// Global HBA Control: AHCI Enable
pub const GHC_AE: u32 = 1 << 31;

/// Interrupt Status (RO)
pub const HBA_IS: u32 = 0x08;

/// Ports Implemented (RO, bitmap of implemented ports)
pub const HBA_PI: u32 = 0x0C;

/// Version
pub const HBA_VS: u32 = 0x10;

// ─────────────────────────────────────────────────────────────────────────────
// Port Registers (base + 0x100 + port * 0x80)
// ─────────────────────────────────────────────────────────────────────────────

/// Port register block base offset
pub const HBA_PORT_REGS: u32 = 0x100;

/// Port Command List Base Address (low 32 bits)
pub const PORT_CLB: u32 = 0x00;

/// Port Command List Base Address (high 32 bits)
pub const PORT_CLBU: u32 = 0x04;

/// Port FIS Base Address (low 32 bits)
pub const PORT_FB: u32 = 0x08;

/// Port FIS Base Address (high 32 bits)
pub const PORT_FBU: u32 = 0x0C;

/// Port Interrupt Status
pub const PORT_IS: u32 = 0x10;

/// Port Interrupt Status: Task File Device Error
pub const PORT_IS_TFES: u32 = 1 << 0;

/// Port Interrupt Enable
pub const PORT_IE: u32 = 0x14;

/// Port Command and Status
pub const PORT_CMD: u32 = 0x18;

/// Port Command: Start
pub const PORT_CMD_ST: u32 = 1 << 0;

/// Port Command: Command List Running
pub const PORT_CMD_CR: u32 = 1 << 15;

/// Port Command: Fris Receive Enable
pub const PORT_CMD_FRE: u32 = 1 << 4;

/// Port Task File Data
pub const PORT_TFD: u32 = 0x20;

/// Task File Data: BSY (busy)
pub const TFD_BSY: u32 = 1 << 7;

/// Task File Data: DRQ (data request)
pub const TFD_DRQ: u32 = 1 << 3;

/// Port Signature
pub const PORT_SIG: u32 = 0x24;

/// Port Serial ATA Status (SCR: Status)
pub const PORT_SSTS: u32 = 0x28;

/// Port Serial ATA Control (SCR: Control)
pub const PORT_SCTL: u32 = 0x2C;

/// Port Serial ATA Error (SCR: Error)
pub const PORT_SERR: u32 = 0x30;

/// Port Serial ATA Active (SCR: Active)
pub const PORT_SACT: u32 = 0x34;

/// Port Command Issue
pub const PORT_CI: u32 = 0x38;

/// Port Serial ATA Notification (SCR: Notification)
pub const PORT_SNTF: u32 = 0x3C;

// ─────────────────────────────────────────────────────────────────────────────
// FIS Types
// ─────────────────────────────────────────────────────────────────────────────

/// FIS type: Host-to-Device Register
pub const FIS_TYPE_H2D: u8 = 0x27;

/// FIS type: Device-to-Host Register
pub const FIS_TYPE_D2H: u8 = 0x34;

/// FIS type: DMA Setup
pub const FIS_TYPE_DMA_SETUP: u8 = 0x41;

/// FIS type: Data
pub const FIS_TYPE_DATA: u8 = 0x46;

// ─────────────────────────────────────────────────────────────────────────────
// DMA Structures
// ─────────────────────────────────────────────────────────────────────────────

/// AHCI Command Header (32 bytes, 1K-aligned per port)
#[repr(C)]
pub struct HbaCmdHeader {
    /// DW0: Command FIS length (bits 0-4), ATAPI (bit 5), write (bit 6),
    /// prefetchable (bit 7), reserved (bits 8-15), PRDT length (bits 16-31)
    pub opts: u32,
    /// DW1: Physical region descriptor byte count (total bytes transferred)
    pub byte_count: u32,
    /// DW2: Command table base address (low 32 bits, bits 2-31)
    pub cmd_table_base_lo: u32,
    /// DW3: Command table base address (high 32 bits)
    pub cmd_table_base_hi: u32,
    /// DW4-7: Reserved
    _reserved: [u32; 4],
}

impl HbaCmdHeader {
    /// Set command FIS length in dwords (bits 0-4 of opts).
    pub fn set_cfl(&mut self, dwords: u32) {
        self.opts = (self.opts & !0x1F) | (dwords & 0x1F);
    }
    /// Set write bit (bit 6 of opts).
    pub fn set_write(&mut self, write: bool) {
        if write {
            self.opts |= 1 << 6;
        } else {
            self.opts &= !(1 << 6);
        }
    }
    /// Set PRDT length in entries (bits 16-31 of opts).
    pub fn set_prdt_len(&mut self, len: u16) {
        self.opts = (self.opts & 0x0000_FFFF) | ((len as u32) << 16);
    }
    /// Set command table base address from a physical address.
    pub fn set_cmd_table_addr(&mut self, phys: u64) {
        self.cmd_table_base_lo = phys as u32;
        self.cmd_table_base_hi = (phys >> 32) as u32;
    }
}

/// Physical Region Descriptor Table Entry (16 bytes)
#[repr(C)]
pub struct HbaPrdtEntry {
    /// Physical address of data buffer
    pub base_addr: u64,
    /// Reserved
    _reserved: u32,
    /// Byte count (0-based) + interrupt on complete (bit 31)
    pub byte_count: u32,
}

impl HbaPrdtEntry {
    /// Set byte count (0-based, bits 0-30) and optional interrupt-on-complete (bit 31).
    pub fn set_byte_count(&mut self, count: u32) {
        self.byte_count = (self.byte_count & 0x8000_0000) | ((count - 1) & 0x7FFF_FFFF);
    }
    /// Set or clear the interrupt-on-complete flag (bit 31).
    pub fn set_interrupt_on_complete(&mut self, ioc: bool) {
        if ioc {
            self.byte_count |= 1 << 31;
        } else {
            self.byte_count &= !(1u32 << 31);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ATA Commands
// ─────────────────────────────────────────────────────────────────────────────

/// ATA Command: IDENTIFY DEVICE
pub const ATA_CMD_IDENTIFY_DEVICE: u8 = 0xEC;

/// ATA Command: READ DMA EXT (LBA48)
pub const ATA_CMD_READ_DMA_EXT: u8 = 0x25;

/// ATA Command: WRITE DMA EXT (LBA48)
pub const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
