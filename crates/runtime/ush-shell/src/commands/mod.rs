//! Tier 1: Tilo's own commands, run in the shell's process on whatever hosts
//! it.
//!
//! The commands that read (`ls`, `cat`, `grep`, …) read through the
//! [`Host`], which answers only what the environment's grants let it. The
//! commands that change files (`cp`, `mv`, `rm`, …) are the environment's
//! actions, so they go to [`Host::file_action`] whole. None of them is a
//! process: a pipe between two of them is a pipe in memory.
//!
//! The commands that run commands (`find -exec`, `xargs`, `env`, `timeout`,
//! `nice`, `nohup`) hand them back to the shell through [`Ctx::dispatch`],
//! so a command they run resolves as any utility does and its file actions
//! belong to the same run.
//!
//! # Values first, then a rendering
//!
//! A command works out typed values (entries, matches, hunks) and renders
//! them for [`Ctx::render`]: [`Render::Text`] is the GNU tools' text, and
//! [`Render::Agent`] is the short form in [`agent`]: grouped, with exact
//! counts, ending with how much it left out and how to get it all
//! (`USH_RENDER=text`). `ls`, `grep`, `find` and `diff` have both renderings
//! now; the others render text for both readers until they get theirs.

use std::io;
use std::path::{Path, PathBuf};

use crate::host::Host;
use crate::io::Stdio;

pub mod agent;
mod cut;
mod diff;
mod files;
mod find;
mod grep;
mod ls;
pub mod options;
mod run;
mod sort;
mod stat;
mod text;
mod tree;
mod util;
mod xargs;

/// The commands that change files: each one is one of the environment's
/// actions ([`Host::file_action`]).
pub const FILE_ACTIONS: &[&str] = &[
    "cp", "mv", "rm", "mkdir", "rmdir", "ln", "truncate", "touch", "append",
];

/// Everything a tier-1 command gets.
pub struct Ctx<'a> {
    pub host: &'a dyn Host,
    /// The directory relative paths are taken against.
    pub cwd: &'a Path,
    pub io: &'a Stdio,
    /// The exported environment, for `env` and `printenv`.
    pub env: &'a [(String, String)],
    /// The command's own name, for its messages.
    pub name: &'a str,
    /// How a command that runs other commands (`find -exec`, `xargs`,
    /// `env`, `timeout`) runs them: back through the shell's own
    /// resolution, never a fork or an exec.
    pub dispatch: &'a dyn Dispatch,
    /// Who the output is for.
    pub render: Render,
    /// The run the command is part of: what its file actions are stamped
    /// with.
    pub run: Option<&'a crate::host::Run>,
}

/// Who a command's output is for. A command works out typed values first
/// (rows, matches, hunks, entries) and renders them for its reader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Render {
    /// People: the GNU tools' text, byte for byte where it matters.
    #[default]
    Text,
    /// An agent: short and structured, grouped, with exact counts; nothing
    /// is left out without saying how much.
    Agent,
}

/// Running a command from inside a command.
pub trait Dispatch: Sync {
    /// Run `argv` as the shell resolves a utility: one of Tilo's own
    /// commands, a file action, or a program; never a function or a
    /// special built-in, as POSIX says of `find -exec` and `xargs`.
    /// Answers its status: 127 when nothing answers to the name.
    fn run(&self, argv: &[String], cwd: &Path, io: &Stdio) -> i32;

    /// [`Self::run`], in an environment changed as `env` changes it.
    fn run_with_env(&self, argv: &[String], cwd: &Path, io: &Stdio, env: &EnvChange) -> i32;

    /// [`Self::run`], stopped when `millis` have passed: `None` then.
    fn run_for(&self, argv: &[String], cwd: &Path, io: &Stdio, millis: u64) -> Option<i32>;

    /// Ask the person a yes-or-no question on the terminal (`find -ok`).
    fn confirm(&self, question: &str) -> bool;
}

/// How `env` changes the environment of the command it runs: everything
/// exported is dropped first under `clear` (`-i`), then `unset` (`-u`), then
/// `set` (`NAME=VALUE`), in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvChange {
    pub clear: bool,
    pub unset: Vec<String>,
    pub set: Vec<(String, String)>,
}

/// Dispatch where there is no shell to dispatch to: every command is
/// unknown.
pub struct NoDispatch;

