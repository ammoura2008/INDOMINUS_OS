//! # Global Descriptor Table (GDT)
//!
//! ## GDT Layout
//!
//! ```
//! Index 0: Null descriptor (required by x86 spec)
//! Index 1: Kernel code segment (64-bit, Ring 0, executable)
//! Index 2: Kernel data segment (Ring 0, writable)
//! Index 3: User code segment   (64-bit, Ring 3, executable)
//! Index 4: User data segment   (Ring 3, writable)
//! Index 5-6: TSS descriptor (128-bit)
//! ```
//!
//! ## Ring transitions
//!
//! When a user process (Ring 3) triggers an interrupt or executes `syscall`:
//! 1. CPU reads TSS.RSP0 to get the kernel stack pointer
//! 2. CPU switches to Ring 0 using that stack
//! 3. On `iretq` back to Ring 3, CPU restores user CS/SS/RSP from the stack frame

use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::{Segment, CS, DS, ES, FS, GS, SS};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;
use spin::Once;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// IST index for the double-fault handler stack.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Size of the double-fault IST stack (16 KiB).
pub const IST_STACK_SIZE: usize = 4096 * 4;

// ─────────────────────────────────────────────────────────────────────────────
// GDT global state
// ─────────────────────────────────────────────────────────────────────────────

/// Double-fault stack (static, zero-initialized).
pub static mut DOUBLE_FAULT_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];

/// The Task State Segment.
/// Wrapped in SyncUnsafeCell to avoid UB from shared references to mutable statics.
static TSS: crate::sync_cell::SyncUnsafeCell<TaskStateSegment> = crate::sync_cell::SyncUnsafeCell::new(TaskStateSegment::new());

/// The Global Descriptor Table and its selectors.
static GDT: Once<(GlobalDescriptorTable, Selectors)> = Once::new();

/// Segment selectors stored after GDT construction.
pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub user_code:   SegmentSelector,
    pub user_data:   SegmentSelector,
    pub tss:         SegmentSelector,
}

// ─────────────────────────────────────────────────────────────────────────────
// Public selectors (available after init)
// ─────────────────────────────────────────────────────────────────────────────

/// Get the user code segment selector (Ring 3, CS).
pub fn user_code_selector() -> SegmentSelector {
    GDT.get().expect("GDT not initialized").1.user_code
}

/// Get the user data segment selector (Ring 3, SS).
pub fn user_data_selector() -> SegmentSelector {
    GDT.get().expect("GDT not initialized").1.user_data
}

/// Get the kernel code segment selector (Ring 0, CS).
pub fn kernel_code_selector() -> SegmentSelector {
    GDT.get().expect("GDT not initialized").1.kernel_code
}

