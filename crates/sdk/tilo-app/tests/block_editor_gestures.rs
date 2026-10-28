//! The block editor driven through the production frame host, as a page
//! hosts its body: caret geometry on wrapped rows, keyboard navigation
//! between visual rows, caret suppression under a modal, and the drag
//! handles' hover, preview, drop, edge scroll and cancel.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;

use tilo_app as ui;
use ui::headless::Framed;
use ui::presence::PresenceCtx;
use ui::synthetic_input::{cursor_moved, mouse_press, mouse_release, named_key};
use ui::taffy::prelude::*;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Rect};
use ui::widget::Widget;
use ui::winit;
use ui::winit::event::{Modifiers, WindowEvent};
use ui::winit::keyboard::NamedKey;

/// A text renderer over the checkout's fixture fonts, so geometry does not
/// depend on the machine's.
fn renderer() -> ui::TextRenderer {
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    ui::TextRenderer::with_font_catalog(&catalog).unwrap()
}

/// Mount `editor` in the production frame host `width` wide at its content
/// height, the way a scrolling page lays its body out, its top-left corner
/// at (50, 50), and run its first frame.
fn layout_block_editor(
    editor: ui::widget::BlockEditor,
    width: f32,
) -> Framed<ui::widget::BlockEditor> {
    // Tall enough that the viewport cull never drops a fixture block.
    const HEIGHT: f32 = 10_000.0;
    const INSET: f32 = 50.0;
    Framed::with_options(
        ui::State::new(),
        editor,
        ui::headless::viewport((INSET + width, HEIGHT)),
        ui::headless::FrameOptions {
            theme: Some(Theme::dark()),
            text: Some(renderer()),
            host_style: Some(ui::taffy::Style {
                flex_direction: ui::taffy::FlexDirection::Column,
                padding: ui::taffy::Rect {
                    left: ui::taffy::LengthPercentage::length(INSET),
                    top: ui::taffy::LengthPercentage::length(INSET),
                    right: ui::taffy::LengthPercentage::length(0.0),
                    bottom: ui::taffy::LengthPercentage::length(0.0),
                },
                ..Default::default()
            }),
            content_height: true,
        },
    )
}

fn block_layout_fixture(markdown: &str, width: f32) -> Framed<ui::widget::BlockEditor> {
    layout_block_editor(
        ui::widget::BlockEditor::from_markdown(markdown).with_wrap_lines(true),
        width,
    )
}

fn block_gesture_fixture() -> Framed<ui::widget::BlockEditor> {
    block_layout_fixture("first\n\nsecond\n\nthird\n\nfourth", 400.0)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "test helper owns each synthetic event for immediate dispatch"
)]
fn block_event(editor: &mut Framed<ui::widget::BlockEditor>, event: WindowEvent) {
    Framed::harness_mut(editor).dispatch_to_root_with(&event, &Modifiers::default());
}

/// Where every block's text starts: past the editor's handle column and
/// the line's own inset.
fn content_edge(editor: &Framed<ui::widget::BlockEditor>) -> f64 {
    editor.get_rect().x0 + editor.text_inset(&Theme::dark())
}

/// The x of a drag handle's centre, left of the content edge.
fn handle_x(editor: &Framed<ui::widget::BlockEditor>) -> f64 {
    content_edge(editor) - 14.0
}

/// Run a frame and return the scene it drew.
fn block_scene(editor: &mut Framed<ui::widget::BlockEditor>) -> Scene {
    let harness = Framed::harness_mut(editor);
    harness.relayout();
    harness.scene().clone()
}

