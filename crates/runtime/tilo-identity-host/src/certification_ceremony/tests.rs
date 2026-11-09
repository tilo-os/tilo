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
        issuer: None,
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

/// A member certificate of `person` for `device_key`, issued `ago_ns` before
/// `NOW`, expiring a day later, as a joined device holds one.
fn member_cert(
    person: [u8; 32],
    device_key: &ed25519_dalek::SigningKey,
    issued: u64,
) -> DeviceCert {
    DeviceCert {
        user_public_key: person,
        device_public_key: device_key.verifying_key().to_bytes(),
        device_label: "tilo-yoga".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: issued,
        expires_at_ns: Some(issued.saturating_add(24 * 60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    }
}

/// The certificate a joined device asks to have renewed: the same device and
/// root, issued now, expiring as the held one did.
fn renewal_of(held: &DeviceCert, now: u64) -> DeviceCert {
    let mut cert = held.clone();
    cert.issued_at_ns = now;
    cert.expires_at_ns = held
        .expires_at_ns
        .map(|expires| now.saturating_add(expires.saturating_sub(held.issued_at_ns)));
    cert.signature = [0; 64];
    cert
}

#[test]
fn a_key_holder_renews_a_joined_devices_lapsed_certificate_without_asking_anyone() {
    let holder = whole(1);
    let person = holder.group_public_key();
    let joined = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
    // The certificate it holds ran out: issued two days before NOW, a day
    // long. A renewal keeps it expiring.
    let held = member_cert(person, &joined, NOW - 48 * 60 * 60 * 1_000_000_000);
    let renewed = renewal_of(&held, NOW);

    let mut gathering = Gathering::renewal([2; 16], renewed, held.clone(), SERVICE, 1, 1);
    let ask = gathering.ask().clone();
    check_renewal(&ask, &held, Some(&held), person, &|_| false).unwrap();

    // Nobody is asked: the device is already the person's, and its renewal
    // grants nothing new. The key holder signs it at once.
    let reply = Signer::default().renew(&holder, &ask, NOW);
    let Progress::Certified(cert) = gathering.answer(device(1), answer(&ask, reply)) else {
        panic!("the person's key holder renews the certificate");
    };
    verify_device_cert(&cert).unwrap();
    assert_eq!(cert.device_public_key, held.device_public_key);
    assert_eq!(cert.user_public_key, person);
    assert_eq!(cert.issued_at_ns, NOW);
    assert!(
        cert.expires_at_ns.is_some_and(|expires| expires > NOW),
        "the renewal is valid now and still expires"
    );
    assert!(cert.renews(&held));
}

#[test]
fn a_renewal_that_widens_or_comes_from_another_root_is_refused() {
    let holder = whole(1);
    let person = holder.group_public_key();
    let joined = ed25519_dalek::SigningKey::from_bytes(&[78; 32]);
    let held = member_cert(person, &joined, NOW - 48 * 60 * 60 * 1_000_000_000);
    let ask_of = |cert: DeviceCert, held: DeviceCert| Ask {
        session: [3; 16],
        cert,
        service: SERVICE,
        step: Step::Renew { held },
    };

    // Wider than the certificate it replaces.
    let mut wider = renewal_of(&held, NOW);
    wider.capabilities = DeviceCapabilities {
        can_read_all_workspaces: true,
        ..held.capabilities
    };
    let reason = check_renewal(
        &ask_of(wider, held.clone()),
        &held,
        Some(&held),
        person,
        &|_| false,
    )
    .expect_err("a renewal may not widen what a device may do");
    assert!(reason.contains("granting no more"), "{reason}");

    // Not later than the certificate it replaces.
    let mut older = renewal_of(&held, NOW);
    older.issued_at_ns = held.issued_at_ns;
    assert!(
        check_renewal(
            &ask_of(older, held.clone()),
            &held,
            Some(&held),
            person,
            &|_| false
        )
        .is_err()
    );

    // Another person's root.
    let mut foreign = renewal_of(&held, NOW);
    foreign.user_public_key = [0xee; 32];
    assert!(
        check_renewal(
            &ask_of(foreign, held.clone()),
            &held,
            Some(&held),
            person,
            &|_| false
        )
        .is_err()
    );

    // The device was removed from the person.
    let renewed = renewal_of(&held, NOW);
    let reason = check_renewal(
        &ask_of(renewed, held.clone()),
        &held,
        Some(&held),
        person,
        &|_| true,
    )
    .expect_err("a removed device is not renewed");
    assert!(reason.contains("removed"), "{reason}");

    // The fleet already holds a newer certificate: the renewal replaces
    // that one or nothing, never the lapsed certificate the asker names.
    let newer = renewal_of(&held, NOW + 1);
    let stale = renewal_of(&held, NOW);
    let reason = check_renewal(
        &ask_of(stale, held.clone()),
        &held,
        Some(&newer),
        person,
        &|_| false,
    )
    .expect_err("a renewal older than the one this identity holds replaces nothing");
    assert!(reason.contains("does not replace"), "{reason}");
}

#[test]
fn a_share_renews_through_the_same_commitment_round_as_a_certification() {
    let devices = two_of_three();
    let person = devices[0].group_public_key();
    let joined = ed25519_dalek::SigningKey::from_bytes(&[79; 32]);
    let held = member_cert(person, &joined, NOW - 48 * 60 * 60 * 1_000_000_000);
    let renewed = renewal_of(&held, NOW);

    let mut gathering = Gathering::renewal([4; 16], renewed, held.clone(), SERVICE, 2, 3);
    let ask = gathering.ask().clone();
    // Each share is on a device of its own, with its own signer state, and
    // commits without anyone being asked.
    let mut first_signer = Signer::default();
    let mut second_signer = Signer::default();
    let first = first_signer.renew(&devices[0], &ask, NOW);
    let second = second_signer.renew(&devices[1], &ask, NOW);
    let progress = gathering.answer(device(1), answer(&ask, first));
    assert!(
        matches!(progress, Progress::Waiting),
        "one share is not enough"
    );
    let progress = gathering.answer(device(2), answer(&ask, second));
    let Progress::Send(rounds) = progress else {
        panic!("two shares of three begin the signing round");
    };
    assert_eq!(rounds.len(), 2);
    let mut progress = Progress::Waiting;
    for (to, signing) in rounds {
        let reply = if to == device(1) {
            first_signer.sign(&devices[0], &signing, NOW)
        } else {
            second_signer.sign(&devices[1], &signing, NOW)
        };
        progress = gathering.answer(to, answer(&signing, reply));
    }
    let Progress::Certified(cert) = progress else {
        panic!("the shares combine into a renewal");
    };
    verify_device_cert(&cert).unwrap();
    assert!(cert.renews(&held));
}
