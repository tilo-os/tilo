//! The live session between the devices of a workspace: one sealed `moq-lite` session per
//! workspace, which this session takes part in through its own device's daemon.
//! Which device relays is durable workspace state, not presence and not the live session itself, so
//! every device names the same one without asking anyone ([`relay_of`]): the device whose signed
//! address the workspace's governance holds, while it is a member. Two people's own devices
//! sharing a workspace one of them serves have a live session without a server.
//! The device's daemon holds its one endpoint, and with it the relay of every workspace this
//! device serves. This session captures, encodes and draws, and is a `moq-lite` client of that
//! relay like any other device: each workspace's live session runs over a link on the daemon's
//! socket ([`tilo_live::link`]), which the daemon takes into its own relay, for a workspace this
//! device serves, or on to the relay of the device serving it. The relay admits this device by
//! the certificate it presents as its grant, as it admits every device.
//! While that relay does not take this device in — it is away, does not know the device yet, or is
//! admitting as many devices as it takes at once — the device asks it again, waiting longer each
//! time, since a relay that comes back says nothing to the devices that missed it.
//! Each member seals its own tracks under the scope's channel content keys
//! ([`tilo_live::live_session::LiveSession`]): the relay copies bytes it cannot open, and it is a
//! member that holds them already.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collab_types::scope::Scope;
use iroh::{EndpointAddr, EndpointId};
use tilo_desktop_model::WorkspaceId;
use tilo_live::frame::LiveKeys;
use tilo_live::grant;
use tilo_live::link;
use tilo_live::live_session::{Heard, Join, LiveError, LiveSession};
use tilo_live::roster::Roster;
use tilo_lock::MutexRecover as _;

use crate::engine::Engine;
use crate::workspace_keys::{self, Device};

/// The scope one workspace's live session carries.
fn scope_of(workspace: WorkspaceId) -> Scope {
    Scope::workspace(workspace.to_string())
}

/// The workspace's relay, as its state names it: `host`, the device serving the workspace, while
/// it is a current member. `None` otherwise, which is a workspace with no live session.
pub(crate) fn relay_of(
    devices: &[Device],
    host: Option<[u8; 32]>,
    now_ns: u64,
) -> Option<[u8; 32]> {
    devices
        .iter()
        .filter(|device| !device.cert.is_expired_at(now_ns))
        .map(|device| device.device)
        .find(|device| Some(*device) == host)
}

/// How long the workspace's relay has to open its live session before this device gives up on this
/// attempt.
const ATTEMPT: Duration = Duration::from_secs(4);
/// How long a workspace whose relays did not take this device in waits before it asks them again:
/// [`RETRY_FIRST`] after the first miss, twice as long after each further one, and never longer
/// than [`RETRY_LONGEST`]. A relay that is away costs one dial of [`ATTEMPT`] each time, so a
/// device whose workspace's host is off for a week asks four or five times a minute and no more.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LONGEST: Duration = Duration::from_secs(15);

/// The wait the schedule gives before a workspace's relays are asked again after `missed` attempts
/// in a row that none of them answered with a live session.
fn retry_schedule(missed: u32) -> Duration {
    let doublings = missed.saturating_sub(1).min(8);
    RETRY_FIRST
        .saturating_mul(1_u32 << doublings)
        .min(RETRY_LONGEST)
}

/// The wait before `device` asks `workspace` 's relays again after `missed` attempts in a row that
/// none of them answered with a live session: the schedule's ([`retry_schedule`]), less up to a quarter
/// that the device takes off by its own key ([`tilo_live::live_session::spread`]).
///
/// The devices of a workspace miss its relay together: it was away and came back, or it was
/// admitting as many as it takes when they all asked. On the schedule alone they would ask it
/// again in the same moment each time, and sixteen of them would be taken at each try. Spread
/// over a quarter, they arrive one after another, and each wait is still longer than the one
/// before it and never longer than the schedule's.
fn retry_wait(device: [u8; 32], workspace: WorkspaceId, missed: u32) -> Duration {
    let mut turn = std::hash::DefaultHasher::new();
    workspace.as_uuid().as_bytes().hash(&mut turn);
    missed.hash(&mut turn);
    tilo_live::live_session::spread(retry_schedule(missed), device, turn.finish())
}

/// Now, in Unix nanoseconds.
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}

/// A device that relays a workspace's live session, as a state names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relayer {
    /// Its device key.
    pub device: [u8; 32],
    /// What its person called it, as the certificate the workspace holds of it says; empty for a
    /// device they never named.
    pub name: String,
    /// Whether it is this device.
    pub here: bool,
}

