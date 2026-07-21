# INDOMINUS REX — Stabilization Audit Plan

**Goal**: Prove the existing kernel foundation is reliable, secure, repeatable, and architecturally understood — not merely that it boots once.

**Constraint**: No commits without explicit user approval. No new features until stabilization is complete.

---

## Phase 1: Scheduler & Context Switching

### 1.1 — State Transition Audit

| Transition | Current Code | Verification |
|---|---|---|
| Ready → Running | `find_next_ready()` sets `Running` | Trace all callers |
| Running → Ready | `switch_next()` sets `Ready` | Verify no data loss |
| Running → Zombie | `kill_process()` sets `Zombie` | Verify cleanup |
| Zombie → (reclaimed) | `waitpid` frees slot | Verify PID reuse |

**Action**: Read every code path that mutates `ProcessState`. Verify no transition is missing or illegal. Confirm no state is left inconsistent after a transition.

### 1.2 — Interrupt & Lock Safety

**Current state**: `SCHEDULER` is `spin::Mutex<Scheduler>`. Acquired in `schedule()`, `schedule_force()`, `kill_process()`. Timer interrupt calls `schedule()`.

**Risk**: Timer fires while scheduler lock held → deadlock.

**Test**: Confirm timer interrupt cannot fire while SCHEDULER lock is held. Verify interrupts are disabled when the lock is taken (naked handler has interrupts off). If not, add interrupt-disable around lock acquisition.

### 1.3 — Timer Preemption

**Test**:
- Spawn 3+ long-running processes
- Verify each process gets roughly equal CPU time
- Verify the quantum (5 ticks = 50ms) is respected
- Verify no process runs indefinitely

### 1.4 — Forced Switching (sys_exit, sys_yield)

**Test**:
- Exit a process, verify it becomes Zombie and the next process runs
- Yield a process, verify it goes to Ready and another process runs
- Verify the force_switch flag is cleared after the switch

### 1.5 — First Dispatch

**Test**:
- Verify `DEFERRED_CR3` mechanism works on first dispatch
- Verify the UEFI boot stack is not used after CR3 switch
- Verify TSS.RSP0 is set correctly before first switch

### 1.6 — CR3 Switching

**Audit every location that writes CR3**:

| Location | Action | Condition |
|---|---|---|
| `vmm::switch_page_table()` | Write CR3 | Called from boot, context switch, syscall buffer validation |
| `context_switch::schedule()` line 516 | Via `switch_page_table()` | Every timer tick |
| `context_switch::schedule_force()` lines 680-697 | Switch to kernel PML4, then restore | sys_exit/sys_yield |
| `context_switch::kill_process()` lines 751-769 | Switch to kernel PML4, then restore | Fault handling |
| `syscall::is_user_buffer_mapped()` | Switch to kernel PML4, walk, restore | sys_write |
| `vmm::free_user_address_space()` | Switch to kernel PML4, free, restore | Process cleanup |

**Invariant**: At every CR3 switch, verify the source stack (RSP) is mapped in the TARGET PML4. If not → double fault.

**Test**: Add a diagnostic counter that increments on every CR3 switch. After 1000 context switches, verify the counter matches expectations.

### 1.7 — TSS.RSP0

**Invariant**: TSS.RSP0 must point to the top of the current process's kernel stack. This is what the CPU uses when transitioning from Ring 3 → Ring 0 (via interrupt/syscall).

**Test**: Verify TSS.RSP0 is updated on every context switch. Verify it's never 0 or pointing to a wrong process's stack.

### 1.8 — Process Isolation

**Test**:
- Process A writes to its own stack → should work
- Process A writes to Process B's stack → should fault
- Process A writes to kernel memory → should fault
- Process A executes kernel code → SMEP fault
- After killing Process A, Process B and the kernel continue operating

### 1.9 — Stress Test

**Test**: Run 1000 iterations of:
- Spawn 7 processes
- Let them all run to completion (yield/exit)
- Verify idle process activates after all exit
- Verify no crash, no hang, no memory leak

---

## Phase 2: Process Lifecycle & Cleanup

### 2.1 — Lifecycle Audit

Full lifecycle: `spawn → run → yield/preempt → exit → zombie → waitpid → reclaim`

**Verify each transition**:
- `spawn()`: PID assigned, page table created, kernel stack allocated, ELF loaded
- `run/yield/preempt`: Scheduler manages state correctly
- `exit()`: Process marked Zombie, force_switch set
- `waitpid()`: Zombie reclaimed, PID slot freed, kernel stack freed, page table freed

