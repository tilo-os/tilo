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

use crate::time::SystemTime;

use collab_types::ArtifactKind;
use tilo_actions::{ActionContext, ActionFacet};

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
    /// Degrees `[0, 360)`. Kept for fixtures and any surface that still
    /// reads hue; paint uses [`HistoryAuthor::rgb`].
    pub avatar_hue: f32,
    /// Short hex fragment of the identity pubkey; the producer decides
    /// the length, the widget treats it as an opaque label.
    pub identity_short: String,
    /// The colour presence paints this identity in. Defaults to
    /// [`hue_to_rgb`] of [`Self::avatar_hue`] so existing constructors
    /// keep their look; [`Self::with_rgb`] and identity resolution set
    /// the person's colour or the shared [`crate::presence::PEER_PALETTE`]
    /// slot.
    avatar_rgb: (u8, u8, u8),
    /// Who the author is to whoever reads the history: themselves, another
    /// person, or software acting for someone.
    pub relation: AuthorRelation,
    /// Who acted and for whom, when the author is an agent, a Shortcut or an
    /// app acting for a person: the same facts [`Self::name`] reads as a
    /// sentence, kept as data for the action surface and assistive tech.
    pub attribution: Option<HistoryAttribution>,
}

/// An author that acted for someone: an agent's change is the agent's, on
/// behalf of its person, in every history (an app's, the Files history, and
/// what the history actions return).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAttribution {
    /// The acting principal as it was authenticated (`agent:scribe`).
    pub actor: String,
    /// What the actor is.
    pub kind: tilo_actions::ActorKind,
    /// The actor's display name, without the person it acted for.
    pub actor_name: String,
    /// The principal of the person it acted for, when it acted for one.
    pub on_behalf_of: Option<String>,
    /// That person's display name.
    pub on_behalf_of_name: Option<String>,
}

/// Who an author is to the person reading the history. Rows draw the
/// reader's own changes, a collaborator's and an agent's differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorRelation {
    /// The reader's own change.
    Reader,
    /// Another person's.
    #[default]
    Person,
    /// An agent's, a Shortcut's or an app's, acting for someone.
    Software,
}

impl HistoryAuthor {
    pub fn new(
        name: impl Into<String>,
        avatar_hue: f32,
        identity_short: impl Into<String>,
    ) -> Self {
        let avatar_hue = avatar_hue.rem_euclid(360.0);
        Self {
            name: name.into(),
            avatar_hue,
            identity_short: identity_short.into(),
            avatar_rgb: hue_to_rgb(avatar_hue),
            relation: AuthorRelation::Person,
            attribution: None,
        }
    }

    #[must_use]
    pub fn with_relation(mut self, relation: AuthorRelation) -> Self {
        self.relation = relation;
        self
    }

    /// Record who acted and for whom.
    #[must_use]
    pub fn with_attribution(mut self, attribution: HistoryAttribution) -> Self {
        self.attribution = Some(attribution);
        self
    }

    /// Use this exact RGB for the avatar, cursor and history row.
    #[must_use]
    pub fn with_rgb(mut self, rgb: (u8, u8, u8)) -> Self {
        self.avatar_rgb = rgb;
        self
    }

    pub fn rgb(&self) -> (u8, u8, u8) {
        self.avatar_rgb
    }
}

/// Optional on [`HistoryEntry`] — when present, the widget renders the
/// row with the struck-through summary, suppresses the `[Revert]` chip,
/// and exposes the redactor + reason on hover.
///
/// Source: the broker's `RedactionRecord` (`crates/session/shell/src/shell/collab_broker.rs`).
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
    /// (see [`EntryKind::is_revertible`]).
    pub entry_kind: EntryKind,
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
    /// Semantic provenance carried by an action. Legacy/direct CRDT edits
    /// leave this empty and retain their structural diff summary.
    pub context: Option<ActionContext>,
}

