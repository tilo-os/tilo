//! The workspaces' live sessions on this device, as the person's desktop takes part in them.
//!
//! A device has one place in each workspace's live session, and it is its daemon's: the daemon
//! owns the endpoint, holds the workspace's keys and relays or dials the workspace's relay
//! (`tilo_runtime`'s live session). This session captures, encodes, plays out and draws, and
//! reaches that one session over a link on the daemon's socket
//! ([`tilo_live::link_call::Call`]): what it publishes goes in unsealed and the daemon seals it
//! under the workspace's keys, and what the live session hears comes back opened. There is no
//! second session here and no key to hold.
//!
//! Presence is the daemon's to say: the device has one word in each workspace, which the daemon
//! puts together from what this session tells it of the person over the shell link
//! ([`Telling`]) and what the device's apps say, and publishes. This session hears everyone
//! else's over its links, as it hears the rest.
//!
//! How far each workspace's live session came — open, waiting for keys, a relay not reached, a
//! relay that refused this device — is the daemon's to know, published as its own reading of the
//! device ([`tilo_live::link::SESSIONS`]); this session follows it and shows it beside the
//! workspace. The endpoints the device's relay turned away ([`tilo_live::link::TURNED_AWAY`]) are
//! followed the same way.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_collab::wire::shell::{ShellCall, ShellInCall, ShellWord, SpokenFor};
use tilo_desktop_model::WorkspaceId;
use tilo_live::capture::Session as CaptureSession;
use tilo_live::link::Opening;
use tilo_live::link_call::Call;
use tilo_lock::MutexRecover as _;

use crate::followed_resource::FollowedReading;

mod own_devices;
pub(crate) use own_devices::OwnDevices;

/// How long one dial of a device's relay may take.
const ATTEMPT: Duration = Duration::from_secs(4);

/// How a live session is read by the capture loops: the one session surface, whether it is the
/// daemon's (over the link) or a local one in a test.
pub(crate) type Sessions = Arc<dyn CaptureSession>;

/// The endpoints the device's relay turned away from each workspace's live session, by
/// workspace, each as the daemon says it ([`tilo_live::link::TURNED_AWAY`]).
pub type TurnedAwayLists = BTreeMap<String, Vec<serde_json::Value>>;

/// How far one workspace's live session came, as the daemon's reading says it
/// ([`tilo_live::link::SESSIONS`]).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub(crate) struct SessionRow {
    #[serde(default)]
    state: String,
    #[serde(default)]
    relay: String,
    #[serde(default)]
    relay_name: String,
    #[serde(default)]
    relay_here: bool,
    #[serde(default)]
    epoch: Option<u64>,
    #[serde(default)]
    lent: bool,
}

/// The daemon's session reading, by workspace.
pub(crate) type SessionRows = BTreeMap<String, SessionRow>;

/// A device that relays a workspace's live session, as a state names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relayer {
    /// Its device key.
    pub device: [u8; 32],
    /// What its person called it; empty for a device they never named.
    pub name: String,
    /// Whether it is this device.
    pub here: bool,
}

/// How far one workspace's live session came on this device. A live session that is not open
/// carries no presence, no call and no picture of a shared window, and nothing else says so. The
/// daemon publishes this, so a person, a tool and a test read why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveState {
    /// This device is in the live session, through `relay`, sealing under `epoch`.
    Open {
        relay: Relayer,
        epoch: u64,
        lent: bool,
    },
    /// The workspace's media keys are not here yet.
    WaitingForKeys,
    /// This device holds no keys of the workspace and none are coming.
    Keyless,
    /// Who is in the workspace cannot be read here.
    Unread,
    /// The workspace names no device that relays its live session.
    NoRelay,
    /// The workspace's relay, `relay`, was not reached.
    Unreached { relay: Relayer },
    /// The relay was reached and was admitting as many devices as it takes at once.
    Busy { relay: Relayer },
    /// `relay` answered and did not admit this device, which presented the keys of `epoch`.
    Refused { relay: Relayer, epoch: u64 },
}

impl LiveState {
    /// Whether the live session carries what this device says and asks for now.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }
}

