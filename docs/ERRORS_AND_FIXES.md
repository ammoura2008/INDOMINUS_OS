# INDOMINUS OS — Errors, Bugs, Gaps & Fixes

This document records every error, bug, gap, and problem encountered during INDOMINUS OS development, how it was found, and how it was fixed. This is a living document — append new entries at the top.

---

## 1. `static_mut_refs` Undefined Behavior (Phase 7.6 — UB Fix Pass)

### Problem
Rust 1.77+ emits `static_mut_refs` warnings for any code that takes a reference (`&` or `&mut`) to a `static mut` variable. These references are **undefined behavior** because:
- The compiler may assume the reference is exclusive (`&mut`) or shared (`&`) but there's no synchronization.
- Multiple references can coexist, creating aliasing violations.
- On x86_64, this can cause the compiler to cache values in registers, skip re-reads, or reorder stores around the reference.

### Scope
18 instances across 10 files. Every `static mut` in the kernel was affected.

### Files Fixed

| File | Variable(s) | Old Type | New Type |
|------|-------------|----------|----------|
| `cpu.rs` | `CPU_FEATURES` | `static mut CpuFeatures` | `static SyncUnsafeCell<CpuFeatures>` |
| `gdt.rs` | `TSS` | `static mut TaskStateSegment` | `static SyncUnsafeCell<TaskStateSegment>` |
| `idt.rs` | `IDT`, `IDT_INITIALIZED` | `static mut Idt`, `static mut bool` | `static SyncUnsafeCell<Idt>`, `static SyncUnsafeCell<bool>` |
| `ioapic.rs` | `IOAPIC` | `static mut Option<MmioRegion>` | `static SyncUnsafeCell<Option<MmioRegion>>` |
| `lapic.rs` | `LAPIC` | `static mut Option<MmioRegion>` | `static SyncUnsafeCell<Option<MmioRegion>>` |
| `keyboard.rs` | `KBD_BUF`, `LINE_BUF` | `static mut [u8; 256]` | `static SyncUnsafeCell<[u8; 256]>` |
| `pmm.rs` | `BITMAP`, `REFCOUNTS`, `TOTAL_FRAMES`, `FREE_FRAMES` | `static mut Vec`, `static mut u64` | `static SyncUnsafeCell<Vec>`, `static SyncUnsafeCell<u64>` |
| `vfs/mod.rs` | `VFS` | `static mut Option<Vfs>` | `static SyncUnsafeCell<Option<Vfs>>` |
| `acpi/mod.rs` | `ACPI_STATE` | `static mut Option<AcpiState>` | `static SyncUnsafeCell<Option<AcpiState>>` |

### Solution: `SyncUnsafeCell<T>`
Created `sync_cell.rs` with a custom `SyncUnsafeCell<T>` wrapper around `UnsafeCell<T>` that implements `Sync`.

```rust
pub struct SyncUnsafeCell<T>(UnsafeCell<T>);

unsafe impl<T> Sync for SyncUnsafeCell<T> {}

impl<T> SyncUnsafeCell<T> {
    pub const fn new(val: T) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut T {
        self.0.get()
    }
}
```

**Safety contract:** All accesses to `SyncUnsafeCell` globals must be protected by:
- Disabling interrupts (for single-CPU globals), OR
- Holding a spinlock (for SMP globals), OR
- Being INIT_ONLY (written once at boot, read-only thereafter).

### Access Pattern Conversions

**Before (UB):**
```rust
static mut KBD_BUF: [u8; 256] = [0u8; 256];
// Usage:
KBD_BUF[head] = scancode;  // UB: implicit reference to static mut
```

**After (safe):**
```rust
static KBD_BUF: SyncUnsafeCell<[u8; 256]> = SyncUnsafeCell::new([0u8; 256]);
// Usage:
unsafe { (*KBD_BUF.get())[head] = scancode; }  // OK: raw pointer, no reference
```

### Pointer Arithmetic Fix (keyboard.rs)
The `SyncUnsafeCell::get()` returns a `*mut T`. For arrays, this is a pointer to the whole array, not element 0. Must add `.add(i)` to get element `i`.

**Before (UB — wrong pointer arithmetic):**
```rust
let p = KBD_BUF.get() as *mut u8;
*p.add(head) = scancode;  // WRONG: KBD_BUF.get() is *[u8; 256], not *u8
```

**After (correct):**
```rust
let p = (*KBD_BUF.get()).as_mut_ptr();
*p.add(head) = scancode;  // OK: as_mut_ptr() returns *mut u8 (element 0)
```

