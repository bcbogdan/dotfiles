use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::model::{HistoryKey, Item};

const VERSION: u8 = 1;
// Retain six months of inactive history, capped at 2,048; visible items are exempt.
const MAX_ENTRIES: usize = 2_048;
const STALE_AFTER_SECS: u64 = 180 * 24 * 60 * 60;
const MAX_FUTURE_SKEW_SECS: u64 = 24 * 60 * 60;
const STALE_TEMP_SECS: u64 = 24 * 60 * 60;
const TEMP_PREFIX: &str = ".picker-history.tmp-";
static UNIQUE_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    entries: HashMap<HistoryKey, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    first_seen: u64,
    last_seen: u64,
    last_accessed: Option<u64>,
}

impl History {
    pub fn observe<'a>(&mut self, items: impl IntoIterator<Item = &'a Item>, now: u64) -> bool {
        let mut changed = false;
        for item in items {
            let key = item.history_key();
            if let Some(entry) = self.entries.get_mut(&key) {
                let next = entry.last_seen.max(now);
                if entry.last_seen != next {
                    entry.last_seen = next;
                    changed = true;
                }
            } else {
                self.entries.insert(
                    key,
                    Entry {
                        first_seen: now,
                        last_seen: now,
                        last_accessed: None,
                    },
                );
                changed = true;
            }
        }
        changed
    }

    pub fn access(&mut self, key: &HistoryKey, now: u64) {
        let entry = self.entries.entry(key.clone()).or_insert(Entry {
            first_seen: now,
            last_seen: now,
            last_accessed: None,
        });
        entry.last_seen = entry.last_seen.max(now);
        entry.last_accessed = Some(entry.last_accessed.map_or(now, |value| value.max(now)));
    }

    pub fn first_seen(&self, key: &HistoryKey) -> u64 {
        self.entries.get(key).map_or(0, |entry| entry.first_seen)
    }

    pub fn last_accessed(&self, key: &HistoryKey) -> Option<u64> {
        self.entries.get(key).and_then(|entry| entry.last_accessed)
    }

    pub fn prune(&mut self, visible: &HashSet<HistoryKey>, now: u64) -> bool {
        let original_len = self.entries.len();
        let cutoff = now.saturating_sub(STALE_AFTER_SECS);
        self.entries
            .retain(|key, entry| visible.contains(key) || entry.last_seen >= cutoff);
        if self.entries.len() > MAX_ENTRIES {
            let mut removable: Vec<_> = self
                .entries
                .iter()
                .filter(|(key, _)| !visible.contains(*key))
                .map(|(key, entry)| (key.clone(), entry.last_seen))
                .collect();
            removable.sort_by_key(|(_, last_seen)| *last_seen);
            let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
            for (key, _) in removable.into_iter().take(excess) {
                self.entries.remove(&key);
            }
        }
        self.entries.len() != original_len
    }

    pub fn merge(&mut self, other: &Self) {
        for (key, incoming) in &other.entries {
            match self.entries.get_mut(key) {
                Some(existing) => {
                    existing.first_seen = existing.first_seen.min(incoming.first_seen);
                    existing.last_seen = existing.last_seen.max(incoming.last_seen);
                    existing.last_accessed = match (existing.last_accessed, incoming.last_accessed)
                    {
                        (Some(left), Some(right)) => Some(left.max(right)),
                        (left, right) => left.or(right),
                    };
                }
                None => {
                    self.entries.insert(key.clone(), incoming.clone());
                }
            }
        }
    }

    fn normalize_import(&mut self, now: u64, trusted_high_water: u64) {
        let ceiling = now
            .saturating_add(MAX_FUTURE_SKEW_SECS)
            .max(trusted_high_water);
        for entry in self.entries.values_mut() {
            entry.first_seen = entry.first_seen.min(ceiling);
            entry.last_seen = entry.last_seen.min(ceiling).max(entry.first_seen);
            entry.last_accessed = entry
                .last_accessed
                .map(|value| value.min(ceiling).max(entry.first_seen));
        }
    }

    fn max_timestamp(&self) -> u64 {
        self.entries.values().fold(0, |maximum, entry| {
            maximum
                .max(entry.first_seen)
                .max(entry.last_seen)
                .max(entry.last_accessed.unwrap_or(0))
        })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u8,
    #[serde(default)]
    high_water: u64,
    entries: Vec<PersistedEntry>,
}

#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    key: HistoryKey,
    first_seen: u64,
    last_seen: u64,
    last_accessed: Option<u64>,
}

