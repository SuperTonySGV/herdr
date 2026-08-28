//! The list the New button opens: somewhere to put remembered directories so
//! they do not each have to cost a pinned sidebar row.
//!
//! Presentation only. The rows are built from the places store, which is
//! server-owned and reachable through `place.*`; nothing here is the authority
//! on what a place *is*.

use std::path::{Path, PathBuf};

use crate::app::state::{text_matches_query, AppState, Mode};
use crate::app::App;
use crate::places;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpacePickerRow {
    /// Exactly what the New button did before there was a picker. Kept first so
    /// the old reflex -- New, Enter -- still does the old thing.
    NewHere { cwd: PathBuf },
    Place {
        label: String,
        path: PathBuf,
        missing: bool,
    },
    Recent {
        label: String,
        path: PathBuf,
        missing: bool,
    },
}

impl SpacePickerRow {
    pub fn path(&self) -> &Path {
        match self {
            Self::NewHere { cwd } => cwd,
            Self::Place { path, .. } | Self::Recent { path, .. } => path,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::NewHere { .. } => "New space here",
            Self::Place { label, .. } | Self::Recent { label, .. } => label,
        }
    }

    pub fn missing(&self) -> bool {
        match self {
            Self::NewHere { .. } => false,
            Self::Place { missing, .. } | Self::Recent { missing, .. } => *missing,
        }
    }

    /// What the filter matches against: the name and the path, so either way of
    /// remembering a repo finds it.
    fn search_text(&self) -> String {
        format!("{} {}", self.label(), self.path().display())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpacePickerState {
    pub rows: Vec<SpacePickerRow>,
    pub query: String,
    pub selected: usize,
    pub offset: usize,
    /// Set when the store could not be read. Shown in place of rows rather than
    /// rendering an empty list, which would look exactly like data loss.
    pub store_error: Option<String>,
}

impl SpacePickerState {
    /// Indices of `rows` that survive the current filter.
    ///
    /// Row 0 always survives: it is the way out of a filter that matched
    /// nothing, and losing it would leave the user stuck in a dialog with no
    /// action available.
    pub fn visible(&self) -> Vec<usize> {
        if self.query.trim().is_empty() {
            return (0..self.rows.len()).collect();
        }
        (0..self.rows.len())
            .filter(|idx| {
                *idx == 0 || text_matches_query(&self.query, &self.rows[*idx].search_text())
            })
            .collect()
    }

    pub fn selected_row(&self) -> Option<&SpacePickerRow> {
        let visible = self.visible();
        visible
            .get(self.selected)
            .and_then(|idx| self.rows.get(*idx))
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, len as isize - 1) as usize;
    }

    /// Keep the selection inside the list after the filter changes.
    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.clamp_selection();
    }
}

impl App {
    /// Build the rows and enter the picker.
    ///
    /// `cwd` is what a plain "new space" would have used, so row 0 reproduces
    /// the pre-picker behaviour exactly.
    pub(crate) fn open_space_picker(&mut self, cwd: PathBuf) {
        let mut rows = vec![SpacePickerRow::NewHere { cwd }];
        let mut store_error = None;

        match places::load(&self.places_path) {
            Ok(file) => {
                for place in &file.places {
                    rows.push(SpacePickerRow::Place {
                        label: display_label(&place.label, &place.path),
                        path: place.path.clone(),
                        missing: !place.path.is_dir(),
                    });
                }
                for recent in file.recents_excluding_places() {
                    rows.push(SpacePickerRow::Recent {
                        label: display_label(
                            recent.label.as_deref().unwrap_or_default(),
                            &recent.path,
                        ),
                        path: recent.path.clone(),
                        missing: !recent.path.is_dir(),
                    });
                }
            }
            // Never silently empty: say what is wrong and leave the file alone.
            Err(err) => store_error = Some(err.to_string()),
        }

        self.state.space_picker = SpacePickerState {
            rows,
            query: String::new(),
            selected: 0,
            offset: 0,
            store_error,
        };
        self.state.mode = Mode::SpacePicker;
    }

    /// Act on the highlighted row.
    ///
    /// Creation is still deferred to the naming dialog when the user asked to
    /// be prompted: that dialog is what makes Esc mean "never mind" today, and
    /// creating first would make cancelling impossible to honour.
    pub(crate) fn accept_space_picker(&mut self) {
        let Some(row) = self.state.space_picker.selected_row().cloned() else {
            return;
        };
        let cwd = row.path().to_path_buf();
        self.state.space_picker = SpacePickerState::default();

        if self.state.prompt_new_workspace_name {
            crate::app::input::open_new_workspace_dialog(&mut self.state, cwd);
            return;
        }

        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
        self.runtime_workspace_create(
            "tui.space_picker.create",
            crate::api::schema::WorkspaceCreateParams {
                cwd: Some(cwd.display().to_string()),
                focus: true,
                label: None,
                env: Default::default(),
            },
        );
    }

