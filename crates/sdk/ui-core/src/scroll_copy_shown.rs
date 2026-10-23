//! What a scroller showed in the frame being drawn and in the frame its host
//! presented, and what a frame that differs from the one on screen repaints.
//!
//! A scroller notes each frame it draws ([`ShownFrames::note`]) and moves the
//! note to the presented side when the host shows the frame
//! ([`ShownFrames::present`]). From its `collect_damage` it then asks how the
//! two compare ([`ShownFrames::compare`]): shown alike but for the scroll,
//! the scroll is offered as a copy ([`super::offer`]) and only what stays put
//! over the content is damage; shown alike in every way, it damages what it
//! knows changed in place; anything else repaints its viewport.
//!
//! Most scrollers describe a frame as a [`Showing`]: the scroll, the clip,
//! the transform, a look that must match for any reuse, and the pieces of
//! content it painted (a text editor's lines and caret, a page stack's
//! pages, a reader's paragraphs), each with a fingerprint of its paint.
//! [`ShownFrames::damage`] then repaints only the pieces that changed,
//! whether the frame scrolled or not.

use std::cell::{Ref, RefCell, RefMut};

use vello::kurbo::{Affine, Rect, Vec2};

/// One frame of a scroller, as a later frame compares it.
pub trait Shown {
    /// How far the content is scrolled: it lands on screen at its origin
    /// minus this, in logical pixels.
    fn scroll(&self) -> Vec2;
    /// Where the content paints, in logical pixels.
    fn clip(&self) -> Rect;
    /// The transform the content landed on screen with
    /// ([`super::on_screen`]).
    fn transform(&self) -> Affine;
    /// Whether `self` shows the content as `other` does but for the scroll
    /// and what the scroller damages in place itself: the same zoom, the
    /// same content size, the same look.
    fn same_view(&self, other: &Self) -> bool;
    /// Whether the content itself is as `other` shows it, so a scroll
    /// between the two only moves it. A scroller whose content changed in
    /// place repaints a scrolled frame.
    fn same_content(&self, _other: &Self) -> bool {
        true
    }
    /// What the scroller paints over its content inside [`Self::clip`] that
    /// does not move with it (a scroll bar's lane, an edge fade, a banner).
    fn overlays(&self, out: &mut Vec<Rect>);
}

/// How the frame being drawn differs from the one on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Since {
    /// Shown alike and not scrolled: the scroller damages what changed in
    /// place. Overlays that changed are already damage.
    Still,
    /// Only scrolled, and the host copies what stays on screen: the
    /// overlays are damage of the copy, and those of the frame on screen
    /// that are gone are already damage.
    Copied,
    /// Shown otherwise, or scrolled for a host that does not copy: the
    /// scroller repaints its viewport.
    Repaint,
}

/// The frame being drawn and the frame on screen, as noted.
#[derive(Debug)]
pub struct ShownFrames<S> {
    drawing: RefCell<Option<S>>,
    presented: Option<S>,
}

impl<S> Default for ShownFrames<S> {
    fn default() -> Self {
        Self {
            drawing: RefCell::new(None),
            presented: None,
        }
    }
}

impl<S> ShownFrames<S> {
    /// From the scroller's draw: what this frame shows.
    pub fn note(&self, shown: S) {
        *self.drawing.borrow_mut() = Some(shown);
    }

    /// From the scroller's `mark_drawn`: the frame just drawn is on screen.
    /// Not drawn this frame, what the screen shows where the scroller lies
    /// is not known to be its content, and its next frame repaints.
    pub fn present(&mut self) {
        self.presented = self.drawing.get_mut().take();
    }

    /// The frame being drawn and the frame on screen, when both are noted.
    pub fn frames(&self) -> Option<(Ref<'_, S>, &S)> {
        let drawing = Ref::filter_map(self.drawing.borrow(), Option::as_ref).ok()?;
        Some((drawing, self.presented.as_ref()?))
    }

    /// The frame being drawn, when noted.
    pub fn drawing(&self) -> Option<Ref<'_, S>> {
        Ref::filter_map(self.drawing.borrow(), Option::as_ref).ok()
    }

    /// The frame being drawn, to add what its draw learns late (the thumbs
    /// its bars painted).
    pub fn drawing_mut(&self) -> Option<RefMut<'_, S>> {
        RefMut::filter_map(self.drawing.borrow_mut(), Option::as_mut).ok()
    }

    /// The frame on screen, when noted.
    pub fn presented(&self) -> Option<&S> {
        self.presented.as_ref()
    }
}

