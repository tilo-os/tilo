use super::*;
use qos_peer_state::{SeedState, seed_room};
use tilo_relay::delivery_v3::{auth, descriptor, http_proof};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const NOW: u64 = 1_000_000_000_000;

#[test]
fn delivery_bootstrap_uses_shared_current_owner_without_media_participation() {
    let fixture = Fixture::new();
    let view: crate::peer_delivery_auth::OwnerView<'_> = fixture.view();
    let (_, request) = crate::peer_delivery_auth::Bootstrap::start(&view, &fixture.hint()).unwrap();
    assert_eq!(request.target, "/peer/v3/bootstrap");
    assert!(request.proof.is_none());
    let wire = media_wire::BootstrapRequest::decode(&request.body).unwrap();
    assert_eq!(wire.device, fixture.device.device_public_key);
    assert_eq!(wire.root, fixture.governor.user_public_key);
    assert_eq!(wire.workspace, view.workspace);
    assert_eq!(wire.room, view.peer.room());
    ed25519_dalek::VerifyingKey::from_bytes(&fixture.device.device_public_key)
        .unwrap()
        .verify_strict(
            &wire.signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(wire.signature)),
        )
        .unwrap();
}

#[test]
fn closing_media_preserves_shared_http_custody_but_expiry_still_fences_it() {
    let fixture = Fixture::new();
    let mut media = fixture.ready_client();
    let authority = media.authority.clone();
    assert!(authority.borrow().current(&fixture.view()));
    media.close();
    assert!(!media.current(&fixture.view()));
    assert!(authority.borrow().current(&fixture.view()));
    let mut expired = fixture.view();
    expired.now_ns = NOW + 100_000_000_000;
    assert!(!authority.borrow().current(&expired));
    let mut foreign = fixture.view();
    foreign.lease_epoch = "another-owner";
    assert!(!authority.borrow().current(&foreign));
}

