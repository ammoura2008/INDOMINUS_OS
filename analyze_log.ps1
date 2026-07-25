$content = [System.IO.File]::ReadAllLines('C:\Users\USER\Documents\indominux rex operating system\interactive_test.log')
Write-Output "Total lines: $($content.Length)"
for ($i = $content.Length - 1; $i -ge 0; $i--) {
    if ($content[$i] -notmatch '^\[AHCI\] TFES') {
        $lineNum = $i + 1
        Write-Output "Last non-TFES line ($lineNum): $($content[$i])"
        break
    }
}
# Also find first TFES line
for ($i = 0; $i -lt $content.Length; $i++) {
    if ($content[$i] -match '^\[AHCI\] TFES') {
        $lineNum = $i + 1
        Write-Output "First TFES line ($lineNum): $($content[$i])"
        break
    }
}
# Count TFES lines
$tfesCount = 0
foreach ($line in $content) {
    if ($line -match '^\[AHCI\] TFES') { $tfesCount++ }
}
Write-Output "Total TFES lines: $tfesCount"
# Count non-TFES lines after first TFES
$foundFirst = $false
$afterCount = 0
foreach ($line in $content) {
    if ($line -match '^\[AHCI\] TFES') { $foundFirst = $true; continue }
    if ($foundFirst) { $afterCount++ }
}
Write-Output "Non-TFES lines after first TFES: $afterCount"
