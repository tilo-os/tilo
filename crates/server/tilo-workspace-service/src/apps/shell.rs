//! The person's shell as the device's broker serves it (`runtime_collab::wire::shell`).
//!
//! A connection that opens with [`runtime_collab::wire::shell::SHELL_MAGIC`] asks to be the
//! shell of the account the kernel names at its other end. [`ShellAccounts`] decides it: the
//! daemon admits an account signed in as one of its people, and refuses any other, and any
//! process of that account the launcher started as an app — an app runs under its person's
//! account too, and is never the shell. An admitted connection is answered from
//! [`ShellServices`]: the device's own documents, read for the device and written as it (the
//! daemon signs and seals them with the device's key), the workspaces' governance and the host's
//! own operations as typed actions, and the workspaces' standing as published resources
//! ([`tilo_peer_state::workspace_view`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::InvokeRequest;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DeviceWrites};

use super::services::PublishedResources;

/// The app id the shell's published-resource reads are made as.
pub const SHELL_APP: &str = "so.tilo.shell";

/// The process at the other end of a shell connection, as the kernel names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellPeer {
    pub uid: u32,
    pub pid: u32,
}

/// Who an admitted shell connection acts as: the person signed in on its account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellCaller {
    /// The person's root key.
    pub person: [u8; 32],
    /// The account the shell runs as.
    pub uid: u32,
}

/// Which accounts are a person's shell on this device.
pub trait ShellAccounts: Send + Sync {
    /// The person `peer` is the shell of, or why it is not one: its account is not signed in as a
    /// person, or the process is an app the launcher started.
    ///
    /// # Errors
    /// The refusal, as the shell's log says it.
    fn admit(&self, peer: ShellPeer) -> Result<ShellCaller, String>;
}

/// An action a shell invoked, once it settled.
pub type ShellInvoked = Pin<Box<dyn Future<Output = Result<InvocationUpdate, String>> + Send>>;

/// The actions a shell invokes as its person on this device.
pub trait ShellActions: Send + Sync {
    /// Invoke `request` as `caller`: a workspace's governance action
    /// (`RemoteActionTarget::workspace_governance`), answered by the workspace the device keeps
    /// or forwarded to its keeping device over tilo-actions/1, or one of the host's own
    /// operations, answered by the device's workspace service.
    fn invoke(&self, caller: &ShellCaller, request: InvokeRequest) -> ShellInvoked;
}

/// What a broker answers a person's shell with.
#[derive(Clone)]
pub struct ShellServices {
    /// Who a shell connection is.
    pub accounts: Arc<dyn ShellAccounts>,
    /// The device's documents, read for the device itself.
    pub documents: Arc<dyn DeviceDocuments>,
    /// The device's documents, written as the device: signed and sealed with its key.
    pub writes: Arc<dyn DeviceWrites>,
    /// The workspaces' governance and the host's own operations.
    pub actions: Arc<dyn ShellActions>,
    /// The workspaces' standing ([`tilo_peer_state::workspace_view`]), watched as published resources.
    pub standing: Arc<dyn PublishedResources>,
}
