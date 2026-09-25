use super::*;
use crate::{
    SessionOwner, delivery_http::tests::Fixture, owner::delivery::NativeDeliveryAuthority,
};
use tilo_relay::delivery_v3::cursor::CursorToken;
use tilo_user_identity::RevocationList;

fn authenticated_session(
    owner: &SessionOwner,
    committed: &crate::committed_authority::CommittedAuthority,
) -> tilo_relay::delivery_v3::auth::VerifiedSession {
    use tilo_relay::delivery_v3::auth::*;
    let current = committed.read().unwrap();
    let authority = NativeDeliveryAuthority {
        view: current.view(),
        workspace: owner.workspace_id(),
        host: &owner.identity.host_cert,
    };
    let binding = Binding {
        workspace: owner.workspace_id(),
        workspace_root: owner.identity.native_cert.user_public_key,
        device: owner.identity.native_cert.device_public_key,
        device_root: owner.identity.native_cert.user_public_key,
        host: owner.identity.host_cert.device_public_key,
        host_root: owner.identity.host_cert.user_public_key,
        origin: "https://localhost:9443".into(),
        descriptor_generation: 1,
        descriptor_digest: [7; 32],
        server_boot_id: [8; 16],
        carrier: Carrier::Https,
        requested_capabilities: 1,
        mandatory_capabilities: 1,
    };
    let revocations = RevocationList::new();
    let context = VerificationContext {
        binding: &binding,
        capabilities: CapabilityPolicy {
            supported: 1,
            allowed: 1,
        },
        credentials: Credentials {
            device: &owner.identity.native_cert,
            host: &owner.identity.host_cert,
            device_revocations: &revocations,
            host_revocations: &revocations,
        },
        authority: &authority,
        now_ns: tilo_relay::clock::now_ns(),
    };
    let (challenge, proof) = Challenge::issue(&context, [9; 32], &owner.identity.host).unwrap();
    let client = verify_server(
        &ClientExpectations {
            scope: ClientScope::from(&binding),
            capabilities: context.capabilities,
            credentials: context.credentials,
            authority: &authority,
            now_ns: context.now_ns,
        },
        [9; 32],
        &proof,
    )
    .unwrap()
    .client_proof(&owner.identity.native)
    .unwrap();
    challenge.finish(&client, &context).unwrap()
}

