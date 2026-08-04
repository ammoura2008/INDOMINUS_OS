import subprocess
import threading
import time
import sys

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

# Wait for shell to start
time.sleep(35)

# Send test_ctxswitch command
proc.stdin.write(b"test_ctxswitch\n")
proc.stdin.flush()

# Wait for all 9 tests to complete (yield, sleep, read, waitpid, timer, fork, signal, multi, rflags)
# Each test takes a few seconds, total ~30s
time.sleep(35)

proc.kill()
t.join(timeout=5)

full_output = b"".join(output).decode("utf-8", errors="replace")

# Print all CTXSW and Phase 13 lines
for line in full_output.split("\n"):
    stripped = line.strip()
    if stripped and any(k in stripped for k in [
        "Phase 13", "CTXSW", "ctxswitch",
        "== Test", "PASS:", "FAIL:", "SKIP:",
        "ALL TESTS", "Results:",
        "RFLAGS before", "RFLAGS after",
        "FAIL at",
    ]):
        print(stripped)
