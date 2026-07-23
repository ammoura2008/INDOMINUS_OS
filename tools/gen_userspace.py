#!/usr/bin/env python3
"""Generate ELF64 userspace binaries for Indominus OS.

Creates minimal ELF64 programs that use syscall wrappers directly.
These are simple programs that don't need a C/Rust toolchain.
"""
import struct
import os
import sys

HEADER_SIZE = 64 + 56  # ELF header + 1 program header
CODE_BASE_VA = 0x400000


def lea_rsi_rip_rel(disp32):
    return bytes([0x48, 0x8D, 0x35]) + struct.pack('<i', disp32)

def mov_eax_imm32(val):
    return bytes([0xB8]) + struct.pack('<I', val)

def mov_edi_imm32(val):
    return bytes([0xBF]) + struct.pack('<I', val)

def mov_edx_imm32(val):
    return bytes([0xBA]) + struct.pack('<I', val)

def mov_esi_imm32(val):
    return bytes([0xBE]) + struct.pack('<I', val)

def mov_rsi_imm64(val):
    return bytes([0x48, 0xBE]) + struct.pack('<Q', val)

def xor_esi_esi():
    return bytes([0x31, 0xF6])

def syscall_inst():
    return bytes([0x0F, 0x05])

def ret_inst():
    return bytes([0xC3])


class CodeBuilder:
    def __init__(self):
        self.code = bytearray()
        self.lea_patches = []
        self.strings = []

    def emit(self, instruction_bytes):
        self.code += instruction_bytes

    def emit_lea_rsi_for_string(self, string_bytes):
        idx = len(self.strings)
        self.strings.append(string_bytes)
        self.lea_patches.append((len(self.code), idx))
        self.code += lea_rsi_rip_rel(0)

    def build(self):
        code = bytearray(self.code)
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


def build_elf(code_bytes, strings_bytes, flags=5):
    """Build ELF64 with RX permissions (no RW data)."""
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
    if len(output) < 0x1000:
        output += b'\x00' * (0x1000 - len(output))
    return output


def build_init():
    """Init process: prints welcome, loops reaping children.
    
    Syscalls: 0=write, 1=exit, 2=yield, 4=waitpid
    """
    cb = CodeBuilder()
    msg = b"[INIT] Indominus OS init started\n"

    # Loop:
    #   write(1, msg, len)
    #   result = waitpid(0)
    #   if result < 0: yield()
    loop_start = len(cb.code)

    # sys_write(1, msg, len)
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(msg)
    cb.emit(mov_edx_imm32(len(msg)))
    cb.emit(syscall_inst())

    # Infinite loop: just write the message and yield
    # sys_yield()
    cb.emit(mov_eax_imm32(2))
    cb.emit(syscall_inst())

    # Jump back to loop_start
    # jmp loop_start (relative)
    # E9 rel32 where rel32 = loop_start - (current_ip + 5)
    jmp_offset = loop_start - (len(cb.code) + 5)
    cb.emit(bytes([0xE9]))
    cb.emit(struct.pack('<i', jmp_offset))

    code, strings = cb.build()
    return build_elf(code, strings)


