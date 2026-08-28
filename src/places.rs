//! Remembered directories, independent of any workspace.
//!
//! Pinning a space used to mean two unrelated things: *remember this path* and
//! *give it a permanent sidebar row*. Places carry the first meaning on their
//! own, so unpinning stops being destructive and the sidebar can shrink to the
//! spaces actually in use.
//!
//! Stored at `config_dir()/places.json` — global to the user, deliberately not
//! under `session::data_dir()`, which is per named session and is deleted
//! wholesale when that session is deleted.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Recents are a convenience and are allowed to age out. Places never are.
pub const RECENTS_CAP: usize = 20;

const VERSION: u32 = 1;

/// Where a place came from, which decides whether it can be evicted and how a
/// later explicit add should treat it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceSource {
    /// The user asked for this one by name.
    Explicit,
    /// Kept because a pinned space was unpinned and its path would otherwise
    /// have been lost.
    PromotedFromPin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub label: String,
    pub path: PathBuf,
    pub source: PlaceSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Touched both when a space is opened at this path and when one closes,
    /// hence "used" rather than "opened".
    pub last_used: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacesFile {
    pub version: u32,
    #[serde(default)]
    pub places: Vec<Place>,
    #[serde(default)]
    pub recents: Vec<Recent>,
}

impl Default for PlacesFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            places: Vec::new(),
            recents: Vec::new(),
        }
    }
}

/// Why the store could not be used.
///
/// Carried rather than swallowed: presenting an empty list for a store we
/// failed to parse is indistinguishable from having lost the user's data, and
/// the next save would make that real.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The file exists but could not be understood. Never overwrite it.
    Unreadable { path: PathBuf, reason: String },
    /// Written by a newer herdr. Never overwrite it.
    UnsupportedVersion { path: PathBuf, found: u32 },
    /// The filesystem said no.
    Io { path: PathBuf, reason: String },
}

impl StoreError {
    pub fn path(&self) -> &Path {
        match self {
            Self::Unreadable { path, .. }
            | Self::UnsupportedVersion { path, .. }
            | Self::Io { path, .. } => path,
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Self::Unreadable { reason, .. } => reason.clone(),
            Self::UnsupportedVersion { found, .. } => {
                format!(
                    "written by a newer herdr (format {found}, this build understands {VERSION})"
                )
            }
            Self::Io { reason, .. } => reason.clone(),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path().display(), self.reason())
    }
}

// ---------------------------------------------------------------------------
// Path identity
// ---------------------------------------------------------------------------

/// The comparison key for a path.
///
/// Two entries are the same place when their keys match. Building one never
/// fails: a path that does not exist still has to have a stable identity, or a
/// place pointing at an unplugged drive would silently duplicate itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlaceKey(String);

impl PlaceKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Normalize a path into its comparison key.
pub fn normalize(path: &Path) -> PlaceKey {
    let expanded = expand(path);
    // Resolving links is only possible for something that exists. When it does
    // not, fall back to a lexical answer rather than giving up: a missing place
    // still needs to dedupe against itself.
    let resolved = std::fs::canonicalize(&expanded).unwrap_or_else(|_| lexical_absolute(&expanded));
    let text = strip_verbatim(&resolved);
    PlaceKey(normalize_case(&text))
}

