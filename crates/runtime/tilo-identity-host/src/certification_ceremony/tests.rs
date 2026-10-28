#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use getrandom::{SysRng, rand_core::UnwrapErr};
use runtime_identity::SigningIdentity;
use threshold_identity::{EnrolmentRound2, ThresholdIdentity};
use tilo_user_identity::DeviceCapabilities;

const NOW: u64 = 1_800_000_000_000_000_000;
const SERVICE: [u8; 32] = [9; 32];

fn record(
    group: [u8; 32],
    single: bool,
    shares: (u16, u16, u16),
    secret: &[u8],
) -> SigningIdentity {
    let (threshold, total, index) = shares;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "display_name": "Fixture", "avatar_color": [1, 2, 3],
        "created_at": "2026-10-22T00:00:00Z", "schema_version": 1,
        "threshold_identity": {
            "single_device": single, "group_public_key": group, "threshold": threshold,
            "total_shares": total, "share_index": index, "secret_bytes": secret,
        },
    }))
    .unwrap();
    runtime_identity::load_from_bytes(&bytes).unwrap()
}

/// A device holding the whole key of the person `seed` makes.
fn whole(seed: u8) -> SigningIdentity {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    record(key.verifying_key().to_bytes(), true, (1, 1, 1), &[seed; 32])
}

/// A device of `person` that holds none of the key.
fn keyless(person: [u8; 32]) -> SigningIdentity {
    record(person, false, (1, 1, 0), &[])
}

/// Three devices each holding one share of a 2-of-3 key.
fn two_of_three() -> Vec<SigningIdentity> {
    let mut rng = UnwrapErr(SysRng);
    let mut devices: Vec<ThresholdIdentity> = (0..3)
        .map(|_| ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap())
        .collect();
    let mut sessions = Vec::new();
    let mut round1 = Vec::new();
    for (index, device) in (1_u16..).zip(devices.iter_mut()) {
        let (session, first) = device.begin_enrolment(index, 2, 3, &mut rng).unwrap();
        sessions.push(session);
        round1.push(first);
    }
    let round2: Vec<_> = sessions
        .iter_mut()
        .enumerate()
        .map(|(me, session)| {
            let others: Vec<_> = round1
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != me)
                .map(|(_, first)| first.clone())
                .collect();
            session.receive_round1(&others).unwrap()
        })
        .collect();
    for (me, session) in sessions.iter_mut().enumerate() {
        let index = u16::try_from(me).unwrap().checked_add(1).unwrap();
        let mine: Vec<EnrolmentRound2> = round2
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != me)
            .map(|(_, out)| out[&index].clone())
            .collect();
        session.receive_round2(&mine).unwrap();
    }
    for (device, session) in devices.iter_mut().zip(sessions) {
        device.adopt_dkg_outcome(session).unwrap();
    }
    devices
        .iter()
        .map(|device| {
            record(
                *device.group_public_key().as_bytes(),
                false,
                (2, 3, device.this_share_index),
                &device.export_device_secret().unwrap(),
            )
        })
        .collect()
}

fn proposal(person: [u8; 32], label: &str) -> DeviceCert {
    DeviceCert {
        user_public_key: person,
        device_public_key: ed25519_dalek::SigningKey::from_bytes(&[42; 32])
            .verifying_key()
            .to_bytes(),
        device_label: label.into(),
        capabilities: EnrollmentRole::PersonalDevice.preset(),
        issued_at_ns: NOW,
        expires_at_ns: None,
        signature: [0; 64],
    }
}

fn answer(ask: &Ask, reply: Reply) -> Answer {
    Answer {
        session: ask.session,
        reply,
    }
}

const fn device(byte: u8) -> [u8; 32] {
    [byte; 32]
}

#[test]
fn one_device_holding_the_whole_key_certifies_alone() {
    let holder = whole(1);
    let person = holder.group_public_key();
    let mut gathering = Gathering::new([1; 16], proposal(person, "Phone"), SERVICE, 1, 1);
    let ask = gathering.ask().clone();
    check_proposal(&ask, person, &|_| false, NOW).unwrap();

    let keyless_reply = Signer::default().approve(&keyless(person), &ask, true, NOW);
    assert!(matches!(keyless_reply, Reply::HoldsNoKey));
    assert!(matches!(
        gathering.answer(device(2), answer(&ask, keyless_reply)),
        Progress::Waiting
    ));
    assert!(gathering.status().contains("other device"));

    let reply = Signer::default().approve(&holder, &ask, true, NOW);
    let Progress::Certified(cert) = gathering.answer(device(1), answer(&ask, reply)) else {
        panic!("one approval of the whole key certifies");
    };
    verify_device_cert(&cert).unwrap();
    assert_eq!(cert.device_label, "Phone");
    assert_eq!(cert.user_public_key, person);
}

#[test]
fn the_only_key_holder_declining_ends_it() {
    let holder = whole(3);
    let person = holder.group_public_key();
    let mut gathering = Gathering::new([2; 16], proposal(person, "Phone"), SERVICE, 1, 1);
    let ask = gathering.ask().clone();
    let reply = Signer::default().approve(&holder, &ask, false, NOW);
    assert!(matches!(reply, Reply::Declined));
    assert!(matches!(
        gathering.answer(device(1), answer(&ask, reply)),
        Progress::Failed(_)
    ));
}

