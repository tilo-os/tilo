//! Real Cloud Store authority and signed control/relay proofs. Descriptor and
//! listener installation are deliberately outside this component fixture.
use super::*;
use crate::{ActionRequest, NativeOperation, Store};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use qos_action_broker::collaboration_admin::{MediaCreateInput, MediaDirectionsInput};
use qos_relay::delivery_v3::{auth, http_proof, media_wire::*};
use qos_session_peer::ingress::{AuthorizationStatus, Direction};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const ORIGIN: &str = "https://localhost:9443";

fn current<R>(source: &CloudStoreSource, run: impl FnOnce(&CloudStoreTurn) -> R) -> R {
    match source.with_current(run) {
        super::super::Current::Current(value) => value,
        super::super::Current::Busy => panic!("unexpected actual Store contention"),
        super::super::Current::Denied => panic!("actual Store authority denied"),
    }
}

fn context<R>(
    turn: &CloudStoreTurn,
    binding: &auth::Binding,
    run: impl FnOnce(&auth::VerificationContext<'_>) -> R,
) -> R {
    let certificates = turn.peer().certificates();
    let device = certificates
        .iter()
        .find(|c| c.device_public_key == binding.device)
        .unwrap();
    let mut revoked = qos_user_identity::RevocationList::new();
    for entry in turn.peer().revocations() {
        revoked.insert(entry).unwrap();
    }
    run(&auth::VerificationContext {
        binding,
        capabilities: auth::CapabilityPolicy {
            supported: 0,
            allowed: 0,
        },
        credentials: auth::Credentials {
            device,
            host: turn.peer().authority(),
            device_revocations: &revoked,
            host_revocations: &revoked,
        },
        authority: turn,
        now_ns: turn.now_ns(),
    })
}

struct Session {
    admission: http_proof::HttpSessionAdmission,
    binding: auth::Binding,
    id: [u8; 16],
    digest: [u8; 32],
    key: SigningKey,
}
impl Session {
    fn new(source: &CloudStoreSource, key: SigningKey) -> Self {
        current(source, |turn| {
            let certificates = turn.peer().certificates();
            let device = certificates
                .iter()
                .find(|c| c.device_public_key == key.verifying_key().to_bytes())
                .unwrap();
            let host = turn.peer().authority();
            let binding = auth::Binding {
                workspace: turn.workspace(),
                workspace_root: host.user_public_key,
                device: device.device_public_key,
                device_root: device.user_public_key,
                host: host.device_public_key,
                host_root: host.user_public_key,
                origin: ORIGIN.into(),
                descriptor_generation: 1,
                descriptor_digest: [18; 32],
                server_boot_id: [19; 16],
                carrier: auth::Carrier::Https,
                requested_capabilities: 0,
                mandatory_capabilities: 0,
            };
            let (admission, id, digest) = context(turn, &binding, |context| {
                let nonce = [21; 32];
                let (challenge, server) =
                    auth::Challenge::issue(context, nonce, &turn.store.key()).unwrap();
                let proof = auth::verify_server(
                    &auth::ClientExpectations {
                        scope: auth::ClientScope::from(&binding),
                        capabilities: context.capabilities,
                        credentials: context.credentials,
                        authority: context.authority,
                        now_ns: context.now_ns,
                    },
                    nonce,
                    &server,
                )
                .unwrap()
                .client_proof(&key)
                .unwrap();
                let verified = challenge.finish(&proof, context).unwrap();
                let id = verified.session_id();
                let digest = http_proof::session_binding_digest(
                    verified.binding(),
                    id,
                    verified.capabilities(),
                    verified.expires_at_ns(),
                )
                .unwrap();
                (
                    http_proof::HttpSessionAdmission::new(verified, context).unwrap(),
                    id,
                    digest,
                )
            });
            Self {
                admission,
                binding,
                id,
                digest,
                key,
            }
        })
    }

    fn request<R>(
        &mut self,
        source: &CloudStoreSource,
        command: ControlCommand,
        run: impl FnOnce(&CloudStoreTurn, &VerificationContext<'_>, &VerifiedRequest<'_>) -> R,
    ) -> R {
        let target = command.target();
        let body = ControlRequest {
            version: 1,
            command,
        }
        .encode()
        .unwrap();
        let exact = http_proof::Request::new("POST", target, &body).unwrap();
        current(source, |turn| {
            context(turn, &self.binding, |context| {
                let proof = http_proof::RequestProof::sign(
                    self.id,
                    self.digest,
                    &exact,
                    context.now_ns,
                    context.now_ns + 1_000_000_000,
                    &self.key,
                )
                .unwrap();
                let request = self.admission.admit(&proof, &exact, context).unwrap();
                run(turn, context, &request)
            })
        })
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    source: CloudStoreSource,
    service: CloudMediaService,
    browser: Session,
    host: Session,
    room: String,
    call: String,
}
impl Fixture {
    fn new() -> Self {
        let (dir, mut store, caller) = crate::actions::media_tests::voice_store();
        let room = store.state.room.clone();
        let receipt = store
            .action(
                caller.device_public_key,
                &ActionRequest {
                    version: 1,
                    room: room.clone(),
                    invocation_id: "media-control-create".into(),
                    operation: NativeOperation::MediaCreate(MediaCreateInput {
                        app_id: "dev.qos.chat".into(),
                        record_id: "".into(),
                        channel_id: "cloud_voice".into(),
                        expected_authorization_revision: None,
                        allowed: MediaDirectionsInput {
                            publish: true,
                            subscribe: true,
                        },
                    }),
                },
                qos_relay::clock::now_ns(),
            )
            .unwrap();
        let result: qos_peer_state::room::media::MediaCallResultV1 =
            serde_json::from_value(receipt.output.unwrap()).unwrap();
        let host_key = store.key();
        let source = CloudStoreSource::bind(&store).unwrap();
        drop(store);
        let service =
            CloudMediaService::new_unqualified(source.clone(), &format!("{ORIGIN}/moq/v21/voice"))
                .unwrap();
        let browser = Session::new(&source, SigningKey::from_bytes(&[8; 32]));
        let host = Session::new(&source, host_key);
        Self {
            dir,
            source,
            service,
            browser,
            host,
            room,
            call: hex::encode(result.call_id),
        }
    }

    fn intent(&self, host: bool) -> GrantRequest {
        GrantRequest {
            device: if host {
                self.host.binding.device
            } else {
                self.browser.binding.device
            },
            room: self.room.clone(),
            call_id: self.call.clone(),
            participation_id: if host { [31; 32] } else { [32; 32] },
            generation: 0,
            directions: MediaDirections {
                publish: true,
                subscribe: true,
            },
        }
    }

    fn control(&mut self, host: bool, command: ControlCommand) -> ControlResult {
        let session = if host {
            &mut self.host
        } else {
            &mut self.browser
        };
        let before = self.source.0.turns.load(Ordering::Relaxed);
        let result = session.request(&self.source, command, |turn, context, request| {
            assert!(
                matches!(
                    crate::delivery::access::try_open(self.dir.path(), "agent-admission"),
                    Err(crate::delivery::access::AccessError::Busy)
                ),
                "control must retain the actual Store transaction"
            );
            self.service
                .control_in(turn, context, request)
                .expect("genuine signed control under the same actual Cloud turn")
        });
        assert_eq!(
            self.source.0.turns.load(Ordering::Relaxed) - before,
            1,
            "control must not reopen or substitute its Store turn"
        );
        result
    }

    fn challenge(&mut self, host: bool, command: ControlCommand) -> SignedChallenge {
        let ControlResult::Challenge(challenge) = self.control(host, command) else {
            panic!("signed challenge expected")
        };
        let session = if host { &self.host } else { &self.browser };
        assert_eq!(challenge.context.delivery_session_id, session.id);
        assert_eq!(
            challenge.context.descriptor_digest,
            session.binding.descriptor_digest
        );
        assert_eq!(
            challenge.context.delivery_boot,
            session.binding.server_boot_id
        );
        assert_eq!(challenge.context.request.device, session.binding.device);
        verify(
            challenge.context.media_host,
            challenge.signing_digest().unwrap(),
            challenge.signature,
        );
        challenge
    }

    fn participation(
        &mut self,
        host: bool,
        request: GrantRequest,
        operation: GrantOperation,
    ) -> ControlResult {
        let challenge = self.challenge(host, ControlCommand::GrantChallenge { request, operation });
        let ChallengeTranscript::Native(native) = &challenge.transcript else {
            panic!("native transcript expected")
        };
        assert_eq!(native.operation, operation);
        let key = if host {
            &self.host.key
        } else {
            &self.browser.key
        };
        let signature = split(
            key.sign(&challenge.transcript.signing_digest().unwrap())
                .to_bytes(),
        );
        self.control(
            host,
            ControlCommand::GrantComplete {
                nonce: native.nonce,
                signature,
            },
        )
    }

    fn join(&mut self, host: bool) -> Vec<u8> {
        let ControlResult::Granted { grant } =
            self.participation(host, self.intent(host), GrantOperation::Join)
        else {
            panic!("actual signed grant expected")
        };
        let signed = SignedGrant::decode(&grant).unwrap();
        assert_eq!(signed.metadata.host, self.host.binding.device);
        assert_eq!(signed.metadata.request.device, self.intent(host).device);
        assert!(signed.metadata.request.generation > 0);
        verify(
            signed.metadata.host,
            signed.signing_digest().unwrap(),
            signed.signature,
        );
        grant
    }

    fn relay_proof(&mut self, host: bool, grant: &[u8]) -> Vec<u8> {
        let challenge = self.challenge(
            host,
            ControlCommand::RelayChallenge {
                grant: grant.to_vec(),
            },
        );
        let ChallengeTranscript::Relay(relay) = &challenge.transcript else {
            panic!("relay transcript expected")
        };
        let key = if host {
            &self.host.key
        } else {
            &self.browser.key
        };
        RelaySubmission {
            version: 1,
            session: relay.session_id,
            nonce: relay.nonce,
            signature: split(
                key.sign(&challenge.transcript.signing_digest().unwrap())
                    .to_bytes(),
            ),
        }
        .encode()
        .unwrap()
    }
}

fn split(signature: [u8; 64]) -> [[u8; 32]; 2] {
    [
        signature[..32].try_into().unwrap(),
        signature[32..].try_into().unwrap(),
    ]
}
fn verify(host: [u8; 32], digest: [u8; 32], signature: [[u8; 32]; 2]) {
    let mut bytes = [0; 64];
    bytes[..32].copy_from_slice(&signature[0]);
    bytes[32..].copy_from_slice(&signature[1]);
    VerifyingKey::from_bytes(&host)
        .unwrap()
        .verify_strict(&digest, &Signature::from_bytes(&bytes))
        .unwrap();
}

#[test]
fn cloud_media_control_uses_exact_source_turn_and_current_signed_context() {
    let mut f = Fixture::new();
    let intent = f.intent(false);
    let other = current(&f.source, |turn| {
        CloudStoreSource::bind(&turn.store).unwrap()
    });
    f.browser.request(
        &other,
        ControlCommand::GrantChallenge {
            request: intent.clone(),
            operation: GrantOperation::Join,
        },
        |turn, context, request| {
            assert!(
                f.service.control_in(turn, context, request).is_err(),
                "same directory under another source is not this service's turn"
            );
        },
    );
    f.browser.request(
        &f.source,
        ControlCommand::GrantChallenge {
            request: intent,
            operation: GrantOperation::Join,
        },
        |turn, context, request| {
            let mut stale = auth::VerificationContext {
                now_ns: context.now_ns - 1,
                ..*context
            };
            assert!(f.service.control_in(turn, &stale, request).is_err());
            stale.now_ns = context.now_ns;
            let mut foreign = context.binding.clone();
            foreign.descriptor_digest[0] ^= 1;
            stale.binding = &foreign;
            assert!(f.service.control_in(turn, &stale, request).is_err());
            assert!(
                matches!(
                    f.service.control_in(turn, context, request),
                    Ok(ControlResult::Challenge(_))
                ),
                "failed context checks must not consume the valid operation"
            );
        },
    );
}

#[test]
fn cloud_media_remote_leave_denies_all_scopes_but_keeps_own_publication_current() {
    let mut f = Fixture::new();
    let remote = f.join(true);
    let own = f.join(false);
    let proof = f.relay_proof(false, &own);
    let permit = f.service.admit(&proof).unwrap();
    assert!(
        f.service.admit(&proof).is_err(),
        "relay proof is single use"
    );
    let signed = SignedGrant::decode(&own).unwrap();
    let namespace = format!(
        "{}/{}",
        hex::encode(signed.metadata.namespace.call),
        hex::encode(signed.metadata.namespace.publisher)
    );
    assert_eq!(permit.publication(), Some(namespace.as_str()));
    assert_eq!(permit.subscriptions().len(), 1);
    let before = f.source.0.turns.load(Ordering::Relaxed);
    assert!(matches!(permit.status_all(), AuthorizationStatus::Current));
    assert_eq!(f.source.0.turns.load(Ordering::Relaxed) - before, 1);

    assert!(matches!(
        f.participation(
            true,
            SignedGrant::decode(&remote).unwrap().metadata.request,
            GrantOperation::Leave
        ),
        ControlResult::Left
    ));
    assert!(permit.current(Direction::Publish, &namespace));
    let before = f.source.0.turns.load(Ordering::Relaxed);
    assert!(matches!(permit.status_all(), AuthorizationStatus::Denied));
    assert_eq!(
        f.source.0.turns.load(Ordering::Relaxed) - before,
        1,
        "all scopes must share one actual Store observation"
    );
}

#[test]
fn cloud_media_busy_preserves_verified_deadline_and_cannot_admit_new_relay() {
    let mut f = Fixture::new();
    let grant = f.join(false);
    let proof = f.relay_proof(false, &grant);
    let signed = SignedGrant::decode(&grant).unwrap();
    let before_ns = current(&f.source, |turn| turn.now_ns());
    let permit = f.service.admit(&proof).unwrap();
    let admitted_at = Instant::now();
    assert!(matches!(permit.status_all(), AuthorizationStatus::Current));
    let pending = f.relay_proof(false, &grant);
    let busy = Store::open(f.dir.path(), "agent-admission").unwrap();
    let before = f.source.0.turns.load(Ordering::Relaxed);
    let AuthorizationStatus::Unavailable { expires_at } = permit.status_all() else {
        panic!("actual Store lock contention must be distinguishable from denial")
    };
    assert_eq!(f.source.0.turns.load(Ordering::Relaxed), before);
    assert!(expires_at > Instant::now());
    assert!(
        expires_at <= admitted_at + Duration::from_nanos(signed.metadata.expires_at_ns - before_ns),
        "temporary contention cannot extend the verified grant"
    );
    for _ in 0..3 {
        assert!(
            matches!(permit.status_all(), AuthorizationStatus::Unavailable { expires_at: again } if again == expires_at),
            "observations cannot restart the lease deadline"
        );
    }
    assert!(matches!(
        permit.status(Direction::Publish, "malformed"),
        AuthorizationStatus::Denied
    ));
    assert!(matches!(
        permit.status(Direction::Subscribe, permit.publication().unwrap()),
        AuthorizationStatus::Denied
    ));
    assert!(
        f.service.admit(&pending).is_err(),
        "Busy cannot manufacture initial Current or consume the pending proof"
    );
    drop(busy);
    assert!(matches!(permit.status_all(), AuthorizationStatus::Current));
    assert!(f.service.admit(&pending).is_ok());
}

#[test]
fn cloud_media_corrupt_store_is_terminal_denial_even_after_file_restore() {
    let mut f = Fixture::new();
    let grant = f.join(false);
    let proof = f.relay_proof(false, &grant);
    let permit = f.service.admit(&proof).unwrap();
    assert!(matches!(permit.status_all(), AuthorizationStatus::Current));
    let pending = f.relay_proof(false, &grant);
    let path = f.dir.path().join("state.json");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{broken").unwrap();
    assert!(matches!(permit.status_all(), AuthorizationStatus::Denied));
    assert!(f.service.admit(&pending).is_err());
    std::fs::write(&path, original).unwrap();
    assert!(matches!(permit.status_all(), AuthorizationStatus::Denied));
    assert!(
        f.service.admit(&pending).is_err(),
        "restoring bytes cannot revive a denied source lifetime"
    );
}
