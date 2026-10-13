use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

const CHANNEL_CAPACITY: usize = 4_096;
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

static STATE: OnceLock<State> = OnceLock::new();

struct State {
    started_at: Instant,
    sender: Option<SyncSender<Message>>,
    dropped: Arc<AtomicU64>,
    accepting: AtomicBool,
    submit_gate: Mutex<()>,
    writer: Mutex<Option<thread::JoinHandle<()>>>,
}

enum Message {
    Record(Record),
    Shutdown(mpsc::Sender<Result<(), String>>),
}

#[derive(Serialize)]
struct Record {
    timestamp_us: u64,
    duration_us: Option<u64>,
    event: &'static str,
    thread: String,
    fields: Value,
}

pub struct Span {
    event: &'static str,
    started_at: Option<Instant>,
    fields: Value,
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(started_at) = self.started_at else {
            return;
        };
        submit(
            self.event,
            Some(started_at.elapsed()),
            std::mem::take(&mut self.fields),
        );
    }
}

pub fn initialize() -> Result<Option<PathBuf>, String> {
    if STATE.get().is_some() {
        return Ok(None);
    }
    let Some(path) = output_path() else {
        let _ = STATE.set(State {
            started_at: Instant::now(),
            sender: None,
            dropped: Arc::new(AtomicU64::new(0)),
            accepting: AtomicBool::new(false),
            submit_gate: Mutex::new(()),
            writer: Mutex::new(None),
        });
        return Ok(None);
    };
    let parent = path
        .parent()
        .ok_or_else(|| "profile output path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let file = secure_create(&path).map_err(|error| error.to_string())?;
    let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let writer_dropped = Arc::clone(&dropped);
    let handle = thread::Builder::new()
        .name("herdr-picker-profile".into())
        .spawn(move || writer(BufWriter::new(file), &receiver, &writer_dropped))
        .map_err(|error| error.to_string())?;
    STATE
        .set(State {
            started_at: Instant::now(),
            sender: Some(sender),
            dropped,
            accepting: AtomicBool::new(true),
            submit_gate: Mutex::new(()),
            writer: Mutex::new(Some(handle)),
        })
        .map_err(|_| "profiler was already initialized".to_string())?;
    record("profile.started", || serde_json::json!({ "path": path }));
    Ok(Some(path))
}

pub fn enabled() -> bool {
    STATE
        .get()
        .and_then(|state| state.sender.as_ref())
        .is_some()
}

pub fn timestamp() -> Option<Instant> {
    enabled().then(Instant::now)
}

pub fn span(event: &'static str, fields: impl FnOnce() -> Value) -> Span {
    if enabled() {
        let fields = fields();
        Span {
            event,
            started_at: Some(Instant::now()),
            fields,
        }
    } else {
        Span {
            event,
            started_at: None,
            fields: Value::Null,
        }
    }
}

pub fn record(event: &'static str, fields: impl FnOnce() -> Value) {
    if enabled() {
        submit(event, None, fields());
    }
}

pub fn record_elapsed(
    event: &'static str,
    started_at: Option<Instant>,
    fields: impl FnOnce() -> Value,
) {
    if let Some(started_at) = started_at {
        submit(event, Some(started_at.elapsed()), fields());
    }
}

pub fn shutdown() -> Result<(), String> {
    let Some(state) = STATE.get() else {
        return Ok(());
    };
    let Some(sender) = &state.sender else {
        return Ok(());
    };
    {
        let _gate = state
            .submit_gate
            .lock()
            .map_err(|error| error.to_string())?;
        state.accepting.store(false, Ordering::Release);
    }
    let (complete, completed) = mpsc::channel();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut message = Message::Shutdown(complete);
    loop {
        match sender.try_send(message) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(pending)) if Instant::now() < deadline => {
                message = pending;
                thread::sleep(Duration::from_millis(5));
            }
            Err(mpsc::TrySendError::Full(_)) => {
                return Err("timed out waiting to shut down profiler".into());
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err("profiler writer stopped unexpectedly".into());
            }
        }
    }
    let result = completed
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| "timed out flushing profiler output".to_string())?;
    if let Some(handle) = state
        .writer
        .lock()
        .map_err(|error| error.to_string())?
        .take()
    {
        handle
            .join()
            .map_err(|_| "profiler writer panicked".to_string())?;
    }
    result
}

