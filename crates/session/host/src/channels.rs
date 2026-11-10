//! The browser as a participant device of `channel-crypto` channels.
//!
//! Every route of a workspace seals its content under the content keys of a
//! key scope ([`collab_types::workspace_scope`]): the workspace's participants
//! share one group and one keyring for every record whose readers are the
//! workspace's, and a record a Read Deny narrows keeps a group of its own.
//! The page takes part in the scope's group exactly as a native device does:
//!
//! - it reads each route's scope and readers off its replica of the
//!   workspace's governance ([`crate::replica`]), the same decision a device
//!   makes ([`tilo_peer_state::PeerGovernance::unit_scope`]), and asks that
//!   group's participants for access, with a request carrying a fresh key
//!   package, signed by its device key (which stays in page custody), re-asking
//!   with backoff until a Welcome arrives (with a fresh member key once an
//!   answer is long overdue);
//! - it applies the commits any participant writes (Welcome with the key
//!   history, then commits), keeps of two commits on one epoch the one with
//!   the lower digest, and hands the session each unit's keys — named for the
//!   unit, taken from the group carrying it — whenever they change;
//! - it checks every commit against the workspace's governance as its replica of
//!   the workspace admits it ([`crate::replica`]): a commit may add only a device
//!   the governance lets read the group, the workspace's governor or this
//!   device, and must be signed by one of them, the participants a native
//!   device admits ([`tilo_peer_state::PeerGovernance::channel_readers`]).
//!
//! **One group per channel, created once.** Whoever holds a group's state and
//! sees no genesis creates it; every other participant waits, asks, and joins
//! by the Welcome that admits it — a page never creates a group. Two geneses
//! made at once reconcile like two commits: every device keeps the one with
//! the lower digest, and a member of the other starts over and joins the
//! winner ([`channel_crypto::peer`]). A page that already held a group a
//! record's channel was given before scopes carries the same rules: its
//! session drops the group for an epoch no participant seals under and joins
//! the one the workspace carries, whose Welcome brings the older content keys
//! along.
//!
//! The page writes no commits itself: whether a device may read a record
//! depends on Read grants over that record, which the page does not hold,
//! so it leaves admitting and removing to the native participants, which
//! do. Nothing this page writes ever leaves in the clear: a route's writes
//! are held until this device holds the route's content keys. What a record
//! held in the clear before it sealed is read as its history, as native
//! devices read it ([`document_history_admission`]): a page joining after
//! the seal takes that base in first. The host carries every message as
//! retained, opaque governance and never holds a key. Channel state is
//! exported for the profile's durable store, so a reloaded page keeps its
//! access instead of asking again. It is sealed under a vault key the store
//! keeps apart from it: in the page, wrapped by a non-extractable
//! `WebCrypto` key.

use std::collections::{BTreeSet, HashMap, HashSet};
use tilo_sort::CompactSort as _;

use channel_crypto::{Access, ChannelContentKeys, ChannelPeer, PeerEvent, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceFrame};

use tilo_peer_state::document_frame::DocumentRoute;

/// First and longest wait before a waiting device asks again. An answer
/// takes the request's turn in the host's order and then a participant's
/// commit's: in a public workspace a crowd is joining, seconds. Every ask is a
/// retained write the host fans out to the whole workspace, so asking again
/// before an answer could arrive only lengthened that wait.
const REQUEST_FIRST_MS: u64 = 8_000;
const REQUEST_LONGEST_MS: u64 = 120_000;
/// The wait from which a device asks with a fresh member key: the host
/// keeps only a channel's newest commit, so an admission replaced before it
/// reached this page is gone, and the participants see the device admitted
/// with the key it asked with. A fresh key makes them admit it again, a
/// change every member applies: asked sooner, in a crowd whose admissions
/// took longer to arrive, it replaced them faster than they arrived.
const REQUEST_FRESH_MS: u64 = 60_000;
/// Frames kept for a channel no route of this page has opened yet, and for
/// an open one whose signer the workspace's governance does not admit yet.
const MAX_UNOPENED_FRAMES: usize = 256;
/// Channels whose frames are kept before a route opens them.
const MAX_UNOPENED_CHANNELS: usize = 256;
/// Routes opened before the workspace's governance could say which group
/// carries them, kept until it can.
const MAX_DEFERRED_ROUTES: usize = 256;

/// What the workspace's governance says now about a route's content: the
/// group whose content keys carry it and the devices that group admits.
pub(crate) struct RecordPolicy {
    /// The route's storage unit: the channel its content is sealed under,
    /// named for the unit while the keys stay the scope's.
    pub unit: ChannelId,
    /// The group whose keys carry the unit: the workspace's key scope, or the
    /// unit itself when a Read Deny narrows its readers.
    pub scope: ChannelId,
    /// The devices that may sign and be added to the group.
    pub allowed: BTreeSet<Ed25519PublicKey>,
}

