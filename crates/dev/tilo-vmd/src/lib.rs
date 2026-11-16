//! `tilo-vmd`: the dev image's owner of layers, accounts, generations and
//! account sessions. Reached only through `tilo-vmd rpc` on stdio, which
//! `tilo-vm` runs over SSH.

#[cfg(test)]
mod account_community_tests;
pub mod accounts;
pub mod apply;
pub mod awake;
pub mod boot;
mod build_user;
mod cohort;
pub mod daemon;
mod device_actions;
mod disk_unlock;
pub mod display;
pub mod error;
pub mod exec;
mod fsutil;
pub mod image;
pub mod input;
pub mod interfaces;
pub mod jobs;
pub mod layers;
mod linker;
pub mod locks;
pub mod packages;
pub mod server;
mod session_view;
pub mod systemd;
pub mod targets;

pub use cohort::SelectedApps;
pub use daemon::Daemon;
pub use error::DaemonError;
