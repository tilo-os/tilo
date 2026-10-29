//! What the shell needs from wherever it runs.
//!
//! The interpreter itself touches nothing: no file, no process, no clock.
//! Everything it reaches is a call on [`Host`], so the same shell runs on a
//! device, on a workspace's host, or compiled to WebAssembly in a browser,
//! and each of those decides what a path means and who may read it.
//!
//! - **Files** are read through [`Host::metadata`], [`Host::read_dir`] and
//!   [`Host::open_read`]; a host checks its grants there. A redirection that
//!   writes a file asks [`Host::open_write`], naming the run it is part of.
//! - **The environment's file actions** (`cp`, `mv`, `rm`, …) go to
//!   [`Host::file_action`] whole, as their command lines: each is one action
//!   on the environment, checked and undone there.
//! - **A program that is none of the shell's own** goes to [`Host::program`].
//!   A host that can run it (a guest on a Linux device) does; one that can't
//!   says so, and the shell tells the person which tier it would need.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use std::sync::Arc;

use crate::io::Stdio;
use crate::jobs::Job;

/// What kind of thing a path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

/// What the shell knows about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub kind: Kind,
    pub len: u64,
    /// Permission bits, as `stat` reports them (`0o644`).
    pub mode: u32,
    /// Last modified, in seconds since the Unix epoch.
    pub modified: i64,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    /// The device and inode, for `-ef`.
    pub dev: u64,
    pub ino: u64,
}

impl Metadata {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    pub fn is_file(&self) -> bool {
        self.kind == Kind::File
    }
}

/// One entry of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: Kind,
}

/// How a redirection writes a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// `>`: replace what is there.
    Truncate,
    /// `>>`: add to the end.
    Append,
    /// `>` under `set -C`: only a file that is not there yet.
    CreateNew,
}

/// The run a command belongs to: one command line a person or an agent
/// ran. Every change a file action of the run makes is stamped with it, so
/// the run is attributed, and undone, as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Stable for the run, unique in the session.
    pub id: String,
    /// What History calls it: the command line.
    pub label: String,
}

/// One of the environment's file actions, as a command line asked for it.
#[derive(Debug, Clone, Copy)]
pub struct FileCall<'a> {
    /// `cp`, `mv`, `rm`, `mkdir`, `rmdir`, `ln`, `truncate`, `touch` or
    /// `append`.
    pub name: &'a str,
    pub args: &'a [String],
    /// The directory relative paths are taken against.
    pub cwd: &'a Path,
    pub io: &'a Stdio,
    /// The run it is part of, if the shell is running one.
    pub run: Option<&'a Run>,
}

/// What became of a program the shell handed to [`Host::program`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// It ran, and ended with this status.
    Exited(i32),
    /// This host has no such program.
    NotFound,
    /// This host cannot run programs that are not the shell's own: it would
    /// need a WebAssembly tool (tier 2) or a Linux place (tier 3).
    NeedsTier,
}

/// A question to the person's command history ([`Host::history`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    /// How many of the newest matching command lines to pass over.
    pub skip: usize,
    /// How many to answer at most.
    pub limit: usize,
    /// Only command lines that contain this; every line when empty.
    pub matching: String,
}

/// Everything the interpreter reaches outside itself.
pub trait Host: Send + Sync {
    fn metadata(&self, path: &Path) -> io::Result<Metadata>;

    /// Like [`Self::metadata`], but of a symlink itself.
    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata> {
        self.metadata(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>>;

    /// A file a redirection (or `tee`, `sort -o`) writes, as part of `run`
    /// when the shell is running one: what it writes is one change of that
    /// run's.
    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        run: Option<&Run>,
    ) -> io::Result<Box<dyn Write + Send>>;

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{}: not a symbolic link", path.display()),
        ))
    }

    /// One of the environment's file actions, as the command line that asked
    /// for it. Answers the status.
    fn file_action(&self, call: &FileCall<'_>) -> i32;

    /// A program that is none of the shell's own, with `env` its whole
    /// environment.
    fn program(&self, argv: &[String], env: &[(String, String)], cwd: &Path, io: &Stdio)
    -> Program;

    /// [`Self::program`], stopped as soon as `stop` says so (`timeout`).
    /// A host that cannot stop a program it started runs it to its end.
    fn program_until(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: &dyn Fn() -> bool,
    ) -> Program {
        let _ = stop;
        self.program(argv, env, cwd, io)
    }

    /// [`Self::program`] as part of `job`: the shell's job control stops
    /// it (Ctrl+Z), continues it, brings it to the foreground and ends it
    /// through what the host attaches to the job ([`Job::attach`]). A host
    /// that cannot signal its programs can only end them.
    fn program_in_job(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        job: &Arc<Job>,
    ) -> Program {
        self.program_until(argv, env, cwd, io, &|| job.killed())
    }

    /// Whether the person pressed Ctrl+Z since the last call: the job in
    /// the foreground stops.
    fn take_suspend(&self) -> bool {
        false
    }

    /// Whether the person asked to stop what is running (Ctrl-C). Long loops
    /// and commands ask between steps.
    fn interrupted(&self) -> bool {
        false
    }

    /// Forget a pending interrupt, once it has stopped what it was for.
    fn clear_interrupt(&self) {}

    /// Seconds since the Unix epoch.
    fn now(&self) -> i64 {
        0
    }

    /// Milliseconds since the Unix epoch, for durations (`timeout`).
    fn now_ms(&self) -> i64 {
        self.now().saturating_mul(1000)
    }

    /// Seconds east of UTC for local times, at `at`.
    fn utc_offset(&self, _at: i64) -> i32 {
        0
    }

    /// Wait this many milliseconds, or until interrupted.
    fn sleep(&self, _millis: u64) {}

    /// The name of the user a uid is, for `ls -l`.
    fn user_name(&self, uid: u32) -> Option<String> {
        let _ = uid;
        None
    }

    /// The home directory of the user `name`, for `~name`.
    fn home_of(&self, name: &str) -> Option<PathBuf> {
        let _ = name;
        None
    }

    fn group_name(&self, gid: u32) -> Option<String> {
        let _ = gid;
        None
    }

    /// The process id `$$` reports.
    fn pid(&self) -> u32 {
        1
    }

    /// The machine's name for prompts.
    fn hostname(&self) -> String {
        String::from("localhost")
    }

    /// Put the terminal on the shell's standard input into taking keys one
    /// at a time, unechoed, for the line editor (`on`), or give it back to
    /// its line discipline. Answers whether there is a terminal to edit on:
    /// without one, the shell reads lines as they come.
    fn edit_mode(&self, on: bool) -> bool {
        let _ = on;
        false
    }

    /// The terminal's width in columns, for the line editor.
    fn columns(&self) -> usize {
        80
    }

    /// The person's command history, newest first, when this host holds it
    /// (`terminal.history`). `None` when it does not: in a guest the record
    /// is Terminal's, and the editor asks Terminal on the session's
    /// terminal instead.
    fn history(&self, query: &HistoryQuery) -> Option<Vec<String>> {
        let _ = query;
        None
    }

    /// The programs this host can run, by name, looked for along `path`
    /// (the shell's `PATH`): the environment's tools, for Tab completion.
    fn programs(&self, path: &str) -> Vec<String> {
        let _ = path;
        Vec::new()
    }
}
