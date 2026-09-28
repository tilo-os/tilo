//! Per-participant Writer views held by one widget.
//!
//! A widget serving remote invocations (the Cloud headless worker, or a
//! desktop window an agent is working in) keeps one [`ParticipantView`] per
//! invoking participant besides its own window's caret. Each participant's
//! resource tree is built from its own view, so one participant's caret,
//! selection or Find query never shapes another participant's tree.
//!
//! - **Keyed by [`ParticipantId`]**, which the host derives from the
//!   authenticated invoker; the view only routes and is never authority.
//! - **Bounded**: at most [`MAX_PARTICIPANT_VIEWS`] views (least recently used
//!   evicted first) and dropped after [`PARTICIPANT_VIEW_TTL`] idle.
//! - **Never in the document**: views are presentation state held here. They
//!   survive only as long as the widget.
//! - **Rebased on every change**, local or remote: each end of a view is held
//!   as an Automerge [`Cursor`] into its block's text, so concurrent inserts
//!   and deletes before it move it exactly as they move the characters. A
//!   view whose block disappears falls back to the end of the document (an
//!   agent's default position) and loses its selection.

use std::collections::HashMap;

use automerge::{AutoCommit, Cursor, ObjId, ObjType, ReadDoc, Value};
use tilo_actions::ParticipantId;
use ui::clock::{Duration, Instant};

use crate::actions::RegionKind;
use crate::actions::participant::{FindState, ParticipantView, ViewPoint};

/// Upper bound on concurrently held participant views per widget.
pub(crate) const MAX_PARTICIPANT_VIEWS: usize = 64;
/// Idle time after which a participant's view is dropped.
pub(crate) const PARTICIPANT_VIEW_TTL: Duration = Duration::from_secs(10 * 60);
/// Upper bound on a participant's Find query, in bytes (the `find-text`
/// argument bound).
pub(crate) use crate::actions::editing::MAX_FIND_QUERY_BYTES;
/// Upper bound on a participant's display label, in bytes.
pub(crate) const MAX_PARTICIPANT_LABEL_BYTES: usize = 128;

/// One view end pinned to the characters around it.
#[derive(Debug, Clone)]
struct Pinned {
    point: ViewPoint,
    /// `None` when the block has no Text object;
    /// the offset is then only clamped.
    cursor: Option<Cursor>,
}

#[derive(Debug, Clone)]
struct Entry {
    caret: Pinned,
    anchor: Option<Pinned>,
    region: RegionKind,
    find: FindState,
    label: Option<String>,
    touched: Instant,
}

/// The per-participant view map. See the module docs for its guarantees.
#[derive(Debug, Default)]
pub(crate) struct ParticipantViews {
    entries: HashMap<ParticipantId, Entry>,
    /// Participants that joined before a document was at hand; placed at
    /// the document end on the next [`Self::rebase`].
    joining: Vec<(ParticipantId, Instant)>,
    /// Document heads each participant's tree was last resolved against, so
    /// a noted edit outcome is adopted only once the document moved on.
    resolved_at: std::cell::RefCell<HashMap<ParticipantId, DocVersion>>,
}

