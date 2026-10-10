//! Read-only native diagnostic state, reached only by the opt-in UI observer.
use super::FileExplorerWidget;
use ui::widget::Widget;
use vello::kurbo::Rect;

fn rect(value: Rect) -> [f64; 4] {
    [value.x0, value.y0, value.x1, value.y1]
}

/// Carry the nearest Scrollable ancestor (the grid's), avoiding the independent
/// sidebar's viewport. The list is a table that scrolls itself, so it has no
/// such ancestor and is its own viewport. No downcast, mutation, widget
/// pointer, or thumbnail request.
fn file_geometry(widget: &dyn Widget, viewport: Option<Rect>) -> Option<(Rect, Rect)> {
    let viewport = if widget.name() == "Scrollable" {
        Some(widget.get_rect())
    } else {
        viewport
    };
    if widget.test_id() == Some("file_view") {
        let content = widget.get_rect();
        return Some((content, viewport.unwrap_or(content)));
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = file_geometry(child, viewport);
        }
    });
    found
}

pub(super) fn snapshot(widget: &FileExplorerWidget) -> serde_json::Value {
    let list = widget.view_mode == crate::model::ViewMode::List;
    let scroll = f64::from(widget.list_slot.scroll_y());
    let geometry = file_geometry(widget, None).map(|(content, viewport)| {
        if list {
            // The list's viewport is its table, which ends above the status
            // lane; its content is the view as if unscrolled, so (as for the
            // grid under its Scrollable) the content's top moves up by the
            // offset.
            let extent = f64::from(widget.list_slot.max_scroll_y());
            (
                Rect::new(
                    content.x0,
                    content.y0 - scroll,
                    content.x1,
                    content.y1 - scroll + extent,
                ),
                widget.list_slot.rect(),
            )
        } else {
            (content, viewport)
        }
    });
    let root = widget.get_rect();
    let valid = |rect: Rect| rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0;
    let ready = !widget.is_loading
        && widget.error.is_none()
        && valid(root)
        && geometry.is_some_and(|(content, viewport)| valid(content) && valid(viewport));
    let selected = widget.selection.selected();
    let selected_count = selected.len();
    let copy_refused = widget
        .dispatcher
        .as_ref()
        .and_then(|dispatcher| dispatcher.refused().get())
        .is_some_and(|refused| refused.action == crate::effects::FilesEffect::COPY_SELECTION.id);
    let names: Vec<&str> = selected
        .iter()
        .filter_map(|&index| widget.entries.get(index))
        .map(|entry| entry.name.as_str())
        .collect();
    let (offset_x, offset_y) = if list {
        (0.0, widget.list_slot.scroll_y())
    } else {
        widget.scroll_offset.get()
    };
    let (
        clipboard_phase,
        clipboard_source_present,
        clipboard_ever_active,
        clipboard_ever_source_present,
    ) = widget.local_clipboard.diagnostic_phase();
    serde_json::json!({
        "app": "so.tilo.file-explorer",
        "view_id": "file_view",
        "directory": widget.current_path.to_string_lossy(),
        "entry_count": widget.entries.len(),
        "order_hash": crate::native_fixture::order_hash(&widget.entries),
        "hash_algorithm": "sha256",
        "ready": ready,
        "is_loading": widget.is_loading,
        "has_error": widget.error.is_some(),
        "thumbnail_pending": widget.thumbs.as_ref().map(|service| service.pending()),
        "selected_indices": selected,
        "selected_names": names,
        "selected_count": selected_count,
        "primary_index": widget.selected_index(),
        "offset_x": offset_x,
        "offset_y": offset_y,
        "offset_units": "pixels",
        "last_cursor": [widget.last_cursor.x, widget.last_cursor.y],
        "root_rect": rect(root),
        "viewport_rect": geometry.map(|(_, viewport)| rect(viewport)),
        "content_rect": geometry.map(|(content, _)| rect(content)),
        "view_mode": format!("{:?}", widget.view_mode),
        "sort_by": format!("{:?}", widget.sort_by),
        "sort_order": format!("{:?}", widget.sort_order),
        "clipboard_phase": clipboard_phase,
        "clipboard_source_present": clipboard_source_present,
        "clipboard_ever_active": clipboard_ever_active,
        "clipboard_ever_source_present": clipboard_ever_source_present,
        "copy_control": {
            "dispatcher_attached": widget.dispatcher.is_some(),
            "hosted_clipboard": widget.hosted_clipboard(),
            "effect_available": widget.hosted_clipboard() && selected_count != 0,
            "refusal_present": widget.dispatcher.as_ref()
                .and_then(|dispatcher| dispatcher.refused().get()).is_some(),
            "copy_refused": copy_refused,
            "result_class": widget.copy_control_result,
        },
    })
}
