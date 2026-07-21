#!/usr/bin/env python3
"""Generate comprehensive ELF64 test binaries for INDOMINUS OS.

Each test is a standalone ELF64 binary that exercises specific scenarios.
Displacements are computed dynamically to avoid hardcoded offset bugs.

ABI: RAX=syscall_num, RDI=arg0, RSI=arg1, RDX=arg2
Syscalls: 0=write, 1=exit, 2=yield, 3=getpid
"""
import struct, sys, os, io

HEADER_SIZE = 64 + 56  # ELF header + 1 program header
CODE_BASE_VA = 0x400000


def lea_rsi_rip_rel(disp32):
    """Encode LEA RSI, [RIP+disp32]."""
    return bytes([0x48, 0x8D, 0x35]) + struct.pack('<i', disp32)


def mov_eax_imm32(val):
    return bytes([0xB8]) + struct.pack('<I', val)


def mov_edi_imm32(val):
    return bytes([0xBF]) + struct.pack('<I', val)


def mov_edx_imm32(val):
    return bytes([0xBA]) + struct.pack('<I', val)


def mov_esi_imm32(val):
    """MOV ESI, imm32 (for passing small integer args)."""
    return bytes([0xBE]) + struct.pack('<I', val)


def mov_rsi_imm64(val):
    """MOV RSI, imm64 (for absolute addresses if needed)."""
    return bytes([0x48, 0xBE]) + struct.pack('<Q', val)


def xor_esi_esi():
    """XOR ESI, ESI — sets RSI to 0."""
    return bytes([0x31, 0xF6])


def syscall_inst():
    return bytes([0x0F, 0x05])


def mov_byte_ptr_null_imm8(val):
    """MOV BYTE PTR [0], imm8 — NULL dereference."""
    return bytes([0xC6, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00, val])


class CodeBuilder:
    """Builds code with deferred string data and auto-computed LEA displacements."""

    def __init__(self):
        self.code = bytearray()     # instruction bytes (LEA disp=0 placeholder)
        self.lea_patches = []       # (offset_in_code, string_index)
        self.strings = []           # (string_bytes, appended after code)

    def emit(self, instruction_bytes):
        self.code += instruction_bytes

    def emit_lea_rsi_for_string(self, string_bytes):
        """Emit LEA RSI, [RIP+0] placeholder. Target string must be added via add_string()."""
        idx = len(self.strings)
        self.strings.append(string_bytes)
        self.lea_patches.append((len(self.code), idx))
        self.code += lea_rsi_rip_rel(0)  # placeholder displacement

    def add_string(self, string_bytes):
        """Register a string and return its index (for later emit_lea_rsi_for_string)."""
        idx = len(self.strings)
        self.strings.append(string_bytes)
        return idx

    def emit_lea_rsi_for_string_idx(self, idx):
        """Emit LEA with a pre-registered string index."""
        self.lea_patches.append((len(self.code), idx))
        self.code += lea_rsi_rip_rel(0)

    def build(self):
        """Return final (code_bytes, strings_bytes) with LEA displacements patched."""
        code = bytearray(self.code)
        # After all code, the strings are appended.
        # For each LEA at code offset `off`, targeting string at index `idx`:
        #   next_instr_va = CODE_BASE_VA + HEADER_SIZE + off + 7
        #   string_va    = CODE_BASE_VA + HEADER_SIZE + len(code) + string_offset
        #   disp32       = string_va - next_instr_va
        #
        # Compute cumulative string offsets
        string_offsets = []
        off = 0
        for s in self.strings:
            string_offsets.append(off)
            off += len(s)

        for lea_off, str_idx in self.lea_patches:
            next_instr_va = CODE_BASE_VA + HEADER_SIZE + lea_off + 7
            string_va = CODE_BASE_VA + HEADER_SIZE + len(code) + string_offsets[str_idx]
            disp = string_va - next_instr_va
            struct.pack_into('<i', code, lea_off + 3, disp)

        strings_data = bytearray()
        for s in self.strings:
            strings_data += s

        return bytes(code), bytes(strings_data)


def build_elf(code_bytes, strings_bytes, flags=7):
    """Build ELF64 with given code+data as a single PT_LOAD segment.

    flags: 5=RX, 6=RW, 7=RWX
    """
    total = code_bytes + strings_bytes
    e_entry = CODE_BASE_VA + HEADER_SIZE
    filesz = HEADER_SIZE + len(total)

    elf_header = struct.pack('<16sHHIQQQIHHHHHH',
        b'\x7fELF' + bytes([2, 1, 1]) + bytes(9),
        2, 0x3E, 1, e_entry, 64, 0, 0, 64, 56, 1, 0, 0, 0,
    )

    phdr = struct.pack('<IIQQQQQQ',
        1, flags, 0, CODE_BASE_VA, CODE_BASE_VA, filesz, filesz, 0x1000,
    )

    output = elf_header + phdr + total
    # Pad to page size
    if len(output) < 0x1000:
        output += b'\x00' * (0x1000 - len(output))
    return output


def build_test1():
    """Normal user process: write, yield, write again, exit."""
    cb = CodeBuilder()
    msg1 = b"TEST1_NORMAL_OK\n"
    msg2 = b"TEST1_RESUMED_OK\n"

    # sys_write(1, msg, len)
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg1)
    cb.emit(mov_edx_imm32(len(msg1)))
    cb.emit(syscall_inst())

    # sys_yield()
    cb.emit(mov_eax_imm32(2))
    cb.emit(syscall_inst())

    # sys_write(1, msg2, len2)
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg2)
    cb.emit(mov_edx_imm32(len(msg2)))
    cb.emit(syscall_inst())

    # sys_exit(0)
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(0))
    cb.emit(syscall_inst())

    code, strings = cb.build()
    return build_elf(code, strings)


