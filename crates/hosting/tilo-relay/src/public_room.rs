//! Hosting a public room whose owner delegated admission to this relay.
//!
//! A public room (for example the Tilo community room) is owned by a person
//! whose device writes its governance. That device is not always online, but
//! strangers must be able to join whenever they like by redeeming the room's
//! standing invitation. The owner therefore signs this relay a delegation — an
//! attenuable [`collab_types::InviteToken`] whose subject is the relay's
//! device key, capped at Commenter — and the operator registers the room's
//! standing invitations here (`tilo-relay-admin community …`).
//!
//! On [`crate::transport::Envelope::Redeem`] the relay checks the invitation
//! is registered, unrevoked, unexpired, standing and the room's own; admits
//! the redeeming connection's authenticated device into the room's
//! membership; and publishes a signed [`collab_types::HostAdmissionV1`] into
//! the room as a retained [`neighbour_wire::NeighbourMsg::RoomAdmission`].
//! Every member verifies that record against the room owner themselves, so
//! the relay's word is never taken on trust: it can admit Commenters and
//! nothing else.
//!
//! The record lives in `<state_root>/workspaces/<id>/public-room.json`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use collab_types::{Ed25519PublicKey, HostAdmissionSpec, HostAdmissionV1, InviteToken, Role};
use serde::{Deserialize, Serialize};

use crate::store::StoreError;

const PUBLIC_ROOM_FILE: &str = "public-room.json";

/// One standing invitation this relay redeems for the room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedStandingInvite {
    /// Hex of `neighbour_wire::invite::invite_id` of `token`.
    pub id: String,
    /// The exact signed bearer, as the room's authority minted it.
    pub token: String,
    /// Revoked by the operator on the owner's behalf: no further
    /// redemptions. People it already admitted stay members.
    #[serde(default)]
    pub revoked: bool,
}

/// Everything this relay knows about hosting one public room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRoomHosting {
    /// The room, as its canonical uuid string.
    pub room: String,
    /// The room owner's user root key; the delegation is rooted here.
    #[serde(with = "hex_key")]
    pub owner: [u8; 32],
    /// Owner-rooted delegation whose last subject is this relay's device.
    pub delegation: Vec<InviteToken>,
    /// Standing invitations registered for redemption.
    #[serde(default)]
    pub invites: Vec<HostedStandingInvite>,
}

/// Why this relay cannot admit a redemption, or cannot register a hosting.
#[derive(Debug, thiserror::Error)]
pub enum PublicRoomError {
    /// The workspace is bound but hosts no public room.
    #[error("this relay hosts no public room for the workspace")]
    NotHosted,
    /// The redeemed invitation was never registered with this relay.
    #[error("the standing invitation is not registered here")]
    UnknownInvite,
    /// The redeemed invitation was revoked.
    #[error("the standing invitation was revoked")]
    Revoked,
    /// The invitation does not validate as this room's standing invitation.
    #[error("invalid standing invitation: {0}")]
    InvalidInvite(String),
    /// The owner's delegation does not cover this relay and room now.
    #[error("the delegation does not let this relay admit into the room: {0}")]
    Delegation(String),
    /// Signing the admission failed.
    #[error("admission: {0}")]
    Admission(#[from] collab_types::HostAdmissionError),
    /// The workspace store failed.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// The hosting record could not be encoded or decoded.
    #[error("public room record: {0}")]
    Json(#[from] serde_json::Error),
}

mod hex_key {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(key: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(key))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        let bytes = hex::decode(text).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("a key is 32 bytes"))
    }
}

fn path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(PUBLIC_ROOM_FILE)
}

