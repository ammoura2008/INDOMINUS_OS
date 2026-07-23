# INDOMINUS OS — Security-First x86_64 Operating System

A from-scratch x86_64 operating system kernel written in Rust and Assembly, designed with security as the #1 priority, low resource usage as #2, and usability as #3. The name "Indominus" is from Jurassic World — this OS is built to be unstoppable.

---

## Current Vision

INDOMINUS OS focuses on three core principles:

### 🛡️ Security First

The operating system is designed around strong isolation:

- User programs run separately from the kernel
- Ring 3 user-space execution
- Page fault isolation
- NX (No Execute) memory protection
- Future application sandboxing
- Future memory protection improvements

The goal is to make unsafe behavior fail safely instead of compromising the entire system.

### ⚡ Lightweight Performance

INDOMINUS OS aims to remain small and efficient:

- Minimal kernel footprint
- Custom memory management
- No unnecessary background services
- Direct hardware interaction
- Controlled resource usage

### 🧩 Native User Experience

Instead of relying on many external extensions, future versions aim to integrate useful features directly into the operating system:

- Native application isolation
- Intelligent window management
- Built-in recovery/versioning systems
- Lightweight customization engine
- Efficient system tools

---

## Current Status

**Phase:** Foundation Freeze (Phase 7 complete, Foundation Hardening complete)
**Stability:** 10/10 regression passes, all critical/high-severity bugs fixed
**Binary size:** 281.8 KB kernel, 51 KB bootloader

### What Works

| Subsystem | Status | Details |
|-----------|--------|---------|
| UEFI Boot | Complete | Custom bootloader, GPT partitioning, RSDP passthrough |
| GDT/TSS | Complete | Virtual-address GDT, per-CPU TSS, Ring 0/3 transitions |
| IDT | Complete | 256 vectors, exception handlers, keyboard IRQ |
| PMM | Complete | Bitmap allocator, reference counting, frame 0 protection |
| VMM | Complete | 4-level page tables, CoW, guard pages, identity-map hardening |
| Kernel Heap | Complete | Linked-list allocator, 4 MiB initial |
| ACPI | Complete | RSDP/XSDT/RSDT parsing, MADT, HPET, MCFG, WAET, BGRT |
| PCI | Complete | Full bus enumeration, BAR parsing, 6 devices detected |
| LAPIC/IOAPIC | Complete | Dynamic routing from MADT, timer at ~100 Hz |
| PIT Timer | Complete | Channel 0, 100 Hz periodic, vector 32 |
| Keyboard | Complete | PS/2 driver, ring buffer, interrupt-driven |
| Syscalls | Complete | 16 syscalls, negative-errno convention |
| ELF Loader | Complete | ELF64 parser, NX enforcement, kernel-space mapping protection |
| Process Mgmt | Complete | Spawn, fork, exec, exit, waitpid, zombie reaping |
| Scheduler | Complete | Preemptive round-robin, 5-tick quantum (50ms) |
| VFS | Complete | RAM filesystem, path resolution, file operations |
| Initrd | Complete | cpio newc parser, security-hardened |
| Pipe IPC | Complete | Ring buffer, blocking read/write, atomic operations |
| User Programs | Complete | 10 test binaries, Python ELF generator |

---

## Architecture

### Memory Layout

```
0x0000000000000000  BIOS IVT / BDA (1 KiB)         Frame 0 (never allocated)
0x0000000000000400  Free physical memory            PMM bitmap tracks this
0x0000000000100000  Kernel physical load            Bootloader places kernel here
0x0000000040000000  User space (Ring 3)             ELF segments loaded here
                    Stack: 0x7FFFFFFF0000
                    Guard: 0x7FFFFFFEB000
                    Max: 256 MiB per process
0x0000800000000000  Non-canonical gap (no access)
0xFFFFFFFF80000000  Kernel virtual (upper half)     Higher-half mapping
                    .text, .rodata, .data, .bss
                    Kernel heap, per-process stacks
0xFFFFFFFFC0000000  Kernel heap (4 MiB initial)
0xFFFFFFFFFFC00000  Kernel page tables (direct map)
```

### Process Memory Layout

```
0x00007FFFFFFF0000  Stack top (RSP starts here)
0x00007FFFFFFEF000  Stack page 1 (top, 4 KiB)
0x00007FFFFFFEE000  Stack page 2
0x00007FFFFFFED000  Stack page 3
0x00007FFFFFFEC000  Stack page 4 (bottom)
0x00007FFFFFFEB000  Guard page (PRESENT, no USER, no WRITABLE)
                    --- guard fault on overflow ---
         ...
0x0000000000400000  ELF segments (PT_LOAD)
         ...
0x0000000000400078  Entry point (typical)
```

### Boot Sequence

