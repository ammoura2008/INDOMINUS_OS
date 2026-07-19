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
    // Now the kernel higher-half is mapped. Switch GDTR to virtual address
    // so it survives CR3 switches to user PML4s (which lack the identity map).
    crate::gdt::switch_gdt_to_virtual();
    unsafe {
        crate::memory::init_heap(
            crate::memory::KERNEL_HEAP_BASE,
            crate::memory::KERNEL_HEAP_INITIAL_SIZE,
        );
    }
    idt::init();
    interrupts::init();

    // Initialize syscall MSRs (STAR, LSTAR, SFMASK, EFER SCE, GSBase)
    crate::syscall::init();

    write_str_nl("[KERNEL] All init done.");

    crate::process::init();
    crate::process::spawn(crate::process::tasks::task_a as *const () as u64);
    crate::process::spawn(crate::process::tasks::task_b as *const () as u64);

    // Load and spawn the user test ELF program
    let user_elf: &[u8] = include_bytes!("../user_test.bin");
    write_str("[KERNEL] User test ELF size: ");
    write_hex(user_elf.len() as u64);
    write_nl();
    match crate::process::spawn_user(user_elf) {
        Some(pid) => {
            write_str("[KERNEL] Spawned user process PID=");
            write_hex(pid);
            write_nl();
        }
        None => {
            write_str_nl("[KERNEL] Failed to spawn user process");
        }
    }
    match crate::process::spawn_user(user_elf) {
        Some(pid) => {
            write_str("[KERNEL] Spawned user process PID=");
            write_hex(pid);
            write_nl();
        }
        None => {
            write_str_nl("[KERNEL] Failed to spawn user process");
        }
    }

    crate::process::start_scheduler();
}
