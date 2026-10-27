//! Redeeming an invitation with the host it names: the typed canonical
//! exchange. The caller supplies its real endpoint and custody.
use crate::{
    NativeWorkspaceOwner, PendingEnrollmentId, PendingEnrollmentStatus, VerifiedNativeEnrollment,
    VerifiedNativeEnrollmentIntent, WorkspaceId, enrollment::ns as now_ns,
};
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::join::{
    self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
    TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1,
};
use tilo_relay_session::workspace_join::WorkspaceJoinClient;
use tilo_user_identity::{DeviceCert, RevocationList};

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
/// Redeem `invitation` for `local`, the workspace slot the room will show
/// in where the host has slots. The signed Complete proof is durable before
/// it is sent, and a retry resends exactly that proof.
///
/// Returns closed response evidence only. The owner still rechecks the
/// target slot and crosses its single installation barrier
/// ([`NativeWorkspaceOwner::install`]) before any Joined result.
///
/// # Errors
/// The invitation does not verify for this device, the pinned host refuses
/// or answers out of scope, custody changes during the exchange, or the
/// Complete is not acknowledged (its proof is then retained for an exact
/// retry).
#[allow(
    clippy::too_many_lines,
    reason = "one ordered exchange: prepare, persist, send, verify and hand back closed evidence"
)]
pub async fn prepare_and_complete(
    client: WorkspaceJoinClient,
    identity: HostIdentity,
    identity_topic: u64,
    local: Option<WorkspaceId>,
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
    let retained = match local {
        Some(local) => owner.pending_enrollment_for_slot(local)?,
        None => owner.pending_enrollment_for_invitation(&invitation)?,
    };
    let (intent, id) = if let Some(pending) = retained {
        if pending.status() != PendingEnrollmentStatus::Active {
            return Err(
                "native enrollment was cancelled; retained proof requires reconciliation".into(),
            );
        }
        let intent = VerifiedNativeEnrollmentIntent::restore(pending.intent_bytes())?;
        if (intent.local_device() != &device
            && !crate::renewal::renews(&device, intent.local_device()))
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
            JoinTransportResponse::Error { message, .. } => return Err(refused(&message)),
            _ => return Err("pinned host inspection changed invitation scope".into()),
        }
        current(&identity, identity_topic, &device)?;
        let mut request_id = [0; 32];
        rand_core::Rng::fill_bytes(
            &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
            &mut request_id,
        );
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
            JoinTransportResponse::Error { message, .. } => return Err(refused(&message)),
            _ => return Err("pinned native host answered no enrollment challenge".into()),
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
        .request(host.endpoint_addr.clone(), complete)
        .await
        .map_err(|e| e.to_string())?;
    current(&identity, identity_topic, &device)?;
    active(&owner, &intent, &id)?;
    match response {
        // Admitted with the certificate the proof was made with, which
        // custody has renewed since: the room takes the renewal through the
        // membership exchange before the device takes the room in, so the
        // host holds the certificate this device presents from now on.
        JoinTransportResponse::Complete(bootstrap) if bootstrap.device_cert != device => {
            let grant = intent.verified_join().grant();
            let bootstrap = prove_membership(
                &client,
                &host.endpoint_addr,
                (grant.room.clone(), grant.workspace_id),
                &device,
                &identity,
            )
            .await?;
            current(&identity, identity_topic, &device)?;
            active(&owner, &intent, &id)?;
            VerifiedNativeEnrollment::finalize(intent, id, bootstrap)
        }
        JoinTransportResponse::Complete(bootstrap) => {
            VerifiedNativeEnrollment::finalize(intent, id, bootstrap)
        }
        // The host admitted this proof before, and the room has moved on
        // since, so it no longer replays its answer: this device is a
        // member, and takes the room's current state through the membership
        // exchange. Resending the proof would be refused forever, keeping
        // the slot from the room it joined. A host from before it said so
        // for a certificate that ran out since refuses that certificate
        // instead; custody has renewed it, and the exchange presents the
        // renewal.
        JoinTransportResponse::Error { code, message }
            if completed(code, &message)
                || (expired(code, &message) && intent.local_device() != &device) =>
        {
            let grant = intent.verified_join().grant();
            let bootstrap = prove_membership(
                &client,
                &host.endpoint_addr,
                (grant.room.clone(), grant.workspace_id),
                &device,
                &identity,
            )
            .await?;
            current(&identity, identity_topic, &device)?;
            active(&owner, &intent, &id)?;
            VerifiedNativeEnrollment::finalize(intent, id, bootstrap)
        }
        // The host holds neither the challenge this proof answers nor a
        // completion of its request: it never admitted it and never will.
        // Kept, the proof would be resent and refused forever, and no new
        // proof could be made for the room; the invitation is redeemed, if
        // at all, through a new challenge.
        JoinTransportResponse::Error { code, message } if lapsed(code, &message) => {
            owner.drop_lapsed_enrollment(intent.room(), &id)?;
            // The proof carried the certificate custody has renewed since:
            // the invitation is asked for again at once, with the renewal.
            if intent.local_device() != &device {
                return Box::pin(prepare_and_complete(
                    client,
                    identity,
                    identity_topic,
                    local,
                    invitation,
                    owner,
                ))
                .await;
            }
            Err(format!(
                "{}; the host never admitted this join, so it is asked for again",
                refused(&message)
            ))
        }
        // Generic transport/protocol errors are not proof that remote admission did not happen.
        JoinTransportResponse::Error { message, .. } => Err(format!(
            "{}; exact signed proof retained",
            refused(&message)
        )),
        _ => Err("native Complete was not acknowledged; exact signed proof retained".into()),
    }
}

