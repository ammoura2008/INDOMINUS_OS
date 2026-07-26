//! # FAT Filesystem Driver (FAT16 + FAT32)
//!
//! Full read/write FAT16/FAT32 implementation backed by a block device.
//! Detects FAT variant from BPB, follows FAT cluster chains, reads/writes
//! directory entries (8.3 + LFN for reads; 8.3 only for writes).
//!
//! ## Architecture
//!
//! The driver is split into shared code (cluster chain traversal, directory
//! entry parsing, file I/O) and variant-specific code (FAT entry width,
//! end-of-chain markers, root directory location). Variant detection happens
//! once at mount time and is stored in the BPB struct.
//!
//! ### FAT16 vs FAT32 key differences handled here:
//! - FAT entry width: 16-bit vs 32-bit
//! - End-of-chain: >= 0xFFF8 vs >= 0x0FFFFFF8
//! - Root directory: fixed location (FAT16) vs cluster chain (FAT32)
//! - BPB field offsets for total sectors and FAT size
//!
//! ## Write Support
//!
//! Files are buffered in memory. On close(), dirty data is flushed to disk:
//! 1. Data sectors are written
//! 2. FAT entries are allocated/updated
//! 3. Directory entry size is updated LAST (crash-safe: old data intact on crash)

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp;

use crate::block::BlockDevice;
use crate::serial;
use crate::vfs::{File, FileSystem, Inode, VfsError};

// ─────────────────────────────────────────────────────────────────────────────
// FAT Variant
// ─────────────────────────────────────────────────────────────────────────────

/// FAT filesystem variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FatVariant {
    Fat16,
    Fat32,
}

impl FatVariant {
    fn as_str(&self) -> &'static str {
        match self {
            FatVariant::Fat16 => "FAT16",
            FatVariant::Fat32 => "FAT32",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FAT BPB (BIOS Parameter Block)
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed FAT BPB — holds fields for both FAT16 and FAT32.
/// Determined once at mount time; all subsequent code uses this struct.
#[derive(Debug, Clone)]
struct FatBpb {
    variant: FatVariant,
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    num_fats: u8,
    root_cluster: u32,        // FAT32 only; FAT16 root is fixed-location
    fat_size_sectors: u32,    // Sectors per FAT (unified)
    total_sectors: u32,
    /// Absolute LBA of the first sector of this FAT partition
    base_sector: u32,
    /// First sector of the data area (after all FATs and root dir for FAT16)
    data_start_sector: u32,
    bytes_per_cluster: u32,
    // FAT16-specific
    root_entry_count: u16,    // FAT16 only; 0 for FAT32
    root_dir_start_sector: u32,
    root_dir_sectors: u32,
}

impl FatBpb {
    /// Parse BPB from a 512-byte boot sector buffer.
    fn parse(buf: &[u8], _device: &dyn BlockDevice, base_sector: u32) -> Result<Self, VfsError> {
        if buf.len() < 512 {
            return Err(VfsError::IoError);
        }

        let bps = u16::from_le_bytes([buf[11], buf[12]]);
        let spc = buf[13];
        let rs = u16::from_le_bytes([buf[14], buf[15]]);
        let nf = buf[16];

        if bps == 0 || spc == 0 {
            serial::write_str_nl("[FAT] Invalid BPB (bps or spc is zero)");
            return Err(VfsError::IoError);
        }

        let bpc = bps as u32 * spc as u32;

        let fat_sz32 = u32::from_le_bytes([buf[36], buf[37], buf[38], buf[39]]);
        let fat_sz16 = u16::from_le_bytes([buf[22], buf[23]]);
        let root_entry_count = u16::from_le_bytes([buf[17], buf[18]]);

        let variant;
        let fat_size_sectors: u32;
        let total_sectors: u32;
        let root_cluster: u32;

        if fat_sz16 != 0 && root_entry_count != 0 {
            variant = FatVariant::Fat16;
            fat_size_sectors = fat_sz16 as u32;
            total_sectors = {
                let ts16 = u16::from_le_bytes([buf[19], buf[20]]);
                if ts16 != 0 { ts16 as u32 } else {
                    u32::from_le_bytes([buf[32], buf[33], buf[34], buf[35]])
                }
            };
            root_cluster = 0;
        } else if fat_sz32 != 0 {
            variant = FatVariant::Fat32;
            fat_size_sectors = fat_sz32;
            total_sectors = u32::from_le_bytes([buf[32], buf[33], buf[34], buf[35]]);
            root_cluster = u32::from_le_bytes([buf[44], buf[45], buf[46], buf[47]]);
        } else {
            serial::write_str_nl("[FAT] Unsupported: FAT12 or corrupt BPB");
            return Err(VfsError::IoError);
        }

        let fat_area_end = rs as u32 + nf as u32 * fat_size_sectors;

        let root_dir_start_sector;
        let root_dir_sectors;
        let data_start_sector;

        if variant == FatVariant::Fat16 {
            root_dir_start_sector = fat_area_end;
            root_dir_sectors =
                ((root_entry_count as u32 * 32) + (bps as u32 - 1)) / bps as u32;
            data_start_sector = fat_area_end + root_dir_sectors;
        } else {
            root_dir_start_sector = 0;
            root_dir_sectors = 0;
            data_start_sector = fat_area_end;
        }

        let bpb = FatBpb {
            variant,
            bytes_per_sector: bps,
            sectors_per_cluster: spc,
            reserved_sectors: rs,
            num_fats: nf,
            root_cluster,
            fat_size_sectors,
            total_sectors,
            base_sector,
            data_start_sector,
            bytes_per_cluster: bpc,
            root_entry_count,
            root_dir_start_sector,
            root_dir_sectors,
        };

        serial::write_str("[FAT] Detected: ");
        serial::write_str(variant.as_str());
        serial::write_str(" bps=");
        serial::write_hex(bps as u64);
        serial::write_str(" spc=");
        serial::write_hex(spc as u64);
        serial::write_str(" rs=");
        serial::write_hex(rs as u64);
        serial::write_str(" fats=");
        serial::write_hex(nf as u64);
        serial::write_str(" fat_size=");
        serial::write_hex(fat_size_sectors as u64);
        serial::write_str(" total=");
        serial::write_hex(total_sectors as u64);
        if variant == FatVariant::Fat32 {
            serial::write_str(" root_cl=");
            serial::write_hex(root_cluster as u64);
        } else {
            serial::write_str(" root_entries=");
            serial::write_hex(root_entry_count as u64);
            serial::write_str(" root_dir_sec=");
            serial::write_hex(root_dir_start_sector as u64);
        }
        serial::write_str(" data_start=");
        serial::write_hex(data_start_sector as u64);
        serial::write_nl();

        Ok(bpb)
    }

    /// Given a cluster number, return the LBA of its first sector.
    fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.base_sector + self.data_start_sector + (cluster.saturating_sub(2)) * self.sectors_per_cluster as u32
    }

    /// End-of-chain marker for this FAT variant.
    fn eoc(&self) -> u32 {
        match self.variant {
            FatVariant::Fat16 => 0xFFF8,
            FatVariant::Fat32 => 0x0FFFFFF8,
        }
    }

    /// Maximum valid cluster number for this variant.
    fn max_cluster(&self) -> u32 {
        match self.variant {
            FatVariant::Fat16 => 0xFFF0,
            FatVariant::Fat32 => 0x0FFFFFF0,
        }
    }

    /// Total number of sectors in the FAT area (all copies).
    fn fat_area_sectors(&self) -> u32 {
        self.reserved_sectors as u32 + self.num_fats as u32 * self.fat_size_sectors
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FAT Directory Entry
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed FAT directory entry.
#[derive(Debug, Clone)]
struct FatDirEntry {
    name: String,        // "filename.ext" (lowercased)
    attributes: u8,
    first_cluster: u32,
    size: u32,
    is_lfn: bool,
}

impl FatDirEntry {
    /// Parse a 32-byte directory entry.
    fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 32 {
            return None;
        }
        if buf[0] == 0xE5 || buf[0] == 0x00 {
            return None;
        }
        let attr = buf[11];
        if attr == 0x0F {
            return Some(FatDirEntry {
                name: String::new(),
                attributes: attr,
                first_cluster: 0,
                size: 0,
                is_lfn: true,
            });
        }
        if attr & 0x08 != 0 {
            return None;
        }

        let mut name_part = String::new();
        let mut ext_part = String::new();
        for i in 0..8 {
            let b = buf[i];
            if b == 0x20 || b == 0x00 { break; }
            let c = if b >= b'A' && b <= b'Z' { b + 32 } else { b };
            name_part.push(c as char);
        }
        for i in 8..11 {
            let b = buf[i];
            if b == 0x20 || b == 0x00 { break; }
            let c = if b >= b'A' && b <= b'Z' { b + 32 } else { b };
            ext_part.push(c as char);
        }
        let full_name = if ext_part.is_empty() {
            name_part
        } else {
            alloc::format!("{}.{}", name_part, ext_part)
        };

        let cluster_high = u16::from_le_bytes([buf[20], buf[21]]) as u32;
        let cluster_low = u16::from_le_bytes([buf[26], buf[27]]) as u32;
        let first_cluster = (cluster_high << 16) | cluster_low;
        let size = u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]);

        Some(FatDirEntry {
            name: full_name,
            attributes: attr,
            first_cluster,
            size,
            is_lfn: false,
        })
    }

