import subprocess
import threading
import sys
import time

QEMU = r"C:\Program Files\qemu\qemu-system-x86_64.exe"
OVMF = r"C:\Program Files\qemu\share\edk2-x86_64-code.fd"
ESP = "build/esp"

cmd = [
    QEMU,
    "-machine", "q35",
    "-cpu", "qemu64",
    "-m", "256M",
    "-serial", "stdio",
    "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF}",
    "-drive", f"format=raw,file=fat:rw:{ESP}",
]

proc = subprocess.Popen(
    cmd,
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
    stdin=subprocess.PIPE,
)

output = []
def reader():
    for line in proc.stdout:
        output.append(line)

t = threading.Thread(target=reader, daemon=True)
t.start()

# Wait for boot and test completion
time.sleep(40)

# Send test_ctxswitch command
proc.stdin.write(b"test_ctxswitch\n")
proc.stdin.flush()

# Wait for test to complete
time.sleep(20)

# Send exit
proc.stdin.write(b"exit\n")
proc.stdin.flush()

time.sleep(3)
proc.kill()
t.join(timeout=5)

full_output = b"".join(output).decode("utf-8", errors="replace")

# Find Phase 13 and CTXSW output
for line in full_output.split("\n"):
    if any(k in line for k in ["Phase 13", "CTXSW", "ctxswitch", "Test 1:", "Test 2:", "Test 3:", "Test 4:", "Test 5:", "Test 6:", "Test 7:", "Test 8:", "Test 9:", "PASS", "FAIL", "ALL TESTS"]):
        print(line)
