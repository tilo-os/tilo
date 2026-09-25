//! Media policy cached only beside the peer in a committed publication.
//! Public room DTOs are data; this private projection never establishes authority
//! independently of its borrowed committed peer and current permissions.
use crate::media_authority::{
    CallPolicy, GrantRequest, MediaCallPolicy, MediaDirections, MediaError,
};
use qos_peer_state::{
    StatePeer,
    room::media::{self, CallPhaseV1, RoomMediaV1},
};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

pub struct RoomMediaPolicy(State);
enum State {
    Disabled,
    Invalid,
    Ready {
        room: String,
        authority: DeviceCert,
        policy: RoomMediaV1,
    },
}
impl RoomMediaPolicy {
    /// Called only when constructing the same committed view that owns `peer`,
    /// after its real durability barrier. Missing bindings or unpromoted local
    /// revocations suspend media without denying unrelated committed data.
    pub fn from_committed(peer: &StatePeer) -> Self {
        #[cfg(test)]
        PROJECTIONS.with(|count| count.set(count.get() + 1));
        Self(match peer.validated_media_policy() {
            Ok(None) => State::Disabled,
            Err(_) => State::Invalid,
            Ok(Some(policy)) => State::Ready {
                room: peer.room().to_owned(),
                authority: peer.authority().clone(),
                policy,
            },
        })
    }
}
impl MediaCallPolicy for RoomMediaPolicy {
    fn current(
        &self,
        peer: &StatePeer,
        request: &GrantRequest,
        now: SystemTime,
    ) -> Result<CallPolicy, MediaError> {
        let State::Ready {
            room,
            authority,
            policy,
        } = &self.0
        else {
            return Err(MediaError::Unauthorized);
        };
        if request.room != *room || peer.room() != room || peer.authority() != authority {
            return Err(MediaError::Unauthorized);
        }
        let id = media::parse_call_id(&request.call_id).map_err(|_| MediaError::Unauthorized)?;
        let call = policy.calls.get(&id).ok_or(MediaError::Unauthorized)?;
        if call.phase != CallPhaseV1::Active {
            return Err(MediaError::Unauthorized);
        }
        // These checks use the current time and exact governed certificate on
        // every operation. No room/Chat parsing or projection cloning occurs in
        // the relay's periodic check. The caller retains the publication guard.
        peer.permissions(authority.device_public_key, now)
            .map_err(|_| MediaError::Unauthorized)?;
        let (_, _, person) = peer
            .permissions(request.device, now)
            .map_err(|_| MediaError::Unauthorized)?;
        let mut root = [0; 32];
        hex::decode_to_slice(&person, &mut root).map_err(|_| MediaError::Unauthorized)?;
        let denied = call.denied.get(&root);
        Ok(CallPolicy {
            epoch: call.epoch,
            membership_generation: policy.authorization_revision,
            directions: MediaDirections {
                publish: call.allowed.publish && !denied.is_some_and(|value| value.publish),
                subscribe: call.allowed.subscribe && !denied.is_some_and(|value| value.subscribe),
            },
        })
    }
}

