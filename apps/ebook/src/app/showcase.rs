//! Capture-only staging and geometry for the hosted reader showcase.
//!
//! These helpers never create a selection or an annotation. The example
//! delivers ordinary pointer events and invokes the published book actions.

#![cfg(any(test, feature = "test-support"))]

use vello::kurbo::Point;

use super::{Action, BooksApp, DocPos, View};
use crate::model::{Block, HiColor};
use crate::resource::Passage;

/// An existing authored book from the screenshot fixture, not a live shelf seed.
pub const BOOK_ID: &str = "book:field-notes";

/// Real passages from the fixture's first two paragraphs.
#[derive(Clone, Debug)]
pub struct ShowcasePassages {
    pub reader: Passage,
    pub collaborator: Passage,
}

/// Pointer endpoints resolved from the reader's actual painted text runs.
#[derive(Clone, Copy, Debug)]
pub struct DragTarget {
    pub start: Point,
    pub end: Point,
}

impl BooksApp {
    /// Open the fixture book with both drawers visible, without selecting text.
    ///
    /// # Errors
    /// Refuses a live reader or a missing/incomplete screenshot fixture.
    pub fn stage_showcase(&mut self) -> Result<ShowcasePassages, String> {
        if self.live || self.sample.is_none() {
            return Err("the showcase requires TestApp::capture_at's sample shelf".into());
        }
        let index = self
            .library
            .iter()
            .position(|book| book.id == BOOK_ID)
            .ok_or("the showcase book is missing from the sample shelf")?;
        let book = self.library.get(index).ok_or("missing showcase book")?;
        let chapter = book.chapters.first().ok_or("missing showcase chapter")?;
        let mut paragraphs = chapter
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, block)| matches!(block, Block::Paragraph { .. }).then_some(index));
        let reader = first_sentence(
            chapter,
            paragraphs.next().ok_or("missing reader paragraph")?,
        )?;
        let collaborator = first_sentence(
            chapter,
            paragraphs.next().ok_or("missing collaborator paragraph")?,
        )?;
        self.view = View::Reader;
        self.book_idx = index;
        self.chapter = 0;
        self.page = 0;
        self.scroll_y = 0.0;
        self.last_persisted = None;
        self.adopt_position = false;
        self.pending_jump_block = None;
        self.pending_last_page = false;
        self.pending_seek_fraction = None;
        self.pending_scroll_end = false;
        self.toc_scroll = 0.0;
        self.notes_scroll = 0.0;
        self.notes_scroll_to_annotation = None;
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.clear_selection();
        // Resolve the scaled drawers before snapping them: a breakpoint
        // crossing otherwise deliberately resets their presentation state.
        self.resolve_drawers();
        self.toc_drawer.snap(true);
        self.notes_drawer.snap(true);
        self.layout = None;
        self.dirty = true;
        Ok(ShowcasePassages {
            reader,
            collaborator,
        })
    }

    /// Resolve a passage to exact pointer endpoints after the hosted frame draws.
    ///
    /// # Errors
    /// Fails if either drawer is an overlay (which would intercept the real
    /// drag), the wrong chapter is open, or the passage is not entirely visible.
    pub fn showcase_drag(&self, passage: &Passage) -> Result<DragTarget, String> {
        if self.view != View::Reader
            || self.chapter != passage.chapter
            || self
                .library
                .get(self.book_idx)
                .is_none_or(|book| book.id != passage.book)
        {
            return Err("the showcase passage is not in the open chapter".into());
        }
        if !self.toc_open()
            || !self.notes_open()
            || self.toc_drawer.is_overlay()
            || self.notes_drawer.is_overlay()
        {
            return Err(
                "both drawers must be docked and open; an overlay would intercept the drag".into(),
            );
        }
        let start = self.showcase_point(DocPos {
            block: passage.block,
            ch: passage.start,
        });
        let end = self.showcase_point(DocPos {
            block: passage.block,
            ch: passage.end,
        });
        Ok(DragTarget {
            start: start.ok_or("the sentence start is not visible")?,
            end: end.ok_or("the sentence end is not visible")?,
        })
    }

    /// What the real drag selected, using the reader's clipboard projection.
    #[must_use]
    pub fn showcase_selection(&self) -> Option<Passage> {
        let selection = self.sel.filter(|selection| !selection.is_empty())?;
        let (start, end) = selection.ordered();
        if start.block != end.block {
            return None;
        }
        Some(Passage {
            book: self.library.get(self.book_idx)?.id.clone(),
            chapter: self.chapter,
            block: start.block,
            start: start.ch,
            end: end.ch,
            excerpt: self.selection_clipboard_text(),
        })
    }

    /// Centre of a colour control actually painted in the selection toolbar.
    #[must_use]
    pub fn showcase_swatch(&self, color: HiColor) -> Option<Point> {
        self.hits.iter().rev().find_map(|hit| {
            matches!(hit.action, Action::SetColor(found) if found == color)
                .then_some(hit.rect.center())
        })
    }

    /// Whether the drawn annotations rail exposes a clickable card for this mark.
    #[must_use]
    pub fn showcase_annotation_visible(&self, id: uuid::Uuid) -> bool {
        let Some(panel) = self.notes_panel_rect() else {
            return false;
        };
        self.hits.iter().any(|hit| {
            let visible = hit.rect.intersect(panel);
            matches!(hit.action, Action::JumpTo(found) if found == id)
                && visible.width() > 0.0
                && visible.height() > 0.0
        })
    }

    /// Whether every fixture card fits without a clipped card at the list edge.
    #[must_use]
    pub fn showcase_annotations_fit(&self) -> bool {
        self.notes_max_scroll <= 0.01
    }

    /// A real visible text location for the collaborator's window-local pointer.
    /// If their passage is on the next page, use the last visible text run;
    /// screen-space pointer presence does not claim a document selection.
    #[must_use]
    pub fn showcase_peer_point(&self, passage: &Passage) -> Option<Point> {
        self.showcase_point(DocPos {
            block: passage.block,
            ch: passage.start,
        })
        .or_else(|| {
            self.sel_runs
                .last()
                .map(|run| Point::new(f64::midpoint(run.x0, run.x1), f64::midpoint(run.y0, run.y1)))
        })
    }

    fn showcase_point(&self, position: DocPos) -> Option<Point> {
        self.sel_runs.iter().find_map(|run| {
            if run.block != position.block
                || run.cl == 0
                || position.ch < run.cs
                || position.ch > run.cs + run.cl
            {
                return None;
            }
            // Invert exactly the run interpolation used by hit_doc_pos,
            // then prove the resulting point maps back through that path.
            let fraction = (position.ch - run.cs) as f64 / run.cl as f64;
            let point = Point::new(
                run.x0 + (run.x1 - run.x0) * fraction,
                f64::midpoint(run.y0, run.y1),
            );
            // At an exact word edge the following run can own the pixel.
            // Probe just inside either edge and verify the real hit-test.
            [
                point,
                Point::new(point.x - 0.25, point.y),
                Point::new(point.x + 0.25, point.y),
            ]
            .into_iter()
            .find(|point| self.hit_doc_pos(*point) == Some(position))
        })
    }
}

fn first_sentence(chapter: &crate::model::Chapter, block: usize) -> Result<Passage, String> {
    let text = chapter.block_text(block);
    let end = text
        .chars()
        .position(|ch| matches!(ch, '.' | '!' | '?'))
        .map(|index| index + 1)
        .ok_or("the showcase paragraph has no complete sentence")?;
    Ok(Passage {
        book: BOOK_ID.into(),
        chapter: 0,
        block,
        start: 0,
        end,
        excerpt: text.chars().take(end).collect(),
    })
}
