use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use wait_timeout::ChildExt;

use crate::model::{canonicalish, Item, Target};
use crate::profile;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(40);
const OUTPUT_LIMIT: usize = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to run {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("{program} timed out")]
    Timeout { program: String },
    #[error("{program} failed: {message}")]
    Command { program: String, message: String },
    #[error("invalid {0} response: {1}")]
    Parse(&'static str, serde_json::Error),
    #[error("invalid {0} response: {1}")]
    Structure(&'static str, String),
    #[error("client navigation timed out; outcome unknown (the request was not cancelled). Check Herdr before retrying")]
    NavigationTimeout,
}

pub trait Runner: Send + Sync {
    fn run(&self, program: &OsStr, args: &[OsString], timeout: Duration) -> Result<String, Error>;
}

#[derive(Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &OsStr, args: &[OsString], timeout: Duration) -> Result<String, Error> {
        let name = program.to_string_lossy().into_owned();
        let invocation = format_invocation(program, args);
        let command_span = profile::span("command.run", || {
            serde_json::json!({
                "program": name,
                "args": args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
                "timeout_ms": timeout.as_millis(),
            })
        });
        let mut stdout = capture_file().map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        let mut stderr = capture_file().map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        // Files avoid inherited-pipe hangs; timed-out grandchildren may still exit later.
        let spawn = profile::span("command.spawn", || serde_json::json!({ "program": name }));
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout.try_clone().map_err(|source| {
                Error::Spawn {
                    program: name.clone(),
                    source,
                }
            })?))
            .stderr(Stdio::from(stderr.try_clone().map_err(|source| {
                Error::Spawn {
                    program: name.clone(),
                    source,
                }
            })?));
        if std::env::var_os("RUST_BACKTRACE").is_none() {
            command.env("RUST_BACKTRACE", "1");
        }
        let mut child = command.spawn().map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        drop(spawn);
        let wait = profile::span("command.wait", || serde_json::json!({ "program": name }));
        if child
            .wait_timeout(timeout)
            .map_err(|source| Error::Spawn {
                program: name.clone(),
                source,
            })?
            .is_none()
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Timeout { program: name });
        }
        drop(wait);
        let status = child.wait().map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        let read = profile::span(
            "command.read_output",
            || serde_json::json!({ "program": name }),
        );
        let stdout = read_tail(&mut stdout).map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        let stderr = read_tail(&mut stderr).map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        drop(read);
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&stdout).trim().to_string();
            let message = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                #[cfg(unix)]
                if let Some(signal) = status.signal() {
                    return Err(Error::Command {
                        program: invocation,
                        message: format!("terminated by signal {signal}"),
                    });
                }
                status.code().map_or_else(
                    || "terminated without an exit status".into(),
                    |code| format!("exit status {code}"),
                )
            };
            return Err(Error::Command {
                program: invocation,
                message,
            });
        }
        drop(command_span);
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
}

fn format_invocation(program: &OsStr, args: &[OsString]) -> String {
    let mut invocation = program.to_string_lossy().into_owned();
    for arg in args {
        let _ = write!(invocation, " {:?}", arg.to_string_lossy());
    }
    invocation
}

fn capture_file() -> std::io::Result<std::fs::File> {
    std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .or_else(|| std::env::var_os("HOME"))
        .map_or_else(tempfile::tempfile, tempfile::tempfile_in)
}

