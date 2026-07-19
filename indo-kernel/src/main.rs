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
mod debug;

use indo_core::BootInfo;

use serial::{write_str, write_hex, write_nl, write_str_nl};

pub static mut CAPTURED_RSP: u64 = 0;

pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

#[no_mangle]
pub extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    let bi = unsafe { &*boot_info };

    write_str_nl("[KERNEL] INDOMINUS OS -- scheduler test");
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
    unsafe { crate::interrupts::lapic::mask_lapic_timer(); }

    write_str_nl("[KERNEL] All init done.");

    // ── Mask timer: IRQ0 at IO-APIC ───────────────────────────────
    unsafe { crate::interrupts::ioapic::mask_irq(0); }
    write_str_nl("[KERNEL] IRQ0 masked — timer disabled.");

    // ── Minimal iretq test ────────────────────────────────────────
    // Goal: kernel → build IRET frame → iretq → task prints → hlt.
    // No scheduler, no timer, no process subsystem.
    let stack_layout = core::alloc::Layout::from_size_align(
        crate::process::KERNEL_STACK_SIZE, 16,
    ).expect("bad layout");
    let stack_base = unsafe { alloc::alloc::alloc(stack_layout) as u64 };
    assert!(stack_base != 0, "kernel stack alloc failed");
    unsafe {
        core::ptr::write_bytes(stack_base as *mut u8, 0, crate::process::KERNEL_STACK_SIZE);
    }
    let stack_top = stack_base + crate::process::KERNEL_STACK_SIZE as u64;

    // Build the IRET frame: [RIP] [CS] [RFLAGS] [RSP] [SS]
    // With PIC, fn pointers contain physical addresses → convert to virtual.
    let entry_phys = test_task as *const () as u64;
    let entry_virt = unsafe { crate::memory::phys_to_kernel_virt(entry_phys) };
    let frame_base = stack_top - 5 * 8;
    unsafe {
        let f = frame_base as *mut u64;
        f.add(0).write(entry_virt);  // RIP
        f.add(1).write(0x08);        // CS  = kernel code (DPL=0, RPL=0)
        f.add(2).write(0x202);       // RFLAGS = IF=1
        f.add(3).write(stack_top);   // RSP = top of kernel stack
        f.add(4).write(0x10);        // SS  = kernel data (DPL=0, RPL=0)
    }

    write_str("[KERNEL] IRET frame = 0x"); write_hex(frame_base); write_nl();
    write_str("[KERNEL] RIP       = 0x"); write_hex(entry_virt); write_nl();
    write_str("[KERNEL] RSP after = 0x"); write_hex(stack_top); write_nl();

    // Execute iretq: load RSP with frame, CPU pops 5 qwords, jumps to task.
    write_str_nl("[KERNEL] About to iretq...");
    unsafe {
        core::arch::asm!(
            "mov rsp, {f}",
            "iretq",
            f = in(reg) frame_base,
            options(noreturn)
        );
    }
}

/// Test task: reached via iretq. Prints a message, halts forever.
#[no_mangle]
fn test_task() -> ! {
    crate::serial::write_str("[TASK] iretq succeeded — RSP is valid!\n");
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
