# INDOMINUS OS — Architecture

## Virtual Address Map

### Kernel Space (Upper Half, PML4 entries 256–511)

```text
0xFFFF_FFFF_FFFF_FFFF ─────────────────────────────────── Top of memory
                    ...
0xFFFF_FFFF_FEE0_0000 ─────────────────────────────────── LAPIC MMIO (1 page)
0xFFFF_FFFF_FED0_0000                                       (mapped from phys 0xFEE00000)
                    ...
0xFFFF_FFFF_D000_0000 ─────────────────────────────────── Kernel stack top (16 KiB)
0xFFFF_FFFF_CFFF_FFFF                                       Kernel stack bottom
0xFFFF_FFFF_C000_0000 ─────────────────────────────────── Kernel heap start (4 MiB)
0xFFFF_FFFF_BFFF_FFFF                                       Kernel heap end
                    ...
0xFFFF_FFFF_8000_0000 ─────────────────────────────────── Kernel .text start
                    ...                                     Kernel .text, .rodata, .data, .bss
0xFFFF_FFFF_800X_XXXX                                       Kernel physical end
```

### User Space (Lower Half, PML4 entries 0–255)

```text
0x0000_7FFF_FFFF_0000 ─────────────────────────────────── User stack top (grows down)
0x0000_7FFF_FFFE_FFFF                                       User stack bottom
                    ...
0x0000_0000_0060_0000 ─────────────────────────────────── User code end (top of 6 MiB)
0x0000_0000_0040_0000 ─────────────────────────────────── User code base (ELF load)
0x0000_0000_0000_0000                                       NULL guard (unmapped)
```

### Key Constants (`memory/mod.rs`)

| Symbol | Value | Description |
|--------|-------|-------------|
| `KERNEL_VIRT_BASE` | `0xFFFFFFFF80000000` | Kernel virtual base (-2 GiB) |
| `KERNEL_HEAP_BASE` | `0xFFFFFFFFC0000000` | Kernel heap start |
| `KERNEL_HEAP_INITIAL_SIZE` | `4 * 1024 * 1024` | 4 MiB initial heap |
| `KERNEL_STACK_TOP` | `0xFFFFFFFFD0000000` | Kernel stack top (16 KiB) |
| `KERNEL_STACK_SIZE` | `16 * 1024` | 16 KiB kernel stack |
| `USER_CODE_BASE` | `0x00400000` | User ELF load address |
| `USER_STACK_TOP` | `0x7FFFFFFF0000` | User stack top |
| `USER_KERNEL_STACK_SIZE` | `8192` | 8 KiB per-process kernel stack |
| `PAGE_SIZE` | `4096` | 4 KiB page |

### Page Table Layout

Kernel page tables (`vmm::init_kernel_page_tables`) create:

1. **Kernel higher-half mapping:** physical kernel pages → virtual `0xFFFFFFFF80000000+`
2. **Kernel heap:** physical frames allocated by PMM → virtual `0xFFFFFFFFC0000000..0xFFFFFFFFC0400000`
3. **Identity map:** first 4 GiB (virtual == physical) for safe CR3 transition
4. **LAPIC MMIO:** physical `0xFEE00000` → virtual `0xFFFFFFFFFEE00000` (1 page, shared via PML4 entries 256–511)

User PML4s (`vmm::create_user_pml4`) copy **only PML4 entries 256–511** from the kernel PML4. This means:
- Kernel code, data, heap, stack, and LAPIC are accessible in all PML4s
- The identity map (PML4 entry 0) is NOT present in user PML4s
- User pages (PML4 entries 0–255) are mapped separately per process

### PIC (Position-Independent Code) Address Model

With PIC, `&raw const` yields **physical** addresses (relocated by bootloader via `R_X86_64_RELATIVE`).
Function pointers in IDT/GDT/TSS entries must be converted to virtual addresses using `phys_to_kernel_virt()`.

```text
virt = phys + (KERNEL_VIRT_BASE - kernel_phys_start)
```

This applies to:
- IDT handler addresses (`handler_virt!` macro)
- GDT/TSS base addresses (`switch_gdt_to_virtual`)
- IST entries, double-fault stack address
- All static references used from Ring 3 (where identity map is absent)

## Boot Initialization Sequence

`kernel_main()` executes the following in order:

```
 1. Set KERNEL_PHYS_START from BootInfo
 2. gdt::init()                    — Build GDT, load TSS, load segment registers
 3. pmm::init(&memory_map)         — Initialize physical memory manager (bitmap)
 4. vmm::init_kernel_page_tables() — Create new PML4 with higher-half + identity map
 5. vmm::switch_page_table()       — Load new PML4 into CR3 (identity map still active)
 6. gdt::switch_gdt_to_virtual()   — Patch GDT/TSS to virtual addresses, reload GDTR+TR
 7. init_heap()                    — Initialize kernel heap allocator (4 MiB)
 8. idt::init()                    — Build IDT with virtual handler addresses
 9. interrupts::init()             — Initialize LAPIC, PIT, IO-APIC
10. syscall::init()                — Initialize syscall MSRs (STAR, LSTAR, SFMASK, EFER)
11. process::init()                — Initialize scheduler, create PID 0 (idle), PID 1/2 (kernel tasks)
12. process::spawn() × 2           — Spawn kernel tasks (task_a, task_b)
13. process::spawn_user() × 2     — Spawn user ELF processes (PID 3, PID 4)
14. process::start_scheduler()     — Enable PIT, start timer-driven context switching
```

