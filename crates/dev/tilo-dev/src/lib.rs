//! `tilo-dev`: the host CLI for Tilo dev VMs. It chooses a VM, talks to its
//! `tilo-devd` over SSH, builds and uploads component layers, and drives the
//! host's hypervisor through thin adapters.

pub mod app;
pub mod build;
pub mod cli;
pub mod config;
pub mod context;
pub mod error;
pub mod guide;
pub mod hypervisor;
pub mod receipt;
pub mod remote;
