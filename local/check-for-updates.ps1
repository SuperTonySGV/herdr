<#
Cheap update check for the patched herdr build.

Fetches upstream and compares SHAs -- about a second, no build. If there is
something to apply, posts a herdr notification. Nothing is built or installed
here; that is `herdr-update`, run when Anthony decides to.

Checking is cheap enough to do often; rebuilding from source is not. That split
is the whole point of this script.

Three things can drift apart, and all three are checked:

  upstream/master  vs  repo HEAD      -> upstream has moved
  repo HEAD        vs  installed exe  -> local commits were never built
  installed exe    vs  running server -> herdr already reports this itself as
                                         `restart_needed`, so it is left alone

The middle one exists because a commit that is never built is invisible: it
happened once, and only surfaced because a session close-out went looking.

Announcements are deduplicated on the combined state, so each distinct situation
is mentioned once rather than at every check.
#>

$ErrorActionPreference = 'Continue'
$repo = 'C:\Users\Anthony\source\repos\herdr'
$binExe = 'C:\Users\Anthony\AppData\Local\Programs\Herdr\bin\herdr.exe'
$stateFile = "$repo\.local\last-announced-state.txt"

Set-Location $repo
$env:PATH = "$env:USERPROFILE\.cargo\bin;" + [Environment]::GetEnvironmentVariable('PATH', 'Machine')

git fetch upstream --quiet 2>&1 | Out-Null
if (-not $?) { exit 1 }

$upstreamHead = (git rev-parse upstream/master).Trim()
$mergeBase = (git merge-base HEAD upstream/master).Trim()
$headShort = (git rev-parse --short=8 HEAD).Trim()

$reasons = @()

# 1. Has upstream moved past what this branch is built on?
if ($mergeBase -ne $upstreamHead) {
    $behind = (git rev-list --count 'HEAD..upstream/master').Trim()
    $commits = if ($behind -eq '1') { '1 new commit' } else { "$behind new commits" }
    $reasons += "$commits upstream"
}

# 2. Are there local commits that were never built into the installed binary?
#    Builds are stamped 0.8.0-pinned.<sha> by sync-and-install.ps1, so the
#    installed binary can report which commit it came from.
$installedSha = $null
if (Test-Path $binExe) {
    $reported = (& $binExe status client 2>$null | Select-String -Pattern '^version:\s*(.+)$').Matches.Groups[1].Value
    if ($reported -match 'pinned\.([0-9a-f]+)') { $installedSha = $Matches[1] }
}
if ($installedSha -and $installedSha -ne $headShort) {
    $localAhead = (git rev-list --count "$installedSha..HEAD" 2>$null)
    if ($LASTEXITCODE -eq 0 -and $localAhead -and $localAhead -ne '0') {
        $plural = if ($localAhead -eq '1') { 'commit is' } else { 'commits are' }
        $reasons += "$localAhead local $plural built but not installed"
    }
} elseif (-not $installedSha) {
    # An unstamped binary predates the versioned builds; rebuilding stamps it.
    $reasons += 'installed build is unstamped'
}

if ($reasons.Count -eq 0) {
    if (Test-Path $stateFile) { Remove-Item $stateFile -Force -ErrorAction SilentlyContinue }
    exit 0
}

# Announce a given combination once.
$stateKey = "$upstreamHead|$headShort|$installedSha"
$alreadyAnnounced = if (Test-Path $stateFile) { (Get-Content $stateFile -Raw).Trim() } else { '' }
if ($alreadyAnnounced -eq $stateKey) { exit 0 }

# The toast needs a running herdr to land in. If it is closed there is nobody to
# tell, so leave the state file alone and announce on a later check.
if (-not (Get-Process -Name herdr -ErrorAction SilentlyContinue)) { exit 0 }

& $binExe notification show 'herdr update available' `
    --body "$($reasons -join '; '). Run  herdr-update  to rebuild and install." `
    --position bottom-right --sound request *> $null

if ($?) { Set-Content -Path $stateFile -Value $stateKey -Encoding ascii }
exit 0
