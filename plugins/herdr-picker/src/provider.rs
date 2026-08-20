use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use wait_timeout::ChildExt;

use crate::model::{canonicalish, Item, Target};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
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
}

pub trait Runner: Send + Sync {
    fn run(&self, program: &OsStr, args: &[OsString], timeout: Duration) -> Result<String, Error>;
}

#[derive(Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &OsStr, args: &[OsString], timeout: Duration) -> Result<String, Error> {
        let name = program.to_string_lossy().into_owned();
        let mut stdout = tempfile::tempfile().map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        let mut stderr = tempfile::tempfile().map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        // Files avoid inherited-pipe hangs; timed-out grandchildren may still exit later.
        let mut child = Command::new(program)
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
            })?))
            .spawn()
            .map_err(|source| Error::Spawn {
                program: name.clone(),
                source,
            })?;
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
        let status = child.wait().map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        let stdout = read_tail(&mut stdout).map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        let stderr = read_tail(&mut stderr).map_err(|source| Error::Spawn {
            program: name.clone(),
            source,
        })?;
        if !status.success() {
            return Err(Error::Command {
                program: name,
                message: String::from_utf8_lossy(&stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
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
}

impl<R: Runner> Client<R> {
    pub fn new(runner: R, herdr: OsString) -> Self {
        Self { runner, herdr }
    }

    pub fn load(&self, self_pane: Option<&str>) -> [Result<Vec<Item>, String>; 3] {
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
            OsStr::new("zoxide"),
            &args(&["query", "-l", "-s"]),
            COMMAND_TIMEOUT,
        ) {
            Ok(raw) => Ok(parse_zoxide(&raw)),
            Err(Error::Spawn { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Err("zoxide is unavailable".to_string())
            }
            Err(error) => Err(error.to_string()),
        };
        [workspace_source, agent_source, zoxide_source]
    }

    pub fn snapshot(&self, pane: &str) -> Result<String, Error> {
        self.herdr(&[
            "pane", "read", pane, "--source", "visible", "--format", "ansi",
        ])
    }

    pub fn confirm(&self, target: &Target, workspaces: &[Item]) -> Result<(), Error> {
        match target {
            Target::Workspace { id } => self.herdr(&["workspace", "focus", id]).map(|_| ()),
            Target::Agent { pane_id } => self.herdr(&["agent", "focus", pane_id]).map(|_| ()),
            Target::Directory { path } => {
                if let Some(id) = workspace_for_path(path, workspaces) {
                    return self.herdr(&["workspace", "focus", id]).map(|_| ());
                }
                let label = sensible_label(path);
                self.herdr_os(&[
                    OsString::from("workspace"),
                    OsString::from("create"),
                    OsString::from("--cwd"),
                    path.as_os_str().to_owned(),
                    OsString::from("--label"),
                    OsString::from(label),
                    OsString::from("--focus"),
                ])
                .map(|_| ())
            }
        }
    }

    fn herdr(&self, values: &[&str]) -> Result<String, Error> {
        self.herdr_os(&args(values))
    }

    fn herdr_os(&self, values: &[OsString]) -> Result<String, Error> {
        self.runner.run(&self.herdr, values, COMMAND_TIMEOUT)
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

pub fn workspace_for_path<'a>(path: &Path, workspaces: &'a [Item]) -> Option<&'a str> {
    let path = canonicalish(path);
    workspaces.iter().find_map(|item| {
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
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            if program == OsStr::new("zoxide") {
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
            Ok("{}".into())
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
        client
            .confirm(
                &Target::Directory {
                    path: PathBuf::from("/tmp/a project"),
                },
                &[],
            )
            .unwrap();
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