/// How far one workspace's live session came on this device. A live session that is not open carries no
/// presence, no call and no picture of a shared window, and nothing else says so: what was asked
/// of it waits without a word. The session publishes this, so a person, a tool and a test read why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveState {
    /// This device is in the live session, through `relay`, sealing under the keys of `epoch`. `lent`
    /// says those are the keys of its own streams alone, lent by the workspace's host
    /// ([`MediaKeysLender`]): it publishes with them and opens nothing another device sends.
    Open {
        relay: Relayer,
        epoch: u64,
        lent: bool,
    },
    /// This device asked for the keys of the workspace's media channel, or asked the workspace's
    /// host to lend it the keys of its own streams, and does not hold them yet.
    WaitingForKeys,
    /// This device holds no keys of the workspace and none are coming: it keeps no secret store,
    /// or the workspace removed it from the channel.
    Keyless,
    /// Who is in the workspace cannot be read here, so nothing names who relays it.
    Unread,
    /// The workspace names no device that relays its live session.
    NoRelay,
    /// The workspace's relay, `relay`, was not reached.
    Unreached { relay: Relayer },
    /// The workspace's relay, `relay`, was reached and took nothing of this device's session: it
    /// was admitting as many devices as it takes at once. It judged nobody, so this is no refusal,
    /// and the relay is asked again like one that was not reached.
    Busy { relay: Relayer },
    /// `relay` answered and did not admit this device, which presented the keys of `epoch`. Why
    /// is the relay's to say, in its journal: it tells the device it turns away nothing but that.
    Refused { relay: Relayer, epoch: u64 },
}

impl LiveState {
    /// Whether the live session carries what this device says and asks for now.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }
}

/// `device` as a state names it, by the certificate `devices` holds of it.
fn relayer(devices: &[Device], device: [u8; 32], own: [u8; 32]) -> Relayer {
    let name = devices
        .iter()
        .find(|held| held.device == device)
        .and_then(|held| tilo_user_identity::device_name(&held.cert.device_label))
        .unwrap_or_default()
        .to_owned();
    Relayer {
        device,
        name,
        here: device == own,
    }
}

/// Why a workspace's relay did not carry its live session for this device: the relay, whether it
/// refused this device, and whether it was busy. A refusal is an answer that waiting alone does
/// not change; a relay that was not reached may be back at any time. A relay that was admitting
/// as many devices as it takes at once judged nobody: it is asked again like one that was not
/// reached, and never stands as a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Missed {
    relay: [u8; 32],
    refused: bool,
    busy: bool,
}

impl Missed {
    /// What `error`, the answer to this device's ask for a live session through `relay`, says of it.
    const fn of(relay: [u8; 32], error: &LiveError) -> Self {
        Self {
            relay,
            refused: matches!(error, LiveError::Denied),
            busy: matches!(error, LiveError::Busy),
        }
    }

    /// How the live session stands after it: refused by `relay` when it presented the keys of `epoch`,
    /// told by it that it is busy, or waiting to reach it.
    fn state(self, relay: Relayer, epoch: u64) -> LiveState {
        if self.refused {
            LiveState::Refused { relay, epoch }
        } else if self.busy {
            LiveState::Busy { relay }
        } else {
            LiveState::Unreached { relay }
        }
    }
}

/// One joined live session: through which relay, and the session itself.
struct Joined {
    relay: [u8; 32],
    live_session: Arc<LiveSession>,
}

/// What the live sessions share: this session's engine, its own device, what it joined, and how
/// far each workspace's live session came.
struct Shared {
    engine: Arc<Engine>,
    device: [u8; 32],
    live_sessions: Mutex<BTreeMap<WorkspaceId, Joined>>,
    /// How far each workspace's live session came here, as it was last settled ([`LiveState`]).
    states: Mutex<BTreeMap<WorkspaceId, LiveState>>,
    /// Counts each move of a state and each live session opened: what the session reads the states
    /// and the live sessions again on, and not on every pass.
    moves: std::sync::atomic::AtomicU64,
    /// What wakes each workspace's reconciler beside the workspace's own changes: a live session of it
    /// ended, so the workspace is settled again through its relay, or this
    /// session was given a lender of media keys ([`Live::lend_media_keys`]).
    nudges: Mutex<BTreeMap<WorkspaceId, Arc<tokio::sync::Notify>>>,
    /// Told when the session's workspaces are to be read again for ones no reconciler runs for
    /// yet, beside the keys that announce a workspace joined later.
    readopt: tokio::sync::Notify,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The runtime the live sessions run on, once the live session's thread started it: what a live session's readers
    /// are started on, whichever thread asks the live session to show a track.
    runtime: std::sync::OnceLock<tokio::runtime::Handle>,
    /// Where a device that holds none of a workspace's media keys is lent the keys of its own
    /// streams ([`MediaKeysLender`]): a session in a workspace's VM, once it can ask its host.
    lender: std::sync::OnceLock<Arc<dyn MediaKeysLender>>,
    /// What opens a link on the daemon's socket ([`Live::open_links`]).
    links: std::sync::OnceLock<Links>,
}

/// What opens one link of the live session on the daemon's socket.
type Links = Arc<dyn Fn() -> link::Opening + Send + Sync>;

