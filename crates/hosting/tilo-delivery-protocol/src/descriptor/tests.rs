use super::*;
use crate::auth;
use ed25519_dalek::SigningKey;
use tilo_peer_state::{SeedState, StatePeer, seed_room};
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert, sign_revocation_entry,
};
use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_join::{self as join, TransportHint};
use web_time::{Duration, SystemTime};
const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

/// The same reads the committed native owner view answers in production
/// (`tilo-joined-room`), here over a real governed `StatePeer`.
impl CommittedWorkspace for StatePeer {
    fn authority(&self) -> &DeviceCert {
        StatePeer::authority(self)
    }
    fn room(&self) -> &str {
        StatePeer::room(self)
    }
    fn governance_owner(&self) -> Option<String> {
        self.admitted_room()
            .and_then(|room| tilo_peer_state::room::governance_of(Some(room)).owner)
    }
    fn revocations(&self) -> Vec<RevocationEntry> {
        StatePeer::revocations(self)
    }
    fn has_certificate(&self, device: &DeviceCert) -> bool {
        self.certificates().contains(device)
    }
    fn is_member_device(&self, device: &[u8; 32], now: SystemTime) -> Result<bool, String> {
        Ok(self
            .governance()
            .governed_access(now)?
            .member_devices
            .contains(device))
    }
    fn has_permissions(&self, device: [u8; 32], now: SystemTime) -> bool {
        self.permissions(device, now).is_ok()
    }
}
const NS: u64 = 1_000_000_000;

struct Fixture {
    root: SigningKey,
    governor_key: SigningKey,
    governor: DeviceCert,
    host_key: SigningKey,
    host: DeviceCert,
    device_key: SigningKey,
    device: DeviceCert,
    peer: StatePeer,
    now: SystemTime,
}
fn cert(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
    expiry: Option<u64>,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: expiry,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}
impl Fixture {
    fn new() -> Self {
        Self::with_host(DeviceCapabilities::host(), None)
    }
    fn with_host(capabilities: DeviceCapabilities, expiry: Option<u64>) -> Self {
        let root = SigningKey::from_bytes(&[1; 32]);
        let governor_key = SigningKey::from_bytes(&[2; 32]);
        let host_key = SigningKey::from_bytes(&[3; 32]);
        let device_key = SigningKey::from_bytes(&[4; 32]);
        let governor = cert(
            &root,
            &governor_key,
            "governor",
            DeviceCapabilities::primary(),
            None,
        );
        let host = cert(&root, &host_key, "host", capabilities, expiry);
        let device = cert(
            &root,
            &device_key,
            "participant",
            DeviceCapabilities::member(),
            None,
        );
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(1000))
            .unwrap();
        let roster = vec![governor.clone(), host.clone(), device.clone()];
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
            root,
            governor_key,
            governor,
            host_key,
            host,
            device_key,
            device,
            peer,
            now,
        }
    }
    fn current(&self) -> DescriptorAuthority<'_> {
        DescriptorAuthority {
            peer: &self.peer,
            pinned_governor: &self.governor,
            pinned_host: &self.host,
            workspace: tilo_relay_protocol::room_workspace_id(ROOM).unwrap(),
            generation: GenerationExpectation::FirstAccepted {
                minimum_generation: 1,
            },
            now: self.now,
        }
    }
    #[expect(
        clippy::unused_self,
        reason = "fixture method keeps call sites uniform across the scenario helpers"
    )]
    fn settings(&self) -> DescriptorSettings {
        DescriptorSettings {
            origin: "https://example.test".into(),
            issued_at_ns: 1000 * NS,
            expires_at_ns: 1100 * NS,
            generation: 1,
        }
    }
    fn issue(&self) -> TransportHint {
        issue_transport_hint(&self.settings(), &self.current(), &self.host_key).unwrap()
    }
    fn resign(&self, payload: &DescriptorPayload) -> TransportHint {
        join::sign_transport_hint(
            KIND,
            serde_json::to_string(&hex::encode(encode_payload(payload).unwrap())).unwrap(),
            self.host.clone(),
            self.host.device_public_key,
            &self.host_key,
        )
        .unwrap()
    }
    fn foreign_host(&mut self) {
        let root = SigningKey::from_bytes(&[41; 32]);
        let key = SigningKey::from_bytes(&[42; 32]);
        let host = cert(
            &root,
            &key,
            "foreign-host",
            DeviceCapabilities::host(),
            None,
        );
        let grant = join::InviteGrant {
            workspace_id: self.current().workspace,
            room: ROOM.into(),
            authority: self.governor.clone(),
            issuer: self.governor.clone(),
            delegation: None,
            role: join::MemberRole::Editor,
            nonce: [8; 16],
            recipient_user_public_key: Some(host.user_public_key),
            invitee_name: None,
            expires_at_ns: 1200 * NS,
            transport: vec![],
        };
        let signed = join::issue_invitation(grant, &self.governor_key).unwrap();
        let verified = join::decode_and_verify_invitation(
            &signed,
            Some(host.user_public_key),
            &RevocationList::new(),
            1000 * NS,
        )
        .unwrap();
        let challenge =
            join::JoinChallenge::new(&verified, &host, [7; 32], [8; 32], 1100 * NS, 1000 * NS)
                .unwrap();
        let proof = join::sign_join_proof(challenge.clone(), &key).unwrap();
        let redemption =
            join::verify_join_proof(&proof, &challenge, &host, &RevocationList::new(), 1000 * NS)
                .unwrap();
        self.peer
            .governance_mut()
            .admit_invited_member(&redemption, &self.governor_key, self.now)
            .unwrap();
        self.host = host;
        self.host_key = key;
    }
}

