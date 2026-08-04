# Engineering Milestones

This document turns the long-term OS vision into a measurable sequence of milestones. The focus is not raw output volume, but uncertainty reduction.

## Milestone 1 — Build and boot determinism ✅ COMPLETE

Goal: make the build and boot path predictable on a fresh Windows machine.

### Success criteria
- The build entrypoint works for build, check, run, and clean flows.
- QEMU and OVMF paths are detected clearly and fail with actionable messages.
- The boot image is produced consistently from the same source state.

### Evidence
- Build script runs without ambiguity.
- CI validates the repo workflow files.
- Boot logs are captured and reviewed for regressions.

## Milestone 2 — Kernel bootstrap smoke test ✅ COMPLETE

Goal: prove that the kernel reaches a stable early runtime state.

### Success criteria
- Early console output is visible.
- Interrupts, memory setup, and basic kernel initialization complete without panics.
- Boot failures point to a specific subsystem instead of stopping at a vague crash.

### Evidence
- Serial log captures the boot path.
- A minimal regression test checks that the kernel reaches a known-safe stage.

## Milestone 3 — Userspace handoff ✅ COMPLETE

Goal: verify that userspace can be launched and executed from the kernel.

### Success criteria
- A minimal userspace program can be loaded and executed.
- Syscall entry and exit paths behave correctly for basic operations.
- The init/shell flow can start without manual intervention.

### Evidence
- A small userspace smoke test runs successfully.
- The shell or init process reaches a steady state.

## Milestone 4 — Core subsystem hardening ✅ COMPLETE

Goal: reduce the uncertainty around the kernel's most critical foundations.

### Success criteria
- Memory management and fault handling are stable under simple stress.
- Process scheduling shows predictable behavior.
- VFS, FAT, and syscall paths respond correctly to normal input.

### Evidence
- Regression cases cover basic process and filesystem operations.
- A repeated boot/run loop shows no regressions.

## Milestone 5 — Repository maintainability ✅ COMPLETE

Goal: keep the engineering workflow trustworthy as the project grows.

### Success criteria
- Documentation, build commands, CI, and roadmap items remain aligned.
- New contributors can understand the next milestone without reading the entire codebase.
- The repository can detect when workflow scaffolding drifts.

### Evidence
- The verification script passes in CI.
- The roadmap board and docs stay synchronized with implementation status.

## Milestone 6 — Kernel trust boundaries ✅ COMPLETE (Phase 14)

Goal: every syscall, interrupt, and context switch follows strict rules.

### Success criteria
- All syscalls validate user pointers before access.
- No kernel code accesses physical memory on user PML4 (identity map requirement).
- Process lifecycle (fork/exec/exit/wait) handles all edge cases.
- Crash diagnostics provide actionable post-mortem data.

### Evidence
- 15/15 context-switch validation tests pass (CoW, fork, memory, pipe, signal stress).
- 2/2 full boot+test runs stable, 0 kernel panics.
- All 16 critical/high bugs from audit fixed and verified.

## Milestone 7 — Next: SMP & Advanced Features

Goal: prepare for multi-core and advanced OS features.

### Success criteria
- FPU/SSE context save/restore works across process switches.
- Kernel stack guard pages prevent stack overflow.
- Signal trampoline/sigreturn enables proper signal handling.
- SMP boot and IPI work correctly.

### Evidence
- FPU stress test passes (floating point across fork/yield).
- Stack overflow test hits guard page, not kernel corruption.
- Signal handler can return cleanly (sigreturn works).
- Multi-core boot log shows all cores online.
