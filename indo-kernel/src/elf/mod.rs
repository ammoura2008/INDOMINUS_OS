//! # ELF64 Loader
//!
//! Parses and loads ELF64 executables into process address spaces.
//!
//! ## ELF64 File Layout
//!
//! ```text
//! ┌──────────────────────┐  Offset 0
//! │ ELF Header (64 bytes)│
//! ├──────────────────────┤  e_phoff
//! │ Program Header [0]   │  (56 bytes each)
//! │ Program Header [1]   │
//! │ ...                  │
//! ├──────────────────────┤
//! │ .text / .data / etc. │  Segment data referenced by phdr.p_offset
//! └──────────────────────┘
//! ```
//!
//! ## Loading Strategy
//!
//! For each PT_LOAD program header:
//! 1. Calculate page-aligned bounds: [p_vaddr & ~(PAGE-1), ceil((p_vaddr+p_memsz)/(PAGE))]
//! 2. Allocate physical frames for each page
//! 3. Map frames at the segment's virtual address in the process's PML4
//! 4. Copy segment data from the ELF binary (p_offset..p_offset+p_filesz)
//! 5. Zero remaining bytes (p_filesz..p_memsz) for BSS-like regions

use crate::memory::{self, vmm, PAGE_SIZE};
use x86_64::structures::paging::{FrameAllocator, PageTableFlags};
use x86_64::VirtAddr;

// ─────────────────────────────────────────────────────────────────────────────
// ELF constants
// ─────────────────────────────────────────────────────────────────────────────

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const PT_LOAD: u32 = 1;

// Program header flags
const PF_X: u32 = 1;
const PF_W: u32 = 2;
// ─────────────────────────────────────────────────────────────────────────────
// ELF header types (packed C repr for direct parsing from byte slice)
// ─────────────────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Result type
// ─────────────────────────────────────────────────────────────────────────────

/// Information about a loaded ELF binary.
pub struct ElfImage {
    /// Virtual address of the entry point.
    pub entry: u64,
}

/// Errors that can occur during ELF loading.
#[derive(Debug)]
pub enum ElfError {
    BadMagic,
    NotElf64,
    NotLittleEndian,
    NotExecutable,
    NoProgramHeaders,
    SegmentOutOfBounds,
    SegmentOverlap,
    MapFailed,
    BadEntry,
    BadPhdrSize,
    SegmentTooLarge,
}

impl ElfError {
    pub fn description(&self) -> &'static str {
        match self {
            ElfError::BadMagic => "bad ELF magic",
            ElfError::NotElf64 => "not ELF64",
            ElfError::NotLittleEndian => "not little-endian",
            ElfError::NotExecutable => "not an executable (ET_EXEC or ET_DYN)",
            ElfError::NoProgramHeaders => "no program headers",
            ElfError::SegmentOutOfBounds => "segment data out of ELF bounds",
            ElfError::SegmentOverlap => "segment overlaps kernel address space",
            ElfError::MapFailed => "failed to map segment pages",
            ElfError::BadEntry => "entry point not in any loaded segment",
            ElfError::BadPhdrSize => "invalid program header size",
            ElfError::SegmentTooLarge => "segment exceeds memory limit",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parser: read helpers (safe, no allocation)
// ─────────────────────────────────────────────────────────────────────────────

/// Read a `u16` from a byte slice at the given offset (little-endian).
fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    if offset + 2 > data.len() { return None; }
    Some(u16::from_le_bytes([data[offset], data[offset + 1]]))
}

/// Read a `u32` from a byte slice at the given offset (little-endian).
fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    if offset + 4 > data.len() { return None; }
    Some(u32::from_le_bytes([
        data[offset], data[offset + 1],
        data[offset + 2], data[offset + 3],
    ]))
}

/// Read a `u64` from a byte slice at the given offset (little-endian).
fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
    if offset + 8 > data.len() { return None; }
    Some(u64::from_le_bytes([
        data[offset],     data[offset + 1],
        data[offset + 2], data[offset + 3],
        data[offset + 4], data[offset + 5],
        data[offset + 6], data[offset + 7],
    ]))
}

