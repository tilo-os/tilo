//! End-to-end encryption of collaboration channels on this device: one MLS group per channel, whose
//! members are exactly the devices the workspace's access policy lets read the route
//! ([`ChannelDirectory::allowed_devices`]), and every participant commits the group to that policy.
//!
//! [`ChannelEncryption::pump`] runs the protocol over the workspace transport: inbound frames
//! advance the group and access requests wait for the next commit, frames the transport did not take
//! are published again, and two geneses made at once resolve to one without losing what was sealed
//! under the other ([`channel_crypto::peer`]). A device that cannot name a channel's record cannot
//! tell who may read it, so it commits nothing there and only follows participants that can.
//!
//! Every change registers the channel's gate ([`ChannelGates`]: plaintext and another channel's
//! ciphertext are refused on every ingress path) and is announced
//! ([`ChannelKeyProvider::channel_changed`]), so an endpoint holding a route's keys asks the identity
//! service again only when they change.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_clock::time::Instant;

use channel_crypto::{
    Access, ChannelContentKeys, ChannelPeer, PeerEvent, VaultKey, WelcomeRefusal,
};
use collab_types::signer::{DeviceSigner, SignFuture};
use collab_types::{
    ChannelId, ChannelRoute, Ed25519PublicKey, EncryptionMetadata, MlsAccessRequestV1,
    MlsGovernanceFrame, SignedOp,
};
use neighbour_wire::NeighbourMsg;

use crate::gates::ChannelGates;
use crate::schedule::{PolicySchedule, RECHECK_INTERVAL};
use tilo_desktop_model::WorkspaceId;

/// How often groups re-check the access policy when their directory
/// announces no changes, the longest an announced change waits, and how soon
/// a pass that left work due tries again ([`crate::schedule`]).
const RECONCILE_INTERVAL: Duration = RECHECK_INTERVAL;
/// First and longest wait before a waiting device asks again.
const REQUEST_FIRST: Duration = Duration::from_secs(2);
const REQUEST_LONGEST: Duration = Duration::from_secs(120);
/// How long a waiting device asks with one member key before it asks with
/// a fresh one: an admission whose Welcome never reached it is then answered
/// again.
const REQUEST_FRESH: Duration = Duration::from_secs(60);
/// How long each member before this one in turn ([`ChannelPeer::turn`]) has
/// to commit a change of a channel's group before this one does: long enough
/// for one member's commit to reach the others, so they follow it rather than
/// commit the same change beside it and fork the group.
const TURN: Duration = Duration::from_secs(2);
/// The least time between two commits this device writes for one channel.
/// Every member follows each commit, and in a group of hundreds a commit costs
/// each of them work that grows with the group: people arriving within this
/// of each other are admitted in one commit rather than one each. A change
/// after a quiet spell is committed at once.
const COMMIT_SPACING: Duration = Duration::from_millis(500);
/// How long the state a frame taken in left changed waits to be written
/// ([`ChannelPeer::save_deferred`]). Sealing the state of a channel of hundreds
/// takes tens of milliseconds, and the Welcome that admits a page arrives as it
/// enters: written at once, it would hold back the page's first frame, which is
/// drawn well within this. A device that stops before only asks again.
const DEFERRED_SAVE: Duration = Duration::from_secs(2);
/// Under the state root, the directory of a workspace's channels.
#[cfg(not(target_arch = "wasm32"))]
const CHANNELS_DIR: &str = "channels";
/// Channels remembered as having a group this device takes no part in.
const MAX_GROUPS_SEEN: usize = 1 << 16;
/// How long a waiting device holds a Welcome whose tree names a device its
/// view of the membership does not, judging it again as that view changes,
/// before it gives up and asks with a fresh member key. Such a tree is an
/// honest one whenever this device's view is behind the committer's (a device
/// admitted a moment ago) or ahead of it (one removed a moment ago), and
/// asking again costs the whole group a commit: a lag of a moment is waited
/// out, and only a view that does not come round in this long, or a Welcome
/// the channel moved past, is given up on.
const WELCOME_WAIT: Duration = Duration::from_secs(10);
/// Frames one channel keeps for the policy that has not named their signer
/// yet. Bounded, newest kept: a frame this device's policy still does not
/// back after that many later ones is not going to be applied.
const MAX_HELD_FRAMES: usize = 8;

/// The widest a device's wait to create a channel's group spreads past
/// [`REQUEST_FIRST`], in milliseconds: devices that hold no state for the same
/// channel wait within these of each other, so one creates the group and the
/// others hear of it in time.
const FOUNDING_SPREAD_MS: u64 = 2_000;

/// The longest a device that holds no state for a channel waits for its genesis
/// (from the workspace's retained frames, or a participant answering its
/// request) before it creates the group itself: [`REQUEST_FIRST`] plus the
/// widest spread between devices. Whoever waits for a fresh channel's keys
/// waits longer than this, or it gives up on a group about to be created.
#[must_use]
pub const fn founding_wait() -> Duration {
    REQUEST_FIRST.saturating_add(Duration::from_millis(FOUNDING_SPREAD_MS))
}

/// How long a device that took a channel up for another one that asked
/// ([`PeerEntry::wanted`]) waits to create its group, once no genesis is on
/// its way: the asker creates it at once if it can, this one only for an
/// asker that cannot (a reader). Each device waits its own while, set by its
/// key and the channel, so one creates the group and the others hear of it.
fn found_after(device: &Ed25519PublicKey, channel: ChannelId) -> Duration {
    let spread =
        device
            .as_bytes()
            .iter()
            .zip(channel.0.as_bytes())
            .fold(0_u64, |acc, (device, channel)| {
                acc.wrapping_mul(31)
                    .wrapping_add(u64::from(device ^ channel))
            })
            % FOUNDING_SPREAD_MS;
    REQUEST_FIRST.saturating_add(Duration::from_millis(spread))
}

impl PeerEntry {
    /// Keep a Welcome `event` refused for a device the view `allowed` did not
    /// name (`frame`, kept while this device waited), judged again as that
    /// view changes; or give up on the one held once a later commit (at
    /// `epoch`) arrived, as a held Welcome is followed by nothing but its own
    /// commit: the members moved past it, and this device asks again to be
    /// admitted where they are. Whether this device now asks again.
    fn hold_welcome(
        &mut self,
        event: &PeerEvent,
        frame: Option<MlsGovernanceFrame>,
        allowed: Option<BTreeSet<Ed25519PublicKey>>,
        epoch: u64,
        store: &ChannelEncryptionConfig,
        now: Instant,
    ) -> bool {
        if let (PeerEvent::WelcomeRefused(WelcomeRefusal::DeviceNotAllowed), Some(frame)) =
            (event, frame)
        {
            let until = self
                .welcome
                .as_ref()
                .filter(|held| held.frame == frame)
                .map_or_else(
                    || now.checked_add(WELCOME_WAIT).unwrap_or(now),
                    |held| held.until,
                );
            self.welcome = Some(HeldWelcome {
                frame,
                judged: allowed,
                until,
            });
            return false;
        }
        if !self
            .welcome
            .as_ref()
            .is_some_and(|held| held.frame.epoch < epoch || self.peer.access() != Access::Waiting)
        {
            return false;
        }
        self.welcome = None;
        if self.peer.access() != Access::Waiting {
            return false;
        }
        give_up_welcome(self, store, now);
        true
    }
}

/// Give up on the Welcome `entry` held for its view of the membership: wait
/// with a fresh member key and ask with it at once, as a desynchronised device
/// does.
fn give_up_welcome(entry: &mut PeerEntry, store: &ChannelEncryptionConfig, now: Instant) {
    entry.welcome = None;
    if let Err(error) = entry.peer.refuse_welcome(&store.vault) {
        tracing::warn!(%error, workspace = %entry.workspace, "channel state could not be written");
    }
    entry.request = None;
    entry.retry = Backoff::now(now, REQUEST_FIRST);
    entry.fresh_at = now;
}

/// What an application endpoint may do with a route's content right now.
#[derive(Clone, Debug)]
pub enum ChannelAccess {
    /// Admitted: seal new content and open history with these keys.
    Ready(ChannelContentKeys),
    /// Asked the participants for access and not admitted yet. Nothing can
    /// be sealed or opened; the endpoint shows a waiting state.
    Waiting,
    Removed,
    /// Nobody can admit this device: the workspace's governance is unknown here.
    Unavailable,
    /// This device has no secret store open, so it keeps no channel state
    /// of its own and cannot join a channel. Asking again changes nothing
    /// while the session runs.
    NoSecretStore,
}

/// A channel governance message received from the workspace transport. Every
/// frame and request is signed by a device key and verified by the channel
/// service before it changes any state.
#[derive(Debug, Clone)]
pub enum InboundChannelGovernance {
    Frame {
        workspace: WorkspaceId,
        frame: Box<collab_types::MlsGovernanceFrame>,
    },
    /// A device asking a channel's participants for membership, with the
    /// route of the channel's record when the device sent it
    /// ([`NeighbourMsg::MlsRoutedAccessRequest`]).
    AccessRequest {
        workspace: WorkspaceId,
        request: Box<collab_types::MlsAccessRequestV1>,
        route: Option<Box<ChannelRoute>>,
    },
    /// The workspace's host carries no genesis of `channel`, which `device`
    /// asked for ([`NeighbourMsg::MlsChannelUnfounded`]).
    Unfounded {
        workspace: WorkspaceId,
        channel: ChannelId,
        device: Ed25519PublicKey,
    },
}

impl InboundChannelGovernance {
    /// The governance `message` carries, if it is one for a valid workspace.
    #[must_use]
    pub fn from_message(message: NeighbourMsg) -> Option<Self> {
        match message {
            NeighbourMsg::MlsGovernance {
                workspace, frame, ..
            } => WorkspaceId::parse(&workspace).map(|workspace| Self::Frame { workspace, frame }),
            NeighbourMsg::MlsAccessRequest {
                workspace, request, ..
            } => WorkspaceId::parse(&workspace).map(|workspace| Self::AccessRequest {
                workspace,
                request,
                route: None,
            }),
            NeighbourMsg::MlsRoutedAccessRequest {
                room: workspace,
                request,
                route,
                ..
            } => WorkspaceId::parse(&workspace).map(|workspace| Self::AccessRequest {
                workspace,
                request,
                route: Some(route),
            }),
            NeighbourMsg::MlsChannelUnfounded {
                workspace,
                channel,
                device,
            } => WorkspaceId::parse(&workspace).map(|workspace| Self::Unfounded {
                workspace,
                channel,
                device,
            }),
            _ => None,
        }
    }
}

/// Where the channel protocol's messages travel: the workspace transport of the
/// host (the desktop session's network, the runtime's relay sessions).
pub trait GovernanceTransport {
    /// Hand one message to the transport. `false` when it was not accepted.
    fn publish(&self, message: NeighbourMsg) -> bool;
    /// Take every message received since the last call.
    fn take_inbound(&self) -> Vec<InboundChannelGovernance>;
}