/// One sender's media keys, lent: what a device confined to its own record publishes its presence,
/// windows and pages with on a workspace's live session, holding nothing that opens anyone's stream
/// ([`tilo_live::live_session::LiveKeys::Sender`]).
pub(crate) trait MediaKeysLender: Send + Sync {
    /// This device's own media keys in `workspace` at the media channel's current epoch, as its
    /// host lends them; `None` when they could not be had now.
    fn lend(
        &self,
        workspace: WorkspaceId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<key_group::SenderMediaKeys>> + Send + '_>,
    >;
}

/// How long one ask for lent keys is waited for before the live session waits for the next event instead.
const LEND_PATIENCE: Duration = Duration::from_secs(20);

impl Shared {
    /// This workspace's devices, and the one among them that may relay its live session; `None` —
    /// after withdrawing the live session — when the membership is unknown or names no relay.
    fn relay_for(&self, workspace: WorkspaceId) -> Option<(Vec<Device>, [u8; 32])> {
        let devices = match workspace_keys::devices_of(&self.engine, workspace) {
            Ok(devices) => devices,
            Err(error) => {
                tracing::warn!(%workspace, %error, "the live session's membership is unknown");
                self.withdraw(workspace);
                self.stand(workspace, LiveState::Unread);
                return None;
            }
        };
        let Some(relay) = relay_of(&devices, self.engine.holder_of(workspace), now_ns()) else {
            // No member may relay it: the workspace has no live session.
            if self.live_sessions.lock_recover().contains_key(&workspace) {
                tracing::info!(%workspace, "no member relays this workspace's live session any more");
            }
            self.withdraw(workspace);
            self.stand(workspace, LiveState::NoRelay);
            return None;
        };
        Some((devices, relay))
    }

    /// The keys of this device's own streams in `workspace`, lent by its host, when this session
    /// has a lender and the host answers in time.
    async fn lent(&self, workspace: WorkspaceId) -> Option<key_group::SenderMediaKeys> {
        let lender = Arc::clone(self.lender.get()?);
        match tokio::time::timeout(LEND_PATIENCE, lender.lend(workspace)).await {
            Ok(Some(lent)) if lent.sender() == self.device => Some(lent),
            Ok(_) => None,
            Err(_) => {
                // Nothing this device shows reaches anyone until it has them: worth a warning.
                tracing::warn!(%workspace, "the host lent no media keys in time");
                None
            }
        }
    }

    /// Read `workspace` as it is now and join its relay, move the live session's keys and roster when
    /// they changed, or leave when the workspace names no relay any more.
    async fn settle(self: &Arc<Self>, workspace: WorkspaceId) {
        let Some((devices, serving)) = self.relay_for(workspace) else {
            return;
        };
        // The live session's tracks are sealed under the workspace's media channel: taking part in it asks
        // the workspace's devices for access, and the live session opens on the keys arriving.
        let keys = self.engine.seal_keys(workspace);
        // A device that may hold none of them (a session in a workspace's VM) is lent its own
        // streams' keys instead, asked afresh at each join: a live session whose epoch moved is cut, and
        // joins again on the keys its host lends then.
        let (keys, lent) = match keys {
            Ok(crate::engine::SealKeys::Here(keys)) => (LiveKeys::from(keys), false),
            held => {
                let Some(lent) = self.lent(workspace).await else {
                    self.without_keys(workspace, &held);
                    return;
                };
                (LiveKeys::from(lent), true)
            }
        };
        // What the workspace's calls allow: whose call media the live session opens and sends.
        let (rules, persons) = self.engine.call_gate(&workspace.to_string());
        let roster = Roster::new(devices.iter().map(|device| device.device));
        let scope = scope_of(workspace);
        let kept = {
            let live_sessions = self.live_sessions.lock_recover();
            live_sessions
                .get(&workspace)
                .filter(|joined| joined.relay == serving && !joined.live_session.ended())
                .map(|joined| (joined.relay, Arc::clone(&joined.live_session)))
        };
        if let Some((at, live_session)) = kept {
            // The workspace moved under a relay that still carries it: the live session keeps its sessions
            // and seals under the keys and roster it now has.
            tracing::debug!(%workspace, relay = %hex::encode(at),
                epoch = keys.epoch(), "the live session keeps its relay");
            let epoch = keys.epoch();
            live_session.set_keys(keys);
            live_session.set_roster(roster);
            live_session.set_calls(rules, persons);
            self.hand_call_keys(workspace, &live_session);
            self.stand(
                workspace,
                LiveState::Open {
                    relay: relayer(&devices, at, self.device),
                    epoch,
                    lent,
                },
            );
            return;
        }
        // A live session whose relay the workspace no longer names, or one that ended, is closed before
        // another is opened in its place.
        drop(self.live_sessions.lock_recover().remove(&workspace));
        let epoch = keys.epoch();
        let join = Join {
            scope: scope.clone(),
            device: self.device,
            keys,
            roster,
            publish: true,
            presence: true,
        };
        let opened =
            if let Some((dial, grant)) = self.dial_target(workspace, &scope, epoch, serving) {
                LiveSession::join_within(dial, &grant, join, ATTEMPT).await
            } else {
                // Where the relay is reached, or the grant it asks for, is not here yet: it was not
                // reached, and is asked again.
                Err(LiveError::Unavailable)
            };
        let live_session = match opened {
            Ok(live_session) => live_session,
            Err(error) => {
                tilo_failure::ignored!(
                    "the live session's state says its relay was missed, and the relay is asked again",
                    tracing::debug!(%workspace, relay = %hex::encode(serving), %error,
                        "this workspace's relay was not reached")
                );
                self.missed(workspace, &devices, Missed::of(serving, &error), epoch);
                return;
            }
        };
        tracing::debug!(%workspace, relay = %hex::encode(serving),
            local = serving == self.device, epoch,
            "the live session opened");
        live_session.set_calls(rules, persons);
        self.hand_call_keys(workspace, &live_session);
        let open = LiveState::Open {
            relay: relayer(&devices, serving, self.device),
            epoch,
            lent,
        };
        self.keep(workspace, serving, live_session, open);
    }

