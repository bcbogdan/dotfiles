use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use crate::model::{Item, Tab};
use crate::profile;
use crate::provider::{directory_summary, Client, Runner, SystemRunner};

struct RefreshRequest {
    generation: u64,
    self_pane: Option<String>,
}

struct PreviewRequest {
    generation: u64,
    item: Item,
    queued_at: Option<Instant>,
}

pub enum Message {
    HostCached {
        machine: crate::machine::Machine,
        epoch: u64,
        result: crate::machine::Sources,
    },
    Catalog(Result<Vec<crate::machine::Machine>, String>),
    HostStarted {
        namespace: String,
        epoch: u64,
    },
    HostRefresh {
        machine: crate::machine::Machine,
        epoch: u64,
        result: crate::machine::Sources,
    },
    Refresh {
        generation: u64,
        result: [Result<Vec<Item>, String>; 3],
    },
    Preview {
        generation: u64,
        result: Result<String, String>,
    },
}

type PreviewSlot = Arc<(Mutex<Option<PreviewRequest>>, Condvar)>;

pub struct Runtime {
    refreshes: SyncSender<RefreshRequest>,
    preview: PreviewSlot,
    remote_previews: Mutex<std::collections::HashMap<String, PreviewSlot>>,
    preview_messages: mpsc::Sender<Message>,
    messages: Receiver<Message>,
}

impl Runtime {
    pub fn start(client: Client<SystemRunner>) -> Self {
        Self::start_workers(client, true)
    }

    #[cfg(test)]
    fn start_with<R>(client: Client<R>) -> Self
    where
        R: Runner + Clone + Send + Sync + 'static,
    {
        Self::start_workers(client, false)
    }

