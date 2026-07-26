# INDOMINUS OS — Errors, Bugs, Gaps & Fixes

This document records every error, bug, gap, and problem encountered during INDOMINUS OS development, how it was found, and how it was fixed. This is a living document — append new entries at the top.

---

## 15. Phase 12 — Full Interactive Userspace & Shell

### Overview
Implemented a complete interactive shell with tokenizer, parser, builtins, pipelines, and file redirection, plus 4 new kernel syscalls and 9 userspace utility binaries.

### New Kernel Syscalls (4)
| # | Name | Purpose |
|---|------|---------|
| 18 | SYS_EXECVE | Execute ELF binary with argc/argv propagation |
| 19 | SYS_CHDIR | Change current working directory |
| 20 | SYS_GETCWD | Get current working directory |
| 21 | SYS_MKDIR | Create directory |

### Shell Features
- **Tokenizer**: Handles words, quoted strings, pipes, redirections (>, >>, <)
- **Pipeline parser**: Chains commands with `|`
- **15 builtins**: help, exit, echo, pwd, cd, clear, cat, ls, mkdir, touch, rm, pid, ps, true, false
- **Process lifecycle**: fork → execve → waitpid (blocking + WNOHANG)
- **PATH resolution**: Searches `/bin/<command>`
- **File redirection**: `>` (truncate), `>>` (append), `<` (input)
- **CWD tracking**: Shell maintains CWD, calls chdir/getcwd syscalls

### Userspace Utilities (9 binaries)
echo, cat, ls, pwd, mkdir, touch, rm, true_bin, false_bin — all no_std, no global allocator, stack-only.

### Key Changes
- `MAX_FDS` increased from 8 to 16
- `Process.cwd: [u8; 256]` added for CWD tracking
- `WaitForChild` wake reason added to scheduler
- `keyboard_wake()` rewritten as two-phase (collect then wake) to avoid borrow conflicts
- `sys_exit()` now calls `keyboard_wake()` so parent blocking waitpid can wake
- O_APPEND flag added to open()
- All `_start` signatures updated to `fn _start(argc: u64, argv: u64)`

### Testing
- 3/3 boot tests pass, 0 TFES, all 6 phases (9.4-9.9) pass
- Shell banner visible in serial output

---

## 14. AHCI PORT_IS_TFES Bit Position Fix (Phase 10C)

### Problem
The AHCI `PORT_IS` (Port Interrupt Status) register used `TFES = 1 << 0` for the Task File Error Status bit. However, bit 0 of PORT_IS is `DHRS` (Device-to-Hregister FIS Received), which fires on **every** command completion. `TFES` is actually bit 30.

This caused **every** AHCI command completion to trigger TFES error handling, producing ~94,000 false TFES events per boot. The recovery mechanism would retry commands unnecessarily, and sector reads sometimes failed on the first attempt but succeeded on retry.

### Root Cause
The original AHCI HBA register definition had the wrong bit position. Bit 0 = DHRS (D2H Register FIS), bit 30 = TFES (Task File Error Status).

### Fix
Changed `PORT_IS_TFES` from `1 << 0` to `1 << 30` in `ahci/hba.rs`.

### Impact
- Eliminated all false TFES errors (0 TFES per boot, down from ~94K)
- All sector reads succeed on first attempt
- All 6 test phases (9.4-9.9) pass reliably
- 5/5 consecutive boot tests pass with 0 panics

### Detection
Found during code audit of `ahci/hba.rs` — the bit position constant was checked against the AHCI 1.0 specification.

---

## 15. validate_elf_header for Partial Reads (Phase 10C)

### Problem
Tests T6.1 and T6.3 performed ELF validation on partial 512-byte reads of kernel/init ELF files. The existing `validate_elf()` checks that all segment data fits within the provided buffer. For a 512-byte buffer reading a 400KB+ kernel ELF, the segment data check always fails ("segment data out of ELF bounds").

### Fix
Added `validate_elf_header()` in `elf/mod.rs` — validates ELF magic, class, data encoding, type, machine, and program header structure without checking segment data bounds. Used by T6.1 and T6.3 for partial header reads.

---

## 16. Comprehensive Code Audit (Phase 10D)

### Findings
Full audit of 21+ source files. Many reported issues (pipe TOCTOU, exec address space destruction, pipe EOF, keyboard backspace) were already handled correctly in the current code:

- **Pipe EOF**: `sys_read` already returns 0 (EOF) when `!p.write_open` and buffer is empty (line 863-864)
- **Exec safety**: `sys_exec` already creates new PML4 first, loads ELF, only frees old PML4 on success (line 1342-1408)
- **Stdin/stdout**: `sys_write` to stdin returns EBADF; `sys_read` from stdout returns EBADF
- **Keyboard backspace**: Already guarded by `if e > r` check

### Documentation Drift Fixed
- `SYSCALL_ABI.md`: Updated syscall table from 12 to 16 syscalls, corrected exec return value
- `qemu_boot_test.py`: Fixed shell_banner detection (never set to True)

---

