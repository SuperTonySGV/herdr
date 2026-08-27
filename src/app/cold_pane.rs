//! Deferred shells for panes nobody has looked at yet.
//!
//! A pinned space must always have a tab (see `Workspace::pinned` and
//! `reseed_pinned_workspace`), but it does not need a *running shell*. Pinning
//! is a bookmark: the sidebar entry, label, cwd and Git status all come from
//! `identity_cwd`, none of which needs a PTY.
//!
//! So a pinned space's pane can be created **cold** — a real pane and terminal
//! with `pending_cold_shell` set and no `TerminalRuntime` behind it. This module
//! spawns the shell the first time such a pane is actually visible, which turns
//! a pinned space from "two processes, forever" into "two processes once you
//! use it".
//!
//! This mirrors `agent_resume`, which already defers spawning for restored agent
//! panes, with one deliberate difference: agent resumes start on a timer, for
//! every pane, because a restored agent should come back on its own. A cold
//! shell starts only when its pane is on screen — nothing to replay, and no
//! reason to pay for a shell in a space the user never opens.

use super::App;

struct ColdShellCandidate {
    pane_id: crate::layout::PaneId,
    terminal_id: crate::terminal::TerminalId,
    cwd: std::path::PathBuf,
    rows: u16,
    cols: u16,
}

impl App {
    /// Enter on a cold pane starts its shell and is swallowed.
    ///
    /// Swallowing matters: the keystroke that starts the shell must not also
    /// land in it as a stray blank line at a fresh prompt. Holding Enter makes
    /// that a real risk rather than a theoretical one -- a held key arrives as
    /// a single press with `repeat_count > 1`, and the lease table would
    /// otherwise see the pane context unchanged, mark the press
    /// `ReprocessRepeats`, and replay the remainder into the runtime this call
    /// just created. Claiming the lease as `SuppressRepeats` is what makes the
    /// swallow honest.
    pub(crate) fn try_start_cold_shell_on_enter(
        &mut self,
        source_id: crate::app::InputSourceId,
        key: &crate::input::TerminalKey,
    ) -> bool {
        if key.code != crossterm::event::KeyCode::Enter
            || !key.modifiers.is_empty()
            || key.kind != crossterm::event::KeyEventKind::Press
        {
            return false;
        }
        if !self.start_focused_cold_shell() {
            return false;
        }
        self.input_leases.insert_consumed(
            crate::app::input::InputLeaseKey::new(source_id, key),
            crate::app::input::ConsumedInputLease::SuppressRepeats,
        );
        true
    }

    /// Start the focused pane's deferred shell.
    ///
    /// Returns true when a shell started, so the caller can swallow the
    /// keystroke and mark the frame dirty.
    pub(crate) fn start_focused_cold_shell(&mut self) -> bool {
        let Some(ColdShellCandidate {
            pane_id,
            terminal_id,
            cwd,
            rows,
            cols,
        }) = self.focused_cold_pane()
        else {
            return false;
        };
        let started = self.start_cold_shell(pane_id, terminal_id, cwd, rows, cols);
        if started {
            self.schedule_session_save();
        }
        started
    }

    /// Start a named pane's deferred shell, wherever it lives.
    ///
    /// Deliberately not focus-scoped: an API client has no focus and no
    /// viewport, so requiring either would leave cold panes reachable only from
    /// the TUI. Returns true when a shell started.
    pub(crate) fn start_cold_shell_for_pane(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> bool {
        let Some(ColdShellCandidate {
            pane_id,
            terminal_id,
            cwd,
            rows,
            cols,
        }) = self.cold_pane(ws_idx, pane_id)
        else {
            return false;
        };
        let started = self.start_cold_shell(pane_id, terminal_id, cwd, rows, cols);
        if started {
            self.schedule_session_save();
        }
        started
    }

    /// The focused pane, if it is cold and has somewhere to spawn.
    fn focused_cold_pane(&self) -> Option<ColdShellCandidate> {
        let ws_idx = self.state.active?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(ws.active_tab_index())?;
        let pane_id = tab.layout.focused();
        self.cold_pane(ws_idx, pane_id)
    }

    /// A pane, if it is cold.
    fn cold_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<ColdShellCandidate> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        if self
            .terminal_runtimes
            .get(&pane.attached_terminal_id)
            .is_some()
        {
            return None;
        }
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        if !terminal.pending_cold_shell {
            return None;
        }
        let (rows, cols) = self.cold_pane_size(pane_id);
        Some(ColdShellCandidate {
            pane_id,
            terminal_id: pane.attached_terminal_id.clone(),
            cwd: terminal.cwd.clone(),
            rows,
            cols,
        })
    }

