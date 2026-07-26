"""
INDOMINUS OS — Phase 12 Interactive Shell Test
Sends commands to the shell via QEMU serial and verifies output.
"""
import subprocess
import time
import sys
import os

QEMU = r"C:\Program Files\qemu\qemu-system-x86_64.exe"
OVMF = r"C:\Program Files\qemu\share\edk2-x86_64-code.fd"
KERNEL = os.path.join(os.path.dirname(__file__), "..", "build", "esp", "EFI", "INDOMINUS", "kernel.elf")

def run_shell_test(commands, timeout=60):
    """Boot QEMU, send commands, capture output."""
    esp_dir = os.path.join(os.path.dirname(__file__), "..", "build", "esp")
    
    cmd = [
        QEMU, "-machine", "q35", "-cpu", "qemu64", "-m", "256M",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF}",
        "-drive", f"format=raw,file=fat:rw:{esp_dir}",
        "-nographic", "-serial", "mon:stdio",
        "-no-reboot", "-no-shutdown",
    ]
    
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    
    output = b""
    start = time.time()
    
    try:
        # Wait for shell prompt
        while time.time() - start < timeout:
            chunk = proc.stdout.read1(4096) if hasattr(proc.stdout, 'read1') else proc.stdout.read(4096)
            if not chunk:
                break
            output += chunk
            text = output.decode('latin-1')
            
            # Check if shell is ready
            if "$ " in text:
                break
        
        # Send each command
        for cmd_str in commands:
            print(f"  >> {cmd_str}")
            proc.stdin.write((cmd_str + "\n").encode())
            proc.stdin.flush()
            time.sleep(0.5)
        
        # Collect remaining output
        time.sleep(1)
        while time.time() - start < timeout:
            try:
                chunk = proc.stdout.read1(4096) if hasattr(proc.stdout, 'read1') else proc.stdout.read(4096)
                if not chunk:
                    break
                output += chunk
            except:
                break
        
    except Exception as e:
        print(f"  ERROR: {e}")
    finally:
        proc.kill()
    
    return output.decode('latin-1', errors='replace')


def main():
    print("=" * 60)
    print("  PHASE 12: INTERACTIVE SHELL TEST")
    print("=" * 60)
    
    tests = [
        ("help", ["help"], ["Commands:", "echo", "pwd", "cd"]),
        ("echo", ["echo hello world"], ["hello world"]),
        ("pwd_root", ["pwd"], ["/"]),
        ("mkdir_touch", ["mkdir /test_dir", "touch /test_dir/file.txt", "ls /test_dir"], ["file.txt"]),
        ("cd_pwd", ["cd /test_dir", "pwd"], ["/test_dir"]),
        ("cat_write", ["echo test content > /test_file.txt", "cat /test_file.txt"], ["test content"]),
        ("cd_dotdot", ["cd /test_dir", "cd ..", "pwd"], ["/"]),
        ("rm", ["touch /rm_test.txt", "rm /rm_test.txt", "ls /"], []),
        ("pid", ["pid"], ["PID:"]),
        ("pipe_echo", ["echo hello | cat"], ["hello"]),
    ]
    
    passed = 0
    failed = 0
    
    for name, commands, expected in tests:
        print(f"\nTest: {name}")
        output = run_shell_test(commands, timeout=30)
        
        ok = True
        for exp in expected:
            if exp not in output:
                print(f"  FAIL: expected '{exp}' in output")
                ok = False
        
        if ok:
            print(f"  PASS")
            passed += 1
        else:
            print(f"  FAIL")
            failed += 1
            # Print relevant output
            lines = output.split('\n')
            for line in lines[-20:]:
                if line.strip():
                    print(f"    | {line.rstrip()}")
    
    print(f"\n{'=' * 60}")
    print(f"  Results: {passed} passed, {failed} failed out of {passed + failed}")
    print(f"{'=' * 60}")
    
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
