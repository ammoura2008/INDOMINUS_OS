# ADR-001 — Kernel Strategy

**Date**: 2026-07-17  
**Status**: ACCEPTED  
**Authors**: INDOMINUS Core Team  

---

## Context

Every OS project must answer the foundational question: which kernel?

The options are: Linux, Linux fork, existing microkernel (seL4, MINIX, L4),
existing hobby kernel (Redox, Theseus), or custom from scratch.

## Decision

**INDOMINUS will use a fully custom kernel written in Rust with C for
architecture-specific assembly stubs.**

Target architecture for Phase 0–3: **x86_64** only.  
Future architectures: AArch64 (Phase 4+), RISC-V (Phase 6+).

Kernel model: **Monolithic with modular drivers** (same model as Linux).

Microkernel was considered (seL4, Fuchsia-style) and rejected — see below.

## Consequences

### Advantages
- Total control over every system call, memory model, and scheduler decision
- No inherited technical debt from 35 years of Linux decisions
- Rust's type system prevents entire classes of kernel bugs at compile time
- Custom kernel enables INDOMINUS-specific optimizations (AI-aware scheduler, etc.)
- Educational: we understand everything we ship

### Disadvantages
- Hardware driver support is zero at start — must write or port everything
- Estimated 3–5 years to reach Linux driver parity on common hardware
- Debugging without mature tooling is painful
- Security vulnerabilities in novel kernel code are likely

### Why not microkernel?
- IPC overhead is real: Mach (macOS's kernel) IPC is a known bottleneck
- seL4 has 10,000 lines of verified C — 2+ years of formal methods work
- Fuchsia (Google's microkernel OS) has been in development since 2016 and
  still has no public release date — a cautionary tale
- Monolithic kernels dominate production: Linux, XNU, Windows NT (largely)

### Why not Linux?
- INDOMINUS's identity requires owning the full stack
- Linux's scheduler, memory model, and driver API are shaped by 30+ years of
  constraints that don't apply to a new OS designed in 2026
- Security: we cannot audit or formally verify code we didn't write

## Alternatives Considered

| Option | Verdict |
|---|---|
| Linux (hardened) | Rejected — INDOMINUS must own the kernel |
| Linux fork | Rejected — inherits all Linux complexity |
| Redox OS (Rust microkernel) | Considered — too microkernel, limited ecosystem |
| seL4 | Considered — formal verification valuable, but scope mismatch |
| Custom from scratch | **ACCEPTED** |

## Review Date

Revisit after Phase 1 completion. If custom kernel development proves
unacceptably slow for hardware support, evaluate a hybrid approach.
