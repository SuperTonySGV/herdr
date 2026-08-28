# Plan: Decouple "remember this path" from "pinned space" — places list + New picker

_Round 4 — APPROVED by Codex. Editorial fixes applied._

Repo: `C:\Users\Anthony\source\repos\herdr`. Current branch `t-cold-pane-explicit-start`
(three commits on `feat/lazy-pinned-panes`, itself on `feat/pinned-spaces`, all unmerged).
This work branches off `feat/pinned-spaces` **after** that chain merges.

## Goal

Today `pinned: bool` on a workspace does two unrelated jobs: it means *"remember this
path across restarts"* and it means *"give this space a permanent row in the sidebar."*
The user wants only the first. The lazy-pins + cold-pane work (`d9738b0d`) made a pin
cheap in processes (34 -> 10 procs, 1512 -> 429 MB) but a pin still costs a sidebar row,
and 16 of them crowd the window.

The user will not unpin, because unpinning loses the path. So: introduce a **places list**
— persisted `{label, path}` entries independent of any workspace — and turn the sidebar
**New** button into a **filterable picker** over them. Unpinning becomes non-destructive
and the sidebar shrinks to the spaces actually in use.

## Staging

| Stage | Contents | Ships value |
|---|---|---|
| **S1** | Places store + `place.*` API, unpin/close capture, picker over places + recents. | Yes — this alone unblocks unpinning. |
| **S2** | `scan_roots` background discovery. | Removes the data-entry cost. |
| **S3** | `herdr place` CLI subcommand (thin client over the S1 API). | Scripting convenience. |

The API moved from S3 into S1 because `AGENTS.md:44-48` forbids shipping a shared runtime
fact reachable only through the private TUI socket — this is the same guardrail that
produced the P1 in the previous feature's round-1 review. **S1 is the deliverable.**

## Approach

### 1. Places store (S1)

New module `src/places.rs`, persisted at **`crate::config::config_dir().join("places.json")`**
— *not* `session::data_dir()`, which resolves to `config_dir/sessions/<name>`
(`src/session.rs:157-165`) and is deleted wholesale by `delete_session` (`:299`). Places
are global to the user, not per session.

```rust
struct PlacesFile { version: u32, places: Vec<Place>, recents: Vec<Recent> }
struct Place  { id: PlaceId, label: String, path: PathBuf, source: PlaceSource }
struct Recent { path: PathBuf, label: Option<String>, last_used: SystemTime }
enum PlaceSource { Explicit, PromotedFromPin }
```

- **`places` is uncapped and never evicted.** `recents` is MRU, capped at 20.
- **Unpinning promotes to `places`, not to recents** — capped recents would let later
  activity evict exactly the paths the feature promised to keep.

**Recents recording semantics** (previously contradictory — the field said "opened", the
only hook was on close):

- Touched from a **single "workspace successfully added" hook at the App layer**, not from
  the `workspace.create` response. Three paths create workspaces and only one produces that
  response: `handle_workspace_create`, worktree open/create via
  `create_workspace_with_options` (`src/app/worktrees.rs:399`), and pane-move-to-new-workspace
  (`src/app/api/panes.rs:923`). All three go through the hook.
- Also touched on close/unpin. Field renamed `last_used` to reflect both events.
- **Recents are best-effort; explicit place mutations and unpin promotion are strict.** A
  recents write cannot fail a create — the workspace already exists and a retried create
  would duplicate it — so a failure logs a warning and raises a toast, and the create still
  succeeds. Close behaves the same. Only `place.*` calls and `set_pinned(false)` return
  errors on a failed write.
- A crashed session loses at most the touch for spaces still open; those are recoverable
  from `session.json` restore anyway.

**Write discipline:**

- Every mutation is a **locked read-modify-write** on `places.json.lock`: acquire, re-read,
  apply delta, write, release. The in-memory copy is a cache, never authoritative at write
  time. **The committed RMW result replaces the cache** after every mutation, and the cache
  is **unconditionally re-read under the lock on picker open** — the file is small, and
  mtime+size comparison misses a same-length write inside timestamp resolution — so a
  second live server cannot render a stale list. This machine
  has demonstrably run two servers at once (43 orphan shells, 8/27 handover).
