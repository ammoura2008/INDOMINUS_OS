//! # Serial UART 16550 Driver
//!
//! Direct port I/O — no Mutex, no statics.
//! Safe to call before VMM initialization (works on the UEFI identity map).

const COM1_BASE: u16 = 0x3F8;

const UART_DATA:        u16 = COM1_BASE + 0;
const UART_INT_ENABLE:  u16 = COM1_BASE + 1;
const UART_LINE_STATUS: u16 = COM1_BASE + 5;

const LSR_THRE: u8 = 1 << 5;
const LSR_DATA_READY: u8 = 1 << 0;

/// Enable UART RX interrupts (IRQ4).
///
/// Sets bit 0 of the Interrupt Enable Register (IER) on COM1.
/// This causes the UART to fire IRQ4 whenever a byte is received.
pub fn enable_rx_interrupt() {
    unsafe {
        outb(UART_INT_ENABLE, 0x01); // Bit 0: RX data ready interrupt
    }
    crate::serial::write_str("[UART] RX interrupts enabled\n");
}

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
///
/// Port 0xE9 is QEMU's debug output — any byte written here appears on
/// stderr immediately, independent of UART state. Works from Ring 0
/// without any driver initialization.
///
/// Compiled to a no-op when `DEBUG_KERNEL` is not set.
#[cfg(DEBUG_KERNEL)]
pub fn ddbg(marker: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0xE9u16, in("al") marker, options(nostack, nomem));
    }
}

#[cfg(not(DEBUG_KERNEL))]
#[inline(always)]
pub fn ddbg(_marker: u8) {}

/// COM1 IRQ4 handler — reads received bytes and feeds them into the
/// keyboard line discipline so they appear on stdin (sys_read).
pub fn serial_rx_handler() {
    unsafe {
        let lsr = inb(UART_LINE_STATUS);
        if lsr & LSR_DATA_READY != 0 {
            let byte = inb(UART_DATA);
            crate::keyboard::line_discipline_input(byte);
        }
    }
}

/// Initialize COM1 serial input: enable RX interrupt and register IRQ4 handler.
pub fn init_rx() {
    crate::serial::write_str("[UART] Initializing serial RX (IRQ4)\n");
    enable_rx_interrupt();
    crate::interrupts::dispatch::register(4, serial_rx_handler);
    crate::serial::write_str("[UART] Serial RX handler registered on IRQ4\n");
}

/// Diagnostic: print [X RAX=0x...] to serial. Called from naked handlers.
/// `label` = 'I' for iretq path, 'S' for syscall entry path.
///
/// Compiled to a no-op when `DEBUG_KERNEL` is not set.
#[cfg(DEBUG_KERNEL)]
#[no_mangle]
pub unsafe extern "C" fn dump_rax(label: u8, rax: u64) {
    write_byte(b'[');
    write_byte(label);
    write_str(" RAX=0x");
    write_hex(rax);
    write_str("]\n");
}

#[cfg(not(DEBUG_KERNEL))]
#[no_mangle]
pub unsafe extern "C" fn dump_rax(_label: u8, _rax: u64) {}

/// Write a marker string to serial. Used from naked_asm.
///
/// Compiled to a no-op when `DEBUG_KERNEL` is not set.
/// # Safety
/// `ptr` must point to a valid buffer of `len` bytes.
#[cfg(DEBUG_KERNEL)]
#[no_mangle]
pub unsafe extern "C" fn write_marker_raw(ptr: *const u8, len: u64) {
    let slice = core::slice::from_raw_parts(ptr, len as usize);
    for &byte in slice {
        wait_for_transmit();
        outb(UART_DATA, byte);
    }
}

#[cfg(not(DEBUG_KERNEL))]
#[no_mangle]
pub unsafe extern "C" fn write_marker_raw(_ptr: *const u8, _len: u64) {}
