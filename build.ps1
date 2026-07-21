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

# Verify binary formats before deploying.
# This catches the most dangerous build mistake: swapping targets.
# Bootloader must be a PE32+ (UEFI) binary. Kernel must be ELF.
Write-Host "`n[VERIFY] Validating binary formats..." -ForegroundColor Cyan

function Verify-BootloaderPE {
    param ([string]$Path)
    if (-not (Test-Path $Path)) {
        Write-Host "[ERROR] Bootloader not found at: $Path" -ForegroundColor Red
        throw "Bootloader not found"
    }
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    # PE32+ must start with "MZ" (0x4D, 0x5A)
    if ($bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        $got = "0x$($bytes[0].ToString('X2')) 0x$($bytes[1].ToString('X2'))"
        Write-Host "[ERROR] Bootloader is NOT a PE32+ binary!" -ForegroundColor Red
        Write-Host "  Expected: MZ header (0x4D 0x5A) — this is a UEFI application" -ForegroundColor Red
        Write-Host "  Got:      $got ($($bytes.Length) bytes)" -ForegroundColor Red
        Write-Host "  Cause:    Built with wrong target (x86_64-unknown-none instead of x86_64-unknown-uefi)" -ForegroundColor Red
        throw "Bootloader format check failed — wrong cargo target"
    }
    if ($bytes.Length -lt 40000) {
        Write-Host "[ERROR] Bootloader suspiciously small: $($bytes.Length) bytes (expected >= 40000)" -ForegroundColor Red
        throw "Bootloader size check failed"
    }
    Write-Host "  Bootloader OK: $($bytes.Length) bytes, PE32+ header confirmed" -ForegroundColor Green
}

function Verify-KernelELF {
    param ([string]$Path)
    if (-not (Test-Path $Path)) {
        Write-Host "[ERROR] Kernel not found at: $Path" -ForegroundColor Red
        throw "Kernel not found"
    }
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    # ELF must start with 0x7F 'E' 'L' 'F' (0x7F, 0x45, 0x4C, 0x46)
    if ($bytes[0] -ne 0x7F -or $bytes[1] -ne 0x45 -or $bytes[2] -ne 0x4C -or $bytes[3] -ne 0x46) {
        $got = ($bytes[0..3] | ForEach-Object { "0x$($_.ToString('X2'))" }) -join " "
        Write-Host "[ERROR] Kernel is NOT an ELF binary!" -ForegroundColor Red
        Write-Host "  Expected: 0x7F 0x45 0x4C 0x46 (\\x7fELF)" -ForegroundColor Red
        Write-Host "  Got:      $got ($($bytes.Length) bytes)" -ForegroundColor Red
        Write-Host "  Cause:    Built with wrong target (x86_64-unknown-uefi instead of x86_64-unknown-none)" -ForegroundColor Red
        throw "Kernel format check failed — wrong cargo target"
    }
    if ($bytes.Length -lt 30000) {
        Write-Host "[ERROR] Kernel suspiciously small: $($bytes.Length) bytes (expected >= 30000)" -ForegroundColor Red
        throw "Kernel size check failed"
    }
    Write-Host "  Kernel OK: $($bytes.Length) bytes, ELF header confirmed" -ForegroundColor Green
}

# Bootloader: must be PE32+ (UEFI)
Verify-BootloaderPE $BootEfi
# Kernel: must be ELF
Verify-KernelELF $KernelElf

# Kernel verification (existing check)
$verifyScript = Join-Path $PSScriptRoot "tools\verify_kernel.py"
if (Test-Path $verifyScript) {
    python $verifyScript $KernelElf
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ERROR] Kernel verification FAILED — not deploying" -ForegroundColor Red
        throw "Kernel verification failed"
    }
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
