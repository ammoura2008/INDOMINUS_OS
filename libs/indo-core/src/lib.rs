//! # indo-core — Shared Boot Protocol Types
//!
//! This library defines the **boot protocol** between the INDOMINUS
//! bootloader (`indo-boot`) and the INDOMINUS kernel (`indo-kernel`).
//!
//! ## What is a boot protocol?
//!
//! When the bootloader loads the kernel and transfers control to it, the
//! kernel needs information it cannot discover on its own at that moment:
//! - Where is physical memory? What regions are usable RAM? What regions
//!   are reserved by firmware, MMIO, or ACPI?
//! - Where was the kernel loaded in memory?
//! - Where is the ACPI RSDP (system hardware description table)?
//! - What framebuffer is available for early graphics output?
//!
//! The bootloader collects all of this from UEFI firmware and writes it
//! into a `BootInfo` struct, then passes a pointer to this struct as the
//! kernel's first argument.
//!
//! ## Safety contract
//!
//! The bootloader MUST:
//! 1. Allocate `BootInfo` in memory that will survive the UEFI boot services exit.
//! 2. Ensure the memory map is complete and accurate.
//! 3. Pass a valid, aligned pointer to `BootInfo` in the first argument register (rdi).
//!
//! The kernel MUST:
//! 1. Treat the `BootInfo` pointer as valid only during early init.
//! 2. Copy all required information before setting up its own memory manager,
//!    which will reclaim the memory containing `BootInfo`.

#![no_std]

// ─────────────────────────────────────────────────────────────────────────────
// Core types
// ─────────────────────────────────────────────────────────────────────────────

/// Physical address type. A newtype wrapper to prevent accidental confusion
/// between physical and virtual addresses — one of the most common bugs in
/// kernel development.
///
/// Physical addresses refer to actual RAM/MMIO locations on the bus.
/// Virtual addresses are what the CPU sees after the MMU translates them.
/// Confusing these two crashes the kernel or silently corrupts memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysAddr(pub u64);

/// Virtual address type. See [`PhysAddr`] for the motivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct VirtAddr(pub u64);

impl PhysAddr {
    /// Creates a new physical address from a raw `u64`.
    #[inline(always)]
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    /// Returns the raw address value.
    #[inline(always)]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Converts to a raw pointer. Only meaningful when identity-mapped.
    ///
    /// # Safety
    /// The caller must ensure this physical address is currently identity-mapped
    /// (physical == virtual). This is only true in early boot.
    #[inline(always)]
    pub unsafe fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    /// Converts to a mutable raw pointer. See [`as_ptr`] safety note.
    #[inline(always)]
    pub unsafe fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }
}

impl VirtAddr {
    #[inline(always)]
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    #[inline(always)]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[inline(always)]
    pub fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    #[inline(always)]
    pub fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Memory map
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum number of memory regions we can represent.
///
/// Why a fixed array? Because at boot time we have NO heap allocator yet.
/// We cannot use Vec. We use a fixed-size array on the stack, which is safe
/// and allocation-free. 256 regions is more than enough for any real machine
/// (typical systems have 10–50 UEFI memory regions).
pub const MAX_MEMORY_REGIONS: usize = 256;

/// The type/purpose of a physical memory region.
///
/// This mirrors UEFI's `EfiMemoryType` enum but is our own type, because we
/// don't want the kernel to depend on UEFI types after boot services exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MemoryRegionKind {
    /// Usable general-purpose RAM. The kernel's physical memory manager
    /// should add these regions to its free list.
    Usable          = 0,
    /// Reserved by firmware. Do NOT allocate from these.
    Reserved        = 1,
    /// ACPI tables. Readable, but treat as reserved until ACPI is parsed.
    AcpiReclaimable = 2,
    /// Non-volatile ACPI storage. Never reclaim.
    AcpiNvs         = 3,
    /// Memory-mapped I/O. Writing random data here kills hardware.
    Mmio            = 4,
    /// Where the bootloader code/data lives. Reclaimable after we're done.
    BootloaderCode  = 5,
    /// Where the kernel's ELF sections are mapped.
    KernelCode      = 6,
    /// Unknown/unrecognized UEFI memory type. Treat as reserved.
    Unknown         = 255,
}

/// A single contiguous region of physical memory.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryRegion {
    /// Starting physical address of the region.
    pub start: PhysAddr,
    /// Length of the region in bytes.
    pub length: u64,
    /// How this region may be used.
    pub kind: MemoryRegionKind,
}

impl MemoryRegion {
    /// Returns the exclusive end address of this region.
    #[inline]
    pub fn end(&self) -> PhysAddr {
        PhysAddr::new(self.start.as_u64() + self.length)
    }

    /// Returns true if this region contains `addr`.
    #[inline]
    pub fn contains(&self, addr: PhysAddr) -> bool {
        addr >= self.start && addr < self.end()
    }
}

/// The complete physical memory map of the system.
///
/// The bootloader fills this in by querying UEFI's `GetMemoryMap` service.
/// The kernel's Physical Memory Manager (PMM) reads this to know which
/// pages of RAM it is allowed to allocate.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryMap {
    /// The actual memory region entries.
    pub regions: [MemoryRegion; MAX_MEMORY_REGIONS],
    /// How many entries in `regions` are valid. The rest are zeroed.
    pub count: usize,
}

