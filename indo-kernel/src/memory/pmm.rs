//! # Physical Memory Manager (PMM)
//!
//! ## What is a PMM?
//!
//! The PMM tracks which 4 KiB physical frames are free and which are allocated.
//! Every other memory subsystem (VMM, heap, user processes) depends on it to
//! obtain physical memory.
//!
//! ## Design: Bitmap Allocator
//!
//! We use a bitmap where each bit represents one 4 KiB physical frame:
//! - Bit = 0: frame is free
//! - Bit = 1: frame is allocated
//!
//! ## Why bitmap?
//!
//! | Allocator   | Pros                        | Cons                          |
//! |-------------|-----------------------------|-------------------------------|
//! | Bitmap      | Simple, O(1) alloc, correct | Uses ~1 byte per 8 frames     |
//! | Buddy       | Better locality, O(log n)   | Complex, internal fragmentation|
//! | Slab        | Best for small objects       | Requires heap (recursive)     |
//!
//! Bitmap is the right choice for Phase 1: simple, correct, and scales to
//! any amount of RAM (1 GiB = 32 KiB bitmap, 16 GiB = 512 KiB).
//!
//! ## Initialization flow
//!
//! 1. Bootloader passes UEFI memory map in `BootInfo`
//! 2. PMM reads the map and marks all usable regions as free
//! 3. PMM marks reserved regions (kernel, MMIO, ACPI, etc.) as used
//! 4. PMM is ready to serve `alloc_frame()` and `free_frame()` calls

use crate::memory::{PAGE_SIZE, PhysAddr};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum physical memory the PMM can track (16 GiB).
/// 16 GiB = 4,194,304 frames = 524,288 bytes = 512 KiB bitmap.
const MAX_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Maximum number of physical frames the PMM can track.
const MAX_FRAMES: usize = (MAX_MEMORY_BYTES / PAGE_SIZE) as usize;

/// Bitmap size in bytes: one bit per frame.
const BITMAP_SIZE: usize = MAX_FRAMES / 8;

// ─────────────────────────────────────────────────────────────────────────────
// Global state
// ─────────────────────────────────────────────────────────────────────────────

/// The bitmap. Each bit represents one 4 KiB physical frame.
/// Stored as a static array in BSS — zero-initialized (all frames marked free).
/// We will mark reserved/used frames during initialization.
static mut BITMAP: [u8; BITMAP_SIZE] = [0; BITMAP_SIZE];

/// Number of physical frames we're actually tracking.
/// Determined by the highest usable address in the UEFI memory map.
static mut TOTAL_FRAMES: usize = 0;

/// Number of currently free frames.
static mut FREE_FRAMES: usize = 0;

/// The PMM is not behind a Mutex because:
/// 1. During early boot, only one CPU exists (no SMP yet)
/// 2. All alloc/free calls happen with interrupts disabled (Phase 2+)
/// 3. A Mutex here would require the heap (which doesn't exist yet)
///
/// When SMP is added (Phase 3+), this must become per-CPU or use a lock.

// ─────────────────────────────────────────────────────────────────────────────
// Bitmap operations
// ─────────────────────────────────────────────────────────────────────────────

/// Set the bit for frame `index` to 1 (allocated).
///
/// # Safety
/// `index` must be less than `TOTAL_FRAMES`.
#[inline]
unsafe fn bitmap_set(index: usize) {
    let byte_index = index / 8;
    let bit_offset = index % 8;
    BITMAP[byte_index] |= 1 << bit_offset;
}

/// Clear the bit for frame `index` to 0 (free).
///
/// # Safety
/// `index` must be less than `TOTAL_FRAMES`.
#[inline]
unsafe fn bitmap_clear(index: usize) {
    let byte_index = index / 8;
    let bit_offset = index % 8;
    BITMAP[byte_index] &= !(1 << bit_offset);
}

