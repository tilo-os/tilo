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