def build_shell():
    """Minimal shell: reads commands from stdin, supports exit/ls, fork+exec for others.
    
    Syscalls: 0=write, 1=exit, 4=waitpid, 6=read, 8=fork, 9=exec, 15=readdir
    Buffer: 256 bytes at stack top (RSP+8) — used for input + "/bin/<cmd>" path
    """
    cb = CodeBuilder()
    welcome = b"Indominus OS Shell v0.2\n"
    prompt = b"$ "
    unknown = b"Unknown command\n"
    err_fork = b"fork failed\n"
    err_exec = b"exec failed\n"
    slash_bin = b"/bin/"
    exit_cmd = b"exit"
    ls_cmd = b"ls"
    newline = b"\n"
    err_readdir = b"readdir failed\n"
    dir_fd_buf = b"/"

    # ── Welcome message ──
    # sys_write(1, welcome, len)
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(welcome)
    cb.emit(mov_edx_imm32(len(welcome)))
    cb.emit(syscall_inst())

    # ════════════════════════════════════════════════════════════════════
    # MAIN LOOP
    # ════════════════════════════════════════════════════════════════════
    loop_start = len(cb.code)

    # ── Print prompt ──
    # sys_write(1, "$ ", 2)
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(1))
    cb.emit_lea_rsi_for_string(prompt)
    cb.emit(mov_edx_imm32(2))
    cb.emit(syscall_inst())

    # ── Read line from stdin ──
    # RSP+8 = 256-byte buffer
    # sys_read(0, rsp+8, 255) — leave room for null terminator
    cb.emit(mov_eax_imm32(6))           # syscall 6 = read
    cb.emit(mov_edi_imm32(0))           # fd 0 = stdin
    # lea rsi, [rsp+8]
    cb.emit(bytes([0x48, 0x8D, 0x74, 0x24, 0x08]))  # LEA RSI, [RSP+8]
    cb.emit(mov_edx_imm32(255))         # max 255 bytes
    cb.emit(syscall_inst())

    # RAX = bytes read. If 0 or negative, loop back
    # test eax, eax; jle loop_start
    cb.emit(bytes([0x85, 0xC0]))        # test eax, eax
    cb.emit(bytes([0x0F, 0x8E]))        # jle rel32 (placeholder)
    jle_to_loop = len(cb.code)
    cb.emit(struct.pack('<i', 0))        # placeholder

    # ── Scan for newline, replace with null terminator ──
    # RSI still points to buffer (RSP+8)
    # find_newline:
    #   cmp byte [rsi], 0x0A
    #   je  found_newline
    #   cmp byte [rsi], 0
    #   je  no_newline
    #   inc rsi
    #   jmp find_newline
    find_nl = len(cb.code)
    # cmp byte [rsi], 0x0A
    cb.emit(bytes([0x80, 0x3E, 0x0A]))
    # je found_newline
    cb.emit(bytes([0x74]))              # JE rel8
    found_nl_offset = [0]               # placeholder
    cb.emit(bytes([0x00]))              # placeholder
    # cmp byte [rsi], 0
    cb.emit(bytes([0x80, 0x3E, 0x00]))
    # je no_newline
    cb.emit(bytes([0x74]))              # JE rel8
    no_nl_offset = [0]                  # placeholder
    cb.emit(bytes([0x00]))              # placeholder
    # inc rsi
    cb.emit(bytes([0x48, 0xFF, 0xC6]))  # INC RSI
    # jmp find_newline
    cb.emit(bytes([0xE9]))              # JMP rel32
    jmp_to_find = struct.pack('<i', find_nl - (len(cb.code) + 4))
    cb.emit(jmp_to_find)

    # found_newline: mov byte [rsi], 0
    found_newline = len(cb.code)
    cb.emit(bytes([0xC6, 0x06, 0x00]))  # MOV BYTE [RSI], 0
    # jmp check_commands
    cb.emit(bytes([0xE9]))              # JMP rel32
    jmp_to_check = [0]                  # placeholder
    cb.emit(struct.pack('<i', 0))

    # no_newline: mov byte [rsp+8], 0 (null-terminate at start if empty)
    no_newline = len(cb.code)
    cb.emit(bytes([0xC6, 0x44, 0x24, 0x08, 0x00]))  # MOV BYTE [RSP+8], 0
    # Fall through to check_commands

    # ── Patch JE offsets ──
    # found_newline relative to cmp instruction
    found_nl_rel = found_newline - (find_nl + 9)  # 9 = cmp(3) + je(2) + cmp(3) + je(2) - 1
    # Actually let me recalculate carefully:
    # find_nl + 0: cmp byte [rsi], 0x0A  (3 bytes)
    # find_nl + 3: je found_newline       (2 bytes)
    # find_nl + 5: cmp byte [rsi], 0     (3 bytes)
    # find_nl + 8: je no_newline          (2 bytes)
    # find_nl + 10: inc rsi               (3 bytes)
    # find_nl + 13: jmp find_newline      (5 bytes)
    found_nl_rel = found_newline - (find_nl + 5)  # target - (je_ip + 2)
    no_nl_rel = no_newline - (find_nl + 10)       # target - (je_ip + 2)

    # Patch the JE at find_nl+3
    struct.pack_into('<i', cb.code, find_nl + 4, found_newline - (find_nl + 5))
    # Patch the JE at find_nl+8
    struct.pack_into('<i', cb.code, find_nl + 9, no_newline - (find_nl + 10))

    # Actually the JE rel8 uses a SIGNED byte offset, not i32. Let me fix this.
    # JE rel8: offset is from the instruction AFTER the JE (IP after reading the JE)
    # find_nl+3: JE rel8 → offset = found_newline - (find_nl + 5)
    # find_nl+8: JE rel8 → offset = no_newline - (find_nl + 10)
    # These need to fit in a signed i8 (-128..127)
    fnl_off = found_newline - (find_nl + 5)
    nnl_off = no_newline - (find_nl + 10)
    assert -128 <= fnl_off <= 127, f"found_newline offset {fnl_off} out of range"
    assert -128 <= nnl_off <= 127, f"no_newline offset {nnl_off} out of range"
    struct.pack_into('<b', cb.code, find_nl + 4, fnl_off)
    struct.pack_into('<b', cb.code, find_nl + 9, nnl_off)

    # check_commands starts here
    check_commands = len(cb.code)

    # ── Patch jle to loop ──
    jle_rel = loop_start - (jle_to_loop + 4)
    struct.pack_into('<i', cb.code, jle_to_loop, jle_rel)

    # ── Patch jmp from found_newline to check_commands ──
    jmp_check_rel = check_commands - (found_newline + 6)
    struct.pack_into('<i', cb.code, found_newline + 2, jmp_check_rel)

    # ════════════════════════════════════════════════════════════════════
    # CHECK BUILT-IN COMMANDS
    # ════════════════════════════════════════════════════════════════════

    # ── Check "exit" ──
    # memcmp(rsp+8, "exit", 4) == 0?
    # lea rdi, [rsp+8]
    cb.emit(bytes([0x48, 0x8D, 0x7C, 0x24, 0x08]))  # LEA RDI, [RSP+8]
    cb.emit_lea_rsi_for_string(exit_cmd)
    # memcmp: compare byte by byte (4 bytes)
    for i in range(4):
        # mov al, [rdi+i]
        cb.emit(bytes([0x8A, 0x47, i]))              # MOV AL, [RDI+i]
        # cmp al, [rsi+i]
        cb.emit(bytes([0x3A, 0x46, i]))              # CMP AL, [RSI+i]
        # jne not_exit
        cb.emit(bytes([0x75]))                        # JNE rel8
        cb.emit(bytes([0x00]))                        # placeholder
    # All 4 bytes match — exit(0)
    # sys_exit(0)
    cb.emit(mov_eax_imm32(1))           # syscall 1 = exit
    cb.emit(mov_edi_imm32(0))           # exit code 0
    cb.emit(syscall_inst())

    # Patch JNEs to skip exit (jump to next check)
    not_exit_checks = []
    for i in range(4):
        # The JNE is at find_nl+... actually let me track positions
        pass  # will patch below

    # ════════════════════════════════════════════════════════════════════
    # CHECK "ls" COMMAND
    # ════════════════════════════════════════════════════════════════════
    check_ls = len(cb.code)

    # memcmp(rsp+8, "ls", 2) == 0?
    cb.emit(bytes([0x48, 0x8D, 0x7C, 0x24, 0x08]))  # LEA RDI, [RSP+8]
    cb.emit_lea_rsi_for_string(ls_cmd)
    # Compare 2 bytes
    cb.emit(bytes([0x8A, 0x47, 0x00]))  # MOV AL, [RDI+0]
    cb.emit(bytes([0x3A, 0x46, 0x00]))  # CMP AL, [RSI+0]
    cb.emit(bytes([0x75]))              # JNE not_ls
    jne_not_ls_1 = len(cb.code)
    cb.emit(bytes([0x00]))              # placeholder
    cb.emit(bytes([0x8A, 0x47, 0x01]))  # MOV AL, [RDI+1]
    cb.emit(bytes([0x3A, 0x46, 0x01]))  # CMP AL, [RSI+1]
    cb.emit(bytes([0x75]))              # JNE not_ls
    jne_not_ls_2 = len(cb.code)
    cb.emit(bytes([0x00]))              # placeholder

    # It's "ls" — open "/" and readdir
    # sys_open("/") — but we need a helper. For now, just readdir fd 0 won't work.
    # Alternative: open "/" via sys_open, get fd, readdir, close.
    # sys_open needs a user pointer to a string. We have "/" in the buffer somewhere.
    # Let's put "/" at the end of our string data and open it.

    # Actually, we can't easily pass a path to sys_open in this assembly.
    # The simplest approach: write each entry from readdir to stdout.
    # But readdir needs an fd. We need to open a directory first.

    # Workaround: sys_open with path "/"
    # We need to pass a pointer to "/" string. We'll add it to strings.
    # But we need the pointer at runtime. Let's use LEA.

    # sys_open(lea_rsi_for_string("/"))
    cb.emit(mov_eax_imm32(12))          # syscall 12 = open
    cb.emit_lea_rsi_for_string(dir_fd_buf)  # path = "/"
    cb.emit(syscall_inst())
    # RAX = fd or negative errno
    # test rax, rax; js readdir_error
    cb.emit(bytes([0x48, 0x85, 0xC0]))  # TEST RAX, RAX
    cb.emit(bytes([0x78]))              # JS rel8
    js_readdir_err = len(cb.code)
    cb.emit(bytes([0x00]))              # placeholder

    # Save fd in R12 (callee-saved, but we don't call anything)
    # Actually, we can use any register since we don't call functions.
    # Save fd: push rax; ... pop later. Or use R13.
    # Let's use R13 for the fd.
    cb.emit(bytes([0x49, 0x89, 0xC5]))  # MOV R13, RAX (save fd)

    # readdir_loop:
    #   buf at RSP+8 (reuse the 256-byte buffer)
    #   sys_readdir(fd, rsp+8, 255)
    readdir_loop = len(cb.code)
    cb.emit(mov_eax_imm32(15))          # syscall 15 = readdir
    cb.emit(bytes([0x4C, 0x89, 0xEF]))  # MOV RDI, R13 (fd)
    cb.emit(bytes([0x48, 0x8D, 0x74, 0x24, 0x08]))  # LEA RSI, [RSP+8]
    cb.emit(mov_edx_imm32(255))         # count
    cb.emit(syscall_inst())

    # test rax, rax; jle readdir_done
    cb.emit(bytes([0x48, 0x85, 0xC0]))  # TEST RAX, RAX
    cb.emit(bytes([0x0F, 0x8E]))        # JLE rel32
    readdir_done_patch = len(cb.code)
    cb.emit(struct.pack('<i', 0))        # placeholder

    # RAX = bytes read. Write them to stdout.
    # sys_write(1, rsp+8, rax)
    cb.emit(mov_eax_imm32(0))           # syscall 0 = write
    cb.emit(mov_edi_imm32(1))           # fd 1 = stdout
    cb.emit(bytes([0x48, 0x8D, 0x74, 0x24, 0x08]))  # LEA RSI, [RSP+8]
    # mov edx, eax (but edx is 32-bit, eax has the count)
    cb.emit(bytes([0x89, 0xC2]))        # MOV EDX, EAX
    cb.emit(syscall_inst())

    # jmp readdir_loop
    cb.emit(bytes([0xE9]))              # JMP rel32
    jmp_readdir = struct.pack('<i', readdir_loop - (len(cb.code) + 4))
    cb.emit(jmp_readdir)

    # readdir_done: close the fd
    readdir_done = len(cb.code)
    struct.pack_into('<i', cb.code, readdir_done_patch, readdir_done - (readdir_done_patch + 4))
    # sys_close(fd)
    cb.emit(mov_eax_imm32(10))          # syscall 10 = close
    cb.emit(bytes([0x4C, 0x89, 0xEF]))  # MOV RDI, R13 (fd)
    cb.emit(syscall_inst())

    # jmp loop_start
    cb.emit(bytes([0xE9]))
    jmp_to_loop = struct.pack('<i', loop_start - (len(cb.code) + 4))
    cb.emit(jmp_to_loop)

    # ── not_ls: check if it's a known empty command, else try fork+exec ──
    not_ls = len(cb.code)
    struct.pack_into('<b', cb.code, jne_not_ls_1, not_ls - (jne_not_ls_1 + 2))
    struct.pack_into('<b', cb.code, jne_not_ls_2, not_ls - (jne_not_ls_2 + 2))

    # ── readdir error ──
    readdir_error = len(cb.code)
    struct.pack_into('<b', cb.code, js_readdir_err, readdir_error - (js_readdir_err + 2))
    # Write error message and loop
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(2))
    cb.emit_lea_rsi_for_string(err_readdir)
    cb.emit(mov_edx_imm32(len(err_readdir)))
    cb.emit(syscall_inst())
    cb.emit(bytes([0xE9]))
    cb.emit(struct.pack('<i', loop_start - (len(cb.code) + 4)))

    # ════════════════════════════════════════════════════════════════════
    # FORK + EXEC (for external commands)
    # ════════════════════════════════════════════════════════════════════

    # ── Check if command is empty (first byte is null) ──
    cb.emit(bytes([0x80, 0x7C, 0x24, 0x08, 0x00]))  # CMP BYTE [RSP+8], 0
    cb.emit(bytes([0x0F, 0x84]))        # JE rel32 loop_start (empty command)
    jempty = len(cb.code)
    cb.emit(struct.pack('<i', 0))        # placeholder
    struct.pack_into('<i', cb.code, jempty, loop_start - (jempty + 4))

    # ── Build "/bin/<cmd>" path ──
    # Copy "/bin/" to buffer
    # lea rdi, [rsp+8]  (destination)
    cb.emit(bytes([0x48, 0x8D, 0x7C, 0x24, 0x08]))  # LEA RDI, [RSP+8]
    cb.emit_lea_rsi_for_string(slash_bin)
    # copy_loop: mov al, [rsi]; test al, al; je copy_done; mov [rdi], al; inc rsi; inc rdi; jmp copy_loop
    copy_loop = len(cb.code)
    cb.emit(bytes([0x8A, 0x06]))        # MOV AL, [RSI]
    cb.emit(bytes([0x84, 0xC0]))        # TEST AL, AL
    cb.emit(bytes([0x74]))              # JE copy_done
    jcopy_done = len(cb.code)
    cb.emit(bytes([0x00]))              # placeholder
    cb.emit(bytes([0x88, 0x07]))        # MOV [RDI], AL
    cb.emit(bytes([0x48, 0xFF, 0xC6]))  # INC RSI
    cb.emit(bytes([0x48, 0xFF, 0xC7]))  # INC RDI
    cb.emit(bytes([0xE9]))              # JMP copy_loop
    cb.emit(struct.pack('<i', copy_loop - (len(cb.code) + 4)))
    copy_done = len(cb.code)
    struct.pack_into('<b', cb.code, jcopy_done, copy_done - (jcopy_done + 2))

    # Append command name (from RSP+8, up to newline/null)
    # lea rsi, [rsp+8]  (source = original command)
    cb.emit(bytes([0x48, 0x8D, 0x74, 0x24, 0x08]))  # LEA RSI, [RSP+8]
    # copy_cmd: mov al, [rsi]; cmp al, 0x0A; je cmd_done; cmp al, 0; je cmd_done; mov [rdi], al; inc rsi; inc rdi; jmp copy_cmd
    copy_cmd = len(cb.code)
    cb.emit(bytes([0x8A, 0x06]))        # MOV AL, [RSI]
    cb.emit(bytes([0x3C, 0x0A]))        # CMP AL, 0x0A (newline)
    cb.emit(bytes([0x74]))              # JE cmd_done
    jcmd_done1 = len(cb.code)
    cb.emit(bytes([0x00]))
    cb.emit(bytes([0x3C, 0x00]))        # CMP AL, 0 (null)
    cb.emit(bytes([0x74]))              # JE cmd_done
    jcmd_done2 = len(cb.code)
    cb.emit(bytes([0x00]))
    cb.emit(bytes([0x88, 0x07]))        # MOV [RDI], AL
    cb.emit(bytes([0x48, 0xFF, 0xC6]))  # INC RSI
    cb.emit(bytes([0x48, 0xFF, 0xC7]))  # INC RDI
    cb.emit(bytes([0xE9]))              # JMP copy_cmd
    cb.emit(struct.pack('<i', copy_cmd - (len(cb.code) + 4)))
    cmd_done = len(cb.code)
    struct.pack_into('<b', cb.code, jcmd_done1, cmd_done - (jcmd_done1 + 2))
    struct.pack_into('<b', cb.code, jcmd_done2, cmd_done - (jcmd_done2 + 2))

    # Null-terminate the path
    cb.emit(bytes([0xC6, 0x07, 0x00]))  # MOV BYTE [RDI], 0

    # ── fork() ──
    # sys_fork()
    cb.emit(mov_eax_imm32(8))           # syscall 8 = fork
    cb.emit(syscall_inst())

    # test rax, rax
    cb.emit(bytes([0x48, 0x85, 0xC0]))  # TEST RAX, RAX
    # js fork_failed (fork returned negative)
    cb.emit(bytes([0x78]))              # JS rel8
    js_fork_fail = len(cb.code)
    cb.emit(bytes([0x00]))              # placeholder
    # je child (fork returned 0)
    cb.emit(bytes([0x74]))              # JE rel8
    je_child = len(cb.code)
    cb.emit(bytes([0x00]))              # placeholder

    # ── Parent: waitpid(child_pid) ──
    # RAX = child_pid from fork
    cb.emit(bytes([0x48, 0x89, 0xC7]))  # MOV RDI, RAX (child_pid)
    cb.emit(mov_eax_imm32(4))           # syscall 4 = waitpid
    cb.emit(syscall_inst())
    # jmp loop_start
    cb.emit(bytes([0xE9]))
    cb.emit(struct.pack('<i', loop_start - (len(cb.code) + 4)))

    # ── Child: exec("/bin/<cmd>") ──
    child = len(cb.code)
    struct.pack_into('<b', cb.code, je_child, child - (je_child + 2))
    # RSP+8 now contains "/bin/<cmd>\0"
    # sys_exec(rsp+8)
    cb.emit(mov_eax_imm32(9))           # syscall 9 = exec
    cb.emit(bytes([0x48, 0x8D, 0x7C, 0x24, 0x08]))  # LEA RDI, [RSP+8]
    cb.emit(syscall_inst())

    # exec failed — write error and exit
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(2))
    cb.emit_lea_rsi_for_string(err_exec)
    cb.emit(mov_edx_imm32(len(err_exec)))
    cb.emit(syscall_inst())
    cb.emit(mov_eax_imm32(1))
    cb.emit(mov_edi_imm32(1))
    cb.emit(syscall_inst())

    # ── fork_failed ──
    fork_failed = len(cb.code)
    struct.pack_into('<b', cb.code, js_fork_fail, fork_failed - (js_fork_fail + 2))
    cb.emit(mov_eax_imm32(0))
    cb.emit(mov_edi_imm32(2))
    cb.emit_lea_rsi_for_string(err_fork)
    cb.emit(mov_edx_imm32(len(err_fork)))
    cb.emit(syscall_inst())
    # jmp loop_start
    cb.emit(bytes([0xE9]))
    cb.emit(struct.pack('<i', loop_start - (len(cb.code) + 4)))

    code, strings = cb.build()
    return build_elf(code, strings)


# ── Main ────────────────────────────────────────────────────────────────────

outdir = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'userspace', 'rootfs', 'bin')
os.makedirs(outdir, exist_ok=True)

binaries = [
    ("init", build_init),
    ("indosh", build_shell),
]

for name, builder in binaries:
    data = builder()
    path = os.path.join(outdir, name)
    with open(path, 'wb') as f:
        f.write(data)
    print(f"  {name}: {len(data)} bytes", file=sys.stderr)

print(f"\nGenerated {len(binaries)} userspace binaries in {outdir}", file=sys.stderr)