/// Whether a host's refusal of a Complete says it never admitted the proof
/// and no longer can ([`join::JOIN_PROOF_LAPSED`]). A host from before that
/// code answers the same finding as a 403 naming the challenge it no longer
/// holds; its `complete` looks up a completed request before the challenge,
/// so that answer, too, means the proof was never admitted.
fn lapsed(code: u16, message: &str) -> bool {
    code == join::JOIN_PROOF_LAPSED
        || (code == 403 && message == "join challenge was not issued by this host")
}

/// Whether a host's refusal of a Complete says it admitted the proof before
/// and the room has moved on ([`join::JOIN_COMPLETED_REFRESH`]). A host from
/// before that code says the same in words, as a 403.
fn completed(code: u16, message: &str) -> bool {
    code == join::JOIN_COMPLETED_REFRESH
        || (code == 403
            && message
                == "join request ID already completed; refresh membership for the current workspace")
}

/// Whether a host refused a Complete for the certificate its proof carries
/// having run out. A host that admitted the proof before answers so where a
/// newer one answers [`join::JOIN_COMPLETED_REFRESH`]; one that never did
/// lets the proof's challenge lapse, and answers
/// [`join::JOIN_PROOF_LAPSED`] once it has.
fn expired(code: u16, message: &str) -> bool {
    code == 403 && message == tilo_user_identity::DeviceCertError::Expired.to_string()
}

/// The room's current state for a device that is a member: the membership
/// exchange for `room` (its id and workspace), challenged and proven with
/// the device's key and presenting `device`, the certificate it is a member
/// with or the renewal of it. The answer is checked by whoever takes it in
/// ([`VerifiedNativeEnrollment::finalize`],
/// [`NativeWorkspaceOwner::renew_membership`]).
async fn prove_membership(
    client: &WorkspaceJoinClient,
    host: &iroh::EndpointAddr,
    room: (String, u64),
    device: &DeviceCert,
    identity: &HostIdentity,
) -> Result<join::JoinBootstrap, String> {
    let (room, workspace_id) = room;
    let mut request_id = [0; 32];
    rand_core::Rng::fill_bytes(
        &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
        &mut request_id,
    );
    let challenge = match client
        .request(
            host.clone(),
            JoinTransportRequest::MembershipChallenge(join::MembershipChallengeRequest {
                room,
                workspace_id,
                device_cert: device.clone(),
                request_id,
            }),
        )
        .await
        .map_err(|e| e.to_string())?
    {
        JoinTransportResponse::MembershipChallenge(value) => value.challenge,
        JoinTransportResponse::Error { message, .. } => return Err(refused(&message)),
        _ => return Err("the host answered no membership challenge".into()),
    };
    let proof = join::sign_membership_proof(challenge, &identity.device_signing_key)?;
    match client
        .request(
            host.clone(),
            JoinTransportRequest::MembershipComplete(join::MembershipCompleteRequest { proof }),
        )
        .await
        .map_err(|e| e.to_string())?
    {
        JoinTransportResponse::MembershipComplete(bootstrap) => Ok(bootstrap),
        JoinTransportResponse::Error { message, .. } => Err(refused(&message)),
        _ => Err("the host answered no membership".into()),
    }
}