    /// Hand `live_session` the keys of the call media it needs now when this device holds them:
    /// the key group of its call's rule's generation, which only the devices the call admits are
    /// given. Asking for them takes this device into the group; the keys arriving changes the
    /// workspace's keys, and the workspace is settled again on it.
    fn hand_call_keys(&self, workspace: WorkspaceId, live_session: &LiveSession) {
        let Some((channel, generation)) = live_session.call_keys_wanted() else {
            return;
        };
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: collab_types::scope::MEDIA_APP.to_owned(),
            cluster_id: workspace.to_string(),
            document: collab_types::calls::media_document(&channel, generation),
        };
        if let Ok(crate::engine::SealKeys::Here(keys)) = self.engine.document_seal_keys(&route) {
            live_session.set_call_keys(&channel, generation, keys);
        }
    }

    /// `workspace` 's keys are not here (`held`), and none were lent: the live session opens when they
    /// are, and until then it stands as waiting for them, or as holding none where none are coming.
    /// A device whose host lends it keys waits for those. A live session already open keeps what it has
    /// until they move.
    fn without_keys(&self, workspace: WorkspaceId, held: &Result<crate::engine::SealKeys, String>) {
        tracing::debug!(%workspace, "the live session waits for the workspace's keys");
        if self.joined(workspace) {
            return;
        }
        let waiting =
            matches!(held, Ok(crate::engine::SealKeys::Waiting)) || self.lender.get().is_some();
        self.stand(
            workspace,
            if waiting {
                LiveState::WaitingForKeys
            } else {
                LiveState::Keyless
            },
        );
    }

    /// Keep `live_session`, opened through the relay `serving`, as `workspace` 's: it wakes the session on
    /// what it hears, has the workspace settled again when it ends, and stands as `open` says.
    fn keep(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        serving: [u8; 32],
        live_session: LiveSession,
        open: LiveState,
    ) {
        live_session.set_wake(Arc::clone(&self.wake));
        let ended = Arc::clone(self);
        live_session.set_ended(Arc::new(move || ended.ended(workspace)));
        // The devices a live session does not hear are part of what the session reads of it.
        let unheard = Arc::clone(self);
        live_session.set_unheard_changed(Arc::new(move || unheard.moved()));
        let replaced = self.live_sessions.lock_recover().insert(
            workspace,
            Joined {
                relay: serving,
                live_session: Arc::new(live_session),
            },
        );
        drop(replaced);
        self.stand(workspace, open);
        // A live session opened again where one stood is another live session, whatever its state says.
        self.moved();
    }

    /// Tell the session that a state moved or a live session opened: it reads both again.
    fn moved(&self) {
        self.moves
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        (self.wake)();
    }

    /// The carrier to the workspace's serving device `serving`, through this device's daemon, and
    /// the grant that admits this device there, or `None` when either is not here yet: this
    /// session has no link to its daemon, or no certificate or keys to present.
    fn dial_target(
        &self,
        workspace: WorkspaceId,
        scope: &Scope,
        epoch: u64,
        serving: [u8; 32],
    ) -> Option<(Arc<dyn tilo_live::admit::Dial>, Vec<u8>)> {
        let links = Arc::clone(self.links.get()?);
        let toward = if serving == self.device {
            link::Toward::Here
        } else {
            let relay_id = match EndpointId::from_bytes(&serving) {
                Ok(relay_id) => relay_id,
                Err(error) => {
                    tilo_failure::ignored!(
                        "a device key that names no endpoint cannot be dialed, and the workspace's \
                         next settle reads its state again",
                        tracing::debug!(%workspace, %error,
                            "the workspace's serving device key is no endpoint")
                    );
                    return None;
                }
            };
            // The member this enrollment pinned is reached at the address its host signed, which
            // is its endpoint's, not its device key's.
            let addr = self
                .engine
                .holder_address(workspace)
                .as_ref()
                .filter(|(pinned, _)| *pinned == serving)
                .map_or_else(|| EndpointAddr::new(relay_id), |(_, addr)| addr.clone());
            link::Toward::Relay(addr)
        };
        let grant = match self.grant(workspace, scope, epoch) {
            Ok(grant) => grant,
            Err(error) => {
                tilo_failure::ignored!(
                    "the live_session opens when the workspace's keys and certificate are here, and the \
                     next settle asks again",
                    tracing::debug!(%workspace, %error,
                        "this device has no grant for the workspace's live session")
                );
                return None;
            }
        };
        let dial = link::Daemon::new(move || links(), toward);
        Some((Arc::new(dial), grant))
    }

