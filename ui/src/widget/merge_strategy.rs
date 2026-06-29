//! Merge-strategy declaration types for the Phase 4 conflict toolkit.
//!
//! A CRDT merges concurrent edits without dropping data, but the
//! result can be semantically wrong: two writers may pick different
//! kanban columns for the same card, two renames may collide on the
//! same file, two scalar writes may produce a tiebreak the user
//! would have wanted to review. The toolkit cannot auto-solve those
//! for every domain. What it *can* do is:
//!
//! 1. Give every CRDT-bound widget a way to **declare** its merge
//!    semantics through [`MergeStrategy`]. The chrome inspects the
//!    declaration to know whether to surface a banner.
//! 2. Provide a [`ConflictResolver`] trait for domain-specific
//!    resolvers (kanban-move, file-rename, …) used by the `Custom`
//!    strategy.
//! 3. Expose two ready-made widgets: a banner that announces "N
//!    unresolved conflicts" ([`crate::widget::conflict_banner`]) and
//!    a side-by-side picker that lets the user choose
//!    ([`crate::widget::conflict_picker`]).
//!
//! Convention: every widget that mounts a CRDT field declares its
//! strategy at construction. A future phase adds a compile-time lint
//! (proc-macro) that warns when a widget binds to a CRDT field
//! without declaring one; for now, the convention is documented here
//! and reinforced by code review.
//!
//! Spec: `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
//! Phase 4 ("Conflict toolkit + reconciliation UI").

use std::sync::Arc;
use std::time::SystemTime;

/// How a CRDT-backed widget wants its concurrent edits resolved when
/// the data layer can converge but the *semantic* outcome may need
/// human judgement.
///
/// The chrome inspects the strategy to decide whether to mount a
/// [`crate::widget::conflict_banner::ConflictBanner`] over the
/// widget. Resolvers used by [`MergeStrategy::Custom`] also drive the
/// picker UI when the user clicks `[Review]`.
#[derive(Clone)]
pub enum MergeStrategy {
    /// Automerge's text RGA. Concurrent edits interleave; no widget
    /// intervention. Right answer for prose, comments, free-form
    /// markdown — the merge produces something readable even when it
    /// isn't what either author wrote verbatim.
    Text,
    /// Set-like collections. Concurrent inserts both land; deletes
    /// shadow but don't conflict. Right for tag lists, attendee
    /// rosters, multi-select choices where the union is what the
    /// users would have agreed on anyway.
    Set,
    /// Scalar fields where concurrent writes produce a tiebreak.
    /// `surface_on_conflict = true` raises a `ConflictBanner` on the
    /// host so the user can review the loser; `false` keeps it
    /// silent (the right setting for ambient counters like
    /// view-counts where a tiebreak is fine).
    LastWriterWins { surface_on_conflict: bool },
    /// Domain-specific resolution. The widget provides a
    /// [`ConflictResolver`]; the toolkit calls into it to classify
    /// concurrent ops and render the picker prompt. Use for kanban
    /// moves, file-renames, schema migrations, anything where the
    /// natural answer is "ask the user."
    Custom(Arc<dyn ConflictResolver>),
}

impl std::fmt::Debug for MergeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeStrategy::Text => write!(f, "MergeStrategy::Text"),
            MergeStrategy::Set => write!(f, "MergeStrategy::Set"),
            MergeStrategy::LastWriterWins { surface_on_conflict } => f
                .debug_struct("MergeStrategy::LastWriterWins")
                .field("surface_on_conflict", surface_on_conflict)
                .finish(),
            MergeStrategy::Custom(resolver) => f
                .debug_struct("MergeStrategy::Custom")
                .field("family", &resolver.family_name())
                .finish(),
        }
    }
}

/// Conflict resolution hook for [`MergeStrategy::Custom`]. The trait
/// classifies a set of concurrent operations into a
/// [`ConflictKind`] and labels the conflict family. The toolkit
/// renders the picker UI using the classification result; the
/// trait itself does *not* paint pixels.
///
/// Implementors must be `Send + Sync` so the resolver can outlive the
/// widget that registered it and be shared across threads (e.g. a
/// per-channel resolver instance held in an `Arc`).
pub trait ConflictResolver: Send + Sync {
    /// Group concurrent operations into a conflict shape the widget
    /// understands. The default classification [`ConflictKind::Auto`]
    /// means "we can resolve without UI"; everything else surfaces a
    /// prompt.
    fn classify(&self, concurrent_ops: &[ConcurrentOp]) -> ConflictKind;

