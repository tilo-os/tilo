use ed25519_dalek::SigningKey;
use tilo_seal::SecretKey;
use tilo_unlock_protocol::{Half, open_release, seal_sibling_half};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
    sign_revocation_entry,
};

use super::*;

const NOW: u64 = 1_000_000_000_000;

fn root() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn device_key() -> SigningKey {
    SigningKey::from_bytes(&[2; 32])
}

fn boot_key() -> SigningKey {
    SigningKey::from_bytes(&[3; 32])
}

fn sibling() -> [u8; 32] {
    SigningKey::from_bytes(&[4; 32]).verifying_key().to_bytes()
}

fn device() -> [u8; 32] {
    device_key().verifying_key().to_bytes()
}

fn device_cert(key: [u8; 32]) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root().verifying_key().to_bytes(),
        device_public_key: key,
        device_label: "Laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root()).unwrap();
    cert
}

fn slot() -> SlotId {
    SlotId {
        volume_uuid: [5; 16],
        keyslot: 2,
    }
}

fn boot_cert(attestation_key: Option<Vec<u8>>) -> BootKeyCert {
    let mut cert = BootKeyCert {
        version: PROTOCOL_VERSION,
        user_public_key: root().verifying_key().to_bytes(),
        device_public_key: device(),
        boot_public_key: boot_key().verifying_key().to_bytes(),
        slot: slot(),
        attestation_key,
        issued_at_ns: 2,
        signature: [0; 64],
    };
    cert.sign(&root()).unwrap();
    cert
}

fn half() -> Half {
    Half::from_bytes([6; 32])
}

struct World {
    fleet: CertifiedFleet,
    enrolments: BTreeMap<[u8; 32], UnlockEnrolment>,
    auto_approvals: BTreeMap<[u8; 32], AutoApproval>,
    secret_sync: SecretKey,
    custodian: bool,
}

impl World {
    fn new(cert: &BootKeyCert) -> Self {
        let mut fleet = CertifiedFleet::new(root().verifying_key().to_bytes());
        fleet.certify(device_cert(device())).unwrap();
        fleet.certify(device_cert(sibling())).unwrap();
        let secret_sync = SecretKey::from_bytes([12; 32]);
        let sealed_half =
            seal_sibling_half(&secret_sync.public_key().unwrap(), cert, &half()).unwrap();
        let enrolment = UnlockEnrolment {
            cert: cert.clone(),
            sealed_half,
            stamp: FabricStamp {
                counter: 1,
                device: DeviceId::new(hex::encode(device())).unwrap(),
            },
        };
        enrolment
            .validate(&DeviceId::new(hex::encode(device())).unwrap())
            .unwrap();
        let mut enrolments = BTreeMap::new();
        enrolments.insert(enrolment.id().unwrap(), enrolment);
        Self {
            fleet,
            enrolments,
            auto_approvals: BTreeMap::new(),
            secret_sync,
            custodian: true,
        }
    }

    fn view(&self) -> UnlockView<'_> {
        UnlockView {
            fleet: &self.fleet,
            enrolments: &self.enrolments,
            auto_approvals: &self.auto_approvals,
            custodian: self.custodian,
            secret_sync: self.custodian.then_some(&self.secret_sync),
        }
    }

    fn revoke(&mut self) {
        let mut entry = RevocationEntry {
            user_public_key: root().verifying_key().to_bytes(),
            revoked_device_public_key: device(),
            revoked_at_ns: NOW,
            reason: RevocationReason::Stolen,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root()).unwrap();
        self.fleet.revoke(entry).unwrap();
    }
}

/// One early boot: its ephemeral key and the exchange so far.
struct Boot {
    hello: UnlockHello,
    ephemeral: SecretKey,
    id: BootId,
}

impl Boot {
    fn new(cert: BootKeyCert, boot_id: u8) -> Self {
        Self {
            hello: UnlockHello {
                version: PROTOCOL_VERSION,
                boot_cert: cert,
            },
            ephemeral: SecretKey::from_bytes([boot_id; 32]),
            id: BootId([boot_id; 16]),
        }
    }

    fn answer(&self, challenge: &Challenge, attestation: Option<attest::Quote>) -> UnlockRequest {
        UnlockRequest::sign(
            &boot_key(),
            &self.hello.boot_cert,
            challenge,
            self.id,
            &self.ephemeral.public_key().unwrap(),
            attestation,
        )
        .unwrap()
    }

    fn open(&self, request: &UnlockRequest, frame: &SiblingFrame) -> Half {
        let SiblingFrame::Released(release) = frame else {
            panic!("expected a release, got {frame:?}");
        };
        let digest = request.digest(&self.hello.boot_cert, &sibling()).unwrap();
        open_release(&self.ephemeral, &digest, release).unwrap()
    }
}

