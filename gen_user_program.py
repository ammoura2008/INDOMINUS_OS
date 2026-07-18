#!/usr/bin/env python3
"""Generate user_program.rs with ELF bytes as a const array."""

import os

# Read the ELF binary
with open('indo-kernel/user_test.bin', 'rb') as f:
    data = f.read()

# Generate Rust source
lines = ['pub static USER_PROGRAM: &[u8] = &[']
for i, byte in enumerate(data):
    if i % 16 == 0:
        lines.append('    ')
    lines.append(f'0x{byte:02X},')
    if i % 16 == 15:
        lines.append('\n')
lines.append('];\n')

with open('indo-kernel/src/user_program.rs', 'w') as f:
    f.write('\n'.join(lines))

print(f'Generated user_program.rs with {len(data)} bytes')
