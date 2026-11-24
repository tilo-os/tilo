//! A connector's outside worker and its host (bots.md §6).
//!
//! The worker is the connector package's own binary, started by the workspace's
//! host with no writable path and its egress pinned to the hosts its manifest
//! declares. It speaks newline-delimited JSON on stdin/stdout
//! ([`protocol`], [`framing`]). It never sees a device key, a replica or a
//! content store: the host hands it the bytes an op needs, keeps its secrets
//! and its resume checkpoint, and appends what it reports to a host-private
//! event log that the bot dispatcher reads with per-installation cursors.

pub mod event_log;
pub mod framing;
pub mod process;
pub mod protocol;

pub use tilo_live::app_call::{
    AppLane, BridgedCall, CallSetup, CallsReport, ConnectorCall, Lane, Out,
};
pub use tilo_live::app_call::{CallLink, OutsideCall, OutsidePerson};

/// The lane of a worker this device runs, whose asks and frames are handed over as they come.
#[must_use]
pub fn lane_of(process: &std::sync::Arc<crate::ConnectorProcess>) -> tilo_live::app_call::Lane {
    let (say, mut says) = tokio::sync::mpsc::unbounded_channel();
    let (speak, mut speaks) = tokio::sync::mpsc::unbounded_channel();
    let asks = std::sync::Arc::clone(process);
    tokio::spawn(async move {
        while let Some((call, status)) = says.recv().await {
            let _ = asks.set_call(call, status).await;
        }
    });
    let speaks_for = std::sync::Arc::clone(process);
    tokio::spawn(async move {
        while let Some((speaker, opus)) = speaks.recv().await {
            let _ = speaks_for.send_audio(speaker, opus).await;
        }
    });
    tilo_live::app_call::Lane {
        calls: process.calls(),
        audio: process.audio(),
        say,
        speak,
    }
}
pub use event_log::{EventLog, LoggedEvent};
pub use process::{BlobAccess, ConnectorProcess, PerformError, WorkerCommand};
pub use protocol::{FromWorker, Outcome, PROTOCOL_VERSION, ToWorker};
