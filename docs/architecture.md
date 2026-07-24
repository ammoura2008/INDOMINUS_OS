# INDOMINUS OS — Architecture

## Virtual Address Map

### Kernel Space (Upper Half, PML4 entries 256–511)

```text
0xFFFF_FFFF_FFFF_FFFF ─────────────────────────────────── Top of memory
                    ...
0xFFFF_FFFF_FEE0_0000 ─────────────────────────────────── LAPIC MMIO (1 page)
0xFFFF_FFFF_FED0_0000                                       (mapped from phys 0xFEE00000)
                    ...
0xFFFF_FFFF_C000_0000 ─────────────────────────────────── Kernel heap start (4 MiB)
0xFFFF_FFFF_BFFF_FFFF                                       Kernel heap end
                    ...
0xFFFF_FFFF_8000_0000 ─────────────────────────────────── Kernel .text start
                    ...                                     Kernel .text, .rodata, .data, .bss
0xFFFF_FFFF_800X_XXXX                                       Kernel physical end
```

### User Space (Lower Half, PML4 entries 0–255)

```text
0x0000_7FFF_FFFF_0000 ─────────────────────────────────── User stack top (grows down)
0x0000_7FFF_FFFE_F000                                       Stack page 1 (top)
0x0000_7FFF_FFFE_E000                                       Stack page 2
0x0000_7FFF_FFFE_D000                                       Stack page 3
0x0000_7FFF_FFFE_C000                                       Stack page 4 (bottom)
0x0000_7FFF_FFFE_B000                                       Guard page (no USER, no WRITABLE)
                    ...
0x0000_0000_0060_0000 ─────────────────────────────────── User code end (top of 6 MiB)
0x0000_0000_0040_0000 ─────────────────────────────────── User code base (ELF load)
0x0000_0000_0000_0000                                       NULL guard (unmapped)
```

### Key Constants (`memory/mod.rs`)

| Symbol | Value | Description |
|--------|-------|-------------|
| `KERNEL_VIRT_BASE` | `0xFFFFFFFF80000000` | Kernel virtual base (-2 GiB) |
| `KERNEL_HEAP_BASE` | `0xFFFFFFFFC0000000` | Kernel heap start |
| `KERNEL_HEAP_INITIAL_SIZE` | `4 * 1024 * 1024` | 4 MiB initial heap |
| `USER_STACK_TOP` | `0x00007FFFFFFF0000` | User stack top |
| `PAGE_SIZE` | `4096` | 4 KiB page |

Note: Kernel stack, user code base, and per-process kernel stack size are allocated dynamically or defined in their respective subsystems (not as global constants).

### Page Table Layout

Kernel page tables (`vmm::init_kernel_page_tables`) create:

1. **Kernel higher-half mapping:** physical kernel pages → virtual `0xFFFFFFFF80000000+`
2. **Kernel heap:** physical frames allocated by PMM → virtual `0xFFFFFFFFC0000000..0xFFFFFFFFC0400000`
3. **Identity map:** first 4 GiB (virtual == physical) for safe CR3 transition
4. **LAPIC MMIO:** physical `0xFEE00000` → virtual `0xFFFFFFFFFEE00000` (1 page, shared via PML4 entries 256–511)

User PML4s (`vmm::create_user_pml4`) copy **only PML4 entries 256–511** from the kernel PML4. This means:
- Kernel code, data, heap, stack, and LAPIC are accessible in all PML4s
- The identity map (PML4 entry 0) is NOT present in user PML4s
- User pages (PML4 entries 0–255) are mapped separately per process

### PIC (Position-Independent Code) Address Model

With PIC, `&raw const` yields **physical** addresses (relocated by bootloader via `R_X86_64_RELATIVE`).
Function pointers in IDT/GDT/TSS entries must be converted to virtual addresses using `phys_to_kernel_virt()`.

```text
virt = phys + (KERNEL_VIRT_BASE - kernel_phys_start)
```

This applies to:
- IDT handler addresses (`handler_virt!` macro)
- GDT/TSS base addresses (`switch_gdt_to_virtual`)
- IST entries, double-fault stack address
- All static references used from Ring 3 (where identity map is absent)

## Boot Initialization Sequence

`kernel_main()` executes the following in order:

