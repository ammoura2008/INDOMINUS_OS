# Engineering Milestones

This document turns the long-term OS vision into a measurable sequence of milestones. The focus is not raw output volume, but uncertainty reduction.

## Milestone 1 — Build and boot determinism

Goal: make the build and boot path predictable on a fresh Windows machine.

### Success criteria
- The build entrypoint works for build, check, run, and clean flows.
- QEMU and OVMF paths are detected clearly and fail with actionable messages.
- The boot image is produced consistently from the same source state.

### Evidence
- Build script runs without ambiguity.
- CI validates the repo workflow files.
- Boot logs are captured and reviewed for regressions.

## Milestone 2 — Kernel bootstrap smoke test

Goal: prove that the kernel reaches a stable early runtime state.

### Success criteria
- Early console output is visible.
- Interrupts, memory setup, and basic kernel initialization complete without panics.
- Boot failures point to a specific subsystem instead of stopping at a vague crash.

### Evidence
- Serial log captures the boot path.
- A minimal regression test checks that the kernel reaches a known-safe stage.

## Milestone 3 — Userspace handoff

Goal: verify that userspace can be launched and executed from the kernel.

### Success criteria
- A minimal userspace program can be loaded and executed.
- Syscall entry and exit paths behave correctly for basic operations.
- The init/shell flow can start without manual intervention.

### Evidence
- A small userspace smoke test runs successfully.
- The shell or init process reaches a steady state.

## Milestone 4 — Core subsystem hardening

Goal: reduce the uncertainty around the kernel’s most critical foundations.

### Success criteria
- Memory management and fault handling are stable under simple stress.
- Process scheduling shows predictable behavior.
- VFS, FAT, and syscall paths respond correctly to normal input.

### Evidence
- Regression cases cover basic process and filesystem operations.
- A repeated boot/run loop shows no regressions.

## Milestone 5 — Repository maintainability

Goal: keep the engineering workflow trustworthy as the project grows.

### Success criteria
- Documentation, build commands, CI, and roadmap items remain aligned.
- New contributors can understand the next milestone without reading the entire codebase.
- The repository can detect when workflow scaffolding drifts.

### Evidence
- The verification script passes in CI.
- The roadmap board and docs stay synchronized with implementation status.