#[test]
fn wrapped_block_caret_stays_on_one_baseline_when_moving_or_typing() {
    use ui::widget::RichTextLine;
    for scale in [1.0, 1.5, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut renderer = renderer();
        renderer.set_text_scale(scale);
        let mut line = RichTextLine::new().with_text("abcdef");
        line.set_wrap(true);
        let mut tree = TaffyTree::new();
        line.build_layout(&mut tree, &theme, &mut HashMap::new());
        let size = line.measure(
            Size {
                width: Some(400.0),
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(400.0),
                height: AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        line.apply_layout_external(Rect::new(40.0, 60.0, 440.0, 60.0 + f64::from(size.height)));
        line.set_focus(true);
        let mut baseline: Option<f64> = None;
        for byte in [0, 1, 2, 3, 2, 1, 0, 6] {
            line.set_cursor_pos(byte);
            line.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                &PresenceCtx::empty(),
            );
            let caret = line.caret_rect(&theme, byte);
            if let Some(y) = baseline {
                assert!(
                    (caret.y0 - y).abs() < 0.01,
                    "caret moved vertically at byte {byte}: {caret:?}, expected {y}, scale {scale}"
                );
            }
            baseline = Some(caret.y0);
        }
    }
}

#[test]
fn empty_wrapped_block_reserves_the_same_caret_band_as_first_character() {
    use ui::widget::RichTextLine;
    let theme = Theme::dark();
    let mut renderer = renderer();
    let mut heights = Vec::new();
    let mut carets = Vec::new();
    for value in ["", "a"] {
        let mut line = RichTextLine::new().with_text(value);
        line.set_wrap(true);
        line.set_focus(true);
        line.build_layout(&mut TaffyTree::new(), &theme, &mut HashMap::new());
        let size = line.measure(
            Size {
                width: Some(400.0),
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(400.0),
                height: AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        heights.push(size.height);
        line.apply_layout_external(Rect::new(40.0, 60.0, 440.0, 60.0 + f64::from(size.height)));
        line.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            &PresenceCtx::empty(),
        );
        carets.push(line.caret_rect(&theme, 0));
    }
    assert_eq!(
        heights[0], heights[1],
        "first character must not resize an empty block"
    );
    assert_eq!(
        carets[0], carets[1],
        "empty and populated caret origins must agree"
    );
}

#[test]
fn wrapped_paragraph_arrow_navigation_visits_each_visual_row() {
    let mut editor = block_layout_fixture(
        "A paragraph that needs several visual lines at this narrow width and must keep its caret within the paragraph until its final row.\n\nNext paragraph",
        210.0,
    );
    editor.set_focus(true);
    editor.focus_block(0, ui::widget::CursorPlacement::Start);
    block_scene(&mut editor);
    let initial = editor.blocks()[0].line.caret_rect(&Theme::dark(), 0);
    block_event(&mut editor, named_key(NamedKey::ArrowDown));
    block_scene(&mut editor);
    assert_eq!(
        editor.focused_index(),
        Some(0),
        "Down must visit wrapped rows before the next block"
    );
    let line = &editor.blocks()[0].line;
    let second = line.caret_rect(&Theme::dark(), line.cursor_pos());
    assert!(second.y0 > initial.y0);
    assert_eq!(
        line.inner().scroll_y(),
        0.0,
        "auto-height paragraphs do not scroll internally"
    );
    block_event(&mut editor, named_key(NamedKey::ArrowUp));
    assert_eq!(editor.focused_index(), Some(0));
    assert_eq!(editor.blocks()[0].line.cursor_pos(), 0);
    for _ in 0..20 {
        if editor.focused_index() == Some(1) {
            break;
        }
        block_event(&mut editor, named_key(NamedKey::ArrowDown));
        block_scene(&mut editor);
    }
    assert_eq!(editor.focused_index(), Some(1));
    block_event(&mut editor, named_key(NamedKey::ArrowUp));
    block_scene(&mut editor);
    assert_eq!(editor.focused_index(), Some(0));
    let line = &editor.blocks()[0].line;
    assert!(
        line.caret_rect(&Theme::dark(), line.cursor_pos()).y0 > second.y0,
        "Up enters the preceding paragraph's final visual row"
    );
}

#[test]
fn modal_caret_suppression_preserves_editor_state_and_restores_caret() {
    let mut editor = block_layout_fixture("Editable note", 400.0);
    editor.focus_block(0, ui::widget::CursorPlacement::End);
    editor.set_focus(true);
    let before = (
        editor.blocks()[0].line.cursor_pos(),
        editor.blocks()[0].line.selection_range(),
    );
    let visible = block_scene(&mut editor).encoding().draw_tags.len();
    editor.suppress_caret_paint(true);
    let hidden = block_scene(&mut editor).encoding().draw_tags.len();
    assert_eq!(
        visible,
        hidden + 1,
        "only the insertion caret disappears beneath the modal"
    );
    assert!(editor.is_focused());
    assert_eq!(
        (
            editor.blocks()[0].line.cursor_pos(),
            editor.blocks()[0].line.selection_range()
        ),
        before
    );
    editor.suppress_caret_paint(false);
    assert_eq!(block_scene(&mut editor).encoding().draw_tags.len(), visible);
}

#[test]
fn rich_block_drag_and_scroll_extent_include_the_whole_preview() {
    use ui::widget::{Block, BlockEditor, Mark, MarkSpan};
    let mut embedded = Block::paragraph("Linked note");
    embedded.line.set_marks(vec![MarkSpan::new(
        0..11,
        Mark::Transclusion(ui::widget::marks::PageId::from_u128(1)),
    )]);
    // Pushed one by one: a literal array of three blocks is too large
    // for the stack.
    let mut blocks = Vec::with_capacity(3);
    blocks.push(Block::table(
        "| Key | Value |\n| --- | --- |\n| Name | Test |",
        true,
    ));
    blocks.push(Block::paragraph("![Diagram](diagram.png)"));
    blocks.push(embedded);
    for block in blocks {
        let mut editor = BlockEditor::new().with_wrap_lines(true);
        editor.set_blocks(vec![block]);
        let mut editor = layout_block_editor(editor, 400.0);
        assert!(
            (editor.content_bottom() - editor.get_rect().y1).abs() < 0.01,
            "scroll extent includes complete image/table/embed, not just its source line"
        );
        // The block's handle shows for the pointer over it; grab it there.
        let line = editor.blocks()[0].line.get_rect();
        block_event(&mut editor, cursor_moved(line.center().x, line.center().y));
        let handle = editor
            .hover_handle()
            .expect("the hovered rich block shows its drag handle")
            .center();
        let (x, y) = (handle.x, handle.y);
        block_event(&mut editor, cursor_moved(x, y));
        let idle = block_scene(&mut editor).encoding().draw_tags.len();
        block_event(&mut editor, mouse_press());
        block_event(&mut editor, cursor_moved(x + 30.0, y + 40.0));
        let dragged = block_scene(&mut editor);
        assert!(
            dragged.encoding().draw_tags.len() > idle + 4,
            "floating preview must contain real rich-block content"
        );
    }
}

#[test]
fn backward_selection_at_block_start_deletes_text_without_merging_blocks() {
    let mut editor = block_gesture_fixture();
    editor.set_focus(true);
    editor.focus_block(1, ui::widget::CursorPlacement::Start);
    editor.blocks_mut()[1].line.inner_mut().set_selection(6, 0);
    block_scene(&mut editor);
    block_event(&mut editor, named_key(NamedKey::Backspace));
    assert_eq!(editor.blocks().len(), 4);
    assert_eq!(editor.blocks()[0].text(), "first");
    assert_eq!(editor.blocks()[1].text(), "");
}

#[test]
fn unfocused_block_handles_paint_on_hover_and_clear_on_pointer_exit() {
    let mut editor = block_gesture_fixture();
    let idle = block_scene(&mut editor).encoding().draw_tags.len();
    let row = editor.blocks()[0].line.get_rect();
    let handle = handle_x(&editor);
    block_event(&mut editor, cursor_moved(row.center().x, row.center().y));
    let hovered = block_scene(&mut editor).encoding().draw_tags.len();
    assert_eq!(
        hovered,
        idle + 6,
        "only the hovered block gets six handle dots"
    );
    block_event(&mut editor, cursor_moved(handle, row.center().y));
    assert_eq!(
        block_scene(&mut editor).encoding().draw_tags.len(),
        hovered + 1,
        "handle remains reachable and gains one hover background across its gutter"
    );
    block_event(
        &mut editor,
        WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        },
    );
    assert_eq!(block_scene(&mut editor).encoding().draw_tags.len(), idle);
}

#[test]
fn caret_block_handle_shows_after_pointer_motion_and_follows_focus() {
    let mut editor = block_gesture_fixture();
    editor.suppress_caret_paint(true);
    editor.set_focus(false);
    let idle = block_scene(&mut editor).encoding().draw_tags.len();
    editor.focus_block(1, ui::widget::CursorPlacement::End);
    editor.set_focus(true);
    assert_eq!(
        block_scene(&mut editor).encoding().draw_tags.len(),
        idle,
        "the handles are for the pointer: none shows before it moves"
    );
    let second = editor.blocks()[1].line.get_rect().center();
    block_event(&mut editor, cursor_moved(second.x, second.y));
    block_event(
        &mut editor,
        WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        },
    );
    assert_eq!(
        block_scene(&mut editor).encoding().draw_tags.len(),
        idle + 6,
        "once the pointer has moved, the caret block keeps its handle away from it"
    );
    let first = editor.blocks()[0].line.get_rect().center();
    block_event(&mut editor, cursor_moved(first.x, first.y));
    assert_eq!(
        block_scene(&mut editor).encoding().draw_tags.len(),
        idle + 12,
        "a different hovered block also gets a handle"
    );
    editor.focus_block(0, ui::widget::CursorPlacement::End);
    assert_eq!(
        block_scene(&mut editor).encoding().draw_tags.len(),
        idle + 6,
        "moving the caret removes the old block handle without duplicating the hovered handle"
    );
    block_event(
        &mut editor,
        WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        },
    );
    editor.set_focus(false);
    assert_eq!(
        block_scene(&mut editor).encoding().draw_tags.len(),
        idle,
        "leaving the editor hides its caret handle"
    );
}

