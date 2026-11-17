//! A kept app in a test: the test binary itself is the keeper, started by a stand-in launcher as
//! `tilo-launcher run-host --keeper` starts `tilo-app-keeper`. The keeper's program is another
//! crate's, which a test of this crate or of the session does not build.

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tilo_app_keeper::wire::Description;

/// What makes a test binary a keeper: its arguments, one per line.
const KEEPER_ARGS: &str = "TILO_TEST_KEEPER_ARGS";

/// Run as the keeper when [`keeper_program`] started this process: the body of the test that
/// program names. Does nothing in any other run of that test.
///
/// # Errors
/// The keeper's arguments do not read, or the keeper failed.
pub fn serve_keeper() -> io::Result<()> {
    let Ok(args) = std::env::var(KEEPER_ARGS) else {
        return Ok(());
    };
    let mut options = tilo_app_keeper::Options::from_args(args.lines().map(Into::into))
        .map_err(io::Error::other)?;
    // A keeper whose app ended waits this long for a session to hear it: a test's next run is
    // there in milliseconds, or never comes.
    options.grace = Duration::from_secs(5);
    tilo_app_keeper::Keeper::new(options).and_then(tilo_app_keeper::Keeper::run)
}

/// Write at `path` a program that runs this test binary's test `test` as the keeper; that test
/// calls [`serve_keeper`].
///
/// # Errors
/// The program could not be written.
pub fn keeper_program(path: &Path, test: &str) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    program(
        path,
        &format!(
            "{KEEPER_ARGS}=\"$(printf '%s\\n' \"$@\")\" exec {} --exact {} --nocapture",
            quoted(&exe.to_string_lossy()),
            quoted(test),
        ),
    )
}

/// Write at `path` a stand-in for the launcher a host app starts through: it starts the keeper
/// it is handed, in the directory and as the launch it is handed, with `app` as the app's command
/// line, and takes no other argument into account.
///
/// # Errors
/// The program could not be written.
pub fn launcher_program(path: &Path, app: &[String]) -> io::Result<()> {
    let app: Vec<String> = app.iter().map(|arg| quoted(arg)).collect();
    program(
        path,
        &format!(
            "while [ $# -gt 0 ]; do case \"$1\" in --keeper) k=$2; shift;; --keep-in) d=$2; shift;; --keep-as) l=$2; shift;; esac; shift; done\n\
             exec \"$k\" --dir \"$d\" --launch \"$l\" -- {}",
            app.join(" ")
        ),
    )
}

/// What each keeper under `root` that answers says it keeps, with its directory.
#[must_use]
pub fn described(root: &Path) -> Vec<(PathBuf, Description)> {
    super::dirs(root)
        .into_iter()
        .filter_map(|dir| {
            let greeted = super::greet(&dir, super::ANSWER).ok()?;
            Some((dir, greeted.description))
        })
        .collect()
}

/// A shell script at `path` running `body`.
fn program(path: &Path, body: &str) -> io::Result<()> {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// `text` as one word of a shell command line.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
