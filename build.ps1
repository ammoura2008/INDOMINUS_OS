# build.ps1 - INDOMINUS OS Windows Build Script
#
# This script builds the INDOMINUS OS natively on Windows without requiring
# WSL2, mtools, or admin privileges.
#
# Usage:
#   .\build.ps1          # Build everything
#   .\build.ps1 run      # Build and run in QEMU
#   .\build.ps1 clean    # Clean build artifacts

param (
    [string]$Action = "build"
)

$ErrorActionPreference = "Stop"
$Profile = "release"
$BootTarget = "x86_64-unknown-uefi"
$KernelTarget = "x86_64-unknown-none"
$RustTargetDir = "target"
$BuildDir = "build"
$EspDir = "$BuildDir\esp"

# Build artifacts
$BootEfi = "$RustTargetDir\$BootTarget\$Profile\indo-boot.efi"
$KernelElf = "$RustTargetDir\$KernelTarget\$Profile\indo-kernel"

# OVMF setup (UEFI firmware) — use QEMU's bundled edk2 firmware
$OvmfFile = "C:\Program Files\qemu\share\edk2-x86_64-code.fd"

function Build-Bootloader {
    Write-Host "[BUILD] Compiling bootloader (indo-boot)..." -ForegroundColor Cyan
    $releaseFlag = if ($Profile -eq "release") { "--release" } else { "" }
    cargo build --package indo-boot --target $BootTarget $releaseFlag
    if ($LASTEXITCODE -ne 0) { throw "Bootloader build failed" }
}

function Build-Kernel {
    Write-Host "`n[BUILD] Compiling kernel (indo-kernel)..." -ForegroundColor Cyan
    $releaseFlag = if ($Profile -eq "release") { "--release" } else { "" }
    cargo build --package indo-kernel --target $KernelTarget $releaseFlag
    if ($LASTEXITCODE -ne 0) { throw "Kernel build failed" }
}

function Setup-ESP {
    Write-Host "`n[IMAGE] Preparing EFI System Partition directory..." -ForegroundColor Cyan
    
    if (-not (Test-Path "$EspDir\EFI\BOOT")) {
        New-Item -ItemType Directory -Force -Path "$EspDir\EFI\BOOT" | Out-Null
    }
    if (-not (Test-Path "$EspDir\EFI\INDOMINUS")) {
        New-Item -ItemType Directory -Force -Path "$EspDir\EFI\INDOMINUS" | Out-Null
    }

    # Copy bootloader to fallback path
    Copy-Item $BootEfi -Destination "$EspDir\EFI\BOOT\BOOTX64.EFI" -Force
    Write-Host "  -> Bootloader installed"

    # Remove stale kernel before copy
    $dest = "$EspDir\EFI\INDOMINUS\kernel.elf"
    if (Test-Path $dest) { Remove-Item $dest -Force }

    # Copy kernel
    Copy-Item $KernelElf -Destination $dest -Force
    $size = (Get-Item $dest).Length
    Write-Host "  -> Kernel installed ($([math]::Round($size/1024, 1)) KB)"
}

function Setup-OVMF {
    if (-not (Test-Path $OvmfFile)) {
        Write-Host "[ERROR] OVMF firmware not found at: $OvmfFile" -ForegroundColor Red
        throw "OVMF not found"
    }
    Write-Host "[SETUP] Using OVMF: $OvmfFile" -ForegroundColor Green
}

function Run-QEMU {
    Write-Host "`n[QEMU] Launching INDOMINUS in QEMU..." -ForegroundColor Green
    Write-Host "[QEMU] Serial output will appear below. Close QEMU window to exit." -ForegroundColor Yellow
    Write-Host "──────────────────────────────────────────────────────────`n"
    
    # QEMU's 'fat:rw:DIR' creates a virtual FAT drive from a local directory!
    # This avoids needing mtools or admin rights to mount VHDs on Windows.
    $QemuArgs = @(
        "-machine", "q35",
        "-cpu", "qemu64",
        "-m", "256M",
        "-drive", "if=pflash,format=raw,readonly=on,file=$OvmfFile",
        "-drive", "format=raw,file=fat:rw:$EspDir",
        "-serial", "stdio",
        "-no-reboot",
        "-no-shutdown"
    )

    & "C:\Program Files\qemu\qemu-system-x86_64.exe" @QemuArgs
}

function Clean-Project {
    Write-Host "[CLEAN] Removing build artifacts..." -ForegroundColor Cyan
    cargo clean
    if (Test-Path $BuildDir) {
        Remove-Item -Recurse -Force $BuildDir
    }
    Write-Host "[CLEAN] Done."
}

# Main Execution
if ($Action -eq "clean") {
    Clean-Project
    exit
}

Setup-OVMF
Build-Bootloader
Build-Kernel

# Verify kernel ELF before deploying
Write-Host "`n[VERIFY] Validating kernel ELF..." -ForegroundColor Cyan
$verifyScript = Join-Path $PSScriptRoot "tools\verify_kernel.py"
if (Test-Path $verifyScript) {
    python $verifyScript $KernelElf
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ERROR] Kernel verification FAILED — not deploying" -ForegroundColor Red
        throw "Kernel verification failed"
    }
} else {
    Write-Host "  [SKIP] verify_kernel.py not found" -ForegroundColor Yellow
}

Setup-ESP

if ($Action -eq "run") {
    Run-QEMU
} else {
    Write-Host "`n══════════════════════════════════════════" -ForegroundColor Green
    Write-Host "  INDOMINUS Build Complete"
    Write-Host "  ESP Directory: $EspDir"
    Write-Host "  Run with:      .\build.ps1 run"
    Write-Host "══════════════════════════════════════════"
}
