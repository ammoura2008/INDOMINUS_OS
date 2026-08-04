# INDOMINUS Roadmap Board

This board is a lightweight project tracker for the workspace.

## Now (Phase 14 Complete)
- ✅ TITAN FORGE kernel hardening complete (16 critical/high fixes)
- ✅ 15/15 userspace context-switch tests pass
- ✅ 2/2 full boot+test runs stable
- ✅ Documentation updated

## Next (Phase 15)
- SMP (Symmetric Multi-Processing) support
- FPU/SSE context save/restore (CR0.TS handling)
- Kernel stack guard pages
- Signal trampoline/sigreturn mechanism
- Replace `static mut` globals with AtomicU64
- Move inline test code from main.rs to dedicated module

## Later
- Display/graphics support (framebuffer, font renderer)
- Input and mouse support (PS/2, USB HID)
- Networking support (NIC drivers, TCP/IP stack)
- Window manager and desktop experience

## Blocked
- Full SMP support (requires Phase 15 prerequisites)
- Advanced security hardening beyond current phase
- Real hardware validation
