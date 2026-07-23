#!/usr/bin/env python3
"""
Indominus OS — Comprehensive Regression Test Suite

Builds kernel, generates test binaries, launches QEMU, captures serial output,
verifies expected results, and reports PASS/FAIL with detailed logs.

Usage:
    python tools/regression_test.py [--iterations N] [--timeout SECONDS] [--verbose]
"""

import subprocess
import os
import sys
import time
import re
import argparse
import json
from pathlib import Path
from dataclasses import dataclass, field
from typing import List, Optional, Dict, Tuple

# ─────────────────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────────────────

WORKSPACE = Path(__file__).parent.parent
BUILD_PS1 = WORKSPACE / "build.ps1"
TOOLS_DIR = WORKSPACE / "tools"
GEN_TEST = TOOLS_DIR / "gen_comprehensive_test.py"
QEMU_PATH = r"C:\Program Files\qemu\qemu-system-x86_64.exe"
OVMF_PATH = r"C:\Program Files\qemu\share\edk2-x86_64-code.fd"
KERNEL_ELF = WORKSPACE / "target" / "x86_64-unknown-none" / "release" / "indo-kernel"
BOOTLOADER_EFI = WORKSPACE / "target" / "x86_64-unknown-uefi" / "release" / "indo-boot" / "BOOTX64.EFI"
ESP_DIR = WORKSPACE / "build" / "esp"
LOG_DIR = WORKSPACE / "test_logs"

# Expected serial output patterns (regex)
EXPECTED_PATTERNS = [
    # Boot sequence
    (r"\[KERNEL\] INDOMINUS OS", "kernel_boot"),
    (r"\[KERNEL\] Kernel phys:", "kernel_phys_info"),
    (r"\[MARK\] Before process init", "process_init_start"),
    (r"\[MARK\] After process init", "process_init_done"),
    (r"\[KERNEL\] All init done", "all_init_done"),
    (r"\[PROC\] Starting scheduler", "scheduler_start"),
    # Hardware init
    (r"\[ACPI\] RSDP at 0x", "acpi_rsdp_found"),
    (r"\[ACPI\] Found 0x0+6 tables", "acpi_tables_found"),
    (r"\[ACPI\]   APIC", "acpi_madt_found"),
    (r"\[PCI\] Found 0x0+6 devices", "pci_devices_found"),
    (r"\[LAPIC\] Mapped at phys=", "lapic_mapped"),
    (r"\[IOAPIC\] Mapped at phys=", "ioapic_mapped"),
    (r"\[INT\] Interrupt subsystem initialized", "interrupts_init"),
    # Process spawning
    (r"\[SCHED\] Spawned user process 1", "spawn_pid1"),
    (r"\[SCHED\] Spawned user process 2", "spawn_pid2"),
    (r"\[SCHED\] Spawned user process 3", "spawn_pid3"),
    (r"\[SCHED\] Spawned user process 4", "spawn_pid4"),
    (r"\[SCHED\] Spawned user process 5", "spawn_pid5"),
    (r"\[SCHED\] Spawned user process 6", "spawn_pid6"),
    (r"\[SCHED\] Spawned user process 7", "spawn_pid7"),
    # Test results
    (r"TEST1_NORMAL_OK", "test1_pass"),
    (r"TEST2_MULTI_PID_OK", "test2_pass"),
    (r"TEST3_NULL_DEREF_BEFORE", "test3_start"),
    (r"TEST4_INVALID_PTR_BEFORE", "test4_start"),
    (r"TEST5_UNMAPPED_BEFORE", "test5_start"),
    (r"TEST5_UNMAPPED_RESULT_OK", "test5_pass"),
    (r"TEST6_NULL_PTR_BEFORE", "test6_start"),
    (r"TEST6_NULL_PTR_RESULT_OK", "test6_pass"),
    (r"TEST7_INVALID_SYSCALL_BEFORE", "test7_start"),
    (r"TEST7_INVALID_SYSCALL_RESULT_OK", "test7_pass"),
    (r"TEST4_INVALID_PTR_RESULT_OK", "test4_pass"),
    (r"TEST1_RESUMED_OK", "test1_resumed"),
    (r"TEST10_ERRNO_BEFORE", "test10_start"),
    (r"TEST10_ERRNO_RESULT_OK", "test10_pass"),
    # Idle
    (r"\[IDLE\] Idle process running", "idle_running"),
    # Expected faults (user faults that should be caught)
    (r"USER FAULT: killing process", "user_fault_caught"),
]

