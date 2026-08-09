# local-tooling

Backup of the machine-local tooling that builds, installs and monitors Anthony's
patched herdr (pinned spaces).

This branch is **orphaned on purpose** — it shares no history with
`feat/pinned-spaces` and contains no herdr source. It exists only so this
tooling is not stored in exactly one unbacked place.

## Why it needs its own branch

The tooling normally lives in `.local/` inside the herdr clone, which upstream's
`.gitignore` excludes (`.gitignore:18`). That is the right call for the working
clone — it keeps the patch branch free of machine-specific files, so rebasing
onto upstream stays clean. The side effect is that the least reproducible work
was also the least protected: the herdr patch itself is safe on
`feat/pinned-spaces`, but everything that builds and installs it was not stored
anywhere else.

## Restoring after a fresh clone

```
git clone https://github.com/SuperTonySGV/herdr.git
cd herdr
git remote rename origin upstream        # canonical upstream
git remote add origin https://github.com/SuperTonySGV/herdr.git
git fetch origin
git checkout feat/pinned-spaces

git config core.autocrlf false           # REQUIRED - see PINNED-SPACES.md
git config core.eol lf
git checkout -- .
```

Then copy `local/` back to `.local/` in the clone, and `bin/herdr-update.cmd` to
a directory on PATH (`C:\Users\Anthony\tools`).

Recreate the update check with a logon (5 minute delay) and daily noon trigger:

```powershell
$name = 'herdr-check-for-updates'
$script = 'C:\Users\Anthony\source\repos\herdr\.local\check-for-updates.ps1'
$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$script`""
$atLogon = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
$atLogon.Delay = 'PT5M'
$daily = New-ScheduledTaskTrigger -Daily -At 12:00pm
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 10)
Register-ScheduledTask -TaskName $name -Action $action -Trigger @($atLogon, $daily) `
    -Settings $settings -Description 'Check whether a herdr update is available and notify in-app.'
```

Build prerequisites: Rust 1.96.1 (rustup honours `rust-toolchain.toml`), Zig
**0.15.2** exactly, MSVC C++ build tools, Windows SDK. Paths are set in
`local/env.ps1`.

## Contents

| Path | What it is |
| --- | --- |
| `local/sync-and-install.ps1` | Rebase onto upstream, build, test, install. Rename-swap so it works with herdr running. |
| `local/check-for-updates.ps1` | Cheap SHA check; notifies in-app when an update exists. Builds nothing. |
| `local/run-sync-logged.ps1` | Wrapper adding UTF-8 logging. What `herdr-update` calls. |
| `local/env.ps1` | Build environment (PATH, `ZIG`). Dot-source before cargo. |
| `local/PINNED-SPACES.md` | How the feature works, why, and its known limits. **Read the line-endings section.** |
| `local/prd/` | Draft of the upstream Discussion, unposted. |
| `bin/herdr-update.cmd` | The "yes, update" command. |

Not backed up: `.local/sync.log` (rolling output) and
`.local/last-announced-state.txt` (notification dedupe), both regenerated.
