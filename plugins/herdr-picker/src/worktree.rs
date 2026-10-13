use std::collections::{HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::model::{Item, Target};
use crate::provider::{Client, ConfirmResult, Runner, SystemRunner};

const READ_TIMEOUT: Duration = Duration::from_secs(4);
const CREATE_TIMEOUT: Duration = Duration::from_secs(120);
const COMPLETION_LIMIT: usize = 40;
const DIRECTORY_CACHE_LIMIT: usize = 16;

struct DirectoryListing {
    directory: PathBuf,
    entries: Result<Vec<PathBuf>, String>,
    scanned: Instant,
}

#[derive(Clone, Default)]
pub struct CompletionGate(Arc<AtomicBool>);

struct CompletionPermit(Arc<AtomicBool>);

impl CompletionGate {
    fn acquire(&self) -> Option<CompletionPermit> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| CompletionPermit(self.0.clone()))
    }
}

impl Drop for CompletionPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn git(
    runner: &impl Runner,
    repo: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<String, String> {
    let mut all = vec![OsString::from("-C"), repo.as_os_str().to_owned()];
    all.extend_from_slice(args);
    runner
        .run(OsStr::new("git"), &all, timeout)
        .map_err(|error| error.to_string())
}

fn read_git(runner: &impl Runner, repo: &Path, args: &[&str]) -> Result<String, String> {
    git(
        runner,
        repo,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        READ_TIMEOUT,
    )
}

#[derive(Clone, Debug)]
struct Branch {
    reference: String,
    label: String,
}

#[derive(Clone, Debug)]
struct Repository {
    root: PathBuf,
    branches: Vec<Branch>,
    current: Option<usize>,
}

fn discover(runner: &impl Runner, source: &Path) -> Result<Repository, String> {
    let root = read_git(runner, source, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root.strip_suffix('\n').unwrap_or(&root));
    let raw = read_git(
        runner,
        &root,
        &[
            "for-each-ref",
            "--format=%(refname)%09%(symref)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let branches: Vec<_> = raw
        .lines()
        .filter_map(|line| {
            let (reference, symbolic) = line.split_once('\t')?;
            symbolic.is_empty().then(|| Branch {
                reference: reference.into(),
                label: reference.strip_prefix("refs/heads/").map_or_else(
                    || format!("{} (remote)", reference.trim_start_matches("refs/remotes/")),
                    str::to_owned,
                ),
            })
        })
        .collect();
    // The selected linked worktree's HEAD is the default, not the main worktree's HEAD.
    let current = read_git(runner, source, &["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .and_then(|head| {
            branches
                .iter()
                .position(|branch| branch.reference == head.trim_end())
        });
    if branches.is_empty() {
        return Err("Repository has no local or remote branches with commits.".into());
    }
    Ok(Repository {
        root,
        branches,
        current,
    })
}

fn validate_name(runner: &impl Runner, repo: &Path, name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('-') {
        return Err("Enter a literal new branch name.".into());
    }
    let checked = read_git(runner, repo, &["check-ref-format", "--branch", name])?;
    if checked.trim_end() != name {
        return Err("Enter a literal new branch name.".into());
    }
    let refs = read_git(
        runner,
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    )?;
    if refs
        .lines()
        .any(|reference| reference == format!("refs/heads/{name}"))
    {
        return Err(format!("Branch {name:?} already exists."));
    }
    Ok(())
}

fn resolve(runner: &impl Runner, repo: &Path, reference: &str) -> Result<String, String> {
    read_git(
        runner,
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .map(|value| value.trim_end().to_owned())
}

fn suggested_path(repo: &Path, name: &str) -> PathBuf {
    let mut directory = repo
        .file_name()
        .unwrap_or(OsStr::new("worktree"))
        .to_os_string();
    directory.push("-");
    directory.push(name.replace('/', "-"));
    repo.parent().unwrap_or(repo).join(directory)
}

fn expand_path(value: &str, parent: &Path) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err("Enter a full destination path.".into());
    }
    let path = if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or(""))
    } else if value.starts_with('~') {
        return Err("Use ~ or ~/path; ~user is not supported.".into());
    } else {
        PathBuf::from(value)
    };
    Ok(if path.is_absolute() {
        path
    } else {
        parent.join(path)
    })
}

#[derive(Clone, Debug)]
struct Plan {
    repo: PathBuf,
    name: String,
    commit: String,
    destination: PathBuf,
}

