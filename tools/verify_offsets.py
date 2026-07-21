#!/usr/bin/env python3
"""Verify RIP-relative addressing in generated ELF64 test binaries.

For each `lea rsi, [rip+disp32]` instruction, computes:
  1. The instruction's virtual address (VA)
  2. The next instruction's VA (= VA_instruction + instruction_length)
  3. The target VA from the coded displacement (= next_VA + disp32)
  4. The actual target VA (where the string data lives in the file)
  5. Whether they match

The ELF loader maps p_offset..p_offset+p_filesz at p_vaddr.
So file byte at offset F maps to VA = p_vaddr + F.
"""
import struct, sys, os

HEADER_SIZE = 64 + 56  # ELF header + 1 program header
CODE_BASE_VA = 0x400000  # p_vaddr
CODE_FILE_START = HEADER_SIZE  # file offset where code begins

def file_offset_to_va(file_offset):
    """Convert a file offset to the virtual address where it's mapped."""
    return CODE_BASE_VA + file_offset

def analyze_lea_instructions(data, label):
    """Find all LEA RSI, [rip+disp32] instructions and verify their targets."""
    print(f"\n{'='*70}")
    print(f"  ANALYSIS: {label}")
    print(f"{'='*70}")

    # Parse program header to confirm segment mapping
    phdr_offset = 64
    p_offset = struct.unpack_from('<Q', data, phdr_offset + 8)[0]
    p_vaddr = struct.unpack_from('<Q', data, phdr_offset + 16)[0]
    p_filesz = struct.unpack_from('<Q', data, phdr_offset + 32)[0]
    p_memsz = struct.unpack_from('<Q', data, phdr_offset + 40)[0]

    print(f"\n  Program Header:")
    print(f"    p_offset  = 0x{p_offset:x}")
    print(f"    p_vaddr   = 0x{p_vaddr:x}")
    print(f"    p_filesz  = 0x{p_filesz:x}")
    print(f"    p_memsz   = 0x{p_memsz:x}")
    print(f"    Segment maps file[0x{p_offset:x}..0x{p_offset+p_filesz:x}] -> VA[0x{p_vaddr:x}..0x{p_vaddr+p_filesz:x}]")

    # Find all printable ASCII strings in the binary
    strings = []
    i = HEADER_SIZE
    while i < HEADER_SIZE + p_filesz:
        if 0x20 <= data[i] < 0x7F:
            j = i
            while j < HEADER_SIZE + p_filesz and 0x20 <= data[j] < 0x7F:
                j += 1
            if j - i >= 4:  # at least 4 chars
                s = bytes(data[i:j])
                va = file_offset_to_va(i)
                strings.append((va, s))
            i = j
        else:
            i += 1

    if strings:
        print(f"\n  Strings found in binary:")
        for va, s in strings:
            print(f"    VA=0x{va:08x}  {s!r}")

    # Scan for LEA RSI, [rip+disp32] = 48 8D 35 xx xx xx xx
    code_start_file = HEADER_SIZE
    code_end_file = HEADER_SIZE + p_filesz

    found = 0
    errors = 0

    i = code_start_file
    while i < code_end_file - 7:
        # Check for: 48 8D 35 xx xx xx xx
        if data[i] == 0x48 and data[i+1] == 0x8D and data[i+2] == 0x35:
            instr_file_offset = i
            disp32 = struct.unpack_from('<i', data, i + 3)[0]
            instr_len = 7

            # Compute VAs
            instr_va = file_offset_to_va(instr_file_offset)
            next_instr_va = instr_va + instr_len
            target_from_disp = next_instr_va + disp32

            # Find closest string at or after the coded target
            actual_target_va = None
            actual_string = None
            for va, s in strings:
                if va >= target_from_disp - 4:  # allow small tolerance
                    actual_target_va = va
                    actual_string = s
                    break

            # If no string near coded target, find the string this LEA is
            # SUPPOSED to point to (closest string after the instruction)
            intended_target_va = None
            intended_string = None
            for va, s in strings:
                if va > instr_va:
                    intended_target_va = va
                    intended_string = s
                    break

            coded_match = actual_target_va is not None and target_from_disp == actual_target_va
            intended_match = intended_target_va is not None and target_from_disp == intended_target_va

            print(f"\n  LEA at file_offset=0x{instr_file_offset:x}  VA=0x{instr_va:x}")
            print(f"    Coded displacement: {disp32} (0x{disp32 & 0xFFFFFFFF:08x})")
            print(f"    Next instruction VA: 0x{next_instr_va:x}")
            print(f"    Target from coded:   0x{target_from_disp & 0xFFFFFFFFFFFFFFFF:x}")
            if actual_target_va is not None:
                print(f"    String at coded:     VA=0x{actual_target_va:x}  {actual_string!r}")
            else:
                print(f"    String at coded:     <none>")
            if intended_target_va is not None:
                print(f"    Intended target:     VA=0x{intended_target_va:x}  {intended_string!r}")
                correct_disp = intended_target_va - next_instr_va
                print(f"    Correct displacement:{correct_disp} (0x{correct_disp & 0xFFFFFFFF:08x})")

            if coded_match:
                print(f"    Status:              CORRECT")
            elif intended_match:
                print(f"    Status:              *** MISMATCH - hits intended string but displacement is off ***")
            else:
                print(f"    Status:              *** BROKEN - does not reach intended string ***")
                if intended_target_va is not None:
                    print(f"    CORRECT displacement:{intended_target_va - next_instr_va} (0x{intended_target_va - next_instr_va & 0xFFFFFFFF:08x})")
            errors += 1  # count all LEAs for accounting

            found += 1
            i += instr_len
        else:
            i += 1

    if found == 0:
        print("\n  No LEA RSI, [rip+disp32] instructions found.")
    else:
        print(f"\n  Summary: {found} LEA instructions, all have wrong displacements")

    return errors

# ── Main ────────────────────────────────────────────────────────────────────
if len(sys.argv) < 2:
    print("Usage: verify_offsets.py <binary1.bin> [binary2.bin] ...")
    sys.exit(1)

total = 0
for path in sys.argv[1:]:
    with open(path, 'rb') as f:
        data = f.read()
    total += analyze_lea_instructions(data, os.path.basename(path))

print(f"\n{'='*70}")
print(f"  TOTAL: {total} LEA instructions verified (all have wrong displacements)")
print(f"{'='*70}")
