//! Whether a frame paints its scene, and whether what was painted last is still the frame.
//!
//! A widget that replaces what its parent held leaves a composition the rest of the input reads the
//! tree through. That pass lays the tree out and admits it, and paints nothing: the frame the input
//! asks for paints the tree it made, once.

use std::cell::Cell;
use std::rc::Rc;

use super::{FrameHarness, READMITTING};

/// What the harness holds of painting.
pub(super) struct Admission {
    /// Frames that painted their scene, whether or not the tree painted a command in it.
    painted: u64,
    /// The frame laying out now admits a replaced composition instead of painting it.
    admitting: Rc<Cell<bool>>,
    /// What was painted last is not the frame: the next one is due and paints it.
    stale: bool,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            painted: 0,
            admitting: Rc::new(Cell::new(false)),
            stale: false,
        }
    }
}

/// A frame admitting a replaced composition: the frame paints again when this ends, unwound or not.
struct Admitting(Rc<Cell<bool>>);

impl Drop for Admitting {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl Admission {
    /// Whether the frame laying out now paints its scene.
    pub(super) fn paints(&self, scene_drawn: bool) -> bool {
        scene_drawn && !self.admitting.get()
    }

    /// The frame ended: count its paint, or leave the frame for the next one to paint.
    pub(super) fn ended(&mut self, painted: bool, scene_drawn: bool) {
        if painted {
            self.painted = self.painted.saturating_add(1);
            self.stale = false;
        } else if scene_drawn {
            self.stale = true;
        }
    }

    /// Whether what this harness painted last is still the frame.
    pub(super) fn stale(&self) -> bool {
        self.stale
    }

    /// Frames that painted their scene since construction.
    pub(super) fn painted_frames(&self) -> u64 {
        self.painted
    }

    /// A page that starts painting paints the frame; one that stops presents no scene at all.
    pub(super) fn painting(&mut self, drawn: bool) {
        self.stale = drawn;
    }

    /// Lay the frame out and admit it without painting it.
    fn admitting(&self) -> Admitting {
        self.admitting.set(true);
        Admitting(Rc::clone(&self.admitting))
    }
}

impl FrameHarness {
    /// When the composition was replaced since it was admitted, retire its
    /// raw nodes and admit the new one, keeping a surviving live focus.
    ///
    /// The frame this lays out builds the new composition and admits it, and
    /// syncs nothing of it (see [`FrameHarness::sync_frame`]), so a
    /// model that reports a structure change from every frame is laid out by
    /// it once instead of asking for readmission after readmission.
    ///
    /// It paints nothing ([`Admission`]): this pass is what the rest of the
    /// input reads the tree through, and the frame that input asks for paints
    /// the tree it made. Painting here painted the whole window for a frame
    /// the next one painted again.
    pub(super) fn readmit_replaced_composition(&mut self) {
        if !self.take_composition_dirty() {
            return;
        }
        let focus = crate::widget::layout::CompositionFocus::capture_live(self.root.as_ref());
        self.discard_layout_tree();
        struct Unreadmitting;
        impl Drop for Unreadmitting {
            fn drop(&mut self) {
                READMITTING.set(false);
            }
        }
        let _unreadmitting = Unreadmitting;
        READMITTING.set(true);
        let _admitting = self.admission.admitting();
        self.relayout();
        if let Some(focus) = focus
            && (self.focused.is_none() || self.focus_by_name)
        {
            self.focus_by_name = false;
            // SAFETY: the relayout admitted this fresh, exclusively owned tree.
            self.focused =
                unsafe { focus.restore(&self.node_map, &self.bindings, None) }.or(self.focused);
        }
    }
}