fn validate_destination(plan: &Plan) -> Result<(), String> {
    match std::fs::symlink_metadata(&plan.destination) {
        Ok(_) => Err("Destination already exists. Choose a new directory.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn reconcile(runner: &impl Runner, plan: &Plan) -> Result<(), String> {
    let raw = read_git(
        runner,
        &plan.repo,
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    let expected_path = format!("worktree {}", plan.destination.display());
    let expected_branch = format!("branch refs/heads/{}", plan.name);
    let found = raw.split("\0\0").any(|entry| {
        let fields: Vec<_> = entry.split('\0').collect();
        fields.contains(&expected_path.as_str()) && fields.contains(&expected_branch.as_str())
    });
    if found
        && resolve(runner, &plan.destination, "HEAD")? == plan.commit
        && read_git(runner, &plan.destination, &["status", "--porcelain", "-z"])?.is_empty()
    {
        return Ok(());
    }
    Err("Creation could not be confirmed. Inspect `git worktree list`, the destination, and the new branch before retrying. No files or branches were removed.".into())
}

enum Creation {
    Created,
    Invalid(String),
    Uncertain(String),
}

fn create(runner: &impl Runner, plan: &Plan) -> Creation {
    if let Err(error) =
        validate_name(runner, &plan.repo, &plan.name).and_then(|()| validate_destination(plan))
    {
        return Creation::Invalid(error);
    }
    let args = vec![
        "worktree".into(),
        "add".into(),
        "--no-track".into(),
        "-b".into(),
        plan.name.clone().into(),
        "--".into(),
        plan.destination.as_os_str().to_owned(),
        plan.commit.clone().into(),
    ];
    match git(runner, &plan.repo, &args, CREATE_TIMEOUT) {
        Ok(_) => Creation::Created,
        Err(error) => match reconcile(runner, plan) {
            Ok(()) => Creation::Created,
            Err(detail) => Creation::Uncertain(format!("{error}\n{detail}")),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Loading,
    Name,
    Base,
    Destination,
    Review,
    Creating,
    Reconcile,
    Opening,
    OpenFailed,
}

#[derive(Default, Debug)]
struct Editor {
    text: String,
    cursor: usize,
}

impl Editor {
    fn set(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        let previous = self.text.clone();
        match key.code {
            KeyCode::Left => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(index, _)| index);
            }
            KeyCode::Right => {
                self.cursor += self.text[self.cursor..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let index = self.text[..self.cursor].char_indices().last().unwrap().0;
                self.text.drain(index..self.cursor);
                self.cursor = index;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                let end = self.cursor + self.text[self.cursor..].chars().next().unwrap().len_utf8();
                self.text.drain(self.cursor..end);
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => self.set(String::new()),
            KeyCode::Char('w') if key.modifiers == KeyModifiers::CONTROL => {
                let before = &self.text[..self.cursor];
                let trimmed = before.trim_end_matches(char::is_whitespace);
                let start = trimmed.rfind(char::is_whitespace).map_or(0, |index| {
                    index + trimmed[index..].chars().next().unwrap().len_utf8()
                });
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(character)
                if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                self.text.insert(self.cursor, character);
                self.cursor += character.len_utf8();
            }
            _ => {}
        }
        previous != self.text
    }
}

enum Reply {
    Repository(Result<Repository, String>),
    Name(Result<(), String>),
    Base(Result<String, String>),
    Destination(Result<Plan, String>),
    Create(Creation),
    Reconcile(Result<(), String>),
    Open(Result<ConfirmResult, String>),
}

fn spawn<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(operation());
    });
    receiver
}

pub struct Flow {
    step: Step,
    repo: Option<Repository>,
    name: Editor,
    base_query: Editor,
    destination: Editor,
    base: Option<usize>,
    commit: String,
    plan: Option<Plan>,
    error: Option<String>,
    job: Option<Receiver<Reply>>,
    completion_job: Option<Receiver<DirectoryListing>>,
    completion_gate: CompletionGate,
    directory_cache: VecDeque<DirectoryListing>,
    completions: Vec<String>,
    highlighted: Option<usize>,
    zoxide: Arc<[PathBuf]>,
    opened: Option<ConfirmResult>,
    retry_open: bool,
}

pub enum Action {
    Stay,
    Cancel,
}

impl Flow {
    pub fn new(source: PathBuf, zoxide: Vec<PathBuf>, completion_gate: CompletionGate) -> Self {
        Self {
            step: Step::Loading,
            repo: None,
            name: Editor::default(),
            base_query: Editor::default(),
            destination: Editor::default(),
            base: None,
            commit: String::new(),
            plan: None,
            error: None,
            job: Some(spawn(move || {
                Reply::Repository(discover(&SystemRunner, &source))
            })),
            completion_job: None,
            completion_gate,
            directory_cache: VecDeque::new(),
            completions: Vec::new(),
            highlighted: None,
            zoxide: zoxide.into(),
            opened: None,
            retry_open: false,
        }
    }

    fn parent(&self) -> &Path {
        let root = &self.repo.as_ref().unwrap().root;
        root.parent().unwrap_or(root)
    }

    fn branches(&self) -> Vec<usize> {
        self.repo.as_ref().map_or_else(Vec::new, |repo| {
            repo.branches
                .iter()
                .enumerate()
                .filter(|(_, branch)| fuzzy(&self.base_query.text, &branch.label))
                .map(|(index, _)| index)
                .collect()
        })
    }

    fn invalidate_completions(&mut self) {
        self.highlighted = None;
        self.refresh_completions();
    }

    fn refresh_completions(&mut self) {
        self.completions = if self.step != Step::Destination {
            Vec::new()
        } else if let Some((directory, query)) =
            completion_directory(&self.destination.text, self.parent())
        {
            self.directory_cache
                .iter()
                .find(|entry| entry.directory == directory)
                .and_then(|entry| entry.entries.as_ref().ok())
                .map_or_else(Vec::new, |entries| filter_directories(&query, entries))
        } else {
            complete_zoxide(&self.destination.text, &self.zoxide)
        };
    }

    pub fn awaiting_worker(&self) -> bool {
        self.job.is_some()
            || self.completion_job.is_some()
            || (self.step == Step::Destination
                && completion_directory(&self.destination.text, self.parent()).is_some_and(
                    |(directory, _)| {
                        !self
                            .directory_cache
                            .iter()
                            .any(|entry| entry.directory == directory)
                    },
                ))
    }

    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(reply) = self.job.as_ref().and_then(|job| job.try_recv().ok()) {
            self.job = None;
            changed = true;
            match reply {
                Reply::Repository(Ok(repo)) => {
                    self.base = repo.current;
                    self.repo = Some(repo);
                    self.step = Step::Name;
                }
                Reply::Repository(Err(error))
                | Reply::Name(Err(error))
                | Reply::Base(Err(error))
                | Reply::Destination(Err(error)) => self.error = Some(error),
                Reply::Name(Ok(())) => self.step = Step::Base,
                Reply::Base(Ok(commit)) => {
                    self.commit = commit;
                    if self.destination.text.is_empty() {
                        self.destination.set(
                            suggested_path(&self.repo.as_ref().unwrap().root, &self.name.text)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                    self.step = Step::Destination;
                    self.invalidate_completions();
                }
                Reply::Destination(Ok(plan)) => {
                    self.destination
                        .set(plan.destination.to_string_lossy().into_owned());
                    self.plan = Some(plan);
                    self.step = Step::Review;
                }
                Reply::Create(Creation::Created) | Reply::Reconcile(Ok(())) => {
                    self.step = Step::Opening;
                }
                Reply::Create(Creation::Invalid(error)) => {
                    self.step = Step::Destination;
                    self.error = Some(error);
                }
                Reply::Create(Creation::Uncertain(error)) | Reply::Reconcile(Err(error)) => {
                    self.step = Step::Reconcile;
                    self.error = Some(error);
                }
                Reply::Open(Ok(result)) => self.opened = Some(result),
                Reply::Open(Err(error)) => {
                    self.step = Step::OpenFailed;
                    self.error = Some(format!("Worktree created, but opening failed: {error}"));
                }
            }
        }
        if let Some(listing) = self
            .completion_job
            .as_ref()
            .and_then(|job| job.try_recv().ok())
        {
            self.completion_job = None;
            if self.directory_cache.len() == DIRECTORY_CACHE_LIMIT {
                self.directory_cache.pop_front();
            }
            self.directory_cache.push_back(listing);
            let previous = self.completions.clone();
            self.refresh_completions();
            changed |= previous != self.completions;
        }
        self.directory_cache.retain(|entry| {
            entry.entries.is_ok()
                || now.saturating_duration_since(entry.scanned) < Duration::from_secs(1)
        });
        if self.step == Step::Destination && self.completion_job.is_none() {
            let Some((directory, _)) = completion_directory(&self.destination.text, self.parent())
            else {
                return changed;
            };
            if self
                .directory_cache
                .iter()
                .any(|entry| entry.directory == directory)
            {
                return changed;
            }
            // Keep the limit across cancelled flows, including stalled network-filesystem scans.
            let Some(permit) = self.completion_gate.acquire() else {
                return changed;
            };
            self.completion_job = Some(spawn(move || {
                let _permit = permit;
                DirectoryListing {
                    entries: scan_directory(&directory),
                    directory,
                    scanned: Instant::now(),
                }
            }));
        }
        changed
    }

    pub fn start_open<R: Runner + Clone + 'static>(
        &mut self,
        client: &Client<R>,
        workspaces: &[Item],
    ) {
        if self.step != Step::Opening || self.job.is_some() || self.opened.is_some() {
            return;
        }
        let destination = self.plan.as_ref().unwrap().destination.clone();
        let retry = self.retry_open;
        let client = client.clone();
        let workspaces = workspaces.to_vec();
        self.job = Some(spawn(move || {
            let workspaces = if retry {
                match client.load(None).into_iter().next().unwrap() {
                    Ok(items) => items,
                    Err(error) => {
                        return Reply::Open(Err(format!(
                            "Could not refresh workspaces before retry: {error}"
                        )))
                    }
                }
            } else {
                workspaces
            };
            let result = client
                .confirm(
                    &Target::Directory {
                        path: destination.clone(),
                    },
                    &workspaces,
                )
                .map_err(|error| error.to_string());
            if result.is_ok() {
                let _ = SystemRunner.run(
                    OsStr::new("zoxide"),
                    &["add".into(), "--".into(), destination.into_os_string()],
                    READ_TIMEOUT,
                );
            }
            Reply::Open(result)
        }));
    }

    pub fn take_opened(&mut self) -> Option<(PathBuf, ConfirmResult)> {
        self.opened
            .take()
            .map(|result| (self.plan.as_ref().unwrap().destination.clone(), result))
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        if matches!(self.step, Step::Creating | Step::Opening) {
            return Action::Stay;
        }
        if key.code == KeyCode::Esc {
            if self.job.is_some()
                || matches!(self.step, Step::Loading | Step::Name | Step::OpenFailed)
            {
                return Action::Cancel;
            }
            self.error = None;
            self.step = match self.step {
                Step::Base => Step::Name,
                Step::Destination => Step::Base,
                Step::Review | Step::Reconcile => Step::Destination,
                _ => return Action::Cancel,
            };
            self.invalidate_completions();
            return Action::Stay;
        }
        if self.job.is_some() {
            return Action::Stay;
        }
        if key.code == KeyCode::BackTab {
            self.step = match self.step {
                Step::Base => Step::Name,
                Step::Destination => Step::Base,
                Step::Review => Step::Destination,
                step => step,
            };
            self.error = None;
            self.invalidate_completions();
            return Action::Stay;
        }
        if matches!(key.code, KeyCode::Up | KeyCode::Down) {
            let backwards = key.code == KeyCode::Up;
            if self.step == Step::Base {
                let rows = self.branches();
                self.base = next_index(
                    self.base
                        .and_then(|base| rows.iter().position(|row| *row == base)),
                    rows.len(),
                    backwards,
                )
                .map(|index| rows[index]);
            } else if self.step == Step::Destination {
                self.highlighted = next_index(self.highlighted, self.completions.len(), backwards);
            }
            return Action::Stay;
        }
        if self.step == Step::Destination
            && (key.code == KeyCode::Tab
                || key.code == KeyCode::Enter && self.highlighted.is_some())
        {
            if let Some(value) = self.completions.get(self.highlighted.unwrap_or(0)).cloned() {
                self.destination.set(value);
                self.invalidate_completions();
            }
            return Action::Stay;
        }
        if key.code == KeyCode::Enter
            || key.code == KeyCode::Tab && matches!(self.step, Step::Name | Step::Base)
        {
            self.submit();
            return Action::Stay;
        }
        match self.step {
            Step::Name => {
                if self.name.key(key) {
                    self.destination.set(String::new());
                    self.error = None;
                }
            }
            Step::Base => {
                if self.base_query.key(key) {
                    self.base = None;
                    self.error = None;
                }
            }
            Step::Destination => {
                if self.destination.key(key) {
                    self.invalidate_completions();
                    self.error = None;
                }
            }
            _ => {}
        }
        Action::Stay
    }

    fn submit(&mut self) {
        self.error = None;
        match self.step {
            Step::Name => {
                let root = self.repo.as_ref().unwrap().root.clone();
                let name = self.name.text.clone();
                self.job = Some(spawn(move || {
                    Reply::Name(validate_name(&SystemRunner, &root, &name))
                }));
            }
            Step::Base => {
                if let Some(base) = self.base.filter(|base| self.branches().contains(base)) {
                    let repo = self.repo.as_ref().unwrap();
                    let root = repo.root.clone();
                    let reference = repo.branches[base].reference.clone();
                    self.job = Some(spawn(move || {
                        Reply::Base(resolve(&SystemRunner, &root, &reference))
                    }));
                } else {
                    self.error =
                        Some("Choose a base branch with ↑/↓ (required for detached HEAD).".into());
                }
            }
            Step::Destination => match expand_path(&self.destination.text, self.parent()) {
                Ok(destination) => {
                    let mut plan = Plan {
                        repo: self.repo.as_ref().unwrap().root.clone(),
                        name: self.name.text.clone(),
                        commit: self.commit.clone(),
                        destination,
                    };
                    self.job = Some(spawn(move || {
                        Reply::Destination((|| {
                            plan.destination = absolute_destination(&plan.destination)?;
                            validate_destination(&plan)?;
                            Ok(plan)
                        })())
                    }));
                }
                Err(error) => self.error = Some(error),
            },
            Step::Review => {
                let plan = self.plan.clone().unwrap();
                self.step = Step::Creating;
                self.job = Some(spawn(move || Reply::Create(create(&SystemRunner, &plan))));
            }
            Step::Reconcile => {
                let plan = self.plan.clone().unwrap();
                self.job = Some(spawn(move || {
                    Reply::Reconcile(reconcile(&SystemRunner, &plan))
                }));
            }
            Step::OpenFailed => {
                self.retry_open = true;
                self.step = Step::Opening;
            }
            _ => {}
        }
    }

    pub fn draw(&self, frame: &mut Frame) {
        let suggestions = u16::try_from(
            match self.step {
                Step::Base => self.branches().len(),
                Step::Destination => self.completions.len(),
                _ => 0,
            }
            .min(5),
        )
        .unwrap_or(5);
        let error_rows = if self.error.is_some() { 3 } else { 1 };
        let area = frame.area();
        let width = area.width.min(80);
        let path_rows = if self.step == Step::Review {
            u16::try_from(
                UnicodeWidthStr::width(self.destination.text.as_str())
                    .div_ceil(usize::from(width.saturating_sub(4).max(1)))
                    .clamp(1, 4),
            )
            .unwrap_or(4)
        } else {
            1
        };
        let height = area
            .height
            .min((15 + suggestions + error_rows + path_rows).clamp(18, 24));
        let area = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let block = Block::bordered().title(" Create Worktree ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [context, name, base, destination, list, status, button, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(2 + path_rows),
            Constraint::Length(suggestions),
            Constraint::Min(error_rows),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        if let Some(repo) = &self.repo {
            frame.render_widget(
                Paragraph::new(format!(
                    "Repo: {}\nRelative paths from: {}",
                    repo.root.display(),
                    self.parent().display()
                ))
                .dim(),
                context,
            );
        }
        Self::draw_field(
            frame,
            name,
            "Name",
            &self.name,
            "New branch name",
            self.step == Step::Name,
        );
        let base_label = self
            .repo
            .as_ref()
            .and_then(|repo| self.base.map(|index| repo.branches[index].label.as_str()))
            .unwrap_or("Choose a base branch");
        Self::draw_field(
            frame,
            base,
            "Base",
            &self.base_query,
            base_label,
            self.step == Step::Base,
        );
        Self::draw_field(
            frame,
            destination,
            "Path",
            &self.destination,
            "Full destination path",
            self.step == Step::Destination,
        );
        self.draw_suggestions(frame, list);
        self.draw_actions(frame, status, button, footer);
    }

    fn draw_suggestions(&self, frame: &mut Frame, list: Rect) {
        let (rows, selected): (Vec<ListItem<'_>>, _) = if self.step == Step::Base {
            let indices = self.branches();
            let selected = self
                .base
                .and_then(|base| indices.iter().position(|index| *index == base));
            (
                indices
                    .iter()
                    .map(|index| {
                        ListItem::new(self.repo.as_ref().unwrap().branches[*index].label.as_str())
                    })
                    .collect(),
                selected,
            )
        } else {
            (
                self.completions
                    .iter()
                    .map(|value| ListItem::new(value.as_str()))
                    .collect(),
                self.highlighted,
            )
        };
        frame.render_stateful_widget(
            List::new(rows)
                .highlight_style(Style::new().black().on_cyan())
                .highlight_symbol("› "),
            list,
            &mut ListState::default().with_selected(selected),
        );
    }

    fn draw_actions(&self, frame: &mut Frame, status: Rect, button: Rect, footer: Rect) {
        let review = format!(
            "Commit: {} · Enter to create",
            &self.commit[..self.commit.len().min(12)]
        );
        let status_text = self.error.as_deref().unwrap_or_else(|| {
            if self.job.is_some() {
                "Working…"
            } else if self.step == Step::Review {
                &review
            } else {
                self.title()
            }
        });
        frame.render_widget(
            Paragraph::new(status_text)
                .style(if self.error.is_some() {
                    Style::new().red()
                } else {
                    Style::new().dim()
                })
                .wrap(Wrap { trim: false }),
            status,
        );
        frame.render_widget(
            Paragraph::new(match self.step {
                Step::OpenFailed => "[ Retry opening ]",
                Step::Reconcile => "[ Recheck creation ]",
                _ => "[ Create & open ]",
            })
            .centered()
            .style(if self.step == Step::Review {
                Style::new().black().on_cyan()
            } else {
                Style::new().dim()
            }),
            button,
        );
        let hint = if self.job.is_some() {
            "Working…"
        } else if self.step == Step::Destination {
            "Enter next/accept · Tab complete · ↑/↓ select · Esc back"
        } else if self.step == Step::Review {
            "Enter Create & open · Esc back"
        } else {
            "Tab/Enter next · Shift-Tab back · Esc back/cancel"
        };
        frame.render_widget(Paragraph::new(hint).dim(), footer);
    }

    fn draw_field(
        frame: &mut Frame,
        area: Rect,
        label: &str,
        editor: &Editor,
        placeholder: &str,
        active: bool,
    ) {
        let block = Block::bordered()
            .title(format!(" {label} "))
            .border_style(if active {
                Style::new().cyan()
            } else {
                Style::new().dim()
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if active {
            draw_editor(frame, inner, editor);
            if editor.text.is_empty() {
                frame.render_widget(Paragraph::new(placeholder).dim(), inner);
            }
        } else {
            let value = if label == "Base" || editor.text.is_empty() {
                placeholder
            } else {
                &editor.text
            };
            frame.render_widget(
                Paragraph::new(value).dim().wrap(Wrap { trim: false }),
                inner,
            );
        }
    }

    fn title(&self) -> &'static str {
        match self.step {
            Step::Loading if self.error.is_some() => "Repository unavailable · Esc to picker",
            Step::Loading => "Discovering repository",
            Step::Name => "1/3 · New branch name",
            Step::Base => "2/3 · Base branch",
            Step::Destination => "3/3 · Full destination path",
            Step::Review => "Create & open",
            Step::Creating => "Creating worktree…",
            Step::Reconcile => "Creation needs inspection · Enter to recheck",
            Step::Opening => "Opening worktree…",
            Step::OpenFailed => "Retry opening · Enter",
        }
    }
}

fn draw_editor(frame: &mut Frame, area: ratatui::layout::Rect, editor: &Editor) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = usize::from(area.width.saturating_sub(1));
    let mut start = editor.cursor;
    let mut used = 0;
    for (index, character) in editor.text[..editor.cursor].char_indices().rev() {
        let size = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + size > width {
            break;
        }
        used += size;
        start = index;
    }
    frame.render_widget(Paragraph::new(&editor.text[start..]), area);
    let offset = UnicodeWidthStr::width(&editor.text[start..editor.cursor]);
    frame.set_cursor_position((area.x + u16::try_from(offset).unwrap_or(0), area.y));
}

fn next_index(current: Option<usize>, length: usize, backwards: bool) -> Option<usize> {
    if length == 0 {
        None
    } else {
        Some(
            current.map_or(if backwards { length - 1 } else { 0 }, |index| {
                if backwards {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(length - 1)
                }
            }),
        )
    }
}

fn fuzzy(query: &str, value: &str) -> bool {
    let value = value.to_lowercase();
    let mut chars = value.chars();
    query
        .to_lowercase()
        .chars()
        .all(|wanted| chars.by_ref().any(|character| character == wanted))
}

fn complete_zoxide(text: &str, zoxide: &[PathBuf]) -> Vec<String> {
    let mut seen = HashSet::new();
    zoxide
        .iter()
        .filter(|path| fuzzy(text, &path.to_string_lossy()))
        .map(|path| format!("{}/", path.to_string_lossy().trim_end_matches('/')))
        .filter(|path| seen.insert(path.clone()))
        .take(COMPLETION_LIMIT)
        .collect()
}

fn completion_directory(text: &str, parent: &Path) -> Option<(PathBuf, String)> {
    if !text.contains('/') && !text.starts_with(['~', '.']) {
        return None;
    }
    let path = expand_path(text, parent).ok()?;
    let (directory, prefix) = if text.ends_with('/') || text == "~" {
        (path.as_path(), String::new())
    } else {
        (
            path.parent().unwrap_or(parent),
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        )
    };
    Some((directory.to_path_buf(), prefix))
}

fn scan_directory(directory: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .take(4096)
        .filter_map(|entry| match entry {
            Ok(entry) => match entry.file_type() {
                Ok(kind) if kind.is_dir() || kind.is_symlink() && entry.path().is_dir() => {
                    Some(Ok(entry.path()))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error.to_string())),
            },
            Err(error) => Some(Err(error.to_string())),
        })
        .collect()
}

fn filter_directories(query: &str, entries: &[PathBuf]) -> Vec<String> {
    let query = query.to_lowercase();
    let mut values: Vec<_> = entries
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            fuzzy(&query, &name).then(|| {
                (
                    if name == query {
                        0
                    } else if name.starts_with(&query) {
                        1
                    } else {
                        2
                    },
                    name,
                    path,
                )
            })
        })
        .collect();
    values.sort();
    values
        .into_iter()
        .take(COMPLETION_LIMIT)
        .map(|(_, _, path)| format!("{}/", path.display()))
        .collect()
}

