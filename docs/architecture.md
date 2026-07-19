# INDOMINUS OS — Architecture

## Syscall ABI

### Overview

INDOMINUS uses the `syscall`/`sysret` mechanism for user→kernel transitions.
The CPU saves RIP→RCX and RFLAGS→R11, then jumps to the LSTAR entry point.
Stack switching is manual (via `swapgs` + per-CPU data).

### Calling Convention

| Register | Purpose |
|----------|---------|
| RAX | Syscall number (0=sys_write, 1=sys_exit, 2=sys_yield, 3=sys_getpid) |
| RDI | Arg0 |
| RSI | Arg1 |
| RDX | Arg2 |
| R10 | Arg3 |
| R8  | Arg4 |
| R9  | Arg5 |

Return value in RAX. On error, RAX = `u64::MAX` (-ENOSYS).

### Canonical SyscallFrame Layout

Both the timer interrupt handler and the syscall entry handler save/restore
GP registers in the **same canonical order**. This frame is 15 qwords (120 bytes),
followed by the 5-qword IRET frame (40 bytes).

```
Offset  Size  Register  Notes
──────  ────  ────────  ──────────────────────────────────
  +0     8    RAX       Syscall number / return value
  +8     8    RBX
 +16     8    RCX       User RIP (saved by CPU on syscall)
 +24     8    RDX       Arg2
 +32     8    RSI       Arg1
 +40     8    RDI       Arg0
 +48     8    RBP
 +56     8    R8        Arg4
 +64     8    R9        Arg5
 +72     8    R10       Arg3
 +80     8    R11       User RFLAGS (saved by CPU on syscall)
 +88     8    R12
 +96     8    R13
+104     8    R14
+112     8    R15
─────── IRET frame (read by `iretq`) ───────
+120     8    RIP
+128     8    CS
+136     8    RFLAGS
+144     8    RSP       (only on privilege-level change)
+152     8    SS        (only on privilege-level change)
```

### Push/Pop Order

**Save (push order):** R15 first → RAX last.
This places RAX at `[RSP+0]` (lowest address = top of stack).

```asm
push r15    ; highest address
push r14
...
push rax    ; lowest address = RSP
```

**Restore (pop order):** RAX first → R15 last.

```asm
pop rax     ; reads [RSP+0]
pop rbx     ; reads [RSP+8]
...
pop r15     ; reads [RSP+112]
```

### Frame Setup (Initial Process Spawn)

`setup_initial_stack_frame_kernel` and `setup_initial_stack_frame_user` in
`process.rs` write the frame in the same order as the pop sequence:

```
frame[0]  = RAX = 0
frame[1]  = RBX = 0
...
frame[14] = R15 = 0
frame[15] = RIP (entry point)
frame[16] = CS  (kernel: 0x08, user: 0x1B)
frame[17] = RFLAGS = 0x202 (IF=1)
frame[18] = RSP (kernel: stack_top, user: user_rsp)
frame[19] = SS  (kernel: 0x10, user: 0x23)
```

### Syscall Entry Flow (`syscall_entry`)

1. `swapgs` — switch to kernel GSBase (per-CPU data)
2. `mov gs:[0], rsp` — save user RSP
3. `mov rsp, gs:[8]` — load kernel RSP
4. Push 15 GP registers (R15→RAX) onto kernel stack
5. `mov rdi, rsp` — pass frame pointer to Rust
6. `call syscall_dispatch`
7. Check `gs:[16]` (force_switch flag)
   - If 0: pop 15 GP registers, `swapgs`, `sysretq`
   - If 1: construct IRET frame, call `schedule()`, switch stack, pop, `iretq`

### Timer Interrupt Flow (`timer_interrupt_handler`)

1. Push 15 GP registers (R15→RAX)
2. `call schedule(saved_rsp)` → returns new SP
3. EOI to LAPIC
4. `mov rsp, r12` — switch to new process stack
5. Pop 15 GP registers (RAX→R15)
6. `iretq`

### Per-CPU Data (GS-relative)

