#!/usr/bin/env python3
"""Generate a minimal ELF64 that triggers a null-pointer page fault.

The program:
  1. sys_write(1, "PAGE_FAULT_TEST\n", 16) — print before fault
  2. mov byte [0], 0                        — dereference NULL → page fault!
  3. sys_exit(99)                           — should never reach here

Expected behavior with Phase 5.4 Layer 1:
  - Page fault fires (fault=14, error_code=2)
  - CS.RPL == 3 → user fault → process killed
  - Kernel continues, other processes still run
  - The sys_exit(99) is never reached

ABI: RAX=syscall_num, RDI=arg0, RSI=arg1, RDX=arg2
"""
import struct, sys

MSG = b"PAGE_FAULT_TEST\n"
MSG_LEN = len(MSG)

code = bytearray()

# sys_write(1, msg, msg_len) — print so we see the test ran before the fault
code += bytes([0xB8, 0x00, 0x00, 0x00, 0x00])                     # mov eax, 0  (sys_write)
code += bytes([0xBF, 0x01, 0x00, 0x00, 0x00])                     # mov edi, 1  (stdout)
# lea rsi, [rip+disp32]  — RIP after this instr will be offset 0x11
# msg is at offset 0x2c, so displacement = 0x2c - 0x11 = 0x1b
code += bytes([0x48, 0x8D, 0x35, 0x1b, 0x00, 0x00, 0x00])        # lea rsi, [rip+0x1b]
code += bytes([0xBA, MSG_LEN, 0x00, 0x00, 0x00])                  # mov edx, msg_len
code += bytes([0x0F, 0x05])                                        # syscall

# mov byte [0], 0  — NULL dereference → page fault!
# C6 04 25 00 00 00 00 00
# C6 = MOV r/m8, imm8
# 04 = ModRM(mod=00, reg=000, rm=100 → SIB follows)
# 25 = SIB(scale=00, index=100=none, base=101=disp32)
# 00 00 00 00 = displacement = absolute address 0x00000000
# 00 = imm8 = 0
code += bytes([0xC6, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00, 0x00])  # mov byte [0], 0

# sys_exit(99) — should never execute if page fault handler works
code += bytes([0xB8, 0x01, 0x00, 0x00, 0x00])                     # mov eax, 1  (sys_exit)
code += bytes([0xBF, 0x63, 0x00, 0x00, 0x00])                     # mov edi, 99
code += bytes([0x0F, 0x05])                                        # syscall

# Message data
code += MSG

# Verify LEA displacement
# mov eax: 5 bytes (offset 0-4)
# mov edi: 5 bytes (offset 5-9)
# lea:     7 bytes (offset 10-16)
# mov edx: 5 bytes (offset 17-21)
# syscall: 2 bytes (offset 22-23)
# null deref: 8 bytes (offset 24-31)
# mov eax: 5 bytes (offset 32-36)
# mov edi: 5 bytes (offset 37-41)
# syscall: 2 bytes (offset 42-43)
# msg:     offset 44
LEA_OFFSET = 10
LEA_NEXT = LEA_OFFSET + 7  # RIP during execution = offset 17
MSG_OFFSET = 44
disp = MSG_OFFSET - LEA_NEXT
assert disp == 0x1b, f"LEA displacement mismatch: expected 0x1b, got 0x{disp:x}"

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

with open(sys.argv[1] if len(sys.argv) > 1 else 'indo-kernel/null_deref_test.bin', 'wb') as f:
    f.write(output)

print(f"Written {len(output)} bytes, entry=0x{e_entry:016x}", file=sys.stderr)
print(f"Null deref instruction: C6 04 25 00 00 00 00 00 (mov byte [0], 0)", file=sys.stderr)