#[cfg(test)]
fn complete(text: &str, parent: &Path, zoxide: &[PathBuf]) -> Vec<String> {
    match completion_directory(text, parent) {
        Some((directory, query)) => {
            filter_directories(&query, &scan_directory(&directory).unwrap_or_default())
        }
        None => complete_zoxide(text, zoxide),
    }
}

fn absolute_destination(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return std::fs::canonicalize(path).map_err(|error| error.to_string());
    }
    let parent = path.parent().ok_or("Invalid destination")?;
    if path.components().next_back() == Some(std::path::Component::ParentDir) {
        // Resolve existing ancestors (including symlinks) before collapsing a missing suffix.
        let mut resolved = absolute_destination(parent)?;
        match std::fs::metadata(&resolved) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err("Destination parent is not a directory".into())
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.to_string())
            }
            _ => {}
        }
        resolved.pop();
        return Ok(resolved);
    }
    let name = path
        .file_name()
        .ok_or("Destination must end in a directory name")?;
    Ok(absolute_destination(parent)?.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_w_deletes_previous_word_and_whitespace_without_touching_suffix() {
        for (text, cursor, expected, position) in [
            ("one two  ", 9, "one ", 4),
            ("one two suffix", 7, "one  suffix", 4),
            ("héllo\u{2003}🦀word  ", 19, "héllo\u{2003}", 9),
            ("/src/my-repo", 12, "", 0),
            ("   ", 3, "", 0),
            ("suffix", 0, "suffix", 0),
        ] {
            let mut editor = Editor {
                text: text.into(),
                cursor,
            };
            let changed = editor.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
            assert_eq!(editor.text, expected);
            assert_eq!(editor.cursor, position);
            assert_eq!(changed, text != expected);
        }
    }

    fn repository() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source repo");
        std::fs::create_dir(&root).unwrap();
        read_git(&SystemRunner, &root, &["init", "-b", "main"]).unwrap();
        read_git(
            &SystemRunner,
            &root,
            &["config", "user.email", "test@example.com"],
        )
        .unwrap();
        read_git(&SystemRunner, &root, &["config", "user.name", "Test"]).unwrap();
        read_git(&SystemRunner, &root, &["config", "commit.gpgsign", "false"]).unwrap();
        std::fs::write(root.join("tracked"), "original\n").unwrap();
        read_git(&SystemRunner, &root, &["add", "tracked"]).unwrap();
        read_git(&SystemRunner, &root, &["commit", "-m", "initial"]).unwrap();
        (temp, std::fs::canonicalize(root).unwrap())
    }

    fn plan(root: &Path) -> Plan {
        Plan {
            repo: root.into(),
            name: "feature/test".into(),
            commit: resolve(&SystemRunner, root, "refs/heads/main").unwrap(),
            destination: suggested_path(root, "feature/test"),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn settle(flow: &mut Flow) {
        let start = Instant::now();
        while flow.job.is_some() {
            flow.poll();
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn discovers_nested_and_linked_repositories_filters_symbolic_remotes_and_detached_default() {
        let (_temp, root) = repository();
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        read_git(
            &SystemRunner,
            &root,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        )
        .unwrap();
        read_git(
            &SystemRunner,
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )
        .unwrap();
        let repo = discover(&SystemRunner, &nested).unwrap();
        assert_eq!(repo.root, root);
        assert_eq!(repo.branches.len(), 2);
        assert_eq!(
            repo.branches[repo.current.unwrap()].reference,
            "refs/heads/main"
        );
        let plan = plan(&root);
        assert!(matches!(create(&SystemRunner, &plan), Creation::Created));
        let linked = discover(&SystemRunner, &plan.destination).unwrap();
        assert_eq!(linked.root, plan.destination);
        assert_eq!(
            linked.branches[linked.current.unwrap()].reference,
            "refs/heads/feature/test"
        );
        read_git(&SystemRunner, &plan.destination, &["checkout", "--detach"]).unwrap();
        assert!(discover(&SystemRunner, &plan.destination)
            .unwrap()
            .current
            .is_none());
    }

    #[test]
    fn validates_names_and_creates_exact_commit_without_upstream_or_shell_interpolation() {
        let (_temp, root) = repository();
        for invalid in ["", "main", "-bad", "bad name", "a..b", "@{-1}"] {
            assert!(
                validate_name(&SystemRunner, &root, invalid).is_err(),
                "{invalid}"
            );
        }
        let mut plan = plan(&root);
        plan.destination = root.parent().unwrap().join("space ; $(literal)");
        assert!(matches!(create(&SystemRunner, &plan), Creation::Created));
        assert_eq!(
            resolve(&SystemRunner, &plan.destination, "HEAD").unwrap(),
            plan.commit
        );
        assert_eq!(
            std::fs::read_to_string(plan.destination.join("tracked")).unwrap(),
            "original\n"
        );
        assert!(read_git(
            &SystemRunner,
            &plan.destination,
            &["rev-parse", "--verify", "@{upstream}"]
        )
        .is_err());
        assert!(matches!(create(&SystemRunner, &plan), Creation::Invalid(_)));
        reconcile(&SystemRunner, &plan).unwrap();
        std::fs::remove_file(plan.destination.join("tracked")).unwrap();
        assert!(reconcile(&SystemRunner, &plan).is_err());
    }

    struct ReportTimeout;
    impl Runner for ReportTimeout {
        fn run(
            &self,
            program: &OsStr,
            args: &[OsString],
            timeout: Duration,
        ) -> Result<String, crate::provider::Error> {
            let result = SystemRunner.run(program, args, timeout)?;
            if args.get(2) == Some(&OsString::from("worktree"))
                && args.get(3) == Some(&OsString::from("add"))
            {
                assert_eq!(timeout, CREATE_TIMEOUT);
                return Err(crate::provider::Error::Timeout {
                    program: "git".into(),
                });
            }
            Ok(result)
        }
    }

    #[test]
    fn reconciles_success_when_create_reports_timeout() {
        let (_temp, root) = repository();
        assert!(matches!(
            create(&ReportTimeout, &plan(&root)),
            Creation::Created
        ));
    }

    struct PartialCreation;
    impl Runner for PartialCreation {
        fn run(
            &self,
            program: &OsStr,
            args: &[OsString],
            timeout: Duration,
        ) -> Result<String, crate::provider::Error> {
            if args.get(2) == Some(&OsString::from("worktree"))
                && args.get(3) == Some(&OsString::from("add"))
            {
                SystemRunner.run(
                    program,
                    &[
                        args[0].clone(),
                        args[1].clone(),
                        "branch".into(),
                        args[6].clone(),
                        args[9].clone(),
                    ],
                    timeout,
                )?;
                return Err(crate::provider::Error::Timeout {
                    program: "git".into(),
                });
            }
            SystemRunner.run(program, args, timeout)
        }
    }

    #[test]
    fn branch_only_partial_creation_stays_uncertain_and_is_not_recreated() {
        let (_temp, root) = repository();
        let plan = plan(&root);
        assert!(matches!(
            create(&PartialCreation, &plan),
            Creation::Uncertain(_)
        ));
        assert!(!plan.destination.exists());
        assert!(validate_name(&SystemRunner, &root, &plan.name).is_err());
        assert!(matches!(create(&SystemRunner, &plan), Creation::Invalid(_)));
        assert_eq!(
            resolve(&SystemRunner, &root, "refs/heads/feature/test").unwrap(),
            plan.commit
        );
    }

    #[derive(Clone)]
    struct FailedOpen(Arc<std::sync::atomic::AtomicUsize>);
    impl Runner for FailedOpen {
        fn run(
            &self,
            _program: &OsStr,
            _args: &[OsString],
            _timeout: Duration,
        ) -> Result<String, crate::provider::Error> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(crate::provider::Error::Timeout {
                program: "herdr".into(),
            })
        }
    }

    #[test]
    fn opening_failure_is_single_flight_and_keeps_created_worktree_for_retry() {
        let (_temp, root) = repository();
        let plan = plan(&root);
        assert!(matches!(create(&SystemRunner, &plan), Creation::Created));
        let mut flow = Flow::new(root, Vec::new(), CompletionGate::default());
        settle(&mut flow);
        flow.plan = Some(plan.clone());
        flow.step = Step::Opening;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = Client::new(FailedOpen(calls.clone()), "herdr".into());
        flow.start_open(&client, &[]);
        flow.start_open(&client, &[]);
        settle(&mut flow);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(flow.step, Step::OpenFailed);
        assert!(flow.take_opened().is_none());
        assert!(flow.error.as_ref().unwrap().contains("Worktree created"));
        reconcile(&SystemRunner, &plan).unwrap();
        flow.key(key(KeyCode::Enter));
        assert_eq!(flow.step, Step::Opening);
        assert!(flow.retry_open);
        assert!(flow.job.is_none());
    }

    #[test]
    fn existing_destination_is_never_overwritten() {
        let (_temp, root) = repository();
        let plan = plan(&root);
        std::fs::create_dir(&plan.destination).unwrap();
        std::fs::write(plan.destination.join("precious"), "keep").unwrap();
        assert!(matches!(create(&SystemRunner, &plan), Creation::Invalid(_)));
        assert_eq!(
            std::fs::read_to_string(plan.destination.join("precious")).unwrap(),
            "keep"
        );
        validate_name(&SystemRunner, &root, &plan.name).unwrap();
    }

    #[test]
    fn completion_modes_relative_paths_and_unicode_cursor_editing() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("apple")).unwrap();
        std::fs::create_dir(temp.path().join("apricot")).unwrap();
        std::fs::write(temp.path().join("ap-file"), "").unwrap();
        let rows = complete("./ap", temp.path(), &[]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|path| path.ends_with('/')));
        assert_eq!(
            complete(
                "src",
                temp.path(),
                &[
                    "/source".into(),
                    "/source/".into(),
                    "/else".into(),
                    "/src-second".into()
                ]
            ),
            vec!["/source/", "/src-second/"]
        );
        assert_eq!(
            expand_path("new", temp.path()).unwrap(),
            temp.path().join("new")
        );
        assert!(expand_path("~user/new", temp.path()).is_err());
        assert_eq!(
            expand_path("~/new", temp.path()).unwrap(),
            PathBuf::from(std::env::var_os("HOME").unwrap()).join("new")
        );
        assert_eq!(
            suggested_path(Path::new("/parent/repo"), "a/b"),
            Path::new("/parent/repo-a-b")
        );
        let mut editor = Editor::default();
        editor.set("a🦀b".into());
        editor.key(key(KeyCode::Left));
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.text, "ab");
        assert_eq!(editor.cursor, 1);
        editor.key(key(KeyCode::Char('é')));
        editor.key(key(KeyCode::Delete));
        assert_eq!(editor.text, "aé");
    }

    #[test]
    fn zoxide_completion_acceptance_keeps_new_directory_under_selected_path() {
        let (_temp, root) = repository();
        let mut flow = Flow::new(root, Vec::new(), CompletionGate::default());
        settle(&mut flow);
        flow.step = Step::Destination;
        flow.completions = complete("src", flow.parent(), &["/source".into()]);
        flow.key(key(KeyCode::Tab));
        for character in "new".chars() {
            flow.key(key(KeyCode::Char(character)));
        }
        assert_eq!(flow.destination.text, "/source/new");
        assert_eq!(
            complete("", Path::new("/"), &["/".into(), "//".into()]),
            vec!["/"]
        );
    }

    #[test]
    fn completion_is_immediate_and_reuses_directory_listing_across_edits() {
        let (_temp, root) = repository();
        std::fs::create_dir(root.join("apple")).unwrap();
        std::fs::create_dir(root.join("apricot")).unwrap();
        let mut flow = Flow::new(
            root.clone(),
            vec!["/source".into()],
            CompletionGate::default(),
        );
        settle(&mut flow);
        flow.step = Step::Destination;
        flow.key(key(KeyCode::Char('s')));
        assert_eq!(flow.completions, vec!["/source/"]);
        assert!(flow.completion_job.is_none());
        flow.destination.set(format!("{}/ap", root.display()));
        flow.invalidate_completions();
        flow.poll();
        assert!(flow.completion_job.is_some());
        let start = Instant::now();
        while flow.completion_job.is_some() {
            flow.poll();
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(flow.completions.len(), 2);
        std::fs::remove_dir(root.join("apple")).unwrap();
        flow.key(key(KeyCode::Char('l')));
        assert_eq!(flow.completions, vec![format!("{}/apple/", root.display())]);
        flow.poll();
        assert!(flow.completion_job.is_none());
        flow.key(key(KeyCode::Backspace));
        assert_eq!(flow.completions.len(), 2);
    }

    #[test]
    fn directory_results_filter_latest_query_rank_matches_and_keep_full_listing() {
        let (_temp, root) = repository();
        let mut entries: Vec<_> = (0..60)
            .map(|i| root.join(format!("apple-{i:02}")))
            .collect();
        entries.extend([root.join("maple"), root.join("ap"), root.join("Apricot")]);
        let mut flow = Flow::new(root.clone(), Vec::new(), CompletionGate::default());
        settle(&mut flow);
        flow.step = Step::Destination;
        flow.destination.set(format!("{}/a", root.display()));
        let (tx, rx) = mpsc::sync_channel(1);
        flow.completion_job = Some(rx);
        flow.key(key(KeyCode::Char('p')));
        tx.send(DirectoryListing {
            directory: root.clone(),
            entries: Ok(entries),
            scanned: Instant::now(),
        })
        .unwrap();
        flow.poll();
        assert_eq!(flow.completions.len(), COMPLETION_LIMIT);
        assert_eq!(flow.completions[0], format!("{}/ap/", root.display()));
        flow.destination.set(format!("{}/act", root.display()));
        flow.invalidate_completions();
        assert_eq!(
            flow.completions,
            vec![format!("{}/Apricot/", root.display())]
        );
        flow.poll();
        assert!(flow.completion_job.is_none());
    }

    #[test]
    fn failed_listings_expire_cache_is_bounded_and_gate_survives_cancel() {
        let (_temp, root) = repository();
        let gate = CompletionGate::default();
        let permit = gate.acquire().unwrap();
        let mut flow = Flow::new(root.clone(), Vec::new(), gate.clone());
        settle(&mut flow);
        let now = Instant::now();
        for i in 0..20 {
            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(DirectoryListing {
                directory: root.join(i.to_string()),
                entries: Ok(Vec::new()),
                scanned: now,
            })
            .unwrap();
            flow.completion_job = Some(rx);
            flow.poll_at(now);
        }
        assert_eq!(flow.directory_cache.len(), DIRECTORY_CACHE_LIMIT);
        flow.directory_cache.push_back(DirectoryListing {
            directory: root.clone(),
            entries: Err("offline".into()),
            scanned: now,
        });
        flow.poll_at(now + Duration::from_secs(2));
        assert!(!flow
            .directory_cache
            .iter()
            .any(|entry| entry.directory == root));
        flow.step = Step::Destination;
        flow.destination.set(format!("{}/", root.display()));
        flow.poll();
        assert!(flow.completion_job.is_none());
        drop(flow);
        let mut next = Flow::new(root.clone(), Vec::new(), gate);
        settle(&mut next);
        next.step = Step::Destination;
        next.destination.set(format!("{}/", root.display()));
        next.poll();
        assert!(next.completion_job.is_none());
        drop(permit);
        next.poll();
        assert!(next.completion_job.is_some());
    }

    #[test]
    fn tab_validates_before_advancing_and_shift_tab_returns_to_previous_field() {
        let (_temp, root) = repository();
        let mut flow = Flow::new(root, Vec::new(), CompletionGate::default());
        settle(&mut flow);
        flow.key(key(KeyCode::Tab));
        settle(&mut flow);
        assert_eq!(flow.step, Step::Name);
        assert!(flow.error.is_some());
        flow.name.set("topic".into());
        flow.key(key(KeyCode::Tab));
        settle(&mut flow);
        assert_eq!(flow.step, Step::Base);
        flow.key(key(KeyCode::Tab));
        settle(&mut flow);
        assert_eq!(flow.step, Step::Destination);
        flow.key(key(KeyCode::BackTab));
        assert_eq!(flow.step, Step::Base);
        flow.key(key(KeyCode::BackTab));
        assert_eq!(flow.step, Step::Name);
    }

    #[test]
    fn git_valid_names_with_separate_at_and_brace_are_allowed_but_checkout_shorthand_is_not() {
        let (_temp, root) = repository();
        validate_name(&SystemRunner, &root, "foo@bar{baz").unwrap();
        read_git(&SystemRunner, &root, &["checkout", "-b", "previous"]).unwrap();
        read_git(&SystemRunner, &root, &["checkout", "main"]).unwrap();
        assert_eq!(
            read_git(
                &SystemRunner,
                &root,
                &["check-ref-format", "--branch", "@{-1}"]
            )
            .unwrap()
            .trim_end(),
            "previous"
        );
        assert!(validate_name(&SystemRunner, &root, "@{-1}")
            .unwrap_err()
            .contains("literal new branch"));
    }

    #[test]
    fn destination_normalizes_parent_components_in_existing_and_missing_suffixes() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        std::fs::create_dir_all(root.join("existing/child")).unwrap();
        assert_eq!(
            absolute_destination(&root.join("existing/child/..")).unwrap(),
            root.join("existing")
        );
        assert_eq!(
            absolute_destination(&root.join("existing/child/../new")).unwrap(),
            root.join("existing/new")
        );
        assert_eq!(
            absolute_destination(&root.join("missing/../new/deep")).unwrap(),
            root.join("new/deep")
        );
        assert!(!root.join("missing").exists());
        std::fs::write(root.join("file"), "keep").unwrap();
        assert!(absolute_destination(&root.join("file/../new")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("existing/child"), root.join("alias")).unwrap();
            assert_eq!(
                absolute_destination(&root.join("alias/../new")).unwrap(),
                root.join("existing/new")
            );
        }
    }

    #[test]
    fn input_screens_show_repository_and_relative_path_context() {
        let (_temp, root) = repository();
        let mut flow = Flow::new(root.clone(), Vec::new(), CompletionGate::default());
        settle(&mut flow);
        for step in [Step::Name, Step::Base, Step::Destination] {
            flow.step = step;
            for (width, height) in [(40, 18), (180, 30)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal.draw(|frame| flow.draw(frame)).unwrap();
                let cursor = terminal.get_cursor_position().unwrap();
                assert!(cursor.x < width && cursor.y < height);
                assert_eq!(
                    terminal.backend().buffer()[(cursor.x - 1, cursor.y)].fg,
                    ratatui::style::Color::Cyan
                );
                let rendered: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect();
                assert!(rendered.contains("Repo:"));
                assert!(rendered.contains("Relative paths from:"));
                for label in [
                    "Create Worktree",
                    " Name ",
                    " Base ",
                    " Path ",
                    "[ Create & open ]",
                ] {
                    assert!(rendered.contains(label), "missing {label}");
                }
                let buffer = terminal.backend().buffer();
                let occupied: Vec<_> = buffer
                    .content
                    .chunks(width as usize)
                    .enumerate()
                    .filter(|(_, row)| row.iter().any(|cell| cell.symbol() != " "))
                    .collect();
                assert!(occupied.len() <= 24);
                if width == 180 {
                    assert!(occupied[0].0 > 0);
                    assert!(buffer[(49, u16::try_from(occupied[0].0).unwrap())].symbol() == " ");
                }
            }
        }
    }

    #[test]
    fn flow_back_completion_generation_single_flight_and_retry_open_states() {
        let (_temp, root) = repository();
        let mut flow = Flow::new(root.clone(), Vec::new(), CompletionGate::default());
        settle(&mut flow);
        assert_eq!(flow.step, Step::Name);
        flow.name.set("new/branch".into());
        flow.key(key(KeyCode::Enter));
        settle(&mut flow);
        assert_eq!(flow.step, Step::Base);
        flow.key(key(KeyCode::Enter));
        settle(&mut flow);
        assert_eq!(flow.step, Step::Destination);
        assert!(flow.destination.text.ends_with("source repo-new-branch"));
        flow.completions = vec!["/chosen/".into()];
        flow.highlighted = Some(0);
        flow.key(key(KeyCode::Enter));
        assert_eq!(flow.step, Step::Destination);
        assert_eq!(flow.destination.text, "/chosen/");
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(DirectoryListing {
            directory: root.clone(),
            entries: Ok(vec![root.join("stale")]),
            scanned: Instant::now(),
        })
        .unwrap();
        flow.completion_job = Some(rx);
        flow.poll();
        assert!(flow.completions.is_empty());
        assert!(flow
            .directory_cache
            .iter()
            .any(|listing| listing.directory == root));
        flow.key(key(KeyCode::Esc));
        assert_eq!(flow.step, Step::Base);
        flow.base = None;
        flow.key(key(KeyCode::Enter));
        assert!(flow.error.is_some());
        assert!(flow.job.is_none());
        flow.step = Step::Creating;
        assert!(matches!(flow.key(key(KeyCode::Esc)), Action::Stay));
        flow.key(key(KeyCode::Enter));
        assert!(flow.job.is_none());
        flow.step = Step::OpenFailed;
        flow.key(key(KeyCode::Enter));
        assert_eq!(flow.step, Step::Opening);
        assert!(flow.retry_open);
    }

    #[test]
    fn flow_validates_destination_before_review_then_creates_asynchronously() {
        let (_temp, root) = repository();
        let mut flow = Flow::new(root, Vec::new(), CompletionGate::default());
        settle(&mut flow);
        flow.name.set("topic".into());
        for _ in 0..3 {
            flow.key(key(KeyCode::Enter));
            settle(&mut flow);
        }
        assert_eq!(flow.step, Step::Review);
        let path = flow.plan.as_ref().unwrap().destination.clone();
        assert!(!path.exists());
        flow.key(key(KeyCode::Enter));
        assert_eq!(flow.step, Step::Creating);
        flow.key(key(KeyCode::Enter));
        settle(&mut flow);
        assert_eq!(flow.step, Step::Opening);
        assert!(path.join("tracked").exists());
    }

    #[test]
    fn responsive_ui_shows_retry_error_and_keeps_unicode_cursor_in_bounds() {
        let (_temp, root) = repository();
        let mut flow = Flow::new(root, Vec::new(), CompletionGate::default());
        settle(&mut flow);
        flow.name.set("a long 🦀 branch name".into());
        for (width, height) in [(1, 1), (10, 3), (40, 10), (100, 30)] {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|frame| flow.draw(frame)).unwrap();
        }
        flow.step = Step::OpenFailed;
        flow.error = Some("Worktree created, but opening failed".into());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| flow.draw(frame)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("Retry opening"));
        assert!(rendered.contains("Worktree created"));
    }
}
