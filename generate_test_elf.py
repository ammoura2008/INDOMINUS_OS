#!/usr/bin/env python3
"""Generate a minimal ELF64 static binary for INDOMINUS testing."""

import struct

# ─── Code (x86-64 machine code) ───────────────────────────────────────────
# This program:
#   write(1, msg, 14)  → prints "Hello Ring 3!\n"
#   exit(0)
#
# Syscall ABI: RAX=syscall_num, RDI=arg0, RSI=arg1, RDX=arg2
# Syscall numbers: 0=write, 1=exit

msg = b"Hello Ring 3!\n"

code = bytearray()

# write(1, msg, 14)
code += bytes([0x48, 0xC7, 0xC0, 0x00, 0x00, 0x00, 0x00])  # mov rax, 0 (SYS_WRITE)
code += bytes([0x48, 0xC7, 0xC7, 0x01, 0x00, 0x00, 0x00])  # mov rdi, 1 (fd=stdout)

# lea rsi, [rip + offset_to_msg] — offset calculated after we know instruction address
msg_offset_pos = len(code) + 3  # RIP at this lea will be len(code)+7, msg at code_end
code += bytes([0x48, 0x8D, 0x35, 0x00, 0x00, 0x00, 0x00])  # lea rsi, [rip+0] (placeholder)

code += bytes([0x48, 0xC7, 0xC2, len(msg) & 0xFF, 0x00, 0x00, 0x00])  # mov rdx, msg_len
code += bytes([0x0F, 0x05])  # syscall

# exit(0)
code += bytes([0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00])  # mov rax, 1 (SYS_EXIT)
code += bytes([0x48, 0x31, 0xFF])  # xor rdi, rdi
code += bytes([0x0F, 0x05])  # syscall

# Append the message data
code += msg

# Fix up the lea rsi, [rip+X] displacement
# During execution, RIP = address of NEXT instruction = msg_offset_pos + 7
# We want [RIP + disp] = msg address = code_end - len(msg) ... no, relative to code start
# Actually the offset is from the end of the lea instruction
# lea is at offset msg_offset_pos - 3, length 7, so next instruction at msg_offset_pos + 4
# msg is at offset len(code) - len(msg)
# displacement = msg_offset - (msg_offset_pos + 7)
# msg_offset_pos + 3 = byte after the displacement field in lea
# RIP during execution = (msg_offset_pos - 3) + 7 = msg_offset_pos + 4
# msg is at offset (len(code) - len(msg)) from code start
# displacement = (len(code) - len(msg)) - (msg_offset_pos + 4)
lea_instr_start = msg_offset_pos - 3
next_instr = lea_instr_start + 7
msg_offset = len(code) - len(msg)
disp = msg_offset - next_instr
code[msg_offset_pos] = disp & 0xFF
code[msg_offset_pos + 1] = (disp >> 8) & 0xFF
code[msg_offset_pos + 2] = (disp >> 16) & 0xFF
code[msg_offset_pos + 3] = (disp >> 24) & 0xFF

print(f"Code size: {len(code)} bytes")
print(f"Msg at offset: {msg_offset}")
print(f"LEA displacement: {disp}")

# ─── ELF64 Header ──────────────────────────────────────────────────────────
# We use ET_EXEC type, load at 0x400000 (standard x86-64 code base)

LOAD_BASE = 0x00400000
PAGE_SIZE = 0x1000

# Align code to page boundary for the segment
code_offset = PAGE_SIZE  # Put segment data after the first page (ELR header goes in page 0)
# Actually for simplicity, put everything in one PT_LOAD segment
# ELF header (64 bytes) + program header (56 bytes) at offset 0
# Code at offset 0x1000 (page-aligned)

e_entry = LOAD_BASE + 0x1000  # Code starts at second page
e_phoff = 64                   # Program headers right after ELF header
e_phentsize = 56               # sizeof(Elf64_Phdr)
e_phnum = 1

# ─── Program Header (PT_LOAD) ──────────────────────────────────────────────
p_type = 1                     # PT_LOAD
p_flags = 5                    # PF_R | PF_X  (read + execute)
p_offset = 0x1000             # File offset of segment data
p_vaddr = LOAD_BASE + 0x1000  # Virtual address
p_paddr = p_vaddr
p_filesz = len(code)
p_memsz = len(code)
p_align = PAGE_SIZE

# ─── Build the ELF binary ──────────────────────────────────────────────────
elf = bytearray(PAGE_SIZE * 2)  # 2 pages: header page + code page

# ELF header (64 bytes at offset 0)
elf[0:4] = b'\x7fELF'          # e_ident[0..4] = magic
elf[4] = 2                      # e_ident[4] = ELFCLASS64
elf[5] = 1                      # e_ident[5] = ELFDATA2LSB
elf[6] = 1                      # e_ident[6] = EV_CURRENT
elf[7] = 0                      # e_ident[7] = ELFOSABI_NONE
elf[8:16] = b'\x00' * 8        # e_ident[8..16] = padding

struct.pack_into('<H', elf, 16, 2)       # e_type = ET_EXEC
struct.pack_into('<H', elf, 18, 0x3E)    # e_machine = EM_X86_64
struct.pack_into('<I', elf, 20, 1)       # e_version = 1
struct.pack_into('<Q', elf, 24, e_entry) # e_entry
struct.pack_into('<Q', elf, 32, e_phoff) # e_phoff
struct.pack_into('<Q', elf, 40, 0)       # e_shoff (no section headers)
struct.pack_into('<I', elf, 48, 0)       # e_flags
struct.pack_into('<H', elf, 52, 64)      # e_ehsize
struct.pack_into('<H', elf, 54, e_phentsize)  # e_phentsize
struct.pack_into('<H', elf, 56, e_phnum)      # e_phnum
struct.pack_into('<H', elf, 58, 0)       # e_shentsize
struct.pack_into('<H', elf, 60, 0)       # e_shnum
struct.pack_into('<H', elf, 62, 0)       # e_shstrndx

# Program header (56 bytes at offset 64)
off = e_phoff
struct.pack_into('<I', elf, off + 0, p_type)
struct.pack_into('<I', elf, off + 4, p_flags)
struct.pack_into('<Q', elf, off + 8, p_offset)
struct.pack_into('<Q', elf, off + 16, p_vaddr)
struct.pack_into('<Q', elf, off + 24, p_paddr)
struct.pack_into('<Q', elf, off + 32, p_filesz)
struct.pack_into('<Q', elf, off + 40, p_memsz)
struct.pack_into('<Q', elf, off + 48, p_align)

# Code at offset 0x1000
elf[0x1000:0x1000 + len(code)] = code

# Write output
with open('user_test.bin', 'wb') as f:
    f.write(elf)

print(f"ELF binary written: user_test.bin ({len(elf)} bytes)")
print(f"Entry point: 0x{e_entry:x}")
print(f"Segment: vaddr=0x{p_vaddr:x}, filesz={p_filesz}, memsz={p_memsz}")