```
UEFI firmware
  -> indo-boot (UEFI application)
     Finds RSDP via UEFI config tables
     Loads kernel ELF from ESP
     Sets up identity + kernel page tables
     Passes BootInfo to kernel_main
   -> kernel_main (Rust)
      GDT init (Ring 0/3 segments, TSS)
      PMM init (bitmap allocator)
      PMM mark kernel physical range used
      CPU feature detection (NX, SMEP, SMAP, APIC)
      VMM init (4-level page tables, 4 GiB identity + kernel)
      Switch to virtual GDT
      Kernel heap init (4 MiB linked-list allocator)
      IDT init (256 vectors, exception handlers)
      ACPI init (RSDP -> XSDT -> MADT, HPET, etc.)
      PCI enumeration (6 devices in QEMU Q35)
      Interrupt init (LAPIC, IOAPIC, PIT from ACPI MADT)
      Keyboard init (PS/2 driver)
      Syscall init (MSRs, STAR/LSTAR/SFMASK)
      Harden identity map (NX on all identity pages)
      Process init (idle task, init/reaper PID 1)
      VFS init (RAM filesystem)
      Initrd load (cpio newc)
      Spawn user processes from VFS or test binaries
      Start scheduler -> HLT loop
        Timer IRQ -> schedule -> first process runs
```

### Context Switch Flow

```
Timer IRQ fires (every 50ms)
  -> Naked handler (assembly)
     Push 15 GP registers (RAX..R15)
     Call schedule(saved_rsp)
       on_tick() -> if quantum expired, switch_next()
       save_current_sp(saved_rsp)
       Mark old process Ready
       Find next Ready process
       Mark new process Running
       Switch CR3 to new PML4
       Update TSS RSP0
       Return new SP
     Save new SP into r12
     EOI to LAPIC (upper-half virtual address)
     mov rsp, r12 (switch to new process stack)
     Deferred CR3 switch (first dispatch only)
     Pop 15 GP registers
     iretq -> new process runs
```

Note: `swapgs` is used in the **syscall entry/exit path**, not the timer handler.
The timer handler runs entirely in Ring 0 (interrupt context).

---

## Security Model

### Ring Enforcement

- **Ring 0 (Kernel):** Full access to all memory, instructions, and MSRs
- **Ring 3 (User):** Only user-mapped pages, NX bit enforced, no kernel access
- **Transition:** SYSCALL/SYSRET (not INT 0x80), IRET for context switches

### Memory Protection

| Protection | Implementation |
|------------|----------------|
| NX (No Execute) | All data/stack/heap pages marked NX; identity map hardened |
| SMAP/SMEP | Enabled if CPU supports it |
| Guard pages | Stack overflow -> page fault -> process killed |
| Kernel isolation | Upper-half only; identity map NX-hardened after init |
| ELF validation | Entry point in user segment; segments below USER_SPACE_END |
| Frame 0 protection | Never allocated (BIOS IVT); explicit assert in free_frame |

### Syscall Security

- **Input validation:** All user pointers checked via `is_valid_user_range` + `is_user_buffer_mapped`
- **Error convention:** Negative errno values (matching Linux)
- **Unknown syscalls:** Return ENOSYS (no panic)
- **Buffer overflow prevention:** Length limits on path/string reads
- **Process isolation:** Each process has its own PML4; kernel entries shared, user entries private

---

## Syscall Reference (16 syscalls)

| # | Name | Args | Returns | Description |
|---|------|------|---------|-------------|
| 0 | write | fd, buf, len | bytes | Write to file descriptor |
| 1 | exit | code | never | Kill process, mark Zombie |
| 2 | yield | - | 0 | Yield CPU to next Ready |
| 3 | getpid | - | pid | Get current process ID |
| 4 | waitpid | child_pid | exit_code | Non-blocking wait (WNOHANG) |
| 5 | sleep | ticks | 0 | Sleep for N timer ticks |
| 6 | read | fd, buf, len | bytes | Read from file descriptor |
| 7 | pipe | - | 0 | Create pipe pair |
| 8 | fork | - | child_pid | Fork via Copy-on-Write |
| 9 | exec | path | - | Execute ELF binary |
| 10 | close | fd | 0 | Close file descriptor |
| 11 | dup | fd | new_fd | Duplicate FD to lowest slot |
| 12 | open | path | fd | Open file from VFS |
| 13 | lseek | fd, off, whence | pos | Seek in file |
| 14 | dup2 | oldfd, newfd | new_fd | Duplicate FD to specific slot |
| 15 | readdir | fd, buf, len | bytes | Read directory entries |

**Error convention:** All errors return negative errno. Userspace detects via `result > -4096UL`.

---

## Build & Run

### Prerequisites

- **Rust nightly** with `rust-src` component
- **QEMU** with UEFI support (OVMF)
- **Python 3** (for test generators)