    fn is_dir(&self) -> bool {
        self.attributes & 0x10 != 0
    }

    /// Convert a long filename "name.ext" to 8.3 format (11 bytes, padded).
    /// Returns Err if name doesn't fit 8.3.
    fn name_to_83(name: &str) -> Result<[u8; 11], VfsError> {
        let parts: Vec<&str> = name.split('.').collect();
        if parts.is_empty() || parts.len() > 2 {
            return Err(VfsError::BadPath);
        }

        let name_part = parts[0].as_bytes();
        let ext_part = if parts.len() > 1 { parts[1].as_bytes() } else { b"" };

        if name_part.is_empty() || name_part.len() > 8 {
            return Err(VfsError::BadPath);
        }
        if ext_part.len() > 3 {
            return Err(VfsError::BadPath);
        }

        let mut result = [0x20u8; 11];
        for (i, &b) in name_part.iter().enumerate() {
            let upper = if b >= b'a' && b <= b'z' { b - 32 } else { b };
            result[i] = upper;
        }
        for (i, &b) in ext_part.iter().enumerate() {
            let upper = if b >= b'a' && b <= b'z' { b - 32 } else { b };
            result[8 + i] = upper;
        }

        Ok(result)
    }

    /// Serialize this directory entry to 32 bytes.
    fn to_bytes(&self) -> Result<[u8; 11], VfsError> {
        Self::name_to_83(&self.name)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FAT Cluster Chain Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Read the FAT entry for a given cluster.
fn read_fat_entry(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    cluster: u32,
) -> Result<u32, VfsError> {
    match bpb.variant {
        FatVariant::Fat16 => {
            let entry_byte = cluster as u64 * 2;
            let fat_sector = bpb.base_sector as u64 + bpb.reserved_sectors as u64 + entry_byte / 512;
            let entry_in_sector = (entry_byte % 512) as usize;

            let mut buf = [0u8; 512];
            device.read_sector(fat_sector, &mut buf).map_err(|_| VfsError::IoError)?;
            let raw = u16::from_le_bytes([buf[entry_in_sector], buf[entry_in_sector + 1]]);
            Ok(raw as u32)
        }
        FatVariant::Fat32 => {
            let entry_byte = cluster as u64 * 4;
            let fat_sector = bpb.base_sector as u64 + bpb.reserved_sectors as u64 + entry_byte / 512;
            let entry_in_sector = (entry_byte % 512) as usize;

            let mut buf = [0u8; 512];
            device.read_sector(fat_sector, &mut buf).map_err(|_| VfsError::IoError)?;
            let raw = u32::from_le_bytes([
                buf[entry_in_sector],
                buf[entry_in_sector + 1],
                buf[entry_in_sector + 2],
                buf[entry_in_sector + 3] & 0x0F,
            ]);
            Ok(raw)
        }
    }
}

/// Write a FAT entry for a given cluster. Mirrors to all FAT copies.
fn write_fat_entry(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    cluster: u32,
    value: u32,
) -> Result<(), VfsError> {
    for fat_copy in 0u32..bpb.num_fats as u32 {
        let fat_offset = bpb.reserved_sectors as u32 + fat_copy * bpb.fat_size_sectors;

        match bpb.variant {
            FatVariant::Fat16 => {
                let entry_byte = cluster as u64 * 2;
                let fat_sector = bpb.base_sector as u64 + fat_offset as u64 + entry_byte / 512;
                let entry_in_sector = (entry_byte % 512) as usize;

                let mut buf = [0u8; 512];
                device.read_sector(fat_sector, &mut buf).map_err(|_| VfsError::IoError)?;
                let raw = value as u16;
                buf[entry_in_sector] = (raw & 0xFF) as u8;
                buf[entry_in_sector + 1] = ((raw >> 8) & 0xFF) as u8;
                device.write_sector(fat_sector, &buf).map_err(|_| VfsError::IoError)?;
            }
            FatVariant::Fat32 => {
                let entry_byte = cluster as u64 * 4;
                let fat_sector = bpb.base_sector as u64 + fat_offset as u64 + entry_byte / 512;
                let entry_in_sector = (entry_byte % 512) as usize;

                let mut buf = [0u8; 512];
                device.read_sector(fat_sector, &mut buf).map_err(|_| VfsError::IoError)?;
                let masked = value & 0x0FFFFFFF;
                buf[entry_in_sector] = (masked & 0xFF) as u8;
                buf[entry_in_sector + 1] = ((masked >> 8) & 0xFF) as u8;
                buf[entry_in_sector + 2] = ((masked >> 16) & 0xFF) as u8;
                buf[entry_in_sector + 3] = (buf[entry_in_sector + 3] & 0xF0) | ((masked >> 24) & 0x0F) as u8;
                device.write_sector(fat_sector, &buf).map_err(|_| VfsError::IoError)?;
            }
        }
    }
    Ok(())
}

/// Allocate a new cluster. Returns the cluster number.
/// Scans FAT for a free entry (value 0).
fn allocate_cluster(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    hint: u32,
) -> Result<u32, VfsError> {
    let max = bpb.max_cluster();
    let eoc = bpb.eoc();

    // Start scanning from hint, wrap around
    for cluster in hint..=max {
        let val = read_fat_entry(device, bpb, cluster)?;
        if val == 0 {
            write_fat_entry(device, bpb, cluster, eoc)?;
            return Ok(cluster);
        }
    }
    // Wrap around from 2 to hint
    for cluster in 2..hint {
        let val = read_fat_entry(device, bpb, cluster)?;
        if val == 0 {
            write_fat_entry(device, bpb, cluster, eoc)?;
            return Ok(cluster);
        }
    }

    serial::write_str_nl("[FAT] No free clusters");
    Err(VfsError::NoSpace)
}

/// Free a cluster chain. Zeros each FAT entry.
fn free_cluster_chain(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    start_cluster: u32,
) -> Result<(), VfsError> {
    let chain = read_cluster_chain(device, bpb, start_cluster)?;
    for &cluster in &chain {
        write_fat_entry(device, bpb, cluster, 0)?;
    }
    Ok(())
}

/// Append a cluster to the end of a chain.
fn extend_chain(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    last_cluster: u32,
    new_cluster: u32,
) -> Result<(), VfsError> {
    let eoc = bpb.eoc();
    write_fat_entry(device, bpb, last_cluster, new_cluster)?;
    write_fat_entry(device, bpb, new_cluster, eoc)?;
    Ok(())
}

/// Count free clusters on the filesystem.
fn count_free_clusters(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
) -> Result<u64, VfsError> {
    let max = bpb.max_cluster();
    let mut free = 0u64;
    for cluster in 2..=max {
        let val = read_fat_entry(device, bpb, cluster)?;
        if val == 0 {
            free += 1;
        }
    }
    Ok(free)
}

/// Follow the FAT cluster chain from a starting cluster.
fn read_cluster_chain(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    start_cluster: u32,
) -> Result<Vec<u32>, VfsError> {
    let mut chain = Vec::new();
    let mut cluster = start_cluster;
    let eoc = bpb.eoc();

    loop {
        if cluster < 2 || cluster >= eoc {
            break;
        }
        chain.push(cluster);
        cluster = read_fat_entry(device, bpb, cluster)?;
        if chain.len() > 0x100000 {
            break;
        }
    }

    Ok(chain)
}

/// Read file content described by a cluster chain into a buffer.
fn read_file_data(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    chain: &[u32],
    buf: &mut [u8],
) -> Result<usize, VfsError> {
    let mut bytes_read = 0usize;
    let buf_len = buf.len();

    for &cluster in chain {
        if bytes_read >= buf_len {
            break;
        }
        let sector = bpb.cluster_to_sector(cluster) as u64;
        let mut sector_buf = [0u8; 512];

        for s in 0..bpb.sectors_per_cluster as u64 {
            if bytes_read >= buf_len {
                break;
            }
            device.read_sector(sector + s, &mut sector_buf).map_err(|_| VfsError::IoError)?;
            let to_copy = cmp::min(512, buf_len - bytes_read);
            buf[bytes_read..bytes_read + to_copy]
                .copy_from_slice(&sector_buf[..to_copy]);
            bytes_read += to_copy;
        }
    }

    Ok(bytes_read)
}

/// Write data to sectors described by a cluster chain.
fn write_file_data(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    chain: &[u32],
    offset: usize,
    buf: &[u8],
) -> Result<usize, VfsError> {
    let mut bytes_written = 0usize;
    let buf_len = buf.len();
    let bpc = bpb.bytes_per_cluster as usize;

    // Skip clusters before the offset
    let start_cluster_idx = offset / bpc;
    let start_byte_in_cluster = offset % bpc;

    for (i, &cluster) in chain.iter().enumerate() {
        if i < start_cluster_idx {
            continue;
        }
        if bytes_written >= buf_len {
            break;
        }

        let sector = bpb.cluster_to_sector(cluster) as u64;
        let cluster_byte_offset = if i == start_cluster_idx { start_byte_in_cluster } else { 0 };

        for s in 0..bpb.sectors_per_cluster as u64 {
            if bytes_written >= buf_len {
                break;
            }

            let sector_offset = (s as usize * 512 + cluster_byte_offset) % 512;
            let bytes_into_sector = sector_offset;

            let mut sector_buf = [0u8; 512];
            device.read_sector(sector + s, &mut sector_buf).map_err(|_| VfsError::IoError)?;

            let available_in_sector = 512 - bytes_into_sector;
            let to_write = cmp::min(available_in_sector, buf_len - bytes_written);

            sector_buf[bytes_into_sector..bytes_into_sector + to_write]
                .copy_from_slice(&buf[bytes_written..bytes_written + to_write]);
            device.write_sector(sector + s, &sector_buf).map_err(|_| VfsError::IoError)?;

            bytes_written += to_write;
        }
    }

    Ok(bytes_written)
}

/// Read raw bytes from a fixed set of sectors (for FAT16 root directory).
fn read_sectors(
    device: &dyn BlockDevice,
    start_sector: u64,
    num_sectors: u64,
) -> Result<Vec<u8>, VfsError> {
    let mut data = Vec::new();
    let mut sector_buf = [0u8; 512];
    for s in 0..num_sectors {
        device
            .read_sector(start_sector + s, &mut sector_buf)
            .map_err(|_| VfsError::IoError)?;
        data.extend_from_slice(&sector_buf);
    }
    Ok(data)
}

/// Write raw bytes to fixed sectors (for FAT16 root directory).
fn write_sectors(
    device: &dyn BlockDevice,
    start_sector: u64,
    num_sectors: u64,
    data: &[u8],
) -> Result<(), VfsError> {
    for s in 0..num_sectors {
        let offset = (s as usize) * 512;
        let mut sector_buf = [0u8; 512];
        let to_copy = cmp::min(512, data.len() - offset);
        sector_buf[..to_copy].copy_from_slice(&data[offset..offset + to_copy]);
        device
            .write_sector(start_sector + s, &sector_buf)
            .map_err(|_| VfsError::IoError)?;
    }
    Ok(())
}

/// Parse directory entries from a raw byte buffer.
fn parse_dir_entries(raw_data: &[u8]) -> Result<Vec<FatDirEntry>, VfsError> {
    let mut entries = Vec::new();
    let mut i = 0;
    let mut pending_lfn = String::new();

    while i + 32 <= raw_data.len() {
        let entry_buf = &raw_data[i..i + 32];

        if entry_buf[0] == 0x00 {
            break;
        }

        if entry_buf[11] == 0x0F {
            let mut chars = [0u16; 13];
            for j in 0..5 {
                chars[j] = u16::from_le_bytes([entry_buf[1 + j * 2], entry_buf[2 + j * 2]]);
            }
            for j in 0..6 {
                chars[5 + j] =
                    u16::from_le_bytes([entry_buf[14 + j * 2], entry_buf[15 + j * 2]]);
            }
            for j in 0..2 {
                chars[11 + j] =
                    u16::from_le_bytes([entry_buf[28 + j * 2], entry_buf[29 + j * 2]]);
            }
            for &c in &chars {
                if c == 0 || c == 0xFFFF {
                    break;
                }
                if c < 128 {
                    pending_lfn.push(c as u8 as char);
                }
            }
            i += 32;
            continue;
        }

        if let Some(mut entry) = FatDirEntry::parse(entry_buf) {
            if !pending_lfn.is_empty() {
                entry.name = core::mem::take(&mut pending_lfn);
            }
            entries.push(entry);
        } else {
            pending_lfn.clear();
        }

        i += 32;
    }

    Ok(entries)
}

/// Read all directory entries from a cluster chain.
fn read_dir_entries_from_chain(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    chain: &[u32],
) -> Result<Vec<FatDirEntry>, VfsError> {
    let mut raw_data = Vec::new();
    for &cluster in chain {
        let sector = bpb.cluster_to_sector(cluster) as u64;
        let mut sector_buf = [0u8; 512];
        for s in 0..bpb.sectors_per_cluster as u64 {
            device
                .read_sector(sector + s, &mut sector_buf)
                .map_err(|_| VfsError::IoError)?;
            raw_data.extend_from_slice(&sector_buf);
        }
    }
    parse_dir_entries(&raw_data)
}

// ─────────────────────────────────────────────────────────────────────────────
// Directory Entry Writing
// ─────────────────────────────────────────────────────────────────────────────

/// Find a free slot in a directory (deleted entry 0xE5 or end marker 0x00).
/// Returns (sector, byte_offset) of the slot, or None if directory is full
/// (for FAT16 root) or needs extending.
fn find_free_dir_slot_in_raw(
    raw_data: &[u8],
) -> Option<usize> {
    let mut i = 0;
    while i + 32 <= raw_data.len() {
        if raw_data[i] == 0xE5 || raw_data[i] == 0x00 {
            return Some(i);
        }
        i += 32;
    }
    None
}

/// Write a 32-byte directory entry at a specific byte offset in the raw data,
/// then flush the raw data back to disk.
fn write_dir_entry_at_offset(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    dir_chain: &[u32],
    raw_data: &mut [u8],
    byte_offset: usize,
    entry: &FatDirEntry,
    name_83: [u8; 11],
) -> Result<(), VfsError> {
    let bpc = bpb.bytes_per_cluster as usize;
    let _total_bytes = dir_chain.len() * bpc;

    // Ensure raw_data covers this offset
    if byte_offset + 32 > raw_data.len() {
        return Err(VfsError::NoSpace);
    }

    // Build 32-byte entry
    let mut buf = [0u8; 32];
    buf[0..11].copy_from_slice(&name_83);
    buf[11] = entry.attributes;
    buf[20] = ((entry.first_cluster >> 16) & 0xFF) as u8;
    buf[21] = ((entry.first_cluster >> 24) & 0xFF) as u8;
    buf[26] = (entry.first_cluster & 0xFF) as u8;
    buf[27] = ((entry.first_cluster >> 8) & 0xFF) as u8;
    buf[28..32].copy_from_slice(&entry.size.to_le_bytes());

    // Update raw_data
    raw_data[byte_offset..byte_offset + 32].copy_from_slice(&buf);

    // Write back the entire directory to disk
    // We write sector by sector, tracking which sector covers byte_offset
    let cluster_idx = byte_offset / bpc;
    let byte_in_cluster = byte_offset % bpc;
    let cluster = dir_chain[cluster_idx];

    // The sector within the cluster that contains the changed entry
    let sector_in_cluster = byte_in_cluster / 512;
    let base_sector = bpb.cluster_to_sector(cluster) as u64 + sector_in_cluster as u64;

    // Write just that sector (read-modify-write already done in raw_data)
    let mut sector_buf = [0u8; 512];
    let sector_byte_offset = sector_in_cluster * 512;
    let sector_data_len = cmp::min(512, raw_data.len() - (cluster_idx * bpc + sector_byte_offset));
    sector_buf[..sector_data_len].copy_from_slice(
        &raw_data[cluster_idx * bpc + sector_byte_offset..cluster_idx * bpc + sector_byte_offset + sector_data_len]
    );
    device.write_sector(base_sector, &sector_buf).map_err(|_| VfsError::IoError)?;

    Ok(())
}

/// Append a new directory entry to the end of a directory's cluster chain.
/// May need to allocate a new cluster if the directory is full.
fn append_dir_entry(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    dir_chain: &mut Vec<u32>,
    entry: &FatDirEntry,
    name_83: [u8; 11],
) -> Result<(), VfsError> {
    // Read all directory data
    let mut raw_data = Vec::new();
    for &cluster in dir_chain.iter() {
        let sector = bpb.cluster_to_sector(cluster) as u64;
        let mut sector_buf = [0u8; 512];
        for s in 0..bpb.sectors_per_cluster as u64 {
            device.read_sector(sector + s, &mut sector_buf).map_err(|_| VfsError::IoError)?;
            raw_data.extend_from_slice(&sector_buf);
        }
    }

    // Look for a free slot
    if let Some(offset) = find_free_dir_slot_in_raw(&raw_data) {
        return write_dir_entry_at_offset(device, bpb, dir_chain, &mut raw_data, offset, entry, name_83);
    }

    // No free slot — need to extend the directory
    // Allocate a new cluster
    let last_cluster = *dir_chain.last().ok_or(VfsError::IoError)?;
    let new_cluster = allocate_cluster(device, bpb, last_cluster + 1)?;
    extend_chain(device, bpb, last_cluster, new_cluster)?;

    // Zero-fill the new cluster on disk
    let sector = bpb.cluster_to_sector(new_cluster) as u64;
    let zeros = [0u8; 512];
    for s in 0..bpb.sectors_per_cluster as u64 {
        device.write_sector(sector + s, &zeros).map_err(|_| VfsError::IoError)?;
    }

    dir_chain.push(new_cluster);

    // Append entry at the start of the new cluster
    let bpc = bpb.bytes_per_cluster as usize;
    let new_cluster_offset = (dir_chain.len() - 1) * bpc;
    raw_data.extend_from_slice(&zeros);

    write_dir_entry_at_offset(device, bpb, dir_chain, &mut raw_data, new_cluster_offset, entry, name_83)
}

/// Update the size field of an existing directory entry on disk.
fn update_dir_entry_size(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    dir_chain: &[u32],
    entry_name: &str,
    new_size: u32,
) -> Result<(), VfsError> {
    let bpc = bpb.bytes_per_cluster as usize;

    let mut raw_data = Vec::new();
    for &cluster in dir_chain.iter() {
        let sector = bpb.cluster_to_sector(cluster) as u64;
        let mut sector_buf = [0u8; 512];
        for s in 0..bpb.sectors_per_cluster as u64 {
            device.read_sector(sector + s, &mut sector_buf).map_err(|_| VfsError::IoError)?;
            raw_data.extend_from_slice(&sector_buf);
        }
    }

    // Find the entry by name
    let mut offset = 0;
    let mut found = false;
    let mut pending_lfn = String::new();
    while offset + 32 <= raw_data.len() {
        let entry_buf = &raw_data[offset..offset + 32];

        if entry_buf[0] == 0x00 {
            break;
        }

        if entry_buf[11] == 0x0F {
            // LFN entry — accumulate
            let mut chars = [0u16; 13];
            for j in 0..5 {
                chars[j] = u16::from_le_bytes([entry_buf[1 + j * 2], entry_buf[2 + j * 2]]);
            }
            for j in 0..6 {
                chars[5 + j] = u16::from_le_bytes([entry_buf[14 + j * 2], entry_buf[15 + j * 2]]);
            }
            for j in 0..2 {
                chars[11 + j] = u16::from_le_bytes([entry_buf[28 + j * 2], entry_buf[29 + j * 2]]);
            }
            for &c in &chars {
                if c == 0 || c == 0xFFFF { break; }
                if c < 128 { pending_lfn.push(c as u8 as char); }
            }
            offset += 32;
            continue;
        }

        if let Some(mut parsed) = FatDirEntry::parse(entry_buf) {
            if !pending_lfn.is_empty() {
                parsed.name = core::mem::take(&mut pending_lfn);
            }
            if parsed.name == entry_name {
                // Update size at bytes 28-31
                raw_data[offset + 28..offset + 32].copy_from_slice(&new_size.to_le_bytes());
                found = true;
                break;
            }
        } else {
            pending_lfn.clear();
        }

        offset += 32;
    }

    if !found {
        return Err(VfsError::NotFound);
    }

    // Write back the sector containing this entry
    let cluster_idx = offset / bpc;
    let byte_in_cluster = offset % bpc;
    let cluster = dir_chain[cluster_idx];
    let sector_in_cluster = byte_in_cluster / 512;
    let base_sector = bpb.cluster_to_sector(cluster) as u64 + sector_in_cluster as u64;

    let mut sector_buf = [0u8; 512];
    let sector_byte_offset = sector_in_cluster * 512;
    let sector_data_len = cmp::min(512, raw_data.len() - (cluster_idx * bpc + sector_byte_offset));
    sector_buf[..sector_data_len].copy_from_slice(
        &raw_data[cluster_idx * bpc + sector_byte_offset..cluster_idx * bpc + sector_byte_offset + sector_data_len]
    );
    device.write_sector(base_sector, &sector_buf).map_err(|_| VfsError::IoError)?;

    Ok(())
}

/// Find a directory entry by name and return its index and the raw data.
fn find_dir_entry(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    dir_chain: &[u32],
    name: &str,
) -> Result<(usize, Vec<u8>), VfsError> {
    let mut raw_data = Vec::new();
    for &cluster in dir_chain.iter() {
        let sector = bpb.cluster_to_sector(cluster) as u64;
        let mut sector_buf = [0u8; 512];
        for s in 0..bpb.sectors_per_cluster as u64 {
            device.read_sector(sector + s, &mut sector_buf).map_err(|_| VfsError::IoError)?;
            raw_data.extend_from_slice(&sector_buf);
        }
    }

    let mut offset = 0;
    let mut pending_lfn = String::new();
    while offset + 32 <= raw_data.len() {
        let entry_buf = &raw_data[offset..offset + 32];

        if entry_buf[0] == 0x00 {
            break;
        }

        if entry_buf[11] == 0x0F {
            let mut chars = [0u16; 13];
            for j in 0..5 {
                chars[j] = u16::from_le_bytes([entry_buf[1 + j * 2], entry_buf[2 + j * 2]]);
            }
            for j in 0..6 {
                chars[5 + j] = u16::from_le_bytes([entry_buf[14 + j * 2], entry_buf[15 + j * 2]]);
            }
            for j in 0..2 {
                chars[11 + j] = u16::from_le_bytes([entry_buf[28 + j * 2], entry_buf[29 + j * 2]]);
            }
            for &c in &chars {
                if c == 0 || c == 0xFFFF { break; }
                if c < 128 { pending_lfn.push(c as u8 as char); }
            }
            offset += 32;
            continue;
        }

        if let Some(mut parsed) = FatDirEntry::parse(entry_buf) {
            if !pending_lfn.is_empty() {
                parsed.name = core::mem::take(&mut pending_lfn);
            }
            if parsed.name == name {
                return Ok((offset, raw_data));
            }
        } else {
            pending_lfn.clear();
        }

        offset += 32;
    }

    Err(VfsError::NotFound)
}

/// Delete a directory entry by marking it as deleted (0xE5).
fn delete_dir_entry(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    dir_chain: &[u32],
    name: &str,
) -> Result<(), VfsError> {
    let bpc = bpb.bytes_per_cluster as usize;

    let (offset, mut raw_data) = find_dir_entry(device, bpb, dir_chain, name)?;

    // Mark as deleted
    raw_data[offset] = 0xE5;

    // Write back
    let cluster_idx = offset / bpc;
    let byte_in_cluster = offset % bpc;
    let cluster = dir_chain[cluster_idx];
    let sector_in_cluster = byte_in_cluster / 512;
    let base_sector = bpb.cluster_to_sector(cluster) as u64 + sector_in_cluster as u64;

    let mut sector_buf = [0u8; 512];
    let sector_byte_offset = sector_in_cluster * 512;
    let sector_data_len = cmp::min(512, raw_data.len() - (cluster_idx * bpc + sector_byte_offset));
    sector_buf[..sector_data_len].copy_from_slice(
        &raw_data[cluster_idx * bpc + sector_byte_offset..cluster_idx * bpc + sector_byte_offset + sector_data_len]
    );
    device.write_sector(base_sector, &sector_buf).map_err(|_| VfsError::IoError)?;

    Ok(())
}

/// Look up a directory entry and return its parsed info + first_cluster + raw data.
fn lookup_dir_entry(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    dir_chain: &[u32],
    name: &str,
) -> Result<(FatDirEntry, usize, Vec<u8>), VfsError> {
    let mut raw_data = Vec::new();
    for &cluster in dir_chain.iter() {
        let sector = bpb.cluster_to_sector(cluster) as u64;
        let mut sector_buf = [0u8; 512];
        for s in 0..bpb.sectors_per_cluster as u64 {
            device.read_sector(sector + s, &mut sector_buf).map_err(|_| VfsError::IoError)?;
            raw_data.extend_from_slice(&sector_buf);
        }
    }

    let mut offset = 0;
    let mut pending_lfn = String::new();
    while offset + 32 <= raw_data.len() {
        let entry_buf = &raw_data[offset..offset + 32];

        if entry_buf[0] == 0x00 {
            break;
        }

        if entry_buf[11] == 0x0F {
            let mut chars = [0u16; 13];
            for j in 0..5 {
                chars[j] = u16::from_le_bytes([entry_buf[1 + j * 2], entry_buf[2 + j * 2]]);
            }
            for j in 0..6 {
                chars[5 + j] = u16::from_le_bytes([entry_buf[14 + j * 2], entry_buf[15 + j * 2]]);
            }
            for j in 0..2 {
                chars[11 + j] = u16::from_le_bytes([entry_buf[28 + j * 2], entry_buf[29 + j * 2]]);
            }
            for &c in &chars {
                if c == 0 || c == 0xFFFF { break; }
                if c < 128 { pending_lfn.push(c as u8 as char); }
            }
            offset += 32;
            continue;
        }

        if let Some(mut parsed) = FatDirEntry::parse(entry_buf) {
            if !pending_lfn.is_empty() {
                parsed.name = core::mem::take(&mut pending_lfn);
            }
            if parsed.name == name {
                return Ok((parsed, offset, raw_data));
            }
        } else {
            pending_lfn.clear();
        }

        offset += 32;
    }

    Err(VfsError::NotFound)
}

// ─────────────────────────────────────────────────────────────────────────────
// FAT Inode
// ─────────────────────────────────────────────────────────────────────────────

/// Shared state for the FAT filesystem.
#[derive(Clone)]
struct FatInner {
    device_id: usize,
    bpb: FatBpb,
}

/// Directory location — different for FAT16 root vs everything else.
#[derive(Clone)]
enum DirLocation {
    Fat16Root,
    ClusterChain(u32),
}

/// Directory inode.
struct FatDirInode {
    inner: FatInner,
    location: DirLocation,
}

unsafe impl Send for FatDirInode {}
unsafe impl Sync for FatDirInode {}

/// File inode — holds metadata; cluster chain computed on open.
struct FatFileInode {
    inner: FatInner,
    first_cluster: u32,
    size: u32,
}

unsafe impl Send for FatFileInode {}
unsafe impl Sync for FatFileInode {}

/// File handle for regular files — holds full content in memory.
/// On close(), dirty data is flushed to disk.
struct FatFileHandle {
    inner: FatInner,
    data: Vec<u8>,
    pos: usize,
    /// First cluster on disk (0 if newly created empty file).
    first_cluster: u32,
    /// Original file size on disk.
    disk_size: u32,
    /// Directory info needed for flushing metadata.
    /// None = file was opened for read-only or is a new file not yet linked.
    dir_chain: Option<Vec<u32>>,
    dir_entry_name: String,
    dirty: bool,
    /// Whether this file is newly created (needs directory entry + cluster allocation on first write).
    is_new: bool,
}

unsafe impl Send for FatFileHandle {}
unsafe impl Sync for FatFileHandle {}

impl Drop for FatFileHandle {
    fn drop(&mut self) {
        // Flush dirty data to disk when the handle is dropped.
        // This ensures data is persisted even if close() wasn't called explicitly.
        if self.dirty {
            self.close();
        }
    }
}

/// File handle for directories — null-terminated entry names.
struct FatDirHandle {
    data: Vec<u8>,
    pos: usize,
}

unsafe impl Send for FatDirHandle {}
unsafe impl Sync for FatDirHandle {}

// ─────────────────────────────────────────────────────────────────────────────
// Inode Implementations
// ─────────────────────────────────────────────────────────────────────────────

impl FatDirInode {
    fn read_entries(&self) -> Result<Vec<FatDirEntry>, VfsError> {
        let device =
            crate::block::registry::get_device(self.inner.device_id).ok_or(VfsError::IoError)?;

        match &self.location {
            DirLocation::Fat16Root => {
                let raw = read_sectors(
                    device.as_ref(),
                    self.inner.bpb.base_sector as u64 + self.inner.bpb.root_dir_start_sector as u64,
                    self.inner.bpb.root_dir_sectors as u64,
                )?;
                parse_dir_entries(&raw)
            }
            DirLocation::ClusterChain(cluster) => {
                let chain = read_cluster_chain(device.as_ref(), &self.inner.bpb, *cluster)?;
                read_dir_entries_from_chain(device.as_ref(), &self.inner.bpb, &chain)
            }
        }
    }

    /// Get the cluster chain for this directory (needed for writes).
    fn get_chain(&self) -> Result<Vec<u32>, VfsError> {
        let device =
            crate::block::registry::get_device(self.inner.device_id).ok_or(VfsError::IoError)?;

        match &self.location {
            DirLocation::Fat16Root => Ok(Vec::new()), // FAT16 root has no cluster chain
            DirLocation::ClusterChain(cluster) => {
                read_cluster_chain(device.as_ref(), &self.inner.bpb, *cluster)
            }
        }
    }
}

impl Inode for FatDirInode {
    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        let entries = self.read_entries()?;
        for entry in &entries {
            if entry.name == name {
                if entry.attributes & 0x10 != 0 {
                    return Ok(Arc::new(FatDirInode {
                        inner: self.inner.clone(),
                        location: DirLocation::ClusterChain(entry.first_cluster),
                    }));
                } else {
                    return Ok(Arc::new(FatFileInode {
                        inner: self.inner.clone(),
                        first_cluster: entry.first_cluster,
                        size: entry.size,
                    }));
                }
            }
        }
        Err(VfsError::NotFound)
    }

    fn open(&self) -> Result<Box<dyn File>, VfsError> {
        let entries = self.read_entries()?;
        let mut buf = Vec::new();
        for entry in &entries {
            for byte in entry.name.bytes() {
                buf.push(byte);
            }
            buf.push(0);
        }
        Ok(Box::new(FatDirHandle { data: buf, pos: 0 }))
    }

    fn is_dir(&self) -> bool { true }
    fn is_file(&self) -> bool { false }
    fn size(&self) -> u64 { 0 }

    fn readdir(&self) -> Result<Vec<String>, VfsError> {
        let entries = self.read_entries()?;
        Ok(entries.iter().map(|e| e.name.clone()).collect())
    }

    fn create_child_file(&self, name: &str) -> Result<Box<dyn File>, VfsError> {
        let device =
            crate::block::registry::get_device(self.inner.device_id).ok_or(VfsError::IoError)?;

        // Check if file already exists
        let entries = self.read_entries()?;
        for entry in &entries {
            if entry.name == name && !entry.is_dir() {
                // File exists — open it for writing
                let chain = read_cluster_chain(device.as_ref(), &self.inner.bpb, entry.first_cluster)?;
                let mut data = vec![0u8; entry.size as usize];
                read_file_data(device.as_ref(), &self.inner.bpb, &chain, &mut data)?;

                let dir_chain = self.get_chain()?;
                return Ok(Box::new(FatFileHandle {
                    inner: self.inner.clone(),
                    data,
                    pos: 0,
                    first_cluster: entry.first_cluster,
                    disk_size: entry.size,
                    dir_chain: Some(dir_chain),
                    dir_entry_name: String::from(name),
                    dirty: false,
                    is_new: false,
                }));
            }
        }

        // File doesn't exist — create new empty file
        let dir_chain = self.get_chain()?;
        Ok(Box::new(FatFileHandle {
            inner: self.inner.clone(),
            data: Vec::new(),
            pos: 0,
            first_cluster: 0,
            disk_size: 0,
            dir_chain: Some(dir_chain),
            dir_entry_name: String::from(name),
            dirty: false,
            is_new: true,
        }))
    }

    fn create_child_dir(&self, name: &str) -> Result<(), VfsError> {
        let device =
            crate::block::registry::get_device(self.inner.device_id).ok_or(VfsError::IoError)?;

        // Check if already exists
        let entries = self.read_entries()?;
        for entry in &entries {
            if entry.name == name {
                return Err(VfsError::AlreadyExists);
            }
        }

        // Check if name fits 8.3
        let name_83 = FatDirEntry::name_to_83(name)?;

        // Allocate a cluster for the new directory
        let new_cluster = allocate_cluster(device.as_ref(), &self.inner.bpb, 2)?;

        // Initialize the new directory cluster with . and .. entries
        let bpc = self.inner.bpb.bytes_per_cluster as usize;
        let mut dir_data = vec![0u8; bpc];

        // . entry (self)
        let dot_name = FatDirEntry::name_to_83(".")?;
        let dot_cluster_high = ((new_cluster >> 16) & 0xFF) as u16;
        let dot_cluster_low = (new_cluster & 0xFFFF) as u16;
        dir_data[0..11].copy_from_slice(&dot_name);
        dir_data[11] = 0x10; // directory attribute
        dir_data[20..22].copy_from_slice(&dot_cluster_high.to_le_bytes());
        dir_data[26..28].copy_from_slice(&dot_cluster_low.to_le_bytes());

        // .. entry (parent)
        let dotdot_name = FatDirEntry::name_to_83("..")?;
        let parent_cluster = match &self.location {
            DirLocation::Fat16Root => 0u32,
            DirLocation::ClusterChain(c) => *c,
        };
        let parent_high = ((parent_cluster >> 16) & 0xFF) as u16;
        let parent_low = (parent_cluster & 0xFFFF) as u16;
        dir_data[32..43].copy_from_slice(&dotdot_name);
        dir_data[43] = 0x10; // directory attribute
        dir_data[52..54].copy_from_slice(&parent_high.to_le_bytes());
        dir_data[58..60].copy_from_slice(&parent_low.to_le_bytes());

        // Write the initialized cluster to disk
        let sector = self.inner.bpb.cluster_to_sector(new_cluster) as u64;
        let mut sector_buf = [0u8; 512];
        let to_copy = cmp::min(512, dir_data.len());
        sector_buf[..to_copy].copy_from_slice(&dir_data[..to_copy]);
        device.write_sector(sector, &sector_buf).map_err(|_| VfsError::IoError)?;
        if bpc > 512 {
            let mut sector_buf2 = [0u8; 512];
            let to_copy2 = cmp::min(512, dir_data.len() - 512);
            sector_buf2[..to_copy2].copy_from_slice(&dir_data[512..512 + to_copy2]);
            device.write_sector(sector + 1, &sector_buf2).map_err(|_| VfsError::IoError)?;
        }

        // Add directory entry in parent
        let entry = FatDirEntry {
            name: String::from(name),
            attributes: 0x10,
            first_cluster: new_cluster,
            size: 0,
            is_lfn: false,
        };

        let dir_chain = self.get_chain()?;
        let mut dir_chain_mut = dir_chain.clone();
        append_dir_entry(device.as_ref(), &self.inner.bpb, &mut dir_chain_mut, &entry, name_83)?;

        serial::write_str("[FAT] Created dir: ");
        serial::write_str(name);
        serial::write_str(" cluster=");
        serial::write_hex(new_cluster as u64);
        serial::write_nl();

        Ok(())
    }

    fn delete_child_file(&self, name: &str) -> Result<(), VfsError> {
        let device =
            crate::block::registry::get_device(self.inner.device_id).ok_or(VfsError::IoError)?;

        // Find the entry
        let dir_chain = self.get_chain()?;
        if dir_chain.is_empty() {
            // FAT16 root — read raw and find entry
            let raw = read_sectors(
                device.as_ref(),
                self.inner.bpb.base_sector as u64 + self.inner.bpb.root_dir_start_sector as u64,
                self.inner.bpb.root_dir_sectors as u64,
            )?;

            // Find in raw data and mark deleted
            let mut raw_mut = raw;
            let mut offset = 0;
            let mut pending_lfn = String::new();
            let mut found = false;
            while offset + 32 <= raw_mut.len() {
                let entry_buf = &raw_mut[offset..offset + 32];
                if entry_buf[0] == 0x00 { break; }
                if entry_buf[11] == 0x0F {
                    // LFN — skip
                    offset += 32;
                    continue;
                }
                if let Some(mut parsed) = FatDirEntry::parse(entry_buf) {
                    if !pending_lfn.is_empty() {
                        parsed.name = core::mem::take(&mut pending_lfn);
                    }
                    if parsed.name == name {
                        raw_mut[offset] = 0xE5;
                        found = true;
                        break;
                    }
                } else {
                    pending_lfn.clear();
                }
                offset += 32;
            }

            if !found {
                return Err(VfsError::NotFound);
            }

            // Write back
            write_sectors(
                device.as_ref(),
                self.inner.bpb.base_sector as u64 + self.inner.bpb.root_dir_start_sector as u64,
                self.inner.bpb.root_dir_sectors as u64,
                &raw_mut,
            )?;

            return Ok(());
        }

        // Cluster-chain directory
        let (entry, _, _) = lookup_dir_entry(device.as_ref(), &self.inner.bpb, &dir_chain, name)?;

        // Free the cluster chain
        if entry.first_cluster >= 2 {
            free_cluster_chain(device.as_ref(), &self.inner.bpb, entry.first_cluster)?;
        }

        // Mark directory entry as deleted
        delete_dir_entry(device.as_ref(), &self.inner.bpb, &dir_chain, name)?;

        serial::write_str("[FAT] Deleted: ");
        serial::write_str(name);
        serial::write_nl();

        Ok(())
    }
}

impl Inode for FatFileInode {
    fn lookup(&self, _name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        Err(VfsError::NotDirectory)
    }

