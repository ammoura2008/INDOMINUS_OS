//! # Paging Audit
//!
//! Diagnostic tool for verifying the virtual memory state.
//!
//! This module does NOT modify any paging structures. It only reads
//! and reports the current state for debugging purposes.
//!
//! ## What it checks
//!
//! 1. Current CR3 (PML4 physical address)
//! 2. Kernel virtual address translation
//! 3. Higher-half mapping verification
//! 4. Identity mapping verification
//! 5. Stack mapping verification
//! 6. phys_to_virt / virt_to_phys consistency

use crate::serial::{write_hex, write_nl, write_str, write_str_nl};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PageTable, PageTableFlags, PageTableIndex};
use x86_64::{PhysAddr, VirtAddr};

/// Run the full paging audit. Safe to call at any time — no modifications.
///
/// `kernel_main_phys` — the PIC (physical) address of kernel_main.
pub fn run_paging_audit(kernel_main_phys: u64) {
    write_str_nl("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    write_str_nl("  PAGING AUDIT");
    write_str_nl("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    audit_cr3();
    audit_kernel_translation(kernel_main_phys);
    audit_higher_half_mapping();
    audit_identity_mapping();
    audit_stack_mapping();
    audit_phys_virt_consistency(kernel_main_phys);

    write_str_nl("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    write_str_nl("  PAGING AUDIT COMPLETE");
    write_str_nl("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. CR3 audit
// ─────────────────────────────────────────────────────────────────────────────

fn audit_cr3() {
    write_str_nl("── 1. CR3 (PML4 physical address) ──");

    let (cr3_frame, cr3_flags) = Cr3::read();
    let cr3_phys = cr3_frame.start_address().as_u64();

    write_str("  CR3 = 0x");
    write_hex(cr3_phys);
    write_nl();

    write_str("  CR3 flags: ");
    if cr3_flags.contains(x86_64::registers::control::Cr3Flags::PAGE_LEVEL_CACHE_DISABLE) {
        write_str("PCD ");
    }
    if cr3_flags.contains(x86_64::registers::control::Cr3Flags::PAGE_LEVEL_WRITETHROUGH) {
        write_str("PWT ");
    }
    write_nl();

    if cr3_phys < 0x1_0000_0000 {
        write_str("  NOTE: CR3 is in identity-mapped range (< 4 GiB)");
        write_nl();
    }

    // Read and display PML4 entries
    write_str_nl("  PML4 entries (non-empty):");
    let pml4 = unsafe { &*(cr3_phys as *const PageTable) };
    for (i, entry) in pml4.iter().enumerate() {
        if !entry.is_unused() {
            let addr = entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0);
            let flags = entry.flags();
            write_str("    [");
            if i < 10 { write_str("  "); } else if i < 100 { write_str(" "); }
            write_hex(i as u64);
            write_str("] → 0x");
            write_hex(addr);
            write_str("  flags=");
            if flags.contains(PageTableFlags::PRESENT) { write_str("P "); }
            if flags.contains(PageTableFlags::WRITABLE) { write_str("W "); }
            if flags.contains(PageTableFlags::USER_ACCESSIBLE) { write_str("U "); }
            if flags.contains(PageTableFlags::ACCESSED) { write_str("A "); }
            if flags.contains(PageTableFlags::DIRTY) { write_str("D "); }
            if flags.contains(PageTableFlags::HUGE_PAGE) { write_str("PS "); }
            if flags.contains(PageTableFlags::NO_EXECUTE) { write_str("NX "); }
            write_nl();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Kernel virtual address translation
// ─────────────────────────────────────────────────────────────────────────────

fn audit_kernel_translation(kernel_main_phys: u64) {
    write_str_nl("── 2. Kernel virtual address translation ──");

    let kernel_main_virt = unsafe {
        crate::memory::phys_to_kernel_virt(kernel_main_phys)
    };

    write_str("  kernel_main physical (PIC): 0x");
    write_hex(kernel_main_phys);
    write_nl();

    write_str("  kernel_main virtual (computed): 0x");
    write_hex(kernel_main_virt);
    write_nl();

    // Try to translate via page tables
    let (cr3_frame, _) = Cr3::read();
    let pml4_phys = cr3_frame.start_address().as_u64();
    let pml4 = unsafe { &*(pml4_phys as *const PageTable) };

    let virt = VirtAddr::new(kernel_main_virt);
    match translate_via_pml4(pml4, virt) {
        Some(phys) => {
            write_str("  Page table translates to: 0x");
            write_hex(phys.as_u64());
            write_nl();
            if phys.as_u64() == kernel_main_phys {
                write_str_nl("  MATCH: page table maps virtual → expected physical");
            } else {
                write_str_nl("  MISMATCH: page table maps virtual → DIFFERENT physical");
                write_str("    Expected: 0x");
                write_hex(kernel_main_phys);
                write_nl();
            }
        }
        None => {
            write_str_nl("  NOT MAPPED: kernel_main virtual address has no page table entry!");
            write_str_nl("  This means the higher-half mapping is NOT working.");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Higher-half mapping check
// ─────────────────────────────────────────────────────────────────────────────

fn audit_higher_half_mapping() {
    write_str_nl("── 3. Higher-half mapping check ──");

    let (cr3_frame, _) = Cr3::read();
    let pml4_phys = cr3_frame.start_address().as_u64();
    let pml4 = unsafe { &*(pml4_phys as *const PageTable) };

    // Compute PML4/PDPT/PD/PT indices from the target address
    let addr = 0xFFFF_FFFF_8000_0000u64;
    let pml4_i = ((addr >> 39) & 0x1FF) as u16;
    let pdpt_i = ((addr >> 30) & 0x1FF) as u16;
    let pd_i   = ((addr >> 21) & 0x1FF) as u16;
    let pt_i   = ((addr >> 12) & 0x1FF) as u16;

    write_str("  PML4 index: ");
    write_hex(pml4_i as u64);
    write_nl();

    let entry = &pml4[PageTableIndex::new(pml4_i)];
    write_str("  PML4[");
    write_hex(pml4_i as u64);
    write_str("] (for 0xFFFF_FFFF_8000_0000): ");
    if entry.is_unused() {
        write_str_nl("EMPTY — higher-half NOT mapped!");
    } else {
        let pdpt_phys = entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0);
        write_str("→ PDPT at 0x");
        write_hex(pdpt_phys);
        write_nl();

        let pdpt = unsafe { &*(pdpt_phys as *const PageTable) };
        write_str("  PDPT index: ");
        write_hex(pdpt_i as u64);
        write_nl();

        let pdpt_entry = &pdpt[PageTableIndex::new(pdpt_i)];
        if pdpt_entry.is_unused() {
            write_str_nl("  PDPT entry is EMPTY — mapping incomplete");
        } else if pdpt_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            write_str("  PDPT entry is a 1 GiB huge page → 0x");
            write_hex(pdpt_entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0));
            write_nl();
        } else {
            let pd_phys = pdpt_entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0);
            write_str("  PDPT → PD at 0x");
            write_hex(pd_phys);
            write_nl();

            let pd = unsafe { &*(pd_phys as *const PageTable) };
            write_str("  PD index: ");
            write_hex(pd_i as u64);
            write_nl();

            let pd_entry = &pd[PageTableIndex::new(pd_i)];
            if pd_entry.is_unused() {
                write_str_nl("  PD entry is EMPTY — mapping incomplete");
            } else if pd_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                write_str("  PD entry is a 2 MiB huge page → 0x");
                write_hex(pd_entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0));
                write_nl();
            } else {
                let pt_phys = pd_entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0);
                write_str("  PD → PT at 0x");
                write_hex(pt_phys);
                write_nl();

                let pt = unsafe { &*(pt_phys as *const PageTable) };
                write_str("  PT index: ");
                write_hex(pt_i as u64);
                write_nl();

                let pt_entry = &pt[PageTableIndex::new(pt_i)];
                if pt_entry.is_unused() {
                    write_str_nl("  PT entry is EMPTY — page not mapped");
                } else {
                    write_str("  PT → physical 0x");
                    write_hex(pt_entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0));
                    write_nl();
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Identity mapping check
// ─────────────────────────────────────────────────────────────────────────────

fn audit_identity_mapping() {
    write_str_nl("── 4. Identity mapping check ──");

    let (cr3_frame, _) = Cr3::read();
    let pml4_phys = cr3_frame.start_address().as_u64();
    let pml4 = unsafe { &*(pml4_phys as *const PageTable) };

    // PML4 index for 0x00000000: index 0
    let entry = &pml4[PageTableIndex::new(0)];
    write_str("  PML4[0] (for identity map): ");
    if entry.is_unused() {
        write_str_nl("EMPTY — identity map NOT present!");
    } else {
        let pdpt_phys = entry.frame().map(|f| f.start_address().as_u64()).unwrap_or(0);
        write_str("→ PDPT at 0x");
        write_hex(pdpt_phys);
        write_nl();
        write_str_nl("  (identity map is present)");
    }

    // Test sample addresses
    let test_addrs: [u64; 4] = [
        0x0000_0000_0000_1000,
        0x0000_0000_000B_8000,
        0x0000_0000_FEE0_0000,
        0x0000_0000_F000_0000,
    ];

    for addr in test_addrs {
        let virt = VirtAddr::new(addr);
        match translate_via_pml4(pml4, virt) {
            Some(phys) => {
                write_str("  0x");
                write_hex(addr);
                write_str(" → 0x");
                write_hex(phys.as_u64());
                if phys.as_u64() == addr {
                    write_str_nl("  (identity: OK)");
                } else {
                    write_str_nl("  (NOT identity)");
                }
            }
            None => {
                write_str("  0x");
                write_hex(addr);
                write_str_nl("  NOT MAPPED");
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Stack mapping
// ─────────────────────────────────────────────────────────────────────────────

fn audit_stack_mapping() {
    write_str_nl("── 5. Stack mapping ──");

    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp); }

    write_str("  Current RSP: 0x");
    write_hex(rsp);
    write_nl();

    let (cr3_frame, _) = Cr3::read();
    let pml4_phys = cr3_frame.start_address().as_u64();
    let pml4 = unsafe { &*(pml4_phys as *const PageTable) };

    let virt = VirtAddr::new(rsp);
    match translate_via_pml4(pml4, virt) {
        Some(phys) => {
            write_str("  RSP physical: 0x");
            write_hex(phys.as_u64());
            write_nl();

            if phys.as_u64() == rsp {
                write_str_nl("  Stack is identity-mapped (physical == virtual)");
            } else {
                write_str_nl("  Stack is NOT identity-mapped");
            }

            let offset = rsp & 0xFFF;
            write_str("    Page offset: 0x");
            write_hex(offset);
            write_nl();
        }
        None => {
            write_str_nl("  RSP page NOT MAPPED — stack is invalid!");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. phys_to_virt / virt_to_phys consistency
// ─────────────────────────────────────────────────────────────────────────────

fn audit_phys_virt_consistency(kernel_main_phys: u64) {
    write_str_nl("── 6. phys_to_virt / virt_to_phys consistency ──");

    let kps = crate::memory::kernel_phys_start();
    let kvb = crate::memory::KERNEL_VIRT_BASE;

    write_str("  kernel_phys_start: 0x");
    write_hex(kps);
    write_nl();
    write_str("  KERNEL_VIRT_BASE:  0x");
    write_hex(kvb);
    write_nl();

    // Test round-trip
    let test_phys = kps + 0x1000;
    let test_virt = unsafe { crate::memory::phys_to_kernel_virt(test_phys) };
    let expected_virt = test_phys.wrapping_add(kvb).wrapping_sub(kps);

    write_str("  Test physical: 0x");
    write_hex(test_phys);
    write_nl();
    write_str("  phys_to_kernel_virt: 0x");
    write_hex(test_virt);
    write_nl();
    write_str("  Expected:            0x");
    write_hex(expected_virt);
    write_nl();

    if test_virt == expected_virt {
        write_str_nl("  MATCH: phys_to_kernel_virt is consistent");
    } else {
        write_str_nl("  MISMATCH: phys_to_kernel_virt is WRONG");
    }

    // Check identity mapping assumption
    write_str_nl("  Identity mapping assumption check:");
    let identity_virt = unsafe { crate::memory::vmm::phys_to_virt(0x1000) };
    write_str("    phys_to_virt(0x1000) = 0x");
    write_hex(identity_virt.as_u64());
    write_nl();

    if identity_virt.as_u64() == 0x1000 {
        write_str_nl("    OK: phys_to_virt returns identity (phys == virt)");
    } else {
        write_str_nl("    NOTE: phys_to_virt does NOT return identity");
    }

    // Check PIC address + offset = virtual address
    let kernel_main_virt_from_offset = kernel_main_phys.wrapping_add(kvb).wrapping_sub(kps);
    let kernel_main_virt_expected = unsafe {
        crate::memory::phys_to_kernel_virt(kernel_main_phys)
    };

    write_str("  kernel_main PIC addr:  0x");
    write_hex(kernel_main_phys);
    write_nl();
    write_str("  After +offset:         0x");
    write_hex(kernel_main_virt_from_offset);
    write_nl();
    write_str("  phys_to_kernel_virt:   0x");
    write_hex(kernel_main_virt_expected);
    write_nl();

    if kernel_main_virt_from_offset == kernel_main_virt_expected {
        write_str_nl("  MATCH: PIC + offset = phys_to_kernel_virt result");
    } else {
        write_str_nl("  MISMATCH: computation differs from phys_to_kernel_virt");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Manually walk the 4-level page table to translate virtual → physical.
fn translate_via_pml4(pml4: &PageTable, virt: VirtAddr) -> Option<PhysAddr> {
    let addr = virt.as_u64();

    let pml4_index = ((addr >> 39) & 0x1FF) as u16;
    let pdpt_index = ((addr >> 30) & 0x1FF) as u16;
    let pd_index   = ((addr >> 21) & 0x1FF) as u16;
    let pt_index   = ((addr >> 12) & 0x1FF) as u16;
    let offset     = (addr & 0xFFF) as u64;

    // PML4 → PDPT
    let pml4_entry = &pml4[PageTableIndex::new(pml4_index)];
    if pml4_entry.is_unused() { return None; }
    if pml4_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        let base = pml4_entry.frame().ok()?.start_address().as_u64();
        return Some(PhysAddr::new(base + (addr & 0x007FFF_FFC00000)));
    }
    let pdpt = unsafe { &*(pml4_entry.frame().ok()?.start_address().as_u64() as *const PageTable) };

    // PDPT → PD
    let pdpt_entry = &pdpt[PageTableIndex::new(pdpt_index)];
    if pdpt_entry.is_unused() { return None; }
    if pdpt_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        let base = pdpt_entry.frame().ok()?.start_address().as_u64();
        return Some(PhysAddr::new(base + (addr & 0x003FFF_FFE00000)));
    }
    let pd = unsafe { &*(pdpt_entry.frame().ok()?.start_address().as_u64() as *const PageTable) };

    // PD → PT
    let pd_entry = &pd[PageTableIndex::new(pd_index)];
    if pd_entry.is_unused() { return None; }
    if pd_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        let base = pd_entry.frame().ok()?.start_address().as_u64();
        return Some(PhysAddr::new(base + (addr & 0x001FFF_FF000000)));
    }
    let pt = unsafe { &*(pd_entry.frame().ok()?.start_address().as_u64() as *const PageTable) };

    // PT → Physical
    let pt_entry = &pt[PageTableIndex::new(pt_index)];
    if pt_entry.is_unused() { return None; }

    let phys = pt_entry.frame().ok()?.start_address().as_u64() + offset;
    Some(PhysAddr::new(phys))
}