fn read_tail(file: &mut std::fs::File) -> std::io::Result<Vec<u8>> {
    let length = file.seek(SeekFrom::End(0))?;
    let start = length.saturating_sub(OUTPUT_LIMIT as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut captured = Vec::with_capacity(usize::try_from(length - start).unwrap_or(OUTPUT_LIMIT));
    file.read_to_end(&mut captured)?;
    Ok(captured)
}

#[derive(Clone)]
pub struct Client<R> {
    runner: R,
    herdr: OsString,
    prefix: Vec<OsString>,
    navigation_enabled: bool,
    pending_workspaces: Arc<Mutex<HashMap<(String, PathBuf), String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmResult {
    pub resolved_workspace_id: Option<String>,
    pub switched_client: bool,
}

impl<R: Runner> Client<R> {
    pub fn new(runner: R, herdr: OsString) -> Self {
        Self::with_client_token(
            runner,
            herdr,
            std::env::var_os("HERDR_CLIENT_TOKEN").as_deref(),
        )
    }

    pub fn with_client_token(runner: R, herdr: OsString, token: Option<&OsStr>) -> Self {
        Self {
            runner,
            herdr,
            prefix: Vec::new(),
            navigation_enabled: token.is_some_and(|token| !token.is_empty()),
            pending_workspaces: Arc::default(),
        }
    }

    pub fn with_session(mut self, session: String) -> Self {
        self.prefix = vec!["--session".into(), session.into()];
        self
    }

    pub fn load(&self, self_pane: Option<&str>) -> [Result<Vec<Item>, String>; 3] {
        let load = profile::span(
            "refresh.load",
            || serde_json::json!({ "self_pane": self_pane }),
        );
        let workspaces = self
            .herdr(&["workspace", "list"])
            .and_then(|raw| parse_workspaces(&raw));
        let panes = self
            .herdr(&["pane", "list"])
            .and_then(|raw| parse_panes(&raw));
        let workspace_source = match (&workspaces, &panes) {
            (Ok(workspaces), Ok(panes)) => Ok(workspace_items(workspaces, panes, self_pane)),
            (Err(error), _) | (_, Err(error)) => Err(error.to_string()),
        };
        let agent_source = self
            .herdr(&["agent", "list"])
            .and_then(|raw| parse_agents(&raw))
            .map(|agents| {
                agent_items(
                    &agents,
                    workspaces.as_ref().map_or(&[], Vec::as_slice),
                    self_pane,
                )
            })
            .map_err(|error| error.to_string());
        let zoxide_source = match self.runner.run(
            &crate::machine::installed_program("zoxide"),
            &args(&["query", "-l", "-s"]),
            COMMAND_TIMEOUT,
        ) {
            Ok(raw) => Ok(parse_zoxide(&raw)),
            Err(Error::Spawn { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Err("zoxide is unavailable".to_string())
            }
            Err(error) => Err(error.to_string()),
        };
        let result = [workspace_source, agent_source, zoxide_source];
        drop(load);
        result
    }

    pub fn snapshot(&self, pane: &str) -> Result<String, Error> {
        self.herdr(&[
            "pane", "read", pane, "--source", "visible", "--format", "ansi",
        ])
    }

    pub fn confirm(&self, target: &Target, workspaces: &[Item]) -> Result<ConfirmResult, Error> {
        match target {
            Target::Remote { machine, target } => {
                if self.navigation_enabled {
                    return self.navigate_remote(machine, target, workspaces);
                }
                let run = |args: &[OsString]| {
                    crate::machine::forwarded(&self.runner, machine, args)
                        .map_err(|message| Error::Structure("remote action", message))
                };
                match target.as_ref() {
                    Target::Workspace { id } => {
                        run(&args(&["workspace", "focus", id]))?;
                        Ok(ConfirmResult {
                            switched_client: false,
                            resolved_workspace_id: None,
                        })
                    }
                    Target::Agent { pane_id } => {
                        run(&args(&["agent", "focus", pane_id]))?;
                        Ok(ConfirmResult {
                            switched_client: false,
                            resolved_workspace_id: None,
                        })
                    }
                    Target::Directory { path } => {
                        let existing = remote_workspace_for_path(machine, path, workspaces);
                        let id = if let Some(id) = existing {
                            run(&args(&["workspace", "focus", &id]))?;
                            id
                        } else {
                            parse_created_workspace_id(&run(&[
                                "workspace".into(),
                                "create".into(),
                                "--cwd".into(),
                                path.as_os_str().to_owned(),
                                "--label".into(),
                                sensible_label(path).into(),
                                "--focus".into(),
                            ])?)?
                        };
                        Ok(ConfirmResult {
                            switched_client: false,
                            resolved_workspace_id: Some(id),
                        })
                    }
                    Target::Remote { .. } => Err(Error::Structure(
                        "remote action",
                        "nested remote target".into(),
                    )),
                }
            }
            Target::Workspace { id } => {
                self.herdr(&["workspace", "focus", id])?;
                Ok(ConfirmResult {
                    switched_client: false,
                    resolved_workspace_id: None,
                })
            }
            Target::Agent { pane_id } => {
                self.herdr(&["agent", "focus", pane_id])?;
                Ok(ConfirmResult {
                    switched_client: false,
                    resolved_workspace_id: None,
                })
            }
            Target::Directory { path } => {
                if let Some(id) = workspace_for_path(path, workspaces) {
                    self.herdr(&["workspace", "focus", id])?;
                    return Ok(ConfirmResult {
                        switched_client: false,
                        resolved_workspace_id: Some(id.to_string()),
                    });
                }
                let label = sensible_label(path);
                let raw = self.herdr_os(&[
                    OsString::from("workspace"),
                    OsString::from("create"),
                    OsString::from("--cwd"),
                    path.as_os_str().to_owned(),
                    OsString::from("--label"),
                    OsString::from(label),
                    OsString::from("--focus"),
                ])?;
                let workspace_id = parse_created_workspace_id(&raw)?;
                Ok(ConfirmResult {
                    switched_client: false,
                    resolved_workspace_id: Some(workspace_id),
                })
            }
        }
    }

    pub fn has_pending_workspace(&self, machine: &crate::machine::Machine, path: &Path) -> bool {
        self.pending_workspaces
            .lock()
            .unwrap()
            .contains_key(&(machine.namespace(), path.to_owned()))
    }

    fn navigate_remote(
        &self,
        machine: &crate::machine::Machine,
        target: &Target,
        workspaces: &[Item],
    ) -> Result<ConfirmResult, Error> {
        crate::machine::validate_profile(&self.runner, machine)
            .map_err(|message| Error::Structure("remote action", message))?;
        if !machine.enabled {
            return Err(Error::Structure(
                "remote action",
                "machine is disabled".into(),
            ));
        }
        let mut pending_key = None;
        let (flag, id, directory) = match target {
            Target::Workspace { id } => ("--workspace", id.clone(), false),
            Target::Agent { pane_id } => ("--pane", pane_id.clone(), false),
            Target::Directory { path } => {
                let key = (machine.namespace(), path.clone());
                let pending = self.pending_workspaces.lock().unwrap().get(&key).cloned();
                let existing =
                    pending.or_else(|| remote_workspace_for_path(machine, path, workspaces));
                let id = if let Some(id) = existing {
                    id
                } else {
                    let raw = crate::machine::forwarded(
                        &self.runner,
                        machine,
                        &[
                            "workspace".into(),
                            "create".into(),
                            "--cwd".into(),
                            path.as_os_str().to_owned(),
                            "--label".into(),
                            sensible_label(path).into(),
                        ],
                    )
                    .map_err(|message| Error::Structure("remote action", message))?;
                    parse_created_workspace_id(&raw)?
                };
                // Cloned async clients share this ID so a failed switch cannot duplicate creation.
                self.pending_workspaces
                    .lock()
                    .unwrap()
                    .insert(key.clone(), id.clone());
                pending_key = Some(key);
                ("--workspace", id, true)
            }
            Target::Remote { .. } => {
                return Err(Error::Structure(
                    "remote action",
                    "nested remote target".into(),
                ))
            }
        };
        // The token and socket are inherited. Navigation belongs to the originating server,
        // not the destination's --machine forwarding connection or session.
        self.runner
            .run(
                &self.herdr,
                &args(&["client", "navigate", &machine.id, flag, &id]),
                NAVIGATION_TIMEOUT,
            )
            .map_err(|error| match error {
                Error::Timeout { .. } => Error::NavigationTimeout,
                error => error,
            })?;
        if let Some(key) = pending_key {
            self.pending_workspaces.lock().unwrap().remove(&key);
        }
        Ok(ConfirmResult {
            resolved_workspace_id: directory.then_some(id),
            switched_client: true,
        })
    }

    fn herdr(&self, values: &[&str]) -> Result<String, Error> {
        self.herdr_os(&args(values))
    }

    fn herdr_os(&self, values: &[OsString]) -> Result<String, Error> {
        let mut args = self.prefix.clone();
        args.extend_from_slice(values);
        self.runner.run(&self.herdr, &args, COMMAND_TIMEOUT)
    }
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[derive(Debug, Deserialize)]
struct Workspace {
    #[serde(rename = "workspace_id")]
    id: String,
    label: String,
    cwd: Option<String>,
    foreground_cwd: Option<String>,
    #[serde(default)]
    focused: bool,
}

#[derive(Debug, Deserialize)]
struct Pane {
    #[serde(rename = "pane_id")]
    id: String,
    workspace_id: String,
    cwd: Option<String>,
    foreground_cwd: Option<String>,
    agent_status: Option<String>,
    #[serde(default)]
    state_change_seq: u64,
    #[serde(default)]
    focused: bool,
}

#[derive(Debug, Deserialize)]
struct Agent {
    #[serde(rename = "pane_id")]
    pane: String,
    workspace_id: Option<String>,
    #[serde(rename = "agent")]
    kind: Option<String>,
    #[serde(rename = "agent_status")]
    status: Option<String>,
    terminal_title_stripped: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    state_change_seq: u64,
}

fn result_array<T: for<'de> Deserialize<'de>>(
    raw: &str,
    field: &'static str,
) -> Result<Vec<T>, Error> {
    let value: Value = serde_json::from_str(raw).map_err(|error| Error::Parse(field, error))?;
    let result = value
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Structure(field, "missing result object".into()))?;
    let entries = result
        .get(field)
        .ok_or_else(|| Error::Structure(field, format!("missing {field} field")))?;
    if !entries.is_array() {
        return Err(Error::Structure(field, format!("{field} is not an array")));
    }
    serde_json::from_value(entries.clone()).map_err(|error| Error::Parse(field, error))
}

fn parse_workspaces(raw: &str) -> Result<Vec<Workspace>, Error> {
    result_array(raw, "workspaces")
}
fn parse_panes(raw: &str) -> Result<Vec<Pane>, Error> {
    result_array(raw, "panes")
}
fn parse_agents(raw: &str) -> Result<Vec<Agent>, Error> {
    result_array(raw, "agents")
}

fn parse_created_workspace_id(raw: &str) -> Result<String, Error> {
    let value: Value =
        serde_json::from_str(raw).map_err(|error| Error::Parse("workspace create", error))?;
    let workspace = value
        .pointer("/result/workspace")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::Structure("workspace create", "missing result.workspace object".into())
        })?;
    workspace
        .get("workspace_id")
        .or_else(|| workspace.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::Structure(
                "workspace create",
                "missing result.workspace.workspace_id".into(),
            )
        })
}

