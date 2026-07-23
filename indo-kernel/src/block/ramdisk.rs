use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;
use super::{BlockDevice, BlockError};

/// A simple in-memory block device for development and testing.
///
/// The RAM disk allocates a fixed amount of heap memory and exposes
/// it as a block device. This allows higher layers (filesystems, VFS)
/// to be tested without real hardware.
///
/// Future drivers (AHCI, NVMe, VirtIO) will implement the same
/// `BlockDevice` trait, making them interchangeable with RAM disk.
///
/// Interior mutability is handled via `Mutex<Vec<u8>>` because the
/// `BlockDevice` trait takes `&self` for write operations (matching
/// real hardware where writes go through MMIO registers despite
/// shared references).
pub struct RamDisk {
    data: Mutex<Vec<u8>>,
    sector_size: u32,
    total_sectors: u64,
}

impl RamDisk {
    /// Create a new RAM disk with the given capacity in sectors.
    ///
    /// Allocates `num_sectors * sector_size` bytes on the heap.
    /// All bytes are initialized to zero.
    ///
    /// # Arguments
    /// * `num_sectors` - Number of sectors (must be > 0)
    /// * `sector_size` - Bytes per sector (typically 512)
    ///
    /// # Panics
    /// Panics if `num_sectors` is 0 or if allocation fails.
    pub fn new(num_sectors: u64, sector_size: u32) -> Self {
        assert!(num_sectors > 0, "RAM disk: num_sectors must be > 0");
        assert!(sector_size > 0, "RAM disk: sector_size must be > 0");

        // Check for overflow in total size calculation
        let total_bytes = num_sectors
            .checked_mul(sector_size as u64)
            .expect("RAM disk: size overflow");

        // Reasonable limit: 16 MiB for development
        assert!(
            total_bytes <= 16 * 1024 * 1024,
            "RAM disk: size {} bytes exceeds 16 MiB limit",
            total_bytes
        );

        let data = vec![0u8; total_bytes as usize];

        crate::serial::write_str("[BLOCK] RamDisk created: sectors=");
        crate::serial::write_hex(num_sectors);
        crate::serial::write_str(" sector_size=");
        crate::serial::write_hex(sector_size as u64);
        crate::serial::write_str(" total=");
        crate::serial::write_hex(total_bytes);
        crate::serial::write_nl();

        RamDisk {
            data: Mutex::new(data),
            sector_size,
            total_sectors: num_sectors,
        }
    }
}

impl BlockDevice for RamDisk {
    fn read_sector(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        // Validate buffer size
        if buf.len() != self.sector_size as usize {
            return Err(BlockError::InvalidBufferSize);
        }

        // Validate LBA
        if lba >= self.total_sectors {
            return Err(BlockError::OutOfBounds);
        }

        // Calculate byte offset (overflow-safe)
        let offset = lba
            .checked_mul(self.sector_size as u64)
            .ok_or(BlockError::OutOfBounds)? as usize;

        // Copy data
        let end = offset + self.sector_size as usize;
        let data = self.data.lock();
        buf.copy_from_slice(&data[offset..end]);

        Ok(())
    }

    fn write_sector(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        // Validate buffer size
        if buf.len() != self.sector_size as usize {
            return Err(BlockError::InvalidBufferSize);
        }

        // Validate LBA
        if lba >= self.total_sectors {
            return Err(BlockError::OutOfBounds);
        }

        // Calculate byte offset (overflow-safe)
        let offset = lba
            .checked_mul(self.sector_size as u64)
            .ok_or(BlockError::OutOfBounds)? as usize;

        // Copy data
        let end = offset + self.sector_size as usize;
        let mut data = self.data.lock();
        data[offset..end].copy_from_slice(buf);

        Ok(())
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    fn name(&self) -> &str {
        "ramdisk"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ramdisk_read_write() {
        let rd = RamDisk::new(8, 512);
        let mut read_buf = [0u8; 512];

        // Write known pattern to sector 0
        let mut write_buf = [0u8; 512];
        for i in 0..512 {
            write_buf[i] = (i % 256) as u8;
        }
        assert!(rd.write_sector(0, &write_buf).is_ok());

        // Read it back
        assert!(rd.read_sector(0, &mut read_buf).is_ok());
        assert_eq!(write_buf, read_buf);
    }

    #[test]
    fn test_ramdisk_bounds_check() {
        let rd = RamDisk::new(4, 512);
        let mut buf = [0u8; 512];

        // Valid access
        assert!(rd.read_sector(3, &mut buf).is_ok());

        // Out of bounds
        assert_eq!(rd.read_sector(4, &mut buf), Err(BlockError::OutOfBounds));
        assert_eq!(rd.read_sector(100, &mut buf), Err(BlockError::OutOfBounds));
    }

    #[test]
    fn test_ramdisk_wrong_buffer_size() {
        let rd = RamDisk::new(4, 512);
        let mut small_buf = [0u8; 256];
        let mut big_buf = [0u8; 1024];

        assert_eq!(
            rd.read_sector(0, &mut small_buf),
            Err(BlockError::InvalidBufferSize)
        );
        assert_eq!(
            rd.read_sector(0, &mut big_buf),
            Err(BlockError::InvalidBufferSize)
        );
    }
}