thread_local! {
    /// Next views of participant edits built on this thread (see
    /// [`note_participant_outcome`]), awaiting the widget's next sync.
    static OUTCOMES: std::cell::RefCell<Vec<(ParticipantId, ParticipantView)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record the view a published Document operation leaves its invoker with
/// (the second half of an `editing::*` outcome), when it is built for a
/// delivered invocation by a participant
/// ([`ui::invocation_scope::current_participant`]). The serving widget adopts
/// it on its next sync if the document changed since the participant's tree
/// was resolved — i.e. the envelope applied. Positional ops that only insert
/// at the caret are also covered by the cursor rebase; this carries the
/// structural ones (Enter, a join, a multi-paragraph paste). Bounded; a no-op
/// outside a participant's invocation.
pub(crate) fn note_participant_outcome(next: &ParticipantView) {
    let Some(participant) = ui::invocation_scope::current_participant() else {
        return;
    };
    OUTCOMES.with_borrow_mut(|outcomes| {
        outcomes.retain(|(id, _)| *id != participant);
        if outcomes.len() >= MAX_PARTICIPANT_VIEWS {
            outcomes.remove(0);
        }
        outcomes.push((participant, next.clone()));
    });
}

fn take_outcomes() -> Vec<(ParticipantId, ParticipantView)> {
    OUTCOMES.with_borrow_mut(std::mem::take)
}

impl ParticipantViews {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn contains(&self, participant: &ParticipantId) -> bool {
        self.entries.contains_key(participant)
            || self.joining.iter().any(|(id, _)| id == participant)
    }

    /// A participant joined; its view is created at the document end on the
    /// next [`Self::rebase`]. Returns whether it is new.
    pub(crate) fn join(&mut self, participant: ParticipantId, now: Instant) -> bool {
        if self.contains(&participant) {
            return false;
        }
        if self.joining.len() >= MAX_PARTICIPANT_VIEWS {
            self.joining.remove(0);
        }
        self.joining.push((participant, now));
        true
    }

    /// Remember that `participant`'s tree is being resolved against `doc`
    /// now (a noted outcome applies only after the document moved on).
    pub(crate) fn note_resolution(&self, participant: &ParticipantId, doc: &AutoCommit) {
        let mut resolved = self.resolved_at.borrow_mut();
        if resolved.len() >= MAX_PARTICIPANT_VIEWS && !resolved.contains_key(participant) {
            resolved.clear();
        }
        resolved.insert(participant.clone(), doc_version(doc));
    }

    /// Adopt the noted outcomes of participant edits that applied: the
    /// document moved on since that participant's tree was resolved. An
    /// outcome whose envelope was refused leaves the view to the rebase.
    pub(crate) fn adopt_outcomes(&mut self, doc: &AutoCommit, now: Instant) {
        let outcomes = take_outcomes();
        if outcomes.is_empty() {
            return;
        }
        let heads = doc_version(doc);
        for (participant, next) in outcomes {
            let applied = self
                .resolved_at
                .borrow_mut()
                .remove(&participant)
                .is_some_and(|before| before != heads);
            if applied {
                self.joining.retain(|(id, _)| *id != participant);
                self.set(&participant, &next, doc, now);
            }
        }
    }

    pub(crate) fn participants(&self) -> impl Iterator<Item = &ParticipantId> {
        self.entries.keys()
    }

    /// `participant`'s view resolved against `doc` now: each end follows the
    /// characters it was placed next to. `None` for an unknown participant.
    pub(crate) fn view(
        &self,
        participant: &ParticipantId,
        doc: &AutoCommit,
    ) -> Option<ParticipantView> {
        self.entries
            .get(participant)
            .map(|entry| resolve(entry, doc))
    }

    pub(crate) fn label(&self, participant: &ParticipantId) -> Option<&str> {
        self.entries.get(participant)?.label.as_deref()
    }

    /// Store `view` for `participant`, valid against `doc` as it is now (so
    /// call it after the participant's envelopes were applied; a pending
    /// caret is settled against `doc`). The Find
    /// query and hits are bounded, and the map evicts expired and then least
    /// recently used views to stay within [`MAX_PARTICIPANT_VIEWS`].
    pub(crate) fn set(
        &mut self,
        participant: &ParticipantId,
        view: &ParticipantView,
        doc: &AutoCommit,
        now: Instant,
    ) {
        let label = self
            .entries
            .get(participant)
            .and_then(|entry| entry.label.clone());
        let entry = pin(view, label, doc, now);
        self.entries.insert(participant.clone(), entry);
        self.expire(now);
        self.evict_lru_except(participant);
    }

    /// Set the name shown next to `participant`'s caret. Unknown participants
    /// are ignored; the label is truncated to a bounded length.
    pub(crate) fn set_label(&mut self, participant: &ParticipantId, label: &str) {
        if let Some(entry) = self.entries.get_mut(participant) {
            entry.label = Some(truncate(label, MAX_PARTICIPANT_LABEL_BYTES).to_string());
        }
    }

    /// Mark `participant` as active now (an invocation resolved its tree).
    #[cfg(test)]
    pub(crate) fn touch(&mut self, participant: &ParticipantId, now: Instant) {
        if let Some(entry) = self.entries.get_mut(participant) {
            entry.touched = now;
        }
    }

    pub(crate) fn remove(&mut self, participant: &ParticipantId) -> bool {
        let joining = self.joining.len();
        self.joining.retain(|(id, _)| id != participant);
        self.resolved_at.borrow_mut().remove(participant);
        self.entries.remove(participant).is_some() || self.joining.len() != joining
    }

    /// Drop views idle for longer than [`PARTICIPANT_VIEW_TTL`].
    pub(crate) fn expire(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            now.saturating_duration_since(entry.touched) <= PARTICIPANT_VIEW_TTL
        });
        self.joining
            .retain(|(_, joined)| now.saturating_duration_since(*joined) <= PARTICIPANT_VIEW_TTL);
    }

    fn evict_lru_except(&mut self, keep: &ParticipantId) {
        while self.entries.len() > MAX_PARTICIPANT_VIEWS {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(id, _)| *id != keep)
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(id, _)| id.clone())
            else {
                return;
            };
            self.entries.remove(&oldest);
        }
    }

    /// Re-resolve every view against `doc` and re-pin the ones whose block
    /// vanished, so stored offsets stay current. Cheap when empty.
    pub(crate) fn rebase(&mut self, doc: &AutoCommit) {
        for (participant, joined) in std::mem::take(&mut self.joining) {
            if let Some(view) = ParticipantView::at_document_end(doc) {
                self.set(&participant, &view, doc, joined);
            }
        }
        if self.entries.is_empty() {
            return;
        }
        for entry in self.entries.values_mut() {
            let view = resolve(entry, doc);
            if view.caret != entry.caret.point
                || view.anchor.as_ref() != entry.anchor.as_ref().map(|a| &a.point)
            {
                *entry = pin(&view, entry.label.take(), doc, entry.touched);
            }
        }
    }
}

