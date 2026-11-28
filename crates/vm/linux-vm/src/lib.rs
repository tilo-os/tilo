//! Linux VMs: software that needs a whole system, in one VM per user and guest OS.
//! The Linux VM is an unprivileged libvirt domain in the user's `qemu:///session`;
//! hosted workspaces run their Linux programs in a Firecracker VM with the same
//! agent and lifecycle ([`hosted`]).

#![allow(
    clippy::missing_errors_doc,
    reason = "errors are typed enums whose variants document themselves"
)]

pub use linux_vm_protocol::GuestOs;

pub mod apps;
pub mod config;
#[cfg(target_os = "linux")]
pub mod confine;
pub mod control;
pub mod domain;
pub mod grants;
#[cfg(target_os = "linux")]
pub mod hosted;
pub mod image;
pub mod lifecycle;
pub mod memory;
#[cfg(target_os = "linux")]
mod peer;
#[cfg(feature = "supervisor")]
pub mod supervisor;
pub mod transport;
pub mod virtiofs;