// ─────────────────────────────────────────────────────────────────────────────
// ELF parsing (in-place, no allocation)
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the ELF64 header from a byte slice.
///
/// Returns the header fields we need, or an error.
fn parse_ehdr(data: &[u8]) -> Result<(u64, u64, u16, u16, u16), ElfError> {
    if data.len() < 64 {
        return Err(ElfError::BadMagic);
    }

    // Validate magic
    if data[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    // ELF64?
    if data[4] != ELFCLASS64 {
        return Err(ElfError::NotElf64);
    }
    // Little-endian?
    if data[5] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }

    let e_type = read_u16(data, 16).ok_or(ElfError::BadMagic)?;
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(ElfError::NotExecutable);
    }

    let e_entry = read_u64(data, 24).ok_or(ElfError::BadMagic)?;
    let e_phoff = read_u64(data, 32).ok_or(ElfError::BadMagic)?;
    let e_phentsize = read_u16(data, 54).ok_or(ElfError::BadMagic)?;
    let e_phnum = read_u16(data, 56).ok_or(ElfError::BadMagic)?;

    if e_phnum == 0 {
        return Err(ElfError::NoProgramHeaders);
    }

    // Validate program header size (must be at least 56 bytes for Elf64Phdr)
    if (e_phentsize as usize) < core::mem::size_of::<Elf64Phdr>() {
        return Err(ElfError::BadPhdrSize);
    }

    // Validate program header offset is within the ELF data
    if e_phoff as usize >= data.len() {
        return Err(ElfError::SegmentOutOfBounds);
    }

    Ok((e_entry, e_phoff, e_phentsize, e_phnum, e_type))
}

