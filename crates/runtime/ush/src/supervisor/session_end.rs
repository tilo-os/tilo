//! How a served session ended, as the supervisor learns it and tells the
//! app on the session's lease: a relay bridge's status, or what `ush run`
//! said while it set a local guest up.

use std::io::{BufRead, BufReader};
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use tilo_environment::Environment;

use crate::records::Records;

/// How a served session ended: its shell exited; for a relay session, the
/// connection was lost or another terminal took it over; for a session on
/// this device, its guest could not be set up and no shell started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuestEnd {
    Exited,
    /// The connection to the relay was lost; the session may still run
    /// there, and the tab re-attaches to it.
    Lost(String),
    /// Another terminal took the session over; it runs on there.
    Superseded,
    /// The guest was never set up: no shell ran, and the reason (for the
    /// log, not the person) is what `ush run` reported.
    SetupFailed(String),
}

impl GuestEnd {
    /// The end a bridge's last status line names; a bridge that stopped
    /// without saying lost its connection, as far as the tab can tell.
    pub(super) fn of(status: Option<crate::relay::BridgeStatus>) -> Self {
        match status {
            Some(crate::relay::BridgeStatus::Ended { code: Some(_), .. }) => Self::Exited,
            Some(crate::relay::BridgeStatus::Ended {
                superseded: true, ..
            }) => Self::Superseded,
            Some(crate::relay::BridgeStatus::Ended {
                reason: Some(reason),
                ..
            }) => Self::Lost(reason),
            _ => Self::Lost("the connection to the relay ended".into()),
        }
    }
}

/// What an app that asked (`end_report`) reads on its lease just before it
/// closes: one JSON line.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "ended", rename_all = "kebab-case")]
pub(super) enum EndReport<'a> {
    Exited,
    Lost {
        reason: &'a str,
    },
    Superseded,
    /// The guest could not be set up; nothing was written on its terminal.
    Failed {
        reason: &'a str,
    },
}

impl<'a> From<&'a GuestEnd> for EndReport<'a> {
    fn from(end: &'a GuestEnd) -> Self {
        match end {
            GuestEnd::Exited => Self::Exited,
            GuestEnd::Lost(reason) => Self::Lost { reason },
            GuestEnd::Superseded => Self::Superseded,
            GuestEnd::SetupFailed(reason) => Self::Failed { reason },
        }
    }
}

/// A session's end, watched — a relay session's bridge status, or a local
/// guest's setup report — and whether the app asked to be told.
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

/// Read a relay session's status channel to its end: the tools the relay
/// builds for it become the record's, as a `pkg install` on this device
/// would, and its last line says how it ended.
pub(super) fn watch_relay_bridge(
    mut status: crate::relay::StatusReader,
    records_root: PathBuf,
) -> std::thread::JoinHandle<GuestEnd> {
    std::thread::spawn(move || {
        let records = Records::at(records_root);
        let mut ended = None;
        while let Some(line) = status.next(None) {
            match line {
                crate::relay::BridgeStatus::ToolBuilt {
                    record,
                    packages,
                    images,
                } => {
                    for (arch, image) in images {
                        let _ = records.apply(
                            &record,
                            Environment::ADD_TOOL.with((packages.clone(), arch, image)),
                        );
                    }
                }
                line @ crate::relay::BridgeStatus::Ended { .. } => ended = Some(line),
                crate::relay::BridgeStatus::Session { .. } => {}
            }
        }
        GuestEnd::of(ended)
    })
}
