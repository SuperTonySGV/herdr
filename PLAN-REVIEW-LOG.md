# Plan Review Log: herdr places list + New picker

Started 2026-08-28 (session start). MAX_ROUNDS=5. PLAN_FILE=PLAN.md.
Codex CLI 0.149.1. Model: CLI default (`~/.codex/config.toml` sets only
`model_reasoning_effort = "high"`, no `model` pin).

## Round 1 — Codex

Material findings:

1. **Unpinning is still destructive.** Recents are capped at 20 and evictable, so the 16 remembered paths can disappear after later workspace activity; this contradicts the plan’s core guarantee ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:37)).  
   **Fix:** Promote an unpinned workspace to an uncapped explicit place, or make pin-derived recents non-evictable.

2. **Persistence scope is wrong or undefined.** `session::data_dir()` is per named session, and deleting a session removes that directory, including the proposed `places.json` ([session.rs](C:/Users/Anthony/source/repos/herdr/src/session.rs:157), [session.rs](C:/Users/Anthony/source/repos/herdr/src/session.rs:299)).  
   **Fix:** Explicitly choose global places under `config_dir()` or document and test session-local semantics.

3. **“No API change” is false.** `cwd` avoids changing `workspace.create`, but `herdr place add|rm|ls` requires new API methods/results if places remain server-owned; direct CLI file writes conflict with “load once” and can be overwritten by stale server state ([schema.rs](C:/Users/Anthony/source/repos/herdr/src/api/schema.rs:45), [cli.rs](C:/Users/Anthony/source/repos/herdr/src/cli.rs:81)).  
   **Fix:** Specify server-owned `place.list/add/remove` methods, CLI routing, runtime wrappers, schema tests, and error responses.

4. **Concurrent writers remain an acknowledged data-loss bug.** A shared global store can be written by multiple named servers, while a direct CLI mutation can race a loaded server; the fixed temp name also allows writers to collide.  
   **Fix:** Perform locked read-modify-write mutations using unique temp files and reload/merge state under the same lock.

5. **The proposed atomic-write claim is not valid on Windows.** The cited helper renames over an existing destination ([persist/io.rs](C:/Users/Anthony/source/repos/herdr/src/persist/io.rs:48)); Windows replacement semantics require special handling, so subsequent saves can fail.  
   **Fix:** Define and test a cross-platform replace primitive rather than copying this rename sequence verbatim.

6. **The naming-prompt sequence is internally contradictory.** The plan creates the workspace first and then says the existing naming prompt “chains afterwards,” but current cancellation semantics depend on deferring creation until the prompt is submitted ([modal.rs](C:/Users/Anthony/source/repos/herdr/src/app/input/modal.rs:384), [modal.rs](C:/Users/Anthony/source/repos/herdr/src/app/input/modal.rs:1027)).  
   **Fix:** On selection, open the naming dialog with the chosen cwd when prompting is enabled; create only on submit.

7. **“Open + pin in one step” has no atomic implementation.** `WorkspaceCreateParams` contains no pin field ([workspaces.rs](C:/Users/Anthony/source/repos/herdr/src/api/schema/workspaces.rs:8)); two independent mutations permit partial success and redundant persistence/events.  
   **Fix:** Add a backward-compatible `pinned: bool` create parameter or define rollback and response parsing for a deliberate two-step operation.

8. **Close recording has no safe mutation seam.** `close_selected_workspace()` is pure `AppState`, can remove an entire worktree group, and removes all members at once ([actions.rs](C:/Users/Anthony/source/repos/herdr/src/app/actions.rs:1668)); workspace removal also occurs through tab, pane, worktree, and API paths.  
   **Fix:** Add an App-layer pre-removal capture helper used by every close path, recording every removed workspace while excluding checkouts being deleted.

9. **The background-scan design is unresolved despite scan roots being declared v1 scope.** A network `read_dir` may block indefinitely, and a late result can populate a closed or reopened picker.  
   **Fix:** Use a background scan with cached results, request generations, stale-result rejection, and visible per-root errors.