#[test]
fn block_drag_previews_motion_and_drops_in_the_nearby_gap() {
    let mut editor = block_gesture_fixture();
    let first = editor.blocks()[0].line.get_rect();
    let second = editor.blocks()[1].line.get_rect();
    let third = editor.blocks()[2].line.get_rect();
    let handle = handle_x(&editor);
    block_event(&mut editor, cursor_moved(handle, first.center().y));
    block_event(&mut editor, mouse_press());
    let before = block_scene(&mut editor).encoding().draw_tags.len();
    let gap = f64::midpoint(second.y0, first.y1);
    block_event(&mut editor, cursor_moved(handle, gap));
    let preview = block_scene(&mut editor);
    assert!(
        preview.encoding().draw_tags.len() > before + 2,
        "drag must paint the source text preview, not just an insertion rule"
    );
    let mut marker_brush = Scene::new();
    marker_brush.fill(
        ui::vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        Theme::dark().tokens.colors.intent.you,
        None,
        &Rect::new(0.0, 0.0, 1.0, 1.0),
    );
    assert!(
        preview.encoding().draw_tags.last() == marker_brush.encoding().draw_tags.last()
            && preview
                .encoding()
                .draw_data
                .ends_with(&marker_brush.encoding().draw_data),
        "the insertion marker must remain visible above the opaque drag preview"
    );
    let transforms = format!("{:?}", preview.encoding().transforms);
    block_event(&mut editor, cursor_moved(handle, gap + 2.0));
    assert_ne!(
        transforms,
        format!("{:?}", block_scene(&mut editor).encoding().transforms),
        "preview follows motion within the same target"
    );
    assert_eq!(
        editor.blocks()[0].text(),
        "first",
        "preview must not mutate document order"
    );
    block_event(&mut editor, mouse_release());
    assert_eq!(
        editor
            .blocks()
            .iter()
            .map(ui::widget::Block::text)
            .collect::<Vec<_>>(),
        ["second", "first", "third", "fourth"]
    );
    assert!(third.y0 > second.y1);
}

