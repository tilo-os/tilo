//! History data types shared by the [`activity_timeline`] and
//! [`history_scrubber`] widgets.
//!
//! Phase 1 of the
//! `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
//! spec ("Audit timeline + soft-delete + restore-from-history") needs a
//! single source-agnostic representation of an entry in an artifact's
//! history. The widgets in this module consume `Vec<HistoryEntry>`; the
//! producer is intentionally external (today: an in-memory iterator over
//! automerge history; later: a signed-op stream from Phase 3). Keeping
//! the type decoupled from any specific transport means the widget can
//! be unit-tested without touching automerge, libp2p, or the broker.
//!
//! Re-exported through `ui::widget::history::*` and (for convenience)
//! `ui::HistoryEntry`, `ui::HistoryFilter`, etc. via [`crate::lib`].
//!
//! [`activity_timeline`]: crate::widget::activity_timeline
//! [`history_scrubber`]: crate::widget::history_scrubber

use std::time::SystemTime;

use qos_collab_types::{ArtifactKind, OpClass};

/// Stable opaque identifier for one entry in an artifact's history.
///
/// The widgets never interpret the bytes — they only compare for
/// equality (revert/restore callbacks pass the id back to the caller).
/// The caller picks the layout: UUID-v4 bytes today, a content hash
/// later. Sixteen bytes covers both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HistoryEntryId(pub [u8; 16]);

impl HistoryEntryId {
    /// Construct from an explicit byte array. Convenience for tests
    /// and the in-memory producer.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 16-byte payload — for hashing into a UI cache
    /// key or rendering a short hex preview alongside the row.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Display-side information for the author of a history entry.
///
/// This is **not** crypto material — Phase 1 doesn't have a signed-op
/// stream yet. The chrome only needs enough metadata to render a
/// per-author colour dot, the author's display name, and a short
/// identity hash for disambiguation when two authors share a name.
///
/// In Phase 3 the producer will derive these fields from the signed-op
/// envelope's `author` pubkey + the channel's member directory; until
/// then the host fills them in by hand.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryAuthor {
    /// Human-readable display name. Rendered next to the avatar dot.
    pub name: String,
    /// Hue in degrees `[0, 360)` used as the dominant colour of the
    /// avatar dot. The widget converts hue → RGB via a fixed
    /// saturation/value so different authors are visually
    /// distinguishable even when names collide.
    pub avatar_hue: f32,
    /// First few hex characters of the author's identity pubkey,
    /// rendered in a dim monospace font next to the name. Eight
    /// characters is the default convention because that's the
    /// minimum length where collisions are visually rare across a
    /// realistic team roster (~50 members). The producer is free to
    /// pad/truncate as it sees fit; the widget treats this as an
    /// opaque label.
    pub identity_short: String,
}

impl HistoryAuthor {
    /// Convenience builder for tests + simple in-process producers.
    pub fn new(name: impl Into<String>, avatar_hue: f32, identity_short: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            avatar_hue: avatar_hue.rem_euclid(360.0),
            identity_short: identity_short.into(),
        }
    }
}

/// One entry in an artifact's audit timeline.
///
/// Source-agnostic by design: the widget never asks where this came
/// from. The producer decides whether an entry corresponds to a
/// single CRDT op, a coalesced run of small splices (a "keystroke
/// burst"), or a checkpoint snapshot. The summary is the human-
/// readable label the user reads in the activity pane.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// Stable id used by the revert/restore callbacks. Comparison
    /// only — the widget doesn't interpret the bytes.
    pub id: HistoryEntryId,
    /// Who authored the change. Display-only; see [`HistoryAuthor`].
    pub author: HistoryAuthor,
    /// The op class — drives the row's icon glyph and decides
    /// whether the `[Revert]` chip is visible (Comments + Govern
    /// are non-revertible).
    pub op_class: OpClass,
    /// What kind of artifact this entry mutated. Mostly informational
    /// at the widget level; the filter pane never filters on it
    /// (filtering on kind would only make sense in a cross-artifact
    /// activity stream, which is a future shell-level pane).
    pub artifact_kind: ArtifactKind,
    /// When the change happened. The widget renders this as a
    /// relative timestamp ("3 min ago") so a stable clock isn't
    /// required.
    pub timestamp: SystemTime,
    /// Human-readable summary: "+12 -3 chars", "Moved to Done",
    /// "Tombstoned". The producer keeps it short — the row is ~40 px
    /// tall and the summary is the load-bearing label.
    pub summary: String,
    /// Opaque bytes for the scrubber to re-derive the artifact's
    /// state at this tick. The widget doesn't crack them open — the
    /// shell layer takes responsibility for replaying. Optional
    /// because some entries (Govern ops, pure metadata edits) have
    /// no per-entry payload to preview.
    pub diff_payload: Option<Vec<u8>>,
}