### 2.2 — PID Reuse

**Test**:
- Spawn PID 1, exit PID 1
- Spawn another process, verify it gets PID 1 (reuse)
- Verify the old PID 1's resources are fully freed before reuse

### 2.3 — Kernel Stack Reclamation

**Invariant**: When a process exits, its kernel stack must be freed. If not → memory leak.

**Test**: Track total heap usage. Spawn and kill 100 processes. Verify heap usage returns to baseline.

### 2.4 — Page Table & Physical Frame Reclamation

**Invariant**: When a process exits, all its user-space page table frames and physical frames must be freed.

**Test**: Same as 2.3 — verify physical memory returns to baseline after process cleanup.

### 2.5 — Stress Test

**Test**: Run 1000 iterations of:
- Spawn process
- Process exits immediately
- Verify no resource leak
- Verify PID reuse works
- Verify kernel stays responsive

---

## Phase 3: Virtual Memory & Page Tables

### 3.1 — Kernel/User Mapping Audit

**Verify**:
- Kernel PML4 has identity map (first 4 GiB) + upper half (0xFFFFFFFF80000000+)
- User PML4 has ONLY upper half (entries 256-511 copied from kernel)
- User PML4 has NO identity map
- User code/stack/data are correctly mapped in user PML4

### 3.2 — CR3 Switch Safety

**Invariant**: After every CR3 switch, the code executing must be mapped in the new PML4.

**Test**: Verify that `switch_page_table()` is never called when executing code that isn't mapped in the target PML4 (except during boot when identity map is always present).

### 3.3 — NX Permissions

**Test**:
- Kernel code pages: PRESENT, NOT writable (or writable for now), EXECUTABLE
- Kernel data pages: PRESENT, WRITABLE, NOT executable
- User code pages: PRESENT, USER_ACCESSIBLE, EXECUTABLE
- User stack pages: PRESENT, USER_ACCESSIBLE, WRITABLE, NOT executable
- Identity-mapped pages: NX set by `harden_identity_map()` (except kernel code)

### 3.4 — Fault Isolation

**Test** (each test should kill only the offending process):
- Unmapped address → page fault → kill process → kernel continues
- Non-canonical address → GP fault → kill process → kernel continues
- Kernel-space address → GP/page fault → kill process → kernel continues
- Write to read-only page → page fault → kill process → kernel continues
- Execute NX page → page fault → kill process → kernel continues

### 3.5 — Buffer Cross-Page Boundary

**Test**: Pass a buffer that crosses a page boundary where one page is mapped and the other isn't. Verify the kernel correctly detects this during `is_user_buffer_mapped`.

---

## Phase 4: Syscalls & Ring 3 → Ring 0 Security

### 4.1 — Adversarial Syscall Tests

Every test assumes a **malicious user process**.

| Test | Input | Expected |
|---|---|---|
| Invalid syscall number | 99, 255, -1 | Return u64::MAX |
| Invalid buffer (unmapped) | 0x1000 | Return u64::MAX |
| Invalid buffer (kernel) | 0xFFFFFFFF80000000 | Return u64::MAX |
| Invalid buffer (non-canonical) | 0x8000000000000000 | Return u64::MAX |
| Null buffer | 0x0 | Return u64::MAX |
| Buffer crossing page boundary | Valid page + unmapped page | Return u64::MAX |
| Read-only page as write buffer | mmap'd read-only page | Return u64::MAX |
| Exit code 0 | Normal exit | Process becomes Zombie |
| Exit code -1 | Negative exit | Process becomes Zombie |
| Waitpid on non-child | Any PID | Return u64::MAX |
| Waitpid WNOHANG on running child | PID of running process | Return 0 |

### 4.2 — Positive Tests

| Test | Input | Expected |
|---|---|---|
| Valid write to stdout | "hello" | Output appears on serial |
| Getpid | syscall(3) | Returns correct PID |
| Yield | syscall(2) | Other process runs |
| Exit | syscall(1) | Process becomes Zombie |
| Waitpid on exited child | PID of Zombie | Returns exit code |

### 4.3 — Isolation Tests

**Test**: Spawn 3 processes. Kill process 2 with a fault. Verify processes 1 and 3 continue operating. Verify the kernel is unaffected.

