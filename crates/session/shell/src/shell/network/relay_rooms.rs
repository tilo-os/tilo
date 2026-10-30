//! Rooms a host of this device's person governs: this device's own host
//! for the workspaces placed here, a relay of theirs for those moved to it.
//! The room's host issues every invitation, admits whoever redeems one and
//! publishes the room's governance; this device holds the room like any
//! member does, its own host's rooms included.
//!
//! Everything here is asked as the host's own actions, over a session with
//! it, addressed to the host's own device: the surface an agent, a script
//! or a screen reader uses, decided by the host as it decides every other
//! door. None of these providers is listed in a session's catalogue, so
//! every invocation names its own.
//!
//! - A room this device holds is administered through the room's own
//!   governance (provider `tilo.workspace`), on the room's session.
//! - The host's own operations (provider `tilo.host`): creating a room,
//!   listing the rooms a host governs, entering one, letting an app author
//!   one. Creating, listing and entering are asked on the host's session of
//!   this person's continuity workspace, which this device holds before it
//!   holds any room; an app's authorship on the room's own session. Creating a room issues the invitation
//!   the share is for, and entering one answers the invitation this device
//!   redeems to hold it: nothing asks a room's governance before the room is
//!   held.
//! - A member's own business in a room a host governs (provider
//!   `tilo.member`): claiming this device's topology namespace, recording
//!   its own effect receipt, and having the host create the empty record of
//!   an app this device opens there first, on the room's session.
use super::{
    NetCmd, PendingInvitation, RegisteredHomePeer, ShellNetwork, persist_pending_invitations,
    registered_home_peer_addrs, room_topic_id,
};
use crate::network::invite::InviteRole;
use crate::network::invite::enrollment_driver;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::{RoomId, WorkspaceId};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ROOM_GOVERNANCE_PROVIDER, ReceiptStatus, RemoteActionTarget,
    room_governance_resource,
};
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_network_actor::room_governance::Request;
use tilo_peer_state::join;
use tilo_relay_session::rooms::{
    ENTER_ROOM, EnteredRoom, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, IssuedInvitation,
    LIST_ROOMS, RoomListing,
};

/// How long one invocation on a room's own session may take.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one host operation may take, dialling the host included.
const HOST_TIMEOUT: Duration = Duration::from_secs(30);
/// The room governance action that issues an invitation.
const INVITE: &str = tilo_action_policy::roles::governance::INVITE;
/// The room governance action that cancels an unredeemed invitation.
const CANCEL_INVITATION: &str = "invitation.cancel";
/// How long the invitation this device redeems into a room it just shared
/// or entered stays redeemable.
const OWN_INVITATION_SECONDS: u64 = 600;
/// The longest room name a relay accepts.
const ROOM_NAME_CHARS: usize = 80;

#[derive(Default)]
pub(super) struct State {
    /// Enrollments of this device into the rooms it shared, installed as
    /// they complete.
    shared: Arc<Mutex<Vec<u64>>>,
    /// One of `shared` has landed since the last pass installing them.
    shared_due: Arc<AtomicBool>,
    /// The passes over `shared` run so far.
    #[cfg(test)]
    shared_passes: std::sync::atomic::AtomicU64,
    /// Host rooms this device is enrolling into, by enrollment.
    joining: Mutex<std::collections::HashMap<RoomId, u64>>,
}

/// A hosted room this device holds and its person administers: its
/// governance answers on the room's session, from the host's own device.
pub(super) struct HeldRoom {
    room: RoomId,
    /// The host's own device, the room's governor.
    host: iroh::EndpointId,
    /// Whether that host is this device's own.
    here: bool,
}

impl HeldRoom {
    pub(super) const fn room(&self) -> RoomId {
        self.room
    }

    /// Whether this device's own host governs the room: a workspace placed
    /// here, private until its person makes it public.
    pub(super) const fn hosted_here(&self) -> bool {
        self.here
    }
}

/// A host of this device's person, where its own operations are asked.
pub(super) enum PersonsHost {
    /// This device's own host, in process.
    Here,
    /// Another of the person's devices hosting rooms, on a session opened
    /// for each request.
    Device {
        addr: iroh::EndpointAddr,
        device: [u8; 32],
    },
    /// A home relay the person paired.
    Relay(RegisteredHomePeer),
    /// A workspace host this session is configured with (a company node),
    /// pinned by its endpoint.
    Configured(iroh::EndpointAddr),
}

