//! One run of a coding agent's harness on this computer, followed by the turn it answers.
//!
//! The harness is a process of this computer, started in the agent's own working copy
//! ([`working_copy`]), as the user the engine runs as. Its output is read line by line into
//! what it said ([`Heard`]); a message for it goes to its input; a stop ends it and every
//! process it started, which share its process group. Its model and Tilo's MCP server are
//! answered at a loopback endpoint of the run's own ([`super::endpoint`]), so the workspace's
//! model key never reaches it.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;
use tilo_workspace_agent::Coding;

use super::endpoint::{Endpoint, Serves};
use super::harness::{Harness, Says, Start};

/// How long a harness that was asked to end is given before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// The most of what a run printed beside its protocol that is kept for the words of a failure.
const BESIDE_KEPT: usize = 4 * 1024;

/// How a run came to its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The harness ended by itself.
    Finished,
    /// It was stopped.
    Stopped,
    /// It ended saying it failed.
    Failed,
    /// It went away under its run: a signal nobody here sent.
    Lost,
}

impl Ending {
    /// The word a record keeps for it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Lost => "lost",
        }
    }
}

/// A run that is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub how: Ending,
    /// The harness's exit status, when it ran to one.
    pub exit: Option<i32>,
    /// One plain line of why, empty for a run that finished.
    pub words: String,
}

/// What a run says, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Said(Says),
    Over(Ended),
}

/// One run of a harness.
pub(super) struct Run {
    harness: Arc<dyn Harness>,
    pid: rustix::process::Pid,
    input: Mutex<Option<ChildStdin>>,
    heard: tokio::sync::mpsc::UnboundedReceiver<Heard>,
    stopping: Arc<AtomicBool>,
    /// Its process ended: its group is nobody's to signal any more.
    exited: Arc<AtomicBool>,
    spent: Arc<AtomicU64>,
    /// Answers its model and Tilo for as long as the run is held.
    _endpoint: Endpoint,
}

impl Run {
    /// Start `harness` for `coding`'s work, taking up its conversation as `start` says, its
    /// endpoints answered by `serves`.
    ///
    /// # Errors
    /// The working copy could not be made, or the harness could not be started: why, in words
    /// its person reads.
    pub fn start(
        harness: Arc<dyn Harness>,
        coding: &Coding,
        start: &Start,
        serves: Serves,
    ) -> Result<Self, String> {
        let directory = working_copy(coding)?;
        let spent = Arc::new(AtomicU64::new(0));
        let endpoint = Endpoint::open(serves, Arc::clone(&spent))
            .map_err(|error| format!("The coding agent's model endpoint did not open: {error}"))?;
        let program = harness.program(start, &endpoint.endpoints());
        let (name, arguments) = program
            .argv
            .split_first()
            .ok_or("a coding harness names no program")?;
        let mut child = Command::new(name)
            .args(arguments)
            .envs(program.env)
            .current_dir(&directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "{} is not installed on this computer, so I cannot work here: install it \
                     and write to me again.",
                    harness.name()
                ),
                _ => format!("{} did not start: {error}", harness.name()),
            })?;
        let pid = rustix::process::Pid::from_child(&child);
        let input = child.stdin.take();
        let (said, heard) = tokio::sync::mpsc::unbounded_channel();
        let beside = Arc::new(Mutex::new(String::new()));
        if let Some(mut errors) = child.stderr.take() {
            let beside = Arc::clone(&beside);
            std::thread::Builder::new()
                .name("coding-run-err".into())
                .spawn(move || {
                    let mut chunk = [0_u8; 4096];
                    while let Ok(read @ 1..) = errors.read(&mut chunk) {
                        keep(
                            &mut beside.lock_recover(),
                            &String::from_utf8_lossy(chunk.get(..read).unwrap_or_default()),
                        );
                    }
                })
                .map_err(|error| error.to_string())?;
        }
        let output = child.stdout.take().ok_or("the harness has no output")?;
        let (reads, stopping) = (harness.reads(), Arc::new(AtomicBool::new(false)));
        let exited = Arc::new(AtomicBool::new(false));
        let (stopped, ended) = (Arc::clone(&stopping), Arc::clone(&exited));
        std::thread::Builder::new()
            .name("coding-run".into())
            .spawn(move || {
                let mut reads = reads;
                for line in BufReader::new(output).lines() {
                    let Ok(line) = line else { break };
                    let says = reads.line(&line);
                    if says.is_empty() && !line.trim().is_empty() {
                        keep(&mut beside.lock_recover(), &format!("{line}\n"));
                    }
                    for says in says {
                        let _ = said.send(Heard::Said(says));
                    }
                }
                let status = child.wait().ok();
                ended.store(true, Ordering::SeqCst);
                let exit = status.and_then(|status| status.code());
                let beside = last_line(&beside.lock_recover()).to_owned();
                let (how, words) = if stopped.load(Ordering::SeqCst) {
                    (Ending::Stopped, String::new())
                } else {
                    match exit {
                        Some(0) => (Ending::Finished, String::new()),
                        Some(_) => (Ending::Failed, beside),
                        None => (Ending::Lost, beside),
                    }
                };
                let _ = said.send(Heard::Over(Ended { how, exit, words }));
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            harness,
            pid,
            input: Mutex::new(input),
            heard,
            stopping,
            exited,
            spent,
            _endpoint: endpoint,
        })
    }

    /// Give the harness `text` as its next message: its person's words, or a note of its
    /// starter. `false` once its input is closed.
    pub fn tell(&self, text: &str) -> bool {
        let mut input = self.input.lock_recover();
        let Some(writing) = input.as_mut() else {
            return false;
        };
        let told = writing
            .write_all(&self.harness.message(text))
            .and_then(|()| writing.flush());
        if told.is_err() {
            *input = None;
        }
        drop(input);
        told.is_ok()
    }

    /// Say that nothing more will be said to it: a harness that reads to the end of its input
    /// finishes and ends by itself.
    pub fn close(&self) {
        self.input.lock_recover().take();
    }

    /// End it and what it started: asked first, killed once its grace passed.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.close();
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        let pid = self.pid;
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        let exited = Arc::clone(&self.exited);
        let _ = std::thread::Builder::new()
            .name("coding-run-stop".into())
            .spawn(move || {
                std::thread::sleep(STOP_GRACE);
                if !exited.load(Ordering::SeqCst) {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
            });
    }

    /// What it says next, waited for at most `wait`; none when it said nothing meanwhile.
    pub async fn next(&mut self, wait: Duration) -> Option<Heard> {
        match tokio::time::timeout(wait, self.heard.recv()).await {
            Ok(Some(heard)) => Some(heard),
            // Whatever follows it is gone: it is over, however it ended.
            Ok(None) => Some(Heard::Over(Ended {
                how: Ending::Lost,
                exit: None,
                words: String::new(),
            })),
            Err(_) => None,
        }
    }

    /// What its model calls cost so far, in millionths of the currency.
    pub fn spent_micros(&self) -> u64 {
        self.spent.load(Ordering::SeqCst)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // A run nobody holds has nobody to answer: it ends with its handle.
        if !self.exited.load(Ordering::SeqCst) {
            let _ = rustix::process::kill_process_group(self.pid, rustix::process::Signal::KILL);
        }
    }
}

