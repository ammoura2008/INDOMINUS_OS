pub mod ramfs;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// VFS error codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    NotFile,
    NotDirectory,
    TooManyOpenFiles,
    InvalidSeek,
    NoSpace,
    IoError,
    BadPath,
}

impl VfsError {
    pub fn to_errno(self) -> i64 {
        match self {
            VfsError::NotFound => -2,
            VfsError::PermissionDenied => -1,
            VfsError::AlreadyExists => -17,
            VfsError::NotFile => -20,
            VfsError::NotDirectory => -20,
            VfsError::TooManyOpenFiles => -24,
            VfsError::InvalidSeek => -29,
            VfsError::NoSpace => -28,
            VfsError::IoError => -5,
            VfsError::BadPath => -2,
        }
    }
}

/// File operations trait
pub trait File: Send + Sync {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VfsError>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, VfsError>;
    fn seek(&mut self, offset: u64) -> Result<(), VfsError>;
    fn close(&mut self);
    fn isatty(&self) -> bool { false }
}

/// Inode operations trait (read-only lookups, creation through filesystem)
pub trait Inode: Send + Sync {
    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError>;
    fn open(&self) -> Result<Box<dyn File>, VfsError>;
    fn is_dir(&self) -> bool;
    fn is_file(&self) -> bool;
    fn size(&self) -> u64;
    fn readdir(&self) -> Result<Vec<String>, VfsError> {
        Err(VfsError::NotDirectory)
    }
    fn create_child_file(&self, _name: &str) -> Result<Box<dyn File>, VfsError> {
        Err(VfsError::NotDirectory)
    }
    fn create_child_dir(&self, _name: &str) -> Result<(), VfsError> {
        Err(VfsError::NotDirectory)
    }
}

/// Filesystem trait (supports creation)
pub trait FileSystem: Send + Sync {
    fn name(&self) -> &str;
    fn root(&self) -> Arc<dyn Inode>;
    fn create_file(&self, name: &str) -> Result<Box<dyn File>, VfsError>;
    fn create_dir(&self, name: &str) -> Result<(), VfsError>;
}

/// Mount point entry
struct MountEntry {
    path: String,
    fs: Arc<dyn FileSystem>,
}

/// Global VFS state
pub struct Vfs {
    mounts: Vec<MountEntry>,
}

impl Vfs {
    pub const fn new() -> Self {
        Vfs {
            mounts: Vec::new(),
        }
    }

    pub fn init(&mut self) {
        let root_fs = Arc::new(ramfs::RamFs::new());
        self.mounts.push(MountEntry {
            path: String::from("/"),
            fs: root_fs,
        });
    }

    pub fn mount(&mut self, path: &str, fs: Arc<dyn FileSystem>) {
        self.mounts.push(MountEntry {
            path: String::from(path),
            fs,
        });
    }

    /// Resolve a path to an inode.
    /// Checks mount points first, then falls back to root fs.
    pub fn resolve(&self, path: &str) -> Result<Arc<dyn Inode>, VfsError> {
        if path == "/" || path.is_empty() {
            return Ok(self.root_fs()?.root());
        }

        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return Ok(self.root_fs()?.root());
        }

        // Check if any mount point matches a prefix of the path
        for mount in &self.mounts {
            if mount.path == "/" {
                continue;
            }
            let mount_parts: Vec<&str> = mount.path.split('/').filter(|s| !s.is_empty()).collect();
            if mount_parts.len() <= parts.len() {
                let matches = mount_parts.iter().zip(parts.iter()).all(|(mp, p)| mp == p);
                if matches {
                    // Resolve remaining path within this mount's root
                    let root = mount.fs.root();
                    let remaining = &parts[mount_parts.len()..];
                    let mut current = root;
                    for part in remaining {
                        current = current.lookup(part)?;
                    }
                    return Ok(current);
                }
            }
        }