impl PersonsHost {
    /// Where the host is reached, unless it is this device's own.
    fn endpoint(&self) -> Option<iroh::EndpointId> {
        match self {
            Self::Here => None,
            Self::Device { addr, .. } | Self::Configured(addr) => Some(addr.id),
            Self::Relay(home) => Some(home.addr.id),
        }
    }
}

/// The device of `room`'s governor, when it is also the host `view`'s
/// transport hint names: a relay, or a person's device hosting the rooms
/// placed on it. `None` when the room is governed elsewhere than where it
/// is served.
pub(super) fn hosting_governor(
    view: &tilo_peer_state::owner::NativeCommittedView,
    room: RoomId,
) -> Result<Option<iroh::EndpointId>, String> {
    let (hint, _) = IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint()?)
        .map_err(|error| error.to_string())?;
    if hint.room != room.to_string() || &hint.host_device_certificate != view.room_host() {
        return Err("the room's host differs from its current admitted governor".into());
    }
    if &hint.host_device_certificate != view.room_host() {
        return Ok(None);
    }
    iroh::EndpointId::from_bytes(&view.room_host().device_public_key)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// What an invitation a relay issues is for.
pub(super) struct RelayInvitation<'a> {
    /// The role it grants.
    pub role: InviteRole,
    /// The only person who may redeem it, if restricted.
    pub recipient: Option<[u8; 32]>,
    /// How it names its recipient to them.
    pub invitee: Option<&'a str>,
    /// How long it stays redeemable, in seconds.
    pub lifetime: u64,
}

impl RelayInvitation<'_> {
    /// The name its recipient is given, when one is.
    fn invitee(&self) -> Option<&str> {
        self.invitee.map(str::trim).filter(|name| !name.is_empty())
    }
}

const fn role_name(role: InviteRole) -> &'static str {
    match role {
        InviteRole::Viewer => "viewer",
        InviteRole::Commenter => "commenter",
        InviteRole::Editor => "editor",
    }
}

fn invite_arguments(terms: &RelayInvitation<'_>) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("role".into(), role_name(terms.role).into());
    if let Some(recipient) = terms.recipient {
        arguments.insert("recipient".into(), hex::encode(recipient).into());
    }
    if let Some(invitee) = terms.invitee() {
        arguments.insert("invitee_name".into(), invitee.into());
    }
    arguments.insert("expires_in_seconds".into(), terms.lifetime.into());
    arguments
}

/// The name a room shared from a workspace called `name` gets.
pub(super) fn room_name(name: &str) -> String {
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(ROOM_NAME_CHARS)
        .collect();
    if name.is_empty() {
        "Shared workspace".to_owned()
    } else {
        name
    }
}

/// The id and the signed invitation the governance invitation answered.
fn issued_invitation(output: &serde_json::Value) -> Result<IssuedInvitation, String> {
    let field = |name: &str| {
        output
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("the relay's invitation has no `{name}`"))
    };
    tilo_relay_session::rooms::ActionInvitation {
        invite: field("invite")?.to_owned(),
        invitation: field("invitation")?.to_owned(),
    }
    .decode()
}

/// What a relay action answered, read as `T`.
pub(super) fn answer_of<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("the relay answered unexpectedly: {error}"))
}

/// Why a host asked on a room's session gave no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostAskError {
    /// The host could not be reached: no session with it and none being
    /// established, the session closed before it answered, or the network
    /// could not take the ask. Worth asking again once a room's state
    /// changes.
    Unreachable(String),
    /// The host answered, and refused.
    Refused(String),
}

impl From<HostAskError> for String {
    fn from(error: HostAskError) -> Self {
        match error {
            HostAskError::Unreachable(reason) | HostAskError::Refused(reason) => reason,
        }
    }
}

/// What an invocation's receipt comes to.
pub(super) fn receipt_value(status: ReceiptStatus) -> Result<serde_json::Value, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(format!("the relay refused: {error}")),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
            Err("the relay did not finish the request".into())
        }
    }
}

