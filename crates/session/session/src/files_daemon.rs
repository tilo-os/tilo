//! The files daemon as the Files app's windowless endpoint.
//!
//! The daemon that mounts the person's own workspace publishes that tree on
//! the record route as the Files app ([`fs::tree_record`]). The login starts
//! it beside the session, so this session records its process in the launch
//! record as it starts and lets the daemon's own connections be attested
//! against it — the same launch record an app the session starts gets, with
//! no window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Launches;

/// The login unit that serves the person's own workspace.
const UNIT: &str = "tilo-files-workspace.service";
/// The app whose endpoint the daemon is: the one that keeps the tree record.
const FILES_APP: &str = "so.tilo.file-explorer";
/// The process to record instead of the unit's, for a run where something other than the login's
/// unit serves the trees: a test that mounts them itself names its own process here.
const ENDPOINT_PID: &str = "TILO_FILES_ENDPOINT_PID";
/// How long the session waits for the unit to run its process.
const WAIT: Duration = Duration::from_secs(30);
/// How often it looks while waiting.
const STEP: Duration = Duration::from_millis(200);

/// Record the files daemon's launch on `cluster`, the person's own workspace,
/// when the login runs it. A session in another desktop's window serves no
/// files daemon of its own, so it records none.
pub fn record(launches: Arc<Launches>, cluster: Option<String>) {
    let Some(cluster) = cluster else {
        // No identity yet: the daemon's connections reach nothing, as before.
        return;
    };
    let recorded = std::thread::Builder::new()
        .name("tilo-files-endpoint".into())
        .spawn(move || {
            let Some(pid) = named_endpoint().or_else(wait_for_process) else {
                tracing::warn!(
                    unit = UNIT,
                    "the files daemon runs no process to record: its tree reaches no device"
                );
                return;
            };
            launches.windowless(pid, FILES_APP, &cluster);
        });
    if let Err(error) = recorded {
        tracing::warn!(%error, "the files daemon's launch cannot be recorded");
    }
}

/// The process named as the endpoint, when this session runs where the trees are served by
/// something other than the login's unit.
fn named_endpoint() -> Option<u32> {
    let named = std::env::var(ENDPOINT_PID).ok()?;
    match named.trim().parse::<u32>() {
        Ok(pid) if pid > 0 => Some(pid),
        Ok(_) => {
            tracing::warn!(%named, "the named files endpoint is no process");
            None
        }
        Err(error) => {
            tracing::warn!(%named, %error, "the named files endpoint is not a pid");
            None
        }
    }
}

/// The unit's main process, once the login is running it.
fn wait_for_process() -> Option<u32> {
    let started = Instant::now();
    loop {
        if let Some(pid) = main_pid() {
            return Some(pid);
        }
        if started.elapsed() >= WAIT {
            return None;
        }
        std::thread::sleep(STEP);
    }
}

/// The unit's main process, as the user manager reports it.
fn main_pid() -> Option<u32> {
    let asked = std::process::Command::new("systemctl")
        .args(["show", "--property=MainPID", "--value", UNIT])
        .output();
    let output = match asked {
        Ok(output) => output,
        Err(error) => {
            // ignored-failure: the user manager may still be starting the unit; the wait loop
            // looks again, and warns when it gives up.
            tracing::debug!(%error, "the user manager cannot say which process a unit runs");
            return None;
        }
    };
    let said = String::from_utf8_lossy(&output.stdout);
    match said.trim().parse::<u32>() {
        Ok(pid) => Some(pid).filter(|pid| *pid > 0),
        Err(error) => {
            // ignored-failure: `0` is a unit that runs nothing yet; the wait loop looks again.
            tracing::debug!(%error, %said, "the unit runs no process");
            None
        }
    }
}
