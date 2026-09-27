#![allow(
    clippy::missing_errors_doc,
    reason = "fallible functions return the errno, I/O or provider error of the step that failed"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "state guards are scoped to short in-memory sections and never held across network I/O; the lint cannot see block-scoped guards end"
)]
//! Cloud accounts as mounted folders (`docs/architecture/storage.md`,
//! §6.10 "Cloud accounts").
//!
//! A `WebDAV`, Nextcloud (and, through the same [`cloud_source::CloudSource`]
//! interface, Google Drive, `OneDrive`, Dropbox or `iCloud` Drive) account set
//! up in Settings appears at `~/Remote/<account label>` as a normal folder
//! every app and POSIX tool can use. The pieces:
//!
//! - [`service`]: the per-user service that follows the account store,
//!   reads each account's short-lived credential from the handoff store
//!   and runs one mount per account;
//! - [`volume`]: one account's engine — cached metadata, ranged hydration,
//!   write-back through a persisted queue, conflict copies;
//! - [`fuse`]: the kernel adapter;
//! - [`credential`]: the one-credential pipe between service and mount;
//! - [`providers`]: which provider serves which account type.
//!
//! This module is separate from the room data plane on purpose: rooms
//! replicate signed records between peers, while a cloud account mirrors a
//! server that is the authority for its own tree. What they share is the
//! conflict-copy convention ([`crate::conflict::conflict_name`]) and the
//! content store: cached bytes live in the daemon's file-backed,
//! range-resident, budgeted cache ([`crate::store::FileArea`]), keyed by
//! remote revision, with identical revisions sharing extents.

pub mod credential;
pub mod fuse;
pub mod providers;
pub mod service;
pub mod volume;

pub use service::{CloudService, InProcessLauncher, Launcher, ProcessLauncher, ServiceConfig};
pub use volume::{CloudVolume, Health, VolumeConfig};
