use super::*;
use crate::delivery_v3::auth::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};
const NS: u64 = 1_000_000_000;
struct Fixture {
    binding: Binding,
    root: ed25519_dalek::SigningKey,
    device_key: ed25519_dalek::SigningKey,
    host_key: ed25519_dalek::SigningKey,
    device: DeviceCert,
    host: DeviceCert,
    device_revocations: RevocationList,
    host_revocations: RevocationList,
    admitted: AtomicBool,
    now: u64,
}
impl Authority for Fixture {
    fn authorize(
        &self,
        binding: &Binding,
        device: &DeviceCert,
        host: &DeviceCert,
        _: u64,
    ) -> Result<(), AuthError> {
        if !self.admitted.load(Ordering::SeqCst)
            || binding.workspace != 7
            || device != &self.device
            || host != &self.host
        {
            return Err(AuthError::PolicyDenied);
        }
        Ok(())
    }
}
impl Fixture {
    fn new() -> Self {
        let root = ed25519_dalek::SigningKey::from_bytes(&[10; 32]);
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        let host_key = ed25519_dalek::SigningKey::from_bytes(&[12; 32]);
        let cert = |key: &ed25519_dalek::SigningKey, capabilities| {
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: key.verifying_key().to_bytes(),
                device_label: "v3-test".into(),
                capabilities,
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).unwrap();
            cert
        };
        let device = cert(&device_key, DeviceCapabilities::primary());
        let host = cert(&host_key, DeviceCapabilities::host());
        let binding = Binding {
            workspace: 7,
            workspace_root: root.verifying_key().to_bytes(),
            device: device.device_public_key,
            device_root: device.user_public_key,
            host: host.device_public_key,
            host_root: host.user_public_key,
            origin: "https://example.test".into(),
            descriptor_generation: 1,
            descriptor_digest: [8; 32],
            server_boot_id: [1; 16],
            carrier: Carrier::Https,
            requested_capabilities: 3,
            mandatory_capabilities: 1,
        };
        Self {
            binding,
            root,
            device_key,
            host_key,
            device,
            host,
            device_revocations: RevocationList::new(),
            host_revocations: RevocationList::new(),
            admitted: AtomicBool::new(true),
            now: 100 * NS,
        }
    }
    fn context(&self) -> VerificationContext<'_> {
        VerificationContext {
            binding: &self.binding,
            capabilities: CapabilityPolicy {
                supported: 3,
                allowed: 3,
            },
            credentials: Credentials {
                device: &self.device,
                host: &self.host,
                device_revocations: &self.device_revocations,
                host_revocations: &self.host_revocations,
            },
            authority: self,
            now_ns: self.now,
        }
    }
    fn client_context(&self) -> ClientExpectations<'_> {
        let server = self.context();
        ClientExpectations {
            scope: ClientScope::from(&self.binding),
            capabilities: server.capabilities,
            credentials: server.credentials,
            authority: self,
            now_ns: self.now,
        }
    }
    fn issue(&self) -> (Challenge, ServerProof) {
        Challenge::issue(&self.context(), [2; 32], &self.host_key).unwrap()
    }
    fn revoke(&mut self, host: bool) {
        let mut entry = RevocationEntry {
            user_public_key: self.root.verifying_key().to_bytes(),
            revoked_device_public_key: if host {
                self.host.device_public_key
            } else {
                self.device.device_public_key
            },
            revoked_at_ns: self.now,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &self.root).unwrap();
        if host {
            self.host_revocations.insert(entry).unwrap();
        } else {
            self.device_revocations.insert(entry).unwrap();
        }
    }
}

impl Fixture {
    fn session(&self) -> (VerifiedSession, [u8; 32]) {
        let (challenge, server) = self.issue();
        let proof = verify_server(&self.client_context(), [2; 32], &server)
            .unwrap()
            .client_proof(&self.device_key)
            .unwrap();
        let session = challenge.finish(&proof, &self.context()).unwrap();
        let digest = session_binding_digest(
            session.binding(),
            session.session_id(),
            session.capabilities(),
            session.expires_at_ns(),
        )
        .unwrap();
        (session, digest)
    }
    fn proof(
        &self,
        session: &VerifiedSession,
        digest: [u8; 32],
        request: &Request<'_>,
    ) -> RequestProof {
        RequestProof::sign(
            session.session_id(),
            digest,
            request,
            self.now,
            self.now.saturating_add(60_u64.saturating_mul(NS)),
            &self.device_key,
        )
        .unwrap()
    }
}