## GDT / TSS Layout

### GDT Entries

```text
Index   Selector   Description
─────   ────────   ──────────────────────────────────────
  0      0x00      Null descriptor (required by x86 spec)
  1      0x08      Kernel code (64-bit, Ring 0, executable)
  2      0x10      Kernel data (Ring 0, writable)
  3      0x18      User code (64-bit, Ring 3, executable)
  4      0x20      User data (Ring 3, writable)
  5+6    0x28      TSS descriptor (128-bit, spans two entries)
```

### Segment Selectors

| Selector | Value | Index | RPL | Usage |
|----------|-------|-------|-----|-------|
| `kernel_code` | `0x08` | 1 | 0 | Kernel CS |
| `kernel_data` | `0x10` | 2 | 0 | Kernel DS/SS |
| `user_code` | `0x1B` | 3 | 3 | User CS (Ring 3) |
| `user_data` | `0x23` | 4 | 3 | User DS/SS (Ring 3) |
| `tss` | `0x28` | 5 | 0 | TSS (loaded by `ltr`) |

### TSS Layout

```text
Offset   Field                Description
──────   ─────                ────────────────────────────────────
  0x00   Reserved             Always 0
  0x04   RSP0                Ring 0 stack pointer (set per-process on context switch)
  0x0C   RSP1                Ring 1 stack pointer (unused)
  0x14   RSP2                Ring 2 stack pointer (unused)
  0x1C   Reserved            Always 0
  0x20   IST[0]              Double-fault stack top (16 KiB, `DOUBLE_FAULT_STACK`)
  0x28   IST[1..6]           Unused IST entries
  0x5C   Reserved            Always 0
  0x66   I/O Map Base        0 = no I/O permission bitmap
```

### TSS Address Patching

With PIC, `Descriptor::tss_segment(&TSS)` bakes the **physical** address of TSS into the GDT descriptor.
After CR3 switches to user PML4 (no identity map), the physical address is unmapped → the CPU cannot access TSS → double fault.

`gdt::switch_gdt_to_virtual()` patches the TSS descriptor's base address in-place:
- Entry 5 (TSS low, offset 40): bits 16..40 = base[0:23], bits 56..64 = base[24:31]
- Entry 6 (TSS high, offset 48): bits 0..32 = base[32:63]
- Also clears bit 41 (busy bit) in Entry 5 before re-loading TR (first `ltr` marks TSS as busy)

### GDT Initialization (Two-Phase)

```text
Phase 1 — gdt::init() (before CR3 switch)
  - Build GDT with physical TSS address
  - Load GDT (GDTR base = physical address, identity-mapped)
  - Load CS, DS, ES, FS, GS, SS, TR

Phase 2 — gdt::switch_gdt_to_virtual() (after CR3 switch)
  - Patch TSS descriptor base to kernel virtual address
  - Clear TSS busy bit (bit 41)
  - Reload GDTR with virtual address
  - Reload TR with patched TSS descriptor
```

## Interrupt Handling

### IDT Vector Layout

| Vector | Name | Type | Stack | Behavior |
|--------|------|------|-------|----------|
| 0 | #DE Division Error | Exception | IST[1] | Fatal (halt) |
| 3 | #BP Breakpoint | Exception | — | Log and continue |
| 8 | #DF Double Fault | Exception | IST[0] | Fatal with full diagnostics |
| 10 | #TS Invalid TSS | Exception | — | Fatal with error code |
| 13 | #GP General Protection | Exception | — | Fatal with error code + diagnostics |
| 14 | #PF Page Fault | Exception | — | Fatal (halt) |
| 12 | #SS Stack Segment Fault | Exception | — | Fatal with error code |
| 32 | PIT Timer | IRQ | — | Context switch (naked handler) |
| 33–47 | Hardware IRQs | IRQ | — | `dispatch::dispatch(vector)` |

### Exception Frame (CPU-Pushed)

For Ring 0 → Ring 0 (no CPL change):
```text
[RSP+0]  = error code (for exceptions that have one)
[RSP+8]  = RIP
[RSP+16] = CS
[RSP+24] = RFLAGS
```

For Ring 3 → Ring 0 (CPL change), the CPU also pushes:
```text
[RSP+32] = RSP
[RSP+40] = SS
```

### Timer Interrupt (Vector 32) — Naked Handler