    /// The grant this device presents to be admitted into `workspace` 's scope: its certificate,
    /// the workspace's uuid bytes and the epoch it seals under.
    fn grant(&self, workspace: WorkspaceId, scope: &Scope, epoch: u64) -> Result<Vec<u8>, String> {
        let certificate = serde_json::to_vec(&self.engine.identity().device_cert)
            .map_err(|error| error.to_string())?;
        grant::Native::new(
            *workspace.as_uuid().as_bytes(),
            &scope.id(),
            epoch,
            certificate,
            Vec::new(),
        )
        .map(|grant| grant.encode())
    }

    /// Whether this device is in `workspace` 's live session now.
    fn joined(&self, workspace: WorkspaceId) -> bool {
        self.live_sessions
            .lock_recover()
            .get(&workspace)
            .is_some_and(|joined| !joined.live_session.ended())
    }

    /// Note how far `workspace` 's live session came, and wake the session when that moved: what it
    /// publishes of the live session is read again. Answers whether it moved.
    fn stand(&self, workspace: WorkspaceId, state: LiveState) -> bool {
        let moved = {
            let mut states = self.states.lock_recover();
            let moved = states.get(&workspace) != Some(&state);
            if moved {
                states.insert(workspace, state);
            }
            moved
        };
        if moved {
            self.moved();
        }
        moved
    }

    /// The relay of `workspace` did not carry its live session for this device, which presented the keys
    /// of `epoch` : the live session stands as waiting for the relay `missed` names, as told by it that it
    /// is busy, or as refused by it, and the journal says so when that is news.
    fn missed(&self, workspace: WorkspaceId, devices: &[Device], missed: Missed, epoch: u64) {
        let relay = relayer(devices, missed.relay, self.device);
        let state = missed.state(relay, epoch);
        if self.stand(workspace, state) {
            tracing::warn!(%workspace, relay = %hex::encode(missed.relay), refused = missed.refused,
                busy = missed.busy, epoch,
                "the live session is not open: no device that relays this workspace took this device in");
        } else {
            tracing::debug!(%workspace, busy = missed.busy,
                "no relay of this workspace was reached");
        }
    }

    /// What wakes `workspace` 's reconciler when a live session of it ended.
    fn nudge(&self, workspace: WorkspaceId) -> Arc<tokio::sync::Notify> {
        Arc::clone(self.nudges.lock_recover().entry(workspace).or_default())
    }

    /// Whether `workspace` names a relay that did not take this device in when it was last settled.
    fn awaits_relay(&self, workspace: WorkspaceId) -> bool {
        matches!(
            self.states.lock_recover().get(&workspace),
            Some(LiveState::Unreached { .. } | LiveState::Busy { .. } | LiveState::Refused { .. })
        )
    }

    /// Stop taking part in `workspace`: its live session is closed.
    fn withdraw(&self, workspace: WorkspaceId) {
        drop(self.live_sessions.lock_recover().remove(&workspace));
    }

    /// A live session of `workspace` ended: settle the workspace again, through its relay.
    fn ended(&self, workspace: WorkspaceId) {
        self.live_sessions.lock_recover().remove(&workspace);
        // Until it is settled again the live session is not open: the relay it ran through is the one it
        // waits to reach.
        let was = self.states.lock_recover().get(&workspace).cloned();
        if let Some(LiveState::Open { relay, .. }) = was {
            self.stand(workspace, LiveState::Unreached { relay });
        }
        self.nudge(workspace).notify_one();
    }
}

/// The endpoints the device's relay turned away from each workspace's live session and has not
/// let in since, by workspace, each as the daemon says it ([`link::TURNED_AWAY`]).
pub(crate) type TurnedAwayLists = BTreeMap<String, Vec<serde_json::Value>>;

