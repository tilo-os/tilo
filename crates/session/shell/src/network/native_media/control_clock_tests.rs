//! Host clock lead: the media host signs with its wall clock, which may lead
//! this device's clock. Signed issuance within the allowance is accepted; the
//! remaining lifetime never exceeds the signed duration.
use super::super::*;
use super::*;
use ed25519_dalek::SigningKey;
use qos_peer_state::{SeedDocument, SeedState, room::media, seed_room};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};
use ui::automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const NOW: u64 = 1_000_000_000_000;
const CALL: [u8; 32] = [7; 32];

fn cert(root: &SigningKey, key: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
    let mut value = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "native media clock fixture".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut value, root).unwrap();
    value
}

struct Fixture {
    governor: DeviceCert,
    governor_key: SigningKey,
    host: DeviceCert,
    host_key: SigningKey,
    device: DeviceCert,
    device_key: SigningKey,
    peer: StatePeer,
    generation: descriptor::GenerationExpectation,
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
        let mut chat = AutoCommit::new();
        let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
        let channel = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
        chat.put(&channel, "kind", "voice").unwrap();
        chat.put(&channel, "workspace", "home").unwrap();
        peer.install_seed(
            SeedState {
                room_changes: vec![],
                documents: vec![SeedDocument {
                    app_id: "so.tilo.chat".into(),
                    record_id: "".into(),
                    kind: "chat".into(),
                    bytes: chat.save(),
                }],
            },
            &governor_key,
            now,
        )
        .unwrap();
        let binding = peer
            .resolve_media_binding("so.tilo.chat", "", "voice")
            .unwrap();
        let policy = media::RoomMediaV1 {
            version: 1,
            room: *uuid::Uuid::parse_str(ROOM).unwrap().as_bytes(),
            authorization_revision: 1,
            authorization_basis: media::security_basis(peer.admitted_room().unwrap(), &[]).unwrap(),
            calls: BTreeMap::from([(
                CALL,
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
        peer.author_media_policy(&policy, &governor_key, now)
            .unwrap();
        let mut fixture = Self {
            governor,
            governor_key,
            host,
            host_key,
            device,
            device_key,
            peer,
            generation: descriptor::GenerationExpectation::FirstAccepted {
                minimum_generation: 1,
            },
        };
        let accepted = descriptor::verify_transport_hint(
            &fixture.hint(),
            &fixture.view().descriptor_authority(),
        )
        .unwrap();
        fixture.generation = descriptor::GenerationExpectation::Current {
            generation: accepted.generation(),
            descriptor_digest: accepted.descriptor_digest(),
        };
        fixture
    }
    fn view(&self) -> OwnerView<'_> {
        OwnerView {
            peer: &self.peer,
            key: &self.device_key,
            governor: &self.governor,
            host: &self.host,
            device: &self.device,
            workspace: tilo_relay::room_workspace_id(ROOM).unwrap(),
            continuity: 1,
            generation: self.generation,
            now_ns: NOW,
        }
    }
    fn hint(&self) -> qos_peer_state::join::TransportHint {
        descriptor::issue_transport_hint_with_media(
            &descriptor::DescriptorSettings {
                origin: "https://media.test".into(),
                delivery_capabilities: 3,
                issued_at_ns: NOW,
                expires_at_ns: NOW + 100_000_000_000,
                generation: 1,
            },
            &self.view().descriptor_authority(),
            &self.host_key,
            descriptor::MEDIA_VOICE_V1,
        )
        .unwrap()
    }
    /// Real HTTP handshake against the verified descriptor.
    fn ready_control(&self) -> NativeControl {
        let view = self.view();
        let mut control = NativeControl::new(&view, &self.hint()).unwrap();
        let prepared = control.start_http(&view).unwrap();
        let (_, _, nonce, _, _): (u16, [u8; 32], [u8; 32], u64, u64) =
            postcard::from_bytes(&prepared.body).unwrap();
        let binding = control
            .descriptor
            .server_binding(
                &view.descriptor_authority(),
                &self.device,
                Carrier::Https,
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
        let Step::Request(prepared) = control
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
            control.advance(&view, prepared.id, &ready).unwrap(),
            Step::HttpReady
        ));
        control
    }
    fn challenge(&self, control: &NativeControl, issued: u64, expires: u64) -> SignedChallenge {
        let mut request = control.participation.as_ref().unwrap().request.clone();
        request.generation = 7;
        let directions = MediaDirections {
            publish: true,
            subscribe: true,
        };
        let transcript = media_wire::NativeChallenge {
            request: request.clone(),
            operation: GrantOperation::Join,
            epoch: 1,
            membership_generation: 1,
            directions,
            audience: "https://media.test/moq/v21/voice".into(),
            host: self.governor.device_public_key,
            host_boot: [10; 32],
            nonce: [11; 32],
            issued_at_ns: issued,
            expires_at_ns: expires,
        };
        let session = &control.session.as_ref().unwrap().proof.transcript;
        let context = MediaContext {
            version: 1,
            descriptor_digest: control.descriptor.descriptor_digest(),
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
            directions,
            inventory: media_wire::PublisherInventory {
                revision: 1,
                namespaces: vec![],
            },
            issued_at_ns: issued,
            expires_at_ns: expires,
        };
        let mut signed = SignedChallenge {
            context,
            transcript: ChallengeTranscript::Native(transcript),
            signature: [[0; 32]; 2],
        };
        signed.signature = media_wire::split_signature(
            self.governor_key
                .sign(&signed.signing_digest().unwrap())
                .to_bytes(),
        );
        signed
    }
    fn grant(&self, control: &NativeControl, issued: u64, expires: u64) -> SignedGrant {
        let p = control.participation.as_ref().unwrap();
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
        let mut grant = SignedGrant {
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
                issued_at_ns: issued,
                expires_at_ns: expires,
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
}

fn reply(result: ControlResult) -> Vec<u8> {
    ControlReply { version: 1, result }.encode().unwrap()
}

#[test]
fn native_join_accepts_bounded_host_clock_lead_without_extending_signed_lifetimes() {
    let f = Fixture::new();
    let lifetime = 60_000_000_000;
    for (lead, accepted) in [
        (0_u64, true),
        (2_000_000_000, true),
        (CLOCK_SKEW_NS, true),
        (CLOCK_SKEW_NS + 1, false),
        (6_000_000_000, false),
    ] {
        let mut control = f.ready_control();
        let view = f.view();
        let prepared = control
            .start_join(
                &view,
                &"07".repeat(32),
                MediaDirections {
                    publish: true,
                    subscribe: true,
                },
            )
            .unwrap();
        let issued = NOW + lead;
        let challenge = f.challenge(&control, issued, issued + lifetime);
        let result = control.advance(
            &view,
            prepared.id,
            &reply(ControlResult::Challenge(challenge)),
        );
        if !accepted {
            assert!(
                result.is_err(),
                "lead {lead} beyond the allowance is rejected"
            );
            continue;
        }
        let Step::Request(completion) = result.unwrap() else {
            panic!("missing grant completion for lead {lead}")
        };
        let grant = f.grant(&control, issued, issued + lifetime);
        let Step::Grant(granted) = control
            .advance(
                &view,
                completion.id,
                &reply(ControlResult::Granted {
                    grant: grant.encode().unwrap(),
                }),
            )
            .unwrap()
        else {
            panic!("missing grant for lead {lead}")
        };
        assert_eq!(
            granted.expires_in_ns, lifetime,
            "lead {lead} never extends the signed grant lifetime"
        );
        assert_eq!(
            granted.presence_in_ns, 15_000_000_000,
            "presence is measured from the earlier of signed issuance and local time"
        );
    }
}

#[test]
fn native_grant_issued_beyond_clock_allowance_is_rejected_after_valid_challenge() {
    let f = Fixture::new();
    let mut control = f.ready_control();
    let view = f.view();
    let prepared = control
        .start_join(
            &view,
            &"07".repeat(32),
            MediaDirections {
                publish: true,
                subscribe: true,
            },
        )
        .unwrap();
    let challenge = f.challenge(&control, NOW, NOW + 60_000_000_000);
    let Step::Request(completion) = control
        .advance(
            &view,
            prepared.id,
            &reply(ControlResult::Challenge(challenge)),
        )
        .unwrap()
    else {
        panic!("missing grant completion")
    };
    let issued = NOW + CLOCK_SKEW_NS + 1;
    let grant = f.grant(&control, issued, issued + 60_000_000_000);
    assert!(
        control
            .advance(
                &view,
                completion.id,
                &reply(ControlResult::Granted {
                    grant: grant.encode().unwrap(),
                }),
            )
            .is_err()
    );
}