#[test]
fn canonical_targets_and_explicit_bounds() {
    for (method, target) in [
        ("get", "/peer/v3/events"),
        ("PATCH", "/peer/v3/state"),
        ("GET", "https://example.test/peer/v3/events"),
        ("GET", "//evil/events"),
        ("GET", "/peer/v3/../events"),
        ("GET", "/peer//events"),
        ("GET", "/peer/events/"),
        ("GET", "/peer/%76%33/events"),
        ("GET", "/peer/v3/events#x"),
        ("GET", "/peer/v3/events?"),
        ("GET", "/peer/v3/events?mode=poll&cursor=a"),
        ("GET", "/peer/v3/events?cursor=a&cursor=b"),
        ("GET", "/peer/v3/events?cursor=%61"),
        ("GET", "/peer/v3/events?cursor=%2f"),
        ("GET", "/peer/v3/events?cursor=%25"),
        ("GET", "/peer/v3/events?cursor=%00"),
        ("GET", "/peer/v3/events?cursor=%7F"),
        ("GET", "/peer/v3/events?cursor=%"),
        ("GET", "/peer/v3/events?cursor=a+b"),
        ("GET", "/peer/v3/events?cursor=a;b"),
        ("GET", "/peer/v3/events?cursor=a=b"),
        ("GET", "/peer/v3/events?cursor=a?b"),
        ("GET", "/peer/v3/é"),
        ("GET", "/peer/v3/e vents"),
    ] {
        assert!(
            Request::new(method, target, b"").is_err(),
            "{method} {target}"
        );
    }
    for target in [
        "/peer/v3/events?cursor=&mode=stream",
        "/peer/v3/events?cursor=%2F%20%FF&mode=poll",
    ] {
        Request::new("GET", target, b"").unwrap();
    }
    let cursor = format!("/peer/v3/events?cursor={}&mode=stream", "%2F".repeat(4096));
    Request::new("GET", &cursor, b"").unwrap();
    let too_long = format!("/peer/v3/events?cursor={}", "%2F".repeat(4097));
    assert!(Request::new("GET", &too_long, b"").is_err());
    let path = format!("/{}", "a".repeat(1023));
    Request::new("POST", &path, &vec![0; 1024 * 1024]).unwrap();
    assert!(Request::new("POST", &(path + "a"), b"").is_err());
    assert!(Request::new("POST", "/peer/v3/state", &vec![0; 1024 * 1024 + 1]).is_err());
    let name = "a".repeat(64);
    Request::new("GET", &format!("/peer/v3/events?{name}=x"), b"").unwrap();
    assert!(Request::new("GET", &format!("/peer/v3/events?{name}a=x"), b"").is_err());
    let params = (0..32)
        .map(|n| format!("p{n:02}=x"))
        .collect::<Vec<_>>()
        .join("&");
    Request::new("GET", &format!("/peer/v3/events?{params}"), b"").unwrap();
    assert!(Request::new("GET", &format!("/peer/v3/events?{params}&p32=x"), b"").is_err());
    let target = format!(
        "/peer/v3/events?a={}&b={}",
        "a".repeat(12288),
        "b".repeat(4075)
    );
    assert_eq!(target.len(), 16384);
    Request::new("GET", &target, b"").unwrap();
    assert!(Request::new("GET", &(target + "b"), b"").is_err());
}

