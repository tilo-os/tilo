use super::*;
use crate::delivery_http::tests::Fixture;
use tilo_relay::delivery_v3::capabilities::PRODUCT_DATA_V1;

fn fixture() -> (
    Fixture,
    crate::https::Ingress,
    crate::https::IngressReceiver,
) {
    let mut f = Fixture::new();
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9443", PRODUCT_DATA_V1, at(f.now))
        })
        .unwrap()
        .unwrap();
    let (ingress, receiver) = crate::https::ingress_channel();
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.attach_data_resources(&ingress.host_bytes())
        })
        .unwrap()
        .unwrap();
    (f, ingress, receiver)
}
fn client(
    f: &Fixture,
    registry: &mut HttpSessions,
    capabilities: u64,
    nonce: u8,
) -> auth::ClientProof {
    let nonce = [nonce; 32];
    let body = postcard::to_stdvec(&(
        3_u16,
        f.owner.identity.native_cert.device_public_key,
        nonce,
        capabilities,
        capabilities,
    ))
    .unwrap();
    let server = auth::ServerProof::decode(
        &registry
            .challenge(&f.owner, &f.current, &body, f.now)
            .unwrap(),
    )
    .unwrap();
    let guard = f.current.read().unwrap();
    let descriptor = f
        .owner
        .verified_delivery_transport(guard.view(), at(f.now), PRODUCT_DATA_V1)
        .unwrap();
    let authority = f.owner.transport_authority(
        guard.peer(),
        GenerationExpectation::Current {
            generation: descriptor.generation(),
            descriptor_digest: descriptor.descriptor_digest(),
        },
        at(f.now),
        PRODUCT_DATA_V1,
    );
    let scope = descriptor
        .client_scope(
            &authority,
            &f.owner.identity.native_cert,
            auth::Carrier::Https,
            capabilities,
            capabilities,
        )
        .unwrap();
    let native = NativeDeliveryAuthority {
        view: guard.view(),
        workspace: f.owner.workspace_id(),
        host: f.owner.host_certificate(),
    };
    let revocations = RevocationList::new();
    auth::verify_server(
        &auth::ClientExpectations {
            scope,
            capabilities: auth::CapabilityPolicy {
                supported: PRODUCT_DATA_V1,
                allowed: PRODUCT_DATA_V1,
            },
            credentials: auth::Credentials {
                device: &f.owner.identity.native_cert,
                host: f.owner.host_certificate(),
                device_revocations: &revocations,
                host_revocations: &revocations,
            },
            authority: &native,
            now_ns: f.now,
        },
        nonce,
        &server,
    )
    .unwrap()
    .client_proof(&f.owner.identity.native)
    .unwrap()
}
#[test]
fn data_registry_registers_real_consumer_before_ready_and_preserves_it_on_reopen() {
    let (mut f, ingress, _receiver) = fixture();
    let mut registry = HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1)
        .expect("real data registry policy");
    let proof = client(&f, &mut registry, PRODUCT_DATA_V1, 151);
    let device = f.owner.identity.native_cert.device_public_key;
    assert!(
        !f.owner
            .delivery_ledger()
            .unwrap()
            .consumers
            .contains_key(&device)
    );
    let generation = f.owner.committed_generation();
    let ready = registry
        .finish_data_at(
            &mut f.owner,
            &f.current,
            &ingress.host_bytes(),
            &proof.encode().unwrap(),
            f.now,
        )
        .unwrap();
    let wire: (u16, [u8; 16], [u8; 32], u64, u64) = postcard::from_bytes(&ready.bytes).unwrap();
    assert_eq!(wire.4, PRODUCT_DATA_V1);
    assert!(ready.retired.is_empty());
    assert_eq!(f.owner.committed_generation(), generation + 1);
    assert_eq!(
        f.current.read().unwrap().generation(),
        f.owner.committed_generation()
    );
    let consumer = f.owner.delivery_ledger().unwrap().consumers[&device].id;
    assert!(registry.live.contains_key(&wire.1));
    let path = f.owner.root().to_owned();
    drop(registry);
    drop(f.owner);
    let reopened = SessionOwner::open(&path).unwrap();
    assert_eq!(
        reopened.delivery_ledger().unwrap().consumers[&device].id,
        consumer
    );
}