/// The access policy inputs this service needs from the workspace model.
pub trait ChannelDirectory: collab_types::signer::MaybeSendSync {
    /// Every device that may read `channel`, one of `workspace`'s channels, at
    /// wall-clock `now` (seconds since the Unix epoch). A channel is one
    /// record's, so a Read Deny on that record, on its app or on any pattern
    /// covering it takes its person's devices out of this channel and no
    /// other: no participant then hands them the record's key, and a device
    /// already holding it is removed in a new epoch.
    ///
    /// `route` is the channel's record when this device knows it: proven
    /// against the channel ([`ChannelRoute::proves`]), from a device's routed
    /// request or this device's own endpoint. Without it the directory names
    /// the record only from grants naming it. `None` for a channel it cannot
    /// name: it may be a member's own record, so who reads it is unknown, and
    /// this device admits and removes no one.
    ///
    /// A device takes part in a channel exactly when this names it: a
    /// workspace's host has no place in its channels that its membership
    /// does not give it.
    ///
    /// A personal cluster ([`collab_types::personal_cluster`]) has no record grants:
    /// its channels are read by the devices of the identity it derives from
    /// (certified, not revoked), and a directory holding that identity
    /// answers them for every channel. A channel is then taken up once the
    /// directory can name its readers.
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>>;
    /// The key scope the storage unit on `route` seals under in `workspace`:
    /// [`collab_types::workspace_scope`] while every device that may read the
    /// workspace may read the record too, the record's own channel when the
    /// policy narrows its readers ([`Self::allowed_devices`] then names the
    /// narrower set for that channel). The unit's keys come from this scope's
    /// group; content stays bound to the unit's own channel.
    ///
    /// `None` (the default) when this directory cannot decide it: the
    /// workspace's governance is unknown, and a guessed scope could hand a
    /// device keys the record's policy withholds.
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        now: u64,
    ) -> Option<ChannelId> {
        let _ = (workspace, route, now);
        None
    }
    /// The [`tilo_identity_host::policy_change`] epoch (or a test's own) when
    /// every write to what [`Self::allowed_devices`] reads is announced and
    /// its expiries can be timed: groups then re-check on announcements and
    /// at [`Self::next_policy_change`]. `None` (the default) keeps the
    /// re-check every [`RECHECK_INTERVAL`].
    fn policy_epoch(&self) -> Option<u64> {
        None
    }
    /// The first wall-clock second after `now` at which
    /// [`Self::allowed_devices`] for `workspace` can answer differently with
    /// nothing written: a grant or a certificate expiring, a revocation
    /// dated ahead. Read only while [`Self::policy_epoch`] is `Some`.
    fn next_policy_change(&self, workspace: WorkspaceId, now: u64) -> Option<u64> {
        let _ = (workspace, now);
        None
    }
    /// Announce a policy change when the wall clock reaches `at` (seconds
    /// since the Unix epoch), an instant [`Self::next_policy_change`] named.
    fn announce_at(&self, at: u64) {
        tilo_identity_host::policy_change::wake_at(at.saturating_mul(1_000_000_000));
    }
    /// Whether this device hosts `workspace`: every genesis of its channels
    /// passes through it, so it creates a group it holds none of as soon as
    /// it needs it. A device of a workspace hosted elsewhere creates one only
    /// once the host said it carries no genesis of the channel
    /// ([`InboundChannelGovernance::Unfounded`]). `true` by default.
    fn hosts(&self, workspace: WorkspaceId) -> bool {
        let _ = workspace;
        true
    }
}

/// The platform secret store's namespace holding a device's channel vault
/// master key: the key the sealed channel state of every workspace is
/// derived from. A host keeps it the way it keeps its other secrets — the
/// system keychain on a desktop, a non-extractable `WebCrypto` key in a
/// page — so the state's own store never holds the key that opens it.
pub const VAULT_SECRET_APP: &str = "so.tilo.collaboration.mls";

/// Where a host keeps this device's channel state: the peers of each
/// workspace's channels and the route of each channel's record. A desktop keeps
/// it in its channels directory ([`ChannelsDir`]); a page keeps it in its store.
pub trait ChannelState: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Make the store ready for the state a channel writes: a directory that
    /// pays for it is created owner-only here, so a write cannot land in one a
    /// person's umask left open. A store that always exists answers nothing.
    ///
    /// # Errors
    /// The store cannot be created.
    fn prepare(&self) -> Result<(), String> {
        Ok(())
    }
    /// The peers of `workspace`'s channels.
    fn peers(&self, workspace: WorkspaceId) -> Arc<dyn channel_crypto::PeerStates>;
    /// The route kept for `channel`, only if it still proves to be `channel`'s
    /// in `workspace`: a damaged or swapped route names no record rather than a
    /// wrong one, and the channel is then decided as one whose record is
    /// unknown, which withholds more, never less.
    fn route(&self, workspace: WorkspaceId, channel: ChannelId) -> Option<ChannelRoute>;
    /// Keep `route` as `channel`'s in `workspace`, when this device takes part
    /// in it.
    fn keep_route(&self, workspace: WorkspaceId, channel: ChannelId, route: &ChannelRoute);
    /// Every channel this store holds peers for, so a service registers their
    /// gates before the first op after a restart. A store that cannot list
    /// them answers none: its channels open as an endpoint asks about them.
    fn held(&self) -> Vec<(WorkspaceId, Vec<ChannelId>)> {
        Vec::new()
    }
    /// Prepare the vault this store seals, and say whether one was already kept
    /// here: a vault whose master key is gone refuses a new one, since that key
    /// opens none of the state sealed under the old. A store that keeps no
    /// marker of its own answers `false`.
    ///
    /// # Errors
    /// The store cannot be prepared for a vault.
    fn open_vault(&self) -> Result<bool, String> {
        Ok(false)
    }
    /// Forget everything kept of `workspace`'s channels: it was deleted. A
    /// store that keeps nothing per workspace forgets nothing.
    ///
    /// # Errors
    /// What it kept could not be removed.
    fn forget(&self, workspace: WorkspaceId) -> Result<(), String> {
        let _ = workspace;
        Ok(())
    }
}

/// A desktop's channels directory, with the layout every earlier build wrote: a
/// directory per workspace under the state root, one `.peer` and one `.route`
/// file per channel in it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct ChannelsDir {
    /// Where the state of every workspace's channels lives.
    state: PathBuf,
    /// Where this device's vault lives, when this directory seals it: the
    /// marker and the files of a retired lifecycle authority beside it.
    vault: Option<PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ChannelsDir {
    /// The state kept under `root`, one directory per workspace.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            state: root.into(),
            vault: None,
        }
    }

    /// This directory also seals the vault at `root`: its marker, and what
    /// earlier builds left beside it.
    #[must_use]
    pub fn with_vault(mut self, root: impl Into<PathBuf>) -> Self {
        self.vault = Some(root.into());
        self
    }

    /// The directory of `workspace`'s channels.
    fn workspace_dir(&self, workspace: WorkspaceId) -> PathBuf {
        self.state.join(workspace.to_string()).join(CHANNELS_DIR)
    }

    fn route_path(&self, workspace: WorkspaceId, channel: ChannelId) -> PathBuf {
        self.workspace_dir(workspace)
            .join(format!("{}.route", channel.0))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ChannelState for ChannelsDir {
    fn prepare(&self) -> Result<(), String> {
        tilo_files::native::atomic::create_private_directory(&self.state)
            .map_err(|error| error.to_string())
    }

    fn peers(&self, workspace: WorkspaceId) -> Arc<dyn channel_crypto::PeerStates> {
        Arc::new(channel_crypto::Files::new(self.workspace_dir(workspace)))
    }

    fn forget(&self, workspace: WorkspaceId) -> Result<(), String> {
        match std::fs::remove_dir_all(self.state.join(workspace.to_string())) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        }
    }

    fn route(&self, workspace: WorkspaceId, channel: ChannelId) -> Option<ChannelRoute> {
        let path = self.route_path(workspace, channel);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "channel route cannot be read");
                return None;
            }
        };
        let route = match postcard::from_bytes::<ChannelRoute>(&bytes) {
            Ok(route) => route,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "channel route does not decode");
                return None;
            }
        };
        if !route.proves(&workspace.to_string(), channel) {
            tracing::warn!(path = %path.display(), "channel route names another channel");
            return None;
        }
        Some(route)
    }

    fn keep_route(&self, workspace: WorkspaceId, channel: ChannelId, route: &ChannelRoute) {
        let path = self.route_path(workspace, channel);
        let saved = postcard::to_allocvec(route)
            .map_err(std::io::Error::other)
            .and_then(|bytes| tilo_files::native::atomic::write_private(&path, &bytes));
        if let Err(error) = saved {
            tracing::warn!(%error, "channel route could not be saved");
        }
    }

    fn held(&self) -> Vec<(WorkspaceId, Vec<ChannelId>)> {
        let entries = match std::fs::read_dir(&self.state) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(dir = %self.state.display(), %error, "channel state cannot be listed");
                return Vec::new();
            }
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let workspace = entry.file_name().to_str().and_then(WorkspaceId::parse)?;
                Some((workspace, self.channels_in(workspace)))
            })
            .filter(|(_, channels)| !channels.is_empty())
            .collect()
    }

    fn open_vault(&self) -> Result<bool, String> {
        let Some(root) = &self.vault else {
            return Ok(false);
        };
        // `registry.postcard` in the layout earlier builds wrote marks a vault whose master key
        // must already be in the secret store. It stays as the marker; its entries named the group,
        // key-package and snapshot-receipt records of a retired lifecycle authority, which nothing
        // reads.
        let registry = root.join(REGISTRY);
        let existed = registry.exists();
        if existed && std::fs::read(&registry).is_ok_and(|bytes| bytes != EMPTY_REGISTRY) {
            let cleared =
                tilo_files::native::atomic::write_with_mode(&registry, &EMPTY_REGISTRY, 0o600)
                    .map_err(|error| error.to_string());
            if let Err(error) = cleared {
                tracing::warn!(%error, "retired MLS registry records could not be cleared");
            }
        }
        remove_retired_files(root);
        Ok(existed)
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// `registry.postcard` at a vault's root: the marker that the vault's master key must already be
/// in the secret store.
const REGISTRY: &str = "registry.postcard";

/// An empty registry: four empty maps, the bytes of the marker with its retired records gone.
#[cfg(not(target_arch = "wasm32"))]
const EMPTY_REGISTRY: [u8; 4] = [0; 4];

/// Remove what the retired MLS lifecycle authority and its snapshot receipts left in `root` : the
/// authority's ledger directory, every compaction envelope copied to `snapshot-*.op` , and the
/// `group-*.mls` and `key-package-*.mls` files its registry named. Nothing reads them; the sealed
/// channel state lives under `channels/` and is untouched. Best-effort.
#[cfg(not(target_arch = "wasm32"))]
fn remove_retired_files(root: &std::path::Path) {
    let authority = root.join("authority");
    if std::fs::symlink_metadata(&authority).is_ok()
        && let Err(error) = std::fs::remove_dir_all(&authority)
    {
        tracing::warn!(%error, "retired MLS authority ledger could not be removed");
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_retired_file(&path)
            && entry.file_type().is_ok_and(|kind| kind.is_file())
            && let Err(error) = std::fs::remove_file(&path)
        {
            tracing::warn!(%error, "retired MLS vault file could not be removed");
        }
    }
}

/// A top-level file only the retired authority wrote: `snapshot-*.op`, `group-*.mls` or
/// `key-package-*.mls`.
#[cfg(not(target_arch = "wasm32"))]
fn is_retired_file(path: &std::path::Path) -> bool {
    let (Some(stem), Some(extension)) = (
        path.file_stem().and_then(|stem| stem.to_str()),
        path.extension().and_then(|extension| extension.to_str()),
    ) else {
        return false;
    };
    match extension {
        "op" => stem.starts_with("snapshot-"),
        "mls" => stem.starts_with("group-") || stem.starts_with("key-package-"),
        _ => false,
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ChannelsDir {
    /// Channel ids of the `*.peer` files of `workspace`. A file whose name is no
    /// channel id is logged and skipped.
    fn channels_in(&self, workspace: WorkspaceId) -> Vec<ChannelId> {
        let dir = self.workspace_dir(workspace);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(dir = %dir.display(), %error, "channel state cannot be listed");
                return Vec::new();
            }
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("peer") {
                    return None;
                }
                let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
                match uuid::Uuid::parse_str(stem) {
                    Ok(channel) => Some(ChannelId(channel)),
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "channel state file names no channel");
                        None
                    }
                }
            })
            .collect()
    }
}

/// Durable location and keys of this device's own channel state: what a
/// participant of a channel needs, and only workspaces this device's own workspace
/// host does not take part in need it.
pub struct ChannelEncryptionConfig {
    /// Where this host keeps each workspace's channel state.
    pub state: Arc<dyn ChannelState>,
    /// Key sealing that state, held by the platform secret store.
    pub vault: VaultKey,
    /// This device's certified key, kept in its custody: the request and frame signer.
    pub signer: Arc<dyn DeviceSigner>,
    /// Whether this device settles its groups or only follows them.
    pub role: Role,
}

/// What a device does in the groups it takes part in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Founds groups and commits them to the access policy.
    Participant,
    /// Asks for access and follows the commits participants write, but never founds a group or
    /// commits: a page or an agent, whose device key signs asynchronously and which may not hold
    /// the grants a commit is decided on.
    Reader,
}

#[derive(Clone, Copy, Debug)]
struct Backoff {
    due: Instant,
    delay: Duration,
}

impl Backoff {
    fn now(now: Instant, first: Duration) -> Self {
        Self {
            due: now,
            delay: first,
        }
    }

    fn is_due(&self, now: Instant) -> bool {
        now >= self.due
    }

    fn fired(&mut self, now: Instant, longest: Duration) {
        self.due = now.checked_add(self.delay).unwrap_or(now);
        self.delay = self.delay.saturating_mul(2).min(longest);
    }
}

/// When this device may create a channel's group itself.
struct Founding {
    /// When a device that took the channel up for another one creates the
    /// group, if no genesis has arrived.
    at: Instant,
    /// Whether the policy, when last read, let this device read the channel
    /// and so create its group.
    allowed: bool,
    /// The workspace's host said it carries no genesis of the channel...
    unfounded: bool,
    /// ...in answer to this device's own request.
    answered: bool,
}