    /// Dispatch one key, including the two that change mode.
    pub(crate) fn handle_space_picker_key_event(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => self.cancel_space_picker(),
            KeyCode::Enter => self.accept_space_picker(),
            _ => {
                handle_space_picker_key(&mut self.state, key);
            }
        }
    }

    pub(crate) fn cancel_space_picker(&mut self) {
        self.state.space_picker = SpacePickerState::default();
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }
}

/// Fall back to the directory name when nothing better was stored.
fn display_label(label: &str, path: &Path) -> String {
    if label.trim().is_empty() {
        crate::workspace::derive_label_from_cwd(path)
    } else {
        label.to_string()
    }
}

/// Route a key press in the picker. Returns whether the picker consumed it.
pub(crate) fn handle_space_picker_key(
    state: &mut AppState,
    key: crossterm::event::KeyEvent,
) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};

    match key.code {
        KeyCode::Esc => false, // the caller cancels; it owns the mode change
        KeyCode::Up => {
            state.space_picker.move_selection(-1);
            true
        }
        KeyCode::Down => {
            state.space_picker.move_selection(1);
            true
        }
        KeyCode::Backspace => {
            let mut query = state.space_picker.query.clone();
            query.pop();
            state.space_picker.set_query(query);
            true
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let mut query = state.space_picker.query.clone();
            query.push(ch);
            state.space_picker.set_query(query);
            true
        }
        _ => true,
    }
}

