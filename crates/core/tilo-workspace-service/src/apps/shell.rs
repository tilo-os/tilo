//! The person's shell as the daemon's dispatch serves it (`runtime_collab::wire::shell`).
//!
//! A connection that opens with [`runtime_collab::wire::shell::SHELL_MAGIC`] and the secret of a
//! link the daemon made ([`ShellLinks`]) is the shell that link was made for; a connection with
//! no such secret is refused before its first call, whatever account it runs as. An admitted
//! connection is answered from [`ShellServices`]: the device's own documents, read for the device
//! and written as it (the daemon signs and seals them with the device's key), every action through
//! the device's one dispatch as its person ([`ShellActions`]), and the workspaces' standing as
//! published resources ([`tilo_peer_state::workspace_view`]). The providers the shell serves are
//! registered with that dispatch for as long as its link lasts.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tilo_actions::remote::{ActionProvider, Caller};
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
/// sends on a connection it opens itself, then hands to the shell it starts. And the
/// connections it asked to hand the device's tilolang processes an instance on.
#[derive(Default)]
pub struct ShellLinks {
    shells: std::sync::Mutex<std::collections::HashMap<[u8; 32], ShellCaller>>,
    processes: std::sync::Mutex<std::collections::HashSet<[u8; 32]>>,
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

    /// A secret admitting one connection to the device's tilolang processes: what an admitted
    /// shell is answered when it asks to hand them an instance.
    #[must_use]
    pub fn processes(&self) -> [u8; 32] {
        let secret: [u8; 32] = rand::random();
        lock(&self.processes).insert(secret);
        secret
    }

    /// Whether `secret` admits a connection to the processes, once.
    #[must_use]
    pub fn take_processes(&self, secret: &[u8; 32]) -> bool {
        lock(&self.processes).remove(secret)
    }
}

/// The device's tilolang processes, as the shell reaches them: each takes one connection, a
/// packet socket the shell's admitted connection carried, and answers the one ask on it
/// (`runtime_portal::device_processes`).
pub type ShellProcesses = Arc<dyn Fn(runtime_portal::packet::Descriptor) + Send + Sync>;

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The dispatch a shell's calls reach, and the caller they are made as there.
pub type ShellDispatch = Pin<
    Box<
        dyn Future<Output = Result<(Arc<dyn ActionProvider + Send + Sync>, Caller), String>> + Send,
    >,
>;

/// The device's one dispatch, as a shell's person reaches it.
pub trait ShellActions: Send + Sync {
    /// The dispatch `caller`'s calls in `workspace` reach, as their person on this device, and
    /// the caller they are made as: what answers every frame a shell sends
    /// ([`runtime_collab::wire::shell::ShellCall::Action`]). In no workspace, the person's
    /// workspace list, a workspace's key access and membership
    /// (`RemoteActionTarget::workspace_governance`) and the host's own operations.
    ///
    /// `pressed` is where on the shell's screen the person pressed the call, when they did: the
    /// daemon's dispatch keeps it as the question's `pressed`, filling the device and the app
    /// itself. It is the shell connection's word alone: a call without one is a call nobody
    /// pressed, and no other connection may name a window.
    fn dispatch(
        &self,
        caller: &ShellCaller,
        workspace: Option<String>,
        pressed: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> ShellDispatch;
}

/// The writes a device takes to its workspaces' documents, as they come.
pub type DocumentWrites = tokio::sync::broadcast::Receiver<crate::workspaces::ResourceWrite>;

/// What the daemon's dispatch answers a person's shell with.
#[derive(Clone)]
pub struct ShellServices {
    /// The links of the shells the daemon starts: a connection is a shell's only with one.
    pub links: Arc<ShellLinks>,
    /// The socket this dispatch answers on: where a connection to the device's tilolang processes
    /// opens.
    pub socket: std::path::PathBuf,
    /// The device's documents, read for the device itself.
    pub documents: Arc<dyn DeviceDocuments>,
    /// The device's documents, written as the device: signed and sealed with its key.
    pub writes: Arc<dyn DeviceWrites>,
    /// Each write the device takes to a document of its workspaces
    /// ([`crate::workspaces::Workspaces::document_writes`]): what a shell reads again by.
    pub document_writes: Arc<dyn Fn() -> DocumentWrites + Send + Sync>,
    /// The device's one dispatch, as the shell's person reaches it.
    pub actions: Arc<dyn ShellActions>,
    /// The workspaces' standing ([`tilo_peer_state::workspace_view`]), watched as published resources.
    pub standing: Arc<dyn PublishedResources>,
    /// The resources this device's services publish to the apps attached to it (its server
    /// settings): the shell link reads and changes them as the app it names, so a session's apps
    /// reach the one copy the daemon keeps.
    pub published: Option<Arc<dyn PublishedResources>>,
    /// The device's tilolang processes, which the shell hands its windows' instances: `None` where
    /// none can start.
    pub processes: Option<ShellProcesses>,
    /// The workspaces the device holds: what the resource commands of the apps the shell runs
    /// itself, and its asks of a workspace's vault, run on.
    pub workspaces: crate::workspaces::Workspaces,
    /// The device's identity service, serving a connection the shell took for it from one of the
    /// person's apps: `None` where the device runs none.
    pub identity: Option<std::sync::Arc<dyn Fn(tilo_socket::Stream) + Send + Sync>>,
    /// What carries a link of the shell's live session (`tilo_live::link`), handed over as a
    /// connection the shell made for it ([`runtime_collab::wire::shell::HandedTo::Live`]): the
    /// device's live session, which runs its relay. `None` where the device runs none, and a
    /// link is refused.
    pub live: Option<LiveLinks>,
}

/// What carries one admitted link of a shell's live session until it ends.
pub type LiveLinks = Arc<
    dyn Fn(tilo_socket::tokio::Stream) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;