fn challenge_of(frame: SiblingFrame) -> Challenge {
    match frame {
        SiblingFrame::Challenge(challenge) => challenge,
        other => panic!("expected a challenge, got {other:?}"),
    }
}

fn pending_code(frame: &SiblingFrame) -> String {
    match frame {
        SiblingFrame::Pending { approval_code } => approval_code.clone(),
        other => panic!("expected pending, got {other:?}"),
    }
}

fn outcomes(sibling: &mut UnlockSibling) -> Vec<UnlockOutcome> {
    sibling
        .drain_history()
        .into_iter()
        .map(|event| event.outcome)
        .collect()
}

#[test]
fn an_approved_request_releases_the_half_to_that_boot_only() {
    let cert = boot_cert(None);
    let world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert, 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    let frame = service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let code = pending_code(&frame);
    let pending: Vec<_> = service.pending().cloned().collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].approval_code, code);
    assert_eq!(pending[0].slot, slot());

    let frame = service
        .decide(
            &pending[0].request_digest,
            UnlockDecision::Approve {
                approver: sibling(),
            },
            &world.view(),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(boot.open(&request, &frame), half());
    assert_eq!(
        outcomes(&mut service),
        vec![
            UnlockOutcome::Requested {
                approval_code: code
            },
            UnlockOutcome::Approved {
                approver: sibling()
            },
        ]
    );
    // Decided once; a second decision finds nothing.
    assert!(
        service
            .decide(
                &pending[0].request_digest,
                UnlockDecision::Approve {
                    approver: sibling()
                },
                &world.view(),
                NOW + 2,
            )
            .is_none()
    );
}

#[test]
fn a_denied_or_unanswered_request_releases_nothing() {
    let cert = boot_cert(None);
    let world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert.clone(), 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let digest = service.pending().next().unwrap().request_digest;
    assert_eq!(
        service.decide(
            &digest,
            UnlockDecision::Deny {
                approver: sibling()
            },
            &world.view(),
            NOW + 1
        ),
        Some(SiblingFrame::Denied)
    );

    // Nobody answers the next boot's request in time.
    let boot = Boot::new(cert, 8);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [10; 32], NOW));
    let request = boot.answer(&challenge, None);
    service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let late = NOW + APPROVAL_TIMEOUT_NS;
    let expired = service.expire(late);
    assert_eq!(expired.len(), 1);
    assert_eq!(service.pending().count(), 0);
    let history = outcomes(&mut service);
    assert!(history.contains(&UnlockOutcome::Denied {
        approver: Some(sibling())
    }));
    assert_eq!(
        history.last(),
        Some(&UnlockOutcome::Denied { approver: None })
    );
}

#[test]
fn replayed_exchanges_are_refused() {
    let cert = boot_cert(None);
    let world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert, 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let digest = service.pending().next().unwrap().request_digest;
    let released = service
        .decide(
            &digest,
            UnlockDecision::Approve {
                approver: sibling(),
            },
            &world.view(),
            NOW,
        )
        .unwrap();
    service.drain_history();

    // The recorded request answered again on the same challenge.
    assert_eq!(
        service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW),
        SiblingFrame::Refused(RefusalReason::UnknownChallenge)
    );
    // The recorded request on a new stream, which got a new challenge.
    let fresh = challenge_of(service.hello(&boot.hello, &world.view(), [10; 32], NOW));
    assert_eq!(
        service.request(&request, &boot.hello, &fresh.nonce, &world.view(), NOW),
        SiblingFrame::Refused(RefusalReason::UnknownChallenge)
    );
    // Even a fresh answer from the same boot gets nothing a second time.
    let again = challenge_of(service.hello(&boot.hello, &world.view(), [11; 32], NOW));
    assert_eq!(
        service.request(
            &boot.answer(&again, None),
            &boot.hello,
            &again.nonce,
            &world.view(),
            NOW
        ),
        SiblingFrame::Refused(RefusalReason::BootAlreadyServed)
    );
    // A recorded release opens nothing without that boot's ephemeral key.
    let SiblingFrame::Released(release) = released else {
        panic!("expected a release");
    };
    assert!(open_release(&SecretKey::from_bytes([1; 32]), &digest, &release).is_err());
    // A late answer to an expired challenge.
    let other = Boot::new(boot.hello.boot_cert.clone(), 12);
    let stale = challenge_of(service.hello(&other.hello, &world.view(), [12; 32], NOW));
    assert_eq!(
        service.request(
            &other.answer(&stale, None),
            &other.hello,
            &stale.nonce,
            &world.view(),
            NOW + CHALLENGE_LIFETIME_NS
        ),
        SiblingFrame::Refused(RefusalReason::ChallengeExpired)
    );
    let refusals = outcomes(&mut service)
        .into_iter()
        .filter(|outcome| matches!(outcome, UnlockOutcome::Refused { .. }))
        .count();
    assert_eq!(refusals, 4, "every refusal is in the history");
}