impl Document {
    fn from_history(history: &History, high_water: u64) -> Self {
        Self {
            version: VERSION,
            high_water,
            entries: history
                .entries
                .iter()
                .map(|(key, entry)| PersistedEntry {
                    key: key.clone(),
                    first_seen: entry.first_seen,
                    last_seen: entry.last_seen,
                    last_accessed: entry.last_accessed,
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
pub struct LoadOutcome {
    pub history: History,
    pub warning: Option<String>,
    pub logical_time: u64,
}

#[derive(Debug)]
pub struct SaveOutcome {
    pub history: History,
    pub warning: Option<String>,
}

pub trait Store {
    fn load(&self) -> Result<LoadOutcome, String>;
    fn save(
        &self,
        history: &History,
        visible: &HashSet<HistoryKey>,
        now: u64,
    ) -> Result<SaveOutcome, String>;
}

#[derive(Clone)]
pub struct FileStore {
    path: Option<PathBuf>,
}

impl FileStore {
    pub fn from_env() -> Self {
        Self {
            path: std::env::var_os("HERDR_PLUGIN_STATE_DIR")
                .map(PathBuf::from)
                .map(|directory| directory.join("picker-history.json")),
        }
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn locked<T>(
        &self,
        operation: impl FnOnce(&Path, &Path) -> Result<T, String>,
    ) -> Result<T, String> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| "history is memory-only".to_string())?;
        let parent = path
            .parent()
            .ok_or_else(|| "history path has no parent".to_string())?;
        secure_directory(parent)?;
        let lock_path = parent.join(".picker-history.lock");
        let lock = secure_open(&lock_path, false)?;
        secure_file_permissions(&lock_path)?;
        lock.lock_exclusive().map_err(|error| error.to_string())?;
        cleanup_stale_temps(parent);
        operation(path, parent)
    }
}

impl Store for FileStore {
    fn load(&self) -> Result<LoadOutcome, String> {
        if self.path.is_none() {
            return Ok(LoadOutcome {
                history: History::default(),
                warning: None,
                logical_time: 0,
            });
        }
        self.locked(|path, parent| load_disk(path, parent, unix_time()))
    }

    fn save(
        &self,
        history: &History,
        visible: &HashSet<HistoryKey>,
        now: u64,
    ) -> Result<SaveOutcome, String> {
        if self.path.is_none() {
            let mut history = history.clone();
            history.prune(visible, now);
            return Ok(SaveOutcome {
                history,
                warning: None,
            });
        }
        self.locked(|path, parent| {
            let loaded = load_disk(path, parent, now)?;
            let mut merged = loaded.history;
            merged.merge(history);
            merged.prune(visible, now);
            let logical_time = loaded.logical_time.max(now).max(merged.max_timestamp());
            write_atomic(path, parent, &merged, logical_time)?;
            Ok(SaveOutcome {
                history: merged,
                warning: loaded.warning,
            })
        })
    }
}

fn load_disk(path: &Path, parent: &Path, now: u64) -> Result<LoadOutcome, String> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadOutcome {
                history: History::default(),
                warning: None,
                logical_time: 0,
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    let parsed = serde_json::from_slice::<Document>(&raw)
        .map_err(|error| error.to_string())
        .and_then(|document| {
            if document.version == VERSION {
                Ok(document)
            } else {
                Err(format!("unsupported history version {}", document.version))
            }
        });
    match parsed {
        Ok(document) => {
            let mut history = History::default();
            for persisted in document.entries {
                let incoming = History {
                    entries: HashMap::from([(
                        persisted.key,
                        Entry {
                            first_seen: persisted.first_seen,
                            last_seen: persisted.last_seen,
                            last_accessed: persisted.last_accessed,
                        },
                    )]),
                };
                history.merge(&incoming);
            }
            history.normalize_import(now, document.high_water);
            let logical_time = document.high_water.max(history.max_timestamp());
            Ok(LoadOutcome {
                history,
                warning: None,
                logical_time,
            })
        }
        Err(error) => {
            let quarantine = unique_path(parent, ".picker-history.corrupt-");
            fs::rename(path, &quarantine).map_err(|rename| {
                format!("history is corrupt ({error}); quarantine failed: {rename}")
            })?;
            secure_file_permissions(&quarantine)?;
            sync_directory(parent)?;
            Ok(LoadOutcome {
                history: History::default(),
                warning: Some(format!(
                    "history reset; corrupt file moved to {}",
                    quarantine.file_name().unwrap_or_default().to_string_lossy()
                )),
                logical_time: 0,
            })
        }
    }
}

fn write_atomic(
    path: &Path,
    parent: &Path,
    history: &History,
    logical_time: u64,
) -> Result<(), String> {
    let temporary = unique_path(parent, TEMP_PREFIX);
    let result = (|| {
        let mut file = secure_open(&temporary, true)?;
        serde_json::to_writer(&mut file, &Document::from_history(history, logical_time))
            .map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        secure_file_permissions(path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn unique_path(parent: &Path, prefix: &str) -> PathBuf {
    let counter = UNIQUE_FILE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!("{prefix}{}-{nanos}-{counter}", std::process::id()))
}

fn cleanup_stale_temps(parent: &Path) {
    let now = std::time::SystemTime::now();
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten().take(256) {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(TEMP_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() >= STALE_TEMP_SECS);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|error| error.to_string())?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn secure_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn secure_open(path: &Path, create_new: bool) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create(!create_new)
        .create_new(create_new)
        .mode(0o600)
        .open(path)
        .map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn secure_open(path: &Path, create_new: bool) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(!create_new)
        .create_new(create_new)
        .open(path)
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn secure_file_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn secure_file_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

pub fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Tab, Target};
    use std::sync::{Arc, Barrier, Mutex};

    fn item(id: &str) -> Item {
        Item {
            id: id.into(),
            label: id.into(),
            detail: String::new(),
            search: id.into(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: Target::Workspace { id: id.into() },
        }
    }

    fn visible(items: &[Item]) -> HashSet<HistoryKey> {
        items.iter().map(Item::history_key).collect()
    }

    #[test]
    fn first_seen_and_access_are_monotonic_across_clock_rollback() {
        let selected = item("w1");
        let key = selected.history_key();
        let mut history = History::default();
        history.observe([&selected], 100);
        history.observe([&selected], 50);
        history.access(&key, 120);
        history.access(&key, 60);
        assert_eq!(history.first_seen(&key), 100);
        assert_eq!(history.last_accessed(&key), Some(120));
    }

    #[test]
    fn unreasonable_future_timestamps_are_clamped() {
        let selected = item("w1");
        let key = selected.history_key();
        let mut history = History::default();
        history.observe([&selected], u64::MAX);
        history.access(&key, u64::MAX);
        history.normalize_import(100, 0);
        assert_eq!(history.first_seen(&key), 100 + MAX_FUTURE_SKEW_SECS);
        assert_eq!(
            history.last_accessed(&key),
            Some(100 + MAX_FUTURE_SKEW_SECS)
        );
    }

    #[test]
    fn atomic_file_roundtrip_replaces_existing_state() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("picker-history.json");
        let store = FileStore::at(path.clone());
        let selected = item("w1");
        let key = selected.history_key();
        let mut history = History::default();
        history.observe([&selected], 10);
        let outcome = store
            .save(&history, &visible(std::slice::from_ref(&selected)), 10)
            .unwrap();
        assert_eq!(store.load().unwrap().history, outcome.history);

        history.access(&key, 20);
        store.save(&history, &visible(&[selected]), 20).unwrap();
        assert_eq!(store.load().unwrap().history.last_accessed(&key), Some(20));
        assert!(fs::read_dir(temporary.path())
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX)));
    }

    #[test]
    fn load_save_roundtrip_preserves_ordering_timestamps_after_large_rollback() {
        use crate::model::{App, SortOrder};

        let temporary = tempfile::tempdir().unwrap();
        let store = FileStore::at(temporary.path().join("picker-history.json"));
        let items = [item("older"), item("newer")];
        let older_key = items[0].history_key();
        let newer_key = items[1].history_key();
        let mut history = History::default();
        history.observe([&items[0]], 1_999_900);
        history.observe([&items[1]], 1_999_950);
        history.access(&older_key, 2_000_000);
        history.access(&newer_key, 1_999_990);
        let current = visible(&items);
        store.save(&history, &current, 2_000_000).unwrap();

        let rollback_now = 2_000_000 - (3 * 24 * 60 * 60);
        let loaded = store.load().unwrap().history;
        let outcome = store.save(&loaded, &current, rollback_now).unwrap();
        assert_eq!(outcome.history.first_seen(&older_key), 1_999_900);
        assert_eq!(outcome.history.first_seen(&newer_key), 1_999_950);
        assert_eq!(outcome.history.last_accessed(&older_key), Some(2_000_000));
        assert_eq!(outcome.history.last_accessed(&newer_key), Some(1_999_990));

        let mut app = App::new(Tab::Workspaces);
        app.set_history(outcome.history);
        app.set_items(Tab::Workspaces, items.to_vec());
        assert_eq!(app.filtered()[0].id, "older");
        app.state_mut().sort = SortOrder::AgeAscending;
        assert_eq!(app.filtered()[0].id, "newer");
    }

    #[test]
    fn concurrent_stores_merge_confirmations_without_lost_updates() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("picker-history.json");
        let stores = [FileStore::at(path.clone()), FileStore::at(path.clone())];
        let items = [item("w1"), item("w2")];
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for (index, store) in stores.into_iter().enumerate() {
            let barrier = Arc::clone(&barrier);
            let selected = items[index].clone();
            threads.push(std::thread::spawn(move || {
                let mut history = History::default();
                history.observe([&selected], 10);
                history.access(&selected.history_key(), 20 + index as u64);
                barrier.wait();
                store.save(&history, &visible(&[selected]), 30).unwrap();
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        let history = FileStore::at(path).load().unwrap().history;
        assert_eq!(history.last_accessed(&items[0].history_key()), Some(20));
        assert_eq!(history.last_accessed(&items[1].history_key()), Some(21));
    }

    #[test]
    fn stale_refresh_save_cannot_erase_a_confirmation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("picker-history.json");
        let store = FileStore::at(path);
        let selected = item("w1");
        let key = selected.history_key();
        let mut stale_refresh = History::default();
        stale_refresh.observe([&selected], 10);
        let mut confirmation = stale_refresh.clone();
        confirmation.access(&key, 30);
        let current = visible(std::slice::from_ref(&selected));
        store.save(&confirmation, &current, 30).unwrap();
        store.save(&stale_refresh, &current, 40).unwrap();
        let history = store.load().unwrap().history;
        assert_eq!(history.first_seen(&key), 10);
        assert_eq!(history.last_accessed(&key), Some(30));
    }

    #[test]
    fn corrupt_and_version_mismatched_files_are_quarantined() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("picker-history.json");
        let store = FileStore::at(path.clone());
        for raw in ["not json", r#"{"version":99,"entries":[]}"#] {
            fs::write(&path, raw).unwrap();
            let outcome = store.load().unwrap();
            assert!(outcome.warning.is_some());
            assert!(outcome.history.entries.is_empty());
            assert!(!path.exists());
        }
        let quarantined = fs::read_dir(temporary.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(quarantined, 2);
    }

    #[test]
    fn pruning_is_bounded_and_retains_currently_visible_items() {
        let mut history = History::default();
        let mut all = Vec::new();
        for index in 0..(MAX_ENTRIES + 100) {
            all.push(item(&format!("old-{index}")));
        }
        history.observe(&all, 1);
        let current = HashSet::from([all[0].history_key()]);
        history.prune(&current, STALE_AFTER_SECS + 2);
        assert_eq!(history.len(), 1);
        assert_eq!(history.first_seen(&all[0].history_key()), 1);
    }

    #[derive(Default)]
    struct FakeStore {
        saved: Mutex<Vec<History>>,
    }

    impl Store for FakeStore {
        fn load(&self) -> Result<LoadOutcome, String> {
            Ok(LoadOutcome {
                history: self
                    .saved
                    .lock()
                    .unwrap()
                    .last()
                    .cloned()
                    .unwrap_or_default(),
                warning: None,
                logical_time: 0,
            })
        }

        fn save(
            &self,
            history: &History,
            _visible: &HashSet<HistoryKey>,
            _now: u64,
        ) -> Result<SaveOutcome, String> {
            self.saved.lock().unwrap().push(history.clone());
            Ok(SaveOutcome {
                history: history.clone(),
                warning: None,
            })
        }
    }

    #[test]
    fn store_seam_supports_fake_roundtrips() {
        let store = FakeStore::default();
        let selected = item(Tab::Workspaces.label());
        let mut history = History::default();
        history.observe([&selected], 1);
        store.save(&history, &visible(&[selected]), 1).unwrap();
        assert_eq!(store.load().unwrap().history, history);
    }

    #[cfg(unix)]
    #[test]
    fn state_files_and_directory_use_private_modes() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        let path = state.join("picker-history.json");
        let store = FileStore::at(path.clone());
        store
            .save(&History::default(), &HashSet::new(), unix_time())
            .unwrap();
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(state.join(".picker-history.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
