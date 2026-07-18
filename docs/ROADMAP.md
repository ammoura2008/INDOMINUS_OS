# INDOMINUS REX — Complete Development Roadmap

**Version**: 1.0  
**Date**: 2026-07-17  
**Goal**: A complete, independent, daily-driver operating system capable of replacing Windows.

---

## Guiding Principles

1. **Every component must justify its existence.** Smaller efficient system over unnecessary complexity.
2. **No temporary solutions that become permanent limitations.** If a design blocks the final objective, fix it now.
3. **Each phase must be self-contained and testable.** A phase is "done" when it boots and works.
4. **Scale test**: Would this choice still make sense with 100 million users?
5. **Architecture test**: Will this support real hardware, multiple CPUs, user isolation, security boundaries?

---

## Phase Overview

| Phase | Name | Key Deliverable | Dependencies |
|-------|------|-----------------|--------------|
| 0 | Bootstrap | UEFI boot, serial, GDT, IDT | — |
| 1 | Memory Management | PMM, VMM, kernel heap | Phase 0 |
| 2 | Interrupts & Timers | LAPIC, IO-APIC, PIT, IRQ dispatch | Phase 1 |
| 3 | Processes & Scheduling | Task struct, context switch, round-robin scheduler | Phase 2 |
| 4 | System Calls | `syscall`/`sysret`, user mode transition, initial syscall set | Phase 3 |
| 5 | Device Discovery | PCI enumeration, ACPI table parsing, MMIO probing | Phase 1 |
| 6 | Storage & Block Devices | AHCI/NVMe drivers, block device abstraction | Phase 5 |
| 7 | Filesystem | VFS layer, FAT32 driver, file operations | Phase 6 |
| 8 | User Space | ELF loader, init process, basic shell | Phase 4, 7 |
| 9 | Memory Protection | CoW, mmap, demand paging, page fault handling | Phase 4 |
| 10 | Process Lifecycle | fork, exec, exit, wait, signals, pipes | Phase 8, 9 |
| 11 | Display & Graphics | Framebuffer console, font renderer, double buffering | Phase 1 |
| 12 | Input System | PS/2 keyboard/mouse, USB HID, input event dispatch | Phase 2, 5 |
| 13 | Window Manager | Compositor, window management, input routing | Phase 11, 12 |
| 14 | Networking | NIC drivers, TCP/IP stack, DNS, sockets API | Phase 4, 5 |
| 15 | USB | XHCI host controller, hub enumeration, device drivers | Phase 5 |
| 16 | Audio | Audio device detection, PCM output, mixer | Phase 5, 15 |
| 17 | Advanced Storage | NVMe full driver, partition table parsing, ext2/FAT32 | Phase 6 |
| 18 | UI Toolkit | Widget library, theme engine, accessibility | Phase 13 |
| 19 | Application Ecosystem | Package manager, SDK, developer tools, app sandbox | Phase 8, 14 |
| 20 | Shell & Utilities | Terminal emulator, file manager, text editor, system monitor | Phase 13, 18 |
| 21 | Security Hardening | ASLR, stack canaries, seccomp, capabilities, audit | Phase 4, 9 |
| 22 | GPU Acceleration | GPU detection, basic 2D acceleration, Vulkan/GL driver path | Phase 13 |
| 23 | AI Integration | Local model inference, AI assistant, intelligent scheduling | Phase 14, 19 |
| 24 | Multi-Boot & Install | Disk installer, bootloader menu, multi-OS detection | Phase 7, 8 |
| 25 | Power Management | ACPI sleep/wake, CPU frequency scaling, thermal management | Phase 2, 5 |
| 26 | Advanced Features | Virtualization, containers, snapshots, live updates | Phase 10, 21 |
| 27 | Polish & Optimization | Boot time, memory footprint, latency, documentation | All previous |
| 28 | Release Candidate | ISO image, real hardware testing, bug fixes, beta program | Phase 27 |

---

## Detailed Phase Descriptions

---

### Phase 0 — Bootstrap ✅ COMPLETE

**Status**: DONE  
**Goal**: Boot through UEFI, initialize fundamental CPU structures, halt safely.

**Deliverables**:
- UEFI bootloader (`indo-boot`) loads kernel ELF from ESP
- Serial output via UART 16550 (`kprint!`/`kprintln!`)
- GDT with kernel code/data segments + TSS with IST
- IDT with 6 exception handlers + spurious IRQ handlers
- Boot protocol library (`indo-core`) shared between bootloader and kernel
- Dual build system (Makefile + PowerShell)

**Architecture Notes**:
- Kernel linked at `0xFFFFFFFF80000000` (higher half)
- SSE/AVX disabled in kernel (soft-float)
- Red zone disabled via linker flags

---

### Phase 1 — Memory Management ✅ COMPLETE

**Goal**: Full physical and virtual memory management. The kernel owns all RAM.

**Dependencies**: Phase 0

**Deliverables**:

#### 1.1 Physical Memory Manager (PMM)
- **Bitmap allocator** for tracking free/used 4 KiB pages
- Initialize from UEFI memory map (mark usable regions as free)
- `alloc_frame() -> PhysAddr` and `free_frame(PhysAddr)`
- Track kernel code/data regions as used
- Handle memory map quirks (non-contiguous regions, gaps)
- Support for 64 GiB+ RAM (bitmap scales linearly)

#### 1.2 Virtual Memory Manager (VMM)
- **Page table management**: create, map, unmap, translate
- Higher-half kernel mapping (identity map kernel `.text`/`.rodata`/`.data`/`.bss`)
- Temporary identity mapping for early boot (needed until VMM is active)
- `MapToError` handling for already-mapped pages
- Page table walker for debugging
- Flush TLB after modifications

#### 1.3 Kernel Heap Allocator
- Wire up `linked_list_allocator` (already declared as dependency)
- Global `#[global_allocator]` using `spin::Mutex<LockedHeap>`
- `alloc` / `dealloc` for `Box`, `Vec`, `String` in kernel
- Heap size: start with 4 MiB, grow on demand
- Guard pages to detect heap overflow

#### 1.4 Stack Setup
- Double-fault IST stack (already in BSS, 16 KiB)
- Kernel stack per-CPU (16 KiB aligned)
- Stack overflow detection via guard pages

**Test Criteria**:
- Kernel boots and prints memory map
- Can allocate and free physical frames
- Can create new page tables and map/unmap pages
- Can allocate `Box<u64>` and `Vec<u8>` on the heap
- No page faults during normal operation

**Architectural Notes**:
- PMM must handle non-EFI memory (firmware may not mark all RAM as "usable")
- VMM must support future demand paging (page fault handler can add mappings)
- Heap allocator must be lock-free or use fine-grained locking for future SMP

---

### Phase 2 — Interrupts & Timers ✅ COMPLETE

**Goal**: Handle hardware interrupts. Establish timing for scheduling.

**Dependencies**: Phase 1

