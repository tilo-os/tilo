//! Per-participant view state.
//!
//! A participant is a person's window or an agent task. Each owns a
//! [`ParticipantView`] — caret, selection anchor, region and Find state — and
//! Writer builds that participant's resource tree from it through the pure
//! [`ParticipantView::ctx`]. The view is presentation state: it is never
//! written into the document.

use automerge::AutoCommit;

use super::{
    MAX_RESOURCE_SEARCH_HITS, RegionKind, SearchTarget, SelectionTarget, Span, WpResourceCtx,
};

/// A position in one block of the participant's region, in characters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ViewPoint {
    pub block: String,
    pub offset: usize,
}

impl ViewPoint {
    pub fn new(block: impl Into<String>, offset: usize) -> Self {
        Self {
            block: block.into(),
            offset,
        }
    }
}

/// A participant's Find state. `hits` are bounded by
/// [`MAX_RESOURCE_SEARCH_HITS`]; use [`FindState::new`] to enforce it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindState {
    pub query: String,
    pub hits: Vec<SearchTarget>,
}

impl FindState {
    /// Keep at most [`MAX_RESOURCE_SEARCH_HITS`] hits, in the order given.
    pub fn new(query: impl Into<String>, hits: impl IntoIterator<Item = SearchTarget>) -> Self {
        Self {
            query: query.into(),
            hits: hits.into_iter().take(MAX_RESOURCE_SEARCH_HITS).collect(),
        }
    }
}

/// A caret an edit could not name yet: the edit inserts blocks whose ids are
/// minted by the pinned actor at apply time (Enter's `SplitBlock`, a
/// multi-paragraph `PasteFragment`). [`ParticipantView::settle`] resolves it
/// against the document once the edit has been applied, exactly as the widget
/// finds the new block by comparing the block order before and after.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingCaret {
    /// The block the edit inserts after.
    pub after: String,
    /// The block that followed `after` before the edit (`None` at the end of
    /// the region). While it still directly follows `after`, the edit has not
    /// been applied and the caret stays at the fallback in `caret`.
    pub was_next: Option<String>,
    /// How many blocks past `after` the caret lands (at least 1).
    pub steps: usize,
    /// Character offset in the landing block.
    pub offset: usize,
}

/// One participant's view of a Writer document.
///
/// `anchor` is the selection's fixed end (the caret is the moving end);
/// `None` or an anchor equal to the caret means no selection. Offsets are
/// character offsets within `region`'s blocks. `pending`, when set, replaces
/// `caret` after the edit that produced it lands (see [`PendingCaret`]); every
/// reader goes through [`ParticipantView::settle`] first, so callers may keep
/// an unsettled view across the dispatch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParticipantView {
    pub caret: ViewPoint,
    pub anchor: Option<ViewPoint>,
    pub region: RegionKind,
    pub find: FindState,
    pub pending: Option<PendingCaret>,
}

impl ParticipantView {
    /// A collapsed caret at `caret` in `region`.
    pub fn at(caret: ViewPoint, region: RegionKind) -> Self {
        Self {
            caret,
            region,
            ..Self::default()
        }
    }

    /// A collapsed caret at the end of the body's last block, or `None` for a
    /// document without body blocks. The default spawn position of an agent.
    pub fn at_document_end(doc: &AutoCommit) -> Option<Self> {
        let containers = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
        let block = tilo_doc::model::region_blocks_in_order(
            doc,
            &containers,
            tilo_doc::model::Region::Body,
        )
        .pop()?;
        let offset = block_chars(doc, &containers, RegionKind::Body, &block);
        Some(Self::at(ViewPoint::new(block, offset), RegionKind::Body))
    }

    /// Rebuild the view a resource context was projected from. The context
    /// keeps the selection in document order, so the anchor is whichever end
    /// the caret is not on (the start when the caret is on neither). Find
    /// hits are kept; the query is not part of the context.
    pub fn from_ctx(ctx: &WpResourceCtx) -> Self {
        let caret = ViewPoint::new(&ctx.caret_block, ctx.caret_offset);
        let anchor = ctx.selection.as_ref().map(|selection| {
            let start = ViewPoint::new(&selection.start_block, selection.start);
            let end = ViewPoint::new(&selection.end_block, selection.end);
            if start == caret { end } else { start }
        });
        let anchor = anchor.filter(|anchor| *anchor != caret);
        Self {
            caret,
            anchor,
            region: ctx.region,
            find: FindState::new("", ctx.search_hits.iter().cloned()),
            pending: None,
        }
    }