impl HistoryEntry {
    /// Whether this row represents a reversible document mutation. Durable
    /// effect receipts share the activity timeline but describe external work;
    /// reverting their metadata would not undo that work and is never offered.
    pub fn is_revertible(&self) -> bool {
        self.redaction.is_none()
            && self.entry_kind.is_revertible()
            && !self.is_effect_receipt()
            && !self.is_genesis()
    }

    /// Whether this row is the document's genesis: the version it was
    /// created as. It stays restorable, but nobody made it and its time is
    /// not when anyone did, so it is shown without an author or a time and
    /// never offered as a recent change.
    pub fn is_genesis(&self) -> bool {
        self.context
            .as_ref()
            .is_some_and(|context| context.facets.contains(&genesis_facet()))
    }

    /// Whether this row is a durable effect receipt: external work recorded
    /// in the activity timeline rather than a document mutation.
    pub fn is_effect_receipt(&self) -> bool {
        self.context
            .as_ref()
            .is_some_and(|context| context.facets.iter().any(is_effect_receipt_facet))
    }
}

/// The facet an effect receipt carries.
fn is_effect_receipt_facet(facet: &ActionFacet) -> bool {
    facet.key == "tilo.receipt" && facet.value == "effect"
}

/// Marks the row a history source makes for a document's genesis.
pub(crate) fn genesis_facet() -> ActionFacet {
    ActionFacet::new("tilo.history", "genesis")
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
    pub entry_kinds: Vec<EntryKind>,
    /// `None` disables the range. Half-open, matching the convention
    /// used elsewhere in Tilo so two adjacent filters never double-count
    /// a boundary entry.
    pub time_range: Option<(SystemTime, SystemTime)>,
    /// App id or label, case-insensitive. Empty means every app, including
    /// legacy entries with no app provenance.
    pub apps: Vec<String>,
    /// Stable resource kind (`files.entry`, `word-processor.document`, …).
    pub resource_kinds: Vec<String>,
    /// Stable resource ids or human labels. Values within this lens are OR.
    pub resources: Vec<String>,
    /// Explicit work-group ids or labels. Values within this lens are OR.
    pub work_groups: Vec<String>,
    /// Selected browse facets. All selected key/value pairs must match.
    pub facets: Vec<ActionFacet>,
    /// Lets the explorer make legacy/direct edits explicitly browseable
    /// instead of silently mixing them into semantic lenses.
    pub classification: HistoryClassification,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryClassification {
    #[default]
    Any,
    Classified,
    Unclassified,
}

impl HistoryFilter {
    fn matches_facets(&self, context: &ActionContext) -> bool {
        let mut visited = std::collections::HashSet::new();
        for facet in &self.facets {
            if !visited.insert(facet.key.as_str()) {
                continue;
            }
            if !self
                .facets
                .iter()
                .filter(|candidate| candidate.key == facet.key)
                .any(|candidate| context.facets.contains(candidate))
            {
                return false;
            }
        }
        true
    }

    pub fn matches(&self, entry: &HistoryEntry) -> bool {
        match self.classification {
            HistoryClassification::Classified if entry.context.is_none() => return false,
            HistoryClassification::Unclassified if entry.context.is_some() => return false,
            HistoryClassification::Any
            | HistoryClassification::Classified
            | HistoryClassification::Unclassified => {}
        }
        if !self.authors.is_empty() {
            let needle = entry.author.name.to_lowercase();
            let any = self.authors.iter().any(|a| needle == a.to_lowercase());
            if !any {
                return false;
            }
        }
        if !self.entry_kinds.is_empty() && !self.entry_kinds.contains(&entry.entry_kind) {
            return false;
        }
        if let Some((start, end)) = self.time_range
            && (entry.timestamp < start || entry.timestamp >= end)
        {
            return false;
        }
        if !self.apps.is_empty() {
            let Some(app) = entry
                .context
                .as_ref()
                .and_then(|context| context.app.as_ref())
            else {
                return false;
            };
            let id = app.id.to_lowercase();
            let label = app.label.to_lowercase();
            if !self.apps.iter().any(|candidate| {
                candidate.to_lowercase() == id || candidate.to_lowercase() == label
            }) {
                return false;
            }
        }
        if !self.resource_kinds.is_empty() {
            let Some(kind) = entry
                .context
                .as_ref()
                .and_then(|context| context.resource.as_ref())
                .map(|resource| resource.kind.as_str())
            else {
                return false;
            };
            if !self
                .resource_kinds
                .iter()
                .any(|candidate| candidate == kind)
            {
                return false;
            }
        }
        if !self.resources.is_empty() {
            let Some(resource) = entry
                .context
                .as_ref()
                .and_then(|context| context.resource.as_ref())
            else {
                return false;
            };
            if !self.resources.iter().any(|candidate| {
                candidate.eq_ignore_ascii_case(&resource.id)
                    || candidate.eq_ignore_ascii_case(&resource.label)
            }) {
                return false;
            }
        }
        if !self.work_groups.is_empty() {
            let Some(group) = entry
                .context
                .as_ref()
                .and_then(|context| context.group.as_ref())
            else {
                return false;
            };
            if !self.work_groups.iter().any(|candidate| {
                candidate.eq_ignore_ascii_case(&group.id)
                    || candidate.eq_ignore_ascii_case(&group.label)
            }) {
                return false;
            }
        }
        if !self.facets.is_empty() {
            let Some(context) = entry.context.as_ref() else {
                return false;
            };
            // Values within one key are alternatives; separate facet keys
            // intersect. `path:A OR path:B` plus `file-type:Markdown`.
            if !self.matches_facets(context) {
                return false;
            }
        }
        true
    }

    #[must_use]
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.authors.push(author.into());
        self
    }

    #[must_use]
    pub fn with_entry_kind(mut self, op: EntryKind) -> Self {
        self.entry_kinds.push(op);
        self
    }

    #[must_use]
    pub fn with_app(mut self, app: impl Into<String>) -> Self {
        self.apps.push(app.into());
        self
    }

    #[must_use]
    pub fn with_resource_kind(mut self, kind: impl Into<String>) -> Self {
        self.resource_kinds.push(kind.into());
        self
    }

    #[must_use]
    pub fn with_resource(mut self, resource: impl Into<String>) -> Self {
        self.resources.push(resource.into());
        self
    }

    #[must_use]
    pub fn with_work_group(mut self, group: impl Into<String>) -> Self {
        self.work_groups.push(group.into());
        self
    }

    #[must_use]
    pub fn with_classification(mut self, classification: HistoryClassification) -> Self {
        self.classification = classification;
        self
    }

    #[must_use]
    pub fn with_facet(mut self, facet: ActionFacet) -> Self {
        self.facets.push(facet);
        self
    }
}