```
 1. Set KERNEL_PHYS_START from BootInfo
 2. gdt::init()                      — Build GDT, load TSS, load segment registers
 3. pmm::init(&memory_map)           — Initialize physical memory manager (bitmap allocator)
 4. pmm::mark_region_used()          — Reserve kernel's physical memory range (from BootInfo)
 5. cpu::detect()                    — Detect CPU features (NX, SMEP, SMAP, APIC)
 6. cpu::print_features()            — Print detected features to serial
 7. cpu::enable_smep_smap()          — Enable SMEP and SMAP if supported
 8. vmm::init_kernel_page_tables()   — Create new PML4 with higher-half + identity map
 9. vmm::switch_page_table()         — Load new PML4 into CR3 (identity map still active)
10. gdt::switch_gdt_to_virtual()     — Patch GDT/TSS to virtual addresses, reload GDTR+TR
11. init_heap()                      — Initialize kernel heap allocator (4 MiB)
12. idt::init()                      — Build IDT with virtual handler addresses
13. acpi::init()                     — Parse ACPI tables (RSDP → XSDT → MADT, HPET, etc.)
14. pci::enumerate()                 — Enumerate PCI devices on all buses
15. phase91_block_test()             — Verify block device abstraction (RAM disk I/O)
16. interrupts::init()               — Initialize LAPIC, PIT, IO-APIC (from ACPI MADT)
17. keyboard::init()                 — Initialize PS/2 keyboard driver
18. syscall::init()                  — Initialize syscall MSRs (STAR, LSTAR, SFMASK, EFER)
19. vmm::harden_identity_map()       — Set NX on all identity-mapped pages
20. process::init()                  — Initialize scheduler, create PID 0 (idle), PID 1 (init/reaper)
21. vfs::init()                      — Initialize VFS (RAM filesystem)
22. initrd::load_initrd()            — Load initrd (cpio newc archive)
23. Spawn user processes             — Load ELF from VFS or test binaries (PID 2+)
24. process::start_scheduler()       — Enable PIT, start timer-driven context switching
```

## GDT / TSS Layout

### GDT Entries

```text
Index   Selector   Description
─────   ────────   ──────────────────────────────────────
  0      0x00      Null descriptor (required by x86 spec)
  1      0x08      Kernel code (64-bit, Ring 0, executable)
  2      0x10      Kernel data (Ring 0, writable)
  3      0x18      User code (64-bit, Ring 3, executable)
  4      0x20      User data (Ring 3, writable)
  5+6    0x28      TSS descriptor (128-bit, spans two entries)
```

### Segment Selectors

| Selector | Value | Index | RPL | Usage |
|----------|-------|-------|-----|-------|
| `kernel_code` | `0x08` | 1 | 0 | Kernel CS |
| `kernel_data` | `0x10` | 2 | 0 | Kernel DS/SS |
| `user_code` | `0x1B` | 3 | 3 | User CS (Ring 3) |
| `user_data` | `0x23` | 4 | 3 | User DS/SS (Ring 3) |
| `tss` | `0x28` | 5 | 0 | TSS (loaded by `ltr`) |

### TSS Layout

```text
Offset   Field                Description
──────   ─────                ────────────────────────────────────
  0x00   Reserved             Always 0
  0x04   RSP0                Ring 0 stack pointer (set per-process on context switch)
  0x0C   RSP1                Ring 1 stack pointer (unused)
  0x14   RSP2                Ring 2 stack pointer (unused)
  0x1C   Reserved            Always 0
  0x20   IST[0]              Double-fault stack top (16 KiB, `DOUBLE_FAULT_STACK`)
  0x28   IST[1..6]           Unused IST entries
  0x5C   Reserved            Always 0
  0x66   I/O Map Base        0 = no I/O permission bitmap
```

### TSS Address Patching

With PIC, `Descriptor::tss_segment(&TSS)` bakes the **physical** address of TSS into the GDT descriptor.
After CR3 switches to user PML4 (no identity map), the physical address is unmapped → the CPU cannot access TSS → double fault.

`gdt::switch_gdt_to_virtual()` patches the TSS descriptor's base address in-place:
- Entry 5 (TSS low, offset 40): bits 16..40 = base[0:23], bits 56..64 = base[24:31]
- Entry 6 (TSS high, offset 48): bits 0..32 = base[32:63]
- Also clears bit 41 (busy bit) in Entry 5 before re-loading TR (first `ltr` marks TSS as busy)

### GDT Initialization (Two-Phase)

```text
Phase 1 — gdt::init() (before CR3 switch)
  - Build GDT with physical TSS address
  - Load GDT (GDTR base = physical address, identity-mapped)
  - Load CS, DS, ES, FS, GS, SS, TR

Phase 2 — gdt::switch_gdt_to_virtual() (after CR3 switch)
  - Patch TSS descriptor base to kernel virtual address
  - Clear TSS busy bit (bit 41)
  - Reload GDTR with virtual address
  - Reload TR with patched TSS descriptor
```

## Interrupt Handling

### IDT Vector Layout

