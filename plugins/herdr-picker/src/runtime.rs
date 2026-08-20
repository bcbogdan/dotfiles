use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::model::{Item, Tab};
use crate::provider::{directory_summary, Client, Runner, SystemRunner};

struct RefreshRequest {
    generation: u64,
    self_pane: Option<String>,
}

struct PreviewRequest {
    generation: u64,
    item: Item,
}

pub enum Message {
    Refresh {
        generation: u64,
        result: [Result<Vec<Item>, String>; 3],
    },
    Preview {
        generation: u64,
        result: Result<String, String>,
    },
}

pub struct Runtime {
    refreshes: SyncSender<RefreshRequest>,
    preview: Arc<(Mutex<Option<PreviewRequest>>, Condvar)>,
    messages: Receiver<Message>,
}

impl Runtime {
    pub fn start(client: Client<SystemRunner>) -> Self {
        Self::start_with(client)
    }

    fn start_with<R>(client: Client<R>) -> Self
    where
        R: Runner + Clone + Send + Sync + 'static,
    {
        let (refresh_tx, refresh_rx) = mpsc::sync_channel(1);
        let (message_tx, message_rx) = mpsc::channel();
        let refresh_messages = message_tx.clone();
        let refresh_client = client.clone();
        thread::Builder::new()
            .name("herdr-picker-refresh".into())
            .spawn(move || refresh_worker(&refresh_client, &refresh_rx, &refresh_messages))
            .expect("failed to start picker refresh worker");

        let preview = Arc::new((Mutex::new(None), Condvar::new()));
        let preview_worker_slot = Arc::clone(&preview);
        thread::Builder::new()
            .name("herdr-picker-preview".into())
            .spawn(move || preview_worker(&client, &preview_worker_slot, &message_tx))
            .expect("failed to start picker preview worker");
        Self {
            refreshes: refresh_tx,
            preview,
            messages: message_rx,
        }
    }

    pub fn refresh(&self, generation: u64, self_pane: Option<String>) {
        let _ = self.refreshes.try_send(RefreshRequest {
            generation,
            self_pane,
        });
    }

    pub fn preview(&self, generation: u64, item: Item) {
        let (slot, ready) = &*self.preview;
        if let Ok(mut pending) = slot.lock() {
            *pending = Some(PreviewRequest { generation, item });
            ready.notify_one();
        }
    }

    pub fn try_recv(&self) -> Option<Message> {
        self.messages.try_recv().ok()
    }
}

fn refresh_worker<R: Runner>(
    client: &Client<R>,
    requests: &Receiver<RefreshRequest>,
    messages: &mpsc::Sender<Message>,
) {
    while let Ok(request) = requests.recv() {
        let result = client.load(request.self_pane.as_deref());
        let _ = messages.send(Message::Refresh {
            generation: request.generation,
            result,
        });
    }
}

fn preview_worker<R: Runner>(
    client: &Client<R>,
    pending: &(Mutex<Option<PreviewRequest>>, Condvar),
    messages: &mpsc::Sender<Message>,
) {
    loop {
        let request = {
            let (slot, ready) = pending;
            let Ok(mut slot) = slot.lock() else { return };
            while slot.is_none() {
                let Ok(next) = ready.wait(slot) else { return };
                slot = next;
            }
            slot.take().expect("preview request was present")
        };
        let result = match (&request.item.preview_pane, &request.item.target) {
            (Some(pane), _) => client.snapshot(pane).map_err(|error| error.to_string()),
            (None, crate::model::Target::Directory { path }) => {
                directory_summary(&SystemRunner, path).map_err(|error| error.to_string())
            }
            (None, _) => Ok(String::new()),
        };
        let _ = messages.send(Message::Preview {
            generation: request.generation,
            result,
        });
    }
}

#[derive(Debug, Default)]
pub struct RefreshScheduler {
    in_flight: bool,
    deferred: bool,
}

impl RefreshScheduler {
    pub fn demand(&mut self, app: &mut crate::model::App) -> Option<u64> {
        if self.in_flight {
            self.deferred = true;
            return None;
        }
        Some(self.dispatch(app))
    }

    pub fn complete(&mut self, app: &mut crate::model::App) -> Option<u64> {
        self.in_flight = false;
        if std::mem::take(&mut self.deferred) {
            Some(self.dispatch(app))
        } else {
            None
        }
    }

    fn dispatch(&mut self, app: &mut crate::model::App) -> u64 {
        self.in_flight = true;
        app.refresh_generation = app.refresh_generation.wrapping_add(1);
        app.refresh_generation
    }
}

