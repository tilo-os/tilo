#![allow(
    clippy::missing_errors_doc,
    reason = "fallible functions return the I/O or record-plane error of the step that failed"
)]
//! The Tier 1 local data plane.
//!
//! On a machine with a dedicated Btrfs store volume and a kernel with
//! pre-content fanotify events, the workspace is a real tree on the store
//! volume, bind-mounted at the workspace path. Reads, writes, `stat` and
//! `mmap` of resident files never reach this daemon. The daemon observes
//! changes afterwards through the change journal, cuts crash-consistent
//! capture epochs and turns each changeset into the same signed record
//! operations the FUSE data plane produces. Remote versions arrive as
//! placeholders (exact metadata, bytes fetched on first access) or as
//! files replaced atomically.
//!
//! Everywhere else the FUSE data plane (Tier 2/3) serves the workspace;
//! [`tier::select`] decides at startup from what the kernel and the store
//! volume actually do.

pub mod fetch;
pub mod lease;
pub mod plane;
pub mod project;
pub mod state;
pub mod tier;

pub use plane::{NativeConfig, NativePlane};
pub use tier::{DataPlane, TierSelection};