| Vector | Name | Type | Stack | Behavior |
|--------|------|------|-------|----------|
| 0 | #DE Division Error | Exception | IST[1] | Fatal (halt) |
| 3 | #BP Breakpoint | Exception | — | Log and continue |
| 8 | #DF Double Fault | Exception | IST[0] | Fatal with full diagnostics |
| 10 | #TS Invalid TSS | Exception | — | Fatal with error code |
| 13 | #GP General Protection | Exception | — | Fatal with error code + diagnostics |
| 14 | #PF Page Fault | Exception | — | Fatal (halt) |
| 12 | #SS Stack Segment Fault | Exception | — | Fatal with error code |
| 32 | PIT Timer | IRQ | — | Context switch (naked handler) |
| 33–47 | Hardware IRQs | IRQ | — | `dispatch::dispatch(vector)` |

### Exception Frame (CPU-Pushed)

For Ring 0 → Ring 0 (no CPL change):
```text
[RSP+0]  = error code (for exceptions that have one)
[RSP+8]  = RIP
[RSP+16] = CS
[RSP+24] = RFLAGS
```

For Ring 3 → Ring 0 (CPL change), the CPU also pushes:
```text
[RSP+32] = RSP
[RSP+40] = SS
```

### Timer Interrupt (Vector 32) — Naked Handler

The timer interrupt handler is a `#[naked]` function that performs context switching directly in assembly:

```text
1. Write 'T' to QEMU debugcon port (0xE9) — Ring 3 tick detection
2. Write [TICK] marker to serial
3. Push 15 GP registers (R15 → RAX, canonical frame)
4. call schedule(saved_rsp) → returns new SP in RAX
5. Write [SWITCH] marker to serial
6. mov r12, rax (save new SP)
7. EOI to LAPIC at 0xFFFFFFFFFEE000B0 (upper-half address)
8. mov rsp, r12 (switch to new process stack)
9. Pop 15 GP registers (RAX → R15)
10. iretq
```

### Exception Handlers

All exception handlers use the `handler_virt!` macro to convert physical handler addresses to kernel virtual addresses for IDT entries.

Double-fault handler (`idt.rs:double_fault_handler`):
- Uses IST[0] (separate 16 KiB stack) to prevent recursive stack overflow
- Prints DF frame (RIP, CS, RFLAGS, RSP, SS, CR2, CR3)
- When `DEBUG_KERNEL`: prints captured diagnostics (SAVED_RSP, RSP_AFTER_LOAD, RSP_BEFORE_IRETQ, frame origin, GDT selector values, CS cross-check)
- When `DEBUG_KERNEL`: dumps 20 qwords at RSP_BEFORE_IRETQ and 20 qwords at CAPTURED_RSP for IRET frame analysis

Page fault handler (`idt.rs:page_fault_handler`):
- Reads CR2 (faulting address), RIP, CS, RFLAGS, error code
- Prints all diagnostics to serial
- Currently halts (no user PF recovery yet — planned for Phase 5.4)

### Interrupt Dispatch

Hardware IRQs (vectors 33–47) use `extern "x86-interrupt"` handlers generated by the `irq_handler!` macro. Each calls `dispatch::dispatch(vector)`, which:
1. Calls the registered handler
2. Sends EOI to LAPIC

### LAPIC Address

Physical `0xFEE00000` is mapped to virtual `0xFFFFFFFFFEE00000` in the kernel's page table (via `vmm::init_kernel_page_tables`). The upper-half mapping is shared by all PML4s (entries 256–511), so it survives CR3 switches to user PML4s (which lack the identity map).

LAPIC EOI register: `0xFFFFFFFFFEE000B0` (physical `0xFEE000B0`).

## Block Device Layer (Phase 9.1)

### Purpose

The block device layer is the boundary between storage hardware and filesystems. It provides a hardware-agnostic interface so that filesystems (FAT32, ext2, etc.) can operate without knowing whether the underlying device is AHCI, NVMe, VirtIO, USB storage, or a RAM disk.

### Architecture

```text
User Applications
       ↓
Syscalls / File Descriptor API
       ↓
Process File Descriptor Table
       ↓
VFS
       ↓
Filesystem Implementation (FAT32, ext2, etc.)
       ↓
Block Device Layer  ← ← ← YOU ARE HERE
       ↓
Hardware Driver (AHCI, NVMe, VirtIO, USB, RAM)
```

### BlockDevice Trait (`block/mod.rs`)

```rust
pub trait BlockDevice: Send + Sync {
    fn read_sector(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError>;
    fn write_sector(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
    fn name(&self) -> &str;
}
```

Key design decisions:
- **Sector-based API:** Initial implementation assumes 512-byte sectors. Callers must always use `sector_size()` — never hardcode 512.
- **`&self` for writes:** Real hardware devices mutate state through MMIO registers despite shared references. The RAM disk uses `Mutex<Vec<u8>>` for interior mutability.
- **`Send + Sync`:** Required for `Arc<dyn BlockDevice>` across threads.
- **Single-sector operations:** Keeps the API simple. Multi-sector transfers can be built on top.

