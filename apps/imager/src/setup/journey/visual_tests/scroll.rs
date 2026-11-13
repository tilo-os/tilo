//! Scroll the real installer body until a control is inside its viewport.

use super::*;

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

pub(in crate::setup::journey) fn scroll_into_view<A: ui::framework::ResourceApp>(
    app: &mut TestApp<A>,
    id: &str,
) {
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