10. **Path identity—the basis of deduplication—is undefined.** Missing paths cannot be canonicalized, and Windows drive case, junctions, UNC aliases, relative roots, and `~` expansion are unresolved ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:120)).  
    **Fix:** Specify one tested path-normalization/identity abstraction plus explicit behavior for nonexistent and inaccessible paths.

11. **The new mode is substantially underspecified.** Adding `Mode::SpacePicker` requires monolithic and headless key dispatch, committed-text/paste handling, mouse hit-testing, geometry, rendering, scrolling, and IME policy updates ([app/mod.rs](C:/Users/Anthony/source/repos/herdr/src/app/mod.rs:1847), [input/mod.rs](C:/Users/Anthony/source/repos/herdr/src/app/input/mod.rs:93)); the plan names none of these.  
    **Fix:** Enumerate the full picker state/input/render integration and keep picker presentation separate from server-owned place facts.

12. **Privacy and corrupt-file handling are unresolved data-loss risks.** Default-on recents persist every opened directory, while no opt-out/clear behavior or strict version/corruption policy is defined; loading an empty fallback and later saving could erase curated places.  
    **Fix:** Add recents enable/disable/clear controls and a strict loader that preserves unsupported or corrupt files and refuses destructive mutation.

13. **Source citations are partly inaccurate.** `text_matches_query` is in `state.rs`, while `navigator_matches` is a private trivial wrapper in `actions.rs` ([state.rs](C:/Users/Anthony/source/repos/herdr/src/app/state.rs:746), [actions.rs](C:/Users/Anthony/source/repos/herdr/src/app/actions.rs:877)); the mouse citation only produces `MouseAction`, with the actual funnel in `input/mod.rs` ([mouse.rs](C:/Users/Anthony/source/repos/herdr/src/app/input/mouse.rs:546), [input/mod.rs](C:/Users/Anthony/source/repos/herdr/src/app/input/mod.rs:395)).  
    **Fix:** Correct the citations and explicitly update the test-only deferred creation path that bypasses the production funnel.

14. **There is no verification or documentation plan.** This touches persisted state, configuration, API/CLI, headless input, and TUI presentation, meeting the repository’s broad-refactor threshold, yet no characterization tests, `just check`, manual TUI proof, or `docs/next` updates are specified.  
    **Fix:** Add a test matrix for persistence, races, dedupe, prompt cancellation, close paths, CLI/API, mouse/keyboard/mobile UI, plus `just check`, live TUI validation, and draft docs.

VERDICT: REVISE
### Claude's response — Round 1

Verified each factual claim against source before acting. Accepted 13 of 14.

**Accepted and fixed in the plan:**

- **#1 unpin still destructive** — real, and it broke the plan's own promise. Unpin now
  promotes to the *uncapped* `places` list, not to capped recents.
- **#2 wrong persistence scope** — verified `session.rs:157-165` (`data_dir()` =
  `config_dir/sessions/<name>`) and `delete_session` at `:299`. Moved to `config_dir()`.
- **#3 "no API change" was false** — true for the CLI half. `place.*` methods now
  specified, and the whole CLI/API surface deferred to stage S3.
- **#4 concurrent writers** — accepted; locked read-modify-write plus a unique temp name
  per write. This machine has demonstrably run two live servers at once.
- **#6 naming-prompt sequence inverted** — verified `modal.rs:384` stashes
  `pending_workspace_create_cwd` and `save_rename_modal_via_api` (`modal.rs:1027-1040`)
  performs the create on submit. Draft would have made Esc uncancellable. Corrected to
  set the pending cwd and defer creation.
- **#7 open+pin not atomic** — accepted; `workspace.set_pinned` already exists
  (`api/schema.rs:76`), so it is a defined two-step with a stated failure mode, no
  rollback, no new create param.
- **#8 no safe close seam** — verified four removal sites (`actions.rs:1709`,
  `actions.rs:3348`, `api/panes.rs:845`, worktree group) and that
  `close_selected_workspace` removes a whole worktree group. Added an App-layer
  pre-removal capture helper plus a guard test, excluding checkouts being deleted.
- **#9 scan design unresolved** — accepted. Moved scan roots to stage S2 and specified
  the async design (generation stamps, cache, per-root visible errors).
