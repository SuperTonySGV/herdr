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

use ratatui::layout::Rect;

use super::App;

struct ColdShellCandidate {
    pane_id: crate::layout::PaneId,
    terminal_id: crate::terminal::TerminalId,
    cwd: std::path::PathBuf,
    rows: u16,
    cols: u16,
}

impl App {
    pub(crate) fn has_cold_shells(&self) -> bool {
        self.state
            .terminals
            .values()
            .any(|terminal| terminal.pending_cold_shell)
    }

    /// Spawn shells for any cold panes that are currently visible.
    ///
    /// Returns true when at least one shell started, so the caller can mark the
    /// frame dirty.
    pub(crate) fn start_cold_shells(&mut self) -> bool {
        if !self.has_cold_shells() {
            return false;
        }
        let mut changed = false;
        for ColdShellCandidate {
            pane_id,
            terminal_id,
            cwd,
            rows,
            cols,
        } in self.cold_shell_candidates()
        {
            if self.terminal_runtimes.get(&terminal_id).is_some() {
                continue;
            }
            changed |= self.start_cold_shell(pane_id, terminal_id, cwd, rows, cols);
        }
        if changed {
            self.schedule_session_save();
        }
        changed
    }

    /// Cold panes that are on screen right now.
    ///
    /// Only the active workspace's active tab is considered: a cold pane in a
    /// background space is exactly the case this feature exists to avoid
    /// spawning. `view.pane_infos` is the geometry the renderer just used, so a
    /// pane listed there is genuinely visible and has a real size to spawn at.
    fn cold_shell_candidates(&self) -> Vec<ColdShellCandidate> {
        let terminal_area = self.state.view.terminal_area;
        if terminal_area.width == 0 || terminal_area.height == 0 {
            return Vec::new();
        }
        let Some(ws_idx) = self.state.active else {
            return Vec::new();
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return Vec::new();
        };
        let Some(tab) = ws.tabs.get(ws.active_tab_index()) else {
            return Vec::new();
        };

        let mut candidates = Vec::new();
        for info in self.cold_shell_pane_infos(tab, terminal_area) {
            let Some(pane) = tab.panes.get(&info.id) else {
                continue;
            };
            if self
                .terminal_runtimes
                .get(&pane.attached_terminal_id)
                .is_some()
            {
                continue;
            }
            let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id) else {
                continue;
            };
            if !terminal.pending_cold_shell {
                continue;
            }
            // A pane with a queued agent resume is agent_resume's job, not ours;
            // spawning a bare shell here would race it and eat the resume.
            if terminal.pending_agent_resume_plan.is_some() {
                continue;
            }
            if info.inner_rect.height == 0 || info.inner_rect.width == 0 {
                continue;
            }
            candidates.push(ColdShellCandidate {
                pane_id: info.id,
                terminal_id: pane.attached_terminal_id.clone(),
                cwd: terminal.cwd.clone(),
                rows: info.inner_rect.height,
                cols: info.inner_rect.width,
            });
        }
        candidates
    }

    fn cold_shell_pane_infos(
        &self,
        tab: &crate::workspace::Tab,
        terminal_area: Rect,
    ) -> Vec<crate::layout::PaneInfo> {
        // Prefer the geometry the renderer actually used; fall back to deriving
        // it when the view has not been painted yet (headless, or the very first
        // frame after restore).
        if !self.state.view.pane_infos.is_empty() {
            return self.state.view.pane_infos.clone();
        }
        crate::ui::apply_pane_chrome(
            tab.layout.panes(terminal_area),
            self.state.pane_borders,
            self.state.pane_gaps,
            self.state.pane_outer_borders,
        )
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
        // A painted viewport, so candidates have somewhere to be.
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        app
    }

    fn candidate_terminal_ids(app: &App) -> Vec<crate::terminal::TerminalId> {
        app.cold_shell_candidates()
            .into_iter()
            .map(|candidate| candidate.terminal_id)
            .collect()
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
    async fn only_the_visible_space_is_a_candidate() {
        // The entire point: a cold pane in a space you are not looking at must
        // stay cold. If this ever selects background spaces, pinning goes back
        // to costing one shell per space at startup.
        let app = app_with_cold_spaces(&["front", "back", "further-back"]);

        let candidates = candidate_terminal_ids(&app);

        assert_eq!(
            candidates,
            vec![root_terminal_id(&app, 0)],
            "only the active space's pane should be spawned"
        );
    }

    #[tokio::test]
    async fn switching_space_moves_the_candidate() {
        let mut app = app_with_cold_spaces(&["front", "back"]);
        app.state.active = Some(1);

        assert_eq!(
            candidate_terminal_ids(&app),
            vec![root_terminal_id(&app, 1)],
            "the space the user moved to is the one that needs a shell"
        );
    }

    #[tokio::test]
    async fn a_pane_that_is_not_cold_is_left_alone() {
        let mut app = app_with_cold_spaces(&["front"]);
        for terminal in app.state.terminals.values_mut() {
            terminal.pending_cold_shell = false;
        }

        assert!(
            candidate_terminal_ids(&app).is_empty(),
            "a normal pane is not this module's business"
        );
        assert!(!app.has_cold_shells());
    }

    #[tokio::test]
    async fn a_pending_agent_resume_wins_over_a_cold_shell() {
        // Both defer a spawn. If this module also spawned a bare shell for a
        // pane with a queued resume, the two would race and the resume command
        // could land in a shell that is about to be replaced.
        let mut app = app_with_cold_spaces(&["front"]);
        let terminal_id = root_terminal_id(&app, 0);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "claude".into(),
            argv: vec!["claude".into(), "--resume".into()],
            dedupe_key: "cold-pane-test".into(),
        });

        assert!(
            candidate_terminal_ids(&app).is_empty(),
            "agent_resume owns this pane"
        );
    }

    #[tokio::test]
    async fn an_unpainted_viewport_spawns_nothing() {
        // Before the first frame there is no geometry, so a shell would have to
        // be spawned at a guessed size. Wait for a real one instead.
        let mut app = app_with_cold_spaces(&["front"]);
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 0, 0);

        assert!(candidate_terminal_ids(&app).is_empty());
    }
}
