# INDOMINUS OS — Regression Test Checklist

After any kernel change, run this checklist to verify no regressions.

## Build Verification

```
[ ] Build succeeds (cargo build --release)
[ ] tools/verify_kernel.py passes (ELF magic, 64-bit, entry point, PT_LOAD)
[ ] Kernel ELF size < 16 MiB
[ ] Entry point in kernel range (0xFFFFFFFF80000000..0xFFFFFFFFC0000000)
```

## Boot Verification

```
[ ] QEMU boots to kernel_main
[ ] Boot log shows "[KERNEL] INDOMINUS OS -- scheduler test"
[ ] Kernel physical address range printed correctly
[ ] "[KERNEL] All init done." appears
[ ] User test ELF size > 0
```

## Process Creation

```
[ ] Minimum 3 processes created (PID 0–2)
[ ] PID 0 = idle (HLT loop)
[ ] PID 1 = init/reaper (kernel-mode, reaps orphaned zombies)
[ ] PID 2 = shell (indosh) or first user process
[ ] PID 3+ = additional user processes / test binaries
```

## Scheduler Behavior

```
[ ] Round-robin scheduling with 5-tick quantum (50ms)
[ ] Shell process (PID 2) receives input via sys_read, produces output via sys_write
[ ] Init process (PID 1) reaps zombie children
[ ] No process starvation (all Ready processes eventually run)
[ ] [TICK] and [SWITCH] markers appear in serial output
```

## Ring 3 Execution (CRITICAL)

```
[ ] Timer interrupts fire from Ring 3 (user processes receive ticks)
[ ] Shell process (PID 2) responds to keyboard input via sys_read
[ ] Shell process writes output via sys_write
[ ] User processes can call sys_exit → transitions to ZOMBIE
[ ] PID 1 (init) reaps zombie children
[ ] No page faults after user process exit
[ ] No triple faults
```

## No Faults

```
[ ] No #PF (Page Fault) exceptions
[ ] No #GP (General Protection Fault) exceptions
[ ] No #DF (Double Fault) exceptions
[ ] No #UD (Invalid Opcode) exceptions
[ ] No #SS (Stack Segment Fault) exceptions
[ ] No triple faults
[ ] No silent hangs
```

## Memory

```
[ ] Kernel heap allocations succeed (no alloc_error_layout panic)
[ ] User PML4s created without identity map (PML4 entry 0 is zero)
[ ] Kernel upper-half entries (256–511) present in user PML4s
[ ] LAPIC MMIO accessible at 0xFFFFFFFFFEE00000 in user PML4s
```

## GDT/TSS

```
[ ] GDT loaded with 6 entries (null, kernel code, kernel data, user code, user data, TSS)
[ ] TSS RSP0 updated on each context switch
[ ] TSS busy bit cleared before re-loading TR
[ ] TR points to kernel virtual address of TSS (not physical)
[ ] GDTR base is kernel virtual address
```

## Syscalls

```
[ ] sys_write returns byte count (0–count) or u64::MAX on error
[ ] sys_exit causes force_switch (never returns)
[ ] sys_yield causes force_switch (always context switches)
[ ] sys_getpid returns current PID
[ ] sys_read reads from file descriptor (stdin/pipe)
[ ] sys_pipe creates pipe pair
[ ] sys_fork creates child via Copy-on-Write
[ ] sys_exec replaces process with new ELF
[ ] sys_close closes file descriptor
[ ] sys_dup duplicates file descriptor
[ ] sys_open opens file from VFS
[ ] sys_lseek seeks in file
[ ] sys_dup2 duplicates to specific FD slot
[ ] sys_readdir reads directory entries
[ ] Syscall frame layout is canonical (R15 first → RAX last)
[ ] swapgs on syscall entry AND before iretq in force_switch path
```

## After Stabilization Changes (P1–P8)

```
[ ] All of the above still pass
[ ] build.ps1 runs clean (release profile)
[ ] verify_kernel.py exits 0 after kernel build
[ ] tools/ directory is clean (no temp files)
[ ] .gitignore covers QEMU logs, test outputs, tmp/
[ ] 0 compiler errors, warnings limited to intentionally-kept items
```

## Phase 9.5-9.9: Integration Test Phases

```
Phase 9.5: FD + Syscall Integration (14/14 pass)
Phase 9.6: ELF Loading from Persistent Filesystem (7/7 pass)
Phase 9.7: User-Space Shell Infrastructure (6/6 pass)
Phase 9.8: Init Process PID 1 (7/7 pass)
Phase 9.9: FAT Persistence + Regression Test Matrix (6/6 pass)
```

## Phase 10: Hardening + Testing

```
Phase 10A: CLOEXEC restructured, overflow guards, doc updates
Phase 10B: Process lifecycle fixes (drop, exit, waitpid)
Phase 10C: AHCI TFES fix (bit 30), validate_elf_header
Phase 10D: Comprehensive code audit + doc fixes
```

## Phase 11: FAT Write Support

