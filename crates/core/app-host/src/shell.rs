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
/// sends on a connection it opens itself, then hands to the shell it starts. And the hand-overs
/// to the device's tilolang processes a linked shell asked for ([`Self::processes`]), which the
/// shell then makes on the daemon's native endpoint ([`Self::hand_processes`]).
#[derive(Default)]
pub struct ShellLinks {
    shells: std::sync::Mutex<std::collections::HashMap<[u8; 32], ShellLink>>,
    processes: std::sync::Mutex<Vec<ProcessesSecret>>,
    /// The device's tilolang processes, once the daemon serves them: `None` where none can start.
    serving: std::sync::OnceLock<ShellProcesses>,
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

    /// Hand the shells' instances to `processes` from now on: the device's tilolang processes,
    /// served once.
    pub fn serve_processes(&self, processes: ShellProcesses) {
        let _ = self.serving.set(processes);
    }

    /// Whether the device's tilolang processes are served here.
    #[must_use]
    pub fn serves_processes(&self) -> bool {
        self.serving.get().is_some()
    }

    /// A secret admitting one hand-over to the device's tilolang processes from the account
    /// `uid`: what a linked shell is answered when it asks to hand them an instance. `uid` is the
    /// account of the seat the shell's link was made for, which the kernel named when the link
    /// was asked for; the secret admits that account alone, once, within
    /// [`PROCESSES_SECRET_LIFETIME`].
    #[must_use]
    pub fn processes(&self, uid: u32) -> [u8; 32] {
        self.mint_processes(uid, tilo_clock::now())
    }

    /// [`Self::processes`], as though minted at `minted`: what a test of a secret that outlived
    /// its lifetime mints with.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn processes_minted_at(&self, uid: u32, minted: tilo_clock::time::Instant) -> [u8; 32] {
        self.mint_processes(uid, minted)
    }

    fn mint_processes(&self, uid: u32, minted: tilo_clock::time::Instant) -> [u8; 32] {
        let secret: [u8; 32] = rand::random();
        let mut waiting = lock(&self.processes);
        // A secret no shell spent in time admits nothing, and is not kept.
        let now = tilo_clock::now();
        waiting.retain(|kept| !kept.expired(now));
        waiting.push(ProcessesSecret {
            secret,
            uid,
            minted,
        });
        secret
    }

    /// Hand the device's tilolang processes `packets`, the packet socket a connection from the
    /// account `uid` (the kernel's word for its peer) carried with `secret`. The secret is spent
    /// by being presented, whatever the answer: it admits one hand-over, from the account it was
    /// minted for, within its lifetime.
    ///
    /// # Errors
    /// Why the hand-over is refused: no such secret waits, it outlived its lifetime, it was minted
    /// for another account, or no processes run here.
    pub fn hand_processes(
        &self,
        uid: u32,
        secret: &[u8; 32],
        packets: runtime_portal::packet::Descriptor,
    ) -> Result<(), String> {
        let mut waiting = lock(&self.processes);
        // Every secret is compared, each in constant time, so how long the search takes says
        // nothing of how near a guess came.
        let mut found = None;
        for (at, kept) in waiting.iter().enumerate() {
            if same_secret(&kept.secret, secret) {
                found = Some(at);
            }
        }
        let taken = found.map(|at| waiting.swap_remove(at));
        drop(waiting);
        let Some(taken) = taken else {
            return Err(
                "this secret is no hand-over a shell asked for on its link, or it was spent"
                    .to_owned(),
            );
        };
        if taken.expired(tilo_clock::now()) {
            return Err("this hand-over's secret outlived its lifetime".to_owned());
        }
        if taken.uid != uid {
            return Err(
                "this hand-over's secret was asked for by another account's shell".to_owned(),
            );
        }
        let processes = self
            .serving
            .get()
            .ok_or("no tilolang process can start on this device")?;
        processes(packets);
        Ok(())
    }
}

/// How long a hand-over's secret admits its connection after the shell was answered it: the shell
/// connects at once, so a secret that waits longer was lost, or taken.
pub const PROCESSES_SECRET_LIFETIME: std::time::Duration = std::time::Duration::from_secs(10);

/// A hand-over to the device's tilolang processes a linked shell asked for: the secret, the
/// account it admits, and when it was minted.
struct ProcessesSecret {
    secret: [u8; 32],
    uid: u32,
    minted: tilo_clock::time::Instant,
}

impl ProcessesSecret {
    fn expired(&self, now: tilo_clock::time::Instant) -> bool {
        now.saturating_duration_since(self.minted) >= PROCESSES_SECRET_LIFETIME
    }
}

/// Whether two secrets are the same, compared in time that does not depend on where they differ.
fn same_secret(kept: &[u8; 32], presented: &[u8; 32]) -> bool {
    let mut difference = 0_u8;
    for (kept, presented) in kept.iter().zip(presented) {
        difference |= kept ^ presented;
    }
    std::hint::black_box(difference) == 0
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
pub type DocumentWrites =
    tokio::sync::broadcast::Receiver<tilo_workspace_service::workspaces::ResourceWrite>;

/// What the daemon's dispatch answers a person's shell with.
#[derive(Clone)]
pub struct ShellServices {
    /// The links of the shells the daemon starts: a connection is a shell's only with one.
    pub links: Arc<ShellLinks>,
    /// The daemon's native endpoint: where the shell hands the device's tilolang processes an
    /// instance ([`ShellLinks::hand_processes`]), on a connection the kernel names its account on.
    pub socket: std::path::PathBuf,
    /// The device's documents, read for the device itself.
    pub documents: Arc<dyn DeviceDocuments>,
    /// The device's documents, written as the device: signed and sealed with its key.
    pub writes: Arc<dyn DeviceWrites>,
    /// Each write the device takes to a document of its workspaces
    /// ([`tilo_workspace_service::workspaces::Workspaces::document_writes`]): what a shell reads again by.
    pub document_writes: Arc<dyn Fn() -> DocumentWrites + Send + Sync>,
    /// The device's one dispatch, as the shell's person reaches it.
    pub actions: Arc<dyn ShellActions>,
    /// The workspaces' standing ([`tilo_peer_state::workspace_view`]), watched as published resources.
    pub standing: Arc<dyn PublishedResources>,
    /// The resources this device's services publish to the apps attached to it (its server
    /// settings): the shell link reads and changes them as the app it names, so a session's apps
    /// reach the one copy the daemon keeps.
    pub published: Option<Arc<dyn PublishedResources>>,
    /// The workspaces the device holds: what the resource commands of the apps the shell runs
    /// itself, and its asks of a workspace's vault, run on.
    pub workspaces: tilo_workspace_service::workspaces::Workspaces,
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
    /// What carries a link of the device VPN ([`device_vpn::link`]), handed over as a connection
    /// the shell made for it ([`runtime_collab::wire::shell::HandedTo::DeviceVpn`]): the daemon,
    /// which holds this device's endpoint and its key. `None` where the daemon serves none, and
    /// a link is refused.
    pub vpn: Option<DeviceVpnLinks>,
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

/// What carries one device-VPN link of a shell's until it ends.
pub type DeviceVpnLinks = Arc<
    dyn Fn(tilo_socket::tokio::Stream) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

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