# Patterns that indicate kernel failure
FAILURE_PATTERNS = [
    r"KERNEL FAULT: halting",
    r"DOUBLE FAULT",
    r"FATAL:",
    r"SYSTEM HALTED",
    r" PANIC",
    r"double fault",
    r"triple fault",
    r"General Protection Fault",
    r"\[LAPIC\] ERROR:",
    r"\[IOAPIC\] ERROR:",
]

@dataclass
class TestResult:
    name: str
    passed: bool
    message: str = ""
    output: str = ""
    duration: float = 0.0

@dataclass
class RegressionResult:
    iteration: int
    passed: bool
    tests: List[TestResult] = field(default_factory=list)
    serial_output: str = ""
    duration: float = 0.0
    qemu_exit_code: int = -1
    failure_reason: str = ""

# ─────────────────────────────────────────────────────────────────────────────
# Build functions
# ─────────────────────────────────────────────────────────────────────────────

def run_cmd(cmd: List[str], cwd: Path = WORKSPACE, timeout: int = 300) -> Tuple[int, str, str]:
    """Run a command and return (returncode, stdout, stderr)."""
    try:
        result = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
            shell=(sys.platform == "win32"),
        )
        return result.returncode, result.stdout, result.stderr
    except subprocess.TimeoutExpired:
        return -1, "", "TIMEOUT"
    except Exception as e:
        return -1, "", str(e)

def build_kernel() -> bool:
    """Build the kernel."""
    print("[BUILD] Building kernel...")
    code, stdout, stderr = run_cmd(
        ["powershell", "-ExecutionPolicy", "Bypass", "-File", "build.ps1", "kernel"],
        timeout=120,
    )
    if code != 0:
        print(f"[BUILD] Kernel build FAILED:\n{stderr}")
        return False
    print("[BUILD] Kernel build OK")
    return True

def build_bootloader() -> bool:
    """Build the bootloader."""
    print("[BUILD] Building bootloader...")
    code, stdout, stderr = run_cmd(
        ["powershell", "-ExecutionPolicy", "Bypass", "-File", "build.ps1", "bootloader"],
        timeout=120,
    )
    if code != 0:
        print(f"[BUILD] Bootloader build FAILED:\n{stderr}")
        return False
    print("[BUILD] Bootloader build OK")
    return True

def generate_test_binaries() -> bool:
    """Generate test ELF binaries."""
    print("[BUILD] Generating test binaries...")
    code, stdout, stderr = run_cmd(
        [sys.executable, str(GEN_TEST)],
        cwd=TOOLS_DIR,
        timeout=60,
    )
    if code != 0:
        print(f"[BUILD] Test generation FAILED:\n{stderr}")
        return False
    print("[BUILD] Test binaries generated")
    return True

def verify_binaries() -> bool:
    """Verify PE/COFF and ELF formats."""
    print("[BUILD] Verifying binary formats...")

    # Check kernel ELF
    if not KERNEL_ELF.exists():
        print(f"[BUILD] Kernel ELF not found: {KERNEL_ELF}")
        return False
    with open(KERNEL_ELF, "rb") as f:
        magic = f.read(4)
    if magic != b"\x7fELF":
        print(f"[BUILD] Kernel ELF magic mismatch: {magic}")
        return False

    # Check bootloader PE/COFF
    if not BOOTLOADER_EFI.exists():
        print(f"[BUILD] Bootloader EFI not found: {BOOTLOADER_EFI}")
        return False
    with open(BOOTLOADER_EFI, "rb") as f:
        magic = f.read(2)
    if magic != b"MZ":
        print(f"[BUILD] Bootloader PE magic mismatch: {magic}")
        return False

    print("[BUILD] Binary formats OK")
    return True

