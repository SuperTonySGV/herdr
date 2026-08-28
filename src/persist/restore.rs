use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};
use tracing::{error, warn};

use crate::detect::AgentState;
use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::{PaneLaunchEnv, PaneState};
use crate::render_signal::RenderSignal;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalState};
use crate::workspace::Workspace;

use super::snapshot::{
    PaneAgentSessionSnapshot, PaneHistorySnapshot, TabHistorySnapshot, WorkspaceHistorySnapshot,
};
use super::{
    DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot, SessionSnapshot, TabSnapshot,
    WorkspaceSnapshot,
};

struct AgentRestoreState<'a> {
    enabled: bool,
    resumed_sessions: &'a mut HashSet<String>,
}

struct PaneRestoreStartup<'a> {
    restore_plan: Option<crate::agent_resume::AgentResumePlan>,
    initial_history_ansi: Option<&'a str>,
    duplicate_agent_session: bool,
    reserved_agent_session: Option<String>,
}

struct RestoreRuntimeContext<'a> {
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'a>,
    resume_agents_on_restore: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
}

type RestoredSession = (
    Vec<Workspace>,
    HashMap<TerminalId, TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredWorkspace = (
    Workspace,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredTab = (
    crate::workspace::Tab,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
    HashMap<PaneId, u32>,
);
type RestoreFailures<T> = (T, usize);

/// Restore workspaces from a snapshot. Each pane gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    resume_agents_on_restore: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoredSession {
    let mut imported_panes = HashMap::new();
    restore_with_imports(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        crate::pane::PaneShellConfig::new(default_shell, shell_mode),
        resume_agents_on_restore,
        &mut imported_panes,
        events,
        render_notify,
        render_dirty,
    )
}

#[cfg(unix)]
pub fn restore_handoff(
    snapshot: &SessionSnapshot,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    imports: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> std::io::Result<RestoredSession> {
    restore_with_imports_strict(
        snapshot,
        None,
        24,
        80,
        scrollback_limit_bytes,
        crate::pane::PaneShellConfig::new(default_shell, shell_mode),
        true,
        imports,
        events,
        render_notify,
        render_dirty,
    )
}

#[cfg(unix)]
pub fn handoff_pane_aliases(
    snapshot: &SessionSnapshot,
    workspaces: &[Workspace],
) -> HashMap<u32, PaneId> {
    let mut aliases = HashMap::new();
    for (ws_snap, workspace) in snapshot.workspaces.iter().zip(workspaces) {
        for (tab_snap, tab) in ws_snap.tabs.iter().zip(&workspace.tabs) {
            let old_ids = collect_snapshot_pane_ids(&tab_snap.layout);
            let new_ids = tab.layout.pane_ids();
            for (old_id, new_id) in old_ids.into_iter().zip(new_ids) {
                if old_id != new_id.raw() {
                    aliases.insert(old_id, new_id);
                }
            }
        }
    }
    aliases
}

#[cfg(unix)]
fn collect_snapshot_pane_ids(node: &LayoutSnapshot) -> Vec<u32> {
    let mut ids = Vec::new();
    collect_snapshot_ids_inner(node, &mut ids);
    ids
}

#[cfg(unix)]
fn collect_snapshot_ids_inner(node: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match node {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            collect_snapshot_ids_inner(first, ids);
            collect_snapshot_ids_inner(second, ids);
        }
    }
}

fn migrated_public_pane_numbers_by_old_raw(
    snap: &WorkspaceSnapshot,
    next_public_pane_number: &mut usize,
) -> HashMap<u32, usize> {
    let mut public_numbers = snap.public_pane_numbers.clone();
    for tab in &snap.tabs {
        let mut pane_ids = Vec::new();
        collect_layout_snapshot_pane_ids(&tab.layout, &mut pane_ids);
        for old_raw in pane_ids {
            public_numbers.entry(old_raw).or_insert_with(|| {
                let number = *next_public_pane_number;
                *next_public_pane_number += 1;
                number
            });
        }
    }
    public_numbers
}

fn collect_layout_snapshot_pane_ids(node: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match node {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            collect_layout_snapshot_pane_ids(first, ids);
            collect_layout_snapshot_pane_ids(second, ids);
        }
    }
}

