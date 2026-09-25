//! Typed canonical exchange. The actor supplies its real endpoint and custody.
use super::*;
use crate::{
    network::actor::HostIdentity,
    shell::{
        native_workspace_owner::{NativeWorkspaceOwner, PendingEnrollmentStatus},
        state::WorkspaceId,
    },
};
use qos_peer_state::join::{JoinChallengeRequest, JoinTransportResponse};
use rand_core::RngCore;
use tilo_relay::workspace_join::WorkspaceJoinClient;

fn current(identity: &HostIdentity, topic: u64, expected: &DeviceCert) -> Result<(), String> {
    let (certificate, _) = identity.current_native_material(topic)?;
    if &certificate != expected {
        return Err("native enrollment custody changed during exchange".into());
    }
    Ok(())
}
fn active(
    owner: &NativeWorkspaceOwner,
    intent: &VerifiedNativeEnrollmentIntent,
    id: &PendingEnrollmentId,
) -> Result<(), String> {
    let pending = owner
        .pending_enrollment(intent.room())?
        .ok_or("native enrollment proof no longer retained")?;
    if pending.id() != id
        || pending.status() != PendingEnrollmentStatus::Active
        || pending.intent_bytes() != intent.durable_bytes()?
    {
        return Err("native enrollment attempt cancelled or replaced".into());
    }
    Ok(())
}
/// Returns closed response evidence only. The owner still rechecks the target
/// slot and crosses its single installation barrier before any Joined result.
pub(crate) async fn prepare_and_complete(
    client: WorkspaceJoinClient,
    identity: HostIdentity,
    identity_topic: u64,
    local: WorkspaceId,
    invitation: String,
    owner: NativeWorkspaceOwner,
) -> Result<VerifiedNativeEnrollment, String> {
    if invitation.len() > 384 * 1024 {
        return Err("native invitation exceeds bound".into());
    }
    let (device, entries) = identity.current_native_material(identity_topic)?;
    let mut revocations = RevocationList::new();
    for entry in entries {
        revocations.insert(entry).map_err(|e| e.to_string())?;
    }
    let (intent, id) = if let Some(pending) = owner.pending_enrollment_for_slot(local)? {
        if pending.status() != PendingEnrollmentStatus::Active {
            return Err(
                "native enrollment was cancelled; retained proof requires reconciliation".into(),
            );
        }
        let intent = VerifiedNativeEnrollmentIntent::restore(pending.intent_bytes())?;
        if intent.local_device() != &device
            || join::encode_invitation_text(intent.verified_join().signed_invitation())
                != invitation
        {
            return Err(
                "native retry differs from persisted invitation or selected custody".into(),
            );
        }
        // Reuse the recorded proof even when its original challenge has expired.
        // The issuing host alone resolves durable Complete idempotency/current membership.
        let id = owner.prepare_enrollment_into(&intent, local, SystemTime::now())?;
        if &id != pending.id() {
            return Err("native retry changed pending generation".into());
        }
        (intent, id)
    } else {
        let join = join::decode_and_verify_invitation_text(
            &invitation,
            Some(device.user_public_key),
            &revocations,
            now_ns(SystemTime::now())?,
        )?;
        if join::encode_invitation_text(join.signed_invitation()) != invitation {
            return Err("noncanonical native invitation text".into());
        }
        let mut hints = join
            .grant()
            .transport
            .iter()
            .filter(|hint| hint.kind == TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1);
        let hint = hints
            .next()
            .cloned()
            .ok_or("invitation has no authenticated native Iroh route")?;
        if hints.next().is_some() {
            return Err("invitation has ambiguous native Iroh routes".into());
        }
        let (host, _) = IrohWorkspaceHostHintV1::authenticate_transport_hint(&hint)
            .map_err(|e| e.to_string())?;
        host.validate_invitation(join.grant())
            .map_err(|e| e.to_string())?;
        match client
            .request(
                host.endpoint_addr.clone(),
                JoinTransportRequest::Inspect {
                    signed_invitation: join.signed_invitation().to_vec(),
                },
            )
            .await
            .map_err(|e| e.to_string())?
        {
            JoinTransportResponse::Inspection(value)
                if value.invite_id == join.invite_id() && value.grant == *join.grant() => {}
            _ => return Err("pinned host inspection changed invitation scope or refused".into()),
        }
        current(&identity, identity_topic, &device)?;
        let mut request_id = [0; 32];
        rand_core::OsRng.fill_bytes(&mut request_id);
        let challenge = match client
            .request(
                host.endpoint_addr.clone(),
                JoinTransportRequest::Challenge(JoinChallengeRequest {
                    signed_invitation: join.signed_invitation().to_vec(),
                    device_cert: device.clone(),
                    request_id,
                }),
            )
            .await
            .map_err(|e| e.to_string())?
        {
            JoinTransportResponse::Challenge(value) => value.challenge,
            _ => return Err("pinned native host refused enrollment challenge".into()),
        };
        current(&identity, identity_topic, &device)?;
        let prepared = SystemTime::now();
        let expected = join::JoinChallenge::new(
            &join,
            &device,
            request_id,
            challenge.nonce,
            challenge.expires_at_ns,
            now_ns(prepared)?,
        )?;
        if challenge != expected {
            return Err("native host challenge changed original identity or scope".into());
        }
        let complete = JoinCompleteRequest {
            proof: join::sign_join_proof(challenge, &identity.device_signing_key)?,
        };
        let intent = VerifiedNativeEnrollmentIntent::from_proof(
            join,
            complete,
            device.clone(),
            hint,
            prepared,
        )?;
        let id = owner.prepare_enrollment_into(&intent, local, prepared)?;
        (intent, id)
    };
    current(&identity, identity_topic, &device)?;
    active(&owner, &intent, &id)?;
    let (host, _) = IrohWorkspaceHostHintV1::authenticate_transport_hint(intent.transport_hint())
        .map_err(|e| e.to_string())?;
    let complete: JoinTransportRequest =
        postcard::from_bytes(intent.complete_bytes()).map_err(|e| e.to_string())?;
    // Re-encoding is verified to be byte-identical before the typed client sends it.
    if postcard::to_stdvec(&complete).map_err(|e| e.to_string())? != intent.complete_bytes() {
        return Err("retained Complete changed encoding".into());
    }
    let response = client
        .request(host.endpoint_addr, complete)
        .await
        .map_err(|e| e.to_string())?;
    current(&identity, identity_topic, &device)?;
    active(&owner, &intent, &id)?;
    match response {
        JoinTransportResponse::Complete(bootstrap) => {
            VerifiedNativeEnrollment::finalize(intent, id, bootstrap)
        }
        // Generic transport/protocol errors are not proof that remote admission did not happen.
        _ => Err("native Complete was not acknowledged; exact signed proof retained".into()),
    }
}

#[cfg(test)]
#[path = "native_exchange_tests.rs"]
mod tests;
