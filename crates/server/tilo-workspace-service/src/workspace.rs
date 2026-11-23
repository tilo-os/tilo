//! Per-workspace routing and authorization metadata: who is subscribed on
//! which connection, who is a member, and where presence and directed frames
//! go. A workspace's changes are not here: they are the device store's, and
//! reach devices over its link.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::transport::Envelope;
use crate::workspace_files::WorkspaceFiles;
use tilo_server_protocol::WorkspaceId;

/// Whether `next` leaves out a device `previous` lists: only such a list narrows anyone's
/// access, and wakes the sessions admitted under it to re-check
/// (`crate::authority_epoch`). One that only adds devices wakes nobody: a public workspace
/// admitting a crowd would otherwise re-audit every member's session for every newcomer.
pub(crate) fn leaves_out(previous: &[[u8; 32]], next: &[[u8; 32]]) -> bool {
    let kept: std::collections::HashSet<&[u8; 32]> = next.iter().collect();
    previous.iter().any(|device| !kept.contains(device))
}

/// One live subscription — the outbound envelope channel a
/// connection task registers.
pub struct Subscriber {
    /// Peer device pubkey. A device may hold several subscriptions at once,
    /// one per connection, told apart by their channel (`tx`).
    pub device_public_key: [u8; 32],
    /// Bounded outbound envelope channel. Closed or full means the peer can
    /// no longer keep up and is pruned immediately.
    pub tx: mpsc::Sender<Envelope>,
    /// Signals the owning peer task when this subscriber cannot keep up. A
    /// reconnect replays its durable queue instead of growing RAM without
    /// bound or leaving the connection silently stale.
    pub disconnect: mpsc::Sender<()>,
    /// Pieces of record content this connection's queue could not take yet,
    /// one queue per sender, served in turn so one sender's burst cannot
    /// crowd another's out ([`crate::content_queue`]).
    pub content_waiting: Arc<crate::content_queue::ContentQueue>,
    /// Who the connection authenticated as: what a frame addressed to the
    /// device is re-authorised against.
    pub peer: crate::auth::AuthenticatedPeer,
}

/// One connection's subscription to the workspace and what the host keeps
/// for that connection alone. A device may hold several at once (a second
/// tab, an agent's session beside its person's page, a page that re-dialled while
/// its old connection is still closing): each is its own, and none displaces
/// another, so one connection entering never ends a sibling's writes.
pub(crate) struct Session {
    pub(crate) subscriber: Subscriber,
    /// Where presence for this connection goes when it takes it batched
    /// ([`crate::presence_mailbox`]); without one, presence goes on its frame
    /// queue.
    presence: Option<Arc<crate::presence_mailbox::PresenceMailbox>>,
}

impl Session {
    /// Whether this is the connection whose outbound channel is `tx`.
    pub(crate) fn is(&self, tx: &mpsc::Sender<Envelope>) -> bool {
        self.subscriber.tx.same_channel(tx)
    }
}

/// The access of each member device of a workspace, by device key.
pub type DeviceAccess = std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>;

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// The workspace this runtime is of, by the one id a workspace has.
    workspace_id: WorkspaceId,
    /// Whether the workspace's governor follows its app surfaces arriving.
    pub(crate) followed: bool,
    /// The in-process authority serving this workspace, if any: it answers
    /// the workspace's calls.
    pub workspace_authority: Option<crate::workspaces::WorkspaceAuthority>,
    /// The access of each member device, as the workspace's signed member list
    /// gives it: what the server decides each change's claimed action against.
    /// `None` until the list settled here; the server then decides changes
    /// against their app surface alone.
    pub(crate) device_access: Option<Arc<DeviceAccess>>,
    /// The workspace's files on this host.
    pub files: WorkspaceFiles,
    /// The workspace left the host's registry ([`crate::workspace_io::evict`]):
    /// its files stay, and this runtime writes them no more.
    pub(crate) evicted: bool,
    /// Who the workspace's signed member list has now: its members by their roots
    /// and their devices, with the browser environments this server placed in it
    /// ([`crate::environment`]). Set when the list settles here and rebuilt from the
    /// durable list when the workspace is bound; never kept on disk. Both lists are
    /// sorted.
    pub(crate) members: Arc<tilo_peer_state::GovernedAccess>,
    /// The devices this workspace keeps as keys its person revoked: refused with how to approve
    /// another wherever they ask, until their person's approval admits a new key.
    pub(crate) revoked_keys: HashSet<[u8; 32]>,
    /// The live subscribed connections of each device, oldest first
    /// ([`Session`]).
    sessions: HashMap<[u8; 32], Vec<Session>>,
    /// Device pubkeys of the hosts this workspace is attached to: the hosts
    /// of its person that serve it, recorded as each announces what it
    /// carries (its hello). A change one of them sent is not carried on to
    /// another, and it carries its members' changes under the members' own
    /// keys.
    pub attached_hosts: HashSet<[u8; 32]>,
    /// What services beside the session stack keep for this workspace (a
    /// server's compute routing), one value per type.
    pub extensions: Extensions,
    /// The host's content reads one member may hold at once, so one
    /// member's pieces cannot take the blocking pool from another's
    /// ([`crate::content_work`]).
    pub(crate) content_work: Arc<crate::content_work::ContentWork>,
    /// The devices on this workspace's live session ([`Self::live_join`]):
    /// each is told of the others as they join and leave, so a device knows
    /// whom its host reaches for content and presence.
    live_members: HashSet<[u8; 32]>,
    /// The signed invocations this host admitted within their freshness
    /// window: one invocation id names one request.
    pub(crate) invocations: tilo_server_protocol::invocation::InvocationReplay,
}

