//! # indo-boot — INDOMINUS UEFI Bootloader
//!
//! ## What is a bootloader?
//!
//! When you press the power button, the CPU starts executing code at a
//! hardwired physical address (the reset vector). On modern x86 systems,
//! this runs UEFI firmware, which initializes hardware and then looks for
//! a bootloader: a specially-formatted executable on an EFI System Partition.
//!
//! Our bootloader's job is EXACTLY this (in order):
//!
//! 1. Greet UEFI and set up its logger
//! 2. Find and load the kernel ELF from the ESP
//! 3. Parse the kernel ELF and copy its segments into memory
//! 4. Query UEFI for the complete physical memory map
//! 5. Query the GOP framebuffer for early display
//! 6. Find the ACPI RSDP pointer
//! 7. Build the `BootInfo` struct with everything the kernel needs
//! 8. EXIT UEFI BOOT SERVICES — point of no return
//! 9. Jump to the kernel entry point

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;

use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as UefiPixelFormat};
use uefi::table::cfg;
use uefi::table::boot::{MemoryDescriptor, MemoryType};

use indo_core::{
    BootInfo, FramebufferInfo, MemoryMap, MemoryRegion, MemoryRegionKind,
    PhysAddr, PixelFormat, VirtAddr, BOOT_INFO_MAGIC, BOOT_PROTOCOL_VERSION, MAX_MEMORY_REGIONS,
};

const KERNEL_PATH_STR: &str = r"\EFI\INDOMINUS\kernel.elf";
const KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Write a byte to QEMU's debug console (port 0xE9). Output appears on the host terminal.
#[inline]
fn debugcon_write(byte: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0xE9u16, in("al") byte, options(nostack, nomem));
    }
}

fn debugcon_print(s: &str) {
    for b in s.bytes() {
        debugcon_write(b);
    }
}

