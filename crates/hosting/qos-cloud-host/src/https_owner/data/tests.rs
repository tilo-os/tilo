use super::super::{CloudStoreSource, Current};
use super::*;
use crate::storage::test_support::with_save_failure;
use atomic_file::test_support::Checkpoint;
use ed25519_dalek::SigningKey;
use qos_relay::delivery_v3::{auth, state::ResetReason};
use qos_session_peer::ingress::{HostBytes, PreparedTransport};
use std::{os::unix::fs::PermissionsExt, path::Path};

// Component fixture only: real selected Store and original ingress pool, with
// explicitly unqualified test entry. No installed bounded-owner witness exists.
pub(in crate::https_owner) struct Fixture {
    pub(in crate::https_owner) root: tempfile::TempDir,
    _transport_root: tempfile::TempDir,
    _transport: PreparedTransport,
    pub(in crate::https_owner) pool: HostBytes,
    pub(in crate::https_owner) source: CloudStoreSource,
    pub(in crate::https_owner) session: auth::VerifiedSession,
    pub(in crate::https_owner) host: SigningKey,
    pub(in crate::https_owner) device: [u8; 32],
}
impl Fixture {
    pub(in crate::https_owner) fn host_work(&self) -> qos_session_peer::ingress::HostWorkInventory {
        self._transport.host_work().unwrap()
    }
}
pub(in crate::https_owner) async fn prepared_transport() -> (tempfile::TempDir, PreparedTransport) {
    let transport_root = tempfile::tempdir().unwrap();
    let cert = transport_root.path().join("cert.der");
    let key = transport_root.path().join("key.der");
    let assets = transport_root.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(&cert, hex::decode(TLS_CERT).unwrap()).unwrap();
    std::fs::write(&key, hex::decode(TLS_KEY).unwrap()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let config = transport_root.path().join("operator.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "delivery_version": 3, "origin": format!("https://127.0.0.1:{port}"),
            "certificate_der": cert, "key_der": key, "assets": assets
        }))
        .unwrap(),
    )
    .unwrap();
    let transport = PreparedTransport::load_explicit(&config).await.unwrap();
    (transport_root, transport)
}
pub(in crate::https_owner) async fn fixture() -> Fixture {
    let (transport_root, transport) = prepared_transport().await;
    let pool = transport.host_bytes().unwrap();
    let (root, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let host = store.key();
    let source = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    let session = match source.with_current(|turn| authenticate(turn, &host, transport.origin())) {
        Current::Current(session) => session,
        _ => panic!("actual fresh Store turn required"),
    };
    Fixture {
        root,
        _transport_root: transport_root,
        _transport: transport,
        pool,
        source,
        session,
        host,
        device,
    }
}
pub(in crate::https_owner) fn authenticate(
    turn: &CloudStoreTurn,
    key: &SigningKey,
    origin: &str,
) -> auth::VerifiedSession {
    authenticate_with_capabilities(turn, key, origin, 1)
}
pub(in crate::https_owner) fn authenticate_with_capabilities(
    turn: &CloudStoreTurn,
    key: &SigningKey,
    origin: &str,
    capabilities: u64,
) -> auth::VerifiedSession {
    let client_key = SigningKey::from_bytes(&[8; 32]);
    let certificates = turn.peer().certificates();
    let device = certificates
        .iter()
        .find(|c| c.device_public_key == client_key.verifying_key().to_bytes())
        .unwrap();
    let host = turn.peer().authority();
    let binding = auth::Binding {
        workspace: turn.workspace(),
        workspace_root: host.user_public_key,
        device: device.device_public_key,
        device_root: device.user_public_key,
        host: host.device_public_key,
        host_root: host.user_public_key,
        origin: origin.into(),
        descriptor_generation: 1,
        descriptor_digest: [7; 32],
        server_boot_id: [8; 16],
        carrier: auth::Carrier::Https,
        requested_capabilities: capabilities,
        mandatory_capabilities: capabilities,
    };
    let revocations = qos_user_identity::RevocationList::new();
    let context = auth::VerificationContext {
        binding: &binding,
        capabilities: auth::CapabilityPolicy {
            supported: capabilities,
            allowed: capabilities,
        },
        credentials: auth::Credentials {
            device,
            host,
            device_revocations: &revocations,
            host_revocations: &revocations,
        },
        authority: turn,
        now_ns: turn.fresh_now_ns().unwrap(),
    };
    let (challenge, server) = auth::Challenge::issue(&context, [9; 32], key).unwrap();
    let client = auth::verify_server(
        &auth::ClientExpectations {
            scope: auth::ClientScope::from(&binding),
            capabilities: context.capabilities,
            credentials: context.credentials,
            authority: turn,
            now_ns: context.now_ns,
        },
        [9; 32],
        &server,
    )
    .unwrap()
    .client_proof(&client_key)
    .unwrap();
    challenge.finish(&client, &context).unwrap()
}
fn run<R>(
    f: &Fixture,
    operation: DeliveryOperation,
    action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
) -> Result<R> {
    match f
        .source
        .with_delivery_turn_unqualified_test(&f.pool, operation, action)
    {
        Current::Current(result) => result,
        Current::Busy => panic!("fixture Store unexpectedly busy"),
        Current::Denied => panic!("fixture Store unexpectedly denied"),
    }
}
pub(in crate::https_owner) fn register(f: &Fixture) -> ConsumerId {
    run(f, DeliveryOperation::RegisterConsumer, |turn| {
        turn.register_delivery_consumer(&f.session)
    })
    .unwrap()
}
fn latest(f: &Fixture) -> CommitRef {
    run(f, DeliveryOperation::Replay, |turn| {
        let (sequence, entry) = turn.delivery_ledger().replay.last_key_value().unwrap();
        Ok(CommitRef {
            sequence: *sequence,
            digest: entry.digest,
        })
    })
    .unwrap()
}
fn issue(f: &Fixture, cut: CommitRef) -> CursorToken {
    run(f, DeliveryOperation::Cursor, |turn| {
        turn.issue_delivery_cursor(&f.session, cut)
    })
    .unwrap()
}
fn state(f: &Fixture) -> (u64, qos_relay::delivery_v3::commit::DeliveryLedgerV1) {
    let store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
    (
        store.state.generation,
        store.state.delivery.as_ref().unwrap().ledger().clone(),
    )
}
fn document(root: &Path, device: [u8; 32]) {
    document_named(root, device, "cloud-data-next");
}
fn document_named(root: &Path, device: [u8; 32], invocation: &str) {
    let mut store = crate::Store::open(root, "agent-admission").unwrap();
    store
        .action(
            device,
            &crate::ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: invocation.into(),
                operation: crate::NativeOperation::WriterInsert {
                    text: "actual indexed suffix".into(),
                },
            },
            qos_relay::clock::now_ns(),
        )
        .unwrap();
}