fn fixture() -> (Fixture, VerifiedSession) {
    let f = Fixture::new();
    let session = authenticated_session(&f.owner, &f.current);
    (f, session)
}
fn basis<'a>(f: &'a Fixture, session: &'a VerifiedSession) -> DeliveryPolicyBasis<'a> {
    DeliveryPolicyBasis::inspect_unqualified(
        f.owner.peer(),
        f.owner.workspace_id(),
        f.owner.host_certificate(),
        session,
        SystemTime::now(),
    )
    .unwrap()
}
fn register(f: &mut Fixture, session: &VerifiedSession) -> CursorToken {
    f.current
        .with_owner_view(&mut f.owner, |view, owner| {
            let source = owner.delivery_source(view, session, tilo_relay::clock::now_ns())?;
            owner.register_delivery_consumer(&source)
        })
        .unwrap()
        .unwrap();
    let guard = f.current.read().unwrap();
    let source = f
        .owner
        .delivery_source(guard.view(), session, tilo_relay::clock::now_ns())
        .unwrap();
    let (sequence, entry) = f
        .owner
        .delivery_ledger()
        .unwrap()
        .replay
        .last_key_value()
        .unwrap();
    f.owner
        .issue_delivery_cursor(
            &source,
            CommitRef {
                sequence: *sequence,
                digest: entry.digest,
            },
        )
        .unwrap()
}
#[test]
fn neutral_policy_and_consumer_plan_preserve_native_bytes_and_identity() {
    let (mut f, session) = fixture();
    let initial = basis(&f, &session);
    let mut ledger = f.owner.delivery_ledger().unwrap().clone();
    let plan = initial.consumer_plan(&ledger, [11; 16]).unwrap();
    let ConsumerPlan::Upsert(consumer) = plan else {
        panic!("missing new consumer plan")
    };
    assert_eq!(consumer.id, [11; 16]);
    assert!(consumer.acknowledged.is_none());
    ledger.consumers.insert(consumer.device, consumer.clone());
    assert_eq!(
        initial.consumer_plan(&ledger, [0; 16]).unwrap(),
        ConsumerPlan::Existing(consumer.id)
    );
    ledger
        .consumers
        .get_mut(&consumer.device)
        .unwrap()
        .authorization_view = [99; 32];
    assert!(
        matches!(initial.consumer_plan(&ledger,[0;16]).unwrap(),ConsumerPlan::Upsert(ref c) if c.id==consumer.id && c.authorization_view==initial.policy_digest() && c.acknowledged.is_none())
    );
    ledger.consumers.get_mut(&consumer.device).unwrap().root = [77; 32];
    assert_eq!(
        initial.consumer_plan(&ledger, [11; 16]),
        Err(PlanningFailure::CurrentDenied)
    );
    drop(initial);
    let native = register(&mut f, &session);
    let signed = native
        .authenticate(&f.owner.identity.host.verifying_key())
        .unwrap();
    let basis = basis(&f, &session);
    assert_eq!(basis.policy_digest(), signed.issue().authorization_view);
    let issue = basis
        .cursor_issue(
            f.owner.delivery_ledger().unwrap(),
            signed.committed().clone(),
        )
        .unwrap();
    assert_eq!(&issue, signed.issue());
    assert_eq!(
        CursorToken::issue(issue, &f.owner.identity.host)
            .unwrap()
            .as_bytes(),
        native.as_bytes()
    );
}
#[test]
fn neutral_cursor_order_and_ack_recheck_actual_ledger() {
    let (mut f, session) = fixture();
    let token = register(&mut f, &session);
    let signed = token
        .authenticate(&f.owner.identity.host.verifying_key())
        .unwrap();
    let basis = basis(&f, &session);
    let mut ledger = f.owner.delivery_ledger().unwrap().clone();
    let cut = basis.cursor_cut(&ledger, &signed).unwrap();
    assert_eq!(
        basis.acknowledge(&ledger, &cut).unwrap(),
        AckPlan::Advance(signed.committed().clone())
    );
    ledger
        .consumers
        .get_mut(&session.device_certificate().device_public_key)
        .unwrap()
        .acknowledged = Some(signed.committed().clone());
    assert_eq!(
        basis.acknowledge(&ledger, &cut).unwrap(),
        AckPlan::Unchanged
    );
    ledger.store_generation = [41; 16];
    assert!(matches!(
        basis.cursor_cut(&ledger, &signed),
        Err(PlanningFailure::Reset(ResetReason::StoreRotated))
    ));
    assert_eq!(
        basis.acknowledge(&ledger, &cut),
        Err(PlanningFailure::Reset(ResetReason::StoreRotated))
    );
    let mut foreign = signed.issue().clone();
    foreign.consumer = [77; 16];
    foreign.committed.sequence = u64::MAX;
    let foreign = CursorToken::issue(foreign, &f.owner.identity.host)
        .unwrap()
        .authenticate(&f.owner.identity.host.verifying_key())
        .unwrap();
    assert!(matches!(
        basis.cursor_cut(&ledger, &foreign),
        Err(PlanningFailure::CurrentDenied)
    ));
    ledger.store_generation = signed.issue().store_generation;
    let c = ledger
        .consumers
        .get_mut(&session.device_certificate().device_public_key)
        .unwrap();
    c.authorization_view = [55; 32];
    assert!(matches!(
        basis.cursor_cut(&ledger, &signed),
        Err(PlanningFailure::Reset(ResetReason::AuthorizationChanged))
    ));
    assert_eq!(
        basis.acknowledge(&ledger, &cut),
        Err(PlanningFailure::Reset(ResetReason::AuthorizationChanged))
    );
    ledger
        .consumers
        .get_mut(&session.device_certificate().device_public_key)
        .unwrap()
        .authorization_view = basis.policy_digest();
    ledger.replay.remove(&signed.committed().sequence);
    assert_eq!(
        basis.acknowledge(&ledger, &cut),
        Err(PlanningFailure::Reset(ResetReason::CursorUnknown))
    );
}
#[test]
fn neutral_replay_borrows_original_backing_and_missing_payload_denies() {
    let (mut f, session) = fixture();
    let token = register(&mut f, &session);
    let signed = token
        .authenticate(&f.owner.identity.host.verifying_key())
        .unwrap();
    let basis = basis(&f, &session);
    let mut ledger = f.owner.delivery_ledger().unwrap().clone();
    let mut pool = PublicationPool::new(1024).unwrap();
    let digest = pool.intern(b"original retained bytes").unwrap();
    let entry = ledger.replay.get_mut(&signed.committed().sequence).unwrap();
    let mut reference = entry.publications.first().unwrap().clone();
    reference.payload = digest;
    entry.publications = vec![reference];
    let selection = basis.replay(&ledger, signed.committed()).unwrap();
    let item = selection.publications(&pool).next().unwrap().unwrap();
    assert_eq!(item.bytes.as_ptr(), pool.resolve(&digest).unwrap().as_ptr());
    let empty = PublicationPool::new(1024).unwrap();
    assert_eq!(
        selection.publications(&empty).next().unwrap(),
        Err(PlanningFailure::Unavailable)
    );
}

#[test]
fn neutral_consumer_capacity_and_candidate_collisions_are_closed() {
    let (f, session) = fixture();
    let basis = basis(&f, &session);
    let mut ledger = f.owner.delivery_ledger().unwrap().clone();
    assert_eq!(
        basis.consumer_plan(&ledger, [0; 16]),
        Err(PlanningFailure::Unavailable)
    );
    // Inert ledger metadata exercises only planning, not real enrollment or authority.
    for i in 1..=MAX_CONSUMERS {
        let mut device = [0; 32];
        device[..8].copy_from_slice(&(i as u64).to_be_bytes());
        let mut id = [0; 16];
        id[..8].copy_from_slice(&(i as u64).to_be_bytes());
        ledger.consumers.insert(
            device,
            ConsumerState {
                id,
                device,
                root: [1; 32],
                authorization_view: [2; 32],
                acknowledged: None,
            },
        );
    }
    assert_eq!(
        basis.consumer_plan(&ledger, [11; 16]),
        Err(PlanningFailure::Capacity(PlanningCapacity::Consumers))
    );
    let existing = ledger.consumers.values().next().unwrap().clone();
    ledger.consumers.clear();
    ledger.consumers.insert(existing.device, existing.clone());
    assert_eq!(
        basis.consumer_plan(&ledger, existing.id),
        Err(PlanningFailure::Unavailable)
    );
}

mod policy_parity;
