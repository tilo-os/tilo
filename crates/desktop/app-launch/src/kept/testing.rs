//! A kept app in a test: the test binary itself is the keeper, started by a stand-in launcher as
//! `tilo-launcher machine-app --keeper` starts `tilo-app-keeper`. The keeper's program is another
//! crate's, which a test of this crate or of the session does not build.

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tilo_app_keeper::wire::Description;

/// A keeper directory a service makes, as the Linux VM's supervisor does.
pub use tilo_app_keeper::registry::new_dir;

/// What makes a test binary a keeper: its arguments, one per line.
const KEEPER_ARGS: &str = "TILO_TEST_KEEPER_ARGS";

/// Run as the keeper when [`keeper_program`] started this process: the body of the test that
/// program names. Does nothing in any other run of that test.
///
/// # Errors
/// The keeper's arguments do not read, or the keeper failed.
#[expect(
    clippy::exit,
    reason = "a test keeper's process ends as the real one's does, with the harness's report kept off a launch's streams"
)]
pub fn serve_keeper() -> io::Result<()> {
    let Ok(args) = std::env::var(KEEPER_ARGS) else {
        return Ok(());
    };
    // The harness around this keeper prints its own report on the stream a launch's output is read
    // from. Its script gave the keeper that stream at descriptor 3 and left the harness's on
    // nothing, so from here on what the app writes is passed on where the real keeper's is.
    // SAFETY: `dup2` and `close` take plain descriptors, and descriptor 3 is this script's.
    unsafe {
        if libc::dup2(3, libc::STDOUT_FILENO) != -1 {
            libc::close(3);
        }
    }
    let mut options = tilo_app_keeper::Options::from_args(args.lines().map(Into::into))
        .map_err(io::Error::other)?;
    // A keeper whose app ended waits this long for a session to hear it: a test's next run is
    // there in milliseconds, or never comes.
    options.grace = Duration::from_secs(5);
    options.aged_by = Some(options.dir.join(AGED));
    let ended = tilo_app_keeper::Keeper::new(options).and_then(tilo_app_keeper::Keeper::run);
    // A keeper's process ends with its app, as the real one's does. This binary is a test harness
    // around it, and the report the harness would print after the keeper returns would land on the
    // stream a launch's output is read from — as no real keeper's own words ever do.
    match ended {
        Ok(()) => std::process::exit(0),
        Err(error) => Err(error),
    }
}

/// The file that makes a test keeper one of an older build ([`age`]).
const AGED: &str = "test-aged";

/// Make the test keeper in `dir` one of an older build from now on: it speaks another version
/// than the session's, and holds its app for a session at another version when `holds`, as every
/// keeper since that rule does; one from before closes the connection instead.
///
/// # Errors
/// The keeper's directory could not be written.
pub fn age(dir: &Path, holds: bool) -> io::Result<()> {
    std::fs::write(dir.join(AGED), if holds { "holds" } else { "closes" })
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
            "{KEEPER_ARGS}=\"$(printf '%s\\n' \"$@\")\" exec {} --exact {} --nocapture 3>&1 1>/dev/null",
            quoted(&exe.to_string_lossy()),
            quoted(test),
        ),
    )
}

/// Write at `path` a stand-in for the launcher a kept app starts through: it starts the keeper
/// it is handed, in the directory and as the launch it is handed, with `app` as the app's command
/// line followed by what it was handed after `--` (a Windows program's sandbox), and takes no
/// other argument into account. What it was handed is kept for [`launcher_handed`].
///
/// Like the real launcher, it asks the keeper for an X11 display when the launch is given one:
/// `windows-program` asks with `--x11`, `machine-app` with `--x11-display <name>`. Both spellings
/// are read here, so a test's launch is served exactly as the real launcher would serve it; the
/// session's own command lines are proved against the real launcher in `kept_apps` and
/// `windows_compat`, so the two cannot drift apart unnoticed.
///
/// # Errors
/// The program could not be written.
pub fn launcher_program(path: &Path, app: &[String]) -> io::Result<()> {
    let app: Vec<String> = app.iter().map(|arg| quoted(arg)).collect();
    program(
        path,
        &format!(
            "for a in \"$@\"; do printf '%s\\000' \"$a\"; done > {}\n\
             for a in \"$@\"; do printf '%s\\t' \"$a\"; done >> {}\n\
             printf '\\n' >> {}\n\
             while [ $# -gt 0 ]; do case \"$1\" in --keeper) k=$2; shift;; --keep-in) d=$2; shift;; --keep-as) l=$2; shift;; --x11) x=--x11;; --x11-display) x=--x11; shift;; --) shift; break;; esac; shift; done\n\
             exec \"$k\" --dir \"$d\" --launch \"$l\" $x -- {} \"$@\"",
            quoted(&handed_at(path).to_string_lossy()),
            quoted(&starts_at(path).to_string_lossy()),
            quoted(&starts_at(path).to_string_lossy()),
            app.join(" ")
        ),
    )
}

/// The command line the last start of the stand-in launcher at `path` was handed, an argument
/// each: what the session asks the real launcher to do.
#[must_use]
pub fn launcher_handed(path: &Path) -> Vec<String> {
    std::fs::read_to_string(handed_at(path))
        .unwrap_or_default()
        .split_terminator('\0')
        .map(str::to_owned)
        .collect()
}

/// Every command line the stand-in launcher at `path` was handed, oldest first, an argument
/// each: what the session asked the real launcher to do across its starts, including the ones a
/// later start replaced in [`launcher_handed`].
#[must_use]
pub fn launcher_starts(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(starts_at(path))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.split_terminator('\t').map(str::to_owned).collect())
        .collect()
}

/// Where the stand-in launcher at `path` keeps what it was handed.
fn handed_at(path: &Path) -> PathBuf {
    path.with_extension("handed")
}

/// Where the stand-in launcher at `path` keeps every command line it was handed, one a line.
fn starts_at(path: &Path) -> PathBuf {
    path.with_extension("starts")
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
