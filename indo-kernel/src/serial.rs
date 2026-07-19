//! # Serial UART 16550 Driver
//!
//! Direct port I/O — no Mutex, no statics.
//! Safe to call before VMM initialization (works on the UEFI identity map).

use core::fmt;

const COM1_BASE: u16 = 0x3F8;

const UART_DATA:        u16 = COM1_BASE + 0;
const UART_INT_ENABLE:  u16 = COM1_BASE + 1;
const UART_FIFO_CTRL:   u16 = COM1_BASE + 2;
const UART_LINE_CTRL:   u16 = COM1_BASE + 3;
const UART_MODEM_CTRL:  u16 = COM1_BASE + 4;
const UART_LINE_STATUS: u16 = COM1_BASE + 5;

const UART_BAUD_LSB:    u16 = COM1_BASE + 0;
const UART_BAUD_MSB:    u16 = COM1_BASE + 1;

const LSR_THRE: u8 = 1 << 5;
const BAUD_DIVISOR_115200: u16 = 1;

#[inline]
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nostack, nomem)
    );
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") value,
        in("dx") port,
        options(nostack, nomem)
    );
    value
}

#[inline]
fn wait_for_transmit() {
    unsafe {
        while (inb(UART_LINE_STATUS) & LSR_THRE) == 0 {
            core::hint::spin_loop();
        }
    }
}

pub fn init() {
    unsafe {
        outb(UART_INT_ENABLE, 0x00);
        outb(UART_LINE_CTRL, 0x80);
        outb(UART_BAUD_LSB, (BAUD_DIVISOR_115200 & 0xFF) as u8);
        outb(UART_BAUD_MSB, ((BAUD_DIVISOR_115200 >> 8) & 0xFF) as u8);
        outb(UART_LINE_CTRL, 0x03);
        outb(UART_FIFO_CTRL, 0xC7);
        outb(UART_MODEM_CTRL, 0x03);
    }
}

#[no_mangle]
pub fn write_byte(byte: u8) {
    wait_for_transmit();
    unsafe {
        outb(UART_DATA, byte);
    }
}

#[inline]
pub fn write_str(s: &str) {
    for byte in s.bytes() {
        write_byte(byte);
    }
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => ({
        use core::fmt::Write;
        struct DirectWriter;
        impl core::fmt::Write for DirectWriter {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                $crate::serial::write_str(s);
                Ok(())
            }
        }
        let mut w = DirectWriter;
        let _ = w.write_fmt(format_args!($($arg)*));
    });
}

#[macro_export]
macro_rules! kprintln {
    ()              => ($crate::kprint!("\r\n"));
    ($($arg:tt)*)  => ({
        $crate::kprint!($($arg)*);
        $crate::kprint!("\r\n");
    });
}

#[no_mangle]
pub fn write_hex(value: u64) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    write_byte(b'0');
    write_byte(b'x');
    for i in (0..=60).rev().step_by(4) {
        write_byte(HEX[(value >> i) as usize & 0xF]);
    }
}

pub fn write_u64(mut value: u64) {
    if value == 0 {
        write_byte(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    while value > 0 {
        i -= 1;
        buf[i] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    for b in &buf[i..] {
        write_byte(*b);
    }
}

pub fn write_nl() {
    write_byte(b'\r');
    write_byte(b'\n');
}

pub fn write_str_nl(s: &str) {
    write_str(s);
    write_nl();
}

/// Debug: write a single marker byte to QEMU debug console (port 0xE9).
pub fn ddbg(_marker: u8) {
    // Placeholder — enable after VMM is up
}

/// Write a marker string to serial. Used from naked_asm.
/// # Safety
/// `ptr` must point to a valid buffer of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn write_marker_raw(ptr: *const u8, len: u64) {
    let slice = core::slice::from_raw_parts(ptr, len as usize);
    for &byte in slice {
        wait_for_transmit();
        outb(UART_DATA, byte);
    }
}