**Deliverables**:

#### 2.1 Local APIC (LAPIC)
- Memory-mapped register access (base address from ACPI MADT)
- EOI (End of Interrupt) signaling
- Spurious interrupt handling
- IPI (Inter-Processor Interrupt) for future SMP
- LVT (Local Vector Table) configuration

#### 2.2 IO-APIC
- MMIO register access
- IRQ redirection table configuration
- Route hardware IRQs to LAPIC vectors
- Mask/unmask individual IRQ lines

#### 2.3 Programmable Interval Timer (PIT)
- Channel 0: periodic timer for scheduler tick
- Configure to ~100 Hz (10 ms period) initially
- Replace with HPET/APIC timer later for better accuracy

#### 2.4 IRQ Dispatch
- Register handler for each IRQ vector
- Handler table with function pointers
- IRQ enable/disable per line
- Nested vs. non-nested interrupt handling

**Test Criteria**:
- Timer fires at configured rate (visible via serial counter)
- IRQs are dispatched to registered handlers
- LAPIC EOI is sent correctly
- No double faults or spurious interrupts

**Architectural Notes**:
- IRQ handler must save/restore full register state (for future context switch)
- Timer tick counter must be atomic (for future SMP)
- IO-APIC configuration depends on ACPI MADT — ties into Phase 5

---

### Phase 3 — Processes & Scheduling

**Goal**: Define the process abstraction. Enable pre-emptive multitasking.

**Dependencies**: Phase 2

**Deliverables**:

#### 3.1 Process Structure
```rust
struct Process {
    pid: u64,
    state: ProcessState,        // Ready, Running, Blocked, Zombie
    page_table: PhysAddr,       // CR3 value
    kernel_stack: VirtAddr,     // Per-process kernel stack
    user_stack: VirtAddr,       // Per-process user stack
    instruction_pointer: VirtAddr,
    registers: SavedRegisters,  // Saved on context switch
    memory_map: Vec<MemoryMapping>,
    file_descriptors: Vec<FileDescriptor>,
    parent: Option<Pid>,
    children: Vec<Pid>,
}
```

#### 3.2 Context Switch
- Assembly routine to save/restore: RAX, RBX, RCX, RDX, RSI, RDI, RBP, RSP, R8-R15, RFLAGS
- Switch CR3 (page tables) between processes
- Switch kernel stack pointer (via TSS or direct)
- Full register state save/restore for pre-emption

#### 3.3 Scheduler
- **Round-robin** initially (fair, simple, correct)
- Timer-driven pre-emption (each tick checks if current process exhausted quantum)
- Process states: Ready queue, Running, Blocked (waiting on I/O), Zombie
- `yield()` for voluntary pre-emption
- Idle process (runs `hlt` when nothing to schedule)

#### 3.4 Process Management
- `spawn(kernel_fn, user_fn) -> Pid`
- `exit(code)`
- `wait() -> (Pid, exit_code)`
- Process table (fixed-size array, max 256 processes initially)

**Test Criteria**:
- Two kernel-mode tasks alternate execution on timer ticks
- Context switch preserves all register state
- Idle process runs when no other process is ready
- Process can be created, run, and destroyed

**Architectural Notes**:
- Process struct uses `Vec` for memory map and FDs — needs heap (Phase 1)
- Context switch must be atomic (interrupts disabled during switch)
- Future: per-CPU scheduler, SMP load balancing
- Process isolation enforced by separate page tables (Phase 9)

---

### Phase 4 — System Calls

**Goal**: User-mode programs can request kernel services.

**Dependencies**: Phase 3

**Deliverables**:

#### 4.1 Syscall Mechanism
- `syscall`/`sysret` instructions (fast path, like Linux)
- MSR setup: `IA32_EFER.SCE=1`, `IA32_STAR`, `IA32_LSTAR`
- User-mode CS/SS via `IA32_FMASK`
- Syscall number in RAX, args in RDI, RSI, RDX, R10
- Return value in RAX

#### 4.2 Initial Syscall Set
| Syscall | Number | Purpose |
|---------|--------|---------|
| `sys_exit` | 0 | Terminate process |
| `sys_write` | 1 | Write to stdout (serial/fbconsole) |
| `sys_read` | 2 | Read from stdin (keyboard) |
| `sys_yield` | 3 | Voluntary pre-emption |
| `sys_getpid` | 4 | Get process ID |
| `sys_sleep` | 5 | Sleep for N ticks |
| `sys_alloc` | 6 | Allocate user memory (temporary, until mmap) |
| `sys_free` | 7 | Free user memory (temporary) |

#### 4.3 Syscall Dispatch Table
- Array of function pointers indexed by syscall number
- Bounds checking on syscall number
- Register validation before dereferencing user pointers