### BlockError Enum

| Variant | errno | Description |
|---------|-------|-------------|
| `InvalidBufferSize` | EINVAL (22) | Buffer size doesn't match sector size |
| `OutOfBounds` | EINVAL (22) | LBA >= total_sectors |
| `DeviceNotReady` | ENODEV (19) | Device not initialized |
| `IoError` | EIO (5) | Hardware read/write failure |
| `ReadOnly` | EROFS (30) | Device is read-only |
| `TooManyDevices` | EMFILE (24) | Registry full (max 16) |
| `DeviceAlreadyExists` | EEXIST (17) | Device already registered |
| `NoSuchDevice` | ENODEV (19) | Device ID not found |

### BlockDeviceRegistry (`block/registry.rs`)

Global registry for block devices. Devices are identified by numeric IDs (0..15).

```rust
pub fn register_device(device: Arc<dyn BlockDevice>) -> Result<usize, BlockError>;
pub fn get_device(id: usize) -> Option<Arc<dyn BlockDevice>>;
pub fn unregister_device(id: usize) -> Option<Arc<dyn BlockDevice>>;
```

Uses `spin::Mutex` for safe concurrent access, matching the PCI device enumeration pattern.

### RAM Disk (`block/ramdisk.rs`)

In-memory block device for development and testing. Allocates a fixed heap buffer (max 16 MiB) and exposes it as a block device.

```rust
let rd = Arc::new(RamDisk::new(8, 512)); // 8 sectors, 512 bytes each
crate::block::registry::register_device(rd.clone())?;
```

The RAM disk proves that higher layers can operate without knowing the underlying storage hardware. Future drivers (AHCI, NVMe, VirtIO) will implement the same `BlockDevice` trait.

### Validation Rules

The block device layer treats all inputs as untrusted:
- Buffer sizes are validated against `sector_size()`
- LBAs are validated against `total_sectors()`
- Integer overflow is checked in offset calculations (`checked_mul`)
- Errors are returned (not panicked) for invalid parameters

### Future Integration

```
AHCI driver    → implements BlockDevice → registered in BlockDeviceRegistry
NVMe driver    → implements BlockDevice → registered in BlockDeviceRegistry
VirtIO driver  → implements BlockDevice → registered in BlockDeviceRegistry
USB driver     → implements BlockDevice → registered in BlockDeviceRegistry
FAT32          → calls read_sector/write_sector via Arc<dyn BlockDevice>
ext2           → calls read_sector/write_sector via Arc<dyn BlockDevice>
procfs/devfs   → filesystem-only, no block device needed
```

## AHCI Storage Driver (Phase 9.3)

### Purpose

Provides SATA disk access via the AHCI (Advanced Host Controller Interface) specification. Implements the `BlockDevice` trait, making SATA disks available to the VFS and filesystem layers.

### Architecture

```text
┌─────────────────────────────────────────────────────────┐
│ AHCI Driver (ahci/mod.rs)                               │
│                                                         │
│  AhciDisk { hba_phys, port: Mutex<AhciPort>, name }    │
│       ↓ implements                                      │
│  BlockDevice trait (read_sector, write_sector)          │
│       ↓ registered in                                   │
│  BlockDeviceRegistry (global, spin::Mutex)              │
└─────────────────────────────────────────────────────────┘
       ↑ MMIO access                 ↑ DMA via identity mapping
       ↓                             ↓
┌──────────────────────┐   ┌──────────────────────────────┐
│ HBA Registers        │   │ DMA Structures (per port)    │
│ (ahci/hba.rs)        │   │                              │
│                      │   │ Command List (32×32B headers) │
│ GHC, CAP, IS, PI, VS │   │ Received FIS (256B)          │
│ Port: CLB, FB, IS,   │   │ Command Table (CFIS + PRDT)  │
│ CMD, TFD, SIG, SSTS, │   │ DMA Buffer (4KB sector data) │
│ SCTL, SERR, SACT, CI │   │                              │
└──────────────────────┘   └──────────────────────────────┘
       ↑ MMIO (upper-half mapped)    ↑ Physical (PMM, identity-mapped)
```

### Initialization Sequence

1. **PCI scan**: Find controller at class=0x01, subclass=0x06, prog_if=0x01|0x02
2. **Enable bus mastering**: Set PCI command bits 1-2
3. **Map ABAR**: BAR5 → upper-half MMIO via `MmioRegion::new()`
4. **HBA reset**: Write GHC.HR=1, wait for clear
5. **Enable AHCI mode**: Set GHC.AE (bit 31)
6. **Port detection**: Read SSTS.DET for each port (0x03 = device present)
7. **Port init**: Stop commands, set CLB/FB, enable FRE, start ST, wait CR
8. **IDENTIFY DEVICE**: Issue 0xEC command, parse 512-byte response
9. **Register**: Add to `BlockDeviceRegistry`

