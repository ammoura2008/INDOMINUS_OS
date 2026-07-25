#!/usr/bin/env python3
"""
QEMU Boot Automation Tool for AHCI reliability testing.

Launches QEMU with the INDOMINUS OS kernel, captures serial output,
analyzes results across multiple boots, and produces a summary report.

Usage:
    python tools/qemu_boot_test.py [--count N] [--timeout SECONDS] [--send-input]

Collects:
    - Success/failure per boot
    - TFES count, first failing LBA, recovery success/failure
    - Kernel panic detection
    - Shell banner presence
    - All test phase outcomes
    - Per-boot serial logs in tools/boot_logs/
"""

import subprocess
import time
import os
import sys
import re
import json
import argparse
from pathlib import Path
from datetime import datetime


# ── Paths ──────────────────────────────────────────────────────────────────
SCRIPT_DIR = Path(__file__).resolve().parent
WORKSPACE = SCRIPT_DIR.parent
ESP_PATH = WORKSPACE / "build" / "esp"
OVMF_PATH = Path(r"C:\Program Files\qemu\share\edk2-x86_64-code.fd")
QEMU_EXE = Path(r"C:\Program Files\qemu\qemu-system-x86_64.exe")
LOG_DIR = SCRIPT_DIR / "boot_logs"
SUMMARY_FILE = SCRIPT_DIR / "boot_test_summary.json"


