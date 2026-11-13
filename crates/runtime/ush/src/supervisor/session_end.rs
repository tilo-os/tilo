//! How a served session ended, as the supervisor learns it and tells the
//! app on the session's lease: what `ush run` said while it set a guest
//! up.

use std::io::{BufRead, BufReader};
use std::os::fd::OwnedFd;

/// How a served session ended: its shell exited, or its guest could not be
/// set up and no shell started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuestEnd {
    Exited,
    /// The guest was never set up: no shell ran, and the reason (for the
    /// log, not the person) is what `ush run` reported.
    SetupFailed(String),
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "ended", rename_all = "kebab-case")]
pub(super) enum EndReport<'a> {
    Exited,
    /// The guest could not be set up; nothing was written on its terminal.
    Failed {
        reason: &'a str,
    },
}

impl<'a> From<&'a GuestEnd> for EndReport<'a> {
    fn from(end: &'a GuestEnd) -> Self {
        match end {
            GuestEnd::Exited => Self::Exited,
            GuestEnd::SetupFailed(reason) => Self::Failed { reason },
        }
    }
}

pub(crate) struct EndWatch {
    pub(super) ended: std::thread::JoinHandle<GuestEnd>,
    pub(super) report: bool,
}

/// Read what `ush run` writes on its own stderr while it sets a guest up,
/// to the end: every line is the supervisor's to log, never the person's
/// terminal's, and a [`crate::setup::SetupReport`] says the guest was not
/// set up. The pipe ends when `ush run` exits, and with it every process it
/// started on the way (the guest's own stderr is its terminal).
pub(super) fn watch_setup(
    diagnostics: OwnedFd,
    session: String,
) -> std::thread::JoinHandle<GuestEnd> {
    std::thread::spawn(move || {
        let mut end = GuestEnd::Exited;
        for line in BufReader::new(std::fs::File::from(diagnostics)).lines() {
            let Ok(line) = line else { break };
            match crate::setup::SetupReport::parse(&line) {
                Some(crate::setup::SetupReport::Failed { reason }) => {
                    tracing::warn!(%session, %reason, "ush: the session's guest could not be set up");
                    end = GuestEnd::SetupFailed(reason);
                }
                None if line.trim().is_empty() => {}
                None => tracing::warn!(%session, "ush run: {line}"),
            }
        }
        end
    })
}