- **Unique temp file** per write (`places.json.<pid>.<nonce>.tmp`), not the fixed
  `.json.tmp` of `src/persist/io.rs:52`.
- **Durable before reporting success.** `sync_all` the temp file before the rename, and
  fsync the parent directory where the platform supports it. Rename is atomic against torn
  reads but not against power loss: without this, an unpin could reach disk while its
  promoted place did not — the exact loss §2 claims to prevent.
- **File permissions:** create the store and lock owner-only — `0o600` on Unix; on Windows
  rely on the user profile's inherited ACL and assert it in a test rather than assuming it.
  The file records repository names and directory structure.
- **Strict loader, and the failure is visible.** An unsupported `version` or a parse
  failure leaves the file untouched and refuses all mutation. It does **not** present an
  empty list — that is indistinguishable from data loss. `place.list` returns a structured
  `store_unavailable` error carrying the reason and the untouched file path, and the picker
  renders a persistent diagnostic row instead of rows. Empty-fallback-then-save is the one
  way this feature could destroy a curated list.
- *(Codex withdrew its Windows-rename objection in round 2 — `std::fs::rename` maps to
  `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`. The fixed temp name was the real defect.)*

### 2. Unpin is transactional (S1)

`handle_workspace_set_pinned` (`src/app/api/workspaces.rs:121`) flips `ws.pinned` directly,
and CLI/API unpin bypasses any TUI-only hook.

**`workspace.set_pinned(false)` persists the promotion first.** If the store is read-only
or the write fails, the handler returns an error and **leaves `pinned` unchanged**. An
unpin that "succeeds" while silently dropping the path is the exact failure the feature
exists to prevent. Pin-on (`true`) is unaffected.

Promotion is **`ensure_place`, not `add`** — idempotent by normalized `PlaceKey`. An entry
that already exists counts as **success**, so a duplicate path, a re-unpin, or two servers
unpinning the same path concurrently all leave the path stored and the unpin allowed.
`place.add`'s `duplicate key` error is for explicit user adds only, and only against an
entry that is **already `Explicit`**: an explicit add over a `PromotedFromPin` entry
upgrades it and succeeds. `duplicate key` must never be the reason an unpin is refused. Precedence on an existing entry: keep the existing `id`, keep
the existing label unless it is empty, and upgrade `source` `PromotedFromPin` -> `Explicit`
but never downgrade.

### 3. Capturing paths on close (S1)

At least four sites drop a workspace: `actions.rs:1709` (`close_selected_workspace`),
`actions.rs:3348`, `api/panes.rs:845`, plus worktree-group closure —
and `close_selected_workspace` removes **every member of a worktree group**
(`actions.rs:1674-1691`). It is a pure `AppState` method with no I/O seam.

A helper taking only indices is not sufficient: linked-worktree closure runs from
`handle_worktree_remove_finished` (`src/app/worktrees.rs:890`) **after** `git worktree
remove` has already deleted the directory, so cwd resolution there fails or resolves to a
path that no longer exists.

Fix: build a **removal plan before any deletion**, carrying for each workspace the resolved
path and an explicit policy — `Capture` or `SkipDeletedWorktree`.

