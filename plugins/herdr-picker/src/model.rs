use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Workspace { id: String },
    Agent { pane_id: String },
    Directory { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub fn preview_identity(&self) -> PreviewIdentity {
        PreviewIdentity {
            item_id: self.id.clone(),
            preview_pane: self.preview_pane.clone(),
            target: self.target.clone(),
        }
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

#[derive(Debug, Clone)]
pub struct TabState {
    pub query: String,
    pub selected_id: Option<String>,
    pub source: LoadState,
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected_id: None,
            source: LoadState::Loading,
        }
    }
}

#[derive(Debug, Clone)]
pub struct App {
    pub tab: Tab,
    pub tabs: [TabState; 3],
    pub preview: PreviewState,
    pub preview_generation: u64,
    pub refresh_generation: u64,
    pub pending_prefix: bool,
    pub should_close: bool,
    workspace_identity: Vec<Item>,
}

impl App {
    pub fn new(tab: Tab) -> Self {
        Self {
            tab,
            tabs: std::array::from_fn(|_| TabState::default()),
            preview: PreviewState::Empty,
            preview_generation: 0,
            refresh_generation: 0,
            pending_prefix: false,
            should_close: false,
            workspace_identity: Vec::new(),
        }
    }

    pub fn state(&self) -> &TabState {
        &self.tabs[self.tab.index()]
    }

    pub fn state_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.tab.index()]
    }

    pub fn filtered(&self) -> Vec<&Item> {
        let query = self.state().query.to_lowercase();
        match &self.state().source {
            LoadState::Ready(items) => items
                .iter()
                .filter(|item| subsequence(&query, &item.search.to_lowercase()))
                .collect(),
            LoadState::Loading | LoadState::Error(_) => Vec::new(),
        }
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
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
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
}
