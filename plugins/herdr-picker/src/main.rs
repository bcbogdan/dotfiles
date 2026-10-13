mod history;
mod input;
mod model;
mod preview;
mod profile;
mod provider;
mod runtime;
mod ui;
mod worktree;

use std::env;
use std::ffi::OsString;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use input::{Effect, Reducer};
use model::{App, LoadState, PreviewMode, PreviewState, Tab};
use provider::{Client, Runner, SystemRunner};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::Rect;
use runtime::{Message, RefreshScheduler, Runtime};

const ACTIVE_PREVIEW_POLL_INTERVAL: Duration = Duration::from_millis(16);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(80);
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

struct KeyboardEnhancements(bool);

impl KeyboardEnhancements {
    fn enable() -> Self {
        use ratatui::crossterm::{
            event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
            execute, terminal,
        };
        let supported = terminal::supports_keyboard_enhancement().unwrap_or(false);
        Self(
            supported
                && execute!(
                    std::io::stdout(),
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )
                .is_ok(),
        )
    }
}

impl Drop for KeyboardEnhancements {
    fn drop(&mut self) {
        if self.0 {
            let _ = ratatui::crossterm::execute!(
                std::io::stdout(),
                ratatui::crossterm::event::PopKeyboardEnhancementFlags
            );
        }
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let preview_mode = match parse_preview_mode(&arguments) {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("herdr-picker: {error}");
            return ExitCode::FAILURE;
        }
    };
    if arguments
        .first()
        .is_some_and(|argument| argument == "--open")
    {
        return open_picker(
            arguments
                .get(1)
                .map_or(Tab::Workspaces, |tab| parse_tab(tab)),
            preview_mode,
        );
    }
    run_picker(preview_mode)
}

fn parse_tab(value: &str) -> Tab {
    if value.eq_ignore_ascii_case("agents") {
        Tab::Agents
    } else {
        Tab::Workspaces
    }
}

fn parse_preview_mode(arguments: &[String]) -> Result<PreviewMode, String> {
    let configured = arguments
        .iter()
        .position(|argument| argument == "--preview")
        .map(|index| {
            arguments
                .get(index + 1)
                .map(String::as_str)
                .ok_or_else(|| "--preview requires live or off".to_string())
        })
        .transpose()?
        .map(str::to_owned)
        .or_else(|| env::var("HERDR_PICKER_PREVIEW").ok());
    match configured.as_deref().unwrap_or("live") {
        "live" => Ok(PreviewMode::Live),
        "off" => Ok(PreviewMode::Off),
        value => Err(format!(
            "invalid preview mode {value:?}; expected live or off"
        )),
    }
}

