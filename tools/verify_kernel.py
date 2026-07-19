#!/usr/bin/env python3
"""verify_kernel.py — Validate a kernel ELF binary before deploying to QEMU.

Checks:
  1. File exists and is non-empty
  2. ELF magic bytes
  3. 64-bit ELF format
  4. Entry point is in kernel virtual range (0xFFFFFFFF80000000..0xFFFFFFFFC0000000)
  5. At least one PT_LOAD segment exists
  6. File size sanity (< 16 MiB)

Usage:
  python tools/verify_kernel.py <path-to-kernel-elf>

Exit codes:
  0 = all checks passed
  1 = one or more checks failed
"""

import struct
import sys
import os

KERNEL_VIRT_BASE = 0xFFFFFFFF80000000
KERNEL_VIRT_END  = 0xFFFFFFFFC0000000
MAX_FILE_SIZE    = 16 * 1024 * 1024  # 16 MiB

def verify_kernel(path: str) -> list[str]:
    errors = []

    # 1. File exists and is non-empty
    if not os.path.isfile(path):
        errors.append(f"File does not exist: {path}")
        return errors

    size = os.path.getsize(path)
    if size == 0:
        errors.append("File is empty (0 bytes)")
        return errors

    if size > MAX_FILE_SIZE:
        errors.append(f"File too large: {size} bytes (max {MAX_FILE_SIZE})")

    # Read the first 64 bytes (ELF header) + extra for program headers
    with open(path, "rb") as f:
        header = f.read(64)

    # 2. ELF magic bytes
    if header[:4] != b"\x7fELF":
        errors.append(f"Bad ELF magic: {header[:4].hex()} (expected 7f454c46)")
        return errors

    # 3. 64-bit ELF
    ei_class = header[4]
    if ei_class != 2:
        errors.append(f"Not ELF64: ei_class={ei_class} (expected 2)")
        return errors

    ei_data = header[5]
    if ei_data != 1:
        errors.append(f"Not little-endian: ei_data={ei_data} (expected 1)")

    e_machine = struct.unpack_from("<H", header, 18)[0]
    if e_machine != 0x3E:
        errors.append(f"Wrong machine: 0x{e_machine:04x} (expected 0x3E = x86_64)")

    # 4. Entry point
    e_entry = struct.unpack_from("<Q", header, 24)[0]
    if e_entry < KERNEL_VIRT_BASE or e_entry >= KERNEL_VIRT_END:
        errors.append(
            f"Entry point 0x{e_entry:016x} outside kernel range "
            f"[0x{KERNEL_VIRT_BASE:016x} .. 0x{KERNEL_VIRT_END:016x})"
        )

    # 5. Program headers — check for PT_LOAD segments
    e_phoff = struct.unpack_from("<Q", header, 32)[0]
    e_phentsize = struct.unpack_from("<H", header, 54)[0]
    e_phnum = struct.unpack_from("<H", header, 56)[0]

    if e_phnum == 0:
        errors.append("No program headers (e_phnum = 0)")
    else:
        load_count = 0
        with open(path, "rb") as f:
            for i in range(e_phnum):
                f.seek(e_phoff + i * e_phentsize)
                phdr = f.read(e_phentsize)
                if len(phdr) < 56:
                    errors.append(f"Program header {i} truncated ({len(phdr)} bytes)")
                    continue
                p_type = struct.unpack_from("<I", phdr, 0)[0]
                p_flags = struct.unpack_from("<I", phdr, 4)[0]
                p_vaddr = struct.unpack_from("<Q", phdr, 16)[0]
                p_filesz = struct.unpack_from("<Q", phdr, 32)[0]
                p_memsz = struct.unpack_from("<Q", phdr, 40)[0]
                if p_type == 1:  # PT_LOAD
                    load_count += 1
                    if p_vaddr < KERNEL_VIRT_BASE or p_vaddr >= KERNEL_VIRT_END:
                        errors.append(
                            f"PT_LOAD segment {i} vaddr 0x{p_vaddr:016x} outside kernel range"
                        )
        if load_count == 0:
            errors.append("No PT_LOAD segments found")

    return errors


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <kernel-elf>", file=sys.stderr)
        sys.exit(1)

    path = sys.argv[1]
    errors = verify_kernel(path)

    size = os.path.getsize(path) if os.path.isfile(path) else 0
    print(f"[VERIFY] {path}: {size} bytes ({size / 1024:.1f} KB)")

    if errors:
        for e in errors:
            print(f"  FAIL: {e}", file=sys.stderr)
        print(f"[VERIFY] RESULT: FAILED ({len(errors)} errors)", file=sys.stderr)
        sys.exit(1)
    else:
        print(f"[VERIFY] RESULT: ALL CHECKS PASSED")
        sys.exit(0)


if __name__ == "__main__":
    main()
