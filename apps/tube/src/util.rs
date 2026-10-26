//! Small pure helpers: human-readable number and duration formatting.
//!
//! None of this touches the CRDT or the toolkit — it's leaf logic, so it
//! lives on its own and is unit-tested in isolation.

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

/// The height a quality choice allows (`"480p"` is `Some(480)`), or `None`
/// for `"auto"` and anything that is not a height: then the best available
/// stream is the answer.
pub fn quality_height(quality: &str) -> Option<u32> {
    quality
        .trim()
        .strip_suffix('p')
        .and_then(|height| height.parse().ok())
        .filter(|height: &u32| *height > 0)
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
    fn a_quality_label_is_the_height_a_source_must_not_exceed() {
        assert_eq!(quality_height("720p"), Some(720));
        assert_eq!(quality_height("1080p"), Some(1080));
        assert_eq!(quality_height("480p"), Some(480));
        // "auto" asks for the best the source has, not for a height.
        assert_eq!(quality_height("auto"), None);
        assert_eq!(quality_height(""), None);
    }

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
        assert_eq!(clock(3_661.9), "1:01:01");
    }
}
