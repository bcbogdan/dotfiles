use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;

use crate::model::{PreviewIdentity, PreviewState};

const MAX_FRAME_LINE: usize = 2 * 1024 * 1024;
const FALLBACK_DELAY: Duration = Duration::from_millis(350);
const MAX_FRAME_DIMENSION: u16 = 1_000;
const MAX_FRAME_CELLS: u32 = 500_000;

pub struct LivePreview {
    child: Option<Child>,
    receiver: Option<Receiver<Result<String, String>>>,
    identity: Option<PreviewIdentity>,
    dimensions: (u16, u16),
}

impl LivePreview {
    pub fn new() -> Self {
        Self {
            child: None,
            receiver: None,
            identity: None,
            dimensions: (0, 0),
        }
    }

    pub fn select(
        &mut self,
        identity: Option<&PreviewIdentity>,
        dimensions: (u16, u16),
    ) -> LiveStart {
        if self.identity.as_ref() == identity && self.dimensions == dimensions {
            return if self.child.is_some() {
                LiveStart::Started
            } else {
                LiveStart::NotApplicable
            };
        }
        self.stop();
        self.identity = identity.cloned();
        self.dimensions = dimensions;
        let Some(target) = identity.and_then(|identity| identity.preview_pane.as_deref()) else {
            return LiveStart::NotApplicable;
        };
        match start(target, dimensions) {
            Ok((child, receiver)) => {
                self.child = Some(child);
                self.receiver = Some(receiver);
                LiveStart::Started
            }
            Err(error) => LiveStart::Unavailable(error),
        }
    }

    pub fn poll(&self) -> Option<Result<String, String>> {
        self.receiver.as_ref()?.try_recv().ok()
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.receiver = None;
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
    fallback: Option<Result<String, String>>,
}

impl Arbiter {
    pub fn new() -> Self {
        Self {
            generation: 0,
            started_at: Instant::now(),
            live: LiveState::NotApplicable,
            fallback: None,
        }
    }

    pub fn reset(&mut self, generation: u64, start: &LiveStart, now: Instant) {
        self.generation = generation;
        self.started_at = now;
        self.fallback = None;
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
        now: Instant,
    ) -> Option<PreviewState> {
        if generation != self.generation || self.live == LiveState::Ready {
            return None;
        }
        self.fallback = Some(result);
        self.release_fallback(now)
    }

    pub fn live(&mut self, result: Result<String, String>) -> Option<PreviewState> {
        match result {
            Ok(frame) => {
                self.live = LiveState::Ready;
                self.fallback = None;
                Some(PreviewState::Ready(frame))
            }
            Err(error) => {
                if self.live == LiveState::Ready {
                    return None;
                }
                self.live = LiveState::Unavailable;
                self.release_fallback(Instant::now())
                    .or(Some(PreviewState::Error(error)))
            }
        }
    }

    pub fn tick(&mut self, now: Instant) -> Option<PreviewState> {
        self.release_fallback(now)
    }

    fn release_fallback(&mut self, now: Instant) -> Option<PreviewState> {
        let due = self.live != LiveState::Waiting
            || now.saturating_duration_since(self.started_at) >= FALLBACK_DELAY;
        if !due {
            return None;
        }
        self.fallback
            .take()
            .map(|result| result.map_or_else(PreviewState::Error, PreviewState::Ready))
    }
}

fn start(
    target: &str,
    (columns, rows): (u16, u16),
) -> Result<(Child, Receiver<Result<String, String>>), String> {
    let herdr = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
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
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "live preview stdout unavailable".to_string())?;
    let (tx, rx) = mpsc::sync_channel(2);
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
                        let _ = tx.try_send(Err("live preview stream ended".into()));
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let _ = tx.try_send(Err(error));
                        break;
                    }
                }
                let result = serde_json::from_slice::<Record<'_>>(&line)
                    .map_err(|error| error.to_string())
                    .and_then(|record| state.apply(&record));
                match result {
                    Ok(Some(ansi)) => {
                        let _ = tx.try_send(Ok(ansi));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = tx.try_send(Err(error));
                        break;
                    }
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok((child, rx))
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
        let bytes = STANDARD
            .decode(frame.bytes.unwrap_or_default())
            .map_err(|error| error.to_string())?;
        parser.process(&bytes);
        self.last_sequence = Some(frame.sequence);
        Ok(Some(
            String::from_utf8_lossy(&parser.screen().contents_formatted()).into_owned(),
        ))
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
    fn live_frame_wins_over_earlier_or_later_fallback() {
        let now = Instant::now();
        let mut arbiter = Arbiter::new();
        arbiter.reset(1, &LiveStart::Started, now);
        assert_eq!(arbiter.fallback(1, Ok("snapshot".into()), now), None);
        assert_eq!(
            arbiter.live(Ok("live".into())),
            Some(PreviewState::Ready("live".into()))
        );
        assert_eq!(
            arbiter.fallback(1, Ok("late snapshot".into()), now + FALLBACK_DELAY),
            None
        );
    }

    #[test]
    fn fallback_waits_for_deadline_when_live_has_no_first_frame() {
        let now = Instant::now();
        let mut arbiter = Arbiter::new();
        arbiter.reset(3, &LiveStart::Started, now);
        assert_eq!(arbiter.fallback(3, Ok("snapshot".into()), now), None);
        assert_eq!(
            arbiter.tick(now + FALLBACK_DELAY),
            Some(PreviewState::Ready("snapshot".into()))
        );
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
