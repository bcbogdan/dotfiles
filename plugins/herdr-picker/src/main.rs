mod history;
mod input;
mod model;
mod preview;
mod provider;
mod runtime;
mod ui;

use std::env;
use std::ffi::OsString;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use input::{Effect, Reducer};
use model::{App, LoadState, PreviewState, Tab};
use provider::{Client, Runner, SystemRunner};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::Rect;
use runtime::{Message, RefreshScheduler, Runtime};

const POLL_INTERVAL: Duration = Duration::from_millis(80);
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--open")
    {
        return open_picker(
            arguments
                .get(1)
                .map_or(Tab::Workspaces, |tab| parse_tab(tab)),
        );
    }
    run_picker()
}

fn parse_tab(value: &str) -> Tab {
    if value.eq_ignore_ascii_case("agents") {
        Tab::Agents
    } else {
        Tab::Workspaces
    }
}

fn open_picker(tab: Tab) -> ExitCode {
    let herdr = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let plugin = env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "local.unified-picker".into());
    let initial = match tab {
        Tab::Agents => "agents",
        Tab::Workspaces | Tab::Zoxide => "workspaces",
    };
    match Command::new(herdr)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            &plugin,
            "--entrypoint",
            "picker",
            "--env",
            &format!("HERDR_PICKER_TAB={initial}"),
            "--focus",
        ])
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("herdr-picker: failed to open picker: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_picker() -> ExitCode {
    let initial = env::var("HERDR_PICKER_TAB").map_or(Tab::Workspaces, |value| parse_tab(&value));
    let herdr = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr"));
    let client = Client::new(SystemRunner, herdr);
    let runtime = Runtime::start(client.clone());
    let store = history::FileStore::from_env();
    let mut app = App::new(initial);
    match history::Store::load(&store) {
        Ok(outcome) => {
            app.set_history(outcome.history);
            app.warning = outcome.warning;
        }
        Err(error) => app.warning = Some(format!("history unavailable: {error}")),
    }
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &runtime, &store);
    ratatui::restore();
    match result {
        Ok(Some(item)) => {
            match confirm_item(&client, &store, &mut app, &item, history::unix_time()) {
                Ok(()) => {
                    if let Some(warning) = &app.warning {
                        eprintln!("herdr-picker: warning: {warning}");
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("herdr-picker: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("herdr-picker: {error}");
            ExitCode::FAILURE
        }
    }
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    runtime: &Runtime,
    store: &impl history::Store,
) -> std::io::Result<Option<model::Item>> {
    let self_pane = env::var("HERDR_PANE_ID").ok();
    let mut reducer = Reducer::new();
    let mut live = preview::LivePreview::new();
    let mut arbiter = preview::Arbiter::new();
    let mut refresh_scheduler = RefreshScheduler::default();
    let mut refreshed_at = Instant::now();
    request_refresh(app, runtime, &mut refresh_scheduler, self_pane.clone());
    let size = terminal.size()?;
    let mut dimensions = ui::preview_dimensions(Rect::new(0, 0, size.width, size.height));

    loop {
        let previous = app.selected().map(model::Item::preview_identity);
        while let Some(message) = runtime.try_recv() {
            match message {
                Message::Refresh { generation, result } => {
                    runtime::apply_refresh(app, generation, result);
                    if app.observe_visible(history::unix_time()) {
                        persist_history(store, app, history::unix_time());
                    }
                    if let Some(next) = refresh_scheduler.complete(app) {
                        runtime.refresh(next, self_pane.clone());
                    }
                }
                Message::Preview { generation, result } => {
                    if let Some(preview) = arbiter.fallback(generation, result, Instant::now()) {
                        app.apply_preview(generation, preview);
                    }
                }
            }
        }
        let current = app.selected().map(model::Item::preview_identity);
        if current != previous {
            request_preview(app, runtime, &mut live, &mut arbiter, dimensions);
        }
        if let Some(result) = live.poll() {
            if let Some(preview) = arbiter.live(result) {
                app.apply_preview(app.preview_generation, preview);
            }
        }
        if let Some(preview) = arbiter.tick(Instant::now()) {
            app.apply_preview(app.preview_generation, preview);
        }
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                Event::Resize(width, height) => {
                    dimensions = ui::preview_dimensions(Rect::new(0, 0, width, height));
                    request_preview(app, runtime, &mut live, &mut arbiter, dimensions);
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let previous = app.selected().map(model::Item::preview_identity);
                    match reducer.key(app, key, Instant::now()) {
                        Effect::Confirm => {
                            return Ok(app.selected().cloned());
                        }
                        Effect::Close => return Ok(None),
                        Effect::SelectionChanged => {
                            let current = app.selected().map(model::Item::preview_identity);
                            if current != previous {
                                request_preview(app, runtime, &mut live, &mut arbiter, dimensions);
                            }
                        }
                        Effect::None => {}
                    }
                }
                _ => {}
            }
        }
        reducer.expire_prefix(app, Instant::now());
        if refreshed_at.elapsed() >= REFRESH_INTERVAL {
            request_refresh(app, runtime, &mut refresh_scheduler, self_pane.clone());
            refreshed_at = Instant::now();
        }
    }
}

fn confirm_item<R: Runner, S: history::Store>(
    client: &Client<R>,
    store: &S,
    app: &mut App,
    item: &model::Item,
    now: u64,
) -> Result<(), provider::Error> {
    let result = client.confirm(&item.target, app.workspace_identity())?;
    app.record_access(&item.history_key(), now);
    if let Some(workspace_id) = result.resolved_workspace_id {
        app.record_access(&model::HistoryKey::workspace(workspace_id), now);
    }
    persist_history(store, app, now);
    Ok(())
}

fn persist_history(store: &impl history::Store, app: &mut App, now: u64) {
    match store.save(app.history(), &app.visible_history_keys(), now) {
        Ok(outcome) => {
            app.set_history(outcome.history);
            app.warning = outcome.warning;
        }
        Err(error) => app.warning = Some(format!("history not saved: {error}")),
    }
}

fn request_refresh(
    app: &mut App,
    runtime: &Runtime,
    scheduler: &mut RefreshScheduler,
    self_pane: Option<String>,
) {
    if app
        .tabs
        .iter()
        .all(|state| matches!(state.source, LoadState::Loading))
    {
        app.preview = PreviewState::Loading;
    }
    if let Some(generation) = scheduler.demand(app) {
        runtime.refresh(generation, self_pane);
    }
}

fn request_preview(
    app: &mut App,
    runtime: &Runtime,
    live: &mut preview::LivePreview,
    arbiter: &mut preview::Arbiter,
    dimensions: (u16, u16),
) {
    app.preview_generation = app.preview_generation.wrapping_add(1);
    let generation = app.preview_generation;
    let selected = app.selected().cloned();
    let identity = selected.as_ref().map(model::Item::preview_identity);
    let live_start = live.select(identity.as_ref(), dimensions);
    arbiter.reset(generation, &live_start, Instant::now());
    match selected {
        Some(item) => {
            app.preview = PreviewState::Loading;
            runtime.preview(generation, item);
        }
        None => app.preview = PreviewState::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::Mutex;

    #[derive(Clone)]
    struct ConfirmRunner {
        succeeds: bool,
    }

    #[derive(Clone)]
    struct CreateRunner;

    impl Runner for CreateRunner {
        fn run(
            &self,
            _program: &OsStr,
            _args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, provider::Error> {
            Ok(r#"{"result":{"type":"workspace_created","workspace":{"workspace_id":"w9"},"tab":{"tab_id":"w9:t1"},"root_pane":{"pane_id":"w9:p1"}}}"#.into())
        }
    }

    impl Runner for ConfirmRunner {
        fn run(
            &self,
            _program: &OsStr,
            _args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, provider::Error> {
            if self.succeeds {
                Ok("{}".into())
            } else {
                Err(provider::Error::Command {
                    program: "herdr".into(),
                    message: "focus failed".into(),
                })
            }
        }
    }

    #[derive(Default)]
    struct FakeStore {
        saves: Mutex<usize>,
    }

    struct FailingStore;

    impl history::Store for FailingStore {
        fn load(&self) -> Result<history::LoadOutcome, String> {
            Err("read-only filesystem".into())
        }

        fn save(
            &self,
            _history: &history::History,
            _visible: &std::collections::HashSet<model::HistoryKey>,
            _now: u64,
        ) -> Result<history::SaveOutcome, String> {
            Err("disk full".into())
        }
    }

    impl history::Store for FakeStore {
        fn load(&self) -> Result<history::LoadOutcome, String> {
            Ok(history::LoadOutcome {
                history: history::History::default(),
                warning: None,
                logical_time: 0,
            })
        }

        fn save(
            &self,
            history: &history::History,
            _visible: &std::collections::HashSet<model::HistoryKey>,
            _now: u64,
        ) -> Result<history::SaveOutcome, String> {
            *self.saves.lock().unwrap() += 1;
            Ok(history::SaveOutcome {
                history: history.clone(),
                warning: None,
            })
        }
    }

    fn selected_app() -> (App, model::Item) {
        let item = model::Item {
            id: "w1".into(),
            label: "workspace".into(),
            detail: String::new(),
            search: String::new(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: model::Target::Workspace { id: "w1".into() },
        };
        let mut app = App::new(Tab::Workspaces);
        app.set_items(Tab::Workspaces, vec![item.clone()]);
        app.observe_visible(10);
        (app, item)
    }

    #[test]
    fn successful_confirmation_updates_access_and_persists() {
        let (mut app, item) = selected_app();
        let store = FakeStore::default();
        confirm_item(
            &Client::new(ConfirmRunner { succeeds: true }, "herdr".into()),
            &store,
            &mut app,
            &item,
            20,
        )
        .unwrap();
        assert_eq!(
            app.history()
                .last_accessed(&model::HistoryKey::workspace("w1")),
            Some(20)
        );
        assert_eq!(*store.saves.lock().unwrap(), 1);
    }

    #[test]
    fn failed_confirmation_does_not_update_or_persist_access() {
        let (mut app, item) = selected_app();
        let store = FakeStore::default();
        assert!(confirm_item(
            &Client::new(ConfirmRunner { succeeds: false }, "herdr".into()),
            &store,
            &mut app,
            &item,
            20,
        )
        .is_err());
        assert_eq!(
            app.history()
                .last_accessed(&model::HistoryKey::workspace("w1")),
            None
        );
        assert_eq!(*store.saves.lock().unwrap(), 0);
    }

    #[test]
    fn zoxide_existing_workspace_confirmation_marks_both_namespaces() {
        let workspace = model::Item {
            id: "w1".into(),
            label: "workspace".into(),
            detail: "/tmp/project".into(),
            search: String::new(),
            preview_pane: None,
            match_paths: vec![model::canonicalish(std::path::Path::new("/tmp/project"))],
            target: model::Target::Workspace { id: "w1".into() },
        };
        let directory = model::Item {
            id: "/tmp/project".into(),
            label: "project".into(),
            detail: "/tmp/project".into(),
            search: String::new(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: model::Target::Directory {
                path: "/tmp/project".into(),
            },
        };
        let mut app = App::new(Tab::Zoxide);
        app.set_items(Tab::Workspaces, vec![workspace]);
        app.set_items(Tab::Zoxide, vec![directory.clone()]);
        app.observe_visible(10);
        confirm_item(
            &Client::new(ConfirmRunner { succeeds: true }, "herdr".into()),
            &FakeStore::default(),
            &mut app,
            &directory,
            20,
        )
        .unwrap();
        assert_eq!(
            app.history().last_accessed(&directory.history_key()),
            Some(20)
        );
        assert_eq!(
            app.history()
                .last_accessed(&model::HistoryKey::workspace("w1")),
            Some(20)
        );
    }

    #[test]
    fn zoxide_created_workspace_confirmation_marks_both_namespaces() {
        let directory = model::Item {
            id: "/tmp/new-project".into(),
            label: "new-project".into(),
            detail: "/tmp/new-project".into(),
            search: String::new(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: model::Target::Directory {
                path: "/tmp/new-project".into(),
            },
        };
        let mut app = App::new(Tab::Zoxide);
        app.set_items(Tab::Zoxide, vec![directory.clone()]);
        app.observe_visible(10);
        confirm_item(
            &Client::new(CreateRunner, "herdr".into()),
            &FakeStore::default(),
            &mut app,
            &directory,
            20,
        )
        .unwrap();
        assert_eq!(
            app.history().last_accessed(&directory.history_key()),
            Some(20)
        );
        assert_eq!(
            app.history()
                .last_accessed(&model::HistoryKey::workspace("w9")),
            Some(20)
        );
    }

    #[test]
    fn zoxide_focus_failure_marks_neither_namespace() {
        let workspace = model::Item {
            id: "w1".into(),
            label: "workspace".into(),
            detail: "/tmp/project".into(),
            search: String::new(),
            preview_pane: None,
            match_paths: vec![model::canonicalish(std::path::Path::new("/tmp/project"))],
            target: model::Target::Workspace { id: "w1".into() },
        };
        let item = model::Item {
            id: "/tmp/project".into(),
            label: "project".into(),
            detail: "/tmp/project".into(),
            search: String::new(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: model::Target::Directory {
                path: "/tmp/project".into(),
            },
        };
        let mut app = App::new(Tab::Zoxide);
        app.set_items(Tab::Workspaces, vec![workspace]);
        app.set_items(Tab::Zoxide, vec![item.clone()]);
        app.observe_visible(10);
        assert!(confirm_item(
            &Client::new(ConfirmRunner { succeeds: false }, "herdr".into()),
            &FakeStore::default(),
            &mut app,
            &item,
            20,
        )
        .is_err());
        assert_eq!(app.history().last_accessed(&item.history_key()), None);
        assert_eq!(
            app.history()
                .last_accessed(&model::HistoryKey::workspace("w1")),
            None
        );
    }

    #[test]
    fn persistence_failure_sets_non_blocking_warning() {
        let (mut app, _) = selected_app();
        persist_history(&FailingStore, &mut app, 20);
        assert_eq!(app.warning.as_deref(), Some("history not saved: disk full"));
    }

    #[test]
    fn clean_save_clears_previous_persistence_warning() {
        let (mut app, _) = selected_app();
        app.warning = Some("history not saved: disk full".into());
        persist_history(&FakeStore::default(), &mut app, 20);
        assert_eq!(app.warning, None);
    }
}
