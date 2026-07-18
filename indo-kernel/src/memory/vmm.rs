//! # Virtual Memory Manager (VMM)
//!
//! ## What is a VMM?
//!
//! The VMM manages the CPU's page tables — the data structures that translate
//! virtual addresses (what code sees) to physical addresses (what RAM the CPU
//! actually accesses).
//!
//! ## Why virtual memory?
//!
//! Without virtual memory, every program sees the same physical RAM. A bug in
//! one program corrupts all others. With virtual memory:
//! - Each process gets its own virtual address space (isolation)
//! - The kernel lives in the upper half, user space in the lower half
//! - Memory can be remapped, shared, or protected (CoW, mmap, etc.)
//!
//! ## x86_64 page table structure
//!
//! The x86_64 page table is a 4-level hierarchy:
//!
//! ```text
//! PML4 (Page Map Level 4)        ← CR3 points here
//!   └─ PDPT (Page Directory Ptr) ← bits 39..48 of virtual addr
//!       └─ PD (Page Directory)   ← bits 30..38
//!           └─ PT (Page Table)   ← bits 21..29
//!               └─ Physical Page ← bits 12..20 (page offset in low 12 bits)
//! ```
//!
//! ## Our kernel's virtual address layout
//!
//! ```text
//! 0xFFFF_FFFF_8000_0000 .. 0xFFFF_FFFF_FFFF_FFFF  Kernel (upper half, 2 GiB)
//!   0xFFFF_FFFF_8000_0000  Kernel .text start
//!   0xFFFF_FFFF_C000_0000  Kernel heap start
//! 0x0000_0000_0000_0000 .. 0x0000_7FFF_FFFF_FFFF  User space (lower half)
//! ```

use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags,
    PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr as X64PhysAddr, VirtAddr};

use super::pmm;
use super::PAGE_SIZE;

// ─────────────────────────────────────────────────────────────────────────────
// Frame allocator for the x86_64 crate
// ─────────────────────────────────────────────────────────────────────────────

