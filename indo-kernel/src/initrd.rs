//! # Initrd Parser
//!
//! Parses a cpio newc (070701) archive and populates the VFS ramfs.
//!
//! The archive is embedded at compile time via `include_bytes!()` and parsed
//! once at boot. Each entry is validated for security before being added to
//! the VFS.

/// Parse a hex ASCII field (8 bytes) from the cpio header.
fn parse_hex8(header: &[u8], offset: usize) -> Result<u64, ()> {
    if offset + 8 > header.len() {
        return Err(());
    }
    let field = &header[offset..offset + 8];
    // Validate all bytes are hex digits
    for &b in field {
        match b {
            b'0'..=b'9' | b'a'..=b'f' => {}
            _ => return Err(()),
        }
    }
    let s = core::str::from_utf8(field).map_err(|_| ())?;
    u64::from_str_radix(s, 16).map_err(|_| ())
}

/// Parse the cpio newc header at the given offset.
///
/// Returns (namesize, filesize, data_offset) on success.
fn parse_header(data: &[u8], offset: usize) -> Result<(usize, usize, usize), ()> {
    if offset + 110 > data.len() {
        return Err(());
    }

    let magic = &data[offset..offset + 6];
    if magic != b"070701" && magic != b"070702" {
        return Err(());
    }

    let namesize = parse_hex8(data, offset + 94)? as usize;
    let filesize = parse_hex8(data, offset + 54)? as usize;

    if namesize == 0 || namesize > 4096 {
        return Err(());
    }

    // cpio newc alignment: name and data are padded to 4-byte boundaries
    // Builder uses (n + 3) & ~3; we must match exactly.
    let namesize_aligned = (namesize + 3) & !3;
    let filesize_aligned = (filesize + 3) & !3;

    let name_start = offset + 110;
    let data_start = name_start + namesize_aligned;
    let next_header = data_start + filesize_aligned;

    // Validate bounds
    if name_start + namesize > data.len() {
        return Err(());
    }
    if data_start + filesize > data.len() {
        return Err(());
    }

    // Validate NUL terminator
    if data[name_start + namesize - 1] != 0 {
        return Err(());
    }

    Ok((namesize, filesize, next_header))
}

/// Parse the initrd archive and populate the VFS ramfs.
///
/// # Security
///
/// - Validates magic ("070701" or "070702")
/// - Validates namesize > 0 and ≤ 4096
/// - Validates NUL terminator in filename
/// - Validates filesize doesn't overflow
/// - Never reads past archive bounds
/// - Rejects path traversal (".." components)
/// - Handles TRAILER!!! end marker
pub fn load_initrd(initrd_data: &[u8]) {
    crate::serial::write_str("[INITRD] Loading initrd: ");
    crate::serial::write_hex(initrd_data.len() as u64);
    crate::serial::write_str(" bytes\n");

    let mut offset = 0;
    let mut file_count = 0u64;
    let mut dir_count = 0u64;

    loop {
        if offset + 110 > initrd_data.len() {
            crate::serial::write_str("[INITRD] ERROR: truncated header at offset ");
            crate::serial::write_hex(offset as u64);
            crate::serial::write_nl();
            break;
        }

        // Check for zero padding between archives (skip it)
        if initrd_data[offset..offset + 6] == [0; 6] {
            offset += 1;
            continue;
        }

        let (namesize, filesize, next_offset) = match parse_header(initrd_data, offset) {
            Ok(v) => v,
            Err(()) => {
                crate::serial::write_str("[INITRD] ERROR: bad header at offset ");
                crate::serial::write_hex(offset as u64);
                crate::serial::write_nl();
                break;
            }
        };

        let name_start = offset + 110;
        let name_bytes = &initrd_data[name_start..name_start + namesize - 1]; // exclude NUL
        let name = match core::str::from_utf8(name_bytes) {
            Ok(s) => s,
            Err(_) => {
                crate::serial::write_str("[INITRD] ERROR: non-UTF8 name at offset ");
                crate::serial::write_hex(offset as u64);
                crate::serial::write_nl();
                offset = next_offset;
                continue;
            }
        };

        // Check for TRAILER!!!
        if name == "TRAILER!!!" {
            break;
        }

        // Security: reject path traversal
        if name.contains("..") {
            crate::serial::write_str("[INITRD] WARN: skipping path with ..: ");
            crate::serial::write_str(name);
            crate::serial::write_nl();
            offset = next_offset;
            continue;
        }

        let data_start = name_start + ((namesize + 3) & !3);
        let file_data = &initrd_data[data_start..data_start + filesize];

        let is_dir = name.ends_with('/');

        // Add to VFS
        if is_dir {
            let dir_name = &name[..name.len() - 1]; // strip trailing /
            if !dir_name.is_empty() {
                crate::serial::write_str("[INITRD] Creating dir: '");
                crate::serial::write_str(dir_name);
                crate::serial::write_str("'\n");
                match crate::vfs::vfs().create_dir(dir_name) {
                    Ok(_) => {
                        crate::serial::write_str("[INITRD]   -> OK\n");
                    }
                    Err(e) => {
                        crate::serial::write_str("[INITRD]   -> Err ");
                        crate::serial::write_hex(e.to_errno() as u64);
                        crate::serial::write_nl();
                    }
                }
                dir_count += 1;
            }
        } else {
            crate::serial::write_str("[INITRD] Creating file: '");
            crate::serial::write_str(name);
            crate::serial::write_str("'\n");
            match crate::vfs::vfs().write_file(name, file_data) {
                Ok(_) => {
                    crate::serial::write_str("[INITRD]   -> OK\n");
                    file_count += 1;
                }
                Err(e) => {
                    crate::serial::write_str("[INITRD] WARN: failed to create '");
                    crate::serial::write_str(name);
                    crate::serial::write_str("': error ");
                    crate::serial::write_hex(e.to_errno() as u64);
                    crate::serial::write_nl();
                }
            }
        }

        offset = next_offset;

        // Safety: prevent infinite loop
        if offset == 0 || offset >= initrd_data.len() {
            break;
        }
    }

    crate::serial::write_str("[INITRD] Loaded ");
    crate::serial::write_hex(file_count);
    crate::serial::write_str(" files, ");
    crate::serial::write_hex(dir_count);
    crate::serial::write_str(" dirs\n");
}