struct Fixture {
    governor: DeviceCert,
    governor_key: SigningKey,
    host: DeviceCert,
    device: DeviceCert,
    host_key: SigningKey,
    device_key: SigningKey,
    peer: StatePeer,
}
fn cert(root: &SigningKey, key: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
    let mut value = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "media fixture".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut value, root).unwrap();
    value
}
impl Fixture {
    fn new() -> Self {
        let root = SigningKey::from_bytes(&[1; 32]);
        let governor_key = SigningKey::from_bytes(&[2; 32]);
        let host_key = SigningKey::from_bytes(&[3; 32]);
        let device_key = SigningKey::from_bytes(&[4; 32]);
        let governor = cert(&root, &governor_key, DeviceCapabilities::primary());
        let host = cert(&root, &host_key, DeviceCapabilities::host());
        let device = cert(&root, &device_key, DeviceCapabilities::member());
        let roster = vec![governor.clone(), host.clone(), device.clone()];
        let now = time(NOW);
        let mut peer = StatePeer::new(
            ROOM,
            governor.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(
            SeedState {
                room_changes: seed_room(ROOM, &governor, &roster).unwrap(),
                documents: vec![],
            },
            &governor_key,
            now,
        )
        .unwrap();
        Self {
            governor,
            governor_key,
            host,
            device,
            host_key,
            device_key,
            peer,
        }
    }
    fn view(&self) -> OwnerView<'_> {
        OwnerView {
            peer: &self.peer,
            key: &self.device_key,
            governor: &self.governor,
            host: &self.host,
            device: &self.device,
            workspace: tilo_relay::room_workspace_id(ROOM).unwrap(),
            lease_epoch: "held-lease",
            generation: descriptor::GenerationExpectation::FirstAccepted {
                minimum_generation: 1,
            },
            now_ns: NOW,
            state_revision: 0,
        }
    }
    fn activate_call(&mut self) {
        use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
        use qos_peer_state::{SeedDocument, room::media};
        let mut chat = AutoCommit::new();
        let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
        let channel = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
        chat.put(&channel, "kind", "voice").unwrap();
        chat.put(&channel, "workspace", "home").unwrap();
        self.peer
            .install_seed(
                SeedState {
                    room_changes: vec![],
                    documents: vec![SeedDocument {
                        app_id: "so.tilo.chat".into(),
                        record_id: "".into(),
                        kind: "chat".into(),
                        bytes: chat.save(),
                    }],
                },
                &self.governor_key,
                time(NOW),
            )
            .unwrap();
        let binding = self
            .peer
            .resolve_media_binding("so.tilo.chat", "", "voice")
            .unwrap();
        let policy = media::RoomMediaV1 {
            version: 1,
            room: *uuid::Uuid::parse_str(ROOM).unwrap().as_bytes(),
            authorization_revision: 1,
            authorization_basis: media::security_basis(self.peer.admitted_room().unwrap(), &[])
                .unwrap(),
            calls: BTreeMap::from([(
                [7; 32],
                media::CallV1 {
                    binding,
                    epoch: 1,
                    phase: media::CallPhaseV1::Active,
                    allowed: media::DirectionsV1 {
                        publish: true,
                        subscribe: true,
                    },
                    denied: BTreeMap::new(),
                },
            )]),
        };
        self.peer
            .author_media_policy(&policy, &self.governor_key, time(NOW))
            .unwrap();
    }
    fn ready_client(&self) -> PeerMedia {
        let mut client = self.client();
        let view = self.view();
        let prepared = client.start_http(&view).unwrap();
        let (_, _, nonce, _, _): (u16, [u8; 32], [u8; 32], u64, u64) =
            postcard::from_bytes(&prepared.body).unwrap();
        let binding = client
            .authority
            .borrow()
            .descriptor
            .server_binding(
                &view.descriptor_authority(),
                &self.device,
                auth::Carrier::Https,
                0,
                0,
                [9; 16],
            )
            .unwrap();
        let revoked = RevocationList::new();
        let context = auth::VerificationContext {
            binding: &binding,
            capabilities: auth::CapabilityPolicy {
                supported: 3,
                allowed: 3,
            },
            credentials: auth::Credentials {
                device: &self.device,
                host: &self.host,
                device_revocations: &revoked,
                host_revocations: &revoked,
            },
            authority: &view,
            now_ns: NOW,
        };
        let (challenge, proof) = auth::Challenge::issue(&context, nonce, &self.host_key).unwrap();
        let Step::Request(prepared) = client
            .advance(&view, prepared.id, &proof.encode().unwrap())
            .unwrap()
        else {
            panic!("missing client proof")
        };
        challenge
            .finish(
                &auth::ClientProof::decode(&prepared.body).unwrap(),
                &context,
            )
            .unwrap();
        let t = &proof.transcript;
        let digest = http_proof::session_binding_digest(
            &binding,
            t.session_id,
            t.negotiated_capabilities,
            t.expires_at_ns,
        )
        .unwrap();
        let ready = postcard::to_stdvec(&(
            3_u16,
            t.session_id,
            digest,
            t.expires_at_ns,
            t.negotiated_capabilities,
        ))
        .unwrap();
        assert!(matches!(
            client.advance(&view, prepared.id, &ready).unwrap(),
            Step::HttpReady
        ));
        client
    }
    fn hint(&self) -> qos_peer_state::join::TransportHint {
        descriptor::issue_transport_hint(
            &descriptor::DescriptorSettings {
                origin: "https://media.test".into(),
                delivery_capabilities: 3,
                issued_at_ns: NOW,
                expires_at_ns: NOW + 100_000_000_000,
                generation: 1,
            },
            &self.view().descriptor_authority(),
            &self.host_key,
        )
        .unwrap()
    }
    fn client(&self) -> PeerMedia {
        use crate::peer_journal::{AcknowledgedDescriptorFloor, DescriptorFloorV1, Journal};
        use ui::portable_storage::{PortableStorage, StorageKey};
        let descriptor =
            descriptor::verify_transport_hint(&self.hint(), &self.view().descriptor_authority())
                .unwrap();
        let identity = crate::peer_delivery_cursor::CursorIdentity {
            workspace: self.view().workspace,
            room: ROOM.into(),
            workspace_root: self.governor.user_public_key,
            device: self.device.device_public_key,
            device_root: self.device.user_public_key,
            host: self.host.device_public_key,
            host_root: self.host.user_public_key,
        };
        let mut journal = Journal::from_state(&self.peer, vec![]).unwrap();
        journal
            .storage
            .advance_descriptor(DescriptorFloorV1 {
                version: 1,
                identity: identity.clone(),
                generation: descriptor.generation(),
                descriptor_digest: descriptor.descriptor_digest(),
            })
            .unwrap();
        let cache = PortableStorage::new("media-test", "device").unwrap();
        cache
            .put(
                StorageKey::Admission {
                    workspace: ROOM.into(),
                },
                journal.encode().unwrap(),
                None,
            )
            .unwrap();
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        PeerMedia::new(
            "held-lease",
            descriptor,
            AcknowledgedDescriptorFloor::read(&cache, &identity).unwrap(),
        )
        .unwrap()
    }
}

#[test]
fn handshake_uses_real_verified_descriptor_and_fences_closed_or_stale_custody() {
    let f = Fixture::new();
    let mut client = f.client();
    let request = client.start_http(&f.view()).unwrap();
    assert_eq!(request.target, "/peer/v3/challenge");
    let (version, device, nonce, requested, mandatory): (u16, [u8; 32], [u8; 32], u64, u64) =
        postcard::from_bytes(&request.body).unwrap();
    assert_eq!(version, 3);
    assert_eq!(device, f.device.device_public_key);
    assert_ne!(nonce, [0; 32]);
    assert_eq!((requested, mandatory), (0, 0));
    let mut stale = f.view();
    stale.lease_epoch = "other-lease";
    assert!(client.start_http(&stale).is_err());
    client.close();
    assert!(client.start_http(&f.view()).is_err());
    assert!(client.advance(&f.view(), request.id, &[1]).is_err());
}

