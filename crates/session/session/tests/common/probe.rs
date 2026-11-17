//! The surface probe: a client of the session's app wire that reports what it read.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use tilo_session::Session;

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The probe's binary: the probe this package's own tests start.
pub fn probe_binary() -> PathBuf {
    tilo_test_env::sibling_binary("tilo-session", "tilo-surface-probe")
}

/// The session's turns until the probe has reported.
pub fn until_reported(session: &mut Session, report: &Path) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.settle_services();
        session.step(REFRESH).expect("a turn of the session");
        if let Ok(report) = std::fs::read_to_string(report) {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "the probe reported nothing: {:?}",
            session.surfaces().names()
        );
    }
}

/// The reading named on the line that starts with `prefix` .
pub fn reading(report: &str, prefix: &str) -> Value {
    let line = report
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("the probe reported no `{prefix}`: {report}"));
    serde_json::from_str(line).expect("a reading is JSON")
}
