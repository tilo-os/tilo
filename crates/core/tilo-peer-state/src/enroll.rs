//! Redeeming an invitation with the host it names: the typed canonical
//! exchange. The caller supplies its real endpoint and ownership.
use crate::enrollment::ns as now_ns;
use crate::join::WorkspaceId;
use crate::join::native::InvitationLink;
use crate::join::{
    self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
};
use crate::join_transport::{WorkspaceJoinClient, invitation_host};
use crate::membership::refused;
use crate::owner::{
    DeviceWorkspaces, PendingEnrollmentId, PendingEnrollmentStatus, VerifiedNativeEnrollment,
    VerifiedNativeEnrollmentIntent,
};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_user_identity::{DeviceCert, RevocationList};

fn current(
    identity: &DeviceIdentity,
    topic: WorkspaceId,
    expected: &DeviceCert,
) -> Result<(), String> {
    let (certificate, _) = identity.current_material(topic)?;
    if &certificate != expected {
        return Err("native enrollment ownership changed during exchange".into());
    }
    Ok(())
}
fn active(
    owner: &DeviceWorkspaces,
    intent: &VerifiedNativeEnrollmentIntent,
    id: &PendingEnrollmentId,
) -> Result<(), String> {
    let pending = owner
        .pending_enrollment(intent.workspace())?
        .ok_or("native enrollment proof no longer retained")?;
    if pending.id() != id
        || pending.status() != PendingEnrollmentStatus::Active
        || pending.intent_bytes() != intent.durable_bytes()?
    {
        return Err("native enrollment attempt cancelled or replaced".into());
    }
    Ok(())
}
/// Redeem `invitation` into `local`, the workspace on the host's shell made for the join, where
/// the host has a shell. The signed Complete proof is durable before
/// it is sent, and a retry resends exactly that proof.
///
/// Returns closed response evidence only. The owner still rechecks the
/// workspace and crosses its single installation barrier
/// ([`DeviceWorkspaces::install`]) before any Joined result.
#[allow(
    clippy::too_many_lines,
    reason = "one ordered exchange: prepare, persist, send, verify and hand back closed evidence"
)]
pub async fn prepare_and_complete(
    client: WorkspaceJoinClient,
    identity: DeviceIdentity,
    identity_topic: WorkspaceId,
    local: Option<WorkspaceId>,
    invitation: String,
    owner: DeviceWorkspaces,
) -> Result<VerifiedNativeEnrollment, String> {
    if invitation.len() > 384 * 1024 {
        return Err("native invitation exceeds bound".into());
    }
    let (device, entries) = identity.current_material(identity_topic)?;
    let mut revocations = RevocationList::new();
    for entry in entries {
        revocations.insert(entry).map_err(|e| e.to_string())?;
    }
    let locator = InvitationLink::parse(&invitation);
    if locator
        .as_ref()
        .is_some_and(|link| link.reference.workspace != identity_topic)
    {
        return Err("native invitation names a different workspace".into());
    }
    let retained = match local.or_else(|| locator.as_ref().map(|link| link.reference.workspace)) {
        Some(local) => owner.pending_enrollment(local)?,
        None => owner.pending_enrollment_for_invitation(&invitation)?,
    };
    let (intent, id) = if let Some(pending) = retained {
        if pending.status() != PendingEnrollmentStatus::Active {
            return Err(
                "native enrollment was cancelled; retained proof requires reconciliation".into(),
            );
        }
        let intent = VerifiedNativeEnrollmentIntent::restore(pending.intent_bytes())?;
        let signed = intent.verified_join().signed_invitation();
        let same_invitation = locator.as_ref().map_or_else(
            || join::encode_invitation_text(signed) == invitation,
            |link| {
                link.reference.matches(signed)
                    && link.service == intent.transport_hint().service_public_key
            },
        );
        if (intent.local_device() != &device
            && !crate::owner::renewal::renews(&device, intent.local_device()))
            || !same_invitation
        {
            return Err(
                "native retry differs from persisted invitation or selected ownership".into(),
            );
        }
        // Reuse the recorded proof even when its original challenge has expired.
        // The issuing host alone resolves durable Complete idempotency/current membership.
        let id = owner.prepare_enrollment_into(&intent, local, tilo_clock::wall())?;
        if &id != pending.id() {
            return Err("native retry changed pending generation".into());
        }
        (intent, id)
    } else {
        let signed = if let Some(locator) = &locator {
            client
                .resolve_invitation(locator, device.clone())
                .await
                .map_err(|error| error.to_string())?
        } else {
            join::invitation_bytes(&invitation)?
        };
        let join = join::decode_and_verify_invitation(
            &signed,
            Some(device.user_public_key),
            &revocations,
            now_ns(tilo_clock::wall())?,
        )?;
        if locator.is_none() && join::encode_invitation_text(join.signed_invitation()) != invitation
        {
            return Err("noncanonical native invitation text".into());
        }
        let (hint, host) = invitation_host(&join)?;
        if locator.as_ref().is_some_and(|link| {
            link.reference.workspace != join.grant().workspace_id
                || link.service != hint.service_public_key
        }) {
            return Err("resolved invitation changed its workspace or issuer endpoint".into());
        }
        let hint = hint.clone();
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
        let (challenge, governance) = match client
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
            JoinTransportResponse::Challenge(value) => (value.challenge, value.governance),
            JoinTransportResponse::Error { message, .. } => return Err(refused(&message)),
            _ => return Err("pinned native host answered no enrollment challenge".into()),
        };
        current(&identity, identity_topic, &device)?;
        // The device's own clock. Checking the host's challenge here with a
        // reading from a second clock refused a certificate the host had just
        // admitted, and hid the host's answer for it from the classifications
        // above, which is what leaves a retained proof unresolved.
        let prepared = tilo_clock::wall();
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
        // The device signs its own admission, the invitation its authority:
        // whoever answered the join carries it to the workspace unchanged. It
        // is kept with the proof, so a retry sends the very same changes.
        let redemption = join::verify_connection_redemption(
            join.signed_invitation(),
            &device,
            request_id,
            &revocations,
            now_ns(prepared)?,
        )?;
        let admission = crate::own_admission::sign_own_admission(
            &join.grant().workspace_id.to_string(),
            &governance,
            &redemption,
            None,
            identity.signer.as_ref(),
            prepared,
        )
        .await?;
        let complete = JoinCompleteRequest {
            proof: join::sign_join_proof(challenge, identity.signer.as_ref())?,
            admission,
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
    let (host, _) = WorkspaceDeviceHintV1::authenticate_transport_hint(intent.transport_hint())
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
        // ownership has renewed since: the workspace takes the renewal through the
        // membership exchange before the device takes the workspace in, so the
        // host holds the certificate this device presents from now on.
        JoinTransportResponse::Complete(bootstrap) if bootstrap.device_cert != device => {
            let grant = intent.verified_join().grant();
            let (bootstrap, _) = prove_membership(
                &client,
                &host.endpoint_addr,
                grant.workspace_id,
                &device,
                identity.signer.as_ref(),
                (&[], &[]),
            )
            .await?;
            current(&identity, identity_topic, &device)?;
            active(&owner, &intent, &id)?;
            VerifiedNativeEnrollment::finalize(intent, id, bootstrap)
        }
        JoinTransportResponse::Complete(bootstrap) => {
            VerifiedNativeEnrollment::finalize(intent, id, bootstrap)
        }
        // The host admitted this proof before, and the workspace has moved on
        // since, so it no longer replays its answer: this device is a
        // member, and takes the workspace's current state through the membership
        // exchange. Resending the proof would be refused forever, keeping
        // this device from the workspace it joined. A host from before it said so
        // for a certificate that ran out since refuses that certificate
        // instead; ownership has renewed it, and the exchange presents the
        // renewal.
        JoinTransportResponse::Error { code, message }
            if completed(code, &message)
                || (expired(code, &message) && intent.local_device() != &device) =>
        {
            let grant = intent.verified_join().grant();
            let (bootstrap, _) = prove_membership(
                &client,
                &host.endpoint_addr,
                grant.workspace_id,
                &device,
                identity.signer.as_ref(),
                (&[], &[]),
            )
            .await?;
            current(&identity, identity_topic, &device)?;
            active(&owner, &intent, &id)?;
            VerifiedNativeEnrollment::finalize(intent, id, bootstrap)
        }
        // The host holds neither the challenge this proof answers nor a
        // completion of its request: it never admitted it and never will.
        // Kept, the proof would be resent and refused forever, and no new
        // proof could be made for the workspace; the invitation is redeemed, if
        // at all, through a new challenge.
        JoinTransportResponse::Error { code, message } if lapsed(code, &message) => {
            owner.drop_lapsed_enrollment(intent.workspace(), &id)?;
            // The proof carried the certificate ownership has renewed since:
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
/// and the workspace has moved on ([`join::JOIN_COMPLETED_REFRESH`]). A host from
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

/// The workspace's current state for a device that is a member
/// ([`crate::membership::prove`]), asked of the host at `host` over iroh, and the
/// row the device signed on top of `earlier`; the host sends only what `held`, the
/// governance the device holds already, lacks.
///
/// # Errors
/// The serving device cannot be reached, or refuses the proof.
pub async fn prove_membership(
    client: &WorkspaceJoinClient,
    host: &iroh::EndpointAddr,
    workspace_id: WorkspaceId,
    device: &DeviceCert,
    signer: &dyn collab_types::signer::DeviceSigner,
    (earlier, held): (&[Vec<u8>], &[Vec<u8>]),
) -> Result<(join::JoinBootstrap, Vec<Vec<u8>>), String> {
    let mut request_id = [0; 32];
    rand_core::Rng::fill_bytes(
        &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
        &mut request_id,
    );
    crate::membership::prove(
        client.endpoint_public_key(),
        |request| async move {
            client
                .request(host.clone(), request)
                .await
                .map_err(|error| error.to_string())
        },
        request_id,
        workspace_id,
        device,
        signer,
        (earlier, held),
    )
    .await
}

/// The member list a device signs its own admission into `workspace_id` on
/// ([`crate::membership::entry_list`]), asked over iroh of the host at `host` whose entry answered
/// the device's entry `request_id` that it may sign itself in. The host sends only what `held`, the
/// list the device took for an earlier answer of the same entry, lacks.
///
/// # Errors
/// The host cannot be reached, or refuses.
pub async fn entry_list(
    client: &WorkspaceJoinClient,
    host: &iroh::EndpointAddr,
    (workspace_id, request_id): (WorkspaceId, [u8; 32]),
    device: &DeviceCert,
    signer: &dyn collab_types::signer::DeviceSigner,
    held: &[Vec<u8>],
) -> Result<Vec<Vec<u8>>, String> {
    crate::membership::entry_list(
        (client.endpoint_public_key(), |request| async move {
            client
                .request(host.clone(), request)
                .await
                .map_err(|error| error.to_string())
        }),
        request_id,
        workspace_id,
        device,
        signer,
        held,
    )
    .await
}

/// Prove this device's membership of `workspace`, a workspace it joined, with the
/// renewal of the certificate the workspace's record holds, when its ownership
/// holds one, and take the host's answer in
/// ([`DeviceWorkspaces::renew_membership`]). Answers whether it renewed.
pub async fn renew_membership(
    client: WorkspaceJoinClient,
    identity: DeviceIdentity,
    owner: DeviceWorkspaces,
    workspace: WorkspaceId,
) -> Result<bool, String> {
    let (topic, hint, held) = owner.with_current(workspace, |view| {
        Ok((
            view.workspace_id(),
            view.transport_hint()?.clone(),
            view.device().clone(),
        ))
    })?;
    let (renewed, _) = identity.current_material(topic)?;
    if !crate::owner::renewal::renews(&renewed, &held) {
        return Ok(false);
    }
    let (serving, _) =
        WorkspaceDeviceHintV1::authenticate_transport_hint(&hint).map_err(|e| e.to_string())?;
    let (bootstrap, _) = prove_membership(
        &client,
        &serving.endpoint_addr,
        topic,
        &renewed,
        identity.signer.as_ref(),
        (&[], &[]),
    )
    .await?;
    current(&identity, topic, &renewed)?;
    owner.renew_membership(workspace, &renewed, &bootstrap, tilo_clock::wall())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::endpoint_delegation;

    tilo_test_env::scenario! {
        fn a_retained_proof_follows_what_its_host_answers;

        phase only_a_host_that_holds_neither_challenge_nor_completion_ends_a_retained_proof;
        {
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

        phase a_host_that_admitted_the_proof_before_sends_the_device_to_its_membership;
        // A device that the host admitted, but whose answer never reached it
        // (the laptop restarted mid-join), takes the workspace through the
        // membership exchange once the workspace moved on, rather than resending
        // its proof forever (the Lounge, 2026-10-20).
        {
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

        phase a_host_that_refuses_the_proofs_expired_certificate_is_told_apart;
        // A host from before it answered a lapsed certificate with
        // [`join::JOIN_COMPLETED_REFRESH`] refuses it in words; the retained
        // proof then goes through the membership exchange with the renewal
        // instead of being resent every retry (the Lounge, 2026-10-23).
        {
            assert!(expired(403, "device cert has expired"));
            assert!(!expired(503, "device cert has expired"));
            assert!(!expired(403, "device cert is not yet valid"));
            assert!(!lapsed(403, "device cert has expired"));
            assert!(!completed(403, "device cert has expired"));
        }
    }

    /// A membership request states whose key dials for its device only when it
    /// has to: a device that dials as its own key carries none, and one whose
    /// key stays held carries a delegation that names the key it dials from.
    #[tokio::test]
    async fn a_membership_request_carries_the_delegation_its_endpoint_needs() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = DeviceCert {
            user_public_key: [1; 32],
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Page".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        assert_eq!(
            endpoint_delegation(device.device_public_key, &device, &key)
                .await
                .unwrap(),
            None,
            "a device that dials as its own key delegates nothing"
        );
        let delegation = endpoint_delegation([7; 32], &device, &key)
            .await
            .unwrap()
            .expect("a key of its own speaks for the device by its delegation");
        assert_eq!(delegation.device_public_key, device.device_public_key);
        assert_eq!(delegation.endpoint_public_key, [7; 32]);
        tilo_user_identity::verify_endpoint_delegation(
            &delegation,
            &[7; 32],
            tilo_clock::unix_now_ns(),
        )
        .expect("the delegation is live and the device's own");
    }
}
