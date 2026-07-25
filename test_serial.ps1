Stop-Process -Name 'qemu-system-x86_64' -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

$espPath = "C:\Users\USER\Documents\indominux rex operating system\build\esp"
$ovmfPath = "C:\Program Files\qemu\share\edk2-x86_64-code.fd"

$driveOvmf = "if=pflash,format=raw,readonly=on,file=`"$ovmfPath`""
$driveEsp = "format=raw,file=fat:rw:`"$espPath`""

$argStr = "-machine q35 -cpu qemu64 -m 256M -drive $driveOvmf -drive $driveEsp -serial stdio -no-reboot -no-shutdown"

$p = Start-Process -FilePath 'C:\Program Files\qemu\qemu-system-x86_64.exe' -ArgumentList $argStr -NoNewWindow -PassThru -RedirectStandardOutput "$PSScriptRoot\serial_rx_test.log"

# Wait for boot + shell to appear
Start-Sleep -Seconds 35

# Type 'help' and Enter
$p.StandardInput.Write("help")
$p.StandardInput.Flush()
Start-Sleep -Seconds 1

$p.StandardInput.Write([char]13)
$p.StandardInput.Flush()
Start-Sleep -Seconds 5

try { $p.Kill() } catch {}
Start-Sleep -Seconds 1

if (Test-Path "$PSScriptRoot\serial_rx_test.log") {
    Get-Content "$PSScriptRoot\serial_rx_test.log"
} else {
    Write-Output "No output file"
}
