//! # FAT Filesystem Driver (FAT16 + FAT32)
//!
//! Read-only FAT16/FAT32 implementation backed by a block device.
//! Detects FAT variant from BPB, follows FAT cluster chains, reads directory
//! entries (8.3 + LFN). Does NOT implement write/allocation.
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

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

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
    /// Absolute LBA of the first sector of this FAT partition (for MBR-partitioned disks)
    base_sector: u32,
    /// First sector of the data area (after all FATs and root dir for FAT16)
    data_start_sector: u32,
    bytes_per_cluster: u32,
    // FAT16-specific
    root_entry_count: u16,    // FAT16 only; 0 for FAT32
    /// Start sector of root directory (FAT16 only)
    root_dir_start_sector: u32,
    /// Size of root directory in sectors (FAT16 only)
    root_dir_sectors: u32,
}

impl FatBpb {
    /// Parse BPB from a 512-byte boot sector buffer.
    fn parse(buf: &[u8], device: &dyn BlockDevice, base_sector: u32) -> Result<Self, VfsError> {
        if buf.len() < 512 {
            return Err(VfsError::IoError);
        }

        // Common fields
        let bps = u16::from_le_bytes([buf[11], buf[12]]);
        let spc = buf[13];
        let rs = u16::from_le_bytes([buf[14], buf[15]]);
        let nf = buf[16];
        let media = buf[21];

        if bps == 0 || spc == 0 {
            serial::write_str_nl("[FAT] Invalid BPB (bps or spc is zero)");
            return Err(VfsError::IoError);
        }

        let bpc = bps as u32 * spc as u32;

        // Determine FAT variant from BPB layout.
        // FAT16: root_entry_count at offset 17 is non-zero, fat_sz16 at offset 22 is non-zero.
        // FAT32: root_entry_count at offset 17 is 0, fat_sz32 at offset 36 is non-zero.
        // NOTE: For FAT16, offset 36-39 contains drive number/serial (often non-zero),
        // so we must check fat_sz16 BEFORE fat_sz32 to avoid false FAT32 detection.
        let fat_sz32 = u32::from_le_bytes([buf[36], buf[37], buf[38], buf[39]]);
        let fat_sz16 = u16::from_le_bytes([buf[22], buf[23]]);
        let root_entry_count = u16::from_le_bytes([buf[17], buf[18]]);

        let variant;
        let fat_size_sectors: u32;
        let total_sectors: u32;
        let root_cluster: u32;

        if fat_sz16 != 0 && root_entry_count != 0 {
            // FAT16: has root_entry_count and 16-bit FAT size
            variant = FatVariant::Fat16;
            fat_size_sectors = fat_sz16 as u32;
            total_sectors = {
                let ts16 = u16::from_le_bytes([buf[19], buf[20]]);
                if ts16 != 0 { ts16 as u32 } else {
                    u32::from_le_bytes([buf[32], buf[33], buf[34], buf[35]])
                }
            };
            root_cluster = 0; // Not used for FAT16 root
        } else if fat_sz32 != 0 {
            variant = FatVariant::Fat32;
            fat_size_sectors = fat_sz32;
            total_sectors = u32::from_le_bytes([buf[32], buf[33], buf[34], buf[35]]);
            root_cluster = u32::from_le_bytes([buf[44], buf[45], buf[46], buf[47]]);
        } else {
            serial::write_str_nl("[FAT] Unsupported: FAT12 or corrupt BPB");
            return Err(VfsError::IoError);
        }

        // Data area starts after reserved sectors + all FAT copies
        let fat_area_end = rs as u32 + nf as u32 * fat_size_sectors;

        // FAT16: root directory is a fixed area between FATs and data
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
            return None; // Volume label
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
}

// ─────────────────────────────────────────────────────────────────────────────
// FAT Cluster Chain Reader
// ─────────────────────────────────────────────────────────────────────────────

/// Read the FAT entry for a given cluster.
/// Handles FAT16 (16-bit entries) and FAT32 (28-bit entries).
fn read_fat_entry(
    device: &dyn BlockDevice,
    bpb: &FatBpb,
    cluster: u32,
) -> Result<u32, VfsError> {
    match bpb.variant {
        FatVariant::Fat16 => {
            // Each FAT16 entry is 2 bytes. Entry at byte offset cluster*2.
            let entry_byte = cluster as u64 * 2;
            let fat_sector = bpb.base_sector as u64 + bpb.reserved_sectors as u64 + entry_byte / 512;
            let entry_in_sector = (entry_byte % 512) as usize;

            let mut buf = [0u8; 512];
            device.read_sector(fat_sector, &mut buf).map_err(|_| VfsError::IoError)?;
            let raw = u16::from_le_bytes([buf[entry_in_sector], buf[entry_in_sector + 1]]);
            Ok(raw as u32)
        }
        FatVariant::Fat32 => {
            // Each FAT32 entry is 4 bytes. Entry at byte offset cluster*4.
            let entry_byte = cluster as u64 * 4;
            let fat_sector = bpb.base_sector as u64 + bpb.reserved_sectors as u64 + entry_byte / 512;
            let entry_in_sector = (entry_byte % 512) as usize;

            let mut buf = [0u8; 512];
            device.read_sector(fat_sector, &mut buf).map_err(|_| VfsError::IoError)?;
            let raw = u32::from_le_bytes([
                buf[entry_in_sector],
                buf[entry_in_sector + 1],
                buf[entry_in_sector + 2],
                buf[entry_in_sector + 3] & 0x0F, // mask high 4 reserved bits
            ]);
            Ok(raw)
        }
    }
}

/// Follow the FAT cluster chain from a starting cluster.
/// Returns a Vec of cluster numbers in order.
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
            break; // Prevent infinite loops
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
            device
                .read_sector(sector + s, &mut sector_buf)
                .map_err(|_| VfsError::IoError)?;
            let to_copy = core::cmp::min(512, buf_len - bytes_read);
            buf[bytes_read..bytes_read + to_copy]
                .copy_from_slice(&sector_buf[..to_copy]);
            bytes_read += to_copy;
        }
    }

    Ok(bytes_read)
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