/// One channel this device takes part in.
struct PeerEntry {
    workspace: WorkspaceId,
    peer: ChannelPeer,
    /// The channel's record, once proven: decides Read on the record, and
    /// goes with every access request so other participants can too.
    route: Option<ChannelRoute>,
    /// The request this device asks with while it waits, re-sent on `retry`.
    request: Option<MlsAccessRequestV1>,
    /// A request whose signature the device's custody has not answered yet.
    signing: Option<(MlsAccessRequestV1, SignFuture<'static>)>,
    retry: Backoff,
    /// When the next request mints a fresh member key: an admission whose
    /// Welcome never arrived here is then answered again.
    fresh_at: Instant,
    /// The group may differ from the policy: reconcile on the next pass.
    dirty: bool,
    /// The group differs from the policy and other members come before this
    /// one in turn to commit the change: it commits at this instant if none
    /// of them has by then.
    turn_at: Option<Instant>,
    /// When this device last committed a change of the channel's group: the
    /// next waits for [`COMMIT_SPACING`] after it.
    committed_at: Option<Instant>,
    /// An endpoint opened the route after this device was removed: ask
    /// again, and the policy decides whether access resumes.
    rejoin: bool,
    founding: Founding,
    /// An endpoint of this device opened the route. A channel taken up only
    /// because another device asked is left once a genesis arrives.
    wanted: bool,
    /// Frames this device's policy does not back yet, oldest first. The
    /// workspace's governance reaches a device as the workspace does, so a
    /// frame from a device admitted a moment ago is judged again once the
    /// policy here names it.
    held: Vec<MlsGovernanceFrame>,
    /// A Welcome refused for a device this device's view of the membership
    /// did not name ([`WELCOME_WAIT`]).
    welcome: Option<HeldWelcome>,
    /// Since when the peer holds a change it has not written
    /// ([`DEFERRED_SAVE`]).
    unsaved_since: Option<Instant>,
}

/// A Welcome a waiting device holds for its view of the membership.
struct HeldWelcome {
    frame: MlsGovernanceFrame,
    /// The devices the view named when it last refused the Welcome: it is
    /// judged again once the view names others.
    judged: Option<BTreeSet<Ed25519PublicKey>>,
    /// When the device gives up on it.
    until: Instant,
}

/// A channel an endpoint asked about that this device could not open: no
/// one can name its readers here, or opening its state failed. Its answer
/// changes with nothing arriving for it, so the service tells the route itself
/// ([`ChannelEncryption::bump`]) rather than leaving the endpoint to ask on
/// a timer.
struct Unopened {
    workspace: WorkspaceId,
    /// `None` while no one can name the channel's readers here: re-checked
    /// whenever the policy inputs announce a change. `Some` after a failed
    /// open: retried on its own backoff.
    retry: Option<Backoff>,
    /// The record an endpoint asked about, when it knew one: a record's
    /// channel is named from its record, so the re-check that waits for the
    /// policy asks with it.
    route: Option<ChannelRoute>,
}

impl Unopened {
    fn failed(workspace: WorkspaceId, now: Instant) -> Self {
        let mut retry = Backoff::now(now, REQUEST_FIRST);
        retry.fired(now, REQUEST_LONGEST);
        Self {
            workspace,
            retry: Some(retry),
            route: None,
        }
    }

    fn unnamed(workspace: WorkspaceId, route: Option<ChannelRoute>) -> Self {
        Self {
            workspace,
            retry: None,
            route,
        }
    }
}

/// This device's side of every encrypted channel it takes part in.
///
/// Taking part in a workspace's channel needs this device's own channel state,
/// kept under a key from the platform secret store; a service opened
/// without it ([`Self::without_store`]) answers every workspace
/// [`ChannelAccess::NoSecretStore`].
pub struct ChannelEncryption {
    /// This device's own channel state; `None` when no secret store opened.
    store: Option<ChannelEncryptionConfig>,
    directory: Box<dyn ChannelDirectory>,
    gates: Arc<dyn ChannelGates>,
    peers: HashMap<ChannelId, PeerEntry>,
    /// When the groups last re-checked the access policy, and what the next
    /// re-check waits for.
    reconcile: PolicySchedule,
    delivery: u64,
    generation: u64,
    /// The [`Self::generation`] each channel last changed at.
    changed: HashMap<ChannelId, u64>,
    /// [`Self::generation`], announced to whoever waits for a change.
    changes: tokio::sync::watch::Sender<u64>,
    /// An endpoint's [`Self::access`] queued governance the next pump sends
    /// (a genesis to announce, an access request to make). A host that parks
    /// its pump while idle wakes it on this; see [`ChannelKeyAccess::with_wake`].
    queued: bool,
    /// Channels asked about that could not be opened; see [`Unopened`].
    unopened: HashMap<ChannelId, Unopened>,
    /// The gate this service last registered with the broker for each
    /// channel. Every access re-asserts its channel's gate, and a channel's
    /// keys are read for every live frame sealed or opened; taking the
    /// broker's lock for each made every such read wait for as long as a
    /// broker thread held it to append and sync durable history, which froze
    /// the desktop while its frame loop was the reader. Only a changed gate
    /// takes the lock.
    registered: HashMap<ChannelId, channel_crypto::ChannelGate>,
    /// Channels whose frames arrived while this device took no part in
    /// them: they have a group, so this device never creates one.
    groups_seen: std::collections::HashSet<ChannelId>,
    /// The key scope carrying each storage unit this device knows the route
    /// of, by the unit's own channel. A unit seals under its scope's keys
    /// while naming its own channel, so a payload's channel is turned back
    /// into its group here.
    units: HashMap<ChannelId, ChannelId>,
    /// The key scope a workspace's host named for a storage unit as this
    /// device entered ([`Self::scope_told`]), by the unit's own channel: what
    /// the unit seals under until the governance here names its scope.
    told: HashMap<ChannelId, ChannelId>,
    /// The policy epoch the last pass read, while the directory announces
    /// its changes: a request that went unanswered under the policy before
    /// it is asked again at once when it moves, rather than waiting out a
    /// backoff that has doubled past what the caller waits
    /// ([`Self::ask_waiting_now`]).
    policy_epoch: Option<u64>,
}

impl ChannelEncryption {
    /// Forget `workspace`, deleted: its channels' groups, keys and routes,
    /// here and in the store.
    ///
    /// # Errors
    /// What the store kept could not be removed.
    pub fn forget_workspace(&mut self, workspace: WorkspaceId) -> Result<(), String> {
        let channels: Vec<ChannelId> = self
            .peers
            .iter()
            .filter(|(_, entry)| entry.workspace == workspace)
            .map(|(channel, _)| *channel)
            .collect();
        for channel in &channels {
            self.peers.remove(channel);
            self.changed.remove(channel);
            self.unopened.remove(channel);
            self.registered.remove(channel);
            self.groups_seen.remove(channel);
        }
        self.units
            .retain(|unit, scope| !channels.contains(unit) && !channels.contains(scope));
        self.told
            .retain(|unit, scope| !channels.contains(unit) && !channels.contains(scope));
        self.store
            .as_ref()
            .map_or(Ok(()), |store| store.state.forget(workspace))
    }

    /// Open every channel state the host keeps and register each channel's
    /// gate with the broker, so the plaintext gate holds from the first op after
    /// a restart.
    pub fn open(
        config: ChannelEncryptionConfig,
        directory: Box<dyn ChannelDirectory>,
        gates: Arc<dyn ChannelGates>,
    ) -> std::io::Result<Self> {
        config.state.prepare().map_err(std::io::Error::other)?;
        let held = config.state.held();
        let mut service = Self::with_store(Some(config), directory, gates);
        let now = tilo_clock::now();
        for (workspace, channels) in held {
            for channel in channels {
                if let Err(error) = service.open_peer(workspace, channel, now) {
                    tracing::warn!(%error, %workspace, "channel state could not be opened");
                }
            }
        }
        // The scope each unit seals under now, read again from the policy
        // rather than remembered: a unit whose state was kept from before key
        // scopes keeps it for reading while new content seals under the scope.
        let routes: Vec<(WorkspaceId, ChannelRoute)> = service
            .peers
            .values()
            .filter_map(|entry| Some((entry.workspace, entry.route.clone()?)))
            .collect();
        let wall = tilo_clock::unix_now();
        for (workspace, route) in routes {
            service.resolve_scope(workspace, &route, wall);
        }
        Ok(service)
    }

    /// The service of a device whose secret store did not open: it answers
    /// every workspace [`ChannelAccess::NoSecretStore`].
    pub fn without_store(
        directory: Box<dyn ChannelDirectory>,
        gates: Arc<dyn ChannelGates>,
    ) -> Self {
        Self::with_store(None, directory, gates)
    }

    fn with_store(
        store: Option<ChannelEncryptionConfig>,
        directory: Box<dyn ChannelDirectory>,
        gates: Arc<dyn ChannelGates>,
    ) -> Self {
        Self {
            store,
            directory,
            gates,
            peers: HashMap::new(),
            reconcile: PolicySchedule::default(),
            delivery: 0,
            generation: 0,
            changed: HashMap::new(),
            changes: tokio::sync::watch::Sender::new(0),
            queued: false,
            unopened: HashMap::new(),
            registered: HashMap::new(),
            groups_seen: std::collections::HashSet::new(),
            units: HashMap::new(),
            told: HashMap::new(),
            policy_epoch: None,
        }
    }

    /// Increases whenever any channel's keys or access change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The [`Self::generation`] at which `channel`'s keys or access last
    /// changed; 0 while they never did in this service. A storage unit
    /// changes when the scope carrying it does, so asking by the unit's
    /// channel answers with the later of its own change (its readers named,
    /// say) and its scope's: a unit told once of its own still hears every
    /// epoch its scope moves to after.
    pub fn channel_change(&self, channel: ChannelId) -> u64 {
        let own = self.changed.get(&channel).copied().unwrap_or(0);
        let scope = self
            .units
            .get(&channel)
            .filter(|scope| **scope != channel)
            .and_then(|scope| self.changed.get(scope))
            .copied()
            .unwrap_or(0);
        own.max(scope)
    }

    /// The key scope that carries every unit of `workspace` whose readers
    /// are the workspace's participants.
    fn workspace_scope(workspace: WorkspaceId) -> ChannelId {
        collab_types::workspace_scope(&workspace.to_string())
    }

    /// The scope the storage unit on `route` seals under in `workspace`,
    /// remembered so a payload naming only the unit finds its group, or
    /// `None` while this device cannot decide it
    /// ([`ChannelDirectory::unit_scope`]).
    fn resolve_scope(
        &mut self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        wall: u64,
    ) -> Option<ChannelId> {
        let unit = route.channel(&workspace.to_string());
        // The governance here decides; before it arrives, what the host named
        // as this device entered.
        let scope = match self.directory.unit_scope(workspace, route, wall) {
            Some(scope) => {
                self.told.remove(&unit);
                scope
            }
            None => *self.told.get(&unit)?,
        };
        self.units.insert(unit, scope);
        Some(scope)
    }

    /// The group that carries `unit`'s keys here: the scope its route named,
    /// or else the unit's own group when this device holds one (a record
    /// whose readers policy narrows, or one whose group was made before key
    /// scopes), or else the workspace's scope.
    fn scope_here(&self, workspace: WorkspaceId, unit: ChannelId) -> ChannelId {
        self.units
            .get(&unit)
            .copied()
            .or_else(|| self.takes_part_in(workspace, unit).then_some(unit))
            .unwrap_or_else(|| Self::workspace_scope(workspace))
    }

    /// Whether this device holds the state of `channel`, one of `workspace`'s
    /// groups.
    fn takes_part_in(&self, workspace: WorkspaceId, channel: ChannelId) -> bool {
        self.peers
            .get(&channel)
            .is_some_and(|entry| entry.workspace == workspace)
    }

    /// What an endpoint of this device may do with `channel` in `workspace`.
    /// The first call for a route starts taking part in its channel: this
    /// device asks the participants for access, and creates the group when
    /// no genesis arrives. The caller has already decided the endpoint may
    /// read the route.
    pub fn access(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        now: Instant,
    ) -> ChannelAccess {
        self.access_routed(workspace, channel, None, now)
    }

