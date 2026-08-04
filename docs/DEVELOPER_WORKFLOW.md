# INDOMINUS Developer Workflow

This workspace now includes a small but useful toolchain for working on the OS project more comfortably.

## Quick start

1. Open the workspace root in VS Code.
2. Install the recommended extensions.
3. Use the built-in tasks for build, run, and regression testing.
4. Use the debugging profile when you need to inspect the kernel under QEMU.

## Common commands

- Build everything: Run the task `Build (boot + kernel)`
- Run in QEMU: Run the task `Run in QEMU`
- Run regression tests: Run the task `Run regression tests`
- Clean artifacts: Run the task `Clean workspace`

## Debugging workflow

- Start the QEMU debug launch profile.
- Attach with the `Indominus: Attach GDB` profile when the guest is waiting at the GDB port.
- Set breakpoints in the kernel entry path, process scheduler, syscall dispatcher, or interrupt handlers.

## Project map

- Bootloader: [indo-boot/src/main.rs](../indo-boot/src/main.rs)
- Kernel entry and tests: [indo-kernel/src/main.rs](../indo-kernel/src/main.rs)
- Memory subsystem: [indo-kernel/src/memory](../indo-kernel/src/memory)
- Process and scheduler: [indo-kernel/src/process](../indo-kernel/src/process)
- Syscalls: [indo-kernel/src/syscall](../indo-kernel/src/syscall)
- Filesystem and storage: [indo-kernel/src/vfs](../indo-kernel/src/vfs), [indo-kernel/src/fat32.rs](../indo-kernel/src/fat32.rs), [indo-kernel/src/ahci](../indo-kernel/src/ahci)
- Userspace shell: [userspace/shell](../userspace/shell)

## Recommended next tasks

- Stabilize the boot/build path so the project is reproducible from a clean checkout.
- Add a lightweight CI check for basic build validation.
- Document each subsystem in one short page of plain English.
- Turn the roadmap items into tracked TODOs inside the workspace.
- Improve the debug story for syscall and scheduler failures.