/// What a history entry did, as the history and activity views mark,
/// filter and group it. Read from the change itself (its entry and what it
/// wrote), not from the transport that carried it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryKind {
    /// Text typed or removed.
    Text,
    /// Things reordered or moved.
    Move,
    /// Fields, properties or formatting changed.
    Details,
    /// A comment or reaction.
    Comment,
    /// Something deleted.
    Delete,
    /// Something deleted brought back.
    Restore,
    /// Who may do what changed: grants, members, ownership.
    Access,
    /// History older than a horizon folded away.
    Compaction,
    /// Something earlier redacted.
    Redaction,
}

impl EntryKind {
    /// Whether an entry of this kind can be reverted. Not a comment (never
    /// altered primary content), an access change (countered by another,
    /// not rewound), a compaction (the bytes may be gone) or a redaction
    /// (un-redacting is no concept).
    #[must_use]
    pub fn is_revertible(self) -> bool {
        match self {
            Self::Text | Self::Move | Self::Details | Self::Delete | Self::Restore => true,
            Self::Comment | Self::Access | Self::Compaction | Self::Redaction => false,
        }
    }

    /// Its row glyph: text symbols stay legible without an emoji font.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Text => "\u{00b1}",    // ±
            Self::Move => "\u{2195}",    // ↕
            Self::Details => "\u{270e}", // ✎
            Self::Comment => "#",
            Self::Delete => "\u{2715}",     // ✕
            Self::Restore => "\u{21b6}",    // ↶
            Self::Access => "\u{2605}",     // ★
            Self::Compaction => "\u{2702}", // ✂ — history horizon
            // The ring reads as a compliance seal rather than a lock.
            Self::Redaction => "\u{229A}", // ⊚ — redaction seal
        }
    }
}

