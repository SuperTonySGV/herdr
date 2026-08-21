# Handover — last_active fix, and a pinned-space cwd fix mid-flight

Written 2026-08-20, replacing the 2026-08-08 pinned-spaces handover (still in
git history at `e65e3bac`; its long-lived items are carried forward below).
Read `.local/PINNED-SPACES.md` first if you are new to the patch.

## UPDATE 2026-08-21 — both in-flight items are now resolved

Anthony restarted herdr and reported the spaces bug still present. Cause: the
fix had never been installed. The binary on PATH was `0.8.0-pinned.802193f6`
(`feat/pinned-spaces`), which carries the **last_active** work only; the cwd fix
sat unbuilt on its own local branch. The restart did activate last_active, which
is why something appeared to change.

Closed out today:

- `a_reseeded_pinned_space_keeps_the_directory_its_pane_was_in` **passes**, and
  it runs in **0.08s** — not the ~5 minutes this file predicted. It is a unit
  test in `src/app/input/navigate.rs`; it never spawns a ConPTY and was never in
  the slow reseed class. The estimate above was wrong; do not budget 5 minutes
  for it.
- `31d0b93e` merged to `feat/pinned-spaces` as `d9114dcb`, pushed, built and
  installed via `sync-and-install.ps1 -SkipUpstream -Force`. Install gates all
  passed (`pin`: 1 known baseline failure, 0 new; `context_menu`; `last_active`).
- Installed `d9114dcb`; upstream still `2c042bb2`. Takes effect on the next
  `herdr server stop` + relaunch.

Still open, unchanged: the install-gate hole, the broad test gate, the upstream
rebase decision, the unposted `.local/prd/` Discussion draft, the
`persist::restore` pinned-space limitation, and the missing pin/unpin keybind.
The `multi_thread` experiment still applies to the genuinely slow reseed tests
(`tui_close_last_tab_of_pinned_space_reseeds_instead_of_closing_it`, 276s).

## State

- Fork: `SuperTonySGV/herdr`.
- `feat/pinned-spaces` HEAD `802193f6`, **pushed**. Contains the whole
  `last_active` feature plus today's fix to it.
- `fix/pinned-space-keeps-current-cwd` HEAD `31d0b93e`, **local only, not
  merged, not pushed, not installed**. Branched off `802193f6`.
- Installed on PATH: `0.8.0-pinned.802193f6`.
- **The running server is older than the installed binary.** The install was
  deliberately done mid-session while herdr was running; it takes effect only
  after `herdr server stop` + relaunch. That restart had not happened when this
  was written.
- Local `master` is 90 behind `upstream/master` (`2c042bb2`). No rebase today.

## Finished and installed: last_active counted the wrong thing

`480a853c`, merged as `802193f6`.

The token read `6h` for agents that were actively chatting. It counted from the
agent's last *effective state change*, which is not the last time it did
anything: screen detection matches Claude Code's prompt box at priority 950, and
that box stays on screen while the agent works, so a pane can chat all afternoon
and never leave `idle`. Confirmed live at the time — three of five panes had a
`state_change_seq` far behind their output `revision`, and this session's own
pane read `idle` while running tools.

The fix gives activity its own signal: the detection task bumps a per-pane
counter when the agent changes what is on **screen text** (not raw PTY bytes, so
an identical repaint is not activity; only while an agent holds the pane), and
the app polls it each scheduled-task pass to stamp `last_agent_activity_at`.
`last_agent_active_at()` returns the later of activity and state change, so a
hook reporting a turn boundary with nothing drawn still counts.

Two things to know:

- A pane whose agent owns its full lifecycle through **hooks** never bumps the
  counter — the detector stops reading that screen entirely (`src/pane.rs`, the
  `lifecycle_authority_active` early `continue`). Those keep counting from state
  changes, which is the accurate signal there. Deliberate; documented on the
  field.
- The same commit also arms the panel's repaint deadline in
  `handle_scheduled_tasks_headless`. The original feature wired it into the
  local app loop **only**, so on a server — the path Anthony actually runs — the
  labels repainted only when something else asked for a frame. Second real bug,
  found while fixing the first.

**Not verified live.** It needs the restart above. After restarting, expect every
pane to show a blank time until its agent next does something (the first counter
reading only seeds a baseline; the stamps are in-memory).

## In flight: pinned space loses its directory (`31d0b93e`)

Reported behaviour: click **New** in spaces, get a shell at `C:\Users\Anthony`,
`cd` into a repo, see the space become that repo, close the one tab — and the
space snaps back to `C:\Users\Anthony`. Other pinned spaces are unaffected
because `herdr-spaces.ps1` creates them with `herdr workspace create <path>`, so
their `identity_cwd` was right from birth. Only click-New-then-`cd` has a stale
one.

