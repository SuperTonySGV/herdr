<#
Rebases Anthony's pinned-spaces patch onto current upstream herdr, rebuilds, and
installs the result as the `herdr` on PATH.

This is what replaces `herdr update` for the patched build. Updates still flow in
-- they just arrive by rebasing onto upstream/master and recompiling instead of
downloading a release binary.

Exit codes:
  0  up to date, or rebuilt and installed successfully
  1  hard failure (build broke, install blocked)
  2  rebase conflicted -- needs Anthony; the rebase is aborted, tree left clean
#>

[CmdletBinding()]
param(
    # Rebuild and reinstall even when upstream had no new commits.
    [switch]$Force,

    # Build and install HEAD as it stands: no fetch, no rebase. For picking up a
    # local commit without also taking whatever upstream has landed since --
    # a rebase is a separate decision and does not belong in an install.
    [switch]$SkipUpstream
)

$ErrorActionPreference = 'Stop'
$repo = 'C:\Users\Anthony\source\repos\herdr'
$branch = 'feat/pinned-spaces'
$releaseDir = 'C:\Users\Anthony\.herdr\packages\standalone\releases\pinned-local'
$currentLink = 'C:\Users\Anthony\.herdr\packages\standalone\current'
$binExe = 'C:\Users\Anthony\AppData\Local\Programs\Herdr\bin\herdr.exe'

function Write-Step($msg) { Write-Host "[sync] $msg" }

. "$repo\.local\env.ps1"
Set-Location $repo

# A dirty tree means work in progress; rebasing over it would be destructive.
if (git status --porcelain) {
    Write-Warning 'Working tree is dirty; refusing to rebase. Commit or stash first.'
    exit 1
}

$startingBranch = git rev-parse --abbrev-ref HEAD
if ($startingBranch -ne $branch) {
    git checkout $branch
    if (-not $?) { Write-Warning "Could not check out $branch."; exit 1 }
}

if ($SkipUpstream) {
    Write-Step 'Skipping upstream fetch/rebase (-SkipUpstream); building HEAD as it stands.'
} else {
    Write-Step 'Fetching upstream...'
    git fetch upstream --tags --quiet
    if (-not $?) { Write-Warning 'git fetch failed.'; exit 1 }
}

$before = git rev-parse HEAD
$upstreamHead = git rev-parse upstream/master
$mergeBase = git merge-base HEAD upstream/master

# Nothing to do only when upstream has not moved AND the installed binary was
# built from the current HEAD. Checking upstream alone is not enough: a local
# commit that was never built would be skipped, which is exactly how the restore
# fix once sat committed but uninstalled.
if (($mergeBase -eq $upstreamHead -or $SkipUpstream) -and -not $Force) {
    $headShort = (git rev-parse --short=8 HEAD).Trim()
    $installedSha = $null
    if (Test-Path $binExe) {
        $reported = (& $binExe status client 2>$null |
            Select-String -Pattern '^version:\s*(.+)$').Matches.Groups[1].Value
        if ($reported -match 'pinned\.([0-9a-f]+)') { $installedSha = $Matches[1] }
    }
    if ($installedSha -eq $headShort) {
        Write-Step 'Already based on current upstream/master, and the installed build matches HEAD. Nothing to do.'
        exit 0
    }
    Write-Step "Upstream is current, but the installed build ($installedSha) is not HEAD ($headShort). Rebuilding."
}

if ($mergeBase -ne $upstreamHead -and -not $SkipUpstream) {
    Write-Step "Rebasing $branch onto upstream/master ($($upstreamHead.Substring(0,8)))..."
    git rebase upstream/master
    if (-not $?) {
        Write-Warning 'Rebase conflicted. Aborting so the tree is left usable.'
        git rebase --abort
        exit 2
    }
}

Write-Step 'Building release binary...'
# Stamp the build so it is distinguishable from whatever is currently running.
# build_info::version() renders these as "0.8.0-pinned.<sha>", which makes
# herdr's own `restart_needed` check work: it compares the running server's
# version string against the binary's, and stock builds all say plain "0.8.0".
$env:HERDR_BUILD_CHANNEL = 'pinned'
$env:HERDR_BUILD_ID = (git rev-parse --short=8 HEAD)
cargo build --release
if (-not $?) {
    Write-Warning "Release build FAILED at upstream $($upstreamHead.Substring(0,8))."
    Write-Warning "The patch may need updating against upstream changes. Tree is on $branch at the rebased commit."
    exit 1
}