    fn access_routed(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: Instant,
    ) -> ChannelAccess {
        if self.store.is_none() {
            self.note_unopened(channel, Unopened::unnamed(workspace, route.cloned()));
            return ChannelAccess::NoSecretStore;
        }
        if !self.peers.contains_key(&channel) {
            match self.start_peer(workspace, channel, route, now) {
                Ok(true) => self.queued = true,
                Ok(false) => {
                    self.note_unopened(channel, Unopened::unnamed(workspace, route.cloned()));
                    return ChannelAccess::Unavailable;
                }
                Err(error) => {
                    tracing::warn!(%error, %workspace, "channel state could not be created");
                    self.note_unopened(channel, Unopened::failed(workspace, now));
                    return ChannelAccess::Unavailable;
                }
            }
        }
        self.unopened.remove(&channel);
        // Learned before it is founded: who may read the channel is read from
        // its record, and the request this device sends carries the route.
        if let Some(route) = route {
            self.learn_route(workspace, channel, route);
        }
        let Some(entry) = self.peers.get_mut(&channel) else {
            return ChannelAccess::Unavailable;
        };
        let was_wanted = entry.wanted;
        entry.wanted = true;
        let access = match entry.peer.access() {
            Access::Ready { .. } => entry
                .peer
                .content_keys()
                .map_or(ChannelAccess::Waiting, ChannelAccess::Ready),
            Access::Waiting => ChannelAccess::Waiting,
            Access::Removed => {
                if !entry.rejoin {
                    entry.rejoin = true;
                    entry.request = None;
                    entry.retry = Backoff::now(now, REQUEST_FIRST);
                    self.queued = true;
                }
                ChannelAccess::Removed
            }
        };
        // The read that first wants a channel wakes the pass that asks for
        // it: a peer another member's access request opened is nobody's
        // wanted channel until then, and the pump would park with the
        // request never sent.
        if !was_wanted {
            self.queued = true;
        }
        access
    }

    /// [`Self::access`] for the channel of `route`'s record in `workspace`,
    /// remembering the route: this device then decides Read on the record
    /// itself, and sends the route with its access request. The answer's
    /// channel is the record's own; its keys are the ones the record's key
    /// scope carries, named for the record.
    pub fn access_record(
        &mut self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        now: Instant,
    ) -> (ChannelId, ChannelAccess) {
        let unit = route.channel(&workspace.to_string());
        let Some(scope) = self.resolve_scope(workspace, route, tilo_clock::unix_now()) else {
            // Nothing here names the scope, and a guessed one could hand a
            // device keys the record's policy withholds.
            self.note_unopened(unit, Unopened::unnamed(workspace, Some(route.clone())));
            return (unit, ChannelAccess::Unavailable);
        };
        // A unit with readers of its own is carried by its own group, whose
        // record this device decides Read on; the workspace's scope is read
        // by the workspace's participants, and needs no route.
        let routed = (scope == unit).then_some(route);
        let access = self.access_routed(workspace, scope, routed, now);
        (unit, self.for_unit(workspace, unit, scope, access))
    }

    /// [`Self::access`] for the group carrying the storage unit `unit`, whose
    /// route this caller does not have: the scope is the one learned for it,
    /// its own group here, or the workspace's scope. The answer's keys are
    /// named for the unit.
    fn access_unit(
        &mut self,
        workspace: WorkspaceId,
        unit: ChannelId,
        now: Instant,
    ) -> ChannelAccess {
        let scope = self.scope_here(workspace, unit);
        let access = self.access_routed(workspace, scope, None, now);
        self.for_unit(workspace, unit, scope, access)
    }

    /// `access`, the answer for the group `scope`, as the storage unit `unit`
    /// uses it: its keys named for the unit, with the keys of a group the
    /// unit had before key scopes merged in, so its whole history opens.
    fn for_unit(
        &self,
        workspace: WorkspaceId,
        unit: ChannelId,
        scope: ChannelId,
        access: ChannelAccess,
    ) -> ChannelAccess {
        let ChannelAccess::Ready(keys) = access else {
            return access;
        };
        let mut keys = keys.for_unit(unit);
        if scope != unit
            && let Some(own) = self.existing_keys(workspace, unit)
            && let Ok(merged) = keys.clone().with_earlier(&own)
        {
            keys = merged;
        }
        ChannelAccess::Ready(keys)
    }

    /// Make the content keys of legacy per-record groups available under the
    /// key scope that now carries their units. A device that holds such a
    /// group hands its keys to the scope's members with the scope's next
    /// commit, and every later admission passes them on, so a device that
    /// joined after key scopes reads the unit's whole history and the holders
    /// of the old group can go offline once one of them has done it.
    ///
    /// Only units the workspace's scope carries: one a Read Deny narrows is
    /// sealed under a group of its own, whose keys no denied device gets, and
    /// those are never handed to the broader scope. A unit whose record this
    /// device has not learned the readers of waits for a policy re-check,
    /// which resolves it.
    fn hand_over_legacy_keys(&mut self, wall: u64, recheck: bool) {
        // Handing keys over is a commit, which a reader leaves to participants.
        if self.reads_only() {
            return;
        }
        let mut handing = Vec::new();
        let mut unresolved = Vec::new();
        for (unit, entry) in &self.peers {
            let scope = Self::workspace_scope(entry.workspace);
            if *unit == scope {
                continue;
            }
            match self.units.get(unit) {
                Some(carried) if *carried == scope => handing.push((*unit, scope)),
                Some(_) => {}
                None => {
                    if let Some(route) = &entry.route {
                        unresolved.push((*unit, entry.workspace, route.clone()));
                    }
                }
            }
        }
        if recheck {
            for (unit, workspace, route) in unresolved {
                let scope = self.resolve_scope(workspace, &route, wall);
                if scope == Some(Self::workspace_scope(workspace)) {
                    handing.push((unit, Self::workspace_scope(workspace)));
                }
            }
        }
        let mut adopted = Vec::new();
        for (unit, scope) in handing {
            let [Some(scope_entry), Some(unit_entry)] =
                self.peers.get_disjoint_mut([&scope, &unit])
            else {
                continue;
            };
            if scope_entry.workspace != unit_entry.workspace
                || !matches!(scope_entry.peer.access(), Access::Ready { .. })
            {
                continue;
            }
            let Some(keys) = unit_entry.peer.keyring() else {
                continue;
            };
            if scope_entry.peer.adopt(keys) {
                // The scope's commit is due now: it is what hands the keys out.
                scope_entry.dirty = true;
                adopted.push(scope);
            }
        }
        for scope in adopted {
            self.bump(scope);
        }
    }

    /// Keep `route`, proven to be `channel`'s in `workspace`, with the channel's
    /// state, durably, and re-check the channel's readers with it.
    fn learn_route(&mut self, workspace: WorkspaceId, channel: ChannelId, route: &ChannelRoute) {
        if !route.proves(&workspace.to_string(), channel) {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        let mut learned = false;
        if let Some(entry) = self.peers.get_mut(&channel)
            && entry.workspace == workspace
            && entry.route.is_none()
        {
            entry.route = Some(route.clone());
            entry.dirty = true;
            learned = true;
            store.state.keep_route(workspace, channel, route);
        }
        // The scope a unit seals under is read from its record, so a route
        // that arrives after the channel was opened resolves it now.
        if learned {
            self.resolve_scope(workspace, route, tilo_clock::unix_now());
        }
    }

    /// Remember that `channel` could not be opened, so its route hears when
    /// that may have changed. A failed open keeps the backoff it already
    /// has; a first entry wakes a parked pump, which then arms its policy
    /// watch or retry deadline for it.
    fn note_unopened(&mut self, channel: ChannelId, entry: Unopened) {
        match self.unopened.get_mut(&channel) {
            Some(known) if known.workspace == entry.workspace => {
                if entry.retry.is_none() {
                    known.retry = None;
                } else if known.retry.is_none() {
                    known.retry = entry.retry;
                }
                // An endpoint that asks with its record hands it over for
                // the re-check that needs it.
                if known.route.is_none() {
                    known.route = entry.route;
                }
            }
            _ => {
                self.unopened.insert(channel, entry);
                self.queued = true;
            }
        }
    }

    /// Tell the routes of unopened channels whose answer may now differ:
    /// on a policy re-check (`recheck`), those this device may now take
    /// part in; and those whose failed open is due again.
    fn wake_unopened(&mut self, now: Instant, recheck: bool) {
        let mut changed = Vec::new();
        // Without a store nothing opens, whatever the workspace's governance says.
        let stored = self.store.is_some();
        let device = self.device();
        for (channel, entry) in &mut self.unopened {
            match &mut entry.retry {
                None => {
                    // A record's channel is named from its record, so the
                    // re-check asks with the route its endpoint gave; only a
                    // channel asked about without one opens on a directory
                    // that names its readers by channel alone.
                    let opens = stored
                        && takes_part(
                            &*self.directory,
                            device,
                            entry.workspace,
                            *channel,
                            entry.route.as_ref(),
                        );
                    if recheck && opens {
                        changed.push((*channel, true));
                    }
                }
                Some(retry) => {
                    if retry.is_due(now) {
                        retry.fired(now, REQUEST_LONGEST);
                        changed.push((*channel, false));
                    }
                }
            }
        }
        for (channel, settled) in changed {
            // A workspace whose governance is now known is told once; its route
            // asks and opens the channel. A failed open stays until one
            // succeeds.
            if settled {
                self.unopened.remove(&channel);
            }
            self.bump(channel);
        }
    }

    /// Whether an unopened channel waits for a re-check of the policy that
    /// may name it. Without a secret store nothing opens, so a device that
    /// holds none has nothing to re-check and parks.
    fn unopened_waits_for_policy(&self) -> bool {
        self.store.is_some() && self.unopened.values().any(|entry| entry.retry.is_none())
    }

    /// Observe a current key without opening a channel, requesting access or
    /// changing a removed member's rejoin state.
    fn existing_keys(
        &self,
        workspace: WorkspaceId,
        channel: ChannelId,
    ) -> Option<ChannelContentKeys> {
        self.peers
            .get(&channel)
            .filter(|entry| entry.workspace == workspace)
            .and_then(|entry| entry.peer.content_keys())
    }

    /// `unit`'s keys as this device holds them, opening, joining or creating
    /// nothing: `None` when this device takes part in no group that could
    /// carry `unit` of `workspace`, `Some(Err(_))` when it does and holds none
    /// of its keys yet. `scope` is the scope its route named, when the caller
    /// has one.
    pub(crate) fn record_keys(
        &self,
        workspace: WorkspaceId,
        unit: ChannelId,
        scope: Option<ChannelId>,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let scope = scope.unwrap_or_else(|| self.scope_here(workspace, unit));
        if let Some(keys) = self.unit_keys(workspace, unit, scope) {
            return Some(Ok(keys));
        }
        // The directory names a channel's readers only for a device that may
        // read it: an unnameable scope is no group of this device's.
        if self.store.is_none()
            || self
                .directory
                .allowed_devices(workspace, scope, None, tilo_clock::unix_now())
                .is_none()
        {
            return None;
        }
        Some(Err("this device holds no channel keys yet".into()))
    }

    /// The keys that open `unit` here: those of the group carrying it, named
    /// for the unit, with the keys of a group the unit had before key scopes
    /// merged in. `None` while neither is here yet.
    fn unit_keys(
        &self,
        workspace: WorkspaceId,
        unit: ChannelId,
        scope: ChannelId,
    ) -> Option<ChannelContentKeys> {
        let held = self.existing_keys(workspace, scope);
        let own = (scope != unit)
            .then(|| self.existing_keys(workspace, unit))
            .flatten();
        match (held, own) {
            (Some(held), Some(own)) => {
                let keys = held.for_unit(unit);
                Some(keys.clone().with_earlier(&own).unwrap_or(keys))
            }
            (Some(held), None) => Some(held.for_unit(unit)),
            (None, Some(own)) => Some(own.for_unit(unit)),
            (None, None) => None,
        }
    }

    fn device(&self) -> Option<Ed25519PublicKey> {
        self.store
            .as_ref()
            .map(|store| Ed25519PublicKey::from_bytes(store.signer.device_public_key()))
    }

    /// Whether this device only follows its groups ([`Role::Reader`]).
    fn reads_only(&self) -> bool {
        self.store
            .as_ref()
            .is_some_and(|store| store.role == Role::Reader)
    }

    /// Whether a request waits on its signature from the device's custody: [`Self::signatures`]
    /// then has something to await.
    #[must_use]
    pub fn signing(&self) -> bool {
        self.peers.values().any(|entry| entry.signing.is_some())
    }

    /// Await every signature the device's custody owes, keeping each signed request for the next
    /// [`Self::pump`] to send.
    pub async fn signatures(&mut self) {
        for (channel, request, signing) in self.take_signing() {
            let signed = signing.await;
            self.signed(channel, request, signed);
        }
    }

    /// The access requests whose signatures the device's custody owes, taken out of the service
    /// so a shared one is not held while they are awaited; each answer goes back with
    /// [`Self::signed`].
    pub fn take_signing(
        &mut self,
    ) -> Vec<(
        ChannelId,
        MlsAccessRequestV1,
        collab_types::signer::SignFuture<'static>,
    )> {
        self.peers
            .iter_mut()
            .filter_map(|(channel, entry)| {
                let (request, signing) = entry.signing.take()?;
                Some((*channel, request, signing))
            })
            .collect()
    }

    /// Keep `request`, signed by the device's custody, for the next [`Self::pump`] to send.
    pub fn signed(
        &mut self,
        channel: ChannelId,
        mut request: MlsAccessRequestV1,
        signed: Result<[u8; 64], collab_types::signer::SignError>,
    ) {
        match signed {
            Ok(signature) => {
                if let Some(entry) = self.peers.get_mut(&channel) {
                    request.signature = collab_types::Signature::from_bytes(signature);
                    entry.request = Some(request);
                    self.queued = true;
                }
            }
            Err(error) => tracing::warn!(%error, "channel access request was not signed"),
        }
    }

    /// Run the governance protocol once: apply what arrived, move groups to
    /// the access policy, ask for access, and publish what is due. `wall` is
    /// the Unix time the policy is evaluated at.
    pub fn pump(&mut self, transport: &dyn GovernanceTransport, now: Instant, wall: u64) {
        let inbound = transport.take_inbound();
        self.pump_inbound(inbound, transport, now, wall);
    }

    /// [`Self::pump`] with `inbound`, the governance already taken from
    /// `transport`.
    fn pump_inbound(
        &mut self,
        inbound: Vec<InboundChannelGovernance>,
        transport: &dyn GovernanceTransport,
        now: Instant,
        wall: u64,
    ) {
        for inbound in inbound {
            self.receive(inbound, now, wall);
        }
        self.retry_held(now, wall);
        self.judge_held_welcomes(now, wall);
        // Read before the policy is: a change announced while the groups
        // reconcile has a later epoch and makes the next re-check due.
        let epoch = self.directory.policy_epoch();
        // The policy moved since the last pass: a request that went unanswered
        // under the one before it may be answered under this one.
        let policy_moved = epoch.is_some() && epoch != self.policy_epoch;
        self.policy_epoch = epoch;
        let reconcile_due = self
            .reconcile
            .due(now, epoch.unwrap_or(0))
            .is_some_and(|due| due <= now);
        let channels: Vec<ChannelId> = self.peers.keys().copied().collect();
        let mut reconciled = false;
        // Before the groups reconcile, so the hand-over commit is written in
        // this pass.
        self.hand_over_legacy_keys(wall, reconcile_due);
        if policy_moved {
            self.ask_waiting_now(now);
        }
        for channel in channels {
            let due = reconcile_due
                || self
                    .peers
                    .get(&channel)
                    .is_some_and(|entry| entry.dirty || entry.turn_at.is_some_and(|at| at <= now));
            self.found(channel, now, wall);
            if due {
                self.reconcile(channel, now, wall);
                reconciled = true;
            }
            self.publish(channel, transport);
            self.ask(channel, transport, now, wall);
            self.save_deferred(channel, now);
        }
        // Unopened channels are re-checked with the policy, and a failed
        // open when its backoff is due.
        self.wake_unopened(now, reconcile_due);
        if reconcile_due {
            self.reconcile
                .checked(now, epoch.unwrap_or(0), epoch.is_none());
        }
        // A newly opened workspace brings its own expiries.
        if reconciled && epoch.is_some() {
            self.register_next_policy_change(wall);
        }
    }

    /// Write every channel's state a frame taken in left unwritten, now
    /// rather than once it has waited [`DEFERRED_SAVE`]: the state carries the
    /// keys a write seals under, and it is written before such a write leaves
    /// this device ([`ChannelPeer::save_deferred`]).
    pub fn save_pending(&mut self) {
        let Some(store) = &self.store else {
            return;
        };
        for entry in self.peers.values_mut() {
            if !entry.peer.has_unsaved() {
                continue;
            }
            entry.unsaved_since = None;
            if let Err(error) = entry.peer.save_deferred(&store.vault) {
                tracing::warn!(%error, workspace = %entry.workspace, "channel state could not be written");
            }
        }
    }

    /// Write what taking in a frame left unwritten in `channel`'s state once
    /// it has waited [`DEFERRED_SAVE`].
    fn save_deferred(&mut self, channel: ChannelId, now: Instant) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        if !entry.peer.has_unsaved() {
            entry.unsaved_since = None;
            return;
        }
        let since = *entry.unsaved_since.get_or_insert(now);
        if now < since.checked_add(DEFERRED_SAVE).unwrap_or(since) {
            return;
        }
        entry.unsaved_since = None;
        if let Err(error) = entry.peer.save_deferred(&store.vault) {
            tracing::warn!(%error, workspace = %entry.workspace, "channel state could not be written");
        }
    }