### AHCI DMA Model

- DMA buffers allocated from PMM (identity-mapped: phys == virt)
- Command List, FIS, Command Table: all PMM-allocated, identity-mapped
- PRDT entries reference physical addresses (for AHCI controller DMA)
- CPU accesses buffers via the same identity-mapped virtual address

### Key Files

| File | Purpose |
|---|---|
| `ahci/mod.rs` | AhciDisk struct, init, port management, command issuing |
| `ahci/hba.rs` | HBA register definitions, CmdHeader/PrdtEntry structs |

### QEMU Verification

- Q35 machine: ICH9 AHCI at PCI 0:1F.2 (vendor=0x8086, dev=0x2922)
- QEMU's `-drive format=raw,file=fat:rw:DIR` attaches drive to AHCI port 0
- Verified: HBA reset, IDENTIFY DEVICE (0xFC000 sectors = 504 MB), MBR read (0x55AA)

### AHCI Command Completion (Phase 9.4 — final design)

Command success/failure is determined **solely** by AHCI/ATA hardware status:

1. **PxCI cleared** — HBA finished processing the command slot
2. **PxIS.TFES == 0** — no Task File Error Status
3. **PxTFD.ERR == 0** — no ATA error bits
4. **PxTFD.DF == 0** — no device fault

If all four conditions hold, the command succeeded and the DMA buffer contains valid data. The DMA probe pattern (`[0xDE, 0xAD, 0xBE, 0xEF]`) is written before each read as **diagnostic-only instrumentation** — it is never used as a success/failure condition.

**Why not probe-based completion:** Real disk data can coincidentally contain any byte value, including probe pattern bytes. Requiring all bytes to differ from the sentinel (`&&` check) produces false negatives when real sector data coincidentally matches one or more bytes. On x86-64, DMA is cache-coherent (HBA snoops CPU cache via MESI), so partial DMA does not occur — if the command completed successfully, the entire buffer was written.

### TFES Recovery

After TFES, the HBA's command engine is in a degraded state where PxCI writes are silently accepted but no DMA occurs. Full recovery requires (per AHCI spec §6.2.2):

1. Stop command processing: PxCMD.ST = 0, wait PxCMD.CR = 0
2. Stop FIS receive: PxCMD.FRE = 0, wait bit 14 = 0
3. Wait for TFD.BSY/DRQ to clear (drive idle)
4. Restart FIS receive: PxCMD.FRE = 1, wait bit 14 = 1
5. Restart command processing: PxCMD.ST = 1, wait PxCMD.CR = 1

Maximum 8 attempts per command. Recovery is bounded and deterministic.

## Syscall ABI

### Overview

INDOMINUS uses the `syscall`/`sysret` mechanism for user→kernel transitions.
The CPU saves RIP→RCX and RFLAGS→R11, then jumps to the LSTAR entry point.
Stack switching is manual (via `swapgs` + per-CPU data).

### Calling Convention

| Register | Purpose |
|----------|---------|
| RAX | Syscall number (0=sys_write, 1=sys_exit, 2=sys_yield, 3=sys_getpid) |
| RDI | Arg0 |
| RSI | Arg1 |
| RDX | Arg2 |
| R10 | Arg3 |
| R8  | Arg4 |
| R9  | Arg5 |

Return value in RAX. On error, RAX = `u64::MAX` (-ENOSYS).

### Canonical SyscallFrame Layout

Both the timer interrupt handler and the syscall entry handler save/restore
GP registers in the **same canonical order**. This frame is 15 qwords (120 bytes),
followed by the 5-qword IRET frame (40 bytes).

```
Offset  Size  Register  Notes
──────  ────  ────────  ──────────────────────────────────
  +0     8    RAX       Syscall number / return value
  +8     8    RBX
 +16     8    RCX       User RIP (saved by CPU on syscall)
 +24     8    RDX       Arg2
 +32     8    RSI       Arg1
 +40     8    RDI       Arg0
 +48     8    RBP
 +56     8    R8        Arg4
 +64     8    R9        Arg5
 +72     8    R10       Arg3
 +80     8    R11       User RFLAGS (saved by CPU on syscall)
 +88     8    R12
 +96     8    R13
+104     8    R14
+112     8    R15
─────── IRET frame (read by `iretq`) ───────
+120     8    RIP
+128     8    CS
+136     8    RFLAGS
+144     8    RSP       (only on privilege-level change)
+152     8    SS        (only on privilege-level change)
```

### Push/Pop Order