/// `route`'s policy as the workspace's governance says now; `None` while this
/// page holds no governance of the route's workspace, or none that decides
/// the route's readers yet.
pub(crate) type Policy<'a> = dyn Fn(&DocumentRoute) -> Option<RecordPolicy> + 'a;

/// A governance message this device has to sign and send: the device key
/// is in page custody, so signing happens in the session.
pub(crate) enum Outgoing {
    /// Ask the participants for access. `route` names the record the group
    /// carries when the group is the unit's own, so they can decide Read on
    /// it; a workspace-wide key scope is read by every participant and its
    /// request names no record.
    Request {
        workspace_id: u64,
        cluster_id: String,
        request: MlsAccessRequestV1,
        route: Option<collab_types::ChannelRoute>,
    },
}

/// What changed for the session after channel state moved.
#[derive(Default)]
pub(crate) struct Changes {
    /// Routes whose content keys changed, with the new keys.
    pub keys: Vec<(DocumentRoute, ChannelContentKeys)>,
    /// Messages to sign and send.
    pub outgoing: Vec<Outgoing>,
}

impl Changes {
    /// Whether nothing changed.
    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.outgoing.is_empty()
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.keys.extend(other.keys);
        self.outgoing.extend(other.outgoing);
    }
}

/// The verdict on an op for `route` from the gate of the page's state for
/// its channel, through the one gate a broker checks too
/// ([`channel_crypto::admission`]). A page that holds the channel's content
/// keys (`sealed`) knows the channel requires them, as a broker that
/// registered its gate does. `governance_known`: the host answered `Ready`
/// for the route's workspace, after the governance it holds, so a channel the
/// page has no state of requires nothing; before that, cleartext waits.
pub fn document_admission(
    gate: Option<channel_crypto::ChannelGate>,
    governance_known: bool,
    sealed: bool,
    route: &DocumentRoute,
    encryption: Option<&collab_types::EncryptionMetadata>,
) -> collab_types::inbound::OpAdmission {
    let keys = sealed.then_some(channel_crypto::ChannelGate {
        encryption_required: true,
    });
    channel_crypto::admission(
        gate.into_iter().chain(keys),
        governance_known,
        Channels::channel(route),
        encryption,
    )
}

/// [`document_admission`] for an op the host is replaying from a record's
/// retained history, as a broker decides one with
/// [`tilo_action_broker::collab_core::BrokerChannels::document_history_allowed`]:
/// a record's cleartext writes stay the base its sealed writes are changes
/// over, so a device that joins the record after it sealed reads it by taking
/// that base in first. Cleartext history is admitted once the workspace's
/// governance is known, whatever the page holds of the channel; a live
/// cleartext write is still refused ([`document_admission`]).
pub fn document_history_admission(
    gate: Option<channel_crypto::ChannelGate>,
    governance_known: bool,
    sealed: bool,
    route: &DocumentRoute,
    encryption: Option<&collab_types::EncryptionMetadata>,
) -> collab_types::inbound::OpAdmission {
    if encryption.is_none() {
        return if governance_known {
            collab_types::inbound::OpAdmission::CleartextAdmitted
        } else {
            collab_types::inbound::OpAdmission::Unstated
        };
    }
    document_admission(gate, governance_known, sealed, route, encryption)
}

/// The page's signer as its channel state sees it: the device key stays in
/// page custody and signs in the session, and the page writes no commits.
struct PageCustody([u8; 32]);

impl channel_crypto::DeviceSigner for PageCustody {
    fn device_public_key(&self) -> [u8; 32] {
        self.0
    }

    fn sign(&self, _: Vec<u8>) -> channel_crypto::SignFuture<'_> {
        Box::pin(async {
            Err(collab_types::signer::SignError(
                "this page writes no channel commits".into(),
            ))
        })
    }
}

/// One storage unit whose content a group's keys carry: the route its
/// content is sealed under, and the epoch last handed to the session.
struct Unit {
    document: DocumentRoute,
    installed_epoch: Option<u64>,
}

/// One group this page takes part in: a key scope and the units its keys
/// carry.
struct Group {
    /// A route the group carries: its workspace names the channel governance
    /// the group belongs to, and the policy its frames are checked against.
    document: DocumentRoute,
    workspace_id: u64,
    /// The record this group is the unit's own for, when a Read Deny narrows
    /// it: participants then decide Read on it, and the request says so.
    /// `None` for a workspace-wide scope, read by every participant.
    route: Option<collab_types::ChannelRoute>,
    peer: ChannelPeer,
    /// Frames refused for a signer the workspace's governance does not admit
    /// yet, applied again when it changes.
    early: Vec<MlsGovernanceFrame>,
    /// The pending request, re-sent until a participant answers it.
    request: Option<MlsAccessRequestV1>,
    next_request_ms: u64,
    request_delay_ms: u64,
    units: HashMap<ChannelId, Unit>,
}

