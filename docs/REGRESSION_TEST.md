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
[ ] 4 processes created (PID 0–3 minimum)
[ ] PID 1 (task_a) prints "[TASK_A] tick" messages
[ ] PID 2 (task_b) prints "[TASK_B] tick" messages
[ ] PID 3 (user) spawned successfully
[ ] PID 4 (user) spawned successfully
```

## Scheduler Behavior

```
[ ] PID 1 and PID 2 alternate in 5-tick quanta
[ ] PID 1 prints 5 ticks, then PID 2 prints 5 ticks
[ ] Round-robin cycling continues indefinitely for kernel tasks
[ ] No process starvation (all Ready processes eventually run)
[ ] [TICK] and [SWITCH] markers appear in serial output
```

## Ring 3 Execution (CRITICAL)

```
[ ] Timer interrupts fire from Ring 3 (user processes receive ticks)
[ ] PID 3 writes "Hello from user!" via sys_write (syscall 0)
[ ] PID 3 calls sys_exit (syscall 1) → transitions to ZOMBIE
[ ] After PID 3 exits, PID 4 runs
[ ] PID 4 writes "Hello from user!" via sys_write
[ ] PID 4 calls sys_exit → transitions to ZOMBIE
[ ] After both user processes exit, PID 1/2 resume round-robin
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

- **PASS:** `Hello from user!` appears twice, then kernel tasks resume
- **FAIL:** Triple fault, triple fault, triple fault (QEMU reboots)
- **FAIL:** Only kernel ticks, no user output (Ring 3 not executing)
- **FAIL:** Page fault after user process exits (syscall exit path broken)
