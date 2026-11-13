#![allow(
    clippy::missing_errors_doc,
    reason = "fallible functions return the errno, I/O or provider error of the step that failed"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "state guards are scoped to short in-memory sections and never held across network I/O; the lint cannot see block-scoped guards end"
)]
//! Cloud accounts as mounted folders: a Settings account appears at
//! `~/Remote/<account label>` as a normal folder every app and POSIX tool can
//! use. [`service`] runs one mount per account, [`volume`] is the account's
//! engine and [`fuse`] the kernel adapter.
//!
//! This is separate from the workspace data plane on purpose: workspaces
//! replicate signed records between peers, while a cloud account mirrors a
//! server that is the authority for its own tree.

pub mod credential;
pub mod fuse;
mod import;
pub mod providers;
pub mod service;
pub mod volume;

pub use service::{CloudService, InProcessLauncher, Launcher, ProcessLauncher, ServiceConfig};
pub use volume::{CloudVolume, VolumeConfig};