fn workspace_items(workspaces: &[Workspace], panes: &[Pane], self_pane: Option<&str>) -> Vec<Item> {
    workspaces
        .iter()
        .map(|workspace| {
            let candidates: Vec<_> = panes
                .iter()
                .filter(|pane| {
                    pane.workspace_id == workspace.id && Some(pane.id.as_str()) != self_pane
                })
                .collect();
            let pane = candidates
                .iter()
                .find(|pane| pane.focused)
                .copied()
                .or_else(|| {
                    candidates
                        .iter()
                        .max_by_key(|pane| {
                            (
                                status_priority(pane.agent_status.as_deref().unwrap_or("unknown")),
                                pane.state_change_seq,
                            )
                        })
                        .copied()
                });
            let cwd = pane
                .and_then(|pane| pane.foreground_cwd.as_ref().or(pane.cwd.as_ref()))
                .cloned()
                .unwrap_or_default();
            let focus = if workspace.focused { "focused" } else { "" };
            let mut match_paths = Vec::new();
            for path in workspace
                .cwd
                .iter()
                .chain(workspace.foreground_cwd.iter())
                .chain(candidates.iter().flat_map(|pane| pane.cwd.iter()))
                .chain(
                    candidates
                        .iter()
                        .flat_map(|pane| pane.foreground_cwd.iter()),
                )
            {
                let path = canonicalish(Path::new(path));
                if !match_paths.contains(&path) {
                    match_paths.push(path);
                }
            }
            Item {
                id: workspace.id.clone(),
                label: workspace.label.clone(),
                detail: cwd.clone(),
                search: format!("{} {cwd} {focus}", workspace.label),
                preview_pane: pane.map(|pane| pane.id.clone()),
                match_paths,
                target: Target::Workspace {
                    id: workspace.id.clone(),
                },
            }
        })
        .collect()
}

