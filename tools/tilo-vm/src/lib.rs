//! `tilo-vm`: the host CLI for Tilo dev VMs. It chooses a VM, asks its device's
//! daemon over action frames as this machine's device, builds and sends component
//! layers, and drives the host's hypervisor through thin adapters.

// The in-process device the tests ask keeps its workspace in automerge documents, which run in
// worker processes this test executable serves.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

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
pub mod disk;
pub mod doctor;
pub mod error;
mod forwarded_port;
pub mod guide;
pub mod host;
pub mod hypervisor;
pub mod image;
pub mod imageup;
pub mod lan;
mod local_layer;
pub mod panel;
pub mod qemu;
pub mod reach;
pub mod receipt;
pub mod remote;
pub mod scenario;
pub mod sound;
mod source_inputs;
pub mod sshguard;
pub mod sshkey;
pub mod vmbuild;