**Test Criteria**:
- User-mode program can invoke `sys_write` to print to serial
- `sys_exit` terminates process and returns to scheduler
- Invalid syscall number returns error (not crash)
- User pointers are validated (kernel doesn't crash on bad addresses)

**Architectural Notes**:
- Syscall dispatch must be fast (hot path)
- User pointer validation is critical for security — never trust user addresses
- Future: seccomp filtering on syscalls (Phase 21)
- Syscall ABI must be stable — changing it breaks all userspace

---

### Phase 5 — Device Discovery

**Goal**: Enumerate and identify hardware devices on the system.

**Dependencies**: Phase 1

**Deliverables**:

#### 5.1 PCI Enumeration
- PCI configuration space access (I/O ports 0xCF8/0xCFC for legacy, MMIO for PCIe)
- Scan all PCI buses, devices, functions
- Build device tree: vendor/device class, BAR addresses, IRQ lines
- PCI capability list parsing (MSI, MSI-X, etc.)

#### 5.2 ACPI Table Parsing
- RSDP → RSDT/XSDT → FADT, MADT, MCFG, HPET, etc.
- MADT: APIC IDs, CPU topology, IO-APIC address, interrupt source overrides
- MCFG: PCIe ECAM base addresses
- HPET: High Precision Event Timer (replaces PIT)
- FADT: ACPI hardware addresses

#### 5.3 MMIO Framework
- Safe MMIO register read/write wrappers
- Volatile access (prevent compiler reordering)
- Register bitfield abstractions

**Test Criteria**:
- PCI devices enumerated and printed (should see QEMU's virtio, ISA bridge, etc.)
- ACPI MADT parsed: APIC type detected
- MMIO reads return correct hardware values

**Architectural Notes**:
- ACPI parser must handle table checksums and validate signatures
- PCI enumeration must handle multi-segment (PCIe domains)
- MMIO wrappers must prevent UB from compiler optimizations
- Device tree is foundation for all future driver work

---

### Phase 6 — Storage & Block Devices

**Goal**: Read/write block devices. Foundation for filesystems.

**Dependencies**: Phase 5

**Deliverables**:

#### 6.1 Block Device Abstraction
```rust
trait BlockDevice {
    fn read_sector(&self, lba: u64, buf: &mut [u8]) -> Result<()>;
    fn write_sector(&self, lba: u64, buf: &[u8]) -> Result<()>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
}
```

#### 6.2 AHCI Driver
- Detect AHCI controller via PCI class 0x01/0x06
- Map ABAR (AHCI Base Address Register)
- HBA initialization (HBA_MEM, port list)
- Command list and FIS-based command setup
- READ_DMA_EXT, WRITE_DMA_EXT commands
- NCQ support detection (optional, for later)

#### 6.3 NVMe Driver (Basic)
- Detect NVMe controller via PCI class 0x01/0x08
- Admin queue and I/O queue setup
- Identify controller and namespace commands
- READ/WRITE commands (simple single-sector initially)

**Test Criteria**:
- AHCI controller detected and initialized
- Can read sector 0 of a disk (MBR/GPT header)
- Data matches what was written (round-trip test)

**Architectural Notes**:
- Block device abstraction allows filesystem drivers to be device-agnostic
- AHCI is the standard SATA interface — must work on real hardware
- NVMe is the modern standard — high priority
- Interrupt-driven I/O (not polling) for performance
- Future: virtio-blk for QEMU performance

---

### Phase 7 — Filesystem

**Goal**: Persistent file storage. Read/write files on disk.

**Dependencies**: Phase 6

**Deliverables**:

#### 7.1 Virtual Filesystem (VFS)
```rust
trait FileSystem {
    fn name(&self) -> &str;
    fn mount(&mut self, device: &dyn BlockDevice) -> Result<()>;
    fn root_dir(&self) -> &dyn Directory;
}

trait Directory {
    fn entry(&self, name: &str) -> Result<FileEntry>;
    fn entries(&self) -> Vec<FileEntry>;
    fn create_file(&self, name: &str) -> Result<Box<dyn File>>;
    fn mkdir(&self, name: &str) -> Result<()>;
}

trait File {
    fn read(&self, buf: &mut [u8], offset: u64) -> Result<usize>;
    fn write(&mut self, data: &[u8], offset: u64) -> Result<usize>;
    fn size(&self) -> u64;
}
```

#### 7.2 FAT32 Driver
- Read boot sector, BPB (BIOS Parameter Block)
- FAT table parsing
- Cluster chain following
- Long filename support (LFN)
- Directory entry parsing
- File read/write operations

#### 7.3 Mount System
- Root filesystem mount at boot
- `/dev/` mount for block devices
- `/boot/` mount for ESP (read-only)

**Test Criteria**:
- Can mount a FAT32-formatted disk image
- Can list directory contents
- Can read a file from disk
- Can write a file and read it back

**Architectural Notes**:
- VFS abstraction allows multiple filesystem types without kernel changes
- FAT32 is required for UEFI ESP compatibility
- Future: ext4 for root filesystem, tmpfs for volatile data
- File locking for concurrent access (future)

---

### Phase 8 — User Space

**Goal**: Run user-mode programs. First interactive shell.

**Dependencies**: Phase 4, Phase 7

**Deliverables**:

#### 8.1 ELF Loader
- Parse ELF64 header and program headers
- Load PT_LOAD segments into user pages
- Set up user page table (lower half only)
- Map user stack (bottom of higher half, grows down)
- Map user heap (above stack, grows up)
- Set entry point from ELF header

#### 8.2 Init Process
- First user-space process (PID 1)
- Runs `/bin/init` (compiled from Rust)
- Sets up stdin/stdout/stderr file descriptors
- Spawns shell as child

#### 8.3 Basic Shell (`indosh`)
- Command line input (keyboard)
- Built-in commands: `echo`, `ls`, `cat`, `clear`, `help`
- External command execution via `exec` syscall
- PATH-based command lookup
- Exit/logout support

#### 8.4 Minimal Utilities
- `init` — process manager, spawns shell
- `echo` — print text
- `ls` — list directory contents
- `cat` — print file contents
- `clear` — clear screen

**Test Criteria**:
- Shell boots automatically after kernel init
- User can type commands and see output
- `echo hello` prints "hello"
- `ls /` lists files on the root filesystem
- `cat /boot/kernel.elf` prints binary data (or "binary file" message)
- Process can exit cleanly

**Architectural Notes**:
- ELF loader must validate all ELF fields (prevent malicious binaries)
- Init process is PID 1 — must never exit (adopt orphan processes)
- Shell is the primary user interface — must be responsive
- Future: proper terminal emulator with escape code support

---

### Phase 9 — Memory Protection

**Goal**: Process isolation. No process can corrupt another or the kernel.

**Dependencies**: Phase 4

**Deliverables**:

#### 9.1 Per-Process Page Tables
- Each process gets its own PML4
- Kernel pages mapped identically in all page tables (upper half)
- User pages mapped only in the owning process's table
- CR3 swap on context switch

#### 9.2 Copy-on-Write (CoW)
- Fork creates shared pages marked read-only
- Write fault triggers page copy (CoW fault handler)
- Reduces fork memory usage (critical for Unix-style process creation)

#### 9.3 Memory Mapping (mmap)
- `mmap(addr, len, prot, flags, fd, offset)`
- Map anonymous pages (heap, stack)
- Map file-backed pages (ELF loading, shared libraries)
- `munmap` to unmap regions
- Protection bits: READ, WRITE, EXECUTE

#### 9.4 Demand Paging
- Pages allocated on first access (not upfront)
- Page fault handler looks up VMA (Virtual Memory Area)
- Allocates and maps page on demand
- Reduces startup memory for processes

**Test Criteria**:
- Process cannot write to another process's memory (page fault)
- Process cannot write to kernel memory (page fault)
- CoW fork: parent and child share pages until one writes
- `mmap` can allocate new memory regions
- Demand paging: process can use memory beyond what was allocated at start

**Architectural Notes**:
- Page fault handler is the most critical security boundary
- Must distinguish legitimate faults (CoW, demand page) from bugs (segfault)
- Kernel must never expose kernel pointers to user space
- Future: ASLR (randomize process memory layout) in Phase 21

---

### Phase 10 — Process Lifecycle

**Goal**: Full Unix-like process model. Real process creation and management.

**Dependencies**: Phase 8, Phase 9

**Deliverables**:

#### 10.1 fork()
- Duplicate process: copy page tables (with CoW), copy kernel state
- New process gets new PID
- Parent returns child PID, child returns 0
- Copy file descriptor table (shared underlying file objects)

#### 10.2 exec()
- Replace current process image with new ELF
- Load ELF segments, set up new page tables
- Reset heap, stack, instruction pointer
- Pass command-line arguments and environment variables

#### 10.3 exit() & wait()
- `exit(code)`: mark process as Zombie, notify parent
- `wait()`: parent blocks until child exits, collects exit code
- Reap zombie processes
- Orphan adoption (reparent to init if parent exited)

#### 10.4 Signals
- Signal delivery: SIGTERM, SIGKILL, SIGSEGV, SIGALRM, SIGINT
- Signal handler registration (user-space handler functions)
- Default actions: terminate, ignore, core dump
- Pending signal queue per process

#### 10.5 Pipes
- `pipe()` creates a unidirectional byte stream
- Read end blocks until data available
- Write end blocks until buffer has space
- Connect stdin/stdout between processes (shell pipelines)

**Test Criteria**:
- `fork()` creates a child process that runs independently
- `exec()` replaces process with a new program
- `wait()` collects child's exit code
- `exit()` with no parent becomes zombie, adopted by init
- Signal delivery terminates a process
- Pipe connects `ls | grep foo`

**Architectural Notes**:
- fork() + CoW is fundamental to Unix process creation
- exec() must clean up old process memory (unmap old pages)
- Signal handlers run in user mode (on user stack)
- Pipes use ring buffers in kernel memory

---

### Phase 11 — Display & Graphics

**Goal**: Visual output. Text and graphics on screen.

**Dependencies**: Phase 1

**Deliverables**:

#### 11.1 Framebuffer Driver
- Map GOP framebuffer to kernel virtual memory
- Pixel writing: `put_pixel(x, y, color)`
- Rectangle fill, line drawing primitives
- Double buffering (off-screen buffer, then copy to display)

#### 11.2 Font Renderer
- Embedded bitmap font (8x16 or similar)
- `draw_char(x, y, ch, fg, bg)`
- Text scrolling (move all rows up when bottom reached)
- Cursor positioning

#### 11.3 Console Driver
- Virtual terminal on framebuffer
- Text output to screen (like Linux `fbcon`)
- ANSI escape code support: cursor movement, colors, clear
- Scrolling buffer (keep last N lines)

#### 11.4 Screen Manager
- Screen resolution detection
- Multiple resolution support
- Screen blanking (power saving)
- Cursor hiding/showing

**Test Criteria**:
- Kernel boots and displays text on screen
- Colors work (green on black, white on blue, etc.)
- Scrolling works when text exceeds screen height
- `kprintln!` outputs to both serial AND screen

**Architectural Notes**:
- Framebuffer driver is early display (before GPU driver loads)
- Font is embedded in kernel binary (static, no filesystem needed)
- Console must handle Unicode eventually (start with ASCII)
- Future: GPU-accelerated rendering replaces framebuffer

---

### Phase 12 — Input System

**Goal**: Accept keyboard and mouse input from the user.

**Dependencies**: Phase 2, Phase 5

**Deliverables**:

#### 12.1 PS/2 Keyboard Driver
- PS/2 controller I/O ports (0x60 data, 0x64 status)
- Scan code set 2 → key code translation
- Key press/release detection
- Modifier key tracking (Shift, Ctrl, Alt)
- Ring buffer for keystroke events

#### 12.2 PS/2 Mouse Driver
- PS/2 auxiliary device (port 2)
- Mouse packet parsing (movement, buttons, scroll)
- Relative movement tracking
- Button state (left, right, middle)

#### 12.3 Input Event System
```rust
struct KeyEvent {
    key: KeyCode,
    state: KeyState,     // Pressed, Released
    modifiers: Modifiers, // Shift, Ctrl, Alt
}

struct MouseEvent {
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    buttons: MouseButton,
    scroll: i32,
}
```
- Event queue (kernel-side)
- Dispatch to focused window/process

#### 12.4 Keyboard Layout
- US QWERTY mapping (default)
- Configurable key maps (load from file)
- Dead key support (accented characters)

**Test Criteria**:
- Key presses appear on serial/screen
- Mouse movement tracked
- Modifier keys (Shift, Ctrl) affect output
- Key repeat works (holding a key)

**Architectural Notes**:
- PS/2 is legacy but universal (works on all x86 hardware)
- USB HID input comes in Phase 15
- Input events must be dispatchable to any subsystem (console, window manager, etc.)
- Key repeat is timer-driven (Phase 2 timer)

---

### Phase 13 — Window Manager

**Goal**: Multi-window desktop environment with mouse interaction.

**Dependencies**: Phase 11, Phase 12

**Deliverables**:

#### 13.1 Compositor
- Composites all visible windows into final framebuffer
- Dirty region tracking (only redraw changed areas)
- Clipping (windows don't draw outside their bounds)
- Z-order management (which window is on top)

#### 13.2 Window Management
```rust
struct Window {
    id: WindowId,
    owner: Pid,
    rect: Rect,
    title: String,
    visible: bool,
    focused: bool,
    input_region: Rect,
}
```
- Create/destroy windows
- Move, resize, minimize, maximize, close
- Focus management (click to focus, Alt+Tab)
- Window decorations (title bar, borders, close button)

#### 13.3 Input Routing
- Mouse events → focused window (or window under cursor)
- Keyboard events → focused window
- System key combos intercepted by compositor (Alt+F4, etc.)

#### 13.4 Window Buffer Protocol
- Each window has its own framebuffer
- Applications draw to their buffer
- Compositor copies window buffer to screen
- Shared memory for window buffers (mmap)

**Test Criteria**:
- Multiple windows can be displayed simultaneously
- Windows can be moved with mouse drag
- Keyboard input goes to focused window
- Z-order changes when clicking windows

**Architectural Notes**:
- Compositor is the core of the desktop — must be fast
- Dirty region tracking prevents full-screen redraws
- Window buffer protocol must be efficient (shared memory, not IPC copies)
- Future: GPU compositing (hardware overlay, vsync)
- This is where the OS starts feeling like a real desktop

---

### Phase 14 — Networking

**Goal**: Connect to networks. TCP/IP stack. Internet access.

**Dependencies**: Phase 4, Phase 5

**Deliverables**:

#### 14.1 NIC Driver
- Detect NICs via PCI
- Intel E1000 driver (QEMU's default NIC)
- Virtio-net driver (QEMU virtio)
- Receive/transmit ring buffers
- Interrupt-driven packet processing

#### 14.2 Network Stack
- **Link layer**: Ethernet frame TX/RX
- **ARP**: IP → MAC address resolution
- **IPv4**: Packet routing, fragmentation/reassembly
- **ICMP**: Ping (for testing)
- **UDP**: Connectionless datagrams
- **TCP**: Reliable byte streams (3-way handshake, retransmission, flow control)

#### 14.3 Socket API
```rust
// Syscalls
sys_socket(domain, type, protocol) -> fd
sys_bind(fd, addr, port)
sys_listen(fd, backlog)
sys_accept(fd) -> new_fd
sys_connect(fd, addr, port)
sys_send(fd, buf, flags) -> bytes_sent
sys_recv(fd, buf, flags) -> bytes_read
sys_close(fd)
```

#### 14.4 DNS
- Parse `/etc/resolv.conf` (or hardcoded resolver)
- UDP DNS query
- Cache resolved names

**Test Criteria**:
- Can ping gateway (from DHCP or static config)
- Can resolve domain names (DNS)
- Can establish TCP connection to a server
- Can download a file via HTTP

**Architectural Notes**:
- TCP state machine is complex — implement incrementally
- Interrupt-driven networking is essential for performance
- Socket API should be POSIX-compatible (familiar to developers)
- Future: TLS/SSL support (Phase 21)
- Network stack must handle malformed packets gracefully (security)

---

### Phase 15 — USB

**Goal**: Support USB devices. Keyboards, mice, storage, peripherals.

**Dependencies**: Phase 5

**Deliverables**:

#### 15.1 XHCI Host Controller
- Detect XHCI controller via PCI
- Initialize controller (reset, configure rings)
- Device slot allocation
- Transfer ring management (TRBs)

#### 15.2 USB Core
- USB device enumeration (get descriptors)
- Configuration/set-configuration
- Standard request handling

#### 15.3 USB HID
- USB keyboard driver
- USB mouse driver
- HID report parsing

#### 15.4 USB Mass Storage
- Bulk-only transport protocol
- SCSI command set (READ_CAPACITY, READ_10, WRITE_10)
- USB flash drive read/write

**Test Criteria**:
- USB keyboard and mouse work
- USB flash drive is detected and readable
- Files can be read from USB storage

**Architectural Notes**:
- XHCI is the modern USB standard (USB 3.x)
- EHCI (USB 2.0) fallback for older hardware
- USB is complex — implement incrementally (keyboard first, then storage)
- Future: USB audio, USB Bluetooth, USB serial

---

### Phase 16 — Audio

**Goal**: Sound output. Play audio files. System sounds.

**Dependencies**: Phase 5, Phase 15

**Deliverables**:

#### 16.1 Audio Device Detection
- HDA (High Definition Audio) controller via PCI
- Codec identification and configuration
- PCM stream setup

#### 16.2 PCM Output
- Ring buffer for audio samples
- DMA transfer to audio device
- Sample rate conversion (if needed)

#### 16.3 Mixer
- Volume control
- Output routing (speakers, headphones)
- Mute/unmute

#### 16.4 WAV Playback
- Parse WAV file headers
- Stream decoded audio to PCM output

**Test Criteria**:
- Audio device detected
- Can play a sine wave tone
- Can play a WAV file through speakers

**Architectural Notes**:
- HDA is the standard on modern x86 hardware
- Audio is latency-sensitive — need small buffers
- Future: MP3/OGG decoding, audio effects, multi-channel

---

### Phase 17 — Advanced Storage

**Goal**: Support multiple filesystem types. Partition handling.

**Dependencies**: Phase 6

**Deliverables**:

#### 17.1 NVMe Full Driver
- Multi-queue I/O (parallel submission/completion)
- Namespace management
- SMART health data
- Error handling

#### 17.2 Partition Table Parsing
- MBR partition table (legacy)
- GPT partition table (modern)
- Partition detection and device creation

#### 17.3 ext2 Filesystem (Read)
- Superblock parsing
- Inode/directory structure
- Block group descriptors
- File read operations

#### 17.4 tmpfs (In-Memory Filesystem)
- Mount at `/tmp`
- RAM-backed storage
- Useful for temporary files, pipes, sockets

**Test Criteria**:
- GPT partition table correctly parsed
- Can read ext2 partition (if present)
- tmpfs works for temporary file storage
- NVMe multi-queue improves I/O throughput

**Architectural Notes**:
- GPT is required for disks > 2 TiB
- ext2 is simple to implement (good first non-FAT filesystem)
- tmpfs is essential for `/tmp`, `/run`, and future socket files
- Future: ext4 (with journaling), Btrfs, ZFS-like features

---

### Phase 18 — UI Toolkit

**Goal**: Reusable UI components. Applications can build consistent interfaces.

**Dependencies**: Phase 13

**Deliverables**:

#### 18.1 Widget Library
- Button, Label, TextBox, ListBox, CheckBox, RadioButton
- Layout managers (vertical, horizontal, grid)
- Drawing primitives (text, rectangles, images)

#### 18.2 Theme Engine
- Configurable colors, fonts, spacing
- Dark mode / light mode
- System-wide theme applied to all applications

#### 18.3 Event System
- Mouse events (click, hover, drag)
- Keyboard events (focus, typing, shortcuts)
- Widget-level event handling

#### 18.4 Accessibility
- Screen reader support (text descriptions of UI elements)
- Keyboard-only navigation
- High contrast mode

**Test Criteria**:
- Application can create a window with buttons and text boxes
- Clicking a button triggers a callback
- Theme changes affect all widgets
- Keyboard navigation works

**Architectural Notes**:
- UI toolkit is what makes applications possible
- Must be lightweight (no Electron-style bloat)
- Native Rust widgets (not web-based)
- Future: custom widget creation, animation, rich text

---

### Phase 19 — Application Ecosystem

**Goal**: Install, manage, and distribute applications.

**Dependencies**: Phase 8, Phase 14

**Deliverables**:

#### 19.1 Package Manager (`indo-pkg`)
- Package format: `.indopkg` (signed tarball)
- Install, remove, update packages
- Dependency resolution
- Repository configuration

#### 19.2 Software Repository
- Central repository server (HTTP-based)
- Package metadata (name, version, dependencies, description)
- Package signing and verification

#### 19.3 SDK
- Cross-compilation toolchain for INDOMINUS
- Standard C library (musl or custom)
- System headers and libraries
- Build system integration (Cargo, CMake)

#### 19.4 App Sandbox
- Filesystem isolation (only /home, /tmp visible)
- Network permission control
- Resource limits (CPU, memory)
- Capability-based access

**Test Criteria**:
- Can install a package from repository
- Can remove an installed package
- Package manager resolves dependencies
- Sandbox prevents app from accessing unauthorized files

**Architectural Notes**:
- Package format must be secure (signed, verified)
- SDK must be easy to use (lower barrier for developers)
- App sandbox is critical for security (untrusted code)
- Future: app store GUI, reviews, ratings

---

### Phase 20 — Shell & Utilities

**Goal**: Complete user-facing tools. Productive desktop experience.

**Dependencies**: Phase 13, Phase 18

**Deliverables**:

#### 20.1 Terminal Emulator
- Full VT100/ANSI escape code support
- Scrollback buffer
- Copy/paste
- Tab completion
- Split panes (optional)

#### 20.2 File Manager
- Visual directory browsing
- File operations (copy, move, delete, rename)
- Drag and drop
- File properties (size, date, permissions)
- Search

#### 20.3 Text Editor
- Syntax highlighting
- Multi-file editing
- Find and replace
- Line numbers
- Auto-save

#### 20.4 System Monitor
- Process list (PID, CPU%, memory, status)
- CPU usage graph
- Memory usage
- Disk usage
- Network activity

#### 20.5 Settings Application
- Display settings (resolution, scaling)
- Network configuration
- Sound settings
- Keyboard/mouse settings
- Theme customization
- User account management

**Test Criteria**:
- Terminal emulator can run shell and display output
- File manager can browse and manipulate files
- Text editor can open and edit source code
- System monitor shows running processes

**Architectural Notes**:
- These are the "killer apps" that make the OS usable
- Must be polished and responsive
- Settings app is critical for user experience
- Future: web browser, email client, office suite

---

### Phase 21 — Security Hardening

**Goal**: Defense in depth. Protect against attacks and vulnerabilities.

**Dependencies**: Phase 4, Phase 9

**Deliverables**:

#### 21.1 ASLR (Address Space Layout Randomization)
- Randomize process stack, heap, mmap base addresses
- Randomize kernel virtual addresses (KASLR)
- Reduces exploitation of memory corruption bugs

#### 21.2 Stack Protections
- Stack canaries (detect stack buffer overflows)
- Non-executable stack (NX bit on stack pages)
- Shadow stack (CET — Control-flow Enforcement Technology)

#### 21.3 Seccomp (Secure Computing Mode)
- Filter syscalls per-process
- Allow/deny syscall policies
- Prevent exploitation of kernel via syscalls

#### 21.4 Capabilities
- Fine-grained permissions (not just root/user)
- Per-process capability sets
- Capability inheritance on exec

#### 21.5 Audit System
- Log security-relevant events
- Process creation/termination
- File access (optional, performance cost)
- Authentication events

#### 21.6 Secure Boot
- Verify bootloader signature
- Verify kernel signature
- Chain of trust from firmware to kernel

**Test Criteria**:
- ASLR: process addresses differ between runs
- Stack canary detects overflow (test with故意 overflow)
- Seccomp blocks disallowed syscalls
- Audit log records process creation

**Architectural Notes**:
- Security must be designed in, not bolted on
- ASLR is simple to implement but highly effective
- Seccomp is essential for sandboxing untrusted apps
- Future: SELinux-like mandatory access control
- Secure boot requires hardware support (TPM)

---

### Phase 22 — GPU Acceleration

**Goal**: Hardware-accelerated graphics. 2D/3D rendering.

**Dependencies**: Phase 13

**Deliverables**:

#### 22.1 GPU Detection
- Enumerate GPUs via PCI
- Identify GPU vendor/model (Intel, AMD, NVIDIA, virtio-gpu)

#### 22.2 Virtio-GPU Driver (for QEMU)
- 2D rendering commands
- Framebuffer resize
- Display scanning

#### 22.3 2D Acceleration
- Hardware-accelerated rectangle fill
- Hardware-accelerated image copy (blit)
- Cursor rendering

#### 22.4 Graphics Abstraction Layer
```rust
trait GpuDevice {
    fn fill_rect(&self, rect: Rect, color: Color);
    fn blit(&self, src: Rect, dst: Rect);
    fn set_cursor(&self, pos: Point);
    fn present(&self);
}
```

**Test Criteria**:
- GPU detected in QEMU
- Virtio-GPU framebuffer works
- Blitting is faster than software rendering

**Architectural Notes**:
- Virtio-GPU is QEMU's virtual GPU — good for development
- Real GPU drivers (Intel i915, AMD amdgpu) are massive — Phase 22+ extension
- 2D acceleration is sufficient for desktop compositing
- 3D (Vulkan/OpenGL) is a multi-year effort — defer to Phase 26+

---

### Phase 23 — AI Integration

**Goal**: Native AI capabilities. Intelligent system features.

**Dependencies**: Phase 14, Phase 19

**Deliverables**:

#### 23.1 Local Model Inference
- GGUF model loading (Llama, Mistral, etc.)
- CPU inference engine (no GPU requirement initially)
- Token generation API

#### 23.2 AI Assistant
- Natural language interface
- System commands via AI ("open file manager", "change wallpaper")
- Contextual help ("what does this error mean?")
- Voice input/output (optional)

#### 23.3 Intelligent Scheduling
- AI predicts which process to run next
- Background process prioritization
- Power-aware scheduling (battery optimization)

#### 23.4 Smart File Management
- AI-powered file search (semantic, not just filename)
- Duplicate file detection
- Auto-organization suggestions

**Test Criteria**:
- Can load and run a small language model
- AI assistant responds to text queries
- Scheduling improves responsiveness

**Architectural Notes**:
- AI inference must not block the system (background threads)
- Model storage needs significant disk space
- Privacy: all inference runs locally (no cloud dependency)
- Future: multimodal AI (vision, audio), on-device training

---

### Phase 24 — Multi-Boot & Installer

**Goal**: Install INDOMINUS on real hardware. Dual-boot with other OSes.

**Dependencies**: Phase 7, Phase 8

**Deliverables**:

#### 24.1 Disk Installer
- Bootable installer USB creation
- Disk partitioning (GPT)
- Format partitions (FAT32 for ESP, ext4 for root)
- Copy system files
- Install bootloader

#### 24.2 Bootloader Menu
- GRUB-like boot menu
- Select OS to boot (INDOMINUS, Windows, Linux)
- Timeout with default selection
- Kernel command-line options

#### 24.3 Multi-OS Detection
- Scan for existing OS installations
- Add entries to boot menu automatically
- Detect Windows Boot Manager, GRUB, etc.

**Test Criteria**:
- Can create bootable installer USB
- Can install INDOMINUS on a real disk
- Can boot INDOMINUS from installed disk
- Can dual-boot with Windows

**Architectural Notes**:
- Installer must be robust (data loss risk)
- Bootloader must handle UEFI Secure Boot
- Dual-boot must not break existing OS
- Future: in-place upgrade, repair mode

---

### Phase 25 — Power Management

**Goal**: Efficient power usage. Laptop battery life. Sleep/wake.

**Dependencies**: Phase 2, Phase 5

**Deliverables**:

#### 25.1 ACPI Power Management
- S3 (Suspend to RAM)
- S4 (Suspend to Disk)
- S5 (Shutdown)
- G3 (Mechanical Off)

#### 25.2 CPU Frequency Scaling
- Intel SpeedStep / AMD Cool'n'Quiet
- Dynamic frequency adjustment based on load
- Performance / Balanced / Power Saver modes

#### 25.3 Thermal Management
- Temperature sensor reading
- Fan control (if available)
- Thermal throttling

#### 25.4 Idle States
- C-states for CPU (deeper sleep when idle)
- Timer coalescing (reduce wake-ups)

**Test Criteria**:
- System can enter S3 sleep and wake
- System can shut down cleanly
- CPU frequency scales with load
- Idle power consumption is low

**Architectural Notes**:
- Power management is critical for laptop viability
- ACPI compliance is mandatory for real hardware
- Sleep/wake must preserve all state (memory, devices)
- Future: runtime power management for individual devices

---

### Phase 26 — Advanced Features

**Goal**: Enterprise-grade features. Virtualization, containers, updates.

**Dependencies**: Phase 10, Phase 21

**Deliverables**:

#### 26.1 Virtualization
- KVM-like API for running VMs
- Hardware-assisted virtualization (VT-x/AMD-V)
- Virtual machine creation and management

#### 26.2 Containers
- Namespace isolation (PID, network, mount, user)
- Cgroup resource limits
- Container runtime (similar to Docker)

#### 26.3 Snapshots
- Filesystem snapshots (Btrfs-like)
- System restore points
- Rollback after failed updates

#### 26.4 Live Updates
- Apply security patches without reboot
- Kernel live patching (if possible)
- Graceful service restart

**Test Criteria**:
- Can run a VM inside INDOMINUS
- Container isolation prevents escape
- Snapshot can restore system state
- Live update applies patch without reboot

**Architectural Notes**:
- Virtualization is essential for developers (running other OSes)
- Containers are essential for deployment (microservices)
- Snapshots require copy-on-write filesystem
- Live updates are complex — kernel live patching is cutting-edge

---

### Phase 27 — Polish & Optimization

**Goal**: Make it fast, small, and polished. Production quality.

**Dependencies**: All previous phases

**Deliverables**:

#### 27.1 Boot Time Optimization
- Parallel driver initialization
- Lazy service loading
- Kernel compression (LZ4/ZSTD)
- Target: < 5 second boot to desktop

#### 27.2 Memory Optimization
- Kernel memory footprint analysis
- Remove unused code/data
- Compress in-memory data
- Target: < 256 MiB idle usage

#### 27.3 Latency Optimization
- Interrupt latency < 10 μs
- Context switch < 5 μs
- System call latency < 1 μs
- Input latency < 16 ms (60 fps)

#### 27.4 Documentation
- Kernel API documentation (rustdoc)
- Driver development guide
- User manual
- Administrator guide
- Developer documentation

#### 27.5 Testing
- Unit tests for kernel subsystems
- Integration tests for system calls
- Hardware compatibility testing
- Stress testing (memory, I/O, CPU)

**Test Criteria**:
- Boot time meets target
- Memory usage meets target
- Latency meets targets
- Documentation is complete and accurate

---

### Phase 28 — Release Candidate

**Goal**: Ready for real users. ISO image. Beta program.

**Dependencies**: Phase 27

**Deliverables**:

#### 28.1 ISO Image
- Bootable ISO image (UEFI + BIOS fallback)
- Live mode (run from USB without install)
- Installer included

#### 28.2 Hardware Testing
- Test on 10+ real hardware configurations
- Document compatibility list
- Fix hardware-specific issues

#### 28.3 Beta Program
- Public beta release
- Bug tracking system
- Community feedback integration
- Regular updates

#### 28.4 Release
- Stable release
- Release notes
- Upgrade path from beta
- Long-term support (LTS) commitment

**Test Criteria**:
- ISO boots on real hardware
- Installer completes successfully
- Desktop is usable for daily tasks
- No critical bugs in core functionality

---

## Appendix A: File Structure (Future State)

```
indominux rex operating system/
├── Cargo.toml                    # Workspace root
├── Makefile
├── build.ps1
├── docs/
│   ├── ROADMAP.md
│   ├── architecture/
│   │   ├── ADR-001-kernel-strategy.md
│   │   └── ...
│   └── user-guide/
│       └── ...
├── libs/
│   └── indo-core/                # Shared types (boot protocol, syscall ABI)
├── bootloader/
│   └── indo-boot/                # UEFI bootloader
├── kernel/
│   ├── indo-kernel/              # Main kernel crate
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   ├── memory/
│   │   │   │   ├── pmm.rs        # Physical Memory Manager
│   │   │   │   ├── vmm.rs        # Virtual Memory Manager
│   │   │   │   ├── heap.rs       # Kernel heap allocator
│   │   │   │   └── page_table.rs # Page table manipulation
│   │   │   ├── interrupt/
│   │   │   │   ├── idt.rs        # Interrupt Descriptor Table
│   │   │   │   ├── lapic.rs      # Local APIC
│   │   │   │   ├── ioapic.rs     # IO-APIC
│   │   │   │   ├── pit.rs        # Programmable Interval Timer
│   │   │   │   └── irq.rs        # IRQ dispatch
│   │   │   ├── process/
│   │   │   │   ├── process.rs    # Process structure
│   │   │   │   ├── scheduler.rs  # Task scheduler
│   │   │   │   ├── context.rs    # Context switch
│   │   │   │   └── signal.rs     # Signal handling
│   │   │   ├── syscall/
│   │   │   │   ├── mod.rs        # Syscall dispatch
│   │   │   │   ├── io.rs         # read/write syscalls
│   │   │   │   ├── process.rs    # fork/exec/exit
│   │   │   │   └── memory.rs     # mmap/brk
│   │   │   ├── drivers/
│   │   │   │   ├── pci.rs        # PCI enumeration
│   │   │   │   ├── ahci.rs       # AHCI storage
│   │   │   │   ├── nvme.rs       # NVMe storage
│   │   │   │   ├── e1000.rs      # Intel NIC
│   │   │   │   ├── ps2.rs        # PS/2 keyboard/mouse
│   │   │   │   ├── usb/
│   │   │   │   │   ├── xhci.rs   # USB host controller
│   │   │   │   │   ├── hid.rs    # USB HID
│   │   │   │   │   └── storage.rs# USB storage
│   │   │   │   ├── framebuffer.rs# Framebuffer display
│   │   │   │   └── hda.rs        # HD Audio
│   │   │   ├── fs/
│   │   │   │   ├── vfs.rs        # Virtual Filesystem
│   │   │   │   ├── fat32.rs      # FAT32 driver
│   │   │   │   ├── ext2.rs       # ext2 driver
│   │   │   │   └── tmpfs.rs      # tmpfs
│   │   │   ├── net/
│   │   │   │   ├── stack.rs      # TCP/IP stack
│   │   │   │   ├── socket.rs     # Socket API
│   │   │   │   ├── tcp.rs        # TCP implementation
│   │   │   │   ├── udp.rs        # UDP implementation
│   │   │   │   ├── ip.rs         # IP routing
│   │   │   │   ├── arp.rs        # ARP
│   │   │   │   └── dns.rs        # DNS resolver
│   │   │   ├── graphics/
│   │   │   │   ├── compositor.rs # Window compositor
│   │   │   │   ├── window.rs     # Window management
│   │   │   │   └── gpu.rs        # GPU abstraction
│   │   │   ├── audio/
│   │   │   │   ├── hda.rs        # HD Audio driver
│   │   │   │   └── mixer.rs      # Audio mixer
│   │   │   ├── security/
│   │   │   │   ├── aslr.rs       # Address space randomization
│   │   │   │   ├── seccomp.rs    # Seccomp filtering
│   │   │   │   └── audit.rs      # Audit logging
│   │   │   ├── serial.rs
│   │   │   ├── gdt.rs
│   │   │   └── panic.rs
│   │   └── kernel.ld
│   └── ...
├── userspace/
│   ├── init/                     # Init process (PID 1)
│   ├── shell/                    # indosh (shell)
│   ├── terminal/                 # Terminal emulator
│   ├── file-manager/             # File manager
│   ├── text-editor/              # Text editor
│   ├── settings/                 # System settings
│   ├── package-manager/          # indo-pkg
│   ├── ai-assistant/             # AI assistant
│   └── libs/
│       ├── libc/                 # C standard library (subset)
│       ├── libui/                # UI toolkit
│       └── libnet/               # Networking library
├── sdk/
│   ├── toolchain/                # Cross-compiler
│   ├── headers/                  # System headers
│   └── examples/                 # Example applications
└── installer/
    ├── install.sh                # Installation script
    └── grub.cfg                  # Boot menu configuration
```

---

## Appendix B: Dependency Graph

```
Phase 0 (Bootstrap)
    │
    ├──> Phase 1 (Memory Management)
    │        │
    │        ├──> Phase 2 (Interrupts & Timers)
    │        │        │
    │        │        ├──> Phase 3 (Processes & Scheduling)
    │        │        │        │
    │        │        │        └──> Phase 4 (System Calls)
    │        │        │                 │
    │        │        │                 ├──> Phase 8 (User Space)
    │        │        │                 │        │
    │        │        │                 │        └──> Phase 10 (Process Lifecycle)
    │        │        │                 │
    │        │        │                 └──> Phase 9 (Memory Protection)
    │        │        │
    │        │        └──> Phase 12 (Input System)
    │        │
    │        ├──> Phase 5 (Device Discovery)
    │        │        │
    │        │        ├──> Phase 6 (Storage & Block Devices)
    │        │        │        │
    │        │        │        └──> Phase 7 (Filesystem)
    │        │        │                 │
    │        │        │                 └──> Phase 24 (Multi-Boot)
    │        │        │
    │        │        ├──> Phase 14 (Networking)
    │        │        │
    │        │        ├──> Phase 15 (USB)
    │        │        │
    │        │        └──> Phase 25 (Power Management)
    │        │
    │        └──> Phase 11 (Display & Graphics)
    │                 │
    │                 └──> Phase 13 (Window Manager)
    │                          │
    │                          ├──> Phase 18 (UI Toolkit)
    │                          │
    │                          └──> Phase 22 (GPU Acceleration)
    │
    └──> Phase 16 (Audio)
             │
             └──> Phase 17 (Advanced Storage)

Phase 4 (System Calls) ──> Phase 21 (Security Hardening)
Phase 14 (Networking) ──> Phase 19 (Application Ecosystem)
Phase 13 (Window Manager) ──> Phase 20 (Shell & Utilities)
Phase 10 (Process Lifecycle) ──> Phase 26 (Advanced Features)
All phases ──> Phase 27 (Polish & Optimization) ──> Phase 28 (Release)
```

---

## Appendix C: Key Architectural Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Language | Rust (nightly) | Memory safety without GC, zero-cost abstractions |
| Kernel model | Monolithic with modular drivers | Proven at scale (Linux), avoids IPC overhead |
| Bootloader | Custom UEFI | Full control, minimal attack surface |
| Memory management | Bitmap PMM + hierarchical VMM | Simple, correct, scalable |
| Scheduler | Round-robin (initial) | Fair, simple, correct; replace with CFS later |
| Syscall mechanism | `syscall`/`sysret` | Fastest path on x86_64 |
| Filesystem | VFS + FAT32 + ext2 | FAT32 for UEFI, ext2 for root |
| Graphics | Framebuffer → compositor → GPU accel | Incremental complexity |
| Networking | Custom TCP/IP stack | Full control, security |
| Package format | Signed tarballs | Simple, verifiable, efficient |

---

## Appendix D: Estimated Timeline

| Phase | Estimated Effort | Notes |
|-------|-----------------|-------|
| Phase 0 | ✅ Done | — |
| Phase 1 | 2-3 weeks | Critical path, must be correct |
| Phase 2 | 1-2 weeks | LAPIC + IO-APIC + PIT |
| Phase 3 | 2-3 weeks | Context switch is tricky |
| Phase 4 | 1-2 weeks | Syscall ABI must be stable |
| Phase 5 | 1-2 weeks | PCI + ACPI parser |
| Phase 6 | 2-3 weeks | AHCI/NVMe are complex |
| Phase 7 | 2-3 weeks | VFS + FAT32 |
| Phase 8 | 1-2 weeks | ELF loader + shell |
| Phase 9 | 2-3 weeks | Memory protection is critical |
| Phase 10 | 2-3 weeks | fork/exec/signals |
| Phase 11 | 1-2 weeks | Framebuffer + fonts |
| Phase 12 | 1-2 weeks | PS/2 input |
| Phase 13 | 3-4 weeks | Compositor is complex |
| Phase 14 | 4-6 weeks | TCP/IP stack is massive |
| Phase 15 | 2-3 weeks | XHCI is complex |
| Phase 16 | 1-2 weeks | HDA audio |
| Phase 17 | 2-3 weeks | ext2 + partition parsing |
| Phase 18 | 2-3 weeks | UI toolkit |
| Phase 19 | 2-3 weeks | Package manager |
| Phase 20 | 3-4 weeks | Multiple applications |
| Phase 21 | 2-3 weeks | Security hardening |
| Phase 22 | 2-3 weeks | GPU driver |
| Phase 23 | 3-4 weeks | AI integration |
| Phase 24 | 2-3 weeks | Installer |
| Phase 25 | 2-3 weeks | Power management |
| Phase 26 | 4-6 weeks | Virtualization + containers |
| Phase 27 | 4-6 weeks | Polish + optimization |
| Phase 28 | 2-4 weeks | Release preparation |

**Total estimated time**: ~60-90 weeks (1.5-2 years of focused development)

---

## Appendix E: Risk Register

| Risk | Severity | Mitigation |
|------|----------|------------|
| Hardware compatibility | HIGH | Test on multiple real machines early |
| TCP/IP stack complexity | HIGH | Implement incrementally, use proven algorithms |
| GPU driver complexity | HIGH | Start with virtio-gpu, defer native GPU drivers |
| Security vulnerabilities | HIGH | Audit critical paths, fuzz testing |
| Performance regression | MEDIUM | Benchmark at each phase, optimize incrementally |
| Developer burnout | MEDIUM | Celebrate milestones, maintain work-life balance |
| Scope creep | HIGH | Stick to phase plan, defer "nice to have" features |
| Dependency on nightly Rust | MEDIUM | Track Rust stabilization, migrate when possible |

---

*This roadmap is a living document. Update as phases are completed and new requirements emerge.*
