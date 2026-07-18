//! # Memory Management
//!
//! ## Architecture
//!
//! INDOMINUS memory management is built in layers:
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │          Kernel Heap Allocator           │  ← Box, Vec, String
//! │      (linked_list_allocator crate)       │
//! ├─────────────────────────────────────────┤
//! │       Virtual Memory Manager (VMM)       │  ← Page tables, mapping
//! │    (x86_64 crate + custom code)          │
//! ├─────────────────────────────────────────┤
//! │     Physical Memory Manager (PMM)        │  ← Frame allocation
//! │          (bitmap allocator)              │
//! └─────────────────────────────────────────┘
//! ```
//!
//! ## Initialization order
//!
//! 1. PMM reads UEFI memory map → marks frames free/used
//! 2. VMM creates new page tables → higher-half kernel mapping
//! 3. CR3 switched to new page tables
//! 4. Heap allocator initialized
//! 5. Kernel now has full memory management

pub mod pmm;
pub mod vmm;

use linked_list_allocator::LockedHeap;

// Re-export indo_core types for convenience
pub use indo_core::PhysAddr;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Size of a single physical page in bytes.
pub const PAGE_SIZE: u64 = 4096;

/// Virtual base address of the kernel (upper half, -2 GiB).
/// All kernel code, data, and static variables are linked at this address.
pub const KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Physical start address of the kernel (set during boot from BootInfo).
/// Used to convert physical addresses to virtual addresses when needed.
/// With PIC, function pointers in the kernel binary contain physical addresses
/// after R_X86_64_RELATIVE relocation: `*P = base_phys + (vaddr - min_vaddr)`.
static mut KERNEL_PHYS_START: u64 = 0;

/// Set the kernel's physical start address.
///
/// # Safety
/// Must be called exactly once during boot, before any process creation.
pub unsafe fn set_kernel_phys_start(phys: u64) {
    KERNEL_PHYS_START = phys;
}

/// Get the kernel's physical start address.
pub fn kernel_phys_start() -> u64 {
    unsafe { KERNEL_PHYS_START }
}

/// Convert a physical address (as stored in relocated kernel data) to its
/// corresponding virtual address in the kernel's higher-half mapping.
///
/// With PIC, function pointers and static addresses in the kernel binary are
/// relocated to physical addresses by the bootloader (R_X86_64_RELATIVE).
/// This function reverses that: `virt = phys + (KERNEL_VIRT_BASE - kernel_phys_start)`.
///
/// # Safety
/// `kernel_phys_start()` must have been set before calling this.
pub unsafe fn phys_to_kernel_virt(phys: u64) -> u64 {
    let kps = KERNEL_PHYS_START;
    phys.wrapping_add(KERNEL_VIRT_BASE).wrapping_sub(kps)
}

/// Virtual base address of the kernel heap.
/// The heap starts here and grows upward (toward higher addresses).
pub const KERNEL_HEAP_BASE: u64 = 0xFFFF_FFFF_C000_0000;

/// Initial size of the kernel heap (4 MiB).
pub const KERNEL_HEAP_INITIAL_SIZE: u64 = 4 * 1024 * 1024;

/// Virtual address of the kernel stack (grows downward from here).
/// Placed at the top of the kernel's address range.
pub const KERNEL_STACK_TOP: u64 = 0xFFFF_FFFF_D000_0000;

/// Size of the kernel stack (16 KiB = 4 pages).
pub const KERNEL_STACK_SIZE: u64 = 16 * 1024;

// ─────────────────────────────────────────────────────────────────────────────
// User-space memory layout (Ring 3)
// ─────────────────────────────────────────────────────────────────────────────

/// Virtual base address for user code (4 MiB, below the kernel).
/// Standard ELF load address — avoids the zero page (NULL pointer safety).
pub const USER_CODE_BASE: u64 = 0x0000_0000_0040_0000;

/// Virtual address of user stack top (grows downward).
/// Placed near the top of the canonical lower half, leaving room for
/// stack growth and guard pages.
pub const USER_STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;

/// Size of each user process's kernel stack (8 KiB = 2 pages).
/// Used for Ring 3 → Ring 0 transitions (syscall, interrupt).
pub const USER_KERNEL_STACK_SIZE: u64 = 8192;

// ─────────────────────────────────────────────────────────────────────────────
// Global heap allocator
// ─────────────────────────────────────────────────────────────────────────────

/// The kernel's global heap allocator.
///
/// Uses `linked_list_allocator` which maintains a linked list of free regions.
/// This is a simple, correct allocator suitable for early kernel development.
///
/// Protected by a spinlock because:
/// - Multiple code paths may allocate concurrently (interrupts, future SMP)
/// - The lock is held briefly (no sleeping in allocation)
#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Initialize the kernel heap allocator.
///
/// # Safety
/// - `heap_start` must be a valid, mapped virtual address
/// - `heap_size` must be within mapped memory
/// - Must be called after VMM has set up page tables
/// - Must be called exactly once
pub unsafe fn init_heap(heap_start: u64, heap_size: u64) {
    HEAP_ALLOCATOR.lock().init(heap_start as *mut u8, heap_size as usize);
}

/// Allocate memory on the kernel heap.
///
/// Returns a pointer to the allocated memory, or null if allocation fails.
/// The memory is uninitialized.
///
/// # Safety
/// The returned pointer is valid until explicitly deallocated.
#[alloc_error_handler]
fn alloc_error_layout(layout: core::alloc::Layout) -> ! {
    panic!(
        "KERNEL PANIC: out of memory allocating {} bytes (align={})",
        layout.size(),
        layout.align()
    );
}
