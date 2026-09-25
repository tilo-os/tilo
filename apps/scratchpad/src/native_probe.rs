//! The native input diagnostic Scratchpad reports to performance captures.
//!
//! A raw toolkit widget around the view — the escape hatch for something the
//! framework has no reason to know about. It reads the real document and the
//! shared scroll offset; it never includes the note text or mutates state.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::automerge::{AutoCommit, ObjId};
use ui::prelude::*;
use ui::vello::kurbo;
use ui::winit::event::Modifiers;

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

impl Widget for NativeProbe {
    fn native_input_snapshot(&self, doc: &AutoCommit, _path: &ObjId) -> Option<serde_json::Value> {
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
        let root_rect = self.get_rect();
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

    fn get_rect(&self) -> Rect {
        self.content.get_rect()
    }

    fn update(&mut self, doc: &mut AutoCommit, path: &ObjId) -> bool {
        self.content.update(doc, path)
    }

    fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        self.content.sync(doc, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.content.build_layout(taffy, theme, nodes)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.content.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.content.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.content.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, taffy,
        );
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.content]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.content]
    }
}