#[test]
fn no_http_ready_context_is_created_from_unverified_response_bytes() {
    let f = Fixture::new();
    let mut client = f.client();
    let request = client.start_http(&f.view()).unwrap();
    let forged =
        postcard::to_stdvec(&(3_u16, [8_u8; 16], [9_u8; 32], NOW + 1_000_000_000, 0_u64)).unwrap();
    assert!(client.advance(&f.view(), request.id, &forged).is_err());
    assert!(!client.http_ready());
    assert!(
        client.advance(&f.view(), request.id, &forged).is_err(),
        "attempt is consumed on failure"
    );
}

#[test]
fn governor_bootstrap_binds_fresh_nonce_exact_hint_and_independent_enrollment_pins() {
    use ed25519_dalek::Signer;
    let f = Fixture::new();
    let hint = f.hint();
    let (bootstrap, prepared) = Bootstrap::start(&f.view(), &hint).unwrap();
    assert_eq!(prepared.target, "/peer/v3/bootstrap");
    let request = media_wire::BootstrapRequest::decode(&prepared.body).unwrap();
    f.device_key
        .verifying_key()
        .verify_strict(
            &request.signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(request.signature)),
        )
        .unwrap();
    let response = |request: media_wire::BootstrapRequest| {
        let mut reply = media_wire::BootstrapResponse {
            version: 1,
            descriptor_digest: request.candidate_digest,
            request,
            host_certificate: postcard::to_stdvec(&f.host).unwrap(),
            hint: postcard::to_stdvec(&hint).unwrap(),
            generation: 1,
            issued_at_ns: NOW,
            expires_at_ns: NOW + 30_000_000_000,
            signature: [[0; 32]; 2],
        };
        reply.signature = media_wire::split_signature(
            f.governor_key
                .sign(&reply.signing_digest().unwrap())
                .to_bytes(),
        );
        reply
    };
    let bytes = response(request.clone()).encode().unwrap();
    let verified = bootstrap.finish(&f.view(), &bytes).unwrap();
    assert_eq!(
        verified.floor.descriptor_digest,
        verified.descriptor.descriptor_digest()
    );
    assert_eq!(verified.floor.identity.host, f.host.device_public_key);
    let (other, _) = Bootstrap::start(&f.view(), &hint).unwrap();
    assert!(
        other.finish(&f.view(), &bytes).is_err(),
        "a prior valid nonce must not satisfy a fresh request"
    );
    let (forged, prepared) = Bootstrap::start(&f.view(), &hint).unwrap();
    let mut reply = response(media_wire::BootstrapRequest::decode(&prepared.body).unwrap());
    reply.signature[0][0] ^= 1;
    assert!(forged.finish(&f.view(), &reply.encode().unwrap()).is_err());
    let (stale, prepared) = Bootstrap::start(&f.view(), &hint).unwrap();
    let reply = response(media_wire::BootstrapRequest::decode(&prepared.body).unwrap());
    let mut view = f.view();
    view.now_ns = NOW + 30_000_000_001;
    assert!(stale.finish(&view, &reply.encode().unwrap()).is_err());
}

#[test]
fn join_uses_current_governed_call_and_rust_nonce_after_real_http_handshake() {
    let mut f = Fixture::new();
    f.activate_call();
    let mut client = f.ready_client();
    let prepared = client
        .start_join(
            &f.view(),
            &"07".repeat(32),
            media_wire::MediaDirections {
                publish: true,
                subscribe: true,
            },
        )
        .unwrap();
    assert_eq!(prepared.target, "/peer/v3/media/grant/challenge");
    assert!(
        prepared
            .proof
            .as_ref()
            .is_some_and(|v| !v.is_empty() && v.len() <= 256)
    );
    let request = media_wire::ControlRequest::decode(&prepared.body).unwrap();
    let media_wire::ControlCommand::GrantChallenge { request, operation } = request.command else {
        panic!("wrong command")
    };
    assert_eq!(operation, media_wire::GrantOperation::Join);
    assert_eq!(request.generation, 0);
    assert_ne!(request.participation_id, [0; 32]);
    assert_eq!(request.device, f.device.device_public_key);
    assert!(
        client
            .start_join(&f.view(), &"07".repeat(32), request.directions)
            .is_err()
    );
    client.close();
    assert!(client.advance(&f.view(), prepared.id, &[1]).is_err());
}