#[test]
fn list_subtree_drop_marker_uses_parent_indentation_after_last_descendant() {
    use ui::widget::{Block, BlockEditor};
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::numbered(0, "Parent A"),
        Block::bullet(1, "Child A"),
        Block::numbered(0, "Parent B"),
        Block::bullet(1, "Child B"),
    ]);
    let mut editor = layout_block_editor(editor, 400.0);
    let root_x = content_edge(&editor);
    let handle = handle_x(&editor);
    let first = editor.blocks()[0].line.get_rect();
    let last = editor.blocks()[3].line.get_rect();
    block_event(&mut editor, cursor_moved(handle, first.center().y));
    block_event(&mut editor, mouse_press());
    block_event(&mut editor, cursor_moved(handle, last.center().y));
    let scene = block_scene(&mut editor);
    let mut expected_marker = Scene::new();
    expected_marker.fill(
        ui::vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        Theme::dark().tokens.colors.intent.you,
        None,
        &Rect::new(root_x - 4.0, last.y1 - 1.0, last.x1, last.y1 + 1.0),
    );
    assert!(
        scene
            .encoding()
            .path_data
            .ends_with(&expected_marker.encoding().path_data),
        "the final drop marker belongs at root depth, below the destination's child"
    );
}

#[test]
fn block_drag_near_the_visible_edge_asks_the_host_to_scroll() {
    let mut editor = block_gesture_fixture();
    let handle = handle_x(&editor);
    let first = editor.blocks()[0].line.get_rect();
    let viewport = Rect::new(0.0, 0.0, 400.0, 600.0);
    let frame = std::time::Duration::from_millis(100);
    // No drag, no scroll.
    assert!(editor.tick_block_drag(viewport, frame).abs() < f64::EPSILON);
    block_event(&mut editor, cursor_moved(handle, first.center().y));
    block_event(&mut editor, mouse_press());
    // Pressed but not yet a drag.
    assert!(editor.tick_block_drag(viewport, frame).abs() < f64::EPSILON);
    // Mid-viewport: a drag that does not scroll.
    block_event(&mut editor, cursor_moved(handle, 300.0));
    assert!(editor.block_drag_active());
    assert!(editor.tick_block_drag(viewport, frame).abs() < f64::EPSILON);
    // In the bottom edge band: scrolls down, faster closer to the edge.
    block_event(&mut editor, cursor_moved(handle, 575.0));
    let slow = editor.tick_block_drag(viewport, frame);
    block_event(&mut editor, cursor_moved(handle, 599.0));
    let fast = editor.tick_block_drag(viewport, frame);
    assert!(slow > 0.0 && fast > slow, "{slow} then {fast}");
    // In the top edge band: scrolls up.
    block_event(&mut editor, cursor_moved(handle, 5.0));
    assert!(editor.tick_block_drag(viewport, frame) < 0.0);
    block_event(&mut editor, named_key(NamedKey::Escape));
    assert!(!editor.block_drag_active());
    assert!(editor.tick_block_drag(viewport, frame).abs() < f64::EPSILON);
}

#[test]
fn block_drag_escape_and_focus_loss_preserve_order() {
    for cancel in [named_key(NamedKey::Escape), WindowEvent::Focused(false)] {
        let mut editor = block_gesture_fixture();
        let first = editor.blocks()[0].line.get_rect();
        let last = editor.blocks()[3].line.get_rect();
        let handle = handle_x(&editor);
        block_event(&mut editor, cursor_moved(handle, first.center().y));
        block_event(&mut editor, mouse_press());
        block_event(&mut editor, cursor_moved(handle, last.center().y));
        block_scene(&mut editor);
        block_event(&mut editor, cancel);
        block_event(&mut editor, mouse_release());
        assert_eq!(
            editor
                .blocks()
                .iter()
                .map(ui::widget::Block::text)
                .collect::<Vec<_>>(),
            ["first", "second", "third", "fourth"]
        );
    }
}