/// Update TSS.RSP0 to point to the given kernel stack top.
///
/// Called during context switch so the CPU uses the correct kernel stack
/// when transitioning from Ring 3 → Ring 0 (on interrupt or syscall).
///
/// # Safety
/// Must be called with interrupts disabled (from the timer handler or
/// with interrupts globally disabled).
pub unsafe fn set_tss_rsp0(rsp0: u64) {
    (*TSS.get()).privilege_stack_table[0] = VirtAddr::new(rsp0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Initialization
// ─────────────────────────────────────────────────────────────────────────────

/// Initialize and load the GDT.
///
/// Must be called before enabling interrupts or setting up syscalls.
/// Uses the physical address for the GDTR base (PIC), which works because
/// the UEFI page tables (and kernel PML4) have the identity map active.
pub fn init() {
    // ── Configure TSS ───────────────────────────────────────────────────────
    unsafe {
        let tss = &mut *TSS.get();
        // IST[0] = double-fault stack (top, since stacks grow downward)
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            // With PIC, &raw const gives a physical address. The IST entry
            // must be a kernel virtual address so it works after CR3 switch
            // to a user PML4 (which lacks the identity map).
            let stack_phys = &raw const DOUBLE_FAULT_STACK as u64;
            let stack_virt = crate::memory::phys_to_kernel_virt(stack_phys);
            VirtAddr::new(stack_virt) + IST_STACK_SIZE as u64
        };
        // RSP0 will be set per-process during context switch
    }

    // ── Build GDT ───────────────────────────────────────────────────────────
    let (gdt, selectors) = GDT.call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();

        let kernel_code = gdt.append(Descriptor::kernel_code_segment());
        let kernel_data = gdt.append(Descriptor::kernel_data_segment());

        let user_code = gdt.append(Descriptor::user_code_segment());
        let user_data = gdt.append(Descriptor::user_data_segment());

        let tss_selector = gdt.append(unsafe { Descriptor::tss_segment(&*TSS.get()) });

        (
            gdt,
            Selectors {
                kernel_code,
                kernel_data,
                user_code,
                user_data,
                tss: tss_selector,
            },
        )
    });

    // ── Load GDT (physical address, identity-mapped) ────────────────────────
    // gdt.load() uses the physical address for GDTR base (PIC relocation).
    // This works because both UEFI and kernel PML4 have the identity map.
    // We'll switch to the virtual address later in switch_gdt_to_virtual().
    gdt.load();

    // ── Load segment registers ──────────────────────────────────────────────
    unsafe {
        CS::set_reg(selectors.kernel_code);
        DS::set_reg(selectors.kernel_data);
        ES::set_reg(selectors.kernel_data);
        FS::set_reg(selectors.kernel_data);
        GS::set_reg(selectors.kernel_data);
        SS::set_reg(selectors.kernel_data);
        load_tss(selectors.tss);
    }
}