fn reply(result: media_wire::ControlResult) -> Vec<u8> {
    media_wire::ControlReply { version: 1, result }
        .encode()
        .unwrap()
}
impl Fixture {
    fn challenge(
        &self,
        client: &PeerMedia,
        operation: media_wire::GrantOperation,
    ) -> media_wire::SignedChallenge {
        let mut request = client
            .participation
            .as_ref()
            .or(client.ending.as_ref())
            .unwrap()
            .request
            .clone();
        if request.generation == 0 {
            request.generation = 7;
        }
        let transcript = media_wire::NativeChallenge {
            request: request.clone(),
            operation,
            epoch: 1,
            membership_generation: 1,
            directions: media_wire::MediaDirections {
                publish: true,
                subscribe: true,
            },
            audience: "https://media.test/moq/v21/voice".into(),
            host: self.governor.device_public_key,
            host_boot: [10; 32],
            nonce: [11; 32],
            issued_at_ns: NOW,
            expires_at_ns: NOW + 60_000_000_000,
        };
        let authority = client.authority.borrow();
        let session = &authority.session.as_ref().unwrap().proof.transcript;
        let context = media_wire::MediaContext {
            version: 1,
            descriptor_digest: authority.descriptor.descriptor_digest(),
            delivery_session_id: session.session_id,
            delivery_boot: session.binding.server_boot_id,
            origin: "https://media.test".into(),
            workspace: self.view().workspace,
            room: ROOM.into(),
            root: self.device.user_public_key,
            delivery_host: self.host.device_public_key,
            media_host: self.governor.device_public_key,
            media_boot: [10; 32],
            request,
            epoch: 1,
            membership_generation: 1,
            directions: transcript.directions,
            inventory: media_wire::PublisherInventory {
                revision: 1,
                namespaces: vec![],
            },
            issued_at_ns: NOW,
            expires_at_ns: NOW + 60_000_000_000,
        };
        self.sign_challenge(media_wire::SignedChallenge {
            context,
            transcript: media_wire::ChallengeTranscript::Native(transcript),
            signature: [[0; 32]; 2],
        })
    }
    fn sign_challenge(&self, mut c: media_wire::SignedChallenge) -> media_wire::SignedChallenge {
        c.signature = media_wire::split_signature(
            self.governor_key
                .sign(&c.signing_digest().unwrap())
                .to_bytes(),
        );
        c
    }
    fn grant(&self, client: &PeerMedia) -> media_wire::SignedGrant {
        let p = client.participation.as_ref().unwrap();
        let c = p.context.as_ref().unwrap();
        let call = tilo_user_identity::canonical::signing_digest(
            media_wire::MEDIA_PROOF_DOMAIN,
            &(
                3_u8,
                c.media_boot,
                &p.request.room,
                &p.request.call_id,
                c.epoch,
            ),
        )
        .unwrap();
        let publisher = tilo_user_identity::canonical::signing_digest(
            media_wire::MEDIA_PROOF_DOMAIN,
            &(
                4_u8,
                call,
                self.device.user_public_key,
                p.request.device,
                p.request.participation_id,
                p.request.generation,
            ),
        )
        .unwrap();
        let mut grant = media_wire::SignedGrant {
            version: 1,
            metadata: media_wire::GrantMetadata {
                root: self.device.user_public_key,
                request: p.request.clone(),
                epoch: 1,
                membership_generation: 1,
                namespace: media_wire::MediaNamespace { call, publisher },
                audience: "https://media.test/moq/v21/voice".into(),
                host: self.governor.device_public_key,
                host_boot: c.media_boot,
                issued_at_ns: NOW,
                expires_at_ns: NOW + 60_000_000_000,
            },
            signature: [[0; 32]; 2],
        };
        grant.signature = media_wire::split_signature(
            self.governor_key
                .sign(&grant.signing_digest().unwrap())
                .to_bytes(),
        );
        grant
    }
    fn joined_client(&self) -> (PeerMedia, u32) {
        let mut client = self.ready_client();
        let prepared = client
            .start_join(
                &self.view(),
                &"07".repeat(32),
                media_wire::MediaDirections {
                    publish: true,
                    subscribe: true,
                },
            )
            .unwrap();
        let challenge = self.challenge(&client, media_wire::GrantOperation::Join);
        let digest = challenge.transcript.signing_digest().unwrap();
        let Step::Request(completion) = client
            .advance(
                &self.view(),
                prepared.id,
                &reply(media_wire::ControlResult::Challenge(challenge)),
            )
            .unwrap()
        else {
            panic!("missing completion")
        };
        let media_wire::ControlCommand::GrantComplete { signature, .. } =
            media_wire::ControlRequest::decode(&completion.body)
                .unwrap()
                .command
        else {
            panic!("wrong completion")
        };
        self.device_key
            .verifying_key()
            .verify_strict(
                &digest,
                &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(signature)),
            )
            .unwrap();
        let grant = self.grant(&client);
        let Step::Grant(grant) = client
            .advance(
                &self.view(),
                completion.id,
                &reply(media_wire::ControlResult::Granted {
                    grant: grant.encode().unwrap(),
                }),
            )
            .unwrap()
        else {
            panic!("missing grant")
        };
        assert_eq!(grant.expires_in_ns, 60_000_000_000);
        assert_eq!(grant.presence_in_ns, 15_000_000_000);
        assert_eq!(
            grant.directions,
            media_wire::MediaDirections {
                publish: true,
                subscribe: true
            }
        );
        (client, grant.handle)
    }
}
#[test]
fn challenge_signature_full_current_context_and_local_leave_fence_precede_device_signature() {
    let mut f = Fixture::new();
    f.activate_call();
    for mutation in 0..5 {
        let mut client = f.ready_client();
        let prepared = client
            .start_join(
                &f.view(),
                &"07".repeat(32),
                media_wire::MediaDirections {
                    publish: true,
                    subscribe: true,
                },
            )
            .unwrap();
        let mut c = f.challenge(&client, media_wire::GrantOperation::Join);
        match mutation {
            0 => c.context.delivery_boot[0] ^= 1,
            1 => c.context.descriptor_digest[0] ^= 1,
            2 => c.context.delivery_host = f.governor.device_public_key,
            3 => {
                c.context.request.participation_id[0] ^= 1;
                if let media_wire::ChallengeTranscript::Native(n) = &mut c.transcript {
                    n.request = c.context.request.clone();
                }
            }
            _ => {
                assert!(matches!(
                    client.leave(&f.view(), prepared.id).unwrap(),
                    Step::Left
                ));
            }
        }
        c = f.sign_challenge(c);
        assert!(
            client
                .advance(
                    &f.view(),
                    prepared.id,
                    &reply(media_wire::ControlResult::Challenge(c))
                )
                .is_err(),
            "mutation{mutation}"
        );
    }
}
#[test]
fn verified_join_heartbeat_and_leave_keep_deadlines_and_purpose_zero_proof_typed() {
    let mut f = Fixture::new();
    f.activate_call();
    let (mut client, handle) = f.joined_client();
    let prepared = client.heartbeat(&f.view(), handle).unwrap();
    let c = f.challenge(&client, media_wire::GrantOperation::Heartbeat);
    let Step::Request(completion) = client
        .advance(
            &f.view(),
            prepared.id,
            &reply(media_wire::ControlResult::Challenge(c)),
        )
        .unwrap()
    else {
        panic!("missing heartbeat proof")
    };
    let mut context = client
        .participation
        .as_ref()
        .unwrap()
        .context
        .clone()
        .unwrap();
    context.expires_at_ns = NOW + 15_000_000_000;
    let mut presence = media_wire::SignedPresence {
        context,
        signature: [[0; 32]; 2],
    };
    presence.signature = media_wire::split_signature(
        f.governor_key
            .sign(&presence.signing_digest().unwrap())
            .to_bytes(),
    );
    let Step::Presence(result) = client
        .advance(
            &f.view(),
            completion.id,
            &reply(media_wire::ControlResult::Present(presence)),
        )
        .unwrap()
    else {
        panic!("missing presence")
    };
    assert_eq!(result.expires_in_ns, 15_000_000_000);
    let Step::Request(leaving) = client.leave(&f.view(), handle).unwrap() else {
        panic!("missing leave")
    };
    assert!(client.renew(&f.view(), handle).is_err());
    let c = f.challenge(&client, media_wire::GrantOperation::Leave);
    let Step::Request(completion) = client
        .advance(
            &f.view(),
            leaving.id,
            &reply(media_wire::ControlResult::Challenge(c)),
        )
        .unwrap()
    else {
        panic!("missing leave proof")
    };
    assert!(matches!(
        client
            .advance(
                &f.view(),
                completion.id,
                &reply(media_wire::ControlResult::Left)
            )
            .unwrap(),
        Step::Left
    ));
    assert!(client.participation.is_none());
}

