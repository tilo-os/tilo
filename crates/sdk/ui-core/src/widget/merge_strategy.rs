//! Merge-strategy declaration types for the conflict toolkit.
//!
//! A CRDT merges concurrent edits without dropping data, but the result
//! can be semantically wrong — two writers picking different kanban
//! columns for the same card, two renames colliding on the same file, a
//! scalar tiebreak the user would have wanted to review. The toolkit
//! can't auto-solve those for every domain, so it gives widgets a way to
//! **declare** their merge semantics through [`MergeStrategy`]; the
//! chrome inspects the declaration to decide whether to surface a
//! [`crate::widget::conflict_banner`] or hand off to a domain-specific
//! [`ConflictResolver`] and [`crate::widget::conflict_picker`].
//!
//! Convention: every widget that mounts a CRDT field declares its
//! strategy at construction, reinforced today by code review rather than
//! a lint.

use crate::time::SystemTime;
use std::sync::Arc;

/// How a CRDT-backed widget wants its concurrent edits resolved when the
/// data layer can converge but the *semantic* outcome may need human
/// judgement.
#[derive(Clone)]
pub enum MergeStrategy {
    /// Automerge's text RGA: concurrent edits interleave, no intervention.
    /// For prose, comments, free-form text.
    Text,
    /// Concurrent inserts both land, deletes shadow but don't conflict.
    /// For tag lists, rosters, multi-select where the union is fine.
    Set,
    /// Scalar fields with a tiebreak. `surface_on_conflict = true` raises a
    /// `ConflictBanner` so the user can review the loser; `false` stays
    /// silent (ambient counters like view-counts).
    LastWriterWins { surface_on_conflict: bool },
    /// Domain-specific resolution via [`ConflictResolver`] — kanban moves,
    /// file-renames, schema migrations: anything where the answer is "ask
    /// the user."
    Custom(Arc<dyn ConflictResolver>),
}

impl std::fmt::Debug for MergeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeStrategy::Text => write!(f, "MergeStrategy::Text"),
            MergeStrategy::Set => write!(f, "MergeStrategy::Set"),
            MergeStrategy::LastWriterWins {
                surface_on_conflict,
            } => f
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

/// Classifies concurrent operations into a [`ConflictKind`] and labels the
/// conflict family; the toolkit renders the picker UI from the result, the
/// trait itself does *not* paint pixels.
///
/// Implementors must be `Send + Sync` so the resolver can outlive the
/// widget that registered it (e.g. a per-channel resolver held in an `Arc`).
pub trait ConflictResolver: Send + Sync {
    fn classify(&self, concurrent_ops: &[ConcurrentOp]) -> ConflictKind;

    /// Used as the banner header. Kept short — banners only have ~140 px of
    /// horizontal room before the chip cluster.
    fn family_name(&self) -> &str;
}

/// One concurrent op as the resolver sees it. The payload is opaque at the
/// toolkit level; the resolver re-interprets it (e.g. decodes a kanban
/// `MoveCard` envelope).
#[derive(Clone, Debug)]
pub struct ConcurrentOp {
    pub author: String,
    pub timestamp: SystemTime,
    /// Rendered directly in the picker (`"moved card to Done"`).
    pub summary: String,
    /// Passed through verbatim when an [`Alternative`] is chosen.
    pub payload: Arc<[u8]>,
}

/// Output of [`ConflictResolver::classify`]. Drives the picker layout.
#[derive(Clone, Debug)]
pub enum ConflictKind {
    /// No human input needed; the resolver is expected to have already
    /// applied / queued the op it settled on.
    Auto,
    /// Categorical choice (moves, status changes).
    PickOne { alternatives: Vec<Alternative> },
    /// Different content in the same scalar field. `merge_preview` lets the
    /// resolver offer a stitched result as a third option.
    MergeOrPick {
        alternatives: Vec<Alternative>,
        merge_preview: Option<String>,
    },
}

