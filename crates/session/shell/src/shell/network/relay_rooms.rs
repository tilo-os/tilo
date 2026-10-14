//! Rooms a relay of this device's person governs. Sharing a workspace
//! creates its room on the person's home relay, which then issues every
//! invitation, admits whoever redeems one and publishes the room's
//! governance; this device holds the room like any member does.
//!
//! A room this device holds is administered through the room's own
//! governance actions (provider `tilo.workspace`), on the room's relay
//! session: the surface an agent, a script or a screen reader uses, decided
//! by the relay as it decides every other door. The catalogue of a session
//! does not list that provider, so every invocation names it.
//!
//! The relay's `tilo-room-admin/1` door is still asked for what those
//! actions do not carry: creating, listing and entering rooms on a host
//! (host operations, not one room's governance), the invitations this
//! device redeems into a room it is enrolling into and the one issued in
//! the same step that shares it (nothing carries an action to a room before
//! this device holds it), a member's own namespace claim and effect
//! receipt, which are not governance, and an app's authorship of a room a
//! host governs (a host operation, the host's person's own).
use super::{
    NetCmd, PendingInvitation, RegisteredHomePeer, ShellNetwork, persist_pending_invitations,
    registered_home_peer_addrs, relay_topics_for_home_peers, room_topic_id,
};
use crate::network::invite::InviteRole;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::{RoomId, WorkspaceId};
use std::sync::Mutex;
use std::time::Duration;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_peer_state::join::{self, MemberRole};
use tilo_relay_session::rooms::{
    GrantAppAuthorshipRequest, IssueInvitationRequest, RoomCommand, RoomReply,
};
use tilo_user_identity::DeviceCert;

/// How long one exchange with a relay's room authority may take.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(15);
/// The room governance action that issues an invitation.
const INVITE: &str = tilo_action_policy::roles::governance::INVITE;
/// The room governance action that cancels an unredeemed invitation.
const CANCEL_INVITATION: &str = "invitation.cancel";
/// How long the invitation this device redeems into a room it just shared
/// stays redeemable.
const OWN_INVITATION_SECONDS: u64 = 600;
/// The longest room name a relay accepts.
const ROOM_NAME_CHARS: usize = 80;

fn home_for_workspace(user: [u8; 32], room: RoomId) -> Result<Option<RegisteredHomePeer>, String> {
    // A fresh local workspace has never been registered on the relay. Its
    // person's continuity topic locates the enrolled home before Create;
    // the authenticated room commands still govern admission.
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

/// A room this device's person administers on the relay governing it.
pub(super) enum AdministeredRoom {
    /// Held here: asked through its governance actions.
    Held(HeldRoom),
    /// Not held here yet: asked at the relay's host door.
    Unheld(Box<UnheldRoom>),
}

/// A relay-governed room this device holds: its governance answers on the
/// room's relay session, from the relay's own device.
pub(super) struct HeldRoom {
    room: RoomId,
    /// The relay's own device, the room's governor.
    host: iroh::EndpointId,
}

/// A room on a relay of this person's that this device does not hold yet,
/// so has no session to it: the relay's host door, as `certificate`.
pub(super) struct UnheldRoom {
    relay: iroh::EndpointAddr,
    room: String,
    certificate: DeviceCert,
}

const fn member_role(role: InviteRole) -> MemberRole {
    match role {
        InviteRole::Viewer => MemberRole::Viewer,
        InviteRole::Commenter => MemberRole::Commenter,
        InviteRole::Editor => MemberRole::Editor,
    }
}

/// The name the room governance actions give `role`.
const fn role_name(role: InviteRole) -> &'static str {
    match role {
        InviteRole::Viewer => "viewer",
        InviteRole::Commenter => "commenter",
        InviteRole::Editor => "editor",
    }
}

