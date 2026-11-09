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
        issuer: None,
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
    /// What the person's devices answered, as their fabric carries it.
    decisions: BTreeMap<DecisionKey, UnlockDecisionRecord>,
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
            decisions: BTreeMap::new(),
            secret_sync,
            custodian: true,
        }
    }

    fn view(&self) -> UnlockView<'_> {
        UnlockView {
            fleet: &self.fleet,
            enrolments: &self.enrolments,
            auto_approvals: &self.auto_approvals,
            decisions: &self.decisions,
            custodian: self.custodian,
            secret_sync: self.custodian.then_some(&self.secret_sync),
        }
    }

    /// What the fabric does with an answer: the answering device keeps it,
    /// and this device's merged view carries it.
    fn kept(&mut self, decided: &Decided) {
        let Some(record) = &decided.record else {
            return;
        };
        record
            .validate(&DeviceId::new(hex::encode(sibling())).unwrap())
            .unwrap();
        self.decisions.insert(record.key(), record.clone());
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

    let decided = service
        .decide(
            &pending[0].request_digest,
            UnlockDecision::Approve {
                approver: sibling(),
            },
            &world.view(),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(boot.open(&request, &decided.frame), half());
    let record = decided
        .record
        .expect("an approval is the person's own signed answer");
    assert_eq!(record.outcome, DecisionOutcome::Approve);
    assert_eq!(record.decided_by, sibling());
    assert_eq!(record.decided_at_ns, NOW + 1);
    assert_eq!(record.boot_id, boot.id);
    assert_eq!(record.slot, slot());
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
    let mut world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert.clone(), 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let digest = service.pending().next().unwrap().request_digest;
    let decided = service
        .decide(
            &digest,
            UnlockDecision::Deny {
                approver: sibling(),
            },
            &world.view(),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(decided.frame, SiblingFrame::Denied);
    let record = decided
        .record
        .clone()
        .expect("a denial is the person's own signed answer");
    assert_eq!(record.outcome, DecisionOutcome::Deny);
    world.kept(&decided);

    // Nobody answers the next boot's request, and its handler goes away with
    // the boot: once its window passes it is no longer pending, and nothing
    // is claimed in the person's name.
    let boot = Boot::new(cert.clone(), 8);
    let late = NOW + APPROVAL_TIMEOUT_NS;
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [10; 32], late));
    let request = boot.answer(&challenge, None);
    assert!(matches!(
        service.request(&request, &boot.hello, &challenge.nonce, &world.view(), late),
        SiblingFrame::Pending { .. }
    ));
    assert_eq!(service.pending().count(), 1);
    let afterwards = late + APPROVAL_TIMEOUT_NS;
    let fresh = Boot::new(cert, 9);
    let challenge = challenge_of(service.hello(&fresh.hello, &world.view(), [11; 32], afterwards));
    let request = fresh.answer(&challenge, None);
    assert!(matches!(
        service.request(
            &request,
            &fresh.hello,
            &challenge.nonce,
            &world.view(),
            afterwards
        ),
        SiblingFrame::Pending { .. }
    ));
    assert_eq!(service.pending().count(), 1, "the spent question is gone");
    assert_eq!(
        world.decisions.len(),
        1,
        "the person's record holds the one answer they made; a question nobody saw is not a denial in their name"
    );
    assert!(world.decisions.contains_key(&record.key()));
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
        .unwrap()
        .frame;

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
        service
            .decide(
                &digest,
                UnlockDecision::Approve {
                    approver: sibling()
                },
                &world.view(),
                NOW
            )
            .map(|decided| decided.frame),
        Some(SiblingFrame::Refused(RefusalReason::Revoked))
    );
    // And any new request is refused at the hello.
    assert_eq!(
        service.hello(&boot.hello, &world.view(), [10; 32], NOW),
        SiblingFrame::Refused(RefusalReason::Revoked)
    );
    service.remove_device(device());
    assert_eq!(
        service.pending().count(),
        0,
        "nothing is kept waiting for a device that left the fleet"
    );
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

    // Everything in place: released without a prompt.
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