### Safety Audit Results (15 globals)

| Global | File | Access Pattern | SMP | Safety Justification |
|--------|------|----------------|-----|---------------------|
| `CPU_FEATURES` | cpu.rs | INIT_ONLY | INIT_ONLY | Written once during `detect()`, read-only thereafter |
| `TSS` | gdt.rs | INIT_ONLY | INIT_ONLY | Written once during `init()`, accessed via fixed address |
| `IDT` | idt.rs | INIT_ONLY | INIT_ONLY | Written once during `init()`, read-only thereafter |
| `IDT_INITIALIZED` | idt.rs | INIT_ONLY | INIT_ONLY | Set once to `true`, never cleared |
| `IOAPIC` | ioapic.rs | INTERRUPT_ACCESSED | INIT_ONLY | Written once during `init()`, read via interrupt handler |
| `LAPIC` | lapic.rs | INTERRUPT_ACCESSED | INIT_ONLY | Written once during `init()`, read via interrupt handler |
| `KBD_BUF` | keyboard.rs | INTERRUPT_ACCESSED | INTERRUPT_ACCESSED | All accesses with interrupts disabled |
| `LINE_BUF` | keyboard.rs | INTERRUPT_ACCESSED | INTERRUPT_ACCESSED | All accesses with interrupts disabled |
| `BITMAP` | pmm.rs | LOCK_REQUIRED | LOCK_REQUIRED | All accesses hold `PMM_LOCK` spinlock |
| `REFCOUNTS` | pmm.rs | LOCK_REQUIRED | LOCK_REQUIRED | All accesses hold `PMM_LOCK` spinlock |
| `TOTAL_FRAMES` | pmm.rs | INIT_ONLY | INIT_ONLY | Written once during `init()`, read-only thereafter |
| `FREE_FRAMES` | pmm.rs | LOCK_REQUIRED | LOCK_REQUIRED | Modified during alloc/free, protected by `PMM_LOCK` |
| `VFS` | vfs/mod.rs | LOCK_REQUIRED | INIT_ONLY | Written once during `init()`, access via `vfs()` accessor |
| `ACPI_STATE` | acpi/mod.rs | INIT_ONLY | INIT_ONLY | Written once during `init()`, read-only thereafter |
| `CAPTURED_RSP` | main.rs | INTERRUPT_ACCESSED | INTERRUPT_ACCESSED | Written in DF handler (interrupts disabled) |

### No Mutable Aliasing Confirmed
All `SyncUnsafeCell` globals are accessed through a single path at any given time:
- INIT_ONLY globals: written once, then only read.
- LOCK_REQUIRED globals: protected by a single spinlock.
- INTERRUPT_ACCESSED globals: protected by interrupt disable.

---

## 2. Dead Code Cleanup (38 items removed)

### Problem
After the UB fix pass, 38 dead code items remained — unused functions, fields, and imports left over from earlier development phases.

### Items Removed

**serial.rs (8 items):**
- `init()` function (UART already initialized by bootloader)
- `UART_*` constants (0x3F8, 0x2F8, 0x3E8, 0x2E8)
- `PORTS` array
- `port_offset()` function

**process/ (12 items):**
- `process.rs`: `entry_addr` field
- `mod.rs`: `spawn()` wrapper function
- `pipe.rs`: `pipe_read()`, `pipe_write()`
- `tasks.rs`: `task_a()`, `task_b()`
- `scheduler.rs`: `spawn()`, `get_entry_addr()`, `find_zombie_child()`, `live_child_count()`
- `context_switch.rs`: `SAVED_RSP_FOR_DIAG`, `OLD_SP_FOR_DIAG`, `OLD_PID_FOR_DIAG`, `NEW_PID_FOR_DIAG`

**memory/ (13 items):**
- `mod.rs`: `KERNEL_STACK_TOP`, `KERNEL_STACK_SIZE`, `USER_CODE_BASE`, `USER_KERNEL_STACK_SIZE`, `USER_SPACE_END`, `USER_HEAP_BASE`, `USER_HEAP_INITIAL_SIZE`, `alloc_per_process_kernel_stack()`
- `vmm.rs`: `unmap_page()`, `virt_to_phys()`

**pci/mod.rs (5 items):**
- `STATUS` constant
- `find_device()`, `find_by_class()`
- `enable_mmio()`, `enable_bus_master()`, `enable_pio()`