/// Test if the bit for frame `index` is set (allocated).
///
/// # Safety
/// `index` must be less than `TOTAL_FRAMES`.
#[inline]
unsafe fn bitmap_test(index: usize) -> bool {
    let byte_index = index / 8;
    let bit_offset = index % 8;
    (BITMAP[byte_index] & (1 << bit_offset)) != 0
}

// ─────────────────────────────────────────────────────────────────────────────
// Initialization
// ─────────────────────────────────────────────────────────────────────────────

/// Initialize the PMM from the UEFI memory map.
///
/// This function:
/// 1. Scans the memory map to find the highest usable address
/// 2. Marks all usable regions as free in the bitmap
/// 3. Marks all non-usable regions as used (reserved, MMIO, kernel, etc.)
/// 4. Marks the PMM's own bitmap as used (prevent self-allocation)
///
/// # Safety
/// - `memory_map` must be a valid memory map from the bootloader
/// - Must be called exactly once, before any `alloc_frame()` calls
/// - Interrupts should be disabled during this call
pub fn init(memory_map: & indo_core::MemoryMap) {
    unsafe {
        // Step 1: Determine total frames to track
        let mut max_addr: u64 = 0;
        for region in memory_map.entries() {
            let end = region.start.as_u64() + region.length;
            if end > max_addr {
                max_addr = end;
            }
        }

        TOTAL_FRAMES = ((max_addr + PAGE_SIZE - 1) / PAGE_SIZE) as usize;
        if TOTAL_FRAMES > MAX_FRAMES {
            TOTAL_FRAMES = MAX_FRAMES;
        }

        // Step 2: Start with all frames marked as used (safe default)
        // The bitmap is already zeroed (BSS), but we need to mark
        // non-usable regions as used. We'll do this by first marking
        // everything as used, then marking usable regions as free.

        // Actually, let's use a simpler approach:
        // 1. Mark everything as used (set all bits)
        // 2. Mark usable regions as free (clear those bits)

        // Set all bits to 1 (all frames used)
        for byte in BITMAP.iter_mut() {
            *byte = 0xFF;
        }
        FREE_FRAMES = 0;

        // Step 3: Mark usable regions as free
        for region in memory_map.usable_regions() {
            let start_frame = (region.start.as_u64() / PAGE_SIZE) as usize;
            let end_frame = ((region.start.as_u64() + region.length + PAGE_SIZE - 1) / PAGE_SIZE) as usize;

            let clamped_end = end_frame.min(TOTAL_FRAMES);

            for frame in start_frame..clamped_end {
                bitmap_clear(frame);
                FREE_FRAMES += 1;
            }
        }

        // Step 4: Reserve frame 0 (BIOS/IVT/interrupt vector table)
        // Must be after marking usable regions to ensure it stays allocated.
        if !bitmap_test(0) {
            bitmap_set(0);
            FREE_FRAMES -= 1;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Frame allocation
// ─────────────────────────────────────────────────────────────────────────────

/// Allocate a single physical frame.
///
/// Returns the physical address of the allocated frame, or `None` if
/// no free frames remain.
///
/// Uses a simple linear scan of the bitmap to find the first free frame.
/// This is O(n) in the worst case but fine for Phase 1. Phase 2+ can
/// add a free-list for O(1) allocation.
pub fn alloc_frame() -> Option<PhysAddr> {
    unsafe {
        // Start at frame 1 to skip physical address 0 (BIOS/IVT area).
        // Frame 0 must never be allocated.
        for index in 1..TOTAL_FRAMES {
            if !bitmap_test(index) {
                bitmap_set(index);
                FREE_FRAMES -= 1;
                return Some(PhysAddr::new(index as u64 * PAGE_SIZE));
            }
        }
        None // Out of memory
    }
}

/// Free a previously allocated physical frame.
///
/// # Safety
/// - `frame` must have been allocated by `alloc_frame()`
/// - `frame` must not have been freed already (double-free is UB)
/// - `frame` must be page-aligned
pub fn free_frame(frame: PhysAddr) {
    let index = (frame.as_u64() / PAGE_SIZE) as usize;

    unsafe {
        assert!(index < TOTAL_FRAMES, "frame address out of range");
        assert!(frame.as_u64() % PAGE_SIZE == 0, "frame not page-aligned");
        assert!(bitmap_test(index), "double-free detected");
        bitmap_clear(index);
        FREE_FRAMES += 1;
    }
}

/// Allocate contiguous physical frames.
///
/// `count` is the number of contiguous frames to allocate.
/// Returns the physical address of the first frame, or `None` if
/// no contiguous region of the requested size is available.
pub fn alloc_contiguous(count: usize) -> Option<PhysAddr> {
    if count == 0 {
        return None;
    }

    unsafe {
        let mut run_start: Option<usize> = None;
        let mut run_length: usize = 0;

        for index in 0..TOTAL_FRAMES {
            if !bitmap_test(index) {
                if run_start.is_none() {
                    run_start = Some(index);
                    run_length = 1;
                } else {
                    run_length += 1;
                }

                if run_length == count {
                    let start = run_start.unwrap();
                    for frame in start..(start + count) {
                        bitmap_set(frame);
                        FREE_FRAMES -= 1;
                    }
                    return Some(PhysAddr::new(start as u64 * PAGE_SIZE));
                }
            } else {
                run_start = None;
                run_length = 0;
            }
        }
        None // No contiguous region found
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Query functions
// ─────────────────────────────────────────────────────────────────────────────

/// Returns the total number of physical frames the PMM tracks.
pub fn total_frames() -> usize {
    unsafe { TOTAL_FRAMES }
}

/// Returns the number of free physical frames.
pub fn free_frames() -> usize {
    unsafe { FREE_FRAMES }
}

/// Returns the total usable physical memory in bytes.
pub fn total_usable_bytes() -> u64 {
    free_frames() as u64 * PAGE_SIZE
}

/// Check if a physical frame is currently allocated.
pub fn is_allocated(frame: PhysAddr) -> bool {
    let index = (frame.as_u64() / PAGE_SIZE) as usize;
    assert!(index < unsafe { TOTAL_FRAMES }, "frame address out of range");
    unsafe { bitmap_test(index) }
}

/// Mark a physical address range as used (reserved) in the PMM bitmap.
///
/// Called after `init()` to reserve regions that must not be allocated,
/// such as the kernel's physical memory and the PMM bitmap itself.
///
/// # Safety
/// - `phys_start` must be page-aligned (or rounded down internally)
/// - `phys_end` must be page-aligned (or rounded up internally)
/// - Must be called after `init()` and before any `alloc_frame()` that
///   could allocate these frames
pub fn mark_region_used(phys_start: u64, phys_end: u64) {
    let start_frame = (phys_start / PAGE_SIZE) as usize;
    let end_frame = ((phys_end + PAGE_SIZE - 1) / PAGE_SIZE) as usize;

    unsafe {
        #[cfg(DEBUG_KERNEL)]
        let before = FREE_FRAMES;
        for frame in start_frame..end_frame {
            if frame < TOTAL_FRAMES && !bitmap_test(frame) {
                bitmap_set(frame);
                FREE_FRAMES -= 1;
            }
        }
        #[cfg(DEBUG_KERNEL)]
        {
            crate::serial::write_str("[PMM] mark_region_used: 0x");
            crate::serial::write_hex(start_frame as u64);
            crate::serial::write_str("..0x");
            crate::serial::write_hex(end_frame as u64);
            crate::serial::write_str(" freed_before=");
            crate::serial::write_hex(before as u64);
            crate::serial::write_str(" freed_after=");
            crate::serial::write_hex(FREE_FRAMES as u64);
            crate::serial::write_nl();
        }
    }
}