/// One choice the user can pick in the conflict picker.
#[derive(Clone, Debug)]
pub struct Alternative {
    /// Used verbatim as the pick button's text.
    pub label: String,
    pub summary: String,
    pub author: String,
    pub timestamp: SystemTime,
    /// Forwarded verbatim through the picker's `on_pick` callback.
    pub payload: Arc<[u8]>,
}

pub fn surface_banner(strategy: &MergeStrategy, kind: &ConflictKind) -> bool {
    match (strategy, kind) {
        (MergeStrategy::Text | MergeStrategy::Set, _) => false,
        (
            MergeStrategy::LastWriterWins {
                surface_on_conflict,
            },
            kind,
        ) => *surface_on_conflict && !matches!(kind, ConflictKind::Auto),
        (MergeStrategy::Custom(_), kind) => !matches!(kind, ConflictKind::Auto),
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn op(author: &str, summary: &str) -> ConcurrentOp {
        ConcurrentOp {
            author: author.into(),
            timestamp: SystemTime::UNIX_EPOCH,
            summary: summary.into(),
            payload: Arc::from(Vec::<u8>::new()),
        }
    }

    fn alt(label: &str, author: &str) -> Alternative {
        Alternative {
            label: label.into(),
            summary: format!("{author} chose {label}"),
            author: author.into(),
            timestamp: SystemTime::UNIX_EPOCH,
            payload: Arc::from(Vec::<u8>::new()),
        }
    }

    /// Stub resolver for exercising `MergeStrategy::Custom` without a
    /// real-domain resolver; `kind` drives what `classify` returns.
    struct StubResolver {
        kind: ConflictKind,
    }

    impl ConflictResolver for StubResolver {
        fn classify(&self, _ops: &[ConcurrentOp]) -> ConflictKind {
            self.kind.clone()
        }
        fn family_name(&self) -> &'static str {
            "Stub"
        }
    }

    #[test]
    fn surface_banner_follows_the_resolver_family() {
        fn surface_banner_text_strategy_never_surfaces() {
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

        fn surface_banner_set_strategy_never_surfaces() {
            let s = MergeStrategy::Set;
            assert!(!surface_banner(&s, &ConflictKind::Auto));
            assert!(!surface_banner(
                &s,
                &ConflictKind::PickOne {
                    alternatives: vec![alt("A", "Alice")]
                }
            ));
        }

        fn surface_banner_lww_surfaces_when_flag_true_and_kind_is_not_auto() {
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

        fn surface_banner_lww_quiet_when_flag_false() {
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

        fn surface_banner_custom_surfaces_for_pick_one() {
            let resolver = Arc::new(StubResolver {
                kind: ConflictKind::PickOne {
                    alternatives: vec![alt("Move", "Alice"), alt("Move", "Bob")],
                },
            });
            let s = MergeStrategy::Custom(resolver.clone());
            let kind = resolver.classify(&[op("Alice", "moved card"), op("Bob", "moved card")]);
            assert!(surface_banner(&s, &kind));
            let auto = Arc::new(StubResolver {
                kind: ConflictKind::Auto,
            });
            assert!(!surface_banner(
                &MergeStrategy::Custom(auto.clone()),
                &auto.classify(&[])
            ));
        }

        fn resolver_family_name_round_trips_through_debug() {
            // Debug must include the resolver family name.
            let resolver = Arc::new(StubResolver {
                kind: ConflictKind::Auto,
            });
            let s = MergeStrategy::Custom(resolver);
            let printed = format!("{s:?}");
            assert!(printed.contains("Stub"), "got: {printed}");
            assert!(printed.contains("Custom"), "got: {printed}");
        }

        surface_banner_text_strategy_never_surfaces();
        surface_banner_set_strategy_never_surfaces();
        surface_banner_lww_surfaces_when_flag_true_and_kind_is_not_auto();
        surface_banner_lww_quiet_when_flag_false();
        surface_banner_custom_surfaces_for_pick_one();
        resolver_family_name_round_trips_through_debug();
    }
}
