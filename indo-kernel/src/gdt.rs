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
static mut TSS: TaskStateSegment = TaskStateSegment::new();

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
    TSS.privilege_stack_table[0] = VirtAddr::new(rsp0);
}

/// Get the current TSS RSP0 value.
pub fn get_tss_rsp0() -> u64 {
    unsafe { TSS.privilege_stack_table[0].as_u64() }
}

// ─────────────────────────────────────────────────────────────────────────────
// Initialization
// ─────────────────────────────────────────────────────────────────────────────

/// Initialize and load the GDT.
///
/// Must be called before enabling interrupts or setting up syscalls.
pub fn init() {
    // ── Configure TSS ───────────────────────────────────────────────────────
    unsafe {
        // IST[0] = double-fault stack (top, since stacks grow downward)
        TSS.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            let stack_start = VirtAddr::from_ptr(&raw const DOUBLE_FAULT_STACK);
            stack_start + IST_STACK_SIZE as u64
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

        let tss_selector = gdt.append(unsafe { Descriptor::tss_segment(&TSS) });

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

    // ── Load GDT ────────────────────────────────────────────────────────────
    gdt.load();

    // ── Reload segment registers ────────────────────────────────────────────
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
