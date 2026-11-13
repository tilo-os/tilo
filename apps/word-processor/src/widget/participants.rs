//! Per-participant Writer views held by one widget: one per invoking participant besides
//! the window's own caret, keyed by the host's [`ParticipantId`]. They are presentation
//! state, never written to the document, bounded by [`MAX_PARTICIPANT_VIEWS`] and
//! [`PARTICIPANT_VIEW_TTL`], and held as Automerge cursors so a concurrent edit moves them
//! with its characters.

use std::collections::HashMap;

use automerge::{AutoCommit, Cursor, ObjId, ObjType, ReadDoc, Value};
use tilo_actions::ParticipantId;
use ui::time::{Duration, Instant};

use crate::actions::RegionKind;
use crate::actions::participant::{FindState, ParticipantView, ViewPoint};

pub(crate) const MAX_PARTICIPANT_VIEWS: usize = 64;
pub(crate) const PARTICIPANT_VIEW_TTL: Duration = Duration::from_secs(10 * 60);
/// The `find-text` argument bound, in bytes.
pub(crate) use crate::actions::editing::MAX_FIND_QUERY_BYTES;
/// Upper bound on a participant's display label, in bytes.
pub(crate) const MAX_PARTICIPANT_LABEL_BYTES: usize = 128;

#[derive(Debug, Clone)]
struct Pinned {
    point: ViewPoint,
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

#[derive(Debug, Default)]
pub(crate) struct ParticipantViews {
    entries: HashMap<ParticipantId, Entry>,
    joining: Vec<(ParticipantId, Instant)>,
    resolved_at: std::cell::RefCell<HashMap<ParticipantId, DocVersion>>,
}

thread_local! {
    static OUTCOMES: std::cell::RefCell<Vec<(ParticipantId, ParticipantView)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

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

    pub(crate) fn note_resolution(&self, participant: &ParticipantId, doc: &AutoCommit) {
        let mut resolved = self.resolved_at.borrow_mut();
        if resolved.len() >= MAX_PARTICIPANT_VIEWS && !resolved.contains_key(participant) {
            resolved.clear();
        }
        resolved.insert(participant.clone(), doc_version(doc));
    }

    /// Adopt the noted outcomes of participant edits that applied.
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

    /// Store `view` for `participant`, settled against `doc` as it is now, so call it
    /// after the participant's envelopes were applied.
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

    pub(crate) fn set_label(&mut self, participant: &ParticipantId, label: &str) {
        if let Some(entry) = self.entries.get_mut(participant) {
            entry.label = Some(truncate(label, MAX_PARTICIPANT_LABEL_BYTES).to_string());
        }
    }

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

fn text_obj(doc: &AutoCommit, region: RegionKind, block: &str) -> Option<ObjId> {
    let containers = crate::model::open(doc).ok()?;
    let obj = tilo_doc::model::region_block_obj(doc, &containers, region.to_region(), block)?;
    match doc.get(&obj, tilo_doc::schema::BLOCK_TEXT).ok()? {
        Some((Value::Object(ObjType::Text), text)) => Some(text),
        _ => None,
    }
}

fn block_len(doc: &AutoCommit, region: RegionKind, block: &str) -> Option<usize> {
    let containers = crate::model::open(doc).ok()?;
    let obj = tilo_doc::model::region_block_obj(doc, &containers, region.to_region(), block)?;
    Some(tilo_doc::model::block_text_on(doc, &obj).map_or(0, |text| text.chars().count()))
}

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
