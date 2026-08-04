import subprocess, time, sys, threading

qemu = r'C:\Program Files\qemu\qemu-system-x86_64.exe'
ovmf = r'C:\Program Files\qemu\share\edk2-x86_64-code.fd'
esp = 'build/esp'

cmd = [qemu, '-machine', 'q35', '-cpu', 'qemu64', '-m', '256M',
       '-drive', f'if=pflash,format=raw,readonly=on,file={ovmf}',
       '-drive', f'fat=rw,x-latency-forced=1000ms,format=raw,file={esp}',
       '-serial', 'stdio', '-nographic', '-no-reboot']

proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

output = []
def reader():
    try:
        for line in proc.stdout:
            output.append(line.decode('utf-8', errors='replace'))
    except:
        pass
threading.Thread(target=reader, daemon=True).start()

time.sleep(50)
proc.kill()

# Print all lines (for debugging)
for i, line in enumerate(output):
    print(f"{i}: {line.rstrip()}")

print(f"\n=== Total lines: {len(output)} ===")