## 17. `static_mut_refs` Undefined Behavior (Phase 7.6 — UB Fix Pass)

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

## 9.3: AHCI Storage Driver

| Issue | Severity | Phase | Resolution |
|---|---|---|---|
| Wrong PCI BAR index (`bars[4]` instead of `bars[5]`) | CRITICAL | 9.3 | **Fixed**: AHCI ABAR is at BAR5 (offset 0x24), not BAR4. Changed to `pci.bar_address(5)` |
| MmioRegion stored physical address, used as virtual | HIGH | 9.3 | **Fixed**: `MmioRegion::new()` now stores the virtual address from `map_mmio_page()` instead of the physical address |
| AHCI PCI prog_if filter too narrow (0x01 only) | MEDIUM | 9.3 | **Fixed**: Accept prog_if 0x01 (AHCI 1.0) or 0x02 (AHCI 1.3) |
| Port signature check unreliable after HBA reset | HIGH | 9.3 | **Fixed**: Use SSTS.DET (device detection status) instead of PORT_SIG to find active ports |
| HbaCmdHeader struct layout wrong (16 bytes, extra `prdta_len` field) | CRITICAL | 9.3 | **Fixed**: Restructured to 32 bytes matching AHCI spec. PRDTL is bits 16-31 of DW0 (`opts`), not a separate field |
| PRDT byte count off-by-one in IDENTIFY DEVICE | MEDIUM | 9.3 | **Fixed**: Changed `set_byte_count(511)` to `set_byte_count(512)` since `set_byte_count` subtracts 1 internally |
| DMA page allocation used NO_CACHE flags for RAM | MEDIUM | 9.3 | **Fixed**: DMA buffers from PMM are identity-mapped; removed incorrect NO_CACHE/WRITE_THROUGH page table flags |
| CAP register read as u64 (includes GHC in high bits) | LOW | 9.3 | **Fixed**: Changed to `read_reg::<u32>()` to read only the 32-bit CAP register |
| Port init missing FRE/FR/CR wait sequence | MEDIUM | 9.3 | **Fixed**: Added wait for FR (FIS Receive Running) after FRE, and wait for CR (Command List Running) after ST |
| Block device test assumed ramdisk at ID 0 | MEDIUM | 9.3 | **Fixed**: Test now uses actual `dev_id` from `register_device()` instead of hardcoded 0 |

---

## 9.4: DMA Probe False Negative + FAT16 End-to-End Verification

| Issue | Severity | Phase | Resolution |
|---|---|---|---|
| AHCI `&&` DMA probe check produced false negatives | CRITICAL | 9.4 | **Fixed**: Removed DMA buffer content comparison from the authoritative success path. Command success is now determined solely by AHCI/ATA status (CI cleared, no TFES, TFD.ERR == 0, TFD.DF == 0). DMA probe kept as diagnostic-only. |
| FAT16 open() failed for multi-cluster files (kernel.elf) | CRITICAL | 9.4 | **Root cause**: The `&&` probe check at LBA 0x527 (kernel.elf cluster 0x2E, last sector) had byte 3 (0xEF) coincidentally matching the probe pattern byte. `&&` requires ALL 4 bytes to differ → false PROBE_FAIL → command retried 8 times then failed. **Fix**: Same as above — AHCI status-based completion. |
| QEMU `fat:rw:` creates MBR-partitioned FAT16 | MEDIUM | 9.4 | **Documented**: QEMU's `fat:rw:` directive creates an MBR partition table with type 0x06 at LBA 0x3F, not a bare FAT filesystem. FAT driver must parse MBR to find partition start. |
| FAT16 root directory size limited to 512 entries | LOW | 9.4 | **Documented**: FAT16 root directory is fixed-size (512 entries in our QEMU image). Subdirectories beyond this limit are handled correctly via cluster chains. |

---

## 10. Lessons Learned

1. **Never assume `static mut` is safe.** Even single-threaded code has UB with `static mut` references in Rust 1.77+.
2. **`SyncUnsafeCell` requires strict discipline.** Every access must be justified by an interrupt-disable or lock.
3. **Pointer arithmetic on `SyncUnsafeCell::get()`** returns a pointer to the whole array, not element 0. Use `.add()` or cast properly.
4. **Dead code accumulates silently.** Regular cleanup passes are essential.
5. **Security audits catch real bugs.** The Foundation Hardening phase found 10 real vulnerabilities.
6. **Regression tests are non-negotiable.** Automated boot tests catch regressions that compilation alone cannot.
7. **DMA buffer content is never a valid success criterion.** On x86-64, the HBA snoops the CPU cache (MESI protocol). DMA transfers overwrite buffers atomically. Requiring buffer content to differ from a sentinel is a heuristic that can produce false negatives when real disk data coincidentally contains the sentinel byte. Always use AHCI/ATA completion status (PxCI clear + PxIS.TFES clear + PxTFD.ERR/DF clear) as the authoritative success test.
8. **Cumulative kernel tests exhaust heap memory.** Each test phase allocates Vec buffers for file reads, FAT metadata, and process structures. When all phases run in sequence, a 4 MiB heap is insufficient. Increasing to 16 MiB resolved the OOM panics.