/// Expand `~` and environment references. Purely textual.
pub fn expand(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let text = if let Some(rest) = text.strip_prefix("~") {
        match home_dir() {
            Some(home) if rest.is_empty() => home.to_string_lossy().into_owned(),
            Some(home) if rest.starts_with(['/', '\\']) => {
                format!("{}{}", home.to_string_lossy(), rest)
            }
            // `~other` is another user's home on Unix and simply not our
            // business; leave it alone rather than mangling it.
            _ => text.into_owned(),
        }
    } else {
        text.into_owned()
    };
    PathBuf::from(expand_env(&text))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Expand `%VAR%` (Windows) and `$VAR` / `${VAR}` (Unix shape).
fn expand_env(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '%' => {
                let name: String = chars.by_ref().take_while(|c| *c != '%').collect();
                match std::env::var(&name) {
                    Ok(value) => out.push_str(&value),
                    // An unset variable is left verbatim: turning it into an
                    // empty string would silently point the place at a
                    // different directory.
                    Err(_) => {
                        out.push('%');
                        out.push_str(&name);
                        out.push('%');
                    }
                }
            }
            '$' => {
                let braced = chars.peek() == Some(&'{');
                if braced {
                    chars.next();
                }
                let mut name = String::new();
                while let Some(c) = chars.peek() {
                    let ok = if braced {
                        *c != '}'
                    } else {
                        c.is_alphanumeric() || *c == '_'
                    };
                    if !ok {
                        break;
                    }
                    name.push(*c);
                    chars.next();
                }
                if braced {
                    chars.next();
                }
                match std::env::var(&name) {
                    Ok(value) => out.push_str(&value),
                    Err(_) => {
                        out.push('$');
                        out.push_str(&name);
                    }
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Make absolute and resolve `.` / `..` without touching the filesystem.
fn lexical_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };

    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                // Popping past the root is a no-op, matching what the kernel
                // does with `/..`.
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Strip Windows verbatim prefixes.
///
/// `\\?\UNC\server\share` has to become `\\server\share`; removing the `\\?\`
/// alone leaves the invalid `UNC\server\share`, which would never match the
/// same share written normally.
fn strip_verbatim(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text
}

/// Case-fold and upcase the drive letter on Windows; leave paths alone
/// elsewhere, where they are case-sensitive.
fn normalize_case(text: &str) -> String {
    if !cfg!(windows) {
        return text.to_string();
    }
    let mut out = text.to_lowercase();
    // A drive letter is the one part conventionally shown uppercase, and
    // upcasing it keeps keys readable in the file.
    let mut chars: Vec<char> = out.chars().collect();
    if chars.len() >= 2 && chars[1] == ':' {
        chars[0] = chars[0].to_ascii_uppercase();
        out = chars.into_iter().collect();
    }
    out
}

// ---------------------------------------------------------------------------
// Store operations
// ---------------------------------------------------------------------------

impl PlacesFile {
    /// Find a place by normalized path.
    pub fn find(&self, key: &PlaceKey) -> Option<&Place> {
        self.places.iter().find(|p| &normalize(&p.path) == key)
    }

    /// Add a place, or fold into the one already there.
    ///
    /// Idempotent on purpose. Unpinning promotes a path here, and an unpin must
    /// never be refused because the path happens to be stored already — the
    /// whole point is that the path survives.
    ///
    /// Returns whether anything changed.
    pub fn ensure_place(&mut self, label: &str, path: &Path, source: PlaceSource) -> bool {
        let key = normalize(path);
        if let Some(existing) = self.places.iter_mut().find(|p| normalize(&p.path) == key) {
            let mut changed = false;
            // An existing name is the user's; only fill a blank one.
            if existing.label.is_empty() && !label.is_empty() {
                existing.label = label.to_string();
                changed = true;
            }
            // Promotion can upgrade to Explicit, never downgrade: an explicitly
            // saved place must not become evictable because a pin was dropped.
            if source == PlaceSource::Explicit && existing.source == PlaceSource::PromotedFromPin {
                existing.source = PlaceSource::Explicit;
                changed = true;
            }
            return changed;
        }
        self.places.push(Place {
            label: label.to_string(),
            path: path.to_path_buf(),
            source,
        });
        true
    }

    /// Remove a place by normalized path. Returns whether it was there.
    pub fn remove_place(&mut self, path: &Path) -> bool {
        let key = normalize(path);
        let before = self.places.len();
        self.places.retain(|p| normalize(&p.path) != key);
        self.places.len() != before
    }

    /// Record use of a directory, most recent first, capped.
    pub fn touch_recent(&mut self, path: &Path, label: Option<&str>, now: SystemTime) {
        let key = normalize(path);
        self.recents.retain(|r| normalize(&r.path) != key);
        self.recents.insert(
            0,
            Recent {
                path: path.to_path_buf(),
                label: label.map(str::to_string),
                last_used: now,
            },
        );
        self.recents.truncate(RECENTS_CAP);
    }

    pub fn clear_recents(&mut self) {
        self.recents.clear();
    }

    /// Recents that are not already explicit places, so the picker does not
    /// show the same directory twice.
    pub fn recents_excluding_places(&self) -> Vec<&Recent> {
        let placed: HashSet<PlaceKey> = self.places.iter().map(|p| normalize(&p.path)).collect();
        self.recents
            .iter()
            .filter(|r| !placed.contains(&normalize(&r.path)))
            .collect()
    }
}

pub fn places_path() -> PathBuf {
    crate::config::config_dir().join("places.json")
}

fn lock_path(store: &Path) -> PathBuf {
    store.with_extension("json.lock")
}

/// Read the store, or say why it could not be read.
///
/// A missing file is not an error — it is an empty store, which is the state
/// every user starts in.
pub fn load(store: &Path) -> Result<PlacesFile, StoreError> {
    let text = match std::fs::read_to_string(store) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(PlacesFile::default()),
        Err(err) => {
            return Err(StoreError::Io {
                path: store.to_path_buf(),
                reason: err.to_string(),
            })
        }
    };

    // Read the version before the body: a file from a newer herdr may not even
    // parse into our shape, and reporting "corrupt" for it would be a lie.
    let probe: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| StoreError::Unreadable {
            path: store.to_path_buf(),
            reason: err.to_string(),
        })?;
    if let Some(found) = probe.get("version").and_then(serde_json::Value::as_u64) {
        if found > u64::from(VERSION) {
            return Err(StoreError::UnsupportedVersion {
                path: store.to_path_buf(),
                found: found as u32,
            });
        }
    }

    serde_json::from_str(&text).map_err(|err| StoreError::Unreadable {
        path: store.to_path_buf(),
        reason: err.to_string(),
    })
}

/// Apply a change under an exclusive lock, re-reading first.
///
/// Read-modify-write rather than writing a cached copy: more than one herdr
/// server can be live at once on the same machine, and a whole-file write from
/// a stale cache silently drops whatever the other one stored.
///
/// The closure sees the current contents and returns whether it changed
/// anything; returning false skips the write entirely.
pub fn mutate<F>(store: &Path, change: F) -> Result<PlacesFile, StoreError>
where
    F: FnOnce(&mut PlacesFile) -> bool,
{
    let _guard = LockGuard::acquire(&lock_path(store))?;
    let mut file = load(store)?;
    if change(&mut file) {
        file.version = VERSION;
        write_durably(store, &file)?;
    }
    Ok(file)
}

/// Serialize to a unique temp file, flush it to disk, then rename into place.
///
/// The unique name matters: a fixed `.tmp` is a collision between two writers
/// even though each individual rename is atomic. The `sync_all` matters
/// because a promoted place must be on disk before the unpin that depends on
/// it is reported as having succeeded.
fn write_durably(store: &Path, file: &PlacesFile) -> Result<(), StoreError> {
    let io_err = |err: io::Error| StoreError::Io {
        path: store.to_path_buf(),
        reason: err.to_string(),
    };

    if let Some(parent) = store.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let json = serde_json::to_string_pretty(file).map_err(|err| StoreError::Io {
        path: store.to_path_buf(),
        reason: err.to_string(),
    })?;

    let temp = store.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));

    let write = || -> io::Result<()> {
        let mut handle = create_private(&temp)?;
        io::Write::write_all(&mut handle, json.as_bytes())?;
        handle.sync_all()
    };
    if let Err(err) = write() {
        let _ = std::fs::remove_file(&temp);
        return Err(io_err(err));
    }

    if let Err(err) = std::fs::rename(&temp, store) {
        let _ = std::fs::remove_file(&temp);
        return Err(io_err(err));
    }

    // Best effort: not every platform lets a directory be opened and synced,
    // and failing the whole write over it would be worse than the small
    // durability gap it leaves.
    if let Some(parent) = store.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// Create a file readable only by its owner.
