#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod gdt;
mod idt;
mod interrupts;
mod memory;
mod process;
mod serial;
mod panic;
mod syscall;
mod elf;

use indo_core::BootInfo;

use serial::{write_str, write_hex, write_nl, write_str_nl};

pub static mut TEST_RSP: u64 = 0;
pub static mut CAPTURED_RSP: u64 = 0;

/// Static IRET frame buffer (256 bytes, page-aligned).
/// Used to eliminate ANY stack-related issues with iretq.
#[repr(C, align(256))]
struct IretFrame {
    rip: u64,
    cs: u64,
    rflags: u64,
    _pad: [u64; 29],
}

static mut IRET_BUF: IretFrame = IretFrame {
    rip: 0,
    cs: 0,
    rflags: 0,
    _pad: [0; 29],
};

#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn test_target() -> ! {
    core::arch::naked_asm!(
        "cli",
        "mov rax, rsp",
        "lea rbx, [rip + {global}]",
        "mov [rbx], rax",
        "1:",
        "hlt",
        "jmp 1b",
        global = sym TEST_RSP,
    );
}

pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

#[no_mangle]
pub extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    let bi = unsafe { &*boot_info };

    write_str_nl("[KERNEL] INDOMINUS OS — IRETQ test v3");
    write_str("[KERNEL] Kernel phys: ");
    write_hex(bi.kernel_phys_start.as_u64());
    write_str(" .. ");
    write_hex(bi.kernel_phys_end.as_u64());
    write_nl();

    unsafe {
        crate::memory::set_kernel_phys_start(bi.kernel_phys_start.as_u64());
    }

    gdt::init();
    crate::memory::pmm::init(&bi.memory_map);
    let new_pml4 = crate::memory::vmm::init_kernel_page_tables(
        bi.kernel_phys_start.as_u64(),
        bi.kernel_phys_end.as_u64(),
    );
    unsafe { crate::memory::vmm::switch_page_table(new_pml4); }
    unsafe {
        crate::memory::init_heap(
            crate::memory::KERNEL_HEAP_BASE,
            crate::memory::KERNEL_HEAP_INITIAL_SIZE,
        );
    }
    idt::init();
    interrupts::init();

    write_str_nl("[TEST] All init done.");

    // ── Capture CPU state BEFORE iretq ──────────────────────────────
    let cr3_val: u64;
    let cs_val: u16;
    let ss_val: u16;
    let rflags_val: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3_val);
        core::arch::asm!("mov {}, cs", out(reg) cs_val);
        core::arch::asm!("mov {}, ss", out(reg) ss_val);
        core::arch::asm!("pushfq; pop {}", out(reg) rflags_val);
    }
    write_str("[TEST] BEFORE iretq: CR3="); write_hex(cr3_val); write_nl();
    write_str("[TEST] BEFORE iretq: CS="); write_hex(cs_val as u64); write_nl();
    write_str("[TEST] BEFORE iretq: SS="); write_hex(ss_val as u64); write_nl();
    write_str("[TEST] BEFORE iretq: RFLAGS="); write_hex(rflags_val); write_nl();

    // Get values we need BEFORE entering the all-asm section
    let target_virt = unsafe { crate::memory::phys_to_kernel_virt(test_target as *const () as u64) };
    write_str("[TEST] target virt=");
    write_hex(target_virt);
    write_nl();

    let rsp_val: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp_val); }
    write_str("[TEST] RSP=");
    write_hex(rsp_val);
    write_nl();

    // What does phys_to_kernel_virt give for the actual function pointer?
    let raw_fn = test_target as *const () as u64;
    write_str("[TEST] test_target raw (PIC) ="); write_hex(raw_fn); write_nl();
    write_str("[TEST] target_virt          ="); write_hex(target_virt); write_nl();

    // Verify IRET_BUF address from Rust (PIC perspective)
    let iret_buf_addr = unsafe { core::ptr::addr_of!(IRET_BUF) } as u64;
    write_str("[TEST] IRET_BUF addr (PIC) ="); write_hex(iret_buf_addr); write_nl();

    // Verify what the asm will compute: lea r9, [rip + IRET_BUF]
    // RIP after lea = iret_buf_addr (roughly), offset from rip to IRET_BUF = 0
    // Actually: the asm computes the PIC address of IRET_BUF via RIP-relative
    let iret_buf_check: u64;
    unsafe { core::arch::asm!("lea {}, [rip + {b}]", out(reg) iret_buf_check, b = sym IRET_BUF); }
    write_str("[TEST] IRET_BUF (asm lea)  ="); write_hex(iret_buf_check); write_nl();

    write_str_nl("[TEST] Entering all-asm IRETQ...");

    // ═══════════════════════════════════════════════════════════════════
    // STATIC BUFFER IRETQ: Use IRET_BUF static instead of stack.
    // Point RSP at the 256-byte aligned static buffer, write frame there,
    // then iretq. This eliminates ANY stack page mapping issues.
    // ═══════════════════════════════════════════════════════════════════
    unsafe {
        core::arch::asm!(
            "cli",
            // Load address of IRET_BUF into r9
            "lea r9, [rip + {irt_buf}]",
            // Write frame to the static buffer
            "mov qword ptr [r9 + 0x00], {rip}",
            "mov qword ptr [r9 + 0x08], 0x08",
            "mov qword ptr [r9 + 0x10], 0x200",
            // Fill in RSP and SS at offsets +24/+32 in case iretq does privilege change
            "mov qword ptr [r9 + 0x18], {kstack}",
            "mov qword ptr [r9 + 0x20], 0x10",
            // Save buffer address to CAPTURED_RSP for diagnostics
            "lea r10, [rip + {rsp_save}]",
            "mov [r10], r9",
            // Point RSP at the static buffer and iretq
            "mov rsp, r9",
            "iretq",
            rip = in(reg) target_virt,
            kstack = in(reg) rsp_val,
            rsp_save = sym CAPTURED_RSP,
            irt_buf = sym IRET_BUF,
            options(noreturn),
        );
    }
}