---

## 11. Phase 9.5-9.9: Integration Test Phases

### 9.5: FD + Syscall Integration (14/14 pass)
Tests T5.1-T5.14: open, read, multi-cluster, EOF, error handling, close/reopen, independent offsets, dup2 shared offset, fork FD inheritance, CLOEXEC. Committed at `6daf951`.

### 9.6: ELF Loading from Persistent Filesystem (7/7 pass)
Tests T6.1-T6.7: FAT→VFS→validate pipeline, non-ELF rejection, truncated ELF, bad magic, non-executable type. Committed at `b1a986d`.

### 9.7: User-Space Shell Infrastructure (6/6 pass)
Tests T7.1-T7.6: shell binary valid ELF in VFS, loadable by spawn_user, FAT file read (cat), FAT directory listing (ls), exec path not found, shell size sanity. Shell source updated to v0.3 with cat/ls/exec/pid commands. Committed at `ed6f4ad`.

### 9.8: Init Process PID 1 (7/7 pass)
Tests T8.1-T8.7: PID 1 exists, init binary in initrd, init/shell ELF validation, process count. Committed at `71ab934`.

### 9.9: FAT Persistence + Regression Test Matrix (6/6 pass)
Tests T9.1-T9.6: FAT16 mountable and consistent, startup.nsh readable ASCII, BOOTX64.EFI MZ header, file size consistent, root dir structure, FAT read-only limitation documented. Committed at `3231882`.

### Cumulative Test Count: 50/50 (all pass)

---

## 12. Kernel Heap Increase (4 MiB → 16 MiB)

| Phase | Issue | Severity | Resolution |
|---|---|---|---|
| 9.9 | OOM panic during FAT32 init when running all phases sequentially | HIGH | Increased `KERNEL_HEAP_INITIAL_SIZE` from 4 MiB to 16 MiB. The cumulative test allocations (Vec buffers, FAT metadata, process structures) exhausted the 4 MiB heap. 16 MiB provides headroom for all test phases plus future subsystems. |

### Why 16 MiB
- 4 MiB was insufficient for cumulative test runs (50 tests across 5 phases)
- Each `read_file()` call allocates a `Vec<u8>` on the heap (kernel.elf = 512KB alone)
- FAT metadata, VFS structures, and process tables add overhead
- 16 MiB is well within the 256 MiB QEMU RAM budget
- VMM maps exactly `heap_pages = 16 MiB / 4 KiB = 4096` frames from PMM

### Files Modified
- `indo-kernel/src/memory/mod.rs`: `KERNEL_HEAP_INITIAL_SIZE` constant (4→16 MiB)
- `docs/architecture.md`: Updated virtual address map, constants table, boot sequence

---

## 13. Known Limitations (Phase 9.x → Phase 11 updated)

| Limitation | Severity | Notes |
|---|---|---|
| ~~FAT filesystem is READ-ONLY~~ | ~~HIGH~~ | ✅ RESOLVED — Phase 11 adds full write support: create, write, truncate, delete files/directories. |
| LFN write support | MEDIUM | Writes limited to 8.3 filenames only. Long filenames return `VfsError::BadPath`. |
| FAT16 root directory fixed size | LOW | Cannot grow FAT16 root directory beyond allocated sectors. Returns NoSpace when full. |
| OOM on full phase sequence without 16 MiB heap | MEDIUM | Resolved by increasing heap to 16 MiB. |
| Userspace shell binary is v0.1 | LOW | Shell v0.3 source exists but Windows toolchain cannot compile it. |
| AHCI TFES errors are intermittent | LOW | Certain LBA reads produce TFES. Recovery mechanism handles this. |
| Shell boots to v0.2 prompt | LOW | Shell ELF on FAT disk is the pre-built v0.1 binary. |

---

## 14. Phase 11: FAT Write Support

### Overview
Full read/write support for FAT16/FAT32 filesystems. Enables userspace processes to create, write, and delete files.

### Key Design Decisions
- **Crash-safe flush ordering**: Data sectors written first, directory entry size updated last (minimizes corruption window)
- **Lazy allocation**: Clusters allocated on `close()`, not on each `write()` call (reduces metadata thrashing)
- **Drop impl on FileHandle**: Auto-flushes dirty data on process exit (prevents data loss)
- **FAT entry mirroring**: All FAT copies updated for consistency
- **Sector read-modify-write**: Required for sub-sector-sized writes

### Files Modified
- `fat32.rs`: Rewrite from read-only to read/write (~1750 lines)
- `syscall/mod.rs`: Added `sys_unlink(16)`, `sys_brk(17)`, O_TRUNC support
- `process/process.rs`: Added `heap_start`, `heap_end` fields
- `vfs/mod.rs`: `create_file()` resolves through mount points
- `userspace/syscall/src/lib.rs`: Added `unlink()`, `brk()` wrappers
- `userspace/test_fat_write/`: New test binary (8 tests)
