# FORTRESS AUDIT v2 - INDOMINUS OS

## First-Pass Fixes (All Verified ✓)

| ID | Issue | Fix | Status |
|----|-------|-----|--------|
| CRIT-01 | PIT TICK_COUNT `static mut` | → `AtomicU64` | ✓ Fixed (pit.rs:46) |
| CRIT-02 | DEFERRED_COUNT/SLOTS `static mut` | → `AtomicUsize` + `spin::Mutex` | ✓ Fixed (context_switch.rs:74-76) |
| CRIT-03 | INIT_PID `static mut` | → `AtomicU64` | ✓ Fixed (scheduler.rs:30) |
| CRIT-04 | PER_CPU `static mut` | Documented safe for single-CPU | ✓ Fixed (syscall/mod.rs:193) |
| HIGH-01 | keyboard_wake() Vec heap alloc | → `[usize; MAX_PROCESSES]` fixed array | ✓ Fixed (mod.rs:184) |
| HIGH-02+03 | send_signal_to_fg TOCTOU + no parent wake | Inlined wake + parent wake under lock | ✓ Fixed (mod.rs:294+) |
| HIGH-04 | Pipe refcount underflow | → `fetch_update` with zero check | ✓ Fixed (process.rs:349) |
| HIGH-05+06 | drain_deferred_free PML4 leak | Now calls `free_user_address_space` | ✓ Fixed (context_switch.rs:100+) |
| HIGH-09 | Signal delivery no RSP validation | Validates `orig_rsp_val` against `USER_ADDR_MAX` | ✓ Fixed (scheduler.rs:639-659) |
| HIGH-10 | exec CLOEXEC pipe close | `fetch_update` refcount in exec paths | ✓ Fixed (syscall/mod.rs) |
| MED-02 | current_stack_pointer dangling | Detects empty slot, clears current_pid | ✓ Fixed (scheduler.rs:184-201) |
| MED-03 | No per-process pipe count limit | `pipe_count: u8` + `MAX_PIPES_PER_PROCESS=8` | ✓ Fixed (process.rs:160-167) |
| MED-04 | Signal mask not checked in send_signal_to_fg | Checks `signals_blocked` bit | ✓ Fixed (mod.rs:334) |
| MED-05 | Pipe close doesn't wake blocked processes | keyboard_wake wakes writers on broken pipe | ✓ Fixed (mod.rs keyboard_wake) |

## NEW Issues Found in Re-Audit

### ~~NEW-MED-01~~: keyboard.rs line discipline — NOT AN ISSUE
- **File**: `keyboard.rs`
- **Finding**: `pop_line_bytes()` referenced in comments does NOT exist. The actual function `read_line()` (line 190) reads directly from static `LINE_BUF` (4096 bytes) — no heap allocation. **No fix needed.**

### NEW-MED-02: PCI bus enumeration scans all 256 buses
- **File**: `pci/mod.rs:236`
- **Issue**: `enumerate()` iterates bus 0..=255, device 0..31, function 0..7. On real hardware and QEMU, all devices are on bus 0. Scanning 256 buses × 32 devices × I/O port reads takes several seconds.
- **Severity**: MEDIUM — Boot time degradation.
- **Fix**: Scan bus 0 only, or stop after 32 consecutive empty buses on the current bus.

### NEW-MED-03: PMM REFCOUNTS u8 overflow silently capped
- **File**: `memory/pmm.rs:246`
- **Issue**: `incref()` caps at 255 (`if refcount < 255 { refcount += 1 }`). If a frame is shared by 256+ processes, additional forks silently don't increment the count, leading to premature freeing.
- **Severity**: MEDIUM — Only matters with extreme fork nesting (>255 shares per frame), unlikely but possible.
- **Fix**: Use `saturating_add` or `u16` for refcounts.