/// What a workspace tells a key its person revoked: the one command that approves another, in the
/// words every surface uses.
pub(crate) fn key_revoked_reason() -> String {
    format!(
        "{}: this key was revoked. {}",
        tilo_actions::remote::KEY_REVOKED,
        tilo_actions::remote::APPROVE_AGAIN
    )
}

impl WorkspaceRuntime {
    /// The workspace this runtime is of.
    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// Whether the person whose root is `user` is a member of this workspace: what
    /// [`crate::workspace_access`] enrols their devices by.
    #[must_use]
    pub fn admits_person(&self, user: &[u8; 32]) -> bool {
        self.members.member_user_roots.binary_search(user).is_ok()
    }

    /// Whether the workspace's member list has `device`.
    #[must_use]
    pub fn lists(&self, device: &[u8; 32]) -> bool {
        self.members.member_devices.binary_search(device).is_ok()
    }

    /// Take `members` as who the workspace has now, `revoked` as the keys its people revoked,
    /// and `device_access` as what each member device may do. A list that leaves a device out
    /// refuses that device's live sessions and wakes every session to re-check.
    pub(crate) fn settle_members(
        &mut self,
        mut members: tilo_peer_state::GovernedAccess,
        revoked: impl IntoIterator<Item = [u8; 32]>,
        device_access: Option<DeviceAccess>,
    ) {
        members.member_devices.sort_unstable();
        members.member_devices.dedup();
        members.member_user_roots.sort_unstable();
        members.member_user_roots.dedup();
        self.revoked_keys.extend(revoked);
        self.revoked_keys
            .retain(|device| members.member_devices.binary_search(device).is_err());
        let removed: Vec<[u8; 32]> = self
            .members
            .member_devices
            .iter()
            .copied()
            .filter(|device| members.member_devices.binary_search(device).is_err())
            .collect();
        let roots_left = leaves_out(&self.members.member_user_roots, &members.member_user_roots);
        self.members = Arc::new(members);
        if let Some(device_access) = device_access {
            self.device_access = Some(Arc::new(device_access));
        }
        if !removed.is_empty() || roots_left {
            self.refuse_removed_live_members(&removed);
            crate::authority_epoch::bump();
        }
    }

    /// List `device`, an environment this server placed in the workspace, beside its members,
    /// or take it out again once the environment stopped.
    pub(crate) fn list_environment(&mut self, device: [u8; 32], listed: bool) {
        let mut members = self.members.as_ref().clone();
        members.member_devices.retain(|listed| *listed != device);
        if listed {
            members.member_devices.push(device);
        }
        self.settle_members(members, [], None);
    }