impl MemoryMap {
    /// Returns only the valid (populated) memory regions.
    #[inline]
    pub fn entries(&self) -> &[MemoryRegion] {
        &self.regions[..self.count]
    }

    /// Returns an iterator over all usable (free RAM) regions.
    #[inline]
    pub fn usable_regions(&self) -> impl Iterator<Item = &MemoryRegion> {
        self.entries()
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
    }

    /// Returns the total amount of usable physical RAM in bytes.
    pub fn total_usable_bytes(&self) -> u64 {
        self.usable_regions().map(|r| r.length).sum()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Framebuffer
// ─────────────────────────────────────────────────────────────────────────────

/// Pixel format of the GOP framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    /// 8 bits Red, 8 bits Green, 8 bits Blue, 8 bits reserved (padding).
    /// Most common format on x86 UEFI systems.
    Bgr  = 0,
    /// 8 bits Blue, 8 bits Green, 8 bits Red, 8 bits reserved.
    Rgb  = 1,
    /// Custom bitmask format. Rare.
    Bitmask = 2,
    /// No framebuffer available.
    None = 3,
}

/// Information about the UEFI GOP (Graphics Output Protocol) framebuffer.
///
/// The bootloader obtains this from UEFI before exiting boot services.
/// The kernel uses it for early console output before a proper GPU driver loads.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FramebufferInfo {
    /// Physical address of the linear framebuffer.
    pub base: PhysAddr,
    /// Total size of the framebuffer in bytes.
    pub size: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixels per scan line (may be larger than width due to alignment padding).
    pub stride: u32,
    /// Pixel format.
    pub pixel_format: PixelFormat,
}

// ─────────────────────────────────────────────────────────────────────────────
// Boot Info — the handoff struct
// ─────────────────────────────────────────────────────────────────────────────

/// Magic number to validate the BootInfo struct at the kernel entry point.
///
/// If the kernel receives a BootInfo with a different magic value, it means
/// either: the wrong bootloader was used, the pointer is garbage, or memory
/// was corrupted. Halt immediately in this case.
///
/// Value chosen as a nod to the project: ASCII for "INDO" = 0x4F444E49
pub const BOOT_INFO_MAGIC: u64 = 0x4F444E49_00000001;

/// The complete handoff structure from bootloader to kernel.
///
/// # Memory layout
/// This struct is `#[repr(C)]` to guarantee a fixed, predictable layout.
/// Both the bootloader and kernel see the exact same field offsets.
/// Any change to this struct is a **breaking boot protocol change** and
/// must be versioned.
///
/// # Lifetime
/// This struct lives in memory allocated by the bootloader. After the kernel
/// sets up its own physical memory manager, it MUST copy out all fields it
/// needs before this memory is reclaimed. The kernel should call
/// `BootInfo::validate()` immediately and panic if it fails.
#[derive(Debug)]
#[repr(C)]
pub struct BootInfo {
    /// Magic number. Kernel MUST check this first thing.
    pub magic: u64,

    /// Boot protocol version for forward/backward compatibility.
    /// Increment this when any field changes.
    pub protocol_version: u32,

    /// Padding to maintain alignment.
    pub _pad: u32,

    /// The physical memory map of the system.
    pub memory_map: MemoryMap,

    /// Information about the GOP framebuffer for early display output.
    pub framebuffer: FramebufferInfo,

    /// Physical address of the ACPI RSDP (Root System Description Pointer).
    /// This is the entry point into the ACPI hardware description tables.
    /// Used to discover CPU topology, PCI devices, interrupt routing, etc.
    pub rsdp_addr: PhysAddr,

    /// Physical address of the kernel's own ELF image in memory.
    /// Used to set up accurate virtual memory mappings for the kernel.
    pub kernel_phys_start: PhysAddr,

    /// Physical end address (exclusive) of the kernel image.
    pub kernel_phys_end: PhysAddr,

    /// Virtual address the kernel was linked to start at.
    /// This is the address the kernel expects its code to live at.
    pub kernel_virt_base: VirtAddr,
}

/// Current boot protocol version. Increment on any breaking change.
pub const BOOT_PROTOCOL_VERSION: u32 = 1;

impl BootInfo {
    /// Validates this `BootInfo` struct. Returns `Err` if corrupted.
    ///
    /// Call this IMMEDIATELY at kernel entry before touching any other field.
    pub fn validate(&self) -> Result<(), BootInfoError> {
        if self.magic != BOOT_INFO_MAGIC {
            return Err(BootInfoError::BadMagic {
                expected: BOOT_INFO_MAGIC,
                found: self.magic,
            });
        }
        if self.protocol_version != BOOT_PROTOCOL_VERSION {
            return Err(BootInfoError::VersionMismatch {
                expected: BOOT_PROTOCOL_VERSION,
                found: self.protocol_version,
            });
        }
        if self.memory_map.count == 0 {
            return Err(BootInfoError::EmptyMemoryMap);
        }
        Ok(())
    }
}

/// Errors that can occur when validating a `BootInfo` struct.
#[derive(Debug)]
pub enum BootInfoError {
    BadMagic   { expected: u64, found: u64 },
    VersionMismatch { expected: u32, found: u32 },
    EmptyMemoryMap,
}
