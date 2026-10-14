use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::model::{Item, Target};
use crate::provider::{Client, Runner, SystemRunner};

const PROTOCOL: u32 = 1;
const READ_TIMEOUT: Duration = Duration::from_secs(25);
pub type Sources = [Result<Vec<Item>, String>; 3];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub id: String,
    pub label: String,
    pub target: String,
    pub session: String,
    pub enabled: bool,
}

impl Machine {
    pub fn namespace(&self) -> String {
        serde_json::to_string(&(&self.id, &self.session)).expect("namespace serialization")
    }
}

pub fn local_label() -> String {
    static LABEL: OnceLock<String> = OnceLock::new();
    LABEL
        .get_or_init(|| {
            std::env::var("HERDR_PICKER_MACHINE_LABEL")
                .ok()
                .filter(|label| !label.is_empty())
                .or_else(|| {
                    std::env::var_os("HERDR_PLUGIN_CONFIG_DIR")
                        .and_then(|directory| {
                            std::fs::read_to_string(
                                PathBuf::from(directory).join("local-machine-label"),
                            )
                            .ok()
                        })
                        .map(|label| label.trim().to_owned())
                        .filter(|label| !label.is_empty())
                })
                .or_else(|| {
                    SystemRunner
                        .run(OsStr::new("hostname"), &[], Duration::from_secs(1))
                        .ok()
                        .map(|hostname| hostname.trim().trim_end_matches(".local").to_owned())
                })
                .unwrap_or_else(|| "local".into())
        })
        .clone()
}

pub fn installed_program(name: &str) -> OsString {
    if let Some(home) = std::env::var_os("HOME") {
        for directory in [".local/bin", ".local/share/mise/shims", ".cargo/bin"] {
            let path = PathBuf::from(&home).join(directory).join(name);
            if path.is_file() {
                return path.into_os_string();
            }
        }
    }
    for directory in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let path = Path::new(directory).join(name);
        if path.is_file() {
            return path.into_os_string();
        }
    }
    name.into()
}

pub fn catalog(runner: &impl Runner) -> Result<Vec<Machine>, String> {
    let raw = runner
        .run(
            &installed_program("herdr"),
            &strings(&["machine", "list", "--json"]),
            Duration::from_secs(5),
        )
        .map_err(|error| error.to_string())?;
    let machines: Vec<Machine> =
        serde_json::from_str(&raw).map_err(|error| format!("machine catalog: {error}"))?;
    if machines.iter().any(|machine| {
        machine.id.is_empty() || machine.target.is_empty() || machine.session.is_empty()
    }) {
        return Err("machine catalog contains an incomplete profile".into());
    }
    Ok(machines)
}

pub fn split_query(query: &str) -> (Vec<String>, String) {
    let mut machines = Vec::new();
    let mut text = Vec::new();
    for word in query.split_whitespace() {
        let word = word.to_lowercase();
        if let Some(machine) = word.strip_prefix("machine:") {
            machines.push(machine.to_owned());
        } else {
            text.push(word);
        }
    }
    (machines, text.join(" "))
}

pub fn matches_query(machine: Option<&Machine>, query: &str) -> bool {
    machine.map_or_else(
        || "local".contains(query) || local_label().to_lowercase().contains(query),
        |machine| {
            machine.label.to_lowercase().contains(query)
                || machine.id.to_lowercase().contains(query)
        },
    )
}

#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
    session: String,
    operation: Operation,
}

#[derive(Serialize, Deserialize)]
pub enum Operation {
    Hello,
    Load,
    Summary(PathBuf),
    Worktree(crate::worktree::Operation),
    ZoxideAdd(PathBuf),
}

#[derive(Serialize, Deserialize)]
struct Response {
    version: u32,
    result: Result<serde_json::Value, String>,
}

#[derive(Clone)]
pub struct Remote {
    pub machine: Machine,
    executable: String,
}

impl Remote {
    pub fn discover(runner: &impl Runner, machine: &Machine) -> Result<Self, String> {
        if !machine.enabled {
            return Err("machine is disabled".into());
        }
        let raw = forwarded(runner, machine, &strings(&["plugin", "list", "--json"]))?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let plugin = value
            .pointer("/result/plugins")
            .and_then(serde_json::Value::as_array)
            .and_then(|plugins| {
                plugins.iter().find(|plugin| {
                    plugin["plugin_id"] == "local.unified-picker" && plugin["enabled"] == true
                })
            })
            .ok_or("enabled local.unified-picker plugin missing on remote host")?;
        let root = plugin["plugin_root"]
            .as_str()
            .filter(|root| root.starts_with('/'))
            .ok_or("remote plugin root missing or not absolute")?;
        let command = plugin["panes"]
            .as_array()
            .and_then(|panes| panes.iter().find(|pane| pane["id"] == "picker"))
            .and_then(|pane| pane["command"][0].as_str())
            .ok_or("remote picker executable missing")?;
        let executable = if command.starts_with('/') {
            command.to_owned()
        } else {
            format!("{root}/{command}")
        };
        let remote = Self {
            machine: machine.clone(),
            executable,
        };
        let greeting: String = remote
            .call_with(runner, Operation::Hello)
            .map_err(|error| {
                format!(
                    "Helper unavailable; install matching picker on {}: {error}",
                    machine.label
                )
            })?;
        if greeting != "herdr-picker" {
            return Err("unexpected remote helper handshake".into());
        }
        Ok(remote)
    }

