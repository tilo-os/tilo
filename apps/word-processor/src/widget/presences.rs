//! Per-presence Writer views held by one widget: one per invoking presence besides
//! the window's own caret, keyed by the host's [`PresenceId`]. They are presentation
//! state, never written to the document, bounded by [`MAX_PRESENCE_VIEWS`] and
//! [`PRESENCE_VIEW_TTL`], and held as Automerge cursors so a concurrent edit moves them
//! with its characters.

use std::collections::HashMap;

use automerge::{AutoCommit, Cursor, ObjId, ReadDoc};
use tilo_actions::PresenceId;
use ui::time::{Duration, Instant};

use crate::actions::RegionKind;
use crate::actions::presence::{FindState, PresenceView, ViewPoint};

pub(crate) const MAX_PRESENCE_VIEWS: usize = 64;
pub(crate) const PRESENCE_VIEW_TTL: Duration = Duration::from_secs(10 * 60);
/// The `find-text` argument bound, in bytes.
pub(crate) use crate::actions::editing::MAX_FIND_QUERY_BYTES;
/// Upper bound on a presence's display label, in bytes.
pub(crate) const MAX_PRESENCE_LABEL_BYTES: usize = 128;

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
pub(crate) struct PresenceViews {
    entries: HashMap<PresenceId, Entry>,
    joining: Vec<(PresenceId, Instant)>,
    resolved_at: std::cell::RefCell<HashMap<PresenceId, DocVersion>>,
}

thread_local! {
    static OUTCOMES: std::cell::RefCell<Vec<(PresenceId, PresenceView)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn note_presence_outcome(next: &PresenceView) {
    let Some(presence) = ui::invocation_scope::current_presence() else {
        return;
    };
    OUTCOMES.with_borrow_mut(|outcomes| {
        outcomes.retain(|(id, _)| *id != presence);
        if outcomes.len() >= MAX_PRESENCE_VIEWS {
            outcomes.remove(0);
        }
        outcomes.push((presence, next.clone()));
    });
}

fn take_outcomes() -> Vec<(PresenceId, PresenceView)> {
    OUTCOMES.with_borrow_mut(std::mem::take)
}

impl PresenceViews {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn contains(&self, presence: &PresenceId) -> bool {
        self.entries.contains_key(presence) || self.joining.iter().any(|(id, _)| id == presence)
    }

    pub(crate) fn join(&mut self, presence: PresenceId, now: Instant) -> bool {
        if self.contains(&presence) {
            return false;
        }
        if self.joining.len() >= MAX_PRESENCE_VIEWS {
            self.joining.remove(0);
        }
        self.joining.push((presence, now));
        true
    }

    pub(crate) fn note_resolution(&self, presence: &PresenceId, doc: &AutoCommit) {
        let mut resolved = self.resolved_at.borrow_mut();
        if resolved.len() >= MAX_PRESENCE_VIEWS && !resolved.contains_key(presence) {
            resolved.clear();
        }
        resolved.insert(presence.clone(), doc_version(doc));
    }

    /// Adopt the noted outcomes of presence edits that applied.
    pub(crate) fn adopt_outcomes(&mut self, doc: &AutoCommit, now: Instant) {
        let outcomes = take_outcomes();
        if outcomes.is_empty() {
            return;
        }
        let heads = doc_version(doc);
        for (presence, next) in outcomes {
            let applied = self
                .resolved_at
                .borrow_mut()
                .remove(&presence)
                .is_some_and(|before| before != heads);
            if applied {
                self.joining.retain(|(id, _)| *id != presence);
                self.set(&presence, &next, doc, now);
            }
        }
    }

    pub(crate) fn presences(&self) -> impl Iterator<Item = &PresenceId> {
        self.entries.keys()
    }

    pub(crate) fn view(&self, presence: &PresenceId, doc: &AutoCommit) -> Option<PresenceView> {
        self.entries.get(presence).map(|entry| resolve(entry, doc))
    }

    pub(crate) fn label(&self, presence: &PresenceId) -> Option<&str> {
        self.entries.get(presence)?.label.as_deref()
    }

    /// Store `view` for `presence`, settled against `doc` as it is now, so call it
    /// after the presence's envelopes were applied.
    pub(crate) fn set(
        &mut self,
        presence: &PresenceId,
        view: &PresenceView,
        doc: &AutoCommit,
        now: Instant,
    ) {
        let label = self
            .entries
            .get(presence)
            .and_then(|entry| entry.label.clone());
        let entry = pin(view, label, doc, now);
        self.entries.insert(presence.clone(), entry);
        self.expire(now);
        self.evict_lru_except(presence);
    }

    pub(crate) fn set_label(&mut self, presence: &PresenceId, label: &str) {
        if let Some(entry) = self.entries.get_mut(presence) {
            entry.label = Some(truncate(label, MAX_PRESENCE_LABEL_BYTES).to_string());
        }
    }

    #[cfg(test)]
    pub(crate) fn touch(&mut self, presence: &PresenceId, now: Instant) {
        if let Some(entry) = self.entries.get_mut(presence) {
            entry.touched = now;
        }
    }

    pub(crate) fn remove(&mut self, presence: &PresenceId) -> bool {
        let joining = self.joining.len();
        self.joining.retain(|(id, _)| id != presence);
        self.resolved_at.borrow_mut().remove(presence);
        self.entries.remove(presence).is_some() || self.joining.len() != joining
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.touched) <= PRESENCE_VIEW_TTL);
        self.joining
            .retain(|(_, joined)| now.saturating_duration_since(*joined) <= PRESENCE_VIEW_TTL);
    }

    fn evict_lru_except(&mut self, keep: &PresenceId) {
        while self.entries.len() > MAX_PRESENCE_VIEWS {
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
        for (presence, joined) in std::mem::take(&mut self.joining) {
            if let Some(view) = PresenceView::at_document_end(doc) {
                self.set(&presence, &view, doc, joined);
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
    tilo_doc::model::block_text_obj(doc, &obj)
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

fn pin(view: &PresenceView, label: Option<String>, doc: &AutoCommit, now: Instant) -> Entry {
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

fn resolve(entry: &Entry, doc: &AutoCommit) -> PresenceView {
    let Some(caret) = resolve_point(doc, entry.region, &entry.caret) else {
        let mut view = PresenceView::at_document_end(doc).unwrap_or_default();
        view.find = entry.find.clone();
        return view;
    };
    let anchor = entry
        .anchor
        .as_ref()
        .and_then(|anchor| resolve_point(doc, entry.region, anchor));
    PresenceView {
        caret,
        anchor,
        region: entry.region,
        find: entry.find.clone(),
        pending: None,
    }
}