    /// When [`Self::pump`] next has work that no inbound message brings:
    /// `None` while there is none, so its caller can park until a message
    /// arrives or an endpoint's access wakes it ([`ChannelKeyAccess::with_wake`]).
    ///
    /// A group re-checks its access policy when the directory announces a
    /// change (`Self::waits_for_policy_change`) and at the next expiry the
    /// last re-check read, which [`Self::pump`] registers with
    /// [`ChannelDirectory::announce_at`]; neither is a deadline here. While
    /// this device has a channel to settle, a directory that announces
    /// nothing re-checks every `RECONCILE_INTERVAL`, and frames the
    /// transport did not take are retried at the same cadence; that re-check
    /// is what lets a channel this device only waits on be founded, or
    /// opened, once the policy names it. A waiting device is due on its
    /// request backoff.
    #[must_use]
    pub fn next_pump_deadline(&self, now: Instant) -> Option<Instant> {
        let settling = !self.peers.is_empty() || self.unopened_waits_for_policy();
        let turn = self.peers.values().filter_map(|entry| entry.turn_at).min();
        let policy = if self.peers.values().any(|entry| entry.dirty) {
            Some(now)
        } else if settling {
            let unpublished = self
                .peers
                .values()
                .any(|entry| !entry.peer.outbox().is_empty())
                .then(|| now.checked_add(RECONCILE_INTERVAL).unwrap_or(now));
            self.reconcile
                .due(now, self.directory.policy_epoch().unwrap_or(0))
                .into_iter()
                .chain(unpublished)
                .min()
        } else {
            None
        };
        let asking = self
            .peers
            .values()
            .filter_map(|entry| {
                let asking = match entry.peer.access() {
                    Access::Waiting => entry.wanted,
                    Access::Removed => entry.rejoin,
                    Access::Ready { .. } => false,
                };
                asking.then_some(entry.retry.due)
            })
            .min();
        let founding = self
            .peers
            .iter()
            .filter(|(channel, entry)| {
                !entry.wanted
                    && entry.founding.allowed
                    && entry.founding.at > now
                    && !entry.peer.knows_group()
                    && !self.groups_seen.contains(channel)
            })
            .map(|(_, entry)| entry.founding.at)
            .min();
        let unopened = self
            .unopened
            .values()
            .filter_map(|entry| entry.retry.as_ref().map(|retry| retry.due))
            .min();
        let welcome = self
            .peers
            .values()
            .filter_map(|entry| entry.welcome.as_ref().map(|held| held.until))
            .min();
        let unsaved = self
            .peers
            .values()
            .filter_map(|entry| entry.unsaved_since)
            .filter_map(|since| since.checked_add(DEFERRED_SAVE))
            .min();
        policy
            .into_iter()
            .chain(unsaved)
            .chain(welcome)
            .chain(turn)
            .chain(asking)
            .chain(founding)
            .chain(unopened)
            .min()
    }

    /// Whether the groups wait for their directory to announce a policy
    /// change: some exist, the directory announces, and nothing changed
    /// since their last re-check. The host's pump then sleeps until an
    /// announcement wakes it ([`ChannelKeyAccess::arm_policy_watch`]).
    fn waits_for_policy_change(&self) -> bool {
        (!self.peers.is_empty() || self.unopened_waits_for_policy())
            && self
                .directory
                .policy_epoch()
                .is_some_and(|epoch| self.reconcile.waits_for_change(epoch))
    }

    /// Register the first instant after `wall` at which the access policy of
    /// a workspace this device takes part in can change by time alone, so the
    /// groups re-check then ([`ChannelDirectory::announce_at`]).
    fn register_next_policy_change(&self, wall: u64) {
        let workspaces: BTreeSet<WorkspaceId> =
            self.peers.values().map(|entry| entry.workspace).collect();
        let next = workspaces
            .into_iter()
            .filter_map(|workspace| self.directory.next_policy_change(workspace, wall))
            .filter(|at| *at > wall)
            .min();
        if let Some(at) = next {
            self.directory.announce_at(at);
        }
    }

    fn receive(&mut self, inbound: InboundChannelGovernance, now: Instant, wall: u64) {
        match inbound {
            InboundChannelGovernance::Unfounded {
                workspace,
                channel,
                device,
            } => {
                let this = self.device() == Some(device);
                let Some(entry) = self
                    .peers
                    .get_mut(&channel)
                    .filter(|entry| entry.workspace == workspace)
                else {
                    return;
                };
                entry.founding.unfounded = true;
                entry.founding.answered |= this;
            }
            InboundChannelGovernance::Frame { workspace, frame } => {
                self.receive_frame(workspace, *frame, now, wall);
            }
            InboundChannelGovernance::AccessRequest {
                workspace,
                request,
                route,
            } => {
                self.receive_request(workspace, *request, route.as_deref(), now, wall);
            }
        }
    }

    /// Judge the frames each channel held for the policy ([`Self::receive_frame`])
    /// again: a frame whose signer the policy names now is applied as if it had
    /// just arrived, and one it still does not back is held again.
    fn retry_held(&mut self, now: Instant, wall: u64) {
        let held: Vec<(WorkspaceId, MlsGovernanceFrame)> = self
            .peers
            .iter_mut()
            .flat_map(|(_, entry)| {
                let workspace = entry.workspace;
                std::mem::take(&mut entry.held)
                    .into_iter()
                    .map(move |frame| (workspace, frame))
            })
            .collect();
        for (workspace, frame) in held {
            self.receive_frame(workspace, frame, now, wall);
        }
    }

    /// Judge again each Welcome held for this device's view of the
    /// membership once that view changed, and give up on one held for
    /// [`WELCOME_WAIT`].
    fn judge_held_welcomes(&mut self, now: Instant, wall: u64) {
        let held: Vec<ChannelId> = self
            .peers
            .iter()
            .filter(|(_, entry)| entry.welcome.is_some())
            .map(|(channel, _)| *channel)
            .collect();
        for channel in held {
            let Some(entry) = self.peers.get(&channel) else {
                continue;
            };
            let allowed = self.directory.allowed_devices(
                entry.workspace,
                channel,
                entry.route.as_ref(),
                wall,
            );
            let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
                continue;
            };
            let Some(held) = &entry.welcome else {
                continue;
            };
            if entry.peer.access() != Access::Waiting {
                entry.welcome = None;
            } else if now >= held.until {
                give_up_welcome(entry, store, now);
                self.bump(channel);
            } else if held.judged != allowed {
                // Held again by `receive_frame` if the view still does not
                // back it, keeping when it is given up on.
                let (workspace, frame) = (entry.workspace, held.frame.clone());
                self.receive_frame(workspace, frame, now, wall);
            }
        }
    }