```
[ ] create_file creates new files
[ ] write_file writes data and reads back correctly
[ ] create_file + write_file end-to-end
[ ] Large file write (3+ clusters) reads back correctly
[ ] create_dir creates directories
[ ] delete_file removes files
[ ] Multiple create/delete cycles work
```

## Phase 12: Full Interactive Userspace & Shell

```
Kernel Syscalls:
[ ] sys_execve (18) executes ELF with argc/argv propagation
[ ] sys_chdir (19) changes CWD, validates directory exists
[ ] sys_getcwd (20) returns CWD to user buffer
[ ] sys_mkdir (21) creates directories
[ ] sys_waitpid blocking: parent blocks until child exits
[ ] sys_waitpid WNOHANG: returns immediately if no child
[ ] O_APPEND flag: writes append to file end

Shell Features:
[ ] Tokenizer handles words, quotes, pipes, redirections
[ ] Builtins: help, exit, echo, pwd, cd, clear, cat, ls, mkdir, touch, rm, pid, ps, true, false
[ ] External commands: fork → execve → waitpid lifecycle
[ ] PATH resolution: /bin/<command>
[ ] File redirection: > (truncate), >> (append), < (input)
[ ] Pipelines: cmd1 | cmd2 chains stdin/stdout
[ ] CWD tracking: cd changes shell CWD

Userspace Utilities:
[ ] echo prints arguments
[ ] cat prints file contents
[ ] ls lists directory entries
[ ] pwd prints current directory
[ ] mkdir creates directories
[ ] touch creates empty files
[ ] rm deletes files
[ ] true exits 0
[ ] false exits 1
```

## Automated Boot Test (qemu_boot_test.py)

```powershell
# Run 10 boot tests
python tools\qemu_boot_test.py --count 10 --timeout 60 --send-input

# Run 3 boot tests with input
python tools\qemu_boot_test.py --count 3 --timeout 60 --send-input
```

Expected: All boots pass all 6 phases (9.4-9.9), 0 TFES, 0 panics.

## Phase 9.4: End-to-End Verification (AHCI + FAT16 + VFS)

```
Section 1: AHCI Raw Sector I/O
[ ] T1.1 MBR read + signature 0x55AA
[ ] T1.2 Partition boot sector + valid BPB
[ ] T1.3 Four consecutive sector reads (LBA 1-4)
[ ] T1.4 Scattered LBA reads OK
[ ] T1.5 Triple read-after-read consistent (LBA 0x3F)
[ ] T1.6 Out-of-bounds read returns error
[ ] T1.7 Wrong buffer size returns error
[ ] T1.8 MBR triple-read consistent + partition table
[ ] T1.9 FAT boot sector triple-read consistent + valid BPB

Section 2: FAT16 Filesystem
[ ] T2.0 FAT16 filesystem mounted
[ ] T2.0b FAT re-mount consistent
[ ] T2.1 Filesystem variant: FAT16
[ ] T2.2 Root dir: EFI, NvVars, startup.nsh present
[ ] T2.2b Root readdir consistent (2 reads)
[ ] T2.3 EFI/BOOT subdirectory found
[ ] T2.4 Deep lookup: EFI/BOOT/BOOTX64.EFI
[ ] T2.5 Read BOOTX64.EFI: MZ + consistent
[ ] T2.6 Read kernel.elf: ELF header + consistent (CRITICAL — multi-cluster, 446,872 bytes)

Section 3: VFS Integration
[ ] T3.0 FAT16 mounted at /disk via VFS
[ ] T3.1 VFS resolve('/disk') -> directory
[ ] T3.2 VFS resolve('/disk/EFI') -> directory
[ ] T3.3 VFS read startup.nsh: consistent
[ ] T3.4 VFS readdir('/disk'): EFI + startup found
[ ] T3.5 VFS read_file: BOOTX64.EFI consistent + MZ
[ ] T3.6 open non-existent -> NotFound
[ ] T3.7 VFS rejects malformed paths

Section 4: Regression Tests
[ ] Phase 9.1 Block device abstraction: ALL PASSED
[ ] Phase 9.2 VFS file I/O: ALL PASSED
[ ] Phase 9.2b FD semantics: ALL PASSED
[ ] Phase 9.3 AHCI disk read: ALL PASSED
```

## How to Run

```powershell
# Build
powershell -ExecutionPolicy Bypass -File build.ps1

# Verify ELF
python tools\verify_kernel.py indominus-kernel\target\x86_64-unknown-none\release\kernel.elf

# Deploy to ESP (build.ps1 does this automatically)

# Run QEMU
powershell -ExecutionPolicy Bypass -File run.bat
```

## What to Look For

- **PASS:** Shell prompt appears, keyboard input produces output, syscalls work
- **PASS:** `[TICK]` and `[SWITCH]` markers appear continuously
- **FAIL:** Triple fault, triple fault, triple fault (QEMU reboots)
- **FAIL:** Only `[TICK]` markers, no `[SWITCH]` (scheduler not context-switching)
- **FAIL:** Page fault or general protection fault (syscall or memory bug)