### 4.4 — Regression Tests

**Test**: After killing a faulting process, run additional syscalls from other processes. Verify all syscalls work correctly. Verify no stale state from the killed process affects anything.

---

## Phase 5: Interrupts, IDT, GDT, TSS

### 5.1 — Handler Address Validity

**Invariant**: Every IDT handler function address must be in kernel space (upper half) and mapped in all PML4s.

**Test**: Dump every IDT entry's address. Verify each is in the range 0xFFFFFFFF80000000..0xFFFFFFFFFFFFFFFF.

### 5.2 — TSS.RSP0 Correctness

**Test**: At each context switch, verify TSS.RSP0 is updated to the new process's kernel stack top. Verify it's never 0 or stale.

### 5.3 — IRET Frame Correctness

**Test**: Verify every IRET frame (syscall return, interrupt return, context switch return) has correct CS, SS, RFLAGS, RIP, RSP values. Specifically:
- User → User: CS=0x1B, SS=0x23, RFLAGS.IF=1
- Kernel → Kernel: CS=0x08, SS=0x10
- Never: User CS with Kernel RSP or vice versa

### 5.4 — Fault Handler Classification

**Test**:
- User page fault: CS.RPL=3 → kill process
- Kernel page fault: CS.RPL=0 → halt kernel
- User GP: CS.RPL=3 → kill process
- Kernel GP: CS.RPL=0 → halt kernel

### 5.5 — Double Fault

**Test**: Verify double fault handler uses IST stack and halts cleanly. Simulate by triggering a fault in the page fault handler itself.

---

## Phase 6: Memory Allocator & Heap

### 6.1 — Force-Unlock Investigation

**Current state**: `init_heap()` force-unlocks the heap by writing 0 to the spinlock byte.

**Investigation**:
- Why does the heap lock start as non-zero?
- Is this a bootloader issue (bootloader writes to heap static)?
- Is this a linker issue (BSS not zeroed)?
- Is this a crate issue (spinlock initialization order)?

**Action**: Read the heap allocator's `LockedHeap` implementation. Check if `spin::Mutex` zero-initializes. Check what value is at the heap lock byte before `init_heap()`.

**Permanent fix options**:
- If the heap is truly zero-initialized, remove the force-unlock
- If not, find why and fix the root cause
- If the issue is crate-specific, document the invariant

### 6.2 — Allocator Safety Under CR3

**Invariant**: All heap allocator function pointers (PIC) contain physical addresses. The allocator can only be called when the kernel PML4 is active (has identity map).

**Current mitigation**: `schedule_force()` and `kill_process()` switch to kernel PML4 before calling `free_kernel_stack()`.

**Test**: Verify that every call to `alloc()`/`dealloc()` in the kernel occurs with kernel PML4 active. Add a debug assertion: `assert!(cr3_is_kernel_pml4())` before every allocator call.

### 6.3 — No Allocator Metadata Overlap

**Test**: Verify that kernel stack allocations and heap allocations don't overlap. Check the memory layout: heap is at 0xFFFFFFFFC0000000+, stacks are at process-specific addresses.

### 6.4 — Stress Test

**Test**: Allocate and free 10000 small objects. Verify no corruption, no leak, no double-free.

---

## Phase 7: Temporary Workarounds

### 7.1 — Workaround Table

| Workaround | Why | Invariant | Permanent Fix | Tested |
|---|---|---|---|---|
| Heap force-unlock | Lock byte starts non-zero | No concurrent heap access during boot | Find root cause of non-zero lock | ❌ |
| spin::Once → MaybeUninit | `call_once` hangs on `InterruptDescriptorTable` | IDT initialized exactly once, no concurrent access | Root-cause the hang | ❌ |
| Deferred CR3 | Boot stack unmapped after CR3 switch | DEFERRED_CR3 read after `mov rsp, r12` on upper-half stack | Formalize the mechanism, document the invariant | ⚠️ |
| PIC identity-map workaround | Heap allocator uses PIC function pointers with physical addresses | Kernel PML4 active during all heap calls | Remove PIC dependency (use `-C relocation-model=static` or `position-independent-code=no`) | ❌ |
| harden_identity_map approximate kernel_end | Hardcoded kernel_end = kernel_phys_start + 0x100000 | Kernel stays under ~1 MiB | Parse actual kernel end from ELF headers | ❌ |
| GS base uses physical address | PER_CPU at physical address | Identity map always active in kernel PML4 | Change to `phys_to_kernel_virt()` | ❌ |
| Double process::init() | Appears to be a copy-paste bug | Second init doesn't break anything (idle is simple) | Remove the second call | ❌ |
| Diagnostic markers [MARK] | Debug tracing | No impact on correctness | Remove after stabilization | ⚠️ |