/// Reload GDTR with the kernel virtual address, and patch the TSS descriptor
/// base address to the kernel virtual address of TSS.
///
/// Must be called AFTER `vmm::switch_page_table()` — the higher-half mapping
/// must be active for the CPU to access the GDT and TSS at virtual addresses.
///
/// # Why this is needed
///
/// The GDT was built with `Descriptor::tss_segment(&TSS)`. With PIC, `&TSS as u64`
/// yields the **physical** address of TSS. The TSS descriptor in the GDT bakes
/// in this physical address as its base.
///
/// After CR3 → user PML4 (which lacks the identity map), the CPU can no longer
/// reach TSS at its physical address. When a timer interrupt fires from Ring 3,
/// the CPU needs TSS.RSP0 to switch stacks — but the physical address is
/// unmapped → page fault → double fault → triple fault.
///
/// Fix: patch the TSS descriptor's base address bits in the GDT to the kernel
/// virtual address of TSS (which IS mapped in user PML4s via upper-half entries),
/// then reload TR so the CPU picks up the new descriptor.
pub fn switch_gdt_to_virtual() {
    let (gdt, selectors) = GDT.get().expect("GDT not initialized");

    let gdt_phys = &raw const *gdt as u64;
    let gdt_virt = unsafe { crate::memory::phys_to_kernel_virt(gdt_phys) };

    // ── Patch TSS descriptor base address in GDT ──────────────────────────
    //
    // GDT layout (8 entries, each 8 bytes):
    //   Entry 0: null          (offset  0)
    //   Entry 1: kernel code   (offset  8)
    //   Entry 2: kernel data   (offset 16)
    //   Entry 3: user code     (offset 24)
    //   Entry 4: user data     (offset 32)
    //   Entry 5: TSS low       (offset 40)  — base[0:23] in bits 16..40, base[24:31] in bits 56..64
    //   Entry 6: TSS high      (offset 48)  — base[32:63] in bits 0..32
    //
    // The x86_64 crate's `tss_segment_raw` encodes:
    //   low:  bits 16..40 = ptr[0..24],  bits 56..64 = ptr[24..32]
    //   high: bits 0..32  = ptr[32..64]
    //
    unsafe {
        // TSS physical address (PIC: &raw const gives relocated phys addr)
        let tss_phys = &raw const *TSS.get() as u64;
        let tss_virt = crate::memory::phys_to_kernel_virt(tss_phys);

        crate::serial::write_str("[TSS] phys="); crate::serial::write_hex(tss_phys);
        crate::serial::write_str(" virt="); crate::serial::write_hex(tss_virt); crate::serial::write_nl();

        // ── Patch Entry 5 (TSS low) ──────────────────────────────────────
        let entry5_ptr = (gdt_virt + 40) as *mut u64;
        let mut entry5 = core::ptr::read_volatile(entry5_ptr);

        // Clear the "busy" bit (bit 41) in the access byte.
        // The first `ltr` in gdt::init() marked the TSS as busy (type 1011 → 1001).
        // The Intel manual says `ltr` generates #GP if the TSS is already busy.
        // We must set it back to "available" (type 1001) before re-loading TR.
        entry5 &= !(1u64 << 41);

        // Clear and set base[0:23] (bits 16..40)
        entry5 &= !(0xFF_FFFF << 16);
        entry5 |= (tss_virt & 0xFFFFFF) << 16;

        // Clear and set base[24:31] (bits 56..64)
        entry5 &= !(0xFF << 56);
        entry5 |= ((tss_virt >> 24) & 0xFF) << 56;

        core::ptr::write_volatile(entry5_ptr, entry5);

        // ── Patch Entry 6 (TSS high) ─────────────────────────────────────
        let entry6_ptr = (gdt_virt + 48) as *mut u64;
        let mut entry6 = core::ptr::read_volatile(entry6_ptr);

        // Clear and set base[32:63] (bits 0..32)
        entry6 &= !0xFFFF_FFFF;
        entry6 |= (tss_virt >> 32) & 0xFFFF_FFFF;

        core::ptr::write_volatile(entry6_ptr, entry6);

        crate::serial::write_str("[TSS] patched entry5="); crate::serial::write_hex(entry5);
        crate::serial::write_str(" entry6="); crate::serial::write_hex(entry6); crate::serial::write_nl();
    }

    // ── Reload GDTR with virtual address ──────────────────────────────────
    #[repr(C, packed)]
    struct Gdtr { limit: u16, base: u64 }
    let gdtr = Gdtr {
        limit: (core::mem::size_of::<(GlobalDescriptorTable, Selectors)>() - 1) as u16,
 base: gdt_virt,
    };
    unsafe {
        core::arch::asm!("lgdt [{}]", in(reg) &gdtr as *const _ as u64, options(readonly, nostack));
    }

    // ── Reload TR to pick up patched TSS descriptor ───────────────────────
    // The TR register caches the TSS descriptor from the GDT. After modifying
    // the TSS base address in the GDT, we must reload TR so the CPU uses the
    // updated descriptor with the kernel virtual address.
    unsafe {
        let tr_selector = selectors.tss;
        core::arch::asm!("ltr {0:x}", in(reg) tr_selector.0, options(readonly, nostack));
    }

    // ── Verify ────────────────────────────────────────────────────────────
    let readback = Gdtr { limit: 0, base: 0 };
    unsafe {
        core::arch::asm!("sgdt [{}]", in(reg) &readback as *const _ as u64, options(readonly, nostack));
    }
    crate::serial::write_str("[GDTR] limit="); crate::serial::write_hex(readback.limit as u64);
    crate::serial::write_str(" base="); crate::serial::write_hex(readback.base); crate::serial::write_nl();

    // Read back TSS descriptor entries to confirm patch
    unsafe {
        let entry5 = core::ptr::read_volatile((gdt_virt + 40) as *const u64);
        let entry6 = core::ptr::read_volatile((gdt_virt + 48) as *const u64);
        // Extract base address from patched descriptor
        let base_0_23 = (entry5 >> 16) & 0xFF_FFFF;
        let base_24_31 = (entry5 >> 56) & 0xFF;
        let base_32_63 = entry6 & 0xFFFF_FFFF;
        let reconstructed_base = base_0_23 | (base_24_31 << 24) | (base_32_63 << 32);
        crate::serial::write_str("[TSS] verified base="); crate::serial::write_hex(reconstructed_base); crate::serial::write_nl();
    }
}