fn status_priority(status: &str) -> u8 {
    match status {
        "blocked" => 4,
        "done" => 3,
        "working" => 2,
        "idle" => 1,
        _ => 0,
    }
}

fn agent_items(agents: &[Agent], workspaces: &[Workspace], self_pane: Option<&str>) -> Vec<Item> {
    let mut agents: Vec<_> = agents
        .iter()
        .filter(|agent| Some(agent.pane.as_str()) != self_pane)
        .collect();
    agents.sort_by_key(|agent| {
        (
            std::cmp::Reverse(status_priority(
                agent.status.as_deref().unwrap_or("unknown"),
            )),
            std::cmp::Reverse(agent.state_change_seq),
        )
    });
    agents
        .into_iter()
        .map(|agent| {
            let kind = agent.kind.as_deref().unwrap_or("agent");
            let status = agent.status.as_deref().unwrap_or("unknown");
            let title = agent.terminal_title_stripped.as_deref().unwrap_or(kind);
            let workspace = agent
                .workspace_id
                .as_deref()
                .and_then(|id| workspaces.iter().find(|workspace| workspace.id == id))
                .map_or("-", |workspace| workspace.label.as_str());
            let cwd = agent.cwd.as_deref().unwrap_or("");
            Item {
                id: agent.pane.clone(),
                label: format!("{kind} · {title}"),
                detail: format!("{status} · {workspace} · {cwd}"),
                search: format!("{kind} {title} {status} {workspace} {cwd}"),
                preview_pane: Some(agent.pane.clone()),
                match_paths: Vec::new(),
                target: Target::Agent {
                    pane_id: agent.pane.clone(),
                },
            }
        })
        .collect()
}

