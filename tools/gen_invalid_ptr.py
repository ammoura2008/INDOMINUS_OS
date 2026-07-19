#!/usr/bin/env python3
"""Generate a minimal ELF64 that tests invalid pointer in sys_write.

Placeholder test — currently sys_write with invalid pointer will just
return u64::MAX (no page fault recovery until Phase 5.4).

After Phase 5.4: the kernel page fault handler should kill the user
process and print diagnostics.

Tests:
  - sys_write(1, 0xDEAD, 1) — invalid buffer pointer
  - sys_exit(0) — clean exit if the process survives
"""
import struct, sys

MSG = b"INVALID_PTR_TEST\n"
MSG_LEN = len(MSG)

code = bytearray()

# sys_write(1, msg, msg_len) — valid write first (sanity check)
code += bytes([0xB8, 0x00, 0x00, 0x00, 0x00])           # mov eax, 0
code += bytes([0xBF, 0x01, 0x00, 0x00, 0x00])           # mov edi, 1
code += bytes([0x48, 0x8D, 0x35, 0x0D, 0x00, 0x00, 0x00])  # lea rsi, [rip+0x0D]
code += bytes([0xBA, MSG_LEN, 0x00, 0x00, 0x00])        # mov edx, msg_len
code += bytes([0x0F, 0x05])                               # syscall

# sys_write(1, 0xDEAD, 1) — invalid buffer pointer
code += bytes([0xB8, 0x00, 0x00, 0x00, 0x00])           # mov eax, 0 (sys_write)
code += bytes([0xBF, 0x01, 0x00, 0x00, 0x00])           # mov edi, 1
code += bytes([0x48, 0xC7, 0xC6, 0xAD, 0xDE, 0x00, 0x00])  # mov esi, 0xDEAD
code += bytes([0xBA, 0x01, 0x00, 0x00, 0x00])           # mov edx, 1
code += bytes([0x0F, 0x05])                               # syscall

# sys_exit(0) — clean exit (should not reach here after Phase 5.4)
code += bytes([0xB8, 0x01, 0x00, 0x00, 0x00])           # mov eax, 1
code += bytes([0xBF, 0x00, 0x00, 0x00, 0x00])           # mov edi, 0
code += bytes([0x0F, 0x05])                               # syscall

code += MSG

HEADER_SIZE = 64 + 56
e_entry = 0x400000 + HEADER_SIZE

elf_header = struct.pack('<16sHHIQQQIHHHHHH',
    b'\x7fELF' + bytes([2, 1, 1]) + bytes(9),
    2, 0x3E, 1, e_entry, 64, 0, 0, 64, 56, 1, 0, 0, 0,
)

filesz = HEADER_SIZE + len(code)
phdr = struct.pack('<IIQQQQQQ',
    1, 5, 0, 0x400000, 0x400000, filesz, filesz, 0x1000,
)

output = elf_header + phdr + bytes(code)
output += b'\x00' * (0x1000 - len(output))

with open(sys.argv[1] if len(sys.argv) > 1 else 'indo-kernel/invalid_ptr_test.bin', 'wb') as f:
    f.write(output)

print(f"Written {len(output)} bytes, entry=0x{e_entry:016x}", file=sys.stderr)