    /// Resolve a [`PendingCaret`] against `doc`: once the edit that set it
    /// has been applied, the caret moves into the block it created and the
    /// selection collapses; before that (or if the edit was refused) the
    /// fallback `caret` stands. A view without a pending caret is returned
    /// unchanged. Pure.
    #[must_use]
    pub fn settle(&self, doc: &AutoCommit) -> Self {
        self.settle_in(doc, None)
    }

    /// [`Self::settle`] reading the body's order from `body`, the kept order
    /// of `doc`, so a body caret settles without listing the body.
    #[must_use]
    pub fn settle_in(&self, doc: &AutoCommit, body: Option<&tilo_document::BodyOrder>) -> Self {
        let mut out = self.clone();
        let Some(pending) = out.pending.take() else {
            return out;
        };
        // The block after `pending.after`, and the one `steps` after it.
        let at = |id_at: &dyn Fn(usize) -> Option<String>, index: usize| {
            (id_at(index + 1), id_at(index + pending.steps.max(1)))
        };
        let found = if let Some(body) = body.filter(|_| self.region == RegionKind::Body) {
            body.position(&pending.after)
                .map(|index| at(&|i| body.id_at(i).map(str::to_owned), index))
        } else {
            let Ok(containers) = tilo_doc::model::open(doc, &automerge::ROOT) else {
                return out;
            };
            let order =
                tilo_doc::model::region_blocks_in_order(doc, &containers, self.region.to_region());
            order
                .iter()
                .position(|block| *block == pending.after)
                .map(|index| at(&|i| order.get(i).cloned(), index))
        };
        let Some((next, target)) = found else {
            return out;
        };
        let applied = next.is_some() && next != pending.was_next;
        if applied && let Some(block) = target {
            out.caret = ViewPoint::new(block, pending.offset);
            out.anchor = None;
        }
        out
    }

    /// The selection in document order when it is non-empty and both ends
    /// are blocks of this view's region in `doc`.
    pub fn selection(&self, doc: &AutoCommit) -> Option<SelectionTarget> {
        if self.pending.is_some() {
            return self.settle(doc).selection(doc);
        }
        let containers = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
        let order =
            tilo_doc::model::region_blocks_in_order(doc, &containers, self.region.to_region());
        self.ordered_selection(&order)
            .map(|(start, end)| SelectionTarget {
                start_block: start.block.clone(),
                start: start.offset,
                end_block: end.block.clone(),
                end: end.offset,
            })
    }

    pub(super) fn ordered_selection<'a>(
        &'a self,
        order: &[String],
    ) -> Option<(&'a ViewPoint, &'a ViewPoint)> {
        self.ordered_selection_by(|block| order.iter().position(|b| b == block))
    }

    /// [`Self::ordered_selection`] with the region's positions read through
    /// `position`.
    pub(super) fn ordered_selection_by(
        &self,
        position: impl Fn(&str) -> Option<usize>,
    ) -> Option<(&ViewPoint, &ViewPoint)> {
        let anchor = self.anchor.as_ref()?;
        if *anchor == self.caret {
            return None;
        }
        let index = |p: &ViewPoint| position(&p.block);
        let key_anchor = (index(anchor)?, anchor.offset);
        let key_caret = (index(&self.caret)?, self.caret.offset);
        Some(if key_anchor <= key_caret {
            (anchor, &self.caret)
        } else {
            (&self.caret, anchor)
        })
    }

    /// Project this view into the resource-tree context, exactly as the
    /// widget's `wp_resource_ctx` does for the local window: `None` when the
    /// caret has no block or its block is not in the view's region (the
    /// widget then publishes no resources), otherwise the caret-anchored
    /// context with the selection's per-block non-empty spans and the
    /// bounded Find hits. Pure: it reads only `self` and `doc`.
    pub fn ctx(&self, doc: &AutoCommit) -> Option<WpResourceCtx> {
        if self.pending.is_some() {
            return self.settle(doc).ctx(doc);
        }
        if self.caret.block.is_empty() {
            return None;
        }
        let containers = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
        let order =
            tilo_doc::model::region_blocks_in_order(doc, &containers, self.region.to_region());
        if !order.contains(&self.caret.block) {
            return None;
        }
        let ordered = self.ordered_selection(&order);
        let mut format_spans = Vec::new();
        if let Some((start, end)) = ordered {
            let (Some(si), Some(ei)) = (
                order.iter().position(|b| *b == start.block),
                order.iter().position(|b| *b == end.block),
            ) else {
                return None;
            };
            for (i, block) in order.iter().enumerate().take(ei + 1).skip(si) {
                let s = if i == si { start.offset } else { 0 };
                let e = if i == ei {
                    end.offset
                } else {
                    block_chars(doc, &containers, self.region, block)
                };
                if s < e {
                    format_spans.push(Span {
                        block: block.clone(),
                        start: s,
                        end: e,
                    });
                }
            }
        }
        Some(WpResourceCtx {
            caret_block: self.caret.block.clone(),
            caret_offset: self.caret.offset,
            // Matches the widget: block-level palette targets anchor on the
            // caret block, not the selection.
            target_blocks: vec![self.caret.block.clone()],
            format_spans,
            selection: ordered.map(|(start, end)| SelectionTarget {
                start_block: start.block.clone(),
                start: start.offset,
                end_block: end.block.clone(),
                end: end.offset,
            }),
            region: self.region,
            search_hits: self
                .find
                .hits
                .iter()
                .take(MAX_RESOURCE_SEARCH_HITS)
                .cloned()
                .collect(),
            // The view owns no content: it reads the store of the window or
            // worker it runs in, else an empty one.
            content: ui::tilo_resource::content::current()
                .unwrap_or_else(tilo_content::ContentStore::memory),
        })
    }
}

