#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod cpu;
mod gdt;
mod idt;
mod interrupts;
mod keyboard;
mod memory;
mod process;
mod serial;
mod panic;
mod syscall;
mod elf;
mod vfs;
mod initrd;
mod acpi;
mod mmio;
mod pci;
mod debug;
pub mod sync_cell;

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

    // Reserve the kernel's physical memory in the PMM.
    // pmm::init() uses linker symbols (__kernel_start/__kernel_end) for this,
    // but those are upper-half virtual addresses (0xFFFFFFFF80000000+) which
    // are beyond the PMM's tracking range. Use the correct physical addresses
    // from BootInfo instead.
    crate::memory::pmm::mark_region_used(
        bi.kernel_phys_start.as_u64(),
        bi.kernel_phys_end.as_u64(),
    );

    // Detect CPU features (before page tables, while identity map is live)
    crate::cpu::detect();
    crate::cpu::print_features();
    crate::cpu::enable_smep_smap();

    let new_pml4 = crate::memory::vmm::init_kernel_page_tables(
        bi.kernel_phys_start.as_u64(),
        bi.kernel_phys_end.as_u64(),
    );
    unsafe {
        crate::memory::vmm::switch_page_table(new_pml4);
        crate::memory::set_kernel_pml4_phys(new_pml4.as_u64());
    }
    // Now the kernel higher-half is mapped. Switch GDTR to virtual address
    // so it survives CR3 switches to user PML4s (which lack the identity map).
    write_str_nl("[MARK] Before switch_gdt_to_virtual");
    crate::gdt::switch_gdt_to_virtual();
    write_str_nl("[MARK] After switch_gdt_to_virtual");
    unsafe {
        crate::memory::init_heap(
            crate::memory::KERNEL_HEAP_BASE,
            crate::memory::KERNEL_HEAP_INITIAL_SIZE,
        );
    }
    write_str_nl("[MARK] After init_heap");

    write_str_nl("[MARK] Before IDT init");
    idt::init();
    write_str_nl("[MARK] After IDT init");

    // Initialize ACPI (after heap init, needs Vec)
    // Use RSDP from bootloader if available, otherwise scan memory
    write_str_nl("[MARK] Before ACPI init");
    let rsdp_from_boot = bi.rsdp_addr.as_u64();
    crate::acpi::init(if rsdp_from_boot != 0 { Some(rsdp_from_boot) } else { None });
    write_str_nl("[MARK] After ACPI init");

    // Enumerate PCI devices
    write_str_nl("[MARK] Before PCI enumerate");
    crate::pci::enumerate();
    write_str_nl("[MARK] After PCI enumerate");

    write_str_nl("[MARK] Before interrupts init");
    let (lapic_phys, ioapic_phys, ioapic_gsi_base) = match crate::acpi::madt_info() {
        Some(madt) => {
            let ioapic_phys = if madt.io_apic_addr != 0 { madt.io_apic_addr } else { 0xFEC0_0000 };
            (madt.local_apic_addr, ioapic_phys, madt.io_apic_gsi_base)
        }
        None => (0xFEE0_0000, 0xFEC0_0000, 0),
    };
    interrupts::init(lapic_phys, ioapic_phys, ioapic_gsi_base);
    write_str_nl("[MARK] After interrupts init");

    // Initialize keyboard driver (after interrupts, before processes)
    write_str_nl("[MARK] Before keyboard init");
    keyboard::init();
    write_str_nl("[MARK] After keyboard init");

    // Initialize syscall MSRs (STAR, LSTAR, SFMASK, EFER SCE, GSBase)
    write_str_nl("[MARK] Before syscall init");
    crate::syscall::init();
    write_str_nl("[MARK] After syscall init");

    // Harden the identity map: set NX on all identity-mapped pages.
    // This prevents code execution via the identity map while keeping it
    // functional for data access (needed to walk user page tables at runtime).
    write_str_nl("[MARK] Before harden_identity_map");
    crate::memory::vmm::harden_identity_map(new_pml4);
    write_str_nl("[MARK] After harden_identity_map");

    write_str_nl("[MARK] Before process init");
    crate::process::init();
    write_str_nl("[MARK] After process init");

    // Initialize VFS and load initrd
    write_str_nl("[MARK] Before VFS init");
    crate::vfs::init();
    write_str_nl("[MARK] After VFS init");
    write_str_nl("[MARK] Before initrd load");
    let initrd_data = include_bytes!("../initrd.img");
    crate::initrd::load_initrd(initrd_data);
    write_str_nl("[MARK] After initrd load");

    write_str_nl("[KERNEL] All init done.");

    // Phase 9: Spawn the shell from VFS.
    // PID 0 = idle, PID 1 = init/reaper (kernel-mode).
    // The shell is loaded from /bin/indosh in the initrd (VFS).
    // parent=Some(1) means PID 1 reaps the shell when it exits.
    match crate::vfs::vfs().read_file("/bin/indosh") {
        Ok(shell_elf) => {
            write_str("[KERNEL] Shell binary found: ");
            write_hex(shell_elf.len() as u64);
            write_str_nl(" bytes");
            match crate::process::spawn_user(&shell_elf, Some(1)) {
                Some(pid) => {
                    write_str("[KERNEL] Shell spawned as PID=");
                    write_hex(pid);
                    write_nl();
                }
                None => {
                    write_str_nl("[KERNEL] FAILED to spawn shell (no slot)");
                }
            }
        }
        Err(e) => {
            write_str("[KERNEL] WARNING: /bin/indosh read failed, errno=");
            write_hex(e.to_errno() as u64);
            write_nl();
            // Also try /indosh (flat path in case nested create failed)
            match crate::vfs::vfs().read_file("/indosh") {
                Ok(shell_elf) => {
                    write_str("[KERNEL] Found /indosh (flat): ");
                    write_hex(shell_elf.len() as u64);
                    write_str_nl(" bytes");
                    match crate::process::spawn_user(&shell_elf, Some(1)) {
                        Some(pid) => {
                            write_str("[KERNEL] Shell spawned as PID=");
                            write_hex(pid);
                            write_nl();
                        }
                        None => {
                            write_str_nl("[KERNEL] FAILED to spawn shell (no slot)");
                        }
                    }
                }
                Err(e2) => {
                    write_str("[KERNEL] /indosh also failed, errno=");
                    write_hex(e2.to_errno() as u64);
                    write_nl();
                    write_str_nl("[KERNEL] Falling back to test binaries");
                }
            }
            // Fallback: spawn test binaries if shell not available
            let tests: &[&[u8]] = &[
                include_bytes!("../test1_normal.bin"),
                include_bytes!("../test2_multi.bin"),
                include_bytes!("../test3_null_deref.bin"),
                include_bytes!("../test4_invalid_ptr.bin"),
                include_bytes!("../test5_unmapped.bin"),
                include_bytes!("../test6_null_ptr.bin"),
                include_bytes!("../test7_bad_syscall.bin"),
                include_bytes!("../test8_sleep.bin"),
                include_bytes!("../test9_stack_overflow.bin"),
                include_bytes!("../test10_errno.bin"),
            ];
            for (i, test_elf) in tests.iter().enumerate() {
                write_str("[KERNEL] Test ");
                write_hex(i as u64 + 1);
                write_str(" ELF size=");
                write_hex(test_elf.len() as u64);
                write_nl();
                match crate::process::spawn_user(test_elf, Some(0)) {
                    Some(pid) => {
                        write_str("[KERNEL]   -> PID=");
                        write_hex(pid);
                        write_nl();
                    }
                    None => {
                        write_str("[KERNEL]   -> FAILED (no slot)\n");
                    }
                }
            }
        }
    }

    crate::process::start_scheduler();
}
