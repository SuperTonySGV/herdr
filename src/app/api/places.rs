use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::api::schema::{
    PlaceAddParams, PlaceInfo, PlaceRemoveParams, PlaceSourceInfo, RecentPlaceInfo, ResponseResult,
};
use crate::app::App;
use crate::places::{self, PlaceSource, PlacesFile, StoreError};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_place_list(&mut self, id: String) -> String {
        // Read through rather than from a cache: more than one herdr server can
        // be live, and a cached list would show this one's stale idea of the
        // store indefinitely.
        match places::load(&self.places_path) {
            Ok(file) => encode_success(id, place_list_result(&file)),
            Err(err) => store_unavailable(id, &err),
        }
    }

    pub(super) fn handle_place_add(&mut self, id: String, params: PlaceAddParams) -> String {
        let path = PathBuf::from(&params.path);
        if let Some(response) = reject_relative(&id, &path) {
            return response;
        }

        let label = params
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| crate::workspace::derive_label_from_cwd(&path));

        // An explicit add over an entry that is already Explicit is a duplicate
        // and is reported as one. Over a promoted entry it is an upgrade, so it
        // succeeds -- refusing it would leave a deliberately saved place still
        // marked as merely inherited from a pin.
        let already_explicit = match places::load(&self.places_path) {
            Ok(file) => file
                .find(&places::normalize(&path))
                .is_some_and(|place| place.source == PlaceSource::Explicit),
            Err(err) => return store_unavailable(id, &err),
        };
        if already_explicit {
            return encode_error(
                id,
                "duplicate_place",
                format!("{} is already saved", path.display()),
            );
        }

        let store = self.places_path.clone();
        match places::mutate(&store, |file| {
            file.ensure_place(&label, &path, PlaceSource::Explicit)
        }) {
            Ok(file) => encode_success(id, place_list_result(&file)),
            Err(err) => store_unavailable(id, &err),
        }
    }

    pub(super) fn handle_place_remove(&mut self, id: String, params: PlaceRemoveParams) -> String {
        let path = PathBuf::from(&params.path);
        if let Some(response) = reject_relative(&id, &path) {
            return response;
        }

        let mut found = false;
        let store = self.places_path.clone();
        match places::mutate(&store, |file| {
            found = file.remove_place(&path);
            found
        }) {
            Ok(file) => {
                if found {
                    encode_success(id, place_list_result(&file))
                } else {
                    encode_error(
                        id,
                        "place_not_found",
                        format!("{} is not saved", path.display()),
                    )
                }
            }
            Err(err) => store_unavailable(id, &err),
        }
    }

    pub(super) fn handle_place_clear_recents(&mut self, id: String) -> String {
        let store = self.places_path.clone();
        match places::mutate(&store, |file| {
            let had_any = !file.recents.is_empty();
            file.clear_recents();
            had_any
        }) {
            Ok(file) => encode_success(id, place_list_result(&file)),
            Err(err) => store_unavailable(id, &err),
        }
    }

    /// Remember a directory a space was opened at or closed from.
    ///
    /// Best effort by design: the space already exists by the time this runs,
    /// so failing the request the caller made would be worse than losing one
    /// recents entry -- a retried create would make a second workspace.
    pub(crate) fn record_recent_place(&mut self, path: &Path, label: Option<&str>) {
        if !self.state.remember_recent_places {
            return;
        }
        let now = SystemTime::now();
        let store = self.places_path.clone();
        if let Err(err) = places::mutate(&store, |file| {
            file.touch_recent(path, label, now);
            true
        }) {
            // Logged rather than surfaced: there is no general-purpose warning
            // toast in this build, and the picker shows a store diagnostic of
            // its own when the store is unusable.
            tracing::warn!(err = %err, path = %path.display(), "could not record a recent place");
        }
    }

    /// Store a path so unpinning cannot lose it.
    ///
    /// Strict, unlike recents: the caller uses the result to decide whether the
    /// unpin may proceed at all.
    pub(crate) fn promote_place_for_unpin(
        &mut self,
        path: &Path,
        label: &str,
    ) -> Result<(), StoreError> {
        let store = self.places_path.clone();
        places::mutate(&store, |file| {
            file.ensure_place(label, path, PlaceSource::PromotedFromPin)
        })
        .map(|_| ())
    }
}

/// The server has no defensible directory to resolve a relative path against:
/// it long outlives whatever shell the caller is in.
fn reject_relative(id: &str, path: &Path) -> Option<String> {
    (!path.is_absolute()).then(|| {
        encode_error(
            id.to_string(),
            "path_not_absolute",
            format!(
                "{} is relative; expand it before calling, as the server has no \
                 caller's working directory to resolve it against",
                path.display()
            ),
        )
    })
}

fn store_unavailable(id: String, err: &StoreError) -> String {
    encode_error(
        id,
        "store_unavailable",
        format!(
            "{} could not be used and was left untouched: {}",
            err.path().display(),
            err.reason()
        ),
    )
}