#[test]
fn local_leave_does_not_wait_for_native_cleanup_before_a_fresh_join() {
    let mut f = Fixture::new();
    f.activate_call();
    let (mut client, old) = f.joined_client();
    let _leaving = client.leave(&f.view(), old).unwrap();
    let fresh = client
        .start_join(
            &f.view(),
            &"07".repeat(32),
            media_wire::MediaDirections {
                publish: false,
                subscribe: true,
            },
        )
        .unwrap();
    assert_ne!(fresh.id, old);
}

#[test]
fn read_only_call_joins_with_only_the_currently_permitted_direction() {
    let mut f = Fixture::new();
    f.activate_call();
    let mut policy = f.peer.validated_media_policy().unwrap().unwrap();
    policy.calls.get_mut(&[7; 32]).unwrap().epoch += 1;
    policy.calls.get_mut(&[7; 32]).unwrap().allowed.publish = false;
    f.peer
        .author_media_policy(&policy, &f.governor_key, time(NOW))
        .unwrap();
    let mut client = f.ready_client();
    let prepared = client
        .start_join(
            &f.view(),
            &"07".repeat(32),
            media_wire::MediaDirections {
                publish: true,
                subscribe: true,
            },
        )
        .unwrap();
    let media_wire::ControlCommand::GrantChallenge { request, .. } =
        media_wire::ControlRequest::decode(&prepared.body)
            .unwrap()
            .command
    else {
        panic!()
    };
    assert_eq!(
        request.directions,
        media_wire::MediaDirections {
            publish: false,
            subscribe: true
        }
    );
    assert!(!client.can_publish(&f.view()).unwrap());
}