    fn receive_frame(
        &mut self,
        workspace: WorkspaceId,
        frame: MlsGovernanceFrame,
        now: Instant,
        wall: u64,
    ) {
        let channel = frame.channel;
        if let Some(entry) = self.peers.get(&channel)
            && entry.workspace != workspace
        {
            return;
        }
        if !self.peers.contains_key(&channel) {
            // A device that was away keeps the state of the channels it takes
            // part in, and its host replays the governance it retained when
            // the device comes back, before its apps read anything: take that
            // in rather than forget it, so the group is current before the
            // first read of its record. A channel this device holds no state
            // of is one it takes no part in: it never creates a second group
            // of a channel whose group it has heard, and if it comes to take
            // part it asks to join.
            let opened = self.store.is_some()
                && match self.open_peer(workspace, channel, now) {
                    Ok(opened) => opened,
                    Err(error) => {
                        tracing::warn!(%error, %workspace, "channel state could not be opened");
                        false
                    }
                };
            if !opened {
                if self.groups_seen.len() < MAX_GROUPS_SEEN {
                    self.groups_seen.insert(channel);
                }
                return;
            }
            self.receive_frame(workspace, frame, now, wall);
            return;
        }
        let Some(route) = self.peers.get(&channel).map(|entry| entry.route.clone()) else {
            return;
        };
        let allowed = self
            .directory
            .allowed_devices(workspace, channel, route.as_ref(), wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        let unjudged = match &allowed {
            // The workspace's governance reached this device before the
            // policy here named the frame's signer.
            Some(allowed) => !channel_crypto::backs(&frame, allowed),
            // A group competing with this device's, while it cannot name who
            // reads the channel: whether its founder may is not known here.
            None => entry.peer.competes(&frame),
        };
        if unjudged {
            // Keep the frame for the policy that moves instead of refusing it
            // for good: a device admitted a moment ago writes, and a winning
            // genesis moves this device to the group the others are in.
            if !entry.held.contains(&frame) {
                if entry.held.len() == MAX_HELD_FRAMES {
                    entry.held.remove(0);
                }
                entry.held.push(frame);
            }
            return;
        }
        let (epoch, signer) = (frame.epoch, frame.signed_by);
        // A waiting device keeps the frame in case it carries a Welcome its
        // view of the membership does not back yet.
        let kept = (entry.peer.access() == Access::Waiting).then(|| frame.clone());
        let received = match &allowed {
            Some(allowed) => entry.peer.receive(frame, allowed, &store.vault),
            None => entry.peer.follow(frame, &store.vault),
        };
        let event = match received {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, %workspace, "channel frame refused");
                return;
            }
        };
        tracing::debug!(%workspace, channel = %channel.0, epoch, ?signer, ?event, access = ?entry.peer.access(), "channel frame taken in");
        if !entry.wanted && entry.peer.access() == Access::Waiting && entry.peer.knows_group() {
            // Taken up only to create the group if no one had: someone has,
            // and its participants admit whoever asked.
            self.peers.remove(&channel);
            self.groups_seen.insert(channel);
            if let Err(error) = store.state.peers(workspace).forget(channel) {
                tracing::warn!(%error, %workspace, "unwanted channel state could not be removed");
            }
            return;
        }
        let changed = match event {
            PeerEvent::Joined { .. } | PeerEvent::Advanced { .. } => {
                // The group may still differ from the policy.
                entry.dirty = true;
                true
            }
            PeerEvent::Removed => true,
            PeerEvent::Desynchronised
            | PeerEvent::WelcomeRefused(WelcomeRefusal::SignedByAnotherDevice) => {
                if let PeerEvent::WelcomeRefused(refusal) = &event {
                    tracing::warn!(%refusal, %workspace, channel = %channel.0, ?signer, "channel Welcome refused");
                }
                // Ask again with a fresh member key; any participant admits
                // a member asking with another key than it joined with (the
                // members that took a refused Welcome's commit hold this
                // device admitted with the key it asked with).
                entry.request = None;
                entry.retry = Backoff::now(now, REQUEST_FIRST);
                entry.fresh_at = now;
                true
            }
            PeerEvent::AdmittedWithAnotherKey => {
                // The participants admit it again with the key it asks with
                // now: ask at once, not on the backoff.
                entry.retry = Backoff::now(now, REQUEST_FIRST);
                false
            }
            // A Welcome refused for this device's view is held below.
            PeerEvent::Genesis
            | PeerEvent::Duplicate
            | PeerEvent::Ignored
            | PeerEvent::WelcomeRefused(WelcomeRefusal::DeviceNotAllowed) => false,
        };
        let changed = entry.hold_welcome(&event, kept, allowed, epoch, store, now) || changed;
        let gate = entry.peer.gate();
        self.register_gate(channel, gate);
        if changed {
            self.bump(channel);
        }
    }

    fn receive_request(
        &mut self,
        workspace: WorkspaceId,
        request: MlsAccessRequestV1,
        route: Option<&ChannelRoute>,
        now: Instant,
        wall: u64,
    ) {
        let channel = request.channel;
        // A reader admits no one.
        if self.reads_only() {
            return;
        }
        // A route that is not the channel's is a forgery: the request goes
        // with it, rather than being decided as if it named no record.
        if route.is_some_and(|route| !route.proves(&workspace.to_string(), channel)) {
            return;
        }
        if !self.peers.contains_key(&channel)
            && request.verify().is_ok()
            && self
                .directory
                .allowed_devices(workspace, channel, route, wall)
                .is_some_and(|allowed| {
                    allowed.contains(&request.device)
                        && self
                            .device()
                            .is_some_and(|device| allowed.contains(&device))
                })
        {
            // A device the policy allows asks for a channel this device may
            // read and holds nothing of: it creates the group if no genesis
            // arrives. Outsiders cannot make it mint state.
            match self.start_peer(workspace, channel, route, now) {
                Ok(true) => {
                    if let Some(entry) = self.peers.get_mut(&channel) {
                        entry.wanted = false;
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, %workspace, "channel state could not be created");
                    return;
                }
            }
        }
        if let Some(route) = route {
            self.learn_route(workspace, channel, route);
        }
        let Some(entry) = self
            .peers
            .get_mut(&channel)
            .filter(|_| self.store.is_some())
        else {
            return;
        };
        if entry.workspace != workspace {
            return;
        }
        let device = request.device;
        let learned = entry.peer.learn_request(request);
        tracing::debug!(%workspace, channel = %channel.0, ?device, ?learned, "channel access request taken");
        match learned {
            Ok(true) => entry.dirty = true,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %workspace, "channel access request refused"),
        }
    }

    fn reconcile(&mut self, channel: ChannelId, now: Instant, wall: u64) {
        let Some((workspace, route)) = self
            .peers
            .get(&channel)
            .map(|entry| (entry.workspace, entry.route.clone()))
        else {
            return;
        };
        let allowed = self
            .directory
            .allowed_devices(workspace, channel, route.as_ref(), wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        entry.dirty = false;
        // Without the channel's record this device cannot tell who may read
        // it: it commits nothing, and removes no one; nor does a reader.
        let Some(allowed) = allowed.filter(|_| store.role == Role::Participant) else {
            entry.turn_at = None;
            return;
        };
        // Members before this one in turn commit the change first; it
        // follows theirs, and commits only once their turns went by.
        let Some(turn) = entry.peer.turn(&allowed) else {
            entry.turn_at = None;
            return;
        };
        let waits = TURN.saturating_mul(u32::try_from(turn).unwrap_or(u32::MAX));
        let spaced = entry
            .committed_at
            .and_then(|at| at.checked_add(COMMIT_SPACING));
        let at = *entry.turn_at.get_or_insert_with(|| {
            now.checked_add(waits)
                .unwrap_or(now)
                .max(spaced.unwrap_or(now))
        });
        if now < at {
            return;
        }
        entry.turn_at = None;
        let outcome = match entry.peer.reconcile_now(&allowed, &store.vault) {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(%error, %workspace, "channel membership change failed");
                return;
            }
        };
        if !outcome.refused.is_empty() {
            tracing::warn!(%workspace, refused = outcome.refused.len(), "malformed channel access requests refused");
        }
        if outcome.changed() {
            tracing::debug!(%workspace, channel = %channel.0, admitted = ?outcome.admitted, removed = ?outcome.removed, "channel group changed");
            entry.committed_at = Some(now);
            let gate = entry.peer.gate();
            self.register_gate(channel, gate);
            self.bump(channel);
        }
    }

    /// Hand the transport every frame this device wrote or must publish
    /// again; kept for the next pass while the transport refuses one.
    fn publish(&mut self, channel: ChannelId, transport: &dyn GovernanceTransport) {
        let Some(entry) = self
            .peers
            .get_mut(&channel)
            .filter(|_| self.store.is_some())
        else {
            return;
        };
        if entry.peer.outbox().is_empty() {
            return;
        }
        let mut accepted = true;
        for frame in entry.peer.outbox() {
            tracing::debug!(workspace = %entry.workspace, channel = %channel.0, epoch = frame.epoch, "channel frame published");
            self.delivery = self.delivery.wrapping_add(1);
            accepted &= transport.publish(NeighbourMsg::MlsGovernance {
                workspace: entry.workspace.to_string(),
                frame: Box::new(frame.clone()),
                delivery: self.delivery,
            });
        }
        if accepted {
            entry.peer.published();
        }
    }

    /// Ask again at once for the channels this device still waits on: the
    /// policy moved, so a request that went unanswered under the one before
    /// it may be answered now, and the request is kept, signed, ready to go
    /// again.
    fn ask_waiting_now(&mut self, now: Instant) {
        for entry in self.peers.values_mut() {
            if entry.wanted && entry.peer.access() == Access::Waiting {
                entry.retry = Backoff::now(now, REQUEST_FIRST);
            }
        }
    }

    /// Ask the participants for access on the request backoff, while waiting.
    fn ask(
        &mut self,
        channel: ChannelId,
        transport: &dyn GovernanceTransport,
        now: Instant,
        wall: u64,
    ) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        let asking = match entry.peer.access() {
            Access::Waiting => entry.wanted,
            Access::Removed => entry.rejoin,
            Access::Ready { .. } => false,
        };
        if !asking || !entry.retry.is_due(now) || entry.signing.is_some() {
            return;
        }
        let fresh = now >= entry.fresh_at;
        if entry.request.is_none() || fresh {
            let request = match entry
                .peer
                .unsigned_access_request(wall, fresh, &store.vault)
            {
                Ok(request) => request,
                Err(error) => {
                    tracing::warn!(%error, "channel access request could not be prepared");
                    return;
                }
            };
            entry.request = None;
            entry.rejoin = false;
            if fresh {
                entry.fresh_at = now.checked_add(REQUEST_FRESH).unwrap_or(now);
            }
            let Some(mut request) = request else {
                return;
            };
            let Ok(message) = request.signing_bytes() else {
                return;
            };
            let signer = Arc::clone(&store.signer);
            let mut signing: SignFuture<'static> =
                Box::pin(async move { signer.sign(message).await });
            // A key that answers at once (a native one) signs in this pass; custody that answers
            // later (a page's) leaves the request for [`Self::signatures`].
            match signing
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            {
                std::task::Poll::Ready(Ok(signature)) => {
                    request.signature = collab_types::Signature::from_bytes(signature);
                    entry.request = Some(request);
                }
                std::task::Poll::Ready(Err(error)) => {
                    tracing::warn!(%error, "channel access request was not signed");
                    return;
                }
                std::task::Poll::Pending => {
                    entry.signing = Some((request, signing));
                    return;
                }
            }
        }
        let Some(request) = entry.request.clone() else {
            return;
        };
        entry.retry.fired(now, REQUEST_LONGEST);
        tracing::debug!(workspace = %entry.workspace, channel = %channel.0, device = ?request.device, issued = request.issued_at, "channel access asked");
        // Routed when the record is known, so participants decide Read on it.
        self.delivery = self.delivery.wrapping_add(1);
        let message = match entry.route.clone() {
            Some(route) => NeighbourMsg::MlsRoutedAccessRequest {
                room: entry.workspace.to_string(),
                request: Box::new(request),
                route: Box::new(route),
                delivery: self.delivery,
            },
            None => NeighbourMsg::MlsAccessRequest {
                workspace: entry.workspace.to_string(),
                request: Box::new(request),
                delivery: self.delivery,
            },
        };
        let _ = transport.publish(message);
    }

    /// Start taking part in `channel`: wait for access, and for the group to
    /// be created ([`Self::found`]). `Ok(false)` while the directory cannot
    /// name the channel's readers here: a workspace whose governance is unknown
    /// and which is no personal cluster of this device's.
    fn start_peer(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: Instant,
    ) -> channel_crypto::Result<bool> {
        if !takes_part(&*self.directory, self.device(), workspace, channel, route) {
            return Ok(false);
        }
        if self.open_peer(workspace, channel, now)? {
            return Ok(true);
        }
        let store = self.store.as_ref().ok_or_else(no_store)?;
        let signer = Box::new(Arc::clone(&store.signer));
        let peer = ChannelPeer::waiting(
            Some(store.state.peers(workspace)),
            channel,
            signer,
            &store.vault,
        )?;
        self.insert_peer(workspace, channel, peer, now);
        Ok(true)
    }

    /// Create `channel`'s group once no genesis of it can be on its way
    /// ([`ChannelDirectory::hosts`]) and the policy lets this device read the
    /// channel. A device that has seen a genesis never creates one.
    fn found(&mut self, channel: ChannelId, now: Instant, wall: u64) {
        let Some(entry) = self.peers.get(&channel) else {
            return;
        };
        if entry.peer.knows_group() || self.groups_seen.contains(&channel) || self.reads_only() {
            return;
        }
        let (workspace, route) = (entry.workspace, entry.route.clone());
        let hosted = self.directory.hosts(workspace);
        let due = if entry.wanted {
            hosted || entry.founding.answered
        } else {
            (hosted || entry.founding.unfounded) && now >= entry.founding.at
        };
        let reads = self
            .directory
            .allowed_devices(workspace, channel, route.as_ref(), wall)
            .zip(self.device())
            .is_some_and(|(allowed, device)| allowed.contains(&device));
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        // Looked at again on every pass, a policy re-check's included.
        entry.founding.allowed = reads;
        if !reads || !due {
            return;
        }
        match entry.peer.found_now(&store.vault) {
            Ok(true) => {
                // The requests it holds are admitted at once.
                entry.dirty = true;
                entry.wanted = true;
                let gate = entry.peer.gate();
                self.register_gate(channel, gate);
                self.bump(channel);
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %workspace, "channel group could not be created"),
        }
    }

    /// Open the state kept for `channel`. `Ok(false)` when there is none.
    fn open_peer(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        now: Instant,
    ) -> channel_crypto::Result<bool> {
        let store = self.store.as_ref().ok_or_else(no_store)?;
        let opened = match ChannelPeer::open(
            Some(store.state.peers(workspace)),
            channel,
            Box::new(Arc::clone(&store.signer)),
            &store.vault,
        ) {
            // A machine copied from another re-keyed its device: the state
            // names the device it was copied from, whose member it is, and
            // can never act for this one. It is dropped, never used, so this
            // device takes no leaf of its source's; it then joins as itself,
            // as any further device of the member does.
            Err(channel_crypto::Error::OtherDevice) => {
                tracing::info!(
                    %workspace,
                    "channel state named the device this machine was copied from; joining as this device"
                );
                store.state.peers(workspace).forget(channel)?;
                return Ok(false);
            }
            // State in a format no upgrade reaches (state an earlier build
            // saved is upgraded as it loads): authentic and this device's own,
            // but unreadable. Dropped, so the device can ask to join again;
            // what only this device held is gone, which is why it is an error.
            Err(channel_crypto::Error::OtherFormat) => {
                tracing::error!(
                    %workspace,
                    channel = %channel.0,
                    "channel state is in a format no upgrade reaches: dropping it and joining again; \
                     keys only this device held are lost"
                );
                store.state.peers(workspace).forget(channel)?;
                return Ok(false);
            }
            opened => opened?,
        };
        let Some(peer) = opened else {
            return Ok(false);
        };
        if let Some(from) = peer.upgraded_from() {
            tracing::info!(
                %workspace,
                channel = %channel.0,
                from,
                "channel state an earlier build saved was upgraded and kept in the current format"
            );
        }
        self.insert_peer(workspace, channel, peer, now);
        Ok(true)
    }

    fn insert_peer(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        peer: ChannelPeer,
        now: Instant,
    ) {
        let route = self
            .store
            .as_ref()
            .and_then(|store| store.state.route(workspace, channel));
        let gate = peer.gate();
        let found_at = now
            .checked_add(found_after(&peer.device(), channel))
            .unwrap_or(now);
        self.peers.insert(
            channel,
            PeerEntry {
                workspace,
                peer,
                route,
                request: None,
                signing: None,
                retry: Backoff::now(now, REQUEST_FIRST),
                fresh_at: now.checked_add(REQUEST_FRESH).unwrap_or(now),
                dirty: true,
                turn_at: None,
                committed_at: None,
                rejoin: false,
                founding: Founding {
                    at: found_at,
                    allowed: !self.reads_only(),
                    unfounded: false,
                    answered: false,
                },
                wanted: true,
                held: Vec::new(),
                welcome: None,
                unsaved_since: None,
            },
        );
        self.register_gate(channel, gate);
    }

    fn register_gate(&mut self, channel: ChannelId, gate: channel_crypto::ChannelGate) {
        if !gate.encryption_required || self.registered.get(&channel) == Some(&gate) {
            return;
        }
        self.gates.register_channel_gate(channel, gate);
        self.registered.insert(channel, gate);
    }

    /// Record that `channel`'s keys or access changed, and announce it.
    fn bump(&mut self, channel: ChannelId) {
        self.generation = self.generation.wrapping_add(1);
        self.changed.insert(channel, self.generation);
        self.changes.send_replace(self.generation);
    }
}

