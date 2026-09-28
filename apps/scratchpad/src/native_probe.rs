//! The native input diagnostic Scratchpad reports to performance captures.
//!
//! A toolkit decorator around the view — the escape hatch for something the
//! framework has no reason to know about. It adds one hook and forwards
//! everything else to the view. It reads the real document and the shared
//! scroll offset; it never includes the note text or mutates state.

use std::cell::Cell;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use ui::prelude::*;
use ui::vello::kurbo;

pub(crate) struct NativeProbe {
    content: Column,
    scroll: Rc<Cell<(f32, f32)>>,
}

impl NativeProbe {
    pub(crate) fn new(content: Column, scroll: Rc<Cell<(f32, f32)>>) -> Self {
        Self { content, scroll }
    }
}

fn rect(rect: kurbo::Rect) -> [f64; 4] {
    [rect.x0, rect.y0, rect.x1, rect.y1]
}

fn find(
    widget: &dyn Widget,
    editor: &mut Option<[f64; 4]>,
    scroll: &mut Option<[f64; 4]>,
    focused: &mut bool,
) {
    if widget.test_id() == Some("note-editor") {
        *editor = Some(rect(widget.get_rect()));
        *focused = widget.is_focused();
    }
    if widget.name() == "Scrollable" {
        *scroll = Some(rect(widget.get_rect()));
    }
    widget.for_each_child(&mut |child| find(child, editor, scroll, focused));
}

impl ui::widget::Decorator for NativeProbe {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.content
    }

    fn native_input_snapshot(&self, doc: &AppModel, path: &ModelPath) -> Option<serde_json::Value> {
        let (doc, _) = ui::doc::document_ref(doc, path);
        let body = <crate::Note as Resource>::read(doc)
            .map(|note| note.body)
            .unwrap_or_default();
        let (mut editor_rect, mut scroll_rect) = (None, None);
        let mut editor_focused = false;
        find(
            &self.content,
            &mut editor_rect,
            &mut scroll_rect,
            &mut editor_focused,
        );
        let root_rect = self.content.get_rect();
        let ready = root_rect.width() > 0.0
            && root_rect.height() > 0.0
            && editor_rect.is_some_and(|[x0, y0, x1, y1]| x1 > x0 && y1 > y0)
            && scroll_rect.is_some_and(|[x0, y0, x1, y1]| x1 > x0 && y1 > y0);
        let (offset_x, offset_y) = self.scroll.get();
        Some(serde_json::json!({
            "app": "so.tilo.scratchpad",
            "editor_id": "note-editor",
            "editor_focused": editor_focused,
            "body_hash": blake3::hash(body.as_bytes()).to_hex().to_string(),
            "body_bytes": body.len(),
            "offset_x": offset_x,
            "offset_y": offset_y,
            "root_rect": rect(root_rect),
            "editor_rect": editor_rect,
            "scroll_rect": scroll_rect,
            "ready": ready,
        }))
    }

    fn name(&self) -> &'static str {
        "Scratchpad"
    }
}
