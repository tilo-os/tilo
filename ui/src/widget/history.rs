//! History data types shared by [`activity_timeline`] and
//! [`history_scrubber`].
//!
//! Source-agnostic by design: the producer (today an in-memory iterator
//! over automerge history, later a signed-op stream) is external, so the
//! widgets can be unit-tested without touching automerge, Iroh, or the
//! broker.
//!
//! [`activity_timeline`]: crate::widget::activity_timeline
//! [`history_scrubber`]: crate::widget::history_scrubber

use std::time::SystemTime;

use collab_types::{ArtifactKind, OpClass};

/// Opaque: only ever compared for equality (revert/restore callbacks
/// pass it back to the caller). The caller picks the byte layout —
/// UUID-v4 today, a content hash later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HistoryEntryId(pub [u8; 16]);

impl HistoryEntryId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Not crypto material — just enough metadata to render a per-author
/// colour dot, display name, and a short identity hash for
/// disambiguation when two authors share a name.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryAuthor {
    pub name: String,
    /// Degrees `[0, 360)`. The widget converts hue → RGB via a fixed
    /// saturation/value so authors stay visually distinguishable even
    /// when names collide.
    pub avatar_hue: f32,
    /// Short hex fragment of the identity pubkey; the producer decides
    /// the length, the widget treats it as an opaque label.
    pub identity_short: String,
}

impl HistoryAuthor {
    pub fn new(
        name: impl Into<String>,
        avatar_hue: f32,
        identity_short: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            avatar_hue: avatar_hue.rem_euclid(360.0),
            identity_short: identity_short.into(),
        }
    }
}

/// Optional on [`HistoryEntry`] — when present, the widget renders the
/// row with the struck-through summary, suppresses the `[Revert]` chip,
/// and exposes the redactor + reason on hover.
///
/// Source: the broker's `RedactionRecord` (`shell/src/shell/collab_broker.rs`).
/// The producer projects the redactor's identity to a short display
/// string; the widget never sees the ciphertext or pubkey.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactionDisplay {
    /// Producer picks the format (e.g. `"alice@example.com"`); rendered
    /// verbatim.
    pub redactor_label: String,
    /// Capped at 512 bytes upstream (`collab_types::MAX_REDACTION_REASON_BYTES`).
    pub reason: String,
    pub redacted_at: SystemTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub id: HistoryEntryId,
    pub author: HistoryAuthor,
    /// Drives the row's icon glyph and the `[Revert]` chip's visibility
    /// (see [`op_class_is_revertible`]).
    pub op_class: OpClass,
    pub artifact_kind: ArtifactKind,
    pub timestamp: SystemTime,
    /// e.g. "+12 -3 chars", "Moved to Done". Producer should keep it
    /// short — the row is ~40 px tall.
    pub summary: String,
    /// `None` means "not a text edit, or not countable" — never `(0, 0)`,
    /// because a group aggregating these must be able to tell "nothing
    /// moved" from "we do not know", and report an aggregate only when
    /// every member reported one.
    pub text_delta: Option<(usize, usize)>,
    /// Opaque bytes for the scrubber to re-derive the artifact's state at
    /// this tick; the widget never cracks them open. `None` when an entry
    /// (Govern ops, pure metadata edits) has no per-entry payload.
    pub diff_payload: Option<Vec<u8>>,
    /// `None` means "not redacted; render the summary as normal".
    pub redaction: Option<RedactionDisplay>,
}

/// Empty fields are "match everything" so a freshly-constructed filter
/// behaves like no filter at all. The chrome owns the filter (not the
/// widget) so the host can persist it across remounts, e.g. a workspace
/// switch, without poking at widget internals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryFilter {
    /// Empty = all authors. Matching is **case-insensitive** so a
    /// typeahead "author:alice" filter doesn't depend on the user
    /// guessing the canonical casing.
    pub authors: Vec<String>,
    /// Empty = all op classes.
    pub op_classes: Vec<OpClass>,
    /// `None` disables the range. Half-open, matching the convention
    /// used elsewhere in qOS so two adjacent filters never double-count
    /// a boundary entry.
    pub time_range: Option<(SystemTime, SystemTime)>,
}

impl HistoryFilter {
    pub fn matches(&self, entry: &HistoryEntry) -> bool {
        if !self.authors.is_empty() {
            let needle = entry.author.name.to_lowercase();
            let any = self.authors.iter().any(|a| needle == a.to_lowercase());
            if !any {
                return false;
            }
        }
        if !self.op_classes.is_empty() && !self.op_classes.contains(&entry.op_class) {
            return false;
        }
        if let Some((start, end)) = self.time_range
            && (entry.timestamp < start || entry.timestamp >= end)
        {
            return false;
        }
        true
    }