/// Why a channel of this device's own could not be opened: no secret store
/// keeps its state. Callers check for a store first.
fn no_store() -> std::io::Error {
    std::io::Error::other("no secret store keeps this device's channel state")
}

/// How long a pumping thread parks after a pass at `now`, given the
/// service's [`ChannelEncryption::next_pump_deadline`]: until the deadline,
/// or [`RECONCILE_INTERVAL`] when the pass left work due that it could not
/// do, so a failing step is retried at the cadence it always had instead of
/// spinning.
fn wait_after_pass(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|deadline| match deadline.checked_duration_since(now) {
        Some(wait) if !wait.is_zero() => wait,
        _ => RECONCILE_INTERVAL,
    })
}

/// Whether this device may take the channel up: the directory names it among
/// the channel's readers. A channel whose readers the directory names without
/// this device can be neither founded nor joined by it, and a channel whose
/// readers no one can name here (a workspace this device holds no view of) opens
/// nothing.
fn takes_part(
    directory: &dyn ChannelDirectory,
    device: Option<Ed25519PublicKey>,
    workspace: WorkspaceId,
    channel: ChannelId,
    route: Option<&ChannelRoute>,
) -> bool {
    directory
        .allowed_devices(workspace, channel, route, tilo_clock::unix_now())
        .zip(device)
        .is_some_and(|(allowed, device)| allowed.contains(&device))
}

/// Wakes a host's parked governance pump.
pub type PumpWake = Arc<dyn Fn() + Send + Sync>;

/// Cheap shared handle for the identity service and the broker.
#[derive(Clone)]
pub struct ChannelKeyAccess {
    inner: Arc<Mutex<ChannelEncryption>>,
    wake: Option<PumpWake>,
}

impl ChannelKeyAccess {
    pub fn new(service: ChannelEncryption) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
            wake: None,
        }
    }

    /// Run `operation` on the service; `None` when the lock is poisoned.
    pub(crate) fn run<T>(&self, operation: impl FnOnce(&mut ChannelEncryption) -> T) -> Option<T> {
        let mut service = self.inner.lock().ok()?;
        Some(operation(&mut service))
    }

    /// See [`ChannelEncryption::ask_on_entry`]; the custody's signature is awaited without
    /// holding the service.
    pub async fn ask_on_entry(
        &self,
        workspace: WorkspaceId,
        now: Instant,
        wall: u64,
    ) -> Option<MlsAccessRequestV1> {
        match self.run(|service| service.entry_request(workspace, now, wall))?? {
            entry::EntryRequest::Signed(request) => Some(request),
            entry::EntryRequest::Unsigned(scope, mut request, signing) => {
                let signature = signing
                    .await
                    .inspect_err(
                        |error| tracing::warn!(%error, "channel access request was not signed"),
                    )
                    .ok()?;
                request.signature = collab_types::Signature::from_bytes(signature);
                self.run(|service| service.entry_signed(scope, request, now))
            }
        }
    }

    /// See [`ChannelEncryption::scope_told`].
    pub fn scope_told(&self, workspace: WorkspaceId, route: &ChannelRoute, scope: ChannelId) {
        self.run(|service| service.scope_told(workspace, route, scope));
    }

    /// Forget `workspace`, deleted ([`ChannelEncryption::forget_workspace`]).
    ///
    /// # Errors
    /// What the store kept could not be removed, or the service is gone.
    pub fn forget_workspace(&self, workspace: WorkspaceId) -> Result<(), String> {
        self.run(|service| service.forget_workspace(workspace))
            .unwrap_or_else(|| Err("the channel service is gone".into()))
    }

    /// As [`Self::new`], for a host that parks its pump until it is poked:
    /// `wake` runs whenever an endpoint's access queues governance the next
    /// pump must send, so a first channel is announced or requested without
    /// waiting for a timer.
    pub fn with_wake(service: ChannelEncryption, wake: PumpWake) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
            wake: Some(wake),
        }
    }

    /// `route`'s channel's keys as this device holds them
    /// ([`ChannelEncryption::record_keys`]), never opening, joining or
    /// creating anything. `None` when this device takes part in no channel
    /// of the route's workspace; [`Err`] when it does and holds none yet.
    pub fn record_keys(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let unit = route.channel(&workspace.to_string());
        self.run(|service| {
            let scope = service
                .directory
                .unit_scope(workspace, route, tilo_clock::unix_now());
            service.record_keys(workspace, unit, scope)
        })
        .unwrap_or_else(|| Some(Err("the channel service is unavailable".into())))
    }

    /// See [`ChannelEncryption::access`]; also returns the service's
    /// generation, which changes whenever any channel's keys or access do.
    pub fn access(&self, workspace: WorkspaceId, channel: ChannelId) -> (ChannelAccess, u64) {
        let Some((access, generation, queued)) = self.run(|service| {
            let access = service.access(workspace, channel, tilo_clock::now());
            (
                access,
                service.generation(),
                std::mem::take(&mut service.queued),
            )
        }) else {
            return (ChannelAccess::Unavailable, 0);
        };
        if queued && let Some(wake) = &self.wake {
            wake();
        }
        (access, generation)
    }

    /// The keys of the group carrying the storage unit `unit`, as
    /// [`Self::access`] has them for a group, opened or joined when this
    /// device takes part and asked for when it does not.
    fn access_unit(&self, workspace: WorkspaceId, unit: ChannelId) -> (ChannelAccess, u64) {
        let Some((access, generation, queued)) = self.run(|service| {
            let access = service.access_unit(workspace, unit, tilo_clock::now());
            (
                access,
                service.generation(),
                std::mem::take(&mut service.queued),
            )
        }) else {
            return (ChannelAccess::Unavailable, 0);
        };
        if queued && let Some(wake) = &self.wake {
            wake();
        }
        (access, generation)
    }

    /// [`ChannelEncryption::signatures`] on the shared service, never holding it while the
    /// device's custody signs.
    pub async fn signatures(&self) {
        let Some(owed) = self.run(ChannelEncryption::take_signing) else {
            return;
        };
        let mut answered = Vec::with_capacity(owed.len());
        for (channel, request, signing) in owed {
            answered.push((channel, request, signing.await));
        }
        let queued = self.run(|service| {
            for (channel, request, signed) in answered {
                service.signed(channel, request, signed);
            }
            std::mem::take(&mut service.queued)
        });
        if queued == Some(true)
            && let Some(wake) = &self.wake
        {
            wake();
        }
    }

    /// Whether the device's custody owes a signature ([`ChannelEncryption::signing`]).
    pub fn signing(&self) -> bool {
        self.run(|service| service.signing()).unwrap_or(false)
    }

    /// See [`ChannelEncryption::save_pending`]: the state a frame taken in
    /// left unwritten, written before a write sealed under its keys leaves.
    pub fn save_pending(&self) {
        self.run(ChannelEncryption::save_pending);
    }

    /// See [`ChannelEncryption::access_record`]; also returns the service's
    /// generation, as [`Self::access`] does.
    pub fn access_record(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
    ) -> (ChannelAccess, u64) {
        let (_, access, generation) = self.record_access(workspace, route, tilo_clock::now());
        (access, generation)
    }

    /// [`Self::access_record`] at the caller's own `now`, with the channel the route's access
    /// is to.
    pub fn record_access(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        now: Instant,
    ) -> (ChannelId, ChannelAccess, u64) {
        // As `ChannelEncryption::access_record` asks: the route only when it
        // is the channel's.
        let Some((channel, access, generation, queued)) = self.run(|service| {
            let (channel, access) = service.access_record(workspace, route, now);
            (
                channel,
                access,
                service.generation(),
                std::mem::take(&mut service.queued),
            )
        }) else {
            return (
                route.channel(&workspace.to_string()),
                ChannelAccess::Unavailable,
                0,
            );
        };
        if queued && let Some(wake) = &self.wake {
            wake();
        }
        (channel, access, generation)
    }

    /// Open `envelope`, a new sealed write to one of `workspace`'s channels, for
    /// the durable owner that holds the channel's keys. Only under the
    /// channel's current epoch, and only while this device reads the
    /// channel; anything else is refused. Call it only once the envelope's
    /// author, role, route and signature were verified.
    pub fn open_new_write(
        &self,
        workspace: WorkspaceId,
        envelope: &SignedOp,
    ) -> Result<Vec<u8>, String> {
        let (keys, metadata, ciphertext) = self.sealed_parts(workspace, envelope)?;
        keys.open_current_content(envelope.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    fn sealed_parts<'a>(
        &self,
        workspace: WorkspaceId,
        envelope: &'a SignedOp,
    ) -> Result<(ChannelContentKeys, EncryptionMetadata, Cow<'a, [u8]>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let ciphertext =
            collab_types::sealed_ciphertext(envelope).map_err(|error| error.to_string())?;
        let (ChannelAccess::Ready(keys), _) = self.access_unit(workspace, metadata.channel) else {
            return Err("this device holds no key for the write's channel".into());
        };
        Ok((keys, metadata, ciphertext))
    }

    /// The payload a renewal of `envelope`, a sealed write of this device
    /// to one of `workspace`'s channels, carries under a fresh timestamp and
    /// sequence: the identical ciphertext while its epoch is the channel's
    /// current one, else its content re-sealed under the current epoch. The
    /// plaintext never leaves this call.
    pub fn renew_sealed(
        &self,
        workspace: WorkspaceId,
        envelope: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let retention = metadata
            .retention()
            .ok_or("the write is not sealed with a content key")?;
        let (ChannelAccess::Ready(keys), _) = self.access_unit(workspace, metadata.channel) else {
            return Err("this device holds no key for the write's channel".into());
        };
        let plaintext = keys
            .open_content(envelope.artifact, &metadata, &envelope.payload)
            .map_err(|error| error.to_string())?;
        if metadata.epoch == keys.epoch() {
            return Ok((metadata, envelope.payload.clone()));
        }
        // The state carrying the epoch's key is written before the write
        // sealed under it goes out ([`ChannelEncryption::save_pending`]).
        self.save_pending();
        keys.seal_content(envelope.artifact, retention, &plaintext)
            .map_err(|error| error.to_string())
    }

    /// See [`ChannelEncryption::pump`].
    pub fn pump(&self, transport: &dyn GovernanceTransport) {
        let _ = self.pump_until_due(transport);
    }

    /// [`Self::pump`] at the caller's own `now`, for a host that keeps a
    /// clock of its own.
    pub fn pump_at(&self, transport: &dyn GovernanceTransport, now: Instant) {
        self.run(|service| service.pump(transport, now, tilo_clock::unix_now()));
    }

    /// [`Self::pump`], then how long until the protocol next has work that
    /// no message brings ([`ChannelEncryption::next_pump_deadline`]); `None`
    /// while it has none, and the caller parks until a governance message
    /// arrives or the wake given to [`Self::with_wake`] fires.
    ///
    /// Work this pass found due and still left due (an access request that
    /// could not be prepared) is retried after `RECONCILE_INTERVAL`, the
    /// cadence every pass had while the caller woke on a fixed timer.
    #[must_use]
    pub fn pump_until_due(&self, transport: &dyn GovernanceTransport) -> Option<Duration> {
        let inbound = transport.take_inbound();
        let now = tilo_clock::now();
        let deadline = self.run(|service| {
            service.pump_inbound(inbound, transport, now, tilo_clock::unix_now());
            service.next_pump_deadline(now)
        })?;
        wait_after_pass(deadline, now)
    }

    /// Arm `watch` (whose wake pokes this host's pump) when groups wait for
    /// a policy announcement, so a revocation, a role change or an expiry
    /// re-checks them with no timer running. Returns `false` when a change
    /// already arrived that the arming will not wake for: the caller pumps
    /// again now instead of parking.
    ///
    /// A device taking part in no channel never arms it, and so sleeps
    /// through every announcement.
    pub fn arm_policy_watch(&self, watch: &tilo_identity_host::policy_change::Watch) -> bool {
        let Ok(service) = self.inner.lock() else {
            return true;
        };
        if !service.waits_for_policy_change() {
            return true;
        }
        let _ = watch.arm();
        let unchanged = service.directory.policy_epoch() == Some(service.reconcile.epoch());
        drop(service);
        unchanged
    }

    /// See [`ChannelEncryption::generation`].
    pub fn generation(&self) -> u64 {
        self.inner.lock().map_or(0, |service| service.generation())
    }

    /// See [`ChannelEncryption::channel_change`].
    pub fn channel_change(&self, channel: ChannelId) -> u64 {
        self.inner
            .lock()
            .map_or(0, |service| service.channel_change(channel))
    }

    /// Wakes whenever any channel's keys or access change; `None` when the
    /// service is unavailable after a poisoned lock.
    pub fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.inner
            .lock()
            .ok()
            .map(|service| service.changes.subscribe())
    }
}