        // Fall back to root filesystem
        let root = self.root_fs()?.root();
        let mut current = root;
        for part in parts {
            current = current.lookup(part)?;
        }
        Ok(current)
    }

    /// Open a file by path
    pub fn open(&self, path: &str) -> Result<Box<dyn File>, VfsError> {
        let inode = self.resolve(path)?;
        inode.open()
    }

    /// Create a file by path (truncates if exists).
    /// Handles nested paths by resolving the parent directory and creating
    /// the file within it. Auto-creates intermediate directories as needed.
    pub fn create_file(&self, path: &str) -> Result<Box<dyn File>, VfsError> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return Err(VfsError::BadPath);
        }

        let fs = self.root_fs()?;

        // Single part: create at root
        if parts.len() == 1 {
            if let Ok(inode) = self.resolve(path) {
                if inode.is_file() {
                    return inode.open();
                }
            }
            return fs.create_file(parts[0]);
        }

        // Multi-part: resolve parent directory, creating intermediates as needed
        let parent = self.resolve_or_create_parents(&parts[..parts.len() - 1])?;

        if !parent.is_dir() {
            return Err(VfsError::NotDirectory);
        }

        let file_name = parts[parts.len() - 1];

        // Try to open existing file
        if let Ok(child) = parent.lookup(file_name) {
            if child.is_file() {
                return child.open();
            }
        }

        // Create new file in parent directory
        parent.create_child_file(file_name)
    }

    /// Create a directory by path.
    pub fn create_dir(&self, path: &str) -> Result<(), VfsError> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return Err(VfsError::BadPath);
        }

        let fs = self.root_fs()?;

        // Single part: create at root
        if parts.len() == 1 {
            return fs.create_dir(parts[0]);
        }

        // Multi-part: resolve parent directory, creating intermediates as needed
        let parent = self.resolve_or_create_parents(&parts[..parts.len() - 1])?;

        if !parent.is_dir() {
            return Err(VfsError::NotDirectory);
        }

        let dir_name = parts[parts.len() - 1];
        parent.create_child_dir(dir_name)
    }

    /// Resolve a path, auto-creating intermediate directories that don't exist.
    fn resolve_or_create_parents(&self, parts: &[&str]) -> Result<Arc<dyn Inode>, VfsError> {
        let fs = self.root_fs()?;
        let root = fs.root();
        let mut current = root;

        for &part in parts {
            match current.lookup(part) {
                Ok(inode) => {
                    if !inode.is_dir() {
                        return Err(VfsError::NotDirectory);
                    }
                    current = inode;
                }
                Err(_) => {
                    // Directory doesn't exist — create it
                    current.create_child_dir(part)?;
                    current = current.lookup(part)?;
                }
            }
        }

        Ok(current)
    }

    /// Write bytes to a file (overwrite)
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<usize, VfsError> {
        let mut file = self.create_file(path)?;
        file.write(data)
    }

    /// Read all bytes from a file
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        let mut file = self.open(path)?;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 512];
        loop {
            match file.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) => return Err(e),
            }
        }
        Ok(buf)
    }

    fn root_fs(&self) -> Result<Arc<dyn FileSystem>, VfsError> {
        self.mounts
            .iter()
            .find(|m| m.path == "/")
            .map(|m| m.fs.clone())
            .ok_or(VfsError::NotFound)
    }
}

/// Global VFS instance
static VFS: crate::sync_cell::SyncUnsafeCell<Option<Vfs>> = crate::sync_cell::SyncUnsafeCell::new(None);

/// Initialize the VFS (called once at boot)
pub fn init() {
    unsafe {
        let mut vfs = Vfs::new();
        vfs.init();
        *VFS.get() = Some(vfs);
    }
}

/// Get a reference to the global VFS
pub fn vfs() -> &'static Vfs {
    unsafe { (*VFS.get()).as_ref().expect("VFS not initialized") }
}

/// Get a mutable reference to the global VFS
pub fn vfs_mut() -> &'static mut Vfs {
    unsafe { (*VFS.get()).as_mut().expect("VFS not initialized") }
}
