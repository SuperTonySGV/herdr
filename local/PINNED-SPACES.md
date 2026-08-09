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

The script runs the `pin` and `context_menu` filters, then the **broad suite**.
The broad run needs two Windows-specific accommodations, both upstream's state
rather than anything this patch caused:

- **Five tests are skipped by name** because they deadlock and never return.
  `desktop_new_workspace_creates_immediately_by_default` hangs even alone under
  `--test-threads=1`, so this is not thread contention. An unfiltered
  `cargo test` on this machine never finishes.
- **~106 tests fail on a clean checkout**, so the exit code is useless as a
  gate. Instead the failures are diffed against `.local\windows-test-baseline.txt`
  and only names *not* in that list fail the install. A run that hangs past 40
  minutes also fails rather than wedging forever.

All three runs — both filters and the broad one — go through the same
`Invoke-GatedTests` helper, and **no caller checks an exit code**. That is not
tidiness. `cargo test pin` is a *substring* filter, so it also matches
`codex_osc_title_braille_sPINner_is_working`, one of the pre-existing
`detect::manifest` failures. The original exit-code check on that filter meant
`herdr-update` refused to install on a completely clean tree, and had done since
whatever upstream change broke those spinner tests — silently, because nobody
runs the updater expecting it to be the thing at fault. Found 2026-08-08 by
running the installer rather than reasoning about it.

This is what makes the update path an actual gate. Until it existed the only
check was those two filters, which is how the last-tab-close bug (`e65e3bac`)
reached a running herdr: every test covering it lived on API handlers that the
broken TUI path never called.

Regenerate the baseline after an upstream rebase shifts it, and keep the diff
honest — a failure removed from that file is a failure nobody will see again.

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