#[cfg(test)]
thread_local! { static PROJECTIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::SessionOwner;
    use std::collections::BTreeMap;
    use ui::automerge::{
        AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable,
    };

    pub(crate) const CALL: [u8; 32] = [7; 32];
    pub(crate) const APP: &str = "so.tilo.chat";
    pub(crate) const CHANNEL: &str = "governed-voice";

    pub(crate) fn commit_room(owner: &mut SessionOwner, mut room: AutoCommit) {
        let mut candidate = owner.peer().clone();
        candidate
            .install_seed(
                qos_peer_state::SeedState {
                    room_changes: room
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(candidate, owner.directory().to_vec())
            .unwrap();
    }
    pub(crate) fn edit_policy(owner: &mut SessionOwner, edit: impl FnOnce(&mut RoomMediaV1)) {
        let mut room = owner.peer().admitted_room().unwrap().clone();
        let mut policy = media::parse(&room).unwrap().unwrap();
        edit(&mut policy);
        room.put(
            ROOT,
            media::FIELD,
            ScalarValue::Bytes(media::encode(&policy).unwrap()),
        )
        .unwrap();
        commit_room(owner, room);
    }
    /// Real signed document admission followed by native durable room publication.
    /// The DTO below alone never reaches an authorization consumer.
    pub(crate) fn activate(owner: &mut SessionOwner) {
        activate_with_record(owner, "");
    }
    pub(crate) fn activate_with_record(owner: &mut SessionOwner, record: &str) {
        let mut peer = owner.peer().clone();
        let mut chat = AutoCommit::load(&peer.document(APP, "").unwrap()).unwrap();
        let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
        let object = chat.put_object(&channels, CHANNEL, ObjType::Map).unwrap();
        chat.put(&object, "kind", "voice").unwrap();
        chat.put(&object, "workspace", "native").unwrap();
        peer.publish_document(
            APP,
            "",
            chat.save(),
            &owner.identity.native,
            SystemTime::now(),
        )
        .unwrap();
        owner
            .commit_mutation_candidate(peer, owner.directory().to_vec())
            .unwrap();
        let mut room = owner.peer().admitted_room().unwrap().clone();
        let policy = RoomMediaV1 {
            version: 1,
            room: *uuid::Uuid::parse_str(owner.peer().room())
                .unwrap()
                .as_bytes(),
            authorization_revision: 1,
            authorization_basis: media::security_basis(&room, &[]).unwrap(),
            calls: BTreeMap::from([(
                CALL,
                media::CallV1 {
                    binding: media::ChatCallBindingV1 {
                        app_id: APP.into(),
                        record_id: record.into(),
                        channel_id: CHANNEL.into(),
                        channel_object: object.to_string(),
                        chat_workspace_id: "native".into(),
                    },
                    epoch: 1,
                    phase: CallPhaseV1::Active,
                    allowed: media::DirectionsV1 {
                        publish: true,
                        subscribe: true,
                    },
                    denied: BTreeMap::new(),
                },
            )]),
        };
        room.put(
            ROOT,
            media::FIELD,
            ScalarValue::Bytes(media::encode(&policy).unwrap()),
        )
        .unwrap();
        commit_room(owner, room);
    }

    pub(crate) fn projection_count() -> u64 {
        PROJECTIONS.with(std::cell::Cell::get)
    }
    pub(crate) fn member(
        owner: &mut SessionOwner,
        role: qos_peer_state::join::MemberRole,
        expires_at_ns: Option<u64>,
    ) -> (
        ed25519_dalek::SigningKey,
        ed25519_dalek::SigningKey,
        DeviceCert,
    ) {
        use crate::collaboration::{CollaborationService, IssueInvitationRequest};
        use qos_peer_state::join::{JoinChallengeRequest, JoinCompleteRequest, sign_join_proof};
        use tilo_user_identity::{DeviceCapabilities, sign_device_cert};
        let now: u64 = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "media guest".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now - 1,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let service = CollaborationService::new(vec![]);
        let administrator = owner.peer().authority().device_public_key;
        let invitation = service
            .issue(
                owner,
                administrator,
                IssueInvitationRequest {
                    request_id: [81; 32],
                    role,
                    recipient_user_public_key: Some(cert.user_public_key),
                    invitee_name: None,
                    expires_at_ns: now + 120_000_000_000,
                },
                now,
            )
            .unwrap();
        let challenge = service
            .challenge(
                owner,
                cert.device_public_key,
                JoinChallengeRequest {
                    signed_invitation: invitation.signed_invitation,
                    device_cert: cert.clone(),
                    request_id: [82; 32],
                },
                now + 1,
            )
            .unwrap()
            .challenge;
        service
            .complete(
                owner,
                cert.device_public_key,
                JoinCompleteRequest {
                    proof: sign_join_proof(challenge, &key).unwrap(),
                },
                now + 2,
            )
            .unwrap();
        (root, key, cert)
    }
}