    pub fn call<T: for<'de> Deserialize<'de>>(&self, operation: Operation) -> Result<T, String> {
        self.call_with(&SystemRunner, operation)
    }

    pub fn call_with<T: for<'de> Deserialize<'de>>(
        &self,
        runner: &impl Runner,
        operation: Operation,
    ) -> Result<T, String> {
        let timeout = if matches!(
            operation,
            Operation::Worktree(crate::worktree::Operation::Create(_))
        ) {
            Duration::from_secs(150)
        } else {
            READ_TIMEOUT
        };
        let payload = STANDARD.encode(
            serde_json::to_vec(&Request {
                version: PROTOCOL,
                session: self.machine.session.clone(),
                operation,
            })
            .map_err(|error| error.to_string())?,
        );
        let args = ssh_args(&self.machine.target, &self.executable, &payload)?;
        let raw = runner
            .run(OsStr::new("ssh"), &args, timeout)
            .map_err(|error| match error {
                crate::provider::Error::Command { message, .. } => {
                    format!("SSH helper failed: {message}")
                }
                error => error.to_string(),
            })?;
        decode_response(&raw)
    }
}

fn decode_response<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, String> {
    let response: Response = serde_json::from_str(raw).map_err(|error| {
        format!("remote helper response: {error}; install matching picker on destination")
    })?;
    if response.version != PROTOCOL {
        return Err(format!(
            "remote helper protocol {} != {PROTOCOL}; install matching picker",
            response.version
        ));
    }
    serde_json::from_value(response.result?)
        .map_err(|error| format!("remote helper payload: {error}"))
}

pub fn forwarded(
    runner: &impl Runner,
    machine: &Machine,
    args: &[OsString],
) -> Result<String, String> {
    if !machine.enabled {
        return Err("machine is disabled".into());
    }
    validate_profile(runner, machine)?;
    let mut all = strings(&["--machine", &machine.id]);
    all.extend_from_slice(args);
    runner
        .run(&installed_program("herdr"), &all, Duration::from_secs(10))
        .map_err(|error| error.to_string())
}

pub fn validate_profile(runner: &impl Runner, machine: &Machine) -> Result<(), String> {
    if catalog(runner)?.iter().any(|current| {
        current.enabled
            && current.id == machine.id
            && current.session == machine.session
            && current.target == machine.target
    }) {
        Ok(())
    } else {
        Err("machine profile changed, was disabled, or was removed; refresh the picker".into())
    }
}

fn strings(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn ssh_args(target: &str, executable: &str, payload: &str) -> Result<Vec<OsString>, String> {
    if target.starts_with('-')
        || target.contains(['\0', '\n', '\r'])
        || target.is_empty()
        || executable.contains('\0')
    {
        return Err("invalid SSH target or helper path".into());
    }
    Ok(strings(&[
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=2",
        "--",
        target,
        &format!("{} --helper-v1 {}", quote(executable), quote(payload)),
    ]))
}

pub fn helper(payload: &str) -> Result<(), String> {
    // This entrypoint runs before threads/UI startup, on the destination host only.
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        for suffix in [".local/bin", ".local/share/mise/shims", ".cargo/bin"] {
            paths.insert(0, PathBuf::from(&home).join(suffix));
        }
    }
    paths.push("/opt/homebrew/bin".into());
    std::env::set_var(
        "PATH",
        std::env::join_paths(paths).map_err(|error| error.to_string())?,
    );
    let result = (|| {
        let bytes = STANDARD
            .decode(payload)
            .map_err(|error| error.to_string())?;
        let request: Request = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if request.version != PROTOCOL {
            return Err("unsupported helper protocol".into());
        }
        let client =
            Client::new(SystemRunner, installed_program("herdr")).with_session(request.session);
        match request.operation {
            Operation::Hello => serde_json::to_value("herdr-picker"),
            Operation::Load => {
                let sources: Sources = client.load(None).map(|source| {
                    source.map(|items| {
                        items
                            .into_iter()
                            .map(|mut item| {
                                if let Target::Directory { path } = &mut item.target {
                                    *path = crate::model::canonicalish(path);
                                }
                                item
                            })
                            .collect()
                    })
                });
                serde_json::to_value(sources)
            }
            Operation::Summary(path) => serde_json::to_value(
                crate::provider::directory_summary(&SystemRunner, &path)
                    .map_err(|error| error.to_string())?,
            ),
            Operation::Worktree(operation) => {
                serde_json::to_value(crate::worktree::execute(operation)?)
            }
            Operation::ZoxideAdd(path) => {
                SystemRunner
                    .run(
                        &installed_program("zoxide"),
                        &["add".into(), "--".into(), path.into_os_string()],
                        Duration::from_secs(4),
                    )
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(())
            }
        }
        .map_err(|error| error.to_string())
    })();
    println!(
        "{}",
        serde_json::to_string(&Response {
            version: PROTOCOL,
            result
        })
        .map_err(|error| error.to_string())?
    );
    Ok(())
}