def build_test2():
    """Second process: write PID, exit."""
    cb = CodeBuilder()
    msg = b"TEST2_MULTI_PID_OK\n"

    # sys_getpid()
    cb.emit(mov_eax_imm32(3))
    cb.emit(syscall_inst())

    # sys_write(1, msg, len)
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg)
    cb.emit(mov_edx_imm32(len(msg)))
    cb.emit(syscall_inst())

    # sys_exit(0)
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(0))
    cb.emit(syscall_inst())

    code, strings = cb.build()
    return build_elf(code, strings)


def build_test3():
    """Null dereference: write, then fault."""
    cb = CodeBuilder()
    msg = b"TEST3_NULL_DEREF_BEFORE\n"

    # sys_write(1, msg, len)
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg)
    cb.emit(mov_edx_imm32(len(msg)))
    cb.emit(syscall_inst())

    # MOV BYTE PTR [0], 0 — NULL dereference
    cb.emit(mov_byte_ptr_null_imm8(0))

    # Should never reach here
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(0x63))
    cb.emit(syscall_inst())

    code, strings = cb.build()
    return build_elf(code, strings)


def build_test4():
    """Invalid pointer (non-canonical) via sys_write."""
    cb = CodeBuilder()
    msg1 = b"TEST4_INVALID_PTR_BEFORE\n"
    msg2 = b"TEST4_INVALID_PTR_RESULT_OK\n"

    # sys_write(1, msg, len) — valid first
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg1)
    cb.emit(mov_edx_imm32(len(msg1)))
    cb.emit(syscall_inst())

    # sys_write(1, 0x8000000000000000, 1) — non-canonical
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit(mov_rsi_imm64(0x8000000000000000))
    cb.emit(mov_edx_imm32(1))
    cb.emit(syscall_inst())

    # sys_write(1, msg2, len2) — prove we survived
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg2)
    cb.emit(mov_edx_imm32(len(msg2)))
    cb.emit(syscall_inst())

    # sys_exit(0)
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(0))
    cb.emit(syscall_inst())

    code, strings = cb.build()
    return build_elf(code, strings)


def build_test5():
    """Unmapped pointer via sys_write."""
    cb = CodeBuilder()
    msg1 = b"TEST5_UNMAPPED_BEFORE\n"
    msg2 = b"TEST5_UNMAPPED_RESULT_OK\n"

    # sys_write(1, msg, len) — valid first
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg1)
    cb.emit(mov_edx_imm32(len(msg1)))
    cb.emit(syscall_inst())

    # sys_write(1, 0x1000, 1) — unmapped user address
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit(mov_esi_imm32(0x1000))
    cb.emit(mov_edx_imm32(1))
    cb.emit(syscall_inst())

    # sys_write(1, msg2, len2) — prove we survived
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg2)
    cb.emit(mov_edx_imm32(len(msg2)))
    cb.emit(syscall_inst())

    # sys_exit(0)
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(0))
    cb.emit(syscall_inst())

    code, strings = cb.build()
    return build_elf(code, strings)


def build_test6():
    """Null pointer write via sys_write."""
    cb = CodeBuilder()
    msg1 = b"TEST6_NULL_PTR_BEFORE\n"
    msg2 = b"TEST6_NULL_PTR_RESULT_OK\n"

    # sys_write(1, msg, len) — valid first
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg1)
    cb.emit(mov_edx_imm32(len(msg1)))
    cb.emit(syscall_inst())

    # sys_write(1, 0, 1) — null pointer
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit(xor_esi_esi())
    cb.emit(mov_edx_imm32(1))
    cb.emit(syscall_inst())

    # sys_write(1, msg2, len2) — prove we survived
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg2)
    cb.emit(mov_edx_imm32(len(msg2)))
    cb.emit(syscall_inst())

    # sys_exit(0)
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(0))
    cb.emit(syscall_inst())

    code, strings = cb.build()
    return build_elf(code, strings)


def build_test7():
    """Invalid syscall number."""
    cb = CodeBuilder()
    msg1 = b"TEST7_INVALID_SYSCALL_BEFORE\n"
    msg2 = b"TEST7_INVALID_SYSCALL_RESULT_OK\n"

    # sys_write(1, msg, len) — valid first
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg1)
    cb.emit(mov_edx_imm32(len(msg1)))
    cb.emit(syscall_inst())

    # syscall 99 (invalid)
    cb.emit(mov_eax_imm32(99))
    cb.emit(syscall_inst())

    # syscall 255 (invalid)
    cb.emit(mov_eax_imm32(255))
    cb.emit(syscall_inst())

    # sys_write(1, msg2, len2) — prove we survived
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg2)
    cb.emit(mov_edx_imm32(len(msg2)))
    cb.emit(syscall_inst())

    # sys_exit(0)
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(0))
    cb.emit(syscall_inst())

    code, strings = cb.build()
    return build_elf(code, strings)


# ── Main ────────────────────────────────────────────────────────────────────

outdir = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'indo-kernel')

tests = [
    ("test1_normal.bin", build_test1),
    ("test2_multi.bin", build_test2),
    ("test3_null_deref.bin", build_test3),
    ("test4_invalid_ptr.bin", build_test4),
    ("test5_unmapped.bin", build_test5),
    ("test6_null_ptr.bin", build_test6),
    ("test7_bad_syscall.bin", build_test7),
]

for name, builder in tests:
    data = builder()
    path = os.path.join(outdir, name)
    with open(path, 'wb') as f:
        f.write(data)
    print(f"  {name}: {len(data)} bytes", file=sys.stderr)

print(f"\nGenerated {len(tests)} test binaries in {outdir}", file=sys.stderr)