#[test]
fn real_governed_host_issues_verified_exact_browser_and_native_routes() {
    let f = Fixture::new();
    let hint = f.issue();
    assert_eq!(hint.kind, "delivery-v3-workspace-host-v1");
    assert_eq!(hint.service_public_key, f.host.device_public_key);
    let verified = verify_transport_hint(&hint, &f.current()).unwrap();
    assert_eq!(verified.origin(), "https://example.test");
    assert_eq!(verified.webtransport_path(), "/peer/v3/connect");
    assert_eq!(verified.https_base_path(), "/peer/v3");
    assert_eq!(
        verified.native_endpoint(),
        "https://example.test/peer/v3/connect"
    );
    assert_eq!(verified.host_certificate(), &f.host);
    assert_eq!(verified.generation(), 1);
    verified.revalidate(&f.current()).unwrap();
    assert!(issue_transport_hint(&f.settings(), &f.current(), &f.device_key).is_err());
}

#[test]
fn every_payload_and_outer_pin_field_is_signed_and_independently_constrained() {
    let f = Fixture::new();
    let hint = f.issue();
    let payload = decode_payload(&hint.opaque_json).unwrap();
    for field in 0..14 {
        let mut changed = payload.clone();
        match field {
            0 => changed.protocol = 4,
            1 => changed.workspace ^= 1,
            2 => changed.workspace_root[0] ^= 1,
            3 => changed.host_root[0] ^= 1,
            4 => changed.host_device[0] ^= 1,
            5 => changed.host_certificate_digest[0] ^= 1,
            6 => changed.origin = "https://other.test".into(),
            7 => changed.webtransport_path.push('x'),
            8 => changed.https_base_path.push('x'),
            9 => changed.native_endpoint.push('x'),
            10 => changed.media_capabilities = 4,
            11 => changed.issued_at_ns += NS,
            12 => changed.expires_at_ns += NS,
            _ => changed.generation += 1,
        }
        let mut tampered = hint.clone();
        tampered.opaque_json =
            serde_json::to_string(&hex::encode(encode_payload(&changed).unwrap())).unwrap();
        assert!(
            verify_transport_hint(&tampered, &f.current()).is_err(),
            "unsigned field {field}"
        );
        if field <= 10 {
            assert!(
                verify_transport_hint(&f.resign(&changed), &f.current()).is_err(),
                "signed invalid field {field}"
            );
        }
    }
    for field in 0..4 {
        let mut changed = hint.clone();
        match field {
            0 => changed.kind = join::TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1.into(),
            1 => changed.service_public_key[0] ^= 1,
            2 => changed.host_signature[0] ^= 1,
            _ => {
                changed.host.device_label.push('x');
                sign_device_cert(&mut changed.host, &f.root).unwrap();
            }
        }
        assert!(verify_transport_hint(&changed, &f.current()).is_err());
    }
}