#[test]
fn refreshed_hint_preserves_enrollment_pins_and_acknowledged_floor() {
    let f = Fixture::new();
    let hint = f.hint();
    let view = f.view();
    assert!(
        view.validate_enrollment_hint(view.workspace, ROOM, &f.governor, &f.host, &f.device, &hint)
            .is_ok()
    );
    assert!(
        view.validate_enrollment_hint(
            view.workspace + 1,
            ROOM,
            &f.governor,
            &f.host,
            &f.device,
            &hint
        )
        .is_err()
    );
    assert!(
        view.validate_enrollment_hint(view.workspace, ROOM, &f.host, &f.host, &f.device, &hint)
            .is_err()
    );
    assert!(
        view.validate_enrollment_hint(
            view.workspace,
            ROOM,
            &f.governor,
            &f.governor,
            &f.device,
            &hint
        )
        .is_err()
    );
    assert!(
        view.validate_enrollment_hint(view.workspace, ROOM, &f.governor, &f.host, &f.host, &hint)
            .is_err()
    );
    let mut advanced = f.view();
    advanced.generation = descriptor::GenerationExpectation::FirstAccepted {
        minimum_generation: 2,
    };
    assert!(
        advanced
            .validate_enrollment_hint(view.workspace, ROOM, &f.governor, &f.host, &f.device, &hint)
            .is_err()
    );
    let mut expired = f.view();
    expired.now_ns = NOW + 900_000_000_000;
    assert!(
        expired
            .validate_enrollment_hint(view.workspace, ROOM, &f.governor, &f.host, &f.device, &hint)
            .is_err()
    );
}

#[test]
fn current_http_custody_expires_and_zero_media_descriptor_does_not_advertise() {
    let f = Fixture::new();
    let client = f.ready_client();
    assert!(client.current(&f.view()));
    assert!(!client.supported(&f.view()));
    let mut later = f.view();
    later.now_ns = NOW + 900_000_000_000;
    assert!(!client.current(&later));
    assert!(!client.supported(&later));
}

#[test]
fn expired_grant_cannot_start_a_relay_proof() {
    let mut f = Fixture::new();
    f.activate_call();
    let (mut client, handle) = f.joined_client();
    let mut later = f.view();
    later.now_ns = NOW + 60_000_000_000;
    assert!(client.relay_challenge(&later, handle).is_err());
}

#[test]
fn relay_and_refresh_sign_exact_retained_purposes_and_close_only_their_carrier() {
    use media_wire::{
        ChallengeTranscript as C, ControlCommand as Q, ControlRequest, ControlResult as R,
        RefreshChallenge, RelayProofChallenge,
    };
    let mut f = Fixture::new();
    f.activate_call();
    let (mut client, handle) = f.joined_client();
    let initial = client.relay_challenge(&f.view(), handle).unwrap();
    let mut challenge = f.challenge(&client, media_wire::GrantOperation::Renew);
    let grant = client
        .participation
        .as_ref()
        .unwrap()
        .grant
        .as_ref()
        .unwrap();
    let relay = RelayProofChallenge {
        grant_digest: media_wire::grant_digest(&grant.encode().unwrap()).unwrap(),
        session_id: [12; 32],
        audience: grant.metadata.audience.clone(),
        host: grant.metadata.host,
        host_boot: grant.metadata.host_boot,
        nonce: [13; 32],
        issued_at_ns: NOW,
        expires_at_ns: NOW + 60_000_000_000,
    };
    challenge.transcript = C::Relay(relay.clone());
    challenge = f.sign_challenge(challenge);
    let Step::Relay(open) = client
        .advance(&f.view(), initial.id, &reply(R::Challenge(challenge)))
        .unwrap()
    else {
        panic!()
    };
    let setup = media_wire::RelaySubmission::decode(&open.setup).unwrap();
    assert_eq!(setup.session, relay.session_id);
    assert_eq!(setup.nonce, relay.nonce);
    f.device_key
        .verifying_key()
        .verify_strict(
            &C::Relay(relay.clone()).signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(setup.signature)),
        )
        .unwrap();
    let carrier = open.carrier;
    let initial = client.refresh(&f.view(), handle, carrier).unwrap();
    let mut challenge = f.challenge(&client, media_wire::GrantOperation::Renew);
    let refresh = RefreshChallenge {
        grant_digest: relay.grant_digest,
        session_id: relay.session_id,
        authorization: relay.nonce,
        audience: relay.audience.clone(),
        host: relay.host,
        host_boot: relay.host_boot,
        nonce: [14; 32],
        issued_at_ns: NOW,
        expires_at_ns: NOW + 60_000_000_000,
    };
    challenge.transcript = C::Refresh(refresh.clone());
    challenge = f.sign_challenge(challenge);
    let Step::Request(completion) = client
        .advance(&f.view(), initial.id, &reply(R::Challenge(challenge)))
        .unwrap()
    else {
        panic!()
    };
    let Q::RefreshComplete {
        session_id,
        nonce,
        signature,
    } = ControlRequest::decode(&completion.body).unwrap().command
    else {
        panic!()
    };
    assert_eq!(session_id, relay.session_id);
    assert_eq!(nonce, refresh.nonce);
    f.device_key
        .verifying_key()
        .verify_strict(
            &C::Refresh(refresh.clone()).signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(signature)),
        )
        .unwrap();
    assert!(matches!(
        client
            .advance(
                &f.view(),
                completion.id,
                &reply(R::Refreshed {
                    session_id,
                    expires_at_ns: NOW + 60_000_000_000
                })
            )
            .unwrap(),
        Step::Refreshed
    ));
    assert_eq!(
        client.participation.as_ref().unwrap().relays[&carrier].nonce,
        refresh.nonce
    );
    assert!(client.close_relay(&f.view(), handle, carrier + 1).is_err());
    let close = client.close_relay(&f.view(), handle, carrier).unwrap();
    client
        .advance(&f.view(), close.id, &reply(R::Closed))
        .unwrap();
    assert!(client.participation.as_ref().unwrap().relays.is_empty());
    assert!(client.can_publish(&f.view()).unwrap());
}

