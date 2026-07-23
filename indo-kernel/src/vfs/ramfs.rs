use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use super::{File, FileSystem, Inode, VfsError};

/// RAM filesystem — simple in-memory key-value store
pub struct RamFs {
    root: Arc<Mutex<RamDir>>,
}

impl RamFs {
    pub fn new() -> Self {
        let root = Arc::new(Mutex::new(RamDir::new()));
        RamFs { root }
    }
}

impl FileSystem for RamFs {
    fn name(&self) -> &str { "ramfs" }

    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(RamDirInode { inner: self.root.clone() }) as Arc<dyn Inode>
    }

    fn create_file(&self, name: &str) -> Result<Box<dyn File>, VfsError> {
        let mut dir = self.root.lock();
        let data = match dir.entries.get(name) {
            Some(RamNode::File(existing)) => existing.clone(),
            Some(RamNode::Dir(_)) => return Err(VfsError::AlreadyExists),
            None => {
                let data = Arc::new(Mutex::new(Vec::new()));
                dir.entries.insert(String::from(name), RamNode::File(data.clone()));
                data
            }
        };
        Ok(Box::new(RamFileHandle { data, pos: 0 }))
    }

    fn create_dir(&self, name: &str) -> Result<(), VfsError> {
        let mut dir = self.root.lock();
        if dir.entries.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let subdir = Arc::new(Mutex::new(RamDir::new()));
        dir.entries.insert(String::from(name), RamNode::Dir(subdir));
        Ok(())
    }
}

/// Directory node wrapped as an Inode
pub struct RamDirInode {
    inner: Arc<Mutex<RamDir>>,
}

impl Inode for RamDirInode {
    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        let dir = self.inner.lock();
        match dir.entries.get(name) {
            Some(RamNode::File(data)) => Ok(Arc::new(RamFileInode { data: data.clone() }) as Arc<dyn Inode>),
            Some(RamNode::Dir(subdir)) => Ok(Arc::new(RamDirInode { inner: subdir.clone() }) as Arc<dyn Inode>),
            None => Err(VfsError::NotFound),
        }
    }

    fn open(&self) -> Result<Box<dyn File>, VfsError> {
        // Open a directory as a file: returns entries as null-terminated strings
        let entries = {
            let dir = self.inner.lock();
            dir.list_entries()
        };
        Ok(Box::new(RamDirHandle { entries, pos: 0 }))
    }

    fn is_dir(&self) -> bool { true }
    fn is_file(&self) -> bool { false }
    fn size(&self) -> u64 { 0 }

    fn readdir(&self) -> Result<Vec<String>, VfsError> {
        let dir = self.inner.lock();
        Ok(dir.list_entries())
    }

    fn create_child_file(&self, name: &str) -> Result<Box<dyn File>, VfsError> {
        let mut dir = self.inner.lock();
        let data = match dir.entries.get(name) {
            Some(RamNode::File(existing)) => existing.clone(),
            Some(RamNode::Dir(_)) => return Err(VfsError::AlreadyExists),
            None => {
                let data = Arc::new(Mutex::new(Vec::new()));
                dir.entries.insert(String::from(name), RamNode::File(data.clone()));
                data
            }
        };
        Ok(Box::new(RamFileHandle { data, pos: 0 }))
    }

    fn create_child_dir(&self, name: &str) -> Result<(), VfsError> {
        let mut dir = self.inner.lock();
        if dir.entries.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let subdir = Arc::new(Mutex::new(RamDir::new()));
        dir.entries.insert(String::from(name), RamNode::Dir(subdir));
        Ok(())
    }
}

/// File node wrapped as an Inode
pub struct RamFileInode {
    data: Arc<Mutex<Vec<u8>>>,
}

impl Inode for RamFileInode {
    fn lookup(&self, _name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        Err(VfsError::NotDirectory)
    }

    fn open(&self) -> Result<Box<dyn File>, VfsError> {
        Ok(Box::new(RamFileHandle { data: self.data.clone(), pos: 0 }))
    }

    fn is_dir(&self) -> bool { false }
    fn is_file(&self) -> bool { true }
    fn size(&self) -> u64 { self.data.lock().len() as u64 }
}

/// File handle for reading/writing
struct RamFileHandle {
    data: Arc<Mutex<Vec<u8>>>,
    pos: usize,
}

/// File handle for reading directory entries.
/// Returns entries as null-terminated strings packed sequentially.
struct RamDirHandle {
    entries: Vec<String>,
    pos: usize,
}

impl File for RamFileHandle {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VfsError> {
        let data = self.data.lock();
        let available = data.len().saturating_sub(self.pos);
        if available == 0 {
            return Ok(0);
        }
        let to_read = core::cmp::min(buf.len(), available);
        buf[..to_read].copy_from_slice(&data[self.pos..self.pos + to_read]);
        self.pos += to_read;
        Ok(to_read)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, VfsError> {
        let mut data = self.data.lock();
        let end = self.pos + buf.len();
        if end > data.len() {
            data.resize(end, 0);
        }
        data[self.pos..end].copy_from_slice(buf);
        self.pos += buf.len();
        Ok(buf.len())
    }

    fn seek(&mut self, offset: u64) -> Result<(), VfsError> {
        self.pos = offset as usize;
        Ok(())
    }

    fn close(&mut self) {}
}

impl File for RamDirHandle {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VfsError> {
        // Build packed entry data: "name1\0name2\0name3\0"
        let mut packed = Vec::new();
        for name in &self.entries[self.pos..] {
            packed.extend_from_slice(name.as_bytes());
            packed.push(0); // null terminator
        }

        if packed.is_empty() {
            return Ok(0);
        }

        let to_read = core::cmp::min(buf.len(), packed.len());
        buf[..to_read].copy_from_slice(&packed[..to_read]);

        // Advance pos by the number of entries fully consumed
        let mut bytes_consumed = 0usize;
        for i in self.pos..self.entries.len() {
            let entry_len = self.entries[i].len() + 1; // +1 for null terminator
            if bytes_consumed + entry_len > to_read {
                break;
            }
            bytes_consumed += entry_len;
            self.pos += 1;
        }

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

/// Internal directory structure
struct RamDir {
    entries: BTreeMap<String, RamNode>,
}

enum RamNode {
    File(Arc<Mutex<Vec<u8>>>),
    Dir(Arc<Mutex<RamDir>>),
}

impl RamDir {
    fn new() -> Self {
        RamDir {
            entries: BTreeMap::new(),
        }
    }

    /// List all entry names in this directory (sorted by BTreeMap key order).
    pub fn list_entries(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
}