#[tokio::test]
async fn cloud_data_registration_is_durable_before_return_and_reopen_is_idempotent() {
    let f = fixture().await;
    let before = state(&f).0;
    let id = register(&f);
    let (generation, ledger) = state(&f);
    assert_eq!(generation, before + 1);
    assert_eq!(ledger.consumers[&f.device].id, id);
    assert_eq!(
        ledger.consumers[&f.device].root,
        f.session.device_certificate().user_public_key
    );
    assert!(ledger.consumers[&f.device].acknowledged.is_none());
    let bytes = std::fs::read(f.root.path().join("state.json")).unwrap();
    assert_eq!(register(&f), id);
    assert_eq!(
        std::fs::read(f.root.path().join("state.json")).unwrap(),
        bytes
    );
    assert_eq!(state(&f).0, generation);
}

#[tokio::test]
async fn cloud_data_signed_cursor_and_ack_use_exact_current_store_and_barrier() {
    let f = fixture().await;
    register(&f);
    let cut = latest(&f);
    let cursor = issue(&f, cut.clone());
    let signed = cursor.authenticate(&f.host.verifying_key()).unwrap();
    assert_eq!(signed.committed(), &cut);
    assert_eq!(signed.issue().consumer_device, f.device);
    run(&f, DeliveryOperation::Cursor, |turn| {
        assert_eq!(
            turn.verify_delivery_cursor(&f.session, &cursor)?
                .committed(),
            &cut
        );
        Ok(())
    })
    .unwrap();
    let generation = state(&f).0;
    run(&f, DeliveryOperation::Acknowledge, |turn| {
        turn.acknowledge_delivery_cursor(&f.session, &cursor)
    })
    .unwrap();
    let (after, ledger) = state(&f);
    assert_eq!(after, generation + 1);
    assert_eq!(
        ledger.consumers[&f.device].acknowledged.as_ref(),
        Some(&cut)
    );
    run(&f, DeliveryOperation::Acknowledge, |turn| {
        turn.acknowledge_delivery_cursor(&f.session, &cursor)
    })
    .unwrap();
    assert_eq!(state(&f).0, after, "identical ACK is not a new Store write");
}