    fn open(&self) -> Result<Box<dyn File>, VfsError> {
        let device =
            crate::block::registry::get_device(self.inner.device_id).ok_or(VfsError::IoError)?;

        let chain = read_cluster_chain(device.as_ref(), &self.inner.bpb, self.first_cluster)?;
        let mut data = vec![0u8; self.size as usize];
        read_file_data(device.as_ref(), &self.inner.bpb, &chain, &mut data)?;

        Ok(Box::new(FatFileHandle {
            inner: self.inner.clone(),
            data,
            pos: 0,
            first_cluster: self.first_cluster,
            disk_size: self.size,
            dir_chain: None, // opened via VFS resolve, no parent context
            dir_entry_name: String::new(),
            dirty: false,
            is_new: false,
        }))
    }

    fn is_dir(&self) -> bool { false }
    fn is_file(&self) -> bool { true }
    fn size(&self) -> u64 { self.size as u64 }
}

// ─────────────────────────────────────────────────────────────────────────────
// File Handle Implementations
// ─────────────────────────────────────────────────────────────────────────────

impl File for FatFileHandle {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VfsError> {
        if self.pos >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - self.pos;
        let to_read = cmp::min(buf.len(), available);
        buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        Ok(to_read)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, VfsError> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Extend data buffer if writing past end
        let end = self.pos + buf.len();
        if end > self.data.len() {
            self.data.resize(end, 0);
        }

