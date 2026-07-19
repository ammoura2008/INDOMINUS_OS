# Architecture Foundation Analysis — Phase 5.4 Checkpoint

Date: 2026-07-19
Commit: `4224787` (Phase 5.4 Layer 1 — page fault classification + user process kill)
Tag: `v0.5.4-page-faults-layer1`

This document captures the architectural analysis at this checkpoint to guide future decisions.

---

## Decision: What Foundation Is Needed NOW vs What Can Wait

### FOUNDATIONS NEEDED NOW (small changes, hard to retrofit)

These are minimal changes that preserve future options without adding complexity.

| # | Foundation | Risk of Adding Now | Cost of Delay | LOC Estimate |
|---|-----------|-------------------|---------------|-------------|
| 1 | NX bit on user data/stack/heap pages | Low | HIGH — retroactive ELF loader + VMM changes | ~20 |
| 2 | SMEP in CR4 | Very low | Medium — must audit all kernel code paths | ~5 |
| 3 | `Blocked` state in ProcessState | Very low | HIGH — scheduler refactor needed later | ~10 |
| 4 | Increase MAX_PROCESSES to 32+ | Low | Medium — blocks real multi-process usage | ~15 |
| 5 | Formalize syscall error convention | Very low | CRITICAL — breaking user-space ABI is extremely costly | ~10 (doc + code) |
| 6 | Heap growth mechanism | Low | HIGH — 4 MiB ceiling becomes hard wall | ~30 |
| 7 | Guard page below user stack | Very low | Medium — stack overflow becomes silent corruption | ~5 |
| 8 | Refactor LAPIC EOI to single Rust function | Low | Medium — maintenance hazard in 3 naked asm sites | ~20 |

### SAFE TO DEFER

| Area | Why It Can Wait |
|------|----------------|
| Demand paging / CoW | Kernel memory model is stable. Adding later doesn't require redesign. |
| SMP-safe PMM | No SMP planned for Phase 5+. Single CPU design is correct for now. |
| File descriptor tables | Simple field addition to Process struct. No architectural blocker. |
| Signals | Requires Blocked state + fd table. Both are small additions. |
| PCI enumeration / device tree | Not needed until real hardware drivers. Current direct MMIO works. |
| DMA safety / IOMMU | Only needed for disk/network drivers. Far future. |
| ASLR | Requires random source + ELF loader changes. Can add after NX. |
| SMAP | Requires `stac`/`clac` around every user-memory access. High audit cost. |
| Driver abstraction layer | Current direct MMIO is fine for boot devices. Premature abstraction. |
| VDSO | Only needed for high-frequency syscalls. No such syscalls yet. |
| Capabilities / privilege separation | All processes are unprivileged user programs. Sufficient for now. |
| Stack canaries | Requires compiler support + canary source. Not needed yet. |

---

## Area-by-Area Analysis

### 1. Process Isolation / Memory Protection

**Current state:** PML4 isolation works. Ring 0/3 enforced. Identity map NOT copied to user PML4s. User stack is a single 4 KiB page with no guard.

**Key finding:** The isolation model is sound. The identity map is correctly excluded from user PML4s. The only gap is the user stack (no guard page, no growth).

**Action needed now:**
- Add guard page below user stack (map the page below `USER_STACK_TOP - PAGE_SIZE` as non-present).
- Remove identity map dependency in PER_CPU GS base (use higher-half virtual address).

**Can wait:** Demand paging, CoW, per-process heap (brk/sbrk).

### 2. Memory Management

**Current state:** Bitmap PMM (16 GiB max), OffsetPageTable, fixed 4 MiB kernel heap, no frame deallocation, no heap growth.

**Key finding:** The heap is permanently limited to 4 MiB. Once exhausted, kernel cannot allocate. Frame deallocation is missing (process create/destroy leaks physical frames).

**Action needed now:**
- Heap growth: when global allocator fails, map one more page from PMM into heap region.
- Frame deallocation in `unmap_page()`: return physical frame to PMM bitmap.

**Can wait:** Better allocator (buddy/slab), huge pages, SMP-safe PMM.

### 3. Syscall ABI

**Current state:** Linux-style 6-arg convention. 4 syscalls (write, exit, yield, getpid). Canonical SyscallFrame layout. Error convention: `u64::MAX` for errors.

**Key finding:** The ABI is well-designed and flexible. The SyscallFrame layout is cross-linked in 5+ assembly files — changing it is extremely costly. The error convention is informal.

**Action needed now:**
- Formalize error convention: decide between Linux-style negative errno (RAX = -1 for error) or custom (RAX = error_code). Document it. This is a user-space ABI decision.
- Document syscall number ranges (0-99 process, 100-199 filesystem, 200-299 IPC).

**Can wait:** VDSO, `sysenter`/`sysexit`, per-CPU GS base (SMP).

### 4. IPC Readiness

**Current state:** No IPC. No `Blocked` state. No fd table. MAX_PROCESSES=8.

**Key finding:** The `Process` struct is small and extensible. Adding fields is easy. The `Blocked` state is a one-line enum change + updating `find_next_ready()`. MAX_PROCESSES=8 is a hard ceiling.

**Action needed now:**
- Add `Blocked` state to `ProcessState` enum.
- Increase MAX_PROCESSES to 32+ and convert to `Vec<Option<Process>>`.
- Add `parent_pid` field to Process.

**Can wait:** fd tables, shared memory, message queues, pipes, signals.

### 5. Driver Architecture

**Current state:** No abstraction. Direct MMIO. Hardcoded LAPIC EOI in 3 assembly locations. No PCI. No DMA safety.

**Key finding:** The LAPIC EOI is duplicated in 3 naked asm sites (timer handler, syscall force-switch, page fault return). This is a maintenance hazard.

**Action needed now:**
- Refactor LAPIC EOI: extract from 3 asm sites into a single Rust function with register save/restore wrappers.

**Can wait:** PCI enumeration, device tree, DMA safety, driver abstraction, plug-and-play.

### 6. Security Boundaries

**Current state:** Ring 0/3 enforced. Kernel pages not user-accessible. No NX, no SMEP/SMAP, no ASLR.

**Key finding:** Without NX, user processes can execute shellcode from data/stack. Without SMEP, the kernel can be tricked into executing user pages. Both are one-bit changes.

**Action needed now:**
- Enable NX on user data/stack/heap pages (EFER.NXE + page table flags).
- Enable SMEP in CR4 (bit 20).

**Can wait:** ASLR, SMAP, capabilities, seccomp, stack canaries.

---

## Recommended Implementation Order

1. **NX + SMEP** (security foundation, ~25 LOC)
2. **Heap growth** (prevents 4 MiB wall, ~30 LOC)
3. **Blocked state + MAX_PROCESSES** (IPC readiness, ~25 LOC)
4. **Guard page for user stack** (stack overflow safety, ~5 LOC)
5. **Error convention formalization** (user-space ABI, ~10 LOC doc + code)
6. **LAPIC EOI refactor** (maintenance, ~20 LOC)
7. **Frame deallocation** (memory leak fix, ~15 LOC)

All 7 items total ~130 LOC. Small, low-risk, high-value.