#[test]
fn a_threshold_of_shares_certifies_together_and_no_fewer() {
    let shares = two_of_three();
    let person = shares[0].group_public_key();
    let mut gathering = Gathering::new([3; 16], proposal(person, "Tablet"), SERVICE, 2, 3);
    let ask = gathering.ask().clone();
    let mut signers: Vec<Signer> = (0..3).map(|_| Signer::default()).collect();

    let first = signers[0].approve(&shares[0], &ask, true, NOW);
    assert!(matches!(first, Reply::Committed(_)));
    assert!(matches!(
        gathering.answer(device(1), answer(&ask, first)),
        Progress::Waiting
    ));
    assert!(
        gathering.status().contains("2 of your devices"),
        "{}",
        gathering.status()
    );
    assert!(
        gathering.status().contains("1 approved"),
        "{}",
        gathering.status()
    );

    let declined = signers[2].approve(&shares[2], &ask, false, NOW);
    assert!(matches!(
        gathering.answer(device(3), answer(&ask, declined)),
        Progress::Waiting
    ));

    let second = signers[1].approve(&shares[1], &ask, true, NOW);
    let Progress::Send(rounds) = gathering.answer(device(2), answer(&ask, second)) else {
        panic!("two approvals start the signing round");
    };
    assert_eq!(rounds.len(), 2);
    let mut certified = None;
    for (to, sign) in rounds {
        let index = usize::from(to[0]) - 1;
        let partial = signers[index].sign(&shares[index], &sign, NOW);
        assert!(matches!(partial, Reply::Partial(_)), "{partial:?}");
        if let Progress::Certified(cert) = gathering.answer(to, answer(&sign, partial)) {
            certified = Some(cert);
        }
    }
    let cert = certified.expect("both partials certify");
    verify_device_cert(&cert).unwrap();
    assert_eq!(cert.user_public_key, person);
}

#[test]
fn a_share_signs_only_what_its_person_approved_once() {
    let shares = two_of_three();
    let person = shares[0].group_public_key();
    let ask = Gathering::new([4; 16], proposal(person, "Tablet"), SERVICE, 2, 3)
        .ask()
        .clone();
    let mut signer = Signer::default();
    let Reply::Committed(mine) = signer.approve(&shares[0], &ask, true, NOW) else {
        panic!("a share commits");
    };
    let Reply::Committed(other) = Signer::default().approve(&shares[1], &ask, true, NOW) else {
        panic!("a share commits");
    };
    let sign = |cert: DeviceCert| Ask {
        cert,
        step: Step::Sign {
            commitments: vec![mine.clone(), other.clone()],
        },
        ..ask.clone()
    };
    let renamed = sign(proposal(person, "Someone else's laptop"));
    assert!(matches!(
        signer.sign(&shares[0], &renamed, NOW),
        Reply::Refused(_)
    ));
    // Refusing spends the approval: nothing it approved is signed later.
    assert!(matches!(
        signer.sign(&shares[0], &sign(ask.cert.clone()), NOW),
        Reply::Refused(_)
    ));
    let unapproved = Signer::default().sign(&shares[2], &sign(ask.cert.clone()), NOW);
    assert!(matches!(unapproved, Reply::Refused(_)));
}

#[test]
fn a_forged_signature_does_not_certify() {
    let person = whole(5).group_public_key();
    let mut gathering = Gathering::new([5; 16], proposal(person, "Phone"), SERVICE, 1, 1);
    let ask = gathering.ask().clone();
    let stranger = Signer::default().approve(&whole(6), &ask, true, NOW);
    assert!(matches!(stranger, Reply::Refused(_)));
    assert!(matches!(
        gathering.answer(device(6), answer(&ask, Reply::Signed(vec![7; 64]))),
        Progress::Failed(_)
    ));
}

#[test]
fn proposals_are_checked_before_anyone_is_asked() {
    let person = whole(8).group_public_key();
    let ask = |cert: DeviceCert| Ask {
        session: [6; 16],
        cert,
        service: SERVICE,
        step: Step::Approve,
    };
    let good = proposal(person, "Phone");
    check_proposal(&ask(good.clone()), person, &|_| false, NOW).unwrap();

    let revoked = check_proposal(
        &ask(good.clone()),
        person,
        &|key| *key == good.device_public_key,
        NOW,
    );
    assert!(revoked.unwrap_err().contains("removed"));
    assert!(check_proposal(&ask(good.clone()), [1; 32], &|_| false, NOW).is_err());

    let mut signed = good.clone();
    signed.signature = [1; 64];
    assert!(check_proposal(&ask(signed), person, &|_| false, NOW).is_err());

    let mut host = good.clone();
    host.capabilities = DeviceCapabilities::host();
    assert!(check_proposal(&ask(host), person, &|_| false, NOW).is_err());

    let mut stale = good.clone();
    stale.issued_at_ns = NOW - 2 * SESSION_LIFETIME_NS;
    assert!(check_proposal(&ask(stale), person, &|_| false, NOW).is_err());

    let mut expiring = good;
    expiring.expires_at_ns = Some(NOW + 1);
    assert!(check_proposal(&ask(expiring), person, &|_| false, NOW).is_err());

    assert!(check_proposal(&ask(proposal(person, " ")), person, &|_| false, NOW).is_err());
}

#[test]
fn every_screen_shows_the_same_six_digits() {
    let code = pairing_code([1; 32], [2; 32]);
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));
    assert_eq!(code, pairing_code([1; 32], [2; 32]));
    assert_ne!(code, pairing_code([1; 32], [3; 32]));
}
