# test_interactive.ps1 - End-to-end serial RX test
Stop-Process -Name 'qemu-system-x86_64' -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

$espPath = "C:\Users\USER\Documents\indominux rex operating system\build\esp"
$ovmfPath = "C:\Program Files\qemu\share\edk2-x86_64-code.fd"
$logFile = "$PSScriptRoot\interactive_test.log"

$driveOvmf = "if=pflash,format=raw,readonly=on,file=`"$ovmfPath`""
$driveEsp = "format=raw,file=fat:rw:`"$espPath`""

$argStr = "-machine q35 -cpu qemu64 -m 256M -drive $driveOvmf -drive $driveEsp -serial stdio -no-reboot -no-shutdown"

$p = Start-Process -FilePath 'C:\Program Files\qemu\qemu-system-x86_64.exe' -ArgumentList $argStr -NoNewWindow -PassThru -RedirectStandardOutput $logFile

if (-not $p) {
    Write-Host "FAILED to start QEMU"
    exit 1
}

Write-Host "QEMU started (PID=$($p.Id)). Waiting 50s for boot..."
Start-Sleep -Seconds 50

Write-Host "Sending 'help' + Enter..."
$p.StandardInput.Write("help")
$p.StandardInput.Flush()
Start-Sleep -Seconds 2
$p.StandardInput.Write([char]13)
$p.StandardInput.Flush()
Start-Sleep -Seconds 5

Write-Host "Sending 'notacommand' + Enter..."
$p.StandardInput.Write("notacommand")
$p.StandardInput.Flush()
Start-Sleep -Seconds 2
$p.StandardInput.Write([char]13)
$p.StandardInput.Flush()
Start-Sleep -Seconds 5

try { $p.Kill() } catch {}
Start-Sleep -Seconds 1

if (Test-Path $logFile) {
    Write-Host "`n===== FULL SERIAL OUTPUT ====="
    Get-Content $logFile
    Write-Host "===== END OUTPUT ====="
    
    $content = Get-Content $logFile -Raw
    $hasShell = $content -match "Indominus OS Shell"
    $hasHelp = $content -match "Commands:"
    $hasEcho = $content -match "notacommand"
    $hasUnknown = $content -match "Unknown command"
    
    Write-Host "`n===== VERIFICATION ====="
    Write-Host "Shell banner:    $(if ($hasShell) { 'PASS' } else { 'FAIL' })"
    Write-Host "Help output:     $(if ($hasHelp) { 'PASS' } else { 'FAIL' })"
    Write-Host "Echo received:   $(if ($hasEcho) { 'PASS' } else { 'FAIL' })"
    Write-Host "Unknown command: $(if ($hasUnknown) { 'PASS' } else { 'FAIL' })"
    
    if ($hasShell -and $hasHelp -and $hasEcho -and $hasUnknown) {
        Write-Host "`nALL TESTS PASSED" -ForegroundColor Green
    } else {
        Write-Host "`nSOME TESTS FAILED" -ForegroundColor Red
    }
} else {
    Write-Host "No output file found"
}
