//! Small pure helpers: human-readable number and duration formatting and a
//! stable string hash.
//!
//! None of this touches the CRDT or the toolkit — it's leaf logic, so it
//! lives on its own and is unit-tested in isolation.

/// FNV-1a 32-bit hash of a string. Stable across runs (no `RandomState`),
/// so the same text always hashes the same.
pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Format a view/subscriber count the way every video site does:
/// `1.2M`, `874K`, `12K`, `342`.
pub fn human_count(n: u64) -> String {
    ui::format::compact_count(n)
}

/// `views` with the trailing noun, e.g. `1.2M views`.
pub fn human_views(n: u64) -> String {
    crate::i18n::t_args(
        "tube-views",
        &[
            crate::i18n::arg("count", n),
            crate::i18n::arg("shown", human_count(n)),
        ],
    )
}

/// Seconds → `h:mm:ss` or `m:ss` (no leading hour when under an hour).
pub fn duration_hms(secs: u32) -> String {
    ui::format::timecode(u64::from(secs))
}

/// Seconds → `mm:ss` always two-field, used by the scrubber time labels.
pub fn clock(secs: f64) -> String {
    // Clamped non-negative immediately above; truncating the fractional
    // second is intended for a `mm:ss` display label.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped non-negative first; dropping the fractional second is intended"
    )]
    let secs = secs.max(0.0) as u64;
    ui::format::timecode(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_count_thresholds() {
        assert_eq!(human_count(342), "342");
        assert_eq!(human_count(1_500), "1.5K");
        assert_eq!(human_count(12_000), "12K");
        assert_eq!(human_count(999_999), "1M");
        assert_eq!(human_count(1_200_000), "1.2M");
        assert_eq!(human_count(2_400_000_000), "2.4B");
    }

    #[test]
    fn duration_formats() {
        assert_eq!(duration_hms(75), "1:15");
        assert_eq!(duration_hms(3_661), "1:01:01");
        assert_eq!(duration_hms(9), "0:09");
        assert_eq!(clock(3_661.9), "1:01:01");
    }

    #[test]
    fn fnv_matches_the_published_vector_and_distinguishes_inputs() {
        assert_eq!(fnv1a("abc"), 0x1a47_e90b);
        assert_ne!(fnv1a("abc"), fnv1a("abd"));
    }
}