#[test]
fn requests_are_rate_limited_per_device() {
    let cert = boot_cert(None);
    let world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert, 7);
    for n in 0..MAX_REQUESTS_PER_WINDOW {
        let nonce = [u8::try_from(n).unwrap(); 32];
        challenge_of(service.hello(&boot.hello, &world.view(), nonce, NOW));
    }
    assert_eq!(
        service.hello(&boot.hello, &world.view(), [99; 32], NOW + 1),
        SiblingFrame::Refused(RefusalReason::RateLimited)
    );
    assert_eq!(
        outcomes(&mut service).last(),
        Some(&UnlockOutcome::Refused {
            reason: RefusalReason::RateLimited
        })
    );
    // The window moves on.
    challenge_of(service.hello(&boot.hello, &world.view(), [98; 32], NOW + RATE_WINDOW_NS));
}

#[test]
fn a_device_removed_from_the_fleet_is_refused() {
    let cert = boot_cert(None);
    let mut world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert, 7);
    // Removed while its request waits for the owner: approval releases nothing.
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    service.request(
        &boot.answer(&challenge, None),
        &boot.hello,
        &challenge.nonce,
        &world.view(),
        NOW,
    );
    let digest = service.pending().next().unwrap().request_digest;
    world.revoke();
    assert_eq!(
        service.decide(
            &digest,
            UnlockDecision::Approve {
                approver: sibling()
            },
            &world.view(),
            NOW
        ),
        Some(SiblingFrame::Refused(RefusalReason::Revoked))
    );
    // And any new request is refused at the hello.
    assert_eq!(
        service.hello(&boot.hello, &world.view(), [10; 32], NOW),
        SiblingFrame::Refused(RefusalReason::Revoked)
    );
    service.remove_device(device(), NOW);
    assert_eq!(outcomes(&mut service).last(), Some(&UnlockOutcome::Removed));
    // With its enrolment gone too, nothing is held for it at all.
    world.enrolments.clear();
    assert_eq!(
        service.hello(&boot.hello, &world.view(), [11; 32], NOW),
        SiblingFrame::Refused(RefusalReason::Revoked)
    );
}

#[test]
fn wrong_slot_unknown_key_and_non_custodians_are_refused() {
    let cert = boot_cert(None);
    let mut world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert.clone(), 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let mut request = boot.answer(&challenge, None);
    request.slot.keyslot = 7;
    assert_eq!(
        service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW),
        SiblingFrame::Refused(RefusalReason::WrongSlot)
    );

    // A certificate for another slot that was never enrolled.
    let mut other_slot = cert.clone();
    other_slot.slot.keyslot = 7;
    other_slot.sign(&root()).unwrap();
    let hello = UnlockHello {
        version: PROTOCOL_VERSION,
        boot_cert: other_slot,
    };
    assert_eq!(
        service.hello(&hello, &world.view(), [10; 32], NOW),
        SiblingFrame::Refused(RefusalReason::NotEnrolled)
    );

    // A boot key the person never certified.
    let mut forged = cert;
    forged.boot_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    let hello = UnlockHello {
        version: PROTOCOL_VERSION,
        boot_cert: forged.clone(),
    };
    assert_eq!(
        service.hello(&hello, &world.view(), [11; 32], NOW),
        SiblingFrame::Refused(RefusalReason::BadCertificate)
    );
    let other_root = SigningKey::from_bytes(&[9; 32]);
    forged.user_public_key = other_root.verifying_key().to_bytes();
    forged.sign(&other_root).unwrap();
    let hello = UnlockHello {
        version: PROTOCOL_VERSION,
        boot_cert: forged,
    };
    assert_eq!(
        service.hello(&hello, &world.view(), [12; 32], NOW),
        SiblingFrame::Refused(RefusalReason::BadCertificate)
    );

    // A sibling without the person's root cannot open halves.
    world.custodian = false;
    assert_eq!(
        service.hello(&boot.hello, &world.view(), [13; 32], NOW),
        SiblingFrame::Refused(RefusalReason::NotACustodian)
    );
}