- **#10 path identity undefined** — accepted; one tested `normalize -> PlaceKey`
  abstraction, never-fail fallback for missing paths, explicit UNC limitation.
- **#11 new mode underspecified** — accepted the charge, kept the decision. Enumerated the
  full integration surface (both dispatch paths, paste/IME, mouse geometry, render,
  mobile, mode predicate). Reuse of `NavigatorRow` still rejected: it trades a bounded
  cost for a broken "every target is live" invariant.
- **#12 privacy / corrupt-file** — accepted; `remember_recents` toggle, clear action, and
  a strict loader that refuses mutation rather than empty-fallback-then-overwrite.
- **#13 citations wrong** — verified. `text_matches_query` is `state.rs:746`;
  `navigator_matches` (`actions.rs:877`) is a private wrapper; the mouse funnel is
  `input/mod.rs:397`, not `mouse.rs:546`. All corrected, plus the `#[cfg(test)]`
  `create_workspace` bypass at `creation.rs:126` is now called out.
- **#14 no verification plan** — accepted; added a Verification section with a test
  matrix, `just check` (`justfile:40`), a required manual TUI proof, and `docs/next`.

**Rejected:**

- **#5 "rename over an existing file is not valid on Windows"** — wrong. Rust's
  `std::fs::rename` maps to `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` and does
  replace; `persist/io.rs` has been saving `session.json` this way on this machine
  throughout. The genuine defect buried in the finding was the *fixed* temp filename
  (`.json.tmp`) colliding between concurrent writers — that is fixed under #4 with a
  per-write unique temp name.

**Also changed, unprompted:** split the work into S1 (store + capture + picker), S2 (scan
roots), S3 (API/CLI). The draft was one lump crossing persistence, config, API, CLI,
headless input and render; S1 alone unblocks the user's actual problem.

## Round 2 — Codex

The Windows rename objection is resolved in the plan’s favor. With the repository’s pinned Rust 1.96.1, `std::fs::rename` uses `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)` on Windows; I withdraw prior finding #5. [Rust implementation](https://github.com/rust-lang/rust/blob/31fca3adb283cc9dfd56b49cdee9a96eb9c96ffd/library/std/src/sys/fs/windows.rs)

1. **Unpin is not transactionally specified.** The existing API handler directly flips `ws.pinned` ([workspaces.rs](C:/Users/Anthony/source/repos/herdr/src/app/api/workspaces.rs:121)); if promotion fails because the store is corrupt/read-only, the promised non-destructive unpin still loses persistence, and CLI/API unpin can bypass any TUI-only hook.  
   Fix — Make `workspace.set_pinned(false)` persist the promotion first and return an error without changing `pinned` if persistence fails.

2. **The proposed removal helper cannot correctly exclude deleted worktrees.** It receives only workspace indices, while linked-worktree closure happens after deletion succeeds ([worktrees.rs](C:/Users/Anthony/source/repos/herdr/src/app/worktrees.rs:890)), when cwd resolution may already fail.  
   Fix — Compute a removal plan before deletion carrying captured paths plus an explicit `Capture`/`SkipDeletedWorktree` policy, then consume that same plan during removal.

3. **Recent-history semantics remain contradictory.** The field is `last_opened` and docs promise every opened directory, but the only specified recording hook is close/unpin; failed opens and crashed sessions are also undefined.  
   Fix — Touch recents only after a successful `workspace.create` response, optionally update them on close, and rename the timestamp to `last_used` if both events count.

4. **The global cache is still incoherent across servers.** Locked RMW prevents lost writes, but a second live server’s picker can indefinitely display its stale in-memory copy.  
   Fix — Reload under the lock on picker open or invalidate by file metadata/version, and replace the cache with the committed RMW result after every mutation.

5. **S1 violates the runtime/client boundary in both directions.** Shared server-owned place facts remain TUI-only until S3, while picker presentation state is added to server-owned `AppState`/`Mode`.  
   Fix — Ship the minimal neutral `place.list/add/remove` API with S1 and explicitly isolate picker query/selection/scroll state in the TUI/client boundary.

6. **Sensitive path history lacks filesystem protection.** “Plain readable JSON” plus default-enabled recents can expose repository names and directory structure through permissive creation modes.  
   Fix — Create the store and lock with user-only permissions on Unix and documented owner-only ACL inheritance/validation on Windows.

