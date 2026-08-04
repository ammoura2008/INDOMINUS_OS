#!/usr/bin/env python3
"""Lightweight smoke verification for the INDOMINUS build artifacts.

This script verifies that the bootloader and kernel artifacts produced by the
build pipeline exist in the expected locations and have the expected format.
It then stages them into an ESP image and launches QEMU for a short boot smoke
check that proves the kernel reaches an early stable boot stage.
"""

import argparse
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def ensure_file(path: Path, description: str) -> None:
    if not path.exists():
        raise RuntimeError(f"Missing {description}: {path}")
    if path.stat().st_size == 0:
        raise RuntimeError(f"Empty {description}: {path}")


def verify_bootloader(path: Path) -> None:
    ensure_file(path, "bootloader binary")
    data = path.read_bytes()[:2]
    if data[0] != 0x4D or data[1] != 0x5A:
        raise RuntimeError(
            f"Bootloader is not a PE32+ image: expected MZ header, got {data.hex()}"
        )


def verify_kernel(path: Path) -> None:
    ensure_file(path, "kernel ELF")
    subprocess.run(
        [sys.executable, str(ROOT / "tools" / "verify_kernel.py"), str(path)],
        check=True,
        cwd=ROOT,
    )


def find_qemu_executable() -> str | None:
    candidates = [
        r"C:\Program Files\qemu\qemu-system-x86_64.exe",
        r"C:\Program Files\qemu\qemu-system-x86_64w.exe",
        "qemu-system-x86_64.exe",
        "qemu-system-x86_64",
    ]
    for candidate in candidates:
        path = shutil.which(candidate) if candidate not in {r"C:\Program Files\qemu\qemu-system-x86_64.exe", r"C:\Program Files\qemu\qemu-system-x86_64w.exe"} else candidate
        if path and Path(path).exists():
            return str(path)
    return None


def find_ovmf_path() -> Path | None:
    candidates = [
        Path(r"C:\Program Files\qemu\share\edk2-x86_64-code.fd"),
        Path("/usr/share/OVMF/OVMF_CODE.fd"),
        Path("/usr/share/OVMF/OVMF.fd"),
        Path("/usr/share/edk2/x64/OVMF_CODE.fd"),
        Path("/usr/share/OVMF/OVMF_CODE.secboot.fd"),
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return None


def prepare_esp(boot_path: Path, kernel_path: Path) -> Path:
    esp_dir = ROOT / "build" / "esp"
    boot_dir = esp_dir / "EFI" / "BOOT"
    kernel_dir = esp_dir / "EFI" / "INDOMINUS"
    boot_dir.mkdir(parents=True, exist_ok=True)
    kernel_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(boot_path, boot_dir / "BOOTX64.EFI")
    shutil.copy2(kernel_path, kernel_dir / "kernel.elf")
    return esp_dir


def verify_boot(boot_path: Path, kernel_path: Path, timeout_seconds: int = 35) -> str:
    qemu_exe = find_qemu_executable()
    ovmf_path = find_ovmf_path()
    if not qemu_exe:
        raise RuntimeError("QEMU executable not found")
    if not ovmf_path:
        raise RuntimeError("OVMF firmware not found")

    esp_dir = prepare_esp(boot_path, kernel_path)
    print(f"[SMOKE] Launching QEMU boot smoke test (timeout={timeout_seconds}s)")
    args = [
        qemu_exe,
        "-machine",
        "q35",
        "-cpu",
        "qemu64",
        "-m",
        "256M",
        "-display",
        "none",
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={ovmf_path}",
        "-drive",
        f"format=raw,file=fat:rw:{esp_dir}",
        "-serial",
        "stdio",
        "-no-reboot",
        "-no-shutdown",
    ]

    proc = subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    output_chunks: list[str] = []

    def read_output() -> None:
        try:
            while True:
                chunk = proc.stdout.read(4096)
                if not chunk:
                    break
                output_chunks.append(chunk)
        except Exception:
            pass

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    time.sleep(timeout_seconds)

    if proc.poll() is None:
        proc.kill()
        proc.wait(timeout=5)

    reader.join(timeout=5)
    output = "".join(output_chunks)

    if not output:
        raise RuntimeError("QEMU produced no serial output")

    markers = []
    if "[KERNEL] INDOMINUS OS" in output:
        markers.append("kernel banner")
    if "[MARK] After process init" in output or "[KERNEL] All init done." in output:
        markers.append("early kernel init")
    if "Indominus OS Shell" in output:
        markers.append("shell banner")

    if not markers:
        sample = output[-2000:] if len(output) > 2000 else output
        raise RuntimeError(f"Kernel did not reach an expected boot stage. Output sample:\n{sample}")

    print(f"[SMOKE] Boot markers observed: {', '.join(markers)}")
    return output


def main() -> int:
    parser = argparse.ArgumentParser(description="Run a build artifact and boot smoke check")
    parser.add_argument(
        "--profile",
        default="debug",
        choices=["debug", "release"],
        help="Artifact profile to verify",
    )
    args = parser.parse_args()

    profile = args.profile
    boot_path = ROOT / "target" / "x86_64-unknown-uefi" / profile / "indo-boot.efi"
    kernel_path = ROOT / "target" / "x86_64-unknown-none" / profile / "indo-kernel"

    print(f"[SMOKE] Checking profile: {profile}")
    print(f"[SMOKE] Bootloader: {boot_path}")
    print(f"[SMOKE] Kernel: {kernel_path}")

    try:
        verify_bootloader(boot_path)
        verify_kernel(kernel_path)
        verify_boot(boot_path, kernel_path)
    except Exception as exc:  # pylint: disable=broad-exception-caught
        print(f"[SMOKE] FAILED: {exc}", file=sys.stderr)
        return 1

    print("[SMOKE] RESULT: PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
