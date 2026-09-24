//! Actual committed owner, installed media descriptor, HTTP registry and grant service.
//! Imports only the portable protocol child; no test witness enters production.
#[path = "../../../../session/shell/src/network/native_media/protocol.rs"]
mod protocol;
use super::*;
use crate::{
    delivery_http::HttpSessions,
    https_assets::AssetRoot,
    media_ingress::{IngressConfig, InstalledIngress},
};
use protocol::{NativeControl, OwnerView, PreparedRequest, Step};
use tilo_relay::delivery_v3::{descriptor, http_proof, media_wire};

struct Fixture {
    _dir: tempfile::TempDir,
    owner: SessionOwner,
    current: CommittedAuthority,
    registry: HttpSessions,
    service: MediaService,
    _ingress: InstalledIngress,
    same_turn: bool,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        crate::room_media_policy::tests::activate(&mut owner);
        owner.activate_delivery().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        owner
            .install_delivery_transport(&origin, 0, web_time::SystemTime::now())
            .unwrap();
        let current = CommittedAuthority::new(&owner);
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let assets = dir.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(assets.join("index.html"), "protocol test").unwrap();
        let ingress = InstalledIngress::start(
            &mut owner,
            current.clone(),
            listener,
            IngressConfig {
                origin: origin.clone(),
                assets: AssetRoot::open(&assets).unwrap(),
                certificates: vec![cert.cert.der().clone()],
                key: rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()),
                ),
                local_enrollment: None,
            },
        )
        .await
        .unwrap();
        let registry = HttpSessions::new(&owner, &current, tilo_relay::clock::now_ns()).unwrap();
        let service =
            MediaService::for_owner(&owner, current.clone(), &format!("{origin}/moq/v21/voice"))
                .unwrap();
        Self {
            _dir: dir,
            owner,
            current,
            registry,
            service,
            _ingress: ingress,
            same_turn: false,
        }
    }
    fn view<T>(&self, use_view: impl FnOnce(&OwnerView<'_>) -> T) -> T {
        let guard = self.current.read().unwrap();
        let meta = descriptor::inspect_persisted_hint(
            self.owner.stored_delivery_hint().unwrap(),
            self.owner.host_certificate(),
        )
        .unwrap();
        use_view(&OwnerView {
            peer: guard.peer(),
            key: &self.owner.identity.native,
            governor: guard.peer().authority(),
            host: self.owner.host_certificate(),
            device: &self.owner.identity.native_cert,
            workspace: self.owner.workspace_id(),
            continuity: guard.continuity(),
            generation: descriptor::GenerationExpectation::Current {
                generation: meta.generation,
                descriptor_digest: meta.descriptor_digest,
            },
            now_ns: tilo_relay::clock::now_ns(),
        })
    }
    fn exchange(&mut self, prepared: &PreparedRequest) -> Vec<u8> {
        let now = tilo_relay::clock::now_ns();
        match prepared.target {
            "/peer/v3/challenge" => self
                .registry
                .challenge(&self.owner, &self.current, &prepared.body, now)
                .unwrap(),
            "/peer/v3/sessions" => self
                .registry
                .finish(&self.owner, &self.current, &prepared.body, now)
                .unwrap(),
            target => {
                let request = http_proof::Request::new("POST", target, &prepared.body).unwrap();
                let proof =
                    http_proof::RequestProof::decode(prepared.proof.as_ref().unwrap()).unwrap();
                let admitted = self
                    .registry
                    .admit(&self.owner, &self.current, &proof, &request, now)
                    .unwrap();
                if !self.same_turn {
                    return crate::media_control::dispatch(&self.owner, &self.service, &admitted)
                        .unwrap();
                }
                use tilo_relay::delivery_v3::auth;
                let observed = self.service.0.source.with_current(|turn| {
                    let view = turn.current.view();
                    let now = SystemTime::UNIX_EPOCH + Duration::from_nanos(turn.now_ns());
                    let descriptor = self
                        .owner
                        .verified_delivery_transport(
                            view,
                            now,
                            crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
                        )
                        .unwrap();
                    let scope = admitted.session().binding();
                    let descriptor_authority = self.owner.transport_authority(
                        turn.peer(),
                        descriptor::GenerationExpectation::Current {
                            generation: descriptor.generation(),
                            descriptor_digest: descriptor.descriptor_digest(),
                        },
                        now,
                        crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
                    );
                    let binding = descriptor
                        .server_binding(
                            &descriptor_authority,
                            admitted.session().device_certificate(),
                            auth::Carrier::Https,
                            scope.requested_capabilities,
                            scope.mandatory_capabilities,
                            scope.server_boot_id,
                        )
                        .unwrap();
                    let mut revocations = tilo_user_identity::RevocationList::new();
                    for entry in turn.peer().revocations() {
                        revocations.insert(entry).unwrap();
                    }
                    let authority = crate::owner::delivery::NativeDeliveryAuthority {
                        view,
                        workspace: turn.workspace(),
                        host: self.owner.host_certificate(),
                    };
                    let mut foreign_binding = binding.clone();
                    foreign_binding.descriptor_digest[0] ^= 1;
                    let mut context = auth::VerificationContext {
                        binding: &binding,
                        capabilities: auth::CapabilityPolicy {
                            supported: 0,
                            allowed: 0,
                        },
                        credentials: auth::Credentials {
                            device: admitted.session().device_certificate(),
                            host: self.owner.host_certificate(),
                            device_revocations: &revocations,
                            host_revocations: &revocations,
                        },
                        authority: &authority,
                        now_ns: turn.now_ns(),
                    };
                    // Failed current-context checks must precede signing and
                    // one-use proof consumption; the real request still works.
                    context.now_ns -= 1;
                    assert!(
                        self.service
                            .control_in(turn, &context, &admitted, |_| panic!(
                                "stale context reached signer"
                            ))
                            .is_err()
                    );
                    context.now_ns = turn.now_ns();
                    context.binding = &foreign_binding;
                    assert!(
                        self.service
                            .control_in(turn, &context, &admitted, |_| panic!(
                                "foreign descriptor reached signer"
                            ))
                            .is_err()
                    );
                    context.binding = &binding;
                    let result = self
                        .service
                        .control_in(turn, &context, &admitted, |input| {
                            Ok(self.owner.identity.native.sign(input.digest()).to_bytes())
                        })
                        .unwrap();
                    media_wire::ControlReply { version: 1, result }
                        .encode()
                        .unwrap()
                });
                let MediaObservation::Current(bytes) = observed else {
                    panic!("actual owner turn");
                };
                bytes
            }
        }
    }
    fn finish(&mut self, client: &mut NativeControl, mut request: PreparedRequest) -> Step {
        for _ in 0..4 {
            let bytes = self.exchange(&request);
            let step = self
                .view(|view| client.advance(view, request.id, &bytes))
                .unwrap();
            match step {
                Step::Request(next) => request = next,
                step => return step,
            }
        }
        panic!("unbounded protocol steps")
    }
}
#[tokio::test]
async fn native_closed_control_real_owner_completes_grant_renew_refresh_and_leave() {
    exercise_control(false).await;
}
#[tokio::test]
async fn native_same_turn_control_uses_original_context_without_reopening_owner() {
    exercise_control(true).await;
}
async fn exercise_control(same_turn: bool) {
    let mut f = Fixture::new().await;
    f.same_turn = same_turn;
    let mut client = f
        .view(|v| NativeControl::new(v, f.owner.stored_delivery_hint().unwrap()))
        .unwrap();
    let request = f.view(|v| client.start_http(v)).unwrap();
    assert!(matches!(f.finish(&mut client, request), Step::HttpReady));
    let request = f
        .view(|v| {
            client.start_join(
                v,
                &hex::encode(crate::room_media_policy::tests::CALL),
                media_wire::MediaDirections {
                    publish: true,
                    subscribe: true,
                },
            )
        })
        .unwrap();
    let Step::Grant(grant) = f.finish(&mut client, request) else {
        panic!("grant")
    };
    let request = f.view(|v| client.relay_challenge(v, grant.handle)).unwrap();
    let Step::Relay(relay) = f.finish(&mut client, request) else {
        panic!("relay")
    };
    let live = f
        .view(|v| client.current_carrier(&v.current_view(), relay.handle, relay.carrier))
        .unwrap();
    assert!(live.directions.publish && live.directions.subscribe);
    assert!(live.presence_in_ns <= 15_000_000_000);
    assert!(
        f.view(|v| {
            let mut current = v.current_view();
            current.now_ns += 15_000_000_001;
            client.current_carrier(&current, relay.handle, relay.carrier)
        })
        .is_err()
    );
    let permit = f.service.admit(&relay.setup).unwrap();
    assert!(permit.current(Direction::Publish, permit.publication().unwrap()));
    let request = f.view(|v| client.heartbeat(v, grant.handle)).unwrap();
    assert!(matches!(f.finish(&mut client, request), Step::Presence(_)));
    let request = f.view(|v| client.renew(v, grant.handle)).unwrap();
    assert!(matches!(f.finish(&mut client, request), Step::Grant(_)));
    let request = f
        .view(|v| client.refresh(v, relay.handle, relay.carrier))
        .unwrap();
    assert!(matches!(f.finish(&mut client, request), Step::Refreshed));
    let request = f
        .view(|v| client.close_relay(v, relay.handle, relay.carrier))
        .unwrap();
    assert!(matches!(f.finish(&mut client, request), Step::RelayClosed));
    assert!(
        f.view(|v| client.current_carrier(&v.current_view(), relay.handle, relay.carrier))
            .is_err()
    );
    assert!(!permit.current(Direction::Publish, permit.publication().unwrap()));
    let Step::Request(request) = f.view(|v| client.leave(v, grant.handle)).unwrap() else {
        panic!("leave")
    };
    assert!(matches!(f.finish(&mut client, request), Step::Left));
}