/// One row of the daemon's reading, as a state.
fn state_of(row: &SessionRow) -> LiveState {
    let relayer = || Relayer {
        device: match hex::decode(&row.relay) {
            Ok(bytes) => {
                if let Ok(device) = bytes.try_into() {
                    device
                } else {
                    tilo_failure::ignored!(
                        "a relay the reading names by no device key shows as none, as a workspace \
                         without a relay shows",
                        tracing::debug!(relay = %row.relay,
                            "a relay key in the session reading is not a device key")
                    );
                    [0; 32]
                }
            }
            Err(error) => {
                tilo_failure::ignored!(
                    "a relay the reading names by no device key shows as none, as a workspace \
                     without a relay shows",
                    tracing::debug!(%error, relay = %row.relay,
                        "a relay key in the session reading is not hex")
                );
                [0; 32]
            }
        },
        name: row.relay_name.clone(),
        here: row.relay_here,
    };
    match row.state.as_str() {
        "open" => LiveState::Open {
            relay: relayer(),
            epoch: row.epoch.unwrap_or(0),
            lent: row.lent,
        },
        "waiting-for-keys" => LiveState::WaitingForKeys,
        "no-keys" => LiveState::Keyless,
        "no-relay" => LiveState::NoRelay,
        "relay-unreached" => LiveState::Unreached { relay: relayer() },
        "relay-busy" => LiveState::Busy { relay: relayer() },
        "refused" => LiveState::Refused {
            relay: relayer(),
            epoch: row.epoch.unwrap_or(0),
        },
        _ => LiveState::Unread,
    }
}

/// What opens one link of the live session on the daemon's socket.
type Links = Arc<dyn Fn() -> Opening + Send + Sync>;

/// How long a workspace whose link the daemon did not open is waited out before it asks again;
/// doubled after each miss, never longer than the schedule.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LONGEST: Duration = Duration::from_secs(15);

/// The workspaces of this session, each reached through the daemon's live session.
pub struct Live {
    shared: Arc<Shared>,
    /// How far each workspace's live session came, as the daemon publishes it, once followed.
    sessions: Option<FollowedReading<SessionRows>>,
    /// Whom the device's relay turned away, once followed.
    turned: Option<FollowedReading<TurnedAwayLists>>,
    /// Told when the session ends; dropping it ends the live thread.
    stopped: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("device", &hex::encode(self.shared.device))
            .finish_non_exhaustive()
    }
}

struct Shared {
    /// This device's certificate: what a grant to join a live session presents.
    cert: tilo_user_identity::DeviceCert,
    device: [u8; 32],
    /// The workspaces the device's daemon keeps, whose live sessions this desktop takes part in.
    view: std::sync::OnceLock<Arc<crate::workspace_view::WorkspaceView>>,
    /// This workspace's link, by workspace: the one place this desktop is in its live session.
    calls: Mutex<BTreeMap<WorkspaceId, Arc<Call>>>,
    /// How long each workspace waits before it asks its daemon for a link again.
    waits: Mutex<BTreeMap<WorkspaceId, Duration>>,
    /// Counts each live session opened: what wakes the session to read them again.
    moves: AtomicU64,
    /// Told when the session's workspaces are to be read again, beside the key service that
    /// announces one joined later.
    readopt: tokio::sync::Notify,
    wake: Arc<dyn Fn() + Send + Sync>,
    runtime: std::sync::OnceLock<tokio::runtime::Handle>,
    links: std::sync::OnceLock<Links>,
    /// The shell link the person's word is told to the daemon on, once the session has it.
    shell: std::sync::OnceLock<runtime_collab::ShellClient>,
    /// What is yet to be told, and what wakes the telling.
    told: Mutex<Told>,
    telling: tokio::sync::Notify,
}

impl Live {
    /// This session's live session. Nothing runs until [`Self::open_links`] hands it the way to
    /// its daemon.
    pub(crate) fn new(
        cert: tilo_user_identity::DeviceCert,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let device = cert.device_public_key;
        Self {
            shared: Arc::new(Shared {
                cert,
                device,
                view: std::sync::OnceLock::new(),
                calls: Mutex::default(),
                waits: Mutex::default(),
                moves: AtomicU64::new(0),
                readopt: tokio::sync::Notify::new(),
                wake,
                runtime: std::sync::OnceLock::new(),
                links: std::sync::OnceLock::new(),
                shell: std::sync::OnceLock::new(),
                told: Mutex::default(),
                telling: tokio::sync::Notify::new(),
            }),
            sessions: None,
            turned: None,
            stopped: None,
            thread: None,
        }
    }