/// Predicate the activity timeline applies to each [`HistoryEntry`]
/// before rendering. Empty fields are "match everything" so a
/// freshly-constructed filter behaves like no filter at all.
///
/// The chrome owns the filter; the widget reads it via
/// [`crate::widget::ActivityTimeline::set_filter`]. Splitting filter
/// state out of the widget means the host can persist the filter
/// across remounts (e.g. across a workspace switch) without poking
/// at widget internals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryFilter {
    /// Restrict to entries authored by any of these display names.
    /// Empty = all authors. Matching is **case-insensitive** so a
    /// typeahead "author:alice" filter doesn't depend on the user
    /// guessing the canonical casing.
    pub authors: Vec<String>,
    /// Restrict to entries whose `op_class` is in this set. Empty =
    /// all op classes. Used by the filter chips at the top of the
    /// timeline (Edits / Comments / Governance / Deletes).
    pub op_classes: Vec<OpClass>,
    /// Restrict to entries whose `timestamp` falls inside
    /// `[start, end)`. `None` disables the range. The half-open
    /// interval matches the convention used elsewhere in qOS so two
    /// adjacent filters never double-count a boundary entry.
    pub time_range: Option<(SystemTime, SystemTime)>,
}

impl HistoryFilter {
    /// Returns true iff `entry` passes every active sub-filter.
    /// Empty sub-filters short-circuit to true ("match all").
    pub fn matches(&self, entry: &HistoryEntry) -> bool {
        if !self.authors.is_empty() {
            let needle = entry.author.name.to_lowercase();
            let any = self
                .authors
                .iter()
                .any(|a| needle == a.to_lowercase());
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

    /// Convenience: install an author filter (replacing any prior).
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.authors.push(author.into());
        self
    }

    /// Convenience: install an op-class filter (replacing any prior).
    pub fn with_op_class(mut self, op: OpClass) -> Self {
        self.op_classes.push(op);
        self
    }
}

/// Whether an op-class supports the "revert" affordance.
///
/// The spec's permission table treats `Comment` and `Govern` as
/// inherently non-revertible: a comment can be tombstoned but not
/// "rolled back" (it never altered the artifact's primary content);
/// a governance op (attestation issue/revoke, channel rename) is its
/// own audit trail and cannot be silently rewound — instead, the
/// owner issues a counter-op. Every other class (`Splice`, `Move`,
/// `EditMeta`, `Tombstone`, `Restore`) has a meaningful inverse and
/// gets the chip.
pub fn op_class_is_revertible(op: OpClass) -> bool {
    match op {
        OpClass::Splice
        | OpClass::Move
        | OpClass::EditMeta
        | OpClass::Tombstone
        | OpClass::Restore => true,
        // `Comment` and `Govern` are not revertible (see the doc
        // comment). Phase 6's `Compaction` is also non-revertible by
        // design: it asserts a snapshot horizon for the channel and
        // "reverting" it would mean un-evicting bytes the local cache
        // may no longer hold. Owners issue a new checkpoint rather
        // than reverting an old one.
        OpClass::Comment | OpClass::Govern | OpClass::Compaction => false,
    }
}

/// Text glyph for an op-class in a row label. Picked so the rendered
/// timeline is legible in environments without an emoji font — the
/// project's CLAUDE.md mandates ASCII/Unicode-symbol fallbacks over
/// pictographic emoji.
pub fn op_class_glyph(op: OpClass) -> &'static str {
    match op {
        OpClass::Splice => "\u{00b1}",     // ±
        OpClass::Move => "\u{2195}",       // ↕
        OpClass::EditMeta => "\u{270e}",   // ✎
        OpClass::Comment => "#",
        OpClass::Tombstone => "\u{2715}",  // ✕
        OpClass::Restore => "\u{21b6}",    // ↶
        OpClass::Govern => "\u{2605}",     // ★
        OpClass::Compaction => "\u{2702}", // ✂ — host-driven horizon
    }
}

/// Render a relative timestamp like `"3 min ago"`. Used by both
/// widgets so the wording stays uniform.
///
/// The thresholds match common social-app conventions (Slack,
/// Discord): under a minute → "just now"; under an hour → "N min
/// ago"; under a day → "N h ago"; older → "N d ago". The widget
/// re-renders on every frame, so the label freshens naturally as
/// `now` advances.
pub fn format_relative_time(ts: SystemTime, now: SystemTime) -> String {
    let delta = now.duration_since(ts).unwrap_or_default();
    let secs = delta.as_secs();
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{} min ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{} h ago", hours);
    }
    let days = hours / 24;
    format!("{} d ago", days)
}

