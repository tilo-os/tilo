//! Scroll the real installer body until a control is inside its viewport.
use super::*;

/// The page body's scrolling viewport, which is smaller than the window:
/// the header sits above it and the actions row below it.
fn body_viewport(root: &mut dyn Widget) -> Option<Rect> {
    // `Scrollable` exposes itself only through the mutable downcast, so the
    // search walks the tree mutably even though it changes nothing.
    if let Some(rect) = root
        .as_any_mut()
        .and_then(|widget| widget.downcast_mut::<ui::widget::Scrollable>())
        .map(|scroll| scroll.content_viewport_rect())
    {
        return Some(rect);
    }
    root.children_mut().into_iter().find_map(body_viewport)
}

/// Scroll the real body until `id` is inside the viewport. A control below
/// the fold is reachable with the same wheel input a person uses, never by
/// lowering the assertion or editing its rectangle.
pub(in crate::setup::journey) fn scroll_into_view<A: ui::framework::ResourceApp>(
    app: &mut TestApp<A>,
    id: &str,
) {
    // A wheel carries no position: it acts on the last pointer position, which
    // must be inside the scrolling body. The body's top-left corner is the
    // page heading, so the click focuses the body at every host scale without
    // choosing, expanding or scrolling anything.
    let pointer = app
        .with_widget::<InstallerPreview, _>(|preview| {
            let viewport = body_viewport(preview).unwrap_or(preview.rect);
            (viewport.x0 + 4.0, viewport.y0 + 4.0)
        })
        .unwrap();
    app.harness().click_at(pointer.0, pointer.1);
    for _ in 0..80 {
        app.settle();
        let found = app
            .with_widget::<InstallerPreview, _>(|preview| {
                control(preview, id).map(|control| (control.get_rect(), preview.rect.y1))
            })
            .flatten();
        let Some((rect, viewport_bottom)) = found else {
            panic!("missing {id}");
        };
        if rect.y0 >= 0.0 && rect.y1 <= viewport_bottom {
            return;
        }
        app.harness().scroll(0.0, -20.0);
        app.settle();
    }
    panic!("{id} never scrolled into view");
}