def create_esp() -> bool:
    """Create ESP directory structure."""
    print("[BUILD] Creating ESP...")
    esp_efi = ESP_DIR / "EFI" / "BOOT"
    esp_kernel = ESP_DIR / "EFI" / "INDOMINUS"
    esp_efi.mkdir(parents=True, exist_ok=True)
    esp_kernel.mkdir(parents=True, exist_ok=True)

    import shutil
    shutil.copy2(BOOTLOADER_EFI, esp_efi / "BOOTX64.EFI")
    shutil.copy2(KERNEL_ELF, esp_kernel / "kernel.elf")

    print("[BUILD] ESP created")
    return True

# ─────────────────────────────────────────────────────────────────────────────
# QEMU runner
# ─────────────────────────────────────────────────────────────────────────────

def run_qemu(timeout: int = 30) -> Tuple[int, str]:
    """Launch QEMU and capture serial output.

    QEMU is killed after `timeout` seconds since it runs with -no-shutdown.
    Output is read in real-time using threads to avoid pipe buffer deadlocks.
    """
    import threading

    cmd = [
        QEMU_PATH,
        "-machine", "q35",
        "-cpu", "qemu64",
        "-m", "256M",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_PATH}",
        "-drive", f"format=raw,file=fat:rw:{ESP_DIR}",
        "-serial", "stdio",
        "-no-reboot",
        "-no-shutdown",
    ]

    print(f"[QEMU] Launching (timeout={timeout}s)...")
    try:
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        # Read stdout in a thread to avoid pipe buffer deadlock
        stdout_chunks = []
        def read_stdout():
            while True:
                chunk = proc.stdout.read(4096)
                if not chunk:
                    break
                stdout_chunks.append(chunk)

        reader_thread = threading.Thread(target=read_stdout, daemon=True)
        reader_thread.start()

        # Wait for timeout, then kill QEMU
        try:
            proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass

        # Terminate
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except:
            proc.kill()
            try:
                proc.wait(timeout=5)
            except:
                pass

        # Wait for reader thread to finish
        reader_thread.join(timeout=2)

        stdout = b"".join(stdout_chunks).decode("utf-8", errors="replace")
        return proc.returncode, stdout
    except Exception as e:
        return -1, str(e)

# ─────────────────────────────────────────────────────────────────────────────
# Verification
# ─────────────────────────────────────────────────────────────────────────────

def verify_output(output: str) -> RegressionResult:
    """Verify serial output against expected patterns."""
    result = RegressionResult(iteration=0, passed=True)

    found_patterns = {}
    for pattern, name in EXPECTED_PATTERNS:
        if re.search(pattern, output):
            found_patterns[name] = True
        else:
            found_patterns[name] = False

    # Check for kernel failures
    for pattern in FAILURE_PATTERNS:
        if re.search(pattern, output, re.IGNORECASE):
            result.passed = False
            result.failure_reason = f"Kernel failure pattern found: {pattern}"
            return result

    # Check critical patterns
    critical = [
        "kernel_boot", "kernel_phys_info", "process_init_start",
        "process_init_done", "all_init_done", "scheduler_start",
        "test1_pass", "test2_pass", "test1_resumed",
    ]
    for name in critical:
        if not found_patterns.get(name, False):
            result.passed = False
            result.failure_reason = f"Missing critical pattern: {name}"
            return result

    # Check test results
    test_patterns = [
        "test1_pass", "test2_pass", "test3_start", "test4_start",
        "test5_start", "test5_pass", "test6_start", "test6_pass",
        "test7_start", "test7_pass", "test4_pass", "test1_resumed",
        "test10_start", "test10_pass",
    ]
    for name in test_patterns:
        if found_patterns.get(name, False):
            result.tests.append(TestResult(name=name, passed=True))
        else:
            result.tests.append(TestResult(name=name, passed=False, message="Pattern not found"))

    result.serial_output = output
    return result

# ─────────────────────────────────────────────────────────────────────────────
# Main test runner
# ─────────────────────────────────────────────────────────────────────────────