/// An invocation of `action` on `resource` of `provider`, with `arguments`.
pub(super) fn invocation(
    provider: &str,
    resource: String,
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        // The provider is named: no catalogue lists it.
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new(resource, action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

impl ShellNetwork {
    /// The host governing `room`, when that host is one of this person's
    /// (this device's own, or a relay of theirs) and this device holds the
    /// room.
    pub(super) fn administered_room(&self, room: RoomId) -> Option<HeldRoom> {
        let owner = self.native_owner().ok()?;
        let identity = self.host_identity.as_ref()?;
        owner
            .with_current(room, |view| {
                let (certificate, _) = identity.current_native_material(view.workspace())?;
                if view.owner() != certificate.user_public_key {
                    return Err("another person governs the room".into());
                }
                Ok(())
            })
            .ok()?;
        self.hosted_room(room)
    }

    /// The host governing `room`, whoever's it is, when this device holds
    /// the room: a member asks it for what the room's policy lets them do,
    /// such as inviting someone, and the host decides.
    pub(super) fn hosted_room(&self, room: RoomId) -> Option<HeldRoom> {
        let owner = self.native_owner().ok()?;
        owner
            .with_current(room, |view| {
                let host = hosting_governor(view, room)?.ok_or("no host governs the room")?;
                Ok(HeldRoom {
                    room,
                    host,
                    here: host == self.local_peer_id,
                })
            })
            .ok()
    }

    /// Queue `invoke` on `ws`'s relay session for `to`, and answer where
    /// its receipt arrives.
    fn ask_relay(
        &self,
        ws: u64,
        to: iroh::EndpointId,
        invoke: InvokeRequest,
    ) -> Result<std::sync::mpsc::Receiver<Result<ReceiptStatus, String>>, String> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.cmd_tx
            .try_send(NetCmd::RoomGovernance(Box::new(Request {
                ws,
                to,
                invoke,
                reply,
            })))
            .map_err(|_| "the network queue is unavailable")?;
        Ok(answer)
    }

    /// Invoke `invoke` on `ws`'s relay session for `to`, as this device,
    /// and answer what it produced within `timeout`.
    fn invoke_relay(
        &self,
        ws: u64,
        to: iroh::EndpointId,
        invoke: InvokeRequest,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let status = self
            .ask_relay(ws, to, invoke)?
            .recv_timeout(timeout)
            .map_err(|_| "the relay did not answer in time")??;
        receipt_value(status)
    }

    /// Invoke `action` of `target`'s governance with `arguments`, as this
    /// device, and answer what it produced.
    pub(super) fn govern(
        &self,
        target: &HeldRoom,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let invoke = invocation(
            ROOM_GOVERNANCE_PROVIDER,
            room_governance_resource(&target.room.to_string()),
            action,
            arguments,
        );
        self.invoke_relay(
            room_topic_id(target.room),
            target.host,
            invoke,
            ADMIN_TIMEOUT,
        )
    }

    /// Invoke the host operation `action` with `arguments` at `host`, as
    /// this device, asking the host directly.
    pub(super) fn ask_host(
        &self,
        host: &PersonsHost,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let (addr, device) = match host {
            PersonsHost::Here => return self.local_host_action(action, arguments),
            PersonsHost::Relay(home) => {
                (home.addr.clone(), Some(home.expectation.device_public_key))
            }
            PersonsHost::Configured(addr) => (addr.clone(), None),
            PersonsHost::Device { addr, device } => (addr.clone(), Some(*device)),
        };
        // This device's own host answers in process: an endpoint cannot
        // dial itself.
        if addr.id == self.local_peer_id {
            return self.local_host_action(action, arguments);
        }
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        let invoke = invocation(
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            action,
            arguments,
        );
        self.cmd_tx
            .try_send(NetCmd::AskHost(Box::new(
                tilo_network_actor::host_ask::Request {
                    addr,
                    device,
                    invoke,
                    reply,
                },
            )))
            .map_err(|_| "the network queue is unavailable")?;
        let status = answer
            .recv_timeout(HOST_TIMEOUT)
            .map_err(|_| "the host did not answer in time")??;
        receipt_value(status)
    }

    /// The device of the room's admitted governor when a host governs
    /// `room`, as this device's current admitted custody of it names it.
    /// `Ok(None)` when the room is governed elsewhere than where it is
    /// served.
    pub(super) fn host_governor(&self, room: RoomId) -> Result<Option<iroh::EndpointId>, String> {
        let owner = self.native_owner()?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        owner.with_current(room, |view| {
            let (certificate, _) = identity.current_native_material(view.workspace())?;
            if view.device() != &certificate {
                return Err("the caller differs from admitted native custody".into());
            }
            hosting_governor(view, room)
        })
    }

    /// Whether `host`, the host governing `room`, is this device's own: the
    /// room is served here, or its host is this device's endpoint.
    pub(super) fn local_host_governs(&self, room: RoomId, host: iroh::EndpointId) -> bool {
        self.hosted_here(room) || host == self.local_peer_id
    }

    /// The device of the host governing `room`, to ask a host operation of
    /// as `caller`, this device's admitted custody of the room:
    /// pinned to the current governor and its authenticated transport hint.
    /// `None` when no host governs the room.
    pub(super) fn governing_host(
        &self,
        room: RoomId,
        caller: &tilo_user_identity::DeviceCert,
    ) -> Result<Option<iroh::EndpointId>, String> {
        self.native_owner().and_then(|owner| {
            owner.with_current(room, |view| {
                let identity = self
                    .host_identity
                    .as_ref()
                    .ok_or("native identity unavailable")?;
                let (certificate, _) = identity.current_native_material(view.workspace())?;
                if view.device() != &certificate || caller != &certificate {
                    return Err("host operation caller differs from admitted native custody".into());
                }
                hosting_governor(view, room)
            })
        })
    }

    /// Keep the invitation `issued` to `room` on `terms` among the pending
    /// ones the owner may resend or cancel, and answer its text.
    fn remember_relay_invitation(
        &self,
        room: RoomId,
        issued: &IssuedInvitation,
        terms: &RelayInvitation<'_>,
    ) -> Result<String, String> {
        let token = join::encode_invitation_text(&issued.signed_invitation);
        let mut pending = self
            .pending_identity_admissions
            .lock()
            .map_err(|_| "invite admission state is unavailable".to_string())?;
        pending.insert(
            issued.invite_id,
            PendingInvitation {
                id: issued.invite_id,
                room,
                name: terms.invitee().unwrap_or("Member").to_string(),
                role: terms.role,
                token: token.clone(),
                recipient_user_public_key: terms.recipient,
                expiry_unix_seconds: tilo_clock::unix_now().saturating_add(terms.lifetime),
            },
        );
        if let Some(path) = self.pending_invitations_path.as_deref() {
            persist_pending_invitations(path, &pending);
        }
        drop(pending);
        self.note_session_reachability(true);
        Ok(token)
    }

    /// Have the relay issue an invitation to `target` on `terms` and keep
    /// it among the pending ones the owner may resend or cancel. Answers
    /// the invitation's text.
    pub(super) fn issue_relay_invitation(
        &self,
        target: &HeldRoom,
        terms: &RelayInvitation<'_>,
    ) -> Result<String, String> {
        let issued = issued_invitation(&self.govern(target, INVITE, invite_arguments(terms))?)?;
        self.remember_relay_invitation(target.room, &issued, terms)
    }

    /// Cancel `invitation` of `room` at the relay governing it. `Ok(false)`
    /// when no relay of this person governs `room`.
    pub(super) fn cancel_relay_invitation(
        &self,
        room: RoomId,
        invitation: [u8; 32],
    ) -> Result<bool, String> {
        let Some(target) = self.administered_room(room) else {
            return Ok(false);
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("invite".into(), hex::encode(invitation).into());
        self.govern(&target, CANCEL_INVITATION, arguments)?;
        Ok(true)
    }

    /// Enter `room` at `host` as this device, and answer the text of the
    /// invitation this device redeems to hold it.
    fn enter_host_room(&self, host: &PersonsHost, room: &str) -> Result<String, String> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("room".into(), room.into());
        arguments.insert("expires_in_seconds".into(), OWN_INVITATION_SECONDS.into());
        let entered: EnteredRoom = answer_of(self.ask_host(host, ENTER_ROOM, arguments)?)?;
        let own = entered.invitation.decode()?;
        Ok(join::encode_invitation_text(&own.signed_invitation))
    }

    /// Enter `room`, called `name`, at the host at `host` as this device
    /// and join it as a workspace of its own, the room, shown at once
    /// when it is the first room this session holds that way: what one of
    /// the person's devices does when their workspace list names a room it
    /// does not hold yet. Nothing when the room is held or being joined.
    pub(super) fn enter_listed_room(
        &self,
        host: &PersonsHost,
        room: RoomId,
        name: &str,
    ) -> Result<(), String> {
        let tree = self.tree.clone();
        let (held, through_hosts) = {
            let tree = tree.read();
            let through_hosts: Vec<RoomId> = tree
                .desktop
                .workspaces
                .iter()
                .map(crate::shell::state::Workspace::room)
                .filter(|room| {
                    (tree.holds_room(*room) || tree.is_joining(*room))
                        && !tree.pending_placements.contains(room)
                })
                .collect();
            // A room already being joined (a site's join waiting for its
            // host) is not entered again: that join goes on by itself.
            (
                tree.holds_room(room) || tree.is_joining(room),
                through_hosts,
            )
        };
        // The first room this session holds through another device's host:
        // until then it has only rooms of its own, waiting for or placed on
        // this device's host.
        let first = through_hosts.into_iter().all(|room| self.hosted_here(room));
        if held {
            return Ok(());
        }
        {
            let mut joining = self
                .relay_rooms
                .joining
                .lock()
                .map_err(|_| "host room enrollments are unavailable")?;
            let attempts = self
                .native_enrollment
                .attempts
                .lock()
                .map_err(|_| "native enrollment registry unavailable")?;
            joining.retain(|_, id| attempts.contains_key(id));
            if joining.contains_key(&room) {
                return Ok(());
            }
        }
        // A join whose exchange or installation failed keeps its proof in
        // the room's workspace: it is redeemed again exactly.
        let (local, own) = if let Some(retained) = self.retained_join(room)? {
            retained
        } else {
            let own = self.enter_host_room(host, &room.to_string())?;
            let local = tree
                .write()
                .join_native_room(name, room)
                .ok_or("the listed room is not joined")?;
            if first {
                tree.write().desktop.active = local;
            }
            (local, own)
        };
        if let PeerSyncResponse::EnrollmentPending { id } = self.accept_invitation_via(
            local,
            &own,
            crate::network::invite::enrollment_driver::Via::Iroh,
        )? {
            self.finish_when_enrolled(room, id, true)?;
        }
        tracing::info!(%room, %name, "joining a room of this person's list at its host");
        Ok(())
    }

    /// Enroll into `room` no second time while enrollment `id` runs, and
    /// install it once it completes, as a shared room's is, when `install`
    /// (else its caller polls it).
    pub(super) fn finish_when_enrolled(
        &self,
        room: RoomId,
        id: u64,
        install: bool,
    ) -> Result<(), String> {
        self.relay_rooms
            .joining
            .lock()
            .map_err(|_| "host room enrollments are unavailable")?
            .insert(room, id);
        if !install {
            return Ok(());
        }
        self.relay_rooms
            .shared
            .lock()
            .map_err(|_| "shared room enrollments are unavailable")?
            .push(id);
        // One that landed before it was listed here was not told to the loop.
        let landed = self
            .native_enrollment
            .attempts
            .lock()
            .ok()
            .and_then(|attempts| {
                attempts
                    .get(&id)
                    .map(enrollment_driver::Attempt::has_landed)
            });
        if landed == Some(true) {
            self.shared_room_landed();
        }
        Ok(())
    }

    /// What an enrollment calls once its result is stored: a shared room's
    /// is due to be installed, and the frame loop is woken to install it.
    pub(super) fn enrollment_landed(&self) -> enrollment_driver::Landed {
        let shared = Arc::clone(&self.relay_rooms.shared);
        let due = Arc::clone(&self.relay_rooms.shared_due);
        let wake = self.loop_waker();
        Arc::new(move |id| {
            if shared.lock().is_ok_and(|shared| shared.contains(&id)) {
                due.store(true, Ordering::Release);
                wake();
            }
        })
    }

    /// A listed shared room's enrollment landed: wake the loop to install it.
    fn shared_room_landed(&self) {
        self.relay_rooms.shared_due.store(true, Ordering::Release);
        (self.loop_waker())();
    }

    /// Whether a pass installing shared rooms is due, taking the ask: one
    /// landed since the last pass. A pass in progress holds the list, and
    /// the ask stays for the next.
    pub(super) fn take_shared_rooms_due(&self) -> bool {
        self.shared_rooms_pending() && self.relay_rooms.shared_due.swap(false, Ordering::AcqRel)
    }

    /// The enrollment into `room` still running, if any.
    pub(super) fn enrolling_into(&self, room: RoomId) -> Option<u64> {
        let id = *self.relay_rooms.joining.lock().ok()?.get(&room)?;
        self.native_enrollment
            .attempts
            .lock()
            .ok()?
            .contains_key(&id)
            .then_some(id)
    }

    /// Whether a shared room's enrollment is still to be installed, never
    /// waiting: a pass installing them holds the list meanwhile.
    pub(super) fn shared_rooms_pending(&self) -> bool {
        self.relay_rooms
            .shared
            .try_lock()
            .is_ok_and(|shared| !shared.is_empty())
    }

    /// Install every shared room whose enrollment completed. How each that
    /// ended went is said where it is polled ([`Self::poll_enrollment`]).
    ///
    /// Answers whether any finished: finishing one changes more than the
    /// tree (a site join's progress, the room's enrollment), which the
    /// frame loop shows.
    pub(super) fn finish_shared_rooms(&self) -> bool {
        let Ok(mut shared) = self.relay_rooms.shared.lock() else {
            return false;
        };
        #[cfg(test)]
        self.relay_rooms
            .shared_passes
            .fetch_add(1, Ordering::Relaxed);
        let before = shared.len();
        shared.retain(|id| {
            matches!(
                self.poll_enrollment(*id),
                Ok(PeerSyncResponse::EnrollmentPending { .. })
            )
        });
        shared.len() != before
    }

    /// The passes installing shared rooms run so far.
    #[cfg(test)]
    pub(super) fn shared_room_passes(&self) -> u64 {
        self.relay_rooms.shared_passes.load(Ordering::Relaxed)
    }

    /// The room and slot the running enrollment `id` is for.
    pub(super) fn enrollment_target(&self, id: u64) -> Option<(RoomId, WorkspaceId)> {
        let room = self
            .relay_rooms
            .joining
            .lock()
            .ok()?
            .iter()
            .find_map(|(room, joining)| (*joining == id).then_some(*room))?;
        let local = self
            .native_enrollment
            .attempts
            .lock()
            .ok()?
            .get(&id)?
            .local();
        Some((room, local))
    }
}

impl ShellNetwork {
    /// The person's own rooms the native owner holds, with the devices
    /// hosting them, for the person's workspace list. Never waits for the
    /// owner to open: the frame loop asks.
    pub(super) fn held_personal_rooms(&self) -> crate::shell::personal_workspaces::HeldRooms {
        #[cfg(test)]
        if let Some(held) = &self.held_personal_rooms_override {
            return held.clone();
        }
        let (Some(owner), Some(user)) = (self.opened_native_owner(), self.local_person) else {
            return crate::shell::personal_workspaces::HeldRooms::new();
        };
        owner
            .rooms()
            .into_iter()
            .filter_map(|room| {
                let host = owner
                    .with_current(room, |view| {
                        if view.owner() != user {
                            return Ok(None);
                        }
                        Ok(Some(view.transport_hint()?.host.device_public_key))
                    })
                    .ok()
                    .flatten()?;
                Some((room, host))
            })
            .collect()
    }

    /// Release the rooms the person's list let go, and enter at their
    /// hosts those it brought: what one of the person's devices does when
    /// their list changes. Blocks on the hosts and the disk: runs on the
    /// peer-sync worker.
    pub(super) fn follow_list(&self, adoption: crate::shell::personal_workspaces::Adoption) {
        for room in adoption.release {
            if let Err(error) =
                crate::shell::native_workspace_owner::release_workspace_room(&self.tree, room)
            {
                tracing::warn!(%room, %error, "a workspace let go on another device stays");
            }
        }
        let Some(user) = self.local_person else {
            return;
        };
        for entry in adoption.enter {
            let Some(host) = self.persons_host(user, entry.host) else {
                tracing::debug!(room = %entry.room, "the list names a host this device cannot reach");
                continue;
            };
            if let Err(error) = self.enter_listed_room(&host, entry.room, &entry.name) {
                tracing::warn!(room = %entry.room, %error, "a room of this person's list was not entered");
            }
        }
    }

    /// Enter every room a host of the person's lists for this person and
    /// this device does not hold: how a device learns which workspaces its
    /// person holds, from the hosts that hold them, and joins them as any
    /// member device. `only` names the host asked: a configured workspace
    /// host, a home relay the person paired, or another of the person's
    /// devices whose session is up; `None` asks every configured workspace
    /// host. A host is asked when its session comes up, a device when its
    /// session starts, the configured hosts when the person's list changes,
    /// rather than any of them on a timer. Blocks on the hosts: runs on the
    /// peer-sync worker.
    pub(super) fn enter_configured_host_rooms(&self, only: Option<iroh::EndpointId>) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        if let Err(error) = identity.current_person_material() {
            tracing::debug!(%error, "host rooms: no device certificate");
            return;
        }
        // Asking one host, the person's relays and that device are candidates too.
        let relays = only
            .map(|_| super::registered_home_peer_addrs(self.local_person).unwrap_or_default())
            .unwrap_or_default();
        let device = only
            .filter(|only| {
                self.local_person
                    .is_some_and(|user| self.personal_route_targets(&user).contains(only))
            })
            .map(|only| PersonsHost::Device {
                addr: iroh::EndpointAddr::new(only),
                device: *only.as_bytes(),
            });
        let hosts = super::configured_host_addrs()
            .into_iter()
            .map(PersonsHost::Configured)
            .chain(relays.into_iter().map(PersonsHost::Relay))
            .chain(device);
        for host in hosts.filter(|host| only.is_none_or(|only| host.endpoint() == Some(only))) {
            let listed = self
                .ask_host(&host, LIST_ROOMS, serde_json::Map::new())
                .and_then(answer_of::<Vec<RoomListing>>);
            let rooms = match listed {
                Ok(rooms) => rooms,
                Err(error) => {
                    tracing::debug!(%error, "host rooms unavailable");
                    continue;
                }
            };
            for listing in rooms {
                let Some(room) = RoomId::parse(&listing.room) else {
                    continue;
                };
                if let Err(error) = self.enter_listed_room(&host, room, &listing.name) {
                    tracing::warn!(%room, %error, "a room of this person's host was not entered");
                }
            }
        }
    }

    /// Where the host on `device`, which `user`'s list names, is asked for
    /// its own operations: this device, a home relay the person paired (at
    /// its service endpoint), or another of their devices (whose endpoint is
    /// its device key, and whose session pins that device of `user`).
    pub(super) fn persons_host(&self, user: [u8; 32], device: [u8; 32]) -> Option<PersonsHost> {
        if &device == self.local_peer_id.as_bytes() {
            return Some(PersonsHost::Here);
        }
        if let Some(home) = registered_home_peer_addrs(Some(user))
            .ok()?
            .into_iter()
            .find(|home| home.expectation.device_public_key == device)
        {
            return Some(PersonsHost::Relay(home));
        }
        let endpoint = iroh::EndpointId::from_bytes(&device).ok()?;
        Some(PersonsHost::Device {
            addr: iroh::EndpointAddr::new(endpoint),
            device,
        })
    }
}

