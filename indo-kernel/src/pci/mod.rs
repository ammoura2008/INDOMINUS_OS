//! # PCI Bus Enumeration
//!
//! Scans the PCI bus to discover devices using the legacy I/O port method
//! (Configuration Space Access via I/O ports 0xCF8/0xCFC).
//!
//! ## PCI Configuration Space
//!
//! Each PCI device has a 256-byte configuration header containing:
//! - Vendor ID (offset 0x00)
//! - Device ID (offset 0x02)
//! - Command (offset 0x04)
//! - Status (offset 0x06)
//! - Revision ID (offset 0x08)
//! - Class Code (offset 0x09)
//! - Subclass (offset 0x0A)
//! - Programming Interface (offset 0x0B)
//! - Header Type (offset 0x0E)
//! - BAR0-BAR5 (offset 0x10-0x24)
//!
//! ## I/O Port Access
//!
//! - `0xCF8` (CONFIG_ADDRESS): 32-bit register
//!   - Bit 31: Enable
//!   - Bits 23-16: Bus number
//!   - Bits 15-11: Device number
//!   - Bits 10-8: Function number
//!   - Bits 7-0: Register offset (must be DWORD-aligned)
//! - `0xCFC` (CONFIG_DATA): 32-bit data port

extern crate alloc;
use alloc::vec::Vec;
use spin::Mutex;

/// PCI I/O ports
const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// PCI configuration register offsets
const VENDOR_ID: u8 = 0x00;
const DEVICE_ID: u8 = 0x02;
const COMMAND: u8 = 0x04;
const REVISION_ID: u8 = 0x08;
const PROG_IF: u8 = 0x09;
const SUBCLASS: u8 = 0x0A;
const CLASS_CODE: u8 = 0x0B;
const HEADER_TYPE: u8 = 0x0E;
const BAR0: u8 = 0x10;
const BAR1: u8 = 0x14;
const BAR2: u8 = 0x18;
const BAR3: u8 = 0x1C;
const BAR4: u8 = 0x20;
const BAR5: u8 = 0x24;
const SUBSYSTEM_VENDOR_ID: u8 = 0x2C;
const SUBSYSTEM_ID: u8 = 0x2E;
const INTERRUPT_LINE: u8 = 0x3C;
const INTERRUPT_PIN: u8 = 0x3D;

/// A discovered PCI device
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub header_type: u8,
    pub bars: [u32; 6],
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
}

impl PciDevice {
    /// Returns true if this is a valid device (vendor_id != 0xFFFF)
    pub fn is_valid(&self) -> bool {
        self.vendor_id != 0xFFFF
    }

    /// Returns true if this is a multi-function device
    pub fn is_multi_function(&self) -> bool {
        self.header_type & 0x80 != 0
    }

    /// Returns the class name as a string
    pub fn class_name(&self) -> &'static str {
        match self.class {
            0x00 => "Legacy Device",
            0x01 => "Mass Storage Controller",
            0x02 => "Network Controller",
            0x03 => "Display Controller",
            0x04 => "Multimedia Controller",
            0x05 => "Memory Controller",
            0x06 => "Bridge Device",
            0x07 => "Communication Controller",
            0x08 => "System Peripheral",
            0x09 => "Input Device",
            0x0A => "Docking Station",
            0x0B => "Processor",
            0x0C => "Serial Bus Controller",
            0x0D => "Wireless Controller",
            0x0E => "Intelligent Controller",
            0x0F => "Satellite Communication",
            0x10 => "Encryption Controller",
            0x11 => "Signal Processing Controller",
            0xFF => "Other",
            _ => "Unknown",
        }
    }

    /// Returns the BAR type (memory or I/O)
    pub fn bar_type(&self, bar_index: usize) -> &'static str {
        if bar_index < 6 {
            if self.bars[bar_index] & 1 != 0 { "I/O" } else { "Memory" }
        } else {
            "Invalid"
        }
    }

    /// Returns the BAR address (masked for type)
    pub fn bar_address(&self, bar_index: usize) -> u64 {
        if bar_index < 6 {
            let bar = self.bars[bar_index];
            if bar & 1 != 0 {
                // I/O BAR: bits 2-31
                (bar & 0xFFFFFFFC) as u64
            } else {
                // Memory BAR: bits 4-31
                (bar & 0xFFFFFFF0) as u64
            }
        } else {
            0
        }
    }
}

/// Global PCI device list
pub static PCI_DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());

/// Build the CONFIG_ADDRESS value for a given bus/device/function/register
fn config_address(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    (1 << 31)                        // Enable bit
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((register as u32) & 0xFC)  // Register must be DWORD-aligned
}

/// Read a 32-bit value from PCI configuration space
unsafe fn pci_read_dword(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    let addr = config_address(bus, device, function, register);
    x86_64::instructions::port::Port::new(CONFIG_ADDRESS).write(addr);
    x86_64::instructions::port::Port::<u32>::new(CONFIG_DATA).read()
}