#[test]
fn independent_wire_and_request_preimage_fixture() {
    let proof = RequestProof {
        wire: ProofWire {
            protocol: 3,
            session_id: [1; 16],
            nonce: [3; 32],
            issued_at_ns: 100,
            expires_at_ns: 120,
            signature: [4; 64],
        },
    };
    let mut wire = vec![3];
    wire.extend([1; 16]);
    wire.extend([3; 32]);
    wire.extend([100, 120, 64]);
    wire.extend([4; 64]);
    assert_eq!(proof.encode().unwrap(), wire);
    let request = Request::new("POST", "/peer/v3/state", b"").unwrap();
    let mut preimage = b"tilo-delivery-v3-http-request\0".to_vec();
    preimage.extend([3]);
    preimage.extend([1; 16]);
    preimage.extend([2; 32]);
    preimage.extend([1, 14]);
    preimage.extend(b"/peer/v3/state");
    preimage.extend(
        hex::decode("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262").unwrap(),
    );
    preimage.extend([3; 32]);
    preimage.extend([100, 120]);
    assert_eq!(
        request_digest(&proof.wire, [2; 32], &request).unwrap(),
        *blake3::hash(&preimage).as_bytes()
    );
}

#[test]
fn bounded_canonical_codec_rejects_all_truncations_and_aliases() {
    let f = Fixture::new();
    let (session, digest) = f.session();
    let request = Request::new("POST", "/peer/v3/state", b"body").unwrap();
    let proof = f.proof(&session, digest, &request);
    let bytes = proof.encode().unwrap();
    assert_eq!(
        RequestProof::decode(&bytes).unwrap().encode().unwrap(),
        bytes
    );
    for end in 0..bytes.len() {
        assert!(RequestProof::decode(&bytes[..end]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(RequestProof::decode(&trailing).is_err());
    let mut alias = bytes.clone();
    alias.splice(0..1, [0x83, 0]);
    assert!(RequestProof::decode(&alias).is_err());
    assert!(RequestProof::decode(&[0; 257]).is_err());
    let mut version = bytes;
    version[0] = 4;
    assert!(RequestProof::decode(&version).is_err());
}

#[test]
fn full_session_request_admission_and_atomic_replay_rejection() {
    let f = Fixture::new();
    let (session, digest) = f.session();
    let request = Request::new("POST", "/peer/v3/state", b"same business id and body").unwrap();
    let proof = f.proof(&session, digest, &request);
    let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
    {
        let accepted = owner.admit(&proof, &request, &f.context()).unwrap();
        assert_eq!(accepted.request().body(), b"same business id and body");
        assert_eq!(accepted.session().binding(), &f.binding);
    }
    assert!(matches!(
        owner.admit(&proof, &request, &f.context()),
        Err(ProofError::Replay)
    ));
    let retry = RequestProof::sign(
        proof.session_id(),
        digest,
        &request,
        f.now,
        f.now + NS,
        &f.device_key,
    )
    .unwrap();
    owner.admit(&retry, &request, &f.context()).unwrap();
}

#[test]
fn exact_request_and_session_binding_cannot_be_substituted() {
    let f = Fixture::new();
    let (session, digest) = f.session();
    let request = Request::new("POST", "/peer/v3/state?a=x", b"body").unwrap();
    let proof = f.proof(&session, digest, &request);
    let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
    for (method, target, body) in [
        ("PUT", "/peer/v3/state?a=x", &b"body"[..]),
        ("POST", "/peer/v3/acks?a=x", &b"body"[..]),
        ("POST", "/peer/v3/state?a=y", &b"body"[..]),
        ("POST", "/peer/v3/state?a=x", &b"other body"[..]),
    ] {
        let changed = Request::new(method, target, body).unwrap();
        assert!(matches!(
            owner.admit(&proof, &changed, &f.context()),
            Err(ProofError::Signature)
        ));
    }
    for which in 0..8 {
        let mut binding = f.binding.clone();
        match which {
            0 => binding.workspace += 1,
            1 => binding.host[0] ^= 1,
            2 => binding.device[0] ^= 1,
            3 => binding.server_boot_id[0] ^= 1,
            4 => binding.descriptor_digest[0] ^= 1,
            5 => binding.origin.push_str(":443"),
            6 => binding.carrier = Carrier::WebTransport,
            _ => binding.workspace_root[0] ^= 1,
        }
        let wrong =
            session_binding_digest(&binding, proof.session_id(), 3, f.now + 900 * NS).unwrap();
        let changed = RequestProof::sign(
            proof.session_id(),
            wrong,
            &request,
            f.now,
            f.now + NS,
            &f.device_key,
        )
        .unwrap();
        assert!(matches!(
            owner.admit(&changed, &request, &f.context()),
            Err(ProofError::Signature)
        ));
    }
    let wrong_key = RequestProof::sign(
        proof.session_id(),
        digest,
        &request,
        f.now,
        f.now + NS,
        &f.host_key,
    )
    .unwrap();
    assert!(matches!(
        owner.admit(&wrong_key, &request, &f.context()),
        Err(ProofError::Signature)
    ));
    let (other, _) = f.session();
    let wrong_session = RequestProof::sign(
        other.session_id(),
        digest,
        &request,
        f.now,
        f.now + NS,
        &f.device_key,
    )
    .unwrap();
    assert!(matches!(
        owner.admit(&wrong_session, &request, &f.context()),
        Err(ProofError::Session)
    ));
    owner.admit(&proof, &request, &f.context()).unwrap(); // Failed verification consumed no nonce.
}

#[test]
fn current_authority_boot_certificates_and_carrier_remain_mandatory() {
    for reason in 0..5 {
        let mut f = Fixture::new();
        let (session, digest) = f.session();
        let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let proof = f.proof(&session, digest, &request);
        let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
        match reason {
            0 => f.binding.server_boot_id[0] ^= 1,
            1 => f.revoke(false),
            2 => f.revoke(true),
            3 => f.admitted.store(false, Ordering::SeqCst),
            _ => {
                f.device.device_label = "replacement".into();
                sign_device_cert(&mut f.device, &f.root).unwrap();
            }
        }
        assert!(matches!(
            owner.admit(&proof, &request, &f.context()),
            Err(ProofError::Authority(_))
        ));
    }
    let mut f = Fixture::new();
    f.binding.carrier = Carrier::WebTransport;
    let (session, _) = f.session();
    assert!(matches!(
        HttpSessionAdmission::new(session, &f.context()),
        Err(ProofError::Carrier)
    ));
}

#[test]
fn lifetime_tolerance_and_strict_session_expiry_boundaries() {
    let f = Fixture::new();
    let request = Request::new("POST", "/peer/v3/acks", b"ack").unwrap();
    for (issued, expires, now, accepted) in [
        (100, 160, 100, true),
        (100, 160, 189, true),
        (100, 160, 190, false),
        (130, 160, 100, true),
        (131, 160, 100, false),
    ] {
        let (session, digest) = f.session();
        let proof = RequestProof::sign(
            session.session_id(),
            digest,
            &request,
            issued * NS,
            expires * NS,
            &f.device_key,
        )
        .unwrap();
        let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
        let mut current = f.context();
        current.now_ns = now * NS;
        assert_eq!(
            owner.admit(&proof, &request, &current).is_ok(),
            accepted,
            "{issued}/{expires}/{now}"
        );
    }
    assert!(RequestProof::sign([1; 16], [2; 32], &request, 100, 100, &f.device_key).is_err());
    assert!(
        RequestProof::sign(
            [1; 16],
            [2; 32],
            &request,
            100,
            100 + 60 * NS + 1,
            &f.device_key
        )
        .is_err()
    );
    assert!(
        RequestProof::sign(
            [1; 16],
            [2; 32],
            &request,
            u64::MAX - 1,
            u64::MAX,
            &f.device_key
        )
        .is_err()
    );
    let mut f = Fixture::new();
    f.device.expires_at_ns = Some(150 * NS);
    sign_device_cert(&mut f.device, &f.root).unwrap();
    let (session, digest) = f.session();
    let beyond = RequestProof::sign(
        session.session_id(),
        digest,
        &request,
        100 * NS,
        151 * NS,
        &f.device_key,
    )
    .unwrap();
    let exact = RequestProof::sign(
        session.session_id(),
        digest,
        &request,
        100 * NS,
        150 * NS,
        &f.device_key,
    )
    .unwrap();
    let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
    assert!(matches!(
        owner.admit(&beyond, &request, &f.context()),
        Err(ProofError::Lifetime)
    ));
    f.now = 150 * NS;
    assert!(owner.admit(&exact, &request, &f.context()).is_err());
}

#[test]
fn bounded_live_nonce_evidence_is_never_evicted_and_clock_rollback_cannot_resurrect_it() {
    let mut f = Fixture::new();
    let (session, digest) = f.session();
    let request = Request::new("POST", "/peer/v3/acks", b"ack").unwrap();
    let first = f.proof(&session, digest, &request);
    let session_id = session.session_id();
    let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
    owner.admit(&first, &request, &f.context()).unwrap();
    for _ in 1..16_384 {
        let proof = RequestProof::sign(
            session_id,
            digest,
            &request,
            f.now,
            f.now + 60 * NS,
            &f.device_key,
        )
        .unwrap();
        owner.admit(&proof, &request, &f.context()).unwrap();
    }
    let overflow = RequestProof::sign(
        session_id,
        digest,
        &request,
        f.now,
        f.now + 60 * NS,
        &f.device_key,
    )
    .unwrap();
    assert!(matches!(
        owner.admit(&overflow, &request, &f.context()),
        Err(ProofError::ReplayCapacity)
    ));
    assert!(matches!(
        owner.admit(&first, &request, &f.context()),
        Err(ProofError::Replay)
    ));
    f.now = 190 * NS;
    let fresh = RequestProof::sign(
        session_id,
        digest,
        &request,
        f.now,
        f.now + 60 * NS,
        &f.device_key,
    )
    .unwrap();
    owner.admit(&fresh, &request, &f.context()).unwrap();
    f.now = 150 * NS;
    assert!(matches!(
        owner.admit(&first, &request, &f.context()),
        Err(ProofError::ClockRollback)
    ));
}

#[test]
fn concurrent_duplicate_admission_has_one_winner() {
    let f = Fixture::new();
    let (session, digest) = f.session();
    let request = Request::new("POST", "/peer/v3/state", b"same body").unwrap();
    let proof = f.proof(&session, digest, &request);
    let owner = Mutex::new(HttpSessionAdmission::new(session, &f.context()).unwrap());
    std::thread::scope(|scope| {
        let attempts = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    owner
                        .lock()
                        .unwrap()
                        .admit(&proof, &request, &f.context())
                        .is_ok()
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            attempts
                .into_iter()
                .map(|a| a.join().unwrap())
                .filter(|accepted| *accepted)
                .count(),
            1
        );
    });
}

#[test]
fn signed_malformed_lifetimes_and_reflected_nonce_signatures_are_rejected() {
    let f = Fixture::new();
    let (session, digest) = f.session();
    let request = Request::new("POST", "/peer/v3/state", b"body").unwrap();
    let original = f.proof(&session, digest, &request);
    let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
    for mutation in 0..4 {
        let mut proof = RequestProof::decode(&original.encode().unwrap()).unwrap();
        match mutation {
            0 => proof.wire.expires_at_ns = proof.wire.issued_at_ns,
            1 => proof.wire.expires_at_ns = proof.wire.issued_at_ns + 60 * NS + 1,
            2 => proof.wire.nonce = [0; 32],
            _ => proof.wire.protocol = 4,
        }
        proof.wire.signature = f
            .device_key
            .sign(&request_digest(&proof.wire, digest, &request).unwrap())
            .to_bytes();
        assert!(owner.admit(&proof, &request, &f.context()).is_err());
    }
    let mut reflection = RequestProof::decode(&original.encode().unwrap()).unwrap();
    reflection.wire.signature = f.device_key.sign(&reflection.wire.nonce).to_bytes();
    assert!(matches!(
        owner.admit(&reflection, &request, &f.context()),
        Err(ProofError::Signature)
    ));
    owner.admit(&original, &request, &f.context()).unwrap();
}

#[test]
fn session_revalidation_preserves_identity_and_nonce_evidence() {
    let mut f = Fixture::new();
    let (session, digest) = f.session();
    let session_id = session.session_id();
    let expires = session.expires_at_ns();
    let request = Request::new("GET", "/peer/v3/events?cursor=a&mode=poll", b"").unwrap();
    let consumed = f.proof(&session, digest, &request);
    let fresh = f.proof(&session, digest, &request);
    let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
    owner.admit(&consumed, &request, &f.context()).unwrap();
    let entries = owner.nonces.clone();
    let deadline = owner.next_deadline;
    let capacity = owner.nonces.capacity();
    for now in [100, 101, 102] {
        f.now = now * NS;
        let verified = owner.revalidate(&f.context()).unwrap();
        assert_eq!(verified.session_id(), session_id);
        assert_eq!(verified.expires_at_ns(), expires);
        assert_eq!(verified.binding(), &f.binding);
        assert_eq!(owner.nonces, entries);
        assert_eq!(owner.next_deadline, deadline);
        assert_eq!(owner.nonces.capacity(), capacity);
    }
    assert!(matches!(
        owner.admit(&consumed, &request, &f.context()),
        Err(ProofError::Replay)
    ));
    owner.admit(&fresh, &request, &f.context()).unwrap();
    let entries = owner.nonces.clone();
    let deadline = owner.next_deadline;
    let capacity = owner.nonces.capacity();
    // Revalidation does not even prune expired proof evidence.
    f.now = 190 * NS;
    owner.revalidate(&f.context()).unwrap();
    assert_eq!(owner.nonces, entries);
    assert_eq!(owner.next_deadline, deadline);
    assert_eq!(owner.nonces.capacity(), capacity);
}

#[test]
fn session_revalidation_rejects_expiry_revocation_denial_and_changed_binding() {
    for reason in 0..9 {
        let mut f = Fixture::new();
        let (session, digest) = f.session();
        let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let proof = f.proof(&session, digest, &request);
        let expires = session.expires_at_ns();
        let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
        owner.admit(&proof, &request, &f.context()).unwrap();
        let entries = owner.nonces.clone();
        let deadline = owner.next_deadline;
        match reason {
            0 => f.now = expires,
            1 => f.revoke(false),
            2 => f.revoke(true),
            3 => f.admitted.store(false, Ordering::SeqCst),
            4 => f.binding.server_boot_id[0] ^= 1,
            5 => f.binding.device[0] ^= 1,
            6 => f.binding.host[0] ^= 1,
            7 => f.binding.carrier = Carrier::WebTransport,
            _ => f.binding.descriptor_digest[0] ^= 1,
        }
        assert!(
            matches!(
                owner.revalidate(&f.context()),
                Err(ProofError::Authority(_))
            ),
            "reason {reason}"
        );
        assert_eq!(owner.nonces, entries);
        assert_eq!(owner.next_deadline, deadline);
    }
}

#[test]
fn session_revalidation_advances_clock_even_when_authority_denies() {
    for denied in [false, true] {
        let mut f = Fixture::new();
        let (session, digest) = f.session();
        let request = Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap();
        let proof = f.proof(&session, digest, &request);
        let mut owner = HttpSessionAdmission::new(session, &f.context()).unwrap();
        f.now = 120 * NS;
        f.admitted.store(!denied, Ordering::SeqCst);
        if denied {
            assert!(matches!(
                owner.revalidate(&f.context()),
                Err(ProofError::Authority(_))
            ));
        } else {
            owner.revalidate(&f.context()).unwrap();
        }
        f.admitted.store(true, Ordering::SeqCst);
        f.now = 119 * NS;
        assert!(matches!(
            owner.revalidate(&f.context()),
            Err(ProofError::ClockRollback)
        ));
        assert!(matches!(
            owner.admit(&proof, &request, &f.context()),
            Err(ProofError::ClockRollback)
        ));
        f.now = 120 * NS;
        owner.revalidate(&f.context()).unwrap();
        owner.admit(&proof, &request, &f.context()).unwrap();
        // The common admission path advances the same clock fence.
        f.now = 121 * NS;
        assert!(matches!(
            owner.admit(&proof, &request, &f.context()),
            Err(ProofError::Replay)
        ));
        f.now = 120 * NS;
        assert!(matches!(
            owner.revalidate(&f.context()),
            Err(ProofError::ClockRollback)
        ));
    }
}

#[test]
fn session_revalidation_requires_https_even_for_a_valid_other_carrier_session() {
    let mut f = Fixture::new();
    f.binding.carrier = Carrier::WebTransport;
    let (session, digest) = f.session();
    // Exercise the independent carrier guard with real signed session evidence;
    // the public constructor already prevents this internal state.
    let mut owner = HttpSessionAdmission {
        session,
        digest,
        nonces: HashMap::new(),
        next_deadline: None,
        high_water_ns: f.now,
    };
    assert!(matches!(
        owner.revalidate(&f.context()),
        Err(ProofError::Carrier)
    ));
}