/// Where the identity service finds this session's channel encryption
/// service. The broker that owns the service can start after the identity
/// service (onboarding defers it), so the slot is filled when it does.
#[derive(Clone, Default)]
pub struct ChannelKeyProvider {
    slot: Arc<Mutex<Option<ChannelKeyAccess>>>,
    /// Counts [`Self::set`] calls, for whoever waits on the service.
    installed: Arc<tokio::sync::watch::Sender<u64>>,
}

impl ChannelKeyProvider {
    /// The slot of this session: its compositor starts one collaboration
    /// broker and one identity service, which meet here.
    pub fn session() -> Self {
        #[cfg(not(target_family = "wasm"))]
        {
            static SESSION: std::sync::OnceLock<ChannelKeyProvider> = std::sync::OnceLock::new();
            SESSION.get_or_init(Self::default).clone()
        }
        #[cfg(target_family = "wasm")]
        {
            // A page's one thread: the slot is the page's own, not a shared static.
            thread_local! {
                static SESSION: ChannelKeyProvider = ChannelKeyProvider::default();
            }
            SESSION.with(Clone::clone)
        }
    }

    /// Point the slot at a broker's service (or at none).
    pub fn set(&self, access: Option<ChannelKeyAccess>) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = access;
        }
        self.installed
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// The current service, if a broker with one is running.
    pub fn get(&self) -> Option<ChannelKeyAccess> {
        self.slot.lock().ok().and_then(|slot| slot.clone())
    }

    /// [`ChannelKeyAccess::generation`] of the service installed now, 0
    /// while none is: what a first [`Self::changed`] waits from.
    pub fn generation(&self) -> u64 {
        self.get().map_or(0, |access| access.generation())
    }

    /// Wait until any channel's keys or this device's access to them may have
    /// changed since `after`, the value the previous wait returned (or
    /// [`Self::generation`] read before looking at the keys), and return the
    /// value to wait from next. As [`Self::channel_changed`], for every
    /// channel at once.
    pub async fn changed(&self, after: u64) -> u64 {
        self.changed_where(after, ChannelKeyAccess::generation)
            .await
    }

    /// Wait until `channel`'s keys or this device's access to them may have
    /// changed since `after`, the value the previous wait returned (0 at
    /// first), and return the value to wait from next.
    ///
    /// Resolves at once when the channel changed since `after`; otherwise
    /// when it next changes, or when the service is installed, replaced or
    /// goes away. It reports only that this one channel changed, nothing
    /// about its keys or any other channel: the endpoint then asks the
    /// identity service, which checks the device's trust and the person's
    /// role on every answer. Costs no wakeup while nothing changes.
    pub async fn channel_changed(&self, channel: ChannelId, after: u64) -> u64 {
        self.changed_where(after, |access| access.channel_change(channel))
            .await
    }

    /// Wait until `current` of the installed service differs from `after`,
    /// or the service is installed, replaced or goes away.
    async fn changed_where(
        &self,
        after: u64,
        current: impl Fn(&ChannelKeyAccess) -> u64 + Send,
    ) -> u64 {
        // Subscribed before looking, so an installation between the two is
        // still seen.
        let mut installed = self.installed.subscribe();
        let Some((access, mut changes)) = self
            .get()
            .and_then(|access| access.changes().map(|changes| (access, changes)))
        else {
            // No service: nothing changes until one is installed. The
            // installation counter lives as long as `self`.
            let _ = installed.changed().await;
            return self.get().map_or(0, |access| current(&access));
        };
        loop {
            let change = current(&access);
            if change != after {
                return change;
            }
            tokio::select! {
                outcome = changes.changed() => {
                    if outcome.is_err() {
                        // The service was dropped: whatever replaces it
                        // answers differently.
                        return change.wrapping_add(1);
                    }
                }
                _ = installed.changed() => return self.get().map_or(0, |access| current(&access)),
            }
        }
    }

    /// [`Self::record_keys`] as a workspace host running on this device asks
    /// for them, by the workspace's id as its records name it: opening the
    /// channel when it has none yet and `create`, and waiting up to `wait` for
    /// the group. `None` when this device takes no part in the workspace's
    /// channels; [`Err`] when it takes part and holds no keys yet.
    pub async fn channel_keys(
        &self,
        workspace: &str,
        route: &ChannelRoute,
        create: bool,
        wait: Duration,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let workspace = WorkspaceId::parse(workspace)?;
        if !create {
            let access = self.get()?;
            // Asked off the caller's task: the service takes its lock and
            // reads its state on that thread, never on the host's.
            #[cfg(not(target_family = "wasm"))]
            {
                let route = route.clone();
                return tokio::task::spawn_blocking(move || access.record_keys(workspace, &route))
                    .await
                    .unwrap_or_else(|error| Some(Err(error.to_string())));
            }
            // A page has one thread: the read runs on it.
            #[cfg(target_family = "wasm")]
            return access.record_keys(workspace, route);
        }
        self.open_record(workspace, route, wait).await
    }

    /// `route`'s channel's keys, opening the channel when it has none yet
    /// ([`ChannelEncryption::access_record`]), and waiting up to `wait` for
    /// the group when the directory backs this device but no genesis has
    /// arrived or been admitted yet. An endpoint writing a record's first
    /// content asks this: the group is created within [`founding_wait`], and
    /// the write lands with its keys rather than failing.
    ///
    /// `None` when this device takes part in no channel of the route's
    /// workspace; [`Err`] when it waited and the keys still are not there,
    /// saying how long it waited.
    pub async fn open_record(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        wait: Duration,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let channel = route.channel(&workspace.to_string());
        let deadline = tilo_clock::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        loop {
            let access = self.get()?;
            // Read before the ask: a change between the two is no wait at all.
            let after = access.channel_change(channel);
            let (state, _) = access.access_record(workspace, route);
            match state {
                ChannelAccess::Ready(keys) => return Some(Ok(keys)),
                ChannelAccess::Waiting => {}
                ChannelAccess::Removed => {
                    return Some(Err(
                        "this device was removed from the channel's group".into()
                    ));
                }
                ChannelAccess::Unavailable | ChannelAccess::NoSecretStore => return None,
            }
            let remaining = deadline.saturating_duration_since(tilo_clock::now());
            if remaining.is_zero() {
                return Some(Err(not_ready(wait)));
            }
            let changed = tokio::time::timeout(remaining, self.channel_changed(channel, after));
            if changed.await.is_err() {
                return Some(Err(not_ready(wait)));
            }
        }
    }
}

/// What a caller waiting `wait` for a channel's keys is told when they never
/// arrived: how long it waited, so whoever reads it knows what the wait was
/// and that nothing more is coming.
fn not_ready(wait: Duration) -> String {
    format!(
        "the channel's keys did not arrive within {}s",
        wait.as_secs()
    )
}

/// The identity service's route keys, from this host's channel service once
/// one runs.
impl tilo_identity_host::sources::ChannelKeySource for ChannelKeyProvider {
    fn access(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        known_generation: u64,
    ) -> Option<Result<tilo_identity_host::sources::ChannelKeys, String>> {
        use runtime_identity::wire::ChannelAccess as Wire;
        let channels = self.get()?;
        let (access, generation) = channels.access_record(workspace, route);
        // A caller holding no keys asks with generation 0, which is also
        // the generation of a service that has changed nothing since it
        // started (a channel it restored from disk, after a restart): it is
        // sent the keys, never told they are unchanged.
        let held = known_generation != 0 && generation == known_generation;
        Some(match access {
            ChannelAccess::Ready(keys) if !held => keys
                .to_bytes()
                .map(|bytes| (Wire::Ready, generation, bytes.to_vec()))
                .map_err(|error| error.to_string()),
            ChannelAccess::Ready(_) => Ok((Wire::Ready, generation, Vec::new())),
            ChannelAccess::Waiting => Ok((Wire::Waiting, generation, Vec::new())),
            ChannelAccess::Removed => Ok((Wire::Removed, generation, Vec::new())),
            ChannelAccess::Unavailable => Ok((Wire::Unavailable, generation, Vec::new())),
            ChannelAccess::NoSecretStore => Ok((Wire::NoSecretStore, generation, Vec::new())),
        })
    }
}

mod entry;

#[cfg(test)]
mod tests {
    include!("encryption_tests.rs");
}