fn submit(event: &'static str, duration: Option<Duration>, fields: Value) {
    let Some(state) = STATE.get() else { return };
    let Some(sender) = &state.sender else { return };
    let Ok(_gate) = state.submit_gate.lock() else {
        return;
    };
    if !state.accepting.load(Ordering::Acquire) {
        return;
    }
    let record = Record {
        timestamp_us: micros(state.started_at.elapsed()),
        duration_us: duration.map(micros),
        event,
        thread: thread::current().name().unwrap_or("unnamed").to_string(),
        fields,
    };
    if sender.try_send(Message::Record(record)).is_err() {
        state.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn writer(mut output: BufWriter<File>, receiver: &mpsc::Receiver<Message>, dropped: &AtomicU64) {
    let mut failure = None;
    loop {
        match receiver.recv_timeout(FLUSH_INTERVAL) {
            Ok(Message::Record(record)) => {
                if failure.is_none() {
                    failure = write_record(&mut output, &record).err();
                }
            }
            Ok(Message::Shutdown(complete)) => {
                let count = dropped.swap(0, Ordering::Relaxed);
                if count > 0 && failure.is_none() {
                    let record = Record {
                        timestamp_us: 0,
                        duration_us: None,
                        event: "profile.dropped",
                        thread: "herdr-picker-profile".into(),
                        fields: serde_json::json!({ "count": count }),
                    };
                    failure = write_record(&mut output, &record).err();
                }
                if failure.is_none() {
                    failure = output.flush().map_err(|error| error.to_string()).err();
                }
                let _ = complete.send(failure.map_or(Ok(()), Err));
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if failure.is_none() {
                    failure = output.flush().map_err(|error| error.to_string()).err();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = output.flush();
                return;
            }
        }
    }
}

fn write_record(output: &mut BufWriter<File>, record: &Record) -> Result<(), String> {
    serde_json::to_writer(&mut *output, record).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())
}

fn output_path() -> Option<PathBuf> {
    let value = std::env::var_os("HERDR_PICKER_PROFILE")?;
    if value.is_empty() || value == "0" {
        return None;
    }
    if value == "1" {
        let directory = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
            .map_or_else(std::env::temp_dir, PathBuf::from);
        return Some(directory.join(format!("picker-profile-{}.jsonl", std::process::id())));
    }
    Some(PathBuf::from(value))
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn secure_create(path: &std::path::Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn secure_create(path: &std::path::Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_flushes_json_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.jsonl");
        let output = BufWriter::new(secure_create(&path).unwrap());
        let (sender, receiver) = mpsc::sync_channel(2);
        let dropped = Arc::new(AtomicU64::new(0));
        let writer_dropped = Arc::clone(&dropped);
        let handle = thread::spawn(move || writer(output, &receiver, &writer_dropped));
        sender
            .send(Message::Record(Record {
                timestamp_us: 12,
                duration_us: Some(34),
                event: "test.event",
                thread: "test".into(),
                fields: serde_json::json!({ "answer": 42 }),
            }))
            .unwrap();
        let (complete, completed) = mpsc::channel();
        sender.send(Message::Shutdown(complete)).unwrap();
        completed.recv().unwrap().unwrap();
        drop(sender);
        handle.join().unwrap();

        let line = fs::read_to_string(path).unwrap();
        let record: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["event"], "test.event");
        assert_eq!(record["duration_us"], 34);
        assert_eq!(record["fields"]["answer"], 42);
    }
}