/// Convert an `HSV(h, 0.65, 0.95)` colour to RGB bytes. Used for the
/// per-author avatar dot — the widget keeps saturation + value
/// constant so the family of generated colours stays visually
/// coherent (no neon-bright outliers, no muddy ones).
pub fn hue_to_rgb(hue_deg: f32) -> (u8, u8, u8) {
    let h = hue_deg.rem_euclid(360.0) / 60.0;
    let s = 0.65_f32;
    let v = 0.95_f32;
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

// =====================================================================
// Tests
// =====================================================================

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
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(ts_offset_secs),
            summary: "test entry".into(),
            diff_payload: None,
        }
    }

    #[test]
    fn filter_matches_empty_means_all() {
        // An untouched filter has no active sub-clause and must accept
        // every entry — that's the contract the chrome relies on when
        // it constructs `HistoryFilter::default()` and only fills in
        // a single field.
        let f = HistoryFilter::default();
        assert!(f.matches(&entry(OpClass::Splice, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Comment, "Bob", 999)));
        assert!(f.matches(&entry(OpClass::Govern, "Carol", 12345)));
    }

    #[test]
    fn filter_matches_author_case_insensitive() {
        // Typeahead "author:alice" must hit a row authored by "Alice".
        // The filter normalises both sides to lowercase before
        // comparing so the chrome doesn't depend on the user typing
        // the canonical casing.
        let f = HistoryFilter::default().with_author("alice");
        assert!(f.matches(&entry(OpClass::Splice, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Splice, "ALICE", 0)));
        assert!(!f.matches(&entry(OpClass::Splice, "Bob", 0)));
    }

    #[test]
    fn filter_matches_op_class_subset() {
        // The "Comments" chip is an op-class subset of size 1. A
        // splice row must not match while a comment row does. The
        // filter stays inclusive within the subset (no implicit
        // "Comments must also be from Alice" cross-product).
        let f = HistoryFilter::default().with_op_class(OpClass::Comment);
        assert!(!f.matches(&entry(OpClass::Splice, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Comment, "Alice", 0)));
        assert!(f.matches(&entry(OpClass::Comment, "Bob", 0)));
    }

    #[test]
    fn filter_matches_time_range() {
        // Half-open interval: start is inclusive, end is exclusive.
        // The chrome relies on this to chain adjacent filters
        // ("yesterday" + "today") without double-counting a boundary
        // entry.
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
        // Pin the revertibility table — a future regression that
        // accidentally makes Comment revertible would silently paint
        // a confusing chip on every comment row. The table mirrors
        // the spec's permission-table prose: only Comment + Govern
        // are non-revertible.
        assert!(op_class_is_revertible(OpClass::Splice));
        assert!(op_class_is_revertible(OpClass::Move));
        assert!(op_class_is_revertible(OpClass::EditMeta));
        assert!(op_class_is_revertible(OpClass::Tombstone));
        assert!(op_class_is_revertible(OpClass::Restore));
        assert!(!op_class_is_revertible(OpClass::Comment));
        assert!(!op_class_is_revertible(OpClass::Govern));
    }

    #[test]
    fn op_class_glyph_is_ascii_or_symbol_not_emoji() {
        // CLAUDE.md forbids emoji in code; the glyph mapping must
        // stay on the Unicode-symbol path. We assert that every
        // glyph is short and not in the emoji-ish ranges (U+1F300+).
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Comment,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
        ] {
            let g = op_class_glyph(op);
            assert!(!g.is_empty(), "glyph for {:?} must not be empty", op);
            for ch in g.chars() {
                let cp = ch as u32;
                assert!(
                    cp < 0x1f300,
                    "glyph for {:?} ({:?} U+{:04X}) must not be a pictographic emoji",
                    op,
                    g,
                    cp,
                );
            }
        }
    }

    #[test]
    fn format_relative_time_thresholds() {
        // Pin the wording at each threshold so a future regression
        // that silently swaps to "1 minute ago" / "1 hour ago" gets
        // flagged. Slack-style brevity matters at the 40 px row
        // height the activity timeline runs at.
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
            format_relative_time(base, base + Duration::from_secs(3 * 86400)),
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
            // No sextant should yield pure black under saturation
            // 0.65 + value 0.95 — that would mean we hit an unmapped
            // branch.
            assert_ne!(rgb, (0, 0, 0), "hue {} produced black", h);
            seen.push(rgb);
        }
        // Most colours should be distinct; allow a couple of
        // near-duplicates at the wrap-around but require at least
        // 5 unique values out of 7 samples.
        seen.sort();
        seen.dedup();
        assert!(seen.len() >= 5, "hues collapsed to {} unique colours", seen.len());
    }
}
