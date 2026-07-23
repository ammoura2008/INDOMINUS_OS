pub mod ramdisk;
pub mod registry;

/// Block device error codes.
///
/// These represent hardware-independent errors for block I/O operations.
/// The `to_errno()` method converts to Linux-compatible errno values
/// for syscall integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// Buffer size does not match the device's sector size.
    InvalidBufferSize,
    /// Logical block address is out of device bounds.
    OutOfBounds,
    /// Device is not ready or not present.
    DeviceNotReady,
    /// I/O error at the hardware level.
    IoError,
    /// Device is read-only.
    ReadOnly,
    /// Device registry is full.
    TooManyDevices,
    /// Device with this ID already exists.
    DeviceAlreadyExists,
    /// No device found with the requested ID.
    NoSuchDevice,
}

impl BlockError {
    pub fn to_errno(self) -> i64 {
        match self {
            BlockError::InvalidBufferSize => -22,   // EINVAL
            BlockError::OutOfBounds => -22,          // EINVAL
            BlockError::DeviceNotReady => -19,       // ENODEV
            BlockError::IoError => -5,               // EIO
            BlockError::ReadOnly => -30,             // EROFS
            BlockError::TooManyDevices => -24,       // EMFILE
            BlockError::DeviceAlreadyExists => -17,  // EEXIST
            BlockError::NoSuchDevice => -19,         // ENODEV
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            BlockError::InvalidBufferSize => "buffer size does not match sector size",
            BlockError::OutOfBounds => "logical block address out of device bounds",
            BlockError::DeviceNotReady => "device not ready or not present",
            BlockError::IoError => "I/O error",
            BlockError::ReadOnly => "device is read-only",
            BlockError::TooManyDevices => "device registry full",
            BlockError::DeviceAlreadyExists => "device already registered",
            BlockError::NoSuchDevice => "no device with requested ID",
        }
    }
}

/// Hardware-agnostic block device interface.
///
/// This trait abstracts storage hardware (AHCI, NVMe, VirtIO, USB, RAM)
/// behind a uniform sector-based I/O API. Filesystems and higher layers
/// depend only on this trait, never on specific hardware drivers.
///
/// Sector size is a property of the device, not the API. Callers must
/// provide buffers of exactly `sector_size()` bytes for each read/write.
/// Future versions may support multi-sector transfers, but the initial
/// API enforces single-sector operations for simplicity and safety.
pub trait BlockDevice: Send + Sync {
    /// Read a single sector from the device.
    ///
    /// `lba` is the logical block address (sector number, 0-indexed).
    /// `buf` must be exactly `sector_size()` bytes.
    ///
    /// # Errors
    /// - `InvalidBufferSize` if `buf.len() != sector_size()`
    /// - `OutOfBounds` if `lba >= total_sectors()`
    /// - `DeviceNotReady` if the device is not initialized
    /// - `IoError` on hardware read failure
    fn read_sector(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError>;

    /// Write a single sector to the device.
    ///
    /// `lba` is the logical block address (sector number, 0-indexed).
    /// `buf` must be exactly `sector_size()` bytes.
    ///
    /// # Errors
    /// - `InvalidBufferSize` if `buf.len() != sector_size()`
    /// - `OutOfBounds` if `lba >= total_sectors()`
    /// - `ReadOnly` if the device is read-only
    /// - `DeviceNotReady` if the device is not initialized
    /// - `IoError` on hardware write failure
    fn write_sector(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError>;

    /// Returns the size of each sector in bytes.
    ///
    /// Initial implementation assumes 512-byte sectors (standard for most
    /// storage devices). Future devices (NVMe) may report larger sizes.
    /// Callers must not hardcode 512; always use this method.
    fn sector_size(&self) -> u32;

    /// Returns the total number of sectors on the device.
    ///
    /// Valid LBAs are `0..total_sectors()`.
    fn total_sectors(&self) -> u64;

    /// Returns a human-readable device name for logging.
    fn name(&self) -> &str;
}