The timer interrupt handler is a `#[naked]` function that performs context switching directly in assembly:

```text
1. Write 'T' to QEMU debugcon port (0xE9) — Ring 3 tick detection
2. Write [TICK] marker to serial
3. Push 15 GP registers (R15 → RAX, canonical frame)
4. call schedule(saved_rsp) → returns new SP in RAX
5. Write [SWITCH] marker to serial
6. mov r12, rax (save new SP)
7. EOI to LAPIC at 0xFFFFFFFFFEE000B0 (upper-half address)
8. mov rsp, r12 (switch to new process stack)
9. Pop 15 GP registers (RAX → R15)
10. iretq
```

### Exception Handlers

All exception handlers use the `handler_virt!` macro to convert physical handler addresses to kernel virtual addresses for IDT entries.

Double-fault handler (`idt.rs:double_fault_handler`):
- Uses IST[0] (separate 16 KiB stack) to prevent recursive stack overflow
- Prints DF frame (RIP, CS, RFLAGS, RSP, SS, CR2, CR3)
- When `DEBUG_KERNEL`: prints captured diagnostics (SAVED_RSP, RSP_AFTER_LOAD, RSP_BEFORE_IRETQ, frame origin, GDT selector values, CS cross-check)
- When `DEBUG_KERNEL`: dumps 20 qwords at RSP_BEFORE_IRETQ and 20 qwords at CAPTURED_RSP for IRET frame analysis

Page fault handler (`idt.rs:page_fault_handler`):
- Reads CR2 (faulting address), RIP, CS, RFLAGS, error code
- Prints all diagnostics to serial
- Currently halts (no user PF recovery yet — planned for Phase 5.4)

### Interrupt Dispatch

Hardware IRQs (vectors 33–47) use `extern "x86-interrupt"` handlers generated by the `irq_handler!` macro. Each calls `dispatch::dispatch(vector)`, which:
1. Calls the registered handler
2. Sends EOI to LAPIC

### LAPIC Address

Physical `0xFEE00000` is mapped to virtual `0xFFFFFFFFFEE00000` in the kernel's page table (via `vmm::init_kernel_page_tables`). The upper-half mapping is shared by all PML4s (entries 256–511), so it survives CR3 switches to user PML4s (which lack the identity map).

LAPIC EOI register: `0xFFFFFFFFFEE000B0` (physical `0xFEE000B0`).

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

## DEBUG_KERNEL — Diagnostic Gating

### Overview

`DEBUG_KERNEL` is a compiler `cfg` gate that controls verbose diagnostic output. When enabled, it adds detailed tracing to context switches, frame dumps, GDT analysis, and double-fault diagnostics. When disabled, these become no-ops.

### Configuration

Enabled in `.cargo/config.toml`:
```toml
rustflags = ["--cfg=DEBUG_KERNEL", ...]
```

To enable: add `--cfg=DEBUG_KERNEL` to rustflags.
To disable: remove it from `.cargo/config.toml`.

### What changes when enabled

| Function | When `DEBUG_KERNEL` | When disabled |
|----------|---------------------|---------------|
| `ddbg(byte)` | Writes byte to QEMU debugcon port (0xE9) | No-op |
| `dump_rax(prefix, value)` | Writes `[prefix] RAX=...` to serial | No-op |
| `write_marker_raw(ptr, len)` | Writes marker bytes to serial | No-op |
| `dump_frame_before_pop(rsp)` | Logs RSP, frame slots (RIP, CS, RFLAGS) | No-op |
| `print_iret_cpl_diagnostics(frame, gdt)` | Dumps frame + GDT descriptors | No-op |
| `schedule()` diagnostics | Logs current_pid, next_pid, FIRST DISPATCH | No-op |
| `schedule_force()` diagnostics | Logs old/new PID, IRET frame, CR3/TSS/RSP0 | No-op |
| Double-fault handler | Dumps frame origin, GDT selectors, 20 qwords at RSP_BEFORE_IRETQ | Skips diagnostics |
| `context_switch::SAVED_RSP` etc. | Statics capture context switch state | Not compiled |

### What is always-on (not gated)

| Message | Condition |
|---------|-----------|
| `[EXCEPTION]` / `FATAL` panic messages | Always printed |
| `[TICK]` / `[SWITCH]` markers | Always written (via no-ops when disabled) |
| QEMU debugcon `T` byte | Always written (no-op when disabled) |

### Adding new DEBUG_KERNEL diagnostics

To gate a new diagnostic behind DEBUG_KERNEL:

```rust
#[cfg(DEBUG_KERNEL)]
{
    crate::serial::write_str("[TAG] diagnostic message");
    crate::serial::write_hex(value);
    crate::serial::write_nl();
}
```

Or use the shorthand helpers:
```rust
crate::serial::ddbg(b'X');      // no-op when DEBUG_KERNEL is off
crate::serial::dump_rax(0x49, rax_value);  // no-op when DEBUG_KERNEL is off
```
