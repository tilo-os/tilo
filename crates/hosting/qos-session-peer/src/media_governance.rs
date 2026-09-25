//! Candidate-only governed media preparation shared by trusted host owners.
//! This module does not authorize a caller, commit storage, or issue a receipt.
use ed25519_dalek::SigningKey;
use qos_action_broker::collaboration_admin::{
    CollaborationAdminError as Error, CollaborationAdminOperation as Operation,
    MediaDirectionsInput, MediaRejectionCode as Code,
};
use qos_peer_state::{
    StatePeer,
    room::media::{self, CallPhaseV1, DirectionsV1, MediaCallResultV1},
};
use rand::RngCore;
use web_time::SystemTime;

/// Reconcile and validate an already authorized owned candidate before its
/// real owner prepares durable publications. This is not caller authorization.
pub fn prepare_media_governance(
    candidate: &mut StatePeer,
    governor: &SigningKey,
    now: SystemTime,
) -> anyhow::Result<()> {
    candidate
        .reconcile_media_governance(governor, now)
        .map_err(anyhow::Error::msg)?;
    candidate
        .validate_media_governance_for_publication()
        .map_err(anyhow::Error::msg)
}

fn semantic(code: Code, message: impl Into<String>) -> Error {
    Error::Semantic {
        code,
        message: message.into(),
    }
}
fn directions(value: MediaDirectionsInput) -> DirectionsV1 {
    DirectionsV1 {
        publish: value.publish,
        subscribe: value.subscribe,
    }
}
fn policy_error(error: media::MediaPolicyError) -> Error {
    let code = match error {
        media::MediaPolicyError::Capacity => Code::Capacity,
        media::MediaPolicyError::Exhausted => Code::Exhausted,
        _ => Code::Binding,
    };
    semantic(code, error.to_string())
}
/// The caller supplies an owned candidate under its existing resource and
/// authorization boundary. On error the candidate is discarded. On success
/// its actual signed mutations still require the owner's publication/barrier.
/// No peer clone, caller check, invocation record or storage write occurs here.
pub fn prepare_media_transition(
    mut candidate: StatePeer,
    governor: &SigningKey,
    operation: &Operation,
    now: SystemTime,
) -> Result<(StatePeer, MediaCallResultV1), Error> {
    prepare_media_governance(&mut candidate, governor, now)
        .map_err(|e| Error::Rejected(e.to_string()))?;
    let current = media::parse(
        candidate
            .admitted_room()
            .ok_or_else(|| Error::Rejected("missing room".into()))?,
    )
    .map_err(policy_error)?;
    let mut policy = current.clone().unwrap_or(media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(candidate.room())
            .map_err(|e| Error::Rejected(e.to_string()))?
            .as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(
            candidate.admitted_room().unwrap(),
            &candidate.revocations(),
        )
        .map_err(policy_error)?,
        calls: Default::default(),
    });
    let id = match operation {
        Operation::MediaCreate(input) => {
            if input.expected_authorization_revision
                != current.as_ref().map(|p| p.authorization_revision)
            {
                return Err(semantic(Code::Stale, "authorization revision changed"));
            }
            let binding = candidate
                .resolve_media_binding(&input.app_id, &input.record_id, &input.channel_id)
                .map_err(|e| semantic(Code::Binding, e))?;
            if policy.calls.values().any(|call| call.binding == binding) {
                return Err(semantic(
                    Code::AlreadyExists,
                    "call binding already exists, including ended calls",
                ));
            }
            if policy.calls.len() >= media::MAX_CALLS {
                return Err(semantic(
                    Code::Capacity,
                    "governed call definition capacity reached",
                ));
            }
            let mut id = [0; 32];
            let mut allocated = false;
            for _ in 0..8 {
                rand::rngs::OsRng
                    .try_fill_bytes(&mut id)
                    .map_err(|e| Error::Rejected(e.to_string()))?;
                if id != [0; 32] && !policy.calls.contains_key(&id) {
                    allocated = true;
                    break;
                }
            }
            if !allocated {
                return Err(Error::Rejected(
                    "unable to allocate unique call identifier".into(),
                ));
            }
            policy.calls.insert(
                id,
                media::CallV1 {
                    binding,
                    epoch: 1,
                    phase: CallPhaseV1::Active,
                    allowed: directions(input.allowed),
                    denied: Default::default(),
                },
            );
            id
        }
        _ => {
            let (id, revision, epoch) = match operation {
                Operation::MediaEnd(v) | Operation::MediaRestart(v) => (
                    &v.call_id,
                    v.expected_authorization_revision,
                    v.expected_epoch,
                ),
                Operation::MediaSetDirections(v) => (
                    &v.call_id,
                    v.expected_authorization_revision,
                    v.expected_epoch,
                ),
                Operation::MediaSetParticipantDenial(v) => (
                    &v.call_id,
                    v.expected_authorization_revision,
                    v.expected_epoch,
                ),
                _ => return Err(Error::Rejected("not a governed media operation".into())),
            };
            if current.is_none() || revision != policy.authorization_revision {
                return Err(semantic(Code::Stale, "authorization revision changed"));
            }
            let id = media::parse_call_id(id).map_err(policy_error)?;
            let total_denials = policy
                .calls
                .values()
                .map(|call| call.denied.len())
                .sum::<usize>();
            let call = policy
                .calls
                .get_mut(&id)
                .ok_or_else(|| semantic(Code::Binding, "unknown call"))?;
            if epoch != call.epoch {
                return Err(semantic(Code::Stale, "call epoch changed"));
            }
            let mut changed = true;
            match operation {
                Operation::MediaEnd(_) => {
                    if call.phase != CallPhaseV1::Active {
                        return Err(semantic(Code::InvalidState, "call already ended"));
                    }
                    call.phase = CallPhaseV1::Ended;
                }
                Operation::MediaRestart(_) => {
                    if call.phase != CallPhaseV1::Ended {
                        return Err(semantic(Code::InvalidState, "call is active"));
                    }
                    let binding = candidate
                        .resolve_media_binding(
                            &call.binding.app_id,
                            &call.binding.record_id,
                            &call.binding.channel_id,
                        )
                        .map_err(|e| semantic(Code::Binding, e))?;
                    if binding != call.binding {
                        return Err(semantic(Code::Binding, "original call binding changed"));
                    }
                    call.phase = CallPhaseV1::Active;
                }
                Operation::MediaSetDirections(v) => {
                    let allowed = directions(v.allowed);
                    changed = call.allowed != allowed;
                    call.allowed = allowed;
                }
                Operation::MediaSetParticipantDenial(v) => {
                    let root = media::parse_call_id(&v.person_root).map_err(policy_error)?;
                    let denied = directions(v.denied);
                    if !denied.publish && !denied.subscribe {
                        changed = call.denied.remove(&root).is_some();
                    } else {
                        changed = call.denied.get(&root) != Some(&denied);
                        if !call.denied.contains_key(&root)
                            && (call.denied.len() >= media::MAX_DENIED_PER_CALL
                                || total_denials >= media::MAX_DENIED_TOTAL)
                        {
                            return Err(semantic(
                                Code::Capacity,
                                "participant denial capacity reached",
                            ));
                        }
                        call.denied.insert(root, denied);
                    }
                }
                _ => unreachable!(),
            }
            if changed {
                call.epoch = media::next_counter(call.epoch).map_err(policy_error)?;
            }
            id
        }
    };
    // Equal moderation still records its invocation, with unchanged room bytes.
    if current.as_ref() != Some(&policy) {
        candidate
            .author_media_policy(&policy, governor, now)
            .map_err(Error::Rejected)?;
    }
    let call = &policy.calls[&id];
    let result = MediaCallResultV1 {
        version: 1,
        call_id: id,
        binding: call.binding.clone(),
        epoch: call.epoch,
        authorization_revision: policy.authorization_revision,
        phase: call.phase,
    };
    Ok((candidate, result))
}

#[cfg(test)]
mod tests;
