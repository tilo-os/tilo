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

/// Start collecting the declarations of one frame. The app's frame pass
/// (`draw_frame_content`) runs it; a host without that pass collects nothing.
#[cfg(any(feature = "native", test))]
pub(crate) fn begin_frame() {
    FRAME.with(|frame| *frame.borrow_mut() = Some(Vec::new()));
}

/// Stop collecting, returning what the frame declared in the logical
/// coordinates `base` (the frame's composite transform) maps from.
#[cfg(any(feature = "native", test))]
pub(crate) fn end_frame(base: Affine) -> Vec<Rect> {
    let declared = FRAME
        .with(|frame| frame.borrow_mut().take())
        .unwrap_or_default();
    let back = base.inverse();
    declared
        .into_iter()
        .map(|rect| back.transform_rect_bbox(rect))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_are_kept_only_inside_a_frame_in_logical_coordinates() {
        declare(Affine::IDENTITY, Rect::new(0.0, 0.0, 10.0, 10.0));
        let base = Affine::scale(2.0);
        begin_frame();
        declare(
            base.pre_translate((5.0, 5.0).into()),
            Rect::new(0.0, 0.0, 10.0, 4.0),
        );
        declare(base, Rect::new(3.0, 3.0, 3.0, 9.0));
        assert_eq!(end_frame(base), vec![Rect::new(5.0, 5.0, 15.0, 9.0)]);
        assert!(end_frame(base).is_empty(), "the frame is over");
    }
}