impl PublicRoomHosting {
    /// Load the hosting record in a workspace directory, if any.
    ///
    /// # Errors
    ///
    /// A read or decode failure of an existing record.
    pub fn load(workspace_root: &Path) -> Result<Option<Self>, PublicRoomError> {
        match std::fs::read(path(workspace_root)) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StoreError::Io(error).into()),
        }
    }

    /// Atomically replace the hosting record.
    ///
    /// # Errors
    ///
    /// An encode or write failure.
    pub fn save(&self, workspace_root: &Path) -> Result<(), PublicRoomError> {
        let bytes = serde_json::to_vec_pretty(self)?;
        atomic_file::write(&path(workspace_root), &bytes).map_err(StoreError::Io)?;
        Ok(())
    }

    /// Check that `delegation` lets `relay_device` admit into `room` for
    /// `owner` right now, returning the workspace id it is scoped to.
    ///
    /// # Errors
    ///
    /// [`PublicRoomError::Delegation`] naming the first failed check.
    pub fn check_delegation(
        room: &str,
        owner: [u8; 32],
        delegation: &[InviteToken],
        relay_device: [u8; 32],
        now: SystemTime,
    ) -> Result<u64, PublicRoomError> {
        let workspace = crate::room_workspace_id(room)
            .map_err(|error| PublicRoomError::Delegation(format!("room id: {error}")))?;
        let delegate =
            collab_types::verify_chain(delegation, now, &[Ed25519PublicKey::from_bytes(owner)])
                .map_err(|error| PublicRoomError::Delegation(error.to_string()))?;
        if delegate.subject.as_bytes() != &relay_device {
            return Err(PublicRoomError::Delegation(
                "it was issued to another device".into(),
            ));
        }
        if delegate.workspace != workspace || delegate.sub_resource.is_some() {
            return Err(PublicRoomError::Delegation(
                "it is scoped to another room".into(),
            ));
        }
        if !collab_types::caps::Verb::Comment.implies(
            collab_types::caps::role_caps(delegate.role).unwrap_or(collab_types::caps::Verb::Admin),
        ) {
            return Err(PublicRoomError::Delegation(format!(
                "it grants {:?}; a host may be delegated at most Commenter admission",
                delegate.role
            )));
        }
        Ok(workspace)
    }

    /// Validate `token` as a standing invitation of this room, minted by a
    /// device of the room's owner.
    ///
    /// # Errors
    ///
    /// [`PublicRoomError::InvalidInvite`] naming the first failed check.
    pub fn check_invite(
        &self,
        token: &str,
        now_unix_seconds: u64,
    ) -> Result<neighbour_wire::invite::PeerInviteToken, PublicRoomError> {
        let parsed = neighbour_wire::invite::validate_invite_token_at(token, now_unix_seconds)
            .map_err(PublicRoomError::InvalidInvite)?;
        if !parsed.standing {
            return Err(PublicRoomError::InvalidInvite(
                "it is a single-use invitation; only standing ones are redeemed by a host".into(),
            ));
        }
        if parsed.room_id != self.room {
            return Err(PublicRoomError::InvalidInvite(
                "it is for another room".into(),
            ));
        }
        if parsed
            .owner_device_cert
            .as_ref()
            .is_none_or(|cert| cert.user_public_key != self.owner)
        {
            return Err(PublicRoomError::InvalidInvite(
                "it was not minted by a device of the room's owner".into(),
            ));
        }
        Ok(parsed)
    }

    /// Take `incoming` — a record prepared where the owner's identity lives
    /// (`tilo-relay-admin community export` there) — as this relay's hosting
    /// of the room, merged into what it already holds (`existing`).
    ///
    /// The delegation must cover `relay_device` now, every invitation must
    /// carry its own id, and every live one must be this room's standing
    /// invitation from a device of its owner. The delegation is replaced;
    /// invitations are merged by id, and a revocation on either side stays:
    /// no record can bring a revoked invitation back. Returns the room's
    /// workspace id and the merged record.
    ///
    /// # Errors
    ///
    /// The first failed check, or a room already hosted here for another
    /// owner.
    pub fn adopt(
        existing: Option<Self>,
        incoming: Self,
        relay_device: [u8; 32],
        now: SystemTime,
    ) -> Result<(u64, Self), PublicRoomError> {
        let workspace = Self::check_delegation(
            &incoming.room,
            incoming.owner,
            &incoming.delegation,
            relay_device,
            now,
        )?;
        let now_unix = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        for invite in &incoming.invites {
            if invite.id != hex::encode(neighbour_wire::invite::invite_id(&invite.token)) {
                return Err(PublicRoomError::InvalidInvite(format!(
                    "invitation {} carries another invitation's id",
                    invite.id
                )));
            }
            if !invite.revoked {
                incoming.check_invite(&invite.token, now_unix)?;
            }
        }
        let mut merged = match existing {
            Some(existing)
                if existing.owner != incoming.owner || existing.room != incoming.room =>
            {
                return Err(PublicRoomError::Delegation(format!(
                    "room {} is already hosted here for owner {}",
                    existing.room,
                    hex::encode(existing.owner)
                )));
            }
            Some(existing) => existing,
            None => Self {
                room: incoming.room.clone(),
                owner: incoming.owner,
                delegation: Vec::new(),
                invites: Vec::new(),
            },
        };
        merged.delegation = incoming.delegation;
        for invite in incoming.invites {
            match merged.invites.iter_mut().find(|held| held.id == invite.id) {
                Some(held) => held.revoked |= invite.revoked,
                None => merged.invites.push(invite),
            }
        }
        Ok((workspace, merged))
    }

    /// Everything [`Envelope::Redeem`](crate::transport::Envelope::Redeem)
    /// needs to admit `member_device` of `member_user`: the registered,
    /// unrevoked invitation `invite_id` checks out, the delegation still
    /// covers this relay, and the resulting admission is signed.
    ///
    /// # Errors
    ///
    /// The first refusal.
    pub fn admit(
        &self,
        invite_id: [u8; 32],
        member_user: [u8; 32],
        member_device: [u8; 32],
        relay_sk: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> Result<HostAdmissionV1, PublicRoomError> {
        let id = hex::encode(invite_id);
        let invite = self
            .invites
            .iter()
            .find(|invite| invite.id == id)
            .ok_or(PublicRoomError::UnknownInvite)?;
        if invite.revoked {
            return Err(PublicRoomError::Revoked);
        }
        let now_unix = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let parsed = self.check_invite(&invite.token, now_unix)?;
        let workspace = Self::check_delegation(
            &self.room,
            self.owner,
            &self.delegation,
            relay_sk.verifying_key().to_bytes(),
            now,
        )?;
        let role = match parsed.role {
            neighbour_wire::invite::InviteRole::Viewer => Role::Viewer,
            neighbour_wire::invite::InviteRole::Commenter => Role::Commenter,
            neighbour_wire::invite::InviteRole::Editor => {
                return Err(PublicRoomError::InvalidInvite(
                    "a standing invitation never grants Editor".into(),
                ));
            }
        };
        Ok(HostAdmissionV1::sign(
            HostAdmissionSpec {
                room: self.room.clone(),
                workspace,
                member_user: Ed25519PublicKey::from_bytes(member_user),
                member_device: Ed25519PublicKey::from_bytes(member_device),
                role,
                invite: invite_id,
                admitted_at_unix_seconds: now_unix,
                delegation: self.delegation.clone(),
            },
            relay_sk,
        )?)
    }
}

