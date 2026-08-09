<#
Wrapper the scheduled tasks call. Runs sync-and-install.ps1 and appends its
output to a log as UTF-8.

Exists because the weekly task previously ran the sync script directly with its
output going nowhere, so a failed rebase left no trace. Writes with an explicit
UTF8 StreamWriter rather than Tee-Object/Out-File, both of which emit UTF-16LE
under Windows PowerShell 5.1 and make the log read with a space between every
character.
#>

$ErrorActionPreference = 'Continue'
$repo = 'C:\Users\Anthony\source\repos\herdr'
$log = "$repo\.local\sync.log"

function Write-Log([string[]]$lines) {
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $writer = New-Object System.IO.StreamWriter($log, $true, $utf8)
    try { foreach ($l in $lines) { $writer.WriteLine($l) } } finally { $writer.Dispose() }
}

# Keep the log from growing without bound.
if ((Test-Path $log) -and (Get-Item $log).Length -gt 1MB) {
    Move-Item $log "$log.old" -Force
}

$stamp = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'
Write-Log @("", "=== $stamp : sync starting ===")

# No -Force: sync-and-install now exits early only when upstream is current AND
# the installed build matches HEAD, so running this speculatively is a genuine
# no-op rather than a pointless rebuild.
$output = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass `
    -File "$repo\.local\sync-and-install.ps1" *>&1 | ForEach-Object { "$_" }
$code = $LASTEXITCODE

Write-Log $output

$verdict = switch ($code) {
    0 { 'OK: up to date or rebuilt and installed' }
    2 { 'CONFLICT: rebase needs a human; rebase was aborted and the tree left clean' }
    default { "FAILED: exit $code" }
}
Write-Log @("--- $verdict ---")

exit $code
