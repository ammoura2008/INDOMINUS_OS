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

/// Physical address of the kernel's PML4 (set during boot).
/// Needed to temporarily switch CR3 when walking user page tables
/// from within syscall handlers (user PML4s lack the identity map).
static mut KERNEL_PML4_PHYS: u64 = 0;

/// Set the kernel's physical start address.
///
/// # Safety
/// Must be called exactly once during boot, before any process creation.
pub unsafe fn set_kernel_phys_start(phys: u64) {
    KERNEL_PHYS_START = phys;
}

/// Set the kernel PML4 physical address (called once during boot).
pub unsafe fn set_kernel_pml4_phys(phys: u64) {
    KERNEL_PML4_PHYS = phys;
}

/// Get the kernel PML4 physical address.
pub fn kernel_pml4_phys() -> u64 {
    unsafe { KERNEL_PML4_PHYS }
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

/// Fix up all PIC-relocated addresses in the kernel binary.
///
/// After the bootloader applies R_X86_64_RELATIVE relocations, all function
/// pointers and data pointers in the kernel contain **physical addresses**:
/// `*P = (base_phys - min_vaddr) + r_addend`.
///
/// User PML4s lack the identity map, so those physical addresses are NOT
/// accessible when running on a user PML4 — any `call *GOT(%rip)` through
/// a PIC-relocated GOT entry faults.
///
/// This function converts every R_X86_64_RELATIVE target from its physical
/// address to the corresponding kernel virtual address:
///   `virt = phys - KERNEL_PHYS_START + KERNEL_VIRT_BASE`
///
/// After this fixup, ALL kernel function/data pointers contain virtual
/// addresses from the upper half (0xFFFFFFFF80000000+), which are mapped
/// in every PML4 (both kernel and user). No identity-map dependency remains.
///
/// # When to call
/// Exactly once in `kernel_main()`, immediately after `set_kernel_phys_start()`
/// and before any other initialisation that might dereference a relocated pointer.
///
/// # Safety
/// Must be called exactly once, after `KERNEL_PHYS_START` has been set and
/// the kernel PML4 is active (so the virtual addresses in .rela.dyn are mapped).
/// `rela_dyn_vaddr` is the linked virtual address of the .rela.dyn section.
/// `rela_dyn_size` is its size in bytes.
pub unsafe fn fixup_pic_relocations(rela_dyn_vaddr: u64, rela_dyn_size: u64) {
    let kps = KERNEL_PHYS_START;
    if kps == 0 || rela_dyn_vaddr == 0 || rela_dyn_size == 0 {
        return; // Not initialized, or no relocations
    }

    let start = rela_dyn_vaddr;
    let total = rela_dyn_size;
    const ENTRY_SIZE: u64 = 24; // sizeof(Elf64_Rela) = 8+8+8

    let num_entries = total / ENTRY_SIZE;

    for i in 0..num_entries {
        let entry_addr = (start + i * ENTRY_SIZE) as *const u8;

        // Read RELA entry: { r_offset(8), r_info(8), r_addend(8) }
        let r_offset = core::ptr::read_volatile(entry_addr as *const u64);
        let r_info   = core::ptr::read_volatile(entry_addr.add(8) as *const u64);

        let rel_type = (r_info & 0xFFFF_FFFF) as u32;

        // Only fix up R_X86_64_RELATIVE (type 8).
        if rel_type != 8 {
            continue;
        }

        // r_offset is the virtual address of the 8-byte location to patch.
        let target = r_offset as *mut u64;

        // Read the current value — physical address from the bootloader.
        let current = core::ptr::read_volatile(target);

        // Convert physical → virtual:
        //   new = current - KERNEL_PHYS_START + KERNEL_VIRT_BASE
        let new_val = current.wrapping_sub(kps).wrapping_add(KERNEL_VIRT_BASE);

        core::ptr::write_volatile(target, new_val);
    }
}

/// Virtual base address of the kernel heap.
/// The heap starts here and grows upward (toward higher addresses).
pub const KERNEL_HEAP_BASE: u64 = 0xFFFF_FFFF_C000_0000;

/// Initial size of the kernel heap (16 MiB).
pub const KERNEL_HEAP_INITIAL_SIZE: u64 = 16 * 1024 * 1024;

/// Virtual address of user stack top (grows downward).
/// Placed near the top of the canonical lower half, leaving room for
/// stack growth and guard pages.
pub const USER_STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;

/// Virtual base address of the user heap (grows upward).
/// Placed in the middle of user space. The heap starts here and grows
/// toward the stack (downward from USER_STACK_TOP).
pub const USER_HEAP_BASE: u64 = 0x0000_4000_0000_0000;

/// Maximum user heap size (256 MiB).
pub const USER_HEAP_MAX_SIZE: u64 = 256 * 1024 * 1024;

/// Virtual base address for mmap regions (grows upward from here).
pub const USER_MMAP_BASE: u64 = 0x0000_2000_0000_0000;

// ─────────────────────────────────────────────────────────────────────────────
// Global heap allocator — CR3-safe wrapper
// ─────────────────────────────────────────────────────────────────────────────

/// Wrapper around `LockedHeap` that switches CR3 to the kernel PML4 before
/// dispatching to the inner allocator.
///
/// **After fixup_pic_relocations():** All kernel pointers now contain virtual
/// addresses (upper half), so this wrapper is no longer strictly required for
/// correctness.  We keep it as a **defence-in-depth** layer: if any pointer
/// somehow still contains a physical address, the identity-map switch ensures
/// it resolves correctly.
///
/// **Interrupt safety:** We DISABLE interrupts for the entire duration of
/// operating on the kernel PML4.  This prevents the timer from firing while
/// on kernel PML4, which would save a physical RIP.  With interrupts disabled,
/// the timer is deferred until we restore the original CR3 and re-enable
/// interrupts.
struct Cr3SafeHeap {
    inner: LockedHeap,
}

// SAFETY: Cr3SafeHeap delegates to LockedHeap which is a proper GlobalAlloc.
// The CR3 switch with interrupts disabled is transparent to the caller.
unsafe impl core::alloc::GlobalAlloc for Cr3SafeHeap {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let kernel_pml4 = kernel_pml4_phys();
        let (saved_cr3, _) = x86_64::registers::control::Cr3::read();
        let saved = saved_cr3.start_address().as_u64();

        if saved != kernel_pml4 {
            // Disable interrupts, switch to kernel PML4, do allocation,
            // switch back — all atomically w.r.t. timer.
            core::arch::asm!("cli");
            x86_64::registers::control::Cr3::write(
                x86_64::structures::paging::PhysFrame::containing_address(
                    x86_64::PhysAddr::new(kernel_pml4),
                ),
                x86_64::registers::control::Cr3Flags::empty(),
            );
            let result = self.inner.alloc(layout);
            x86_64::registers::control::Cr3::write(
                x86_64::structures::paging::PhysFrame::containing_address(
                    x86_64::PhysAddr::new(saved),
                ),
                x86_64::registers::control::Cr3Flags::empty(),
            );
            // Do NOT re-enable interrupts here.  SYSCALL clears IF via SFMASK,
            // so most callers expect IF=0.  Let the caller manage interrupts.
            result
        } else {
            // Already on kernel PML4 — no switch needed.
            self.inner.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        let kernel_pml4 = kernel_pml4_phys();
        let (saved_cr3, _) = x86_64::registers::control::Cr3::read();
        let saved = saved_cr3.start_address().as_u64();

        if saved != kernel_pml4 {
            core::arch::asm!("cli");
            x86_64::registers::control::Cr3::write(
                x86_64::structures::paging::PhysFrame::containing_address(
                    x86_64::PhysAddr::new(kernel_pml4),
                ),
                x86_64::registers::control::Cr3Flags::empty(),
            );
            self.inner.dealloc(ptr, layout);
            x86_64::registers::control::Cr3::write(
                x86_64::structures::paging::PhysFrame::containing_address(
                    x86_64::PhysAddr::new(saved),
                ),
                x86_64::registers::control::Cr3Flags::empty(),
            );
        } else {
            self.inner.dealloc(ptr, layout);
        }
    }
}

#[global_allocator]
static HEAP_ALLOCATOR: Cr3SafeHeap = Cr3SafeHeap {
    inner: LockedHeap::empty(),
};

/// Initialize the kernel heap allocator.
///
/// # Safety
/// - `heap_start` must be a valid, mapped virtual address
/// - `heap_size` must be within mapped memory
/// - Must be called after VMM has set up page tables
/// - Must be called exactly once
pub unsafe fn init_heap(heap_start: u64, heap_size: u64) {
    // Safety: the spinlock byte should be 0 (unlocked) since this is the first
    // access. If it's nonzero due to stale memory from the bootloader or PMM,
    // force-unlock before initializing.
    let lock_ptr = core::ptr::addr_of!(HEAP_ALLOCATOR.inner) as *mut u8;
    core::ptr::write_volatile(lock_ptr, 0);
    HEAP_ALLOCATOR.inner.lock().init(heap_start as *mut u8, heap_size as usize);
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