#[test]
fn a_denied_boot_hears_it_again_without_a_second_question() {
    let cert = boot_cert(None);
    let mut world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert.clone(), 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    assert!(matches!(
        service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW),
        SiblingFrame::Pending { .. }
    ));
    let digest = service.pending().next().unwrap().request_digest;
    let decided = service
        .decide(
            &digest,
            UnlockDecision::Deny {
                approver: sibling(),
            },
            &world.view(),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(decided.frame, SiblingFrame::Denied);
    world.kept(&decided);

    // The boot's stream went away before the answer reached it: it comes back
    // with a fresh challenge and its own new key, and hears the same answer.
    let mut again = Boot::new(cert, 7);
    again.ephemeral = SecretKey::from_bytes([13; 32]);
    let challenge = challenge_of(service.hello(&again.hello, &world.view(), [10; 32], NOW + 2));
    let request = again.answer(&challenge, None);
    assert_eq!(
        service.request(
            &request,
            &again.hello,
            &challenge.nonce,
            &world.view(),
            NOW + 2
        ),
        SiblingFrame::Denied
    );
    assert_eq!(
        service.pending().count(),
        0,
        "nobody is asked a second time"
    );
    assert_eq!(
        world.decisions.len(),
        1,
        "the person's record holds the one answer they made"
    );

    // Once the answer no longer holds, the person is asked again.
    let later = NOW + DECISION_LIFETIME_NS + 1;
    let challenge = challenge_of(service.hello(&again.hello, &world.view(), [11; 32], later));
    let request = again.answer(&challenge, None);
    assert!(matches!(
        service.request(
            &request,
            &again.hello,
            &challenge.nonce,
            &world.view(),
            later
        ),
        SiblingFrame::Pending { .. }
    ));
}