#[test]
fn canonical_payload_literal_and_existing_signature_contract_are_preserved() {
    let payload = DescriptorPayload {
        protocol: 3,
        workspace: 7,
        workspace_root: [1; 32],
        host_root: [2; 32],
        host_device: [3; 32],
        host_certificate_digest: [4; 32],
        origin: "https://a.test".into(),
        webtransport_path: "/peer/v3/connect".into(),
        https_base_path: "/peer/v3".into(),
        native_endpoint: "https://a.test/peer/v3/connect".into(),
        media_capabilities: 0,
        issued_at_ns: 10,
        expires_at_ns: 20,
        generation: 1,
    };
    let mut golden = vec![3, 7];
    for value in [1, 2, 3, 4] {
        golden.extend([value; 32]);
    }
    golden.push(14);
    golden.extend(b"https://a.test");
    golden.push(16);
    golden.extend(b"/peer/v3/connect");
    golden.push(8);
    golden.extend(b"/peer/v3");
    golden.push(30);
    golden.extend(b"https://a.test/peer/v3/connect");
    golden.extend([0, 10, 20, 1]);
    assert_eq!(encode_payload(&payload).unwrap(), golden);
    let f = Fixture::new();
    let hint = f.issue();
    join::verify_transport_hint_at(&hint, &RevocationList::new(), 1000 * NS).unwrap();
    let mut preimage = b"tilo-delivery-v3-descriptor\0".to_vec();
    preimage.extend(postcard::to_stdvec(&hint).unwrap());
    assert_eq!(
        verify_transport_hint(&hint, &f.current())
            .unwrap()
            .descriptor_digest(),
        *blake3::hash(&preimage).as_bytes()
    );
}

