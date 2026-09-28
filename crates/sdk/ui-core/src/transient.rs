//! Pixels a widget paints on this device's own clock.
//!
//! Some of what the toolkit paints follows nothing a session carries: a
//! spinner's orbit, an indeterminate bar's sweep, a status pill or toast that
//! appears on a local event and fades on the wall clock, a tooltip that opens
//! after a hover delay. A recording's replay cannot reproduce those pixels,
//! so the widgets that paint them declare where, as they paint, and the app
//! publishes each declared rectangle as a
//! [`transient`](crate::a11y_tree::SemanticNode::transient) node of its semantic
//! tree. A recording's frame check then leaves them out rather than blame the
//! renderer for them. Apps get this by painting with those widgets; nothing
//! is opted into.

use std::cell::RefCell;

use vello::kurbo::{Affine, Rect};

thread_local! {
    /// The rectangles declared during the frame being painted on this
    /// thread, in the device pixels the frame's base transform maps to.
    /// `None` outside a frame.
    static FRAME: RefCell<Option<Vec<Rect>>> = const { RefCell::new(None) };
}

/// Declare that `rect`, painted with `transform`, shows transient feedback.
/// Outside a frame being painted (a widget drawn into a thumbnail, a test
/// scene) nothing is recorded.
pub fn declare(transform: Affine, rect: Rect) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let painted = transform.transform_rect_bbox(rect);
    FRAME.with(|frame| {
        if let Some(rects) = frame.borrow_mut().as_mut() {
            rects.push(painted);
        }
    });
}

/// The declarations of one frame being painted, collected from
/// [`Frame::begin`] to [`Frame::finish`]. Every host's frame pass collects
/// through it — the native runner's and the portable frame harness's that
/// the browser and Cloud clients paint with — so a declaration reaches the
/// semantic tree the same way wherever the app is painted. A frame abandoned
/// part-way (dropped) keeps nothing.
#[must_use = "a frame collects until it is finished"]
pub(crate) struct Frame(());

impl Frame {
    /// Start collecting the declarations of one frame.
    pub(crate) fn begin() -> Self {
        FRAME.with(|frame| *frame.borrow_mut() = Some(Vec::new()));
        Self(())
    }

    /// Stop collecting, returning what the frame declared in the logical
    /// coordinates `base` (the frame's composite transform) maps from.
    pub(crate) fn finish(self, base: Affine) -> Vec<Rect> {
        let declared = FRAME
            .with(|frame| frame.borrow_mut().take())
            .unwrap_or_default();
        let back = base.inverse();
        declared
            .into_iter()
            .map(|rect| back.transform_rect_bbox(rect))
            .collect()
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        FRAME.with(|frame| *frame.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_are_kept_only_inside_a_frame_in_logical_coordinates() {
        declare(Affine::IDENTITY, Rect::new(0.0, 0.0, 10.0, 10.0));
        let base = Affine::scale(2.0);
        let frame = Frame::begin();
        declare(
            base.pre_translate((5.0, 5.0).into()),
            Rect::new(0.0, 0.0, 10.0, 4.0),
        );
        declare(base, Rect::new(3.0, 3.0, 3.0, 9.0));
        assert_eq!(frame.finish(base), vec![Rect::new(5.0, 5.0, 15.0, 9.0)]);
        let abandoned = Frame::begin();
        declare(base, Rect::new(0.0, 0.0, 4.0, 4.0));
        drop(abandoned);
        assert!(
            Frame::begin().finish(base).is_empty(),
            "an abandoned frame keeps nothing"
        );
    }
}
