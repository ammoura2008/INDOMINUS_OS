# INDOMINUS OS — Makefile
#
# This Makefile orchestrates building the complete INDOMINUS OS image:
# 1. Build the UEFI bootloader (indo-boot) → .efi file
# 2. Build the kernel (indo-kernel) → .elf file
# 3. Create a disk image with an EFI System Partition
# 4. Copy bootloader and kernel to the correct ESP paths
# 5. Launch QEMU with the disk image and OVMF UEFI firmware
#
# Prerequisites (install these first — see docs/dev-env-setup.md):
#   - Rust (rustup) with nightly toolchain
#   - Targets: x86_64-unknown-uefi, x86_64-unknown-none
#   - QEMU (qemu-system-x86_64)
#   - OVMF firmware files (for UEFI emulation)
#   - mtools (mformat, mcopy — for FAT32 image creation without root)
#   - llvm-tools-preview (for objcopy, to extract .efi from PE)
#
# Usage:
#   make          — Build everything
#   make run      — Build and run in QEMU
#   make clean    — Remove build artifacts
#   make check    — Check compilation without producing binaries

# ─────────────────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────────────────

# Rust toolchain and profile
CARGO     := cargo
PROFILE   := debug
RUST_TARGET_DIR := target

# Bootloader build artifacts
BOOT_TARGET  := x86_64-unknown-uefi
BOOT_EFI     := $(RUST_TARGET_DIR)/$(BOOT_TARGET)/$(PROFILE)/indo-boot.efi

# Kernel build artifacts
KERNEL_TARGET := x86_64-unknown-none
KERNEL_ELF    := $(RUST_TARGET_DIR)/$(KERNEL_TARGET)/$(PROFILE)/indo-kernel

# Disk image configuration
DISK_IMAGE   := build/indominus.img
DISK_SIZE_MB := 64

# OVMF UEFI firmware path
# Adjust this to match your OVMF installation:
#   Linux:   /usr/share/OVMF/OVMF_CODE.fd
#   macOS:   $(brew --prefix)/share/qemu/edk2-x86_64-code.fd
#   Windows: C:/Program Files/qemu/share/OVMF.fd
OVMF_CODE := /usr/share/OVMF/OVMF_CODE.fd
OVMF_VARS := /usr/share/OVMF/OVMF_VARS.fd

# QEMU configuration
QEMU := qemu-system-x86_64
QEMU_FLAGS := \
    -machine q35                     \
    -cpu qemu64                      \
    -m 256M                          \
    -drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE)  \
    -drive if=pflash,format=raw,file=build/OVMF_VARS.fd        \
    -drive format=raw,file=$(DISK_IMAGE)                        \
    -serial stdio                    \
    -display none                    \
    -no-reboot                       \
    -no-shutdown

# Note on QEMU flags:
# -machine q35      : Modern Intel Q35 chipset (PCIe, ICH9 — needed for UEFI)
# -cpu qemu64       : Generic 64-bit CPU, compatible, no host-specific features
# -m 256M           : 256 MiB RAM (enough for Phase 0-2)
# -drive if=pflash  : UEFI firmware in persistent flash (writable VARS)
# -serial stdio     : Map COM1 to terminal (THIS is how we see kernel output)
# -display none     : No graphical display window (Phase 0 is serial-only)
# -no-reboot        : Don't restart on triple fault (let QEMU hang so we see it)
# -no-shutdown      : Keep QEMU alive after shutdown so we can inspect state

# ─────────────────────────────────────────────────────────────────────────────
# Default target
# ─────────────────────────────────────────────────────────────────────────────

.PHONY: all
all: $(DISK_IMAGE)
	@echo ""
	@echo "══════════════════════════════════════════"
	@echo "  INDOMINUS Build Complete"
	@echo "  Disk image: $(DISK_IMAGE)"
	@echo "  Run with:   make run"
	@echo "══════════════════════════════════════════"

# ─────────────────────────────────────────────────────────────────────────────
# Build bootloader
# ─────────────────────────────────────────────────────────────────────────────

.PHONY: boot
boot:
	@echo "[BUILD] Compiling bootloader (indo-boot, target=$(BOOT_TARGET))..."
	$(CARGO) build \
		--package indo-boot \
		--target $(BOOT_TARGET) \
		$(if $(filter release,$(PROFILE)),--release,)
	@echo "[BUILD] Bootloader: $(BOOT_EFI)"

# ─────────────────────────────────────────────────────────────────────────────
# Build kernel
# ─────────────────────────────────────────────────────────────────────────────