pub fn apply_refresh(
    app: &mut crate::model::App,
    generation: u64,
    result: [Result<Vec<Item>, String>; 3],
) {
    if generation != app.refresh_generation {
        return;
    }
    for (tab, source) in Tab::ALL.into_iter().zip(result) {
        match source {
            Ok(items) => app.set_items(tab, items),
            Err(error) => app.tabs[tab.index()].source = crate::model::LoadState::Error(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{App, LoadState};
    use crate::provider::Error;
    use std::ffi::{OsStr, OsString};
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct BlockingRunner {
        gate: Arc<(Mutex<(bool, bool)>, Condvar)>,
    }

    #[derive(Clone, Default)]
    struct RecordingRunner {
        calls: Arc<Mutex<Vec<Vec<OsString>>>>,
    }

    impl Runner for RecordingRunner {
        fn run(
            &self,
            _program: &OsStr,
            args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, Error> {
            self.calls.lock().unwrap().push(args.to_vec());
            Ok("{}".into())
        }
    }

    impl Runner for BlockingRunner {
        fn run(
            &self,
            program: &OsStr,
            args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, Error> {
            if program == OsStr::new("zoxide") {
                return Ok(String::new());
            }
            if args.first().is_some_and(|arg| arg == "workspace") {
                let (state, changed) = &*self.gate;
                let mut state = state.lock().unwrap();
                state.0 = true;
                changed.notify_all();
                while !state.1 {
                    state = changed.wait(state).unwrap();
                }
                return Ok(r#"{"result":{"workspaces":[]}}"#.into());
            }
            if args.first().is_some_and(|arg| arg == "pane")
                && args.get(1).is_some_and(|arg| arg == "read")
            {
                return Ok("snapshot".into());
            }
            let field = if args.first().is_some_and(|arg| arg == "pane") {
                "panes"
            } else {
                "agents"
            };
            Ok(format!(r#"{{"result":{{"{field}":[]}}}}"#))
        }
    }

    #[test]
    fn repeated_refresh_demand_is_one_in_flight_plus_one_deferred() {
        let mut app = App::new(Tab::Workspaces);
        let mut scheduler = RefreshScheduler::default();
        assert_eq!(scheduler.demand(&mut app), Some(1));
        for _ in 0..100 {
            assert_eq!(scheduler.demand(&mut app), None);
        }
        assert_eq!(app.refresh_generation, 1);
        assert_eq!(scheduler.complete(&mut app), Some(2));
        assert_eq!(scheduler.complete(&mut app), None);
    }

    #[test]
    fn slow_initial_refresh_remains_current_and_is_applied() {
        let mut app = App::new(Tab::Workspaces);
        let mut scheduler = RefreshScheduler::default();
        let generation = scheduler.demand(&mut app).unwrap();
        assert_eq!(scheduler.demand(&mut app), None);
        apply_refresh(
            &mut app,
            generation,
            std::array::from_fn(|_| Ok(Vec::new())),
        );
        assert_eq!(app.tabs[0].source, LoadState::Ready(Vec::new()));
    }

    #[test]
    fn stale_refresh_does_not_replace_newer_data() {
        let mut app = App::new(Tab::Workspaces);
        app.refresh_generation = 2;
        apply_refresh(&mut app, 1, std::array::from_fn(|_| Ok(Vec::new())));
        assert_eq!(app.tabs[0].source, LoadState::Loading);
    }

    #[test]
    fn latest_preview_slot_is_bounded() {
        let slot = Mutex::new(None);
        for generation in 1..=100 {
            *slot.lock().unwrap() = Some(PreviewRequest {
                generation,
                item: test_item(),
            });
        }
        let pending = slot.lock().unwrap();
        assert_eq!(pending.as_ref().unwrap().generation, 100);
    }

    #[test]
    fn preview_completes_while_slow_refresh_is_in_flight() {
        let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let runtime = Runtime::start_with(Client::new(
            BlockingRunner {
                gate: Arc::clone(&gate),
            },
            OsString::from("herdr"),
        ));
        runtime.refresh(1, None);
        {
            let (state, changed) = &*gate;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
        }
        let mut item = test_item();
        item.preview_pane = Some("p1".into());
        runtime.preview(7, item);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut preview_arrived = false;
        while Instant::now() < deadline {
            if matches!(
                runtime.try_recv(),
                Some(Message::Preview { generation: 7, .. })
            ) {
                preview_arrived = true;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let (state, changed) = &*gate;
        state.lock().unwrap().1 = true;
        changed.notify_all();
        assert!(preview_arrived);
    }

    #[test]
    fn failed_refresh_keeps_workspace_identity_for_zoxide_confirmation() {
        let mut app = App::new(Tab::Workspaces);
        app.refresh_generation = 1;
        let mut workspace = test_item();
        workspace.match_paths = vec![crate::model::canonicalish(std::path::Path::new(
            "/tmp/project",
        ))];
        apply_refresh(
            &mut app,
            1,
            [Ok(vec![workspace]), Ok(Vec::new()), Ok(Vec::new())],
        );
        app.refresh_generation = 2;
        apply_refresh(
            &mut app,
            2,
            std::array::from_fn(|_| Err("transient failure".into())),
        );
        assert!(matches!(app.tabs[0].source, LoadState::Error(_)));

        let runner = RecordingRunner::default();
        let calls = Arc::clone(&runner.calls);
        Client::new(runner, OsString::from("herdr"))
            .confirm(
                &crate::model::Target::Directory {
                    path: "/tmp/project".into(),
                },
                app.workspace_identity(),
            )
            .unwrap();
        assert_eq!(
            calls.lock().unwrap()[0],
            ["workspace", "focus", "w1"].map(OsString::from)
        );
    }

    #[test]
    fn initial_failed_refresh_has_no_workspace_identity() {
        let mut app = App::new(Tab::Workspaces);
        app.refresh_generation = 1;
        apply_refresh(
            &mut app,
            1,
            std::array::from_fn(|_| Err("initial failure".into())),
        );
        assert!(app.workspace_identity().is_empty());
    }

    fn test_item() -> Item {
        Item {
            id: "w1".into(),
            label: "one".into(),
            detail: String::new(),
            search: String::new(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: crate::model::Target::Workspace { id: "w1".into() },
        }
    }
}