#[cfg(unix)]
fn restore_with_imports_strict(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> std::io::Result<RestoredSession> {
    let (restored, failed_imports) = restore_with_imports_and_failures(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        shell_config,
        resume_agents_on_restore,
        imported_panes,
        events,
        render_notify,
        render_dirty,
    );
    if failed_imports > 0 {
        return Err(std::io::Error::other(format!(
            "handoff failed to restore {failed_imports} imported pane runtime(s)"
        )));
    }
    if !imported_panes.is_empty() {
        return Err(std::io::Error::other(format!(
            "handoff import did not consume {} pane runtime(s)",
            imported_panes.len()
        )));
    }
    Ok(restored)
}

fn restore_with_imports(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoredSession {
    restore_with_imports_and_failures(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        shell_config,
        resume_agents_on_restore,
        imported_panes,
        events,
        render_notify,
        render_dirty,
    )
    .0
}

fn restore_with_imports_and_failures(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoreFailures<RestoredSession> {
    let mut workspaces = Vec::new();
    let mut terminals = HashMap::new();
    let mut terminal_runtimes = HashMap::new();
    let mut resumed_agent_sessions = HashSet::new();
    let mut failed_imports = 0;
    for (idx, ws_snap) in snapshot.workspaces.iter().enumerate() {
        let runtime_context = RestoreRuntimeContext {
            scrollback_limit_bytes,
            shell_config,
            resume_agents_on_restore,
            events: events.clone(),
            render_notify: render_notify.clone(),
            render_dirty: render_dirty.clone(),
        };
        // A pinned space that is not the one being restored *into* does not
        // need its shells yet. It keeps its sidebar entry, label and cwd; the
        // shell arrives if the user actually opens it.
        //
        // The active space normally restores hot, so what you land on is live --
        // but not if it was still waiting for its explicit start when the
        // snapshot was taken. Browsing to a pinned space makes it active, and
        // without this check a glance bought a shell on the next restart, which
        // is the cost cold panes exist to avoid.
        let restore_cold = ws_snap.pinned
            && (snapshot.active != Some(idx) || workspace_was_entirely_cold(ws_snap));
        let (restored, workspace_failed_imports) = restore_workspace(
            ws_snap,
            history.and_then(|history| history.workspaces.get(idx)),
            rows,
            cols,
            &runtime_context,
            &mut resumed_agent_sessions,
            imported_panes,
            restore_cold,
        );
        failed_imports += workspace_failed_imports;
        if let Some((workspace, restored_terminals, restored_runtimes)) = restored {
            for terminal in restored_terminals {
                terminals.insert(terminal.id.clone(), terminal);
            }
            terminal_runtimes.extend(restored_runtimes);
            workspaces.push(workspace);
        }
    }
    crate::workspace::reserve_workspace_ids(&workspaces);
    ((workspaces, terminals, terminal_runtimes), failed_imports)
}

/// Whether every pane in the space was still waiting for its explicit start.
///
/// Deliberately "entirely", not "any": a space with one live shell and one cold
/// pane is a space you were working in, and landing on it dead would be worse
/// than the one shell it costs. A space with no panes at all is not cold -- it
/// has nothing to defer, and treating it as cold would suppress the reseeded
/// shell a pinned space is supposed to get.
fn workspace_was_entirely_cold(ws_snap: &crate::persist::snapshot::WorkspaceSnapshot) -> bool {
    let mut saw_pane = false;
    for tab in &ws_snap.tabs {
        for pane in tab.panes.values() {
            saw_pane = true;
            if !pane.cold {
                return false;
            }
        }
    }
    saw_pane
}

fn restore_workspace(
    snap: &WorkspaceSnapshot,
    history: Option<&WorkspaceHistorySnapshot>,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    restore_cold: bool,
) -> RestoreFailures<Option<RestoredWorkspace>> {
    let mut tabs = Vec::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let workspace_id = snap
        .id
        .clone()
        .unwrap_or_else(crate::workspace::generate_workspace_id);
    let mut next_public_pane_number = snap
        .public_pane_numbers
        .values()
        .copied()
        .max()
        .and_then(|max| max.checked_add(1))
        .unwrap_or(1)
        .max(snap.next_public_pane_number);
    let public_pane_numbers_by_old_raw =
        migrated_public_pane_numbers_by_old_raw(snap, &mut next_public_pane_number);
    let public_pane_ids_by_old_raw: HashMap<u32, String> = public_pane_numbers_by_old_raw
        .iter()
        .map(|(old_raw, public_number)| {
            (
                *old_raw,
                format!(
                    "{}:p{}",
                    workspace_id,
                    crate::workspace::encode_public_number(*public_number)
                ),
            )
        })
        .collect();
    let mut public_pane_numbers = HashMap::new();
    let mut next_public_tab_number = snap
        .public_tab_numbers
        .iter()
        .copied()
        .max()
        .and_then(|max| max.checked_add(1))
        .unwrap_or(1)
        .max(snap.next_public_tab_number);
    let mut failed_imports = 0;

    for (idx, tab_snap) in snap.tabs.iter().enumerate() {
        let tab_number = snap.public_tab_numbers.get(idx).copied().unwrap_or(idx + 1);
        let (restored_tab, tab_failed_imports) = restore_tab(
            tab_snap,
            history.and_then(|history| history.tabs.get(idx)),
            tab_number,
            &workspace_id,
            rows,
            cols,
            runtime_context,
            resumed_agent_sessions,
            imported_panes,
            &public_pane_ids_by_old_raw,
            restore_cold,
        );
        failed_imports += tab_failed_imports;
        let Some((mut tab, restored_terminals, restored_runtimes, reverse_id_map)) = restored_tab
        else {
            continue;
        };
        if let Some(public_tab_number) = snap.public_tab_numbers.get(idx).copied() {
            tab.number = public_tab_number;
        }
        next_public_tab_number = next_public_tab_number.max(tab.number + 1);
        for pane_id in tab.layout.pane_ids() {
            let public_number = public_pane_numbers_by_old_raw
                .get(
                    &reverse_id_map
                        .get(&pane_id)
                        .copied()
                        .unwrap_or(pane_id.raw()),
                )
                .copied()
                .unwrap_or_else(|| {
                    let number = next_public_pane_number;
                    next_public_pane_number += 1;
                    number
                });
            public_pane_numbers.insert(pane_id, public_number);
            next_public_pane_number = next_public_pane_number.max(public_number + 1);
        }
        terminals.extend(restored_terminals);
        terminal_runtimes.extend(restored_runtimes);
        tabs.push(tab);
    }

    // A pinned space is meant to survive losing its contents, and that promise
    // should hold across a restart too. If nothing could be rebuilt -- typically
    // because the saved directory no longer exists -- fall back to a single
    // plain shell in the nearest directory that does, rather than dropping the
    // space and silently losing the pin.
    if tabs.is_empty() && snap.pinned {
        let fallback_cwd = nearest_existing_ancestor(&snap.identity_cwd);
        let fallback_snap = TabSnapshot {
            custom_name: None,
            layout: LayoutSnapshot::Pane(1),
            panes: HashMap::from([(
                1,
                super::snapshot::PaneSnapshot {
                    cold: false,
                    cwd: fallback_cwd,
                    label: None,
                    agent_name: None,
                    managed_agent_kind: None,
                    agent_session: None,
                    launch_argv: None,
                },
            )]),
            zoomed: false,
            focused: Some(1),
            root_pane: Some(1),
        };
        let (restored_tab, fallback_failed_imports) = restore_tab(
            &fallback_snap,
            None,
            1,
            &workspace_id,
            rows,
            cols,
            runtime_context,
            resumed_agent_sessions,
            imported_panes,
            &public_pane_ids_by_old_raw,
            restore_cold,
        );
        failed_imports += fallback_failed_imports;
        if let Some((tab, restored_terminals, restored_runtimes, _)) = restored_tab {
            warn!(
                workspace = %workspace_id,
                "pinned workspace had no restorable tabs; seeded a replacement shell"
            );
            for pane_id in tab.layout.pane_ids() {
                public_pane_numbers.insert(pane_id, 1);
                next_public_pane_number = next_public_pane_number.max(2);
            }
            next_public_tab_number = next_public_tab_number.max(2);
            terminals.extend(restored_terminals);
            terminal_runtimes.extend(restored_runtimes);
            tabs.push(tab);
        }
    }

    if tabs.is_empty() {
        return (None, failed_imports);
    }

    let worktree_space = restored_worktree_space_membership(snap.worktree_space.clone());
    let (cached_git_space, cached_auto_label, cached_git_status_key) =
        crate::workspace::discover_workspace_git_identity(&snap.identity_cwd);

    (
        Some(Workspace {
            id: workspace_id,
            custom_name: snap.custom_name.clone(),
            identity_cwd: snap.identity_cwd.clone(),
            cached_identity_cwd: snap.identity_cwd.clone(),
            cached_auto_label,
            cached_git_status_key,
            cached_git_branch: crate::workspace::git_branch(&snap.identity_cwd),
            cached_git_ahead_behind: None,
            cached_git_space,
            worktree_space,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            metadata_token_sequences: HashMap::new(),
            public_pane_numbers,
            next_public_pane_number,
            next_public_tab_number,
            active_tab: snap.active_tab.min(tabs.len().saturating_sub(1)),
            tabs,
            pinned: snap.pinned,
            #[cfg(test)]
            test_runtimes: HashMap::new(),
        })
        .map(|workspace| (workspace, terminals, terminal_runtimes)),
        failed_imports,
    )
}

/// First existing directory at or above `path`, falling back to the home
/// directory and then the filesystem root. Used to place a pinned space's
/// replacement shell somewhere a process can actually start.
fn nearest_existing_ancestor(path: &std::path::Path) -> PathBuf {
    for candidate in path.ancestors() {
        if candidate.is_dir() {
            return candidate.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(std::path::MAIN_SEPARATOR_STR))
}

fn restored_worktree_space_membership(
    space: Option<crate::workspace::WorktreeSpaceMembership>,
) -> Option<crate::workspace::WorktreeSpaceMembership> {
    space.filter(|space| {
        space.checkout_path.exists()
            && crate::workspace::git_space_metadata(&space.checkout_path)
                .is_some_and(|current| current.key == space.key)
    })
}

fn restore_tab(
    snap: &TabSnapshot,
    history: Option<&TabHistorySnapshot>,
    number: usize,
    workspace_id: &str,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    public_pane_ids_by_old_raw: &HashMap<u32, String>,
    restore_cold: bool,
) -> RestoreFailures<Option<RestoredTab>> {
    let (node, id_map) = restore_node_remapped(&snap.layout);
    let reverse_id_map: HashMap<PaneId, u32> = id_map
        .iter()
        .map(|(&old_id, &new_id)| (new_id, old_id))
        .collect();
    let pane_ids = collect_pane_ids(&node);

    let mut panes = HashMap::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let mut failed_imports = 0;
    for id in &pane_ids {
        let old_id = reverse_id_map.get(id);
        let saved_pane = old_id.and_then(|old_id| snap.panes.get(old_id));
        let saved_cwd = saved_pane
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));

        let cwd = if saved_cwd.exists() {
            saved_cwd
        } else {
            warn!(
                cwd = %saved_cwd.display(),
                "saved pane cwd does not exist, falling back to HOME"
            );
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/"));
            if home.exists() {
                home
            } else {
                PathBuf::from("/")
            }
        };

        let saved_label = saved_pane.and_then(|p| p.label.clone());
        let saved_agent_name = saved_pane.and_then(|p| p.agent_name.clone());
        let saved_managed_agent = saved_pane
            .and_then(|pane| pane.managed_agent_kind.as_deref())
            .and_then(crate::detect::parse_canonical_agent_label);
        let saved_launch_argv = saved_pane.and_then(|p| p.launch_argv.clone());
        let saved_agent_session = saved_pane.and_then(|p| p.agent_session.as_ref());
        let saved_history =
            old_id.and_then(|old_id| history.and_then(|history| history.panes.get(old_id)));
        let startup = {
            let mut agent_restore = AgentRestoreState {
                enabled: runtime_context.resume_agents_on_restore,
                resumed_sessions: resumed_agent_sessions,
            };
            pane_restore_startup(saved_agent_session, saved_history, &mut agent_restore)
        };
        let restored_agent_session =
            restored_terminal_agent_session(saved_agent_session, startup.duplicate_agent_session);
        let initial_restore_agent = startup
            .restore_plan
            .as_ref()
            .and_then(|plan| crate::detect::parse_agent_label(&plan.agent));

        let old_pane_id = reverse_id_map.get(id).copied();
        let public_pane_id = old_pane_id
            .and_then(|old_id| public_pane_ids_by_old_raw.get(&old_id))
            .map(String::as_str);
        let launch_env = public_pane_id
            .map(|pane_id| {
                PaneLaunchEnv::from_extra(Vec::new()).with_identity(
                    workspace_id.to_string(),
                    crate::workspace::public_tab_id_for_number(workspace_id, number),
                    pane_id.to_string(),
                )
            })
            .unwrap_or_default();
        let imported_runtime = old_pane_id.and_then(|old_id| imported_panes.remove(&old_id));
        let was_imported = imported_runtime.is_some();
        let pending_native_agent_restore = if was_imported {
            None
        } else {
            startup.restore_plan.clone()
        };
        if let Some(plan) = pending_native_agent_restore {
            let terminal_id = TerminalId::alloc();
            let mut terminal = TerminalState::new(terminal_id.clone(), cwd.clone())
                .with_pending_agent_resume_plan(plan);
            if let Some(label) = saved_label {
                terminal.set_manual_label(label);
            }
            if let Some(session) = restored_agent_session {
                terminal.set_persisted_agent_session(session);
            }
            match (saved_agent_name, saved_managed_agent) {
                (Some(agent_name), Some(agent)) => {
                    terminal.restore_managed_agent(agent_name, agent)
                }
                (Some(_), None) => {}
                (None, _) => {}
            }
            if let Some(agent) = initial_restore_agent {
                let _ = terminal.set_detected_state_with_screen_signals_at(
                    Some(agent),
                    AgentState::Idle,
                    false,
                    false,
                    false,
                    false,
                    std::time::Instant::now(),
                );
            }
            panes.insert(*id, PaneState::new(terminal_id));
            terminals.push(terminal);
            continue;
        }

        // Cold restore for a pinned space nobody is looking at: build the pane
        // and its terminal, but no shell. `start_cold_shells` spawns one when
        // the pane first becomes visible.
        //
        // Ordered after the agent-resume branch on purpose -- a pane with an
        // agent to bring back is that branch's business, and it has its own
        // deferred spawn. Imported handoff panes are excluded too: they already
        // own a live runtime that would be dropped on the floor here.
        if restore_cold && !was_imported {
            let terminal_id = TerminalId::alloc();
            let mut terminal =
                TerminalState::new(terminal_id.clone(), cwd.clone()).with_pending_cold_shell();
            if let Some(label) = saved_label {
                terminal.set_manual_label(label);
            }
            if let Some(session) = restored_agent_session {
                terminal.set_persisted_agent_session(session);
            }
            panes.insert(*id, PaneState::new(terminal_id));
            terminals.push(terminal);
            continue;
        }

        #[cfg(not(unix))]
        if imported_runtime.is_some() {
            failed_imports += 1;
            continue;
        }

        let runtime_result = {
            #[cfg(unix)]
            if let Some(imported) = imported_runtime {
                TerminalRuntime::from_handoff_fd(
                    crate::handoff_runtime::ImportedHandoffRuntime {
                        master_fd: imported.master_fd,
                        state: imported.state.with_pane_id(*id),
                    },
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            } else {
                TerminalRuntime::spawn_with_initial_history(
                    *id,
                    rows,
                    cols,
                    cwd.clone(),
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.shell_config,
                    &launch_env,
                    startup.initial_history_ansi,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            }

            #[cfg(not(unix))]
            {
                TerminalRuntime::spawn_with_initial_history(
                    *id,
                    rows,
                    cols,
                    cwd.clone(),
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.shell_config,
                    &launch_env,
                    startup.initial_history_ansi,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            }
        };

        match runtime_result {
            Ok(runtime) => {
                let terminal_id = TerminalId::alloc();
                let mut terminal = TerminalState::new(terminal_id.clone(), cwd.clone());
                if was_imported {
                    if let Some(argv) = saved_launch_argv {
                        terminal = terminal.with_launch_argv(argv).with_respawn_shell_on_exit();
                    }
                }
                if let Some(label) = saved_label {
                    terminal.set_manual_label(label);
                }
                if let Some(session) = restored_agent_session {
                    terminal.set_persisted_agent_session(session);
                }
                match (saved_agent_name, saved_managed_agent) {
                    (Some(agent_name), Some(agent)) if was_imported => {
                        terminal.restore_managed_agent(agent_name, agent)
                    }
                    (Some(_), Some(_)) => {}
                    (Some(agent_name), None) if was_imported => terminal.set_agent_name(agent_name),
                    (Some(_), None) => {}
                    (None, _) => {}
                }
                if let Some(agent) = initial_restore_agent {
                    let _ = terminal.set_detected_state_with_screen_signals_at(
                        Some(agent),
                        AgentState::Idle,
                        false,
                        false,
                        false,
                        false,
                        std::time::Instant::now(),
                    );
                }
                panes.insert(*id, PaneState::new(terminal_id.clone()));
                terminal_runtimes.insert(terminal_id, runtime);
                terminals.push(terminal);
            }
            Err(e) => {
                if let Some(key) = startup.reserved_agent_session.as_deref() {
                    resumed_agent_sessions.remove(key);
                }
                if was_imported {
                    failed_imports += 1;
                    error!(
                        tab = ?snap.custom_name,
                        pane_id = id.raw(),
                        err = %e,
                        "failed to restore imported pane"
                    );
                }
                error!(
                    tab = ?snap.custom_name,
                    pane_id = id.raw(),
                    err = %e,
                    "failed to restore pane, skipping"
                );
            }
        }
    }

    if panes.is_empty() {
        warn!(
            tab = ?snap.custom_name,
            "no panes could be restored for tab, dropping it"
        );
        return (None, failed_imports);
    }

    let surviving: HashSet<PaneId> = panes.keys().copied().collect();
    let Some(node) = prune_restored_node(node, &surviving) else {
        warn!(
            tab = ?snap.custom_name,
            "restored tab lost all panes after pruning missing layout nodes"
        );
        return (None, failed_imports);
    };
    let pane_ids = collect_pane_ids(&node);
    let Some(focus) = resolve_restored_pane(snap.focused, &id_map, &surviving, &pane_ids) else {
        return (None, failed_imports);
    };
    let Some(root_pane) = resolve_restored_pane(snap.root_pane, &id_map, &surviving, &pane_ids)
    else {
        return (None, failed_imports);
    };
    let layout = TileLayout::from_saved(node, focus);

    (
        Some((
            crate::workspace::Tab {
                custom_name: snap.custom_name.clone(),
                number,
                root_pane,
                layout,
                panes,
                #[cfg(test)]
                runtimes: HashMap::new(),
                zoomed: snap.zoomed,
                events: runtime_context.events.clone(),
                render_notify: runtime_context.render_notify.clone(),
                render_dirty: runtime_context.render_dirty.clone(),
            },
            terminals,
            terminal_runtimes,
            reverse_id_map,
        )),
        failed_imports,
    )
}

fn pane_restore_startup<'a>(
    session: Option<&PaneAgentSessionSnapshot>,
    history: Option<&'a PaneHistorySnapshot>,
    agent_restore: &mut AgentRestoreState<'_>,
) -> PaneRestoreStartup<'a> {
    // Native agent resume owns the conversation history. If a pane has a
    // resumable agent session and resume is enabled, do not replay saved pane
    // presentation history into that terminal, even when this pane is a
    // duplicate suppressed by session de-duplication.
    let restore_plan =
        session.and_then(|session| restore_plan_for_snapshot(session, agent_restore.enabled));
    let has_native_agent_restore = restore_plan.is_some();
    // Reserve before spawning so later panes in the same restore pass cannot
    // launch the same native agent session. The caller rolls this reservation
    // back if runtime spawn fails before any agent process is started.
    let mut reserved_agent_session = None;
    let duplicate_agent_session = restore_plan.as_ref().is_some_and(|plan| {
        if agent_restore
            .resumed_sessions
            .insert(plan.dedupe_key.clone())
        {
            reserved_agent_session = Some(plan.dedupe_key.clone());
            false
        } else {
            true
        }
    });
    let restore_plan = if duplicate_agent_session {
        None
    } else {
        restore_plan
    };

    PaneRestoreStartup {
        restore_plan,
        initial_history_ansi: if has_native_agent_restore {
            None
        } else {
            history.map(|history| history.ansi.as_str())
        },
        duplicate_agent_session,
        reserved_agent_session,
    }
}

fn restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
) -> Option<crate::agent_resume::AgentResumePlan> {
    if !resume_agents_on_restore {
        return None;
    }
    let persisted = persisted_agent_session_from_snapshot(session)?;
    crate::agent_resume::plan(&session.source, &session.agent, &persisted.session_ref)
}

fn persisted_agent_session_from_snapshot(
    session: &PaneAgentSessionSnapshot,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    crate::agent_resume::session_ref_from_snapshot(
        &session.source,
        &session.agent,
        session.kind,
        &session.value,
    )
}

fn restored_terminal_agent_session(
    session: Option<&PaneAgentSessionSnapshot>,
    duplicate_agent_session: bool,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    if duplicate_agent_session {
        return None;
    }
    session.and_then(persisted_agent_session_from_snapshot)
}

#[cfg(test)]
fn take_restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
    resumed_agent_sessions: &mut HashSet<String>,
) -> Option<crate::agent_resume::AgentResumePlan> {
    restore_plan_for_snapshot(session, resume_agents_on_restore)
        .filter(|plan| resumed_agent_sessions.insert(plan.dedupe_key.clone()))
}

pub(super) fn prune_restored_node(node: Node, surviving: &HashSet<PaneId>) -> Option<Node> {
    match node {
        Node::Pane(id) => surviving.contains(&id).then_some(Node::Pane(id)),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first = prune_restored_node(*first, surviving);
            let second = prune_restored_node(*second, surviving);
            match (first, second) {
                (Some(first), Some(second)) => Some(Node::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            }
        }
    }
}

pub(super) fn resolve_restored_pane(
    saved_old_id: Option<u32>,
    id_map: &HashMap<u32, PaneId>,
    surviving: &HashSet<PaneId>,
    pane_ids: &[PaneId],
) -> Option<PaneId> {
    saved_old_id
        .and_then(|old_id| id_map.get(&old_id).copied())
        .filter(|pane_id| surviving.contains(pane_id))
        .or_else(|| pane_ids.first().copied())
}

/// Restore a layout tree, remapping every pane ID to a fresh globally unique one.
/// Returns the new tree and a map of old_raw_id → new PaneId.
pub(super) fn restore_node_remapped(snap: &LayoutSnapshot) -> (Node, HashMap<u32, PaneId>) {
    let mut id_map = HashMap::new();
    let node = remap_inner(snap, &mut id_map);
    (node, id_map)
}

fn remap_inner(snap: &LayoutSnapshot, id_map: &mut HashMap<u32, PaneId>) -> Node {
    match snap {
        LayoutSnapshot::Pane(old_id) => {
            let new_id = PaneId::alloc();
            id_map.insert(*old_id, new_id);
            Node::Pane(new_id)
        }
        LayoutSnapshot::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first_node = remap_inner(first, id_map);
            let second_node = remap_inner(second, id_map);
            let dir = match direction {
                DirectionSnapshot::Horizontal => Direction::Horizontal,
                DirectionSnapshot::Vertical => Direction::Vertical,
            };
            Node::Split {
                direction: dir,
                ratio: *ratio,
                first: Box::new(first_node),
                second: Box::new(second_node),
            }
        }
    }
}

