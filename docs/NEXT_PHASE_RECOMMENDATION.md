# INDOMINUS OS — Next Phase Recommendation

**Updated**: Phase 14 complete (TITAN FORGE)

---

## Current State (Phase 14)

| Subsystem | Status | Notes |
|-----------|--------|-------|
| Bootloader (UEFI) | ✅ Complete | Custom bootloader, GPT, RSDP |
| Memory (PMM + VMM) | ✅ Complete | Bitmap allocator, 4-level page tables, CoW, ASLR |
| Interrupts | ✅ Complete | LAPIC + IOAPIC + PIT + keyboard + serial |
| Scheduler | ✅ Complete | Preemptive round-robin, 5-tick quantum |
| Syscalls | ✅ Complete | 30 syscalls, all CR3-safe |
| User programs | ✅ Complete | ELF loader, Ring 3, fork/exec/exit/wait |
| Shell | ✅ Complete | 15 builtins, pipelines, redirections |
| FAT16 filesystem | ✅ Complete | Read/write, VFS layer |
| AHCI storage | ✅ Complete | DMA reads, TFES recovery |
| Pipe IPC | ✅ Complete | Ring buffer, blocking read/write |
| Signals | ✅ Complete | SIGUSR1 delivery, handler registration |
| Kernel hardening | ✅ Complete | 16 critical/high fixes, 15 context-switch tests |

## What's Next

### Priority 1: FPU/SSE Context Save/Restore
**Why**: Any userspace program using floating point will corrupt other processes' FP state.
- Save/restore MXCSR, x87 control word, and 512-byte FXSAVE area on context switch
- Enable CR0.TS for lazy FPU switching or use FXSAVE/FXRSTOR eagerly
- Test: fork + float arithmetic in parent/child, verify no corruption

### Priority 2: Kernel Stack Guard Pages
**Why**: Kernel stack overflow corrupts adjacent memory silently.
- Map a guard page (non-present) below each kernel stack
- Stack overflow hits guard → page fault → clear diagnostics
- Test: deep recursion in kernel mode hits guard page

### Priority 3: Signal Trampoline/sigreturn
**Why**: Current signal handlers can't return cleanly. `sigreturn` needed for proper signal handling.
- Implement `sigreturn` syscall to restore pre-signal context
- Create user-space signal trampoline page
- Test: signal handler returns and execution continues correctly

### Priority 4: Replace `static mut` with Atomics
**Why**: `static mut` is undefined behavior in Rust; `DEFERRED_CR3` should be `AtomicU64`.
- Replace all `static mut` globals with atomic types
- Audit for remaining unsafe static references

### Priority 5: SMP (Symmetric Multi-Processing)
**Why**: Single-core limits throughput. SMP enables parallel execution.
- AP bootstrap via SIPI (Startup IPI)
- Per-CPU scheduler with lock-free run queues
- IPI for inter-processor interrupts
- Test: spawn N processes, verify all cores are utilized

## Long-Term Roadmap
1. Display/graphics (framebuffer, font renderer)
2. Input (PS/2 mouse, USB HID)
3. Networking (NIC drivers, TCP/IP stack)
4. Window manager and desktop