pub fn parse_zoxide(raw: &str) -> Vec<Item> {
    raw.lines()
        .filter_map(|line| {
            let (_, path) = line.trim().split_once(char::is_whitespace)?;
            let path = PathBuf::from(path.trim());
            let label = sensible_label(&path);
            Some(Item {
                id: canonicalish(&path).to_string_lossy().into_owned(),
                label: label.clone(),
                detail: path.display().to_string(),
                search: format!("{label} {}", path.display()),
                preview_pane: None,
                match_paths: Vec::new(),
                target: Target::Directory { path },
            })
        })
        .collect()
}

fn remote_workspace_for_path(
    machine: &crate::machine::Machine,
    path: &Path,
    workspaces: &[Item],
) -> Option<String> {
    workspaces.iter().find_map(|item| {
        if item
            .machine()
            .is_some_and(|host| host.namespace() == machine.namespace())
            && item.match_paths.iter().any(|candidate| candidate == path)
        {
            if let Target::Workspace { id } = item.target.inner() {
                return Some(id.clone());
            }
        }
        None
    })
}

pub fn workspace_for_path<'a>(path: &Path, workspaces: &'a [Item]) -> Option<&'a str> {
    let path = canonicalish(path);
    workspaces.iter().find_map(|item| {
        if item.machine().is_some() {
            return None;
        }
        item.match_paths
            .iter()
            .any(|candidate| candidate == &path)
            .then_some(item.id.as_str())
    })
}

