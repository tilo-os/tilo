//! The device's root steps for `tilo-vm`: layers, accounts, generations and account sessions on
//! a dev image or a person's device. Each step is one action on the device's
//! `devices/<device>/vm` resource; the device's daemon admits the caller and runs the step as
//! its own oneshot unit ([`step`]).

#[cfg(test)]
mod account_community_tests;
pub mod accounts;
pub mod apply;
pub mod awake;
pub mod boot;
mod build_user;
pub mod catalogue;
mod cohort;
pub mod content;
pub mod daemon;
mod disk_unlock;
pub mod display;
pub mod error;
pub mod exec;
mod fsutil;
pub mod image;
pub mod image_upgrade;
pub mod incoming;
pub mod input;
pub mod interfaces;
pub mod jobs;
pub mod layers;
mod linker;
pub mod locks;
pub mod packages;
pub mod power;
mod session_view;
pub mod step;
pub mod systemd;
pub mod targets;
pub mod tree_sync;

pub use cohort::SelectedApps;
pub use daemon::Daemon;
pub use error::DaemonError;
pub use step::{run_step, run_step_on};