/// Direct rooms with the owners of the public rooms this relay hosts.
///
/// Private feedback goes to the reporter's direct room with the room owner
/// (`tilo_reference::DirectRoom`). Its id is derived from the two people, so
/// neither end invites the other; this relay binds such a room the first time
/// either of them subscribes, admitting exactly those two people. Nobody else
/// can name a direct room with an owner that includes them, because the id
/// is a digest of both.
#[derive(Clone, Debug, Default)]
pub struct DirectRoomHosting {
    /// Where bound direct rooms keep their state.
    pub state_root: PathBuf,
    /// User root keys of the hosted public rooms' owners.
    pub owners: Vec<[u8; 32]>,
}

fn person(key: &[u8; 32]) -> String {
    collab_types::caps::Principal::person_from_key(Ed25519PublicKey::from_bytes(*key))
        .id()
        .to_string()
}

impl DirectRoomHosting {
    /// Collect the owners of every bound workspace that hosts a public room.
    ///
    /// # Errors
    ///
    /// A hosting record that exists but cannot be read.
    pub fn scan(state_root: &Path, workspace_ids: &[u64]) -> Result<Self, PublicRoomError> {
        let mut owners = Vec::new();
        for id in workspace_ids {
            let dir = state_root.join("workspaces").join(id.to_string());
            if let Some(hosting) = PublicRoomHosting::load(&dir)?
                && !owners.contains(&hosting.owner)
            {
                owners.push(hosting.owner);
            }
        }
        Ok(Self {
            state_root: state_root.to_path_buf(),
            owners,
        })
    }

    /// The two people of the direct room `workspace_id`, when it is the
    /// direct room between `user` and a hosted room's owner.
    #[must_use]
    pub fn members(&self, workspace_id: u64, user: [u8; 32]) -> Option<[[u8; 32]; 2]> {
        self.owners.iter().find_map(|owner| {
            let room = tilo_reference::DirectRoom::between(&person(&user), &person(owner))?;
            (crate::room_workspace_id(&room.room_id()).ok()? == workspace_id)
                .then_some([user, *owner])
        })
    }
}

