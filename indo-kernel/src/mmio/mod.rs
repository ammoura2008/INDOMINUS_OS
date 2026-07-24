//! # MMIO Framework
//!
//! Provides safe abstractions for Memory-Mapped I/O register access.
//!
//! ## Design
//!
//! All MMIO regions are mapped into the kernel's upper-half virtual address space
//! at a fixed offset from physical: `MMIO_BASE + phys_addr`. This survives CR3
//! switches to user PML4s (which don't have the lower-half identity map).
//!
//! ## Safety
//!
//! MMIO accesses must be volatile (no compiler reordering, no caching).
//! The `MmioReg` type enforces this via `read_volatile`/`write_volatile`.

use core::ptr;

/// Base virtual address for MMIO mappings in the upper half.
/// Physical 0xFEC00000 (IOAPIC) maps to 0xFFFF_FFFF_FEC0_0000.
/// Physical 0xFEE00000 (LAPIC) maps to 0xFFFF_FFFF_FEE0_0000.
/// Physical 0x000FED00 (HPET) maps to 0xFFFF_FFFF_000F_ED00.
///
/// Convention: `mmio_virt = 0xFFFF_FFFF_0000_0000 | phys_addr`
/// This works for all x86 MMIO regions (typically >= 0xFExxxxxx).
const MMIO_UPPER_BASE: u64 = 0xFFFF_FFFF_0000_0000;

/// Convert a physical MMIO address to its kernel virtual address.
#[inline]
pub fn mmio_to_virt(phys: u64) -> u64 {
    MMIO_UPPER_BASE | phys
}

/// Map a physical MMIO page into the kernel's upper-half page tables.
/// If the page is already mapped, this is a no-op.
///
/// # Safety
/// - `phys` must be page-aligned and point to a valid MMIO region.
/// - Caller must ensure the physical page exists and is MMIO (not RAM).
pub unsafe fn map_mmio_page(phys: u64) -> u64 {
    let virt = mmio_to_virt(phys);
    // Check if already mapped (e.g., LAPIC mapped in init_kernel_page_tables)
    if let Some(_existing) = crate::memory::vmm::translate_addr(
        crate::memory::PhysAddr::new(crate::memory::kernel_pml4_phys()),
        x86_64::VirtAddr::new(virt),
    ) {
        return virt; // Already mapped
    }
    let pml4 = crate::memory::kernel_pml4_phys();
    let flags = x86_64::structures::paging::PageTableFlags::PRESENT
        | x86_64::structures::paging::PageTableFlags::WRITABLE
        | x86_64::structures::paging::PageTableFlags::NO_CACHE
        | x86_64::structures::paging::PageTableFlags::WRITE_THROUGH;
    crate::memory::vmm::map_page(
        crate::memory::PhysAddr::new(pml4),
        x86_64::VirtAddr::new(virt),
        crate::memory::PhysAddr::new(phys),
        flags,
    );
    virt
}

/// A typed MMIO register at a given offset from a base address.
///
/// `T` is the register width: `u8`, `u16`, `u32`, or `u64`.
///
/// # Example
/// ```ignore
/// let lapic = MmioRegion::new(0xFEE0_0000);
/// let id = lapic.read_reg::<u32>(0x020);
/// lapic.write_reg::<u32>(0x0B0, 0); // EOI
/// ```
pub struct MmioRegion {
    base: u64,
}

impl MmioRegion {
    /// Create a new MMIO region from a physical base address.
    /// Maps the page into upper-half virtual space and stores the virtual address.
    pub fn new(phys_base: u64) -> Self {
        let page_phys = phys_base & !0xFFF;
        let virt = unsafe { map_mmio_page(page_phys) };
        MmioRegion { base: virt }
    }

    /// Create an MMIO region from a pre-computed virtual address.
    /// Use when the caller has already mapped the page.
    pub unsafe fn from_virt(virt_base: u64) -> Self {
        MmioRegion { base: virt_base }
    }

    /// Get the virtual address of a register at the given byte offset.
    #[inline]
    pub fn reg_addr(&self, offset: u32) -> u64 {
        self.base + offset as u64
    }

    /// Read a volatile value from a register at `offset`.
    ///
    /// # Safety
    /// Caller must ensure `offset` points to a valid readable MMIO register
    /// and `T` matches the register width.
    #[inline]
    pub unsafe fn read_reg<T: MmioReadWrite>(&self, offset: u32) -> T {
        let addr = self.reg_addr(offset);
        ptr::read_volatile(addr as *const T)
    }

    /// Write a volatile value to a register at `offset`.
    ///
    /// # Safety
    /// Caller must ensure `offset` points to a valid writable MMIO register
    /// and `T` matches the register width.
    #[inline]
    pub unsafe fn write_reg<T: MmioReadWrite>(&self, offset: u32, value: T) {
        let addr = self.reg_addr(offset);
        ptr::write_volatile(addr as *mut T, value);
    }

    /// Read-modify-write: read, apply `f`, write back.
    ///
    /// # Safety
    /// Same as `write_reg`.
    #[inline]
    pub unsafe fn modify_reg<T: MmioReadWrite, F: FnOnce(T) -> T>(&self, offset: u32, f: F) {
        let val = self.read_reg::<T>(offset);
        self.write_reg(offset, f(val));
    }

    /// Set bits in a register (read, OR with mask, write back).
    ///
    /// # Safety
    /// Same as `write_reg`.
    #[inline]
    pub unsafe fn set_bits(&self, offset: u32, mask: u32) {
        self.modify_reg::<u32, _>(offset, |v| v | mask);
    }

    /// Clear bits in a register (read, AND with !mask, write back).
    ///
    /// # Safety
    /// Same as `write_reg`.
    #[inline]
    pub unsafe fn clear_bits(&self, offset: u32, mask: u32) {
        self.modify_reg::<u32, _>(offset, |v| v & !mask);
    }
}

/// Trait for types that can be read/written via MMIO.
pub trait MmioReadWrite: Copy {
    /// Read from a volatile pointer.
    unsafe fn read_volatile(ptr: *const Self) -> Self;
    /// Write to a volatile pointer.
    unsafe fn write_volatile(ptr: *mut Self, val: Self);
}

impl MmioReadWrite for u8 {
    #[inline]
    unsafe fn read_volatile(ptr: *const Self) -> u8 { ptr::read_volatile(ptr) }
    #[inline]
    unsafe fn write_volatile(ptr: *mut Self, val: u8) { ptr::write_volatile(ptr, val); }
}

impl MmioReadWrite for u16 {
    #[inline]
    unsafe fn read_volatile(ptr: *const Self) -> u16 { ptr::read_volatile(ptr) }
    #[inline]
    unsafe fn write_volatile(ptr: *mut Self, val: u16) { ptr::write_volatile(ptr, val); }
}

impl MmioReadWrite for u32 {
    #[inline]
    unsafe fn read_volatile(ptr: *const Self) -> u32 { ptr::read_volatile(ptr) }
    #[inline]
    unsafe fn write_volatile(ptr: *mut Self, val: u32) { ptr::write_volatile(ptr, val); }
}

impl MmioReadWrite for u64 {
    #[inline]
    unsafe fn read_volatile(ptr: *const Self) -> u64 { ptr::read_volatile(ptr) }
    #[inline]
    unsafe fn write_volatile(ptr: *mut Self, val: u64) { ptr::write_volatile(ptr, val); }
}
