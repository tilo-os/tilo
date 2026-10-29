//! ush's shell: POSIX sh, and the bash features people use, interpreted in
//! Rust.
//!
//! It runs wherever a workspace is open: on a device, on a workspace's host,
//! or compiled to WebAssembly in a browser. Nothing in it forks or execs. A
//! subshell is a copy of the shell's state, and a pipe between two of its
//! own commands is a pipe in memory. Everything it reaches outside itself is
//! a call on the [`Host`] that runs it (see `docs/app-model/ush-portable.md`).
//!
//! Each command resolves, in order, to a function, one of the shell's own
//! built-ins, a tier-1 command ([`commands`]: Tilo's own, run in-process,
//! the file-changing ones as the environment's actions), and only then to
//! [`Host::program`]. A host that can't run programs says so, and the shell
//! tells the person the command needs tier 2 or 3.

mod arith;
mod brace;
mod builtins;
pub mod commands;
mod dispatch;
mod editor;
mod exec;
mod expand;
mod glob;
pub mod host;
pub mod io;
pub mod jobs;
pub mod memory;
pub mod paths;
mod pattern;
mod printf;
pub mod repl;
mod shell;
mod test;
mod vars;

pub use host::{DirEntry, FileCall, HistoryQuery, Host, Kind, Metadata, Program, Run, WriteMode};
pub use io::{Input, Output, Std, Stdio};
pub use memory::MemoryHost;
pub use repl::{Session, incomplete};
pub use shell::Shell;