/// Append committed or pasted text to the filter.
pub(crate) fn insert_space_picker_text(state: &mut AppState, text: &str) {
    let mut query = state.space_picker.query.clone();
    query.push_str(text);
    state.space_picker.set_query(query);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker_with(rows: Vec<SpacePickerRow>) -> SpacePickerState {
        SpacePickerState {
            rows,
            ..Default::default()
        }
    }

    fn place(label: &str, path: &str) -> SpacePickerRow {
        SpacePickerRow::Place {
            label: label.into(),
            path: PathBuf::from(path),
            missing: false,
        }
    }

    #[test]
    fn the_new_here_row_is_first_and_selected() {
        // New, Enter has to keep meaning what it meant before the picker
        // existed, or every existing reflex breaks at once.
        let picker = picker_with(vec![
            SpacePickerRow::NewHere {
                cwd: PathBuf::from("/here"),
            },
            place("herdr", "/repos/herdr"),
        ]);
        assert!(matches!(
            picker.selected_row(),
            Some(SpacePickerRow::NewHere { .. })
        ));
    }

    #[test]
    fn filtering_matches_both_the_label_and_the_path() {
        let mut picker = picker_with(vec![
            SpacePickerRow::NewHere {
                cwd: PathBuf::from("/here"),
            },
            place("herdr", "/repos/herdr"),
            place("compass", "/repos/compass"),
        ]);

        picker.set_query("compass".into());
        let visible = picker.visible();
        assert_eq!(visible.len(), 2, "row 0 plus the one match");

        picker.set_query("repos".into());
        assert_eq!(picker.visible().len(), 3, "the path matches both places");
    }

    #[test]
    fn a_filter_that_matches_nothing_still_leaves_a_way_out() {
        // Filtering row 0 away would strand the user in a dialog with nothing
        // to press.
        let mut picker = picker_with(vec![
            SpacePickerRow::NewHere {
                cwd: PathBuf::from("/here"),
            },
            place("herdr", "/repos/herdr"),
        ]);
        picker.set_query("nothing-matches-this".into());

        assert_eq!(picker.visible(), vec![0]);
        assert!(matches!(
            picker.selected_row(),
            Some(SpacePickerRow::NewHere { .. })
        ));
    }

    #[test]
    fn the_selection_is_pulled_back_when_the_filter_shrinks_the_list() {
        let mut picker = picker_with(vec![
            SpacePickerRow::NewHere {
                cwd: PathBuf::from("/here"),
            },
            place("herdr", "/repos/herdr"),
            place("compass", "/repos/compass"),
        ]);
        picker.move_selection(2);
        assert_eq!(picker.selected, 2);

        picker.set_query("nothing".into());
        assert_eq!(picker.selected, 0, "a stale index would select nothing");
    }

    #[test]
    fn selection_does_not_run_off_either_end() {
        let mut picker = picker_with(vec![
            SpacePickerRow::NewHere {
                cwd: PathBuf::from("/here"),
            },
            place("herdr", "/repos/herdr"),
        ]);
        picker.move_selection(-5);
        assert_eq!(picker.selected, 0);
        picker.move_selection(99);
        assert_eq!(picker.selected, 1);
    }

    #[test]
    fn a_missing_directory_is_still_offered_rather_than_hidden() {
        // Dropping an entry because a drive is unplugged is the silent loss
        // this feature exists to prevent.
        let row = SpacePickerRow::Place {
            label: "gone".into(),
            path: PathBuf::from("/definitely/not/here"),
            missing: true,
        };
        let picker = picker_with(vec![
            SpacePickerRow::NewHere {
                cwd: PathBuf::from("/here"),
            },
            row,
        ]);
        assert_eq!(picker.visible().len(), 2);
        assert!(picker.rows[1].missing());
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::app::state::Mode;
    use crate::app::App;
    use crate::config::Config;
    use crate::workspace::Workspace;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("here")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-picker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        app.places_path = dir.join("places.json");
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[tokio::test]
    async fn the_new_button_opens_the_picker_without_creating_anything() {
        let mut app = app();
        let before = app.state.workspaces.len();

        app.begin_tui_workspace_create("test");

        assert_eq!(app.state.mode, Mode::SpacePicker);
        assert_eq!(
            app.state.workspaces.len(),
            before,
            "opening the picker must not create a space"
        );
    }

    #[tokio::test]
    async fn escape_creates_nothing_and_returns_to_the_terminal() {
        // Esc means "never mind" everywhere else in the app, and the pre-picker
        // New flow honoured it too.
        let mut app = app();
        let before = app.state.workspaces.len();
        app.begin_tui_workspace_create("test");

        app.handle_space_picker_key_event(key(KeyCode::Esc));

        assert_eq!(app.state.workspaces.len(), before);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(
            app.state.space_picker.rows.is_empty(),
            "state must be reset"
        );
    }

    #[tokio::test]
    async fn enter_on_the_first_row_reproduces_the_old_new_button() {
        // The pre-picker reflex is New then Enter. With prompting enabled that
        // has to land in the naming dialog with the same cwd it always used --
        // and still create nothing until the name is submitted.
        let mut app = app();
        app.state.prompt_new_workspace_name = true;
        let before = app.state.workspaces.len();

        app.begin_tui_workspace_create("test");
        let expected = app.state.space_picker.rows[0].path().to_path_buf();
        app.handle_space_picker_key_event(key(KeyCode::Enter));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert_eq!(
            app.state.pending_workspace_create_cwd.as_deref(),
            Some(expected.as_path())
        );
        assert_eq!(
            app.state.workspaces.len(),
            before,
            "creation still waits for the name to be submitted"
        );
    }

    #[tokio::test]
    async fn typing_filters_and_backspace_restores() {
        let mut app = app();
        app.begin_tui_workspace_create("test");
        app.state.space_picker.rows.push(SpacePickerRow::Place {
            label: "compass".into(),
            path: PathBuf::from("/repos/compass"),
            missing: false,
        });

        app.handle_space_picker_key_event(key(KeyCode::Char('z')));
        assert_eq!(app.state.space_picker.visible(), vec![0]);

        app.handle_space_picker_key_event(key(KeyCode::Backspace));
        assert_eq!(app.state.space_picker.visible().len(), 2);
    }

    #[tokio::test]
    async fn the_picker_can_be_turned_off_entirely() {
        // The escape hatch for anyone who wants the old immediate create.
        let mut app = app();
        app.state.new_opens_picker = false;
        app.state.prompt_new_workspace_name = true;

        app.begin_tui_workspace_create("test");

        assert_eq!(
            app.state.mode,
            Mode::RenameWorkspace,
            "with the picker off, New goes straight to the old prompt"
        );
    }

    #[tokio::test]
    async fn an_unreadable_store_shows_a_diagnostic_and_still_offers_new_here() {
        let mut app = app();
        std::fs::write(&app.places_path, "{ not json").unwrap();

        app.begin_tui_workspace_create("test");

        assert!(app.state.space_picker.store_error.is_some());
        assert_eq!(
            app.state.space_picker.rows.len(),
            1,
            "the New-here row must survive so the button still does something"
        );
    }
}