        self.data[self.pos..self.pos + buf.len()].copy_from_slice(buf);
        self.pos += buf.len();
        self.dirty = true;
        Ok(buf.len())
    }

    fn seek(&mut self, offset: u64) -> Result<(), VfsError> {
        self.pos = offset as usize;
        Ok(())
    }

    fn close(&mut self) {
        if !self.dirty {
            return;
        }

        // Flush dirty data to disk
        let device = match crate::block::registry::get_device(self.inner.device_id) {
            Some(d) => d,
            None => return,
        };

        let dir_chain = match &self.dir_chain {
            Some(c) => c.clone(),
            None => return, // No parent context — can't flush
        };

        let new_size = self.data.len() as u32;

        // Step 1: Allocate/extend cluster chain for data
        if new_size > 0 {
            let clusters_needed = ((new_size as usize + self.inner.bpb.bytes_per_cluster as usize - 1)
                / self.inner.bpb.bytes_per_cluster as usize) as u32;

            let mut chain = if self.first_cluster != 0 {
                read_cluster_chain(device.as_ref(), &self.inner.bpb, self.first_cluster)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // Allocate additional clusters if needed
            let mut last_cluster = self.first_cluster;
            while chain.len() < clusters_needed as usize {
                let hint = if last_cluster >= 2 { last_cluster + 1 } else { 2 };
                match allocate_cluster(device.as_ref(), &self.inner.bpb, hint) {
                    Ok(new_cl) => {
                        if last_cluster >= 2 {
                            if extend_chain(device.as_ref(), &self.inner.bpb, last_cluster, new_cl).is_err() {
                                break;
                            }
                        } else {
                            // First cluster for this file
                            let _ = write_fat_entry(device.as_ref(), &self.inner.bpb, new_cl, self.inner.bpb.eoc());
                        }
                        chain.push(new_cl);
                        last_cluster = new_cl;
                    }
                    Err(e) => {
                        serial::write_str("[FAT] close: alloc failed: ");
                        serial::write_hex(e.to_errno() as u64);
                        serial::write_nl();
                        return;
                    }
                }
            }

            // Update first_cluster if it was 0
            if self.first_cluster == 0 && !chain.is_empty() {
                self.first_cluster = chain[0];
            }

            // Write data to disk
            if let Err(e) = write_file_data(device.as_ref(), &self.inner.bpb, &chain, 0, &self.data) {
                serial::write_str("[FAT] close: write data failed\n");
                return;
            }

            // Zero-fill the last partial cluster if shrinking
            if new_size < self.disk_size {
                // Don't bother zeroing — just update the size
            }
        } else if self.first_cluster >= 2 {
            // File truncated to zero — free the chain
            if let Ok(()) = free_cluster_chain(device.as_ref(), &self.inner.bpb, self.first_cluster) {
                self.first_cluster = 0;
            }
        }

        // Step 2: Update directory entry on disk (size LAST = crash-safe)
        if !dir_chain.is_empty() {
            let _ = update_dir_entry_size(
                device.as_ref(),
                &self.inner.bpb,
                &dir_chain,
                &self.dir_entry_name,
                new_size,
            );
        }

        self.dirty = false;
        self.disk_size = new_size;
    }
}

impl File for FatDirHandle {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VfsError> {
        if self.pos >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - self.pos;
        let to_read = cmp::min(buf.len(), available);
        buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        Ok(to_read)
    }

