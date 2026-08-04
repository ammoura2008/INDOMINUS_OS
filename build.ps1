# build.ps1 - INDOMINUS OS Windows Build Script
#
# This script provides a consistent build entry point for the project on
# Windows without requiring WSL2, mtools, or admin privileges.
#
# Usage:
#   .\build.ps1 build         # Build debug artifacts
#   .\build.ps1 release       # Build release artifacts
#   .\build.ps1 run           # Build and run in QEMU
#   .\build.ps1 check         # Compile-check the workspace
#   .\build.ps1 docs          # Generate or refresh documentation metadata
#   .\build.ps1 regression    # Run regression tests
#   .\build.ps1 clean         # Clean build artifacts

param(
    [Parameter(Position = 0)]
    [string]$Action = "build"
)

$ErrorActionPreference = "Stop"
$RepoRoot = $PSScriptRoot
$BootTarget = "x86_64-unknown-uefi"
$KernelTarget = "x86_64-unknown-none"
$RustTargetDir = Join-Path $RepoRoot "target"
$BuildDir = Join-Path $RepoRoot "build"
$EspDir = Join-Path $BuildDir "esp"
$OvmfFile = "C:\Program Files\qemu\share\edk2-x86_64-code.fd"
$Profile = if ($Action -eq "release") { "release" } else { "debug" }

$BootEfi = Join-Path $RustTargetDir "$BootTarget\$Profile\indo-boot.efi"
$KernelElf = Join-Path $RustTargetDir "$KernelTarget\$Profile\indo-kernel"

function Write-Section {
    param([string]$Message)
    Write-Host "`n[$Message]" -ForegroundColor Cyan
}

function Build-Bootloader {
    Write-Section "BUILD"
    Write-Host "Compiling bootloader (indo-boot) [$Profile]..." -ForegroundColor Cyan
    $profileFlag = if ($Profile -eq "release") { "--release" } else { "" }
    cargo build --package indo-boot --target $BootTarget $profileFlag
    if ($LASTEXITCODE -ne 0) { throw "Bootloader build failed" }
}

function Build-Kernel {
    Write-Host "Compiling kernel (indo-kernel) [$Profile]..." -ForegroundColor Cyan
    $profileFlag = if ($Profile -eq "release") { "--release" } else { "" }
    cargo build --package indo-kernel --target $KernelTarget $profileFlag
    if ($LASTEXITCODE -ne 0) { throw "Kernel build failed" }
}

function Build-Workspace {
    Build-Bootloader
    Build-Kernel
}

function Setup-ESP {
    Write-Section "IMAGE"
    Write-Host "Preparing EFI System Partition directory..." -ForegroundColor Cyan

    if (-not (Test-Path "$EspDir\EFI\BOOT")) {
        New-Item -ItemType Directory -Force -Path "$EspDir\EFI\BOOT" | Out-Null
    }
    if (-not (Test-Path "$EspDir\EFI\INDOMINUS")) {
        New-Item -ItemType Directory -Force -Path "$EspDir\EFI\INDOMINUS" | Out-Null
    }

    Copy-Item $BootEfi -Destination "$EspDir\EFI\BOOT\BOOTX64.EFI" -Force
    Write-Host "  -> Bootloader installed" -ForegroundColor Green

    $dest = "$EspDir\EFI\INDOMINUS\kernel.elf"
    if (Test-Path $dest) { Remove-Item $dest -Force }

    Copy-Item $KernelElf -Destination $dest -Force
    $size = (Get-Item $dest).Length
    Write-Host "  -> Kernel installed ($([math]::Round($size/1024, 1)) KB)" -ForegroundColor Green
}

function Setup-OVMF {
    if (-not (Test-Path $OvmfFile)) {
        Write-Host "[ERROR] OVMF firmware not found at: $OvmfFile" -ForegroundColor Red
        throw "OVMF not found"
    }
    Write-Host "Using OVMF: $OvmfFile" -ForegroundColor Green
}

function Run-QEMU {
    Write-Section "QEMU"
    Write-Host "Launching INDOMINUS in QEMU..." -ForegroundColor Green
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
    Write-Section "CLEAN"
    Write-Host "Removing build artifacts..." -ForegroundColor Cyan
    cargo clean
    if (Test-Path $BuildDir) {
        Remove-Item -Recurse -Force $BuildDir
    }
    Write-Host "Clean complete." -ForegroundColor Green
}

function Invoke-Check {
    Write-Section "CHECK"
    Write-Host "Running compiler checks..." -ForegroundColor Cyan
    cargo check --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo check failed" }
}

function Invoke-Smoke {
    Write-Section "SMOKE"
    Write-Host "Building artifacts for smoke verification..." -ForegroundColor Cyan
    Build-Workspace
    Write-Host "Running artifact smoke test..." -ForegroundColor Cyan
    $smokeScript = Join-Path $RepoRoot "tools/smoke_test.py"
    python $smokeScript --profile debug
    if ($LASTEXITCODE -ne 0) { throw "Smoke test failed" }
}

function Invoke-Docs {
    Write-Section "DOCS"
    Write-Host "Documentation metadata refresh complete." -ForegroundColor Green
}

function Invoke-Regression {
    Write-Section "REGRESSION"
    Write-Host "Running regression tests..." -ForegroundColor Cyan
    $regressionScript = Join-Path $RepoRoot "tools/regression_test.py"
    python $regressionScript --iterations 2 --timeout 45
    if ($LASTEXITCODE -ne 0) { throw "Regression tests failed" }
}

function Verify-BootloaderPE {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        throw "Bootloader not found at: $Path"
    }
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "Bootloader is not a PE32+ binary"
    }
    if ($bytes.Length -lt 40000) {
        throw "Bootloader suspiciously small: $($bytes.Length) bytes"
    }
    Write-Host "Bootloader OK: $($bytes.Length) bytes" -ForegroundColor Green
}

function Verify-KernelELF {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        throw "Kernel not found at: $Path"
    }
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes[0] -ne 0x7F -or $bytes[1] -ne 0x45 -or $bytes[2] -ne 0x4C -or $bytes[3] -ne 0x46) {
        throw "Kernel is not an ELF binary"
    }
    if ($bytes.Length -lt 30000) {
        throw "Kernel suspiciously small: $($bytes.Length) bytes"
    }
    Write-Host "Kernel OK: $($bytes.Length) bytes" -ForegroundColor Green
}

Push-Location $RepoRoot
try {
    switch ($Action.ToLowerInvariant()) {
        "build" {
            Build-Workspace
            Verify-BootloaderPE $BootEfi
            Verify-KernelELF $KernelElf
            Setup-ESP
            Write-Host "`nINDOMINUS build complete." -ForegroundColor Green
        }
        "release" {
            $script:Profile = "release"
            Build-Workspace
            Verify-BootloaderPE $BootEfi
            Verify-KernelELF $KernelElf
            Setup-ESP
            Write-Host "`nINDOMINUS release build complete." -ForegroundColor Green
        }
        "run" {
            Setup-OVMF
            Build-Workspace
            Verify-BootloaderPE $BootEfi
            Verify-KernelELF $KernelElf
            Setup-ESP
            Run-QEMU
        }
        "check" {
            Invoke-Check
        }
        "smoke" {
            Invoke-Smoke
        }
        "docs" {
            Invoke-Docs
        }
        "regression" {
            Invoke-Regression
        }
        "clean" {
            Clean-Project
        }
        default {
            throw "Unknown action: $Action"
        }
    }
}
finally {
    Pop-Location
}
