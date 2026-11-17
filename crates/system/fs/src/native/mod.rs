#![allow(
    clippy::missing_errors_doc,
    reason = "fallible functions return the I/O or record-backend error of the step that failed"
)]
//! The Tier 1 local backend.
//!
//! On a machine with a dedicated Btrfs store volume and a kernel with
//! pre-content fanotify events, the workspace is a real tree on the store
//! volume, bind-mounted at the workspace path. Reads, writes, `stat` and
//! `mmap` of resident files never reach this daemon. The daemon observes
//! changes afterwards through the change journal, cuts crash-consistent
//! capture epochs and turns each changeset into the same signed record
//! operations the FUSE backend produces. Remote versions arrive as
//! placeholders (exact metadata, bytes fetched on first access) or as
//! files replaced atomically.
//!
//! Everywhere else the FUSE backend (Tier 2/3) serves the workspace;
//! [`tier::select`] decides at startup from what the kernel and the store
//! volume actually do.

pub mod backend;
pub mod fetch;
pub mod lease;
pub mod project;
pub mod state;
pub mod tier;

pub use backend::{NativeBackend, NativeConfig};
pub use tier::{Backend, TierSelection};