**Save (push order):** R15 first → RAX last.
This places RAX at `[RSP+0]` (lowest address = top of stack).

```asm
push r15    ; highest address
push r14
...
push rax    ; lowest address = RSP
```

**Restore (pop order):** RAX first → R15 last.

```asm
pop rax     ; reads [RSP+0]
pop rbx     ; reads [RSP+8]
...
pop r15     ; reads [RSP+112]
```

### Frame Setup (Initial Process Spawn)

`setup_initial_stack_frame_kernel` and `setup_initial_stack_frame_user` in
`process.rs` write the frame in the same order as the pop sequence:

```
frame[0]  = RAX = 0
frame[1]  = RBX = 0
...
frame[14] = R15 = 0
frame[15] = RIP (entry point)
frame[16] = CS  (kernel: 0x08, user: 0x1B)
frame[17] = RFLAGS = 0x202 (IF=1)
frame[18] = RSP (kernel: stack_top, user: user_rsp)
frame[19] = SS  (kernel: 0x10, user: 0x23)
```

### Syscall Entry Flow (`syscall_entry`)

1. `swapgs` — switch to kernel GSBase (per-CPU data)
2. `mov gs:[0], rsp` — save user RSP
3. `mov rsp, gs:[8]` — load kernel RSP
4. Push 15 GP registers (R15→RAX) onto kernel stack
5. `mov rdi, rsp` — pass frame pointer to Rust
6. `call syscall_dispatch`
7. Check `gs:[16]` (force_switch flag)
   - If 0: pop 15 GP registers, `swapgs`, `sysretq`
   - If 1: construct IRET frame, call `schedule()`, switch stack, pop, `iretq`

### Timer Interrupt Flow (`timer_interrupt_handler`)

1. Push 15 GP registers (R15→RAX)
2. `call schedule(saved_rsp)` → returns new SP
3. EOI to LAPIC
4. `mov rsp, r12` — switch to new process stack
5. Pop 15 GP registers (RAX→R15)
6. `iretq`

### Per-CPU Data (GS-relative)

| Offset | Field | Description |
|--------|-------|-------------|
| 0 | user_rsp | User RSP saved on syscall entry |
| 8 | kernel_rsp | Top of current process's kernel stack |
| 16 | force_switch | 1 = context switch after syscall, 0 = sysret |

KERNEL_GS_BASE = `phys_to_kernel_virt(&raw const PER_CPU)` (higher-half, mapped in all PML4s).
GS_BASE = 0 (user mode).

### Key Invariants

1. **Frame layout is canonical.** Both timer handler and syscall entry use the
   exact same push/pop order. The frame setup code writes in pop order.
2. **IRET frame follows GP frame.** Always at offset +120 from frame base.
3. **Higher-half addresses only.** After CR3 switch to user PML4, all kernel
   code/data/LAPIC/per-CPU use higher-half virtual addresses (entries 256–511).
4. **No identity map in user PML4.** Only upper-half entries are copied.

### Current Syscall Table (16 syscalls)

| Num | Name | Args | Returns |
|-----|------|------|---------|
| 0 | sys_write | fd, buf_ptr, count | bytes written or u64::MAX |
| 1 | sys_exit | exit_code | never (force_switch) |
| 2 | sys_yield | — | 0 (force_switch) |
| 3 | sys_getpid | — | current PID |
| 4 | sys_waitpid | child_pid | exit_code or u64::MAX |
| 5 | sys_sleep | ticks | 0 |
| 6 | sys_read | fd, buf_ptr, count | bytes read or u64::MAX |
| 7 | sys_pipe | — | 0 or u64::MAX |
| 8 | sys_fork | — | child_pid or u64::MAX |
| 9 | sys_exec | path_ptr | never (force_switch) or u64::MAX |
| 10 | sys_close | fd | 0 or u64::MAX |
| 11 | sys_dup | fd | new_fd or u64::MAX |
| 12 | sys_open | path_ptr, flags | fd or u64::MAX |
| 13 | sys_lseek | fd, offset, whence | position or u64::MAX |
| 14 | sys_dup2 | oldfd, newfd | new_fd or u64::MAX |
| 15 | sys_readdir | fd, buf_ptr, count | bytes read or u64::MAX |

## VFS Core & File Descriptor Model (Phase 9.2)

### Overview

The VFS provides a unified, filesystem-independent view. Every file operation
flows through the FD table → VFS → filesystem → block device chain.

### Core Traits (`vfs/mod.rs`)