    fn start_workers<R>(client: Client<R>, aggregate: bool) -> Self
    where
        R: Runner + Clone + Send + Sync + 'static,
    {
        let (refresh_tx, refresh_rx) = mpsc::sync_channel(1);
        let (message_tx, message_rx) = mpsc::channel();
        let refresh_messages = message_tx.clone();
        let refresh_client = client.clone();
        if aggregate {
            let sender = message_tx.clone();
            thread::spawn(move || machine_workers(&sender));
        }
        thread::Builder::new()
            .name("herdr-picker-refresh".into())
            .spawn(move || refresh_worker(&refresh_client, &refresh_rx, &refresh_messages))
            .expect("failed to start picker refresh worker");

        let preview = Arc::new((Mutex::new(None), Condvar::new()));
        let preview_messages = message_tx.clone();
        let preview_worker_slot = Arc::clone(&preview);
        thread::Builder::new()
            .name("herdr-picker-preview".into())
            .spawn(move || preview_worker(&client, &preview_worker_slot, &message_tx))
            .expect("failed to start picker preview worker");
        Self {
            refreshes: refresh_tx,
            preview,
            remote_previews: Mutex::new(std::collections::HashMap::new()),
            preview_messages,
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
        let pending = if let Some(machine) = item.machine() {
            let Ok(mut slots) = self.remote_previews.lock() else {
                return;
            };
            slots
                .entry(machine.namespace())
                .or_insert_with(|| {
                    let slot = Arc::new((Mutex::new(None), Condvar::new()));
                    let worker_slot = slot.clone();
                    let messages = self.preview_messages.clone();
                    thread::spawn(move || {
                        preview_worker(
                            &Client::new(SystemRunner, crate::machine::installed_program("herdr")),
                            &worker_slot,
                            &messages,
                        );
                    });
                    slot
                })
                .clone()
        } else {
            self.preview.clone()
        };
        let (slot, ready) = &*pending;
        if let Ok(mut pending) = slot.lock() {
            *pending = Some(PreviewRequest {
                generation,
                item,
                queued_at: profile::timestamp(),
            });
            ready.notify_one();
        };
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
        let refresh = profile::span(
            "refresh.worker",
            || serde_json::json!({ "generation": request.generation }),
        );
        let result = client.load(request.self_pane.as_deref());
        let _ = messages.send(Message::Refresh {
            generation: request.generation,
            result,
        });
        drop(refresh);
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
        profile::record_elapsed("preview.queue_wait", request.queued_at, || {
            serde_json::json!({
                "generation": request.generation,
                "item_id": request.item.id,
            })
        });
        let preview = profile::span("preview.fallback", || {
            serde_json::json!({
                "generation": request.generation,
                "item_id": request.item.id,
                "pane_id": request.item.preview_pane,
            })
        });
        let result = match (&request.item.preview_pane, &request.item.target) {
            (pane, crate::model::Target::Remote { machine, target }) => {
                if let Some(pane) = pane {
                    crate::machine::forwarded(
                        &SystemRunner,
                        machine,
                        &[
                            "pane".into(),
                            "read".into(),
                            pane.into(),
                            "--source".into(),
                            "visible".into(),
                            "--format".into(),
                            "ansi".into(),
                        ],
                    )
                } else if let crate::model::Target::Directory { path } = target.as_ref() {
                    crate::machine::Remote::discover(&SystemRunner, machine).and_then(|remote| {
                        remote.call(crate::machine::Operation::Summary(path.clone()))
                    })
                } else {
                    Ok(String::new())
                }
            }
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
        drop(preview);
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
    app.host_sources.insert("local".into(), result.clone());
    if !app.machines.is_empty() {
        app.rebuild_hosts();
        return;
    }
    for (tab, source) in Tab::ALL.into_iter().zip(result) {
        match source {
            Ok(items) => app.set_items(tab, items),
            Err(error) => app.tabs[tab.index()].source = crate::model::LoadState::Error(error),
        }
    }
}

fn machine_workers(messages: &mpsc::Sender<Message>) {
    let cache = Arc::new(Mutex::new(crate::listing_cache::Cache::open()));
    let mut workers: std::collections::HashMap<
        String,
        (crate::machine::Machine, SyncSender<crate::machine::Machine>),
    > = std::collections::HashMap::new();
    let mut epoch = 0u64;
    loop {
        match crate::machine::catalog(&SystemRunner) {
            Ok(machines) => {
                if let Ok(mut cache) = cache.lock() {
                    cache.catalog(&machines, crate::history::unix_time());
                }
                if messages
                    .send(Message::Catalog(Ok(machines.clone())))
                    .is_err()
                {
                    return;
                }
                workers.retain(|key, (previous, _)| {
                    machines.iter().any(|machine| {
                        machine.enabled && machine.namespace() == *key && machine == previous
                    })
                });
                for machine in machines.into_iter().filter(|machine| machine.enabled) {
                    let sender = workers.entry(machine.namespace()).or_insert_with(|| {
                        epoch = epoch.wrapping_add(1);
                        let epoch = epoch;
                        let _ = messages.send(Message::HostStarted {
                            namespace: machine.namespace(),
                            epoch,
                        });
                        if let Some(sources) = cache
                            .lock()
                            .ok()
                            .and_then(|cache| cache.get(&machine, crate::history::unix_time()))
                        {
                            let _ = messages.send(Message::HostCached {
                                machine: machine.clone(),
                                epoch,
                                result: crate::machine::remote_items(&machine, sources),
                            });
                        }
                        let (sender, requests) = mpsc::sync_channel::<crate::machine::Machine>(1);
                        let messages = messages.clone();
                        let cache = cache.clone();
                        thread::spawn(move || {
                            host_worker(&SystemRunner, &requests, &messages, epoch, Some(&cache));
                        });
                        (machine.clone(), sender)
                    });
                    // One running + one queued refresh per host; no host waits for another.
                    let _ = sender.1.try_send(machine);
                }
            }
            Err(error) => {
                workers.clear();
                if let Ok(mut cache) = cache.lock() {
                    cache.catalog(&[], crate::history::unix_time());
                }
                if messages.send(Message::Catalog(Err(error))).is_err() {
                    return;
                }
            }
        }
        thread::sleep(std::time::Duration::from_secs(3));
    }
}

fn host_worker(
    runner: &impl Runner,
    requests: &Receiver<crate::machine::Machine>,
    messages: &mpsc::Sender<Message>,
    epoch: u64,
    cache: Option<&Mutex<crate::listing_cache::Cache>>,
) {
    let mut remote: Option<crate::machine::Remote> = None;
    while let Ok(machine) = requests.recv() {
        let result = (|| {
            if remote
                .as_ref()
                .is_none_or(|remote| remote.machine != machine)
            {
                remote = Some(crate::machine::Remote::discover(runner, &machine)?);
            }
            remote
                .as_ref()
                .expect("remote discovered")
                .call_with(runner, crate::machine::Operation::Load)
        })();
        let result = match result {
            Ok(sources) => sources,
            Err(error) => std::array::from_fn(|_| Err(error.clone())),
        };
        if let Some(cache) = cache {
            if let Ok(mut cache) = cache.lock() {
                cache.update(&machine, &result, crate::history::unix_time());
            }
        }
        let result = crate::machine::remote_items(&machine, result);
        if result.iter().any(Result::is_err) {
            remote = None;
        }
        if messages
            .send(Message::HostRefresh {
                machine,
                epoch,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

pub fn apply_catalog(
    app: &mut crate::model::App,
    result: Result<Vec<crate::machine::Machine>, String>,
) {
    match result {
        Ok(machines) => {
            app.catalog_error = None;
            app.host_epochs.retain(|namespace, _| {
                machines.iter().any(|machine| {
                    machine.enabled
                        && machine.namespace() == *namespace
                        && app.machines.contains(machine)
                })
            });
            app.host_sources.retain(|namespace, _| {
                namespace == "local"
                    || machines.iter().any(|machine| {
                        machine.enabled
                            && machine.namespace() == *namespace
                            && app.machines.contains(machine)
                    })
            });
            app.machines = machines;
            if app.active_machine != "local"
                && !app
                    .machines
                    .iter()
                    .any(|machine| machine.enabled && machine.namespace() == app.active_machine)
            {
                app.active_machine = "local".into();
            }
        }
        Err(error) => {
            app.catalog_error = Some(format!("Machine catalog: {error}"));
            app.host_epochs.clear();
            app.host_sources.retain(|namespace, _| namespace == "local");
        }
    }
    app.cached_hosts
        .retain(|namespace| app.host_sources.contains_key(namespace));
    app.rebuild_hosts();
}

pub fn apply_cached(
    app: &mut crate::model::App,
    machine: &crate::machine::Machine,
    epoch: u64,
    result: crate::machine::Sources,
) {
    if !app.host_sources.contains_key(&machine.namespace())
        && app.catalog_error.is_none()
        && app.host_epochs.get(&machine.namespace()) == Some(&epoch)
        && machine.enabled
        && app.machines.contains(machine)
    {
        app.cached_hosts.insert(machine.namespace());
        app.host_sources.insert(machine.namespace(), result);
        app.rebuild_hosts();
    }
}

pub fn apply_host(
    app: &mut crate::model::App,
    machine: &crate::machine::Machine,
    epoch: u64,
    result: crate::machine::Sources,
) {
    if app.catalog_error.is_none()
        && app.host_epochs.get(&machine.namespace()) == Some(&epoch)
        && machine.enabled
        && app.machines.contains(machine)
    {
        app.cached_hosts.remove(&machine.namespace());
        if app
            .selected()
            .and_then(Item::machine)
            .is_some_and(|selected| selected.namespace() == machine.namespace())
        {
            app.host_preview_revision = app.host_preview_revision.wrapping_add(1);
        }
        app.host_sources.insert(machine.namespace(), result);
        app.rebuild_hosts();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{App, LoadState};
    use crate::provider::Error;
    use std::ffi::{OsStr, OsString};
    use std::time::{Duration, Instant};

    #[test]
    fn cached_selection_is_visible_but_inert_until_live_and_failure_removes_it() {
        use crate::machine::testing::{item, machine};
        let machine = machine();
        let remote = item(crate::model::Target::Workspace { id: "w1".into() }).on_machine(&machine);
        let sources = || std::array::from_fn(|_| Ok(vec![remote.clone()]));
        let mut app = App::new(Tab::Workspaces);
        apply_catalog(&mut app, Ok(vec![machine.clone()]));
        app.host_epochs.insert(machine.namespace(), 1);
        app.cycle_machine();
        apply_cached(&mut app, &machine, 1, sources());
        assert_eq!(app.selected(), Some(&remote));
        assert!(!app.actionable(app.selected().unwrap()));
        assert!(app
            .host_errors()
            .iter()
            .any(|error| error.contains("cached · refreshing")));
        let selected = app.state().selected_id.clone();
        apply_host(&mut app, &machine, 1, sources());
        assert_eq!(app.state().selected_id, selected);
        assert!(app.actionable(app.selected().unwrap()));
        apply_cached(&mut app, &machine, 1, sources());
        assert!(app.cached_hosts.is_empty());
        apply_host(
            &mut app,
            &machine,
            1,
            std::array::from_fn(|_| Err("offline".into())),
        );
        assert!(app.selected().is_none());
        app.host_sources.remove(&machine.namespace());
        apply_cached(&mut app, &machine, 1, sources());
        assert!(!app.actionable(app.selected().unwrap()));
        apply_host(
            &mut app,
            &machine,
            1,
            std::array::from_fn(|_| Err("offline".into())),
        );
        assert!(app.selected().is_none());
        assert!(app.cached_hosts.is_empty());
    }

    #[test]
    fn host_reuses_discovery_until_failure_then_discovers_again() {
        use crate::machine::testing::{machine, payload, RemoteRunner};
        struct RunnerWithFailure {
            inner: RemoteRunner,
            loads: Mutex<usize>,
        }
        impl Runner for RunnerWithFailure {
            fn run(
                &self,
                program: &OsStr,
                args: &[OsString],
                timeout: Duration,
            ) -> Result<String, Error> {
                if program == "ssh" && payload(args)["operation"] == "Load" {
                    let mut loads = self.loads.lock().unwrap();
                    *loads += 1;
                    if *loads == 3 {
                        return Err(Error::Timeout {
                            program: "ssh".into(),
                        });
                    }
                }
                self.inner.run(program, args, timeout)
            }
        }
        let runner = RunnerWithFailure {
            inner: RemoteRunner {
                helper_reply: serde_json::to_value(std::array::from_fn::<_, 3, _>(|_| {
                    Ok::<Vec<Item>, String>(vec![])
                }))
                .unwrap(),
                ..Default::default()
            },
            loads: Mutex::new(0),
        };
        let (requests, receive) = mpsc::sync_channel(4);
        for _ in 0..4 {
            requests.send(machine()).unwrap();
        }
        drop(requests);
        let (messages, results) = mpsc::channel();
        host_worker(&runner, &receive, &messages, 1, None);
        let results: Vec<_> = results.try_iter().collect();
        assert_eq!(results.len(), 4);
        assert!(
            matches!(&results[2], Message::HostRefresh { result, .. } if result.iter().all(Result::is_err))
        );
        assert!(
            matches!(&results[3], Message::HostRefresh { result, .. } if result.iter().all(Result::is_ok))
        );
        let calls = runner.inner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|(program, args)| program == "ssh" && payload(args)["operation"] == "Hello")
                .count(),
            2
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(_, args)| args.get(2).is_some_and(|arg| arg == "plugin"))
                .count(),
            2
        );
    }

    #[test]
    fn per_machine_errors_disable_only_that_host_and_old_epochs_are_ignored() {
        use crate::machine::testing::{item, machine};
        let machine = machine();
        let local = item(crate::model::Target::Workspace { id: "w1".into() });
        let remote = local.clone().on_machine(&machine);
        let mut app = App::new(Tab::Workspaces);
        apply_refresh(
            &mut app,
            0,
            std::array::from_fn(|_| Ok(vec![local.clone()])),
        );
        apply_catalog(&mut app, Ok(vec![machine.clone()]));
        app.host_epochs.insert(machine.namespace(), 2);
        apply_host(
            &mut app,
            &machine,
            2,
            std::array::from_fn(|_| Ok(vec![remote.clone()])),
        );
        assert_eq!(app.filtered(), vec![&local]);
        app.cycle_machine();
        assert_eq!(app.filtered(), vec![&remote]);
        apply_host(
            &mut app,
            &machine,
            2,
            std::array::from_fn(|_| Err("SSH unavailable".into())),
        );
        assert!(app.filtered().is_empty());
        assert!(app
            .host_errors()
            .iter()
            .any(|error| error.contains("goblin: SSH unavailable")));
        apply_host(
            &mut app,
            &machine,
            1,
            std::array::from_fn(|_| Ok(vec![remote.clone()])),
        );
        assert!(app.filtered().is_empty());
        app.cycle_machine();
        assert_eq!(app.filtered(), vec![&local]);
        assert!(app.host_errors().is_empty());
        app.cycle_machine();
        let mut disabled = machine.clone();
        disabled.enabled = false;
        apply_catalog(&mut app, Ok(vec![disabled]));
        apply_host(
            &mut app,
            &machine,
            2,
            std::array::from_fn(|_| Ok(vec![remote.clone()])),
        );
        assert_eq!(app.filtered(), vec![&local]);
        assert_eq!(app.active_machine, "local");
        app.cycle_machine();
        assert_eq!(app.active_machine, "local");
    }

    #[test]
    fn unreachable_remote_worker_does_not_delay_local_refresh() {
        #[derive(Clone)]
        struct SlowHost(Arc<(Mutex<bool>, Condvar)>, mpsc::Sender<()>);
        impl Runner for SlowHost {
            fn run(&self, _: &OsStr, _: &[OsString], _: Duration) -> Result<String, Error> {
                self.1.send(()).unwrap();
                let (lock, ready) = &*self.0;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = ready.wait(released).unwrap();
                }
                Err(Error::Timeout {
                    program: "remote".into(),
                })
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let runner = SlowHost(gate.clone(), entered_tx);
        let (remote_tx, remote_rx) = mpsc::sync_channel(1);
        let (messages, results) = mpsc::channel();
        let host_messages = messages.clone();
        let remote =
            thread::spawn(move || host_worker(&runner, &remote_rx, &host_messages, 1, None));
        remote_tx.send(crate::machine::testing::machine()).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (local_tx, local_rx) = mpsc::sync_channel(1);
        local_tx
            .send(RefreshRequest {
                generation: 1,
                self_pane: None,
            })
            .unwrap();
        drop(local_tx);
        refresh_worker(
            &Client::new(RecordingRunner::default(), "local".into()),
            &local_rx,
            &messages,
        );
        assert!(matches!(
            results.recv_timeout(Duration::from_secs(1)).unwrap(),
            Message::Refresh { generation: 1, .. }
        ));
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        drop(remote_tx);
        remote.join().unwrap();
        assert!(matches!(
            results.recv_timeout(Duration::from_secs(1)).unwrap(),
            Message::HostRefresh { .. }
        ));
    }

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
            if std::path::Path::new(program).file_name() == Some(OsStr::new("zoxide")) {
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
                queued_at: None,
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
