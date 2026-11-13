//! A file command line, carried out: `cp a b` read into its action, made absolute against
//! the directory it was typed in, and invoked on the environment through the broker
//! inside a guest, or carried out with the person's own permissions outside one.
//!
//! The `cp` a guest's `PATH` finds and the one the session's shell runs in-process are
//! this one function, so they cannot answer differently.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::broker::{Request, Response};

use super::{FileAction, Recoverable, anchored, cli, perform};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunGroup {
    pub id: String,
    pub label: String,
}

pub const COMMAND_RUN: &str = "command-run";

pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    pub broker: Option<&'a Path>,
    pub recoverable: PathBuf,
    pub progress: bool,
}

pub fn run(name: &str, args: &[String], cwd: &Path, io: &mut Io<'_>) -> i32 {
    run_within(name, args, cwd, None, io)
}

pub fn run_within(
    name: &str,
    args: &[String],
    cwd: &Path,
    run: Option<&RunGroup>,
    io: &mut Io<'_>,
) -> i32 {
    let Some(tool) = cli::Tool::of(name) else {
        let _ = writeln!(io.err, "{name}: not a file action this runtime answers");
        return 1;
    };
    let (json, args) = cli::take_json(args);
    match cli::parse(tool, &args) {
        Ok(action) => act(name, action, json, cwd, run, io),
        Err(error) => refused(io, json, name, &error.to_string()),
    }
}

pub fn run_action(name: &str, action: FileAction, json: bool, cwd: &Path, io: &mut Io<'_>) -> i32 {
    act(name, action, json, cwd, None, io)
}

fn act(
    name: &str,
    action: FileAction,
    json: bool,
    cwd: &Path,
    run: Option<&RunGroup>,
    io: &mut Io<'_>,
) -> i32 {
    let verb = action.verb();
    let action = anchored(action, cwd);
    if let Some(broker) = io.broker {
        let request = Request::FileAction {
            action,
            run: run.cloned(),
        };
        let answer = crate::broker::ask_streaming_with_progress(
            broker,
            &request,
            io.out,
            io.err,
            io.progress,
        );
        return match answer {
            Ok(Response::FileAction { id, operations }) => {
                done(io, json, verb, Some(&id), operations)
            }
            Ok(Response::Denied { reason }) => refused(io, json, name, &reason),
            Ok(other) => {
                let _ = writeln!(io.out, "{other:?}");
                0
            }
            Err(e) => refused(
                io,
                json,
                name,
                &format!("the broker is not reachable at {} ({e})", broker.display()),
            ),
        };
    }
    let store = Recoverable::at(io.recoverable.clone());
    let policy = crate::policy::Policy::unbounded();
    let result = match &action {
        FileAction::Restore(op) => store
            .entry(&op.kept)
            .ok_or_else(|| format!("{}: nothing kept under that id", op.kept))
            .and_then(|entry| {
                store
                    .restore(&op.kept, &entry.path)
                    .map(|()| 1)
                    .map_err(|error| error.to_string())
            }),
        action => perform(
            action,
            &policy,
            &store,
            &tilo_content::ContentStore::memory(),
            None,
        )
        .map(|done| done.len())
        .map_err(|(error, _)| error.to_string()),
    };
    match result {
        Ok(operations) => done(io, json, verb, None, operations),
        Err(reason) => refused(io, json, name, &reason),
    }
}

fn done(io: &mut Io<'_>, json: bool, verb: &str, entry: Option<&str>, operations: usize) -> i32 {
    cli::answer_done(io.out, json, verb, entry, operations)
}

fn refused(io: &mut Io<'_>, json: bool, name: &str, reason: &str) -> i32 {
    cli::answer_refused(io.out, io.err, json, name, reason)
}
