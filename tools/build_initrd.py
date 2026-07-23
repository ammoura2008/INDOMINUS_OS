#!/usr/bin/env python3
"""Build an initrd (cpio newc archive) from a directory.

Usage:
    python tools/build_initrd.py --input userspace/rootfs --output indo-kernel/initrd.img

The archive uses the cpio "newc" format (070701) which is the Linux standard
for initramfs. Each entry has a 110-byte header, filename, and data section.
"""

import argparse
import os
import struct
import sys


def align4(n):
    """Align to 4-byte boundary."""
    return (n + 3) & ~3


def hex8(val):
    """Format a value as 8-byte lowercase hex ASCII."""
    return f"{val & 0xFFFFFFFF:08x}".encode('ascii')


def build_entry(name, data, mode=0o100644, is_dir=False):
    """Build a single cpio newc entry."""
    if is_dir:
        mode = 0o040755  # directory
        data = b''
    elif isinstance(data, str):
        data = data.encode('utf-8')

    name_bytes = name.encode('utf-8') if isinstance(name, str) else name
    # Name must be NUL-terminated
    name_with_nul = name_bytes + b'\x00'
    namesize = len(name_with_nul)
    filesize = len(data)

    # cpio newc header (110 bytes)
    header = b''
    header += b'070701'           # c_magic
    header += hex8(0)             # c_ino
    header += hex8(mode)          # c_mode
    header += hex8(0)             # c_uid
    header += hex8(0)             # c_gid
    header += hex8(1)             # c_nlink
    header += hex8(0)             # c_mtime
    header += hex8(filesize)      # c_filesize
    header += hex8(0)             # c_maj (dev major)
    header += hex8(0)             # c_min (dev minor)
    header += hex8(0)             # c_rmaj (rdev major)
    header += hex8(0)             # c_rmin (rdev minor)
    header += hex8(namesize)      # c_namesize
    header += hex8(0)             # c_chksum (always 0 for newc)

    assert len(header) == 110, f"Header size {len(header)} != 110"

    # Pad name to 4-byte alignment
    name_padded_len = align4(namesize)
    name_padded = name_with_nul + b'\x00' * (name_padded_len - namesize)

    # Pad data to 4-byte alignment
    data_padded_len = align4(filesize)
    data_padded = data + b'\x00' * (data_padded_len - filesize)

    return header + name_padded + data_padded


def build_trailer():
    """Build the TRAILER!!! end marker."""
    return build_entry('TRAILER!!!', b'', mode=0)


def build_initrd(root_dir):
    """Build a complete initrd from a directory tree."""
    archive = bytearray()

    for dirpath, dirs, files in os.walk(root_dir):
        # Get relative path from root
        rel = os.path.relpath(dirpath, root_dir)
        if rel == '.':
            rel = ''

        # Add directory entry
        if rel:
            archive += build_entry(rel.replace('\\', '/') + '/', b'', is_dir=True)

        # Add file entries
        for name in sorted(files):
            filepath = os.path.join(dirpath, name)
            file_rel = os.path.join(rel, name) if rel else name
            # Normalize to forward slashes for cross-platform compatibility
            file_rel = file_rel.replace('\\', '/')
            with open(filepath, 'rb') as f:
                data = f.read()
            archive += build_entry(file_rel, data)

        # Add subdirectory entries (sorted for deterministic output)
        for name in sorted(dirs):
            dir_rel = os.path.join(rel, name) if rel else name
            # Normalize to forward slashes for cross-platform compatibility
            dir_rel = dir_rel.replace('\\', '/')
            archive += build_entry(dir_rel + '/', b'', is_dir=True)

    # Add trailer
    archive += build_trailer()

    return bytes(archive)


def main():
    parser = argparse.ArgumentParser(description="Build initrd (cpio newc archive)")
    parser.add_argument("--input", "-i", required=True,
                        help="Root directory for the initrd filesystem")
    parser.add_argument("--output", "-o", required=True,
                        help="Output initrd image path")
    args = parser.parse_args()

    if not os.path.isdir(args.input):
        print(f"Error: input directory '{args.input}' does not exist", file=sys.stderr)
        sys.exit(1)

    archive = build_initrd(args.input)

    os.makedirs(os.path.dirname(args.output) or '.', exist_ok=True)
    with open(args.output, 'wb') as f:
        f.write(archive)

    print(f"Initrd: {len(archive)} bytes ({len(archive)/1024:.1f} KB) -> {args.output}", file=sys.stderr)


if __name__ == '__main__':
    main()