///
/// The store records repository names and directory layout, which is more than
/// it needs to hand to every account on the machine.
fn create_private(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// An exclusive lock held for the length of one read-modify-write.
///
/// Built on `create_new`, which is atomic on every platform we target, rather
/// than on a locking crate — the dependency would buy little here, since the
/// contention is between at most a couple of herdr processes making a
/// sub-millisecond edit.
struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    /// How long a lock file may sit before it is assumed to belong to a process
    /// that died holding it. Generous: a real hold is measured in microseconds,
    /// so anything near this is a corpse.
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
    const ATTEMPTS: u32 = 50;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

    fn acquire(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| StoreError::Io {
                path: path.to_path_buf(),
                reason: err.to_string(),
            })?;
        }

        for _ in 0..Self::ATTEMPTS {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                    })
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(path) {
                        warn!(
                            path = %path.display(),
                            "removing a places lock left behind by a dead process"
                        );
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    std::thread::sleep(Self::RETRY_DELAY);
                }
                Err(err) => {
                    return Err(StoreError::Io {
                        path: path.to_path_buf(),
                        reason: err.to_string(),
                    })
                }
            }
        }

        Err(StoreError::Io {
            path: path.to_path_buf(),
            reason: "timed out waiting for the places lock".to_string(),
        })
    }

    fn is_stale(path: &Path) -> bool {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .and_then(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .map_err(|_| io::Error::other("clock went backwards"))
            })
            .is_ok_and(|age| age > Self::STALE_AFTER)
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-places-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("places.json")
    }

    #[test]
    fn a_missing_store_is_empty_not_an_error() {
        let store = temp_store("missing");
        let file = load(&store).expect("a store that does not exist yet is simply empty");
        assert!(file.places.is_empty());
        assert!(file.recents.is_empty());
    }

    #[test]
    fn a_corrupt_store_refuses_to_load_and_is_never_overwritten() {
        // Loading an empty fallback and then saving is the one way this feature
        // could destroy a curated list, so a parse failure has to be loud.
        let store = temp_store("corrupt");
        std::fs::write(&store, "{ this is not json").unwrap();

        let err = load(&store).expect_err("a corrupt store must not load as empty");
        assert!(matches!(err, StoreError::Unreadable { .. }));

        let result = mutate(&store, |file| {
            file.ensure_place("x", Path::new("/tmp/x"), PlaceSource::Explicit)
        });
        assert!(result.is_err(), "a corrupt store must refuse mutation");
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            "{ this is not json",
            "the file must be left exactly as found"
        );
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_downgraded() {
        let store = temp_store("newer");
        std::fs::write(&store, r#"{"version":99,"places":[],"recents":[]}"#).unwrap();

        match load(&store) {
            Err(StoreError::UnsupportedVersion { found, .. }) => assert_eq!(found, 99),
            other => panic!("expected an unsupported-version refusal, got {other:?}"),
        }
        assert!(mutate(&store, |_| true).is_err());
    }

    #[test]
    fn places_survive_a_round_trip() {
        let store = temp_store("roundtrip");
        let cwd = std::env::current_dir().unwrap();

        mutate(&store, |file| {
            file.ensure_place("herdr", &cwd, PlaceSource::Explicit)
        })
        .unwrap();

        let read_back = load(&store).unwrap();
        assert_eq!(read_back.places.len(), 1);
        assert_eq!(read_back.places[0].label, "herdr");
        assert_eq!(read_back.version, VERSION);
    }

    #[test]
    fn promotion_is_idempotent_so_an_unpin_is_never_refused() {
        // Two servers unpinning the same path, or the same path unpinned twice,
        // must both leave the path stored and the unpin allowed.
        let mut file = PlacesFile::default();
        let cwd = std::env::current_dir().unwrap();

        assert!(file.ensure_place("repo", &cwd, PlaceSource::PromotedFromPin));
        assert!(!file.ensure_place("repo", &cwd, PlaceSource::PromotedFromPin));
        assert_eq!(file.places.len(), 1, "the same path must not stack up");
    }

    #[test]
    fn an_explicit_add_upgrades_a_promotion_but_never_downgrades() {
        let mut file = PlacesFile::default();
        let cwd = std::env::current_dir().unwrap();

        file.ensure_place("repo", &cwd, PlaceSource::PromotedFromPin);
        assert!(file.ensure_place("repo", &cwd, PlaceSource::Explicit));
        assert_eq!(file.places[0].source, PlaceSource::Explicit);

        file.ensure_place("repo", &cwd, PlaceSource::PromotedFromPin);
        assert_eq!(
            file.places[0].source,
            PlaceSource::Explicit,
            "a dropped pin must not make a deliberately saved place evictable"
        );
    }

    #[test]
    fn an_existing_label_is_the_users_and_is_kept() {
        let mut file = PlacesFile::default();
        let cwd = std::env::current_dir().unwrap();

        file.ensure_place("my name", &cwd, PlaceSource::Explicit);
        file.ensure_place("derived", &cwd, PlaceSource::PromotedFromPin);
        assert_eq!(file.places[0].label, "my name");
    }

    #[test]
    fn recents_are_capped_but_places_are_not() {
        let mut file = PlacesFile::default();
        let now = SystemTime::now();
        for i in 0..(RECENTS_CAP + 10) {
            let path = PathBuf::from(format!("/tmp/place-{i}"));
            file.ensure_place(&format!("p{i}"), &path, PlaceSource::Explicit);
            file.touch_recent(&path, None, now);
        }

        assert_eq!(file.recents.len(), RECENTS_CAP, "recents age out");
        assert_eq!(
            file.places.len(),
            RECENTS_CAP + 10,
            "places must never age out -- that is the promise unpinning relies on"
        );
    }

    #[test]
    fn touching_a_recent_moves_it_to_the_front_without_duplicating() {
        let mut file = PlacesFile::default();
        let now = SystemTime::now();
        let a = PathBuf::from("/tmp/a");
        let b = PathBuf::from("/tmp/b");

        file.touch_recent(&a, None, now);
        file.touch_recent(&b, None, now);
        file.touch_recent(&a, None, now);

        assert_eq!(file.recents.len(), 2);
        assert_eq!(normalize(&file.recents[0].path), normalize(&a));
    }

    #[test]
    fn a_recent_that_is_also_a_place_is_not_listed_twice() {
        let mut file = PlacesFile::default();
        let cwd = std::env::current_dir().unwrap();
        file.ensure_place("repo", &cwd, PlaceSource::Explicit);
        file.touch_recent(&cwd, None, SystemTime::now());

        assert!(file.recents_excluding_places().is_empty());
    }

    #[test]
    fn removing_a_place_matches_on_the_normalized_path() {
        let mut file = PlacesFile::default();
        let cwd = std::env::current_dir().unwrap();
        file.ensure_place("repo", &cwd, PlaceSource::Explicit);

        let noisy = cwd.join(".").join("..").join(cwd.file_name().unwrap());
        assert!(
            file.remove_place(&noisy),
            "a path spelled differently is still the same place"
        );
        assert!(file.places.is_empty());
    }

    #[test]
    fn a_missing_path_still_normalizes_to_something_stable() {
        // canonicalize() fails outright for a path that does not exist, and a
        // place on an unplugged drive must still dedupe against itself.
        let missing = if cfg!(windows) {
            PathBuf::from(r"C:\definitely\not\here\at\all")
        } else {
            PathBuf::from("/definitely/not/here/at/all")
        };
        assert_eq!(normalize(&missing), normalize(&missing));
        assert!(!normalize(&missing).as_str().is_empty());
    }

    #[test]
    fn a_relative_path_normalizes_against_the_current_directory() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(normalize(Path::new(".")), normalize(&cwd));
    }

    #[test]
    fn dot_segments_are_resolved_even_when_the_path_is_missing() {
        let base = if cfg!(windows) {
            r"C:\nope\a\..\b"
        } else {
            "/nope/a/../b"
        };
        let flat = if cfg!(windows) {
            r"C:\nope\b"
        } else {
            "/nope/b"
        };
        assert_eq!(normalize(Path::new(base)), normalize(Path::new(flat)));
    }

    #[test]
    fn a_verbatim_unc_prefix_becomes_an_ordinary_unc_path() {
        // Stripping `\\?\` alone would leave `UNC\server\share`, which is not a
        // path at all and would never match the same share written normally.
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\server\share\dir")),
            r"\\server\share\dir"
        );
        assert_eq!(strip_verbatim(Path::new(r"\\?\C:\dir")), r"C:\dir");
        assert_eq!(
            strip_verbatim(Path::new(r"\\server\share")),
            r"\\server\share"
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_paths_compare_case_insensitively_with_an_upper_drive_letter() {
        let lower = normalize(Path::new(r"c:\Windows\Temp"));
        let upper = normalize(Path::new(r"C:\WINDOWS\temp"));
        assert_eq!(lower, upper);
        assert!(lower.as_str().starts_with("C:"));
    }

    #[test]
    fn a_tilde_expands_to_the_home_directory() {
        let Some(home) = home_dir() else {
            return;
        };
        assert_eq!(normalize(Path::new("~")), normalize(&home));
    }

    #[test]
    fn an_unset_environment_reference_is_left_alone_rather_than_emptied() {
        // Expanding to nothing would quietly point the place at a different
        // directory, which is worse than leaving a path that visibly fails.
        let text = expand_env("%HERDR_DEFINITELY_UNSET_VAR%/x");
        assert!(text.starts_with("%HERDR_DEFINITELY_UNSET_VAR%"), "{text}");
    }

    #[test]
    fn a_second_writer_does_not_lose_the_first_writers_entry() {
        // The condition this machine actually reaches: two live herdr servers.
        // A cached whole-file write would drop one of these silently.
        let store = temp_store("concurrent");
        let a = PathBuf::from("/tmp/writer-a");
        let b = PathBuf::from("/tmp/writer-b");

        mutate(&store, |file| {
            file.ensure_place("a", &a, PlaceSource::Explicit)
        })
        .unwrap();
        mutate(&store, |file| {
            file.ensure_place("b", &b, PlaceSource::Explicit)
        })
        .unwrap();

        let read_back = load(&store).unwrap();
        assert_eq!(read_back.places.len(), 2, "neither writer may be lost");
    }

    #[test]
    fn the_lock_is_released_when_the_mutation_finishes() {
        let store = temp_store("lock-release");
        mutate(&store, |_| false).unwrap();
        assert!(
            !lock_path(&store).exists(),
            "a held lock would deadlock every later write"
        );
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_taken_over() {
        let store = temp_store("stale-lock");
        let lock = lock_path(&store);
        std::fs::write(&lock, "").unwrap();
        let old = SystemTime::now() - LockGuard::STALE_AFTER - std::time::Duration::from_secs(60);
        // A crashed herdr leaves this behind; without takeover, places would be
        // permanently unwritable until someone deleted the file by hand.
        filetime_set(&lock, old);

        let result = mutate(&store, |file| {
            file.ensure_place("x", Path::new("/tmp/x"), PlaceSource::Explicit)
        });
        assert!(result.is_ok(), "a stale lock must not wedge the store");
    }

    /// Backdate a file so the stale-lock path can be tested without sleeping.
    fn filetime_set(path: &Path, when: SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn a_write_leaves_no_temp_files_behind() {
        let store = temp_store("no-temp");
        mutate(&store, |file| {
            file.ensure_place("x", Path::new("/tmp/x"), PlaceSource::Explicit)
        })
        .unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(store.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    #[test]
    #[cfg(unix)]
    fn the_store_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let store = temp_store("perms");
        mutate(&store, |file| {
            file.ensure_place("x", Path::new("/tmp/x"), PlaceSource::Explicit)
        })
        .unwrap();
        let mode = std::fs::metadata(&store).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode was {mode:o}");
    }
}