    /// Build a runtime for a bound workspace from the files its host keeps.
    ///
    /// # Errors
    ///
    /// The workspace's directory can't be created, or its access record
    /// can't be read.
    pub fn open(
        state_root: &std::path::Path,
        workspace_id: WorkspaceId,
    ) -> Result<Self, crate::workspace_files::FilesError> {
        let files = WorkspaceFiles::open(state_root, workspace_id)?;
        Ok(Self {
            workspace_id,
            files,
            evicted: false,
            members: Arc::default(),
            revoked_keys: HashSet::new(),
            sessions: HashMap::new(),
            attached_hosts: HashSet::new(),
            extensions: Extensions::default(),
            content_work: Arc::default(),
            live_members: HashSet::new(),
            followed: false,
            workspace_authority: None,
            device_access: None,
            invocations: tilo_server_protocol::invocation::InvocationReplay::default(),
        })
    }

    /// Note that `device` is a host this workspace is attached to.
    /// Idempotent; called as a host announces what it carries, and as a
    /// device hands one of its workspaces to a host to serve.
    pub fn note_attached_host(&mut self, device: [u8; 32]) {
        self.attached_hosts.insert(device);
    }

    /// Whether `device` is a host this workspace is attached to.
    pub(crate) fn attached(&self, device: &[u8; 32]) -> bool {
        self.attached_hosts.contains(device)
    }

    /// Register the subscription of `sub`'s connection. Another connection
    /// of the same device keeps its own.
    pub fn add_subscriber(&mut self, sub: Subscriber) {
        let sessions = self.sessions.entry(sub.device_public_key).or_default();
        let same = sessions.iter().position(|session| session.is(&sub.tx));
        if let Some(session) = same.and_then(|at| sessions.get_mut(at)) {
            session.subscriber = sub;
            return;
        }
        sessions.push(Session {
            subscriber: sub,
            presence: None,
        });
    }

    /// The subscription of `device`'s connection `tx`, while it holds one.
    pub(crate) fn session(
        &self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) -> Option<&Session> {
        self.sessions_of(device)
            .iter()
            .find(|session| session.is(tx))
    }

    fn session_mut(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) -> Option<&mut Session> {
        self.sessions
            .get_mut(device)?
            .iter_mut()
            .find(|session| session.is(tx))
    }

    /// The subscribed connections of `device`, oldest first.
    pub(crate) fn sessions_of(&self, device: &[u8; 32]) -> &[Session] {
        self.sessions.get(device).map_or(&[], Vec::as_slice)
    }

    /// Every subscribed connection, with its device.
    pub(crate) fn all_sessions(&self) -> impl Iterator<Item = (&[u8; 32], &Session)> {
        self.sessions
            .iter()
            .flat_map(|(device, sessions)| sessions.iter().map(move |session| (device, session)))
    }

    /// The devices holding at least one subscription here.
    pub(crate) fn subscribed_devices(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.sessions.keys()
    }

    /// The subscriber of `device`'s newest connection: where what is
    /// addressed to the device alone goes.
    pub fn subscriber(&self, device: &[u8; 32]) -> Option<&Subscriber> {
        self.sessions_of(device)
            .last()
            .map(|session| &session.subscriber)
    }

    /// Hand the presence fanned out to `device`'s connection `tx` to that
    /// connection's `mailbox` rather than its frame queue. Call right after
    /// [`Self::add_subscriber`] registered that connection; removing the
    /// subscription drops the route.
    pub fn route_presence(
        &mut self,
        device: [u8; 32],
        tx: &mpsc::Sender<Envelope>,
        mailbox: Arc<crate::presence_mailbox::PresenceMailbox>,
    ) {
        if let Some(session) = self.session_mut(&device, tx) {
            session.presence = Some(mailbox);
        }
    }

    /// Add subscriber `device` to the live session and tell the other live
    /// members. Returns the members already present. A device that is not
    /// subscribed cannot join.
    pub fn live_join(
        &mut self,
        device: [u8; 32],
        workspace_id: WorkspaceId,
    ) -> Option<Vec<[u8; 32]>> {
        if !self.has_subscriber(&device) {
            return None;
        }
        let existing: Vec<[u8; 32]> = self
            .live_members
            .iter()
            .copied()
            .filter(|member| *member != device)
            .collect();
        if self.live_members.insert(device) {
            self.announce_live_member(device, workspace_id, true);
        }
        Some(existing)
    }

    /// Remove `device` from the live session and tell the remaining members.
    pub fn live_leave(&mut self, device: &[u8; 32], workspace_id: WorkspaceId) {
        if self.live_members.remove(device) {
            self.announce_live_member(*device, workspace_id, false);
        }
    }