#[entry]
fn efi_main(image: Handle, mut system_table: SystemTable<Boot>) -> Status {
    // Initialize the UEFI allocator so we can use Vec, Box, etc.
    unsafe { uefi::allocator::init(system_table.boot_services()); }

    // Initialize the UEFI logger so log::info! works.
    {
        let stdout_ptr: *mut uefi::proto::console::text::Output =
            system_table.stdout() as *mut _;
        let logger = unsafe { uefi::logger::Logger::new(&mut *stdout_ptr) };
        let logger_static: &'static uefi::logger::Logger = Box::leak(Box::new(logger));
        log::set_logger(logger_static).expect("Failed to set logger");
        log::set_max_level(log::LevelFilter::Info);
    }

    debugcon_print("[BOOT] Bootloader started\n");

    let boot_services = system_table.boot_services();

    // ── Step 1: Load the kernel ELF from the ESP ──────────────────────────
    debugcon_print("[BOOT] Loading kernel from ESP\n");
    let kernel_data = load_kernel_from_esp(boot_services, image)
        .expect("Failed to load kernel ELF from ESP");
    debugcon_print("[BOOT] Kernel ELF loaded\n");

    // ── Step 2: Parse the ELF and map it into memory ──────────────────────
    debugcon_print("[BOOT] Parsing kernel ELF\n");
    let (kernel_phys_start, kernel_phys_end, load_offset) = load_elf(boot_services, &kernel_data)
        .expect("Failed to parse and load kernel ELF");

    // ── Step 3: Get the kernel entry point from the ELF ───────────────────
    let kernel_entry_virt = get_elf_entry(&kernel_data)
        .expect("Failed to get kernel entry point from ELF");
    let kernel_entry_phys = (kernel_entry_virt as i64 + load_offset) as u64;
    debugcon_print("[BOOT] Kernel entry translated\n");

    // ── Step 4: Query the GOP framebuffer ─────────────────────────────────
    let framebuffer = query_framebuffer(boot_services)
        .unwrap_or_else(|_| FramebufferInfo {
            base:         PhysAddr::new(0),
            size:         0,
            width:        0,
            height:       0,
            stride:       0,
            pixel_format: PixelFormat::None,
        });

    // ── Step 5: Find ACPI RSDP ────────────────────────────────────────────
    let rsdp_addr = find_rsdp(&system_table);

    // ── Step 6: Exit UEFI Boot Services and get the memory map ────────────
    debugcon_print("[BOOT] Exiting UEFI Boot Services\n");
    let (_runtime_table, uefi_memory_map) = system_table.exit_boot_services();

    // Build our memory map directly from the UEFI iterator — no Vec allocation.
    let mut descriptors = [MemoryDescriptor::default(); MAX_MEMORY_REGIONS];
    let mut count = 0;
    for desc in uefi_memory_map.entries() {
        if count < MAX_MEMORY_REGIONS {
            descriptors[count] = desc.clone();
            count += 1;
        }
    }
    let memory_map = build_memory_map(&descriptors[..count]);

    // ── Step 7: Build the BootInfo struct ─────────────────────────────────
    // BootInfo is ~6KB; too large for the post-exit-boot-services stack.
    // Allocate it BEFORE exit_boot_services, then fill it after.
    // But we need memory_map after exit, so we build it on a static.
    static mut BOOT_INFO_MEM: BootInfo = BootInfo {
        magic:            0,
        protocol_version: 0,
        _pad:             0,
        memory_map: MemoryMap {
            regions: [MemoryRegion {
                start:  PhysAddr::new(0),
                length: 0,
                kind:   MemoryRegionKind::Reserved,
            }; MAX_MEMORY_REGIONS],
            count: 0,
        },
        framebuffer: FramebufferInfo {
            base:         PhysAddr::new(0),
            size:         0,
            width:        0,
            height:       0,
            stride:       0,
            pixel_format: PixelFormat::None,
        },
        rsdp_addr:       PhysAddr::new(0),
        kernel_phys_start: PhysAddr::new(0),
        kernel_phys_end:   PhysAddr::new(0),
        kernel_virt_base:  VirtAddr::new(0),
    };

    unsafe {
        BOOT_INFO_MEM.magic            = BOOT_INFO_MAGIC;
        BOOT_INFO_MEM.protocol_version = BOOT_PROTOCOL_VERSION;
        BOOT_INFO_MEM.memory_map       = memory_map;
        BOOT_INFO_MEM.framebuffer      = framebuffer;
        BOOT_INFO_MEM.rsdp_addr        = rsdp_addr;
        BOOT_INFO_MEM.kernel_phys_start = kernel_phys_start;
        BOOT_INFO_MEM.kernel_phys_end   = kernel_phys_end;
        BOOT_INFO_MEM.kernel_virt_base  = VirtAddr::new(KERNEL_VIRT_BASE);
    }

    let boot_info_ptr = unsafe { &BOOT_INFO_MEM as *const BootInfo };

    debugcon_print("[BOOT] Jumping to kernel\n");

    // ── Step 8: Jump to the kernel! ───────────────────────────────────────
    // IMPORTANT: The kernel targets x86_64-unknown-none which uses the System V
    // AMD64 ABI (first arg in RDI). The bootloader targets x86_64-unknown-uefi
    // which uses the Microsoft x64 ABI (first arg in RCX). We MUST use
    // "sysv64" ABI here so the boot_info pointer arrives in RDI as the kernel expects.
    let kernel_fn: extern "sysv64" fn(boot_info: *const BootInfo) -> ! =
        unsafe { core::mem::transmute(kernel_entry_phys as usize) };

    kernel_fn(boot_info_ptr);
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: Load kernel ELF from the EFI System Partition
// ─────────────────────────────────────────────────────────────────────────────

fn load_kernel_from_esp(
    boot_services: &BootServices,
    image: Handle,
) -> Result<Vec<u8>, &'static str> {
    let mut fs = boot_services
        .get_image_file_system(image)
        .map_err(|_| "Failed to open image file system")?;

    let path = uefi::cstr16!(r"\EFI\INDOMINUS\kernel.elf");
    let data = fs
        .read(path)
        .map_err(|_| "Failed to read kernel ELF from filesystem")?;

    Ok(data)
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: Parse ELF and load segments into memory
// ─────────────────────────────────────────────────────────────────────────────

fn elf_read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn elf_read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn elf_read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off], buf[off + 1], buf[off + 2], buf[off + 3],
        buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7],
    ])
}