7. **Windows verbatim UNC normalization is wrong as written.** Stripping `\\?\` from `\\?\UNC\server\share` produces invalid `UNC\server\share`, not `\\server\share`.  
   Fix — Handle `\\?\UNC\` separately and add verbatim drive, verbatim UNC, ordinary UNC, and missing-UNC table tests.

8. **The worktree dedupe claim is false.** Linked worktrees have distinct checkout paths, so a path-derived `PlaceKey` will not absorb their entries.  
   Fix — State that distinct worktree checkouts remain distinct, or define a separate Git-identity policy and its consequences explicitly.

9. **The cited mode invariant does not exist.** [state.rs](C:/Users/Anthony/source/repos/herdr/src/app/state.rs:803) contains `mouse_motion_changes_view` and `wants_ascii_input`, not an `is_modal` predicate; “focus/paint/input suppression” therefore has no verified single integration point.  
   Fix — Enumerate every actual exhaustive mode match and explicitly decide hover repaint, cursor, paste, committed text, and IME behavior for `SpacePicker`.

10. **Verification misstates the Windows gate and treats a hanging suite too casually.** On Windows, `just check` invokes `scripts/windows_check.ps1`, not the Unix `ci + windows-lint` recipe ([justfile](C:/Users/Anthony/source/repos/herdr/justfile:44)); manual TUI proof cannot replace the required automated gate.  
   Fix — Correct the recipe citation and require `just check` to complete, diagnosing the hang or obtaining explicit approval for a narrower documented gate.

11. **The compatibility flag description is reversed.** With `new_opens_picker = true`, the picker opens; `false` restores immediate creation ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:131)).  
   Fix — Change the sentence to say that setting the flag to `false` restores the old behavior.

VERDICT: REVISE
### Claude's response — Round 2

Codex withdrew round-1 finding #5 (Windows rename) after checking the Rust source — the
rejection stands. All 11 new findings accepted; each was verified against source first.

- **#1 unpin not transactional** — verified `handle_workspace_set_pinned`
  (`api/workspaces.rs:121`) flips `ws.pinned` directly with no persistence hook.
  `set_pinned(false)` now persists the promotion first and returns an error leaving
  `pinned` unchanged on failure. Pin-on stays untouched (D7: asymmetric on purpose).
- **#2 removal helper cannot exclude deleted worktrees** — verified
  `handle_worktree_remove_finished` (`worktrees.rs:890`) runs *after* `git worktree remove`
  has deleted the directory, so an index-only helper resolves a path that no longer exists.
  Replaced with a removal plan built before deletion carrying an explicit
  `Capture` / `SkipDeletedWorktree` policy.
- **#3 recents semantics contradictory** — accepted. Touch on successful `workspace.create`
  response (not on request, so failed opens record nothing) plus on close; field renamed
  `last_opened` -> `last_used`.
- **#4 cache incoherent across servers** — accepted. Committed RMW result replaces the
  cache after every mutation, and the cache is revalidated under the lock on picker open.
- **#5 runtime/client boundary violated both ways** — the strongest finding. Verified
  against `AGENTS.md:44-48`: "Do not add new shared behavior that only works through the
  private TUI client socket." Places are a shared session fact, so the API moved from S3
  into S1 (`place.list/add/remove/clear_recents`, neutral names), and picker
  query/selection/scroll are explicitly TUI-layer state. This is the same guardrail that
  produced the P1 in the cold-pane feature's own round-1 review.
- **#6 file permissions** — accepted; owner-only creation for store and lock, `0o600` on
  Unix, asserted-inherited-ACL on Windows (logged as R2 — the weakest part of the plan).
- **#7 verbatim UNC normalization wrong** — correct, `\?\UNC\server\share` with `\?\`
  stripped yields the invalid `UNC\server\share`. Handled separately, with a test table.
- **#8 worktree dedupe claim false** — correct; linked checkouts have distinct paths and a
  path-derived key cannot merge them. Round-1 R4 was wrong; distinct checkouts now stay
  distinct and Git-identity grouping is out of scope.
- **#9 cited mode invariant does not exist** — verified `state.rs:803-830` has
  `mouse_motion_changes_view` and `wants_ascii_input`, no `is_modal`. Replaced with a table
  of every real exhaustive match and a decision per site. Notably `wants_ascii_input` is an
  explicit allowlist, so `SpacePicker` is deliberately *not* added — free-text filter,
  leave the IME alone.
- **#10 wrong `just check` recipe, TUI proof over-credited** — verified `justfile:44`: on
  Windows `check` runs `scripts\windows_check.ps1 -Mode check`, not the Unix `ci +
  windows-lint` at `:40`. Corrected, and the automated gate is now stated as required, with
  the known `input`-suite hang named as something to diagnose or get explicit approval to
  narrow — not something a manual pass covers.
- **#11 flag description reversed** — trivially correct; fixed.

Nothing rejected this round.

## Round 3 — Codex

The prior eleven findings are resolved, but the revision introduces or exposes several remaining correctness gaps.

1. **Cache revalidation can still miss another server’s write.** Comparing only mtime and size ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:68)) fails for same-length writes within timestamp resolution or editors preserving timestamps.  
   Fix — Unconditionally reread this small file under the lock on picker open, or persist and compare a monotonic revision.

2. **Transactional unpin conflicts with duplicate-key errors.** If the path already exists—or two servers unpin it concurrently—the second promotion may return `duplicate key` and leave the workspace pinned even though its path is safely stored ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:87), [PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:191)).  
   Fix — Define promotion as an idempotent `ensure_place` operation where an existing normalized key counts as success, with explicit label/source/ID precedence.

3. **Failure semantics for incidental recents are undefined.** A recents write after workspace creation cannot turn the response into an error—the workspace already exists and a retry could create a duplicate—but silently ignoring it leaves no observability; close has the same ambiguity.  
   Fix — Make create/close recents best-effort with a warning and toast while keeping explicit place mutations and unpin promotion strict.

4. **The create hook does not cover every workspace creation path.** Worktree open/create calls `create_workspace_with_options` directly ([worktrees.rs](C:/Users/Anthony/source/repos/herdr/src/app/worktrees.rs:399)), while pane movement can construct and push a new workspace itself ([panes.rs](C:/Users/Anthony/source/repos/herdr/src/app/api/panes.rs:923)); neither necessarily produces the specified `workspace.create` response.  
   Fix — Put recent recording behind a common successful-workspace-add hook used by API create, worktrees, and pane-to-new-workspace creation.

5. **Close capture can record the wrong directory.** Preferring the focused pane cwd means opening `C:\repo`, focusing a pane that has `cd`’d into `C:\repo\target`, and closing records `target` rather than the workspace’s remembered identity.  
   Fix — Record the workspace identity cwd as authoritative and use a pane cwd only as an explicitly documented last-resort fallback.

6. **“Transactional” unpin is not crash-durable.** Temp-write plus rename is atomic against torn reads, but without syncing the temp and rename, the session’s unpinned state could reach disk while the promoted place is still lost after power failure.  
   Fix — `sync_all` the temp before rename and sync the parent directory where supported before reporting promotion success.

7. **Relative API paths have no valid reference cwd.** `place.add` runs in the long-lived server, so resolving a relative path against “cwd” ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:171)) can use the daemon’s unrelated startup directory rather than the caller’s.  
   Fix — Require absolute paths at the S1 API boundary and make the S3 CLI expand relative paths, `~`, and environment references client-side.

8. **Corrupt-store read behavior is not user-visible.** A parse failure cannot actually “load read-only”; `place.list` must either fail or present an empty cache, and the latter looks exactly like data loss.  
   Fix — Return a structured unavailable/corrupt error from `place.list` and render a persistent picker diagnostic with the untouched file path.

9. **The API schema artifact is missing from the implementation checklist.** New `place.*` variants make [herdr-api.schema.json](C:/Users/Anthony/source/repos/herdr/docs/next/api/herdr-api.schema.json) stale, which the existing test explicitly rejects ([tests.rs](C:/Users/Anthony/source/repos/herdr/src/api/schema/tests.rs:153)).  
   Fix — Add schema regeneration via the test’s documented command plus socket-API documentation updates to S1 verification.

VERDICT: REVISE
### Claude's response — Round 3

All 9 accepted; nothing rejected. Findings are now refinements rather than structural
defects, which is itself a signal about convergence.

- **#1 mtime+size revalidation is insufficient** — accepted. Unconditional re-read under
  the lock on picker open; the file is small enough that the comparison bought nothing.
- **#2 duplicate-key conflicts with transactional unpin** — the sharpest finding this
  round. Promotion is now `ensure_place`, idempotent by `PlaceKey`: an existing entry is
  success, so a re-unpin or a concurrent unpin cannot refuse the unpin. `duplicate key` is
  reserved for explicit user adds. Label/id/source precedence spelled out.
- **#3 recents failure semantics undefined** — accepted. Recents are best-effort (warn +
  toast, create still succeeds, since a retried create would duplicate a workspace that
  already exists); `place.*` and `set_pinned(false)` stay strict.
- **#4 create hook misses paths** — verified `create_workspace_with_options` called
  directly at `worktrees.rs:399` and a workspace constructed inline at `api/panes.rs:923`.
  Moved recording to a shared App-layer "workspace successfully added" hook covering all
  three, instead of hanging it off the `workspace.create` response.
- **#5 close capture records the wrong directory** — good catch. Inverted the resolution
  order: `seed_cwd_from_workspace` (workspace identity) is authoritative,
  `focused_pane_cwd_in_workspace` is a documented last resort. A pane that has `cd`'d into
  `target/` must not rewrite the space's remembered path.
- **#6 not crash-durable** — accepted. `sync_all` the temp before rename, fsync the parent
  where supported, before reporting promotion success.
- **#7 relative paths have no server-side reference cwd** — accepted, and correct: the
  server is long-lived and its startup directory is unrelated to the caller's. The API now
  requires absolute paths (`path_not_absolute`); expansion is client-side in both the TUI
  and the S3 CLI.
- **#8 corrupt store invisible** — accepted. `place.list` returns a structured
  `store_unavailable` with the reason and untouched path; the picker shows a persistent
  diagnostic rather than an empty list that looks like data loss.
- **#9 schema artifact missing from the checklist** — verified
  `generated_protocol_schema_artifact_is_current` (`src/api/schema/tests.rs:152`) and its
  `HERDR_UPDATE_API_SCHEMA` regeneration path. Added to S1 verification along with the
  socket-API docs.

Verification section extended with cases for #2, #4, #5, #7, #8, #9.

## Round 4 — Codex

The plan is now sound enough to implement. The remaining issues are editorial, not material:

1. **D6 still says picker opening costs only a stat**, contradicting the new unconditional reread ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:290)).  
   Fix — Change “a stat per picker open” to “a locked reread per picker open.”

2. **The Windows path examples contain literal carriage-return/tab corruption** around `C:\repo\target` ([PLAN.md](C:/Users/Anthony/source/repos/herdr/PLAN.md:131)).  
   Fix — Rewrite those examples using clean code spans with intact backslashes.

3. **One revised citation is off:** `focused_pane_cwd_in_workspace` begins at [creation.rs](C:/Users/Anthony/source/repos/herdr/src/app/creation.rs:75), not line 59.  
   Fix — Correct the citation from `creation.rs:59` to `creation.rs:75`.

4. **Explicit-add duplicate wording is slightly ambiguous:** verification establishes that adding over `PromotedFromPin` upgrades it, so `duplicate key` should apply only to an already-`Explicit` entry.  
   Fix — State that exception directly in the API error contract.

VERDICT: APPROVED
### Claude's response — Round 4

Converged. All four editorial fixes applied: D6 wording corrected to "a locked re-read per
picker open", the mangled Windows path examples rewritten with forward slashes,
`creation.rs:59` corrected to `creation.rs:75`, and the `duplicate key` contract narrowed
to already-`Explicit` entries so an explicit add over a `PromotedFromPin` entry upgrades
rather than failing.

**Outcome: APPROVED after 4 rounds.** 34 findings raised, 33 accepted, 1 rejected and
subsequently withdrawn by Codex itself (the Windows `std::fs::rename` claim). No code was
written during the loop.