#[tokio::test]
async fn native_closed_control_requires_real_current_floor_key_and_continuity() {
    let f = Fixture::new().await;
    let wrong_key = ed25519_dalek::SigningKey::from_bytes(&[101; 32]);
    for wrong_signer in [false, true] {
        f.view(|v| {
            let current = v.current_view();
            let bad = OwnerView {
                peer: v.peer,
                key: if wrong_signer { &wrong_key } else { v.key },
                governor: v.governor,
                host: v.host,
                device: v.device,
                workspace: v.workspace,
                continuity: v.continuity,
                generation: if wrong_signer {
                    current.generation
                } else {
                    descriptor::GenerationExpectation::FirstAccepted {
                        minimum_generation: 1,
                    }
                },
                now_ns: v.now_ns,
            };
            assert!(NativeControl::new(&bad, f.owner.stored_delivery_hint().unwrap()).is_err());
        });
    }
    let mut client = f
        .view(|v| NativeControl::new(v, f.owner.stored_delivery_hint().unwrap()))
        .unwrap();
    f.view(|v| {
        let bad = OwnerView {
            peer: v.peer,
            key: v.key,
            governor: v.governor,
            host: v.host,
            device: v.device,
            workspace: v.workspace,
            continuity: v.continuity + 1,
            generation: v.generation,
            now_ns: v.now_ns,
        };
        assert!(client.start_http(&bad).is_err());
    });
    client.close();
    assert!(f.view(|v| client.start_http(v)).is_err());
}