pub(super) fn block_chars(
    doc: &AutoCommit,
    containers: &tilo_doc::Containers,
    region: RegionKind,
    block: &str,
) -> usize {
    tilo_doc::model::region_block_obj(doc, containers, region.to_region(), block)
        .and_then(|obj| tilo_doc::model::block_text_on(doc, &obj))
        .map_or(0, |text| text.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(paragraphs: &[&str]) -> (AutoCommit, Vec<String>) {
        let mut doc = AutoCommit::new();
        let containers = tilo_doc::model::init(&mut doc, &automerge::ROOT);
        let mut ids = tilo_doc::model::seeded_id_gen(&doc, &containers);
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &containers,
            ids: &mut ids,
        };
        let mut after: Option<String> = None;
        let mut out = Vec::new();
        for text in paragraphs {
            let id = dm.insert_block(after.as_deref(), "paragraph");
            dm.splice_text(&id, 0, 0, text);
            after = Some(id.clone());
            out.push(id);
        }
        (doc, out)
    }

    #[test]
    fn collapsed_view_projects_caret_context() {
        let (doc, ids) = doc_with(&["hello", "world"]);
        let view = ParticipantView::at(ViewPoint::new(&ids[1], 2), RegionKind::Body);
        let ctx = view.ctx(&doc).unwrap();
        assert_eq!(ctx.caret_block, ids[1]);
        assert_eq!(ctx.caret_offset, 2);
        assert_eq!(ctx.target_blocks, vec![ids[1].clone()]);
        assert!(ctx.selection.is_none());
        assert!(ctx.format_spans.is_empty());
    }

    #[test]
    fn backwards_selection_is_ordered_with_per_block_spans() {
        let (doc, ids) = doc_with(&["hello", "big", "world"]);
        let view = ParticipantView {
            caret: ViewPoint::new(&ids[0], 3),
            anchor: Some(ViewPoint::new(&ids[2], 2)),
            ..ParticipantView::default()
        };
        let ctx = view.ctx(&doc).unwrap();
        assert_eq!(
            ctx.selection,
            Some(SelectionTarget {
                start_block: ids[0].clone(),
                start: 3,
                end_block: ids[2].clone(),
                end: 2,
            })
        );
        let spans: Vec<_> = ctx
            .format_spans
            .iter()
            .map(|s| (s.block.as_str(), s.start, s.end))
            .collect();
        assert_eq!(
            spans,
            vec![
                (ids[0].as_str(), 3, 5),
                (ids[1].as_str(), 0, 3),
                (ids[2].as_str(), 0, 2)
            ]
        );
        assert_eq!(view.selection(&doc), ctx.selection);
    }

    #[test]
    fn unknown_caret_publishes_nothing_and_find_is_bounded() {
        let (doc, ids) = doc_with(&["abc"]);
        assert!(ParticipantView::default().ctx(&doc).is_none());
        let stale = ParticipantView::at(ViewPoint::new("gone", 0), RegionKind::Body);
        assert!(stale.ctx(&doc).is_none());
        let hits = (0..200).map(|i| SearchTarget {
            block: ids[0].clone(),
            start: i,
            end: i + 1,
        });
        let view = ParticipantView {
            find: FindState::new("a", hits),
            ..ParticipantView::at(ViewPoint::new(&ids[0], 0), RegionKind::Body)
        };
        assert_eq!(view.find.hits.len(), MAX_RESOURCE_SEARCH_HITS);
        assert_eq!(
            view.ctx(&doc).unwrap().search_hits.len(),
            MAX_RESOURCE_SEARCH_HITS
        );
        let end = ParticipantView::at_document_end(&doc).unwrap();
        assert_eq!(end.caret, ViewPoint::new(&ids[0], 3));
    }
}