### Commands

```powershell
# Build everything (kernel + bootloader + tests + initrd)
powershell -ExecutionPolicy Bypass -File "build.ps1" build

# Run in QEMU
powershell -ExecutionPolicy Bypass -File "build.ps1" run

# Run regression tests
python tools/regression_test.py --iterations 10 --timeout 45

# Generate test binaries
python tools/gen_comprehensive_test.py

# Generate userspace binaries
python tools/gen_userspace.py

# Build initrd archive
python tools/build_initrd.py --input userspace/rootfs --output indo-kernel/initrd.img
```

---

## Test Suite

### Automated Regression Tests

The `regression_test.py` runs the kernel in QEMU and checks serial output:

| Test | Pattern | Validates |
|------|---------|-----------|
| test1 | `TEST1_NORMAL_OK` + `TEST1_RESUMED_OK` | Yield and resume |
| test2 | `TEST2_MULTI_PID_OK` | Multi-process support |
| test3 | `TEST3_NULL_DEREF_BEFORE` | Null pointer page fault kill |
| test4 | `TEST4_INVALID_PTR_RESULT_OK` | Invalid pointer handling |
| test5 | `TEST5_UNMAPPED_RESULT_OK` | Unmapped memory access |
| test6 | `TEST6_NULL_PTR_RESULT_OK` | NULL pointer protection |
| test7 | `TEST7_INVALID_SYSCALL_RESULT_OK` | Unknown syscall ENOSYS |
| test8 | `TEST8_SLEEP_BEFORE` | Sleep/wake |
| test9 | `TEST9_GUARD_START` | Stack guard page fault |
| test10 | `TEST10_ERRNO_RESULT_OK` | Error number correctness |

Additional patterns: `RSDP`, `FACP`, `APIC`, `HPET`, `MCFG`, `WAET`, `BGRT` (ACPI), `6 devices` (PCI), `LAPIC`, `IOAPIC` (interrupts).

---

## Project Structure

```
indominus rex operating system/
  indo-boot/                    UEFI bootloader (Rust)
    src/main.rs                 Boots kernel, finds RSDP, page tables
  indo-kernel/                  Kernel (Rust, no_std)
    src/
      main.rs                   Entry, init sequence, test spawning
      acpi/                     ACPI table parsing
        mod.rs                  XSDT/RSDT discovery, global state
        rsdp.rs                 RSDP scanning (bootloader + memory)
        madt.rs                 MADT records (APIC, IRQ overrides)
      cpu.rs                    Feature detection (NX, SMEP, SMAP, APIC)
      debug.rs                  Debug utilities
      elf/mod.rs                ELF64 loader with security validation
      gdt.rs                    GDT/TSS setup, Ring 0/3 segments
      idt.rs                    IDT setup, exception handlers
      initrd.rs                 cpio newc parser (security-hardened)
      interrupts/
        mod.rs                  Interrupt subsystem init
        lapic.rs                Local APIC (MmioRegion-based)
        ioapic.rs               I/O APIC (MmioRegion-based)
        pit.rs                  PIT Channel 0 (100 Hz)
        dispatch.rs             IRQ dispatch table
      keyboard.rs               PS/2 keyboard driver
      memory/
        mod.rs                  Memory constants, heap init
        pmm.rs                  Physical Memory Manager (bitmap+refcount)
        vmm.rs                  Virtual Memory Manager (page tables, CoW)
      mmio/mod.rs               Generic MMIO framework
      panic.rs                  Panic handler
      pci/mod.rs                PCI bus enumeration
      process/
        mod.rs                  Process init, spawn_user, yield_now
        process.rs              Process struct, Drop impl
        scheduler.rs            Round-robin scheduler
        context_switch.rs       Timer handler, force_switch, kill_process
        idle.rs                 Idle process (HLT loop)
        init.rs                 Init/reaper process (PID 1)
        pipe.rs                 Pipe IPC (ring buffer, atomic ops)
      serial.rs                 Serial port output (COM1)
      sync_cell.rs              SyncUnsafeCell<T> for safe static mut access
      syscall/
        mod.rs                  16 syscalls, naked handler, MSR setup
        errno.rs                Negative errno definitions
      vfs/
        mod.rs                  VFS core (File, Inode, FileSystem traits)
        ramfs.rs                RAM filesystem
  tools/
    regression_test.py          Automated regression test suite
    gen_comprehensive_test.py   Generate 10 ELF64 test binaries
    gen_userspace.py            Python ELF generator
    build_initrd.py             cpio newc archive builder
  userspace/
    syscall/                    Indo syscall crate (no_std)
    rootfs/                     Root filesystem (init, shell, hello.txt)
  build.ps1                     Build script
  kernel.ld                     Kernel linker script
  indominus-x86_64.json         Target specification
```