Cause: `identity_cwd` is the creation directory. The label, branch and Git status
all come from `resolved_identity_cwd_from`, which reads the live pane's cwd and
falls back to `identity_cwd` only when there is no pane. The reseed in
`reseed_pinned_workspace` (`src/app/api/tabs.rs`) spawned the replacement tab
from the stale `identity_cwd`. It now adopts the resolved cwd first — the rule
`persist::snapshot` already follows, which is why a *restart* always preserved
the `cd` while the in-memory path did not.

**Outstanding on this branch:**

1. **The new test never finished.** `cargo test --bins --
   a_reseeded_pinned_space_keeps`, allow ~5 minutes. It was still running at
   session end. Nothing about this fix is confirmed by a green test yet.
2. Not merged into `feat/pinned-spaces`, not pushed, not installed.
3. Consider whether the adopted cwd should be validated before it is taken (a
   pane reporting a deleted or unreachable path would poison the pinned space).
   Not done; no evidence it happens.

## Correction to the old handover's item 1

It claimed the two PTY-spawning reseed tests "pass reliably under the `pin`
filter in well under a second". That is wrong. Measured today on a
**stashed-clean tree**:
`tui_close_last_tab_of_pinned_space_reseeds_instead_of_closing_it` alone took
**276.54s** and passed. The `#[tokio::test(flavor = "multi_thread")]` experiment
the old handover proposed is still untried and still worth trying.

Ruled out today as the cause of the 276s: PowerShell startup (0.30s with
Anthony's profile, 0.17s without) and pane teardown (`shutdown_pane_processes` is
bounded at ~750ms — three 250ms graces). Where the time actually goes is
**unknown**; the search was timeboxed and stopped. Tests that do not spawn a
replacement pane, like `tui_close_last_tab_of_unpinned_space_still_closes_it`,
finish instantly.

## Open items carried forward

### The install gate does not cover what its comment claims

`sync-and-install.ps1` gates on `pin`, `context_menu` and `last_active`. During
today's install the `pin` filter reported **26 passed in 0.15s** — it cannot have
executed the three reseed tests at that speed, and one of them takes 276s alone.
So the gate is not protecting the reseed path. Worse, `Invoke-GatedTests` treats a
timeout as a failure, so if it ever did run them it would block installs at the
10-minute mark. Unexamined; found by reading the output, not by testing.

### There is still no broad test gate on this machine

Unchanged and still unsolved — see the 2026-08-08 handover in git history for the
full account (five deterministic deadlocks, a shifting set of PTY stalls,
lowering parallelism makes it worse). Do not restart it by adding names to a skip
list. `.local/windows-test-baseline.txt` (106 names) is still current; the one
failure seen today, `codex_osc_title_braille_spinner_is_working`, is line 28 of
it.

### Upstream has moved further

`upstream/master` is now `2c042bb2`; the 2026-08-08 handover recorded `1777e9bb`.
`feat/pinned-spaces` is not based on it. Plain `herdr-update` will rebase and
rebuild — a real decision. `-SkipUpstream` installs a local commit without taking
upstream.

### Older loose ends, unchanged

- Upstream Discussion drafted, **unposted**: `.local/prd/`. Anthony's call.
- `persist::restore` still drops a pinned space whose panes all fail to restore,
  before the pinned flag is consulted — see Known limitations in
  `PINNED-SPACES.md`.
- No keybind for pin/unpin; right-click menu and CLI only.

## Suggested order next session

1. **Restart herdr.** Two fixes are installed and neither is running. This is the
   whole point of the last_active work.
2. Finish `fix/pinned-space-keeps-current-cwd`: run the test to completion, then
   merge, push and install. Small, and it is the bug Anthony hit today.
3. The `multi_thread` experiment on the reseed tests. Still the cheapest way to
   make this patch's own tests usable.
4. The install-gate hole above — it is why item 3 matters.
5. Decide on the upstream rebase. It only gets harder.

## Working agreements that bit during this work

- `.local/` is gitignored. **This file is invisible to git** and must be copied
  to the orphan `local-tooling` branch to survive. That copy had NOT been done
  when this was written.
- `.local\env.ps1` must be sourced before any cargo command, or the build fails:
  zig is not on PATH. (`ZIG=...\zig-x86_64-windows-0.15.2\zig.exe` inline works
  from a bash shell.)
- Do not pipe `cargo test` through `tail` — the pipe buffers and a slow test
  looks like a hang with no output at all. Cost a wasted 10-minute timeout today.
- `git commit -m` with a PowerShell here-string breaks on double quotes; use
  `git commit -F` or a bash heredoc.
- Do not use `Get-Content -Raw` to compare files (PowerShell 5.1 reads ANSI); use
  `git hash-object`. `Set-Content -Encoding utf8` adds a BOM.
