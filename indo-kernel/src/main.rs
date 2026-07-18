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

use serial::{write_str, write_hex, write_u64, write_nl, write_str_nl, write_byte};

// ─────────────────────────────────────────────────────────────────────────────
// Test tasks — naked, no compiler-generated code
// ─────────────────────────────────────────────────────────────────────────────

/// RSP the CPU actually has when it first enters task A.
pub static mut TASK_A_RSP: u64 = 0;

/// RSP the CPU actually has when it first enters task B.
pub static mut TASK_B_RSP: u64 = 0;

/// Naked task A: first instruction saves RSP to a global.
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn task_a_main() -> ! {
    core::arch::naked_asm!(
        "1:",
        "mov rax, rsp",
        "lea rbx, [rip + {global}]",
        "mov [rbx], rax",
        "hlt",
        "jmp 1b",
        global = sym TASK_A_RSP,
    );
}

/// Naked task B: first instruction saves RSP to a global.
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn task_b_main() -> ! {
    core::arch::naked_asm!(
        "1:",
        "mov rax, rsp",
        "lea rbx, [rip + {global}]",
        "mov [rbx], rax",
        "hlt",
        "jmp 1b",
        global = sym TASK_B_RSP,
    );
}

/// Halt the CPU forever.
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Kernel entry point
// ─────────────────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    let bi = unsafe { &*boot_info };

    write_str_nl("[KERNEL] INDOMINUS OS — Phase 5");
    write_str("[KERNEL] Kernel phys: ");
    write_hex(bi.kernel_phys_start.as_u64());
    write_str(" .. ");
    write_hex(bi.kernel_phys_end.as_u64());
    write_nl();

    unsafe {
        crate::memory::set_kernel_phys_start(bi.kernel_phys_start.as_u64());
    }

    // GDT
    write_str_nl("[KERNEL] Initializing GDT...");
    gdt::init();
    write_str_nl("[KERNEL] GDT done");

    // Memory: PMM, VMM, heap
    write_str_nl("[KERNEL] Initializing PMM...");
    crate::memory::pmm::init(&bi.memory_map);
    write_str_nl("[KERNEL] PMM done");

    write_str_nl("[KERNEL] Initializing VMM...");
    let new_pml4 = crate::memory::vmm::init_kernel_page_tables(
        bi.kernel_phys_start.as_u64(),
        bi.kernel_phys_end.as_u64(),
    );
    write_str_nl("[KERNEL] VMM done, switching CR3...");

    // Switch to our new page tables (they include identity map, so this is safe)
    unsafe {
        crate::memory::vmm::switch_page_table(new_pml4);
    }
    write_str_nl("[KERNEL] CR3 switched");

    write_str_nl("[KERNEL] Initializing heap...");
    write_str("[KERNEL] Heap base ptr: ");
    write_hex(crate::memory::KERNEL_HEAP_BASE);
    write_nl();
    unsafe {
        crate::memory::init_heap(
            crate::memory::KERNEL_HEAP_BASE,
            crate::memory::KERNEL_HEAP_INITIAL_SIZE,
        );
    }
    write_str_nl("[KERNEL] Heap done");

    // IDT (must be after GDT)
    write_str_nl("[KERNEL] Initializing IDT...");
    idt::init();
    write_str_nl("[KERNEL] IDT done");

    // Interrupts: LAPIC, IO-APIC, PIT
    write_str_nl("[KERNEL] Initializing interrupts...");
    interrupts::init();
    write_str_nl("[KERNEL] Interrupts done");

    // Register keyboard handler (IRQ1 → vector 33)
    crate::interrupts::dispatch::register(1, keyboard_handler);

    // Process subsystem
    process::init();

    // Spawn naked tasks
    let pid_a = process::spawn(task_a_main as *const () as u64);
    let pid_b = process::spawn(task_b_main as *const () as u64);

    // Start scheduler — never returns
    process::start_scheduler();
}

/// Keyboard interrupt handler (IRQ1, vector 33).
fn keyboard_handler() {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0x60);
        let scancode: u8 = port.read();

        if scancode & 0x80 == 0 {
            crate::kprintln!("[KBD] Scancode: {:#04x}", scancode);
        }
    }
}