/// A bot the person asks a room for, or takes back: a governed request
/// the room's host follows (it installs, runs and uninstalls the bot).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotAsk {
    /// Run the bot of directory entry `entry` at `version` in the room.
    Add { entry: String, version: String },
    /// Take back the room's request for directory entry `entry`.
    Remove { entry: String },
}

impl BotAsk {
    /// The room's governance command this asks for.
    #[must_use]
    pub fn command(&self) -> tilo_peer_state::workspace_membership::WorkspaceMembershipCommand {
        use tilo_peer_state::workspace_membership::{BotRequest, WorkspaceMembershipCommand};
        match self {
            Self::Add { entry, version } => WorkspaceMembershipCommand::RequestBot(BotRequest {
                entry: entry.clone(),
                version: version.clone(),
                inputs: std::collections::BTreeMap::new(),
                program: None,
            }),
            Self::Remove { entry } => WorkspaceMembershipCommand::WithdrawBotRequest {
                entry: entry.clone(),
            },
        }
    }

    /// The room governance action a host serves it as, with its arguments.
    fn governance(&self) -> (&'static str, serde_json::Map<String, serde_json::Value>) {
        let mut arguments = serde_json::Map::new();
        match self {
            Self::Add { entry, version } => {
                arguments.insert("entry".into(), entry.clone().into());
                arguments.insert("version".into(), version.clone().into());
                (REQUEST_BOT, arguments)
            }
            Self::Remove { entry } => {
                arguments.insert("entry".into(), entry.clone().into());
                (WITHDRAW_BOT_REQUEST, arguments)
            }
        }
    }
}

