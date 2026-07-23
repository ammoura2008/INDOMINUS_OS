use core::cell::UnsafeCell;

/// A wrapper around `UnsafeCell<T>` that implements `Sync`.
///
/// # Safety Contract
/// The caller must ensure that:
/// - All accesses are protected by disabling interrupts or holding a lock.
/// - Only one core accesses the cell at a time (single-threaded init, or locks for SMP).
pub struct SyncUnsafeCell<T>(UnsafeCell<T>);

// Safety: Protected by interrupts-disabled or lock at all call sites.
unsafe impl<T> Sync for SyncUnsafeCell<T> {}
unsafe impl<T> Send for SyncUnsafeCell<T> {}

impl<T> SyncUnsafeCell<T> {
    pub const fn new(val: T) -> Self {
        Self(UnsafeCell::new(val))
    }

    #[inline]
    pub fn get(&self) -> *mut T {
        self.0.get()
    }
}