fn place_list_result(file: &PlacesFile) -> ResponseResult {
    ResponseResult::PlaceList {
        places: file
            .places
            .iter()
            .map(|place| PlaceInfo {
                label: place.label.clone(),
                path: place.path.display().to_string(),
                source: match place.source {
                    PlaceSource::Explicit => PlaceSourceInfo::Explicit,
                    PlaceSource::PromotedFromPin => PlaceSourceInfo::PromotedFromPin,
                },
                missing: !place.path.is_dir(),
            })
            .collect(),
        recents: file
            .recents_excluding_places()
            .into_iter()
            .map(|recent| RecentPlaceInfo {
                path: recent.path.display().to_string(),
                label: recent.label.clone(),
                last_used_unix: recent
                    .last_used
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or_default(),
                missing: !recent.path.is_dir(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{EmptyParams, Method, Request};
    use crate::app::App;
    use crate::config::Config;
    use crate::workspace::Workspace;

    /// An App whose store is a throwaway file, so tests never read or write the
    /// real user's places.
    fn app_with_store() -> (App, PathBuf) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let dir = std::env::temp_dir().join(format!(
            "herdr-api-places-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("places.json");
        app.places_path = store.clone();
        (app, store)
    }

    fn call(app: &mut App, method: Method) -> String {
        app.handle_api_request(Request {
            id: "req".into(),
            method,
        })
    }

    fn absolute_dir() -> String {
        std::env::current_dir().unwrap().display().to_string()
    }

    #[tokio::test]
    async fn a_place_can_be_added_listed_and_removed() {
        let (mut app, _store) = app_with_store();
        let path = absolute_dir();

        let added = call(
            &mut app,
            Method::PlaceAdd(PlaceAddParams {
                path: path.clone(),
                label: Some("herdr".into()),
            }),
        );
        assert!(added.contains("herdr"), "{added}");

        let listed = call(&mut app, Method::PlaceList(EmptyParams::default()));
        assert!(listed.contains("herdr"), "{listed}");

        let removed = call(
            &mut app,
            Method::PlaceRemove(PlaceRemoveParams { path: path.clone() }),
        );
        assert!(!removed.contains("\"error\""), "{removed}");

        let listed = call(&mut app, Method::PlaceList(EmptyParams::default()));
        assert!(!listed.contains("herdr"), "{listed}");
    }

    #[tokio::test]
    async fn a_relative_path_is_refused_because_the_server_has_no_caller_cwd() {
        // The server long outlives whatever shell asked, so resolving against
        // its own working directory would silently pick a different place.
        let (mut app, _store) = app_with_store();

        let response = call(
            &mut app,
            Method::PlaceAdd(PlaceAddParams {
                path: "relative/path".into(),
                label: None,
            }),
        );
        assert!(response.contains("path_not_absolute"), "{response}");
    }

    #[tokio::test]
    async fn adding_the_same_place_twice_is_reported_as_a_duplicate() {
        let (mut app, _store) = app_with_store();
        let path = absolute_dir();

        call(
            &mut app,
            Method::PlaceAdd(PlaceAddParams {
                path: path.clone(),
                label: None,
            }),
        );
        let again = call(
            &mut app,
            Method::PlaceAdd(PlaceAddParams { path, label: None }),
        );
        assert!(again.contains("duplicate_place"), "{again}");
    }

    #[tokio::test]
    async fn an_explicit_add_over_a_promoted_place_upgrades_it_rather_than_failing() {
        // Duplicate is about two deliberate saves. A promoted entry is not one,
        // so saving it deliberately has to be allowed to stick.
        let (mut app, store) = app_with_store();
        let path = std::env::current_dir().unwrap();
        app.promote_place_for_unpin(&path, "repo").unwrap();

        let response = call(
            &mut app,
            Method::PlaceAdd(PlaceAddParams {
                path: path.display().to_string(),
                label: Some("repo".into()),
            }),
        );
        assert!(!response.contains("duplicate_place"), "{response}");
        let file = places::load(&store).unwrap();
        assert_eq!(file.places[0].source, PlaceSource::Explicit);
    }

    #[tokio::test]
    async fn a_corrupt_store_reports_itself_instead_of_looking_empty() {
        // An empty list for a store we could not parse is indistinguishable
        // from having lost everything in it.
        let (mut app, store) = app_with_store();
        std::fs::write(&store, "{ not json").unwrap();

        let response = call(&mut app, Method::PlaceList(EmptyParams::default()));
        assert!(response.contains("store_unavailable"), "{response}");
        assert_eq!(std::fs::read_to_string(&store).unwrap(), "{ not json");
    }

    #[tokio::test]
    async fn unpinning_stores_the_path_before_dropping_the_pin() {
        let (mut app, store) = app_with_store();
        app.state.workspaces = vec![Workspace::test_new("repo")];
        app.state.ensure_test_terminals();
        let workspace_id = app.public_workspace_id(0);

        let response = call(
            &mut app,
            Method::WorkspaceSetPinned(crate::api::schema::WorkspaceSetPinnedParams {
                workspace_id,
                pinned: false,
            }),
        );

        assert!(!response.contains("\"error\""), "{response}");
        let file = places::load(&store).unwrap();
        assert_eq!(
            file.places.len(),
            1,
            "the path must be stored before the pin that was keeping it goes"
        );
    }

    #[tokio::test]
    async fn an_unpin_is_refused_when_the_path_cannot_be_stored() {
        // Reporting success while silently dropping the path is the one
        // outcome this whole feature exists to prevent.
        let (mut app, store) = app_with_store();
        std::fs::write(&store, "{ not json").unwrap();
        app.state.workspaces = vec![Workspace::test_new("repo")];
        app.state.ensure_test_terminals();
        app.state.workspaces[0].pinned = true;
        let workspace_id = app.public_workspace_id(0);

        let response = call(
            &mut app,
            Method::WorkspaceSetPinned(crate::api::schema::WorkspaceSetPinnedParams {
                workspace_id,
                pinned: false,
            }),
        );

        assert!(response.contains("store_unavailable"), "{response}");
        assert!(
            app.state.workspaces[0].pinned,
            "the space must stay pinned rather than lose its path"
        );
    }

    #[tokio::test]
    async fn recents_are_not_written_when_the_user_turned_them_off() {
        let (mut app, store) = app_with_store();
        app.state.remember_recent_places = false;

        app.record_recent_place(&std::env::current_dir().unwrap(), None);

        assert!(
            places::load(&store).unwrap().recents.is_empty(),
            "remember_recents = false must write nothing at all"
        );
    }
}