/// The live session of one session: the live sessions it joined, on a thread of their own.
pub struct Live {
    shared: Arc<Shared>,
    /// Whom the device's relay turned away, as its daemon publishes it, once followed.
    turned: Option<crate::followed_resource::FollowedReading<TurnedAwayLists>>,
    /// Told when the session ends; dropping it ends the live session's thread.
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

impl Live {
    /// This session's live session. Nothing runs until [`Self::open_links`] hands it the way to
    /// its daemon.
    pub(crate) fn new(engine: Arc<Engine>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let device = engine.identity().device_cert.device_public_key;
        let shared = Arc::new(Shared {
            engine,
            device,
            live_sessions: Mutex::new(BTreeMap::new()),
            states: Mutex::new(BTreeMap::new()),
            moves: std::sync::atomic::AtomicU64::new(0),
            nudges: Mutex::new(BTreeMap::new()),
            readopt: tokio::sync::Notify::new(),
            wake,
            runtime: std::sync::OnceLock::new(),
            lender: std::sync::OnceLock::new(),
            links: std::sync::OnceLock::new(),
        });
        Self {
            shared,
            turned: None,
            stopped: None,
            thread: None,
        }
    }

    /// Show `turned`, whom the device's relay turned away as its daemon publishes it, beside the
    /// live sessions from now on ([`Self::turned_away`]).
    pub(crate) fn follow_turned_away(
        &mut self,
        turned: crate::followed_resource::FollowedReading<TurnedAwayLists>,
    ) {
        self.turned = Some(turned);
    }

    /// The endpoints the device's relay turned away from each workspace's live session and has
    /// not let in since, by workspace: none until its daemon's reading arrived.
    #[must_use]
    pub(crate) fn turned_away(&self) -> TurnedAwayLists {
        self.turned
            .as_ref()
            .map(crate::followed_resource::FollowedReading::get)
            .unwrap_or_default()
    }

    /// Have `lender` lend this device its own streams' media keys in every workspace whose keys
    /// it holds none of, from now on: each such workspace is joined again at once. Only the first
    /// lender counts.
    pub(crate) fn lend_media_keys(&self, lender: Arc<dyn MediaKeysLender>) {
        if self.shared.lender.set(lender).is_err() {
            return;
        }
        // Each workspace's own reconciler settles it again, and a workspace none runs for yet is
        // given one, which settles it at once.
        for workspace in self.shared.engine.workspaces() {
            self.shared.nudge(workspace).notify_one();
        }
        self.shared.readopt.notify_one();
    }

    /// This device came to hold another workspace: its host admitted this device, as the person's
    /// list or an invitation brought it, or this device founded it. Every workspace held here that
    /// has no live session yet is given its reconciler now, which asks for its keys and joins its
    /// relay.
    ///
    /// The session's own passes say so, not the key service: it announces a workspace's keys once
    /// this device asked for them, and the live session is what asks.
    pub(crate) fn workspace_arrived(&self) {
        self.shared.readopt.notify_one();
    }

    /// Run for the rest of the session, opening each workspace's link with `links` on the
    /// daemon's socket: one live session per workspace this device holds. Only the first call
    /// counts.
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

    /// The next object heard in `workspace` 's live session, or `None` .
    #[must_use]
    pub fn try_recv(&self, workspace: WorkspaceId) -> Option<Heard> {
        self.shared
            .live_sessions
            .lock_recover()
            .get(&workspace)
            .and_then(|joined| joined.live_session.try_recv())
    }

    /// The live sessions this device is in, by workspace: what a call's audio is published into, and who
    /// it hears there.
    #[must_use]
    pub fn live_sessions(&self) -> Vec<(WorkspaceId, Arc<LiveSession>)> {
        self.shared
            .live_sessions
            .lock_recover()
            .iter()
            .map(|(workspace, joined)| (*workspace, Arc::clone(&joined.live_session)))
            .collect()
    }

    /// How far each workspace's live session came on this device, by workspace: open, or what it waits
    /// for. A workspace this session has not looked at yet is not among them.
    #[must_use]
    pub fn states(&self) -> Vec<(WorkspaceId, LiveState)> {
        self.shared
            .states
            .lock_recover()
            .iter()
            .map(|(workspace, state)| (*workspace, state.clone()))
            .collect()
    }