#[tokio::test]
async fn native_closed_control_rejects_signed_foreign_session_and_inventory_completions() {
    for foreign_inventory in [false, true] {
        let mut f = Fixture::new().await;
        let mut client = f
            .view(|v| NativeControl::new(v, f.owner.stored_delivery_hint().unwrap()))
            .unwrap();
        let request = f.view(|v| client.start_http(v)).unwrap();
        assert!(matches!(f.finish(&mut client, request), Step::HttpReady));
        let request = f
            .view(|v| {
                client.start_join(
                    v,
                    &hex::encode(crate::room_media_policy::tests::CALL),
                    media_wire::MediaDirections {
                        publish: true,
                        subscribe: true,
                    },
                )
            })
            .unwrap();
        let original = f.exchange(&request);
        let mut reply = media_wire::ControlReply::decode(&original).unwrap();
        let media_wire::ControlResult::Challenge(challenge) = &mut reply.result else {
            panic!("real challenge")
        };
        if foreign_inventory {
            challenge
                .context
                .inventory
                .namespaces
                .push(media_wire::MediaNamespace {
                    call: [51; 32],
                    publisher: [52; 32],
                });
        } else {
            challenge.context.delivery_session_id = [53; 16];
        }
        // The actual governor signs the altered valid wire value. Exact current
        // custody must reject it independently of signature verification.
        challenge.signature = media_wire::split_signature(
            f.owner
                .identity
                .native
                .sign(&challenge.signing_digest().unwrap())
                .to_bytes(),
        );
        assert!(
            f.view(|v| client.advance(v, request.id, &reply.encode().unwrap()))
                .is_err()
        );
        assert!(
            f.view(|v| client.advance(v, request.id, &original))
                .is_err(),
            "failed completion is consumed, not replayable"
        );
    }
}

