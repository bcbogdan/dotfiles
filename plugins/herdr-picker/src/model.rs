use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::history::History;
use crate::profile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Workspaces,
    Agents,
    Zoxide,
}

impl Tab {
    pub const ALL: [Self; 3] = [Self::Workspaces, Self::Agents, Self::Zoxide];

    pub fn index(self) -> usize {
        match self {
            Self::Workspaces => 0,
            Self::Agents => 1,
            Self::Zoxide => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Workspaces => "Workspaces",
            Self::Agents => "Agents",
            Self::Zoxide => "Zoxide",
        }
    }

    pub fn adjacent(self, delta: isize) -> Self {
        let index = (self.index().cast_signed() + delta)
            .rem_euclid(3)
            .cast_unsigned();
        Self::ALL[index]
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    Recent,
    AgeAscending,
    AgeDescending,
}

impl SortOrder {
    pub fn next(self) -> Self {
        match self {
            Self::Recent => Self::AgeAscending,
            Self::AgeAscending => Self::AgeDescending,
            Self::AgeDescending => Self::Recent,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::AgeAscending => "Age ↑",
            Self::AgeDescending => "Age ↓",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    Remote {
        machine: crate::machine::Machine,
        target: Box<Target>,
    },
    Workspace {
        id: String,
    },
    Agent {
        pane_id: String,
    },
    Directory {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HistoryKey {
    Remote {
        namespace: String,
        key: Box<HistoryKey>,
    },
    Workspace(String),
    Agent(String),
    Directory(PathBuf),
}

impl HistoryKey {
    pub fn workspace(id: impl Into<String>) -> Self {
        Self::Workspace(id.into())
    }

    pub fn agent(pane_id: impl Into<String>) -> Self {
        Self::Agent(pane_id.into())
    }

    pub fn directory(path: &Path) -> Self {
        Self::Directory(canonicalish(path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub search: String,
    pub preview_pane: Option<String>,
    pub match_paths: Vec<PathBuf>,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewIdentity {
    pub item_id: String,
    pub preview_pane: Option<String>,
    pub target: Target,
}

impl Item {
    pub fn machine(&self) -> Option<&crate::machine::Machine> {
        self.target.machine()
    }

    pub fn machine_label(&self) -> String {
        self.machine()
            .map_or_else(crate::machine::local_label, |machine| machine.label.clone())
    }

    pub fn on_machine(mut self, machine: &crate::machine::Machine) -> Self {
        self.id = serde_json::to_string(&(machine.namespace(), &self.id))
            .expect("identity serialization");
        self.target = Target::Remote {
            machine: machine.clone(),
            target: Box::new(self.target),
        };
        self
    }

    pub fn preview_identity(&self) -> PreviewIdentity {
        PreviewIdentity {
            item_id: self.id.clone(),
            preview_pane: self.preview_pane.clone(),
            target: self.target.clone(),
        }
    }

    pub fn history_key(&self) -> HistoryKey {
        self.target.history_key()
    }
}

impl Target {
    pub fn machine(&self) -> Option<&crate::machine::Machine> {
        match self {
            Self::Remote { machine, .. } => Some(machine),
            _ => None,
        }
    }

    pub fn inner(&self) -> &Self {
        match self {
            Self::Remote { target, .. } => target,
            _ => self,
        }
    }

    pub fn history_key(&self) -> HistoryKey {
        match self {
            Target::Remote { machine, target } => {
                let key = match target.as_ref() {
                    Target::Directory { path } => HistoryKey::Directory(path.clone()),
                    _ => target.history_key(),
                };
                HistoryKey::Remote {
                    namespace: machine.namespace(),
                    key: Box::new(key),
                }
            }
            Target::Workspace { id } => HistoryKey::workspace(id),
            Target::Agent { pane_id } => HistoryKey::agent(pane_id),
            Target::Directory { path } => HistoryKey::directory(path),
        }
    }
}

#[cfg(test)]
mod machine_tests {
    use super::*;
    use crate::machine::testing::{item, machine};

    #[test]
    fn host_qualified_ids_history_and_preview_do_not_collide() {
        let local = item(Target::Workspace { id: "w1".into() });
        let remote = local.clone().on_machine(&machine());
        assert_ne!(local.id, remote.id);
        assert_ne!(local.history_key(), remote.history_key());
        assert_ne!(local.preview_identity(), remote.preview_identity());
        let mut other_session = machine();
        other_session.session = "other".into();
        assert_ne!(
            remote.history_key(),
            local.clone().on_machine(&other_session).history_key()
        );
        let mut renamed = machine();
        renamed.label = "new label".into();
        assert_eq!(
            remote.history_key(),
            local.on_machine(&renamed).history_key()
        );
    }

    #[test]
    fn remote_directory_history_never_resolves_coordinator_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("alias");
        std::os::unix::fs::symlink(temp.path(), &path).unwrap();
        let local = item(Target::Directory { path: path.clone() });
        let remote = local.clone().on_machine(&machine());
        assert_eq!(local.history_key(), HistoryKey::directory(temp.path()));
        assert_eq!(
            remote.history_key(),
            HistoryKey::Remote {
                namespace: machine().namespace(),
                key: Box::new(HistoryKey::Directory(path))
            }
        );
    }

    #[test]
    fn machine_scope_starts_current_and_applies_across_tabs() {
        let mut app = App::new(Tab::Workspaces);
        let host = machine();
        let mut disabled = host.clone();
        disabled.id = "disabled".into();
        disabled.enabled = false;
        app.machines = vec![disabled, host.clone()];
        let local = item(Target::Workspace { id: "w1".into() });
        let remote = local.clone().on_machine(&host);
        for tab in Tab::ALL {
            app.set_items(tab, vec![local.clone(), remote.clone()]);
        }
        assert_eq!(app.filtered(), vec![&local]);
        app.state_mut().query = "project".into();
        app.cycle_machine();
        assert_eq!(app.active_machine, host.namespace());
        assert_eq!(app.state().query, "project");
        for tab in Tab::ALL {
            app.switch_tab(tab);
            assert_eq!(app.filtered(), vec![&remote]);
        }
        app.cycle_machine();
        assert_eq!(app.active_machine, "local");
        assert_eq!(app.filtered(), vec![&local]);
    }

    #[test]
    fn machine_cycle_and_query_intersect_without_editing_text() {
        let mut app = App::new(Tab::Workspaces);
        app.machines = vec![machine()];
        let local = item(Target::Workspace { id: "w1".into() });
        app.set_items(
            Tab::Workspaces,
            vec![local.clone(), local.on_machine(&machine())],
        );
        app.state_mut().query = "machine:goblin prj".into();
        assert_eq!(app.active_machine, "local");
        assert!(app.filtered().is_empty());
        app.cycle_machine();
        assert_eq!(app.filter_label(), "goblin");
        assert_eq!(app.filtered().len(), 1);
        app.cycle_machine();
        assert_eq!(app.active_machine, "local");
        assert!(app.filtered().is_empty());
        assert_eq!(app.state().query, "machine:goblin prj");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    Loading,
    Ready(Vec<Item>),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewState {
    Empty,
    Loading,
    Ready(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PreviewMode {
    #[default]
    Live,
    Off,
}

impl PreviewMode {
    pub fn enabled(self) -> bool {
        self == Self::Live
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TabState {
    pub query: String,
    pub selected_id: Option<String>,
    pub source: LoadState,
    pub sort: SortOrder,
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected_id: None,
            source: LoadState::Loading,
            sort: SortOrder::Recent,
        }
    }
}

#[derive(Debug, Clone)]
pub struct App {
    pub cached_hosts: HashSet<String>,
    pub host_preview_revision: u64,
    pub host_epochs: std::collections::HashMap<String, u64>,
    pub machines: Vec<crate::machine::Machine>,
    pub active_machine: String,
    pub host_sources: std::collections::BTreeMap<String, [Result<Vec<Item>, String>; 3]>,
    pub catalog_error: Option<String>,
    pub tab: Tab,
    pub tabs: [TabState; 3],
    pub preview: PreviewState,
    pub preview_generation: u64,
    pub refresh_generation: u64,
    pub pending_prefix: bool,
    pub should_close: bool,
    pub warning: Option<String>,
    pub preview_mode: PreviewMode,
    workspace_identity: Vec<Item>,
    history: History,
}

impl App {
    pub fn new(tab: Tab) -> Self {
        Self {
            host_preview_revision: 0,
            cached_hosts: HashSet::new(),
            host_epochs: std::collections::HashMap::new(),
            machines: Vec::new(),
            active_machine: "local".into(),
            host_sources: std::collections::BTreeMap::new(),
            catalog_error: None,
            tab,
            tabs: std::array::from_fn(|_| TabState::default()),
            preview: PreviewState::Empty,
            preview_generation: 0,
            refresh_generation: 0,
            pending_prefix: false,
            should_close: false,
            warning: None,
            preview_mode: PreviewMode::Live,
            workspace_identity: Vec::new(),
            history: History::default(),
        }
    }

    pub fn state(&self) -> &TabState {
        &self.tabs[self.tab.index()]
    }

    pub fn state_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.tab.index()]
    }

    pub fn filtered(&self) -> Vec<&Item> {
        let (machine_queries, query) = crate::machine::split_query(&self.state().query);
        let filtered_span = profile::span("model.filtered", || {
            serde_json::json!({
                "query_bytes": query.len(),
                "sort": self.state().sort.label(),
                "source_items": match &self.state().source {
                    LoadState::Ready(items) => items.len(),
                    LoadState::Loading | LoadState::Error(_) => 0,
                },
                "tab": self.tab.label(),
            })
        });
        let mut filtered: Vec<_> = match &self.state().source {
            LoadState::Ready(items) => items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    let namespace = item
                        .machine()
                        .map_or_else(|| "local".into(), crate::machine::Machine::namespace);
                    self.active_machine == namespace
                        && machine_queries
                            .iter()
                            .all(|query| crate::machine::matches_query(item.machine(), query))
                        && subsequence(&query, &item.search.to_lowercase())
                })
                .collect(),
            LoadState::Loading | LoadState::Error(_) => Vec::new(),
        };
        let history = &self.history;
        filtered.sort_by(|(left_index, left), (right_index, right)| {
            let first_seen = || {
                history
                    .first_seen(&right.history_key())
                    .cmp(&history.first_seen(&left.history_key()))
            };
            let order = match self.state().sort {
                SortOrder::Recent => match (
                    history.last_accessed(&left.history_key()),
                    history.last_accessed(&right.history_key()),
                ) {
                    (Some(left), Some(right)) => right.cmp(&left).then_with(first_seen),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => first_seen(),
                },
                SortOrder::AgeAscending => first_seen(),
                SortOrder::AgeDescending => history
                    .first_seen(&left.history_key())
                    .cmp(&history.first_seen(&right.history_key())),
            };
            order.then_with(|| left_index.cmp(right_index))
        });
        let result = filtered.into_iter().map(|(_, item)| item).collect();
        drop(filtered_span);
        result
    }

    pub fn actionable(&self, item: &Item) -> bool {
        item.machine()
            .is_none_or(|machine| !self.cached_hosts.contains(&machine.namespace()))
    }

    pub fn selected(&self) -> Option<&Item> {
        let selected = self.state().selected_id.as_deref();
        let filtered = self.filtered();
        filtered
            .iter()
            .find(|item| Some(item.id.as_str()) == selected)
            .copied()
            .or_else(|| filtered.first().copied())
    }

    pub fn cycle_machine(&mut self) {
        let choices: Vec<_> = std::iter::once("local".to_owned())
            .chain(
                self.machines
                    .iter()
                    .filter(|machine| machine.enabled)
                    .map(crate::machine::Machine::namespace),
            )
            .collect();
        let index = choices
            .iter()
            .position(|choice| *choice == self.active_machine)
            .unwrap_or(0);
        self.active_machine
            .clone_from(&choices[(index + 1) % choices.len()]);
        self.reconcile_selection();
    }

    pub fn filter_label(&self) -> String {
        match self.active_machine.as_str() {
            "local" => crate::machine::local_label(),
            namespace => self
                .machines
                .iter()
                .find(|machine| machine.namespace() == namespace)
                .map_or_else(|| "Unavailable".into(), |machine| machine.label.clone()),
        }
    }

    pub fn host_errors(&self) -> Vec<String> {
        let mut errors: Vec<_> = self.catalog_error.iter().cloned().collect();
        for (namespace, sources) in &self.host_sources {
            if self.active_machine != *namespace {
                continue;
            }
            let label = self
                .machines
                .iter()
                .find(|machine| machine.namespace() == *namespace)
                .map_or_else(crate::machine::local_label, |machine| machine.label.clone());
            if self.cached_hosts.contains(namespace) {
                errors.push(format!(
                    "{label}: cached · refreshing (actions unavailable)…"
                ));
            }
            for error in sources.iter().filter_map(|source| source.as_ref().err()) {
                let error = format!("{label}: {error}");
                if !errors.contains(&error) {
                    errors.push(error);
                }
            }
        }
        errors.extend(
            self.machines
                .iter()
                .filter(|machine| !machine.enabled && self.active_machine == machine.namespace())
                .map(|machine| format!("{}: disabled", machine.label)),
        );
        errors.extend(
            self.machines
                .iter()
                .filter(|machine| {
                    machine.enabled
                        && !self.host_sources.contains_key(&machine.namespace())
                        && self.catalog_error.is_none()
                        && self.active_machine == machine.namespace()
                })
                .map(|machine| format!("{}: loading…", machine.label)),
        );
        errors
    }

    pub fn rebuild_hosts(&mut self) {
        for tab in Tab::ALL {
            let items = self
                .host_sources
                .values()
                .filter_map(|sources| sources[tab.index()].as_ref().ok())
                .flatten()
                .cloned()
                .collect();
            self.set_items(tab, items);
        }
    }

    pub fn reconcile_selection(&mut self) {
        let selected = self.selected().map(|item| item.id.clone());
        self.state_mut().selected_id = selected;
    }

    pub fn set_items(&mut self, tab: Tab, items: Vec<Item>) {
        if tab == Tab::Workspaces {
            self.workspace_identity.clone_from(&items);
        }
        let keep = self.tabs[tab.index()].selected_id.clone();
        self.tabs[tab.index()].source = LoadState::Ready(items);
        self.tabs[tab.index()].selected_id = keep;
        if tab == self.tab {
            self.reconcile_selection();
        }
    }

    pub fn workspace_identity(&self) -> &[Item] {
        &self.workspace_identity
    }

    pub fn set_history(&mut self, history: History) {
        self.history = history;
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn observe_visible(&mut self, now: u64) -> bool {
        let visible: Vec<_> = self
            .tabs
            .iter()
            .filter_map(|state| match &state.source {
                LoadState::Ready(items) => Some(items.as_slice()),
                LoadState::Loading | LoadState::Error(_) => None,
            })
            .flatten()
            .collect();
        let observed = self.history.observe(visible.iter().copied(), now);
        let keys = visible.iter().map(|item| item.history_key()).collect();
        observed | self.history.prune(&keys, now)
    }

    pub fn visible_history_keys(&self) -> HashSet<HistoryKey> {
        self.tabs
            .iter()
            .filter_map(|state| match &state.source {
                LoadState::Ready(items) => Some(items.as_slice()),
                LoadState::Loading | LoadState::Error(_) => None,
            })
            .flatten()
            .map(Item::history_key)
            .collect()
    }

    pub fn record_access(&mut self, key: &HistoryKey, now: u64) {
        self.history.access(key, now);
    }

    pub fn cycle_sort(&mut self) {
        let next = self.state().sort.next();
        self.state_mut().sort = next;
        self.reconcile_selection();
    }

    pub fn move_selection(&mut self, delta: isize) {
        let filtered = self.filtered();
        if filtered.is_empty() {
            self.state_mut().selected_id = None;
            return;
        }
        let current = self
            .state()
            .selected_id
            .as_deref()
            .and_then(|id| filtered.iter().position(|item| item.id == id))
            .unwrap_or(0);
        let next = (current.cast_signed() + delta)
            .rem_euclid(filtered.len().cast_signed())
            .cast_unsigned();
        self.state_mut().selected_id = Some(filtered[next].id.clone());
    }

    pub fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.pending_prefix = false;
        self.reconcile_selection();
    }

    pub fn apply_preview(&mut self, generation: u64, preview: PreviewState) {
        if generation == self.preview_generation {
            self.preview = preview;
        }
    }
}

pub fn canonicalish(path: &Path) -> PathBuf {
    let canonicalize = profile::span("path.canonicalize", || serde_json::json!({ "path": path }));
    let result = std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path));
    drop(canonicalize);
    result
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = needle.chars();
    let mut wanted = chars.next();
    for character in haystack.chars() {
        if wanted == Some(character) {
            wanted = chars.next();
        }
    }
    wanted.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, search: &str) -> Item {
        Item {
            id: id.into(),
            label: id.into(),
            detail: String::new(),
            search: search.into(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: Target::Workspace { id: id.into() },
        }
    }

    #[test]
    fn filtering_is_case_insensitive_subsequence_matching() {
        let mut app = App::new(Tab::Workspaces);
        app.set_items(
            Tab::Workspaces,
            vec![item("one", "Dot Files"), item("two", "docs")],
        );
        app.state_mut().query = "dtf".into();
        app.reconcile_selection();
        assert_eq!(app.selected().unwrap().id, "one");
    }

    #[test]
    fn preview_mode_defaults_to_live_and_off_disables_preview() {
        assert_eq!(PreviewMode::default(), PreviewMode::Live);
        assert_eq!(App::new(Tab::Workspaces).preview_mode, PreviewMode::Live);
        assert!(PreviewMode::Live.enabled());
        assert!(!PreviewMode::Off.enabled());
        assert_eq!(PreviewMode::Live.label(), "live");
        assert_eq!(PreviewMode::Off.label(), "off");
    }

    #[test]
    fn query_and_stable_selection_are_independent_per_tab() {
        let mut app = App::new(Tab::Workspaces);
        app.set_items(Tab::Workspaces, vec![item("w1", "one"), item("w2", "two")]);
        app.move_selection(1);
        app.state_mut().query = "w".into();
        app.switch_tab(Tab::Agents);
        app.set_items(Tab::Agents, vec![item("a1", "agent")]);
        app.state_mut().query = "agent".into();
        app.switch_tab(Tab::Workspaces);
        assert_eq!(app.state().query, "w");
        assert_eq!(app.state().selected_id.as_deref(), Some("w2"));
    }

    #[test]
    fn stale_preview_is_ignored() {
        let mut app = App::new(Tab::Workspaces);
        app.preview_generation = 4;
        app.apply_preview(3, PreviewState::Ready("old".into()));
        assert_eq!(app.preview, PreviewState::Empty);
        app.apply_preview(4, PreviewState::Ready("new".into()));
        assert_eq!(app.preview, PreviewState::Ready("new".into()));
    }

    #[test]
    fn canonicalish_normalizes_nonexistent_paths() {
        assert_eq!(
            canonicalish(Path::new("/tmp/a/../b/./c")),
            PathBuf::from("/tmp/b/c")
        );
    }

    #[test]
    fn preview_identity_changes_when_best_pane_changes_for_same_item() {
        let mut item = item("w1", "workspace");
        item.preview_pane = Some("p1".into());
        let first = item.preview_identity();
        item.preview_pane = Some("p2".into());
        assert_ne!(first, item.preview_identity());
    }

    #[test]
    fn workspace_identity_survives_a_display_error() {
        let mut app = App::new(Tab::Workspaces);
        app.set_items(Tab::Workspaces, vec![item("w1", "workspace")]);
        app.tabs[Tab::Workspaces.index()].source = LoadState::Error("refresh failed".into());
        assert_eq!(app.workspace_identity()[0].id, "w1");
    }

    #[test]
    fn sort_defaults_to_recent_and_is_independent_per_tab() {
        let mut app = App::new(Tab::Workspaces);
        assert!(app.tabs.iter().all(|state| state.sort == SortOrder::Recent));
        app.cycle_sort();
        assert_eq!(app.state().sort, SortOrder::AgeAscending);
        app.switch_tab(Tab::Agents);
        assert_eq!(app.state().sort, SortOrder::Recent);
    }

    #[test]
    fn sort_orders_use_history_then_provider_order() {
        let mut app = App::new(Tab::Workspaces);
        let items = vec![
            item("a", "item"),
            item("b", "item"),
            item("c", "item"),
            item("d", "item"),
        ];
        let mut history = History::default();
        history.observe([&items[0]], 10);
        history.observe([&items[1]], 20);
        history.observe([&items[2], &items[3]], 30);
        history.access(&items[1].history_key(), 40);
        history.access(&items[0].history_key(), 50);
        app.set_history(history);
        app.set_items(Tab::Workspaces, items);

        assert_eq!(ids(&app), ["a", "b", "c", "d"]);
        app.cycle_sort();
        assert_eq!(ids(&app), ["c", "d", "b", "a"]);
        app.cycle_sort();
        assert_eq!(ids(&app), ["a", "b", "c", "d"]);
        app.cycle_sort();
        assert_eq!(ids(&app), ["a", "b", "c", "d"]);
    }

    #[test]
    fn filtering_happens_before_sorting() {
        let mut app = App::new(Tab::Workspaces);
        let items = vec![
            item("old-match", "match"),
            item("new-other", "other"),
            item("new-match", "match"),
        ];
        let mut history = History::default();
        history.observe([&items[0]], 1);
        history.observe([&items[1], &items[2]], 2);
        app.set_history(history);
        app.set_items(Tab::Workspaces, items);
        app.state_mut().query = "match".into();
        assert_eq!(ids(&app), ["new-match", "old-match"]);
    }

    #[test]
    fn cycling_sort_preserves_selection_and_preview_identity() {
        let mut app = App::new(Tab::Workspaces);
        let mut first = item("a", "item");
        first.preview_pane = Some("p1".into());
        app.set_items(Tab::Workspaces, vec![first, item("b", "item")]);
        app.move_selection(1);
        let identity = app.selected().unwrap().preview_identity();
        app.cycle_sort();
        assert_eq!(app.state().selected_id.as_deref(), Some("b"));
        assert_eq!(app.selected().unwrap().preview_identity(), identity);
    }

    fn ids(app: &App) -> Vec<&str> {
        app.filtered()
            .into_iter()
            .map(|item| item.id.as_str())
            .collect()
    }

    #[test]
    fn history_keys_are_namespaced_against_cross_source_collisions() {
        let workspace = HistoryKey::workspace("same");
        let agent = HistoryKey::agent("same");
        let directory = HistoryKey::directory(Path::new("same"));
        assert_ne!(workspace, agent);
        assert_ne!(workspace, directory);
        assert_ne!(agent, directory);
        assert_eq!(
            HistoryKey::directory(Path::new("/tmp/a/../b")),
            HistoryKey::directory(Path::new("/tmp/b"))
        );
    }
}