fn sensible_label(path: &Path) -> String {
    path.file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

pub fn directory_summary<R: Runner>(runner: &R, path: &Path) -> Result<String, Error> {
    let directory = profile::span("preview.directory", || serde_json::json!({ "path": path }));
    let mut summary = format!("{}\n", path.display());
    let git_args = vec![
        OsString::from("-C"),
        path.as_os_str().to_owned(),
        OsString::from("status"),
        OsString::from("--short"),
        OsString::from("--branch"),
    ];
    if let Ok(status) = runner.run(OsStr::new("git"), &git_args, Duration::from_secs(2)) {
        summary.push_str("\nGit\n");
        summary.push_str(if status.trim().is_empty() {
            "clean\n"
        } else {
            &status
        });
    }
    summary.push_str("\nFiles\n");
    let mut entries = std::fs::read_dir(path)
        .map_err(|source| Error::Spawn {
            program: format!("read {}", path.display()),
            source,
        })?
        .take(256)
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries.into_iter().take(80) {
        let suffix = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            "/"
        } else {
            ""
        };
        let _ = writeln!(summary, "{}{}", entry.file_name().to_string_lossy(), suffix);
    }
    drop(directory);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::testing::{item, machine, RemoteRunner};

    #[derive(Clone, Default)]
    struct NavigationRunner {
        remote: RemoteRunner,
        failure: Arc<Mutex<Option<&'static str>>>,
    }

    impl Runner for NavigationRunner {
        fn run(
            &self,
            program: &OsStr,
            values: &[OsString],
            timeout: Duration,
        ) -> Result<String, Error> {
            if program == "origin-herdr" {
                self.remote
                    .calls
                    .lock()
                    .unwrap()
                    .push((program.into(), values.to_vec()));
                if values.first().is_some_and(|arg| arg == "client") {
                    assert!(timeout > Duration::from_secs(30));
                    if let Some(message) = *self.failure.lock().unwrap() {
                        if message == "timeout" {
                            return Err(Error::Timeout {
                                program: "origin-herdr".into(),
                            });
                        }
                        return Err(Error::Command {
                            program: "origin-herdr".into(),
                            message: message.into(),
                        });
                    }
                }
                return Ok("{}".into());
            }
            self.remote.run(program, values, timeout)
        }
    }

    #[test]
    fn token_routes_workspace_and_pane_via_origin_without_forwarding_or_session() {
        let runner = NavigationRunner::default();
        let client = Client::with_client_token(
            runner.clone(),
            "origin-herdr".into(),
            Some(OsStr::new("inherited-token")),
        )
        .with_session("origin-session".into());
        for (target, flag, id) in [
            (Target::Workspace { id: "w1".into() }, "--workspace", "w1"),
            (
                Target::Agent {
                    pane_id: "p1".into(),
                },
                "--pane",
                "p1",
            ),
        ] {
            let result = client
                .confirm(&item(target).on_machine(&machine()).target, &[])
                .unwrap();
            assert!(result.switched_client);
            let calls = runner.remote.calls.lock().unwrap();
            assert_eq!(
                calls.last().unwrap(),
                &(
                    "origin-herdr".into(),
                    args(&["client", "navigate", "opaque-profile-id", flag, id])
                )
            );
            assert!(calls
                .iter()
                .all(|(_, args)| !args.iter().any(|arg| arg == "--machine")));
        }
    }

    #[test]
    fn navigation_errors_never_fall_back_and_manual_retry_reuses_created_workspace() {
        let runner = NavigationRunner::default();
        let client = Client::with_client_token(
            runner.clone(),
            "origin-herdr".into(),
            Some(OsStr::new("token")),
        );
        let target = item(Target::Directory {
            path: "/same/path".into(),
        })
        .on_machine(&machine())
        .target;
        for failure in ["navigation unsupported", "timeout"] {
            *runner.failure.lock().unwrap() = Some(failure);
            let error = client
                .clone()
                .confirm(&target, &[])
                .unwrap_err()
                .to_string();
            assert!(error.contains(if failure == "timeout" {
                "outcome unknown"
            } else {
                failure
            }));
            assert!(client.has_pending_workspace(&machine(), Path::new("/same/path")));
        }
        *runner.failure.lock().unwrap() = None;
        let result = client.clone().confirm(&target, &[]).unwrap();
        assert!(result.switched_client);
        assert_eq!(result.resolved_workspace_id.as_deref(), Some("w9"));
        let calls = runner.remote.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|(_, args)| args.iter().any(|arg| arg == "create"))
                .count(),
            1
        );
        assert!(calls
            .iter()
            .all(|(_, args)| !args.iter().any(|arg| arg == "--focus" || arg == "focus")));
        assert_eq!(
            calls.last().unwrap().1,
            args(&[
                "client",
                "navigate",
                "opaque-profile-id",
                "--workspace",
                "w9"
            ])
        );
    }

    #[test]
    fn matching_remote_directory_navigates_and_local_focus_stays_on_executing_server() {
        let runner = NavigationRunner::default();
        let client = Client::with_client_token(
            runner.clone(),
            "origin-herdr".into(),
            Some(OsStr::new("token")),
        );
        let workspace = item(Target::Workspace {
            id: "existing".into(),
        })
        .on_machine(&machine());
        let target = item(Target::Directory {
            path: "/same/path".into(),
        })
        .on_machine(&machine())
        .target;
        let result = client.confirm(&target, &[workspace]).unwrap();
        assert_eq!(result.resolved_workspace_id.as_deref(), Some("existing"));
        assert!(result.switched_client);
        assert!(
            !client
                .confirm(
                    &Target::Workspace {
                        id: "local-w1".into()
                    },
                    &[]
                )
                .unwrap()
                .switched_client
        );
        let calls = runner.remote.calls.lock().unwrap();
        assert_eq!(
            calls.last().unwrap().1,
            args(&["workspace", "focus", "local-w1"])
        );
        assert!(calls
            .iter()
            .all(|(_, args)| !args.iter().any(|arg| arg == "--machine" || arg == "create")));
    }

    #[test]
    fn absent_or_empty_token_retains_legacy_remote_focus() {
        for token in [None, Some(OsStr::new(""))] {
            let runner = RemoteRunner::default();
            let client = Client::with_client_token(runner.clone(), "must-not-run".into(), token);
            let result = client
                .confirm(
                    &item(Target::Workspace { id: "w1".into() })
                        .on_machine(&machine())
                        .target,
                    &[],
                )
                .unwrap();
            assert!(!result.switched_client);
            assert_eq!(
                runner.calls.lock().unwrap().last().unwrap().1,
                args(&["--machine", "opaque-profile-id", "workspace", "focus", "w1"])
            );
        }
    }

    #[test]
    fn remote_actions_are_profile_qualified_and_never_fall_back_locally() {
        let runner = RemoteRunner {
            fail_forward: true,
            ..Default::default()
        };
        let client =
            Client::with_client_token(runner.clone(), "local-compatible-backup".into(), None);
        for target in [
            Target::Workspace { id: "w1".into() },
            Target::Agent {
                pane_id: "p1".into(),
            },
            Target::Directory {
                path: "/same/path".into(),
            },
        ] {
            assert!(client
                .confirm(&item(target).on_machine(&machine()).target, &[])
                .is_err());
        }
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|(_, args)| args[0] == "--machine")
                .count(),
            3
        );
        assert!(calls
            .iter()
            .all(|(program, _)| program != "local-compatible-backup"));
    }

    #[test]
    fn directory_resolution_is_scoped_to_machine_and_disabled_profile_cannot_act() {
        let runner = RemoteRunner::default();
        let client =
            Client::with_client_token(runner.clone(), "local-compatible-backup".into(), None);
        let local = item(Target::Workspace {
            id: "local-w1".into(),
        });
        let remote = item(Target::Workspace {
            id: "remote-w1".into(),
        })
        .on_machine(&machine());
        let target = item(Target::Directory {
            path: "/same/path".into(),
        })
        .on_machine(&machine())
        .target;
        assert_eq!(
            client
                .confirm(&target, &[local.clone(), remote.clone()])
                .unwrap()
                .resolved_workspace_id
                .as_deref(),
            Some("remote-w1")
        );
        assert_eq!(
            workspace_for_path(Path::new("/same/path"), &[remote, local]),
            Some("same-id")
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls.last().unwrap().1,
            args(&[
                "--machine",
                "opaque-profile-id",
                "workspace",
                "focus",
                "remote-w1"
            ])
        );
        drop(calls);
        let disabled = RemoteRunner {
            disabled: true,
            ..Default::default()
        };
        assert!(Client::new(disabled.clone(), "local".into())
            .confirm(&target, &[])
            .is_err());
        assert_eq!(disabled.calls.lock().unwrap().len(), 1);
    }
    use std::sync::Mutex;

    struct FakeRunner {
        calls: Mutex<Vec<Vec<OsString>>>,
    }

    struct MissingZoxideRunner;
    impl Runner for MissingZoxideRunner {
        fn run(
            &self,
            program: &OsStr,
            args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, Error> {
            if Path::new(program).file_name() == Some(OsStr::new("zoxide")) {
                return Err(Error::Spawn {
                    program: "zoxide".into(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                });
            }
            let field = if args.iter().any(|arg| arg == "workspace") {
                "workspaces"
            } else if args.iter().any(|arg| arg == "pane") {
                "panes"
            } else {
                "agents"
            };
            Ok(format!(r#"{{"result":{{"{field}":[]}}}}"#))
        }
    }
    impl Runner for FakeRunner {
        fn run(
            &self,
            _program: &OsStr,
            args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, Error> {
            self.calls.lock().unwrap().push(args.to_vec());
            if args.iter().any(|arg| arg == "create") {
                Ok(r#"{"id":"cli:workspace:create","result":{"type":"workspace_created","workspace":{"workspace_id":"w9","label":"project"},"tab":{"tab_id":"w9:t1"},"root_pane":{"pane_id":"w9:p1"}}}"#.into())
            } else {
                Ok("{}".into())
            }
        }
    }

    struct RawRunner;
    impl Runner for RawRunner {
        fn run(
            &self,
            _program: &OsStr,
            _args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, Error> {
            Ok("\x1b[31mraw ansi\x1b[0m".into())
        }
    }

    #[test]
    fn parses_wrapped_sources_and_agent_priority() {
        let workspaces = parse_workspaces(
            r#"{"id":"x","result":{"workspaces":[{"workspace_id":"w1","label":"one"}]}}"#,
        )
        .unwrap();
        let agents = parse_agents(r#"{"result":{"agents":[{"pane_id":"p1","agent_status":"idle","state_change_seq":9},{"pane_id":"p2","agent_status":"blocked","state_change_seq":1}]}}"#).unwrap();
        assert_eq!(agent_items(&agents, &workspaces, None)[0].id, "p2");
    }

    #[test]
    fn structurally_invalid_json_is_not_an_empty_source() {
        assert!(parse_workspaces("{}").is_err());
        assert!(parse_panes(r#"{"result":{}}"#).is_err());
        assert!(parse_agents(r#"{"result":{"agents":{}}}"#).is_err());
    }

    #[test]
    fn parses_herdr_workspace_create_response_and_rejects_malformed_success() {
        let raw = r#"{"id":"cli:workspace:create","result":{"type":"workspace_created","workspace":{"workspace_id":"w9","label":"project"},"tab":{"tab_id":"w9:t1"},"root_pane":{"pane_id":"w9:p1"}}}"#;
        assert_eq!(parse_created_workspace_id(raw).unwrap(), "w9");
        assert!(parse_created_workspace_id(r#"{"result":{"workspace":{}}}"#).is_err());
        assert!(parse_created_workspace_id(r#"{"result":{}}"#).is_err());
    }

    #[test]
    fn snapshot_returns_raw_ansi_stdout() {
        let client = Client::new(RawRunner, OsString::from("herdr"));
        assert_eq!(client.snapshot("p1").unwrap(), "\x1b[31mraw ansi\x1b[0m");
    }

    #[test]
    fn zoxide_parser_keeps_paths_with_spaces() {
        let items = parse_zoxide("42.0 /tmp/a project\n");
        assert_eq!(items[0].detail, "/tmp/a project");
    }

    #[test]
    fn zoxide_action_focuses_normalized_existing_workspace() {
        let workspace = Item {
            id: "w1".into(),
            label: "one".into(),
            detail: "/tmp/a/b".into(),
            search: String::new(),
            preview_pane: None,
            match_paths: vec![canonicalish(Path::new("/tmp/a/b"))],
            target: Target::Workspace { id: "w1".into() },
        };
        assert_eq!(
            workspace_for_path(Path::new("/tmp/a/x/../b"), &[workspace]),
            Some("w1")
        );
    }

    #[test]
    fn workspace_matching_uses_all_pane_and_foreground_paths() {
        let workspaces = parse_workspaces(
            r#"{"result":{"workspaces":[{"workspace_id":"w1","label":"one","cwd":"/tmp/root"}]}}"#,
        )
        .unwrap();
        let panes = parse_panes(
            r#"{"result":{"panes":[
                {"pane_id":"p1","workspace_id":"w1","cwd":"/tmp/old","foreground_cwd":"/tmp/new","focused":true},
                {"pane_id":"p2","workspace_id":"w1","cwd":"/tmp/second"}
            ]}}"#,
        )
        .unwrap();
        let items = workspace_items(&workspaces, &panes, None);
        assert_eq!(items[0].detail, "/tmp/new");
        for path in ["/tmp/root", "/tmp/old", "/tmp/new", "/tmp/second"] {
            assert_eq!(workspace_for_path(Path::new(path), &items), Some("w1"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_matching_resolves_canonical_aliases() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let real = temporary.path().join("real");
        let alias = temporary.path().join("alias");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, &alias).unwrap();
        let item = Item {
            id: "w1".into(),
            label: "one".into(),
            detail: alias.display().to_string(),
            search: String::new(),
            preview_pane: None,
            match_paths: vec![canonicalish(&alias)],
            target: Target::Workspace { id: "w1".into() },
        };
        assert_eq!(workspace_for_path(&real, &[item]), Some("w1"));
    }

    #[test]
    fn directory_create_passes_path_as_one_argv_value() {
        let runner = FakeRunner {
            calls: Mutex::new(Vec::new()),
        };
        let client = Client::new(runner, OsString::from("herdr"));
        let result = client
            .confirm(
                &Target::Directory {
                    path: PathBuf::from("/tmp/a project"),
                },
                &[],
            )
            .unwrap();
        assert_eq!(result.resolved_workspace_id.as_deref(), Some("w9"));
        let calls = client.runner.calls.lock().unwrap();
        assert!(calls[0].contains(&OsString::from("/tmp/a project")));
    }

    #[test]
    fn missing_zoxide_does_not_fail_herdr_sources() {
        let sources = Client::new(MissingZoxideRunner, OsString::from("herdr")).load(None);
        assert!(sources[0].is_ok());
        assert!(sources[1].is_ok());
        assert_eq!(sources[2].as_ref().unwrap_err(), "zoxide is unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn command_failure_reports_stdout_or_signal_when_stderr_is_empty() {
        let stdout_error = SystemRunner
            .run(
                OsStr::new("/bin/sh"),
                &args(&["-c", "printf structured-error; exit 7"]),
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(
            stdout_error.to_string(),
            "/bin/sh \"-c\" \"printf structured-error; exit 7\" failed: structured-error"
        );

        let status_error = SystemRunner
            .run(
                OsStr::new("/bin/sh"),
                &args(&["-c", "exit 7"]),
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(
            status_error.to_string(),
            "/bin/sh \"-c\" \"exit 7\" failed: exit status 7"
        );

        let signal_error = SystemRunner
            .run(
                OsStr::new("/bin/sh"),
                &args(&["-c", "kill -9 $$"]),
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(
            signal_error.to_string(),
            "/bin/sh \"-c\" \"kill -9 $$\" failed: terminated by signal 9"
        );
    }

    #[test]
    fn directory_summary_output_and_scan_are_bounded() {
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..300 {
            std::fs::write(temporary.path().join(format!("file-{index:03}")), "").unwrap();
        }
        let summary = directory_summary(&RawRunner, temporary.path()).unwrap();
        assert!(
            summary
                .lines()
                .filter(|line| line.starts_with("file-"))
                .count()
                <= 80
        );
    }
}
