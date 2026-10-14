use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;

use crate::model::{PreviewIdentity, PreviewState};
use crate::profile;

const MAX_FRAME_LINE: usize = 2 * 1024 * 1024;
const FALLBACK_REQUEST_DELAY: Duration = Duration::from_millis(120);
const MAX_FRAME_DIMENSION: u16 = 1_000;
const MAX_FRAME_CELLS: u32 = 500_000;

pub struct LivePreview {
    child: Option<Child>,
    pending: Option<Arc<Mutex<PendingLive>>>,
    identity: Option<PreviewIdentity>,
    dimensions: (u16, u16),
    first_frame_at: Option<Instant>,
    stream_active: bool,
}

impl LivePreview {
    pub fn new() -> Self {
        Self {
            child: None,
            pending: None,
            identity: None,
            dimensions: (0, 0),
            first_frame_at: None,
            stream_active: false,
        }
    }

    pub fn select(
        &mut self,
        identity: Option<&PreviewIdentity>,
        dimensions: (u16, u16),
    ) -> LiveStart {
        if self.can_reuse(identity, dimensions) {
            return LiveStart::Started;
        }
        self.stop();
        self.identity = identity.cloned();
        self.dimensions = dimensions;
        self.first_frame_at = profile::timestamp();
        let Some(target) = identity
            .filter(|identity| identity.target.machine().is_none())
            .and_then(|identity| identity.preview_pane.as_deref())
        else {
            return LiveStart::NotApplicable;
        };
        match start(target, dimensions) {
            Ok((child, pending)) => {
                self.child = Some(child);
                self.pending = Some(pending);
                self.stream_active = true;
                LiveStart::Started
            }
            Err(error) => LiveStart::Unavailable(error),
        }
    }

    pub fn poll(&mut self) -> Option<Result<String, String>> {
        let frame = self.pending.as_ref()?.lock().ok()?.take()?;
        profile::record_elapsed("preview.frame_delivery", frame.ready_at, || {
            serde_json::json!({
                "dimensions": [self.dimensions.0, self.dimensions.1],
                "item_id": self.identity.as_ref().map(|identity| identity.item_id.as_str()),
            })
        });
        let result = frame.result;
        if result.is_err() {
            self.stream_active = false;
        }
        profile::record_elapsed("preview.first_frame", self.first_frame_at.take(), || {
            serde_json::json!({
                "dimensions": [self.dimensions.0, self.dimensions.1],
                "item_id": self.identity.as_ref().map(|identity| identity.item_id.as_str()),
                "success": result.is_ok(),
            })
        });
        Some(result)
    }

    pub fn is_active(&self) -> bool {
        self.child.is_some() && self.stream_active
    }

    fn can_reuse(&self, identity: Option<&PreviewIdentity>, dimensions: (u16, u16)) -> bool {
        self.identity.as_ref() == identity && self.dimensions == dimensions && self.is_active()
    }

    fn stop(&mut self) {
        let stop = profile::span("preview.observer_stop", || {
            serde_json::json!({
                "item_id": self.identity.as_ref().map(|identity| identity.item_id.as_str()),
                "running": self.child.is_some(),
            })
        });
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.pending = None;
        self.first_frame_at = None;
        self.stream_active = false;
        drop(stop);
    }
}

impl Drop for LivePreview {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LiveStart {
    Started,
    NotApplicable,
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveState {
    Waiting,
    Ready,
    Unavailable,
    NotApplicable,
}

pub struct Arbiter {
    generation: u64,
    started_at: Instant,
    live: LiveState,
    fallback_requested: bool,
    fallback_visible: bool,
}

impl Arbiter {
    pub fn new() -> Self {
        Self {
            generation: 0,
            started_at: Instant::now(),
            live: LiveState::NotApplicable,
            fallback_requested: false,
            fallback_visible: false,
        }
    }

    pub fn reset(&mut self, generation: u64, start: &LiveStart, now: Instant) {
        self.generation = generation;
        self.started_at = now;
        self.fallback_requested = false;
        self.fallback_visible = false;
        self.live = match start {
            LiveStart::Started => LiveState::Waiting,
            LiveStart::NotApplicable => LiveState::NotApplicable,
            LiveStart::Unavailable(_) => LiveState::Unavailable,
        };
    }