fn finish(
    f: &mut Fixture,
    registry: &mut HttpSessions,
    budget: &HostBytes,
    proof: &auth::ClientProof,
) -> DataReady {
    registry
        .finish_data_at(
            &mut f.owner,
            &f.current,
            budget,
            &proof.encode().unwrap(),
            f.now,
        )
        .unwrap()
}
fn ready_id(ready: &DataReady) -> [u8; 16] {
    let wire: (u16, [u8; 16], [u8; 32], u64, u64) = postcard::from_bytes(&ready.bytes).unwrap();
    wire.1
}
#[test]
fn data_registry_replacement_fences_only_prior_data_and_preserves_media_at_device_capacity() {
    let (mut f, ingress, _receiver) = fixture();
    let budget = ingress.host_bytes();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
    let media = client(&f, &mut registry, 0, 152);
    let media = DataReady {
        bytes: registry
            .finish(&f.owner, &f.current, &media.encode().unwrap(), f.now)
            .unwrap(),
        retired: vec![],
    };
    assert!(f.owner.delivery_ledger().unwrap().consumers.is_empty());
    assert!(registry.reserve_private_slot(ready_id(&media)).is_err());
    let first = client(&f, &mut registry, PRODUCT_DATA_V1, 153);
    let first = finish(&mut f, &mut registry, &budget, &first);
    assert_eq!(registry.live.len(), LIVE_DEVICE);
    let before = f.owner.committed_generation();
    let consumer = f.owner.delivery_ledger().unwrap().consumers
        [&f.owner.identity.native_cert.device_public_key]
        .id;
    let second = client(&f, &mut registry, PRODUCT_DATA_V1, 154);
    let second = finish(&mut f, &mut registry, &budget, &second);
    assert_eq!(second.retired, vec![ready_id(&first)]);
    assert!(registry.live.contains_key(&ready_id(&media)));
    assert!(registry.live.contains_key(&ready_id(&second)));
    assert!(!registry.live.contains_key(&ready_id(&first)));
    assert_eq!(registry.live.len(), LIVE_DEVICE);
    assert_eq!(
        f.owner.committed_generation(),
        before,
        "unchanged consumer is not recommitted"
    );
    assert_eq!(
        f.owner.delivery_ledger().unwrap().consumers
            [&f.owner.identity.native_cert.device_public_key]
            .id,
        consumer
    );
}
#[test]
fn data_registry_legacy_finish_cannot_bypass_consumer_commit() {
    let (f, _ingress, _receiver) = fixture();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
    let proof = client(&f, &mut registry, PRODUCT_DATA_V1, 155);
    assert!(
        registry
            .finish(&f.owner, &f.current, &proof.encode().unwrap(), f.now)
            .is_err()
    );
    assert!(registry.live.is_empty());
    assert!(f.owner.delivery_ledger().unwrap().consumers.is_empty());
}
#[test]
fn data_registry_failed_consumer_barrier_exposes_no_ready_or_live_data_session() {
    use crate::store::test_fault::{Point, with_fault};
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        let (mut f, ingress, _receiver) = fixture();
        let mut registry =
            HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
        let proof = client(&f, &mut registry, PRODUCT_DATA_V1, 156);
        let body = proof.encode().unwrap();
        let path = f.owner.root().to_owned();
        let result = with_fault(&path, point, || {
            registry.finish_data_at(
                &mut f.owner,
                &f.current,
                &ingress.host_bytes(),
                &body,
                f.now,
            )
        });
        assert!(result.is_err());
        assert!(registry.live.is_empty());
        assert!(f.current.read().is_err());
        assert!(!f.owner.permits_publication());
    }
}
#[test]
fn data_registry_policy_rejects_unknown_masks_and_unattached_owner_has_no_ready() {
    let mut f = Fixture::new();
    assert!(HttpSessions::new_for_policy(&f.owner, &f.current, f.now, 1 << 63).is_err());
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9443", PRODUCT_DATA_V1, at(f.now))
        })
        .unwrap()
        .unwrap();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
    let proof = client(&f, &mut registry, PRODUCT_DATA_V1, 157);
    assert!(
        registry
            .finish_data_at(
                &mut f.owner,
                &f.current,
                &ingress.host_bytes(),
                &proof.encode().unwrap(),
                f.now
            )
            .is_err()
    );
    assert!(registry.live.is_empty());
    assert!(f.owner.delivery_ledger().unwrap().consumers.is_empty());
}