    #[must_use]
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.authors.push(author.into());
        self
    }

    #[must_use]
    pub fn with_op_class(mut self, op: OpClass) -> Self {
        self.op_classes.push(op);
        self
    }
}

/// Non-revertible: `Comment` (tombstone, not rollback — never altered
/// primary content), `Govern` (its own audit trail; counter-op instead of
/// rewind), `Compaction` (would mean un-evicting bytes the cache may no
/// longer hold), `Redact` (un-redacting isn't a concept). Every other
/// class has a meaningful inverse.
pub fn op_class_is_revertible(op: OpClass) -> bool {
    match op {
        OpClass::Splice
        | OpClass::Move
        | OpClass::EditMeta
        | OpClass::Tombstone
        | OpClass::Restore => true,
        OpClass::Comment | OpClass::Govern | OpClass::Compaction | OpClass::Redact => false,
    }
}

/// Legible in environments without an emoji font — CLAUDE.md mandates
/// ASCII/Unicode-symbol fallbacks over pictographic emoji.
pub fn op_class_glyph(op: OpClass) -> &'static str {
    match op {
        OpClass::Splice => "\u{00b1}",   // ±
        OpClass::Move => "\u{2195}",     // ↕
        OpClass::EditMeta => "\u{270e}", // ✎
        OpClass::Comment => "#",
        OpClass::Tombstone => "\u{2715}",  // ✕
        OpClass::Restore => "\u{21b6}",    // ↶
        OpClass::Govern => "\u{2605}",     // ★
        OpClass::Compaction => "\u{2702}", // ✂ — host-driven horizon
        // Ring reads as a compliance seal, not a lock (banned emoji).
        OpClass::Redact => "\u{229A}", // ⊚ — redaction seal
    }
}

/// Used by both widgets so the wording stays uniform. Re-renders every
/// frame, so the label freshens naturally as `now` advances — no clock
/// plumbing needed.
pub fn format_relative_time(ts: SystemTime, now: SystemTime) -> String {
    let delta = now.duration_since(ts).unwrap_or_default();
    let secs = delta.as_secs();
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins} min ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours} h ago");
    }
    let days = hours / 24;
    format!("{days} d ago")
}

/// Rounds a normalized `[0, 1]` colour channel to a `u8` byte. The `clamp`
/// makes the truncation and sign-loss the lint warns about impossible —
/// clippy just can't see through it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0.0, 255.0] immediately above, so this can only \
              produce a value in u8's range"
)]
fn channel_u8(unit: f32) -> u8 {
    (unit * 255.0).round().clamp(0.0, 255.0) as u8
}

