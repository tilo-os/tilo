use super::*;
use crate::delivery_http::tests::Fixture;
use ed25519_dalek::SigningKey;
use tilo_relay::delivery_v3::capabilities::STATE_REPLAY_V1;

fn fixture() -> Fixture {
    let mut f = Fixture::new();
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9443", STATE_REPLAY_V1, at(f.now))
        })
        .unwrap()
        .unwrap();
    f
}
fn enroll(f: &mut Fixture, key: &SigningKey, request_id: u8) {
    use qos_peer_state::enrollment::{self, ChallengeRequest};
    let mut issuer = crate::enrollment::EnrollmentAuthority::default();
    let challenge = issuer
        .challenge(
            &f.owner,
            ChallengeRequest {
                device_public_key: hex::encode(key.verifying_key().to_bytes()),
                request_id: hex::encode([request_id; 32]),
            },
            f.now,
        )
        .unwrap();
    let complete =
        enrollment::sign(&enrollment::decode(&challenge.challenge).unwrap(), key).unwrap();
    f.current
        .with_owner(&mut f.owner, |owner| {
            issuer.complete(owner, complete, f.now)
        })
        .unwrap()
        .unwrap();
}
fn finish(
    f: &Fixture,
    registry: &mut HttpSessions,
    key: &SigningKey,
    requested: u64,
    allowed: u64,
) -> CarrierFinish {
    let nonce = [registry.next_entry as u8 + 21; 32];
    let body = postcard::to_stdvec(&(
        3_u16,
        key.verifying_key().to_bytes(),
        nonce,
        requested,
        0_u64,
    ))
    .unwrap();
    let identity = HttpSessions::challenge_intent(&body).unwrap().0;
    let guard = f.current.read().unwrap();
    // The independently pinned context comes from the actual committed owner;
    // only its downward capability allowance changes in this carrier fixture.
    let context = registry
        .context(&f.owner, guard.view(), identity, f.now)
        .unwrap();
    let mut verification = context.verification();
    verification.capabilities.allowed = allowed;
    let wire = registry
        .challenge_in(&body, &verification, &f.owner.identity.host)
        .unwrap();
    let server = auth::ServerProof::decode(&wire).unwrap();
    let client = auth::verify_server(
        &auth::ClientExpectations {
            scope: auth::ClientScope::from(verification.binding),
            capabilities: verification.capabilities,
            credentials: auth::Credentials {
                device: verification.credentials.device,
                host: verification.credentials.host,
                device_revocations: verification.credentials.device_revocations,
                host_revocations: verification.credentials.host_revocations,
            },
            authority: verification.authority,
            now_ns: f.now,
        },
        nonce,
        &server,
    )
    .unwrap()
    .client_proof(key)
    .unwrap();
    registry
        .finish_in(&client.encode().unwrap(), &verification)
        .unwrap()
}
fn publish(
    f: &Fixture,
    registry: &mut HttpSessions,
    finish: CarrierFinish,
    allowed: u64,
) -> Result<CarrierReady> {
    let guard = f.current.read().unwrap();
    let context = registry
        .context(&f.owner, guard.view(), finish.identity, f.now)
        .unwrap();
    let mut verification = context.verification();
    verification.capabilities.allowed = allowed;
    registry.publish_in(finish, &verification)
}
fn insert(f: &Fixture, registry: &mut HttpSessions, key: &SigningKey, requested: u64) -> [u8; 16] {
    let finish = finish(f, registry, key, requested, STATE_REPLAY_V1);
    let id = finish.session().session_id();
    publish(f, registry, finish, STATE_REPLAY_V1).unwrap();
    id
}
fn assert_downgrade_denied_then_real_replacement(
    f: &Fixture,
    registry: &mut HttpSessions,
    key: &SigningKey,
    old_data: [u8; 16],
) {
    let original: Vec<_> = registry.live.keys().copied().collect();
    let next_entry = registry.next_entry;
    let downgrade = finish(f, registry, key, STATE_REPLAY_V1, 0);
    assert_eq!(
        downgrade.session().capabilities(),
        0,
        "actual signed negotiation downgraded optional bits"
    );
    assert!(
        publish(f, registry, downgrade, 0).is_err(),
        "zero-capability publication must not discount an existing data session"
    );
    assert_eq!(registry.live.keys().copied().collect::<Vec<_>>(), original);
    assert_eq!(registry.next_entry, next_entry);
    let replacement = finish(f, registry, key, STATE_REPLAY_V1, STATE_REPLAY_V1);
    let new_id = replacement.session().session_id();
    let ready = publish(f, registry, replacement, STATE_REPLAY_V1).unwrap();
    assert_eq!(ready.retired, vec![old_data]);
    assert_eq!(registry.live.len(), original.len());
    assert!(registry.live.contains_key(&new_id));
    for id in original.into_iter().filter(|id| *id != old_data) {
        assert!(registry.live.contains_key(&id));
    }
}
#[test]
fn negotiated_capacity_downgrade_cannot_exceed_two_sessions_for_actual_device() {
    let f = fixture();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, STATE_REPLAY_V1).unwrap();
    let key = f.owner.identity.native.clone();
    let old_data = insert(&f, &mut registry, &key, STATE_REPLAY_V1);
    insert(&f, &mut registry, &key, 0);
    assert_eq!(registry.live.len(), LIVE_DEVICE);
    assert!(registry.live.len() < LIVE_LIMIT);
    assert_downgrade_denied_then_real_replacement(&f, &mut registry, &key, old_data);
}
#[test]
fn negotiated_capacity_downgrade_cannot_exceed_sixteen_actual_sessions_globally() {
    let mut f = fixture();
    let keys: Vec<_> = (40..48)
        .map(|seed| SigningKey::from_bytes(&[seed; 32]))
        .collect();
    for (index, key) in keys.iter().enumerate() {
        enroll(&mut f, key, index as u8 + 1);
    }
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, STATE_REPLAY_V1).unwrap();
    let key = f.owner.identity.native.clone();
    let old_data = insert(&f, &mut registry, &key, STATE_REPLAY_V1);
    for other in &keys[..7] {
        insert(&f, &mut registry, other, STATE_REPLAY_V1);
        insert(&f, &mut registry, other, 0);
    }
    insert(&f, &mut registry, &keys[7], 0);
    assert_eq!(registry.live.len(), LIVE_LIMIT);
    assert_eq!(
        registry
            .live
            .values()
            .filter(|live| live.identity.device == key.verifying_key().to_bytes())
            .count(),
        1,
        "target device has room; global capacity alone must deny"
    );
    assert_downgrade_denied_then_real_replacement(&f, &mut registry, &key, old_data);
}
