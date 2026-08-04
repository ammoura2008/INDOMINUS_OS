# Phase 15 Plan — HARDEN: FPU, Guard Pages, Signal Return, SMP

**Goal**: Eliminate the 4 highest-impact remaining risks from the Kernel Confidence Report. Move "Processes" and "Signals" from 8-9/10 toward 10/10.

**Timeline**: ~2-3 weeks of focused work  
**Prerequisites**: Phase 14 complete (16 critical fixes, 15/15 tests passing)

---

## Feature 1: FPU/SSE Context Save/Restore

**Current risk**: Any userspace program using floating point silently corrupts other processes' FP state. This is a correctness AND security bug (CVE-2018-3665 side-channel).

### What changes

| File | Change | Why |
|------|--------|-----|
| `indo-kernel/src/gdt.rs` | Enable CR0.EM=0, CR0.MP=1, CR4.OSFXSR=1, CR4.OSXMMEXCPT=1 | Enable FPU/SSE hardware |
| `indo-kernel/src/process/process.rs` | Add `fpu_state: AlignedFpuState` field to `Process` struct (512 bytes, 16-byte aligned) | Store per-process FPU/SSE state |
| `indo-kernel/src/process/context_switch.rs` | Add `fxsave` in context-out, `fxrstor` in context-in | Eager save/restore on every switch |
| `indo-kernel/src/idt.rs` | Add `#NM` (Device Not Available, vector 7) handler | Catch FPU access before initialization |
| `indo-kernel/src/main.rs` | Call `enable_fpu()` during boot before any user process runs | Initialize FPU hardware |

### Implementation steps

1. **Boot-time FPU enable** (`gdt.rs` or `main.rs`):
   - Read CR0, clear bit 2 (EM), set bit 1 (MP)
   - Read CR4, set bit 9 (OSFXSR), set bit 10 (OSXMMEXCPT)
   - Execute `FNINIT` to reset x87 state
   - Execute `LDMXCSR [default_mxcsr]` to mask all FP exceptions (0x1F80)

2. **Per-process FPU state** (`process.rs`):
   ```rust
   // 512-byte FXSAVE area, 16-byte aligned
   #[repr(C, align(16))]
   struct FpuState {
       data: [u8; 512],
   }
   
   // In Process struct:
   fpu_state: FpuState,  // initialized to zeroed (FNINIT state)
   ```

3. **Context switch save/restore** (`context_switch.rs`):
   ```rust
   // In switch_to(), AFTER saving general-purpose regs:
   unsafe {
       // Save outgoing process FPU state
       core::arch::asm!(
           "fxsave [{fpu}]",
           fpu = in(reg) &mut prev.fpu_state.data as *mut _ as u64,
           options(nostack)
       );
       // Restore incoming process FPU state
       core::arch::asm!(
           "fxrstor [{fpu}]",
           fpu = in(reg) &next.fpu_state.data as *const _ as u64,
           options(nostack)
       );
   }
   ```

4. **#NM handler** (`idt.rs`):
   - On #NM during kernel mode: panic with diagnostic
   - On #NM during user mode: this shouldn't happen with eager FPU, but log warning