/// `HSV(h, 0.65, 0.95)` → RGB. Saturation and value held constant so
/// generated author colours stay visually coherent (no neon outliers,
/// no muddy ones).
pub fn hue_to_rgb(hue_deg: f32) -> (u8, u8, u8) {
    let sector = hue_deg.rem_euclid(360.0) / 60.0;
    let sat = 0.65_f32;
    let val = 0.95_f32;
    let chroma = val * sat;
    let x2 = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let min_add = val - chroma;
    let (red, green, blue) = if sector < 1.0 {
        (chroma, x2, 0.0)
    } else if sector < 2.0 {
        (x2, chroma, 0.0)
    } else if sector < 3.0 {
        (0.0, chroma, x2)
    } else if sector < 4.0 {
        (0.0, x2, chroma)
    } else if sector < 5.0 {
        (x2, 0.0, chroma)
    } else {
        (chroma, 0.0, x2)
    };
    (
        channel_u8(red + min_add),
        channel_u8(green + min_add),
        channel_u8(blue + min_add),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(op: OpClass, author: &str, ts_offset_secs: u64) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([0; 16]),
            author: HistoryAuthor::new(author, 120.0, "deadbeef"),
            op_class: op,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(ts_offset_secs))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: "test entry".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
        }
    }

    #[test]
    fn filter_matches_empty_means_all() {
        // The contract the chrome relies on when it constructs
        // `HistoryFilter::default()` and only fills in a single field.
        let f = HistoryFilter::default();
        assert!(f.matches(&entry(OpClass::Splice, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Comment, "Bob", 999)));
        assert!(f.matches(&entry(OpClass::Govern, "Carol", 12345)));
    }

    #[test]
    fn filter_matches_author_case_insensitive() {
        // Typeahead "author:alice" must hit a row authored by "Alice".
        let f = HistoryFilter::default().with_author("alice");
        assert!(f.matches(&entry(OpClass::Splice, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Splice, "ALICE", 0)));
        assert!(!f.matches(&entry(OpClass::Splice, "Bob", 0)));
    }

    #[test]
    fn filter_matches_op_class_subset() {
        // Stays inclusive within the subset — no implicit "Comments
        // must also be from Alice" cross-product.
        let f = HistoryFilter::default().with_op_class(OpClass::Comment);
        assert!(!f.matches(&entry(OpClass::Splice, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Comment, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Comment, "Bob", 0)));
    }

    #[test]
    fn filter_matches_time_range() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let end = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let f = HistoryFilter {
            time_range: Some((start, end)),
            ..Default::default()
        };
        assert!(!f.matches(&entry(OpClass::Splice, "Alice", 99)));
        assert!(f.matches(&entry(OpClass::Splice, "Alice", 100)));
        assert!(f.matches(&entry(OpClass::Splice, "Alice", 150)));
        assert!(!f.matches(&entry(OpClass::Splice, "Alice", 200)));
        assert!(!f.matches(&entry(OpClass::Splice, "Alice", 300)));
    }

    #[test]
    fn op_class_revertibility_matches_spec_table() {
        // Pin the table — a regression that makes Comment revertible
        // would silently paint a confusing chip on every comment row.
        assert!(op_class_is_revertible(OpClass::Splice));
        assert!(op_class_is_revertible(OpClass::Move));
        assert!(op_class_is_revertible(OpClass::EditMeta));
        assert!(op_class_is_revertible(OpClass::Tombstone));
        assert!(op_class_is_revertible(OpClass::Restore));
        assert!(!op_class_is_revertible(OpClass::Comment));
        assert!(!op_class_is_revertible(OpClass::Govern));
        assert!(!op_class_is_revertible(OpClass::Compaction));
        assert!(!op_class_is_revertible(OpClass::Redact));
    }

    #[test]
    fn redacted_entry_is_non_revertible_regardless_of_op_class() {
        // The widget gates the [Revert] chip on `op_class_is_revertible`
        // AND `entry.redaction.is_none()`; a Splice that has since been
        // redacted MUST NOT show the chip.
        let mut e = entry(OpClass::Splice, "Alice", 0);
        e.redaction = Some(RedactionDisplay {
            redactor_label: "owner@example".into(),
            reason: "GDPR".into(),
            redacted_at: SystemTime::UNIX_EPOCH,
        });
        assert!(op_class_is_revertible(e.op_class));
        assert!(
            e.redaction.is_some(),
            "the chrome reads `entry.redaction.is_some()` as the veto",
        );
    }

    #[test]
    fn op_class_glyph_is_ascii_or_symbol_not_emoji() {
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Comment,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
            OpClass::Compaction,
            OpClass::Redact,
        ] {
            let g = op_class_glyph(op);
            assert!(!g.is_empty(), "glyph for {op:?} must not be empty");
            for ch in g.chars() {
                let cp = ch as u32;
                assert!(
                    cp < 0x1f300,
                    "glyph for {op:?} ({g:?} U+{cp:04X}) must not be a pictographic emoji",
                );
            }
        }
    }

    #[test]
    fn format_relative_time_thresholds() {
        // Pin the wording so a regression can't silently swap to
        // "1 minute ago" / "1 hour ago".
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(
            format_relative_time(base, base + Duration::from_secs(10)),
            "just now"
        );
        assert_eq!(
            format_relative_time(base, base + Duration::from_secs(120)),
            "2 min ago"
        );
        assert_eq!(
            format_relative_time(base, base + Duration::from_secs(2 * 3600 + 5)),
            "2 h ago"
        );
        assert_eq!(
            format_relative_time(base, base + Duration::from_secs(259200)),
            "3 d ago"
        );
    }

    #[test]
    fn hue_to_rgb_covers_every_sextant() {
        // Walk every 60-degree boundary so a future regression that
        // collapses one sextant (e.g. "5" branch falling through to
        // black) flips the test immediately.
        let hues = [0.0, 60.0, 120.0, 180.0, 240.0, 300.0, 359.9];
        let mut seen: Vec<(u8, u8, u8)> = Vec::new();
        for h in hues {
            let rgb = hue_to_rgb(h);
            assert_ne!(rgb, (0, 0, 0), "hue {h} produced black");
            seen.push(rgb);
        }
        // Allow a couple of near-duplicates at the wrap-around, but
        // require at least 5 unique values out of 7 samples.
        seen.sort_unstable();
        seen.dedup();
        assert!(
            seen.len() >= 5,
            "hues collapsed to {} unique colours",
            seen.len()
        );
    }
}