/// A channel's state as [`Channels::export_if_changed`] hands it to the
/// durable store.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedMember {
    pub channel: ChannelId,
    pub sealed: Vec<u8>,
}

/// This page's channel state for the durable store, sealed under the
/// store's vault ([`crate::PeerStore::channel_vault`]), which is kept apart
/// from it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedChannels {
    pub members: Vec<SavedMember>,
}

/// Every channel this page takes part in.
pub(crate) struct Channels {
    device: Ed25519PublicKey,
    vault: VaultKey,
    /// The groups this page takes part in, by key scope.
    groups: HashMap<ChannelId, Group>,
    /// The group each opened unit's content is carried by.
    scopes: HashMap<ChannelId, ChannelId>,
    /// Routes opened before the workspace's governance could say which group
    /// carries them.
    deferred: Vec<DocumentRoute>,
    /// Frames kept for a group no route of this page has opened yet.
    unopened: HashMap<ChannelId, Vec<MlsGovernanceFrame>>,
    /// Restored group state, waiting for its group to open.
    saved: HashMap<ChannelId, SavedMember>,
    saved_requests: HashMap<ChannelId, MlsAccessRequestV1>,
    /// Content keys a host handed this session for a record whose group it
    /// is not admitted to itself ([`Self::install_handed`]), by unit.
    handed: HashMap<ChannelId, ChannelContentKeys>,
    dirty: bool,
}

impl Channels {
    /// Channel state for `device`, sealed at rest under `vault`.
    pub(crate) fn new(device: [u8; 32], vault: [u8; 32]) -> Self {
        Self {
            device: Ed25519PublicKey::from_bytes(device),
            vault: VaultKey::new(vault),
            groups: HashMap::new(),
            scopes: HashMap::new(),
            deferred: Vec::new(),
            unopened: HashMap::new(),
            saved: HashMap::new(),
            saved_requests: HashMap::new(),
            handed: HashMap::new(),
            dirty: false,
        }
    }

    /// Pick up where a previous page left off.
    pub(crate) fn restore(&mut self, saved: SavedChannels) {
        self.saved = saved
            .members
            .into_iter()
            .map(|member| (member.channel, member))
            .collect();
    }

    /// Restore requests with their sealed state, so a reload cannot race an
    /// earlier request with an unnecessary replacement key package.
    pub(crate) fn restore_requests(&mut self, requests: Vec<MlsAccessRequestV1>) {
        self.saved_requests = requests
            .into_iter()
            .filter(|request| {
                request.device == self.device && self.saved.contains_key(&request.channel)
            })
            .map(|request| (request.channel, request))
            .collect();
    }

    pub(crate) fn pending_requests(&self) -> Vec<MlsAccessRequestV1> {
        let mut requests = self.saved_requests.clone();
        for (channel, group) in &self.groups {
            if let Some(request) = &group.request
                && matches!(group.peer.access(), Access::Waiting)
            {
                requests.insert(*channel, request.clone());
            } else {
                requests.remove(channel);
            }
        }
        let mut requests: Vec<_> = requests.into_values().collect();
        requests.compact_sort_by_key(|request| request.channel.0);
        requests
    }

    /// Have the next [`Self::export_if_changed`] export.
    pub(crate) fn mark_changed(&mut self) {
        self.dirty = true;
    }

