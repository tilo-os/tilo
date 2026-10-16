//! A connector's outside worker and its host (bots.md §6).
//!
//! The worker is the connector package's own binary, started by the room's
//! host with no writable path and its egress pinned to the hosts its
//! manifest declares. It speaks newline-delimited JSON on stdin/stdout
//! ([`protocol`], [`framing`]). It never sees a device key, a replica or a
//! content store: the host hands it the bytes an op needs, keeps its secrets
//! and its resume checkpoint, and appends what it reports to a host-private
//! event log that the bot dispatcher reads with per-installation cursors.

pub mod event_log;
pub mod framing;
pub mod process;
pub mod protocol;

pub use event_log::{EventLog, LoggedEvent};
pub use process::{BlobAccess, ConnectorProcess, PerformError, WorkerCommand};
pub use protocol::{FromWorker, Outcome, PROTOCOL_VERSION, ToWorker};