#[test]
fn an_allowed_boot_that_reconnects_gets_a_half_for_its_own_key() {
    let cert = boot_cert(None);
    let mut world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert.clone(), 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let digest = service.pending().next().unwrap().request_digest;
    let decided = service
        .decide(
            &digest,
            UnlockDecision::Approve {
                approver: sibling(),
            },
            &world.view(),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(boot.open(&request, &decided.frame), half());
    world.kept(&decided);

    // The boot that asked never read its release: its next attempt draws a
    // new key, and the half is sealed to that one.
    let mut again = Boot::new(cert, 7);
    again.ephemeral = SecretKey::from_bytes([14; 32]);
    let challenge = challenge_of(service.hello(&again.hello, &world.view(), [10; 32], NOW + 2));
    let request = again.answer(&challenge, None);
    let frame = service.request(
        &request,
        &again.hello,
        &challenge.nonce,
        &world.view(),
        NOW + 2,
    );
    assert_eq!(again.open(&request, &frame), half());
    assert_eq!(
        service.pending().count(),
        0,
        "nobody is asked a second time"
    );
    assert_eq!(
        world.decisions.len(),
        1,
        "releasing the same boot again adds no second answer"
    );
}

#[test]
fn boots_whose_handlers_died_do_not_refuse_later_boots() {
    let cert = boot_cert(None);
    let world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    // Seventeen boots asked, and every handler died before the person
    // answered. Each question is spent by the time the next boot asks, so a
    // later boot still has the person's attention instead of a refusal that
    // nobody can act on.
    for n in 0..=MAX_PENDING {
        let at = NOW + u64::try_from(n).unwrap() * RATE_WINDOW_NS;
        let boot = Boot::new(cert.clone(), u8::try_from(n).unwrap());
        let nonce = [u8::try_from(n).unwrap(); 32];
        let challenge = challenge_of(service.hello(&boot.hello, &world.view(), nonce, at));
        let request = boot.answer(&challenge, None);
        assert!(
            matches!(
                service.request(&request, &boot.hello, &challenge.nonce, &world.view(), at),
                SiblingFrame::Pending { .. }
            ),
            "boot {n} is asked for"
        );
    }
    assert!(
        service.pending().count() <= 1,
        "the questions nobody can answer are let go"
    );
}

#[test]
fn a_boot_that_went_away_gives_its_question_up() {
    let cert = boot_cert(None);
    let world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert, 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let digest = service.pending().next().unwrap().request_digest;
    service.withdraw(&digest);
    assert_eq!(service.pending().count(), 0);
    assert!(
        service
            .decide(
                &digest,
                UnlockDecision::Deny {
                    approver: sibling()
                },
                &world.view(),
                NOW + 1
            )
            .is_none(),
        "an answer on a question that went away decides nothing"
    );
}

#[test]
fn answers_merge_from_the_person_devices_only_while_they_hold() {
    let cert = boot_cert(None);
    let world = World::new(&cert);
    let mut service = UnlockSibling::new(sibling());
    let boot = Boot::new(cert, 7);
    let challenge = challenge_of(service.hello(&boot.hello, &world.view(), [9; 32], NOW));
    let request = boot.answer(&challenge, None);
    service.request(&request, &boot.hello, &challenge.nonce, &world.view(), NOW);
    let digest = service.pending().next().unwrap().request_digest;
    let record = service
        .decide(
            &digest,
            UnlockDecision::Deny {
                approver: sibling(),
            },
            &world.view(),
            NOW + 1,
        )
        .unwrap()
        .record
        .unwrap();

    let answering = DeviceId::new(hex::encode(sibling())).unwrap();
    let state = UnlockFabricState {
        decisions: vec![record.clone()],
        ..UnlockFabricState::default()
    };
    state.validate(&answering).unwrap();
    // A device writing another's answer is not believed.
    assert!(
        state
            .validate(&DeviceId::new(hex::encode(device())).unwrap())
            .is_err()
    );
    // Nor are two answers to one boot, or one that holds for no time at all.
    let mut doubled = state.clone();
    doubled.decisions.push(record.clone());
    assert!(doubled.validate(&answering).is_err());
    let mut unheld = record.clone();
    unheld.valid_until_ns = unheld.decided_at_ns;
    assert!(
        UnlockFabricState {
            decisions: vec![unheld],
            ..UnlockFabricState::default()
        }
        .validate(&answering)
        .is_err()
    );

    let found = |merged: &MergedUnlock, now: u64| {
        decision_for(
            &merged.decisions,
            &record.boot_cert_digest,
            record.boot_id,
            record.slot,
            now,
        )
        .map(|found| found.outcome)
    };
    let merged = merged_unlock_state([&state], &world.fleet, NOW + 2);
    assert_eq!(found(&merged, NOW + 2), Some(DecisionOutcome::Deny));
    // After its lifetime, nothing is in force.
    let merged = merged_unlock_state([&state], &world.fleet, record.valid_until_ns);
    assert_eq!(found(&merged, record.valid_until_ns), None);

    // An approval made later on another of the person's devices wins, and one
    // made by a device the fleet does not certify is not believed at all.
    let mut approved = record.clone();
    approved.outcome = DecisionOutcome::Approve;
    approved.decided_at_ns = NOW + 600;
    approved.valid_until_ns = NOW + 1200;
    let approval = UnlockFabricState {
        decisions: vec![approved],
        ..UnlockFabricState::default()
    };
    let merged = merged_unlock_state([&state, &approval], &world.fleet, NOW + 700);
    assert_eq!(found(&merged, NOW + 700), Some(DecisionOutcome::Approve));
    let mut stranger = record.clone();
    stranger.decided_by = [42; 32];
    stranger.decided_at_ns = NOW + 900;
    stranger.valid_until_ns = NOW + 1500;
    let merged = merged_unlock_state(
        [
            &state,
            &UnlockFabricState {
                decisions: vec![stranger],
                ..UnlockFabricState::default()
            },
        ],
        &world.fleet,
        NOW + 1000,
    );
    assert_eq!(found(&merged, NOW + 1000), Some(DecisionOutcome::Deny));
}
