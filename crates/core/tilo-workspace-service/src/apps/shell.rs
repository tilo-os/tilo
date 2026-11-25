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

/// Who a shell link was made for, and the seat it is for, if any.
pub type ShellLink = (ShellCaller, Option<u64>);

/// The shell links the daemon made and no shell took yet: each a one-use secret the daemon
/// sends on a connection it opens itself, then hands to the shell it starts. And the
/// connections it asked to hand the device's tilolang processes an instance on.
#[derive(Default)]
pub struct ShellLinks {
    shells: std::sync::Mutex<std::collections::HashMap<[u8; 32], ShellLink>>,
    processes: std::sync::Mutex<std::collections::HashSet<[u8; 32]>>,
    /// The seats a shell link is admitted for now: a seat has one shell, and a second link for it
    /// is refused while the first stands.
    live: std::sync::Mutex<std::collections::HashSet<u64>>,
}

impl ShellLinks {
    /// A new link's secret, admitting one connection as `caller`'s shell, for no seat in
    /// particular: what a test's link and a shell with no seat by it are made with.
    #[must_use]
    pub fn make(&self, caller: ShellCaller) -> [u8; 32] {
        self.make_for(caller, None)
    }

    /// A new link's secret, admitting one connection as `caller`'s shell for `seat`, whose one
    /// shell it is: while the seat has a live link, its next link is refused.
    #[must_use]
    pub fn make_for(&self, caller: ShellCaller, seat: Option<u64>) -> [u8; 32] {
        let secret: [u8; 32] = rand::random();
        lock(&self.shells).insert(secret, (caller, seat));
        secret
    }

    /// Let the link with `secret` be made no more: a link whose dispatch could not be reached
    /// never admits a connection, and is not kept waiting for one.
    pub fn forget(&self, secret: &[u8; 32]) {
        lock(&self.shells).remove(secret);
    }

    /// Who the link with `secret` was made for, without taking it: what a daemon asks to know
    /// whose link it is about to make.
    #[must_use]
    pub fn peek(&self, secret: &[u8; 32]) -> Option<ShellCaller> {
        lock(&self.shells).get(secret).map(|(caller, _)| *caller)
    }

    /// The seat the link with `secret` is made for, once the shell that takes it is known: the
    /// secret then admits one connection for that seat, which takes no other while it stands.
    /// `false` when no such secret waits.
    pub fn assign(&self, secret: &[u8; 32], seat: u64) -> bool {
        match lock(&self.shells).get_mut(secret) {
            Some((_, held)) => {
                *held = Some(seat);
                true
            }
            None => false,
        }
    }

    /// Who the link with `secret` was made for and the seat it is, once: a secret is taken by its
    /// first connection.
    #[must_use]
    pub fn take(&self, secret: &[u8; 32]) -> Option<ShellLink> {
        lock(&self.shells).remove(secret)
    }

    /// Whether the seat has a shell link standing now.
    #[must_use]
    pub fn live(&self, seat: u64) -> bool {
        lock(&self.live).contains(&seat)
    }

    /// The seat's shell link began: `false` when one stands already, which is refused.
    #[must_use]
    pub fn began(&self, seat: u64) -> bool {
        lock(&self.live).insert(seat)
    }

    /// The seat's shell link ended: its seat takes a new one from now on.
    pub fn ended(&self, seat: u64) {
        lock(&self.live).remove(&seat);
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
    /// Tilo's API for the person's own programs on this computer, serving a connection the shell
    /// took at their API socket ([`runtime_collab::wire::shell::HandedTo::Api`]): `None` where
    /// the device serves no API, and the connection is refused.
    pub api: Option<ShellApi>,
    /// What carries a link of the shell's live session (`tilo_live::link`), handed over as a
    /// connection the shell made for it ([`runtime_collab::wire::shell::HandedTo::Live`]): the
    /// device's live session, which runs its relay. `None` where the device runs none, and a
    /// link is refused.
    pub live: Option<LiveLinks>,
    /// The device's one word in each workspace's live session, which the shell tells what the
    /// person says of themselves and which call they are in. `None` where the device runs no
    /// live session, and the shell's word is refused.
    pub speaking: Option<Arc<dyn ShellSpeaking>>,
}

/// The device's one word in each of its workspaces' live sessions, as the person's shell tells
/// it: the daemon says it, beside what the device's apps say, and holds the media keys of the
/// call the person is in.
pub trait ShellSpeaking: Send + Sync {
    /// The person says `word` of themselves in `workspace` now, or nothing any more (`None`),
    /// and the device executes `agents` there.
    ///
    /// # Errors
    /// The device takes no part in `workspace`'s live session.
    fn say(
        &self,
        workspace: &str,
        word: Option<runtime_collab::wire::shell::ShellWord>,
        agents: Vec<runtime_collab::wire::shell::SpokenFor>,
    ) -> Result<(), String>;

    /// The person joins `call` in `workspace`, or leaves the call they are in (`None`): the
    /// call's channel and its rule's generation once the device holds its media keys.
    fn join(
        &self,
        workspace: &str,
        call: Option<runtime_collab::wire::shell::ShellInCall>,
    ) -> JoinAnswer;

    /// The shell's link ended: the person says nothing of themselves and is in no workspace's
    /// call any more, and the device says only what it is there.
    fn gone(&self);
}

/// What [`ShellSpeaking::join`] answers once the device holds the call's media keys: the call's
/// channel and its rule's generation.
pub type JoinAnswer = Pin<Box<dyn Future<Output = Result<Option<(String, u64)>, String>> + Send>>;

/// Tilo's API as the device serves it to the programs of the shell's person: it serves the
/// connection, or refuses it, saying why, for a shell whose person it does not act for.
pub type ShellApi =
    Arc<dyn Fn(&ShellCaller, tilo_socket::Stream) -> Result<(), String> + Send + Sync>;

/// What carries one admitted link of a shell's live session until it ends.
pub type LiveLinks = Arc<
    dyn Fn(tilo_socket::tokio::Stream) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A seat has one shell: its link is admitted once, and a second link for the same seat is
    /// refused until the first ends.
    #[test]
    fn a_seat_admits_one_shell_link_at_a_time() {
        let links = ShellLinks::default();
        let caller = ShellCaller { person: [3; 32] };
        let secret = links.make_for(caller, Some(7));
        assert_eq!(
            links.peek(&secret),
            Some(caller),
            "the secret waits for its link"
        );
        assert!(!links.live(7), "nothing is linked yet");
        let (taken, seat) = links
            .take(&secret)
            .expect("the secret admits one connection");
        assert_eq!(taken.person, caller.person);
        assert_eq!(seat, Some(7));
        assert!(links.began(7), "the first connection takes the seat");
        assert!(
            !links.began(7),
            "a second connection for the seat is refused"
        );
        links.ended(7);
        assert!(links.began(7), "a seat whose link ended takes another");
        // A secret that names no seat is admitted and holds none.
        let secret = links.make(caller);
        assert_eq!(links.take(&secret).map(|(_, seat)| seat), Some(None));
    }
}