/// Committed heads plus the open transaction's size: changes whenever an
/// envelope applied, committed or not.
type DocVersion = (Vec<automerge::ChangeHash>, usize);

fn doc_version(doc: &AutoCommit) -> DocVersion {
    use automerge::transaction::Transactable;
    (doc.base_heads(), doc.pending_ops())
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    text.get(..text.floor_char_boundary(max)).unwrap_or(text)
}

/// The canonical Text object of `block` in `region`, if it has one.
fn text_obj(doc: &AutoCommit, region: RegionKind, block: &str) -> Option<ObjId> {
    let containers = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
    let obj = tilo_doc::model::region_block_obj(doc, &containers, region.to_region(), block)?;
    match doc.get(&obj, tilo_doc::schema::BLOCK_TEXT).ok()? {
        Some((Value::Object(ObjType::Text), text)) => Some(text),
        _ => None,
    }
}

fn block_len(doc: &AutoCommit, region: RegionKind, block: &str) -> Option<usize> {
    let containers = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
    let obj = tilo_doc::model::region_block_obj(doc, &containers, region.to_region(), block)?;
    Some(tilo_doc::model::block_text_on(doc, &obj).map_or(0, |text| text.chars().count()))
}

/// Pin `point` to the character right after it (or to the block end), so an
/// insert *at* the point lands before it and pushes it forward: the caret of
/// a participant that types keeps advancing past its own text, and a
/// deletion around it collapses it onto the next surviving character.
fn pin_point(doc: &AutoCommit, region: RegionKind, point: ViewPoint) -> Pinned {
    let cursor = text_obj(doc, region, &point.block).and_then(|text| {
        let len = doc.length(&text);
        if point.offset >= len {
            doc.get_cursor(&text, automerge::CursorPosition::End, None)
                .ok()
        } else {
            doc.get_cursor_moving(&text, point.offset, None, automerge::MoveCursor::After)
                .ok()
        }
    });
    Pinned { point, cursor }
}

fn pin(view: &ParticipantView, label: Option<String>, doc: &AutoCommit, now: Instant) -> Entry {
    // A caret the edit could only describe relative to blocks it minted
    // resolves now, against the document the edit produced.
    let view = view.settle(doc);
    let region = view.region;
    let query = truncate(&view.find.query, MAX_FIND_QUERY_BYTES).to_string();
    Entry {
        caret: pin_point(doc, region, view.caret),
        anchor: view.anchor.map(|anchor| pin_point(doc, region, anchor)),
        region,
        find: FindState::new(query, view.find.hits),
        label,
        touched: now,
    }
}

/// Where `pinned` is in `doc` now, or `None` when its block is gone.
fn resolve_point(doc: &AutoCommit, region: RegionKind, pinned: &Pinned) -> Option<ViewPoint> {
    let block = &pinned.point.block;
    if let (Some(cursor), Some(text)) = (&pinned.cursor, text_obj(doc, region, block))
        && let Ok(offset) = doc.get_cursor_position(&text, cursor, None)
    {
        return Some(ViewPoint::new(block.clone(), offset));
    }
    let len = block_len(doc, region, block)?;
    Some(ViewPoint::new(block.clone(), pinned.point.offset.min(len)))
}

fn resolve(entry: &Entry, doc: &AutoCommit) -> ParticipantView {
    let Some(caret) = resolve_point(doc, entry.region, &entry.caret) else {
        // The caret's block was deleted: park at the document end, like a
        // freshly spawned agent, and drop the selection.
        let mut view = ParticipantView::at_document_end(doc).unwrap_or_default();
        view.find = entry.find.clone();
        return view;
    };
    let anchor = entry
        .anchor
        .as_ref()
        .and_then(|anchor| resolve_point(doc, entry.region, anchor));
    ParticipantView {
        caret,
        anchor,
        region: entry.region,
        find: entry.find.clone(),
        pending: None,
    }
}
