//! Rooms a relay of this device's person governs. Sharing a workspace
//! creates its room on the person's home relay, which then issues every
//! invitation, admits whoever redeems one and publishes the room's
//! governance; this device holds the room like any member does.
use super::{
    NetCmd, PendingInvitation, ShellNetwork, persist_pending_invitations,
    registered_home_peer_addrs, room_topic_id,
};
use crate::network::invite::InviteRole;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::{RoomId, WorkspaceId};
use std::sync::Mutex;
use std::time::Duration;
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_peer_state::join::{self, MemberRole};
use tilo_relay_session::rooms::{
    CancelInvitationRequest, IssueInvitationRequest, RoomCommand, RoomReply,
};
use tilo_user_identity::DeviceCert;

/// How long one exchange with a relay's room authority may take.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the invitation this device redeems into a room it just shared
/// stays redeemable.
const OWN_INVITATION_SECONDS: u64 = 600;
/// The longest room name a relay accepts.
const ROOM_NAME_CHARS: usize = 80;

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
pub(super) struct AdministeredRoom {
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
                Ok(AdministeredRoom {
                    relay: hint.endpoint_addr,
                    room: room.to_string(),
                    certificate,
                })
            })
            .ok()
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
        Ok((
            join::encode_invitation_text(&issued.signed_invitation),
            issued.invite_id,
            expiry,
        ))
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
        let room = RoomId::parse(&target.room).ok_or("the relay named an invalid room")?;
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
        Ok(token)
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
        self.room_admin(
            target.relay,
            target.certificate,
            RoomCommand::Cancel {
                room: target.room,
                request: CancelInvitationRequest {
                    request_id: request_id()?,
                    invite_id: invitation,
                },
            },
        )?;
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
        let Some(home) = registered_home_peer_addrs(
            Some(certificate.user_public_key),
            &std::collections::BTreeSet::from([room_topic_id(room)]),
        )?
        .into_iter()
        .next() else {
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
        let target = AdministeredRoom {
            relay: home.addr,
            room: summary.room,
            certificate,
        };
        let (own, _, _) = self.invite_to_relay_room(
            &target,
            InviteRole::Editor,
            Some(target.certificate.user_public_key),
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
        let target = AdministeredRoom {
            relay: relay.clone(),
            room: summary.room.clone(),
            certificate: certificate.clone(),
        };
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
    fn invitation_roles_map_to_the_room_roles_the_relay_grants() {
        assert_eq!(member_role(InviteRole::Viewer), MemberRole::Viewer);
        assert_eq!(member_role(InviteRole::Commenter), MemberRole::Commenter);
        assert_eq!(member_role(InviteRole::Editor), MemberRole::Editor);
    }
}
