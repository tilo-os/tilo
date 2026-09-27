#![expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "fixture layouts are built from small constant entry counts"
)]
use super::*;
fn put(b: &mut [u8], at: usize, value: u32) {
    b[at..at + 4].copy_from_slice(&value.to_ne_bytes());
}
fn caps() -> [u8; 56] {
    let mut b = [0; 56];
    for (at, v) in [
        (0, 56),
        (4, RETURNED),
        (8, VOL_CAPS),
        (28, EXTENDED),
        (44, EXTENDED),
    ] {
        put(&mut b, at, v);
    }
    b
}
fn acl(count: usize) -> Vec<u8> {
    let mut b = vec![0; 76 + count * 24];
    let len = b.len() as u32;
    for (at, v) in [
        (0, len),
        (4, RETURNED | SECURITY),
        (24, 8),
        (28, 44 + count as u32 * 24),
        (32, MAGIC),
        (68, count as u32),
    ] {
        put(&mut b, at, v);
    }
    for i in 0..count {
        put(&mut b, 92 + i * 24, 2);
    }
    b
}
#[test]
fn bounded_privacy_parser_requires_affirmative_supported_capability_before_omitted_acl() {
    let good = caps();
    capabilities(&good).unwrap();
    for at in [8, 28, 44] {
        let mut b = good;
        put(&mut b, at, 0);
        assert!(capabilities(&b).is_err());
    }
    for len in 0..56 {
        assert!(capabilities(&good[..len]).is_err());
    }
    let mut empty = [0; 32];
    put(&mut empty, 0, 32);
    put(&mut empty, 4, RETURNED);
    security(&empty).unwrap();
    empty[31] = 1;
    assert!(security(&empty).is_err());
    put(&mut empty, 4, RETURNED | SECURITY);
    assert!(security(&empty).is_err());
}
#[test]
fn bounded_privacy_parser_maximum_deny_and_malformed_ranges_do_not_widen_profile() {
    let good = acl(128);
    security(&good).unwrap();
    assert!(security(&acl(129)).is_err());
    for len in 0..good.len() {
        assert!(security(&good[..len]).is_err());
    }
    for (at, v) in [
        (0, u32::MAX),
        (24, u32::MAX),
        (24, 4),
        (28, u32::MAX),
        (32, 0),
        (68, u32::MAX),
        (72, 1 << 16),
        (92, 1),
        (92, 3),
        (92, 2 | (1 << 20)),
    ] {
        let mut b = good.clone();
        put(&mut b, at, v);
        assert!(security(&b).is_err(), "offset{at}");
    }
    for flags in [2, 2 | 16, 2 | 32 | 64 | 128 | 256] {
        let mut b = acl(1);
        put(&mut b, 92, flags);
        security(&b).unwrap();
    }
}

#[test]
fn bounded_privacy_normalization_preserves_filesystem_private_flags() {
    let mut current = acl(1);
    put(&mut current, 92, 1);
    put(&mut current, 72, 0x0001_00a5);
    let input = normalized_input(&current).unwrap();
    assert_eq!(word(&input, 48).unwrap(), 0x0002_00a5);
    assert_eq!(word(&input, 44).unwrap(), 0);
    put(&mut current, 72, 1 << 31);
    assert!(normalized_input(&current).is_err());
}