#[tokio::test]
async fn cloud_data_cursor_rejects_wrong_signature_and_preserves_reset_precedence() {
    let f = fixture().await;
    register(&f);
    let token = issue(&f, latest(&f));
    let claims = token
        .authenticate(&f.host.verifying_key())
        .unwrap()
        .issue()
        .clone();
    let wrong_key = CursorToken::issue(claims.clone(), &SigningKey::from_bytes(&[91; 32])).unwrap();
    assert!(
        run(&f, DeliveryOperation::Cursor, |turn| turn
            .verify_delivery_cursor(&f.session, &wrong_key)
            .map(|_| ()))
        .is_err()
    );
    for (mutated, expected) in [
        (0, CloudDeliveryError::Denied),
        (1, CloudDeliveryError::Reset(ResetReason::StoreRotated)),
        (
            2,
            CloudDeliveryError::Reset(ResetReason::AuthorizationChanged),
        ),
        (3, CloudDeliveryError::Reset(ResetReason::CursorAhead)),
        (4, CloudDeliveryError::Reset(ResetReason::CommitMismatch)),
    ] {
        let mut foreign = claims.clone();
        match mutated {
            0 => {
                foreign.consumer = [71; 16];
                foreign.store_generation = [72; 16];
                foreign.committed.sequence = u64::MAX;
            }
            1 => {
                foreign.store_generation = [72; 16];
                foreign.authorization_view = [73; 32];
                foreign.committed.sequence = u64::MAX;
            }
            2 => {
                foreign.authorization_view = [73; 32];
                foreign.committed.sequence = u64::MAX;
            }
            3 => foreign.committed.sequence = u64::MAX,
            4 => foreign.committed.digest = [74; 32],
            _ => unreachable!(),
        }
        let changed = CursorToken::issue(foreign, &f.host).unwrap();
        assert_eq!(
            run(&f, DeliveryOperation::Cursor, |turn| turn
                .verify_delivery_cursor(&f.session, &changed)
                .map(|_| ())),
            Err(expected)
        );
    }
    assert!(state(&f).1.consumers[&f.device].acknowledged.is_none());
}

#[tokio::test]
async fn cloud_data_replay_borrows_real_backing_and_moves_strictly_after_signed_cut() {
    let f = fixture().await;
    register(&f);
    let initial = latest(&f);
    let token = issue(&f, initial.clone());
    run(&f, DeliveryOperation::Replay, |turn| {
        let first = turn.next_delivery_replay(&f.session, None)?.unwrap();
        assert_eq!(first.committed(), &initial);
        for (reference, publication) in first
            .entry()
            .publications
            .iter()
            .zip(first.publications(turn.publication_pool()))
        {
            let publication = publication.unwrap();
            let actual = turn.publication_pool().resolve(&reference.payload).unwrap();
            assert_eq!(publication.bytes.as_ptr(), actual.as_ptr());
            assert_eq!(publication.bytes, actual);
            assert_eq!(publication.kind, reference.kind);
        }
        assert!(
            turn.next_delivery_replay(&f.session, Some(&token))?
                .is_none()
        );
        Ok(())
    })
    .unwrap();
    document(f.root.path(), f.device);
    let suffix = latest(&f);
    assert!(suffix.sequence > initial.sequence);
    let before = state(&f).0;
    run(&f, DeliveryOperation::Replay, |turn| {
        let next = turn
            .next_delivery_replay(&f.session, Some(&token))?
            .unwrap();
        assert_eq!(next.committed(), &suffix);
        let exact = turn.delivery_replay(&f.session, &suffix)?;
        assert_eq!(next.entry().publications, exact.entry().publications);
        Ok(())
    })
    .unwrap();
    let (after, ledger) = state(&f);
    assert_eq!(after, before);
    assert!(ledger.consumers[&f.device].acknowledged.is_none());
}