### Why eager (not lazy)?
- Lazy FPU switching is insecure (CVE-2018-3665 — speculative execution leaks FP regs)
- Lazy FPU complicates SMP (sleeping thread with unsaved FPU can't migrate)
- Linux switched to eager FPU in kernel 4.9 (2016)
- 512-byte FXSAVE/FXRSTOR is ~40 cycles — negligible overhead

### New tests

| # | Test | What it validates |
|---|------|-------------------|
| 16 | `test_fpu_basic` | Float math in child survives fork+context-switch |
| 17 | `test_fpu_isolation` | Parent float, child float, both correct after switch |
| 18 | `test_fpu_stress` | 10 fork+float cycles, all values correct |

### Expected confidence change
- Processes: 9/10 → 9.5/10
- Signals: 8/10 → 8.5/10 (FPU state in signal frames)

---

## Feature 2: Kernel Stack Guard Pages

**Current risk**: Kernel stack overflow silently corrupts adjacent memory. The IST stack for double-fault exists but regular kernel stacks have no guard.

### What changes

| File | Change | Why |
|------|--------|-----|
| `indo-kernel/src/process/mod.rs` | Allocate guard page below each kernel stack | Catch stack overflow via page fault |
| `indo-kernel/src/memory/vmm.rs` | Map guard page as non-present | Page fault on overflow |
| `indo-kernel/src/idt.rs` | Detect stack overflow in page fault handler | Clear diagnostics, halt |

### Implementation steps

1. **Guard page allocation** (`process/mod.rs`):
   ```rust
   const KERNEL_STACK_SIZE: usize = 8192;  // 8 KiB (2 pages)
   const GUARD_PAGE_SIZE: usize = 4096;     // 1 page
   
   // When spawning a process:
   // 1. Allocate: guard_page (1 page) + kernel_stack (2 pages) = 3 pages contiguous
   // 2. Map guard_page as non-present (no PAGE_PRESENT flag)
   // 3. Map kernel_stack pages as present + writable
   // 4. kernel_stack_base = guard_page_addr + GUARD_PAGE_SIZE
   ```

2. **Stack overflow detection** (`idt.rs`):
   ```rust
   // In page_fault handler, for kernel-mode faults:
   if address is within [kernel_stack_base - GUARD_PAGE_SIZE, kernel_stack_base) {
       // Stack overflow!
       kprintln!("[PANIC] Kernel stack overflow at {:#x}", address);
       process_dump();
       halt();
   }
   ```

3. **Existing stack layout**:
   ```
   Before:
   ┌─────────────────┐  High
   │  Kernel Stack    │  8 KiB (2 pages)
   └─────────────────┘  Low
   
   After:
   ┌─────────────────┐  High
   │  Kernel Stack    │  8 KiB (2 pages)
   ├─────────────────┤
   │  Guard Page      │  4 KiB (non-present)
   └─────────────────┘  Low
   ```

### Why not stack canaries?
- Guard pages catch overflow immediately via hardware
- Stack canaries only detect corruption on function return
- Use both: guard pages as primary, canaries as defense-in-depth
- Rust `-C stack-protector=all` can add canaries later

### New tests

| # | Test | What it validates |
|---|------|-------------------|
| 19 | `test_stack_guard` | Deep recursion in kernel mode hits guard page, diagnostic appears |
| 20 | `test_stack_normal` | Normal kernel operations don't trigger guard page fault |

### Expected confidence change
- Diagnostics: 8/10 → 8.5/10 (clear stack overflow detection)

---

## Feature 3: Signal Trampoline / sigreturn

**Current risk**: Signal handlers can't return cleanly. Must call `_exit()` or `longjmp()`. This breaks standard C/Rust calling conventions.

### What changes

| File | Change | Why |
|------|--------|-----|
| `indo-kernel/src/syscall/mod.rs` | Add `sys_rt_sigreturn` syscall (#30) | Restore pre-signal context |
| `indo-kernel/src/syscall/mod.rs` | Add `sys_sigaltstack` syscall (#31) | Alternate signal stack |
| `indo-kernel/src/process/process.rs` | Add `signal_stack` and `saved_context` fields | Store signal context |
| `indo-kernel/src/syscall/mod.rs` | Modify signal delivery to push `RtSigframe` on user stack | Save full register context |
| `userspace/syscall/src/lib.rs` | Add `sigreturn()` and `sigaltstack()` wrappers | User-space API |

### Implementation steps

1. **Signal frame structure** (new file or in `syscall/mod.rs`):
   ```rust
   #[repr(C)]
   struct RtSigframe {
       // Trampoline return address
       pretcode: u64,        // pointer to trampoline code
       // Signal info
       pinfo: u64,           // &siginfo_t
       puc: u64,             // &ucontext_t
       info: Siginfo,        // 128 bytes
       // Context (mcontext)
       uc: Ucontext,
       // FPU state (64-byte aligned)
       fpstate: [u8; 512],
   }
   
   #[repr(C)]
   struct Ucontext {
       uc_flags: u64,
       uc_link: u64,
       uc_stack: StackDef,   // sigaltstack info
       uc_mcontext: Mcontext, // all registers
       uc_sigmask: u64,      // signal mask
   }
   
   #[repr(C)]
   struct Mcontext {
       r8: u64, r9: u64, r10: u64, r11: u64,
       r12: u64, r13: u64, r14: u64, r15: u64,
       rdi: u64, rsi: u64, rbp: u64, rdx: u64,
       rax: u64, rcx: u64, rsp: u64, rip: u64,
       eflags: u64,
       cs: u64, ss: u64, ds: u64, es: u64,
       fs: u64, gs: u64,
       fpstate_ptr: u64,    // pointer to XSAVE area
   }
   ```

2. **Signal delivery** (`syscall/mod.rs` — modify `deliver_signal`):
   ```rust
   fn deliver_signal(process: &mut Process, signum: u8) {
       let handler = process.signal_handlers[signum as usize];
       if handler == 0 || handler == 1 { return; } // default/ignore
       
       // 1. Save current register context to signal frame on user stack
       let frame_size = size_of::<RtSigframe>();
       let frame_sp = process.user_rsp.unwrap() - frame_size;
       let frame = frame_sp as *mut RtSigframe;
       
       unsafe {
           (*frame).uc.uc_mcontext.rip = process.user_rip.unwrap();
           (*frame).uc.uc_mcontext.rsp = process.user_rsp.unwrap();
           // ... save all registers ...
           
           // Save FPU state
           core::arch::asm!("fxsave [{fpu}]",
               fpu = in(reg) &mut (*frame).fpstate as *mut _ as u64);
           
           // Set trampoline return address
           (*frame).pretcode = get_trampoline_addr();
       }
       
       // 2. Redirect to handler
       process.user_rip = Some(handler);
       process.user_rsp = Some(frame_sp);
       // RDI = signal number (first arg)
   }
   ```

3. **Trampoline code** (new userspace binary or embedded in kernel):
   ```nasm
   ; Trampoline — mapped into every process at a known address
   ; Called when signal handler returns
   global _sigreturn_trampoline
   _sigreturn_trampoline:
       mov rax, 30          ; SYS_RT_SIGRETURN
       syscall              ; never returns
       ud2                  ; crash if syscall fails
   ```

4. **sys_rt_sigreturn** (new syscall):
   ```rust
   fn sys_rt_sigreturn(regs: &mut PtRegs) -> ! {
       let frame = regs.rsp as *const RtSigframe;
       
       // Restore all registers from frame
       regs.rip = (*frame).uc.uc_mcontext.rip;
       regs.rsp = (*frame).uc.uc_mcontext.rsp;
       // ... restore all registers ...
       
       // Restore FPU state
       core::arch::asm!("fxrstor [{fpu}]",
           fpu = in(reg) &(*frame).fpstate as *const _ as u64);
       
       // Return to original user context
       restore_to_user_mode(regs);
   }
   ```

5. **sys_sigaltstack** (new syscall):
   ```rust
   fn sys_sigaltstack(new: u64, old: u64) -> Result<()> {
       // Save old alt stack
       if old != 0 {
           let old_stack = old as *mut SigStack;
           unsafe {
               (*old_stack) = current_thread().sigaltstack;
           }
       }
       // Set new alt stack
       if new != 0 {
           let new_stack = unsafe { &*(new as *const SigStack) };
           validate_user_range(new_stack.ss_sp, new_stack.ss_size)?;
           current_thread().sigaltstack = *new_stack;
       }
       Ok(())
   }
   ```

### How it works end-to-end

```
Process calls sys_sigaction(SIGUSR1, handler) → registers handler address

Process receives SIGUSR1:
  1. Kernel pushes RtSigframe on user stack (saves all registers + FPU)
  2. Kernel sets RIP = handler address, RSP = frame
  3. User executes handler function
  4. Handler calls sys_rt_sigreturn()
  5. Kernel restores all registers + FPU from frame
  6. Process resumes at original instruction
```

### New tests

| # | Test | What it validates |
|---|------|-------------------|
| 21 | `test_sigreturn_basic` | Signal handler returns, execution continues correctly |
| 22 | `test_sigreturn_regs` | All registers restored correctly after sigreturn |
| 23 | `test_sigreturn_nested` | Signal during handler → nested handler → both return |

### Expected confidence change
- Signals: 8/10 → 9/10
- Syscalls: 9/10 → 9.5/10

---

## Feature 4: Replace `static mut` with Atomics

**Current risk**: `DEFERRED_CR3` is `static mut` — undefined behavior in Rust. Any concurrent access is UB.

### What changes

| File | Change | Why |
|------|--------|-----|
| `indo-kernel/src/process/context_switch.rs` | Change `DEFERRED_CR3` from `static mut` to `AtomicU64` | Eliminate UB |
| `indo-kernel/src/process/context_switch.rs` | Change `DEFERRED_SP` from `static mut` to `AtomicU64` | Eliminate UB |

### Implementation

```rust
// Before:
static mut DEFERRED_CR3: u64 = 0;
static mut DEFERRED_SP: u64 = 0;

// After:
use core::sync::atomic::{AtomicU64, Ordering};

static DEFERRED_CR3: AtomicU64 = AtomicU64::new(0);
static DEFERRED_SP: AtomicU64 = AtomicU64::new(0);

// Usage:
DEFERRED_CR3.store(new_cr3, Ordering::Relaxed);
let cr3 = DEFERRED_CR3.load(Ordering::Relaxed);
DEFERRED_CR3.store(0, Ordering::Relaxed); // clear
```

### Expected confidence change
- Minor: eliminates UB, improves code quality score

---

## Feature 5: `ps` Command Implementation

**Current risk**: `ps` is a stub. No process status visibility.

### What changes

| File | Change | Why |
|------|--------|-----|
| `indo-kernel/src/syscall/mod.rs` | Add `sys_getprocs` syscall (#32) — returns process table snapshot | Allow userspace to enumerate processes |
| `userspace/shell/src/lib.rs` | Implement `ps` builtin using `sys_getprocs` | Process status display |

### Implementation

```rust
// New syscall: sys_getprocs
// Writes array of ProcsEntry to user buffer
#[repr(C)]
struct ProcsEntry {
    pid: u64,
    state: u8,      // 0=Ready, 1=Running, 2=Blocked, 3=Zombie
    parent_pid: u64,
    kstack_used: u32,  // bytes used of kernel stack
}

fn sys_getprocs(buf_ptr: u64, count: u64) -> Result<()> {
    // Iterate process table, write entries to user buffer
}
```

### Expected confidence change
- Diagnostics: 8/10 → 8.5/10

---

## Implementation Order

| Step | Feature | Dependencies | Estimated effort |
|------|---------|-------------|-----------------|
| 1 | Replace `static mut` with Atomics | None | 30 minutes |
| 2 | FPU/SSE enable + context save/restore | None | 1-2 days |
| 3 | Kernel stack guard pages | None | 1 day |
| 4 | Signal trampoline + sigreturn | Feature 2 (FPU in signal frame) | 2-3 days |
| 5 | sys_getprocs + ps command | None | 1 day |
| 6 | Test all 23 tests | Features 1-5 | 1 day |
| 7 | Documentation + push to GitHub | All | 1 hour |

### Critical path
```
Step 1 (Atomics) ──┐
Step 2 (FPU) ──────┤──→ Step 4 (Signal trampoline) ──→ Step 6 (Test) ──→ Step 7 (Push)
Step 3 (Guard) ────┤
Step 5 (ps) ───────┘
```

Steps 1, 2, 3, 5 are independent. Step 4 depends on Step 2 (FPU state in signal frame).

---

## Files to Create/Modify

### New files
- `userspace/test_ctxswitch/src/main.rs` — add tests 16-23
- `userspace/rootfs/bin/test_ctxswitch` — rebuilt binary

### Modified files
| File | Changes |
|------|---------|
| `indo-kernel/src/gdt.rs` | FPU enable in boot sequence |
| `indo-kernel/src/main.rs` | Call `enable_fpu()`, add `sys_getprocs` dispatch |
| `indo-kernel/src/idt.rs` | #NM handler, stack overflow detection |
| `indo-kernel/src/process/process.rs` | Add `fpu_state`, `signal_stack`, `saved_context` fields |
| `indo-kernel/src/process/context_switch.rs` | FXSAVE/FXRSTOR, AtomicU64 for DEFERRED_CR3/SP |
| `indo-kernel/src/process/mod.rs` | Guard page allocation |
| `indo-kernel/src/memory/vmm.rs` | Guard page mapping helper |
| `indo-kernel/src/syscall/mod.rs` | sys_rt_sigreturn (#30), sys_sigaltstack (#31), sys_getprocs (#32), signal delivery rewrite |
| `userspace/syscall/src/lib.rs` | sigreturn(), sigaltstack(), getprocs() wrappers |
| `userspace/shell/src/lib.rs` | ps builtin implementation |
| `docs/ERRORS_AND_FIXES.md` | Section 18: Phase 15 fixes |
| `docs/ROADMAP.md` | Phase 15 row |
| `docs/ROADMAP_BOARD.md` | Update status |
| `docs/ENGINEERING_MILESTONES.md` | Milestone 7 complete |

---

## Test Plan

### Build and run sequence
```bash
# 1. Build userspace (includes new tests)
python tools/build_userspace.py

# 2. Build kernel (embeds initrd with new test_ctxswitch)
cargo build --release -p indo-kernel

# 3. Copy kernel to ESP
Copy-Item target\x86_64-unknown-none\release\indo-kernel build\esp\EFI\INDOMINUS\kernel.elf

# 4. Run tests
python tools/run_ctxswitch_full.py
```

### Expected serial output
```
[CTXSW] === Phase 15: HARDEN Test Suite ===
[CTXSW] [1/23] yield: PASS
[CTXSW] [2/23] sleep: PASS
...
[CTXSW] [16/23] fpu_basic: PASS
[CTXSW] [17/23] fpu_isolation: PASS
[CTXSW] [18/23] fpu_stress: PASS
[CTXSW] [19/23] stack_guard: PASS
[CTXSW] [20/23] stack_normal: PASS
[CTXSW] [21/23] sigreturn_basic: PASS
[CTXSW] [22/23] sigreturn_regs: PASS
[CTXSW] [23/23] sigreturn_nested: PASS
[CTXSW] === ALL 23 TESTS PASSED ===
```

---

## Success Criteria

| Metric | Target |
|--------|--------|
| Tests passing | 23/23 |
| QEMU runs without crash | 2 consecutive |
| FPU corruption | Zero (float math survives fork + context switch) |
| Stack overflow | Detected and halted with diagnostic |
| Signal handler return | Clean return to original execution |
| `ps` command | Shows all running processes |
| `static mut` | Zero instances in kernel code |

---

## Post-Phase 15 Confidence Targets

| Subsystem | Before | After | Delta |
|-----------|--------|-------|-------|
| Boot | 10/10 | 10/10 | — |
| Interrupts | 9/10 | 9/10 | — |
| Scheduler | 9/10 | 9/10 | — |
| Virtual Memory | 9/10 | 9.5/10 | +0.5 (guard pages) |
| Physical Memory | 9/10 | 9/10 | — |
| Copy-on-Write | 9/10 | 9/10 | — |
| Processes | 9/10 | 9.5/10 | +0.5 (FPU, guard pages) |
| Signals | 8/10 | 9/10 | +1.0 (sigreturn, trampoline) |
| Syscalls | 9/10 | 9.5/10 | +0.5 (sigreturn, getprocs) |
| Diagnostics | 8/10 | 9/10 | +1.0 (stack overflow, ps) |
| Filesystem | 6/10 | 6/10 | — |
| Drivers | 1/10 | 1/10 | — |
| Networking | 0/10 | 0/10 | — |
| Graphics | 0/10 | 0/10 | — |
