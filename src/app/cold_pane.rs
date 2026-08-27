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
    /// land in it as a stray blank line at a fresh prompt.
    pub(crate) fn try_start_cold_shell_on_enter(
        &mut self,
        key: &crossterm::event::KeyEvent,
    ) -> bool {
        if key.code != crossterm::event::KeyCode::Enter
            || !key.modifiers.is_empty()
            || key.kind != crossterm::event::KeyEventKind::Press
        {
            return false;
        }
        self.start_focused_cold_shell()
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

    /// The focused pane, if it is cold and has somewhere to spawn.
    fn focused_cold_pane(&self) -> Option<ColdShellCandidate> {
        let ws_idx = self.state.active?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(ws.active_tab_index())?;
        let pane_id = tab.layout.focused();
        let pane = tab.panes.get(&pane_id)?;
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
        let (rows, cols) = self.cold_pane_size(pane_id)?;
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
    /// This is the geometry the renderer actually used, so a pane with an entry
    /// here is on screen with a real size. No entry means nothing has been
    /// painted yet and there is no honest size to spawn at.
    fn cold_pane_size(&self, pane_id: crate::layout::PaneId) -> Option<(u16, u16)> {
        let info = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)?;
        (info.inner_rect.height > 0 && info.inner_rect.width > 0)
            .then_some((info.inner_rect.height, info.inner_rect.width))
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
    async fn an_unpainted_pane_has_no_size_to_spawn_at() {
        // Before the first frame there is no geometry, so a shell would have to
        // be spawned at a guessed size. Wait for a real one instead.
        let mut app = app_with_cold_spaces(&["front"]);
        app.state.view.pane_infos.clear();

        assert!(
            !app.start_focused_cold_shell(),
            "nothing should spawn without real geometry"
        );
    }

    #[tokio::test]
    async fn only_the_focused_pane_is_startable() {
        // The whole point of the rewrite: being *looked at* must never spawn a
        // shell. Only the focused pane, and only on an explicit start.
        let mut app = app_with_cold_spaces(&["front", "back", "further-back"]);

        // Backgrounded spaces stay cold no matter how many frames go by.
        app.state.active = Some(1);
        app.state.view.pane_infos.clear();
        assert!(
            !app.start_focused_cold_shell(),
            "a space with no painted pane must not be startable"
        );

        let still_cold = app
            .state
            .terminals
            .values()
            .filter(|terminal| terminal.pending_cold_shell)
            .count();
        assert_eq!(still_cold, 3, "every space must still be cold");
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
