//! A file command line, carried out: `cp a b` read into its action, made
//! absolute against the directory it was typed in, and invoked on the
//! environment through the broker inside a guest, or carried out with the
//! person's own permissions outside one (the host's shell has no record).
//!
//! The `cp` a guest's `PATH` finds and the `cp` the session's shell runs
//! in-process are this one function, so they cannot answer differently.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::broker::{Request, Response};

use super::{FileAction, Recoverable, anchored, cli, perform};

/// One command line a shell ran, as History groups what it changed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunGroup {
    pub id: String,
    /// The command line.
    pub label: String,
}

/// The kind of group a shell's run is in History: one undo takes all of it
/// back. The same name as `collaboration`'s `steps::COMMAND_RUN`.
pub const COMMAND_RUN: &str = "command-run";

/// Where a file command answers: its output and its errors, and the
/// broker of the guest it runs in, when it runs in one.
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// The guest's broker socket; `None` outside a guest.
    pub broker: Option<&'a Path>,
    /// Where what an action replaces is kept outside a guest.
    pub recoverable: PathBuf,
    /// Whether the broker's progress lines are shown (on a terminal).
    pub progress: bool,
}

/// Run the file command `name` with `args` typed in `cwd`. Answers its
/// status.
pub fn run(name: &str, args: &[String], cwd: &Path, io: &mut Io<'_>) -> i32 {
    run_within(name, args, cwd, None, io)
}

/// [`run`], as part of the shell's run `run`: History groups what it
/// changes under that run.
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

/// Carry out `action` for the command `name`.
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
        action => perform(action, &policy, &store, None)
            .map(|done| done.len())
            .map_err(|(error, _)| error.to_string()),
    };
    match result {
        Ok(operations) => done(io, json, verb, None, operations),
        Err(reason) => refused(io, json, name, &reason),
    }
}

/// Say what a file command did; `entry` names it in the environment's
/// History when it was the record's action.
fn done(io: &mut Io<'_>, json: bool, verb: &str, entry: Option<&str>, operations: usize) -> i32 {
    if json {
        let line = serde_json::json!({"action": verb, "entry": entry, "operations": operations});
        let _ = writeln!(io.out, "{line}");
        return 0;
    }
    let word = if operations == 1 {
        "operation"
    } else {
        "operations"
    };
    let _ = if entry.is_some() {
        writeln!(
            io.out,
            "done ({operations} {word}); undo it from the environment's History"
        )
    } else {
        writeln!(
            io.out,
            "done ({operations} {word}); `history` lists what it kept aside"
        )
    };
    0
}

fn refused(io: &mut Io<'_>, json: bool, name: &str, reason: &str) -> i32 {
    if json {
        let line = serde_json::json!({"action": name, "error": reason});
        let _ = writeln!(io.out, "{line}");
    } else {
        let _ = writeln!(io.err, "{name}: {reason}");
    }
    1
}