    pub fn fallback(
        &mut self,
        generation: u64,
        result: Result<String, String>,
    ) -> Option<PreviewState> {
        if generation != self.generation || self.live == LiveState::Ready {
            return None;
        }
        match result {
            Ok(snapshot) => {
                self.fallback_visible = true;
                Some(PreviewState::Ready(snapshot))
            }
            Err(error) => Some(PreviewState::Error(error)),
        }
    }

    pub fn live(&mut self, result: Result<String, String>) -> Option<PreviewState> {
        match result {
            Ok(frame) => {
                self.live = LiveState::Ready;
                self.fallback_visible = false;
                Some(PreviewState::Ready(frame))
            }
            Err(error) => {
                if self.live == LiveState::Ready {
                    return None;
                }
                self.live = LiveState::Unavailable;
                (!self.fallback_visible).then_some(PreviewState::Error(error))
            }
        }
    }

    pub fn fallback_due(&mut self, now: Instant) -> bool {
        if self.fallback_requested || self.live == LiveState::Ready {
            return false;
        }
        let due = self.live != LiveState::Waiting
            || now.saturating_duration_since(self.started_at) >= FALLBACK_REQUEST_DELAY;
        self.fallback_requested = due;
        due
    }
}

struct LiveFrame {
    result: Result<String, String>,
    ready_at: Option<Instant>,
}

#[derive(Default)]
struct PendingLive {
    frame: Option<LiveFrame>,
    terminal: Option<LiveFrame>,
}

impl PendingLive {
    fn take(&mut self) -> Option<LiveFrame> {
        self.frame.take().or_else(|| self.terminal.take())
    }
}

fn start(
    target: &str,
    (columns, rows): (u16, u16),
) -> Result<(Child, Arc<Mutex<PendingLive>>), String> {
    let herdr = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let spawn = profile::span("preview.observer_spawn", || {
        serde_json::json!({
            "target": target,
            "dimensions": [columns, rows],
        })
    });
    let mut child = Command::new(herdr)
        .args(["terminal", "session", "observe", target, "--cols"])
        .arg(columns.to_string())
        .arg("--rows")
        .arg(rows.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("live preview unavailable: {error}"))?;
    drop(spawn);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "live preview stdout unavailable".to_string())?;
    let pending = Arc::new(Mutex::new(PendingLive::default()));
    let observer_pending = Arc::clone(&pending);
    thread::Builder::new()
        .name("herdr-picker-observer".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            let mut state = StreamState::new(Some((columns, rows)));
            loop {
                line.clear();
                match read_limited_line(&mut reader, &mut line) {
                    Ok(0) => {
                        publish(
                            &observer_pending,
                            LiveFrame {
                                result: Err("live preview stream ended".into()),
                                ready_at: profile::timestamp(),
                            },
                        );
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        publish(
                            &observer_pending,
                            LiveFrame {
                                result: Err(error),
                                ready_at: profile::timestamp(),
                            },
                        );
                        break;
                    }
                }
                let decode = profile::span(
                    "preview.frame_json",
                    || serde_json::json!({ "bytes": line.len() }),
                );
                let record =
                    serde_json::from_slice::<Record<'_>>(&line).map_err(|error| error.to_string());
                drop(decode);
                let result = record.and_then(|record| state.apply(&record));
                match result {
                    Ok(Some(ansi)) => {
                        publish(
                            &observer_pending,
                            LiveFrame {
                                result: Ok(ansi),
                                ready_at: profile::timestamp(),
                            },
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        publish(
                            &observer_pending,
                            LiveFrame {
                                result: Err(error),
                                ready_at: profile::timestamp(),
                            },
                        );
                        break;
                    }
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok((child, pending))
}

fn publish(pending: &Mutex<PendingLive>, frame: LiveFrame) {
    if let Ok(mut pending) = pending.lock() {
        if frame.result.is_ok() {
            pending.frame = Some(frame);
        } else {
            pending.terminal = Some(frame);
        }
    }
}

fn read_limited_line(reader: &mut impl BufRead, output: &mut Vec<u8>) -> Result<usize, String> {
    loop {
        let available = reader.fill_buf().map_err(|error| error.to_string())?;
        if available.is_empty() {
            return Ok(output.len());
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len().saturating_add(consumed) > MAX_FRAME_LINE {
            return Err(format!("terminal frame exceeds {MAX_FRAME_LINE} bytes"));
        }
        output.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if output.last() == Some(&b'\n') {
            return Ok(output.len());
        }
    }
}

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
    #[serde(borrow)]
    bytes: Option<&'a str>,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    height: u16,
    #[serde(default, rename = "seq")]
    sequence: u64,
    #[serde(default)]
    width: u16,
    #[serde(default, borrow)]
    encoding: Option<&'a str>,
}

struct StreamState {
    parser: Option<vt100::Parser>,
    last_sequence: Option<u64>,
    expected_dimensions: Option<(u16, u16)>,
}

impl StreamState {
    fn new(expected_dimensions: Option<(u16, u16)>) -> Self {
        Self {
            parser: None,
            last_sequence: None,
            expected_dimensions,
        }
    }

    fn apply(&mut self, frame: &Record<'_>) -> Result<Option<String>, String> {
        if frame.kind != "terminal.frame" {
            return Ok(None);
        }
        if frame.encoding != Some("ansi") {
            return Err("terminal frame encoding is not ANSI".into());
        }
        if frame.width == 0 || frame.height == 0 {
            return Err("terminal frame has zero dimensions".into());
        }
        if frame.width > MAX_FRAME_DIMENSION
            || frame.height > MAX_FRAME_DIMENSION
            || u32::from(frame.width) * u32::from(frame.height) > MAX_FRAME_CELLS
        {
            return Err("terminal frame dimensions exceed preview limits".into());
        }
        if self
            .expected_dimensions
            .is_some_and(|expected| expected != (frame.width, frame.height))
        {
            return Err("terminal frame dimensions do not match preview geometry".into());
        }
        if self
            .last_sequence
            .is_some_and(|sequence| frame.sequence <= sequence)
        {
            return Err("terminal frame sequence did not advance".into());
        }
        if !frame.full
            && self
                .last_sequence
                .is_none_or(|sequence| frame.sequence != sequence + 1)
        {
            return Err("terminal frame sequence gap".into());
        }
        if frame.full {
            self.parser = Some(vt100::Parser::new(frame.height, frame.width, 0));
        }
        let parser = self
            .parser
            .as_mut()
            .ok_or_else(|| "incremental frame before full frame".to_string())?;
        let decode = profile::span("preview.frame_base64", || {
            serde_json::json!({
                "encoded_bytes": frame.bytes.map_or(0, str::len),
                "full": frame.full,
                "sequence": frame.sequence,
            })
        });
        let bytes = STANDARD
            .decode(frame.bytes.unwrap_or_default())
            .map_err(|error| error.to_string())?;
        drop(decode);
        let process = profile::span("preview.frame_vt100", || {
            serde_json::json!({
                "bytes": bytes.len(),
                "dimensions": [frame.width, frame.height],
                "full": frame.full,
                "sequence": frame.sequence,
            })
        });
        parser.process(&bytes);
        drop(process);
        self.last_sequence = Some(frame.sequence);
        let serialize = profile::span("preview.frame_serialize", || {
            serde_json::json!({
                "dimensions": [frame.width, frame.height],
                "sequence": frame.sequence,
            })
        });
        let formatted = String::from_utf8_lossy(&parser.screen().contents_formatted()).into_owned();
        drop(serialize);
        Ok(Some(formatted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(sequence: u64, full: bool, bytes: &'static str) -> Record<'static> {
        Record {
            kind: "terminal.frame",
            bytes: Some(bytes),
            full,
            height: 2,
            sequence,
            width: 10,
            encoding: Some("ansi"),
        }
    }

    #[test]
    fn full_and_incremental_frames_require_contiguous_sequences() {
        let mut state = StreamState::new(None);
        assert!(state.apply(&record(1, true, "aGVsbG8=")).is_ok());
        assert!(state.apply(&record(2, false, "d29ybGQ=")).is_ok());
        assert_eq!(
            state.apply(&record(4, false, "eA==")).unwrap_err(),
            "terminal frame sequence gap"
        );
    }

    #[test]
    fn stale_full_frame_is_rejected() {
        let mut state = StreamState::new(None);
        state.apply(&record(2, true, "bmV3")).unwrap();
        assert_eq!(
            state.apply(&record(1, true, "b2xk")).unwrap_err(),
            "terminal frame sequence did not advance"
        );
    }

    #[test]
    fn oversized_line_is_rejected_before_json_parsing() {
        let input = vec![b'x'; MAX_FRAME_LINE + 1];
        let mut reader = BufReader::new(input.as_slice());
        assert!(read_limited_line(&mut reader, &mut Vec::new())
            .unwrap_err()
            .contains("exceeds"));
    }

    #[test]
    fn huge_frame_dimensions_are_rejected_before_parser_construction() {
        let mut state = StreamState::new(None);
        let mut frame = record(1, true, "eA==");
        frame.width = MAX_FRAME_DIMENSION + 1;
        assert_eq!(
            state.apply(&frame).unwrap_err(),
            "terminal frame dimensions exceed preview limits"
        );
    }

    #[test]
    fn fallback_becomes_due_at_120ms_exactly_once() {
        let now = Instant::now();
        let mut arbiter = Arbiter::new();
        arbiter.reset(1, &LiveStart::Started, now);

        assert!(!arbiter.fallback_due(now));
        assert!(!arbiter.fallback_due(
            (now + FALLBACK_REQUEST_DELAY)
                .checked_sub(Duration::from_millis(1))
                .unwrap()
        ));
        assert!(arbiter.fallback_due(now + FALLBACK_REQUEST_DELAY));
        assert!(!arbiter.fallback_due(now + FALLBACK_REQUEST_DELAY + Duration::from_secs(1)));
    }

    #[test]
    fn fallback_result_is_released_immediately_after_arrival() {
        let now = Instant::now();
        let mut arbiter = Arbiter::new();
        arbiter.reset(3, &LiveStart::Started, now);

        assert_eq!(
            arbiter.fallback(3, Ok("snapshot".into())),
            Some(PreviewState::Ready("snapshot".into()))
        );
    }

    #[test]
    fn live_error_does_not_replace_a_visible_fallback() {
        let now = Instant::now();
        let mut arbiter = Arbiter::new();
        arbiter.reset(3, &LiveStart::Started, now);
        assert_eq!(
            arbiter.fallback(3, Ok("snapshot".into())),
            Some(PreviewState::Ready("snapshot".into()))
        );
        assert_eq!(arbiter.live(Err("observer ended".into())), None);
    }

    #[test]
    fn pending_live_keeps_latest_frame_and_terminal_notification() {
        let pending = Mutex::new(PendingLive::default());
        publish(
            &pending,
            LiveFrame {
                result: Ok("first".into()),
                ready_at: None,
            },
        );
        publish(
            &pending,
            LiveFrame {
                result: Ok("latest".into()),
                ready_at: None,
            },
        );
        publish(
            &pending,
            LiveFrame {
                result: Err("ended".into()),
                ready_at: None,
            },
        );

        let mut pending = pending.lock().unwrap();
        assert_eq!(pending.take().unwrap().result, Ok("latest".into()));
        assert_eq!(pending.take().unwrap().result, Err("ended".into()));
        assert!(pending.take().is_none());
    }

    #[test]
    fn inactive_observer_is_not_reusable() {
        let preview = LivePreview {
            child: None,
            pending: None,
            identity: Some(PreviewIdentity {
                item_id: "w1".into(),
                preview_pane: Some("w1:p1".into()),
                target: crate::model::Target::Workspace { id: "w1".into() },
            }),
            dimensions: (80, 24),
            first_frame_at: None,
            stream_active: false,
        };
        assert!(!preview.is_active());
        assert!(!preview.can_reuse(preview.identity.as_ref(), (80, 24)));
    }

    #[test]
    fn live_frame_wins_over_a_later_fallback_result() {
        let now = Instant::now();
        let mut arbiter = Arbiter::new();
        arbiter.reset(1, &LiveStart::Started, now);

        assert_eq!(
            arbiter.live(Ok("live".into())),
            Some(PreviewState::Ready("live".into()))
        );
        assert_eq!(arbiter.fallback(1, Ok("late snapshot".into())), None);
        assert!(!arbiter.fallback_due(now + FALLBACK_REQUEST_DELAY));
    }

    #[test]
    fn stream_error_does_not_replace_a_valid_live_frame() {
        let now = Instant::now();
        let mut arbiter = Arbiter::new();
        arbiter.reset(1, &LiveStart::Started, now);
        assert!(arbiter.live(Ok("live".into())).is_some());
        assert_eq!(arbiter.live(Err("stream ended".into())), None);
    }
}