---

## Bugs Fixed in Foundation Hardening

### Critical Fixes

| Bug | File | Description |
|-----|------|-------------|
| ELF kernel mapping | elf/mod.rs | Segments near 0x800000000000 could cross into kernel space; added virt_end validation after alignment |
| sys_exec use-after-free | syscall/mod.rs | Old PML4 freed before ELF load; fixed by creating new PML4 first, freeing old only on success |
| alloc_contiguous frame 0 | pmm.rs | BIOS IVT at 0x0; skipping frame 0 in contiguous allocator |
| Process Drop double-free | context_switch.rs | force_switch zeroed resources for ALL old processes including yielded ones; gated on dead_kstack != 0 |
| Guard page USER_ACCESSIBLE | syscall/mod.rs | Guard page in execve allowed user access; removed USER_ACCESSIBLE flag |
| alloc_contiguous REFCOUNTS | pmm.rs | Contiguous frames had refcount 0; added REFCOUNTS[frame] = 1 |
| free_frame frame 0 | pmm.rs | No check prevented freeing frame 0; added assert |
| Process Drop address space leak | process.rs | Reaped zombies never freed PML4/pages; Drop now calls free_user_address_space |
| sys_dup use-after-free | syscall/mod.rs | FsFile dup had no clone; rejected with EBADF until Arc-based handles |
| sys_pipe FD exhaustion leak | syscall/mod.rs | Pipe not freed when FD allocation failed; added free_pipe on error path |

### False Positives Confirmed

| Finding | Why It's Safe |
|---------|---------------|
| decref without VMM unmap | Both call sites (free_user_address_space, CoW) properly destroy PTEs via page table frame freeing |
| Scheduler lock ordering | All acquisitions happen with interrupts disabled; single lock, no deadlock possible |
| kill_process from page fault | Runs with IF=0 (interrupt gate); no preemption during cleanup |

---

## Known Limitations

| Issue | Severity | Notes |
|-------|----------|-------|
| Orphan processes never reaped | HIGH | Needs init/reaper process (Phase 8) |
| PID reuse allows cross-family reaping | HIGH | Needs PID generation counter (Phase 8) |
| sys_dup cannot handle FsFile | MEDIUM | Needs Arc\<dyn File\> ref counting (Phase 8) |
| sys_close doesn't free pipe slots | MEDIUM | Needs ref-counted pipes (Phase 8) |
| No kernel stack guard page | LOW | Heap overflow risk (Phase 9+) |
| No SMP support | LOW | Single-CPU only; all globals unsynchronized |
| REFCOUNTS overflow silent clamp at 255 | LOW | Theoretical only (Phase 9+) |

---

## Roadmap

### Phase 7: Foundation (COMPLETE)
- GDT/TSS/IDT, PMM, VMM, heap, timer, scheduler
- SYSCALL/SYSRET, ELF64 loading, Ring 3 execution
- Process lifecycle: spawn, fork, exec, exit, waitpid
- Page fault handling, Copy-on-Write, pipe IPC
- ACPI parsing, PCI enumeration, LAPIC/IOAPIC
- MMIO framework, VFS, initrd
- 10/10 automated regression passes

### Phase 8: Foundation Hardening (CURRENT)
- Security audit: PMM, VMM, Process, ELF, Syscalls (4 parallel audits)
- Fix 12 critical/high-severity bugs
- Comprehensive audit documentation in README
- [ ] Init/reaper process for orphan adoption
- [ ] PID generation counter (prevent reuse)
- [ ] Arc-based file handle ref counting (enable safe dup)
- [ ] Ref-counted pipe slots (enable safe close)
- [ ] Fix VMM copy_user_pages partial failure leak
- [ ] Fix ELF MAX_USER_MEM per-segment (make cumulative)
- [ ] Fix PMM free_frame unconditional refcount destroy

### Phase 9: Userspace Environment (NEXT)
- [ ] Shell (indosh) with command parsing
- [ ] File creation and deletion
- [ ] Process spawning from shell
- [ ] Standard library for userspace
- [ ] Console/TTY driver

### Phase 10: Device Drivers
- [ ] VGA/framebuffer text mode
- [ ] ATA/AHCI disk driver
- [ ] Keyboard layout (US QWERTY)
- [ ] Mouse (PS/2)

### Phase 11: Networking
- [ ] e1000e NIC driver
- [ ] ARP, ICMP, TCP/IP stack
- [ ] Socket API

### Phase 12: Advanced Features
- [ ] SMP (multi-core)
- [ ] Shared memory
- [ ] Signals
- [ ] Dynamic linking

---

## Contributing

This is a personal learning project. Contributions are welcome but please open an issue first to discuss proposed changes.

## License

This project is currently not licensed. All rights reserved by the author.