/// Prove this device's membership of `room`, a room it joined, with the
/// renewal of the certificate the room's record holds, when its custody
/// holds one, and take the governor's answer in
/// ([`NativeWorkspaceOwner::renew_membership`]). Answers whether it renewed.
///
/// # Errors
/// The room is not joined here, custody is unreadable or changes during
/// the exchange, or the host refuses.
pub async fn renew_membership(
    client: WorkspaceJoinClient,
    identity: HostIdentity,
    owner: NativeWorkspaceOwner,
    room: crate::RoomId,
) -> Result<bool, String> {
    let (topic, hint, held) = owner.with_current(room, |view| {
        Ok((
            view.workspace(),
            view.transport_hint()?.clone(),
            view.device().clone(),
        ))
    })?;
    let (renewed, _) = identity.current_native_material(topic)?;
    if !crate::renewal::renews(&renewed, &held) {
        return Ok(false);
    }
    let (host, _) =
        IrohWorkspaceHostHintV1::authenticate_transport_hint(&hint).map_err(|e| e.to_string())?;
    let bootstrap = prove_membership(
        &client,
        &host.endpoint_addr,
        (room.to_string(), topic),
        &renewed,
        &identity,
    )
    .await?;
    current(&identity, topic, &renewed)?;
    owner.renew_membership(room, &renewed, &bootstrap, SystemTime::now())?;
    Ok(true)
}

/// The host's refusal, in its own words: what the person is shown.
fn refused(message: &str) -> String {
    let message: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect();
    format!("the workspace's host refused: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_host_that_holds_neither_challenge_nor_completion_ends_a_retained_proof() {
        assert!(lapsed(join::JOIN_PROOF_LAPSED, "anything"));
        assert!(
            lapsed(403, "join challenge was not issued by this host"),
            "a host from before the code says the same in words"
        );
        assert!(!lapsed(403, "invitation was cancelled"));
        assert!(!lapsed(
            403,
            "join proof does not match authenticated transport endpoint"
        ));
        assert!(!lapsed(503, "join challenge was not issued by this host"));
    }

    /// A device that the host admitted, but whose answer never reached it
    /// (the laptop restarted mid-join), takes the room through the
    /// membership exchange once the room moved on, rather than resending
    /// its proof forever (the Lounge, 2026-10-20).
    #[test]
    fn a_host_that_admitted_the_proof_before_sends_the_device_to_its_membership() {
        assert!(completed(join::JOIN_COMPLETED_REFRESH, "anything"));
        assert!(
            completed(
                403,
                "join request ID already completed; refresh membership for the current workspace"
            ),
            "a host from before the code says the same in words"
        );
        assert!(!completed(403, "invitation was cancelled"));
        assert!(!lapsed(join::JOIN_COMPLETED_REFRESH, "anything"));
    }

    /// A host from before it answered a lapsed certificate with
    /// [`join::JOIN_COMPLETED_REFRESH`] refuses it in words; the retained
    /// proof then goes through the membership exchange with the renewal
    /// instead of being resent every retry (the Lounge, 2026-10-23).
    #[test]
    fn a_host_that_refuses_the_proofs_expired_certificate_is_told_apart() {
        assert!(expired(403, "device cert has expired"));
        assert!(!expired(503, "device cert has expired"));
        assert!(!expired(403, "device cert is not yet valid"));
        assert!(!lapsed(403, "device cert has expired"));
        assert!(!completed(403, "device cert has expired"));
    }
}