/// Read a 16-bit value from PCI configuration space
unsafe fn pci_read_word(bus: u8, device: u8, function: u8, register: u8) -> u16 {
    let dword = pci_read_dword(bus, device, function, register);
    let offset = (register & 2) * 8; // 0 or 16 bits
    ((dword >> offset) & 0xFFFF) as u16
}

/// Read an 8-bit value from PCI configuration space
unsafe fn pci_read_byte(bus: u8, device: u8, function: u8, register: u8) -> u8 {
    let dword = pci_read_dword(bus, device, function, register);
    let offset = (register & 3) * 8; // 0, 8, 16, or 24 bits
    ((dword >> offset) & 0xFF) as u8
}

/// Write a 32-bit value to PCI configuration space
unsafe fn pci_write_dword(bus: u8, device: u8, function: u8, register: u8, value: u32) {
    let addr = config_address(bus, device, function, register);
    x86_64::instructions::port::Port::new(CONFIG_ADDRESS).write(addr);
    x86_64::instructions::port::Port::new(CONFIG_DATA).write(value);
}

/// Read the full configuration header for a PCI device
fn read_device(bus: u8, device: u8, function: u8) -> Option<PciDevice> {
    unsafe {
        let vendor_id = pci_read_word(bus, device, function, VENDOR_ID);
        if vendor_id == 0xFFFF {
            return None; // No device at this slot
        }

        let device_id = pci_read_word(bus, device, function, DEVICE_ID);
        let revision = pci_read_byte(bus, device, function, REVISION_ID);
        let prog_if = pci_read_byte(bus, device, function, PROG_IF);
        let subclass = pci_read_byte(bus, device, function, SUBCLASS);
        let class = pci_read_byte(bus, device, function, CLASS_CODE);
        let header_type = pci_read_byte(bus, device, function, HEADER_TYPE);

        let mut bars = [0u32; 6];
        bars[0] = pci_read_dword(bus, device, function, BAR0);
        bars[1] = pci_read_dword(bus, device, function, BAR1);
        bars[2] = pci_read_dword(bus, device, function, BAR2);
        bars[3] = pci_read_dword(bus, device, function, BAR3);
        bars[4] = pci_read_dword(bus, device, function, BAR4);
        bars[5] = pci_read_dword(bus, device, function, BAR5);

        let interrupt_line = pci_read_byte(bus, device, function, INTERRUPT_LINE);
        let interrupt_pin = pci_read_byte(bus, device, function, INTERRUPT_PIN);
        let subsystem_vendor_id = pci_read_word(bus, device, function, SUBSYSTEM_VENDOR_ID);
        let subsystem_id = pci_read_word(bus, device, function, SUBSYSTEM_ID);

        Some(PciDevice {
            bus,
            device,
            function,
            vendor_id,
            device_id,
            class,
            subclass,
            prog_if,
            revision,
            header_type,
            bars,
            interrupt_line,
            interrupt_pin,
            subsystem_vendor_id,
            subsystem_id,
        })
    }
}

/// Enumerate all PCI devices on the bus
pub fn enumerate() {
    crate::serial::write_str_nl("[PCI] Enumerating PCI bus...");

    let mut devices = PCI_DEVICES.lock();
    devices.clear();

    for bus in 0..=255u16 {
        for device in 0..32u8 {
            // Try function 0 first
            if let Some(dev) = read_device(bus as u8, device, 0) {
                if dev.is_multi_function() {
                    // Multi-function device: scan functions 1-7
                    for function in 1..8u8 {
                        if let Some(func_dev) = read_device(bus as u8, device, function) {
                            devices.push(func_dev);
                        }
                    }
                }
                devices.push(dev);
            }
        }
    }

    crate::serial::write_str("[PCI] Found ");
    crate::serial::write_hex(devices.len() as u64);
    crate::serial::write_str_nl(" devices");

    // Print device summary
    for dev in devices.iter() {
        crate::serial::write_str("[PCI]   ");
        crate::serial::write_hex(dev.bus as u64);
        crate::serial::write_str(":");
        crate::serial::write_hex(dev.device as u64);
        crate::serial::write_str(".");
        crate::serial::write_hex(dev.function as u64);
        crate::serial::write_str(" [");
        crate::serial::write_str(dev.class_name());
        crate::serial::write_str("] vendor=0x");
        crate::serial::write_hex(dev.vendor_id as u64);
        crate::serial::write_str(" dev=0x");
        crate::serial::write_hex(dev.device_id as u64);
        if dev.interrupt_pin != 0 {
            crate::serial::write_str(" irq=");
            crate::serial::write_hex(dev.interrupt_line as u64);
        }
        crate::serial::write_nl();
    }
}