impl<S: Shown + PartialEq> ShownFrames<S> {
    /// From the scroller's `collect_damage`: how this frame differs from
    /// the one on screen, offering a scroll to a host that copies on
    /// behalf of `owner` (the widget in the tree the scroller draws in).
    /// What stays put over the content and changed (or that a copy moves)
    /// is added to `regions`.
    pub fn compare<T: ?Sized>(&self, owner: &T, regions: &mut Vec<Rect>) -> Since {
        let Some((drawing, presented)) = self.frames() else {
            return Since::Repaint;
        };
        if drawing.clip() != presented.clip()
            || drawing.transform() != presented.transform()
            || !drawing.same_view(presented)
        {
            return Since::Repaint;
        }
        let delta = presented.scroll() - drawing.scroll();
        if delta == Vec2::ZERO {
            if *drawing != *presented {
                let (mut now, mut then) = (Vec::new(), Vec::new());
                drawing.overlays(&mut now);
                presented.overlays(&mut then);
                if now != then {
                    regions.extend(now.into_iter().chain(then));
                }
            }
            return Since::Still;
        }
        if !drawing.same_content(presented) {
            return Since::Repaint;
        }
        let mut overlays = Vec::new();
        drawing.overlays(&mut overlays);
        if !super::offer(
            owner,
            drawing.clip(),
            delta,
            overlays.clone(),
            drawing.transform(),
        ) {
            return Since::Repaint;
        }
        let mut then = Vec::new();
        presented.overlays(&mut then);
        regions.extend(then.into_iter().filter(|rect| !overlays.contains(rect)));
        Since::Copied
    }
}

/// One piece of what a scroller paints: a line, a page, a caret. `rect` is
/// where it paints with the content unscrolled (on screen plus the scroll),
/// or on screen for a piece that does not move with the content; `paint`
/// fingerprints its pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub key: u64,
    pub rect: Rect,
    pub paint: u64,
}

/// A scroller's frame as most scrollers describe it ([`Shown`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Showing<L> {
    /// How far the content is scrolled ([`Shown::scroll`]).
    pub scroll: Vec2,
    /// Where the content paints, in logical pixels.
    pub clip: Rect,
    /// The transform the content landed on screen with
    /// ([`super::on_screen`]).
    pub transform: Affine,
    /// What the whole view's paint depends on (a zoom, a theme's colours,
    /// a selection painted across pieces): another look repaints it.
    pub look: L,
    /// What paints over the content without moving with it.
    pub overlays: Vec<Rect>,
    /// The pieces of content in view, by ascending key.
    pub pieces: Vec<Piece>,
    /// What paints beside or over the content at fixed places (a thumb in a
    /// bar, a strip of thumbnails), by ascending key, `rect` on screen.
    pub fixed: Vec<Piece>,
}

impl<L: PartialEq> Shown for Showing<L> {
    fn scroll(&self) -> Vec2 {
        self.scroll
    }

    fn clip(&self) -> Rect {
        self.clip
    }

    fn transform(&self) -> Affine {
        self.transform
    }

    fn same_view(&self, other: &Self) -> bool {
        self.look == other.look
    }

    fn overlays(&self, out: &mut Vec<Rect>) {
        out.extend(&self.overlays);
    }
}

impl<L> Showing<L> {
    fn piece(&self, key: u64) -> Option<&Piece> {
        let at = self
            .pieces
            .binary_search_by_key(&key, |piece| piece.key)
            .ok()?;
        self.pieces.get(at)
    }

    /// Where `piece` of this frame paints on screen, within the clip.
    fn on_screen(&self, piece: &Piece) -> Rect {
        (piece.rect - self.scroll).intersect(self.clip)
    }
}

impl<L: PartialEq> ShownFrames<Showing<L>> {
    /// From the scroller's `collect_damage`: [`Self::compare`], then the
    /// pieces that changed, came or went where the screen showed them
    /// before; the viewport when the frame repaints. Returns how the frame
    /// compared.
    pub fn damage<T: ?Sized>(&self, owner: &T, regions: &mut Vec<Rect>) -> Since {
        let since = self.compare(owner, regions);
        let Some((drawing, presented)) = self.frames().filter(|_| since != Since::Repaint) else {
            if let Some(drawing) = self.drawing() {
                regions.push(drawing.clip);
            }
            return since;
        };
        // What the screen showed before, where this frame draws: a piece
        // that came into it is new content in place, one that came into
        // view only is in the strip the scroll exposed.
        let seen = (presented.clip + presented.scroll - drawing.scroll).intersect(drawing.clip);
        for piece in &drawing.pieces {
            match presented.piece(piece.key) {
                Some(was) if was == piece => {}
                Some(was) => {
                    // The old paint, moved with the content by any copy.
                    regions.push(drawing.on_screen(piece));
                    regions.push(drawing.on_screen(was));
                }
                None => regions.push(drawing.on_screen(piece).intersect(seen)),
            }
        }
        for was in &presented.pieces {
            if drawing.piece(was.key).is_none() {
                regions.push(drawing.on_screen(was));
            }
        }
        if drawing.fixed != presented.fixed {
            for (now, then) in [
                (&drawing.fixed, &presented.fixed),
                (&presented.fixed, &drawing.fixed),
            ] {
                regions.extend(
                    now.iter()
                        .filter(|piece| !then.contains(piece))
                        .map(|piece| piece.rect),
                );
            }
        }
        regions.retain(|rect| rect.width() > 0.0 && rect.height() > 0.0);
        since
    }
}