### 7.2 — Each Workaround Investigation

For each workaround:
1. Read the exact code
2. Find the exact root cause
3. Determine if it's safe or dangerous
4. Design a permanent fix
5. Determine if the fix is in scope for Phase 1 stabilization or deferred

---

## Phase 8: Automated Regression Suite

### 8.1 — Test Harness Design

**Script**: `tools/regression_test.py` (Python)

**Steps**:
1. Build kernel (`cargo build --release`)
2. Build bootloader (`cargo build --release --target x86_64-unknown-uefi`)
3. Verify PE/COFF magic of bootloader
4. Verify ELF magic of kernel
5. Generate test binaries (existing `gen_comprehensive_test.py`)
6. Create ESP directory structure
7. Launch QEMU with timeout (30 seconds)
8. Capture serial output
9. Parse output for expected patterns
10. Detect unexpected faults
11. Detect hangs/timeouts
12. Return PASS/FAIL with detailed log

### 8.2 — Expected Output Patterns

```
TEST1_NORMAL_OK
TEST2_MULTI_PID_OK
TEST3_NULL_DEREF_BEFORE
TEST4_INVALID_PTR_BEFORE
TEST5_UNMAPPED_BEFORE
TEST5_UNMAPPED_RESULT_OK
TEST6_NULL_PTR_BEFORE
TEST6_NULL_PTR_RESULT_OK
TEST7_INVALID_SYSCALL_BEFORE
TEST7_INVALID_SYSCALL_RESULT_OK
TEST4_INVALID_PTR_RESULT_OK
TEST1_RESUMED_OK
[IDLE] Idle process running
```

### 8.3 — Unexpected Fault Detection

Watch for:
- Page faults in kernel code (not user-initiated)
- Double faults
- Triple faults
- Any `[FATAL]` messages
- QEMU exit code != 0

---

## Phase 9: Repetition Testing

### 9.1 — 100 Iterations

Run the full test suite 100 times. Verify 100% pass rate.

### 9.2 — 1000 Iterations (if practical)

Run a subset of tests 1000 times:
- Spawn/exit cycle
- Syscall under load
- Page fault handling
- Process creation/destruction

### 9.3 — Varying Conditions

- Different timer frequencies (if configurable)
- Different numbers of concurrent processes
- Different timing patterns (rapid exit vs. long-running)

---

## Phase 10: Final Stability Report

### Deliverable

A structured report containing:

1. **Tests Performed**: List every test with description
2. **Repetitions**: How many times each test was run
3. **Results**: Pass/Fail count per test
4. **Bugs Found**: Description, root cause, severity
5. **Fixes Applied**: What was changed and why
6. **Remaining Risks**: Known issues not yet fixed
7. **Temporary Workarounds**: Status of each workaround
8. **Unexplained Behavior**: Anything that works but we don't know why
9. **Security Weaknesses**: Known attack surfaces
10. **Recommended Next Phase**: What to do after stabilization

---

## Execution Order

1. **Phase 1-6** (Subsystem audits) — Do first, find bugs, fix them
2. **Phase 7** (Workarounds) — Investigate alongside Phase 1-6
3. **Phase 8** (Regression suite) — Build after bugs are fixed
4. **Phase 9** (Repetition) — Run after regression suite exists
5. **Phase 10** (Report) — Final deliverable

Within each phase:
- Read the code
- Identify the invariant
- Write a test (positive, adversarial, isolation, regression)
- Run the test
- Fix any bugs found
- Re-run to confirm fix
- Mark subsystem as verified only when all tests pass

---

## Success Criteria

A subsystem is marked **STABLE** when:

1. All invariants are explicitly documented
2. Positive tests pass (legitimate operations work)
3. Adversarial tests pass (malicious operations rejected)
4. Isolation tests pass (failures don't propagate)
5. Regression tests pass (other code works after failure)
6. Stress tests pass (no leaks, no corruption)
7. Repetition tests pass (works N times in a row)

The kernel is marked **STABLE** when all subsystems are stable.