#[tokio::test]
async fn cloud_data_registration_barrier_failure_never_returns_success_and_reopen_resolves() {
    for (checkpoint, published) in [
        (Checkpoint::BeforePublish, false),
        (Checkpoint::BeforeCommit, true),
    ] {
        let f = fixture().await;
        let before = state(&f).0;
        let result = with_save_failure(f.root.path(), checkpoint, || {
            run(&f, DeliveryOperation::RegisterConsumer, |turn| {
                turn.register_delivery_consumer(&f.session)
            })
        });
        assert!(matches!(result, Err(CloudDeliveryError::OwnerUncertain(_))));
        let (generation, ledger) = state(&f);
        assert_eq!(generation, before + u64::from(published));
        assert_eq!(ledger.consumers.contains_key(&f.device), published);
    }
}

#[tokio::test]
async fn cloud_data_ack_barrier_failure_has_no_optimistic_success_or_second_mutation() {
    for (checkpoint, published) in [
        (Checkpoint::BeforePublish, false),
        (Checkpoint::BeforeCommit, true),
    ] {
        let f = fixture().await;
        register(&f);
        let cut = latest(&f);
        let token = issue(&f, cut.clone());
        let before = state(&f).0;
        let result = with_save_failure(f.root.path(), checkpoint, || {
            run(&f, DeliveryOperation::Acknowledge, |turn| {
                turn.acknowledge_delivery_cursor(&f.session, &token)
            })
        });
        assert!(matches!(result, Err(CloudDeliveryError::OwnerUncertain(_))));
        let (generation, ledger) = state(&f);
        assert_eq!(generation, before + u64::from(published));
        assert_eq!(
            ledger.consumers[&f.device].acknowledged.as_ref(),
            published.then_some(&cut)
        );
    }
}

#[tokio::test]
async fn cloud_data_original_session_cannot_authorize_another_actual_store() {
    let first = fixture().await;
    let second = fixture().await;
    register(&first);
    register(&second);
    let before = state(&second).0;
    assert!(matches!(
        run(&second, DeliveryOperation::RegisterConsumer, |turn| turn
            .register_delivery_consumer(&first.session)),
        Err(CloudDeliveryError::Denied)
    ));
    assert!(matches!(
        run(&second, DeliveryOperation::Replay, |turn| turn
            .next_delivery_replay(&first.session, None)
            .map(|_| ())),
        Err(CloudDeliveryError::Denied)
    ));
    assert_eq!(state(&second).0, before);
}

#[tokio::test]
async fn cloud_data_private_only_session_cannot_use_data_operation() {
    let f = fixture().await;
    let private = match f.source.with_current(|turn| {
        authenticate_with_capabilities(turn, &f.host, &f.session.binding().origin, 2)
    }) {
        Current::Current(session) => session,
        _ => panic!("actual current Store required"),
    };
    let before = state(&f).0;
    assert!(matches!(
        run(&f, DeliveryOperation::RegisterConsumer, |turn| turn
            .register_delivery_consumer(&private)),
        Err(CloudDeliveryError::Denied)
    ));
    assert_eq!(state(&f).0, before);
    register(&f);
    let cut = latest(&f);
    let token = issue(&f, cut.clone());
    assert!(matches!(
        run(&f, DeliveryOperation::Cursor, |turn| turn
            .issue_delivery_cursor(&private, cut.clone())),
        Err(CloudDeliveryError::Denied)
    ));
    assert!(matches!(
        run(&f, DeliveryOperation::Cursor, |turn| turn
            .verify_delivery_cursor(&private, &token)
            .map(|_| ())),
        Err(CloudDeliveryError::Denied)
    ));
    assert!(matches!(
        run(&f, DeliveryOperation::Acknowledge, |turn| turn
            .acknowledge_delivery_cursor(&private, &token)),
        Err(CloudDeliveryError::Denied)
    ));
    assert!(matches!(
        run(&f, DeliveryOperation::Replay, |turn| turn
            .delivery_replay(&private, &cut)
            .map(|_| ())),
        Err(CloudDeliveryError::Denied)
    ));
    assert!(matches!(
        run(&f, DeliveryOperation::Replay, |turn| turn
            .next_delivery_replay(&private, None)
            .map(|_| ())),
        Err(CloudDeliveryError::Denied)
    ));
}

