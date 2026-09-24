//! A small native application exercising UI, FS and actions together.

pub mod actions;
mod native_fixture;
pub use native_fixture::{NativeFixtureManifest, seed_native_fixture};
mod storage;

use actions::EDIT;
pub use actions::{EditNote, register};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::Path,
    rc::Rc,
};
use taffy::{NodeId, TaffyTree};
use tilo_actions::{
    ActionBinding, ActionSpec, EncodedAction, ExecutionLocus, OperationKind, Resource,
};
use ui::widget::{Button, Column, Row, Scrollable, Text, TextInput, Widget};
use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
use vello::{
    Scene,
    kurbo::{Affine, Point},
};
use winit::event::{Modifiers, WindowEvent};

const SAVE: &str = "so.tilo.scratchpad.save";

fn body(doc: &AutoCommit) -> String {
    doc.get(ROOT, "body")
        .ok()
        .flatten()
        .and_then(|(value, id)| match value {
            Value::Object(ObjType::Text) => doc.text(id).ok(),
            value => value.as_str().map(str::to_owned),
        })
        .unwrap_or_default()
}

#[expect(
    clippy::expect_used,
    reason = "a struct containing only a String always serializes"
)]
fn edit_spec() -> ActionSpec {
    ActionSpec::prompted(
        "edit",
        "Edit note",
        "Note text",
        "Write a note…",
        ExecutionLocus::ClientLocal,
        |text| {
            EncodedAction::encode(EDIT, &EditNote { text: text.into() }).expect("serialize text")
        },
    )
}

/// Open the app's state and the same retained tree used by every runner.
///
/// # Errors
/// Returns storage, locking, malformed-document or action-binding errors.
pub fn open(directory: &Path) -> anyhow::Result<(ui::State, Box<dyn Widget>)> {
    let (store, doc) = storage::NoteStore::open(directory)?;
    if let Some((value, _)) = doc.get(ROOT, "body")? {
        anyhow::ensure!(
            value.as_str().is_some() || matches!(value, Value::Object(ObjType::Text)),
            "saved note body is not text"
        );
    }
    let saved = body(&doc);
    let mut state = ui::State::new();
    anyhow::ensure!(state.adopt_doc(doc), "could not adopt saved note");
    let edit = edit_spec();
    let resource = Resource::new("scratchpad", "note", "Scratchpad")
        .with_action(edit.clone())
        .with_action(ActionSpec::operation(
            "save",
            "Save note",
            OperationKind::Effect,
            SAVE,
            &(),
        )?);
    let binding = ActionBinding::new(&resource, &edit, |text: String| {
        serde_json::Map::from_iter([("value".into(), serde_json::Value::String(text))])
    })?;
    let status = Rc::new(RefCell::new(String::new()));
    let status_source = Rc::clone(&status);
    let editor = TextInput::from_document_string("body")
        .with_id("note-editor")
        .with_placeholder("A thought worth keeping…")
        .with_font_size(18.0)
        .multiline(true)
        .on_action(binding);
    let scroll_offset = Rc::new(Cell::new((0.0, 0.0)));
    let content = Column::new()
        .with_horizontal_padding(32.0, 32.0)
        .with_vertical_padding(28.0)
        .with_gap(18.0)
        .push(Box::new(
            Text::new("Scratchpad")
                .with_size(30.0)
                .with_heading_level(1),
        ))
        .push(Box::new(
            Text::new("A little space for your next idea.").dimmed(),
        ))
        .push(Box::new(
            Column::new()
                .with_no_padding()
                .with_fill_remaining_height()
                .push(Box::new(
                    Scrollable::new_vertical()
                        .with_persisted_offset(Rc::clone(&scroll_offset))
                        .fill_parent_height()
                        .with_content_min_viewport_height()
                        .with_child(Box::new(editor)),
                )),
        ))
        .push(Box::new(
            Row::new()
                .push(Box::new(
                    Button::new("save-note", "Save", None)
                        .on_action(ActionBinding::unit_for(&resource, "save")?),
                ))
                .push(Box::new(
                    Text::dynamic(move || status_source.borrow().clone())
                        .with_id("save-status")
                        .with_live(ui::a11y::Politeness::Polite),
                )),
        ))
        .push(Box::new(
            Text::new("Save before closing to keep your changes.")
                .with_size(12.0)
                .dimmed(),
        ));
    Ok((
        state,
        Box::new(Scratchpad {
            content,
            scroll_offset,
            store,
            resource,
            status,
            saved,
            error: None,
        }),
    ))
}

struct Scratchpad {
    content: Column,
    scroll_offset: Rc<Cell<(f32, f32)>>,
    store: storage::NoteStore,
    resource: Resource,
    status: Rc<RefCell<String>>,
    saved: String,
    error: Option<String>,
}

impl Widget for Scratchpad {
    fn native_input_snapshot(&self, doc: &AutoCommit, _path: &ObjId) -> Option<serde_json::Value> {
        fn rect(rect: vello::kurbo::Rect) -> [f64; 4] {
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
        let body = body(doc);
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
        let (offset_x, offset_y) = self.scroll_offset.get();
        Some(serde_json::json!({
            "app": "so.tilo.scratchpad",
            "editor_id": "note-editor",
            "editor_focused": editor_focused,
            "body_hash": iroh_blobs::Hash::new(body.as_bytes()).to_hex(),
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
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.content.get_rect()
    }
    fn update(&mut self, doc: &mut AutoCommit, path: &ObjId) -> bool {
        self.content.update(doc, path)
    }
    fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        *self.status.borrow_mut() = if let Some(error) = &self.error {
            format!("Save failed: {error}")
        } else if body(doc) != self.saved {
            "Unsaved changes".into()
        } else {
            "Saved locally".into()
        };
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.bg_app,
            None,
            &self.get_rect(),
        );
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
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        vec![self.resource.clone()]
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[SAVE]
    }
    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != OperationKind::Effect || type_id != SAVE {
            return Ok(false);
        }
        serde_json::from_slice::<()>(payload).map_err(|e| e.to_string())?;
        match self.store.save(doc) {
            Ok(()) => {
                self.saved = body(doc);
                self.error = None;
                Ok(true)
            }
            Err(error) => {
                let message = format!("{error:#}");
                self.error = Some(message.clone());
                Err(message)
            }
        }
    }
}
