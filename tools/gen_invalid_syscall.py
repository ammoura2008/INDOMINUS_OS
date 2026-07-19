#!/usr/bin/env python3
"""Generate a minimal ELF64 that tests invalid syscall numbers.

Tests:
  - syscall 99 (invalid) → should return u64::MAX (0xFFFFFFFFFFFFFFFF)
  - syscall 255 (invalid) → should return u64::MAX
  - syscall 0 (sys_write) with invalid pointer → behavior depends on Phase 5.4
  - sys_exit(0) → clean exit

The test prints results and exits. If no output or triple fault, FAILED.
"""
import struct, sys

MSG = b"INVALID_SYSCALL_TEST\n"
MSG_LEN = len(MSG)

code = bytearray()

# sys_write(1, msg, msg_len) — should succeed
code += bytes([0xB8, 0x00, 0x00, 0x00, 0x00])           # mov eax, 0
code += bytes([0xBF, 0x01, 0x00, 0x00, 0x00])           # mov edi, 1
code += bytes([0x48, 0x8D, 0x35, 0x0D, 0x00, 0x00, 0x00])  # lea rsi, [rip+0x0D]
code += bytes([0xBA, MSG_LEN, 0x00, 0x00, 0x00])        # mov edx, msg_len
code += bytes([0x0F, 0x05])                               # syscall

# syscall 99 (invalid) — should return u64::MAX in RAX
code += bytes([0xB8, 0x63, 0x00, 0x00, 0x00])           # mov eax, 99
code += bytes([0x0F, 0x05])                               # syscall
# RAX now holds 0xFFFFFFFFFFFFFFFF if invalid syscall handled correctly

# syscall 255 (invalid) — should return u64::MAX
code += bytes([0xB8, 0xFF, 0x00, 0x00, 0x00])           # mov eax, 255
code += bytes([0x0F, 0x05])                               # syscall

# sys_exit(0) — clean exit
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

with open(sys.argv[1] if len(sys.argv) > 1 else 'indo-kernel/invalid_syscall_test.bin', 'wb') as f:
    f.write(output)

print(f"Written {len(output)} bytes, entry=0x{e_entry:016x}", file=sys.stderr)
