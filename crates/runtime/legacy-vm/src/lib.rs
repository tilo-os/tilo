//! Legacy VMs: software that needs a whole system, in one VM per user and
//! guest OS.
//!
//! The Linux VM is an unprivileged libvirt domain in the user's own
//! `qemu:///session`, created from a signed guest image ([`image`]). The
//! first launch wakes it and an idle VM is saved to disk ([`lifecycle`]); the
//! [`supervisor`] carries that out, grows the balloon under pressure
//! ([`memory`]), shares the user's granted directories at the same paths
//! ([`grants`], [`virtiofs`]) and forwards windows to the compositor
//! (`waypipe` over vsock, see [`legacy_agent_protocol::waypipe`]). Installed
//! apps get search entries and environment shims ([`apps`]).
//!
//! Windows is not a legacy VM: the Windows appliance in `windows-vm` is a
//! separate VM type (one owner-managed system domain, `RemoteApp` windows and
//! an HTTPS guest bridge). The two share only [`libvirt_domain`].

#![allow(
    clippy::missing_errors_doc,
    reason = "errors are typed enums whose variants document themselves"
)]

pub use legacy_agent_protocol::GuestOs;

pub mod apps;
pub mod config;
pub mod confine;
pub mod control;
pub mod domain;
pub mod grants;
pub mod image;
pub mod lifecycle;
pub mod memory;
#[cfg(feature = "relay-stream")]
pub mod stream;
pub mod supervisor;
pub mod transport;
pub mod virtiofs;