impl Dispatch for NoDispatch {
    fn run(&self, argv: &[String], _: &Path, io: &Stdio) -> i32 {
        let name = argv.first().map_or("", String::as_str);
        let _ = io.stderr.write_str(&format!("{name}: command not found\n"));
        127
    }

    fn run_with_env(&self, argv: &[String], cwd: &Path, io: &Stdio, _: &EnvChange) -> i32 {
        self.run(argv, cwd, io)
    }

    fn run_for(&self, argv: &[String], cwd: &Path, io: &Stdio, _: u64) -> Option<i32> {
        Some(self.run(argv, cwd, io))
    }

    fn confirm(&self, _: &str) -> bool {
        false
    }
}

/// Why a command stopped writing: its reader went away, or the person
/// interrupted it. Either way it ends quietly.
#[derive(Debug, Clone, Copy)]
pub struct Stop(pub i32);

impl From<io::Error> for Stop {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::BrokenPipe {
            Self(141)
        } else {
            Self(1)
        }
    }
}

impl Ctx<'_> {
    /// Ask for one of the environment's file actions (`find -delete` is
    /// `rm`), as part of this command's run.
    pub fn file_action(&self, name: &str, args: &[String]) -> i32 {
        self.host.file_action(&crate::host::FileCall {
            name,
            args,
            cwd: self.cwd,
            io: self.io,
            run: self.run,
        })
    }

    /// `path` made absolute against the working directory.
    pub fn resolve(&self, path: &str) -> PathBuf {
        crate::paths::join(self.cwd, path)
    }

    pub fn out(&self, bytes: &[u8]) -> Result<(), Stop> {
        if self.host.interrupted() {
            return Err(Stop(130));
        }
        self.io.stdout.write_all(bytes).map_err(Stop::from)
    }

    pub fn out_str(&self, text: &str) -> Result<(), Stop> {
        self.out(text.as_bytes())
    }

    /// Say something went wrong, as `name: message`.
    pub fn error(&self, message: impl std::fmt::Display) {
        let _ = self
            .io
            .stderr
            .write_str(&format!("{}: {message}\n", self.name));
    }

    /// An operand's error, as the GNU tools word it: `name: path: why`.
    pub fn io_error(&self, operand: &str, error: &io::Error) {
        self.error(format!("{operand}: {}", describe(error)));
    }

    /// Refuse a flag this command does not implement, by its spelling.
    pub fn unsupported(&self, flag: &str) -> i32 {
        self.error(format!("{flag} is not supported"));
        2
    }

    /// The bytes of `operand`: standard input for `-`, else the file.
    pub fn read_operand(&self, operand: &str) -> io::Result<Vec<u8>> {
        if operand == "-" {
            return self.io.stdin.read_all();
        }
        let path = self.resolve(operand);
        if self.host.metadata(&path)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "Is a directory",
            ));
        }
        let mut reader = self.host.open_read(&path)?;
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut reader, &mut bytes)?;
        Ok(bytes)
    }
}

/// An I/O error as the shell says it: `No such file or directory`, not
/// `entity not found`.
pub fn describe(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".into(),
        // A host's own refusal says why (a path no grant covers); the
        // system's says only that.
        io::ErrorKind::PermissionDenied if error.get_ref().is_some() => error.to_string(),
        io::ErrorKind::PermissionDenied => "Permission denied".into(),
        io::ErrorKind::IsADirectory => "Is a directory".into(),
        io::ErrorKind::NotADirectory => "Not a directory".into(),
        io::ErrorKind::AlreadyExists => "File exists".into(),
        _ => error.to_string(),
    }
}

/// A tier-1 command.
pub type Command = fn(&Ctx<'_>, &[String]) -> i32;

/// The tier-1 command `name`, if there is one. The file actions are not
/// here: they are the host's ([`FILE_ACTIONS`]).
pub fn lookup(name: &str) -> Option<Command> {
    files::lookup(name)
        .or_else(|| text::lookup(name))
        .or_else(|| run::lookup(name))
}

/// Every tier-1 command's name, file actions included, sorted.
pub fn names() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = files::NAMES
        .iter()
        .chain(text::NAMES)
        .chain(run::NAMES)
        .chain(FILE_ACTIONS)
        .copied()
        .collect();
    all.sort_unstable();
    all
}