    /// Tell every other live member that `device` is on the live session (`active`)
    /// or left it. Ephemeral: a member notice lost to a full queue costs
    /// only a stale peer list, never a change.
    fn announce_live_member(&self, device: [u8; 32], workspace_id: WorkspaceId, active: bool) {
        let envelope = Envelope::LiveMember {
            workspace_id,
            device_public_key: device,
            active,
        };
        for member in &self.live_members {
            if *member == device {
                continue;
            }
            for session in self.sessions_of(member) {
                let _ = session.subscriber.tx.try_send(envelope.clone());
            }
        }
    }

    /// Drop every subscription of `device`, and its live membership.
    /// Idempotent.
    pub fn remove_subscriber(&mut self, device: &[u8; 32]) {
        if self.sessions.remove(device).is_some() {
            // A connection whose subscription went is ended by its next
            // access audit.
            crate::authority_epoch::bump();
        }
        let workspace_id = self.files.workspace_id();
        self.live_leave(device, workspace_id);
    }

    /// Remove only the subscription of `device`'s connection `tx`: the
    /// device's other connections keep theirs, and a closing connection's
    /// delayed teardown touches no other. The device leaves the live session
    /// with its last connection.
    pub(crate) fn remove_subscriber_session(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) {
        let Some(sessions) = self.sessions.get_mut(device) else {
            return;
        };
        let Some(at) = sessions.iter().position(|session| session.is(tx)) else {
            return;
        };
        sessions.remove(at);
        crate::authority_epoch::bump();
        if sessions.is_empty() {
            self.remove_subscriber(device);
        }
    }

    /// Whether `device` currently holds a subscription here.
    pub fn has_subscriber(&self, device: &[u8; 32]) -> bool {
        !self.sessions_of(device).is_empty()
    }

    /// Whether `device`'s connection `tx` holds a subscription here: the
    /// connection whose writes and requests the workspace takes.
    pub(crate) fn current_session(&self, device: &[u8; 32], tx: &mpsc::Sender<Envelope>) -> bool {
        self.session(device, tx).is_some()
    }

    /// Whether this workspace keeps `device` as a key its person revoked: nothing it asks is
    /// answered as a member's until they approve another.
    #[must_use]
    pub(crate) fn revoked_key(&self, device: &[u8; 32]) -> bool {
        self.revoked_keys.contains(device)
    }

    /// Tell live devices that just lost membership they are no longer
    /// authorised, while their connection is still open, so a concurrent audit
    /// cannot close the session without an authenticated refusal. The
    /// subscriber is then unregistered
    /// so a follow-up write closes instead of emitting a second refusal.
    pub(crate) fn refuse_removed_live_members(&mut self, removed: &[[u8; 32]]) {
        let (revoked, others): (Vec<[u8; 32]>, Vec<[u8; 32]>) = removed
            .iter()
            .copied()
            .partition(|device| self.revoked_key(device));
        if !revoked.is_empty() {
            self.refuse_live_members(
                &revoked,
                &tilo_server_protocol::refusal::Refusal::NotAMember(key_revoked_reason()),
            );
        }
        if !others.is_empty() {
            self.refuse_live_members(
                &others,
                &tilo_server_protocol::refusal::Refusal::NotAMember(
                    "workspace membership changed".into(),
                ),
            );
        }
    }

    /// Tell every live device of the workspace it is no longer authorised,
    /// and why, then unregister them: the workspace was deleted.
    pub(crate) fn refuse_every_live_member(
        &mut self,
        refusal: &tilo_server_protocol::refusal::Refusal,
    ) {
        let live: Vec<[u8; 32]> = self.subscribed_devices().copied().collect();
        self.refuse_live_members(&live, refusal);
    }

    fn refuse_live_members(
        &mut self,
        removed: &[[u8; 32]],
        refusal: &tilo_server_protocol::refusal::Refusal,
    ) {
        if removed.is_empty() {
            return;
        }
        let workspace_id = self.files.workspace_id();
        for device in removed {
            for session in self.sessions_of(device) {
                let _ = session.subscriber.tx.try_send(Envelope::NotAuthorised {
                    workspace_id,
                    refusal: refusal.clone(),
                });
            }
        }
        for device in removed {
            if self.has_subscriber(device) {
                self.remove_subscriber(device);
            }
        }
    }