```rust
trait Inode {
    fn inode_number(&self) -> usize;
    fn name(&self) -> &str;
    fn file_type(&self) -> FileType;
    fn size(&self) -> usize;
    fn open(&self) -> Result<Arc<Mutex<Box<dyn File>>>, VfsError>;
}

trait File: Send + Sync {
    fn read(&self, buf: &mut [u8], offset: usize) -> Result<usize, VfsError>;
    fn write(&self, data: &[u8], offset: usize) -> Result<usize, VfsError>;
    fn seek(&self, pos: SeekFrom) -> Result<usize, VfsError>;
    fn truncate(&self, size: usize) -> Result<(), VfsError>;
    fn close(&self);
}

trait FileSystem: Send + Sync {
    fn name(&self) -> &str;
    fn root(&self) -> Arc<Mutex<dyn Inode>>;
    fn open(&self, path: &str, flags: u32) -> Result<Arc<Mutex<dyn Inode>>, VfsError>;
    fn create(&self, path: &str) -> Result<Arc<Mutex<dyn Inode>>, VfsError>;
    fn ls(&self, path: &str) -> Result<Vec<(String, FileType)>, VfsError>;
}
```

### File Descriptor Model (`process/process.rs`)

```rust
enum FdType {
    File(Arc<Mutex<Box<dyn File>>>),
    Pipe(Arc<Pipe>),
    None,
}

struct FileDescriptor {
    fd_type: FdType,
    ref_count: u8,
    position: usize,
}
```

- Max 8 FDs per process, 16 global file handles
- FD 0 = stdin (pipe), FD 1 = stdout (pipe)
- Ref-counting supports dup/dup2/fork/inheritance
- Open flags: O_RDONLY (0), O_WRONLY (1), O_RDWR (2), O_CREAT (0x40), O_TRUNC (0x200), O_CLOEXEC (0x80000)

### FD Semantics (POSIX-compatible)

**dup2(oldfd, newfd):**
- Both FDs share the same open-file description (same `Arc<Mutex<Box<dyn File>>>`)
- Shared state: file offset, status flags — read/write on either FD advances the shared offset
- close-on-exec flag is cleared on newfd (POSIX: dup never inherits CLOEXEC)
- Ref-counted: closing one FD does not affect the other

**Independent open() calls:**
- Each open() creates a new `RamFileHandle` with `pos: 0`
- The underlying data (`Arc<Mutex<Vec<u8>>>`) is shared (same file data)
- The file position is independent — read/write on one does not affect the other

**fork():**
- Parent and child share the same open-file descriptions (Arc refcount++)
- File offsets are shared — read/write by one process advances the other's offset
- This is correct POSIX behavior (not CoW for file offsets)

**exec():**
- Only FDs with the O_CLOEXEC flag (bit 0 of fd_flags) are closed
- FDs without O_CLOEXEC are inherited by the new program
- FDs 0, 1, 2 (stdin/stdout/stderr) are never closed by exec
- Default: O_CLOEXEC is clear (FDs are inherited across exec)
- dup/dup2 always clear O_CLOEXEC on the new FD

### FD Lifecycle

```
open() → allocates fd_slot + file_handles slot → FdType::FsFile { index }
  ↓
dup2(old, new) → copies FdType, clears fd_flags[new] → both share same handle
  ↓
fork() → clones fd_types + file_handles (Arc refcount++) → shared file descriptions
  ↓
exec() → closes FDs with O_CLOEXEC set → inherited FDs remain open
  ↓
close() → if last reference, drops Arc → frees file handle
  ↓
Process::drop() → closes all FDs, decrements pipe refcounts
```

### VFS Flow

```
Userspace: open(path) → syscall(SYS_OPEN)
  → sys_open(path_ptr, flags)
  → VFS::open(path, flags)
    → if O_CREAT: RamFs::create(path) → FileEntry → Arc<File>
    → RamFs::open(path) → FileEntry → Arc<File>
  → process.install_file_handle(Arc<File>)
  → returns fd index

Userspace: write(fd, buf) → syscall(SYS_WRITE)
  → sys_write(fd, buf)
  → process.get_file_handle(fd) → Arc<File>
  → file.write(buf) → updates inode size

Userspace: read(fd, buf) → syscall(SYS_READ)
  → sys_read(fd, buf)
  → process.get_file_handle(fd) → Arc<File>
  → file.read(buf, offset) → data
  → process.file_handles[fd].position += bytes_read
```

## Process Lifecycle

### State Machine

```text
                    spawn()
                       │
                       ▼
    ┌──────────────────────────────────┐
    │            READY                  │
    │  (in run queue, waiting for CPU) │
    └──────────┬───────────────────────┘
               │
          dispatch()
               │
               ▼
    ┌──────────────────────────────────┐
    │           RUNNING                 │
    │  (currently executing on CPU)    │
    └──────┬────────────────┬──────────┘
           │                │
      timer tick       sys_exit()
      (quantum expired)     │
           │                ▼
           │    ┌──────────────────────────────────┐
           │    │           ZOMBIE                  │
           │    │  (exited, resources not freed)    │
           │    └──────────────────────────────────┘
           │
           ▼
    ┌──────────────────────────────────┐
    │            READY                 │
    │  (returned to run queue)         │
    └──────────────────────────────────┘
```

