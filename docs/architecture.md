# INDOMINUS OS Architecture

This document describes the internal architecture of the INDOMINUS kernel. It is written for contributors who need to understand how the system works before making changes.

---

## Boot Flow

```
Power On
  │
  ▼
UEFI Firmware (OVMF in QEMU)
  │  Initializes hardware, loads EFI binaries
  │
  ▼
indo-boot (EFI/BOOT/BOOTX64.EFI)
  │  1. Load kernel ELF from EFI\INDOMINUS\kernel.elf
  │  2. Parse ELF, copy PT_LOAD segments to physical memory
  │  3. Apply R_X86_64_RELATIVE relocations (PIC fixups)
  │  4. Query UEFI memory map
  │  5. Build BootInfo struct (phys addr, memory map, framebuffer, RSDP)
  │  6. Exit UEFI Boot Services (point of no return)
  │  7. Jump to kernel_main(boot_info_ptr) with sysv64 ABI
  │
  ▼
kernel_main (indo-kernel/src/main.rs)
  │  1. gdt::init()           — Build GDT, load TSS, set segment registers
  │  2. pmm::init()           — Read UEFI memory map, build bitmap allocator
  │  3. vmm::init_kernel_page_tables()  — Create new PML4:
  │  │     - Higher-half mapping (kernel phys → 0xFFFFFFFF80000000)
  │  │     - Heap mapping (KERNEL_HEAP_BASE, 4 MiB)
  │  │     - Identity map first 4 GiB
  │  4. vmm::switch_page_table()  — Write new PML4 to CR3
  │  5. init_heap()           — Initialize linked_list_allocator
  │  6. idt::init()           — Build IDT, set exception/IRQ handlers
  │  7. interrupts::init()    — Configure LAPIC, IO-APIC, PIT
  │  8. [process/spawn calls] — Create kernel processes
  │  9. [start_scheduler]     — sti + hlt loop (first timer IRQ dispatches)
  │
  ▼
First timer interrupt fires
  │
  ▼
timer_interrupt_handler (naked)
  │  1. push rax-r15 (save current registers)
  │  2. call schedule(saved_rsp) → returns new RSP
  │  3. mov r12, rax (save new SP before EOI)
  │  4. Write EOI to LAPIC
  │  5. mov rsp, r12 (switch to new process stack)
  │  6. pop r15-rax (restore new registers)
  │  7. iretq (resume new process)
```

---

## Memory Model

### Physical Memory

Physical memory is managed by the PMM (Physical Memory Manager), a bitmap allocator that tracks 4 KiB frames across a 16 GiB address space. The PMM is initialized from the UEFI memory map passed by the bootloader.

Key regions:

| Physical Range | Owner | Notes |
|---|---|---|
| 0x0000 - 0x0FFF | BIOS/IVT | Reserved |
| 0x1000 - 0x1FFF | Boot page table | UEFI or kernel PML4 (depends on allocation) |
| 0x08000 - 0x0FFFF | EBDA | Extended BIOS Data Area |
| 0xA0000 - 0xBFFFF | VGA | Video memory (MMIO) |
| 0xC0000 - 0xFFFFF | ROMs | Option ROMs |
| kernel_phys_start .. kernel_phys_end | Kernel | Loaded by bootloader |
| remainder | PMM bitmap | Tracks all frames |

### Virtual Address Layout

```
0x0000_0000_0000_0000 .. 0x0000_7FFF_FFFF_FFFF  User space (lower half)
  0x0000_0000_0040_0000  User code base (ELF)
  0x0000_7FFF_FFFF_0000  User stack top (grows down)

0xFFFF_8000_0000_0000 .. 0xFFFF_FFFF_FFFF_FFFF  Kernel space (upper half)
  0xFFFF_FFFF_8000_0000  Kernel .text start (linked address)
  0xFFFF_FFFF_C000_0000  Kernel heap start (4 MiB)
  0xFFFF_FFFF_D000_0000  Kernel stack top (16 KiB)
```

### Page Tables

The kernel creates its own PML4 during boot. The PML4 contains three mapping regions:

**1. Higher-half kernel mapping:**
- Virtual: `0xFFFFFFFF80000000 + offset`
- Physical: `kernel_phys_start + offset`
- Flags: PRESENT | WRITABLE

**2. Kernel heap mapping:**
- Virtual: `0xFFFFFFFFC000_0000 .. 0xFFFFFFFFC000_0000 + 4 MiB`
- Physical: PMM-allocated frames
- Flags: PRESENT | WRITABLE

**3. Identity mapping (first 4 GiB):**
- Virtual: `0x0000_0000 .. 0x0000_FFFF_FFFF`
- Physical: Same as virtual
- Flags: PRESENT | WRITABLE
- Purpose: Safe CR3 transition; allows kernel code to execute at physical addresses during early boot

### Address Translation Functions

```
phys_to_kernel_virt(phys) → phys + KERNEL_VIRT_BASE - kernel_phys_start
  Used for: Converting PIC-relocated addresses to virtual addresses

phys_to_virt(phys) → VirtAddr::new(phys)
  Used for: Page table manipulation (identity map is active)

virt_to_phys(virt) → PhysAddr::new(virt)
  Used for: Reverse lookup (identity map assumed)
```

### PIC (Position-Independent Code)

The kernel is compiled with `-C relocation-model=pic`. This means:

- The linker places code at virtual address `0xFFFFFFFF80000000` in the ELF
- Function pointers and static addresses contain PIC offsets in the binary
- The bootloader applies `R_X86_64_RELATIVE` relocations: `*P = base_phys + (vaddr - min_vaddr)`
- At runtime, all "address" values in the kernel are **physical addresses**, not virtual

