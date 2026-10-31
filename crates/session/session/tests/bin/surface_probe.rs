//! A surface app that reaches its session: it reads what the session
//! publishes, changes it, and reports both in a file.
//!
//! The tests start it the way the session starts a panel of the desktop, so
//! what it proves is what a panel does — an ordinary app, started by the
//! session as its launch, reading and invoking through the connection the
//! session's broker admits it for. The test names what it reads, what it
//! changes and where it reports on this process's environment, beside what
//! the session writes there.

#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "a test fixture: a step it cannot take is the failure the test reads"
)]

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use ui::collaboration::SessionResource;

/// Where the app reports what it read and changed.
const REPORT: &str = "SURFACE_PROBE_REPORT";
/// The resource it reads.
const RESOURCE: &str = "SURFACE_PROBE_RESOURCE";
/// The action it takes on it, with its arguments as JSON.
const ACTION: &str = "SURFACE_PROBE_ACTION";
const ARGUMENTS: &str = "SURFACE_PROBE_ARGUMENTS";

/// How long the change may take to land: the session publishes the reading
/// again when the device has taken it, and a device that never does is the
/// failure the test reads.
const CHANGING: Duration = Duration::from_secs(10);

/// How long the reading must stand still before it is the one the action
/// landed at: a reading that has only begun to move is drawn as it is.
const SETTLING: Duration = Duration::from_millis(250);

fn main() {
    let report = std::env::var(REPORT).expect("the file to report in");
    // What every surface app does before its window opens. From here on the
    // session's published resources are this app's, as they are any app's.
    ui::surface_services::connect();
    // Written whole and then put in place, so the test reads a report or
    // nothing rather than the middle of one.
    let whole = format!("{report}.whole");
    std::fs::write(&whole, read_and_change()).expect("the report is written");
    std::fs::rename(&whole, &report).expect("the report is put in place");
}

/// The reading the resource stands at, then the reading the action lands at,
/// one per line; what went wrong instead.
fn read_and_change() -> String {
    let resource = std::env::var(RESOURCE).expect("the resource to read");
    let opened = match SessionResource::open(&resource) {
        Ok(opened) => opened,
        Err(error) => return format!("error: {error}\n"),
    };
    let Some(Ok(first)) = opened.latest() else {
        return String::from("error: the session answered no reading\n");
    };
    let mut report = format!("read: {first}\n");
    let Ok(action) = std::env::var(ACTION) else {
        return report;
    };
    let arguments = std::env::var(ARGUMENTS)
        .ok()
        .map(|arguments| serde_json::from_str(&arguments).expect("the action's arguments"))
        .unwrap_or_default();
    if let Err(error) = opened.invoke(&action, &arguments) {
        return format!("{report}error: {error}\n");
    }
    report.push_str("taken\n");
    // The action is taken once the session has taken it; what the resource
    // reads afterwards is the change itself, and it arrives on the watch, in
    // as many readings as the device takes to land it. The reading the panel
    // draws is the last one: every one it saw is reported, and the one it
    // ends at is the change.
    let deadline = Instant::now() + CHANGING;
    let mut drawn = first.clone();
    let mut still_since = Instant::now();
    loop {
        match opened.latest() {
            Some(Ok(reading)) if reading != drawn => {
                let _ = writeln!(report, "seen: {reading}");
                drawn = reading;
                still_since = Instant::now();
            }
            Some(Ok(_unchanged)) => {}
            Some(Err(error)) => return format!("{report}error: {error}\n"),
            None => {}
        }
        if drawn != first && still_since.elapsed() >= SETTLING {
            let _ = writeln!(report, "changed: {drawn}");
            return report;
        }
        if Instant::now() >= deadline {
            return format!("{report}error: the change never showed\n");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
