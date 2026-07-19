#!/usr/bin/env python3
"""Generate a minimal ELF64 that just loops forever (no syscalls).
Tests if Ring 3 execution works at all."""
import struct, sys

# Simple infinite loop: jmp .  (EB FE)
code = bytes([0xEB, 0xFE])

HEADER_SIZE = 64 + 56  # ELF header + 1 phdr

elf_header = struct.pack('<16sHHIQQQIHHHHHH',
    b'\x7fELF' + bytes([2, 1, 1]) + bytes(9),
    2,            # ET_EXEC
    0x3E,         # EM_X86_64
    1,            # EV_CURRENT
    0x400000 + HEADER_SIZE,  # e_entry
    64,           # e_phoff
    0,            # e_shoff
    0,            # e_flags
    64,           # e_ehsize
    56,           # e_phentsize
    1,            # e_phnum
    0, 0, 0,
)

filesz = HEADER_SIZE + len(code)
phdr = struct.pack('<IIQQQQQQ',
    1,        # PT_LOAD
    5,        # PF_R | PF_X
    0,        # p_offset
    0x400000, # p_vaddr
    0x400000, # p_paddr
    filesz,   # p_filesz
    filesz,   # p_memsz
    0x1000,   # p_align
)

output = elf_header + phdr + code
output += b'\x00' * (0x1000 - len(output))

with open(sys.argv[1], 'wb') as f:
    f.write(output)
print(f"Written {len(output)} bytes, entry=0x{0x400000 + HEADER_SIZE:016x}", file=sys.stderr)