/// Parse a single program header at the given index.
fn parse_phdr(data: &[u8], phoff: u64, phentsize: u16, index: u16) -> Option<Elf64Phdr> {
    let offset = (phoff as usize) + (index as usize) * (phentsize as usize);
    if offset + 56 > data.len() { return None; }

    Some(Elf64Phdr {
        p_type:   read_u32(data, offset)?,
        p_flags:  read_u32(data, offset + 4)?,
        p_offset: read_u64(data, offset + 8)?,
        p_vaddr:  read_u64(data, offset + 16)?,
        p_paddr:  read_u64(data, offset + 24)?,
        p_filesz: read_u64(data, offset + 32)?,
        p_memsz:  read_u64(data, offset + 40)?,
        p_align:  read_u64(data, offset + 48)?,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API: load ELF into a process PML4
// ─────────────────────────────────────────────────────────────────────────────

/// Load an ELF64 binary into a process's address space.
///
/// For each PT_LOAD segment:
/// 1. Allocates physical frames
/// 2. Maps them at `p_vaddr` in `pml4_phys` with appropriate flags
/// 3. Copies segment data from `elf_data`
/// 4. Zeros the BSS portion (p_filesz..p_memsz)
///
/// # Arguments
/// * `elf_data` — the raw ELF binary bytes
/// * `pml4_phys` — the process's page table (physical address)
///
/// # Returns
/// `ElfImage` with entry point and max virtual address.
///
/// # Panics
/// Panics if frame allocation fails or page mapping fails.
/// Maximum user-space memory per process (256 MiB).
const MAX_USER_MEM: u64 = 256 * 1024 * 1024;

/// Canonical user-space boundary: everything below this is user-accessible.
const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

pub fn load_elf(elf_data: &[u8], pml4_phys: memory::PhysAddr) -> Result<ElfImage, ElfError> {
    let (entry, phoff, phentsize, phnum, _type) = parse_ehdr(elf_data)?;

    let mut entry_in_segment = false;
    let mut total_mem: u64 = 0; // Cumulative memory across all segments

    for i in 0..phnum {
        let phdr = parse_phdr(elf_data, phoff, phentsize, i)
            .ok_or(ElfError::SegmentOutOfBounds)?;

        // Only process PT_LOAD segments
        if phdr.p_type != PT_LOAD {
            continue;
        }

        // Validate segment data bounds
        let seg_end = phdr.p_offset.saturating_add(phdr.p_filesz);
        if seg_end > elf_data.len() as u64 {
            return Err(ElfError::SegmentOutOfBounds);
        }

        // Don't allow mapping into kernel space (upper half) or non-canonical gap
        if phdr.p_vaddr >= USER_SPACE_END {
            return Err(ElfError::SegmentOverlap);
        }

        // Calculate page-aligned bounds with overflow check
        let virt_start = phdr.p_vaddr & !(PAGE_SIZE - 1);
        let raw_end = phdr.p_vaddr.checked_add(phdr.p_memsz)
            .ok_or(ElfError::SegmentOverlap)?;
        if raw_end >= USER_SPACE_END {
            return Err(ElfError::SegmentOverlap);
        }
        let virt_end = (raw_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        // Prevent non-canonical addresses (gap between user and kernel halves)
        if virt_end >= USER_SPACE_END {
            return Err(ElfError::SegmentOverlap);
        }

        // Cumulative memory limit across ALL segments
        let seg_mem = virt_end - virt_start;
        total_mem = total_mem.checked_add(seg_mem).ok_or(ElfError::SegmentTooLarge)?;
        if total_mem > MAX_USER_MEM {
            return Err(ElfError::SegmentTooLarge);
        }

        // Check if entry point falls within this segment
        if entry >= virt_start && entry < virt_end {
            entry_in_segment = true;
        }

        // Build page table flags from ELF flags
        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if phdr.p_flags & PF_W != 0 {
            flags |= PageTableFlags::WRITABLE;
        }
        if phdr.p_flags & PF_X == 0 {
            flags |= PageTableFlags::NO_EXECUTE;
        }

        // Map pages and copy data
        let num_pages = (virt_end - virt_start) / PAGE_SIZE;
        for page_idx in 0..num_pages {
            let page_virt = VirtAddr::new(virt_start + page_idx * PAGE_SIZE);

            let frame = vmm::PmmFrameAllocator.allocate_frame()
                .ok_or(ElfError::MapFailed)?;

            vmm::map_page(
                pml4_phys,
                page_virt,
                memory::PhysAddr::new(frame.start_address().as_u64()),
                flags,
            );

            let frame_ptr = unsafe {
                vmm::phys_to_virt(frame.start_address().as_u64()).as_mut_ptr()
            };

            let page_virt_addr = virt_start + page_idx * PAGE_SIZE;
            let seg_data_start = if page_virt_addr >= phdr.p_vaddr {
                page_virt_addr - phdr.p_vaddr
            } else {
                0
            };
            let seg_data_end = core::cmp::min(
                seg_data_start + PAGE_SIZE,
                phdr.p_memsz,
            );

            let data_in_page = if seg_data_start < phdr.p_filesz {
                core::cmp::min(seg_data_end, phdr.p_filesz) - seg_data_start
            } else {
                0
            };

            let bss_in_page = if seg_data_end > seg_data_start {
                seg_data_end - seg_data_start - data_in_page
            } else {
                0
            };

            if data_in_page > 0 {
                let src_offset = phdr.p_offset + seg_data_start;
                let src = &elf_data[src_offset as usize..(src_offset + data_in_page) as usize];
                unsafe {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), frame_ptr, data_in_page as usize);
                }
            }

            if bss_in_page > 0 {
                unsafe {
                    core::ptr::write_bytes(
                        frame_ptr.add((data_in_page) as usize),
                        0,
                        bss_in_page as usize,
                    );
                }
            }
        }
    }

    // Validate entry point is within a loaded segment
    if !entry_in_segment {
        return Err(ElfError::BadEntry);
    }

    Ok(ElfImage {
        entry,
    })
}

/// Validate an ELF binary without loading it.
///
/// Returns Ok((entry, total_memory_bytes)) if valid, or Err with the reason.
pub fn validate_elf(elf_data: &[u8]) -> Result<(u64, u64), ElfError> {
    let (entry, phoff, phentsize, phnum, _type) = parse_ehdr(elf_data)?;

    let mut total_mem: u64 = 0;
    for i in 0..phnum {
        let phdr = parse_phdr(elf_data, phoff, phentsize, i)
            .ok_or(ElfError::SegmentOutOfBounds)?;

        if phdr.p_type != PT_LOAD {
            continue;
        }

        let seg_end = phdr.p_offset.saturating_add(phdr.p_filesz);
        if seg_end > elf_data.len() as u64 {
            return Err(ElfError::SegmentOutOfBounds);
        }

        total_mem += phdr.p_memsz;
    }

    Ok((entry, total_mem))
}
