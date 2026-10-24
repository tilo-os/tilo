//! Rooms a relay of this device's person governs. Sharing a workspace
//! creates its room on the person's home relay, which then issues every
//! invitation, admits whoever redeems one and publishes the room's
//! governance; this device holds the room like any member does.
//!
//! Everything here is asked as the relay's own actions, over a relay
//! session, addressed to the relay's own device: the surface an agent, a
//! script or a screen reader uses, decided by the relay as it decides every
//! other door. None of these providers is listed in a session's catalogue,
//! so every invocation names its own.
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
    personal_continuity_topic_id, registered_home_peer_addrs, relay_topics_for_home_peers,
    room_topic_id,
};
use crate::network::actor::room_governance::{Addressee, Request};
use crate::network::invite::InviteRole;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::{RoomId, WorkspaceId};
use std::sync::Mutex;
use std::time::Duration;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ROOM_GOVERNANCE_PROVIDER, ReceiptStatus, RemoteActionTarget,
    room_governance_resource,
};
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_peer_state::join;
use tilo_relay_session::rooms::{
    CLAIM_TOPOLOGY_NAMESPACE, CREATE_ROOM, CreatedRoom, ENTER_ROOM, EnteredRoom,
    GRANT_APP_AUTHORSHIP, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, IssuedInvitation, LIST_ROOMS,
    MEMBER_ACTIONS_PROVIDER, PREPARE_APPLICATION_RECORD, RECORD_EFFECT_RECEIPT, RoomListing,
    member_resource,
};

/// How long one invocation on a room's own session may take.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one host operation may take: the host's session of the
/// continuity workspace may still have to come up.
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

fn home_for_workspace(user: [u8; 32], room: RoomId) -> Result<Option<RegisteredHomePeer>, String> {
    // A fresh local workspace has never been registered on the relay. Its
    // person's continuity topic locates the enrolled home before the room
    // is created; the relay's own decision still governs admission.
    let topics = relay_topics_for_home_peers([room], Some(user));
    Ok(registered_home_peer_addrs(Some(user), &topics)?
        .into_iter()
        .next())
}

/// Set where the session holds every workspace that a configured host of
/// its person governs (a company node's own session, which nobody sits at):
/// it joins each one, as one of the person's devices, once it appears.
pub const FOLLOW_HOST_ROOMS_ENV: &str = "TILO_SHELL_FOLLOW_HOST_ROOMS";
/// How often the session asks its hosts for rooms it does not hold yet.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct State {
    /// Enrollments of this device into the rooms it shared, installed as
    /// they complete.
    shared: Mutex<Vec<u64>>,
    /// Host rooms this device is enrolling into, by enrollment.
    joining: Mutex<std::collections::HashMap<RoomId, u64>>,
}

/// A relay-governed room this device holds and its person administers: its
/// governance answers on the room's relay session, from the relay's own
/// device.
pub(super) struct HeldRoom {
    room: RoomId,
    /// The relay's own device, the room's governor.
    host: iroh::EndpointId,
}

impl HeldRoom {
    pub(super) const fn room(&self) -> RoomId {
        self.room
    }
}

/// What moving a workspace onto the person's home relay made.
pub(super) struct MovedRoom {
    /// The room the relay governs now.
    pub room: RoomId,
    /// The first invitation, when one was asked for.
    pub issued: Option<IssuedInvitation>,
    /// This device's enrollment into the room, while it is installed.
    pub enrollment: Option<u64>,
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

/// The arguments of creating a room named `name`, with `first` as its first
/// invitation when there is one.
fn create_arguments(
    name: &str,
    first: Option<&RelayInvitation<'_>>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), name.into());
    let Some(first) = first else {
        return arguments;
    };
    arguments.insert("invite_role".into(), role_name(first.role).into());
    if let Some(recipient) = first.recipient {
        arguments.insert("invite_recipient".into(), hex::encode(recipient).into());
    }
    if let Some(invitee) = first.invitee() {
        arguments.insert("invitee_name".into(), invitee.into());
    }
    arguments.insert("invite_expires_in_seconds".into(), first.lifetime.into());
    arguments
}

/// The name a room shared from a workspace called `name` gets.
fn room_name(name: &str) -> String {
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
fn answer_of<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("the relay answered unexpectedly: {error}"))
}