#[test]
fn common_delivery_close_fences_all_borrowers_and_product_requires_full_capabilities() {
    let fixture = Fixture::new();
    let mut media = fixture.ready_client();
    let common = media.authority.clone();
    let other = PeerMedia::with_authority(common.clone());
    media.close();
    assert!(other.current(&fixture.view()));
    common.borrow_mut().close();
    assert!(!other.current(&fixture.view()));
    assert!(
        common
            .borrow()
            .prove(&fixture.view(), "GET", "/peer/v3/events?mode=poll", &[])
            .is_err()
    );
    let incomplete = fixture.client();
    assert!(
        incomplete
            .authority
            .borrow_mut()
            .start_product_http(&fixture.view())
            .is_err()
    );
    assert!(!incomplete.http_ready());
}

#[test]
fn authorization_expiry_retires_grant_and_rejoins_only_original_intent_with_fresh_nonce() {
    let mut f = Fixture::new();
    f.activate_call();
    let (mut client, old) = f.joined_client();
    let original = client.participation.as_ref().unwrap().request.clone();
    let pending = client.renew(&f.view(), old).unwrap();
    let late = f.challenge(&client, media_wire::GrantOperation::Renew);
    client.expire_authorization();
    assert!(client.participation.is_none());
    assert!(
        client
            .advance(
                &f.view(),
                pending.id,
                &reply(media_wire::ControlResult::Challenge(late))
            )
            .is_err()
    );
    client
        .rebind_authority(&f.view(), f.ready_client().authority)
        .unwrap();
    let first = client.reauthorize(&f.view(), old).unwrap();
    let media_wire::ControlCommand::GrantChallenge { request, operation } =
        media_wire::ControlRequest::decode(&first.body)
            .unwrap()
            .command
    else {
        panic!()
    };
    assert_eq!(operation, media_wire::GrantOperation::Join);
    assert_eq!(request.call_id, original.call_id);
    assert_eq!(request.directions, original.directions);
    assert_eq!(request.generation, 0);
    assert_ne!(request.participation_id, original.participation_id);
    assert_ne!(first.id, old);
    assert!(!client.can_publish(&f.view()).unwrap());
    let retry = client.reauthorize(&f.view(), old).unwrap();
    let media_wire::ControlCommand::GrantChallenge {
        request: retried, ..
    } = media_wire::ControlRequest::decode(&retry.body)
        .unwrap()
        .command
    else {
        panic!()
    };
    assert_eq!(
        retried, request,
        "uncertain reauthorization retries keep the same fresh nonce"
    );
    client.leave(&f.view(), old).unwrap();
    assert!(client.reauthorize(&f.view(), old).is_err());
    assert!(
        client
            .advance(&f.view(), retry.id, &reply(media_wire::ControlResult::Left))
            .is_err()
    );
}