/// Keep the end of `text` for the words of a failure.
fn keep(beside: &mut String, text: &str) {
    beside.push_str(text);
    if beside.len() > BESIDE_KEPT {
        let mut from = beside.len().saturating_sub(BESIDE_KEPT);
        while !beside.is_char_boundary(from) {
            from = from.saturating_add(1);
        }
        beside.drain(..from);
    }
}

/// The last line that says something of `text`.
fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

/// `path` on this computer: as given when absolute, else under the home folder of the user the
/// engine runs as.
fn on_this_computer(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_owned();
    }
    std::env::var_os("HOME").map_or_else(|| path.to_owned(), |home| PathBuf::from(home).join(path))
}

/// `git` with `arguments`, its own words when it refuses.
fn git(arguments: &[&std::ffi::OsStr]) -> Result<String, String> {
    let ran = Command::new("git")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("git did not start on this computer: {error}"))?;
    if ran.status.success() {
        return Ok(String::from_utf8_lossy(&ran.stdout).trim().to_owned());
    }
    Err(last_line(&String::from_utf8_lossy(&ran.stderr)).to_owned())
}

/// The coding agent's own working copy of its repository, on its own branch: made when it is
/// not there yet (a run that is taken up again finds it). Several agents work on one repository
/// at once this way, and git itself keeps the rule that a branch is checked out in one working
/// copy at a time.
///
/// # Errors
/// The repository is none, or git refused: why, in words its person reads.
fn working_copy(coding: &Coding) -> Result<PathBuf, String> {
    let directory = on_this_computer(&coding.directory);
    if directory.join(".git").exists() {
        return Ok(directory);
    }
    let repository = on_this_computer(&coding.repository);
    let os = |text: &'static str| std::ffi::OsStr::new(text);
    git(&[
        os("-C"),
        repository.as_os_str(),
        os("rev-parse"),
        os("--git-dir"),
    ])
    .map_err(|_| {
        format!(
            "{} is not a git repository on this computer, so no working copy of it can be made.",
            coding.repository
        )
    })?;
    if let Some(parent) = directory.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("The working copy's folder was not made: {error}"))?;
    }
    let branch = std::ffi::OsString::from(&coding.branch);
    let reference = std::ffi::OsString::from(format!("refs/heads/{}", coding.branch));
    let held = git(&[
        os("-C"),
        repository.as_os_str(),
        os("show-ref"),
        os("--verify"),
        os("--quiet"),
        &reference,
    ])
    .is_ok();
    let made = if held {
        git(&[
            os("-C"),
            repository.as_os_str(),
            os("worktree"),
            os("add"),
            directory.as_os_str(),
            &branch,
        ])
    } else {
        let base = std::ffi::OsString::from(&coding.base);
        let mut arguments = vec![
            os("-C"),
            repository.as_os_str(),
            os("worktree"),
            os("add"),
            os("-b"),
            &branch,
            directory.as_os_str(),
        ];
        if !coding.base.is_empty() {
            arguments.push(&base);
        }
        git(&arguments)
    };
    made.map_err(|why| format!("No working copy of {} was made: {why}", coding.repository))?;
    Ok(directory)
}