#[test]
fn data_registry_never_negotiates_optional_bits_absent_from_persisted_descriptor() {
    let f = Fixture::new();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
    let request = postcard::to_stdvec(&(
        3_u16,
        f.owner.identity.native_cert.device_public_key,
        [158_u8; 32],
        PRODUCT_DATA_V1,
        0_u64,
    ))
    .unwrap();
    let server = auth::ServerProof::decode(
        &registry
            .challenge(&f.owner, &f.current, &request, f.now)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        server.transcript.negotiated_capabilities, 0,
        "installed policy cannot broaden persisted advertisement"
    );
}

#[test]
fn data_registry_expiry_after_consumer_commit_does_not_publish_ready() {
    let (mut f, ingress, _receiver) = fixture();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
    let proof = client(&f, &mut registry, PRODUCT_DATA_V1, 159);
    let device = f.owner.identity.native_cert.device_public_key;
    let mut times = [f.now, f.now, f.now + 15 * 60 * 1_000_000_000].into_iter();
    let result = registry.finish_data_clock(
        &mut f.owner,
        &f.current,
        &ingress.host_bytes(),
        &proof.encode().unwrap(),
        || times.next().unwrap(),
    );
    assert!(result.is_err());
    assert!(registry.live.is_empty());
    assert!(
        f.owner
            .delivery_ledger()
            .unwrap()
            .consumers
            .contains_key(&device)
    );
    assert_eq!(
        f.current.read().unwrap().generation(),
        f.owner.committed_generation()
    );
}

#[test]
fn parent_installation_descriptor_changes_require_real_owner_scope() {
    use crate::owner::resources::OwnerOperation;
    let (mut f, ingress, _receiver) = fixture();
    let before = f.owner.committed_generation();
    let unscoped = f
        .current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9443", PRODUCT_DATA_V1, at(f.now))
        })
        .unwrap();
    assert!(
        unscoped.is_err(),
        "attached owner cannot allocate/sign a new descriptor outside an owner scope"
    );
    assert_eq!(f.owner.committed_generation(), before);
    let mut scope = f
        .owner
        .prepare_owner_scope(&ingress.host_bytes(), OwnerOperation::Descriptor)
        .unwrap();
    f.owner
        .with_owner_scope(&mut scope, &f.current, |owner| {
            owner.install_delivery_transport("https://localhost:9444", PRODUCT_DATA_V1, at(f.now))
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        f.current.read().unwrap().generation(),
        f.owner.committed_generation()
    );
    assert!(f.owner.committed_generation() > before);
}

#[test]
fn private_session_slots_are_bounded_and_old_output_does_not_recycle_replacement_credit() {
    let (mut f, ingress, _receiver) = fixture();
    let budget = ingress.host_bytes();
    let mut registry =
        HttpSessions::new_for_policy(&f.owner, &f.current, f.now, PRODUCT_DATA_V1).unwrap();
    let proof = client(&f, &mut registry, PRODUCT_DATA_V1, 170);
    let first = finish(&mut f, &mut registry, &budget, &proof);
    let id = ready_id(&first);
    let mut held = (0..8)
        .map(|_| registry.reserve_private_slot(id).unwrap())
        .collect::<Vec<_>>();
    assert!(registry.reserve_private_slot(id).is_err());
    drop(held.pop());
    held.push(registry.reserve_private_slot(id).unwrap());
    let proof = client(&f, &mut registry, PRODUCT_DATA_V1, 171);
    let next = finish(&mut f, &mut registry, &budget, &proof);
    assert_eq!(next.retired, vec![id]);
    assert!(registry.reserve_private_slot(id).is_err());
    let next_id = ready_id(&next);
    let next_held = (0..8)
        .map(|_| registry.reserve_private_slot(next_id).unwrap())
        .collect::<Vec<_>>();
    drop(held);
    assert!(
        registry.reserve_private_slot(next_id).is_err(),
        "old output drain cannot release new session capacity"
    );
    drop(next_held);
    assert!(registry.reserve_private_slot(next_id).is_ok());
}