This is critical to understand: when Rust code reads `&some_static as *const _ as u64`, it gets the physical address. The `phys_to_kernel_virt()` function must be used to convert to the higher-half virtual address.

---

## Context Switching

### Overview

The scheduler implements preemptive round-robin multitasking. The timer interrupt (vector 32) triggers a context switch every ~10 ms.

### Stack Layout (per-process)

Each kernel process has an 8 KiB kernel stack. The initial stack frame is constructed by `setup_initial_stack_frame_kernel`:

```
Stack top (kernel_stack_base + KERNEL_STACK_SIZE)
  │
  │  [15 GP registers pushed by timer handler]     ← saved_rsp points here
  │  rax, rbx, rcx, rdx, rsi, rdi, rbp,
  │  r8, r9, r10, r11, r12, r13, r14, r15
  │
  │  IRET frame (manually constructed):            ← RSP after 15 pops
  │  [rsp+0]  = RIP  (entry point)
  │  [rsp+8]  = CS   (kernel code selector 0x08)
  │  [rsp+16] = RFLAGS (0x202, IF enabled)
  │
  │  [rsp+24] = RSP  (new RSP if privilege change)
  │  [rsp+32] = SS   (new SS if privilege change)
  │
Stack bottom
```

### Timer Interrupt Handler (naked, vector 32)

```asm
push rax            ; Save all 15 GP registers
push rbx
...
push r15

mov rdi, rsp       ; First arg = saved RSP
call schedule       ; Returns new RSP in RAX

mov r12, rax       ; Save new SP (before EOI)
mov rax, 0xFEE000B0 ; LAPIC EOI register
mov dword [rax], 0  ; Send EOI

mov rsp, r12       ; Switch to new process stack

pop r15             ; Restore all 15 GP registers
...
pop rax

iretq              ; Return to new process
```

### schedule() Function

Called from the naked timer handler with interrupts disabled:

1. **First dispatch:** If no current process, find first Ready task, call `dispatch_first()` which returns its initial stack pointer
2. **Normal path:** Save current process SP, call `on_tick()` for round-robin, switch CR3 if new process has different page tables, update TSS.RSP0

---

## Interrupt Subsystem

### Initialization Order

1. **GDT:** Ring 0/3 code/data segments, TSS with IST[0] for double-fault
2. **IDT:** 256 entries, CPU exception handlers, hardware IRQ handlers
3. **LAPIC:** Memory-mapped at 0xFEE00000, timer configured for ~100 Hz
4. **IO-APIC:** IRQ0 (PIT) → vector 32, IRQ1 (keyboard) → vector 33
5. **PIT:** Channel 0, divisor 11931 (~100 Hz), connected to IRQ0

### Exception Handlers

| Vector | Name | Handler | Behavior |
|---|---|---|---|
| 0 | #DE | divide_error_handler | Fatal |
| 8 | #DF | double_fault_handler | Fatal, uses IST stack |
| 10 | #TS | invalid_tss_handler | Fatal |
| 12 | #SS | stack_segment_fault_handler | Fatal |
| 13 | #GP | general_protection_fault_handler | Fatal, dumps IRET frame |
| 14 | #PF | page_fault_handler | Fatal, dumps CR2 |

### Hardware IRQs

| Vector | Source | Handler |
|---|---|---|
| 32 | PIT (IRQ0) | timer_interrupt_handler (naked) |
| 33 | Keyboard (IRQ1) | irq_handler_33 → dispatch |
| 34-47 | Other | irq_handler_XX → dispatch |

---

## Key Data Structures

### BootInfo (passed from bootloader)

```rust
struct BootInfo {
    magic: u64,              // 0x494E444F4D494E55 ("INDOMINU")
    protocol_version: u64,
    memory_map: MemoryMap,   // Physical memory regions
    framebuffer: FramebufferInfo,
    rsdp_addr: PhysAddr,     // ACPI RSDP
    kernel_phys_start: PhysAddr,
    kernel_phys_end: PhysAddr,
    kernel_virt_base: VirtAddr,
}
```

### Process

```rust
struct Process {
    pid: Pid,
    state: ProcessState,       // Ready, Running, Blocked, Exited
    kernel_stack_base: u64,    // Physical address of stack allocation
    pml4_phys: u64,            // Physical address of process PML4
    entry_addr: u64,           // PIC physical address of entry point
    is_user: bool,
    user_rip: Option<u64>,
    user_rsp: Option<u64>,
}
```

### Scheduler

```rust
struct Scheduler {
    processes: [Option<Process>; MAX_PROCESSES],
    current_pid: Option<Pid>,
    next_pid: Pid,
    idle_pid: Pid,
}
```

---

## Build Configuration

### .cargo/config.toml

```toml
[build]
target = "x86_64-unknown-none"

[target.x86_64-unknown-none]
rustflags = [
    "-C", "link-arg=-Tkernel.ld",
    "-C", "target-feature=-mmx,-sse,-sse2,...",
    "-C", "target-feature=+soft-float",
    "-C", "relocation-model=pic",
    "-C", "linker-flavor=ld.lld",
    "-C", "linker=rust-lld",
]
```

### kernel.ld

- Linked at `0xFFFFFFFF80000000` (upper half, -2 GiB)
- Sections: .text → .rodata → .data → .bss
- Page-aligned sections for permission control

### Dependencies

| Crate | Version | Purpose |
|---|---|---|
| x86_64 | 0.15.5 | Page tables, IDT, GDT, control registers |
| spin | 0.9.9 | Spinlock for scheduler |
| linked_list_allocator | — | Kernel heap |