/// What an invocation's receipt comes to.
fn receipt_value(status: ReceiptStatus) -> Result<serde_json::Value, String> {
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
fn invocation(
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
    /// The relay governing `room`, when that relay is one of this person's:
    /// the room was shared by this person and this device holds it.
    pub(super) fn administered_room(&self, room: RoomId) -> Option<HeldRoom> {
        let owner = self.native_owner().ok()?;
        let identity = self.host_identity.as_ref()?;
        owner
            .with_current(room, |view| {
                let (certificate, _) = identity.current_native_material(view.workspace())?;
                if view.governor().user_public_key != certificate.user_public_key {
                    return Err("another person governs the room".into());
                }
                Ok(())
            })
            .ok()?;
        self.relay_governed_room(room)
    }

    /// The relay governing `room`, whoever's it is, when this device holds
    /// the room: a member asks it for what the room's policy lets them do,
    /// such as inviting someone, and the relay decides.
    pub(super) fn relay_governed_room(&self, room: RoomId) -> Option<HeldRoom> {
        let owner = self.native_owner().ok()?;
        owner
            .with_current(room, |view| {
                let host = crate::shell::invite_policy::governing_relay(view)
                    .ok_or("no relay governs the room")?;
                Ok(HeldRoom { room, host })
            })
            .ok()
    }

    /// Queue `invoke` on `ws`'s relay session for `to`, and answer where
    /// its receipt arrives.
    fn ask_relay(
        &self,
        ws: u64,
        to: Addressee,
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
        to: Addressee,
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
            Addressee::Device(target.host),
            invoke,
            ADMIN_TIMEOUT,
        )
    }

    /// Invoke the host operation `action` with `arguments` at the host at
    /// `relay`, as this device of `user`, on the host's session of the
    /// person's continuity workspace.
    fn host_action(
        &self,
        relay: iroh::EndpointId,
        user: [u8; 32],
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let ws = personal_continuity_topic_id(&user);
        // Every host is dialled with each topic this device joined.
        if !self.join_topic(ws) {
            return Err("the network queue is unavailable".into());
        }
        let invoke = invocation(
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            action,
            arguments,
        );
        self.invoke_relay(ws, Addressee::Host(relay), invoke, HOST_TIMEOUT)
    }

    /// Dial `home` as a workspace host if this session does not already.
    fn connect_home(&self, home: &RegisteredHomePeer) {
        let Ok(mut hosts) = self.host_addrs.lock() else {
            return;
        };
        if hosts.iter().any(|known| known.id == home.addr.id) {
            return;
        }
        if self
            .cmd_tx
            .try_send(NetCmd::ConnectHost {
                addr: home.addr.clone(),
                expectation: Some(home.expectation.clone()),
            })
            .is_ok()
        {
            hosts.push(home.addr.clone());
        }
    }

    /// The device of the room's admitted governor when a host governs
    /// `room`, as this device's current admitted custody of it names it.
    /// `Ok(None)` when another device governs it.
    fn host_governor(&self, room: RoomId) -> Result<Option<iroh::EndpointId>, String> {
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
            let (hint, _) =
                IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint()?)
                    .map_err(|error| error.to_string())?;
            if hint.room != room.to_string() || &hint.governing_authority != view.governor() {
                return Err("the room's host differs from its current admitted governor".into());
            }
            if !view.governor().capabilities.can_act_as_host
                || &hint.host_device_certificate != view.governor()
            {
                return Ok(None);
            }
            iroh::EndpointId::from_bytes(&view.governor().device_public_key)
                .map(Some)
                .map_err(|error| error.to_string())
        })
    }

    /// Queue the member's own `action` with `arguments` in `room`, at its
    /// host governor `host`.
    fn ask_as_member(
        &self,
        room: RoomId,
        host: iroh::EndpointId,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<std::sync::mpsc::Receiver<Result<ReceiptStatus, String>>, String> {
        let invoke = invocation(
            MEMBER_ACTIONS_PROVIDER,
            member_resource(&room.to_string()),
            action,
            arguments,
        );
        self.ask_relay(room_topic_id(room), Addressee::Device(host), invoke)
    }

    /// A host governor answers namespace claims as the member's own action
    /// on the room's session, not a private action subscriber that the host
    /// does not run. None retains the native-device governor's ordinary
    /// semantic action path.
    pub(super) fn claim_namespace_at_host(
        &self,
        room: RoomId,
        salt: u16,
    ) -> Option<tilo_actions::remote::Pending<Result<serde_json::Value, String>>> {
        // A room a device governs keeps its own semantic action path, and
        // without native custody this device holds no room a host governs.
        let hosted = self.native_owner().ok()?.with_current(room, |view| {
            Ok(view.governor().capabilities.can_act_as_host)
        });
        if matches!(hosted, Ok(false)) {
            return None;
        }
        let (completer, answer) = tilo_actions::remote::pending();
        let host = match self.host_governor(room) {
            Ok(None) => return None,
            Ok(Some(host)) => host,
            Err(error) => {
                completer.complete(Err(error));
                return Some(answer);
            }
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("salt".into(), salt.into());
        let receiver = match self.ask_as_member(room, host, CLAIM_TOPOLOGY_NAMESPACE, arguments) {
            Ok(receiver) => receiver,
            Err(error) => {
                completer.complete(Err(error));
                return Some(answer);
            }
        };
        let Some(runtime) = self.runtime.as_ref() else {
            completer.complete(Err("topology authority queue is unavailable".into()));
            return Some(answer);
        };
        runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || receiver.recv_timeout(ADMIN_TIMEOUT))
                .await
                .map_err(|_| "topology reply worker stopped".to_owned())
                .and_then(|reply| {
                    reply.map_err(|_| "topology governor did not answer in time".to_owned())
                })
                .and_then(std::convert::identity)
                .and_then(receipt_value);
            completer.complete(result);
        });
        Some(answer)
    }

    /// Send a person's own bounded terminal audit to the exact signed governor.
    /// Room administration retains its independent same-person owner checks.
    pub(crate) async fn record_governed_effect_receipt(
        &self,
        room: RoomId,
        invocation: uuid::Uuid,
        redacted: Vec<u8>,
    ) -> Result<(), String> {
        let Some(host) = self.host_governor(room)? else {
            return self
                .record_effect_at_governor(room, invocation, redacted)
                .await;
        };
        let redacted = String::from_utf8(redacted).map_err(|_| "invalid effect encoding")?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("invocation".into(), invocation.to_string().into());
        arguments.insert("redacted".into(), redacted.into());
        let answer = self.ask_as_member(room, host, RECORD_EFFECT_RECEIPT, arguments)?;
        let status = tokio::task::spawn_blocking(move || answer.recv_timeout(ADMIN_TIMEOUT))
            .await
            .map_err(|_| "effect reply worker stopped")?
            .map_err(|_| "effect governor did not answer in time; outcome unknown")??;
        receipt_value(status).map(drop)
    }

    /// Have the host governing `room` let `app` author the room's records,
    /// as `caller`, this device: what opening an app there for the first time
    /// asks, the host's own operation on the room's session. `None` when no
    /// host governs the room: a device governing it decides through its own
    /// owner. Answers the governance generation the host committed it in.
    pub(crate) async fn grant_app_authorship_at_host(
        &self,
        room: RoomId,
        caller: &tilo_user_identity::DeviceCert,
        app: &str,
    ) -> Option<Result<u64, String>> {
        let host = match self.governing_host(room, caller) {
            Ok(None) => return None,
            Ok(Some(host)) => host,
            Err(error) => return Some(Err(error)),
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("room".into(), room.to_string().into());
        arguments.insert("app".into(), app.into());
        let invoke = invocation(
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            GRANT_APP_AUTHORSHIP,
            arguments,
        );
        Some(
            async {
                let answer =
                    self.ask_relay(room_topic_id(room), Addressee::Device(host), invoke)?;
                let status =
                    tokio::task::spawn_blocking(move || answer.recv_timeout(ADMIN_TIMEOUT))
                        .await
                        .map_err(|_| "app authorship reply worker stopped")?
                        .map_err(|_| "the host did not answer the app authorship in time")??;
                receipt_value(status)?
                    .get("generation")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        "the host answered the app authorship with something else".into()
                    })
            }
            .await,
        )
    }

    /// Have the host governing `room` create `app`'s record `record` there
    /// (`""` for the primary), empty and signed by the host, as `caller`,
    /// this device: what opening an app there for the first time asks when
    /// the room does not hold its record. The app's first state then goes to
    /// the record as this device's own change. `None` when no host governs
    /// the room: a device governing it creates the record through its own
    /// owner. Answers whether the host created it (rather than finding it).
    pub(crate) async fn prepare_application_record_at_host(
        &self,
        room: RoomId,
        caller: &tilo_user_identity::DeviceCert,
        app: &str,
        record: &str,
    ) -> Option<Result<bool, String>> {
        let host = match self.governing_host(room, caller) {
            Ok(None) => return None,
            Ok(Some(host)) => host,
            Err(error) => return Some(Err(error)),
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("app".into(), app.into());
        arguments.insert("record".into(), record.into());
        let invoke = invocation(
            MEMBER_ACTIONS_PROVIDER,
            member_resource(&room.to_string()),
            PREPARE_APPLICATION_RECORD,
            arguments,
        );
        Some(
            async {
                let answer =
                    self.ask_relay(room_topic_id(room), Addressee::Device(host), invoke)?;
                let status =
                    tokio::task::spawn_blocking(move || answer.recv_timeout(ADMIN_TIMEOUT))
                        .await
                        .map_err(|_| "app record reply worker stopped")?
                        .map_err(|_| "the host did not answer the app record in time")??;
                receipt_value(status)?
                    .get("created")
                    .and_then(serde_json::Value::as_bool)
                    .ok_or_else(|| "the host answered the app record with something else".into())
            }
            .await,
        )
    }

    /// The device of the host governing `room`, to ask a host or member
    /// operation of as `caller`, this device's admitted custody of the room:
    /// pinned to the current governor and its authenticated transport hint.
    /// `None` when no host governs the room.
    fn governing_host(
        &self,
        room: RoomId,
        caller: &tilo_user_identity::DeviceCert,
    ) -> Result<Option<iroh::EndpointId>, String> {
        self.native_owner().and_then(|owner| {
            owner.with_current(room, |view| {
                if !view.governor().capabilities.can_act_as_host {
                    return Ok(None);
                }
                let identity = self
                    .host_identity
                    .as_ref()
                    .ok_or("native identity unavailable")?;
                let (certificate, _) = identity.current_native_material(view.workspace())?;
                if view.device() != &certificate || caller != &certificate {
                    return Err("host operation caller differs from admitted native custody".into());
                }
                let (hint, _) =
                    IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint()?)
                        .map_err(|error| error.to_string())?;
                if hint.room != room.to_string()
                    || &hint.governing_authority != view.governor()
                    || &hint.host_device_certificate != view.governor()
                {
                    return Err("host differs from current admitted governor".into());
                }
                iroh::EndpointId::from_bytes(&view.governor().device_public_key)
                    .map(Some)
                    .map_err(|error| error.to_string())
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

    /// Share the local workspace `local`, which is `room`, on this person's
    /// home relay: the relay takes over governing that same room, named
    /// `name`, with the invitation `first`, and admits this device as its
    /// owner, and this device enrolls `local` into it. Answers the text of
    /// `first`, or `None` when this device has no identity or its person no
    /// home relay.
    pub(super) fn share_on_home_relay(
        &self,
        local: WorkspaceId,
        room: RoomId,
        name: &str,
        first: &RelayInvitation<'_>,
    ) -> Result<Option<String>, String> {
        let Some(moved) = self.move_to_home_relay(local, room, name, Some(first))? else {
            return Ok(None);
        };
        if let Some(id) = moved.enrollment {
            self.relay_rooms
                .shared
                .lock()
                .map_err(|_| "shared room enrollments are unavailable")?
                .push(id);
        }
        let issued = moved
            .issued
            .ok_or("the relay did not issue the invitation")?;
        self.remember_relay_invitation(moved.room, &issued, first)
            .map(Some)
    }

    /// Move the governance of the workspace `local`, which is `room` and is
    /// called `name`, to this person's home relay, which governs it from
    /// then on: the relay takes the room over under its own identity (a
    /// workspace is its room, and keeps it when its governance moves),
    /// issuing `first` with it when given, and this device holds the same
    /// workspace as a member of the room the relay governs. `None` when the
    /// person has no home relay. The caller finishes the returned
    /// enrollment.
    pub(super) fn move_to_home_relay(
        &self,
        local: WorkspaceId,
        room: RoomId,
        name: &str,
        first: Option<&RelayInvitation<'_>>,
    ) -> Result<Option<MovedRoom>, String> {
        let Some(identity) = self.host_identity.as_ref() else {
            return Ok(None);
        };
        let (certificate, _) = identity.current_native_material(room_topic_id(room))?;
        let user = certificate.user_public_key;
        let Some(home) = home_for_workspace(user, room)? else {
            return Ok(None);
        };
        self.connect_home(&home);
        let mut arguments = create_arguments(&room_name(name), first);
        arguments.insert("room".into(), room.to_string().into());
        let created: CreatedRoom =
            answer_of(self.host_action(home.addr.id, user, CREATE_ROOM, arguments)?)?;
        if RoomId::parse(&created.room.room) != Some(room) {
            return Err(
                "the home relay made another room instead of taking this one over; it needs to update"
                    .into(),
            );
        }
        let issued = match first {
            Some(_) => Some(
                created
                    .invitation
                    .ok_or("the relay did not issue the invitation")?
                    .decode()?,
            ),
            None => None,
        };
        let own = self.enter_host_room(home.addr.id, user, &created.room.room)?;
        // The same workspace joins the room the relay governs now: it stays
        // out of the shell's own engine from here, and says Joining… until
        // the relay's replica installs into it.
        let replica = self
            .replica
            .lock()
            .map_err(|_| "native replica unavailable")?;
        let joined = replica.tree.write().join_native_room(name, room);
        drop(replica);
        if joined != Some(local) {
            return Err("the workspace moving to the home relay is gone".into());
        }
        let enrollment = match self.accept_invitation_via(
            local,
            &own,
            crate::network::invite::enrollment_driver::Via::Iroh,
        )? {
            PeerSyncResponse::EnrollmentPending { id } => Some(id),
            _ => None,
        };
        Ok(Some(MovedRoom {
            room,
            issued,
            enrollment,
        }))
    }

    /// Enter `room` at the host at `relay` as this device of `user`, and
    /// answer the text of the invitation this device redeems to hold it.
    fn enter_host_room(
        &self,
        relay: iroh::EndpointId,
        user: [u8; 32],
        room: &str,
    ) -> Result<String, String> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("room".into(), room.into());
        arguments.insert("expires_in_seconds".into(), OWN_INVITATION_SECONDS.into());
        let entered: EnteredRoom =
            answer_of(self.host_action(relay, user, ENTER_ROOM, arguments)?)?;
        let own = entered.invitation.decode()?;
        Ok(join::encode_invitation_text(&own.signed_invitation))
    }

    /// With [`FOLLOW_HOST_ROOMS_ENV`] set, keep joining the rooms the
    /// configured hosts of this person govern, as they appear.
    pub(crate) fn follow_host_rooms(self: &std::sync::Arc<Self>) {
        if std::env::var_os(FOLLOW_HOST_ROOMS_ENV).is_none() {
            return;
        }
        let net = std::sync::Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("tilo-host-rooms".into())
            .spawn(move || {
                while let Some(net) = net.upgrade() {
                    net.join_host_rooms();
                    drop(net);
                    std::thread::sleep(FOLLOW_INTERVAL);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "host rooms are not followed");
        }
    }

    /// Join every room a configured host of this person governs that no
    /// workspace here holds yet.
    pub(super) fn join_host_rooms(&self) {
        let Some(identity) = self.host_identity.as_ref() else {
            return;
        };
        let user = match identity.current_native_material(0) {
            Ok((certificate, _)) => certificate.user_public_key,
            Err(error) => {
                tracing::debug!(%error, "host rooms: no device certificate");
                return;
            }
        };
        for relay in super::configured_host_addrs() {
            let listed = self
                .host_action(relay.id, user, LIST_ROOMS, serde_json::Map::new())
                .and_then(answer_of::<Vec<RoomListing>>);
            let rooms = match listed {
                Ok(rooms) => rooms,
                Err(error) => {
                    tracing::debug!(%error, "host rooms unavailable");
                    continue;
                }
            };
            for listing in rooms {
                if let Err(error) = self.join_host_room(relay.id, user, &listing) {
                    tracing::warn!(room = %listing.room, %error, "host room not joined");
                }
            }
        }
    }

    /// Enter `listing`'s room at the host at `relay` as this device of
    /// `user` and join it as a workspace of its own, the room, shown at once
    /// when it is the first room this session holds that way.
    fn join_host_room(
        &self,
        relay: iroh::EndpointId,
        user: [u8; 32],
        listing: &RoomListing,
    ) -> Result<(), String> {
        let room = RoomId::parse(&listing.room).ok_or("the host named an invalid room")?;
        let tree = self
            .replica
            .lock()
            .map_err(|_| "native replica unavailable")?
            .tree
            .clone();
        let (held, first) = {
            let tree = tree.read();
            (
                tree.holds_room(room),
                !tree
                    .desktop
                    .workspaces
                    .iter()
                    .any(|workspace| tree.requires_native_owner(workspace.room())),
            )
        };
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
            let own = self.enter_host_room(relay, user, &listing.room)?;
            let local = tree
                .write()
                .join_native_room(&listing.name, room)
                .ok_or("the host named a room that is not joined")?;
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
            self.relay_rooms
                .joining
                .lock()
                .map_err(|_| "host room enrollments are unavailable")?
                .insert(room, id);
            self.relay_rooms
                .shared
                .lock()
                .map_err(|_| "shared room enrollments are unavailable")?
                .push(id);
        }
        tracing::info!(room = %listing.room, name = %listing.name, "joining a host room");
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
        Ok(())
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
    pub(super) fn finish_shared_rooms(&self) {
        let Ok(mut shared) = self.relay_rooms.shared.lock() else {
            return;
        };
        shared.retain(|id| {
            matches!(
                self.poll_enrollment(*id),
                Ok(PeerSyncResponse::EnrollmentPending { .. })
            )
        });
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
                .invoke_relay(
                    room_topic_id(room),
                    Addressee::Device(host),
                    invoke,
                    ADMIN_TIMEOUT,
                )
                .map(drop);
        }
        let owner = self.native_owner()?;
        let caller = owner.with_current(room, |view| Ok(view.device().clone()))?;
        let request = tilo_owner_commit::OwnerCommitRequest {
            room: room.to_string(),
            operation: tilo_owner_commit::CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller,
                command,
            },
        };
        let (key, governor) = tilo_identity_host::broker::trusted_current_device(&request.room)
            .map_err(|error| format!("workspace governor is unavailable: {error}"))?;
        owner.commit_workspace_membership_command(
            request,
            &governor,
            &key,
            std::time::SystemTime::now(),
        )?;
        self.schedule_native_owner_publication(room);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(process_env)]
    fn fresh_workspace_finds_its_enrolled_home_on_the_person_topic() {
        let temp = tempfile::tempdir().expect("temp");
        let registry = temp.path().join("home-peers.json");
        let user = super::super::decode_lower_hex_32(
            "61fd5d11de98fe19ddba9ae2149dd14abb8c3c8c72473bb787b0f1552f751241",
        )
        .expect("public person key");
        let room = RoomId::parse("3291bed4-ee10-492a-877c-dbe6b8027a94").expect("fresh workspace");
        let service = iroh::SecretKey::from_bytes(&[0x22; 32]).public();
        let device = iroh::SecretKey::from_bytes(&[0x33; 32]).public();
        let registered_topics = [
            12_552_341_473_564_032_145_u64,
            7_702_587_935_360_457_883,
            4_296_909_156_967_491_521,
        ];
        assert!(!registered_topics.contains(&room_topic_id(room)));
        assert!(registered_topics.contains(&super::super::personal_continuity_topic_id(&user)));
        std::fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!([{
                "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                "device_node_id": hex::encode(device.as_bytes()),
                "relay_service_node_id": hex::encode(service.as_bytes()),
                "label": "Enrolled home",
                "user_public_key": hex::encode(user),
                "relay_workspace_ids": registered_topics,
            }]))
            .expect("registry"),
        )
        .expect("write registry");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &registry);
        let home = home_for_workspace(user, room)
            .expect("lookup")
            .expect("enrolled home exists before the room is created");
        assert_eq!(home.addr.id, service);
        assert_eq!(home.expectation.user_public_key, user);
        assert_eq!(home.expectation.device_public_key, *device.as_bytes());
        assert!(
            home_for_workspace([0x44; 32], room)
                .expect("other person")
                .is_none()
        );
    }

    #[test]
    fn an_invitation_asks_the_rooms_action_for_exactly_what_was_chosen() {
        let grace = RelayInvitation {
            role: InviteRole::Commenter,
            recipient: Some([7; 32]),
            invitee: Some(" Grace "),
            lifetime: 600,
        };
        assert_eq!(
            serde_json::Value::Object(invite_arguments(&grace)),
            serde_json::json!({
                "role": "commenter",
                "recipient": hex::encode([7; 32]),
                "invitee_name": "Grace",
                "expires_in_seconds": 600,
            })
        );
        let open = invite_arguments(&RelayInvitation {
            role: InviteRole::Viewer,
            recipient: None,
            invitee: Some("  "),
            lifetime: 3_601,
        });
        assert!(!open.contains_key("recipient") && !open.contains_key("invitee_name"));
        assert_eq!(open["expires_in_seconds"], 3_601);
    }

    #[test]
    fn moving_a_workspace_to_publish_it_creates_its_room_without_an_invitation() {
        assert_eq!(
            serde_json::Value::Object(create_arguments(&room_name("Coffee"), None)),
            serde_json::json!({ "name": "Coffee" })
        );
    }

    #[test]
    fn sharing_creates_the_room_with_exactly_the_invitation_chosen() {
        let grace = RelayInvitation {
            role: InviteRole::Editor,
            recipient: Some([9; 32]),
            invitee: Some("Grace"),
            lifetime: 3_600,
        };
        assert_eq!(
            serde_json::Value::Object(create_arguments(&room_name(" Plans\u{7} "), Some(&grace))),
            serde_json::json!({
                "name": "Plans",
                "invite_role": "editor",
                "invite_recipient": hex::encode([9; 32]),
                "invitee_name": "Grace",
                "invite_expires_in_seconds": 3_600,
            })
        );
        assert_eq!(room_name(" \n "), "Shared workspace");
        assert_eq!(room_name(&"x".repeat(200)).chars().count(), ROOM_NAME_CHARS);
    }

    #[test]
    fn the_hosts_answers_read_as_the_rooms_they_name() {
        let created: CreatedRoom = answer_of(serde_json::json!({
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
        let member = invocation(
            MEMBER_ACTIONS_PROVIDER,
            member_resource("r"),
            CLAIM_TOPOLOGY_NAMESPACE,
            serde_json::Map::new(),
        );
        assert_eq!(member.target.provider, "tilo.member");
        assert_eq!(member.target.address.resource, "workspace.member:r");
    }
}