    /// Human-readable name of the conflict family, used as the
    /// banner header (e.g. `"File rename collision"`,
    /// `"Kanban move"`). Kept short — banners only have ~140 px of
    /// horizontal room before the chip cluster.
    fn family_name(&self) -> &str;
}

/// One concurrent op as the resolver sees it. The payload is opaque
/// at the toolkit level; the resolver re-interprets it (e.g. decodes
/// a kanban `MoveCard` envelope to extract `from_column` /
/// `to_column`).
#[derive(Clone, Debug)]
pub struct ConcurrentOp {
    /// Display name of the author of this op.
    pub author: String,
    /// When the op was authored, in the author's wall-clock time.
    pub timestamp: SystemTime,
    /// Human summary the toolkit can render directly in the picker
    /// (`"moved card to Done"`, `"renamed page to Architecture"`).
    pub summary: String,
    /// Opaque envelope re-interpreted by the resolver. The toolkit
    /// passes this through verbatim when an [`Alternative`] is
    /// chosen.
    pub payload: Arc<[u8]>,
}

/// Output of [`ConflictResolver::classify`]. Drives the picker
/// layout.
#[derive(Clone, Debug)]
pub enum ConflictKind {
    /// No human input needed — toolkit suppresses the banner and
    /// proceeds with the resolver's automatic choice. The resolver
    /// is expected to have already applied / queued the op it
    /// settled on.
    Auto,
    /// "These two (or more) writers want different outcomes; pick
    /// one." Used for moves, status changes, anything categorical.
    PickOne { alternatives: Vec<Alternative> },
    /// "These writers wrote different content into the same scalar
    /// field. Merge or pick." The optional `merge_preview` lets the
    /// resolver suggest a stitched result (e.g. concatenated lines)
    /// that the user can accept as a third option.
    MergeOrPick {
        alternatives: Vec<Alternative>,
        merge_preview: Option<String>,
    },
}

/// One choice the user can pick in the conflict picker.
#[derive(Clone, Debug)]
pub struct Alternative {
    /// Short label for the pick button (`"Pick Alice's"`,
    /// `"Move to Done"`). Used verbatim as the button text.
    pub label: String,
    /// Longer description rendered inside the alternative card.
    pub summary: String,
    /// Author whose op this alternative represents. Drives the
    /// per-card byline.
    pub author: String,
    /// When the op was authored (for the per-card timestamp).
    pub timestamp: SystemTime,
    /// Opaque payload re-applied if the user picks this
    /// alternative. Forwarded verbatim through the picker's
    /// `on_pick` callback.
    pub payload: Arc<[u8]>,
}