Write-Step 'Running unit tests for the patched behavior...'
# `pin` covers the flag, persistence and API round-trip; `context_menu` covers
# the Pin/Unpin entry and the close-by-label selections that an upstream menu
# reorder would otherwise break silently.
foreach ($filter in @('pin', 'context_menu')) {
    cargo test --bins $filter
    if (-not $?) {
        Write-Warning "Tests matching '$filter' FAILED after rebase. Not installing."
        exit 1
    }
}

# Then the broad suite. Two Windows-specific problems make this more than a
# plain `cargo test`, and both are upstream's, not the patch's:
#
#  1. A handful of tests that spawn a real PTY deadlock and never return, so an
#     unfiltered run never finishes. They are skipped by name below.
#     `desktop_new_workspace_creates_immediately_by_default` hangs even alone at
#     --test-threads=1, so this is not thread contention that a serial run fixes.
#  2. ~106 tests fail on a clean checkout. Gating on the exit code would block
#     every install forever, so instead the failures are diffed against a
#     recorded baseline and only *new* names are treated as a regression.
#
# This is what makes the update path a real gate. Before it existed the only
# check was the two filters above, which is how a last-tab-close bug in the
# pinned-spaces patch reached a running herdr (fixed in e65e3bac).
$knownHangs = @(
    'desktop_new_workspace_creates_immediately_by_default',
    'new_workspace_key_opens_prefilled_prompt_and_preserves_captured_cwd',
    'new_workspace_prompt_saves_custom_name_atomically',
    'navigate_mode_matches_legacy_uppercase_shifted_letter',
    'navigate_mode_runs_prefix_action_rhs_without_pressing_prefix_again'
)
$baselineFile = "$repo\.local\windows-test-baseline.txt"
$broadLog = Join-Path $env:TEMP 'herdr-broad-tests.txt'
$broadErr = Join-Path $env:TEMP 'herdr-broad-tests.err.txt'