### State Transitions

| From | To | Trigger | Code |
|------|----|---------|------|
| — | READY | `spawn_idle()` / `spawn_kernel()` / `spawn_user()` | `Scheduler::spawn*()` |
| READY | RUNNING | `dispatch_first()` or `switch_next()` | `Scheduler::switch_next*()` |
| RUNNING | READY | Timer quantum expires | `switch_next()` |
| RUNNING | Zombie | `sys_exit()` | `sys_exit()` |
| Zombie | — | Never returns to RUNNING | `find_next_ready()` skips non-Ready |

### Key Rules

1. **Zombie processes are never scheduled.** `find_next_ready()` only returns
   processes in `Ready` state. A Zombie process is permanently excluded.
2. **force_switch always yields.** `schedule_force()` (used by sys_exit, sys_yield)
   calls `switch_next_force()` which always performs a context switch, regardless
   of the time quantum.
3. **Timer-driven preemption is quantum-gated.** `schedule()` → `on_tick()` only
   calls `switch_next()` when the quantum expires (every 5 ticks = 50ms).

### Scheduler Selection

`find_next_ready(after_pid)` performs round-robin scanning:

1. **Pass 1:** Scan from `after_pid+1` to `after_pid`, skipping PID 0 (idle).
   Return the first `Ready` process found.
2. **Pass 2:** If no non-idle Ready process exists, return PID 0 (idle) as fallback.
3. **Return None** if no process is ready at all (should never happen with idle).

### Process Table

Maximum 8 concurrent processes (`MAX_PROCESSES = 8`).

| PID | Role | Kernel Stack | Page Table |
|-----|------|-------------|------------|
| 0 | Idle (HLT loop) | Boot stack | Kernel PML4 |
| 1 | Init/reaper (kernel-mode) | Heap-allocated | Kernel PML4 |
| 2+ | User processes (shell, test binaries) | Heap-allocated | Per-process PML4 |

## DEBUG_KERNEL — Diagnostic Gating

### Overview

`DEBUG_KERNEL` is a compiler `cfg` gate that controls verbose diagnostic output. When enabled, it adds detailed tracing to context switches, frame dumps, GDT analysis, and double-fault diagnostics. When disabled, these become no-ops.

### Configuration

Enabled in `.cargo/config.toml`:
```toml
rustflags = ["--cfg=DEBUG_KERNEL", ...]
```

To enable: add `--cfg=DEBUG_KERNEL` to rustflags.
To disable: remove it from `.cargo/config.toml`.

### What changes when enabled

| Function | When `DEBUG_KERNEL` | When disabled |
|----------|---------------------|---------------|
| `ddbg(byte)` | Writes byte to QEMU debugcon port (0xE9) | No-op |
| `dump_rax(prefix, value)` | Writes `[prefix] RAX=...` to serial | No-op |
| `write_marker_raw(ptr, len)` | Writes marker bytes to serial | No-op |
| `dump_frame_before_pop(rsp)` | Logs RSP, frame slots (RIP, CS, RFLAGS) | No-op |
| `print_iret_cpl_diagnostics(frame, gdt)` | Dumps frame + GDT descriptors | No-op |
| `schedule()` diagnostics | Logs current_pid, next_pid, FIRST DISPATCH | No-op |
| `schedule_force()` diagnostics | Logs old/new PID, IRET frame, CR3/TSS/RSP0 | No-op |
| Double-fault handler | Dumps frame origin, GDT selectors, 20 qwords at RSP_BEFORE_IRETQ | Skips diagnostics |
| `context_switch::SAVED_RSP` etc. | Statics capture context switch state | Not compiled |

### What is always-on (not gated)

| Message | Condition |
|---------|-----------|
| `[EXCEPTION]` / `FATAL` panic messages | Always printed |
| `[TICK]` / `[SWITCH]` markers | Always written (via no-ops when disabled) |
| QEMU debugcon `T` byte | Always written (no-op when disabled) |

### Adding new DEBUG_KERNEL diagnostics

To gate a new diagnostic behind DEBUG_KERNEL:

```rust
#[cfg(DEBUG_KERNEL)]
{
    crate::serial::write_str("[TAG] diagnostic message");
    crate::serial::write_hex(value);
    crate::serial::write_nl();
}
```

Or use the shorthand helpers:
```rust
crate::serial::ddbg(b'X');      // no-op when DEBUG_KERNEL is off
crate::serial::dump_rax(0x49, rax_value);  // no-op when DEBUG_KERNEL is off
```
