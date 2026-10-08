//! `tilo-vmd`: the dev image's owner of layers, slots, generations and
//! slot sessions. Reached only through `tilo-vmd rpc` on stdio, which
//! `tilo-vm` runs over SSH.

pub mod apply;
pub mod awake;
pub mod boot;
mod cohort;
pub mod daemon;
mod device_actions;
mod disk_unlock;
pub mod display;
pub mod error;
pub mod exec;
mod fsutil;
pub mod input;
pub mod jobs;
pub mod layers;
mod linker;
pub mod locks;
pub mod packages;
pub mod server;
mod session_view;
pub mod slots;
pub mod systemd;
pub mod targets;

pub use daemon::Daemon;
pub use error::DaemonError;
