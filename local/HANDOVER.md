# Handover — pinned spaces, open bug

Written 2026-08-09. Previous session ran long; picking up in a fresh one.

## State

Everything below is committed, pushed and installed unless stated otherwise.

- Fork: `SuperTonySGV/herdr`, branch `feat/pinned-spaces`, HEAD `e3d75270`.
- Tooling backup: branch `local-tooling` (orphan), HEAD `eea666fb`.
- Installed and running: `0.8.0-pinned.e3d75270`. `restart_needed: no`.
- Anthony has pinned ~10 spaces and is using the feature for real.
- Read `.local/PINNED-SPACES.md` first — how the feature works, why, the
  line-ending setting that must not be undone, and the update model.

## The open bug — a pinned space still vanishes

**Symptom.** Anthony closed the last tab in the pinned `Anthony` space; the
whole space disappeared. Same for `compass` (`wQ`) moments later. Both were
pinned.

**Confirmed, not suspected.** Root cause is
`src/app/input/navigate.rs::close_active_tab_via_api_requires_confirmation`
(~line 464):

```rust
if ws.tabs.len() <= 1 {
    if self.state.confirm_implicit_worktree_group_close(ws_idx) { return true; }
    self.close_workspace_idx_via_api(ws_idx);   // <-- short-circuit
    return false;
}
let tab_idx = ...;
self.runtime_tab_close("tui.tab.close", tab_id);
```

When the TUI closes the **last** tab it never issues `tab.close` at all — it
converts the action into `workspace.close`. The pinned guard added in this work
lives in `App::handle_tab_close` (`src/app/api/tabs.rs`), which that path never
reaches. `workspace.close` is an *explicit* close, which deliberately closes a
pinned space, so the space is destroyed.

**Log evidence** (`%APPDATA%\herdr\herdr-server.log`, 2026-08-09T01:16:39):
`workspace.focus wQ` → `tab.focus wQ:t2` → `tab.focus wQ:t2` →
`workspace.close wQ`. No `tab.close`. Compare 2026-08-08T20:32:20, a CLI
`tab.close` on an *unpinned* space, which does show the API request.

**Why the tests did not catch it.** Coverage is on `handle_tab_close`,
`close_pane`, and the `PaneDied` path — all reached via the API. No test drives
the TUI keybinding/menu route, and on Windows the `tests/` integration suite
does not compile at all, so nothing exercises the real input path.

## Suggested fix

In `close_active_tab_via_api_requires_confirmation`, do not short-circuit when
the workspace is pinned — fall through to `runtime_tab_close` and let
`handle_tab_close` do the re-seed it already knows how to do:

```rust
let pinned = self.state.workspaces.get(ws_idx).is_some_and(|ws| ws.pinned);
if !pinned && ws.tabs.len() <= 1 { ...existing short-circuit... }
```

Keep the worktree-group confirmation check ahead of it either way.

**Check the sibling paths before declaring it fixed** — the same short-circuit
shape may exist for:

- closing the last **pane** from the TUI (`navigate.rs`, look for
  `close_pane_would_close_workspace` and any direct `close_workspace_idx_via_api`)
- the tab context menu (`modal.rs` ~1298 calls the same function, so one fix
  covers it — verify)
- the mouse path in `mouse.rs` (tab bar close button / middle-click)

Grep for `close_workspace_idx_via_api` and audit every caller: any call reached
from *closing a tab or pane* rather than from an explicit "close space" action
is the same bug.

**Add a regression test at the input layer**, not the API layer — that is the
gap that let this through. `AppState::test_new()` plus the navigate action
should be enough without a PTY; if a real tab is needed, note that
`reseed_pinned_workspace` needs a runtime and cannot run under `test_new`.

## What is already verified working

Do not re-litigate these; they were exercised against a live server:

- Pinned space survives closing the last tab **via the API** (`tab.close`)
- Pinned space survives closing the last **pane** (`pane.close`)
- Pinned space survives its shell being killed (`PaneDied`)
- Unpinned space still closes normally (control)
- `pinned` persists across a full server restart — 11 spaces came back pinned
- Right-click menu shows Pin/Unpin with the label reflecting state

## Deliberate design decisions (not bugs)

- **Explicit close closes a pinned space.** Pinning guards against *incidental*
  loss. Anthony was asked and is fine with this. The bug above is that a
  last-tab close is being silently reclassified as an explicit close.
- Windows has no live handoff, so a new build only runs after
  `herdr server stop` + relaunch. No reboot needed.
- Nothing rebuilds unattended: `herdr-check-for-updates` notifies,
  `herdr-update` applies.

## Loose ends unrelated to the bug

- Upstream Discussion drafted, **unposted**: `.local/prd/`. Anthony's call.
- ~41MB of `herdr.exe.old*` in `AppData\Local\Programs\Herdr\bin`; swept by the
  next real `herdr-update` install.
- `master` in the fork is stale. Harmless — nothing uses it.

## Working agreements that bit during this work

- `.local/` is gitignored; anything put there is invisible to git and must be
  copied to `local-tooling` to survive.
- Do not use `Get-Content -Raw` to compare files — PowerShell 5.1 reads ANSI and
  produces false diffs on UTF-8. Use `git hash-object`.
- `Set-Content -Encoding utf8` corrupts existing UTF-8 and adds a BOM. Use a
  `UTF8Encoding($false)` StreamWriter.