fn elf_read_i64(buf: &[u8], off: usize) -> i64 {
    i64::from_le_bytes([
        buf[off], buf[off + 1], buf[off + 2], buf[off + 3],
        buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7],
    ])
}

/// Apply R_X86_64_RELATIVE relocations (type 8) directly into the loaded kernel image.
///
/// The kernel is compiled as PIC. Its `.got` and `.data.rel.ro` sections contain
/// addresses that need to be adjusted for where the kernel was physically loaded.
///
/// For R_X86_64_RELATIVE: `*target = B + r_addend`
/// where `B = base_phys - min_vaddr` (the relocation base).
///
/// We read relocation entries from the ELF data but write the patched values into
/// the allocated physical memory where the kernel actually lives.
fn apply_relocations(
    elf_data: &[u8],
    kernel_mem: *mut u8,
    kernel_mem_size: usize,
    base_phys: u64,
    min_vaddr: u64,
) -> Result<(), &'static str> {
    let e_shoff = elf_read_u64(elf_data, 40) as usize;
    let e_shentsize = elf_read_u16(elf_data, 58) as usize;
    let e_shnum = elf_read_u16(elf_data, 60) as usize;

    if e_shoff == 0 || e_shnum == 0 {
        return Ok(());
    }

    let mut relocated_count = 0u32;

    for i in 0..e_shnum {
        let off = e_shoff + i * e_shentsize;
        if off + e_shentsize > elf_data.len() {
            break;
        }
        let sh_type = elf_read_u32(elf_data, off + 4);

        // SHT_RELA = 4
        if sh_type != 4 {
            continue;
        }

        let sh_offset = elf_read_u64(elf_data, off + 24) as usize;
        let sh_size = elf_read_u64(elf_data, off + 32) as usize;
        let sh_entsize = elf_read_u64(elf_data, off + 56) as usize;
        if sh_entsize == 0 || sh_offset + sh_size > elf_data.len() {
            continue;
        }
        let count = sh_size / sh_entsize;

        for j in 0..count {
            let ent = sh_offset + j * sh_entsize;
            let r_offset = elf_read_u64(elf_data, ent);
            let r_info = elf_read_u64(elf_data, ent + 8);
            let r_addend = elf_read_i64(elf_data, ent + 16);

            // ELF64_R_TYPE(info) = info & 0xFFFFFFFF (type in lower 32 bits)
            let rel_type = (r_info & 0xFFFFFFFF) as u32;

            // R_X86_64_RELATIVE = 8
            if rel_type != 8 {
                continue;
            }

            // r_offset is the virtual address of the target.
            // Translate to offset within our kernel memory image.
            let target_offset = (r_offset as i64 - min_vaddr as i64) as usize;

            if target_offset + 8 > kernel_mem_size {
                continue;
            }

            // R_X86_64_RELATIVE: *P = B + A
            // B = base_phys - min_vaddr, A = r_addend
            // value = base_phys + (r_addend - min_vaddr)
            let value = (base_phys as i64 + r_addend - min_vaddr as i64) as u64;

            let bytes = value.to_le_bytes();
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    kernel_mem.add(target_offset),
                    8,
                );
            }
            relocated_count += 1;
        }
    }

    Ok(())
}
fn load_elf(
    boot_services: &BootServices,
    data: &[u8],
) -> Result<(PhysAddr, PhysAddr, i64), &'static str> {
    if data.len() < 4 || &data[0..4] != b"\x7FELF" {
        return Err("Not a valid ELF file (bad magic)");
    }
    if data[4] != 2 {
        return Err("Not a 64-bit ELF (ELFCLASS64 required)");
    }

    let e_phoff     = u64::from_le_bytes(data[32..40].try_into().unwrap()) as usize;
    let e_phentsize = u16::from_le_bytes(data[54..56].try_into().unwrap()) as usize;
    let e_phnum     = u16::from_le_bytes(data[56..58].try_into().unwrap()) as usize;

    // First pass: find the virtual address range and total physical size needed.
    let mut min_vaddr = u64::MAX;
    let mut max_vaddr = 0u64;
    let mut total_memsz = 0u64;

    for i in 0..e_phnum {
        let ph_offset = e_phoff + i * e_phentsize;
        if ph_offset + e_phentsize > data.len() {
            return Err("ELF program header out of bounds");
        }

        let ph = &data[ph_offset..ph_offset + e_phentsize];
        let p_type   = u32::from_le_bytes(ph[0..4].try_into().unwrap());
        let p_vaddr  = u64::from_le_bytes(ph[16..24].try_into().unwrap());
        let p_memsz  = u64::from_le_bytes(ph[40..48].try_into().unwrap());

        if p_type != 1 {
            continue;
        }

        min_vaddr = min_vaddr.min(p_vaddr);
        max_vaddr = max_vaddr.max(p_vaddr + p_memsz);
        total_memsz += p_memsz;
    }

    if min_vaddr == u64::MAX {
        return Err("ELF has no PT_LOAD segments");
    }

    // Align total size up to page boundary.
    let total_pages = ((total_memsz + 4095) / 4096) as usize;

    // Allocate a SINGLE contiguous block for all segments.
    // Using AnyPages gives us physical addresses; the relative offsets between
    // segments are preserved because we copy them to the correct positions.
    let base_phys = boot_services
        .allocate_pages(
            uefi::table::boot::AllocateType::AnyPages,
            MemoryType::LOADER_DATA,
            total_pages,
        )
        .map_err(|_| "Failed to allocate contiguous pages for kernel")?;

    // Zero the entire region (for BSS sections).
    unsafe {
        core::ptr::write_bytes(base_phys as *mut u8, 0, total_pages * 4096);
    }

    // Second pass: copy each segment to its correct position within the block.
    let mut phys_start = u64::MAX;
    let mut phys_end   = 0u64;

    for i in 0..e_phnum {
        let ph_offset = e_phoff + i * e_phentsize;
        let ph = &data[ph_offset..ph_offset + e_phentsize];
        let p_type   = u32::from_le_bytes(ph[0..4].try_into().unwrap());
        let p_offset = u64::from_le_bytes(ph[8..16].try_into().unwrap()) as usize;
        let p_vaddr  = u64::from_le_bytes(ph[16..24].try_into().unwrap());
        let p_filesz = u64::from_le_bytes(ph[32..40].try_into().unwrap()) as usize;
        let p_memsz  = u64::from_le_bytes(ph[40..48].try_into().unwrap()) as usize;

        if p_type != 1 {
            continue;
        }

        // Compute physical address: base + offset from first vaddr.
        let offset_in_block = p_vaddr - min_vaddr;
        let seg_phys = base_phys + offset_in_block;

        unsafe {
            let dest = seg_phys as *mut u8;
            core::ptr::copy_nonoverlapping(
                data.as_ptr().add(p_offset),
                dest,
                p_filesz,
            );
            // BSS is already zeroed from the initial write_bytes.
        }

        phys_start = phys_start.min(seg_phys);
        phys_end   = phys_end.max(seg_phys + p_memsz as u64);
    }

    // load_offset: virtual_addr + load_offset = physical_addr
    let load_offset = base_phys as i64 - min_vaddr as i64;

    // ── Apply ELF relocations ──────────────────────────────────────────────
    // With PIC (position-independent code), the kernel's GOT and data sections
    // contain addresses that need to be adjusted by the difference between the
    // physical load address and the linked virtual address.
    // IMPORTANT: We must patch the actual kernel memory, not the ELF data buffer.
    let kernel_mem_size = total_pages * 4096;
    apply_relocations(data, base_phys as *mut u8, kernel_mem_size, base_phys, min_vaddr)?;

    Ok((PhysAddr::new(phys_start), PhysAddr::new(phys_end), load_offset))
}

