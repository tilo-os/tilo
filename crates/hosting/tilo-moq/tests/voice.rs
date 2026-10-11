#![allow(
    clippy::unwrap_used,
    reason = "fixture builders encode known-valid objects; a failure is the assertion"
)]

use tilo_moq::voice::{FRAME_SAMPLES, HEADER_BYTES, MAX_OBJECT_BYTES, VoiceObject};

const GOLDEN: &[u8] = &[
    1, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 24, 23, 22, 21, 20, 19, 18, 17, 192, 3, 128, 187, 0, 0, 1,
    0, 3, 0, 0, 0, 248, 255, 254,
];

fn fixture() -> VoiceObject {
    VoiceObject {
        sequence: 0x0102_0304_0506_0708,
        timestamp: 0x1112_1314_1516_1718,
        duration: FRAME_SAMPLES,
        payload: vec![0xf8, 0xff, 0xfe],
    }
}

#[test]
fn golden_object_matches_browser_wire_bytes() {
    assert_eq!(fixture().encode().unwrap(), GOLDEN);
    assert_eq!(VoiceObject::decode(GOLDEN).unwrap(), fixture());
}

#[test]
fn unsigned_64_bit_edges_and_maximum_payload_round_trip() {
    let object = VoiceObject {
        sequence: u64::MAX,
        timestamp: 0,
        payload: vec![7; MAX_OBJECT_BYTES - HEADER_BYTES],
        ..fixture()
    };
    let bytes = object.encode().unwrap();
    assert_eq!(bytes.len(), MAX_OBJECT_BYTES);
    assert_eq!(VoiceObject::decode(&bytes).unwrap(), object);
}

#[test]
fn rejects_truncated_extra_oversized_and_malformed_objects() {
    for end in 0..GOLDEN.len() {
        assert!(VoiceObject::decode(&GOLDEN[..end]).is_err());
    }
    let mut extra = GOLDEN.to_vec();
    extra.push(0);
    assert!(VoiceObject::decode(&extra).is_err());
    assert!(VoiceObject::decode(&vec![0; MAX_OBJECT_BYTES + 1]).is_err());
    for offset in [0, 1, 2, 3, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31] {
        let mut bytes = GOLDEN.to_vec();
        bytes[offset] ^= 1;
        assert!(VoiceObject::decode(&bytes).is_err(), "header byte {offset}");
    }
    let mut empty = GOLDEN[..HEADER_BYTES].to_vec();
    empty[28..32].fill(0);
    assert!(VoiceObject::decode(&empty).is_err());
    for duration in [0, 959, 961, u16::MAX] {
        assert!(
            VoiceObject {
                duration,
                ..fixture()
            }
            .encode()
            .is_err()
        );
    }
    for length in [0, MAX_OBJECT_BYTES - HEADER_BYTES + 1] {
        assert!(
            VoiceObject {
                payload: vec![0; length],
                ..fixture()
            }
            .encode()
            .is_err()
        );
    }
}

#[test]
fn datagram_bundles_hold_one_to_three_valid_objects() {
    use tilo_moq::voice::{bundle_objects, validate_bundle};
    let second = VoiceObject {
        sequence: fixture().sequence - 1,
        ..fixture()
    }
    .encode()
    .unwrap();
    let mut bundle = GOLDEN.to_vec();
    bundle.extend_from_slice(&second);
    assert_eq!(bundle_objects(&bundle).unwrap(), vec![GOLDEN, &second[..]]);
    assert!(validate_bundle(GOLDEN).is_ok());
    let mut four = bundle.clone();
    four.extend_from_slice(GOLDEN);
    assert!(validate_bundle(&four).is_ok());
    four.extend_from_slice(GOLDEN);
    assert!(validate_bundle(&four).is_err(), "at most three objects");
    for end in 1..bundle.len() {
        if end != GOLDEN.len() {
            assert!(
                validate_bundle(&bundle[..end]).is_err(),
                "truncated at {end}"
            );
        }
    }
    let mut corrupt = bundle.clone();
    corrupt[GOLDEN.len()] ^= 1;
    assert!(validate_bundle(&corrupt).is_err());
    assert!(validate_bundle(&[]).is_err());
}

fn frame(sequence: u64, payload: usize) -> Vec<u8> {
    VoiceObject {
        sequence,
        timestamp: sequence.saturating_mul(u64::from(FRAME_SAMPLES)),
        duration: FRAME_SAMPLES,
        payload: vec![7; payload],
    }
    .encode()
    .unwrap()
}

fn sequences(bundle: &[u8]) -> Vec<u64> {
    tilo_moq::voice::bundle_objects(bundle)
        .unwrap()
        .into_iter()
        .map(|object| VoiceObject::decode(object).unwrap().sequence)
        .collect()
}