    /// Route a presence envelope (no persistence, drop-on-empty). What an
    /// attached host sent is not carried on to another. A subscriber whose
    /// connection has a presence mailbox gets it there; the payload is shared
    /// by every such subscriber, not copied.
    pub fn fanout_presence(&mut self, author: &[u8; 32], envelope: &Envelope) -> usize {
        let mut delivered = 0usize;
        let mut dead: Vec<([u8; 32], mpsc::Sender<Envelope>)> = Vec::new();
        let mut shared: Option<Arc<[u8]>> = None;
        let from_attached = self.attached(author);
        let now = tokio::time::Instant::now();
        for (peer, session) in self.all_sessions() {
            let sub = &session.subscriber;
            if peer == author {
                continue;
            }
            if from_attached && self.attached(peer) {
                continue;
            }
            if let (
                Some(mailbox),
                Envelope::Presence {
                    workspace_id,
                    payload,
                },
            ) = (session.presence.as_ref(), envelope)
            {
                let payload = shared
                    .get_or_insert_with(|| Arc::from(payload.as_slice()))
                    .clone();
                mailbox.post(*workspace_id, payload, now);
                delivered = delivered.saturating_add(1);
                continue;
            }
            match sub.tx.try_send(envelope.clone()) {
                Ok(()) => delivered = delivered.saturating_add(1),
                // Presence is ephemeral: a full queue simply drops this frame
                // (backpressure), it never disconnects the subscriber. A closed
                // channel means the task is gone, so prune it.
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    dead.push((*peer, sub.tx.clone()));
                }
            }
        }
        for (device, tx) in dead {
            self.remove_subscriber_session(&device, &tx);
        }
        delivered
    }

    /// Deliver a directed envelope to the live subscriber `target` only, on
    /// its newest connection: a request is carried out once, not once per connection.
    /// Never queued: a directed request is live traffic. Returns whether it
    /// was handed to that connection.
    pub fn route_directed(&mut self, target: &[u8; 32], envelope: Envelope) -> bool {
        let Some(sub) = self.subscriber(target) else {
            return false;
        };
        if sub.tx.try_send(envelope).is_ok() {
            return true;
        }
        let _ = sub.disconnect.try_send(());
        let tx = sub.tx.clone();
        self.remove_subscriber_session(target, &tx);
        false
    }

    /// Deliver a piece of record content `from` sent to the subscriber
    /// `target` only, through its fairness queue ([`crate::content_queue`]):
    /// a piece the connection's queue cannot take waits with its sender's
    /// own pieces, never ahead of another sender's. Returns whether `target`
    /// is subscribed. Each of `target`'s connections gets it: whichever of
    /// them asked for the resource takes it.
    pub fn route_content(&self, from: &[u8; 32], target: &[u8; 32], envelope: &Envelope) -> bool {
        let sessions = self.sessions_of(target);
        for session in sessions {
            let sub = &session.subscriber;
            sub.content_waiting
                .deliver_piece(*from, envelope.clone(), &sub.tx);
        }
        !sessions.is_empty()
    }
}

/// Shorthand: a workspace runtime held under an async mutex.
pub type SharedWorkspace = Arc<AsyncMutex<WorkspaceRuntime>>;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#[allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
mod tests {
    use super::*;
    use crate::transport::Envelope;

    /// `device` as a connection authenticated it, for routing alone.
    fn peer(device: [u8; 32]) -> crate::auth::AuthenticatedPeer {
        crate::auth::AuthenticatedPeer {
            cert: tilo_user_identity::DeviceCert {
                user_public_key: [7; 32],
                device_public_key: device,
                device_label: "device".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            },
            user_public_key: [7; 32],
            device_public_key: device,
        }
    }

    /// Only a membership that leaves a device out narrows anyone's access,
    /// and so wakes the sessions admitted under it.
    #[test]
    fn only_a_membership_that_leaves_a_device_out_narrows() {
        assert!(!leaves_out(&[[1; 32]], &[[1; 32], [2; 32]]));
        assert!(!leaves_out(&[[1; 32], [2; 32]], &[[2; 32], [1; 32]]));
        assert!(!leaves_out(&[], &[[3; 32]]));
        assert!(leaves_out(&[[1; 32], [2; 32]], &[[2; 32], [3; 32]]));
        assert!(leaves_out(&[[1; 32]], &[]));
    }

