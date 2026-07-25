#!/usr/bin/env python3
"""Create a cpio newc (070701) archive from a directory tree.

Usage:
    python tools/make_initrd.py <rootfs_dir> <output_img>
"""

import os
import sys


def cpio_newc_entry(name: str, data: bytes, is_dir: bool = False) -> bytes:
    """Create a single cpio newc (070701) entry.

    Header layout (110 bytes, all ASCII hex zero-padded to 8 chars):
      0:6     magic       "070701"
      6:14    dev
      14:22   ino
      22:30   mode        (040755=dir, 0100644=reg)
      30:38   uid
      38:46   gid
      46:54   nlink
      54:62   filesize    <-- kernel reads this
      62:70   mtime
      70:78   chksum      (0 in 070701)
      78:86   type        (0=reg, 4=dir)
      86:94   devmajor
      94:102  namesize    <-- kernel reads this (includes NUL terminator)
      102:110 rdevmajor+minor (combined, usually 0)
    """
    if is_dir:
        name_str = name.rstrip("/") + "/"
        filesize = 0
        data = b""
        mode_str = "000041fd"
        type_val = "00000004"
    else:
        name_str = name
        filesize = len(data)
        mode_str = "000081a4"
        type_val = "00000000"

    namesize = len(name_str) + 1  # include NUL terminator

    def f(val):
        return f"{val:08x}".encode()

    header = bytearray(110)
    header[0:6]    = b"070701"
    header[6:14]   = f(0)             # dev
    header[14:22]  = f(0)             # ino
    header[22:30]  = mode_str.encode()# mode
    header[30:38]  = f(0)             # uid
    header[38:46]  = f(0)             # gid
    header[46:54]  = f(1)             # nlink
    header[54:62]  = f(filesize)      # filesize
    header[62:70]  = f(0)             # mtime
    header[70:78]  = f(0)             # chksum
    header[78:86]  = type_val.encode()# type
    header[86:94]  = f(0)             # devmajor
    header[94:102] = f(namesize)      # namesize (kernel reads this!)
    header[102:110]= f(0)             # rdevmajor+minor

    result = bytes(header) + name_str.encode("ascii") + b"\x00"

    # Pad name to 4-byte boundary — kernel uses (namesize+3)&~3
    namesize_aligned = (namesize + 3) & ~3
    result += b"\x00" * (namesize_aligned - namesize)

    result += data

    # Pad data to 4-byte boundary
    filesize_aligned = (filesize + 3) & ~3
    result += b"\x00" * (filesize_aligned - filesize)

    return result


def make_trailer() -> bytes:
    return cpio_newc_entry("TRAILER!!!", b"")


def build_initrd(rootfs_dir: str) -> bytes:
    """Build a cpio newc archive from a rootfs directory."""
    archive = bytearray()

    entries = []

    for dirpath, dirnames, filenames in os.walk(rootfs_dir):
        rel = os.path.relpath(dirpath, rootfs_dir)
        if rel == ".":
            rel = ""

        if rel:
            entries.append((rel.replace("\\", "/") + "/", b"", True))

        for name in sorted(filenames):
            filepath = os.path.join(dirpath, name)
            file_rel = os.path.join(rel, name) if rel else name
            # Use forward slashes for cpio (Linux kernel expects /)
            file_rel = file_rel.replace("\\", "/")
            with open(filepath, "rb") as f:
                data = f.read()
            entries.append((file_rel, data, False))

        dirnames.sort()

    entries.insert(0, ("", b"", True))

    for name, data, is_dir in entries:
        archive.extend(cpio_newc_entry(name, data, is_dir))

    archive.extend(make_trailer())

    return bytes(archive)


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <rootfs_dir> <output_img>")
        sys.exit(1)

    rootfs_dir = sys.argv[1]
    output_img = sys.argv[2]

    if not os.path.isdir(rootfs_dir):
        print(f"Error: {rootfs_dir} is not a directory")
        sys.exit(1)

    archive = build_initrd(rootfs_dir)

    os.makedirs(os.path.dirname(output_img) or ".", exist_ok=True)

    with open(output_img, "wb") as f:
        f.write(archive)

    print(f"[make_initrd] Created {output_img}: {len(archive)} bytes")

    # Verify by parsing back (same logic as kernel initrd.rs)
    offset = 0
    count = 0
    while offset + 110 <= len(archive):
        magic = archive[offset:offset + 6]
        if magic == b"000000":
            offset += 1
            continue
        if magic != b"070701":
            break

        filesize = int(archive[offset + 54:offset + 62], 16)
        namesize = int(archive[offset + 94:offset + 102], 16)

        if namesize == 0 or namesize > 4096:
            print(f"[make_initrd] WARN: bad namesize {namesize} at offset {offset}")
            break

        name_start = offset + 110
        name_end = name_start + namesize - 1
        name = archive[name_start:name_end].decode("ascii")

        if name == "TRAILER!!!":
            break

        namesize_aligned = (namesize + 3) & ~3
        filesize_aligned = (filesize + 3) & ~3
        data_start = name_start + namesize_aligned

        kind = "dir" if name.endswith("/") else f"{filesize}B"
        print(f"  {name} ({kind})")

        offset = data_start + filesize_aligned
        count += 1

    print(f"[make_initrd] {count} entries verified")


if __name__ == "__main__":
    main()
