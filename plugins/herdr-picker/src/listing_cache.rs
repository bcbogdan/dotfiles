use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::machine::{Machine, Sources};
use crate::model::Item;

const MAX_AGE: u64 = 24 * 60 * 60;
const MAX_HOSTS: usize = 64;
const MAX_ITEMS: usize = 1000;
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Entry {
    machine: Machine,
    saved_at: u64,
    sources: [Option<Vec<Item>>; 3],
}

#[derive(Default, Serialize, Deserialize)]
struct Document {
    version: u32,
    entries: Vec<Entry>,
}

pub struct Cache {
    path: Option<PathBuf>,
    document: Document,
    allowed: Vec<Machine>,
}

impl Cache {
    pub fn open() -> Self {
        let root = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME").map(|home| {
                            PathBuf::from(home).join(if cfg!(target_os = "macos") {
                                "Library/Caches"
                            } else {
                                ".cache"
                            })
                        })
                    })
                    .map(|root| root.join("herdr-picker"))
            });
        Self::at(root.map(|root| root.join("remote-listings-v1.json")))
    }

    fn at(path: Option<PathBuf>) -> Self {
        let document = (|| {
            let file = std::fs::File::open(path.as_ref()?).ok()?;
            if file.metadata().ok()?.len() > MAX_BYTES as u64 {
                return None;
            }
            let mut bytes = Vec::new();
            file.take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            let document: Document = serde_json::from_slice(&bytes).ok()?;
            (bytes.len() <= MAX_BYTES
                && document.version == 1
                && document.entries.len() <= MAX_HOSTS)
                .then_some(document)
        })()
        .unwrap_or_default();
        Self {
            path,
            document,
            allowed: Vec::new(),
        }
    }

    pub fn catalog(&mut self, machines: &[Machine], now: u64) {
        self.allowed = machines
            .iter()
            .filter(|machine| machine.enabled)
            .cloned()
            .collect();
        self.document
            .entries
            .retain(|entry| self.allowed.contains(&entry.machine) && valid(entry, now));
    }

    pub fn get(&self, machine: &Machine, now: u64) -> Option<Sources> {
        self.document
            .entries
            .iter()
            .find(|entry| {
                &entry.machine == machine && valid(entry, now) && self.allowed.contains(machine)
            })
            .map(|entry| {
                entry
                    .sources
                    .clone()
                    .map(|source| source.ok_or_else(|| "refreshing…".into()))
            })
    }

    pub fn update(&mut self, machine: &Machine, sources: &Sources, now: u64) {
        if !self.allowed.contains(machine) {
            return;
        }
        self.document
            .entries
            .retain(|entry| &entry.machine != machine && valid(entry, now));
        let entry = Entry {
            machine: machine.clone(),
            saved_at: now,
            sources: sources.clone().map(Result::ok),
        };
        if valid(&entry, now) && entry.sources.iter().any(Option::is_some) {
            self.document.entries.push(entry);
        }
        self.document
            .entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.saved_at));
        self.document.entries.truncate(MAX_HOSTS);
        let _ = self.save();
    }

    fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent)?;
        self.document.version = 1;
        let bytes = loop {
            let bytes = serde_json::to_vec(&self.document)?;
            if bytes.len() <= MAX_BYTES {
                break bytes;
            }
            self.document.entries.pop();
        };
        // NamedTempFile is private (0600); rename exposes only a complete document.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    }
}