    /// Show how far each workspace's live session came as its daemon publishes it
    /// ([`tilo_live::link::SESSIONS`]).
    pub(crate) fn follow_sessions(&mut self, sessions: FollowedReading<SessionRows>) {
        self.sessions = Some(sessions);
    }

    /// Show `turned`, whom the device's relay turned away as its daemon publishes it.
    pub(crate) fn follow_turned_away(&mut self, turned: FollowedReading<TurnedAwayLists>) {
        self.turned = Some(turned);
    }

    /// The endpoints the device's relay turned away from each workspace's live session and has
    /// not let in since, by workspace: none until its daemon's reading arrived.
    #[must_use]
    pub(crate) fn turned_away(&self) -> TurnedAwayLists {
        self.turned
            .as_ref()
            .map(FollowedReading::get)
            .unwrap_or_default()
    }

    /// This device came to hold another workspace: its link is opened now.
    pub(crate) fn workspace_arrived(&self) {
        self.shared.readopt.notify_one();
    }

    /// Run for the rest of the session, opening each workspace's link with `links` on the
    /// daemon's socket. Only the first call counts.
    /// Take part in the live sessions of the workspaces `view` reads the daemon keeping, from now
    /// on: one the daemon takes in later is opened as it arrives.
    pub(crate) fn follow_workspaces(&self, view: Arc<crate::workspace_view::WorkspaceView>) {
        let _ = self.shared.view.set(view);
        self.shared.readopt.notify_one();
    }

    pub(crate) fn open_links(&mut self, links: Links) {
        if self.shared.links.set(links).is_err() {
            return;
        }
        let (stop, stopped) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::clone(&self.shared);
        match std::thread::Builder::new()
            .name("tilo-live".into())
            .spawn(move || run(shared, stopped))
        {
            Ok(thread) => {
                self.thread = Some(thread);
                self.stopped = Some(stop);
            }
            Err(error) => tracing::warn!(%error, "the live session runs no thread"),
        }
    }

    /// The next object heard in `workspace`'s live session, or `None`.
    #[must_use]
    pub fn try_recv(&self, workspace: WorkspaceId) -> Option<tilo_live::live_session::Heard> {
        self.shared
            .calls
            .lock_recover()
            .get(&workspace)
            .and_then(|call| call.try_recv())
    }

    /// This device's live sessions, by workspace: what a call's audio is published into, and who
    /// it hears there.
    #[must_use]
    pub fn live_sessions(&self) -> BTreeMap<WorkspaceId, Sessions> {
        self.shared
            .calls
            .lock_recover()
            .iter()
            .map(|(workspace, call)| (*workspace, Arc::clone(call) as Sessions))
            .collect()
    }

    /// How far each workspace's live session came, as the daemon last published it.
    #[must_use]
    pub fn states(&self) -> Vec<(WorkspaceId, LiveState)> {
        let Some(sessions) = &self.sessions else {
            return Vec::new();
        };
        sessions
            .get()
            .iter()
            .filter_map(|(workspace, row)| Some((WorkspaceId::parse(workspace)?, state_of(row))))
            .collect()
    }

    /// Moves whenever a state did, a live session opened or whom the relay turned away did.
    #[must_use]
    pub fn states_revision(&self) -> u64 {
        let sessions = self.sessions.as_ref().map_or(0, FollowedReading::revision);
        let turned = self.turned.as_ref().map_or(0, FollowedReading::revision);
        self.shared
            .moves
            .load(Ordering::Relaxed)
            .wrapping_add(sessions)
            .wrapping_add(turned)
    }

    /// The runtime the live session's links run on, once its thread started it.
    #[must_use]
    pub fn runtime(&self) -> Option<tokio::runtime::Handle> {
        self.shared.runtime.get().cloned()
    }

    /// The way to the person's other devices, each through its own relay, and from them to this
    /// one: what drives one of their devices from another.
    pub(crate) fn own_devices(&self) -> OwnDevices {
        OwnDevices::new(Arc::clone(&self.shared))
    }