#[test]
fn codec_rejects_noncanonical_hex_json_postcard_and_truncation() {
    let f = Fixture::new();
    let hint = f.issue();
    let payload = decode_payload(&hint.opaque_json).unwrap();
    let bytes = encode_payload(&payload).unwrap();
    for end in 0..bytes.len() {
        let text = serde_json::to_string(&hex::encode(&bytes[..end])).unwrap();
        assert!(decode_payload(&text).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    let mut overlong = bytes;
    overlong.splice(0..1, [0x83, 0]);
    for bytes in [trailing, overlong] {
        assert!(decode_payload(&serde_json::to_string(&hex::encode(bytes)).unwrap()).is_err());
    }
    assert!(decode_payload(&format!(" {}", hint.opaque_json)).is_err());
    assert!(decode_payload(&hint.opaque_json.to_uppercase()).is_err());
    assert!(decode_payload("\"\\u0030\"").is_err());
    assert!(decode_payload("\"0\"").is_err());
    assert!(decode_payload("{}").is_err());
    assert!(decode_payload(&format!("\"{}\"", "0".repeat(16386))).is_err());
}

#[test]
fn signed_url_aliases_and_certificate_string_bounds_fail_closed() {
    let f = Fixture::new();
    for origin in [
        "http://example.test",
        "https://user@example.test",
        "https://example.test/",
        "https://example.test?q=x",
        "https://example.test#x",
        "https://EXAMPLE.test",
        "https://example.test\\evil",
        "https://example.test:0",
        "https://example.test:65536",
        "https://a..test",
        "https://example.test\n",
    ] {
        let mut settings = f.settings();
        settings.origin = origin.into();
        assert!(
            issue_transport_hint(&settings, &f.current(), &f.host_key).is_err(),
            "{origin}"
        );
    }
    for origin in [
        "https://example.test:443",
        "https://localhost:8443",
        "https://127.0.0.1:8443",
        "https://[::1]:8443",
    ] {
        let mut settings = f.settings();
        settings.origin = origin.into();
        issue_transport_hint(&settings, &f.current(), &f.host_key).unwrap();
    }
    let mut payload = decode_payload(&f.issue().opaque_json).unwrap();
    payload.origin = "x".repeat(2049);
    assert!(
        decode_payload(
            &serde_json::to_string(&hex::encode(encode_payload(&payload).unwrap())).unwrap()
        )
        .is_err()
    );
    payload.origin = "https://example.test".into();
    payload.native_endpoint = "x".repeat(2113);
    assert!(
        decode_payload(
            &serde_json::to_string(&hex::encode(encode_payload(&payload).unwrap())).unwrap()
        )
        .is_err()
    );
    let mut cert = f.host;
    cert.device_label = "a".repeat(1024);
    certificate_digest(&cert).unwrap();
    cert.device_label.push('a');
    assert!(certificate_digest(&cert).is_err());
}

#[test]
fn generation_floor_rollback_equivocation_and_persisted_pin_are_checked() {
    let f = Fixture::new();
    let hint = f.issue();
    let verified = verify_transport_hint(&hint, &f.current()).unwrap();
    let mut current = f.current();
    current.generation = GenerationExpectation::Current {
        generation: 1,
        descriptor_digest: verified.descriptor_digest(),
    };
    verify_transport_hint(&hint, &current).unwrap();
    let mut settings = f.settings();
    settings.expires_at_ns += NS;
    assert!(issue_transport_hint(&settings, &current, &f.host_key).is_err());
    settings.generation = 2;
    let newer = issue_transport_hint(&settings, &current, &f.host_key).unwrap();
    let new_verified = verify_transport_hint(&newer, &current).unwrap();
    current.generation = GenerationExpectation::Current {
        generation: 2,
        descriptor_digest: new_verified.descriptor_digest(),
    };
    assert!(verify_transport_hint(&hint, &current).is_err());
    let persisted = postcard::to_stdvec(&newer).unwrap();
    let restored = postcard::from_bytes(&persisted).unwrap();
    verify_transport_hint(&restored, &current).unwrap();
    current.generation = GenerationExpectation::FirstAccepted {
        minimum_generation: 3,
    };
    assert!(verify_transport_hint(&newer, &current).is_err());
    current.generation = GenerationExpectation::FirstAccepted {
        minimum_generation: 0,
    };
    assert!(verify_transport_hint(&newer, &current).is_err());
}

#[test]
fn current_governance_revocation_host_pin_and_primary_only_mode_are_mandatory() {
    let mut f = Fixture::new();
    let hint = f.issue();
    let verified = verify_transport_hint(&hint, &f.current()).unwrap();
    let mut entry = RevocationEntry {
        user_public_key: f.root.verifying_key().to_bytes(),
        revoked_device_public_key: f.host.device_public_key,
        revoked_at_ns: 1000 * NS,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &f.root).unwrap();
    f.peer
        .governance_mut()
        .publish_governed_revocations(&[entry], &f.governor_key, f.now)
        .unwrap();
    assert!(verified.revalidate(&f.current()).is_err());
    assert!(verify_transport_hint(&hint, &f.current()).is_err());
    let f = Fixture::new();
    let hint = f.issue();
    let mut current = f.current();
    current.pinned_host = &f.device;
    assert!(verify_transport_hint(&hint, &current).is_err());
    current = f.current();
    current.pinned_governor = &f.device;
    assert!(verify_transport_hint(&hint, &current).is_err());
    current = f.current();
    current.workspace ^= 1;
    assert!(verify_transport_hint(&hint, &current).is_err());
    let empty = StatePeer::new(
        ROOM,
        f.governor.clone(),
        vec![f.host.clone()],
        RevocationList::new(),
        f.now,
    )
    .unwrap();
    current = f.current();
    current.peer = &empty;
    assert!(verify_transport_hint(&hint, &current).is_err());
    let f = Fixture::with_host(DeviceCapabilities::primary(), None);
    assert!(issue_transport_hint(&f.settings(), &f.current(), &f.host_key).is_err());
}

#[test]
fn real_foreign_root_host_role_removal_invalidates_existing_descriptor() {
    let mut f = Fixture::new();
    f.foreign_host();
    let hint = f.issue();
    let verified = verify_transport_hint(&hint, &f.current()).unwrap();
    assert!(
        f.peer
            .governance_mut()
            .remove_member(f.host.user_public_key, &f.governor_key, f.now)
            .unwrap()
    );
    assert!(verify_transport_hint(&hint, &f.current()).is_err());
    assert!(
        verified
            .client_scope(&f.current(), &f.device, auth::Carrier::Https)
            .is_err()
    );
}

#[test]
fn descriptor_lifetime_and_host_certificate_expiry_are_strict() {
    let f = Fixture::new();
    for (issued, expires) in [(1001, 1100), (1000, 1000), (1000, 1901), (999, 999)] {
        let mut settings = f.settings();
        settings.issued_at_ns = issued * NS;
        settings.expires_at_ns = expires * NS;
        assert!(issue_transport_hint(&settings, &f.current(), &f.host_key).is_err());
    }
    let mut settings = f.settings();
    settings.expires_at_ns = 1900 * NS;
    issue_transport_hint(&settings, &f.current(), &f.host_key).unwrap();
    let hint = f.issue();
    let mut current = f.current();
    current.now = SystemTime::UNIX_EPOCH + Duration::from_secs(1100);
    assert!(verify_transport_hint(&hint, &current).is_err());
    let f = Fixture::with_host(DeviceCapabilities::host(), Some(1050 * NS));
    assert!(issue_transport_hint(&f.settings(), &f.current(), &f.host_key).is_err());
    let mut settings = f.settings();
    settings.expires_at_ns = 1050 * NS;
    issue_transport_hint(&settings, &f.current(), &f.host_key).unwrap();
}

impl auth::Authority for Fixture {
    fn authorize(
        &self,
        binding: &auth::Binding,
        device: &DeviceCert,
        host: &DeviceCert,
        now_ns: u64,
    ) -> Result<(), auth::AuthError> {
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_nanos(now_ns))
            .unwrap();
        if binding.workspace != self.current().workspace
            || host != &self.host
            || !self.peer.certificates().contains(device)
            || self
                .peer
                .permissions(device.device_public_key, now)
                .is_err()
            || self.peer.permissions(host.device_public_key, now).is_err()
        {
            return Err(auth::AuthError::PolicyDenied);
        }
        Ok(())
    }
}
#[test]
fn verified_descriptor_derives_scopes_for_real_asymmetric_authentication() {
    let f = Fixture::new();
    let verified = verify_transport_hint(&f.issue(), &f.current()).unwrap();
    let scope = verified
        .client_scope(&f.current(), &f.device, auth::Carrier::Https)
        .unwrap();
    assert_eq!(scope.host, f.host.device_public_key);
    assert_eq!(scope.device_root, f.device.user_public_key);
    assert_eq!(scope.workspace_root, f.governor.user_public_key);
    assert_eq!(scope.descriptor_digest, verified.descriptor_digest());
    assert_eq!(scope.descriptor_generation, 1);
    assert_eq!(scope.origin, "https://example.test");
    assert!(
        verified
            .server_binding(&f.current(), &f.device, auth::Carrier::Https, [0; 16])
            .is_err()
    );
    let binding = verified
        .server_binding(&f.current(), &f.device, auth::Carrier::Https, [9; 16])
        .unwrap();
    let revoked = RevocationList::new();
    let credentials = auth::Credentials {
        device: &f.device,
        host: &f.host,
        device_revocations: &revoked,
        host_revocations: &revoked,
    };
    let current = auth::VerificationContext {
        binding: &binding,
        credentials,
        authority: &f,
        now_ns: 1000 * NS,
    };
    let expected = auth::ClientExpectations {
        scope,
        credentials,
        authority: &f,
        now_ns: 1000 * NS,
    };
    let (challenge, proof) = auth::Challenge::issue(&current, [6; 32], &f.host_key).unwrap();
    let client = auth::verify_server(&expected, [6; 32], &proof)
        .unwrap()
        .client_proof(&f.device_key)
        .unwrap();
    let session = challenge.finish(&client, &current).unwrap();
    assert_eq!(session.binding().server_boot_id, [9; 16]);
}

#[test]
fn matching_zero_roots_are_rejected_before_scope_acceptance() {
    let f = Fixture::new();
    let valid = decode_payload(&f.issue().opaque_json).unwrap();
    for (zero_workspace_root, zero_host_root) in [(true, false), (false, true), (true, true)] {
        let mut governor = f.governor.clone();
        let mut host = f.host.clone();
        let mut payload = valid.clone();
        if zero_workspace_root {
            governor.user_public_key = [0; 32];
            payload.workspace_root = [0; 32];
        }
        if zero_host_root {
            host.user_public_key = [0; 32];
            payload.host_root = [0; 32];
        }
        payload.host_certificate_digest = certificate_digest(&host).unwrap();
        let mut current = f.current();
        current.pinned_governor = &governor;
        current.pinned_host = &host;
        assert_eq!(
            payload.workspace_root,
            current.pinned_governor.user_public_key
        );
        assert_eq!(payload.host_root, current.pinned_host.user_public_key);
        // Isolate the descriptor identity invariant: matching pins must not make
        // zero roots acceptable, independently of certificate verification.
        assert!(matches!(
            validate_payload(&payload, &current),
            Err(DescriptorError::Scope)
        ));
    }
}

#[test]
fn persisted_hint_metadata_recovers_expired_generation_without_authorizing() {
    let mut f = Fixture::new();
    let hint = f.issue();
    f.now = SystemTime::UNIX_EPOCH + Duration::from_secs(1200);
    assert!(verify_transport_hint(&hint, &f.current()).is_err());
    let metadata = inspect_persisted_hint(&hint, &f.host).unwrap();
    assert_eq!(metadata.generation, 1);
    assert_eq!(metadata.issued_at_ns, 1000 * NS);
    assert_eq!(metadata.expires_at_ns, 1100 * NS);
    assert!(inspect_persisted_hint(&hint, &f.governor).is_err());
    let mut altered = hint.clone();
    altered.opaque_json.push(' ');
    assert!(inspect_persisted_hint(&altered, &f.host).is_err());
    let mut altered = hint.clone();
    altered.host_signature[0] ^= 1;
    assert!(inspect_persisted_hint(&altered, &f.host).is_err());
    let mut payload = decode_payload(&hint.opaque_json).unwrap();
    payload.generation = 0;
    assert!(inspect_persisted_hint(&f.resign(&payload), &f.host).is_err());
    payload.generation = 1;
    payload.expires_at_ns = payload.issued_at_ns + 901 * NS;
    assert!(inspect_persisted_hint(&f.resign(&payload), &f.host).is_err());
}

#[test]
fn signed_known_media_bits_verify_and_unknown_bits_fail() {
    let f = Fixture::new();
    let hint = f.issue();
    let mut payload = decode_payload(&hint.opaque_json).unwrap();
    assert_eq!(payload.media_capabilities, 0);
    payload.media_capabilities = 1;
    let signed = f.resign(&payload);
    let verified = verify_transport_hint(&signed, &f.current()).unwrap();
    assert_eq!(verified.media_capabilities(), MEDIA_VOICE_V1);
    assert!(issue_transport_hint_with_media(&f.settings(), &f.current(), &f.host_key, 4).is_err());
    let live = MEDIA_LIVE_V1;
    assert!(
        issue_transport_hint_with_media(
            &f.settings(),
            &f.current(),
            &f.host_key,
            MEDIA_VOICE_V1 | live
        )
        .is_ok()
    );
    let issued =
        issue_transport_hint_with_media(&f.settings(), &f.current(), &f.host_key, MEDIA_VOICE_V1)
            .unwrap();
    assert_eq!(
        verify_transport_hint(&issued, &f.current())
            .unwrap()
            .media_capabilities(),
        MEDIA_VOICE_V1
    );
    inspect_persisted_hint(&signed, &f.host).unwrap();
    payload.media_capabilities = 4;
    let unknown = f.resign(&payload);
    assert!(verify_transport_hint(&unknown, &f.current()).is_err());
    assert!(inspect_persisted_hint(&unknown, &f.host).is_err());
}

#[test]
fn pinned_hint_scope_preserves_exact_signed_routing_without_current_authority() {
    let mut f = Fixture::new();
    let hint = f.issue();
    let scope = inspect_pinned_hint_scope(&hint, &f.host).unwrap();
    assert_eq!(scope.origin, f.settings().origin);
    assert_eq!(scope.workspace, f.current().workspace);
    assert_eq!(scope.workspace_root, f.governor.user_public_key);
    assert_eq!(
        scope.metadata,
        inspect_persisted_hint(&hint, &f.host).unwrap()
    );
    assert!(inspect_pinned_hint_scope(&hint, &f.governor).is_err());
    for mutation in 0..3 {
        let mut payload = decode_payload(&hint.opaque_json).unwrap();
        match mutation {
            0 => payload.origin = "http://example.test".into(),
            1 => payload.workspace_root = [0; 32],
            _ => payload.expires_at_ns = payload.issued_at_ns,
        }
        assert!(inspect_pinned_hint_scope(&f.resign(&payload), &f.host).is_err());
    }
    // Historical inspection deliberately does not grant current use at expiry.
    f.now = SystemTime::UNIX_EPOCH + Duration::from_secs(1200);
    let expired = inspect_pinned_hint_scope(&hint, &f.host).unwrap();
    assert!(expired.metadata.expires_at_ns < 1200 * NS);
    assert!(verify_transport_hint(&hint, &f.current()).is_err());
    // Another valid signed audience/scope stays inert and cannot become the
    // independently enrolled audience merely because inspection succeeded.
    let mut payload = decode_payload(&hint.opaque_json).unwrap();
    payload.origin = "https://other.test".into();
    payload.native_endpoint = format!("{}{WEBTRANSPORT_PATH}", payload.origin);
    payload.workspace += 1;
    let other = inspect_pinned_hint_scope(&f.resign(&payload), &f.host).unwrap();
    assert_ne!(other.origin, scope.origin);
    assert_ne!(other.workspace, scope.workspace);
    assert!(verify_transport_hint(&f.resign(&payload), &f.current()).is_err());
}

#[test]
fn descriptors_may_advertise_live_beside_voice_but_nothing_else() {
    assert_eq!(MEDIA_CAPABILITIES, MEDIA_VOICE_V1 | MEDIA_LIVE_V1);
    assert_eq!(MEDIA_VOICE_V1 & MEDIA_LIVE_V1, 0);
}