def run_single_iteration(iteration: int, timeout: int = 30, verbose: bool = False) -> RegressionResult:
    """Run a single test iteration."""
    result = RegressionResult(iteration=iteration, passed=False)
    start_time = time.time()

    # Run QEMU
    exit_code, output = run_qemu(timeout=timeout)
    result.qemu_exit_code = exit_code
    result.serial_output = output
    result.duration = time.time() - start_time

    if verbose:
        print(f"\n[ITERATION {iteration}] Serial output:")
        print(output[:2000] if len(output) > 2000 else output)

    # Verify output
    if exit_code == -1 and "TIMEOUT" in output:
        result.passed = False
        result.failure_reason = "QEMU timeout"
        return result

    verify_result = verify_output(output)
    result.passed = verify_result.passed
    result.tests = verify_result.tests
    result.failure_reason = verify_result.failure_reason

    return result

def save_log(result: RegressionResult, log_dir: Path):
    """Save test log."""
    log_dir.mkdir(parents=True, exist_ok=True)
    log_file = log_dir / f"iteration_{result.iteration:04d}.log"
    with open(log_file, "w") as f:
        f.write(f"Iteration: {result.iteration}\n")
        f.write(f"Passed: {result.passed}\n")
        f.write(f"Duration: {result.duration:.2f}s\n")
        f.write(f"QEMU exit code: {result.qemu_exit_code}\n")
        f.write(f"Failure reason: {result.failure_reason}\n")
        f.write(f"\n--- Serial Output ---\n")
        f.write(result.serial_output)

def main():
    parser = argparse.ArgumentParser(description="Indominus OS Regression Test Suite")
    parser.add_argument("--iterations", type=int, default=1, help="Number of test iterations")
    parser.add_argument("--timeout", type=int, default=30, help="QEMU timeout in seconds")
    parser.add_argument("--verbose", action="store_true", help="Print serial output")
    parser.add_argument("--skip-build", action="store_true", help="Skip build steps")
    parser.add_argument("--log-dir", type=str, default=str(LOG_DIR), help="Log directory")
    args = parser.parse_args()

    log_dir = Path(args.log_dir)

    print("=" * 60)
    print("INDOMINUS OS — Regression Test Suite")
    print(f"Iterations: {args.iterations}")
    print(f"Timeout: {args.timeout}s")
    print("=" * 60)

    # Build phase
    if not args.skip_build:
        if not build_kernel():
            sys.exit(1)
        if not build_bootloader():
            sys.exit(1)
        if not generate_test_binaries():
            sys.exit(1)
        if not verify_binaries():
            sys.exit(1)
        if not create_esp():
            sys.exit(1)

    # Test phase
    results = []
    passed_count = 0
    failed_count = 0

    for i in range(1, args.iterations + 1):
        print(f"\n[ITERATION {i}/{args.iterations}]", end=" ")
        result = run_single_iteration(i, timeout=args.timeout, verbose=args.verbose)
        results.append(result)
        save_log(result, log_dir)

        if result.passed:
            print(f"PASS ({result.duration:.1f}s)")
            passed_count += 1
        else:
            print(f"FAIL ({result.duration:.1f}s) — {result.failure_reason}")
            failed_count += 1

    # Summary
    print("\n" + "=" * 60)
    print("RESULTS SUMMARY")
    print("=" * 60)
    print(f"Total iterations: {args.iterations}")
    print(f"Passed: {passed_count}")
    print(f"Failed: {failed_count}")
    print(f"Pass rate: {passed_count/args.iterations*100:.1f}%")

    if failed_count > 0:
        print("\nFailed iterations:")
        for r in results:
            if not r.passed:
                print(f"  Iteration {r.iteration}: {r.failure_reason}")

    # Save summary
    summary_file = log_dir / "summary.json"
    summary = {
        "iterations": args.iterations,
        "passed": passed_count,
        "failed": failed_count,
        "pass_rate": passed_count / args.iterations * 100 if args.iterations > 0 else 0,
        "results": [
            {
                "iteration": r.iteration,
                "passed": r.passed,
                "duration": r.duration,
                "failure_reason": r.failure_reason,
            }
            for r in results
        ],
    }
    with open(summary_file, "w") as f:
        json.dump(summary, f, indent=2)

    print(f"\nLogs saved to: {log_dir}")
    print(f"Summary: {summary_file}")

    sys.exit(0 if failed_count == 0 else 1)

if __name__ == "__main__":
    main()
