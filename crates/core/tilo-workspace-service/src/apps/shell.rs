//! The person's shell as the daemon's dispatch serves it (`runtime_collab::wire::shell`).
//!
//! A connection that opens with [`runtime_collab::wire::shell::SHELL_MAGIC`] and the secret of a
//! link the daemon made ([`ShellLinks`]) is the shell that link was made for; a connection with
//! no such secret is refused before its first call, whatever account it runs as. An admitted
//! connection is answered from [`ShellServices`]: the device's own documents, read for the device
//! and written as it (the daemon signs and seals them with the device's key), the workspaces' key
//! access and membership and the host's own operations as typed actions, and the workspaces'
//! standing as published resources ([`tilo_peer_state::workspace_view`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use runtime_collab::wire::shell::Vouch;
use tilo_actions::remote::InvokeRequest;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DeviceWrites};

use super::services::PublishedResources;

/// The app id the shell's published-resource reads are made as.
pub const SHELL_APP: &str = "so.tilo.shell";

/// Who an admitted shell connection acts as: the person the daemon made its link for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellCaller {
    /// The person's root key.
    pub person: [u8; 32],
}

/// The shell links the daemon made and no shell took yet: each a one-use secret the daemon
/// sends on a connection it opens itself, then hands to the shell it starts. And the connections
/// an admitted shell vouched for, one for each app it launched, that did not open yet.
#[derive(Default)]
pub struct ShellLinks {
    shells: std::sync::Mutex<std::collections::HashMap<[u8; 32], ShellCaller>>,
    vouched: std::sync::Mutex<std::collections::HashMap<[u8; 32], Vouch>>,
}

impl ShellLinks {
    /// A new link's secret, admitting one connection as `caller`'s shell.
    #[must_use]
    pub fn make(&self, caller: ShellCaller) -> [u8; 32] {
        let secret: [u8; 32] = rand::random();
        lock(&self.shells).insert(secret, caller);
        secret
    }

    /// Who the link with `secret` was made for, once: a secret is taken by its first connection.
    #[must_use]
    pub fn take(&self, secret: &[u8; 32]) -> Option<ShellCaller> {
        lock(&self.shells).remove(secret)
    }

    /// A secret admitting one connection as the app `vouch` names: what an admitted shell is
    /// answered when it vouches for an app it launched.
    #[must_use]
    pub fn vouch(&self, vouch: Vouch) -> [u8; 32] {
        let secret: [u8; 32] = rand::random();
        lock(&self.vouched).insert(secret, vouch);
        secret
    }

    /// The app the vouch with `secret` named, once.
    #[must_use]
    pub fn take_vouched(&self, secret: &[u8; 32]) -> Option<Vouch> {
        lock(&self.vouched).remove(secret)
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An action a shell invoked, once it settled: the value it answered, or why it failed.
pub type ShellInvoked = Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>;

/// The actions a shell invokes as its person on this device.
pub trait ShellActions: Send + Sync {
    /// Invoke `request` as `caller`: a workspace's key access and membership action
    /// (`RemoteActionTarget::workspace_governance`), answered by the workspace the device keeps
    /// or forwarded to its keeping device over tilo-actions/1, or one of the host's own
    /// operations, answered by the device's workspace service.
    fn invoke(&self, caller: &ShellCaller, request: InvokeRequest) -> ShellInvoked;
}

/// The writes a device takes to its workspaces' documents, as they come.
pub type DocumentWrites = tokio::sync::broadcast::Receiver<crate::workspaces::ResourceWrite>;

/// What the daemon's dispatch answers a person's shell with.
#[derive(Clone)]
pub struct ShellServices {
    /// The links of the shells the daemon starts: a connection is a shell's only with one.
    pub links: Arc<ShellLinks>,
    /// The socket this dispatch answers on: where a connection a shell vouched for opens.
    pub socket: std::path::PathBuf,
    /// The device's documents, read for the device itself.
    pub documents: Arc<dyn DeviceDocuments>,
    /// The device's documents, written as the device: signed and sealed with its key.
    pub writes: Arc<dyn DeviceWrites>,
    /// Each write the device takes to a document of its workspaces
    /// ([`crate::workspaces::Workspaces::document_writes`]): what a shell reads again by.
    pub document_writes: Arc<dyn Fn() -> DocumentWrites + Send + Sync>,
    /// The workspaces' key access and membership, and the host's own operations.
    pub actions: Arc<dyn ShellActions>,
    /// The workspaces' standing ([`tilo_peer_state::workspace_view`]), watched as published resources.
    pub standing: Arc<dyn PublishedResources>,
    /// The workspaces the device holds: what the resource commands of the apps the shell runs
    /// itself, and its asks of a workspace's vault, run on.
    pub workspaces: crate::workspaces::Workspaces,
}