| Offset | Field | Description |
|--------|-------|-------------|
| 0 | user_rsp | User RSP saved on syscall entry |
| 8 | kernel_rsp | Top of current process's kernel stack |
| 16 | force_switch | 1 = context switch after syscall, 0 = sysret |

KERNEL_GS_BASE = `phys_to_kernel_virt(&raw const PER_CPU)` (higher-half, mapped in all PML4s).
GS_BASE = 0 (user mode).

### Key Invariants

1. **Frame layout is canonical.** Both timer handler and syscall entry use the
   exact same push/pop order. The frame setup code writes in pop order.
2. **IRET frame follows GP frame.** Always at offset +120 from frame base.
3. **Higher-half addresses only.** After CR3 switch to user PML4, all kernel
   code/data/LAPIC/per-CPU use higher-half virtual addresses (entries 256–511).
4. **No identity map in user PML4.** Only upper-half entries are copied.

### Current Syscall Table

| Num | Name | Args | Returns |
|-----|------|------|---------|
| 0 | sys_write | fd, buf_ptr, count | bytes written or u64::MAX |
| 1 | sys_exit | exit_code | never (force_switch) |
| 2 | sys_yield | — | 0 (force_switch) |
| 3 | sys_getpid | — | current PID |

## Process Lifecycle

### State Machine

```text
                    spawn()
                       │
                       ▼
    ┌──────────────────────────────────┐
    │            READY                  │
    │  (in run queue, waiting for CPU) │
    └──────────┬───────────────────────┘
               │
          dispatch()
               │
               ▼
    ┌──────────────────────────────────┐
    │           RUNNING                 │
    │  (currently executing on CPU)    │
    └──────┬────────────────┬──────────┘
           │                │
      timer tick       sys_exit()
      (quantum expired)     │
           │                ▼
           │    ┌──────────────────────────────────┐
           │    │           ZOMBIE                  │
           │    │  (exited, resources not freed)    │
           │    └──────────────────────────────────┘
           │
           ▼
    ┌──────────────────────────────────┐
    │            READY                 │
    │  (returned to run queue)         │
    └──────────────────────────────────┘
```

### State Transitions

| From | To | Trigger | Code |
|------|----|---------|------|
| — | READY | `spawn()` / `spawn_user()` | `Scheduler::spawn*()` |
| READY | RUNNING | `dispatch_first()` or `switch_next()` | `Scheduler::switch_next*()` |
| RUNNING | READY | Timer quantum expires | `switch_next()` |
| RUNNING | Zombie | `sys_exit()` | `sys_exit()` |
| Zombie | — | Never returns to RUNNING | `find_next_ready()` skips non-Ready |

### Key Rules

1. **Zombie processes are never scheduled.** `find_next_ready()` only returns
   processes in `Ready` state. A Zombie process is permanently excluded.
2. **force_switch always yields.** `schedule_force()` (used by sys_exit, sys_yield)
   calls `switch_next_force()` which always performs a context switch, regardless
   of the time quantum.
3. **Timer-driven preemption is quantum-gated.** `schedule()` → `on_tick()` only
   calls `switch_next()` when the quantum expires (every 5 ticks = 50ms).

### Scheduler Selection

`find_next_ready(after_pid)` performs round-robin scanning:

1. **Pass 1:** Scan from `after_pid+1` to `after_pid`, skipping PID 0 (idle).
   Return the first `Ready` process found.
2. **Pass 2:** If no non-idle Ready process exists, return PID 0 (idle) as fallback.
3. **Return None** if no process is ready at all (should never happen with idle).

### Process Table

Maximum 8 concurrent processes (`MAX_PROCESSES = 8`).

| PID | Role | Kernel Stack | Page Table |
|-----|------|-------------|------------|
| 0 | Idle (HLT loop) | Boot stack | Kernel PML4 |
| 1 | task_a (kernel) | Heap-allocated | Kernel PML4 |
| 2 | task_b (kernel) | Heap-allocated | Kernel PML4 |
| 3+ | User processes | Heap-allocated | Per-process PML4 |