.PHONY: kernel
kernel:
	@echo "[BUILD] Compiling kernel (indo-kernel, target=$(KERNEL_TARGET))..."
	$(CARGO) build \
		--package indo-kernel \
		--target $(KERNEL_TARGET) \
		$(if $(filter release,$(PROFILE)),--release,)
	@echo "[BUILD] Kernel ELF: $(KERNEL_ELF)"

# ─────────────────────────────────────────────────────────────────────────────
# Create disk image
# ─────────────────────────────────────────────────────────────────────────────
# We use mtools to create a FAT32 disk image without requiring root privileges.
# mtools operates on raw disk image files and treats them as FAT filesystems.
#
# Why FAT32?
# UEFI firmware can ONLY read FAT12, FAT16, and FAT32 from the EFI System
# Partition. This is specified in the UEFI spec. Our ESP must be FAT32.

$(DISK_IMAGE): boot kernel | build
	@echo "[IMAGE] Creating disk image ($(DISK_SIZE_MB) MiB)..."

	# Create raw disk image file filled with zeros
	dd if=/dev/zero of=$(DISK_IMAGE) bs=1M count=$(DISK_SIZE_MB) status=none

	# Format as FAT32 using mformat.
	# -i: image file, -F: force FAT32, -v: volume label
	mformat -i $(DISK_IMAGE)@@1M -F -v INDOMINUS

	# Create the required EFI directory structure on the image.
	# UEFI looks for bootloaders at \EFI\BOOT\BOOTX64.EFI (fallback path)
	# or at the path stored in UEFI boot variables.
	mmd -i $(DISK_IMAGE)@@1M ::/EFI
	mmd -i $(DISK_IMAGE)@@1M ::/EFI/BOOT
	mmd -i $(DISK_IMAGE)@@1M ::/EFI/INDOMINUS

	# Copy bootloader to the UEFI fallback boot path.
	# BOOTX64.EFI is the standard fallback path for x86_64 UEFI.
	mcopy -i $(DISK_IMAGE)@@1M $(BOOT_EFI) ::/EFI/BOOT/BOOTX64.EFI

	# Copy kernel ELF to its expected location.
	# The bootloader will load it from \EFI\INDOMINUS\kernel.elf
	mcopy -i $(DISK_IMAGE)@@1M $(KERNEL_ELF) ::/EFI/INDOMINUS/kernel.elf

	@echo "[IMAGE] Disk image ready: $(DISK_IMAGE)"

# ─────────────────────────────────────────────────────────────────────────────
# Prepare OVMF VARS (writable UEFI variable storage)
# ─────────────────────────────────────────────────────────────────────────────

build/OVMF_VARS.fd: | build
	@echo "[SETUP] Copying OVMF VARS (writable UEFI variable store)..."
	cp $(OVMF_VARS) build/OVMF_VARS.fd

build:
	mkdir -p build

# ─────────────────────────────────────────────────────────────────────────────
# Run in QEMU
# ─────────────────────────────────────────────────────────────────────────────

.PHONY: run
run: $(DISK_IMAGE) build/OVMF_VARS.fd
	@echo "[QEMU] Launching INDOMINUS in QEMU..."
	@echo "[QEMU] Serial output will appear below. Ctrl+A X to exit."
	@echo "──────────────────────────────────────────────────────────"
	$(QEMU) $(QEMU_FLAGS)

# Run with a graphical window and GDB server for debugging
.PHONY: run-debug
run-debug: $(DISK_IMAGE) build/OVMF_VARS.fd
	@echo "[QEMU] Debug mode: GDB server on localhost:1234"
	$(QEMU) $(QEMU_FLAGS) \
		-display sdl \
		-s -S

# ─────────────────────────────────────────────────────────────────────────────
# Check (compile without linking — fast feedback loop)
# ─────────────────────────────────────────────────────────────────────────────

.PHONY: check
check:
	@echo "[CHECK] Checking indo-core..."
	$(CARGO) check --package indo-core
	@echo "[CHECK] Checking indo-boot..."
	$(CARGO) check --package indo-boot --target $(BOOT_TARGET)
	@echo "[CHECK] Checking indo-kernel..."
	$(CARGO) check --package indo-kernel --target $(KERNEL_TARGET)
	@echo "[CHECK] All checks passed!"

# ─────────────────────────────────────────────────────────────────────────────
# Clean
# ─────────────────────────────────────────────────────────────────────────────

.PHONY: clean
clean:
	@echo "[CLEAN] Removing build artifacts..."
	$(CARGO) clean
	rm -rf build/
	@echo "[CLEAN] Done."