fn valid(entry: &Entry, now: u64) -> bool {
    entry.machine.enabled
        && now
            .checked_sub(entry.saved_at)
            .is_some_and(|age| age <= MAX_AGE)
        && entry.sources.iter().flatten().map(Vec::len).sum::<usize>() <= MAX_ITEMS
        && entry
            .sources
            .iter()
            .flatten()
            .flatten()
            .all(|item| item.machine().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::testing::{item, machine};
    use crate::model::Target;

    fn sources() -> Sources {
        [
            Ok(vec![item(Target::Workspace { id: "w1".into() })]),
            Ok(Vec::new()),
            Err("missing zoxide".into()),
        ]
    }

    #[test]
    fn round_trip_binds_every_profile_field_and_expires() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.json");
        let host = machine();
        let mut cache = Cache::at(Some(path.clone()));
        cache.catalog(std::slice::from_ref(&host), 100);
        cache.update(&host, &sources(), 100);
        let mut cache = Cache::at(Some(path.clone()));
        assert!(cache.get(&host, 100).is_none());
        cache.catalog(std::slice::from_ref(&host), 100);
        assert_eq!(cache.get(&host, 100).unwrap()[0], sources()[0]);
        assert!(cache.get(&host, 99).is_none());
        assert!(cache.get(&host, 101 + MAX_AGE).is_none());
        for field in ["id", "label", "target", "session", "enabled"] {
            let mut changed = host.clone();
            match field {
                "id" => changed.id.push('x'),
                "label" => changed.label.push('x'),
                "target" => changed.target.push('x'),
                "session" => changed.session.push('x'),
                _ => changed.enabled = false,
            }
            let mut cache = Cache::at(Some(path.clone()));
            cache.catalog(std::slice::from_ref(&changed), 100);
            assert!(cache.get(&changed, 100).is_none(), "{field}");
            assert!(cache.get(&host, 100).is_none(), "{field}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn corruption_partial_writes_and_failures_do_not_restore_stale_data() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.json");
        for bytes in [
            "garbage",
            "{\"version\":1,\"entries\":[",
            "{\"version\":99,\"entries\":[]}",
        ] {
            std::fs::write(&path, bytes).unwrap();
            let mut cache = Cache::at(Some(path.clone()));
            cache.catalog(&[machine()], 100);
            assert!(cache.get(&machine(), 100).is_none());
        }
        let mut cache = Cache::at(Some(path.clone()));
        cache.catalog(&[machine()], 100);
        cache.update(&machine(), &sources(), 100);
        // An interrupted writer's sibling is never read as the committed cache.
        std::fs::write(temp.path().join(".tmp-interrupted"), "{").unwrap();
        let mut reopened = Cache::at(Some(path.clone()));
        reopened.catalog(&[machine()], 100);
        assert!(reopened.get(&machine(), 100).is_some());
        cache.update(
            &machine(),
            &std::array::from_fn(|_| Err("offline".into())),
            101,
        );
        let mut reopened = Cache::at(Some(path));
        reopened.catalog(&[machine()], 101);
        assert!(reopened.get(&machine(), 101).is_none());
    }

    #[test]
    fn bounds_entries_and_rejects_nested_targets_and_oversized_sources() {
        let mut cache = Cache::at(None);
        let hosts: Vec<_> = (0..MAX_HOSTS + 2)
            .map(|index| {
                let mut host = machine();
                host.id = index.to_string();
                host
            })
            .collect();
        cache.catalog(&hosts, 100);
        for host in &hosts {
            cache.update(host, &sources(), 100);
        }
        assert_eq!(cache.document.entries.len(), MAX_HOSTS);
        let nested = sources().map(|source| {
            source.map(|items| {
                items
                    .into_iter()
                    .map(|item| item.on_machine(&hosts[0]))
                    .collect()
            })
        });
        cache.update(&hosts[0], &nested, 100);
        assert!(cache.get(&hosts[0], 100).is_none());
        let large = [
            Ok(vec![
                item(Target::Workspace { id: "w1".into() });
                MAX_ITEMS + 1
            ]),
            Ok(vec![]),
            Ok(vec![]),
        ];
        cache.update(&hosts[0], &large, 100);
        assert!(cache.get(&hosts[0], 100).is_none());
        cache.catalog(&[], 100);
        cache.update(&hosts[0], &sources(), 100);
        assert!(cache.get(&hosts[0], 100).is_none());
    }
}
