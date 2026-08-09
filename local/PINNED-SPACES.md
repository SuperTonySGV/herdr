# Pinned spaces (local fork feature)

Branch: `feat/pinned-spaces` in `SuperTonySGV/herdr`, rebased onto `upstream/master`.

Kept in `.local/` (git-ignored) on purpose: it stays out of the upstream diff, so
rebases only ever have to deal with the actual code change.

## What it does

A pinned space does not disappear when it loses its last tab. Instead it is
re-seeded with a fresh tab in the space's own directory, named the way a new tab
is normally named. Unpinned spaces behave exactly as before.

This covers all three ways a space empties out:

- closing the last pane
- closing the last tab
- the last pane's process exiting on its own (agent or shell quits)

Explicitly closing a space (`prefix+shift+d`, the right-click **Close** entry,
`herdr workspace close`) still closes it, pinned or not. Pinning protects against
incidental loss, not against a deliberate close.

From the TUI, an explicit close of a *pinned* space always shows the confirmation
dialog first — titled "Close pinned workspace?" — even when `ui.confirm_close` is
off. The CLI/API `workspace.close` is unchanged and closes without asking; there
is nowhere to prompt.

## Using it

Right-click a space row in the sidebar and choose **Pin** (or **Unpin**). The
entry sits between Rename and Close, on both plain and git-backed spaces.

Or from the CLI:

```
herdr workspace list                 # find the workspace id
herdr workspace pin w1
herdr workspace unpin w1
```

Pinned rows show a 📌 prefix in the sidebar. The flag is saved with the session
snapshot, so pins survive restarts and reattaches.

The API method is `workspace.set_pinned` (`{workspace_id, pinned}`), and
`WorkspaceInfo` now carries a `pinned` field, so `herdr workspace list --json`
and any plugin can read it.

## Why it is implemented the way it is

`Workspace` derefs to its active tab and **panics** when it has none
("workspace must always have at least one active tab"). So a pinned space can
never be observably tab-less, not even for one statement.

The patch therefore adds the replacement tab *before* the removal that would have
emptied the space. Once a second tab exists, every existing close path sees a
normal multi-tab space, removes one tab, and reports no workspace close — so the
downstream logic is completely untouched. That is why the diff is small and why
it should rebase quietly.

If spawning the replacement tab fails, the code deliberately falls back to
closing the space, because the alternative is a panic.

## Known limitations

- **Restore drops spaces whose panes all fail to restore.** `persist::restore`
  discards a workspace when no tab could be rebuilt, before the pinned flag is
  ever consulted. A pinned space whose saved directory no longer exists can still
  disappear across a restart.
- **No keybind yet.** Pinning is reachable from the right-click menu and the
  CLI/API, but not bound to a key. Adding one means touching
  `config/keybinds.rs` plus the navigate-mode action table.
- **The TUI reclassified a last-tab close as a space close.** Fixed on
  `fix/pinned-space-last-tab-close`: `close_active_tab_via_api_requires_confirmation`
  short-circuited to `workspace.close` whenever the space had one tab, so the
  guard in `handle_tab_close` was never reached and pinned spaces died. It now
  falls through to `tab.close` when the space is pinned. The regression tests for
  it live at the *input* layer (`navigate.rs`, `modal.rs`), which is the gap that
  let this ship.
- **The integration test suite (`tests/`) is Unix-only** and does not compile on
  Windows at all — this is upstream's state, not something the patch caused. The
  re-seed behavior itself needs a PTY, so it is verified by running the built
  binary rather than by a unit test on this machine.

## Line endings (do not undo this)

This clone is configured with `core.autocrlf false` and `core.eol lf`, local to
the repo.

Git's global `autocrlf=true` was rewriting checked-out files to CRLF. That
breaks `generated_protocol_schema_artifact_is_current`, which compares the
generated schema (LF) against the file on disk (CRLF) and fails on every
checkout -- meaning after every rebase. Upstream is LF-native and never sees
this.

If that test starts failing with a diff that looks identical apart from `\r`,
the setting has been lost. Restore it with:

```
git config core.autocrlf false
git config core.eol lf
git checkout -- .
```

Note that `git add --renormalize .` is the wrong tool here: with autocrlf off it
stores the working tree's CRLF *into* the index, which is backwards. Re-checking
out from HEAD is the fix.

## Keeping current

Nothing rebuilds unattended. The model is the same as any other app: something
cheap checks for updates and tells you, and you decide when to apply one.

**Checking** is a scheduled task, `herdr-check-for-updates`, running five minutes
after logon and daily at noon. It runs `.local\check-for-updates.ps1`: a `git
fetch` and a couple of SHA comparisons, about a second, no build. When there is
something to apply it posts a herdr notification. Each distinct situation is
announced once, deduplicated in `.local\last-announced-state.txt`.

It compares three things, because all three can drift apart:

