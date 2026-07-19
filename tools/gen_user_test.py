#!/usr/bin/env python3
"""Generate a minimal ELF64 user test binary for INDOMINUS OS.

The binary:
  1. Calls sys_write(1, "Hello from user!\n", 17)  -- syscall 0
  2. Calls sys_exit(42)                             -- syscall 1

ABI: RAX=syscall_num, RDI=arg0, RSI=arg1, RDX=arg2
"""
import struct
import sys

# ── x86-64 machine code ──────────────────────────────────────────────
# Base address: 0x400000
# Code starts at offset 120 (after ELF header + 1 phdr)

# "Hello from user!\n"
MSG = b"Hello from user!\n"
MSG_LEN = len(MSG)

# Layout after headers (offset 120):
code = bytearray()

# sys_write(1, msg, msg_len)
code += bytes([0xB8, 0x00, 0x00, 0x00, 0x00])       # mov eax, 0 (sys_write)
code += bytes([0xBF, 0x01, 0x00, 0x00, 0x00])       # mov edi, 1 (stdout)
code += bytes([0x48, 0x8D, 0x35, 0x13, 0x00, 0x00, 0x00])  # lea rsi, [rip+0x13]
code += bytes([0xBA, MSG_LEN, 0x00, 0x00, 0x00])    # mov edx, msg_len
code += bytes([0x0F, 0x05])                           # syscall

# sys_exit(42)
code += bytes([0xB8, 0x01, 0x00, 0x00, 0x00])       # mov eax, 1 (sys_exit)
code += bytes([0xBF, 0x2A, 0x00, 0x00, 0x00])       # mov edi, 42
code += bytes([0x0F, 0x05])                           # syscall

# Append message
code += MSG

print(f"Code size: {len(code)} bytes", file=sys.stderr)

# ── ELF64 header ─────────────────────────────────────────────────────
HEADER_SIZE = 64 + 56  # ELF header + 1 program header

e_entry = 0x00400000 + HEADER_SIZE  # 0x400078
e_phoff = 64                         # immediately after ELF header
e_phentsize = 56
e_phnum = 1

elf_header = struct.pack('<16sHHIQQQIHHHHHH',
    b'\x7fELF'           # e_ident[0..4]
    + bytes([2, 1, 1])   # e_ident[4..7]: ELFCLASS64, ELFDATA2LSB, EV_CURRENT
    + bytes(9),           # e_ident[7..16]: padding
    2,                    # e_type: ET_EXEC
    0x3E,                 # e_machine: EM_X86_64
    1,                    # e_version
    e_entry,              # e_entry
    e_phoff,              # e_phoff
    0,                    # e_shoff
    0,                    # e_flags
    64,                   # e_ehsize
    e_phentsize,          # e_phentsize
    e_phnum,              # e_phnum
    0,                    # e_shentsize
    0,                    # e_shnum
    0,                    # e_shstrndx
)

# ── Program header ───────────────────────────────────────────────────
# Single PT_LOAD segment: headers + code + data, mapped R-X at 0x400000
import math
filesz = HEADER_SIZE + len(code)
memsz = filesz  # no BSS

phdr = struct.pack('<IIQQQQQQ',
    1,          # p_type: PT_LOAD
    5,          # p_flags: PF_R | PF_X
    0,          # p_offset
    0x400000,   # p_vaddr
    0x400000,   # p_paddr
    filesz,     # p_filesz
    memsz,      # p_memsz
    0x1000,     # p_align
)

# ── Assemble ─────────────────────────────────────────────────────────
output = elf_header + phdr + bytes(code)

# Pad to page size
output += b'\x00' * (0x1000 - len(output))

with open(sys.argv[1] if len(sys.argv) > 1 else 'indo-kernel/user_test.bin', 'wb') as f:
    f.write(output)

print(f"Written {len(output)} bytes", file=sys.stderr)
print(f"Entry point: 0x{e_entry:016x}", file=sys.stderr)
print(f"Segment: 0x400000..0x{0x400000 + filesz:x}", file=sys.stderr)