**Other (5 items):**
- `gdt.rs`: `get_tss_rsp0()`
- `interrupts/lapic.rs`: `LAPIC_CURRENT_COUNT`, `LVT_TIMER_MASK`, `mask_lapic_timer()`
- `interrupts/dispatch.rs`: `is_hardware_irq()`
- `interrupts/pit.rs`: `sleep_ms()`
- `syscall/mod.rs`: `get_kernel_rsp()`, `SegmentSelector` import, unused `'outer:` label

---

## 3. `asm_sub_register` Warning Fix

### Problem
In `idt.rs`, the `asm_sub_register` lint warned about using `u16` values in inline assembly operands for `cs` and `ss` segment registers.

### Fix
Changed `cs` and `ss` fields in the `IdtStackFrame` from `u16` to `u64` in the inline assembly push sequence.

---

## 4. Unnecessary `unsafe` Block

### Problem
In `main.rs`, the call to `harden_identity_map()` was wrapped in an `unsafe` block, but the function is already `pub unsafe fn`.

### Fix
Removed the unnecessary outer `unsafe` block.

---

## 5. Unused Import and Variable Warnings

### Problem
Multiple files had unused imports (`use` statements) and unused variables (prefixed with `_` but still generating warnings).

### Files Fixed
- `cpu.rs`: removed `lahf_lm` function
- `syscall/mod.rs`: removed `SegmentSelector` import, removed unused `'outer:` label
- `process/context_switch.rs`: removed diagnostic statics when `DEBUG_KERNEL` is off

---

## 6. Foundation Hardening Bugs (Phase 8 — Security Audit)

### 6.1 ELF Kernel Mapping Bypass
**File:** `elf/mod.rs`
**Severity:** CRITICAL
**Problem:** ELF segments near `0x800000000000` could cross into kernel space after alignment, allowing user code to map into kernel memory.
**Fix:** Added `virt_end` validation after alignment to ensure segments stay below `USER_SPACE_END`.

### 6.2 sys_exec Use-After-Free
**File:** `syscall/mod.rs`
**Severity:** CRITICAL
**Problem:** `sys_exec` freed the old PML4 before loading the new ELF. If the ELF load failed, the process was left with no address space.
**Fix:** Create new PML4 first, load ELF into it, only free old PML4 on success.

### 6.3 alloc_contiguous Frame 0
**File:** `pmm.rs`
**Severity:** HIGH
**Problem:** The contiguous allocator could return frame 0 (BIOS IVT/BDA), which must never be allocated.
**Fix:** Skip frame 0 in the contiguous allocator scan.

### 6.4 Process Drop Double-Free
**File:** `context_switch.rs`
**Severity:** HIGH
**Problem:** `force_switch` zeroed resources for ALL old processes, including yielded ones, causing double-frees when the yielded process was later scheduled again.
**Fix:** Gated resource cleanup on `dead_kstack != 0` (only for processes that actually exited).

### 6.5 Guard Page User-Accessible
**File:** `syscall/mod.rs`
**Severity:** HIGH
**Problem:** The guard page in `execve` was mapped with `USER_ACCESSIBLE` flag, allowing user code to write to it.
**Fix:** Removed `USER_ACCESSIBLE` from guard page flags.

### 6.6 alloc_contiguous REFCOUNTS
**File:** `pmm.rs`
**Severity:** MEDIUM
**Problem:** Contiguous frames allocated by `alloc_contiguous` had refcount 0, meaning `free_frame` would underflow.
**Fix:** Set `REFCOUNTS[frame] = 1` for each contiguous frame allocated.

### 6.7 free_frame Frame 0
**File:** `pmm.rs`
**Severity:** MEDIUM
**Problem:** No check prevented freeing frame 0, which could corrupt BIOS structures.
**Fix:** Added `assert!(frame != 0, "PMM: cannot free frame 0")`.

### 6.8 Process Drop Address Space Leak
**File:** `process.rs`
**Severity:** MEDIUM
**Problem:** Reaped zombie processes never freed their PML4 or user pages, leaking physical memory.
**Fix:** `Drop` implementation now calls `free_user_address_space()` for non-kernel processes.

### 6.9 sys_dup Use-After-Free
**File:** `syscall/mod.rs`
**Severity:** MEDIUM
**Problem:** `sys_dup` for `FsFile` didn't clone the file handle, creating aliased references.
**Fix:** Rejected `FsFile` dup with `EBADF` until Arc-based handles are implemented.