#[tokio::test]
async fn cloud_data_closed_operations_and_capacity_refusal_cannot_mutate() {
    let f = fixture().await;
    register(&f);
    let cut = latest(&f);
    let token = issue(&f, cut.clone());
    let before = std::fs::read(f.root.path().join("state.json")).unwrap();
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(turn.register_delivery_consumer(&f.session).is_err());
        assert!(turn.issue_delivery_cursor(&f.session, cut).is_err());
        assert!(turn.verify_delivery_cursor(&f.session, &token).is_err());
        assert!(
            turn.acknowledge_delivery_cursor(&f.session, &token)
                .is_err()
        );
        Ok(())
    })
    .unwrap();
    // Exhaust the original pool: the reservation precedes the Store lock and
    // decode, so the production callback is never invoked without credit.
    let held = f
        .pool
        .try_acquire_control(f.pool.available_bytes())
        .unwrap();
    assert!(matches!(
        f.source.with_delivery_turn(
            &f.pool,
            DeliveryOperation::RegisterConsumer,
            |_| -> Result<()> { panic!("unreserved turn must not invoke production callback") }
        ),
        Current::Current(Err(CloudDeliveryError::Capacity))
    ));
    assert_eq!(
        std::fs::read(f.root.path().join("state.json")).unwrap(),
        before
    );
    drop(held);
    let available = f.pool.available_bytes();
    // A reserved production turn holds its credit exactly for the callback.
    let observed = match f.source.with_delivery_turn(
        &f.pool,
        DeliveryOperation::Replay,
        |turn| -> Result<usize> {
            turn.next_delivery_replay(&f.session, None)?;
            Ok(f.pool.available_bytes())
        },
    ) {
        Current::Current(result) => result.unwrap(),
        _ => panic!("actual Store turn required"),
    };
    assert!(observed < available, "turn retained a real reservation");
    // Only the charged retained decode outlives the turn; releasing it
    // restores the pool exactly.
    assert!(
        f.pool.available_bytes() < available,
        "retained decode is charged"
    );
    f.source.release_retained();
    assert_eq!(
        f.pool.available_bytes(),
        available,
        "credit released after turn"
    );
    assert!(matches!(
        f.source
            .with_delivery_turn(&f.pool, DeliveryOperation::State, |_| -> Result<()> {
                panic!("state requires its input-length reservation")
            }),
        Current::Current(Err(CloudDeliveryError::InvalidInput))
    ));
}

#[tokio::test]
async fn cloud_data_production_turn_commits_consumer_and_ack_durably() {
    let f = fixture().await;
    let consumer =
        match f
            .source
            .with_delivery_turn(&f.pool, DeliveryOperation::RegisterConsumer, |turn| {
                turn.register_delivery_consumer(&f.session)
            }) {
            Current::Current(result) => result.unwrap(),
            _ => panic!("actual Store turn required"),
        };
    let cut = latest(&f);
    let token = match f
        .source
        .with_delivery_turn(&f.pool, DeliveryOperation::Cursor, |turn| {
            turn.issue_delivery_cursor(&f.session, cut.clone())
        }) {
        Current::Current(result) => result.unwrap(),
        _ => panic!("actual Store turn required"),
    };
    match f
        .source
        .with_delivery_turn(&f.pool, DeliveryOperation::Acknowledge, |turn| {
            turn.acknowledge_delivery_cursor(&f.session, &token)
        }) {
        Current::Current(result) => result.unwrap(),
        _ => panic!("actual Store turn required"),
    }
    // Reopen the actual Store: both barriers were durably published.
    let (_, ledger) = state(&f);
    let stored = &ledger.consumers[&f.device];
    assert_eq!(stored.id, consumer);
    assert_eq!(stored.acknowledged.as_ref(), Some(&cut));
}

#[tokio::test]
async fn cloud_data_persisted_history_gap_is_denied_before_callback_without_rewrite() {
    let f = fixture().await;
    register(&f);
    let original = latest(&f);
    let token = issue(&f, original.clone());
    run(&f, DeliveryOperation::Cursor, |turn| {
        turn.verify_delivery_cursor(&f.session, &token).map(|_| ())
    })
    .unwrap();
    document(f.root.path(), f.device);
    let omitted = latest(&f);
    document_named(f.root.path(), f.device, "cloud-data-after-gap");
    let mut ledger = state(&f).1;
    assert!(ledger.replay.len() >= 3);
    assert!(ledger.replay.remove(&omitted.sequence).is_some());
    assert!(ledger.replay.contains_key(&original.sequence));
    assert!(
        ledger.validate().is_err(),
        "current schema forbids retained holes"
    );
    let path = f.root.path().join("state.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // Intentionally corrupt only persisted ledger bytes. No fake usable turn or
    // unchecked publication is admitted; actual Store open must reject this.
    json["delivery"]["ledger"] =
        serde_json::to_value(postcard::to_stdvec(&ledger).unwrap()).unwrap();
    let corrupt = serde_json::to_vec(&json).unwrap();
    std::fs::write(&path, &corrupt).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            f.source.with_delivery_turn_unqualified_test(
                &f.pool,
                DeliveryOperation::Replay,
                |turn| {
                    let _ = turn.next_delivery_replay(&f.session, Some(&token));
                    panic!("corrupt actual ledger must be denied before callback");
                    #[allow(unreachable_code, reason = "test/stub closure keeps its declared return type after a diverging call")]
                    Ok(())
                }
            ),
            Current::Denied
        ));
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }
}

