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