#[test]
fn a_clean_hop_forwards_each_object_once_even_from_a_triple_redundant_publisher() {
    use tilo_moq::voice::Rebundler;
    let mut hop = Rebundler::default();
    // An older browser publisher always bundles the two previous objects.
    for sequence in 0..10_u64 {
        let mut bundle = frame(sequence, 80);
        for earlier in (sequence.saturating_sub(2)..sequence).rev() {
            bundle.extend_from_slice(&frame(earlier, 80));
        }
        let out = hop.next(&bundle, 0).unwrap().expect("a new object");
        assert_eq!(sequences(&out), vec![sequence], "one object per datagram");
        assert_eq!(out.len(), HEADER_BYTES + 80);
    }
    // A copy that carries nothing new is not forwarded at all.
    assert_eq!(hop.next(&frame(9, 80), 0).unwrap(), None);
    // Objects the uplink lost but a later bundle recovered still go out.
    let mut recovered = frame(12, 80);
    recovered.extend_from_slice(&frame(11, 80));
    recovered.extend_from_slice(&frame(10, 80));
    assert_eq!(
        sequences(&hop.next(&recovered, 0).unwrap().unwrap()),
        vec![12, 11, 10]
    );
}

#[test]
fn a_lossy_hop_adds_the_frames_just_before_up_to_three_objects() {
    use tilo_moq::voice::Rebundler;
    let mut hop = Rebundler::default();
    let mut next = |sequence, redundancy| {
        sequences(&hop.next(&frame(sequence, 80), redundancy).unwrap().unwrap())
    };
    assert_eq!(next(0, 1), vec![0]);
    assert_eq!(next(1, 1), vec![1, 0]);
    assert_eq!(next(2, 2), vec![2, 1, 0]);
    assert_eq!(next(3, 5), vec![3, 2, 1], "never more than three objects");
    assert_eq!(next(4, 0), vec![4]);
    // Redundant copies stay within one datagram's budget...
    let mut big = Rebundler::default();
    for sequence in 0..3 {
        let _ = big.next(&frame(sequence, 600), 2).unwrap();
    }
    assert_eq!(
        sequences(&big.next(&frame(3, 600), 2).unwrap().unwrap()),
        vec![3]
    );
    // ...and never reach back across a DTX pause: those frames are long late.
    let mut paused = Rebundler::default();
    let _ = paused.next(&frame(0, 80), 2).unwrap();
    let after_pause = VoiceObject {
        sequence: 1,
        timestamp: 20 * u64::from(FRAME_SAMPLES),
        duration: FRAME_SAMPLES,
        payload: vec![7; 30],
    }
    .encode()
    .unwrap();
    assert_eq!(
        sequences(&paused.next(&after_pause, 2).unwrap().unwrap()),
        vec![1]
    );
    // A restarted publisher (sequence back at zero) is forwarded afresh.
    let mut restarted = Rebundler::default();
    let _ = restarted.next(&frame(5_000, 80), 0).unwrap();
    assert_eq!(
        sequences(&restarted.next(&frame(0, 80), 0).unwrap().unwrap()),
        vec![0]
    );
    assert!(restarted.next(&[0; 40], 0).is_err());
}

#[test]
fn redundancy_follows_measured_loss_up_at_once_and_down_after_a_calm_hold() {
    use std::time::{Duration, Instant};
    use tilo_moq::voice::{REDUNDANCY_HOLD, Redundancy};
    let start = Instant::now();
    let mut hop = Redundancy::default();
    let (mut sent, mut lost) = (0_u64, 0_u64);
    let mut window = |hop: &mut Redundancy, second: u64, packets: u64, losses: u64| {
        sent = sent.saturating_add(packets);
        lost = lost.saturating_add(losses);
        hop.observe(start + Duration::from_secs(second), sent, lost)
    };
    assert_eq!(
        window(&mut hop, 0, 0, 0),
        0,
        "the first sample is a baseline"
    );
    assert_eq!(window(&mut hop, 1, 100, 0), 0, "clean");
    assert_eq!(
        window(&mut hop, 2, 100, 1),
        1,
        "1% loss: repair isolated losses"
    );
    assert_eq!(
        window(&mut hop, 3, 100, 6),
        2,
        "5% loss: repair 40 ms bursts"
    );
    assert_eq!(
        window(&mut hop, 4, 3, 0),
        2,
        "a silent window measures nothing"
    );
    let mut levels = Vec::new();
    let mut second = 5;
    while second <= 5 + 3 * REDUNDANCY_HOLD.as_secs() {
        levels.push(window(&mut hop, second, 100, 0));
        second += 1;
    }
    assert_eq!(
        levels.first(),
        Some(&2),
        "one calm window does not step down"
    );
    assert!(levels.contains(&1), "steps down one level at a time");
    assert_eq!(
        levels.last(),
        Some(&0),
        "a calm hop returns to no redundancy"
    );
    assert_eq!(
        window(&mut hop, second, 100, 10),
        2,
        "loss returns: up at once"
    );
}
