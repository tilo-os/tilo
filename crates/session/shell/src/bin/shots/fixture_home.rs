//! Shots run against a fixture home, never the host's.
//!
//! Every app a scenario opens reads the person's home the way it would on a
//! real machine: Files lists it, its sidebar names its folders, pickers
//! start in it. Rendering the build host's own home into a shot leaks its
//! files and makes the shot depend on whoever ran it. So `shots` runs
//! itself in a home it makes: the same few folders and files, with the same
//! timestamps, as the same person in the same time zone, every time.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Set in the environment of the run that already has its fixture home.
const MARKER: &str = "TILO_SHOTS_FIXTURE_HOME";

/// When every fixture entry was last changed: a fixed moment, so the
/// Modified column reads the same in every shot.
fn fixture_time() -> SystemTime {
    // 2026-03-02 09:30 UTC.
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(1_772_443_800))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Folders a home has, and the files in them.
const ENTRIES: &[(&str, &[(&str, &str)])] = &[
    ("Desktop", &[]),
    (
        "Documents",
        &[
            (
                "Launch plan.md",
                "# Launch plan\n\nShip the remark surface.\n",
            ),
            ("Invoice March.txt", "Invoice for March\n"),
        ],
    ),
    ("Downloads", &[("release-notes.txt", "What changed.\n")]),
    ("Music", &[]),
    ("Pictures", &[]),
    ("Videos", &[]),
];

/// The fixture home of this run, when it has one.
pub fn path() -> Option<PathBuf> {
    std::env::var_os(MARKER).map(PathBuf::from)
}

/// Run this process again inside a fresh fixture home, unless it already
/// is in one. Returns the child's exit code when it re-ran.
///
/// # Errors
///
/// When the fixture home cannot be made or the run cannot be started.
pub fn reenter() -> Result<Option<i32>, String> {
    if path().is_some() {
        return Ok(None);
    }
    let root = tempfile::Builder::new()
        .prefix("tilo-shots-home-")
        .tempdir()
        .map_err(|error| format!("shots: no fixture home: {error}"))?;
    let home = root.path().join("home");
    populate(&home).map_err(|error| format!("shots: fixture home: {error}"))?;
    let exe = std::env::current_exe().map_err(|error| format!("shots: {error}"))?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let status = std::process::Command::new(exe)
        .args(args)
        .env(MARKER, &home)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        // Who is at the machine and what time it is there, the same for
        // every run.
        .env("USER", "tilo")
        .env("LOGNAME", "tilo")
        .env("TZ", "UTC")
        .status()
        .map_err(|error| format!("shots: could not run in the fixture home: {error}"))?;
    Ok(Some(status.code().unwrap_or(1)))
}

fn populate(home: &Path) -> std::io::Result<()> {
    for dir in [".config", ".local/share", ".local/state"] {
        std::fs::create_dir_all(home.join(dir))?;
    }
    for (folder, files) in ENTRIES {
        let folder = home.join(folder);
        std::fs::create_dir_all(&folder)?;
        for (name, contents) in *files {
            let path = folder.join(name);
            std::fs::write(&path, contents)?;
            stamp(&path)?;
        }
        stamp(&folder)?;
    }
    std::fs::write(home.join("Welcome.txt"), "Welcome to Tilo.\n")?;
    stamp(&home.join("Welcome.txt"))
}

fn stamp(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.set_modified(fixture_time())
}