/// The room governance action that asks a room's host for a bot.
const REQUEST_BOT: &str = "request-bot";
/// The room governance action that takes a bot request back.
const WITHDRAW_BOT_REQUEST: &str = "withdraw-bot-request";

impl ShellNetwork {
    /// Ask for (or take back) a bot in `room`, as this device's person: at
    /// the host that governs the room, through the room's own governance
    /// action, which decides whether this person may install (uninstall)
    /// bots there; in a room this device governs, through its owner's
    /// governance writer, which decides the same. Blocks on the host: call
    /// it off the frame loop.
    ///
    /// # Errors
    ///
    /// A malformed request, a room this device holds no custody of, a
    /// person the room does not let install (uninstall) bots, or a host that
    /// did not answer.
    pub(crate) fn ask_for_bot(&self, room: RoomId, ask: &BotAsk) -> Result<(), String> {
        let command = ask.command();
        command.validate()?;
        if let Some(host) = self.host_governor(room)? {
            let (action, arguments) = ask.governance();
            let invoke = invocation(
                ROOM_GOVERNANCE_PROVIDER,
                room_governance_resource(&room.to_string()),
                action,
                arguments,
            );
            return self
                .invoke_relay(room_topic_id(room), host, invoke, ADMIN_TIMEOUT)
                .map(drop);
        }
        let owner = self.native_owner()?;
        let caller = owner.with_current(room, |view| Ok(view.device().clone()))?;
        self.commit_membership_here(&owner, room, caller, command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_name_becomes_a_bounded_room_name() {
        assert_eq!(room_name(" Plans\u{7} "), "Plans");
        assert_eq!(room_name(" \n "), "Shared workspace");
        assert_eq!(room_name(&"x".repeat(200)).chars().count(), ROOM_NAME_CHARS);
    }

    #[test]
    fn the_hosts_answers_read_as_the_rooms_they_name() {
        let created: tilo_relay_session::rooms::CreatedRoom = answer_of(serde_json::json!({
            "room": "3291bed4-ee10-492a-877c-dbe6b8027a94",
            "workspace_id": 7,
            "name": "Plans",
            "generation": 2,
            "invitation": { "invite": hex::encode([3; 32]), "invitation": hex::encode([1, 2]) },
        }))
        .expect("a created room");
        assert_eq!(created.room.workspace_id, 7);
        assert_eq!(
            created.invitation.expect("asked for").decode(),
            Ok(IssuedInvitation {
                invite_id: [3; 32],
                signed_invitation: vec![1, 2],
            })
        );
        let listed: Vec<RoomListing> = answer_of(serde_json::json!([{
            "room": "r", "workspace_id": 1, "name": "One", "generation": 1,
        }]))
        .expect("listed rooms");
        assert_eq!(listed.first().map(|room| room.name.as_str()), Some("One"));
        assert!(answer_of::<EnteredRoom>(serde_json::json!({ "changed": true })).is_err());
    }

    #[test]
    fn the_rooms_answer_is_the_invitation_it_issued_or_why_not() {
        let issued = receipt_value(ReceiptStatus::Completed {
            output: tilo_actions::ActionOutput {
                value: Some(serde_json::json!({
                    "invite": hex::encode([3; 32]),
                    "invitation": hex::encode([1, 2, 3]),
                })),
                resources: Vec::new(),
            },
        })
        .expect("completed");
        assert_eq!(
            issued_invitation(&issued),
            Ok(IssuedInvitation {
                invite_id: [3; 32],
                signed_invitation: vec![1, 2, 3],
            })
        );
        assert!(issued_invitation(&serde_json::json!({ "invite": "zz" })).is_err());
        let refused = receipt_value(ReceiptStatus::Rejected {
            error: tilo_actions::remote::ActionHostError::UnknownAction,
        });
        assert!(refused.is_err());
        assert!(receipt_value(ReceiptStatus::Accepted).is_err());
    }

    #[test]
    fn a_bot_ask_is_the_rooms_own_governance_command_and_action() {
        let add = BotAsk::Add {
            entry: "so.tilo.bots.standup".into(),
            version: "1.2.0".into(),
        };
        let (action, arguments) = add.governance();
        assert_eq!(action, "request-bot");
        assert_eq!(arguments["entry"], "so.tilo.bots.standup");
        assert_eq!(arguments["version"], "1.2.0");
        assert!(add.command().validate().is_ok());
        let remove = BotAsk::Remove {
            entry: "so.tilo.bots.standup".into(),
        };
        let (action, arguments) = remove.governance();
        assert_eq!(action, "withdraw-bot-request");
        assert_eq!(arguments.len(), 1);
        assert!(
            BotAsk::Remove {
                entry: "../standup".into()
            }
            .command()
            .validate()
            .is_err(),
            "an entry is a directory entry, never a path"
        );
    }

    #[test]
    fn every_invocation_names_its_provider() {
        let host = invocation(
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            LIST_ROOMS,
            serde_json::Map::new(),
        );
        assert_eq!(host.target.provider, "tilo.host");
        assert_eq!(host.target.address.resource, "host.rooms");
        assert_eq!(host.target.address.action, "room.list");
    }
}