#[test]
fn authorization_expiry_does_not_resume_changed_epoch_or_explicitly_closed_intent() {
    let mut f = Fixture::new();
    f.activate_call();
    let (mut client, old) = f.joined_client();
    client.expire_authorization();
    let mut policy = f.peer.validated_media_policy().unwrap().unwrap();
    for publish in [false, true] {
        let call = policy.calls.get_mut(&[7; 32]).unwrap();
        call.epoch += 1;
        call.allowed.publish = publish;
        f.peer
            .author_media_policy(&policy, &f.governor_key, time(NOW))
            .unwrap();
    }
    client
        .rebind_authority(&f.view(), f.ready_client().authority)
        .unwrap();
    assert!(client.reauthorize(&f.view(), old).is_err());
    let mut f = Fixture::new();
    f.activate_call();
    let (mut client, old) = f.joined_client();
    client.expire_authorization();
    client.close();
    assert!(
        client
            .rebind_authority(&f.view(), f.ready_client().authority)
            .is_err()
    );
    assert!(client.reauthorize(&f.view(), old).is_err());
}
#[test]
fn join_accepts_bounded_host_clock_lead_without_extending_signed_lifetimes() {
    // The host signs with its wall clock; a browser clock trailing it by less
    // than the skew allowance must still join, and never gains lifetime.
    let mut f = Fixture::new();
    f.activate_call();
    for (lead, accepted) in [(2_000_000_000_u64, true), (6_000_000_000, false)] {
        let mut client = f.ready_client();
        let prepared = client
            .start_join(
                &f.view(),
                &"07".repeat(32),
                media_wire::MediaDirections {
                    publish: true,
                    subscribe: true,
                },
            )
            .unwrap();
        let mut c = f.challenge(&client, media_wire::GrantOperation::Join);
        c.context.issued_at_ns = NOW + lead;
        c.context.expires_at_ns = NOW + lead + 60_000_000_000;
        if let media_wire::ChallengeTranscript::Native(n) = &mut c.transcript {
            n.issued_at_ns = c.context.issued_at_ns;
            n.expires_at_ns = c.context.expires_at_ns;
        }
        c = f.sign_challenge(c);
        let result = client.advance(
            &f.view(),
            prepared.id,
            &reply(media_wire::ControlResult::Challenge(c)),
        );
        if !accepted {
            assert!(result.is_err(), "a lead beyond the allowance is rejected");
            continue;
        }
        let Step::Request(completion) = result.unwrap() else {
            panic!("missing completion")
        };
        let mut grant = f.grant(&client);
        grant.metadata.issued_at_ns = NOW + lead;
        grant.metadata.expires_at_ns = NOW + lead + 60_000_000_000;
        grant.signature = media_wire::split_signature(
            f.governor_key
                .sign(&grant.signing_digest().unwrap())
                .to_bytes(),
        );
        let Step::Grant(view) = client
            .advance(
                &f.view(),
                completion.id,
                &reply(media_wire::ControlResult::Granted {
                    grant: grant.encode().unwrap(),
                }),
            )
            .unwrap()
        else {
            panic!("missing grant")
        };
        assert_eq!(view.expires_in_ns, 60_000_000_000);
        assert_eq!(view.presence_in_ns, 15_000_000_000);
    }
}

/// A host that renewed its descriptor between this client's bootstrap and its
/// challenge answers with a transcript bound to the new descriptor. The
/// handshake still refuses it, but as a distinct "descriptor-renewed" error
/// (re-enroll with the fresh descriptor), never "scope mismatch".
#[test]
fn challenge_bound_to_a_renewed_descriptor_is_reported_as_renewed_not_scope_mismatch() {
    let f = Fixture::new();
    let view = f.view();
    let renewed_hint = descriptor::issue_transport_hint(
        &descriptor::DescriptorSettings {
            origin: "https://media.test".into(),
            delivery_capabilities: 3,
            issued_at_ns: NOW,
            expires_at_ns: NOW + 100_000_000_000,
            generation: 2,
        },
        &view.descriptor_authority(),
        &f.host_key,
    )
    .unwrap();
    let renewed =
        descriptor::verify_transport_hint(&renewed_hint, &view.descriptor_authority()).unwrap();
    let revoked = RevocationList::new();
    let issue = |descriptor: &descriptor::VerifiedDescriptor, nonce: [u8; 32]| {
        let binding = descriptor
            .server_binding(
                &view.descriptor_authority(),
                &f.device,
                auth::Carrier::Https,
                0,
                0,
                [9; 16],
            )
            .unwrap();
        let context = auth::VerificationContext {
            binding: &binding,
            capabilities: auth::CapabilityPolicy {
                supported: 3,
                allowed: 3,
            },
            credentials: auth::Credentials {
                device: &f.device,
                host: &f.host,
                device_revocations: &revoked,
                host_revocations: &revoked,
            },
            authority: &view,
            now_ns: NOW,
        };
        auth::Challenge::issue(&context, nonce, &f.host_key)
            .unwrap()
            .1
            .encode()
            .unwrap()
    };
    // Renewed descriptor: distinct error.
    let mut client = f.client();
    let prepared = client.start_http(&view).unwrap();
    let (_, _, nonce, _, _): (u16, [u8; 32], [u8; 32], u64, u64) =
        postcard::from_bytes(&prepared.body).unwrap();
    let error = client
        .advance(&view, prepared.id, &issue(&renewed, nonce))
        .err()
        .expect("a renewed descriptor never verifies");
    assert!(
        error.starts_with("descriptor-renewed:"),
        "unexpected error: {error}"
    );
    // A wrong nonce under the same descriptor is still the closed scope refusal.
    let mut client = f.client();
    let prepared = client.start_http(&view).unwrap();
    let pinned =
        descriptor::verify_transport_hint(&f.hint(), &view.descriptor_authority()).unwrap();
    let error = client
        .advance(&view, prepared.id, &issue(&pinned, [5; 32]))
        .err()
        .expect("a foreign nonce never verifies");
    assert!(
        error.contains("scope mismatch"),
        "unexpected error: {error}"
    );
}
