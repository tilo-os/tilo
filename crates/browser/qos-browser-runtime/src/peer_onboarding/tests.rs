use super::*;
use ed25519_dalek::{Signer, SigningKey};
use qos_peer_state::join::{self, InviteGrant, JoinCompleteRequest, MemberRole};
use tilo_relay::delivery_v3::onboarding_wire::{OnboardingOperation, OnboardingRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

const NOW: u64 = 1_000_000_000_000;
const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ORIGIN: &str = "https://delivery.example";

fn certificate(
    root: &SigningKey,
    key: &SigningKey,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "onboarding fixture".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn invited() -> (InvitedIdentity, DeviceCert, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let governor_key = SigningKey::from_bytes(&[2; 32]);
    let delivery_key = SigningKey::from_bytes(&[3; 32]);
    let governor = certificate(&root, &governor_key, DeviceCapabilities::primary());
    let delivery = certificate(&root, &delivery_key, DeviceCapabilities::host());
    let locator = tilo_relay::workspace_join::DeliveryOriginLocatorV1 {
        version: 1,
        origin: ORIGIN.into(),
    }
    .transport_hint(&delivery, &delivery_key)
    .unwrap();
    let grant = InviteGrant {
        workspace_id: tilo_relay::room_workspace_id(ROOM).unwrap(),
        room: ROOM.into(),
        authority: governor.clone(),
        issuer: governor.clone(),
        delegation: None,
        role: MemberRole::Editor,
        nonce: [4; 16],
        recipient_user_public_key: None,
        invitee_name: None,
        expires_at_ns: NOW + 600_000_000_000,
        transport: vec![locator],
    };
    let invitation =
        join::encode_invitation_text(&join::issue_invitation(grant, &governor_key).unwrap());
    let material = InvitedIdentity::create(&invitation, "Independent guest", NOW).unwrap();
    let mut identity = InvitedIdentity::read(&material, NOW).unwrap();
    let challenge = join::JoinChallenge::new(
        &identity.invitation(NOW).unwrap(),
        identity.certificate(),
        identity.request_id(),
        [9; 32],
        NOW + 30_000_000_000,
        NOW,
    )
    .unwrap();
    let checkpoint = identity.checkpoint_challenge(challenge, NOW).unwrap();
    // The original signed completion survives reload and challenge expiry.
    let restored = InvitedIdentity::read(&checkpoint, NOW + 31_000_000_000).unwrap();
    (restored, governor, delivery)
}

#[test]
fn prepared_wrapper_preserves_durable_complete_and_pins_distinct_delivery_host() {
    let (identity, governor, delivery) = invited();
    assert_ne!(
        identity.certificate().user_public_key,
        governor.user_public_key
    );
    assert_ne!(delivery.device_public_key, governor.device_public_key);
    let complete: JoinCompleteRequest =
        postcard::from_bytes(identity.completion().unwrap()).unwrap();
    let request = JoinTransportRequest::Complete(complete);
    let expected_legacy = postcard::to_stdvec(&request).unwrap();
    let budget = IncomingBudget::new(4 * 1024 * 1024).unwrap();
    let before = budget.available();
    let prepared =
        prepare_invited(&identity, request.clone(), &budget, NOW + 31_000_000_000).unwrap();
    let wire = OnboardingRequest::decode(prepared.request_bytes()).unwrap();
    assert_eq!(wire.operation, OnboardingOperation::Complete);
    assert_eq!(wire.scope.origin, ORIGIN);
    assert_eq!(wire.scope.host, delivery.device_public_key);
    assert_eq!(wire.scope.root, governor.user_public_key);
    assert_eq!(wire.scope.room.to_string(), ROOM);
    assert_eq!(wire.body, expected_legacy);
    assert_eq!(wire.device_certificate().unwrap(), *identity.certificate());
    wire.verify_device_signature(wire.scope, identity.certificate(), NOW + 31_000_000_000)
        .unwrap();
    assert!(
        budget.available() < before,
        "prepared backing retains real incoming credit"
    );
    let retry = prepare_invited(&identity, request, &budget, NOW + 32_000_000_000).unwrap();
    let retried = OnboardingRequest::decode(retry.request_bytes()).unwrap();
    assert_ne!(wire.nonce, retried.nonce);
    assert_eq!(wire.body, retried.body);
    drop(retry);
    drop(prepared);
    assert_eq!(budget.available(), before);
}

#[test]
fn retained_completion_preflight_counts_actual_owned_fields_and_rejects_hostile_lengths() {
    let (identity, _, _) = invited();
    let original = identity.completion().unwrap();
    let complete: JoinCompleteRequest = postcard::from_bytes(original).unwrap();
    // The string and invitation own payloads; legacy bytes64 temporarily owns
    // 64 signature bytes while decoding its fixed final array.
    assert_eq!(
        preflight_completion(original).unwrap(),
        complete.proof.challenge.signed_invitation.len() + 36 + 64
    );
    let mut trailing = original.to_vec();
    trailing.push(0);
    assert!(preflight_completion(&trailing).is_err());
    assert!(preflight_completion(&[1, 0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
    assert!(preflight_completion(&[]).is_err());
}

#[test]
fn signed_reply_must_match_the_original_prepared_request_not_only_its_host() {
    use tilo_relay::delivery_v3::onboarding_wire::{OnboardingResponse, ReplyBodyKind};
    let (identity, _, _) = invited();
    let complete = postcard::from_bytes(identity.completion().unwrap()).unwrap();
    let budget = IncomingBudget::new(4 * 1024 * 1024).unwrap();
    let prepared = prepare_invited(
        &identity,
        JoinTransportRequest::Complete(complete),
        &budget,
        NOW,
    )
    .unwrap();
    let request = OnboardingRequest::decode(prepared.request_bytes()).unwrap();
    let inner = postcard::to_stdvec(&join::JoinTransportResponse::Error {
        code: 403,
        message: "current policy denied".into(),
    })
    .unwrap();
    let mut reply = OnboardingResponse {
        version: 3,
        operation: OnboardingOperation::Complete,
        body_kind: ReplyBodyKind::InlineLegacy,
        request_digest: request.request_digest().unwrap(),
        request_nonce: request.nonce,
        host: request.scope.host,
        server_boot_id: [8; 16],
        issued_at_ns: NOW,
        expires_at_ns: NOW + 30_000_000_000,
        body_digest: canonical_item_digest(&inner),
        body: &inner,
        signature: [[0; 32]; 2],
    };
    let sign = |reply: &mut OnboardingResponse<'_>| {
        let signature = SigningKey::from_bytes(&[3; 32])
            .sign(&reply.signing_digest().unwrap())
            .to_bytes();
        reply.signature[0].copy_from_slice(&signature[..32]);
        reply.signature[1].copy_from_slice(&signature[32..]);
        reply.encode().unwrap()
    };
    let correct = sign(&mut reply);
    let verified = prepared.verify_reply(&correct, NOW).unwrap();
    assert_eq!(verified.body, inner);
    assert_eq!(verified.kind, ReplyBodyKind::InlineLegacy);
    reply.request_nonce = [7; 32];
    assert!(prepared.verify_reply(&sign(&mut reply), NOW).is_err());
    assert!(
        prepared
            .verify_reply(&correct, NOW + 30_000_000_000)
            .is_err()
    );
    let mut corrupt = correct;
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(prepared.verify_reply(&corrupt, NOW).is_err());
}

fn bootstrap(
    identity: &InvitedIdentity,
    governor: &DeviceCert,
    host: &DeviceCert,
) -> join::JoinBootstrap {
    use automerge::transaction::Transactable;
    let governor_key = SigningKey::from_bytes(&[2; 32]);
    let complete: JoinCompleteRequest =
        postcard::from_bytes(identity.completion().unwrap()).unwrap();
    let redemption = join::verify_join_proof(
        &complete.proof,
        &complete.proof.challenge,
        identity.certificate(),
        &tilo_user_identity::RevocationList::new(),
        NOW,
    )
    .unwrap();
    let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(NOW);
    let roster = vec![governor.clone(), host.clone()];
    let mut native = qos_peer_state::StatePeer::new(
        ROOM,
        governor.clone(),
        roster.clone(),
        tilo_user_identity::RevocationList::new(),
        time,
    )
    .unwrap();
    let mut doc = automerge::AutoCommit::new();
    let mut state = 7u32;
    let data: Vec<u8> = (0..350_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    doc.put(
        automerge::ROOT,
        "retained",
        automerge::ScalarValue::Bytes(data),
    )
    .unwrap();
    native
        .install_seed(
            qos_peer_state::SeedState {
                room_changes: qos_peer_state::seed_room(ROOM, governor, &roster).unwrap(),
                documents: vec![qos_peer_state::SeedDocument {
                    app_id: "test.app".into(),
                    record_id: "tab:1".into(),
                    kind: "test".into(),
                    bytes: doc.save(),
                }],
            },
            &governor_key,
            time,
        )
        .unwrap();
    native
        .admit_invited_member(&redemption, &governor_key, time)
        .unwrap();
    join::JoinBootstrap {
        version: 1,
        invite_id: redemption.join().invite_id(),
        device_cert: identity.certificate().clone(),
        role: MemberRole::Editor,
        room: ROOM.into(),
        workspace_id: tilo_relay::room_workspace_id(ROOM).unwrap(),
        authority: governor.clone(),
        certified_roster: vec![identity.certificate().clone()],
        revocations: vec![],
        records: vec![],
        governance_messages: native
            .bootstrap_messages(&governor_key, time)
            .unwrap()
            .iter()
            .map(|m| postcard::to_stdvec(m).unwrap())
            .collect(),
        generation: 1,
    }
}

#[test]
fn handoff_reassembles_exact_signed_native_bootstrap_under_shared_credit() {
    use super::handoff::{Handoff, ManifestContext};
    use wire::{HandoffChunk, HandoffManifest, HandoffPurpose, HandoffSegment};
    let (identity, governor, host) = invited();
    let inner = postcard::to_stdvec(&join::JoinTransportResponse::Complete(bootstrap(
        &identity, &governor, &host,
    )))
    .unwrap();
    assert!(inner.len() > wire::MAX_HANDOFF_RANGE_BYTES);
    let budget = IncomingBudget::new(8 * 1024 * 1024).unwrap();
    let complete = postcard::from_bytes(identity.completion().unwrap()).unwrap();
    let prepared = prepare_invited(
        &identity,
        JoinTransportRequest::Complete(complete),
        &budget,
        NOW,
    )
    .unwrap();
    let request = OnboardingRequest::decode(prepared.request_bytes()).unwrap();
    let context = ManifestContext {
        scope: request.scope,
        purpose: HandoffPurpose::JoinResponse,
        device: identity.certificate().device_public_key,
        request_digest: request.request_digest().unwrap(),
        request_nonce: request.nonce,
        expires_at_ns: request.expires_at_ns,
    };
    let mut manifest = HandoffManifest {
        version: 1,
        purpose: context.purpose,
        scope: context.scope,
        device: context.device,
        request_digest: context.request_digest,
        request_nonce: context.request_nonce,
        owner_generation: 1,
        server_boot_id: [8; 16],
        issued_at_ns: NOW,
        expires_at_ns: NOW + 30_000_000_000,
        total_bytes: inner.len() as u64,
        content_digest: canonical_item_digest(&inner),
        segments: inner
            .chunks(250_000)
            .map(|part| HandoffSegment {
                digest: canonical_item_digest(part),
                length: part.len() as u64,
            })
            .collect(),
        signature: [[0; 32]; 2],
    };
    let host_key = SigningKey::from_bytes(&[3; 32]);
    let signature = host_key
        .sign(&manifest.signing_digest().unwrap())
        .to_bytes();
    manifest.signature[0].copy_from_slice(&signature[..32]);
    manifest.signature[1].copy_from_slice(&signature[32..]);
    let encoded = manifest.encode().unwrap();
    let before = budget.available();
    let mut transfer = Handoff::start(&encoded, context, &budget, NOW).unwrap();
    assert!(budget.available() < before);
    assert!(
        Handoff::start(
            &encoded,
            ManifestContext {
                request_nonce: [4; 32],
                ..context
            },
            &budget,
            NOW
        )
        .is_err()
    );
    assert!(Handoff::start(&encoded, context, &IncomingBudget::new(4096).unwrap(), NOW).is_err());
    while transfer.remaining() != 0 {
        let fetch = transfer.request(&identity.device_key(), NOW).unwrap();
        assert!(fetch.length as usize <= 240 * 1024);
        let body = &inner[fetch.offset as usize..fetch.offset as usize + fetch.length as usize];
        let mut chunk = HandoffChunk {
            version: 1,
            manifest_digest: manifest.manifest_digest().unwrap(),
            request_digest: fetch.request_digest().unwrap(),
            offset: fetch.offset,
            body_digest: canonical_item_digest(body),
            body,
            signature: [[0; 32]; 2],
        };
        let signature = host_key.sign(&chunk.signing_digest().unwrap()).to_bytes();
        chunk.signature[0].copy_from_slice(&signature[..32]);
        chunk.signature[1].copy_from_slice(&signature[32..]);
        transfer
            .push(&fetch, &chunk.encode().unwrap(), NOW)
            .unwrap();
    }
    let held = transfer.finish(NOW).unwrap();
    assert_eq!(held.bytes, inner);
    assert!(
        budget.available() < before,
        "complete output still owns its credit"
    );
    drop(held);
    assert_eq!(budget.available(), before);
}

fn local_fixture(descriptor_expires: u64) -> (Vec<u8>, String, String, Vec<u8>) {
    use qos_peer_state::enrollment::{
        self, ChallengeRequest, ChallengeResponse, EnrollmentChallenge,
    };
    use qos_peer_state::transport::Retention;
    let material = crate::identity::create();
    let request: ChallengeRequest =
        serde_json::from_str(&crate::identity::request(&material).unwrap()).unwrap();
    let unhex = |s: &str| -> [u8; 32] {
        std::array::from_fn(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
    };
    let root = SigningKey::from_bytes(&[1; 32]);
    let host = certificate(
        &root,
        &SigningKey::from_bytes(&[3; 32]),
        DeviceCapabilities::host(),
    );
    let mut cert = host.clone();
    cert.device_public_key = unhex(&request.device_public_key);
    cert.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut cert, &root).unwrap();
    let challenge = EnrollmentChallenge {
        version: 2,
        root_public_key: root.verifying_key().to_bytes(),
        room: ROOM.into(),
        workspace_id: tilo_relay::room_workspace_id(ROOM).unwrap(),
        device_public_key: cert.device_public_key,
        request_id: unhex(&request.request_id),
        nonce: [8; 32],
        expires_at_ns: NOW + 60_000_000_000,
    };
    let challenge = serde_json::to_string(&ChallengeResponse {
        version: 2,
        challenge: enrollment::encode(&challenge).unwrap(),
    })
    .unwrap();
    let governor = certificate(
        &root,
        &SigningKey::from_bytes(&[2; 32]),
        DeviceCapabilities::primary(),
    );
    let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(NOW);
    let roster = vec![governor.clone(), host.clone(), cert.clone()];
    let mut owner = qos_peer_state::StatePeer::new(
        ROOM,
        governor.clone(),
        roster.clone(),
        tilo_user_identity::RevocationList::new(),
        time,
    )
    .unwrap();
    owner
        .install_seed(
            qos_peer_state::SeedState {
                room_changes: qos_peer_state::seed_room(ROOM, &governor, &roster).unwrap(),
                documents: vec![],
            },
            &SigningKey::from_bytes(&[2; 32]),
            time,
        )
        .unwrap();
    use tilo_relay::delivery_v3::descriptor::{
        self, DescriptorAuthority, DescriptorSettings, GenerationExpectation,
    };
    let authority = DescriptorAuthority {
        peer: &owner,
        pinned_governor: &governor,
        pinned_host: &host,
        workspace: tilo_relay::room_workspace_id(ROOM).unwrap(),
        generation: GenerationExpectation::FirstAccepted {
            minimum_generation: 1,
        },
        allowed_delivery_capabilities: 0,
        now: time,
    };
    let hint = descriptor::issue_transport_hint(
        &DescriptorSettings {
            origin: ORIGIN.into(),
            delivery_capabilities: 0,
            issued_at_ns: NOW,
            expires_at_ns: descriptor_expires,
            generation: 1,
        },
        &authority,
        &SigningKey::from_bytes(&[3; 32]),
    )
    .unwrap();
    let (identity, governor, delivery) = invited();
    let actual = bootstrap(&identity, &governor, &delivery).governance_messages;
    let kinds: Vec<_> = actual.iter().map(|_| Retention::Checkpoint).collect();
    let raw = serde_json::json!({"version":2,"certificate":cert,"root_public_key":root.verifying_key().to_bytes(),"host_certificate":host,"bootstrap":{"room":ROOM,"workspace_id":tilo_relay::room_workspace_id(ROOM).unwrap().to_string(),"delivery_hint":hint,"governance_messages":actual,"governance_retention":kinds,"governance_generation":"17"}}).to_string();
    // The inline fixture must fit native's exact initial reply ceiling.
    // Use a smaller, still genuinely signed governance publication here.
    let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let small = &actual[0];
    value["bootstrap"]["governance_messages"] = serde_json::json!([small]);
    value["bootstrap"]["governance_retention"] = serde_json::json!([Retention::Checkpoint]);
    let raw = value.to_string();
    assert!(raw.len() < 1024 * 1024);
    (material, challenge, raw, small.clone())
}

#[test]
fn local_governance_requires_exact_durable_binding_and_retains_credit() {
    use qos_peer_state::transport::Retention;
    let (material, challenge, raw, small) = local_fixture(NOW + 60_000_000_000);
    let budget = IncomingBudget::new(8 * 1024 * 1024).unwrap();
    let before = budget.available();
    let bound = bind_local(&material, &challenge, &raw, budget.clone(), NOW).unwrap();
    assert_eq!(
        bound.material(),
        crate::identity::bind(&material, &challenge, &raw).unwrap()
    );
    let durable = bound.material().to_vec();
    let context = bound.into_context();
    assert!(context.inline_governance(&material, &raw, NOW).is_err());
    let reply = context.inline_governance(&durable, &raw, NOW).unwrap();
    assert_eq!(reply.generation(), 17);
    let received: Vec<_> = reply.messages().collect();
    assert_eq!(received, vec![(small.as_slice(), Retention::Checkpoint)]);
    assert!(
        context.inline_governance(&durable, &raw, NOW).is_err(),
        "context consumes once"
    );
    assert!(budget.available() < before);
    drop(reply);
    drop(context);
    assert_eq!(budget.available(), before);
}

#[test]
fn decoded_reply_keeps_nested_bootstrap_credit_until_holder_drops() {
    let (identity, governor, host) = invited();
    let inner = postcard::to_stdvec(&join::JoinTransportResponse::Complete(bootstrap(
        &identity, &governor, &host,
    )))
    .unwrap();
    let budget = IncomingBudget::new(64 * 1024 * 1024).unwrap();
    let before = budget.available();
    let held = OnboardingReply::decode(&inner, &budget).unwrap();
    let join::JoinTransportResponse::Complete(value) = held.response() else {
        panic!("wrong response");
    };
    assert!(!value.governance_messages.is_empty());
    assert_eq!(postcard::to_stdvec(held.response()).unwrap(), inner);
    assert!(budget.available() < before);
    assert!(OnboardingReply::decode(&inner, &IncomingBudget::new(4096).unwrap()).is_err());
    drop(held);
    assert_eq!(budget.available(), before);
}

#[test]
fn governance_bundle_lends_exact_paired_messages_without_escaping_custody() {
    use qos_peer_state::transport::Retention;
    let (identity, governor, host) = invited();
    let data = bootstrap(&identity, &governor, &host).governance_messages;
    let bundle = wire::GovernanceBundle {
        items: data
            .iter()
            .map(|bytes| wire::GovernanceItem {
                bytes,
                retention: Retention::Delta,
            })
            .collect(),
    };
    let encoded = postcard::to_stdvec(&bundle).unwrap();
    let budget = IncomingBudget::new(8 * 1024 * 1024).unwrap();
    let before = budget.available();
    let reply =
        GovernanceReply::bundle(ChargedBytes::copy(&encoded, &budget).unwrap(), 42, &budget)
            .unwrap();
    assert_eq!(reply.generation(), 42);
    assert_eq!(
        reply.messages().collect::<Vec<_>>(),
        data.iter()
            .map(|b| (b.as_slice(), Retention::Delta))
            .collect::<Vec<_>>()
    );
    assert!(budget.available() < before);
    drop(reply);
    assert_eq!(budget.available(), before);
}

#[test]
fn handoff_rejects_host_signed_bad_digests_and_replayed_ranges() {
    use handoff::{Handoff, ManifestContext};
    let (identity, _, host) = invited();
    let bytes = vec![9u8; 300_000];
    let context = ManifestContext {
        scope: wire::OnboardingScope {
            origin: ORIGIN,
            host: host.device_public_key,
            workspace: tilo_relay::room_workspace_id(ROOM).unwrap(),
            root: host.user_public_key,
            room: ROOM.parse().unwrap(),
        },
        purpose: wire::HandoffPurpose::JoinResponse,
        device: identity.certificate().device_public_key,
        request_digest: [6; 32],
        request_nonce: [7; 32],
        expires_at_ns: NOW + 30_000_000_000,
    };
    let host_key = SigningKey::from_bytes(&[3; 32]);
    for mode in 0..3 {
        let budget = IncomingBudget::new(4 * 1024 * 1024).unwrap();
        let before = budget.available();
        let mut manifest = wire::HandoffManifest {
            version: 1,
            purpose: context.purpose,
            scope: context.scope,
            device: context.device,
            request_digest: context.request_digest,
            request_nonce: context.request_nonce,
            owner_generation: 1,
            server_boot_id: [8; 16],
            issued_at_ns: NOW,
            expires_at_ns: NOW + 30_000_000_000,
            total_bytes: bytes.len() as u64,
            content_digest: if mode == 1 {
                [2; 32]
            } else {
                canonical_item_digest(&bytes)
            },
            segments: bytes
                .chunks(150_000)
                .map(|part| wire::HandoffSegment {
                    digest: if mode == 0 {
                        [1; 32]
                    } else {
                        canonical_item_digest(part)
                    },
                    length: part.len() as u64,
                })
                .collect(),
            signature: [[0; 32]; 2],
        };
        let sign = host_key
            .sign(&manifest.signing_digest().unwrap())
            .to_bytes();
        manifest.signature[0].copy_from_slice(&sign[..32]);
        manifest.signature[1].copy_from_slice(&sign[32..]);
        let mut transfer =
            Handoff::start(&manifest.encode().unwrap(), context, &budget, NOW).unwrap();
        loop {
            let request = transfer.request(&identity.device_key(), NOW).unwrap();
            let body =
                &bytes[request.offset as usize..request.offset as usize + request.length as usize];
            let mut chunk = wire::HandoffChunk {
                version: 1,
                manifest_digest: manifest.manifest_digest().unwrap(),
                request_digest: request.request_digest().unwrap(),
                offset: request.offset,
                body_digest: canonical_item_digest(body),
                body,
                signature: [[0; 32]; 2],
            };
            let sign = host_key.sign(&chunk.signing_digest().unwrap()).to_bytes();
            chunk.signature[0].copy_from_slice(&sign[..32]);
            chunk.signature[1].copy_from_slice(&sign[32..]);
            let encoded = chunk.encode().unwrap();
            let result = transfer.push(&request, &encoded, NOW);
            if mode == 0 {
                assert!(result.is_err());
                break;
            }
            result.unwrap();
            if mode == 2 {
                assert!(transfer.push(&request, &encoded, NOW).is_err());
                break;
            }
            if transfer.remaining() == 0 {
                break;
            }
        }
        assert!(transfer.finish(NOW).is_err());
        assert_eq!(
            budget.available(),
            before,
            "failure drops all backing before credit"
        );
    }
}

fn signed_deadline_manifest<'a>(
    context: handoff::ManifestContext<'a>,
    body: &[u8],
    issued: u64,
    expires: u64,
) -> wire::HandoffManifest<'a> {
    let mut value = wire::HandoffManifest {
        version: 1,
        purpose: context.purpose,
        scope: context.scope,
        device: context.device,
        request_digest: context.request_digest,
        request_nonce: context.request_nonce,
        owner_generation: 1,
        server_boot_id: [8; 16],
        issued_at_ns: issued,
        expires_at_ns: expires,
        total_bytes: body.len() as u64,
        content_digest: canonical_item_digest(body),
        segments: vec![wire::HandoffSegment {
            length: body.len() as u64,
            digest: canonical_item_digest(body),
        }],
        signature: [[0; 32]; 2],
    };
    let signature = SigningKey::from_bytes(&[3; 32])
        .sign(&value.signing_digest().unwrap())
        .to_bytes();
    value.signature[0].copy_from_slice(&signature[..32]);
    value.signature[1].copy_from_slice(&signature[32..]);
    value
}
fn signed_deadline_chunk(
    manifest: &wire::HandoffManifest<'_>,
    request: &wire::HandoffRequest,
    body: &[u8],
) -> Vec<u8> {
    let mut chunk = wire::HandoffChunk {
        version: 1,
        manifest_digest: manifest.manifest_digest().unwrap(),
        request_digest: request.request_digest().unwrap(),
        offset: request.offset,
        body_digest: canonical_item_digest(body),
        body,
        signature: [[0; 32]; 2],
    };
    let signature = SigningKey::from_bytes(&[3; 32])
        .sign(&chunk.signing_digest().unwrap())
        .to_bytes();
    chunk.signature[0].copy_from_slice(&signature[..32]);
    chunk.signature[1].copy_from_slice(&signature[32..]);
    chunk.encode().unwrap()
}
#[test]
fn original_deadline_local_descriptor_survives_claim_but_not_expiry() {
    let deadline = NOW + 1_000_000_000;
    let (material, challenge, raw, _) = local_fixture(deadline);
    let budget = IncomingBudget::new(8 * 1024 * 1024).unwrap();
    let bound = bind_local(&material, &challenge, &raw, budget, NOW).unwrap();
    let durable = bound.material().to_vec();
    let context = bound.into_context();
    let _reply = context.inline_governance(&durable, &raw, NOW).unwrap();
    assert_eq!(context.expires_at_ns(), deadline);
    context.check_continuation(deadline - 1).unwrap();
    assert!(
        context.check_continuation(deadline).is_err(),
        "claimed original descriptor expiry must still fence continuations"
    );
}
#[test]
fn original_deadline_caps_signed_ranges_and_completed_output_for_local_and_invited() {
    use handoff::{Handoff, ManifestContext};
    let deadline = NOW + 1_000_000_000;
    let (material, challenge, raw, body) = local_fixture(deadline);
    let budget = IncomingBudget::new(8 * 1024 * 1024).unwrap();
    let bound = bind_local(&material, &challenge, &raw, budget.clone(), NOW).unwrap();
    let key = crate::identity::key(bound.material(), &raw).unwrap();
    let local = bound.into_context();
    let (identity, _, _) = invited();
    let prepared = prepare_invited(
        &identity,
        JoinTransportRequest::Complete(
            postcard::from_bytes(identity.completion().unwrap()).unwrap(),
        ),
        &budget,
        NOW,
    )
    .unwrap();
    let request = OnboardingRequest::decode(prepared.request_bytes()).unwrap();
    let invited = ManifestContext {
        scope: request.scope,
        purpose: wire::HandoffPurpose::JoinResponse,
        device: identity.certificate().device_public_key,
        request_digest: request.request_digest().unwrap(),
        request_nonce: request.nonce,
        expires_at_ns: request.expires_at_ns,
    };
    for (context, key, now) in [
        (local.manifest_context(), key, NOW),
        (
            invited,
            identity.device_key(),
            request.expires_at_ns - 1_000_000_000,
        ),
    ] {
        let deadline = context.expires_at_ns;
        let manifest = signed_deadline_manifest(context, &body, now, now + 30_000_000_000);
        manifest
            .verify_host_signature(
                context.scope,
                context.device,
                context.request_digest,
                context.request_nonce,
                deadline,
            )
            .unwrap();
        let encoded = manifest.encode().unwrap();
        let before = budget.available();
        let mut positive = Handoff::start(&encoded, context, &budget, now).unwrap();
        let range = positive.request(&key, now).unwrap();
        assert_eq!(
            range.expires_at_ns, deadline,
            "signed host manifest cannot extend original deadline"
        );
        let chunk = signed_deadline_chunk(&manifest, &range, &body);
        positive.push(&range, &chunk, deadline - 1).unwrap();
        assert_eq!(positive.finish(deadline - 1).unwrap().bytes, body);
        let mut late = Handoff::start(&encoded, context, &budget, now).unwrap();
        let range = late.request(&key, now).unwrap();
        let chunk = signed_deadline_chunk(&manifest, &range, &body);
        assert!(late.push(&range, &chunk, deadline).is_err());
        assert!(late.finish(deadline).is_err());
        let mut finished = Handoff::start(&encoded, context, &budget, now).unwrap();
        let range = finished.request(&key, now).unwrap();
        finished
            .push(
                &range,
                &signed_deadline_chunk(&manifest, &range, &body),
                deadline - 1,
            )
            .unwrap();
        assert!(
            finished.finish(deadline).is_err(),
            "final output remains fenced after successful last range"
        );
        assert!(Handoff::start(&encoded, context, &budget, deadline).is_err());
        assert_eq!(budget.available(), before);
    }
}