/// Parse directory entries from a raw byte buffer (shared by FAT16 root,
/// FAT16 subdirectories, and FAT32 directories).
fn parse_dir_entries(raw_data: &[u8]) -> Result<Vec<FatDirEntry>, VfsError> {
    let mut entries = Vec::new();
    let mut i = 0;
    let mut pending_lfn = String::new();

    while i + 32 <= raw_data.len() {
        let entry_buf = &raw_data[i..i + 32];

        if entry_buf[0] == 0x00 {
            break; // End of directory
        }

        if entry_buf[11] == 0x0F {
            // Long filename entry — extract ASCII chars
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

/// Read all directory entries from a cluster chain (FAT16 subdirs + FAT32).
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
    /// FAT16 root directory: fixed area on disk, not a cluster chain.
    Fat16Root,
    /// Any directory accessible via a cluster chain (FAT32 root, FAT16 subdirs).
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
struct FatFileHandle {
    data: Vec<u8>,
    pos: usize,
}

unsafe impl Send for FatFileHandle {}
unsafe impl Sync for FatFileHandle {}

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
    /// Read directory entries from this inode's location.
    fn read_entries(&self) -> Result<Vec<FatDirEntry>, VfsError> {
        let device =
            crate::block::registry::get_device(self.inner.device_id).ok_or(VfsError::IoError)?;

        match &self.location {
            DirLocation::Fat16Root => {
                // FAT16 root: read from fixed location on disk
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
}

impl Inode for FatDirInode {
    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        let entries = self.read_entries()?;
        for entry in &entries {
            if entry.name == name {
                if entry.attributes & 0x10 != 0 {
                    // Subdirectory
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

    fn is_dir(&self) -> bool {
        true
    }

    fn is_file(&self) -> bool {
        false
    }

    fn size(&self) -> u64 {
        0
    }

    fn readdir(&self) -> Result<Vec<String>, VfsError> {
        let entries = self.read_entries()?;
        Ok(entries.iter().map(|e| e.name.clone()).collect())
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
            data,
            pos: 0,
        }))
    }

    fn is_dir(&self) -> bool {
        false
    }

    fn is_file(&self) -> bool {
        true
    }

    fn size(&self) -> u64 {
        self.size as u64
    }
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
        let to_read = core::cmp::min(buf.len(), available);
        buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        Ok(to_read)
    }

    fn write(&mut self, _buf: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::IoError)
    }

    fn seek(&mut self, offset: u64) -> Result<(), VfsError> {
        self.pos = offset as usize;
        Ok(())
    }

    fn close(&mut self) {}
}

impl File for FatDirHandle {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VfsError> {
        if self.pos >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - self.pos;
        let to_read = core::cmp::min(buf.len(), available);
        buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        Ok(to_read)
    }

    fn write(&mut self, _buf: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::IoError)
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
    /// Detects FAT variant (FAT16 or FAT32) from the BPB.
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

        // Check boot signature at bytes 510-511
        if boot[510] != 0x55 || boot[511] != 0xAA {
            serial::write_str_nl("[FAT] Invalid boot signature");
            return Err(VfsError::IoError);
        }

        // Detect MBR partition table
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
            let ptype = mbr_partition_type.unwrap();
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

    fn create_file(&self, _name: &str) -> Result<Box<dyn File>, VfsError> {
        Err(VfsError::IoError) // Read-only
    }

    fn create_dir(&self, _name: &str) -> Result<(), VfsError> {
        Err(VfsError::IoError) // Read-only
    }
}