fn get_elf_entry(data: &[u8]) -> Result<u64, &'static str> {
    if data.len() < 24 {
        return Err("ELF too small to have entry point field");
    }
    Ok(u64::from_le_bytes(data[24..32].try_into().unwrap()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: Query GOP Framebuffer
// ─────────────────────────────────────────────────────────────────────────────

fn query_framebuffer(boot_services: &BootServices) -> Result<FramebufferInfo, &'static str> {
    let gop_handle = boot_services
        .get_handle_for_protocol::<GraphicsOutput>()
        .map_err(|_| "No GOP handle")?;

    let mut gop = boot_services
        .open_protocol_exclusive::<GraphicsOutput>(gop_handle)
        .map_err(|_| "Cannot open GOP")?;

    let mode_info = gop.current_mode_info();
    let mut fb = gop.frame_buffer();

    let pixel_format = match mode_info.pixel_format() {
        UefiPixelFormat::Rgb     => PixelFormat::Rgb,
        UefiPixelFormat::Bgr     => PixelFormat::Bgr,
        UefiPixelFormat::Bitmask => PixelFormat::Bitmask,
        _                        => PixelFormat::None,
    };

    let (width, height) = mode_info.resolution();
    let stride = mode_info.stride();

    Ok(FramebufferInfo {
        base:         PhysAddr::new(fb.as_mut_ptr() as u64),
        size:         fb.size() as u64,
        width:        width as u32,
        height:       height as u32,
        stride:       stride as u32,
        pixel_format,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: Find ACPI RSDP
// ─────────────────────────────────────────────────────────────────────────────

fn find_rsdp(system_table: &SystemTable<Boot>) -> PhysAddr {
    for entry in system_table.config_table() {
        if entry.guid == cfg::ACPI2_GUID {
            return PhysAddr::new(entry.address as u64);
        }
    }
    for entry in system_table.config_table() {
        if entry.guid == cfg::ACPI_GUID {
            return PhysAddr::new(entry.address as u64);
        }
    }
    PhysAddr::new(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: Build our MemoryMap from UEFI descriptors
// ─────────────────────────────────────────────────────────────────────────────

fn build_memory_map(uefi_map: &[MemoryDescriptor]) -> MemoryMap {
    let mut map = MemoryMap {
        regions: [MemoryRegion {
            start:  PhysAddr::new(0),
            length: 0,
            kind:   MemoryRegionKind::Reserved,
        }; MAX_MEMORY_REGIONS],
        count: 0,
    };

    for descriptor in uefi_map {
        if map.count >= MAX_MEMORY_REGIONS {
            break;
        }

        let kind = match descriptor.ty {
            MemoryType::CONVENTIONAL         => MemoryRegionKind::Usable,
            MemoryType::BOOT_SERVICES_CODE
            | MemoryType::BOOT_SERVICES_DATA => MemoryRegionKind::BootloaderCode,
            MemoryType::LOADER_CODE
            | MemoryType::LOADER_DATA        => MemoryRegionKind::KernelCode,
            MemoryType::ACPI_RECLAIM         => MemoryRegionKind::AcpiReclaimable,
            MemoryType::ACPI_NON_VOLATILE    => MemoryRegionKind::AcpiNvs,
            MemoryType::MMIO
            | MemoryType::MMIO_PORT_SPACE    => MemoryRegionKind::Mmio,
            _                                => MemoryRegionKind::Reserved,
        };

        map.regions[map.count] = MemoryRegion {
            start:  PhysAddr::new(descriptor.phys_start),
            length: descriptor.page_count * 4096,
            kind,
        };
        map.count += 1;
    }

    map
}

// ─────────────────────────────────────────────────────────────────────────────
// Panic handler (required for no_std UEFI apps)
// ─────────────────────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log::error!("PANIC: {}", _info);
    loop {}
}
