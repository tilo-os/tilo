//! Real native client, grant service and pinned localhost WebTransport.
//! Generated Opus silence qualifies software transport only, not audio hardware.
use super::*;
use crate::room_media_policy::tests::{CALL, activate};
use ed25519_dalek::SigningKey;
use qos_moq::client::{
    CapturedOpus, Client, ClientAuthority, ClientError, Connection, ConnectionScope, Status,
};

struct Custody {
    service: MediaService,
    scope: ConnectionScope,
    grant: Vec<u8>,
}
impl ClientAuthority for Custody {
    fn current(&self, scope: &ConnectionScope, direction: Direction, name: &str) -> bool {
        if scope != &self.scope {
            return false;
        }
        self.service
            .turn(|peer, policy, core, now| {
                let metadata = core
                    .authority
                    .control_grant(peer, policy, &self.grant, now)?;
                let (_, _, own, publishers) =
                    core.authority
                        .control_view(peer, policy, &metadata.request, now)?;
                let allowed = match direction {
                    Direction::Publish => {
                        metadata.request.directions.publish && namespace(own) == name
                    }
                    Direction::Subscribe => {
                        metadata.request.directions.subscribe
                            && publishers
                                .iter()
                                .any(|value| *value != own && namespace(*value) == name)
                    }
                };
                if allowed {
                    Ok(())
                } else {
                    Err(MediaError::Unauthorized)
                }
            })
            .is_ok()
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    owner: SessionOwner,
    service: MediaService,
    publisher: MediaGrant,
    subscriber: MediaGrant,
    subscriber_key: SigningKey,
    secondary: Option<(SigningKey, MediaGrant)>,
    endpoint: String,
    fingerprints: Vec<String>,
    relay: qos_moq::relay::Relay,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    fn new() -> Self {
        Self::with_secondary(false)
    }
    fn with_secondary(include: bool) -> Self {
        let mut config = moq_native::ServerConfig::default();
        config.bind = Some("127.0.0.1:0".into());
        config.version = vec!["moq-transport-21".parse().unwrap()];
        config.tls.generate = vec!["localhost".into()];
        let server = config.init().unwrap();
        let endpoint = format!(
            "https://localhost:{}/moq/v21/voice",
            server.local_addr().unwrap().port()
        );
        let fingerprints = server.certificates().fingerprints();
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        activate(&mut owner);
        let subscriber_key = SigningKey::from_bytes(&[97; 32]);
        let mut enrollment = crate::enrollment::EnrollmentAuthority::default();
        let now = tilo_relay::clock::now_ns();
        let challenge = enrollment
            .challenge(
                &owner,
                qos_peer_state::enrollment::ChallengeRequest {
                    device_public_key: hex::encode(subscriber_key.verifying_key().to_bytes()),
                    request_id: hex::encode([96; 32]),
                },
                now,
            )
            .unwrap();
        let proof = qos_peer_state::enrollment::sign(
            &qos_peer_state::enrollment::decode(&challenge.challenge).unwrap(),
            &subscriber_key,
        )
        .unwrap();
        enrollment.complete(&mut owner, proof, now).unwrap();
        let secondary_key = include.then(|| SigningKey::from_bytes(&[95; 32]));
        if let Some(key) = &secondary_key {
            let challenge = enrollment
                .challenge(
                    &owner,
                    qos_peer_state::enrollment::ChallengeRequest {
                        device_public_key: hex::encode(key.verifying_key().to_bytes()),
                        request_id: hex::encode([94; 32]),
                    },
                    now,
                )
                .unwrap();
            let proof = qos_peer_state::enrollment::sign(
                &qos_peer_state::enrollment::decode(&challenge.challenge).unwrap(),
                key,
            )
            .unwrap();
            enrollment.complete(&mut owner, proof, now).unwrap();
        }
        let service =
            MediaService::for_owner(&owner, CommittedAuthority::new(&owner), &endpoint).unwrap();
        let grant = |key: &SigningKey, nonce, publish| {
            let request = GrantRequest {
                device: key.verifying_key().to_bytes(),
                room: owner.peer().room().into(),
                call_id: hex::encode(CALL),
                participation_id: [nonce; 32],
                generation: 1,
                directions: MediaDirections {
                    publish,
                    subscribe: true,
                },
            };
            let challenge = service.challenge(request, GrantOperation::Join).unwrap();
            let GrantCompletion::Granted(grant) = service
                .complete(
                    &owner,
                    challenge.nonce,
                    key.sign(&challenge.signing_digest).to_bytes(),
                )
                .unwrap()
            else {
                panic!("native grant")
            };
            grant
        };
        let publisher = grant(&owner.identity.native, 7, true);
        let subscriber = grant(&subscriber_key, 8, false);
        let secondary = secondary_key.map(|key| {
            let value = grant(&key, 9, true);
            (key, value)
        });
        let relay = qos_moq::relay::Relay::new(Arc::new(service.clone()));
        let server = tokio::spawn(relay.clone().serve(server));
        Self {
            _dir: dir,
            owner,
            service,
            publisher,
            subscriber,
            subscriber_key,
            secondary,
            endpoint,
            fingerprints,
            relay,
            server,
        }
    }
    fn prepare(&self, publish: bool, generation: u64) -> (Connection, Arc<Custody>) {
        let (grant, key) = if publish {
            (&self.publisher, &self.owner.identity.native)
        } else {
            (&self.subscriber, &self.subscriber_key)
        };
        self.prepare_exact(
            grant,
            key,
            publish,
            if publish {
                vec![]
            } else {
                vec![namespace(self.publisher.metadata.namespace)]
            },
            generation,
        )
    }
    fn prepare_exact(
        &self,
        grant: &MediaGrant,
        key: &SigningKey,
        publish: bool,
        subscriptions: Vec<String>,
        generation: u64,
    ) -> (Connection, Arc<Custody>) {
        let relay = self.service.relay_challenge(grant.as_bytes()).unwrap();
        let authorization = encode_relay_proof(
            relay.session_id,
            relay.challenge.nonce,
            key.sign(&relay.challenge.signing_digest).to_bytes(),
        )
        .unwrap();
        let scope = ConnectionScope {
            endpoint: self.endpoint.clone(),
            fingerprints: self.fingerprints.clone(),
            owner_epoch: grant.metadata.host_boot,
            carrier_generation: generation,
            publication: publish.then(|| namespace(grant.metadata.namespace)),
            subscriptions,
        };
        let custody = Arc::new(Custody {
            service: self.service.clone(),
            scope: scope.clone(),
            grant: grant.as_bytes().to_vec(),
        });
        (
            Connection {
                scope,
                authorization,
            },
            custody,
        )
    }
    fn client(&self, publish: bool, generation: u64) -> Client {
        let (connection, custody) = self.prepare(publish, generation);
        Client::start(connection, custody).expect("current native grant client")
    }
}
async fn live(client: &Client) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status() == Status::Connecting {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(client.status(), Status::Live);
    })
    .await
    .expect("native client ready deadline");
}
fn frame(sequence: u64) -> CapturedOpus {
    CapturedOpus {
        timestamp_48k: sequence * 960,
        duration_samples: 960,
        payload: vec![0xf8, 0xff, 0xfe],
        captured_at: Instant::now(),
    }
}
#[tokio::test]
async fn native_client_uses_real_grants_muted_start_five_objects_and_synchronous_stop() {
    let fixture = Fixture::new();
    let receiver = fixture.client(false, 3);
    let sender = fixture.client(true, 2);
    live(&receiver).await;
    live(&sender).await;
    assert_eq!(receiver.enable_publication(), Err(ClientError::Denied));
    assert_eq!(sender.try_send(frame(0)), Err(ClientError::Muted));
    sender.enable_publication().unwrap();
    let mut expired = frame(0);
    expired.captured_at -= Duration::from_millis(101);
    assert_eq!(sender.try_send(expired), Err(ClientError::Expired));
    // Five software-generated objects enter one group within the sender budget.
    // Waiting for each network round trip before producing the next object
    // would legitimately expire that group on a slow test machine.
    for sequence in 0..5 {
        sender.try_send(frame(sequence)).unwrap();
    }
    for sequence in 0..6 {
        if sequence == 5 {
            sender.try_send(frame(sequence)).unwrap();
        }
        let received = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(value) = receiver.try_recv() {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("real native media object");
        assert_eq!(received.object.sequence, sequence);
        assert_eq!(received.object.timestamp, sequence * 960);
        assert_eq!(received.object.payload, [0xf8, 0xff, 0xfe]);
        assert_eq!(received.group, sequence / 5);
        assert_eq!(
            received.publisher.namespace,
            namespace(fixture.publisher.metadata.namespace)
        );
        assert_eq!(received.publisher.carrier_generation, 3);
        assert!(
            received.expires_at.duration_since(received.received_at) <= Duration::from_millis(200)
        );
    }
    sender.stop_publication();
    assert_eq!(sender.try_send(frame(6)), Err(ClientError::Muted));
    sender.close();
    assert_eq!(sender.status(), Status::Closed);
    assert!(sender.enable_publication().is_err());
    receiver.close();
}

#[tokio::test]
async fn native_client_rejects_scope_substitution_and_single_use_proof_replay() {
    let fixture = Fixture::new();
    for mutation in 0..5 {
        let (mut connection, custody) = fixture.prepare(true, mutation + 1);
        match mutation {
            0 => connection.scope.owner_epoch = [9; 32],
            1 => connection.scope.carrier_generation += 1,
            2 => connection.scope.fingerprints = vec!["ab".repeat(32)],
            3 => connection.scope.endpoint = "https://other.test/moq/v21/voice".into(),
            _ => connection
                .scope
                .publication
                .as_mut()
                .unwrap()
                .replace_range(65..67, "ab"),
        }
        assert!(matches!(
            Client::start(connection, custody),
            Err(ClientError::Denied)
        ));
    }
    let (connection, custody) = fixture.prepare(true, 8);
    let replay = Connection {
        scope: connection.scope.clone(),
        authorization: connection.authorization.clone(),
    };
    let sender = Client::start(connection, custody.clone()).unwrap();
    live(&sender).await;
    // Root cause of the former flake: a Draft 21 client connection resolves
    // once CLIENT_SETUP is sent, before the relay has admitted (consumed) the
    // proof. Client `Live` therefore did not order the original admission
    // before the replay; whichever CONNECT the relay admitted first won. Wait
    // for the relay's actual admission of the original before replaying.
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.relay.counters().accepted < 1 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("original proof admitted by the relay");
    let before = fixture.relay.counters();
    let replayed = Client::start(replay, custody).unwrap();
    // The replay may briefly report transport-level Live (SETUP sent) until
    // the relay's refusal closes it; it must end Failed and never be admitted.
    tokio::time::timeout(Duration::from_secs(5), async {
        while replayed.status() != Status::Failed {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("replayed single-use proof refused");
    let after = fixture.relay.counters();
    assert_eq!(after.accepted, before.accepted, "replay never admitted");
    assert_eq!(after.rejected, before.rejected + 1);
    assert_eq!(sender.status(), Status::Live);
}

#[tokio::test]
async fn native_client_fences_pending_close_policy_revocation_and_restart() {
    let mut fixture = Fixture::new();
    let pending = fixture.client(true, 1);
    assert_eq!(pending.try_send(frame(0)), Err(ClientError::Unavailable));
    pending.close();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(pending.status(), Status::Closed);
    let sender = fixture.client(true, 2);
    live(&sender).await;
    sender.enable_publication().unwrap();
    fixture
        .service
        .owner_update(&mut fixture.owner, |owner| {
            crate::room_media_policy::tests::edit_policy(owner, |policy| {
                let call = policy.calls.get_mut(&CALL).unwrap();
                call.epoch = 2;
                call.allowed.publish = false;
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(sender.try_send(frame(0)), Err(ClientError::Denied));
    assert_eq!(sender.status(), Status::Denied);
    assert_eq!(sender.counters().queued_received, 0);
    assert!(sender.try_recv().is_none());
    // A replacement media owner has a fresh boot and accepts none of the old grants.
    let restarted = MediaService::for_owner(
        &fixture.owner,
        CommittedAuthority::new(&fixture.owner),
        &fixture.endpoint,
    )
    .unwrap();
    assert!(
        restarted
            .relay_challenge(fixture.subscriber.as_bytes())
            .is_err()
    );
    sender.close();
    assert_eq!(sender.status(), Status::Closed);
}

#[tokio::test]
async fn native_client_expires_received_audio_and_reconnects_with_fresh_proof() {
    let fixture = Fixture::new();
    let receiver = fixture.client(false, 1);
    let sender = fixture.client(true, 1);
    live(&receiver).await;
    live(&sender).await;
    sender.enable_publication().unwrap();
    sender.try_send(frame(0)).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while receiver.counters().queued_received == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert!(receiver.try_recv().is_none());
    assert_eq!(receiver.counters().queued_received, 0);
    assert!(receiver.counters().late_dropped >= 1);
    sender.close();
    receiver.close();
    let receiver = fixture.client(false, 2);
    let sender = fixture.client(true, 2);
    live(&receiver).await;
    live(&sender).await;
    assert_eq!(sender.try_send(frame(0)), Err(ClientError::Muted));
    assert!(receiver.try_recv().is_none(), "no outage replay");
    sender.enable_publication().unwrap();
    sender.try_send(frame(0)).unwrap();
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(next) = receiver.try_recv() {
                break next;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(next.publisher.carrier_generation, 2);
    assert_eq!(next.object.sequence, 0);
}

#[tokio::test]
async fn native_client_observes_one_publisher_replacement_and_recovers_other_scopes() {
    let f = Fixture::with_secondary(true);
    let (key, grant) = f.secondary.as_ref().unwrap();
    let names = vec![
        namespace(f.publisher.metadata.namespace),
        namespace(grant.metadata.namespace),
    ];
    let receiver = |generation| {
        let (connection, authority) = f.prepare_exact(
            &f.subscriber,
            &f.subscriber_key,
            false,
            names.clone(),
            generation,
        );
        Client::start(connection, authority).unwrap()
    };
    let old = receiver(1);
    let sender = f.client(true, 1);
    let (connection, authority) = f.prepare_exact(grant, key, true, vec![], 1);
    let unaffected = Client::start(connection, authority).unwrap();
    live(&old).await;
    live(&sender).await;
    live(&unaffected).await;
    sender.enable_publication().unwrap();
    unaffected.enable_publication().unwrap();
    sender.try_send(frame(0)).unwrap();
    unaffected.try_send(frame(0)).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while old.counters().queued_received < 2 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    sender.close();
    tokio::time::timeout(Duration::from_secs(5), async {
        while old.status() == Status::Live {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("remote publisher loss must be observable, not silently Live");
    assert_eq!(old.status(), Status::Failed);
    assert!(old.try_recv().is_none());
    assert_eq!(old.counters().queued_received, 0);
    assert_eq!(unaffected.status(), Status::Live);
    // Expire the original publisher cache before obtaining the replacement proof.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let replacement = f.client(true, 2);
    let fresh = receiver(2);
    live(&replacement).await;
    live(&fresh).await;
    assert!(fresh.try_recv().is_none());
    assert_eq!(replacement.try_send(frame(10)), Err(ClientError::Muted));
    replacement.enable_publication().unwrap();
    replacement.try_send(frame(10)).unwrap();
    unaffected.try_send(frame(20)).unwrap();
    let mut received = std::collections::BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while received.len() != 2 {
            if let Some(value) = fresh.try_recv() {
                assert_eq!(value.publisher.carrier_generation, 2);
                assert!(names.contains(&value.publisher.namespace));
                let expected = if value.publisher.namespace == names[0] {
                    10
                } else {
                    20
                };
                assert_eq!(
                    value.object.timestamp,
                    expected * 960,
                    "old carrier packets must not replay"
                );
                received.insert(value.publisher.namespace);
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(unaffected.status(), Status::Live);
    assert_eq!(fresh.status(), Status::Live);
}

#[tokio::test]
async fn native_relay_keeps_a_subscriber_live_when_a_new_publisher_joins() {
    let f = Fixture::with_secondary(true);
    let (key, grant) = f.secondary.as_ref().unwrap();
    let signed = |request: GrantRequest, operation| {
        let challenge = f.service.challenge(request, operation).unwrap();
        f.service
            .complete(
                &f.owner,
                challenge.nonce,
                key.sign(&challenge.signing_digest).to_bytes(),
            )
            .unwrap()
    };
    // The second publisher leaves before the subscriber is admitted.
    signed(grant.metadata.request.clone(), GrantOperation::Leave);
    let receiver = f.client(false, 1);
    live(&receiver).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.relay.counters().accepted < 1 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    // An unchanged call inventory keeps the admitted subscriber open.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(receiver.status(), Status::Live);
    // It rejoins (same device, next participation generation, no membership
    // change). The subscriber is scoped to its call, so the roster growth is
    // offered in place instead of closing the session.
    let mut rejoin = grant.metadata.request.clone();
    rejoin.generation += 1;
    let GrantCompletion::Granted(_) = signed(rejoin, GrantOperation::Join) else {
        panic!("second publisher rejoin grant")
    };
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(receiver.status(), Status::Live);
    assert_eq!(f.relay.counters().expired, 0);
}