    /// Presence to a subscriber with a mailbox goes there, batched, and the
    /// subscriber without one gets it on its frame queue.
    #[test]
    fn presence_goes_to_the_mailbox_of_the_subscriber_that_has_one() {
        use crate::presence_mailbox::{BATCH_WINDOW, PresenceMailbox};
        use tokio::time::Instant;

        const AUTHOR: [u8; 32] = [1; 32];
        const RECIPIENT: [u8; 32] = [2; 32];
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), WorkspaceId::from_u128(9)).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: RECIPIENT,
            tx: tx.clone(),
            disconnect,
            content_waiting: Arc::default(),
            peer: peer(RECIPIENT),
        });
        let mailbox = Arc::new(PresenceMailbox::default());
        rt.route_presence(RECIPIENT, &tx, Arc::clone(&mailbox));
        let presence = Envelope::Presence {
            workspace_id: WorkspaceId::from_u128(9),
            payload: b"caret".to_vec(),
        };

        assert_eq!(rt.fanout_presence(&AUTHOR, &presence), 1);
        assert!(
            rx.try_recv().is_err(),
            "the mailbox took it, not the frame queue"
        );
        assert_eq!(mailbox.take(Instant::now() + BATCH_WINDOW, false).len(), 1);

        rt.session_mut(&RECIPIENT, &tx).unwrap().presence = None;
        assert_eq!(rt.fanout_presence(&AUTHOR, &presence), 1);
        assert_eq!(rx.try_recv().unwrap(), presence);
    }

    /// A burst of content from one sender waits for the connection's queue
    /// without taking the room another sender's piece needs.
    #[test]
    fn a_content_burst_from_one_sender_leaves_room_for_another() {
        const A: [u8; 32] = [1; 32];
        const B: [u8; 32] = [2; 32];
        const TO: [u8; 32] = [3; 32];
        const WS: WorkspaceId = WorkspaceId::from_u128(9);
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), WS).unwrap();
        let (tx, mut rx) = mpsc::channel(2);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: TO,
            tx: tx.clone(),
            disconnect,
            content_waiting: Arc::default(),
            peer: peer(TO),
        });
        let piece = |from: [u8; 32], byte: u8| Envelope::DocumentContent {
            workspace_id: WS,
            to_device_public_key: TO,
            from_device_public_key: from,
            frame: vec![byte],
        };
        for number in 0..20 {
            assert!(rt.route_content(&A, &TO, &piece(A, number)));
        }
        assert!(rt.route_content(&B, &TO, &piece(B, 0xBB)));
        assert!(
            !rt.route_content(&B, &[9; 32], &piece(B, 0xBB)),
            "a piece for a device that is not subscribed goes nowhere"
        );

        let queue = Arc::clone(&rt.subscriber(&TO).unwrap().content_waiting);
        let mut seen = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let Envelope::DocumentContent {
                from_device_public_key,
                ..
            } = envelope
            {
                seen.push(from_device_public_key);
            }
            // The connection's writer takes one, the queue's content returns.
            queue.serve_waiting(&tx);
        }
        assert_eq!(seen.len(), 21, "the burst under the bound arrives whole");
        let b_at = seen.iter().position(|from| *from == B).unwrap();
        assert!(
            b_at <= 4,
            "B's piece came within a turn, after {b_at} of A's, not behind the whole burst"
        );
    }
}

/// What services beside the session stack keep per workspace, one value per
/// type: a server keeps its compute routing here. The workspace knows nothing
/// of what they hold.
#[derive(Default)]
pub struct Extensions(HashMap<std::any::TypeId, Box<dyn std::any::Any + Send>>);

impl Extensions {
    /// The value of type `T`, when one was made.
    #[must_use]
    pub fn get<T: std::any::Any + Send>(&self) -> Option<&T> {
        self.0
            .get(&std::any::TypeId::of::<T>())
            .and_then(|value| value.downcast_ref())
    }

    /// The value of type `T`, made by `make` the first time.
    pub fn get_or_insert_with<T: std::any::Any + Send>(
        &mut self,
        make: impl FnOnce() -> T,
    ) -> &mut T {
        let value = self
            .0
            .entry(std::any::TypeId::of::<T>())
            .or_insert_with(|| Box::new(make()));
        #[expect(
            clippy::expect_used,
            reason = "the entry under a type's id is only ever made as that type"
        )]
        let value = value
            .downcast_mut()
            .expect("an extension is kept under its own type");
        value
    }
}
