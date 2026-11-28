//! A long note in a window: what typing and selecting in it repaint.

use std::collections::HashMap;

use tilo_rich_edit::BlockEditor;
use tilo_ui::{widget::AppKind, widget::Scrollable};
use tilo_ui_app::headless::Driver;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{
    widget::AppModel, widget::EventCtx, widget::ModelPath, widget::ScrollAxis, widget::Widget,
};
use tilo_ui_model::{
    widget::AppWidget, widget::IconDescriptor, widget::TileContent, widget::TileHost,
};
use tilo_ui_render::retained_raster::{Repaint, RetainedFrames, TargetKey};
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

const SIZE: (u32, u32) = (1200, 800);

fn key(harness: &Driver, (width, height): (u32, u32), generation: u64) -> TargetKey {
    TargetKey {
        width,
        height,
        to_device: Affine::IDENTITY,
        base_color: harness.frame_render_input().background,
        generation,
    }
}

fn present(harness: &mut Driver, frames: &mut RetainedFrames, key: TargetKey) -> Repaint {
    harness.poll_frame().unwrap();
    let scene = harness.frame_render_input().scene;
    let repaint = frames.repaint(scene, harness.scene_parts(), key);
    frames.presented(scene, harness.scene_parts(), key);
    repaint
}

/// An app window holding one scrolled page.
struct Note {
    page: Scrollable,
}

impl Widget for Note {
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        self.page.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.page.build_layout(tree, theme, nodes)
    }
    fn apply_layout(&mut self, tree: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.page.apply_layout(tree, node, origin);
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.page.draw(&mut tilo_ui_base::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.page.handle_event(event, ctx);
    }
    fn get_rect(&self) -> Rect {
        self.page.get_rect()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.page]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.page]
    }
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.page.collect_damage(regions);
    }
    fn name(&self) -> &str {
        "Note"
    }
}

impl AppWidget for Note {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Note"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("T")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

/// A note of eight paragraphs in a window, drawn once with its scene parts tracked, and where its
/// editor is.
fn note_window() -> (Driver, RetainedFrames, TargetKey, Rect) {
    let markdown = (0..8)
        .map(|i| format!("Paragraph number {i} of the note."))
        .collect::<Vec<_>>()
        .join("\n\n");
    let note = Note {
        page: Scrollable::new(ScrollAxis::Vertical)
            .with_child(Box::new(BlockEditor::from_markdown(&markdown))),
    };
    let mut harness = Driver::for_root(
        tilo_ui_model::State::new(),
        TileHost::new(TileContent::App(Box::new(note))),
        SIZE,
        Theme::dark(),
    );
    harness.track_scene_parts(true);
    harness.set_coalesce_input_frames(true);
    harness.relayout();
    let mut frames = RetainedFrames::default();
    let key = key(&harness, SIZE, 0);
    present(&mut harness, &mut frames, key);
    let editor = harness
        .targets()
        .into_iter()
        .find(|(key, _)| key == "block_editor")
        .map(|(_, rect)| rect)
        .expect("the note's editor");
    assert!(editor.height() > 200.0, "the note is long: {editor:?}");
    (harness, frames, key, editor)
}

/// Each region of `repaint` is a few lines high, not the editor.
fn assert_lines(what: &str, repaint: Repaint, editor: Rect) {
    let Repaint::Regions(regions) = repaint else {
        panic!("{what} repaints the whole frame");
    };
    assert!(!regions.is_empty(), "{what} repaints nothing");
    for region in &regions {
        assert!(
            region.height() < 120.0,
            "{what} repainted {region:?}, more than a few lines of the editor {editor:?}"
        );
    }
}

/// A new paragraph at the end of a note grows its editor by a line: the tile repaints that line and the
/// caret, not the whole editor, whose own damage says where its blocks were and are.
#[test]
fn a_new_paragraph_repaints_its_line_not_the_editor() {
    let (mut harness, mut frames, key, editor) = note_window();
    harness.click_at(editor.x0 + 40.0, editor.y0 + 12.0);
    for _ in 0..8 {
        harness.key("ArrowDown");
    }
    harness.key("End");
    present(&mut harness, &mut frames, key);
    assert!(harness.ime_window().is_some(), "the click focuses the note");
    for (what, step) in [("Enter", "Enter"), ("a letter", "x"), ("another", "y")] {
        harness.key(step);
        let repaint = present(&mut harness, &mut frames, key);
        assert_lines(what, repaint, editor);
    }
}

/// A selection dragged down a note, within a paragraph and then across paragraphs, repaints the rows
/// whose selection changed with each move, not the editor.
#[test]
fn a_dragged_selection_repaints_the_rows_it_changed() {
    let (mut harness, mut frames, key, editor) = note_window();
    let scale = harness.input_scale();
    let move_to = |harness: &mut Driver, x: f64, y: f64| {
        harness.send(&tilo_ui_base::synthetic_input::cursor_moved(
            x * scale,
            y * scale,
        ));
    };
    let (x, y) = (editor.x0 + 40.0, editor.y0 + 12.0);
    move_to(&mut harness, x, y);
    harness.send(&tilo_ui_base::synthetic_input::mouse_press());
    present(&mut harness, &mut frames, key);
    move_to(&mut harness, x + 60.0, y);
    let repaint = present(&mut harness, &mut frames, key);
    assert_lines("a drag along the paragraph", repaint, editor);
    for step in 1..5 {
        move_to(&mut harness, x + 60.0, y + 30.0 * f64::from(step));
        let repaint = present(&mut harness, &mut frames, key);
        assert_lines("a drag across the paragraphs", repaint, editor);
    }
    harness.send(&tilo_ui_base::synthetic_input::mouse_release());
}