    /// This page's channel state for the durable store, when it changed
    /// since the last call.
    pub(crate) fn export_if_changed(&mut self) -> Option<SavedChannels> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let mut members: HashMap<ChannelId, SavedMember> = self.saved.clone();
        for (channel, group) in &self.groups {
            if let Ok(sealed) = group.peer.export_sealed(&self.vault) {
                members.insert(
                    *channel,
                    SavedMember {
                        channel: *channel,
                        sealed,
                    },
                );
            }
        }
        let mut members: Vec<SavedMember> = members.into_values().collect();
        members.compact_sort_by_key(|member| member.channel.0);
        Some(SavedChannels { members })
    }

    /// The route's storage unit: the channel its content is sealed under.
    /// The keys are its key scope's, named for the unit.
    pub(crate) fn channel(route: &DocumentRoute) -> ChannelId {
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record)
    }

    /// The content keys this device holds for the unit `channel`, once its
    /// access is ready: what content sealed for the unit is opened with.
    ///
    /// Keys a host device handed over for the unit are the workspace's own
    /// and speak first; the group carrying the unit speaks when no such keys
    /// arrived.
    pub(crate) fn content_keys(&self, channel: ChannelId) -> Option<ChannelContentKeys> {
        self.handed.get(&channel).cloned().or_else(|| {
            let scope = *self.scopes.get(&channel)?;
            self.groups
                .get(&scope)?
                .peer
                .content_keys()
                .map(|keys| keys.for_unit(channel))
        })
    }

    /// Hold `keys` for `route`'s unit, as a host device hands them over: they
    /// are the workspace's own keys for the record, named for its unit.
    pub(crate) fn install_handed(&mut self, route: &DocumentRoute, keys: ChannelContentKeys) {
        self.handed.insert(Self::channel(route), keys);
    }

    /// The verdict on an op for `route` from this page's state for the group
    /// carrying it ([`document_admission`], or [`document_history_admission`]
    /// for what the host replays).
    pub(crate) fn admission(
        &self,
        route: &DocumentRoute,
        (governance_known, sealed): (bool, bool),
        retained: bool,
        encryption: Option<&collab_types::EncryptionMetadata>,
    ) -> collab_types::inbound::OpAdmission {
        let gate = self
            .scopes
            .get(&Self::channel(route))
            .and_then(|scope| self.groups.get(scope))
            .map(|group| group.peer.gate())
            .or_else(|| {
                self.groups
                    .get(&Self::channel(route))
                    .map(|group| group.peer.gate())
            });
        if retained {
            document_history_admission(gate, governance_known, sealed, route, encryption)
        } else {
            document_admission(gate, governance_known, sealed, route, encryption)
        }
    }

    /// Open what waited for the workspace's governance to name its group, and
    /// apply again the frames held for a signer it does not admit yet.
    pub(crate) fn retry(&mut self, policy: &Policy<'_>, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        for route in std::mem::take(&mut self.deferred) {
            changes.extend(self.open(&route, policy, now_ms));
        }
        let scopes: Vec<ChannelId> = self
            .groups
            .iter()
            .filter(|(_, group)| !group.early.is_empty())
            .map(|(scope, _)| *scope)
            .collect();
        for scope in scopes {
            let early = self
                .groups
                .get_mut(&scope)
                .map(|group| std::mem::take(&mut group.early))
                .unwrap_or_default();
            for frame in early {
                self.apply(scope, frame, policy, now_ms, &mut changes);
            }
        }
        changes
    }

    /// Take part in the group carrying `route`'s content from now on.
    pub(crate) fn open(
        &mut self,
        route: &DocumentRoute,
        policy: &Policy<'_>,
        now_ms: u64,
    ) -> Changes {
        let mut changes = Changes::default();
        let Ok(workspace_id) = route.workspace_id() else {
            return changes;
        };
        let Some(decided) = policy(route) else {
            // Which group carries the route, and who reads it, is unknown
            // until the workspace's governance arrives: asking then could ask
            // for a group no participant seals under.
            if !self.deferred.contains(route) && self.deferred.len() < MAX_DEFERRED_ROUTES {
                self.deferred.push(route.clone());
            }
            return changes;
        };
        let (unit, scope) = (decided.unit, decided.scope);
        // A record whose keys a host handed over needs no group of this
        // page's own.
        if self.handed.contains_key(&unit) {
            return changes;
        }
        self.scopes.insert(unit, scope);
        if self.groups.contains_key(&scope) {
            if let Some(group) = self.groups.get_mut(&scope) {
                group.units.entry(unit).or_insert_with(|| Unit {
                    document: route.clone(),
                    installed_epoch: None,
                });
            }
            self.after(scope, None, now_ms, &mut changes);
            return changes;
        }
        let signer = || Box::new(PageCustody(*self.device.as_bytes()));
        let restored = self.saved.remove(&scope).and_then(|saved| {
            ChannelPeer::restore(scope, signer(), &saved.sealed, &self.vault).ok()
        });
        let request = restored
            .as_ref()
            .filter(|peer| matches!(peer.access(), Access::Waiting))
            .and_then(|_| self.saved_requests.remove(&scope));
        let peer = match restored {
            Some(peer) => peer,
            None => match ChannelPeer::waiting(None, scope, signer(), &self.vault) {
                Ok(peer) => peer,
                Err(_) => return changes,
            },
        };
        let mut units = HashMap::new();
        units.insert(
            unit,
            Unit {
                document: route.clone(),
                installed_epoch: None,
            },
        );
        self.groups.insert(
            scope,
            Group {
                document: route.clone(),
                workspace_id,
                route: (scope == unit).then(|| {
                    collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str())
                }),
                peer,
                early: Vec::new(),
                request,
                next_request_ms: now_ms,
                request_delay_ms: REQUEST_FIRST_MS,
                units,
            },
        );
        self.deferred.retain(|waiting| waiting != route);
        self.dirty = true;
        for frame in self.unopened.remove(&scope).unwrap_or_default() {
            self.apply(scope, frame, policy, now_ms, &mut changes);
        }
        self.after(scope, None, now_ms, &mut changes);
        changes
    }

    /// Apply one governance frame of `workspace` the host delivered.
    pub(crate) fn receive_frame(
        &mut self,
        workspace: &str,
        frame: MlsGovernanceFrame,
        policy: &Policy<'_>,
        now_ms: u64,
    ) -> Changes {
        let mut changes = Changes::default();
        let channel = frame.channel;
        if !self.groups.contains_key(&channel) {
            if self.unopened.len() < MAX_UNOPENED_CHANNELS || self.unopened.contains_key(&channel) {
                let frames = self.unopened.entry(channel).or_default();
                if frames.len() < MAX_UNOPENED_FRAMES {
                    frames.push(frame);
                }
            }
            return changes;
        }
        if self
            .groups
            .get(&channel)
            .is_some_and(|group| group.document.cluster_id == workspace)
        {
            self.apply(channel, frame, policy, now_ms, &mut changes);
        }
        changes
    }

    fn apply(
        &mut self,
        channel: ChannelId,
        frame: MlsGovernanceFrame,
        policy: &Policy<'_>,
        now_ms: u64,
        changes: &mut Changes,
    ) {
        let Some(group) = self.groups.get_mut(&channel) else {
            return;
        };
        // A commit of a workspace whose governance this page does not hold yet,
        // or one naming a device that governance does not admit yet, waits for
        // the governance to change. A group the governance no longer carries
        // for this route waits too: the route's group is another one now.
        let decided = policy(&group.document).filter(|decided| decided.scope == channel);
        let Some(decided) = decided else {
            if group.early.len() < MAX_UNOPENED_FRAMES {
                group.early.push(frame);
            }
            return;
        };
        let allowed = decided.allowed;
        let unknown = std::iter::once(&frame.signed_by)
            .chain(frame.added())
            .any(|device| !allowed.contains(device));
        match group.peer.receive(frame.clone(), &allowed, &self.vault) {
            Ok(event) => self.after(channel, Some(event), now_ms, changes),
            Err(_) if unknown => {
                if group.early.len() < MAX_UNOPENED_FRAMES {
                    group.early.push(frame);
                }
            }
            Err(_) => {}
        }
    }

    /// One line per group for diagnostics: where each group's membership
    /// stands and the units its keys carry.
    pub(crate) fn describe(&self) -> String {
        let mut lines = Vec::new();
        for (channel, group) in &self.groups {
            let access = match group.peer.access() {
                Access::Ready { .. } => "ready".to_owned(),
                other => format!("{other:?}"),
            };
            let units: Vec<String> = group
                .units
                .iter()
                .filter(|(unit, _)| !self.handed.contains_key(unit))
                .map(|(_, unit)| format!("{}/{}", unit.document.app_id, unit.document.record))
                .collect();
            lines.push(format!(
                "group {channel:?} ({}): {access}; request {}; early {}; units {}",
                group.document.cluster_id,
                group.request.is_some(),
                group.early.len(),
                units.join(", ")
            ));
            // Each unit's own line: a route's state is what a reader of the
            // status looks for, whatever group carries it.
            for unit in group.units.values() {
                lines.push(format!(
                    "route {}/{} {channel:?}: {access};",
                    unit.document.app_id, unit.document.record
                ));
            }
        }
        for (channel, frames) in &self.unopened {
            lines.push(format!("unopened {channel:?}: {} frames", frames.len()));
        }
        if !self.deferred.is_empty() {
            lines.push(format!("deferred: {} routes", self.deferred.len()));
        }
        lines.join("\n")
    }

    /// Ask again where an answer is overdue.
    pub(crate) fn tick(&mut self, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let channels: Vec<ChannelId> = self.groups.keys().copied().collect();
        for channel in channels {
            self.request_if_due(channel, now_ms, &mut changes);
        }
        changes
    }

    /// Ask at once wherever a channel of `workspace_id` awaits its keys: the
    /// host admitted this session to the workspace just now, and a request is
    /// sent only to a workspace that admitted the session, so what was asked
    /// meanwhile went nowhere and is owed now, not after its backoff.
    pub(crate) fn ask_now(&mut self, workspace_id: u64, now_ms: u64) -> Changes {
        let mut changes = Changes::default();
        let channels: Vec<ChannelId> = self
            .groups
            .iter()
            .filter(|(_, group)| group.workspace_id == workspace_id)
            .map(|(channel, _)| *channel)
            .collect();
        for channel in channels {
            if let Some(group) = self.groups.get_mut(&channel) {
                group.next_request_ms = now_ms;
            }
            self.request_if_due(channel, now_ms, &mut changes);
        }
        changes
    }

    fn after(
        &mut self,
        channel: ChannelId,
        event: Option<PeerEvent>,
        now_ms: u64,
        changes: &mut Changes,
    ) {
        let handed: HashSet<ChannelId> = self.handed.keys().copied().collect();
        let Some(group) = self.groups.get_mut(&channel) else {
            return;
        };
        if event.is_some_and(|event| !matches!(event, PeerEvent::Duplicate | PeerEvent::Ignored)) {
            self.dirty = true;
        }
        // Ask again with a fresh member key: the request sent before was
        // answered with a Welcome this device cannot open, by an admission
        // since replaced, or this device was removed.
        if matches!(event, Some(PeerEvent::Desynchronised | PeerEvent::Removed)) {
            group.request = None;
            group.next_request_ms = now_ms;
            group.request_delay_ms = REQUEST_FIRST_MS;
        }
        if let Some(keys) = group.peer.content_keys() {
            for (unit, entry) in &mut group.units {
                if handed.contains(unit) || entry.installed_epoch == Some(keys.epoch()) {
                    continue;
                }
                entry.installed_epoch = Some(keys.epoch());
                changes
                    .keys
                    .push((entry.document.clone(), keys.clone().for_unit(*unit)));
            }
        }
        self.request_if_due(channel, now_ms, changes);
    }

    fn request_if_due(&mut self, channel: ChannelId, now_ms: u64, changes: &mut Changes) {
        let Some(group) = self.groups.get_mut(&channel) else {
            return;
        };
        if matches!(group.peer.access(), Access::Ready { .. }) || now_ms < group.next_request_ms {
            return;
        }
        let fresh = group.request_delay_ms >= REQUEST_FRESH_MS;
        if fresh {
            group.request = None;
        }
        if group.request.is_none() {
            group.request = group
                .peer
                .unsigned_access_request(now_ms / 1000, fresh, &self.vault)
                .ok()
                .flatten();
            self.dirty = true;
        }
        let Some(request) = group.request.clone() else {
            return;
        };
        group.next_request_ms = now_ms.saturating_add(group.request_delay_ms);
        group.request_delay_ms = group
            .request_delay_ms
            .saturating_mul(2)
            .min(REQUEST_LONGEST_MS);
        changes.outgoing.push(Outgoing::Request {
            workspace_id: group.workspace_id,
            cluster_id: group.document.cluster_id.clone(),
            request,
            route: group.route.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    /// The group carrying `route`: the workspace's scope unless the test's
    /// policy narrows the record to its own.
    fn scope(route: &DocumentRoute) -> ChannelId {
        collab_types::workspace_scope(&route.cluster_id)
    }

    /// The workspace's governance admitting exactly `devices`, every route
    /// carried by the workspace's key scope.
    fn admitting(
        devices: &[&SigningKey],
    ) -> impl Fn(&DocumentRoute) -> Option<RecordPolicy> + use<> {
        let devices: BTreeSet<_> = devices.iter().map(|key| public(key)).collect();
        move |route| {
            Some(RecordPolicy {
                unit: Channels::channel(route),
                scope: scope(route),
                allowed: devices.clone(),
            })
        }
    }

    fn route(record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: WORKSPACE.into(),
            record: record.into(),
        }
    }

    fn device(key: &SigningKey) -> [u8; 32] {
        key.verifying_key().to_bytes()
    }

    fn public(key: &SigningKey) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(device(key))
    }

    fn requests(changes: &Changes) -> Vec<MlsAccessRequestV1> {
        changes
            .outgoing
            .iter()
            .map(|Outgoing::Request { request, .. }| request.clone())
            .collect()
    }

    fn signed(browser: &SigningKey, mut request: MlsAccessRequestV1) -> MlsAccessRequestV1 {
        request.signature = collab_types::Signature::from_bytes(
            browser.sign(&request.signing_bytes().unwrap()).to_bytes(),
        );
        request
    }

    /// A native participant that created the group carrying `route`.
    fn laptop(key: &SigningKey, route: &DocumentRoute) -> ChannelPeer {
        ChannelPeer::create_now(
            None,
            scope(route),
            Box::new(key.clone()),
            &VaultKey::new([9; 32]),
        )
        .unwrap()
    }

    /// Hand the page what `committer` queued, as the host would, with the
    /// workspace's governance admitting `policy`.
    fn deliver(
        channels: &mut Channels,
        committer: &mut ChannelPeer,
        policy: &Policy<'_>,
    ) -> Changes {
        let mut changes = Changes::default();
        for frame in committer.outbox().to_vec() {
            changes.extend(channels.receive_frame(WORKSPACE, frame, policy, 0));
        }
        committer.published(&VaultKey::new([9; 32])).unwrap();
        changes
    }

    /// A request asked before the host admitted the session to its workspace
    /// went nowhere: the workspace's admission asks again at once, for that
    /// workspace's channels only, not after the request's backoff.
    #[test]
    fn a_workspaces_admission_asks_again_at_once_for_its_channels() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-before-admission");
        let policy = admitting(&[&laptop_key, &browser]);
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let asked = requests(&channels.open(&note, &policy, 0));
        assert_eq!(asked.len(), 1, "opening asks");
        assert!(
            requests(&channels.tick(1)).is_empty(),
            "the backoff holds the next ask back"
        );
        let workspace_id = note.workspace_id().unwrap();
        assert!(
            requests(&channels.ask_now(workspace_id.wrapping_add(1), 1)).is_empty(),
            "another workspace's admission asks nothing of this one"
        );
        let again = requests(&channels.ask_now(workspace_id, 1));
        assert_eq!(
            again, asked,
            "the workspace's admission asks the same again"
        );
    }

    /// A route of the workspace's key scope asks for the scope's group
    /// without naming a record: every participant reads it. A record a Read
    /// Deny narrows asks for its own group and names the record, so its
    /// participants decide Read on it.
    #[test]
    fn the_scope_decides_whether_a_request_names_the_record() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let shared = route("note-shared");
        let narrowed = route("note-narrowed");
        let policy = {
            let laptop = public(&laptop_key);
            let browser = public(&browser);
            move |route: &DocumentRoute| {
                let unit = Channels::channel(route);
                let scope = if route.record == "note-narrowed" {
                    unit
                } else {
                    collab_types::workspace_scope(&route.cluster_id)
                };
                Some(RecordPolicy {
                    unit,
                    scope,
                    allowed: BTreeSet::from([laptop, browser]),
                })
            }
        };
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let shared_changes = channels.open(&shared, &policy, 0);
        let narrowed_changes = channels.open(&narrowed, &policy, 0);
        let [Outgoing::Request { route, .. }] = shared_changes.outgoing.as_slice() else {
            panic!("an access request");
        };
        assert!(route.is_none(), "the workspace's scope names no record");
        let [Outgoing::Request { route, .. }] = narrowed_changes.outgoing.as_slice() else {
            panic!("an access request");
        };
        assert_eq!(
            route.as_ref().map(|route| route.record.as_str()),
            Some("note-narrowed"),
            "a narrowed record's request names it"
        );
    }

    #[test]
    fn membership_survives_a_reload_without_asking_again() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-1");
        let mut laptop = laptop(&laptop_key, &note);
        let policy = admitting(&[&laptop_key, &browser]);
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let changes = channels.open(&note, &policy, 0);
        let request = requests(&changes).pop().expect("an access request");
        deliver(&mut channels, &mut laptop, &policy);
        laptop
            .learn_request(signed(&browser, request), &VaultKey::new([9; 32]))
            .unwrap();
        let allowed = BTreeSet::from([public(&laptop_key), public(&browser)]);
        laptop
            .reconcile_now(&allowed, &VaultKey::new([9; 32]))
            .unwrap();
        let (_, keys) = deliver(&mut channels, &mut laptop, &policy)
            .keys
            .into_iter()
            .next()
            .expect("admitted");
        assert_eq!(
            keys.channel_id(),
            Channels::channel(&note),
            "the keys are named for the unit, not the group"
        );
        let saved = channels.export_if_changed().expect("state to save");
        assert!(
            channels.export_if_changed().is_none(),
            "nothing new to save"
        );

        let mut reloaded = Channels::new(device(&browser), [4; 32]);
        reloaded.restore(saved.clone());
        let changes = reloaded.open(&note, &policy, 0);
        assert!(requests(&changes).is_empty(), "a reload does not ask again");
        let (_, restored) = changes.keys.into_iter().next().expect("keys are back");
        assert_eq!(restored.epoch(), keys.epoch());

        // Under another vault the stored state does not open, so the page
        // asks again.
        let mut elsewhere = Channels::new(device(&browser), [5; 32]);
        elsewhere.restore(saved);
        let changes = elsewhere.open(&note, &policy, 0);
        assert!(changes.keys.is_empty());
        assert_eq!(requests(&changes).len(), 1);
    }

    /// A participant may take the request the old page queued before the
    /// reloaded page's. The reload joins from whichever admission names the
    /// key it holds.
    #[test]
    fn a_pending_reload_survives_the_old_request_being_admitted_first() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-pending-reload");
        let mut laptop = laptop(&laptop_key, &note);
        let policy = admitting(&[&laptop_key, &browser]);
        let mut first = Channels::new(device(&browser), [4; 32]);
        let first_changes = first.open(&note, &policy, 0);
        let saved = first.export_if_changed().expect("waiting state is stored");
        let pending = first.pending_requests();
        let mut reload = Channels::new(device(&browser), [4; 32]);
        reload.restore(saved);
        reload.restore_requests(pending);
        let reload_changes = reload.open(&note, &policy, 1);
        let allowed = BTreeSet::from([public(&laptop_key), public(&browser)]);
        deliver(&mut reload, &mut laptop, &policy);
        for changes in [first_changes, reload_changes] {
            for request in requests(&changes) {
                laptop
                    .learn_request(signed(&browser, request), &VaultKey::new([9; 32]))
                    .unwrap();
                laptop
                    .reconcile_now(&allowed, &VaultKey::new([9; 32]))
                    .unwrap();
                deliver(&mut reload, &mut laptop, &policy);
            }
        }
        let keys = reload
            .content_keys(Channels::channel(&note))
            .expect("the reload keeps its admitted channel keys");
        let artifact =
            collab_types::document_artifact_id(&note.app_id, &note.cluster_id, &note.record);
        let (metadata, sealed) = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Checkpoint,
                b"before the reload",
            )
            .unwrap();
        assert_eq!(
            laptop
                .content_keys()
                .unwrap()
                .for_unit(Channels::channel(&note))
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .to_vec(),
            b"before the reload",
        );
    }

    /// The page follows a commit whoever of the workspace's devices wrote it, and
    /// holds one adding a device the workspace's governance does not admit until
    /// it does.
    #[test]
    fn commits_of_any_participant_are_followed_and_checked() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let phone_key = SigningKey::from_bytes(&[5; 32]);
        let note = route("note-any-participant");
        let mut laptop = laptop(&laptop_key, &note);
        let vault = VaultKey::new([9; 32]);
        let before = admitting(&[&laptop_key, &browser]);
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let request = requests(&channels.open(&note, &before, 0)).pop().unwrap();
        deliver(&mut channels, &mut laptop, &before);
        laptop
            .learn_request(signed(&browser, request), &vault)
            .unwrap();
        let everyone = BTreeSet::from([public(&laptop_key), public(&browser), public(&phone_key)]);
        laptop.reconcile_now(&everyone, &vault).unwrap();
        deliver(&mut channels, &mut laptop, &before);
        let joined = channels.groups[&scope(&note)].peer.access();
        assert_eq!(joined, Access::Ready { epoch: 1 });

        // The laptop admits the phone, which the workspace's governance does not
        // admit as the page holds it: the commit waits, and applies once it
        // does.
        let mut phone =
            ChannelPeer::waiting(None, scope(&note), Box::new(phone_key.clone()), &vault).unwrap();
        let ask = phone.access_request_now(1, false, &vault).unwrap().unwrap();
        laptop.learn_request(ask, &vault).unwrap();
        laptop.reconcile_now(&everyone, &vault).unwrap();
        deliver(&mut channels, &mut laptop, &before);
        let state = |channels: &Channels| channels.groups[&scope(&note)].peer.access();
        assert_eq!(state(&channels), Access::Ready { epoch: 1 });
        let after = admitting(&[&laptop_key, &browser, &phone_key]);
        let changes = channels.retry(&after, 0);
        assert_eq!(state(&channels), Access::Ready { epoch: 2 });
        assert_eq!(changes.keys.len(), 1);
    }

    /// A page unsuspecting of the workspace's governance asks for nothing: it
    /// would not know which group to ask, and a guessed one could ask for a
    /// group no participant seals under. The route waits, and opens once the
    /// governance names it.
    #[test]
    fn a_route_waits_for_the_governance_that_names_its_group() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-before-governance");
        let unknown = |_: &DocumentRoute| None;
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let changes = channels.open(&note, &unknown, 0);
        assert!(changes.is_empty(), "nothing is asked without governance");
        let known = admitting(&[&laptop_key, &browser]);
        let asked = requests(&channels.retry(&known, 1));
        assert_eq!(asked.len(), 1, "the governance names the group, so it asks");
        assert_eq!(asked[0].channel, scope(&note));
    }

    /// The bug this page had: a page and a desk each made a group for the
    /// same channel. The unit's keys come from the group its scope names, so
    /// a page and a desk in the same scope seal and open the same content —
    /// media keys included, which hash the unit's channel.
    #[test]
    fn a_unit_seals_under_its_scope_and_names_itself() {
        let browser = SigningKey::from_bytes(&[3; 32]);
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let note = route("note-one-scope");
        let mut laptop = laptop(&laptop_key, &note);
        let policy = admitting(&[&laptop_key, &browser]);
        let mut channels = Channels::new(device(&browser), [4; 32]);
        let request = requests(&channels.open(&note, &policy, 0)).pop().unwrap();
        deliver(&mut channels, &mut laptop, &policy);
        laptop
            .learn_request(signed(&browser, request), &VaultKey::new([9; 32]))
            .unwrap();
        let allowed = BTreeSet::from([public(&laptop_key), public(&browser)]);
        laptop
            .reconcile_now(&allowed, &VaultKey::new([9; 32]))
            .unwrap();
        deliver(&mut channels, &mut laptop, &policy);
        let unit = Channels::channel(&note);
        let page = channels
            .content_keys(unit)
            .expect("the page holds the unit's keys");
        let desk = laptop.content_keys().unwrap().for_unit(unit);
        assert_eq!(page.channel_id(), unit);
        assert_eq!(page.epoch(), desk.epoch());
        let stream = [7; channel_crypto::MEDIA_STREAM_TAG_LEN];
        assert_eq!(
            page.media_key(page.epoch(), device(&laptop_key), stream)
                .unwrap(),
            desk.media_key(desk.epoch(), device(&laptop_key), stream)
                .unwrap(),
            "the page and the desk derive one media key for the unit"
        );
        // The unit's cleartext base, written before the scope sealed, is read
        // from the host's replay; a live cleartext write is refused.
        assert_eq!(
            channels
                .admission(&note, (true, true), true, None)
                .admits_cleartext(),
            Some(true)
        );
        assert_eq!(
            channels
                .admission(&note, (true, true), false, None)
                .admits_cleartext(),
            Some(false)
        );
    }
}
