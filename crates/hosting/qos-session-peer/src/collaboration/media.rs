//! Existing administration receipt, exact current caller and one owner barrier.
use super::*;
use collab_types::{
    Role,
    caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability},
};
use qos_action_broker::collaboration_admin::{
    CollaborationAdminError as Error, CollaborationAdminOperation as Operation,
    CollaborationAdminRequest,
};
use qos_peer_state::room::media::MediaCallResultV1;

impl CollaborationService {
    pub(crate) fn media_from_action(
        &self,
        owner: &mut SessionOwner,
        request: &CollaborationAdminRequest,
        now_ns: u64,
    ) -> Result<MediaCallResultV1, Error> {
        if !owner.supports_media_governance() {
            return Err(Error::Rejected(
                "operational native V5 media writer required".into(),
            ));
        }
        // The room authority itself, or a browser device the local owner
        // enrolled on this host (same precedent as invitation administration).
        // Invited people, unenrolled same-root devices, expired or revoked
        // certificates remain refused; role/structural/Admin checks follow.
        let caller = &request.caller;
        let enrolled = owner_enrolled_device(owner, caller, now_ns)
            && !owner
                .peer()
                .revocations()
                .iter()
                .any(|entry| entry.revoked_device_public_key == caller.device_public_key);
        if request.room != owner.peer().room()
            || (*caller != *owner.peer().authority() && !enrolled)
        {
            return Err(Error::Rejected(
                "media administration requires the exact native owner certificate".into(),
            ));
        }
        let now = system_time(now_ns);
        let (role, caps, person) = owner
            .peer()
            .permissions(request.caller.device_public_key, now)
            .map_err(Error::Rejected)?;
        let structural = qos_peer_state::room::governance_of(owner.peer().admitted_room()).0;
        let actor = Actor::person(Some(role), &person).in_room(request.room.clone());
        if role != Role::Owner
            || structural.as_deref() != Some(&hex::encode(request.caller.user_public_key))
            || decide_capability(
                &actor,
                &ResourceRef::Room(request.room.clone()),
                Verb::Admin,
                &caps,
                Some(now_ns / 1_000_000_000),
            ) != CapDecision::Allow
        {
            return Err(Error::Rejected(
                "media administration requires structural owner room Admin".into(),
            ));
        }
        let (kind, args) = match &request.operation {
            Operation::MediaCreate(v) => ("create", serde_json::to_vec(v)),
            Operation::MediaEnd(v) => ("end", serde_json::to_vec(v)),
            Operation::MediaRestart(v) => ("restart", serde_json::to_vec(v)),
            Operation::MediaSetDirections(v) => ("directions", serde_json::to_vec(v)),
            Operation::MediaSetParticipantDenial(v) => ("denial", serde_json::to_vec(v)),
            _ => return Err(Error::Rejected("not a media administration action".into())),
        };
        let args = args.map_err(|e| Error::Rejected(e.to_string()))?;
        let mut id = blake3::Hasher::new();
        id.update(b"tilo/workspace-collaboration-action/v1\0");
        id.update(request.invocation.0.as_bytes());
        id.update(request.room.as_bytes());
        let request_id = *id.finalize().as_bytes();
        let material = postcard::to_allocvec(&(
            kind,
            &request.room,
            request.caller.device_public_key,
            request.caller.user_public_key,
            args,
        ))
        .map_err(|e| Error::Rejected(e.to_string()))?;
        let mut digest = blake3::Hasher::new();
        digest.update(b"tilo/workspace-media-administration/v1\0");
        digest.update(&material);
        let request_digest = *digest.finalize().as_bytes();
        if let Some(previous) = owner
            .collaboration_state()
            .administration
            .iter()
            .find(|record| record.request_id == request_id)
        {
            if previous.request_digest != request_digest {
                return Err(Error::Rejected(
                    "administration invocation reused with different request".into(),
                ));
            }
            return previous.media_result.clone().ok_or_else(|| {
                Error::Rejected("invocation belongs to nonmedia administration".into())
            });
        }
        let (candidate, result) = owner.prepare_media_transition(&request.operation, now)?;
        let mut state = owner.collaboration_state().clone();
        let generation = owner
            .committed_generation()
            .checked_add(1)
            .ok_or_else(|| Error::Rejected("owner generation exhausted".into()))?;
        state.administration.push(AdministrationRecord {
            request_id,
            request_digest,
            generation,
            media_result: Some(result.clone()),
        });
        owner
            .commit_collaboration_candidate(candidate, state)
            .map_err(|e| Error::Uncertain(e.to_string()))?;
        Ok(result)
    }
}

#[cfg(all(test, feature = "media"))]
mod tests;