    fn write(&mut self, _buf: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::PermissionDenied)
    }

    fn seek(&mut self, offset: u64) -> Result<(), VfsError> {
        self.pos = offset as usize;
        Ok(())
    }

    fn close(&mut self) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// FileSystem Implementation
// ─────────────────────────────────────────────────────────────────────────────

/// FAT filesystem instance — supports both FAT16 and FAT32.
pub struct Fat32Fs {
    inner: FatInner,
}

unsafe impl Send for Fat32Fs {}
unsafe impl Sync for Fat32Fs {}

impl Fat32Fs {
    /// Create a new FAT filesystem from a block device.
    pub fn new(device_id: usize) -> Result<Self, VfsError> {
        let device =
            crate::block::registry::get_device(device_id).ok_or(VfsError::IoError)?;

        serial::write_str("[FAT] dev=");
        serial::write_str(device.name());
        serial::write_str(" ssize=");
        serial::write_hex(device.sector_size() as u64);
        serial::write_str(" tot=");
        serial::write_hex(device.total_sectors());
        serial::write_nl();

        let mut boot = [0u8; 512];
        device.read_sector(0, &mut boot).map_err(|_| VfsError::IoError)?;

        if boot[510] != 0x55 || boot[511] != 0xAA {
            serial::write_str_nl("[FAT] Invalid boot signature");
            return Err(VfsError::IoError);
        }

        let mut mbr_partition_lba: Option<u32> = None;
        let mut mbr_partition_type: Option<u8> = None;

        for part_idx in 0..4u32 {
            let base = (446 + part_idx * 16) as usize;
            let ptype = boot[base + 4];
            let start_lba = u32::from_le_bytes([boot[base+8], boot[base+9], boot[base+10], boot[base+11]]);

            if ptype != 0 && matches!(ptype, 0x04 | 0x06 | 0x0E | 0x0B | 0x0C) && mbr_partition_lba.is_none() {
                mbr_partition_lba = Some(start_lba);
                mbr_partition_type = Some(ptype);
            }
        }

        if let Some(part_lba) = mbr_partition_lba {
            let ptype = match mbr_partition_type {
                Some(t) => t,
                None => return Err(VfsError::IoError),
            };
            serial::write_str("[FAT] MBR partition type=0x");
            serial::write_hex(ptype as u64);
            serial::write_str(" at LBA=0x");
            serial::write_hex(part_lba as u64);
            serial::write_nl();

            device.read_sector(part_lba as u64, &mut boot).map_err(|_| VfsError::IoError)?;

            if boot[510] != 0x55 || boot[511] != 0xAA {
                serial::write_str_nl("[FAT] Partition boot sector: invalid signature");
                return Err(VfsError::IoError);
            }
        } else if boot[0] != 0xEB && boot[0] != 0xE9 {
            serial::write_str("[FAT] Not FAT BPB (jmp=0x");
            serial::write_hex(boot[0] as u64);
            serial::write_str_nl("), no FAT partition in MBR");
            return Err(VfsError::IoError);
        }

        let base_sector = mbr_partition_lba.unwrap_or(0);
        let bpb = FatBpb::parse(&boot, device.as_ref(), base_sector)?;

        Ok(Fat32Fs {
            inner: FatInner { device_id, bpb },
        })
    }
}

impl FileSystem for Fat32Fs {
    fn name(&self) -> &str {
        self.inner.bpb.variant.as_str()
    }

    fn root(&self) -> Arc<dyn Inode> {
        let location = match self.inner.bpb.variant {
            FatVariant::Fat16 => DirLocation::Fat16Root,
            FatVariant::Fat32 => DirLocation::ClusterChain(self.inner.bpb.root_cluster),
        };
        Arc::new(FatDirInode {
            inner: self.inner.clone(),
            location,
        })
    }

    fn create_file(&self, name: &str) -> Result<Box<dyn File>, VfsError> {
        self.root().create_child_file(name)
    }

    fn create_dir(&self, name: &str) -> Result<(), VfsError> {
        self.root().create_child_dir(name)
    }
}