    /// What tells the device's daemon the person's word, for the daemon to say as the device's
    /// one word in each workspace: the session publishes no presence of its own.
    pub(crate) fn telling(&self) -> Telling {
        Telling(Arc::clone(&self.shared))
    }

    /// Tell the daemon the person's word over `shell`, the shell link it handed this session,
    /// from now on. Only the first call counts.
    pub(crate) fn tell_through(&self, shell: runtime_collab::ShellClient) {
        let _ = self.shared.shell.set(shell);
        self.shared.telling.notify_one();
    }

    /// Take part in the workspaces' live sessions through `daemon`, the shell link it handed this
    /// session, from now on: each workspace's link opens on its socket, which the daemon carries
    /// into its relay or on to another device's, and the person's word is told over it. What the
    /// daemon publishes of how far each session came and whom its relay turned away is shown
    /// beside them.
    pub(crate) fn through(
        &mut self,
        daemon: runtime_collab::ShellClient,
        view: Option<Arc<crate::workspace_view::WorkspaceView>>,
        turned: FollowedReading<TurnedAwayLists>,
        sessions: FollowedReading<SessionRows>,
    ) {
        if let Some(view) = view {
            self.follow_workspaces(view);
        }
        self.follow_turned_away(turned);
        self.follow_sessions(sessions);
        self.tell_through(daemon.clone());
        self.open_links(Arc::new(move || {
            let daemon = daemon.clone();
            Box::pin(async move {
                let stream = daemon
                    .live()
                    .await
                    .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
                let (reader, writer) = stream.into_split();
                Ok((
                    Box::new(reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                    Box::new(writer) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                ))
            })
        }));
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        drop(self.stopped.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The live session's thread: one link per workspace, each opened through the daemon and
/// reopened when it ends.
fn run(shared: Arc<Shared>, mut stopped: tokio::sync::mpsc::UnboundedReceiver<()>) {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("tilo-live-io")
        .build()
    else {
        tracing::warn!("the live session runs no runtime");
        return;
    };
    let _ = shared.runtime.set(runtime.handle().clone());
    runtime.spawn(tell_daemon(Arc::clone(&shared)));
    runtime.block_on(async move {
        loop {
            let view = shared.view.get().cloned();
            let seen = view.as_ref().map_or(0, |view| view.revision());
            let workspaces = view
                .as_ref()
                .map(|view| view.workspaces())
                .unwrap_or_default();
            for workspace in &workspaces {
                open_or_keep(&shared, *workspace).await;
            }
            let changed = async {
                match &view {
                    Some(view) => view.until(|view| view.revision() != seen).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = shared.readopt.notified() => {}
                () = changed => {}
                _ = stopped.recv() => return,
                () = tokio::time::sleep(RETRY_LONGEST) => {}
            }
        }
    });
}

/// Open `workspace`'s link unless one is live, and let go of one that ended.
async fn open_or_keep(shared: &Arc<Shared>, workspace: WorkspaceId) {
    {
        let mut calls = shared.calls.lock_recover();
        if calls.get(&workspace).is_some_and(|call| !call.ended()) {
            return;
        }
        calls.remove(&workspace);
    }
    let Some(links) = shared.links.get() else {
        return;
    };
    let links = Arc::clone(links);
    let device = shared.device;
    let opened = Call::open(move || links(), workspace.to_string(), device).await;
    match opened {
        Ok(call) => {
            let wake = Arc::clone(&shared.wake);
            call.set_wake(Arc::clone(&wake));
            {
                let weak = Arc::downgrade(shared);
                let moved = Arc::clone(&wake);
                call.set_ended(Arc::new(move || {
                    if let Some(shared) = weak.upgrade() {
                        shared.moves.fetch_add(1, Ordering::Relaxed);
                        drop(shared);
                    }
                    moved();
                }));
            }
            shared.calls.lock_recover().insert(workspace, call);
            shared.waits.lock_recover().insert(workspace, RETRY_FIRST);
            shared.moves.fetch_add(1, Ordering::Relaxed);
            (shared.wake)();
            tracing::debug!(%workspace, "the desktop's live link is open");
        }
        Err(error) => {
            let wait = {
                let mut waits = shared.waits.lock_recover();
                let wait = waits.get(&workspace).copied().unwrap_or(RETRY_FIRST);
                let next = wait.saturating_mul(2).min(RETRY_LONGEST);
                waits.insert(workspace, next);
                wait
            };
            tilo_failure::ignored!(
                "the workspace's link is asked for again on the next pass",
                tracing::debug!(%workspace, %error, ?wait,
                    "the daemon opened no live link for the workspace yet")
            );
            tokio::time::sleep(wait).await;
        }
    }
}

impl Shared {
    /// The grant this device presents to join `scope` of `workspace`, sealed under `epoch`.
    fn grant(
        &self,
        workspace: WorkspaceId,
        scope: &collab_types::scope::Scope,
        epoch: u64,
    ) -> Result<Vec<u8>, String> {
        let certificate = serde_json::to_vec(&self.cert).map_err(|error| error.to_string())?;
        tilo_live::grant::Native::new(
            *workspace.as_uuid().as_bytes(),
            &scope.id(),
            epoch,
            certificate,
            Vec::new(),
        )
        .map(|grant| grant.encode())
    }
}

/// What the session tells the device's daemon of the person: their word in each workspace, and
/// the calls they join and leave, each on the shell link. Only the newest word of a workspace is
/// told: one that was overtaken before it left is not owed.
pub(crate) struct Telling(Arc<Shared>);

impl std::fmt::Debug for Telling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the device's daemon")
    }
}

impl tilo_session_client::TellWord for Telling {
    fn tell(
        &self,
        workspace: WorkspaceId,
        word: Option<tilo_live::presence::Own>,
        agents: Vec<(String, String, collab_types::presence::ShellPresence)>,
    ) {
        let said = ShellCall::Say {
            workspace: workspace.to_string(),
            word: word.map(|word| Box::new(word_of(word))),
            agents: agents
                .into_iter()
                .map(|(session, member, face)| SpokenFor {
                    session,
                    member,
                    face,
                })
                .collect(),
        };
        self.0.told.lock_recover().words.insert(workspace, said);
        self.0.telling.notify_one();
    }

    fn join(&self, workspace: WorkspaceId, call: Option<tilo_live::presence::Call>) {
        let workspace = workspace.to_string();
        let asked = match call {
            Some(call) => ShellCall::Join {
                workspace,
                call: ShellInCall {
                    call: call.call,
                    camera: call.camera,
                    screen: call.screen,
                    muted: call.muted,
                    deafened: call.deafened,
                },
            },
            None => ShellCall::Leave { workspace },
        };
        self.0.told.lock_recover().calls.push(asked);
        self.0.telling.notify_one();
    }
}

/// What the session has yet to tell the daemon.
#[derive(Default)]
struct Told {
    /// The newest word of each workspace.
    words: BTreeMap<WorkspaceId, ShellCall>,
    /// The joins and leaves, in the order the person made them.
    calls: Vec<ShellCall>,
}

/// Tell the daemon what the session has to tell, as it comes, until the session ends.
async fn tell_daemon(shared: Arc<Shared>) {
    loop {
        shared.telling.notified().await;
        let Some(shell) = shared.shell.get() else {
            continue;
        };
        let told = std::mem::take(&mut *shared.told.lock_recover());
        for call in told.calls.into_iter().chain(told.words.into_values()) {
            if let Err(error) = shell.call(call).await {
                tilo_failure::ignored!(
                    "presence is ephemeral: the next word says where the person is again",
                    tracing::debug!(?error, "the daemon was not told the person's word")
                );
            }
        }
    }
}

/// The person's word, as the shell link carries it to the daemon.
fn word_of(own: tilo_live::presence::Own) -> ShellWord {
    ShellWord {
        label: own.label,
        device: own.device,
        user: own.user,
        profile_ms: own.profile_ms,
        status: own.status.map(|status| (status.text, status.automatic)),
        availability: own.availability,
        call: own.call.map(|call| ShellInCall {
            call: call.call,
            camera: call.camera,
            screen: call.screen,
            muted: call.muted,
            deafened: call.deafened,
        }),
        following: own.following,
        wave: own.wave,
        pointer: own.pointer.map(|point| (point.tab, point.x, point.y)),
        shell: own.shell,
        apps: own.apps,
    }
}
