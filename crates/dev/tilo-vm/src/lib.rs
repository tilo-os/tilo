//! `tilo-vm`: the host CLI for Tilo dev VMs. It chooses a VM, talks to its
//! `tilo-vmd` over SSH, builds and uploads component layers, and drives the
//! host's hypervisor through thin adapters.

pub mod app;
pub mod build;
pub mod ci;
pub mod cli;
pub mod config;
pub mod context;
pub mod disk;
pub mod doctor;
pub mod error;
pub mod guide;
pub mod host;
pub mod hypervisor;
pub mod receipt;
pub mod remote;
pub mod sshkey;
pub mod vmbuild;