Write-Step "Running the broad suite ($($knownHangs.Count) known-hanging tests skipped; this takes ~15 min)..."
$testArgs = @('test', '--bins', '--')
foreach ($h in $knownHangs) { $testArgs += '--skip'; $testArgs += $h }
$proc = Start-Process -FilePath 'cargo' -ArgumentList $testArgs -NoNewWindow -PassThru `
    -RedirectStandardOutput $broadLog -RedirectStandardError $broadErr
# Generous ceiling: the run takes ~15 min, so 40 means something genuinely hung
# rather than a slow machine. A new hang must not wedge the install forever.
if (-not $proc.WaitForExit(2400000)) {
    try { $proc.Kill() } catch { }
    Write-Warning 'The broad suite did not finish within 40 minutes -- a test is hanging.'
    Write-Warning "Partial output: $broadLog"
    Write-Warning 'Not installing. Find the hang, then add it to $knownHangs if it is pre-existing.'
    exit 1
}

$failed = @(Select-String -Path $broadLog -Pattern '^test (\S+) \.\.\. FAILED' |
    ForEach-Object { $_.Matches.Groups[1].Value } | Sort-Object -Unique)
$baseline = @()
if (Test-Path $baselineFile) {
    $baseline = @(Get-Content $baselineFile | Where-Object { $_.Trim() -and $_ -notmatch '^\s*#' })
} else {
    Write-Warning "No baseline at $baselineFile; treating every failure as new."
}

$regressions = @($failed | Where-Object { $baseline -notcontains $_ })
if ($regressions.Count -gt 0) {
    Write-Warning "$($regressions.Count) test(s) fail that are NOT in the known-failure baseline. Not installing:"
    $regressions | ForEach-Object { Write-Warning "  $_" }
    Write-Warning "Full output: $broadLog"
    exit 1
}

# Not a failure, just drift worth knowing about: the baseline is stale in the
# harmless direction and can be trimmed.
$nowPassing = @($baseline | Where-Object { $failed -notcontains $_ })
if ($nowPassing.Count -gt 0) {
    Write-Step "$($nowPassing.Count) baseline failure(s) now pass; consider trimming $baselineFile"
}
Write-Step "Broad suite clean against baseline ($($failed.Count) known failures, 0 new)."

Write-Step 'Installing patched binary...'

# Windows will not let you overwrite a running .exe, but it will happily let you
# RENAME one: the running process keeps its handle to the renamed file while a
# new file takes the original path. So the install never has to wait for herdr
# to be closed -- the swap always succeeds and the new binary takes effect the
# next time herdr starts.
function Install-Binary([string]$source, [string]$destination) {
    if (Test-Path $destination) {
        $retired = "$destination.old"
        # A previous .old may still be held by a herdr that is running now;
        # give it a unique name rather than failing.
        if (Test-Path $retired) {
            try { Remove-Item $retired -Force -ErrorAction Stop }
            catch { $retired = "$destination.old-" + (Get-Date -Format 'yyyyMMddHHmmss') }
        }
        Move-Item $destination $retired -Force -ErrorAction Stop
        try {
            Copy-Item $source $destination -Force -ErrorAction Stop
        } catch {
            # Put the working binary back rather than leaving a hole on PATH.
            Move-Item $retired $destination -Force
            throw
        }
        # Best effort: fails while an old herdr still runs from it, cleaned up
        # by the next install.
        Remove-Item $retired -Force -ErrorAction SilentlyContinue
    } else {
        Copy-Item $source $destination -Force -ErrorAction Stop
    }
}
# Sweep retired binaries from previous installs. Each is ~21MB and one is left
# behind whenever the install happens while herdr is running; Remove-Item simply
# fails for any still held by a live process, which is the wanted behaviour.
foreach ($dir in @((Split-Path $binExe -Parent), $releaseDir)) {
    if (Test-Path $dir) {
        Get-ChildItem $dir -Filter 'herdr.exe.old*' -ErrorAction SilentlyContinue | ForEach-Object {
            try {
                Remove-Item $_.FullName -Force -ErrorAction Stop
                Write-Step "Removed retired binary $($_.Name)"
            } catch {
                # Still in use by a running herdr; a later run will get it.
            }
        }
    }
}

if (-not (Test-Path $releaseDir)) { New-Item -ItemType Directory -Force $releaseDir | Out-Null }
Install-Binary "$repo\target\release\herdr.exe" "$releaseDir\herdr.exe"

# Carry over the sidecar files the packaged release ships alongside the exe.
$packaged = Get-Item $currentLink -Force
if ($packaged.LinkType -eq 'Junction' -and $packaged.Target) {
    $packagedDir = @($packaged.Target)[0]
    foreach ($extra in @('conpty', 'THIRD-PARTY-NOTICES')) {
        $src = Join-Path $packagedDir $extra
        if ((Test-Path $src) -and -not (Test-Path (Join-Path $releaseDir $extra))) {
            Copy-Item $src (Join-Path $releaseDir $extra) -Recurse -Force
        }
    }
}

# Repoint `current` at the patched build.
#
# Deliberately NOT `Remove-Item -Recurse`: on Windows PowerShell 5.1 that can
# follow the junction and delete the *target* directory's contents, which would
# destroy the packaged release this junction currently points at.
# Directory.Delete removes only the reparse point itself.
if (Test-Path $currentLink) {
    $link = Get-Item $currentLink -Force
    if ($link.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        [System.IO.Directory]::Delete($currentLink, $false)
    } else {
        # A real directory here would be unexpected; refuse rather than guess.
        Write-Warning "$currentLink is a real directory, not a junction. Not touching it."
        exit 1
    }
}
New-Item -ItemType Junction -Path $currentLink -Target $releaseDir | Out-Null

# Replace the binary that is actually on PATH.
Install-Binary "$releaseDir\herdr.exe" $binExe

$after = git rev-parse HEAD
Write-Step "Installed. $($before.Substring(0,8)) -> $($after.Substring(0,8)) (upstream $($upstreamHead.Substring(0,8)))"

# The swap does not touch the herdr already in memory. Windows has no live
# handoff (platform capabilities report live_handoff = false), so the new code
# only runs once the server process restarts.
if (Get-Process -Name herdr -ErrorAction SilentlyContinue) {
    Write-Step 'Installed while herdr was running. The new build takes effect after the next herdr restart:'
    Write-Step '  herdr server stop     (then relaunch herdr; spaces restore from the session snapshot)'

    # Surface it in the app rather than only in this log, which nobody reads.
    # Best effort: a toast is not worth failing an otherwise good install over.
    try {
        & $binExe notification show 'herdr update ready' `
            --body "Rebuilt at $($after.Substring(0,8)). Run 'herdr server stop' and relaunch to apply." `
            --position bottom-right --sound done *> $null
    } catch {
        Write-Step 'Could not post the in-app notification (server may be mid-restart).'
    }
}

exit 0
