//! Explicit root qualification: real QPE1 child, actual canonical signatures.
use super::*;
use ed25519_dalek::{Signer, SigningKey};
use runtime_portal::process_binding;
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Child, Command},
};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
};
const CHILD_FD: &str = "TILO_RETAINED_PERSON_QUALIFICATION_FD";
const CHILD_TEST: &str = "retained_person::tests::retained_person_peer_child";

#[test]
fn retained_person_peer_child() {
    let Ok(raw) = std::env::var(CHILD_FD) else {
        return;
    };
    // SAFETY: this fixture transfers exactly this owned descriptor pre-exec.
    let mut stream = unsafe { UnixStream::from_raw_fd(raw.parse().unwrap()) };
    assert_eq!(effective_uid(), 1000);
    process_binding::complete(&mut stream).unwrap();
    let mut finish = [0];
    stream.read_exact(&mut finish).unwrap();
    assert_eq!(finish, [9]);
}
struct PeerFixture {
    child: Child,
    stream: UnixStream,
    peer: process_binding::ExecutingPeer,
}
impl PeerFixture {
    fn start() -> io::Result<Self> {
        if effective_uid() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "explicit native proof qualification requires root",
            ));
        }
        let (mut stream, child_stream) = UnixStream::pair()?;
        stream.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
        stream.set_write_timeout(Some(tilo_test_env::hang_bound()))?;
        let descriptor = child_stream.as_raw_fd();
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_FD, descriptor.to_string());
        // SAFETY: only async-signal-safe credential/descriptor syscalls in the
        // fixture child. Parent credentials and native storage are untouched.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(descriptor, libc::F_SETFD, 0) < 0
                    || libc::setgroups(0, std::ptr::null()) < 0
                    || libc::setgid(1000) < 0
                    || libc::setuid(1000) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        process_binding::arm(&stream)?;
        stream.write_all(&process_binding::MAGIC)?;
        stream.write_all(&[61; 32])?;
        let child = command.spawn_locked()?;
        drop(child_stream);
        let peer = process_binding::receive(&stream, &[61; 32], 1000)?;
        if peer.pid() != child.id() {
            return Err(io::Error::other("admitted peer is not the spawned child"));
        }
        if !peer.current() {
            return Err(io::Error::other("spawned kernel task was not admitted"));
        }
        stream.write_all(&[1])?;
        Ok(Self {
            child,
            stream,
            peer,
        })
    }
    fn finish(&mut self) -> io::Result<()> {
        self.stream.write_all(&[9])?;
        if !self.child.wait()?.success() {
            return Err(io::Error::other("qualification child failed"));
        }
        if self.peer.current() {
            return Err(io::Error::other(
                "exited kernel task cannot remain admitted",
            ));
        }
        Ok(())
    }
}
impl Drop for PeerFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn certificate(root: &SigningKey, device: &SigningKey) -> io::Result<DeviceCert> {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "owned native proof fixture".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).map_err(io::Error::other)?;
    Ok(cert)
}
fn proof(
    peer: &process_binding::ExecutingPeer,
    cert: &DeviceCert,
    device: &SigningKey,
) -> io::Result<SignedRetainedPersonProof> {
    let claims = RetainedPersonClaims {
        binding: RetainedLaunchBinding {
            operation_id: "a3a5760f-066e-4c9a-92d7-07a3ee73c650".into(),
            app_id: "so.tilo.files".into(),
            package_sha256: "a".repeat(64),
            room: personal_cluster(&cert.user_public_key),
            resource_record: "owned-fixture-record".into(),
            target_uid: peer.uid(),
            target_pid: peer.pid(),
            root_pid: std::process::id(),
            context_sha256: "b".repeat(64),
        },
        challenge: [17; 32],
        certificate: serde_json::to_vec(cert).map_err(io::Error::other)?,
        revocations: vec![],
        execution_id: "39b8f599-0d98-4d89-9c11-4d8ea6401c1e".into(),
    };
    let signature = device.sign(&claims.signing_bytes()?).to_bytes().to_vec();
    Ok(SignedRetainedPersonProof { claims, signature })
}
fn resign(proof: &mut SignedRetainedPersonProof, device: &SigningKey) -> io::Result<()> {
    proof.signature = device
        .sign(&proof.claims.signing_bytes()?)
        .to_bytes()
        .to_vec();
    Ok(())
}
fn revocation(root: &SigningKey, key: [u8; 32]) -> io::Result<RevocationEntry> {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: key,
        revoked_at_ns: 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, root).map_err(io::Error::other)?;
    Ok(entry)
}