#[test]
fn auto_approval_needs_the_owner_setting_and_an_attested_boot_chain() {
    use p256::ecdsa::{Signature as P256Signature, SigningKey as TpmKey, signature::Signer as _};

    let tpm = TpmKey::from_slice(&[9_u8; 32]).unwrap();
    let ak = tpm.verifying_key().to_sec1_bytes().to_vec();
    let cert = boot_cert(Some(ak));
    let mut world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let pcrs = [(4_u8, [1_u8; 32]), (7, [2; 32])];
    let chain = attest::pcr_composite(&pcrs);

    let quote_for = |boot: &Boot, challenge: &Challenge| {
        let nonce = attest::qualifying_data(
            &boot.hello.boot_cert,
            challenge,
            &boot.id,
            boot.ephemeral.public_key().unwrap().as_bytes(),
        )
        .unwrap();
        let attest = attest_bytes(&nonce, &pcrs);
        let signature: P256Signature = tpm.sign(&attest);
        attest::Quote {
            attest,
            signature: signature.to_der().as_bytes().to_vec(),
            pcrs: pcrs.to_vec(),
        }
    };

    // Without the owner's setting, an attested request still asks the owner.
    let boot = Boot::new(cert.clone(), 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [1; 32], NOW));
    let request = boot.answer(&challenge, Some(quote_for(&boot, &challenge)));
    assert!(matches!(
        service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW),
        SiblingFrame::Pending { .. }
    ));

    // A setting the device signed for itself is not the owner's.
    let forged = AutoApproval::signed(&device_key(), device(), vec![chain], NOW);
    world.auto_approvals.insert(device(), forged.unwrap());
    let boot = Boot::new(cert.clone(), 8);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [2; 32], NOW));
    let request = boot.answer(&challenge, Some(quote_for(&boot, &challenge)));
    assert!(matches!(
        service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW),
        SiblingFrame::Pending { .. }
    ));

    // The owner's setting, but another boot chain: ask the owner.
    let owner = AutoApproval::signed(&root(), device(), vec![[7; 32]], NOW).unwrap();
    world.auto_approvals.insert(device(), owner);
    let boot = Boot::new(cert.clone(), 9);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [3; 32], NOW));
    let request = boot.answer(&challenge, Some(quote_for(&boot, &challenge)));
    assert!(matches!(
        service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW),
        SiblingFrame::Pending { .. }
    ));

    // The owner's setting and the accepted chain, but a quote made for
    // another request: ask the owner.
    let owner = AutoApproval::signed(&root(), device(), vec![chain], NOW).unwrap();
    world.auto_approvals.insert(device(), owner);
    let boot = Boot::new(cert.clone(), 10);
    let challenge =
        challenge_of(service.hello(&boot.hello, &world.view(), [4; 32], NOW + RATE_WINDOW_NS));
    let mut stale = challenge.clone();
    stale.nonce = [0; 32];
    let request = boot.answer(&challenge, Some(quote_for(&boot, &stale)));
    assert!(matches!(
        service.request(
            &request,
            &boot.hello,
            &challenge.nonce,
            &world.view(),
            NOW + RATE_WINDOW_NS
        ),
        SiblingFrame::Pending { .. }
    ));

    // Everything in place: released without a prompt, and logged.
    service.drain_history();
    let boot = Boot::new(cert, 11);
    let challenge =
        challenge_of(service.hello(&boot.hello, &world.view(), [5; 32], NOW + RATE_WINDOW_NS));
    let request = boot.answer(&challenge, Some(quote_for(&boot, &challenge)));
    let frame = service.request(
        &request,
        &boot.hello,
        &challenge.nonce,
        &world.view(),
        NOW + RATE_WINDOW_NS,
    );
    assert_eq!(boot.open(&request, &frame), half());
    assert_eq!(
        outcomes(&mut service),
        vec![UnlockOutcome::AutoApproved { boot_chain: chain }]
    );
}

/// A marshalled TPM2 quote over `pcrs`, as a TPM would produce it.
fn attest_bytes(extra: &[u8], pcrs: &[(u8, [u8; 32])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xff54_4347_u32.to_be_bytes());
    out.extend_from_slice(&0x8018_u16.to_be_bytes());
    out.extend_from_slice(&0_u16.to_be_bytes());
    out.extend_from_slice(&u16::try_from(extra.len()).unwrap().to_be_bytes());
    out.extend_from_slice(extra);
    out.extend_from_slice(&[0; 25]);
    out.extend_from_slice(&1_u32.to_be_bytes());
    out.extend_from_slice(&0x000b_u16.to_be_bytes());
    out.push(3);
    let mut bitmap = [0_u8; 3];
    for (index, _) in pcrs {
        bitmap[usize::from(*index / 8)] |= 1 << (index % 8);
    }
    out.extend_from_slice(&bitmap);
    out.extend_from_slice(&32_u16.to_be_bytes());
    out.extend_from_slice(&attest::pcr_composite(pcrs));
    out
}
