import subprocess
import threading
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

# Wait for shell to start
time.sleep(35)

# Send test_ctxswitch command
proc.stdin.write(b"test_ctxswitch\n")
proc.stdin.flush()

# Wait for tests to complete - give more time
time.sleep(60)

proc.kill()
t.join(timeout=5)

full_output = b"".join(output).decode("utf-8", errors="replace")

# Write ALL output to file
with open("ctxswitch_full_output.txt", "w", encoding="utf-8", errors="replace") as f:
    f.write(full_output)

# Now search for CTXSW, Test, fork, pipe, SIGUSR, exit, PAGE FAULT, panic
print("=== FILTERED OUTPUT (CTXSW/Test/fault/panic/exit) ===")
for line in full_output.split("\n"):
    s = line.strip()
    if s and any(k in s for k in [
        "CTXSW", "ctxswitch", "== Test", "PASS:", "FAIL:", "SKIP:",
        "ALL TESTS", "Results:", "RFLAGS",
        "PAGE FAULT", "panic", "PANIC", "fault", "IRET",
        "Phase 13", "CR3", "CR2", "RSP", "RAX", "RIP",
        "PML4", "PID", "FREE", "ALLOC", "DOUBLE",
        "test_ctxswitch", "pf_class", "page_fault",
        "PDPT", "PD ", "PT ", "rw", "present", "user",
        "rsp=", "rip=", "cr2=",
        "PMM", "0x0286", "FREE_WALK", "COW",
    ]):
        print(s)
