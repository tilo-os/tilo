//! `tilo-vm`: the host CLI for Tilo dev VMs. It chooses a VM, talks to its
//! `tilo-vmd` over SSH, builds and uploads component layers, and drives the
//! host's hypervisor through thin adapters.

pub mod app;
pub mod appsets;
mod artifact;
pub mod build;
pub mod builderapp;
pub mod builderlayer;
pub mod ci;
pub mod cicache;
pub mod cli;
pub mod config;
pub mod context;
pub mod device_actions;
pub mod devtrust;
pub mod disk;
pub mod doctor;
pub mod error;
mod forwarded_port;
pub mod guide;
pub mod host;
pub mod hypervisor;
pub mod image;
pub mod imageup;
mod local_layer;
pub mod reach;
pub mod receipt;
pub mod remote;
pub mod scenario;
mod source_inputs;
pub mod sshguard;
pub mod sshkey;
pub mod vmbuild;