/// The arguments of the governance invitation `role`, for `recipient`
/// only when named, called `invitee`, redeemable `lifetime` seconds.
fn invite_arguments(
    role: InviteRole,
    recipient: Option<[u8; 32]>,
    invitee: Option<&str>,
    lifetime: u64,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("role".into(), role_name(role).into());
    if let Some(recipient) = recipient {
        arguments.insert("recipient".into(), hex::encode(recipient).into());
    }
    if let Some(invitee) = invitee {
        arguments.insert("invitee_name".into(), invitee.into());
    }
    arguments.insert("expires_in_seconds".into(), lifetime.into());
    // A relay that predates seconds reads hours: never shorter than asked.
    arguments.insert("expires_in_hours".into(), lifetime.div_ceil(3_600).into());
    arguments
}

/// The id and the signed invitation the governance invitation answered.
fn issued_invitation(output: &serde_json::Value) -> Result<([u8; 32], Vec<u8>), String> {
    let field = |name: &str| {
        output
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("the relay's invitation has no `{name}`"))
    };
    let mut id = [0; 32];
    hex::decode_to_slice(field("invite")?, &mut id)
        .map_err(|_| "the relay named an invalid invitation id".to_string())?;
    let signed = hex::decode(field("invitation")?)
        .map_err(|_| "the relay sent an invalid invitation".to_string())?;
    Ok((id, signed))
}

/// What a governance invocation's receipt comes to.
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

fn request_id() -> Result<[u8; 32], String> {
    let mut id = [0; 32];
    getrandom::fill(&mut id).map_err(|error| error.to_string())?;
    Ok(id)
}

