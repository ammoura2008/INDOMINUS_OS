# INDOMINUS OS — Developer Environment Setup

> **Platform**: This guide targets Linux (Ubuntu 22.04+ / Debian) as the build host.  
> Windows users: use WSL2 (Windows Subsystem for Linux 2).

---

## 1. Install WSL2 (Windows only)

Open PowerShell as Administrator:

```powershell
wsl --install -d Ubuntu-24.04
```

Restart, then open Ubuntu from the Start menu. All commands below run inside WSL2.

---

## 2. Install System Dependencies

```bash
sudo apt update && sudo apt install -y \
    build-essential    \   # gcc, make, etc.
    curl               \   # for rustup
    git                \   # version control
    qemu-system-x86    \   # QEMU emulator
    ovmf               \   # UEFI firmware for QEMU
    mtools             \   # FAT32 image manipulation (no root needed)
    llvm               \   # LLVM tools (objcopy, etc.)
    lld                    # LLVM linker (faster, better for cross-compilation)
```

Verify OVMF is installed:

```bash
ls /usr/share/OVMF/
# Should show: OVMF_CODE.fd  OVMF_VARS.fd
```

---

## 3. Install Rust (nightly toolchain)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install the nightly toolchain (required for no_std kernel features)
rustup toolchain install nightly
rustup default nightly

# Verify
rustc --version
# Expected: rustc 1.XX.0-nightly (...)
```

---

## 4. Install Cross-Compilation Targets

```bash
# Target for our UEFI bootloader (produces PE32+ .efi file)
rustup target add x86_64-unknown-uefi

# Target for our bare-metal kernel (produces ELF with no OS assumptions)
rustup target add x86_64-unknown-none

# Verify
rustup target list --installed | grep x86_64
```

---

## 5. Install Rust Components

```bash
# Rust source code (needed for compiling core + alloc for custom targets)
rustup component add rust-src

# LLVM tools (for objcopy, size, nm — useful for inspecting kernel binaries)
rustup component add llvm-tools-preview

# Rustfmt (code formatting)
rustup component add rustfmt

# Clippy (linter)
rustup component add clippy
```

---

## 6. Clone the Repository

```bash
# Clone into a convenient location within WSL2
git clone https://github.com/indominus-os/indominus ~/indominus
cd ~/indominus
```

Or if working directly from the Windows path:

```bash
cd /mnt/c/Users/USER/Documents/indominux\ rex\ operating\ system/
```

---

## 7. Build and Run

```bash
# Compile everything (bootloader + kernel)
make

# Build and launch in QEMU
make run
```

Expected serial output:

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  INDOMINUS OS — Custom Kernel
  Phase 0 — Bootstrap
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
[KERNEL] Serial output initialized (COM1)
[KERNEL] Validating BootInfo from bootloader...
[KERNEL] BootInfo valid. Protocol version: 1
[KERNEL] Memory map: XX regions, XXX MiB usable RAM
[KERNEL] Initializing GDT...
[KERNEL] GDT loaded
[KERNEL] Initializing IDT...
[KERNEL] IDT loaded
[KERNEL] Interrupts enabled
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  INDOMINUS Phase 0 COMPLETE
  The kernel has control. The Indominus Rex awakens.
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

---

## 8. Debugging with GDB

```bash
# Terminal 1: Start QEMU in debug mode (waits for GDB before booting)
make run-debug

# Terminal 2: Connect GDB
rust-gdb target/x86_64-unknown-none/debug/indo-kernel
(gdb) target remote localhost:1234
(gdb) break kernel_main
(gdb) continue
```

---

## 9. Inspecting the Kernel Binary

```bash
# View ELF sections and sizes
llvm-size target/x86_64-unknown-none/debug/indo-kernel

# View section layout (confirm kernel is linked at 0xFFFFFFFF80000000)
llvm-objdump -h target/x86_64-unknown-none/debug/indo-kernel

# Disassemble the entry point
llvm-objdump -d target/x86_64-unknown-none/debug/indo-kernel | head -100

# View the symbol table
llvm-nm target/x86_64-unknown-none/debug/indo-kernel | sort
```

---

## Troubleshooting

### QEMU triple faults immediately (screen flashes, restarts)

- Check OVMF path in Makefile is correct
- Run with `-d int` QEMU flag to log interrupts: `QEMU_FLAGS += -d int,cpu_reset`
- Run with GDB (`make run-debug`) and break at `kernel_main`

### `cargo: command not found`

```bash
source ~/.cargo/env
```

### mtools errors creating disk image

```bash
sudo apt install mtools
```

### OVMF not found

```bash
sudo apt install ovmf
# Then update OVMF_CODE path in Makefile
```