    /// The size the deferred shell should spawn at.
    ///
    /// Prefer the geometry the renderer actually used. Falling back matters for
    /// the API path, which has no viewport at all: spawning at a conventional
    /// size is right, because the next layout resizes the pane anyway, whereas
    /// refusing to spawn would leave a headless client unable to start a shell
    /// it just asked for.
    fn cold_pane_size(&self, pane_id: crate::layout::PaneId) -> (u16, u16) {
        const FALLBACK: (u16, u16) = (24, 80);

        let painted = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .map(|info| info.inner_rect)
            .filter(|rect| rect.height > 0 && rect.width > 0);
        if let Some(rect) = painted {
            return (rect.height, rect.width);
        }
        let area = self.state.view.terminal_area;
        if area.height > 0 && area.width > 0 {
            return (area.height, area.width);
        }
        FALLBACK
    }

    fn start_cold_shell(
        &mut self,
        pane_id: crate::layout::PaneId,
        terminal_id: crate::terminal::TerminalId,
        cwd: std::path::PathBuf,
        rows: u16,
        cols: u16,
    ) -> bool {
        let host_terminal_theme = self.state.host_terminal_theme;
        let Some(launch_env) = self
            .find_pane(pane_id)
            .and_then(|(ws_idx, _)| self.pane_launch_env(ws_idx, pane_id, Vec::new()))
        else {
            // Never silently: a pane that cannot build a launch env stays blank
            // forever, and a blank pane with no log line is indistinguishable
            // from a pane that is simply waiting.
            tracing::warn!(
                pane = pane_id.raw(),
                terminal = %terminal_id,
                "no launch env for cold pane; deferred shell cannot start"
            );
            return false;
        };

        let runtime = match crate::terminal::TerminalRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            &launch_env,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(err) => {
                // Leave `pending_cold_shell` set so a later frame can retry;
                // the pane stays visible and empty rather than disappearing.
                tracing::warn!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    err = %err,
                    "failed to start deferred shell for cold pane"
                );
                return false;
            }
        };

        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.pending_cold_shell = false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::workspace::Workspace;

    fn app_with_cold_spaces(names: &[&str]) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        app.state.ensure_test_terminals();
        app.state.active = (!app.state.workspaces.is_empty()).then_some(0);
        app.state.selected = 0;
        // Test workspaces come with terminals but no runtimes, which is exactly
        // the shape a cold restore produces.
        for terminal in app.state.terminals.values_mut() {
            terminal.pending_cold_shell = true;
        }
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        paint_focused_pane(&mut app);
        app
    }

    /// Give the focused pane the painted geometry `cold_pane_size` requires.
    fn paint_focused_pane(app: &mut App) {
        let Some(ws_idx) = app.state.active else {
            return;
        };
        let ws = &app.state.workspaces[ws_idx];
        let tab = &ws.tabs[ws.active_tab_index()];
        let pane_id = tab.layout.focused();
        app.state.view.pane_infos = vec![crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        }];
    }

    #[tokio::test]
    async fn a_cold_focused_pane_can_be_started() {
        // This is what puts the "press enter" hint on screen. If it ever
        // reports false for a cold pane the pane looks broken rather than
        // waiting -- which is exactly the bug that motivated the rewrite.
        let mut app = app_with_cold_spaces(&["front"]);

        assert!(
            app.start_focused_cold_shell(),
            "a painted, focused, cold pane must be startable"
        );
    }

    #[tokio::test]
    async fn a_normal_pane_is_not_cold() {
        let mut app = app_with_cold_spaces(&["front"]);
        for terminal in app.state.terminals.values_mut() {
            terminal.pending_cold_shell = false;
        }

        assert!(
            !app.start_focused_cold_shell(),
            "a normal pane is not this module's business"
        );
    }

    #[tokio::test]
    async fn an_unpainted_pane_still_spawns_at_a_fallback_size() {
        // An API client has no viewport, so refusing to spawn without painted
        // geometry would make a cold pane startable only from the TUI. The next
        // layout resizes the pane anyway, so a conventional size is the right
        // answer rather than a refusal.
        let mut app = app_with_cold_spaces(&["front"]);
        app.state.view.pane_infos.clear();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 0, 0);

        assert!(
            app.start_focused_cold_shell(),
            "a cold pane must be startable without a painted viewport"
        );
    }

    #[tokio::test]
    async fn starting_one_space_leaves_the_others_cold() {
        // The whole point of the rewrite: being *looked at* must never spawn a
        // shell. An explicit start reaches exactly one pane, and every other
        // pinned space keeps costing nothing.
        let mut app = app_with_cold_spaces(&["front", "back", "further-back"]);
        assert_eq!(cold_count(&app), 3, "all three start out cold");

        assert!(app.start_focused_cold_shell(), "the focused pane starts");

        assert_eq!(
            cold_count(&app),
            2,
            "the two spaces nobody asked for must still be cold"
        );
    }

    #[tokio::test]
    async fn a_background_space_is_never_the_one_that_starts() {
        // `start_focused_cold_shell` follows the active space. If it ever
        // reached past it, pinning would go back to costing a shell per space.
        let mut app = app_with_cold_spaces(&["front", "back"]);
        app.state.active = Some(1);
        paint_focused_pane(&mut app);

        let front_terminal = root_terminal_id(&app, 0);
        assert!(app.start_focused_cold_shell());

        assert!(
            app.state.terminals[&front_terminal].pending_cold_shell,
            "the space that was not active must be untouched"
        );
    }

    #[tokio::test]
    async fn a_named_pane_starts_for_an_api_client() {
        // The API path: no focus, no viewport, still startable. Without this a
        // headless client could focus a restored pinned space and have no way
        // to get a shell in it.
        let mut app = app_with_cold_spaces(&["front", "back"]);
        app.state.view.pane_infos.clear();
        let back_terminal = root_terminal_id(&app, 1);
        let back_pane = app.state.workspaces[1].tabs[0].root_pane;

        assert!(
            app.start_cold_shell_for_pane(1, back_pane),
            "a named pane must start regardless of focus"
        );
        assert!(
            !app.state.terminals[&back_terminal].pending_cold_shell,
            "and stop being cold"
        );
        assert!(
            !app.start_cold_shell_for_pane(1, back_pane),
            "starting an already-live pane must not spawn a second shell"
        );
    }

    #[tokio::test]
    async fn holding_enter_does_not_leak_repeats_into_the_new_shell() {
        // A held Enter arrives as one press with repeat_count > 1. The pane
        // context is unchanged by starting a shell, so without an explicit
        // SuppressRepeats lease the table would mark the press
        // ReprocessRepeats and replay the remainder straight into the runtime
        // that this keystroke just created.
        use crate::app::input::{InputLeaseKey, RepeatPlan};

        let mut app = app_with_cold_spaces(&["front"]);
        let mut key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        );
        key.repeat_count = 4;

        assert!(
            app.try_start_cold_shell_on_enter(crate::app::LOCAL_INPUT_SOURCE, &key),
            "enter must start the shell"
        );

        // The guarantee that matters is not which enum variant landed in the
        // table, but that the remaining repeats are never replayed anywhere.
        let plan = app.input_leases.plan_repeat(
            InputLeaseKey::new(crate::app::LOCAL_INPUT_SOURCE, &key),
            &key,
            None,
        );
        assert!(
            matches!(plan, RepeatPlan::Ignore),
            "held-enter repeats must not be reprocessed into the new shell"
        );
    }

    #[tokio::test]
    async fn a_non_enter_key_does_not_start_a_shell() {
        let mut app = app_with_cold_spaces(&["front"]);
        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        );

        assert!(!app.try_start_cold_shell_on_enter(crate::app::LOCAL_INPUT_SOURCE, &key));
    }

    fn cold_count(app: &App) -> usize {
        app.state
            .terminals
            .values()
            .filter(|terminal| terminal.pending_cold_shell)
            .count()
    }

    fn root_terminal_id(app: &App, ws_idx: usize) -> crate::terminal::TerminalId {
        let ws = &app.state.workspaces[ws_idx];
        let root = ws.tabs[0].root_pane;
        ws.tabs[0]
            .panes
            .get(&root)
            .map(|pane| pane.attached_terminal_id.clone())
            .expect("root pane must have a terminal")
    }

    #[tokio::test]
    async fn starting_a_shell_clears_the_cold_flag() {
        let mut app = app_with_cold_spaces(&["front"]);
        let ws = &app.state.workspaces[0];
        let tab = &ws.tabs[ws.active_tab_index()];
        let pane_id = tab.layout.focused();
        let terminal_id = tab.panes[&pane_id].attached_terminal_id.clone();

        assert!(app.start_focused_cold_shell(), "the shell should start");

        assert!(
            !app.state.terminals[&terminal_id].pending_cold_shell,
            "the hint must go away once the shell is running"
        );
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_some(),
            "and the pane must now have a real runtime"
        );
        assert!(
            !app.start_focused_cold_shell(),
            "starting twice must not spawn a second shell"
        );
    }
}