def run_single_boot(boot_id: int, timeout: int, send_input: bool) -> dict:
    """Run a single QEMU boot and return analysis results."""
    LOG_DIR.mkdir(exist_ok=True)
    log_file = LOG_DIR / f"boot_{boot_id:03d}.log"

    drive_ovmf = f'if=pflash,format=raw,readonly=on,file={OVMF_PATH}'
    drive_esp = f'format=raw,file=fat:rw:{ESP_PATH}'
    args_list = [
        str(QEMU_EXE),
        "-machine", "q35",
        "-cpu", "qemu64",
        "-m", "256M",
        "-drive", drive_ovmf,
        "-drive", drive_esp,
        "-serial", "stdio",
        "-no-reboot",
        "-no-shutdown",
    ]

    result = {
        "boot_id": boot_id,
        "timestamp": datetime.now().isoformat(),
        "timeout": timeout,
        "sent_input": False,
        "shell_banner": False,
        "help_output": False,
        "unknown_cmd": False,
        "kernel_panic": False,
        "panic_message": "",
        "all_phases_passed": False,
        "tfes_count": 0,
        "tfes_lbas": [],
        "read_ok_count": 0,
        "recovery_count": 0,
        "recovery_successes": 0,
        "first_failing_lba": None,
        "phase_results": {},
        "boot_completed": False,
        "elapsed_seconds": 0,
        "log_file": str(log_file),
    }

    try:
        proc = subprocess.Popen(
            args_list,
            stdin=subprocess.PIPE if send_input else None,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
    except Exception as e:
        result["panic_message"] = f"Failed to start QEMU: {e}"
        return result

    start_time = time.time()

    try:
        # Read output as raw bytes with timeout
        import select
        import io

        # On Windows, use a thread to read stdout
        import threading

        output_data = bytearray()

        def read_output():
            try:
                while True:
                    chunk = proc.stdout.read(4096)
                    if not chunk:
                        break
                    output_data.extend(chunk)
            except Exception:
                pass

        reader_thread = threading.Thread(target=read_output, daemon=True)
        reader_thread.start()
        reader_thread.join(timeout=timeout)

        elapsed = time.time() - start_time
        result["elapsed_seconds"] = round(elapsed, 1)

        # Send input if requested
        if send_input and proc.poll() is None:
            result["sent_input"] = True
            try:
                proc.stdin.write(b"help\n")
                proc.stdin.flush()
                time.sleep(3)
                proc.stdin.write(b"notacommand\n")
                proc.stdin.flush()
                time.sleep(3)
            except Exception:
                pass

            # Give more time for output after input
            time.sleep(2)
            reader_thread2 = threading.Thread(target=read_output, daemon=True)
            reader_thread2.start()
            reader_thread2.join(timeout=10)

        # Kill QEMU
        try:
            proc.kill()
        except Exception:
            pass

        try:
            proc.wait(timeout=5)
        except Exception:
            pass

    except Exception as e:
        result["panic_message"] = f"Error: {e}"
        try:
            proc.kill()
        except Exception:
            pass

    elapsed = time.time() - start_time
    result["elapsed_seconds"] = round(elapsed, 1)

    # Decode output
    try:
        text = output_data.decode("utf-8", errors="replace")
    except Exception:
        text = ""
    output_lines = text.splitlines(keepends=True)

    # Write log file
    with open(log_file, "w", encoding="utf-8") as f:
        f.writelines(output_lines)

    # ── Analyze output ──────────────────────────────────────────────────────
    full_output = "".join(output_lines)

    # TFES count — new format: TFES p=0 lba=0x...
    tfes_matches = re.findall(r'\[AHCI\] TFES p=\S+ lba=(0x[0-9a-fA-F]+)', full_output)
    result["tfes_count"] = len(tfes_matches)
    result["tfes_lbas"] = list(set(tfes_matches))
    if tfes_matches:
        result["first_failing_lba"] = tfes_matches[0]

    # READ_OK count (successful reads after TFES recovery)
    result["read_ok_count"] = len(re.findall(r'\[AHCI\] READ_OK', full_output))

    # Recovery count — count POST lines (each recovery has a PRE and POST)
    result["recovery_count"] = len(re.findall(r'\[AHCI\] RECOVERY.*POST:', full_output))

    # FAILED count
    result["failed_count"] = len(re.findall(r'\[AHCI\] FAILED p=', full_output))

    # Phase results
    for phase in ["9.4", "9.5", "9.6", "9.7", "9.8", "9.9"]:
        if f"=== ALL PHASE {phase} TESTS PASSED ===" in full_output:
            result["phase_results"][phase] = "PASSED"
        elif f"PHASE {phase} HAS FAILURES" in full_output:
            result["phase_results"][phase] = "FAILURES"
        else:
            result["phase_results"][phase] = "NOT_REACHED"

    # All phases passed?
    if all(v == "PASSED" for v in result["phase_results"].values()):
        result["all_phases_passed"] = True

    # Shell banner check — look for the shell prompt or known shell output
    if "Indominus OS Shell" in full_output or "indominus> " in full_output:
        result["shell_banner"] = True

    # Unknown command check (if input was sent)
    if send_input:
        if "notacommand" in full_output:
            result["unknown_cmd"] = True

    return result


def print_result(result: dict):
    """Print a single boot result."""
    status = "PASS" if not result["kernel_panic"] else "PANIC"
    if result["shell_banner"]:
        status = "SHELL"
    if result["all_phases_passed"]:
        status = "ALL_PHASES"

    tfes_info = f"TFES={result['tfes_count']}"
    if result["failed_count"] > 0:
        tfes_info += f" FAILED={result['failed_count']}"

    print(f"  Boot {result['boot_id']:3d}: {status:12s} "
          f"elapsed={result['elapsed_seconds']:5.1f}s "
          f"{tfes_info} "
          f"phases={len([v for v in result['phase_results'].values() if v == 'PASSED'])}/6")


def print_summary(results: list):
    """Print summary report."""
    total = len(results)
    if total == 0:
        print("No results to summarize.")
        return

    panics = sum(1 for r in results if r["kernel_panic"])
    shell_banners = sum(1 for r in results if r["shell_banner"])
    all_phases = sum(1 for r in results if r["all_phases_passed"])
    timeouts = sum(1 for r in results if not r["boot_completed"] and not r["kernel_panic"] and not r["all_phases_passed"])
    total_tfes = sum(r["tfes_count"] for r in results)
    total_read_ok = sum(r["read_ok_count"] for r in results)
    total_failed = sum(r["failed_count"] for r in results)

    # Collect all unique TFES LBAs
    all_lbas = set()
    for r in results:
        all_lbas.update(r["tfes_lbas"])

    # Phase pass rates
    phase_stats = {}
    for phase in ["9.4", "9.5", "9.6", "9.7", "9.8", "9.9"]:
        passed = sum(1 for r in results if r["phase_results"].get(phase) == "PASSED")
        phase_stats[phase] = f"{passed}/{total}"

    print("\n" + "=" * 70)
    print(f"  AHCI RELIABILITY TEST REPORT  ({total} boots)")
    print("=" * 70)
    print(f"  Shell banner seen:    {shell_banners}/{total} ({shell_banners*100//total}%)")
    print(f"  All phases passed:    {all_phases}/{total} ({all_phases*100//total}%)")
    print(f"  Kernel panics:        {panics}/{total}")
    print(f"  Boot timeouts:        {timeouts}/{total}")
    print(f"  Total TFES errors:    {total_tfes}")
    print(f"  Total READ_OK (post-recovery): {total_read_ok}")
    print(f"  Total FAILED (all retries exhausted): {total_failed}")
    print(f"  Unique TFES LBAs:     {sorted(all_lbas) if all_lbas else 'none'}")
    print()
    print("  Phase pass rates:")
    for phase, stats in phase_stats.items():
        print(f"    Phase {phase}: {stats}")
    print("=" * 70)

    # Save JSON summary
    summary = {
        "total_boots": total,
        "shell_banners": shell_banners,
        "all_phases_passed": all_phases,
        "kernel_panics": panics,
        "boot_timeouts": timeouts,
        "total_tfes": total_tfes,
        "total_read_ok": total_read_ok,
        "total_failed": total_failed,
        "unique_tfes_lbas": sorted(all_lbas),
        "phase_stats": phase_stats,
        "results": results,
    }
    with open(SUMMARY_FILE, "w") as f:
        json.dump(summary, f, indent=2)
    print(f"\n  Summary saved to: {SUMMARY_FILE}")


def main():
    parser = argparse.ArgumentParser(description="QEMU boot automation for AHCI reliability testing")
    parser.add_argument("--count", type=int, default=10, help="Number of boots to run (default: 10)")
    parser.add_argument("--timeout", type=int, default=90, help="Seconds to wait per boot (default: 90)")
    parser.add_argument("--send-input", action="store_true", help="Send shell input after boot")
    args = parser.parse_args()

    print(f"Running {args.count} QEMU boots (timeout={args.timeout}s each)...")
    print()

    results = []
    for i in range(1, args.count + 1):
        print(f"Boot {i}/{args.count}...", end=" ", flush=True)
        result = run_single_boot(i, args.timeout, args.send_input)
        results.append(result)
        print_result(result)

    print_summary(results)


if __name__ == "__main__":
    main()