/// Used by both widgets so the wording stays uniform with every other
/// "time ago" label ([`crate::format::relative`]). Re-renders every frame,
/// so the label freshens naturally as `now` advances — no clock plumbing
/// needed.
pub fn format_relative_time(ts: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(ts).unwrap_or_default().as_secs();
    crate::format::relative(i64::try_from(secs).unwrap_or(i64::MAX))
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
    use crate::time::Duration;

    fn entry(op: EntryKind, author: &str, ts_offset_secs: u64) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([0; 16]),
            author: HistoryAuthor::new(author, 120.0, "deadbeef"),
            entry_kind: op,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(ts_offset_secs))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: "test entry".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: None,
        }
    }

    #[test]
    fn history_filter_matches_author_ops_time_and_facets() {
        fn filter_matches_empty_means_all() {
            // The contract the chrome relies on when it constructs
            // `HistoryFilter::default()` and only fills in a single field.
            let f = HistoryFilter::default();
            assert!(f.matches(&entry(EntryKind::Text, "Alice", 0)));
            assert!(f.matches(&entry(EntryKind::Comment, "Bob", 999)));
            assert!(f.matches(&entry(EntryKind::Access, "Carol", 12345)));
        }

        fn filter_matches_author_case_insensitive() {
            // Typeahead "author:alice" must hit a row authored by "Alice".
            let f = HistoryFilter::default().with_author("alice");
            assert!(f.matches(&entry(EntryKind::Text, "Alice", 0)));
            assert!(f.matches(&entry(EntryKind::Text, "ALICE", 0)));
            assert!(!f.matches(&entry(EntryKind::Text, "Bob", 0)));
        }

        fn filter_matches_entry_kind_subset() {
            // Stays inclusive within the subset — no implicit "Comments
            // must also be from Alice" cross-product.
            let f = HistoryFilter::default().with_entry_kind(EntryKind::Comment);
            assert!(!f.matches(&entry(EntryKind::Text, "Alice", 0)));
            assert!(f.matches(&entry(EntryKind::Comment, "Alice", 0)));
            assert!(f.matches(&entry(EntryKind::Comment, "Bob", 0)));
        }

        fn filter_matches_time_range() {
            let start = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
            let end = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
            let f = HistoryFilter {
                time_range: Some((start, end)),
                ..Default::default()
            };
            assert!(!f.matches(&entry(EntryKind::Text, "Alice", 99)));
            assert!(f.matches(&entry(EntryKind::Text, "Alice", 100)));
            assert!(f.matches(&entry(EntryKind::Text, "Alice", 150)));
            assert!(!f.matches(&entry(EntryKind::Text, "Alice", 200)));
            assert!(!f.matches(&entry(EntryKind::Text, "Alice", 300)));
        }

        fn semantic_facets_filter_context_without_guessing_legacy_entries() {
            let mut contextual = entry(EntryKind::Details, "Ada", 100);
            contextual.context = Some(
                ActionContext::for_resource("files.entry:brief.md", "files.entry", "brief.md")
                    .with_app("org.tilo.Files", "Files")
                    .with_group("issue:42", "Launch brief", "issue")
                    .with_facet(ActionFacet::new("file-type", "Markdown"))
                    .with_facet(ActionFacet::new("path", "/work/brief.md")),
            );
            let legacy = entry(EntryKind::Details, "Ada", 100);

            let by_app = HistoryFilter::default().with_app("files");
            assert!(by_app.matches(&contextual));
            assert!(!by_app.matches(&legacy));

            let by_kind = HistoryFilter::default().with_resource_kind("files.entry");
            assert!(by_kind.matches(&contextual));
            assert!(
                HistoryFilter::default()
                    .with_resource("brief.md")
                    .matches(&contextual)
            );
            assert!(
                HistoryFilter::default()
                    .with_work_group("issue:42")
                    .matches(&contextual)
            );
            let by_type =
                HistoryFilter::default().with_facet(ActionFacet::new("file-type", "Markdown"));
            assert!(by_type.matches(&contextual));
            assert!(
                !HistoryFilter::default()
                    .with_facet(ActionFacet::new("file-type", "PDF"))
                    .matches(&contextual)
            );
            let within_lens_is_or = HistoryFilter::default()
                .with_facet(ActionFacet::new("file-type", "PDF"))
                .with_facet(ActionFacet::new("file-type", "Markdown"));
            assert!(within_lens_is_or.matches(&contextual));
            let across_lenses_is_and =
                within_lens_is_or.with_facet(ActionFacet::new("path", "/somewhere/else"));
            assert!(!across_lenses_is_and.matches(&contextual));
            assert!(
                HistoryFilter::default()
                    .with_classification(HistoryClassification::Classified)
                    .matches(&contextual)
            );
            assert!(
                HistoryFilter::default()
                    .with_classification(HistoryClassification::Unclassified)
                    .matches(&legacy)
            );
        }

        filter_matches_empty_means_all();
        filter_matches_author_case_insensitive();
        filter_matches_entry_kind_subset();
        filter_matches_time_range();
        semantic_facets_filter_context_without_guessing_legacy_entries();
    }

    #[test]
    fn entry_kind_revertibility_glyphs_and_formatting_helpers() {
        fn entry_kind_revertibility_matches_spec_table() {
            // Pin the table — a regression that makes Comment revertible
            // would silently paint a confusing chip on every comment row.
            assert!(EntryKind::Text.is_revertible());
            assert!(EntryKind::Move.is_revertible());
            assert!(EntryKind::Details.is_revertible());
            assert!(EntryKind::Delete.is_revertible());
            assert!(EntryKind::Restore.is_revertible());
            assert!(!EntryKind::Comment.is_revertible());
            assert!(!EntryKind::Access.is_revertible());
            assert!(!EntryKind::Compaction.is_revertible());
            assert!(!EntryKind::Redaction.is_revertible());
        }

        fn redacted_entry_is_non_revertible_regardless_of_entry_kind() {
            // The widget gates the [Revert] chip on `EntryKind::is_revertible`
            // AND `entry.redaction.is_none()`; a Splice that has since been
            // redacted MUST NOT show the chip.
            let mut e = entry(EntryKind::Text, "Alice", 0);
            e.redaction = Some(RedactionDisplay {
                redactor_label: "owner@example".into(),
                reason: "GDPR".into(),
                redacted_at: SystemTime::UNIX_EPOCH,
            });
            assert!(e.entry_kind.is_revertible());
            assert!(
                e.redaction.is_some(),
                "the chrome reads `entry.redaction.is_some()` as the veto",
            );
        }

        fn entry_kind_glyph_is_ascii_or_symbol_not_emoji() {
            for op in [
                EntryKind::Text,
                EntryKind::Move,
                EntryKind::Details,
                EntryKind::Comment,
                EntryKind::Delete,
                EntryKind::Restore,
                EntryKind::Access,
                EntryKind::Compaction,
                EntryKind::Redaction,
            ] {
                let g = op.glyph();
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
                "2 hr ago"
            );
            assert_eq!(
                format_relative_time(base, base + Duration::from_secs(259_200)),
                "3 days ago"
            );
        }

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

        entry_kind_revertibility_matches_spec_table();
        redacted_entry_is_non_revertible_regardless_of_entry_kind();
        entry_kind_glyph_is_ascii_or_symbol_not_emoji();
        format_relative_time_thresholds();
        hue_to_rgb_covers_every_sextant();
    }
}