pub fn remote_items(machine: &Machine, sources: Sources) -> Sources {
    sources.map(|source| {
        source.and_then(|items| {
            if items
                .iter()
                .any(|item| matches!(item.target, Target::Remote { .. }))
            {
                return Err("nested remote helper target".into());
            }
            Ok(items
                .into_iter()
                .map(|item| item.on_machine(machine))
                .collect())
        })
    })
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub fn machine() -> Machine {
        Machine {
            id: "opaque-profile-id".into(),
            label: "goblin".into(),
            target: "dev@ssh-alias".into(),
            session: "review".into(),
            enabled: true,
        }
    }

    pub fn item(target: Target) -> Item {
        Item {
            id: "same-id".into(),
            label: "project".into(),
            detail: "/same/path".into(),
            search: "project /same/path".into(),
            preview_pane: Some("p1".into()),
            match_paths: vec!["/same/path".into()],
            target,
        }
    }

    type Calls = Arc<Mutex<Vec<(OsString, Vec<OsString>)>>>;

    #[derive(Clone, Default)]
    pub struct RemoteRunner {
        pub calls: Calls,
        pub helper_reply: serde_json::Value,
        pub fail_forward: bool,
        pub disabled: bool,
    }

    pub fn payload(args: &[OsString]) -> serde_json::Value {
        let command = args.last().unwrap().to_str().unwrap();
        let encoded = command.rsplit(' ').next().unwrap().trim_matches('\'');
        serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap()
    }

    impl Runner for RemoteRunner {
        fn run(
            &self,
            program: &OsStr,
            args: &[OsString],
            _: Duration,
        ) -> Result<String, crate::provider::Error> {
            self.calls
                .lock()
                .unwrap()
                .push((program.into(), args.to_vec()));
            if args.first().is_some_and(|arg| arg == "machine") {
                let mut machine = machine();
                machine.enabled = !self.disabled;
                return Ok(serde_json::to_string(&vec![machine]).unwrap());
            }
            if program == "ssh" {
                let request = payload(args);
                assert_eq!(request["version"], PROTOCOL);
                assert_eq!(request["session"], "review");
                let result = if request["operation"] == "Hello" {
                    serde_json::json!("herdr-picker")
                } else {
                    self.helper_reply.clone()
                };
                return Ok(
                    serde_json::json!({"version":PROTOCOL,"result":{"Ok":result}}).to_string(),
                );
            }
            if args.first().is_some_and(|arg| arg == "--machine") {
                assert_eq!(args[1], "opaque-profile-id");
                if args.get(2).is_some_and(|arg| arg == "plugin") {
                    return Ok(serde_json::json!({"result":{"plugins":[{"plugin_id":"local.unified-picker","enabled":true,"plugin_root":"/remote/a'b","panes":[{"id":"picker","command":["./target/release/herdr-picker"]}]}]}}).to_string());
                }
                if self.fail_forward {
                    return Err(crate::provider::Error::Command {
                        program: "remote herdr".into(),
                        message: "connection lost".into(),
                    });
                }
                return Ok(r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#.into());
            }
            panic!("unexpected local command: {program:?} {args:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_arguments_are_literal_and_options_cannot_be_injected() {
        let args = ssh_args("dev@host", "/a'b/$(touch nope)/picker", "a'b").unwrap();
        assert_eq!(
            args.last().unwrap(),
            "'/a'\\''b/$(touch nope)/picker' --helper-v1 'a'\\''b'"
        );
        assert!(ssh_args("-oProxyCommand=bad", "/picker", "").is_err());
    }
    #[test]
    fn rejects_protocol_mismatch_before_payload() {
        assert!(
            decode_response::<String>(r#"{"version":2,"result":{"Ok":"hello"}}"#)
                .unwrap_err()
                .contains("protocol")
        );
    }
    #[test]
    fn machine_query_is_independent_of_fuzzy_text() {
        assert_eq!(
            split_query("repo machine:goblin cli"),
            (vec!["goblin".into()], "repo cli".into())
        );
    }

    #[test]
    fn helper_discovery_uses_opaque_profile_and_installed_binary() {
        let runner = testing::RemoteRunner {
            helper_reply: serde_json::json!("summary"),
            ..Default::default()
        };
        let remote = Remote::discover(&runner, &testing::machine()).unwrap();
        assert_eq!(
            remote.executable,
            "/remote/a'b/./target/release/herdr-picker"
        );
        assert_eq!(
            remote
                .call_with::<String>(&runner, Operation::Summary("~/opaque".into()))
                .unwrap(),
            "summary"
        );
        let calls = runner.calls.lock().unwrap();
        let request = testing::payload(&calls.last().unwrap().1);
        assert_eq!(request["operation"]["Summary"], "~/opaque");
        assert!(calls
            .iter()
            .filter(|(program, _)| program != "ssh")
            .all(|(program, _)| *program == installed_program("herdr")));
    }
}
