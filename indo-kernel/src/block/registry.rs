use alloc::sync::Arc;
use spin::Mutex;
use super::{BlockDevice, BlockError};

/// Maximum number of block devices that can be registered.
///
/// This is a fixed limit to avoid dynamic allocation in the registry.
/// AHCI may expose multiple ports, NVMe may expose multiple namespaces,
/// but 16 is more than sufficient for a single-machine OS.
const MAX_DEVICES: usize = 16;

/// Global block device registry.
///
/// Provides safe registration and lookup of block devices. Devices are
/// identified by numeric IDs (0..MAX_DEVICES). Higher layers (filesystems,
/// VFS) obtain `Arc<dyn BlockDevice>` handles from the registry.
///
/// The registry uses `spin::Mutex` for safe concurrent access, matching
/// the pattern used by PCI device enumeration.
pub struct BlockDeviceRegistry {
    devices: [Option<Arc<dyn BlockDevice>>; MAX_DEVICES],
}

impl BlockDeviceRegistry {
    /// Create a new empty registry.
    const fn new() -> Self {
        // const fn requires manual array initialization
        // We cannot use [None; MAX_DEVICES] in const context with Option<T>
        // that isn't Copy, so we build it manually.
        BlockDeviceRegistry {
            devices: [
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
            ],
        }
    }

    /// Register a block device, returning its assigned ID.
    ///
    /// The device is assigned the first available ID in `0..MAX_DEVICES`.
    /// Returns `Err(TooManyDevices)` if all slots are occupied.
    fn register(&mut self, device: Arc<dyn BlockDevice>) -> Result<usize, BlockError> {
        for i in 0..MAX_DEVICES {
            if self.devices[i].is_none() {
                self.devices[i] = Some(device);
                return Ok(i);
            }
        }
        Err(BlockError::TooManyDevices)
    }

    /// Look up a device by its ID.
    ///
    /// Returns `None` if no device is registered with the given ID.
    fn get(&self, id: usize) -> Option<Arc<dyn BlockDevice>> {
        if id >= MAX_DEVICES {
            return None;
        }
        self.devices[id].clone()
    }

    /// Remove a device from the registry.
    ///
    /// Returns the removed device, or `None` if the slot was empty.
    fn unregister(&mut self, id: usize) -> Option<Arc<dyn BlockDevice>> {
        if id >= MAX_DEVICES {
            return None;
        }
        self.devices[id].take()
    }
}

/// Global registry instance.
static REGISTRY: Mutex<BlockDeviceRegistry> = Mutex::new(BlockDeviceRegistry::new());

/// Register a block device globally.
///
/// Returns the assigned device ID on success.
pub fn register_device(device: Arc<dyn BlockDevice>) -> Result<usize, BlockError> {
    let mut reg = REGISTRY.lock();
    let id = reg.register(device.clone())?;
    crate::serial::write_str("[BLOCK] Registered device id=");
    crate::serial::write_hex(id as u64);
    crate::serial::write_str(" name=");
    crate::serial::write_str(device.name());
    crate::serial::write_nl();
    Ok(id)
}

/// Look up a device by ID.
pub fn get_device(id: usize) -> Option<Arc<dyn BlockDevice>> {
    let reg = REGISTRY.lock();
    reg.get(id)
}

/// Remove a device from the registry.
pub fn unregister_device(id: usize) -> Option<Arc<dyn BlockDevice>> {
    let mut reg = REGISTRY.lock();
    reg.unregister(id)
}