impl ShellNetwork {
    /// The relay governing `room`, when that relay is one of this person's:
    /// the room was shared by this person and this device holds it.
    pub(super) fn administered_room(&self, room: RoomId) -> Option<AdministeredRoom> {
        let owner = self.native_owner().ok()?;
        let identity = self.host_identity.as_ref()?;
        owner
            .with_current(room, |view| {
                let (certificate, _) = identity.current_native_material(view.workspace())?;
                if view.governor().user_public_key != certificate.user_public_key {
                    return Err("another person governs the room".into());
                }
                let (hint, _) =
                    IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint())
                        .map_err(|error| error.to_string())?;
                if !view.governor().capabilities.can_act_as_host
                    || &hint.host_device_certificate != view.governor()
                {
                    return Err("no relay governs the room".into());
                }
                let host = iroh::EndpointId::from_bytes(&view.governor().device_public_key)
                    .map_err(|error| error.to_string())?;
                Ok(AdministeredRoom::Held(HeldRoom { room, host }))
            })
            .ok()
    }

    /// Invoke `action` of `target`'s governance with `arguments`, as this
    /// device, and answer what it produced.
    fn govern(
        &self,
        target: &HeldRoom,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        let request = crate::network::actor::room_governance::Request {
            ws: room_topic_id(target.room),
            host: target.host,
            invoke: InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                // The room's provider is named: no catalogue lists it.
                target: RemoteActionTarget::room_governance(&target.room.to_string(), action),
                argument: InvokeArgument::Json(arguments),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            },
            reply,
        };
        self.cmd_tx
            .try_send(NetCmd::RoomGovernance(Box::new(request)))
            .map_err(|_| "the network queue is unavailable")?;
        let status = answer
            .recv_timeout(ADMIN_TIMEOUT)
            .map_err(|_| "the relay did not answer in time")??;
        receipt_value(status)
    }

    /// Ask the room authority at `relay` and wait for its answer.
    fn room_admin(
        &self,
        relay: iroh::EndpointAddr,
        certificate: DeviceCert,
        command: RoomCommand,
    ) -> Result<RoomReply, String> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.cmd_tx
            .try_send(NetCmd::RoomAdmin {
                relay,
                certificate,
                command: Box::new(command),
                reply,
            })
            .map_err(|_| "the network queue is unavailable")?;
        answer
            .recv_timeout(ADMIN_TIMEOUT)
            .map_err(|_| "the relay did not answer in time")?
    }

    /// A host governor answers namespace claims through its existing room
    /// authority, not a private action subscriber that the host does not run.
    /// None retains the native-device governor's ordinary semantic action path.
    pub(super) fn claim_namespace_at_host(
        &self,
        room: RoomId,
        salt: u16,
    ) -> Option<tilo_actions::remote::Pending<Result<serde_json::Value, String>>> {
        let owner = self.native_owner().ok()?;
        let route = owner.with_current(room, |view| {
            if !view.governor().capabilities.can_act_as_host {
                return Ok(None);
            }
            let identity = self
                .host_identity
                .as_ref()
                .ok_or("native identity unavailable")?;
            let (certificate, _) = identity.current_native_material(view.workspace())?;
            if view.device() != &certificate {
                return Err("topology caller differs from admitted native custody".into());
            }
            let (hint, _) =
                IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint())
                    .map_err(|error| error.to_string())?;
            if hint.room != room.to_string()
                || &hint.governing_authority != view.governor()
                || &hint.host_device_certificate != view.governor()
            {
                return Err("topology host differs from current admitted governor".into());
            }
            Ok(Some((hint.endpoint_addr, certificate)))
        });
        let (completer, answer) = tilo_actions::remote::pending();
        let (relay, certificate) = match route {
            Ok(None) => return None,
            Ok(Some(route)) => route,
            Err(error) => {
                completer.complete(Err(error));
                return Some(answer);
            }
        };
        let (reply, receiver) = std::sync::mpsc::sync_channel(1);
        let queued = self.cmd_tx.try_send(NetCmd::RoomAdmin {
            relay,
            certificate,
            command: Box::new(RoomCommand::ClaimTopologyNamespace {
                room: room.to_string(),
                salt,
            }),
            reply,
        });
        if queued.is_err() || self.runtime.is_none() {
            completer.complete(Err("topology authority queue is unavailable".into()));
            return Some(answer);
        }
        drop(queued);
        self.runtime.as_ref()?.spawn(async move {
            let result = tokio::task::spawn_blocking(move || receiver.recv_timeout(ADMIN_TIMEOUT))
                .await
                .map_err(|_| "topology reply worker stopped".to_owned())
                .and_then(|reply| {
                    reply.map_err(|_| "topology governor did not answer in time".to_owned())
                })
                .and_then(std::convert::identity)
                .and_then(|reply| match reply {
                    RoomReply::Governance { generation } => {
                        Ok(serde_json::json!({"generation": generation}))
                    }
                    _ => Err("topology governor returned an unexpected answer".into()),
                });
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
        let owner = self.native_owner()?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        let (relay, certificate, governor_lane) = owner.with_current(room, |view| {
            let (certificate, _) = identity.current_native_material(view.workspace())?;
            if view.device() != &certificate {
                return Err("effect caller differs from admitted native custody".into());
            }
            let (hint, _) =
                IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint())
                    .map_err(|error| error.to_string())?;
            if hint.room != room.to_string() || &hint.governing_authority != view.governor() {
                return Err("effect host differs from current admitted governor".into());
            }
            Ok((
                hint.endpoint_addr,
                certificate,
                !view.governor().capabilities.can_act_as_host
                    || &hint.host_device_certificate != view.governor(),
            ))
        })?;
        if governor_lane {
            return self
                .record_effect_at_governor(room, invocation, redacted)
                .await;
        }
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.cmd_tx
            .try_send(NetCmd::RoomAdmin {
                relay,
                certificate,
                command: Box::new(RoomCommand::RecordEffectReceipt {
                    room: room.to_string(),
                    invocation,
                    redacted,
                }),
                reply,
            })
            .map_err(|_| "the network queue is unavailable")?;
        let response = tokio::task::spawn_blocking(move || answer.recv_timeout(ADMIN_TIMEOUT))
            .await
            .map_err(|_| "effect reply worker stopped")?
            .map_err(|_| "effect governor did not answer in time; outcome unknown")??;
        match response {
            RoomReply::Governance { .. } => Ok(()),
            _ => Err("effect governor returned an unexpected answer".into()),
        }
    }

    /// Have the host governing `room` let `app` author the room's records,
    /// as `caller`, this device: what opening an app there for the first time
    /// asks. `None` when no host governs the room: a device governing it
    /// decides through its own owner. Answers the governance generation the
    /// host committed it in.
    pub(crate) async fn grant_app_authorship_at_host(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        app: &str,
    ) -> Option<Result<u64, String>> {
        let route = self.native_owner().and_then(|owner| {
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
                    return Err("app authorship caller differs from admitted native custody".into());
                }
                let (hint, _) =
                    IrohWorkspaceHostHintV1::authenticate_transport_hint(view.transport_hint())
                        .map_err(|error| error.to_string())?;
                if hint.room != room.to_string()
                    || &hint.governing_authority != view.governor()
                    || &hint.host_device_certificate != view.governor()
                {
                    return Err("app authorship host differs from current admitted governor".into());
                }
                Ok(Some((hint.endpoint_addr, certificate)))
            })
        });
        let (relay, certificate) = match route {
            Ok(None) => return None,
            Ok(Some(route)) => route,
            Err(error) => return Some(Err(error)),
        };
        Some(
            async {
                let (reply, answer) = std::sync::mpsc::sync_channel(1);
                self.cmd_tx
                    .try_send(NetCmd::RoomAdmin {
                        relay,
                        certificate,
                        command: Box::new(RoomCommand::GrantAppAuthorship {
                            room: room.to_string(),
                            request: GrantAppAuthorshipRequest {
                                request_id: request_id()?,
                                app: app.to_owned(),
                            },
                        }),
                        reply,
                    })
                    .map_err(|_| "the network queue is unavailable")?;
                let response =
                    tokio::task::spawn_blocking(move || answer.recv_timeout(ADMIN_TIMEOUT))
                        .await
                        .map_err(|_| "app authorship reply worker stopped")?
                        .map_err(|_| "the host did not answer the app authorship in time")??;
                match response {
                    RoomReply::Governance { generation } => Ok(generation),
                    _ => Err("the host answered the app authorship with something else".into()),
                }
            }
            .await,
        )
    }

    /// Have the relay issue an invitation to `target`. Answers the
    /// invitation's text, its id and when it expires.
    fn invite_to_relay_room(
        &self,
        target: &AdministeredRoom,
        role: InviteRole,
        recipient: Option<[u8; 32]>,
        invitee: Option<&str>,
        expiry_seconds_from_now: u64,
    ) -> Result<(String, [u8; 32], u64), String> {
        let expiry = tilo_clock::unix_now().saturating_add(expiry_seconds_from_now);
        let (id, signed) = match target {
            AdministeredRoom::Held(held) => issued_invitation(&self.govern(
                held,
                INVITE,
                invite_arguments(role, recipient, invitee, expiry_seconds_from_now),
            )?)?,
            AdministeredRoom::Unheld(unheld) => {
                self.invite_at_host_door(unheld, role, recipient, invitee, expiry)?
            }
        };
        Ok((join::encode_invitation_text(&signed), id, expiry))
    }

    /// Have the relay issue an invitation to a room this device does not
    /// hold yet, at its host door, expiring at `expiry` (Unix seconds).
    fn invite_at_host_door(
        &self,
        target: &UnheldRoom,
        role: InviteRole,
        recipient: Option<[u8; 32]>,
        invitee: Option<&str>,
        expiry: u64,
    ) -> Result<([u8; 32], Vec<u8>), String> {
        let reply = self.room_admin(
            target.relay.clone(),
            target.certificate.clone(),
            RoomCommand::Invite {
                room: target.room.clone(),
                request: IssueInvitationRequest {
                    request_id: request_id()?,
                    role: member_role(role),
                    recipient_user_public_key: recipient,
                    invitee_name: invitee.map(str::to_owned),
                    expires_at_ns: expiry.saturating_mul(1_000_000_000),
                    inviter_name: None,
                },
            },
        )?;
        let RoomReply::Invitation(issued) = reply else {
            return Err("the relay did not issue an invitation".into());
        };
        Ok((issued.invite_id, issued.signed_invitation))
    }

    /// Issue an invitation to `target` and keep it among the pending ones
    /// the owner may resend or cancel.
    pub(super) fn issue_relay_invitation(
        &self,
        target: &AdministeredRoom,
        role: InviteRole,
        recipient: Option<[u8; 32]>,
        invitee: Option<&str>,
        expiry_seconds_from_now: u64,
    ) -> Result<String, String> {
        let invitee = invitee.map(str::trim).filter(|name| !name.is_empty());
        let room = match target {
            AdministeredRoom::Held(held) => held.room,
            AdministeredRoom::Unheld(unheld) => {
                RoomId::parse(&unheld.room).ok_or("the relay named an invalid room")?
            }
        };
        let (token, id, expiry_unix_seconds) =
            self.invite_to_relay_room(target, role, recipient, invitee, expiry_seconds_from_now)?;
        let mut pending = self
            .pending_identity_admissions
            .lock()
            .map_err(|_| "invite admission state is unavailable".to_string())?;
        pending.insert(
            id,
            PendingInvitation {
                id,
                room,
                name: invitee.unwrap_or("Member").to_string(),
                role,
                token: token.clone(),
                recipient_user_public_key: recipient,
                expiry_unix_seconds,
            },
        );
        if let Some(path) = self.pending_invitations_path.as_deref() {
            persist_pending_invitations(path, &pending);
        }
        drop(pending);
        self.note_session_reachability(true);
        Ok(token)
    }

    /// Cancel `invitation` of `room` at the relay governing it. `Ok(false)`
    /// when no relay of this person governs `room`.
    pub(super) fn cancel_relay_invitation(
        &self,
        room: RoomId,
        invitation: [u8; 32],
    ) -> Result<bool, String> {
        let Some(AdministeredRoom::Held(target)) = self.administered_room(room) else {
            return Ok(false);
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("invite".into(), hex::encode(invitation).into());
        self.govern(&target, CANCEL_INVITATION, arguments)?;
        Ok(true)
    }

    /// Share the local workspace `local`, showing `room`, on this person's
    /// home relay: the relay creates a room named `name` and admits this
    /// device as its owner, and this device enrolls `local` into it.
    /// `None` when this device has no identity or its person no home relay.
    pub(super) fn share_on_home_relay(
        &self,
        local: WorkspaceId,
        room: RoomId,
        name: &str,
    ) -> Result<Option<AdministeredRoom>, String> {
        let Some(identity) = self.host_identity.as_ref() else {
            return Ok(None);
        };
        let (certificate, _) = identity.current_native_material(room_topic_id(room))?;
        let name: String = name
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(ROOM_NAME_CHARS)
            .collect();
        let name = if name.is_empty() {
            "Shared workspace".to_owned()
        } else {
            name
        };
        let Some(home) = home_for_workspace(certificate.user_public_key, room)? else {
            return Ok(None);
        };
        let RoomReply::Room(summary) = self.room_admin(
            home.addr.clone(),
            certificate.clone(),
            RoomCommand::Create { name },
        )?
        else {
            return Err("the home relay did not create the room".into());
        };
        self.room_admin(
            home.addr.clone(),
            certificate.clone(),
            RoomCommand::Enter {
                room: summary.room.clone(),
            },
        )?;
        let user = certificate.user_public_key;
        // This device does not hold the room until it redeems this
        // invitation, and the one the share is for is issued before then:
        // both at the host door.
        let target = AdministeredRoom::Unheld(Box::new(UnheldRoom {
            relay: home.addr,
            room: summary.room,
            certificate,
        }));
        let (own, _, _) = self.invite_to_relay_room(
            &target,
            InviteRole::Editor,
            Some(user),
            None,
            OWN_INVITATION_SECONDS,
        )?;
        if let PeerSyncResponse::EnrollmentPending { id } =
            self.accept_peer_invitation(local, &own)?
        {
            self.relay_rooms
                .shared
                .lock()
                .map_err(|_| "shared room enrollments are unavailable")?
                .push(id);
        }
        Ok(Some(target))
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
        let certificate = match identity.current_native_material(0) {
            Ok((certificate, _)) => certificate,
            Err(error) => {
                tracing::debug!(%error, "host rooms: no device certificate");
                return;
            }
        };
        for relay in super::configured_host_addrs() {
            let rooms = match self.room_admin(relay.clone(), certificate.clone(), RoomCommand::List)
            {
                Ok(RoomReply::Rooms(rooms)) => rooms,
                Ok(_) => continue,
                Err(error) => {
                    tracing::debug!(%error, "host rooms unavailable");
                    continue;
                }
            };
            for summary in rooms {
                if let Err(error) = self.join_host_room(&relay, &certificate, &summary) {
                    tracing::warn!(room = %summary.room, %error, "host room not joined");
                }
            }
        }
    }

    /// Enter `summary`'s room at `relay` as this person's device and enroll
    /// a workspace into it: the active one when it is the first room this
    /// session holds that way, else a new one under the room's name.
    fn join_host_room(
        &self,
        relay: &iroh::EndpointAddr,
        certificate: &DeviceCert,
        summary: &tilo_relay_session::rooms::RoomSummary,
    ) -> Result<(), String> {
        let room = RoomId::parse(&summary.room).ok_or("the host named an invalid room")?;
        let tree = self
            .replica
            .lock()
            .map_err(|_| "native replica unavailable")?
            .tree
            .clone();
        let (held, first, active) = {
            let tree = tree.read();
            let workspaces = &tree.desktop.workspaces;
            (
                workspaces.iter().any(|workspace| workspace.room == room),
                !workspaces
                    .iter()
                    .any(|workspace| tree.requires_native_owner(workspace.room)),
                tree.desktop.active,
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
        self.room_admin(
            relay.clone(),
            certificate.clone(),
            RoomCommand::Enter {
                room: summary.room.clone(),
            },
        )?;
        let target = AdministeredRoom::Unheld(Box::new(UnheldRoom {
            relay: relay.clone(),
            room: summary.room.clone(),
            certificate: certificate.clone(),
        }));
        let (own, _, _) = self.invite_to_relay_room(
            &target,
            InviteRole::Editor,
            Some(certificate.user_public_key),
            None,
            OWN_INVITATION_SECONDS,
        )?;
        let local = if first {
            active
        } else {
            tree.write().append_empty_workspace(&summary.name)
        };
        if let PeerSyncResponse::EnrollmentPending { id } =
            self.accept_peer_invitation(local, &own)?
        {
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
        tracing::info!(room = %summary.room, name = %summary.name, "joining a host room");
        Ok(())
    }

    /// Install every shared room whose enrollment completed.
    pub(super) fn finish_shared_rooms(&self) {
        let Ok(mut shared) = self.relay_rooms.shared.lock() else {
            return;
        };
        shared.retain(|id| match self.poll_enrollment(*id) {
            Ok(PeerSyncResponse::EnrollmentPending { .. }) => true,
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(%error, "a shared workspace was not enrolled");
                false
            }
        });
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
        let arguments = invite_arguments(InviteRole::Commenter, Some([7; 32]), Some("Grace"), 600);
        assert_eq!(
            serde_json::Value::Object(arguments),
            serde_json::json!({
                "role": "commenter",
                "recipient": hex::encode([7; 32]),
                "invitee_name": "Grace",
                "expires_in_seconds": 600,
                "expires_in_hours": 1,
            })
        );
        let open = invite_arguments(InviteRole::Viewer, None, None, 3_601);
        assert!(!open.contains_key("recipient") && !open.contains_key("invitee_name"));
        assert_eq!(open["expires_in_hours"], 2, "never shorter than asked");
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
        assert_eq!(issued_invitation(&issued), Ok(([3; 32], vec![1, 2, 3])));
        assert!(issued_invitation(&serde_json::json!({ "invite": "zz" })).is_err());
        let refused = receipt_value(ReceiptStatus::Rejected {
            error: tilo_actions::remote::ActionHostError::UnknownAction,
        });
        assert!(refused.is_err());
        assert!(receipt_value(ReceiptStatus::Accepted).is_err());
    }

    #[test]
    fn invitation_roles_map_to_the_room_roles_the_relay_grants() {
        assert_eq!(member_role(InviteRole::Viewer), MemberRole::Viewer);
        assert_eq!(member_role(InviteRole::Commenter), MemberRole::Commenter);
        assert_eq!(member_role(InviteRole::Editor), MemberRole::Editor);
    }
}