| Comparison | Meaning |
| --- | --- |
| `upstream/master` vs `HEAD` | upstream has moved |
| `HEAD` vs installed binary | local commits were never built |
| installed binary vs running server | herdr reports this itself as `restart_needed` |

The middle one exists because a commit that is never built is otherwise
invisible. It happened once, and only surfaced because a session close-out went
looking.

**Applying** is a command, on PATH as `C:\Users\Anthony\tools\herdr-update.cmd`:

```
herdr-update
```

which runs `.local\run-sync-logged.ps1` -> `.local\sync-and-install.ps1`:
fetch, rebase, build, test, install. Logs to `.local\sync.log` as UTF-8,
rotating at 1MB. Exit code 2 means the rebase conflicted and needs a human; the
script aborts the rebase and leaves the tree clean.

`-SkipUpstream` builds and installs HEAD as it stands, with no fetch and no
rebase. Use it to pick up a local commit without also taking whatever upstream
has landed since; rebasing is a separate decision and shouldn't ride along with
an install.

### The test gate

The script runs the `pin` and `context_menu` filters through `Invoke-GatedTests`,
which diffs failures against `.local\windows-test-baseline.txt` and fails only
on names absent from it. **No caller checks an exit code**, and that is load
bearing rather than tidy.

`cargo test pin` is a *substring* filter, so it also matches
`codex_osc_title_braille_sPINner_is_working`, one of ~106 tests that fail on
this machine on a clean checkout. The original exit-code check meant
`herdr-update` refused to install on any tree at all, and had been doing so
unnoticed since whatever upstream change broke those spinner tests — nobody runs
the updater suspecting the updater. Found 2026-08-08 by running it rather than
reasoning about it.

The gate is **narrow on purpose**: it protects the patch, not the binary.

### Why there is no broad test gate (2026-08-08)

An attempt to gate the install on the full suite was written and then removed.
It cannot be made to finish on this machine:

- Five tests deadlock deterministically.
  `desktop_new_workspace_creates_immediately_by_default` hangs alone at
  `--test-threads=1`, so it is not thread contention.
- Beyond those, **a shifting handful of PTY-spawning tests stall on every run**,
  a different set each time — so skipping by name never converges. Three
  successive 40-minute runs each stalled ~3 fresh names.
- Lowering parallelism makes it *worse*, not better: `--test-threads=4` stalled
  at 796/2940 where the default stalled at 2922. So contention is not the
  mechanism, and the cause is unknown.

Useful diagnostic found late: libtest prints
`test <name> has been running for over 60 seconds`, which identifies stuck tests
directly instead of diffing `--list` against completed output.

**Two of the tests added in `e65e3bac` are among the frequent stallers** —
`tui_close_last_tab_of_pinned_space_reseeds_instead_of_closing_it` and
`api_context_menu_close_last_tab_of_pinned_space_keeps_the_space`. Both spawn a
real PTY via `reseed_pinned_workspace`. They pass reliably under the `pin`
filter, in well under a second, which is how the gate still covers them. Untested
hypothesis worth trying first: `#[tokio::test]` defaults to a current-thread
runtime, and the PTY may need background progress that a busy machine starves —
`#[tokio::test(flavor = "multi_thread")]` is the one-line experiment.

A full run *can* be completed by hand when it matters; one on `e65e3bac` reached
2928/2940 with 105 failures, all matching the baseline, zero new. Regenerate the
baseline after an upstream rebase, and keep it honest: a name deleted from that
file is a failure nobody will ever see again.

Safe to run speculatively -- it exits in about a second when upstream is current
*and* the installed build matches HEAD. Both conditions are required, which is
why there is no `-Force`: checking upstream alone would skip an unbuilt local
commit.

Builds are stamped `0.8.0-pinned.<sha>` via `HERDR_BUILD_CHANNEL` and
`HERDR_BUILD_ID`, which is what lets the installed binary say which commit it
came from and makes herdr's own `restart_needed` meaningful.

### Installing over a running herdr

Windows refuses to overwrite a running `.exe` but allows *renaming* one: the
running process keeps its handle to the renamed file while a new file takes the
original path. `Install-Binary` uses that, so the install never has to wait for
herdr to be closed. Retired binaries are left as `herdr.exe.old*` when a live
herdr still holds them, and swept on a later run.

The swap does not affect the herdr already in memory. Windows has no live
handoff -- `platform::capabilities()` sets `live_handoff: cfg!(unix)`, because
the mechanism passes file descriptors between processes -- so new code only runs
after the server restarts:

```
herdr server stop      # then relaunch herdr
```

Spaces restore from the session snapshot and agents resume their conversations.
A full machine reboot is never required; it just happens to restart the server
too.

`herdr update` would replace the patched binary with an official release, so
`update.version_check` and `update.manifest_check` are set to `false` in the
herdr config.