### NEW-MED-04: Deferred-free slots can leak PML4 if full
- **File**: `process/context_switch.rs:83-93`
- **Issue**: `defer_free()` only has MAX_DEFERRED=4 slots. If all 4 are occupied (multiple rapid process exits), the 5th exit's PML4 is silently leaked. The code just returns without freeing.
- **Severity**: MEDIUM — PML4 is one 4KiB frame per leaked exit. With rapid exits, physical memory leaks.
- **Fix**: If slots are full, force synchronous free (switch to kernel PML4, call `free_user_address_space`, free kstack directly).

### NEW-LOW-01: aslr.rs has unused AtomicU64 import
- **File**: `aslr.rs:7`
- **Issue**: `use core::sync::atomic::{AtomicU64, Ordering}` — `AtomicU64` is not used (was used before CRIT-01 fix moved it to pit.rs).
- **Severity**: LOW — Compiler warning only.
- **Fix**: Remove unused import.

### NEW-LOW-02: ELF loader allows non-contiguous overlapping segments
- **File**: `elf/mod.rs:296-360`
- **Issue**: The ELF loader doesn't check if two PT_LOAD segments map overlapping virtual address ranges. While rare in practice, a malicious ELF could exploit this to overwrite memory.
- **Severity**: LOW — The cumulative size check prevents total memory exhaustion.
- **Fix**: Track mapped ranges and reject overlaps.

## Remaining Open Issues (Deferred)

| ID | Issue | Reason Deferred |
|----|-------|-----------------|
| HIGH-07 | No FPU/SSE context save/restore | Complex — needs 512-byte save area per process |
| HIGH-08 | No kernel stack guard pages | Needs VMM integration |
| MED-06 | O_APPEND non-atomic | Single-CPU, acceptable |
| MED-07 | INIT_PID hardcoded to 3 | Works correctly, not a bug |
| MED-08 | Low ASLR entropy | Limitation of current virtual address space |
| MED-09 | No RLIMIT enforcement | Feature, not a bug |
| MED-10 | Fixed-size process table | Acceptable for Phase 1 |
| LOW-01 through LOW-06 | Various low-priority items | Deferred |

## Architecture Summary

```
┌─────────────────────────────────────────────────────┐
│                    Userspace                         │
│  Shell (PID 3) │ init (PID 1) │ stress tests        │
├─────────────────────────────────────────────────────┤
│                  Syscall Layer (30 syscalls)         │
│  SYSCALL MSR → LSTAR → syscall_handler              │
│  STAR for segments, SFMASK clears IF                 │
├─────────────────────────────────────────────────────┤
│              Process Management (32 max)             │
│  Scheduler: round-robin, force/preemptive switch    │
│  Context: naked asm, 15 GP regs + stack swap         │
│  Deferred free: AtomicUsize + spin::Mutex slots      │
├─────────────────────────────────────────────────────┤
│               Memory Management                      │
│  PMM: bitmap allocator (16 GiB max)                 │
│  VMM: 4-level x86_64, CoW fork, user PML4           │
│  Heap: linked_list_allocator, 16 MiB                 │
├─────────────────────────────────────────────────────┤
│              Interrupts & Timing                      │
│  IDT: 256 entries, IST for DF/NMI                   │
│  LAPIC: MMIO in upper half (shared via PML4 256+)   │
│  PIT: 100 Hz, AtomicU64 tick counter                │
│  Keyboard: PS/2 IRQ1, line discipline (no heap)     │
├─────────────────────────────────────────────────────┤
│              Storage & Filesystem                     │
│  BlockDevice trait → AHCI / RamDisk                  │
│  VFS: ramfs (in-memory)                             │
│  FAT32: directory tree, file create/delete/grow      │
│  Initrd: cpio newc parser → VFS                     │
├─────────────────────────────────────────────────────┤
│              Hardware Abstraction                     │
│  PCI: legacy I/O port enumeration                   │
│  AHCI: HBA init, port I/O, DMA, TFES recovery      │
│  ACPI: RSDP scan, MADT parse (LAPIC/IOAPIC info)    │
│  MMIO: upper-half mapping (0xFFFF_FFFF_0000_0000|x) │
└─────────────────────────────────────────────────────┘
```