    /// Moves whenever a state did, a live session opened or whom the relay turned away did:
    /// [`Self::states`], [`Self::live_sessions`] and [`Self::turned_away`] say nothing new while
    /// it stands still.
    #[must_use]
    pub fn states_revision(&self) -> u64 {
        let turned = self
            .turned
            .as_ref()
            .map_or(0, crate::followed_resource::FollowedReading::revision);
        self.shared
            .moves
            .load(std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(turned)
    }

    /// The runtime the live sessions run on, once it started: a live session asked to show a track starts its
    /// reader there.
    #[must_use]
    pub fn runtime(&self) -> Option<tokio::runtime::Handle> {
        self.shared.runtime.get().cloned()
    }

    /// A handle that says this live session's presence frames, for the transport the session says on.
    pub(crate) fn saying(&self) -> Say {
        Say(Arc::clone(&self.shared))
    }
}

/// This live session's presence track, as the transport the session says on carries it: the words this
/// device says of itself, said in the workspace they are about.
pub(crate) struct Say(Arc<Shared>);

impl std::fmt::Debug for Say {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("live session")
    }
}

impl crate::sync::SayPresence for Say {
    fn say(
        &self,
        workspace: WorkspaceId,
        frames: &[(u32, collab_types::presence::Presence)],
    ) -> Result<(), String> {
        let live_session = self
            .0
            .live_sessions
            .lock_recover()
            .get(&workspace)
            .map(|joined| Arc::clone(&joined.live_session))
            .ok_or_else(|| "this workspace has no live session".to_owned())?;
        let wanted = live_session.call_keys_wanted();
        tilo_live::presence::say(&live_session, frames).map_err(|error| error.to_string())?;
        // A device that joined another call needs that call's keys: the workspace is settled
        // again, which asks for them.
        if live_session.call_keys_wanted() != wanted {
            self.0.nudge(workspace).notify_one();
        }
        Ok(())
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

/// The live session's thread: one reconciler per workspace, each woken by the channel service when
/// the workspace moved.
fn run(shared: Arc<Shared>, mut stopped: tokio::sync::mpsc::UnboundedReceiver<()>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        tracing::warn!("the live session could not start");
        return;
    };
    let _ = shared.runtime.set(runtime.handle().clone());
    runtime.block_on(async move {
        tracing::debug!(device = %hex::encode(shared.device),
            "the live session runs through this device's daemon");
        let keys = shared.engine.keys();
        let mut keyed = keys.generation();
        let mut known = BTreeSet::new();
        adopt(&shared, &mut known);
        // A workspace this session joins later opens its live session the same way: the session says
        // when its host admitted this device ([`Live::workspace_arrived`]), its channel keys
        // arriving is announced by the channel service, and the workspace list is read again then.
        // Each workspace's own reconciler settles it again when a live session of it ended, or when no
        // relay took this device in ([`reconcile`]).
        loop {
            tokio::select! {
                _ = stopped.recv() => break,
                changed = keys.changed(keyed) => {
                    keyed = changed;
                    adopt(&shared, &mut known);
                }
                () = shared.readopt.notified() => {
                    adopt(&shared, &mut known);
                }
            }
        }
    });
}

/// Start a reconciler for every workspace of `shared` this session has not opened a live session for yet.
fn adopt(shared: &Arc<Shared>, known: &mut BTreeSet<WorkspaceId>) {
    for workspace in shared.engine.workspaces() {
        if known.insert(workspace) {
            tokio::spawn(reconcile(Arc::clone(shared), workspace));
        }
    }
}

/// Keep one workspace's live session in step with its membership and keys, on the event that moved them:
/// the workspace's media channel changed, or a live session of the workspace ended.
///
/// One thing has no event. A relay that was away when this device asked, or that did not know this
/// device yet, says nothing when that changes, and the keys this device already holds do not move
/// because of it. So while the workspace names a relay and none took this device in, its relays
/// are asked again after [`retry_wait`], until the live session opens. Without that a device whose session
/// started before the device relaying the workspace was reachable stayed out of the workspace's
/// live session for as long as both ran: no presence, no call and no shared window crossed, and
/// nothing said so.
async fn reconcile(shared: Arc<Shared>, workspace: WorkspaceId) {
    let keys = shared.engine.keys();
    let nudge = shared.nudge(workspace);
    let mut after = 0;
    let mut missed: u32 = 0;
    loop {
        Arc::clone(&shared).settle(workspace).await;
        missed = if shared.awaits_relay(workspace) {
            missed.saturating_add(1)
        } else {
            0
        };
        let again = async {
            if missed == 0 {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(retry_wait(shared.device, workspace, missed)).await;
        };
        tokio::select! {
            changed = keys.workspace_changed(workspace, after) => after = changed,
            () = nudge.notified() => {}
            () = again => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    fn cert(device: [u8; 32], expires_at_ns: Option<u64>) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: device,
            device_label: format!("device {}", device[0]),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        }
    }

    fn device(device: [u8; 32], expires_at_ns: Option<u64>) -> Device {
        Device {
            device,
            cert: cert(device, expires_at_ns),
        }
    }

    tilo_test_env::scenario! {
        fn a_workspaces_relay_is_the_device_its_state_names;

        phase the_device_serving_the_workspace_relays;
        {
            let devices = vec![device([1; 32], None), device([2; 32], None)];
            assert_eq!(relay_of(&devices, Some([2; 32]), 10), Some([2; 32]));
            assert!(
                relay_of(&devices, None, 10).is_none(),
                "a workspace nobody serves has no live session"
            );
            assert!(
                relay_of(&devices, Some([7; 32]), 10).is_none(),
                "a device that is no member relays nothing"
            );
            let devices = vec![device([1; 32], Some(10)), device([2; 32], None)];
            assert!(relay_of(&devices, Some([1; 32]), 10).is_none());
        }

        phase a_relay_is_named_as_its_certificate_names_it;
        // A relay is named by what its person called it, as the workspace's certificate of it says,
        // and a device they never named by nothing.
        {
            let devices = vec![device([1; 32], None), device([2; 32], None)];
            assert_eq!(
                relayer(&devices, [2; 32], [1; 32]),
                Relayer {
                    device: [2; 32],
                    name: "device 2".to_owned(),
                    here: false
                }
            );
            assert!(relayer(&devices, [1; 32], [1; 32]).here);
            assert_eq!(relayer(&devices, [7; 32], [1; 32]).name, "");
        }

    }

    tilo_test_env::scenario! {
        fn a_relay_that_did_not_admit_says_why_and_is_asked_again;

        phase a_relay_that_refuses_is_told_apart_from_one_that_is_away;
        // A relay that answers and refuses this device is told apart from one that was not reached:
        // waiting reaches the one, and changes nothing about the other. A relay that was admitting as
        // many devices as it takes at once is neither: it judged nobody, so it is no refusal.
        {
            assert_eq!(
                Missed::of([2; 32], &LiveError::Denied),
                Missed {
                    relay: [2; 32],
                    refused: true,
                    busy: false
                }
            );
            assert_eq!(
                Missed::of([1; 32], &LiveError::Unavailable),
                Missed {
                    relay: [1; 32],
                    refused: false,
                    busy: false
                },
                "the relay that was away is the one the live session waits to reach"
            );
            assert_eq!(
                Missed::of([3; 32], &LiveError::Busy),
                Missed {
                    relay: [3; 32],
                    refused: false,
                    busy: true
                },
                "a relay that had no room judged nobody: it is asked again, and refused nobody"
            );
        }

        phase a_relay_that_was_missed_is_asked_again_ever_less_often_and_always;
        // A workspace whose relay did not take this device in asks it again soon, then ever less
        // often, and never stops asking: the relay it waits for may come back at any time.
        {
            assert_eq!(retry_schedule(1), RETRY_FIRST);
            assert_eq!(retry_schedule(2), RETRY_FIRST * 2);
            assert_eq!(retry_schedule(3), RETRY_FIRST * 4);
            let waits: Vec<Duration> = (1..40).map(retry_schedule).collect();
            assert!(waits.windows(2).all(|pair| pair[0] <= pair[1]), "{waits:?}");
            assert_eq!(waits.last(), Some(&RETRY_LONGEST));
            assert_eq!(retry_schedule(u32::MAX), RETRY_LONGEST);
            assert_eq!(retry_schedule(0), RETRY_FIRST);
        }

        phase devices_that_missed_a_relay_together_do_not_ask_it_again_together;
        // The devices of a workspace that missed its relay in one moment do not ask it again in one
        // moment: each takes up to a quarter off the schedule's wait, by its own key. The schedule
        // keeps its shape for every one of them: each wait is longer than the one before it, until
        // the longest, and none is longer than the schedule's.
        {
            let workspace = WorkspaceId::new();
            for missed in 1..=6 {
                let schedule = retry_schedule(missed);
                let waits: BTreeSet<Duration> = (0..64_u8)
                    .map(|device| retry_wait([device; 32], workspace, missed))
                    .collect();
                assert!(
                    waits
                        .iter()
                        .all(|wait| *wait <= schedule
                            && *wait >= schedule.saturating_sub(schedule / 4)),
                    "after {missed} misses every device waits within a quarter of {schedule:?}: \
                 {waits:?}"
                );
                assert!(
                    waits.len() > 56,
                    "sixty-four devices wait {} different times after {missed} misses",
                    waits.len()
                );
            }
            for device in 0..64_u8 {
                let waits: Vec<Duration> = (1..=5)
                    .map(|missed| retry_wait([device; 32], workspace, missed))
                    .collect();
                assert!(
                    waits.windows(2).all(|pair| pair[0] < pair[1]),
                    "a device waits longer each time: {waits:?}"
                );
                assert_eq!(
                    waits,
                    (1..=5)
                        .map(|missed| retry_wait([device; 32], workspace, missed))
                        .collect::<Vec<_>>(),
                    "a device's own waits are the same whenever it is asked"
                );
            }
            // One device in two workspaces asks each at a time of its own.
            assert_ne!(
                retry_wait([1; 32], workspace, 1),
                retry_wait([1; 32], WorkspaceId::new(), 1)
            );
        }

        phase a_busy_relay_stands_as_busy_and_never_as_a_refusal;
        // A relay that had no room stands as busy, by name, and never as refused or as not reached:
        // the three are told apart from the relay's answer alone.
        {
            let relay = Relayer {
                device: [3; 32],
                name: "device 3".to_owned(),
                here: false,
            };
            assert_eq!(
                Missed::of([3; 32], &LiveError::Busy).state(relay.clone(), 4),
                LiveState::Busy {
                    relay: relay.clone()
                }
            );
            assert_eq!(
                Missed::of([3; 32], &LiveError::Unavailable).state(relay.clone(), 4),
                LiveState::Unreached {
                    relay: relay.clone()
                }
            );
            assert_eq!(
                Missed::of([3; 32], &LiveError::Denied).state(relay.clone(), 4),
                LiveState::Refused { relay, epoch: 4 }
            );
        }

    }
}