#[test]
fn actual_peer_signatures_refuse_forgery_replay_rebinding_and_wrong_installed_root() {
    let mut fixture = PeerFixture::start().expect("root proof qualification fixture");
    let root = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[7; 32]);
    let cert = certificate(&root, &device).expect("certificate fixture");
    let original = proof(&fixture.peer, &cert, &device).expect("proof fixture");
    let binding = original.claims.binding.clone();
    let list = RevocationList::new();
    let wire = original.encode().unwrap();
    let decoded = SignedRetainedPersonProof::decode(&wire).unwrap();
    decoded
        .verify(&binding, &[17; 32], &fixture.peer, &cert, &list)
        .unwrap();
    let mut forged = original.clone();
    forged.signature[0] ^= 1;
    assert!(
        forged
            .verify(&binding, &[17; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
    assert!(
        original
            .verify(&binding, &[18; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
    let mut rebound = original.clone();
    rebound.claims.binding.context_sha256 = "c".repeat(64);
    resign(&mut rebound, &device).expect("proof signature");
    assert!(
        rebound
            .verify(&binding, &[17; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
    let mut wrong_uid = binding.clone();
    wrong_uid.target_uid = 1001;
    assert!(
        original
            .verify(&wrong_uid, &[17; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
    let wrong_cert =
        certificate(&SigningKey::from_bytes(&[11; 32]), &device).expect("certificate fixture");
    assert!(
        original
            .verify(&binding, &[17; 32], &fixture.peer, &wrong_cert, &list)
            .is_err()
    );
    let mut wrong_root = original.clone();
    wrong_root.claims.binding.root_pid += 1;
    resign(&mut wrong_root, &device).expect("proof signature");
    assert!(
        wrong_root
            .verify(
                &wrong_root.claims.binding,
                &[17; 32],
                &fixture.peer,
                &cert,
                &list
            )
            .is_err()
    );
    let mut expired = cert.clone();
    expired.expires_at_ns = Some(2);
    tilo_user_identity::sign_device_cert(&mut expired, &root).unwrap();
    let expired_proof = proof(&fixture.peer, &expired, &device).expect("proof fixture");
    assert!(
        expired_proof
            .verify(
                &expired_proof.claims.binding,
                &[17; 32],
                &fixture.peer,
                &expired,
                &list
            )
            .is_err()
    );
    fixture.finish().expect("qualification child exits");
    assert!(
        original
            .verify(&binding, &[17; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
}

#[test]
fn epoch_refuses_changed_session_and_never_revives_after_restoration_or_revoke() {
    let mut fixture = PeerFixture::start().expect("root proof qualification fixture");
    let root = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[7; 32]);
    let cert = certificate(&root, &device).expect("certificate fixture");
    let original = proof(&fixture.peer, &cert, &device).expect("proof fixture");
    let binding = original.claims.binding.clone();
    let list = RevocationList::new();
    let epoch = original
        .verify(&binding, &[17; 32], &fixture.peer, &cert, &list)
        .unwrap();
    let mut fresh = original.clone();
    fresh.claims.challenge = [19; 32];
    resign(&mut fresh, &device).expect("proof signature");
    epoch
        .revalidate(&fresh, &[19; 32], &fixture.peer, &cert, &list)
        .unwrap();
    let mut changed = fresh.clone();
    changed.claims.execution_id = "6671df0d-2be1-484b-9220-08e1b0f2f9b2".into();
    resign(&mut changed, &device).expect("proof signature");
    assert!(
        epoch
            .revalidate(&changed, &[19; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
    assert!(
        epoch
            .revalidate(&fresh, &[19; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
    let other = original
        .verify(&binding, &[17; 32], &fixture.peer, &cert, &list)
        .unwrap();
    other.revoke();
    assert!(
        other
            .revalidate(&fresh, &[19; 32], &fixture.peer, &cert, &list)
            .is_err()
    );
    fixture.finish().expect("qualification child exits");
}

#[test]
fn signed_revocations_require_exact_root_and_outer_proof_before_monotonic_merge() {
    let mut fixture = PeerFixture::start().expect("root proof qualification fixture");
    let root = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[7; 32]);
    let cert = certificate(&root, &device).expect("certificate fixture");
    let mut current = proof(&fixture.peer, &cert, &device).expect("proof fixture");
    let binding = current.claims.binding.clone();
    let mut held = RevocationList::new();
    current.claims.revocations = vec![revocation(&root, [31; 32]).expect("revocation fixture")];
    resign(&mut current, &device).expect("proof signature");
    let updates = current
        .authenticated_revocations(&binding, &[17; 32], &fixture.peer, &cert, &held)
        .unwrap();
    assert_eq!(updates, current.claims.revocations);
    for entry in updates {
        held.insert(entry).unwrap();
    }
    let epoch = current
        .verify(&binding, &[17; 32], &fixture.peer, &cert, &held)
        .unwrap();
    let mut omitted = current.clone();
    omitted.claims.revocations.clear();
    omitted.claims.challenge = [19; 32];
    resign(&mut omitted, &device).expect("proof signature");
    epoch
        .revalidate(&omitted, &[19; 32], &fixture.peer, &cert, &held)
        .unwrap();
    assert!(
        held.is_revoked(&root.verifying_key().to_bytes(), &[31; 32]),
        "omission cannot erase root retained entries"
    );
    let mut revoked = current.clone();
    revoked
        .claims
        .revocations
        .push(revocation(&root, cert.device_public_key).expect("revocation fixture"));
    resign(&mut revoked, &device).expect("proof signature");
    let updates = revoked
        .authenticated_revocations(&binding, &[17; 32], &fixture.peer, &cert, &held)
        .unwrap();
    assert!(
        revoked
            .verify(&binding, &[17; 32], &fixture.peer, &cert, &held)
            .is_err(),
        "even an actor forgetting merge cannot admit a signer its proof revokes"
    );
    let before_revocation = held.clone();
    for entry in updates {
        held.insert(entry).unwrap();
    }
    assert!(
        epoch
            .revalidate(&omitted, &[19; 32], &fixture.peer, &cert, &held)
            .is_err()
    );
    assert!(
        epoch
            .revalidate(
                &omitted,
                &[19; 32],
                &fixture.peer,
                &cert,
                &before_revocation
            )
            .is_err(),
        "restored root bytes cannot revive an epoch after observed revocation"
    );
    fixture.finish().expect("qualification child exits");
}

#[test]
fn signed_revocations_refuse_wrong_issuer_entry_outer_signatures_and_overflow() {
    let mut fixture = PeerFixture::start().expect("root proof qualification fixture");
    let root = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[7; 32]);
    let cert = certificate(&root, &device).expect("certificate fixture");
    let mut current = proof(&fixture.peer, &cert, &device).expect("proof fixture");
    let binding = current.claims.binding.clone();
    let held = RevocationList::new();
    current.claims.revocations = vec![revocation(&root, [31; 32]).expect("revocation fixture")];
    resign(&mut current, &device).expect("proof signature");
    let mut wrong_issuer = current.clone();
    wrong_issuer.claims.revocations =
        vec![revocation(&SigningKey::from_bytes(&[13; 32]), [32; 32]).expect("revocation fixture")];
    resign(&mut wrong_issuer, &device).expect("proof signature");
    assert!(
        wrong_issuer
            .authenticated_revocations(&binding, &[17; 32], &fixture.peer, &cert, &held)
            .is_err()
    );
    let mut bad_entry = current.clone();
    bad_entry.claims.revocations[0].signature[0] ^= 1;
    resign(&mut bad_entry, &device).expect("proof signature");
    assert!(
        bad_entry
            .authenticated_revocations(&binding, &[17; 32], &fixture.peer, &cert, &held)
            .is_err()
    );
    let mut maximum = current.clone();
    maximum.claims.revocations = vec![current.claims.revocations[0].clone(); 256];
    resign(&mut maximum, &device).expect("proof signature");
    assert_eq!(
        maximum
            .authenticated_revocations(&binding, &[17; 32], &fixture.peer, &cert, &held)
            .unwrap()
            .len(),
        256
    );
    maximum
        .claims
        .revocations
        .push(current.claims.revocations[0].clone());
    assert!(
        maximum.encode().is_err(),
        "257 signed revocation entries exceed the fixed authority bound"
    );
    let mut unsigned_outer = current.clone();
    unsigned_outer.signature[0] ^= 1;
    assert!(
        unsigned_outer
            .authenticated_revocations(&binding, &[17; 32], &fixture.peer, &cert, &held)
            .is_err()
    );

    let mut contaminated = held;
    contaminated
        .insert(
            revocation(&SigningKey::from_bytes(&[13; 32]), [32; 32]).expect("revocation fixture"),
        )
        .unwrap();
    assert!(
        current
            .authenticated_revocations(&binding, &[17; 32], &fixture.peer, &cert, &contaminated)
            .is_err(),
        "a valid signature under another root cannot contaminate the root-held person list"
    );
    fixture.finish().expect("qualification child exits");
}

#[test]
fn correctly_signed_other_workspace_binding_cannot_borrow_personal_source_context() {
    let mut fixture = PeerFixture::start().expect("root proof qualification fixture");
    let root = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[7; 32]);
    let cert = certificate(&root, &device).expect("certificate fixture");
    let mut foreign = proof(&fixture.peer, &cert, &device).expect("proof fixture");
    foreign.claims.binding.room = "ee242d82-6913-4b20-a719-aeb96a6e052a".into();
    assert_ne!(
        foreign.claims.binding.room,
        personal_cluster(&cert.user_public_key)
    );
    resign(&mut foreign, &device).expect("proof signature");
    // Expected request agrees with the foreign workspace and the outer signature is
    // genuine. Rejection must come from the person's own cluster.
    assert!(
        foreign
            .authenticated_revocations(
                &foreign.claims.binding,
                &[17; 32],
                &fixture.peer,
                &cert,
                &RevocationList::new()
            )
            .is_err()
    );
    assert!(
        foreign
            .verify(
                &foreign.claims.binding,
                &[17; 32],
                &fixture.peer,
                &cert,
                &RevocationList::new()
            )
            .is_err()
    );
    fixture.finish().expect("qualification child exits");
}

#[test]
fn genuinely_signed_future_and_invalid_interval_certificates_fail_both_time_checks() {
    let mut fixture = PeerFixture::start().expect("root proof qualification fixture");
    let root = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[7; 32]);
    let trusted = certificate(&root, &device).expect("certificate fixture");
    let held = RevocationList::new();
    for (issued, expires, expected) in [
        (
            u64::MAX - 1,
            None,
            tilo_user_identity::DeviceCertError::NotYetValid,
        ),
        (
            3,
            Some(2),
            tilo_user_identity::DeviceCertError::InvalidValidity,
        ),
        (
            3,
            Some(3),
            tilo_user_identity::DeviceCertError::InvalidValidity,
        ),
    ] {
        let mut invalid = trusted.clone();
        invalid.issued_at_ns = issued;
        invalid.expires_at_ns = expires;
        // Negative fixture: the production signer correctly refuses invalid
        // intervals. Sign its canonical digest directly with the genuine root
        // key so production verification, rather than fixture setup, rejects it.
        invalid.signature = root
            .sign(&tilo_user_identity::device_cert_signing_digest(&invalid).unwrap())
            .to_bytes();
        // All signatures are genuinely valid: only current-time admission
        // rejects them. Proof also has a genuine admitted-device signature.
        tilo_user_identity::verify_device_cert(&invalid).unwrap();
        let signed = proof(&fixture.peer, &invalid, &device).expect("proof fixture");
        for installed in [&invalid, &trusted] {
            let error = signed
                .verify(
                    &signed.claims.binding,
                    &[17; 32],
                    &fixture.peer,
                    installed,
                    &held,
                )
                .err()
                .expect("invalid time refused");
            assert_eq!(
                error
                    .get_ref()
                    .and_then(|error| error.downcast_ref::<tilo_user_identity::DeviceCertError>()),
                Some(&expected),
                "the actual production root verifier must check both installed and received time validity before canonical payload comparison"
            );
        }
    }
    fixture.finish().expect("qualification child exits");
}

#[test]
fn observed_installed_expiry_failure_retires_epoch_even_when_valid_custody_is_restored() {
    let mut fixture = PeerFixture::start().expect("root proof qualification fixture");
    let root = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[7; 32]);
    let trusted = certificate(&root, &device).expect("certificate fixture");
    let held = RevocationList::new();
    let original = proof(&fixture.peer, &trusted, &device).expect("proof fixture");
    let epoch = original
        .verify(
            &original.claims.binding,
            &[17; 32],
            &fixture.peer,
            &trusted,
            &held,
        )
        .unwrap();
    let mut expired = trusted.clone();
    expired.expires_at_ns = Some(2);
    tilo_user_identity::sign_device_cert(&mut expired, &root).unwrap();
    tilo_user_identity::verify_device_cert(&expired).unwrap();
    let mut fresh = original;
    fresh.claims.challenge = [19; 32];
    resign(&mut fresh, &device).expect("proof signature");
    let error = epoch
        .revalidate(&fresh, &[19; 32], &fixture.peer, &expired, &held)
        .unwrap_err();
    assert_eq!(
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<tilo_user_identity::DeviceCertError>()),
        Some(&tilo_user_identity::DeviceCertError::Expired)
    );
    fresh.claims.challenge = [21; 32];
    resign(&mut fresh, &device).expect("proof signature");
    assert!(
        epoch
            .revalidate(&fresh, &[21; 32], &fixture.peer, &trusted, &held)
            .is_err(),
        "restoring valid root custody with a fresh valid proof cannot revive the observed failed epoch"
    );
    fixture.finish().expect("qualification child exits");
}