### 6.10 sys_pipe FD Exhaustion Leak
**File:** `syscall/mod.rs`
**Severity:** LOW
**Problem:** If FD allocation failed after creating a pipe, the pipe was never freed.
**Fix:** Added `free_pipe` on the error path.

---

## 7. False Positives Confirmed

### 7.1 decref Without VMM Unmap
Both call sites (`free_user_address_space`, CoW) properly destroy PTEs via page table frame freeing.

### 7.2 Scheduler Lock Ordering
All acquisitions happen with interrupts disabled. Single lock, no deadlock possible.

### 7.3 kill_process From Page Fault
Runs with `IF=0` (interrupt gate). No preemption during cleanup.

---

## 8. Regression Test Results

### Build Verification
- `cargo build --release`: **CLEAN** (0 errors, warnings limited to intentionally-kept API_NEEDED items)
- `verify_kernel.py`: **PASS** (ELF magic, 64-bit, entry point, PT_LOAD)
- Kernel binary size: **281.8 KB** (288,592 bytes)
- Entry point: `0xFFFFFFFF80001000` (in kernel range)

### Boot Verification (QEMU)
- All `[MARK]` initialization markers printed in order
- Shell binary found and spawned as PID 2
- 10 test binaries spawned as PID 3–12
- `[TICK]` and `[SWITCH]` markers appearing (context switching working)
- No triple faults, no page faults, no panics
- System stable and running indefinitely

### Warning Count
- Before: 146 warnings
- After: 41 warnings (all intentionally-kept `API_NEEDED` and `DEBUG_TOOL` items)
- `indo-core` target: 2 warnings (outside kernel scope)

---

## 9. Known Gaps (Not Yet Fixed)

| Gap | Severity | Phase | Notes |
|-----|----------|-------|-------|
| Orphan processes never reaped | HIGH | Phase 8 | Needs init/reaper (now implemented as PID 1) |
| PID reuse allows cross-family reaping | HIGH | Phase 8 | Needs PID generation counter |
| sys_dup cannot handle FsFile | MEDIUM | Phase 8 | Needs `Arc<dyn File>` ref counting |
| sys_close doesn't free pipe slots | MEDIUM | Phase 8 | Needs ref-counted pipes |
| No kernel stack guard page | LOW | Phase 9+ | Heap overflow risk |
| No SMP support | LOW | Phase 12+ | Single-CPU only; all globals unsynchronized |
| REFCOUNTS overflow silent clamp at 255 | LOW | Phase 9+ | Theoretical only |

### Phase 9.2 Issues (Resolved)

| Issue | Severity | Phase | Status |
|-------|----------|-------|--------|
| sys_open had no flags parameter | MEDIUM | 9.2 | **Fixed**: Added flags arg (O_RDONLY/O_WRONLY/O_RDWR/O_CREAT/O_TRUNC) |
| sys_exec didn't close FDs before loading | HIGH | 9.2 | **Fixed**: Closes FDs with O_CLOEXEC flag, inherits others |
| Userspace lacked dup2/readdir wrappers | LOW | 9.2 | **Fixed**: Added dup2() and readdir() wrappers |
| VFS had no end-to-end file I/O test | LOW | 9.2 | **Fixed**: Added phase92_vfs_file_test in main.rs |
| File descriptor model incomplete | MEDIUM | 9.2 | **Fixed**: Added FdType::File with Arc<Mutex<Box<dyn File>>> + ref_count |
| exec() unconditionally closed FDs 3+ | HIGH | 9.2b | **Fixed**: Added O_CLOEXEC flag per FD; exec only closes flagged FDs |
| dup/dup2 didn't clear close-on-exec | MEDIUM | 9.2b | **Fixed**: dup/dup2 always clear O_CLOEXEC on new FD |
| fd_flags not cleaned up on close | LOW | 9.2b | **Fixed**: sys_close clears fd_flags[fd] |

---

## 10. Lessons Learned

1. **Never assume `static mut` is safe.** Even single-threaded code has UB with `static mut` references in Rust 1.77+.
2. **`SyncUnsafeCell` requires strict discipline.** Every access must be justified by an interrupt-disable or lock.
3. **Pointer arithmetic on `SyncUnsafeCell::get()`** returns a pointer to the whole array, not element 0. Use `.add()` or cast properly.
4. **Dead code accumulates silently.** Regular cleanup passes are essential.
5. **Security audits catch real bugs.** The Foundation Hardening phase found 10 real vulnerabilities.
6. **Regression tests are non-negotiable.** Automated boot tests catch regressions that compilation alone cannot.