/// Bridges our PMM to the x86_64 crate's `FrameAllocator` trait.
pub struct PmmFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for PmmFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        pmm::alloc_frame().map(|addr| {
            unsafe { PhysFrame::containing_address(X64PhysAddr::new(addr.as_u64())) }
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: create a mapper from a PML4 physical address
// ─────────────────────────────────────────────────────────────────────────────

/// Create an `OffsetPageTable` mapper from a PML4 physical address.
///
/// # Safety
/// - `pml4_phys` must point to a valid, allocated PML4
/// - The identity map must be active (physical == virtual for page table access)
/// - No other mutable reference to the same PML4 may exist
unsafe fn mapper_from_pml4(pml4_phys: super::PhysAddr) -> OffsetPageTable<'static> {
    let pml4_ptr = phys_to_virt(pml4_phys.as_u64()).as_mut_ptr()
        as *mut x86_64::structures::paging::PageTable;
    // OffsetPageTable with offset 0 = identity mapping (physical == virtual)
    // Safety: caller guarantees pml4_phys is valid and identity map is active
    OffsetPageTable::new(&mut *pml4_ptr, VirtAddr::new(0))
}

// ─────────────────────────────────────────────────────────────────────────────
// Page table operations
// ─────────────────────────────────────────────────────────────────────────────

/// Create a new, empty PML4 page table.
///
/// Allocates a physical frame for the PML4 and zero-fills it.
/// Returns the physical address of the new PML4 (for loading into CR3).
pub fn create_empty_pml4() -> super::PhysAddr {
    let frame = pmm::alloc_frame().expect("PMM: out of memory for PML4");
    let virt = unsafe { phys_to_virt(frame.as_u64()) };

    // Zero the page table (all entries unused)
    unsafe {
        let pt = &mut *(virt.as_mut_ptr() as *mut x86_64::structures::paging::PageTable);
        pt.zero();
    }

    frame
}

/// Map a single 4 KiB page: `virtual_addr` → `physical_addr`.
///
/// # Panics
/// - If the physical frame cannot be allocated (for intermediate tables)
/// - If the page is already mapped
pub fn map_page(
    pml4_phys: super::PhysAddr,
    virtual_addr: VirtAddr,
    physical_addr: super::PhysAddr,
    flags: PageTableFlags,
) {
    let page = Page::<Size4KiB>::containing_address(virtual_addr);
    let frame = PhysFrame::<Size4KiB>::containing_address(X64PhysAddr::new(physical_addr.as_u64()));

    // Safety: pml4_phys is valid, identity map is active
    let mut mapper = unsafe { mapper_from_pml4(pml4_phys) };
    let mut frame_allocator = PmmFrameAllocator;

    unsafe {
        let flush = mapper.map_to(page, frame, flags, &mut frame_allocator)
            .expect("VMM: failed to map page");
        flush.flush();
    }
}

/// Unmap a single 4 KiB page at `virtual_addr`.
///
/// # Panics
/// - If the page is not mapped
pub fn unmap_page(pml4_phys: super::PhysAddr, virtual_addr: VirtAddr) {
    let page = Page::<Size4KiB>::containing_address(virtual_addr);

    // Safety: pml4_phys is valid, identity map is active
    let mut mapper = unsafe { mapper_from_pml4(pml4_phys) };

    let (_frame, flush) = mapper.unmap(page)
        .expect("VMM: failed to unmap page");
    flush.flush();
}

/// Translate a virtual address to its physical address.
///
/// Returns `None` if the page is not mapped.
pub fn translate_addr(pml4_phys: super::PhysAddr, virtual_addr: VirtAddr) -> Option<super::PhysAddr> {
    // Safety: pml4_phys is valid, identity map is active
    let mapper = unsafe { mapper_from_pml4(pml4_phys) };

    let page = Page::<Size4KiB>::containing_address(virtual_addr);
    let result = mapper.translate_page(page);

    match result {
        Ok(frame) => {
            Some(super::PhysAddr::new(frame.start_address().as_u64()))
        }
        Err(_) => None,
    }
}

/// Flush the entire TLB by reloading CR3.
pub fn flush_tlb_full() {
    unsafe {
        let cr3 = x86_64::registers::control::Cr3::read();
        x86_64::registers::control::Cr3::write(cr3.0, cr3.1);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Address conversion (identity map aware)
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a physical address to a virtual address.
///
/// During early boot (before CR3 switch), the CPU uses UEFI's identity map
/// where physical == virtual. We keep this identity mapping for the boot
/// region so that page table manipulation continues to work after the CR3
/// switch.
///
/// # Safety
/// The caller must ensure the physical address is valid and accessible.
pub unsafe fn phys_to_virt(phys: u64) -> VirtAddr {
    // Identity mapping: virtual == physical
    VirtAddr::new(phys)
}

/// Convert a virtual address to a physical address.
///
/// # Safety
/// The caller must ensure the virtual address is mapped.
pub unsafe fn virt_to_phys(virt: VirtAddr) -> super::PhysAddr {
    super::PhysAddr::new(virt.as_u64())
}

// ─────────────────────────────────────────────────────────────────────────────
// Kernel page table setup
// ─────────────────────────────────────────────────────────────────────────────

/// Set up the kernel's page tables.
///
/// Creates a new PML4 with:
/// 1. Kernel higher-half mapping: physical kernel → virtual 0xFFFFFFFF80000000
/// 2. Identity mapping of the first 4 GiB (for CR3 switch safety)
///
/// Returns the physical address of the new PML4 (for CR3).
pub fn init_kernel_page_tables(
    kernel_phys_start: u64,
    kernel_phys_end: u64,
) -> super::PhysAddr {
    let pml4_phys = create_empty_pml4();

    // Calculate the kernel's physical-to-virtual offset
    // virt_addr = phys_addr + virt_offset
    // where virt_offset = kernel_virt_base - kernel_phys_start
    let kernel_virt_base: u64 = super::KERNEL_VIRT_BASE;
    let virt_offset = kernel_virt_base.wrapping_sub(kernel_phys_start);

    // Map the kernel's physical pages to their virtual addresses
    let mut phys_addr = kernel_phys_start;
    while phys_addr < kernel_phys_end {
        let virt_addr = VirtAddr::new(phys_addr.wrapping_add(virt_offset));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        map_page(pml4_phys, virt_addr, super::PhysAddr::new(phys_addr), flags);
        phys_addr += PAGE_SIZE;
    }

    // Map the kernel heap: allocate physical frames and map them to
    // KERNEL_HEAP_BASE .. KERNEL_HEAP_BASE + KERNEL_HEAP_INITIAL_SIZE
    let heap_pages = super::KERNEL_HEAP_INITIAL_SIZE / PAGE_SIZE;
    for i in 0..heap_pages {
        let frame = pmm::alloc_frame().expect("VMM: out of memory for heap pages");
        let virt_addr = VirtAddr::new(super::KERNEL_HEAP_BASE + i * PAGE_SIZE);
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        map_page(pml4_phys, virt_addr, frame, flags);
    }

    // Identity map the first 4 GiB (for safe CR3 transition)
    // After we switch CR3, the CPU will be executing at physical addresses.
    // The identity map keeps the current code accessible during the transition.
    let mut addr: u64 = 0;
    while addr < 0x1_0000_0000 { // 4 GiB
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        map_page(pml4_phys, VirtAddr::new(addr), super::PhysAddr::new(addr), flags);
        addr += PAGE_SIZE;
    }

    pml4_phys
}

/// Create a per-process PML4 with kernel pages shared.
///
/// Allocates a new PML4 and copies the kernel's upper-half entries (indices
/// 256–511) from `kernel_pml4_phys`. The lower half starts empty — user pages
/// are mapped separately by the process creator.
///
/// Because PML4 entries 256–511 point to the same PDPT/PD/PT structures used
/// by the kernel, all processes share the same kernel virtual mappings. The kernel
/// pages are safe from user access because their USER_ACCESSIBLE bit is clear.
///
/// Returns the physical address of the new PML4 (for CR3).
pub fn create_user_pml4(kernel_pml4_phys: super::PhysAddr) -> super::PhysAddr {
    let new_pml4 = create_empty_pml4();

    // Source: kernel's PML4 (identity-mapped, accessible via phys_to_virt)
    let src_ptr = unsafe {
        let v = phys_to_virt(kernel_pml4_phys.as_u64());
        v.as_mut_ptr() as *const x86_64::structures::paging::PageTable
    };
    // Destination: new PML4 (identity-mapped)
    let dst_ptr = unsafe {
        let v = phys_to_virt(new_pml4.as_u64());
        v.as_mut_ptr() as *mut x86_64::structures::paging::PageTable
    };

    unsafe {
        let src = &*src_ptr;
        let dst = &mut *dst_ptr;

        // Copy PML4 entries 256..512 (upper half = kernel space) as raw bytes.
        // Each PageTableEntry is 8 bytes, 256 entries = 2048 bytes.
        let src_bytes = (src as *const _ as *const u8).add(256 * 8);
        let dst_bytes = (dst as *mut _ as *mut u8).add(256 * 8);
        core::ptr::copy_nonoverlapping(src_bytes, dst_bytes, 256 * 8);
    }

    new_pml4
}

/// Switch CR3 to the given page table and return the old CR3 value.
///
/// # Safety
/// - `new_pml4_phys` must point to a valid PML4 with correct mappings
/// - The current code must be accessible in the new page tables
/// - Interrupts should be disabled
pub unsafe fn switch_page_table(new_pml4_phys: super::PhysAddr) -> super::PhysAddr {
    let (old_frame, old_flags) = x86_64::registers::control::Cr3::read();
    let new_frame = PhysFrame::containing_address(X64PhysAddr::new(new_pml4_phys.as_u64()));
    x86_64::registers::control::Cr3::write(new_frame, old_flags);
    super::PhysAddr::new(old_frame.start_address().as_u64())
}

/// Remove the identity mapping of the first 4 GiB from the given PML4.
///
/// This should be called AFTER switching to the new page tables and
/// AFTER the kernel code is running at its higher-half virtual address.
///
/// # Safety
/// - The kernel must be executing at its higher-half address
/// - The identity map must exist in the given PML4
pub fn remove_identity_map(pml4_phys: super::PhysAddr) {
    // Directly zero the PML4 entry to remove the identity mapping.
    // Index 0 = PML4 entry for lower half (identity map region).
    // This leaks the PDPT frame (and all sub-tables), but that's acceptable
    // for Phase 1. Phase 9 will implement proper recursive freeing.
    unsafe {
        let pml4_virt = phys_to_virt(pml4_phys.as_u64());
        let pml4 = &mut *(pml4_virt.as_mut_ptr() as *mut x86_64::structures::paging::PageTable);
        pml4[x86_64::structures::paging::PageTableIndex::new(0)].set_unused();
    }

    flush_tlb_full();
}