// Public localhost fixture identity, never production credentials.
const TLS_CERT: &str = "3082017d30820123a00302010202147ac297cf4de83a0525b286d5c7a90672c8fac5da300a06082a8648ce3d04030230143112301006035504030c096c6f63616c686f7374301e170d3236303932323039333131355a170d3336303931393039333131355a30143112301006035504030c096c6f63616c686f73743059301306072a8648ce3d020106082a8648ce3d0301070342000457ce6d80dfc27f71bc5c546417cf92a1621aa614a707057f2447ea54ede7034f0976988f9774dd07fa000cc12de7f304af8f32cb0ca315166245077b1b314655a3533051301d0603551d0e04160414d967e7ab5d6613dd0e53a648ff9b5d90657566f8301f0603551d23041830168014d967e7ab5d6613dd0e53a648ff9b5d90657566f8300f0603551d130101ff040530030101ff300a06082a8648ce3d0403020348003045022100985d69619df31e351a28d29da6c86ef8fb7d33ad87da5302228479f1614b4cf502203285805ff0e92e18155aab56f0229536582cee0c38653fca44e50f61cf7959f5";
const TLS_KEY: &str = "308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b020101042058815db9251f1c7a1bbb3ccfd7ea7d5c0b0b680eac16f8bb2b09b60e06cc771da1440342000457ce6d80dfc27f71bc5c546417cf92a1621aa614a707057f2447ea54ede7034f0976988f9774dd07fa000cc12de7f304af8f32cb0ca315166245077b1b314655";

/// Consecutive turns over an unchanged aggregate reuse the retained validated
/// decode; any replacement of `state.json` (another component's commit or
/// this owner's own) forces a full read, reconciliation and validation.
#[tokio::test]
async fn cloud_retained_decode_is_reused_only_for_the_identical_aggregate() {
    let f = fixture().await;
    let generation = |f: &Fixture| match f.source.with_delivery_turn(
        &f.pool,
        DeliveryOperation::Replay,
        |turn| -> Result<u64> { Ok(turn.generation()) },
    ) {
        Current::Current(result) => result.unwrap(),
        _ => panic!("actual Store turn required"),
    };
    let first = generation(&f);
    let decodes = f.source.decode_count();
    assert_eq!(generation(&f), first);
    assert_eq!(generation(&f), first);
    assert_eq!(
        f.source.decode_count(),
        decodes,
        "unchanged aggregate reused"
    );

    // Another Cloud component commits through its own Store handle.
    {
        let mut store = crate::delivery::access::open(f.root.path(), "agent-admission").unwrap();
        let next = store.state.clone();
        store.commit(next).unwrap();
    }
    assert_eq!(generation(&f), first + 1, "external commit observed");
    assert_eq!(
        f.source.decode_count(),
        decodes + 1,
        "replaced aggregate decoded"
    );
    assert_eq!(generation(&f), first + 1);
    assert_eq!(f.source.decode_count(), decodes + 1);

    // This owner's own commit is never retained as an unvalidated decode.
    match f
        .source
        .with_delivery_turn(&f.pool, DeliveryOperation::RegisterConsumer, |turn| {
            turn.register_delivery_consumer(&f.session).map(|_| ())
        }) {
        Current::Current(result) => result.unwrap(),
        _ => panic!("actual Store turn required"),
    };
    let after = generation(&f);
    assert!(after > first + 1);
    assert_eq!(
        f.source.decode_count(),
        decodes + 2,
        "own commit re-read and validated"
    );
    // Nothing is retained across a denied source.
    f.source.retire();
    assert!(matches!(
        f.source
            .with_delivery_turn(&f.pool, DeliveryOperation::Replay, |_| -> Result<()> {
                Ok(())
            }),
        Current::Denied
    ));
}