/// Whether the chrome should mount a `ConflictBanner` over a widget
/// declaring `strategy` whose pending conflict has shape `kind`.
///
/// Rules (mirroring the spec):
/// - `Text` / `Set` never surface — the CRDT merge is the right
///   answer.
/// - `LastWriterWins { surface_on_conflict: true }` surfaces unless
///   the resolver said `Auto`.
/// - `LastWriterWins { surface_on_conflict: false }` stays quiet.
/// - `Custom` surfaces for any non-`Auto` kind — the resolver opted
///   into UI by returning `PickOne` / `MergeOrPick`.
pub fn surface_banner(strategy: &MergeStrategy, kind: &ConflictKind) -> bool {
    match (strategy, kind) {
        (MergeStrategy::Text, _) => false,
        (MergeStrategy::Set, _) => false,
        (MergeStrategy::LastWriterWins { surface_on_conflict }, kind) => {
            *surface_on_conflict && !matches!(kind, ConflictKind::Auto)
        }
        (MergeStrategy::Custom(_), kind) => !matches!(kind, ConflictKind::Auto),
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn op(author: &str, summary: &str) -> ConcurrentOp {
        ConcurrentOp {
            author: author.into(),
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(0),
            summary: summary.into(),
            payload: Arc::from(Vec::<u8>::new()),
        }
    }

    fn alt(label: &str, author: &str) -> Alternative {
        Alternative {
            label: label.into(),
            summary: format!("{} chose {}", author, label),
            author: author.into(),
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(0),
            payload: Arc::from(Vec::<u8>::new()),
        }
    }

    /// Stub resolver used to exercise `MergeStrategy::Custom` paths
    /// without dragging in real-domain code. The `kind` field
    /// determines what `classify` returns; the family name is
    /// fixed.
    struct StubResolver {
        kind: ConflictKind,
    }

    impl ConflictResolver for StubResolver {
        fn classify(&self, _ops: &[ConcurrentOp]) -> ConflictKind {
            self.kind.clone()
        }
        fn family_name(&self) -> &str {
            "Stub"
        }
    }

    #[test]
    fn surface_banner_text_strategy_never_surfaces() {
        // Text strategy is "RGA interleave is fine" — the toolkit
        // never raises a banner regardless of what shape `kind`
        // takes. Pins the rule that prose widgets stay quiet.
        let s = MergeStrategy::Text;
        assert!(!surface_banner(&s, &ConflictKind::Auto));
        assert!(!surface_banner(
            &s,
            &ConflictKind::PickOne {
                alternatives: vec![alt("A", "Alice"), alt("B", "Bob")]
            }
        ));
        assert!(!surface_banner(
            &s,
            &ConflictKind::MergeOrPick {
                alternatives: vec![alt("A", "Alice")],
                merge_preview: Some("merged".into())
            }
        ));
    }

    #[test]
    fn surface_banner_set_strategy_never_surfaces() {
        // Set strategy is "union-on-merge" — both inserts land, no
        // conflict to review. Banner stays off.
        let s = MergeStrategy::Set;
        assert!(!surface_banner(&s, &ConflictKind::Auto));
        assert!(!surface_banner(
            &s,
            &ConflictKind::PickOne {
                alternatives: vec![alt("A", "Alice")]
            }
        ));
    }

    #[test]
    fn surface_banner_lww_surfaces_when_flag_true_and_kind_is_not_auto() {
        // LWW with `surface_on_conflict: true` is the explicit
        // "raise me a banner" toggle. Asserts it triggers for
        // PickOne + MergeOrPick but not for Auto (Auto means "I
        // already settled this, no need to bother the user").
        let s = MergeStrategy::LastWriterWins {
            surface_on_conflict: true,
        };
        assert!(!surface_banner(&s, &ConflictKind::Auto));
        assert!(surface_banner(
            &s,
            &ConflictKind::PickOne {
                alternatives: vec![alt("A", "Alice"), alt("B", "Bob")]
            }
        ));
        assert!(surface_banner(
            &s,
            &ConflictKind::MergeOrPick {
                alternatives: vec![alt("A", "Alice")],
                merge_preview: None
            }
        ));
    }

    #[test]
    fn surface_banner_lww_quiet_when_flag_false() {
        // LWW with the flag off is the "ambient counter" mode —
        // tiebreak is fine, never bother the user. Banner stays
        // off regardless of conflict shape.
        let s = MergeStrategy::LastWriterWins {
            surface_on_conflict: false,
        };
        assert!(!surface_banner(&s, &ConflictKind::Auto));
        assert!(!surface_banner(
            &s,
            &ConflictKind::PickOne {
                alternatives: vec![alt("A", "Alice")]
            }
        ));
        assert!(!surface_banner(
            &s,
            &ConflictKind::MergeOrPick {
                alternatives: vec![alt("A", "Alice")],
                merge_preview: Some("merged".into())
            }
        ));
    }

    #[test]
    fn surface_banner_custom_surfaces_for_pick_one() {
        // A Custom strategy that classified as PickOne opted into
        // UI. Banner must surface so the user can review.
        let resolver = Arc::new(StubResolver {
            kind: ConflictKind::PickOne {
                alternatives: vec![alt("Move", "Alice"), alt("Move", "Bob")],
            },
        });
        let s = MergeStrategy::Custom(resolver.clone());
        let kind = resolver.classify(&[op("Alice", "moved card"), op("Bob", "moved card")]);
        assert!(surface_banner(&s, &kind));
        // Also pin the "Custom + Auto = quiet" path: the resolver
        // can declare it handled the conflict itself.
        let auto = Arc::new(StubResolver {
            kind: ConflictKind::Auto,
        });
        assert!(!surface_banner(
            &MergeStrategy::Custom(auto.clone()),
            &auto.classify(&[])
        ));
    }

    #[test]
    fn resolver_family_name_round_trips_through_debug() {
        // Debug of MergeStrategy::Custom must include the family
        // name so log lines + assertions stay meaningful. Pinned
        // because the debug impl is hand-rolled.
        let resolver = Arc::new(StubResolver {
            kind: ConflictKind::Auto,
        });
        let s = MergeStrategy::Custom(resolver);
        let printed = format!("{:?}", s);
        assert!(printed.contains("Stub"), "got: {}", printed);
        assert!(printed.contains("Custom"), "got: {}", printed);
    }
}