pub(super) fn collect_pane_ids(node: &Node) -> Vec<PaneId> {
    let mut ids = Vec::new();
    collect_ids_inner(node, &mut ids);
    ids
}

fn collect_ids_inner(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids_inner(first, ids);
            collect_ids_inner(second, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    #[cfg(windows)]
    fn test_restore_shell() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn test_restore_shell() -> &'static str {
        "/bin/sh"
    }

    #[test]
    fn capture_and_restore_node_round_trip() {
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(PaneId::from_raw(0))),
            second: Box::new(Node::Split {
                direction: Direction::Vertical,
                ratio: 0.3,
                first: Box::new(Node::Pane(PaneId::from_raw(1))),
                second: Box::new(Node::Pane(PaneId::from_raw(2))),
            }),
        };

        let snap = super::super::snapshot::capture_node(&node);
        let (restored, id_map) = restore_node_remapped(&snap);

        assert_eq!(id_map.len(), 3);
        let ids = collect_pane_ids(&restored);
        assert_eq!(ids.len(), 3);
        let unique: std::collections::HashSet<u32> = ids.iter().map(|id| id.raw()).collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn prune_restored_node_collapses_missing_branch() {
        let keep = PaneId::from_raw(11);
        let missing = PaneId::from_raw(12);
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(keep)),
            second: Box::new(Node::Pane(missing)),
        };
        let surviving = std::collections::HashSet::from([keep]);

        let pruned = prune_restored_node(node, &surviving).expect("remaining pane should survive");

        assert!(matches!(pruned, Node::Pane(id) if id == keep));
    }

    #[test]
    fn resolve_restored_pane_prefers_surviving_saved_id_and_falls_back_to_first_remaining() {
        let first = PaneId::from_raw(21);
        let second = PaneId::from_raw(22);
        let id_map = HashMap::from([(0_u32, first), (1_u32, second)]);
        let surviving = std::collections::HashSet::from([first]);
        let pane_ids = vec![first];

        assert_eq!(
            resolve_restored_pane(Some(0), &id_map, &surviving, &pane_ids),
            Some(first)
        );
        assert_eq!(
            resolve_restored_pane(Some(1), &id_map, &surviving, &pane_ids),
            Some(first)
        );
    }

    #[test]
    fn restored_worktree_space_membership_drops_missing_checkout() {
        let missing =
            std::env::temp_dir().join(format!("herdr-missing-worktree-{}", std::process::id()));
        let membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: missing.join("repo"),
            checkout_path: missing.join("checkout"),
            is_linked_worktree: true,
        };

        assert_eq!(restored_worktree_space_membership(Some(membership)), None);
    }

    #[test]
    fn restore_plan_respects_opt_in_and_allowlist() {
        let pi_session_path = test_session_path("pi-session.jsonl");
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: pi_session_path.clone(),
        };

        assert!(restore_plan_for_snapshot(&session, false).is_none());
        assert_eq!(
            restore_plan_for_snapshot(&session, true).unwrap().argv,
            vec!["pi", "--session", pi_session_path.as_str()]
        );

        let unsupported_path = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("claude-session"),
        };
        assert!(restore_plan_for_snapshot(&unsupported_path, true).is_none());
    }

    #[test]
    fn restore_plan_selection_suppresses_duplicates() {
        let pi_session_path = test_session_path("pi-session.jsonl");
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: pi_session_path.clone(),
        };
        let mut resumed = HashSet::new();

        assert!(take_restore_plan_for_snapshot(&session, false, &mut resumed).is_none());
        assert!(resumed.is_empty());

        let first = take_restore_plan_for_snapshot(&session, true, &mut resumed)
            .expect("first restore should get a plan");
        assert_eq!(
            first.argv,
            vec!["pi", "--session", pi_session_path.as_str()]
        );
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_none());
    }

    #[test]
    fn pane_restore_startup_suppresses_history_for_native_agent_resume() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: true,
            resumed_sessions: &mut resumed,
        };

        let startup = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(startup.restore_plan.is_some());
        assert!(startup.initial_history_ansi.is_none());
        assert!(!startup.duplicate_agent_session);
    }

    #[test]
    fn pane_restore_startup_suppresses_history_for_duplicate_native_agent_session() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: true,
            resumed_sessions: &mut resumed,
        };

        let first = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);
        let duplicate = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(first.restore_plan.is_some());
        assert!(first.initial_history_ansi.is_none());
        assert!(duplicate.restore_plan.is_none());
        assert!(duplicate.initial_history_ansi.is_none());
        assert!(duplicate.duplicate_agent_session);
    }

    #[test]
    fn pane_restore_startup_keeps_history_without_native_agent_resume() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: false,
            resumed_sessions: &mut resumed,
        };

        let startup = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(startup.restore_plan.is_none());
        assert_eq!(startup.initial_history_ansi, Some("RESTORED_HISTORY\r\n"));
        assert!(!startup.duplicate_agent_session);
        assert!(resumed.is_empty());
    }

    #[test]
    fn restore_rehydrates_agent_session_metadata() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Id,
            value: "hermes-session".into(),
        };

        let preserved = restored_terminal_agent_session(Some(&session), false)
            .expect("restore should preserve metadata");
        assert_eq!(preserved.source, "herdr:hermes");
        assert_eq!(preserved.agent, "hermes");
        assert_eq!(preserved.session_ref.value, "hermes-session");
    }

    #[test]
    fn restore_does_not_rehydrate_duplicate_agent_session_metadata() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let mut resumed = HashSet::new();
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_some());
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_none());

        assert!(restored_terminal_agent_session(Some(&session), true).is_none());
    }

    /// A workspace snapshot with nothing restorable in it, and an identity_cwd
    /// pointing at a directory that no longer exists. This is the state that
    /// used to make a workspace vanish: every tab failed to rebuild, so `tabs`
    /// came out empty.
    fn snapshot_with_no_restorable_tabs(pinned: bool) -> SessionSnapshot {
        let missing = std::env::current_dir()
            .unwrap()
            .join("definitely-not-a-real-directory-for-restore-tests");
        SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                pinned,
                id: Some("workspace".into()),
                custom_name: Some("gone".into()),
                identity_cwd: missing,
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: Vec::new(),
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        }
    }

    fn restore_snapshot(snapshot: &SessionSnapshot) -> Vec<Workspace> {
        let (events, _event_rx) = mpsc::channel(4);
        let (workspaces, _terminals, _runtimes) = restore(
            snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );
        workspaces
    }

    #[tokio::test]
    async fn pinned_workspace_with_nothing_restorable_is_seeded_instead_of_dropped() {
        let workspaces = restore_snapshot(&snapshot_with_no_restorable_tabs(true));

        assert_eq!(
            workspaces.len(),
            1,
            "a pinned space must not disappear just because nothing could be rebuilt"
        );
        assert!(workspaces[0].pinned, "the pin itself must survive too");
        assert_eq!(
            workspaces[0].tabs.len(),
            1,
            "the replacement shell should leave exactly one tab"
        );
        assert_eq!(workspaces[0].custom_name.as_deref(), Some("gone"));
        // The saved directory is gone, so the shell must have landed on an
        // ancestor that exists rather than failing to start.
        assert!(
            workspaces[0].identity_cwd.parent().is_some(),
            "replacement shell needs a usable directory"
        );
        workspaces[0].assert_invariants_for_test();
    }

    #[tokio::test]
    async fn unpinned_workspace_with_nothing_restorable_is_still_dropped() {
        let workspaces = restore_snapshot(&snapshot_with_no_restorable_tabs(false));

        assert!(
            workspaces.is_empty(),
            "unpinned spaces keep the existing drop behavior"
        );
    }

    /// Two pinned spaces, one active. Restore should bring the active one back
    /// with a live shell and leave the background one cold.
    ///
    /// This is the whole point of the change: 16 pinned spaces used to mean 16
    /// shells at every startup, whether or not any of them were opened.
    fn two_pinned_spaces_snapshot(active: Option<usize>) -> SessionSnapshot {
        let cwd = std::env::current_dir().unwrap();
        let space = |id: &str| WorkspaceSnapshot {
            pinned: true,
            id: Some(id.into()),
            custom_name: Some(id.into()),
            identity_cwd: cwd.clone(),
            worktree_space: None,
            public_pane_numbers: HashMap::new(),
            next_public_pane_number: 0,
            public_tab_numbers: Vec::new(),
            next_public_tab_number: 0,
            tabs: vec![TabSnapshot {
                custom_name: None,
                layout: LayoutSnapshot::Pane(0),
                panes: HashMap::from([(
                    0,
                    super::super::snapshot::PaneSnapshot {
                        cold: false,
                        cwd: cwd.clone(),
                        label: None,
                        agent_name: None,
                        managed_agent_kind: None,
                        agent_session: None,
                        launch_argv: None,
                    },
                )]),
                zoomed: false,
                focused: Some(0),
                root_pane: Some(0),
            }],
            active_tab: 0,
        };
        SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![space("front"), space("back")],
            active,
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        }
    }

    fn restore_snapshot_full(
        snapshot: &SessionSnapshot,
    ) -> (
        Vec<Workspace>,
        HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
        HashMap<crate::terminal::TerminalId, crate::terminal::TerminalRuntime>,
    ) {
        let (events, _event_rx) = mpsc::channel(4);
        restore(
            snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
    }

    /// Returns (has_runtime, pending_cold_shell) for a workspace's root pane.
    fn pane_liveness(
        ws: &Workspace,
        terminals: &HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
        runtimes: &HashMap<crate::terminal::TerminalId, crate::terminal::TerminalRuntime>,
    ) -> (bool, bool) {
        let root = ws.tabs[0].root_pane;
        let terminal_id = ws.tabs[0]
            .panes
            .get(&root)
            .map(|pane| pane.attached_terminal_id.clone())
            .expect("root pane must have a terminal");
        (
            runtimes.contains_key(&terminal_id),
            terminals
                .get(&terminal_id)
                .is_some_and(|terminal| terminal.pending_cold_shell),
        )
    }

    #[tokio::test]
    async fn a_cold_restored_pane_has_a_public_pane_number() {
        // `start_cold_shell` needs `pane_launch_env`, which walks
        // public_workspace_id -> find_tab_index_for_pane -> public_tab_id ->
        // public_pane_id. That last hop reads `public_pane_numbers`. If a cold
        // pane is missing from that map the whole chain returns None and the
        // shell silently never starts -- the pane just stays blank forever.
        let (workspaces, _terminals, _runtimes) =
            restore_snapshot_full(&two_pinned_spaces_snapshot(Some(0)));

        let cold = &workspaces[1];
        let root = cold.tabs[0].root_pane;

        assert!(
            cold.find_tab_index_for_pane(root).is_some(),
            "the cold pane must be findable in a tab"
        );
        assert!(
            cold.public_pane_number(root).is_some(),
            "a cold pane must have a public pane number, or pane_launch_env              returns None and the deferred shell can never spawn"
        );
    }

    /// Mark every pane in the snapshot as still waiting for its explicit start.
    fn mark_all_panes_cold(snapshot: &mut SessionSnapshot) {
        for ws in &mut snapshot.workspaces {
            for tab in &mut ws.tabs {
                for pane in tab.panes.values_mut() {
                    pane.cold = true;
                }
            }
        }
    }

    #[tokio::test]
    async fn a_glanced_at_pinned_space_does_not_restore_hot() {
        // Browsing to a cold pinned space makes it the active space, and the
        // active space restores hot. So without the recorded cold flag, merely
        // looking at a pinned space bought it a shell on the next restart --
        // undoing exactly what explicit start was built to prevent, one restart
        // later and far from the cause.
        let mut snapshot = two_pinned_spaces_snapshot(Some(0));
        mark_all_panes_cold(&mut snapshot);

        let (workspaces, terminals, runtimes) = restore_snapshot_full(&snapshot);

        let (front_live, front_cold) = pane_liveness(&workspaces[0], &terminals, &runtimes);
        assert!(
            !front_live,
            "a space that was never started must not come back with a shell just \
             because it was the last one looked at"
        );
        assert!(
            front_cold,
            "it must still be waiting for its explicit start"
        );
        // The pin itself is untouched: this is about the shell, not the entry.
        assert!(workspaces[0].pinned);
        assert_eq!(workspaces[0].custom_name.as_deref(), Some("front"));
        workspaces[0].assert_invariants_for_test();
    }

    #[tokio::test]
    async fn a_snapshot_without_cold_flags_restores_the_active_space_hot() {
        // Snapshots written before the flag existed have `cold: false`
        // everywhere, and must keep the old behaviour rather than silently
        // becoming dead spaces on the first restart after upgrading.
        let (workspaces, terminals, runtimes) =
            restore_snapshot_full(&two_pinned_spaces_snapshot(Some(0)));
        let (front_live, _) = pane_liveness(&workspaces[0], &terminals, &runtimes);
        assert!(front_live, "an old snapshot must still restore hot");
    }

    #[tokio::test]
    async fn a_background_pinned_space_restores_without_a_shell() {
        let (workspaces, terminals, runtimes) =
            restore_snapshot_full(&two_pinned_spaces_snapshot(Some(0)));

        assert_eq!(workspaces.len(), 2, "both pinned spaces should come back");

        let (front_live, front_cold) = pane_liveness(&workspaces[0], &terminals, &runtimes);
        assert!(
            front_live,
            "the active space must restore hot -- what you land on has to work"
        );
        assert!(!front_cold, "and must not be marked for a deferred shell");

        let (back_live, back_cold) = pane_liveness(&workspaces[1], &terminals, &runtimes);
        assert!(
            !back_live,
            "a pinned space nobody is looking at must not spawn a shell"
        );
        assert!(
            back_cold,
            "it should be marked so the shell starts when the pane is first seen"
        );

        // The sidebar entry is the point of the pin, and it must be intact
        // without a process behind it.
        assert!(workspaces[1].pinned);
        assert_eq!(workspaces[1].custom_name.as_deref(), Some("back"));
        assert_eq!(workspaces[1].tabs.len(), 1);
        workspaces[1].assert_invariants_for_test();
    }

    #[tokio::test]
    async fn a_cold_space_survives_a_save_and_restore_round_trip() {
        // A cold pane has no runtime, so `cwd_for_pane` has to fall back to the
        // TerminalState to snapshot it. If that ever regressed, a pinned space
        // would quietly drift to the wrong directory across restarts -- and it
        // would only show up on the *second* restart, which is a miserable bug
        // to find.
        let (workspaces, terminals, runtimes) =
            restore_snapshot_full(&two_pinned_spaces_snapshot(Some(0)));
        let expected_cwd = std::env::current_dir().unwrap();

        let mut registry = crate::terminal::TerminalRuntimeRegistry::new();
        for (terminal_id, runtime) in runtimes {
            registry.insert(terminal_id, runtime);
        }
        let snapshot = super::super::snapshot::capture(
            &workspaces,
            &terminals,
            &registry,
            Some(0),
            0,
            0,
            0.5,
            Default::default(),
        );

        assert_eq!(snapshot.workspaces.len(), 2);
        assert!(
            snapshot.workspaces[1].pinned,
            "the pin has to survive a save while cold"
        );

        let (again, terminals_again, runtimes_again) = restore_snapshot_full(&snapshot);
        assert_eq!(again.len(), 2);

        let (live, cold) = pane_liveness(&again[1], &terminals_again, &runtimes_again);
        assert!(!live, "still nobody looking at it, so still no shell");
        assert!(cold);

        let root = again[1].tabs[0].root_pane;
        let mut registry_again = crate::terminal::TerminalRuntimeRegistry::new();
        for (terminal_id, runtime) in runtimes_again {
            registry_again.insert(terminal_id, runtime);
        }
        assert_eq!(
            again[1].tabs[0].cwd_for_pane(root, &terminals_again, &registry_again),
            Some(expected_cwd),
            "a cold pane must remember where it is meant to open"
        );
    }

    #[tokio::test]
    async fn an_unpinned_background_space_still_restores_hot() {
        // Cold restore is scoped to pinned spaces. An ordinary space is one the
        // user opened for work, not a bookmark, so it keeps its shell.
        let mut snapshot = two_pinned_spaces_snapshot(Some(0));
        snapshot.workspaces[1].pinned = false;

        let (workspaces, terminals, runtimes) = restore_snapshot_full(&snapshot);

        let (live, cold) = pane_liveness(&workspaces[1], &terminals, &runtimes);
        assert!(live, "unpinned spaces keep the existing restore behavior");
        assert!(!cold);
    }

    #[tokio::test]
    async fn with_no_active_space_every_pinned_space_restores_cold() {
        // `active: None` means nothing is focused yet, so nothing is visible and
        // nothing needs a shell. The first space the user opens spawns its own.
        let (workspaces, terminals, runtimes) =
            restore_snapshot_full(&two_pinned_spaces_snapshot(None));

        for ws in &workspaces {
            let (live, cold) = pane_liveness(ws, &terminals, &runtimes);
            assert!(!live, "no space is visible, so none should have a shell");
            assert!(cold);
        }
    }

    #[tokio::test]
    async fn a_missing_directory_alone_does_not_drop_a_workspace() {
        // Characterization: spawning a shell whose cwd no longer exists still
        // succeeds here, so a moved or deleted repo does NOT by itself trigger
        // the drop path. Recorded so the seeding fallback above is understood
        // as a guard for the general "nothing rebuilt" case rather than a fix
        // for missing directories specifically.
        let missing = std::env::current_dir()
            .unwrap()
            .join("definitely-not-a-real-directory-for-restore-tests");
        let mut snapshot = snapshot_with_no_restorable_tabs(false);
        snapshot.workspaces[0].tabs = vec![TabSnapshot {
            custom_name: None,
            layout: LayoutSnapshot::Pane(0),
            panes: HashMap::from([(
                0,
                super::super::snapshot::PaneSnapshot {
                    cold: false,
                    cwd: missing,
                    label: None,
                    agent_name: None,
                    managed_agent_kind: None,
                    agent_session: None,
                    launch_argv: None,
                },
            )]),
            zoomed: false,
            focused: Some(0),
            root_pane: Some(0),
        }];

        let workspaces = restore_snapshot(&snapshot);

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].tabs.len(), 1);
    }

    #[tokio::test]
    async fn restore_carries_persisted_agent_session_metadata() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                pinned: false,
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    layout: LayoutSnapshot::Pane(0),
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneSnapshot {
                            cold: false,
                            cwd,
                            label: Some("reviewer".into()),
                            agent_name: Some("reviewer".into()),
                            managed_agent_kind: Some("opencode".into()),
                            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                                source: "herdr:opencode".into(),
                                agent: "opencode".into(),
                                kind: crate::agent_resume::AgentSessionRefKind::Id,
                                value: "opencode-session".into(),
                            }),
                            launch_argv: None,
                        },
                    )]),
                    zoomed: false,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (_workspaces, terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let terminal = terminals
            .values()
            .next()
            .expect("restored terminal should exist");
        assert!(
            !terminal.respawn_shell_on_exit,
            "agent sessions should not use native restore lifecycle when resume_agents_on_restore is disabled"
        );
        assert_eq!(terminal.agent_name, None);
        assert_eq!(terminal.manual_label.as_deref(), Some("reviewer"));
        let session = terminal
            .persisted_agent_session
            .as_ref()
            .expect("persisted agent session should survive restore");
        assert_eq!(session.source, "herdr:opencode");
        assert_eq!(session.agent, "opencode");
        assert_eq!(session.session_ref.value, "opencode-session");
    }

    #[tokio::test]
    async fn restore_preserves_public_id_mapping_after_pane_id_remap() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                pinned: false,
                id: Some("w1".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                public_pane_numbers: HashMap::from([(10, 1), (20, 3)]),
                next_public_pane_number: 4,
                public_tab_numbers: vec![5],
                next_public_tab_number: 6,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    layout: LayoutSnapshot::Split {
                        direction: super::super::snapshot::DirectionSnapshot::Horizontal,
                        ratio: 0.5,
                        first: Box::new(LayoutSnapshot::Pane(10)),
                        second: Box::new(LayoutSnapshot::Pane(20)),
                    },
                    panes: HashMap::from([
                        (
                            10,
                            super::super::snapshot::PaneSnapshot {
                                cold: false,
                                cwd: cwd.clone(),
                                label: None,
                                agent_name: None,
                                managed_agent_kind: None,
                                agent_session: None,
                                launch_argv: None,
                            },
                        ),
                        (
                            20,
                            super::super::snapshot::PaneSnapshot {
                                cold: false,
                                cwd: cwd.clone(),
                                label: None,
                                agent_name: None,
                                managed_agent_kind: None,
                                agent_session: None,
                                launch_argv: None,
                            },
                        ),
                    ]),
                    zoomed: false,
                    focused: Some(10),
                    root_pane: Some(10),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (workspaces, _terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let workspace = workspaces.first().expect("workspace should restore");
        let mut public_numbers: Vec<_> = workspace.public_pane_numbers.values().copied().collect();
        public_numbers.sort_unstable();
        assert_eq!(public_numbers, vec![1, 3]);
        assert_eq!(workspace.next_public_pane_number, 4);
        assert_eq!(workspace.tabs[0].number, 5);
        assert_eq!(workspace.next_public_tab_number, 6);
    }

    #[tokio::test]
    async fn cold_restore_with_gapped_public_tab_numbers_drops_unmanaged_agent_name() {
        let cwd = std::env::current_dir().unwrap();
        let pane_snap = |id: &str| {
            (
                id.parse::<u32>().unwrap(),
                super::super::snapshot::PaneSnapshot {
                    cold: false,
                    cwd: cwd.clone(),
                    label: None,
                    agent_name: None,
                    managed_agent_kind: None,
                    agent_session: None,
                    launch_argv: None,
                },
            )
        };
        let final_pane = super::super::snapshot::PaneSnapshot {
            cold: false,
            cwd: cwd.clone(),
            label: Some("planner".into()),
            agent_name: Some("planner".into()),
            managed_agent_kind: None,
            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                kind: crate::agent_resume::AgentSessionRefKind::Id,
                value: "codex-session".into(),
            }),
            launch_argv: None,
        };
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                pinned: false,
                id: Some("w1".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                public_pane_numbers: HashMap::from([(10, 1), (11, 2), (12, 3), (13, 4)]),
                next_public_pane_number: 5,
                public_tab_numbers: vec![1, 3, 4, 5],
                next_public_tab_number: 6,
                tabs: vec![
                    TabSnapshot {
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(10),
                        panes: HashMap::from([pane_snap("10")]),
                        zoomed: false,
                        focused: Some(10),
                        root_pane: Some(10),
                    },
                    TabSnapshot {
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(11),
                        panes: HashMap::from([pane_snap("11")]),
                        zoomed: false,
                        focused: Some(11),
                        root_pane: Some(11),
                    },
                    TabSnapshot {
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(12),
                        panes: HashMap::from([pane_snap("12")]),
                        zoomed: false,
                        focused: Some(12),
                        root_pane: Some(12),
                    },
                    TabSnapshot {
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(13),
                        panes: HashMap::from([(13, final_pane)]),
                        zoomed: false,
                        focused: Some(13),
                        root_pane: Some(13),
                    },
                ],
                active_tab: 3,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (workspaces, terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let workspace = workspaces.first().expect("workspace should restore");
        assert_eq!(workspace.active_tab, 3);
        assert_eq!(workspace.tabs[3].number, 5);
        let agent_pane = workspace.tabs[3].root_pane;
        let terminal_id = &workspace.tabs[3].panes[&agent_pane].attached_terminal_id;
        assert!(terminals[terminal_id].agent_name.is_none());
        assert_eq!(terminals[terminal_id].managed_agent_kind(), None);
        assert!(workspace
            .pane_details(&terminals)
            .into_iter()
            .all(|detail| detail.pane_id != agent_pane));
    }

    #[test]
    fn legacy_restore_precomputes_missing_public_pane_numbers() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = WorkspaceSnapshot {
            pinned: false,
            id: Some("w1".into()),
            custom_name: None,
            identity_cwd: cwd,
            worktree_space: None,
            public_pane_numbers: HashMap::new(),
            next_public_pane_number: 0,
            public_tab_numbers: Vec::new(),
            next_public_tab_number: 0,
            tabs: vec![TabSnapshot {
                custom_name: None,
                layout: LayoutSnapshot::Split {
                    direction: super::super::snapshot::DirectionSnapshot::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutSnapshot::Pane(10)),
                    second: Box::new(LayoutSnapshot::Pane(20)),
                },
                panes: HashMap::new(),
                zoomed: false,
                focused: Some(10),
                root_pane: Some(10),
            }],
            active_tab: 0,
        };
        let mut next_public_pane_number = 1;

        let public_numbers =
            migrated_public_pane_numbers_by_old_raw(&snapshot, &mut next_public_pane_number);

        assert_eq!(public_numbers, HashMap::from([(10, 1), (20, 2)]));
        assert_eq!(next_public_pane_number, 3);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn native_agent_restore_defers_runtime_launch() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    layout: LayoutSnapshot::Pane(0),
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneSnapshot {
                            cold: false,
                            cwd,
                            label: None,
                            agent_name: None,
                            managed_agent_kind: None,
                            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                                source: "herdr:codex".into(),
                                agent: "codex".into(),
                                kind: crate::agent_resume::AgentSessionRefKind::Id,
                                value: "codex-session".into(),
                            }),
                            launch_argv: None,
                        },
                    )]),
                    zoomed: false,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (_workspaces, terminals, runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            true,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let terminal = terminals
            .values()
            .next()
            .expect("native agent restore should create terminal state");
        assert!(
            terminal.pending_agent_resume_plan.is_some(),
            "restored native agent panes should defer resume until client terminal context is known"
        );
        assert!(
            !terminal.respawn_shell_on_exit,
            "deferred agent resume should not use native restore lifecycle before launch"
        );
        assert!(
            runtimes.is_empty(),
            "native agent restore should not spawn a fallback-size runtime during snapshot restore"
        );
        let mut imports = HashMap::new();
        let (_handoff_workspaces, handoff_terminals, handoff_runtimes) = restore_handoff(
            &snapshot,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            &mut imports,
            mpsc::channel(4).0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .expect("handoff restore should preserve pending native agent resume");
        let handoff_terminal = handoff_terminals
            .values()
            .next()
            .expect("handoff restore should create terminal state");
        assert!(
            handoff_terminal.pending_agent_resume_plan.is_some(),
            "handoff restore should preserve pending native agent resume intent"
        );
        assert!(
            handoff_runtimes.is_empty(),
            "handoff restore should not replace pending native agent resume with a shell runtime"
        );
    }

    #[tokio::test]
    async fn restore_seeds_saved_pane_history_into_runtime() {
        let (snapshot, history) = snapshot_with_saved_pane_history();
        let (events, _events_rx) = mpsc::channel(8);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());

        let (_workspaces, _terminals, runtimes) = restore(
            &snapshot,
            Some(&history),
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            render_notify,
            render_dirty,
        );
        let runtime = runtimes
            .values()
            .next()
            .expect("restored runtime should exist");

        let restored_text = runtime.recent_unwrapped_text(10);
        assert!(
            restored_text.contains("RESTORED_HISTORY 👨‍👩‍👧 LINK"),
            "styled Unicode and hyperlink text should survive history replay"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = runtime.try_send_bytes(bytes::Bytes::from_static(b"exit\n"));
    }

    #[tokio::test]
    async fn restore_without_history_snapshot_keeps_pane_contents_empty() {
        let (snapshot, _history) = snapshot_with_saved_pane_history();
        let (events, _events_rx) = mpsc::channel(8);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());

        let (_workspaces, _terminals, runtimes) = restore(
            &snapshot,
            None,
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            render_notify,
            render_dirty,
        );
        let runtime = runtimes
            .values()
            .next()
            .expect("restored runtime should exist");

        assert!(
            !runtime
                .recent_unwrapped_text(10)
                .contains("RESTORED_HISTORY"),
            "pane history should not restore unless a history snapshot is supplied"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = runtime.try_send_bytes(bytes::Bytes::from_static(b"exit\n"));
    }

    fn snapshot_with_saved_pane_history() -> (SessionSnapshot, SessionHistorySnapshot) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut panes = HashMap::new();
        panes.insert(
            0,
            super::super::snapshot::PaneSnapshot {
                cold: false,
                cwd: cwd.clone(),
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                launch_argv: None,
            },
        );
        let history = SessionHistorySnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceHistorySnapshot {
                tabs: vec![super::super::snapshot::TabHistorySnapshot {
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneHistorySnapshot {
                            ansi: concat!(
                                "\x1b[31mRESTORED_HISTORY 👨‍👩‍👧\x1b[0m ",
                                "\x1b]8;;https://example.com\x1b\\LINK\x1b]8;;\x1b\\\r\n"
                            )
                            .to_string(),
                            lines: 1,
                        },
                    )]),
                }],
            }],
        };
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                pinned: false,
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd,
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    layout: LayoutSnapshot::Pane(0),
                    panes,
                    zoomed: false,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: Default::default(),
        };
        (snapshot, history)
    }
}