**Resolution order is identity-first**, inverting the round-2 draft: `seed_cwd_from_workspace()`
(`creation.rs:52`, the workspace's `resolved_identity_cwd_from`) is authoritative, and
`focused_pane_cwd_in_workspace()` (`creation.rs:75`) is a documented last resort. A pane
that has `cd`-ed into a subdirectory must not rewrite the space's remembered path: a
workspace opened at `C:/repo` whose focused pane sits in `C:/repo/target` still records
`C:/repo`.

### 4. New button opens the picker (S1)

`begin_tui_workspace_create` (`src/app/creation.rs:99`) is the single production funnel,
reached from the mouse via `MouseAction::NewWorkspace` -> `src/app/input/mod.rs:397` and
from the keybinding at `src/app/input/navigate.rs:191`. The `#[cfg(test)]`
`create_workspace` (`creation.rs:126`) bypasses it and must be updated or tests will
silently exercise the old path.

```
+ New space here                      C:\...\herdr
  herdr                               source\repos\herdr
  compass                             source\repos\compass
  --- recent ---
  brandon                             closed 2h ago
```

**Selection semantics.** `open_new_workspace_dialog` (`modal.rs:384`) stashes
`pending_workspace_create_cwd`; the workspace is created **only on submit** in
`save_rename_modal_via_api` (`modal.rs:1027-1040`), which is why Esc today creates nothing.

- `prompt_new_workspace_name = true` -> selection sets `pending_workspace_create_cwd` to the
  chosen path and opens the existing naming dialog. Create still happens on submit. **Zero
  change to the create path.**
- `prompt_new_workspace_name = false` -> selection calls `runtime_workspace_create` with
  `cwd: Some(path)` (already `Option<String>`, `src/api/schema/workspaces.rs:10`).
- Esc at the picker creates nothing and restores the prior mode.

Row 0 preserves the current reflex: Enter-Enter is byte-identical to today.
Config `[spaces] new_opens_picker` defaults to `true` (picker opens); **setting it to
`false` restores the old immediate-create behaviour.**

### 5. `Mode::SpacePicker` integration surface (S1)

Per `AGENTS.md:44-48`, **place facts are server-owned** (§7 API) and **picker query,
selection index, and scroll offset are TUI/client state**, held outside the shared
workspace/session model.

Every exhaustive `Mode` match that must gain an arm, with the decision:

| Site | Decision |
|---|---|
| `Mode::mouse_motion_changes_view` (`state.rs:804`) | **Add** — rows highlight on hover, like `Navigator`. |
| `Mode::wants_ascii_input` (`state.rs:818`) | **Do not add.** It is an explicit allowlist; the filter is free text, so leaving the IME alone is correct. Documented deliberate divergence from `Navigator`, whose search box is held on ASCII as a known limitation. |
| Key dispatch, `src/app/mod.rs:1847` | Add — TUI path. |
| Headless dispatch, `src/app/input/mod.rs:93` | Add — or the mode is unreachable from the API/test harness. |
| Committed text / paste | Route to the filter buffer, matching the rename modal. |
| Cursor visibility / placement | Show at the filter caret. |
| Mouse hit-test + view geometry | Row click, scroll, dialog rect in the geometry pass. |
| Render (`src/ui/dialogs.rs`) | Dialog + scrollbar; narrow-terminal fallback (<32 cols is reachable in a vertical split — the cold-pane hint work established this). |
| `MobileSwitcherTarget::NewWorkspace` (`mouse.rs:1176`) | **Keep immediate create.** The picker is a desktop affordance. |

Filter reuses `text_matches_query` (`src/app/state.rs:746`; `navigator_matches` at
`actions.rs:877` is a private one-line wrapper).

Per `AGENTS.md:36`, the picker reuses the existing modal structure and close actions rather
than inventing a new screen shape.

### 6. Path identity (S1)

One tested `places::normalize(path) -> PlaceKey`:

- **The `place.*` API requires absolute paths** and rejects anything else. The server is
  long-lived and its startup directory is unrelated to the caller's, so there is no valid
  reference cwd for resolving a relative path server-side. `~`, environment refs, and
  relative paths are expanded **client-side** — by the TUI, and by the S3 CLI.
- **Windows verbatim prefixes handled separately:** `\\?\UNC\server\share` -> `\\server\share`
  (stripping `\\?\` alone yields the invalid `UNC\server\share`); `\\?\C:\x` -> `C:\x`.
  Uppercase the drive letter; compare case-insensitively.
- Resolve junctions/symlinks **only when the target exists**; otherwise fall back to the
  lexically-normalized absolute path. Never fail — a missing path still gets a stable key.
- UNC and mapped-drive aliases of the same share are **not** unified (documented limit).

**Missing paths:** shown greyed with a `missing` tag, still selectable; selecting one
surfaces the create error as a toast and offers "remove this place". Never silently
dropped.

**Worktrees:** linked worktree checkouts have genuinely distinct paths, so `PlaceKey` will
**not** merge them — the round-1 claim that dedupe "absorbs" them was wrong. Distinct
checkouts stay distinct entries; no Git-identity grouping in this plan.

### 7. `place.*` API (S1)

Neutral, non-UI names per the guardrail: `place.list`, `place.add`, `place.remove`,
`place.clear_recents`. Each gets a schema entry alongside the existing `workspace.*` methods
(`src/api/schema.rs:66-84`), a runtime wrapper, defined error responses (`store_unavailable`,
`duplicate key` — explicit adds only, `unknown id`, `path_not_absolute`), and a schema
round-trip test.

**The generated schema artifact must be regenerated**: `docs/next/api/herdr-api.schema.json`
goes stale the moment a `place.*` variant is added, and
`generated_protocol_schema_artifact_is_current` (`src/api/schema/tests.rs:152`) fails on it.
Regenerate with `HERDR_UPDATE_API_SCHEMA=1`, and update the socket-API documentation
alongside it. The TUI is one client of these,
not a privileged one.

### 8. Open + pin (S1)

`WorkspaceCreateParams` has no pin field and does not need one: `workspace.create` then
**`workspace.set_pinned`** (already in the schema, `src/api/schema.rs:76`) is a defined
two-step. Pin failure surfaces as a toast and leaves the created space open — no rollback,
because silently closing a space the user just opened is worse than an unpinned one.

### 9. Promotion from the UI (S1)

Sidebar space right-click (`ContextMenuKind::Workspace` / `GitWorkspace`, built at
`src/app/input/mouse.rs:1059-1064`) gains **"Save as place"**.

### 10. Privacy and control (S1)

- `[spaces] remember_recents` (default **true**) — off means no recents are ever written.
- "Clear recents" in the picker, backed by `place.clear_recents`.
- Owner-only file permissions (§1). Explicit places unaffected by the toggle.

### 11. Scan roots (S2)

`[spaces] scan_roots = [...]`, non-recursive `read_dir`, entries containing `.git`. Fully
asynchronous — a network or spun-down root can block indefinitely:

- Runs off the render thread; results arrive on the existing event channel.
- **Generation-stamped**; a result whose generation is stale (picker closed and reopened,
  roots edited) is discarded.
- Cached with a TTL; the picker renders instantly from cache and refreshes in place.
- Per-root failure is a visible row (`scan failed: <root>`), not swallowed.

## Key decisions & tradeoffs

**D1 — Separate places store, not a third pin state.** The alternative (`parked: bool` on
the workspace, reusing snapshot persistence) is less code but keeps pane-less zombie
workspaces in `session.json` and forces `persist/restore.rs` to handle a state it has no
concept of, immediately after a week spent making cold restore correct. Unchanged through
both rounds.

**D2 — Separate `Mode::SpacePicker`, not `Mode::Navigator` reuse.** `NavigatorRow`
(`state.rs:852`) carries `AgentState`, `is_workspace`, `is_tab`, `expanded`, `depth`, and
every `NavigatorTarget` is keyed by `ws_idx`. Reuse means dummy values in ~8 fields and a
target that is not a live object. Codex charged twice that a new mode is expensive;
answered by enumerating the surface (§5) rather than reversing, because reuse trades a
bounded, now-listed cost for a broken invariant.

**D3 — Row 0 is "New space here".** Protects the existing reflex at the cost of one
keystroke to the first repo.

**D4 — API ships in S1, not S3.** Forced by `AGENTS.md:44-48`. Costs four schema methods up
front; buys not repeating the previous feature's P1.

**D5 — `places.json` under `config_dir`, one file for both lists.** Two sources of truth
for one list is a merge problem; recents are churny machine state that does not belong in
a hand-edited `config.toml`.

**D6 — Locked RMW with cache revalidation on picker open.** Costs a lock file, a re-read
per mutation, and a locked re-read per picker open; buys correctness under the multi-server
condition this machine reaches.

**D7 — Unpin is transactional; pin is not.** Asymmetric on purpose: only the unpin
direction can lose data.

## Risks / open questions

- **R1 — Advisory locking is not uniform across platforms** and does not protect against a
  user editing the file in an editor. The strict loader limits the blast radius.
- **R2 — Windows ACL assertion is the weakest part of §1.** Inherited profile ACLs are the
  norm but not guaranteed; the test may prove less than it appears to.
- **R3 — `just check` hangs on this machine.** See Verification — this is a gate, not a
  risk to accept.
- **R4 — Branch depth.** Building before the existing chain merges makes a fourth unmerged
  level, and `sync-and-install.ps1` force-checks-out `feat/pinned-spaces` and would
  silently revert everything. **Gate: do not start until the handover's step-5 merge lands.**

## Verification

**Automated gate: `just check` must complete.** On Windows that recipe is
`scripts\windows_check.ps1 -Mode check` (`justfile:44`) — *not* the Unix `ci + windows-lint`
recipe at `justfile:40`, which the round-1 plan cited wrongly. The `input` suite has been
hanging on this machine (killed at 10 min, documented in the 8/27 handover) and the picker
lands squarely in the input path. **Diagnose the hang, or get Anthony's explicit approval
for a narrower, documented gate before merging.** A manual TUI pass is evidence, not a
substitute.

- **Unit/characterization:** `normalize` table (verbatim drive, verbatim UNC, ordinary UNC,
  missing UNC, Windows drive case, `~`, relative, junction); strict loader refuses mutation
  on bad version and corrupt JSON; MRU cap evicts recents but never `places`; unpin
  promotes; `set_pinned(false)` leaves `pinned` set when the store write fails; dedupe;
  file-permission assertion.
- **Removal plan:** every removal site routes through it; a deleted worktree is
  `SkipDeletedWorktree`; a closed worktree group captures each surviving member once.
- **Recents:** touched via the shared hook from all three creation paths (API create,
  worktree open, pane-move-to-new-workspace); not touched on a failed create; touched on
  close; `remember_recents = false` writes nothing; a failed recents write warns and toasts
  but does not fail the create.
- **Idempotent promotion:** unpinning a path already in `places` succeeds and does not
  return `duplicate key`; re-unpin is a no-op; `PromotedFromPin` upgrades to `Explicit` on
  an explicit add but never downgrades.
- **Close capture prefers identity:** a workspace opened at `C:/repo` whose focused pane
  sits in `C:/repo/target` records `C:/repo`.
- **API path rules:** `place.add` rejects a relative path with `path_not_absolute`; the TUI
  and CLI expand `~` and relative paths before calling.
- **Corrupt store:** `place.list` returns `store_unavailable` (not an empty list), the file
  is byte-identical afterwards, and the picker shows the diagnostic.
- **Schema artifact:** `generated_protocol_schema_artifact_is_current`
  (`src/api/schema/tests.rs:152`) passes after regenerating with `HERDR_UPDATE_API_SCHEMA=1`.
- **Prompt semantics:** Esc at the picker creates nothing; Esc at the chained naming dialog
  creates nothing; submit creates at the picked cwd, not the inherited one.
- **Concurrency:** two writers interleaved lose no entry; a second process's mutation is
  visible to the first on next picker open.
- **API:** schema round-trip per new method; error responses.
- **Manual TUI proof:** picker from button and keybinding; filter; open a place; Esc at both
  dialogs; narrow vertical split; unpin and confirm the path appears; mobile switcher
  unchanged.
- **Formatting:** `cargo fmt --check` with the CRLF guard from the handover (`git diff
  --stat` before committing — `cargo fmt` rewrote every `.rs` to CRLF once already).
- **Docs:** `docs/next` entry covering the config keys, `places.json` location and format,
  the recents opt-out, and the changed New-button behaviour.

## Out of scope

- The pane-teardown leak (273 `PaneDied for unknown pane` + 40 forced-shutdown warnings,
  source of the 43 orphans). Separate ticket.
- Cold-pane / lazy-pin behaviour. Does not touch `app/cold_pane.rs`.
- Recursive or watched scanning; project-type detection beyond `.git`.
- Git-identity grouping of worktree checkouts.
- Any change to what `pinned` means.
- Syncing places across machines.