#[tokio::test]
async fn native_closed_control_retirement_requires_fresh_single_use_carrier() {
    let mut f = Fixture::new().await;
    let mut client = f
        .view(|v| NativeControl::new(v, f.owner.stored_delivery_hint().unwrap()))
        .unwrap();
    let request = f.view(|v| client.start_http(v)).unwrap();
    assert!(matches!(f.finish(&mut client, request), Step::HttpReady));
    let request = f
        .view(|v| {
            client.start_join(
                v,
                &hex::encode(crate::room_media_policy::tests::CALL),
                media_wire::MediaDirections {
                    publish: true,
                    subscribe: true,
                },
            )
        })
        .unwrap();
    let Step::Grant(grant) = f.finish(&mut client, request) else {
        panic!("grant required")
    };
    let request = f.view(|v| client.relay_challenge(v, grant.handle)).unwrap();
    let Step::Relay(old) = f.finish(&mut client, request) else {
        panic!("relay required")
    };
    let permit = f.service.admit(&old.setup).unwrap();
    assert!(
        f.view(|v| client.current_carrier(&v.current_view(), old.handle, old.carrier))
            .is_ok()
    );
    client.retire_carrier(old.carrier);
    assert!(
        f.view(|v| client.current_carrier(&v.current_view(), old.handle, old.carrier))
            .is_err(),
        "retired local scope cannot remain current"
    );
    drop(permit); // The actual relay permit dies when its native session closes.
    let request = f.view(|v| client.relay_challenge(v, grant.handle)).unwrap();
    let Step::Relay(new) = f.finish(&mut client, request) else {
        panic!("fresh relay required")
    };
    assert_ne!(new.carrier, old.carrier);
    assert_ne!(new.setup, old.setup);
    assert_eq!(new.namespace, old.namespace);
    let permit = f.service.admit(&new.setup).unwrap();
    assert!(permit.current(Direction::Publish, permit.publication().unwrap()));
    assert!(
        f.service.admit(&old.setup).is_err(),
        "old proof is never replayed for recovery"
    );
    assert!(
        f.view(|v| client.current_carrier(&v.current_view(), new.handle, new.carrier))
            .is_ok()
    );
}