fn open_picker(tab: Tab, preview_mode: PreviewMode) -> ExitCode {
    let herdr = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let plugin = env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "local.unified-picker".into());
    let initial = match tab {
        Tab::Agents => "agents",
        Tab::Workspaces | Tab::Zoxide => "workspaces",
    };
    let mut command = Command::new(herdr);
    command.args([
        "plugin",
        "pane",
        "open",
        "--plugin",
        &plugin,
        "--entrypoint",
        "picker",
        "--env",
        &format!("HERDR_PICKER_TAB={initial}"),
        "--env",
        &format!("HERDR_PICKER_PREVIEW={}", preview_mode.label()),
    ]);
    if let Some(profile) = env::var_os("HERDR_PICKER_PROFILE") {
        command.arg("--env").arg(format!(
            "HERDR_PICKER_PROFILE={}",
            profile.to_string_lossy()
        ));
    }
    command.arg("--focus");
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("herdr-picker: failed to open picker: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_picker(preview_mode: PreviewMode) -> ExitCode {
    let profile_warning = profile::initialize().err();
    let startup = profile::span("picker.run", || serde_json::json!({}));
    let initial = env::var("HERDR_PICKER_TAB").map_or(Tab::Workspaces, |value| parse_tab(&value));
    let herdr = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr"));
    let client = Client::new(SystemRunner, herdr);
    let runtime = Runtime::start(client.clone());
    let store = history::FileStore::from_env();
    let mut app = App::new(initial);
    app.preview_mode = preview_mode;
    let history_load = profile::span("history.load", || serde_json::json!({}));
    match history::Store::load(&store) {
        Ok(outcome) => {
            app.set_history(outcome.history);
            app.warning = outcome.warning;
        }
        Err(error) => app.warning = Some(format!("history unavailable: {error}")),
    }
    drop(history_load);
    if let Some(error) = profile_warning {
        let warning = format!("profiler unavailable: {error}");
        app.warning = Some(
            app.warning
                .map_or(warning.clone(), |current| format!("{current}; {warning}")),
        );
    }
    let mut terminal = ratatui::init();
    let keyboard = KeyboardEnhancements::enable();
    let result = event_loop(&mut terminal, &mut app, &runtime, &store, &client);
    drop(keyboard);
    ratatui::restore();
    let exit = match result {
        Ok(Some(item)) => {
            match confirm_item(&client, &store, &mut app, &item, history::unix_time()) {
                Ok(()) => {
                    if let Some(warning) = &app.warning {
                        eprintln!("herdr-picker: warning: {warning}");
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    persist_history(&store, &mut app, history::unix_time());
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
    };
    drop(startup);
    if let Err(error) = profile::shutdown() {
        eprintln!("herdr-picker: profiler shutdown failed: {error}");
    }
    exit
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    runtime: &Runtime,
    store: &impl history::Store,
    client: &Client<SystemRunner>,
) -> std::io::Result<Option<model::Item>> {
    let mut history_dirty = false;
    let result = event_loop_inner(terminal, app, runtime, &mut history_dirty, client);
    let confirmation_will_persist = matches!(&result, Ok(Some(_)));
    if history_dirty && !confirmation_will_persist {
        persist_history(store, app, history::unix_time());
    }
    result
}

fn event_loop_inner(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    runtime: &Runtime,
    history_dirty: &mut bool,
    client: &Client<SystemRunner>,
) -> std::io::Result<Option<model::Item>> {
    let self_pane = env::var("HERDR_PANE_ID").ok();
    let mut reducer = Reducer::new();
    let mut live = preview::LivePreview::new();
    let mut arbiter = preview::Arbiter::new();
    let mut refresh_scheduler = RefreshScheduler::default();
    let mut refreshed_at = Instant::now();
    request_refresh(app, runtime, &mut refresh_scheduler, self_pane.clone());
    let size = terminal.size()?;
    let mut dimensions = ui::preview_dimensions(
        Rect::new(0, 0, size.width, size.height),
        app.preview_mode.enabled(),
    );
    let mut input_started = None;
    let mut needs_draw = true;
    let mut flow: Option<worktree::Flow> = None;
    let completion_gate = worktree::CompletionGate::default();

    Ok(loop {
        if poll_worktree(app, flow.as_mut(), client, &mut needs_draw) {
            *history_dirty = true;
            break None;
        }
        let previous = app.selected().map(model::Item::preview_identity);
        let (messages_changed_history, messages_need_draw) = process_runtime_messages(
            app,
            runtime,
            &mut refresh_scheduler,
            self_pane.as_ref(),
            &mut arbiter,
        );
        *history_dirty |= messages_changed_history;
        needs_draw |= messages_need_draw;
        let current = app.selected().map(model::Item::preview_identity);
        if flow.is_none() && app.preview_mode.enabled() && current != previous {
            request_preview(app, runtime, &mut live, &mut arbiter, dimensions);
            needs_draw = true;
        }
        if flow.is_none() {
            needs_draw |= apply_live_previews(app, runtime, &mut live, &mut arbiter);
        }
        if needs_draw {
            draw_frame(terminal, app, flow.as_ref(), input_started.take())?;
            needs_draw = false;
        }

        if event::poll(poll_interval(flow.as_ref(), &live))? {
            match event::read()? {
                Event::Resize(width, height) => {
                    dimensions = ui::preview_dimensions(
                        Rect::new(0, 0, width, height),
                        app.preview_mode.enabled(),
                    );
                    if flow.is_none() && app.preview_mode.enabled() {
                        request_preview(app, runtime, &mut live, &mut arbiter, dimensions);
                    }
                    needs_draw = true;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if worktree_key(&mut flow, key) {
                        if flow.is_none() {
                            request_preview(app, runtime, &mut live, &mut arbiter, dimensions);
                        }
                        needs_draw = true;
                        continue;
                    }
                    input_started = profile::timestamp();
                    let input = input_span(key);
                    let previous = app.selected().map(model::Item::preview_identity);
                    needs_draw = true;
                    match reducer.key(app, key, Instant::now()) {
                        Effect::Confirm => {
                            break app.selected().cloned();
                        }
                        Effect::CreateWorktree => {
                            flow = start_worktree(app, &completion_gate);
                            if flow.is_some() {
                                live.select(None, dimensions);
                            }
                        }
                        Effect::Close => break None,
                        Effect::SelectionChanged => {
                            let current = app.selected().map(model::Item::preview_identity);
                            if app.preview_mode.enabled() && current != previous {
                                request_preview(app, runtime, &mut live, &mut arbiter, dimensions);
                            }
                        }
                        Effect::None => {}
                    }
                    drop(input);
                }
                _ => {}
            }
        }
        if reducer.expire_prefix(app, Instant::now()) {
            needs_draw = true;
        }
        if refreshed_at.elapsed() >= REFRESH_INTERVAL {
            request_refresh(app, runtime, &mut refresh_scheduler, self_pane.clone());
            refreshed_at = Instant::now();
        }
    })
}

fn process_runtime_messages(
    app: &mut App,
    runtime: &Runtime,
    refresh_scheduler: &mut RefreshScheduler,
    self_pane: Option<&String>,
    arbiter: &mut preview::Arbiter,
) -> (bool, bool) {
    let mut history_dirty = false;
    let mut needs_draw = false;
    while let Some(message) = runtime.try_recv() {
        match message {
            Message::Refresh { generation, result } => {
                let apply = profile::span(
                    "refresh.apply",
                    || serde_json::json!({ "generation": generation }),
                );
                runtime::apply_refresh(app, generation, result);
                history_dirty |= app.observe_visible(history::unix_time());
                if let Some(next) = refresh_scheduler.complete(app) {
                    runtime.refresh(next, self_pane.cloned());
                }
                drop(apply);
                needs_draw = true;
            }
            Message::Preview { generation, result } => {
                if let Some(preview) = arbiter.fallback(generation, result) {
                    app.apply_preview(generation, preview);
                    needs_draw = true;
                }
            }
        }
    }
    (history_dirty, needs_draw)
}

fn input_span(key: event::KeyEvent) -> profile::Span {
    profile::span("input.handle", || {
        serde_json::json!({
            "code": format!("{:?}", key.code),
            "modifiers": format!("{:?}", key.modifiers),
        })
    })
}

fn worktree_key(flow: &mut Option<worktree::Flow>, key: event::KeyEvent) -> bool {
    let Some(active) = flow else {
        return false;
    };
    if matches!(active.key(key), worktree::Action::Cancel) {
        *flow = None;
    }
    true
}

fn start_worktree(app: &App, completion_gate: &worktree::CompletionGate) -> Option<worktree::Flow> {
    let model::Target::Directory { path } = &app.selected()?.target else {
        return None;
    };
    let cached = match &app.tabs[Tab::Zoxide.index()].source {
        LoadState::Ready(items) => items
            .iter()
            .filter_map(|item| match &item.target {
                model::Target::Directory { path } => Some(path.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Some(worktree::Flow::new(
        path.clone(),
        cached,
        completion_gate.clone(),
    ))
}

fn poll_worktree(
    app: &mut App,
    flow: Option<&mut worktree::Flow>,
    client: &Client<SystemRunner>,
    needs_draw: &mut bool,
) -> bool {
    let Some(active) = flow else {
        return false;
    };
    *needs_draw |= active.poll();
    active.start_open(client, app.workspace_identity());
    let Some((path, result)) = active.take_opened() else {
        return false;
    };
    let now = history::unix_time();
    app.record_access(&model::HistoryKey::directory(&path), now);
    if let Some(id) = result.resolved_workspace_id {
        app.record_access(&model::HistoryKey::workspace(id), now);
    }
    true
}

fn apply_live_previews(
    app: &mut App,
    runtime: &Runtime,
    live: &mut preview::LivePreview,
    arbiter: &mut preview::Arbiter,
) -> bool {
    let mut needs_draw = false;
    while let Some(result) = live.poll() {
        if let Some(preview) = arbiter.live(result) {
            app.apply_preview(app.preview_generation, preview);
            needs_draw = true;
        }
    }
    if app.preview_mode.enabled() {
        request_fallback_if_due(app, runtime, arbiter, Instant::now());
    }
    needs_draw
}

fn poll_interval(flow: Option<&worktree::Flow>, live: &preview::LivePreview) -> Duration {
    if flow.is_some_and(worktree::Flow::awaiting_worker) {
        Duration::from_millis(16)
    } else if live.is_active() {
        ACTIVE_PREVIEW_POLL_INTERVAL
    } else {
        IDLE_POLL_INTERVAL
    }
}

fn draw_frame(
    terminal: &mut ratatui::DefaultTerminal,
    app: &App,
    flow: Option<&worktree::Flow>,
    input_started: Option<Instant>,
) -> std::io::Result<()> {
    let draw = profile::span("ui.draw", || {
        serde_json::json!({
            "tab": app.tab.label(),
            "selected_id": app.state().selected_id,
        })
    });
    terminal.draw(|frame| {
        if let Some(active) = flow {
            active.draw(frame);
        } else {
            ui::draw(frame, app);
        }
    })?;
    drop(draw);
    profile::record_elapsed("input.to_draw", input_started, || {
        serde_json::json!({
            "tab": app.tab.label(),
            "selected_id": app.state().selected_id,
        })
    });
    Ok(())
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
    let span = profile::span(
        "history.persist",
        || serde_json::json!({ "visible_items": app.visible_history_keys().len() }),
    );
    match store.save(app.history(), &app.visible_history_keys(), now) {
        Ok(outcome) => {
            app.set_history(outcome.history);
            app.warning = outcome.warning;
        }
        Err(error) => app.warning = Some(format!("history not saved: {error}")),
    }
    drop(span);
}

fn request_refresh(
    app: &mut App,
    runtime: &Runtime,
    scheduler: &mut RefreshScheduler,
    self_pane: Option<String>,
) {
    if app.preview_mode.enabled()
        && app
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
    if !app.preview_mode.enabled() {
        return;
    }
    let request = profile::span("preview.request", || {
        serde_json::json!({
            "dimensions": [dimensions.0, dimensions.1],
            "selected_id": app.state().selected_id,
            "tab": app.tab.label(),
        })
    });
    app.preview_generation = app.preview_generation.wrapping_add(1);
    let generation = app.preview_generation;
    let selected = app.selected().cloned();
    let identity = selected.as_ref().map(model::Item::preview_identity);
    let live_start = live.select(identity.as_ref(), dimensions);
    arbiter.reset(generation, &live_start, Instant::now());
    match selected {
        Some(_) => {
            app.preview = PreviewState::Loading;
            request_fallback_if_due(app, runtime, arbiter, Instant::now());
        }
        None => app.preview = PreviewState::Empty,
    }
    drop(request);
}

fn request_fallback_if_due(
    app: &App,
    runtime: &Runtime,
    arbiter: &mut preview::Arbiter,
    now: Instant,
) {
    if arbiter.fallback_due(now) {
        if let Some(item) = app.selected().cloned() {
            runtime.preview(app.preview_generation, item);
        }
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

    #[test]
    fn explicit_preview_modes_parse_and_invalid_values_are_rejected() {
        assert_eq!(
            parse_preview_mode(&["--preview".into(), "live".into()]),
            Ok(PreviewMode::Live)
        );
        assert_eq!(
            parse_preview_mode(&["--preview".into(), "off".into()]),
            Ok(PreviewMode::Off)
        );
        assert_eq!(
            parse_preview_mode(&["--preview".into()]),
            Err("--preview requires live or off".into())
        );
        assert!(parse_preview_mode(&["--preview".into(), "auto".into()])
            .unwrap_err()
            .contains("expected live or off"));
    }

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
