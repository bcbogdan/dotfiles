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
use provider::{Client, SystemRunner};
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
    let mut app = App::new(initial);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &runtime);
    ratatui::restore();
    match result {
        Ok(Some(target)) => match client.confirm(&target, app.workspace_identity()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("herdr-picker: {error}");
                ExitCode::FAILURE
            }
        },
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
) -> std::io::Result<Option<model::Target>> {
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
                            return Ok(app.selected().map(|item| item.target.clone()))
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