/// The retained `Delta` that carries `admission` into the room, authored by
/// this relay's device, with the delta bytes it carries.
///
/// # Errors
///
/// An encoding failure.
pub fn admission_envelope(
    workspace_id: u64,
    relay_device: [u8; 32],
    admission: HostAdmissionV1,
) -> Result<(crate::transport::Envelope, Vec<u8>), postcard::Error> {
    let message = neighbour_wire::NeighbourMsg::RoomAdmission {
        room: admission.room.clone(),
        admission: Box::new(admission),
    };
    let body = postcard::to_allocvec(&message)?;
    let channel =
        neighbour_wire::retention(&message, false).map_or([0; 32], |(channel, _)| channel);
    let payload = crate::coedit::CoeditPayload::Checkpoint {
        channel,
        entry: *blake3::hash(&body).as_bytes(),
        coverage: crate::coedit::Coverage::Supersedes,
        body,
    }
    .encode()?;
    let envelope = crate::transport::Envelope::Delta {
        workspace_id,
        author_device_public_key: relay_device,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: payload.clone(),
        signature: [0; 64],
    };
    Ok((envelope, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    struct Team {
        root: ed25519_dalek::SigningKey,
        device: iroh_base::SecretKey,
        cert: DeviceCert,
    }

    fn team() -> Team {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0x71; 32]);
        let device = iroh_base::SecretKey::from_bytes(&[0x72; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Team laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        Team { root, device, cert }
    }

    fn standing(team: &Team, room: &str, role: neighbour_wire::invite::InviteRole) -> String {
        let mut draft = neighbour_wire::invite::InviteDraft::new(
            crate::room_workspace_id(room).unwrap(),
            room,
            iroh_base::EndpointAddr::new(team.device.public()),
            role,
            u64::MAX,
        );
        draft.owner_device_cert = Some(team.cert.clone());
        draft.standing = true;
        neighbour_wire::invite::mint_invite(&team.device, draft).unwrap()
    }

    fn hosting(team: &Team, room: &str, relay: &ed25519_dalek::SigningKey) -> PublicRoomHosting {
        let delegation = collab_types::mint_root(
            collab_types::InviteSpec {
                subject: Ed25519PublicKey::from_bytes(relay.verifying_key().to_bytes()),
                workspace: crate::room_workspace_id(room).unwrap(),
                role: Role::Commenter,
                caps: BTreeSet::from([
                    collab_types::caps::Verb::Read,
                    collab_types::caps::Verb::Comment,
                ]),
                sub_resource: None,
                not_after: None,
            },
            &team.root,
        )
        .unwrap();
        PublicRoomHosting {
            room: room.into(),
            owner: team.root.verifying_key().to_bytes(),
            delegation: vec![delegation],
            invites: Vec::new(),
        }
    }

    #[test]
    fn a_registered_standing_invite_admits_a_verifiable_commenter() {
        let team = team();
        let room = uuid::Uuid::new_v4().to_string();
        let relay = ed25519_dalek::SigningKey::from_bytes(&[0x73; 32]);
        let mut hosting = hosting(&team, &room, &relay);
        let token = standing(&team, &room, neighbour_wire::invite::InviteRole::Commenter);
        let id = neighbour_wire::invite::invite_id(&token);
        hosting.invites.push(HostedStandingInvite {
            id: hex::encode(id),
            token,
            revoked: false,
        });

        let admission = hosting
            .admit(id, [1; 32], [2; 32], &relay, SystemTime::now())
            .unwrap();
        assert_eq!(
            admission.verify(
                Ed25519PublicKey::from_bytes(hosting.owner),
                SystemTime::now()
            ),
            Ok(Role::Commenter)
        );

        assert!(matches!(
            hosting.admit([9; 32], [1; 32], [2; 32], &relay, SystemTime::now()),
            Err(PublicRoomError::UnknownInvite)
        ));
        hosting.invites[0].revoked = true;
        assert!(matches!(
            hosting.admit(id, [1; 32], [2; 32], &relay, SystemTime::now()),
            Err(PublicRoomError::Revoked)
        ));
    }

    #[test]
    fn an_exported_record_is_adopted_only_by_its_relay_and_never_unrevokes() {
        let team = team();
        let room = uuid::Uuid::new_v4().to_string();
        let relay = ed25519_dalek::SigningKey::from_bytes(&[0x76; 32]);
        let relay_device = relay.verifying_key().to_bytes();
        let first = standing(&team, &room, neighbour_wire::invite::InviteRole::Commenter);
        let second = standing(&team, &room, neighbour_wire::invite::InviteRole::Viewer);
        let entry = |token: &str, revoked| HostedStandingInvite {
            id: hex::encode(neighbour_wire::invite::invite_id(token)),
            token: token.to_string(),
            revoked,
        };
        let mut exported = hosting(&team, &room, &relay);
        exported.invites.push(entry(&first, true));
        let now = SystemTime::now();

        let other = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
        assert!(matches!(
            PublicRoomHosting::adopt(
                None,
                exported.clone(),
                other.verifying_key().to_bytes(),
                now
            ),
            Err(PublicRoomError::Delegation(_))
        ));

        let (workspace, held) =
            PublicRoomHosting::adopt(None, exported.clone(), relay_device, now).unwrap();
        assert_eq!(workspace, crate::room_workspace_id(&room).unwrap());

        // A later record naming the first invitation live again, plus a new
        // one, revokes nothing new and brings nothing back.
        let mut rotated = hosting(&team, &room, &relay);
        rotated.invites.push(entry(&first, false));
        rotated.invites.push(entry(&second, false));
        let (_, held) = PublicRoomHosting::adopt(Some(held), rotated, relay_device, now).unwrap();
        assert_eq!(held.invites.len(), 2);
        assert!(held.invites[0].revoked, "a revocation is never undone");
        assert!(!held.invites[1].revoked);

        let mut forged = hosting(&team, &room, &relay);
        forged.invites.push(HostedStandingInvite {
            id: hex::encode([0; 32]),
            ..entry(&second, false)
        });
        assert!(matches!(
            PublicRoomHosting::adopt(Some(held.clone()), forged, relay_device, now),
            Err(PublicRoomError::InvalidInvite(_))
        ));

        let stranger = Team {
            root: ed25519_dalek::SigningKey::from_bytes(&[0x78; 32]),
            ..self::team()
        };
        let foreign = hosting(&stranger, &room, &relay);
        assert!(matches!(
            PublicRoomHosting::adopt(Some(held), foreign, relay_device, now),
            Err(PublicRoomError::Delegation(_))
        ));
    }

    #[test]
    fn a_relay_without_the_delegation_admits_nobody() {
        let team = team();
        let room = uuid::Uuid::new_v4().to_string();
        let relay = ed25519_dalek::SigningKey::from_bytes(&[0x74; 32]);
        let other_relay = ed25519_dalek::SigningKey::from_bytes(&[0x75; 32]);
        let mut hosting = hosting(&team, &room, &relay);
        let token = standing(&team, &room, neighbour_wire::invite::InviteRole::Commenter);
        let id = neighbour_wire::invite::invite_id(&token);
        hosting.invites.push(HostedStandingInvite {
            id: hex::encode(id),
            token,
            revoked: false,
        });
        assert!(matches!(
            hosting.admit(id, [1; 32], [2; 32], &other_relay, SystemTime::now()),
            Err(PublicRoomError::Delegation(_))
        ));
    }

    #[test]
    fn only_the_owners_standing_invites_for_this_room_register() {
        let team = team();
        let room = uuid::Uuid::new_v4().to_string();
        let relay = ed25519_dalek::SigningKey::from_bytes(&[0x76; 32]);
        let hosting = hosting(&team, &room, &relay);
        let other_room = uuid::Uuid::new_v4().to_string();
        assert!(
            hosting
                .check_invite(
                    &standing(
                        &team,
                        &other_room,
                        neighbour_wire::invite::InviteRole::Commenter
                    ),
                    0
                )
                .is_err()
        );
        let mut single = neighbour_wire::invite::InviteDraft::new(
            1,
            room.clone(),
            iroh_base::EndpointAddr::new(team.device.public()),
            neighbour_wire::invite::InviteRole::Viewer,
            u64::MAX,
        );
        single.owner_device_cert = Some(team.cert.clone());
        let single = neighbour_wire::invite::mint_invite(&team.device, single).unwrap();
        assert!(hosting.check_invite(&single, 0).is_err());
        let stranger = iroh_base::SecretKey::from_bytes(&[0x77; 32]);
        let mut foreign = neighbour_wire::invite::InviteDraft::new(
            1,
            room.clone(),
            iroh_base::EndpointAddr::new(stranger.public()),
            neighbour_wire::invite::InviteRole::Commenter,
            u64::MAX,
        );
        foreign.standing = true;
        let foreign = neighbour_wire::invite::mint_invite(&stranger, foreign).unwrap();
        assert!(
            hosting.check_invite(&foreign, 0).is_err(),
            "an invitation not minted by the owner's certified device is refused"
        );
        assert!(
            hosting
                .check_invite(
                    &standing(&team, &room, neighbour_wire::invite::InviteRole::Commenter),
                    0
                )
                .is_ok()
        );
    }
}
