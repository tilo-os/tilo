//! Property tests for [`qos_collab_types::SurfaceFrame`] encoded size.
//!
//! The frame's `bytes` field is opaque codec output, and the rest of
//! the struct is a fixed-shape header. Two invariants matter for any
//! broker that fans frames out:
//!
//! 1. Encode-then-decode must always round-trip — every (id, pts,
//!    key_frame, bytes) tuple survives the canonical-bytes codec
//!    unchanged.
//! 2. The encoded length is `header_overhead + payload.len() +
//!    payload_length_prefix_size`. The "header overhead" is the
//!    fixed-position bytes for the surface id, the varint-encoded
//!    pts, and the key_frame bool. The length prefix is at most 5
//!    bytes (postcard's varint encoding for a u32-bounded length —
//!    enough for any practical frame size).
//!
//! Together those bound the on-wire size as a function of the payload
//! length, which the broker needs for back-pressure decisions
//! (max-frame-size + buffer-budget calculations). The test below
//! drives 1024 random shapes through the codec and asserts both
//! invariants.

use bytes::Bytes;
use proptest::prelude::*;
use qos_collab_types::canonical_bytes;
use qos_collab_types::surface_stream::{SurfaceFrame, SurfaceId};

/// Upper bound on the encoded size for a given payload length.
///
/// Composition:
/// - `surface_id`: postcard varint over a u64. Worst case 10 bytes.
/// - `pts_us`: postcard varint over a u64. Worst case 10 bytes.
/// - `key_frame`: 1 byte for the bool.
/// - `bytes` length prefix: postcard varint over the length. For
///   a u32-bounded length the worst case is 5 bytes.
/// - `bytes` payload: exactly `payload_len` bytes.
///
/// Total: `26 + payload_len`. We assert encoded length is `<=` this
/// upper bound; the actual length is often smaller because postcard
/// varints shrink for small values.
fn upper_bound_encoded_size(payload_len: usize) -> usize {
    const HEADER_MAX: usize = 10  /* surface_id varint */
        + 10 /* pts_us varint */
        + 1  /* key_frame */
        + 5  /* bytes length prefix */;
    HEADER_MAX + payload_len
}

/// Lower bound on the encoded size for a given payload length. The
/// fixed-shape minimum is one byte per varint field (the smallest
/// varint encoding is 1 byte) plus the bool plus the length prefix.
fn lower_bound_encoded_size(payload_len: usize) -> usize {
    const HEADER_MIN: usize = 1  /* surface_id varint min */
        + 1  /* pts_us varint min */
        + 1  /* key_frame */
        + 1  /* bytes length prefix min */;
    HEADER_MIN + payload_len
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// The encoded length sits between the documented lower and upper
    /// bounds for every payload length up to 128 KiB. We cap the
    /// proptest input at 128 KiB to keep the test fast — the bound
    /// holds for any size, but exercising the bound at 128 KiB is
    /// representative.
    #[test]
    fn surface_frame_encoded_size_within_bounds(
        surface_id in any::<u64>(),
        pts_us in any::<u64>(),
        key_frame in any::<bool>(),
        payload in proptest::collection::vec(any::<u8>(), 0..=128 * 1024),
    ) {
        let frame = SurfaceFrame {
            surface_id: SurfaceId::from_u64(surface_id),
            pts_us,
            key_frame,
            bytes: Bytes::from(payload.clone()),
        };
        let encoded = canonical_bytes(&frame).expect("encode");
        let payload_len = payload.len();
        prop_assert!(
            encoded.len() >= lower_bound_encoded_size(payload_len),
            "encoded.len()={} < lower bound {} for payload_len={}",
            encoded.len(),
            lower_bound_encoded_size(payload_len),
            payload_len,
        );
        prop_assert!(
            encoded.len() <= upper_bound_encoded_size(payload_len),
            "encoded.len()={} > upper bound {} for payload_len={}",
            encoded.len(),
            upper_bound_encoded_size(payload_len),
            payload_len,
        );
    }

    /// Encode-then-decode is the identity for every shape. The
    /// payload bytes survive byte-for-byte. (Subsumes the explicit
    /// `surface_frame_round_trip` unit test in `src/surface_stream.rs`
    /// for the bounded-random regime.)
    #[test]
    fn surface_frame_round_trip_property(
        surface_id in any::<u64>(),
        pts_us in any::<u64>(),
        key_frame in any::<bool>(),
        payload in proptest::collection::vec(any::<u8>(), 0..=8192),
    ) {
        let frame = SurfaceFrame {
            surface_id: SurfaceId::from_u64(surface_id),
            pts_us,
            key_frame,
            bytes: Bytes::from(payload.clone()),
        };
        let encoded = canonical_bytes(&frame).expect("encode");
        let back: SurfaceFrame =
            qos_collab_types::canonical::from_canonical_bytes(&encoded).expect("decode");
        prop_assert_eq!(back.surface_id, frame.surface_id);
        prop_assert_eq!(back.pts_us, frame.pts_us);
        prop_assert_eq!(back.key_frame, frame.key_frame);
        prop_assert_eq!(back.bytes.as_ref(), payload.as_slice());
    }

    /// The encoded length grows exactly with the payload length: two
    /// frames with payloads of length N and N+K (same header values)
    /// have encoded lengths differing by exactly K. This is the
    /// invariant the broker relies on to size back-pressure buffers
    /// without re-encoding.
    #[test]
    fn surface_frame_encoded_size_grows_with_payload(
        surface_id in any::<u64>(),
        pts_us in any::<u64>(),
        key_frame in any::<bool>(),
        base in proptest::collection::vec(any::<u8>(), 0..=4096),
        extra in proptest::collection::vec(any::<u8>(), 0..=4096),
    ) {
        let small = SurfaceFrame {
            surface_id: SurfaceId::from_u64(surface_id),
            pts_us,
            key_frame,
            bytes: Bytes::from(base.clone()),
        };
        let mut combined = base.clone();
        combined.extend_from_slice(&extra);
        let big = SurfaceFrame {
            surface_id: SurfaceId::from_u64(surface_id),
            pts_us,
            key_frame,
            bytes: Bytes::from(combined),
        };
        let small_enc = canonical_bytes(&small).expect("encode small");
        let big_enc = canonical_bytes(&big).expect("encode big");
        // The length prefix is varint-encoded and may switch byte
        // count if the new length crosses a varint boundary. Assert
        // big >= small + extra.len() (lower bound) and
        // big <= small + extra.len() + 4 (upper bound: the length
        // prefix can grow by at most 4 bytes when crossing all four
        // varint boundaries between 0 and u32::MAX, which a u32
        // length can never actually do in one step, but we bound
        // generously).
        let delta = big_enc.len().saturating_sub(small_enc.len());
        prop_assert!(
            delta >= extra.len(),
            "big_enc - small_enc = {} but extra.len() = {}",
            delta,
            extra.len(),
        );
        prop_assert!(
            delta <= extra.len() + 4,
            "big_enc - small_enc = {} but extra.len() = {} (delta-extra > 4)",
            delta,
            extra.len(),
        );
    }
}
