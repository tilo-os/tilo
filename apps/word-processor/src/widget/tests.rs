use super::accessibility::review_action_target;
use super::block_layout::{line_spacing as block_line_spacing, line_spacing_label, list_marker_x};
use super::image_layout::{
    cropped_source as cropped_image_source, paint_transform as image_paint_transform,
};
use super::text_runs::{
    block_change_decos, block_comment_ranges, block_note_refs, block_runs, composed_block,
    note_numbering, note_text, running_head_runs,
};
use super::toolbar::{HIGHLIGHT_PALETTE, STYLE_PALETTE};
use super::window_bar::BarCommand;
use super::*;
use crate::compact_swatch_palette::PaletteKind;
use crate::effects::WordProcessorEffect;
use crate::file_menu::FileChoice;
use crate::style_menu::StyleMenuAction;
use tilo_test_env::SpawnUnderEnvLock as _;
use ui::{PresenceCtx, clipboard_paste};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};
fn edits(doc: &mut automerge::AutoCommit) -> ui::widget::Edits<'_> {
    ui::widget::Edits::lend(doc)
}
fn seed_text(wp: &mut Editor, state: &mut ui::State, text: &str) {
    wp.model_mut().insert_str(&mut edits(&mut state.doc), text);
}
use std::fmt::Write as _;
use ui::headless::Framed;
use ui::testing::Dispatch;

mod harness;
mod images;

pub(super) use harness::{
    Lent, editor, mount, mount_themed, on_editor, pipeline_draw, pipeline_draw_themed, split,
};

pub(super) const TEST_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2, 0,
    0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 159, 1,
    140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
    130,
];

#[test]
fn images_and_comment_threads_stay_inside_their_frames() {
    {
        let frame = Rect::new(10.0, 20.0, 110.0, 70.0);
        let source = cropped_image_source(frame, (0.1, 0.2, 0.3, 0.1));

        assert!((source.x0 - (-6.666_666_666_666_668)).abs() < 1e-9);
        assert!((source.y0 - 5.714_285_714_285_713).abs() < 1e-9);
        assert!((source.x1 - 160.0).abs() < 1e-9);
        assert!((source.y1 - 77.142_857_142_857_14).abs() < 1e-9);
        assert_eq!(cropped_image_source(frame, (0.0, 0.0, 0.0, 0.0)), frame);
    }

    {
        let frame = Rect::new(10.0, 20.0, 110.0, 70.0);
        let horizontal = image_paint_transform(
            Affine::IDENTITY,
            frame,
            ImagePaintOptions {
                flip_horizontal: true,
                ..ImagePaintOptions::default()
            },
        );
        let vertical = image_paint_transform(
            Affine::IDENTITY,
            frame,
            ImagePaintOptions {
                flip_vertical: true,
                ..ImagePaintOptions::default()
            },
        );

        assert_eq!(
            horizontal * Point::new(frame.x0, frame.y0),
            Point::new(frame.x1, frame.y0)
        );
        assert_eq!(
            vertical * Point::new(frame.x0, frame.y0),
            Point::new(frame.x0, frame.y1)
        );
        let center = Point::new(frame.center().x, frame.center().y);
        assert_eq!(horizontal * center, center);
        assert_eq!(vertical * center, center);
    }

    {
        let host = Rect::new(0.0, 0.0, 300.0, 480.0);
        let layout = CommentThreadLayout::for_host(host, 1.0);

        assert_eq!(layout.panel, Rect::new(12.0, 82.0, 288.0, 442.0));
        for control in [
            layout.reply_field,
            layout.cancel_button,
            layout.submit_button,
        ] {
            assert!(layout.panel.contains(Point::new(control.x0, control.y0)));
            assert!(layout.panel.contains(Point::new(control.x1, control.y1)));
        }
        assert!(layout.cancel_button.x1 < layout.submit_button.x0);
        assert!(layout.reply_field.y1 < layout.submit_button.y0);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn native_edits_keep_views_provenance_and_the_real_caret_text() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "fixture é");
        wp.scroll_y = 144.0;
        wp.max_scroll = 2000.0;
        let heads = state.doc.get_heads();
        let caret = wp.model().caret().clone();
        let dirty = wp.lifecycle.dirty;
        let snapshot = wp.native_input_snapshot(&state.doc, &state.root);
        assert_eq!(snapshot["offset_y"], 144.0);
        assert_eq!(snapshot["max_scroll"], 2000.0);
        assert_eq!(snapshot["caret_text"], "fixture é");
        assert_eq!(snapshot["caret_offset"], 9);
        assert_eq!(snapshot["caret_block"], caret.block);
        assert_eq!(snapshot["caret_region"], "body");
        assert_eq!(snapshot["has_selection"], false);
        assert_eq!(state.doc.get_heads(), heads);
        assert_eq!(wp.model().caret(), &caret);
        assert_eq!(wp.lifecycle.dirty, dirty);
        assert_eq!(wp.scroll_y, 144.0);
        wp.inner.region = Region::Header;
        let header = wp.native_input_snapshot(&state.doc, &state.root);
        assert_eq!(header["caret_region"], "header");
        assert!(
            header["caret_text"].is_null(),
            "running regions must not materialize text for diagnostics"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.inner
            .insert_str(&mut edits(&mut state.doc), "Untouched paragraph");
        wp.inner.enter(&mut edits(&mut state.doc));
        wp.inner
            .insert_str(&mut edits(&mut state.doc), "Edited paragraph");
        wp.refresh_blocks(&state.doc);
        let untouched = wp.blocks[0].text.as_ptr();
        for (key, text) in [
            (Key::Character("é".into()), Some("é")),
            (Key::Named(NamedKey::Backspace), None),
        ] {
            let mut handled = false;
            wp.handle_key_press(
                &mut edits(&mut state.doc),
                &key,
                text,
                false,
                false,
                &mut handled,
            );
            assert!(handled);
            assert_eq!(
                wp.blocks[0].text.as_ptr(),
                untouched,
                "text-only edits must retain unrelated block views"
            );
            let c = wp.inner.containers.as_ref().unwrap();
            assert_eq!(
                wp.blocks,
                tilo_doc::model::materialize(&state.doc, c).blocks
            );
        }
        let first = wp.blocks[0].id.clone();
        let caret = wp.inner.caret().clone();
        wp.inner.set_caret(Caret {
            block: first,
            offset: 0,
        });
        wp.inner.insert_str(&mut edits(&mut state.doc), "External ");
        wp.inner.set_caret(caret);
        wp.handle_key_press(
            &mut edits(&mut state.doc),
            &Key::Character("x".into()),
            Some("x"),
            false,
            false,
            &mut false,
        );
        assert!(wp.blocks[0].text.starts_with("External "));
        wp.handle_key_press(
            &mut edits(&mut state.doc),
            &Key::Named(NamedKey::Enter),
            None,
            false,
            false,
            &mut false,
        );
        let c = wp.inner.containers.as_ref().unwrap();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize(&state.doc, c).blocks
        );
        assert_eq!(wp.blocks.len(), 3);
        wp.handle_key_press(
            &mut edits(&mut state.doc),
            &Key::Named(NamedKey::Backspace),
            None,
            false,
            false,
            &mut false,
        );
        assert_eq!(wp.blocks.len(), 2, "structural backspace merges blocks");
        wp.inner.set_caret(Caret {
            block: wp.blocks[0].id.clone(),
            offset: 0,
        });
        wp.inner.set_anchor();
        wp.inner.set_caret(Caret {
            block: wp.blocks[1].id.clone(),
            offset: 2,
        });
        wp.handle_key_press(
            &mut edits(&mut state.doc),
            &Key::Character("x".into()),
            Some("x"),
            false,
            false,
            &mut false,
        );
        let c = wp.inner.containers.as_ref().unwrap();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize(&state.doc, c).blocks
        );
        assert_eq!(
            wp.blocks.len(),
            1,
            "cross-block replacement uses full refresh"
        );
    }

    {
        let mut state = large_body();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        let old_start = wp.body_window_start;
        assert!(wp.blocks.iter().any(|b| b.id == wp.inner.caret.block));
        wp.scroll_y = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 2) * ESTIMATED_BLOCK_EXTENT;
        wp.handle_key_press(
            &mut edits(&mut state.doc),
            &Key::Character("x".into()),
            Some("x"),
            false,
            false,
            &mut false,
        );
        assert_ne!(wp.body_window_start, old_start);
        let (start, end) = wp.body_window_bounds();
        let c = wp.inner.containers.as_ref().unwrap();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize_blocks(
                &state.doc,
                c,
                &wp.body_order
                    .ids(start..end)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            )
        );
    }
}

#[test]
fn resources_navigate_by_stable_ranges_and_files_are_records() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "findable text");
        wp.sync(&state.doc, &state.root);
        let block = wp.model().caret().block.clone();

        wp.navigate_to_range(&state.doc, &block, 0, 8)
            .expect("canonical range exists");
        assert_eq!(wp.model().caret().offset, 8);
        assert!(wp.model().has_selection());
        let before = wp.model().caret().clone();
        assert!(wp.navigate_to_range(&state.doc, &block, 0, 99).is_err());
        assert_eq!(
            wp.model().caret(),
            &before,
            "stale navigation changes nothing"
        );
    }
}

fn populated() -> (ui::State, Editor) {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "Title");
    wp.model_mut().enter(&mut edits(&mut state.doc));
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "Body text here");
    wp.outline.toggle();
    (state, wp)
}

#[test]
fn paper_affordances_follow_the_accent_and_read_on_paper() {
    use ui::a11y::contrast_ratio;
    use ui::tokens::colors::{NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST};
    for name in Theme::ALL {
        let theme = Theme::named(name).expect("shipped theme resolves");
        let cc = ChromeColors::for_theme(&theme);
        for (role, mark) in [("accent", cc.paper_accent), ("danger", cc.paper_danger)] {
            let r = contrast_ratio(mark, cc.paper);
            assert!(
                r >= NON_TEXT_MIN_CONTRAST,
                "{name}: paper {role} measures {r:.2}:1 on the sheet"
            );
        }
        let paper_warning = theme.tokens.colors.ink_on(
            cc.paper,
            theme.tokens.colors.semantic.warning,
            NON_TEXT_MIN_CONTRAST,
        );
        assert!(
            contrast_ratio(paper_warning, cc.paper) >= NON_TEXT_MIN_CONTRAST,
            "{name}: paper warning must clear the non-text floor"
        );
        assert_eq!(
            cc.paper_tint,
            theme.on_surface(cc.paper).tint,
            "{name}: paper tint is on_surface(paper).tint"
        );
        let label = contrast_ratio(cc.on_paper_accent, cc.paper_accent);
        assert!(
            label >= ON_FILL_MIN_CONTRAST,
            "{name}: band tab label measures {label:.2}:1 on its tab"
        );
    }
    let blue = ChromeColors::for_theme(&Theme::dark());
    let green = ChromeColors::for_theme(
        &Theme::dark().with_your_colour(ui::tokens::colors::PresenceHue::Green),
    );
    assert_ne!(
        blue.paper_accent, green.paper_accent,
        "the caret is your colour rather than a fixed blue"
    );
}

#[test]
fn paper_stays_readable_and_the_draw_follows_the_theme() {
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let cc = ChromeColors::for_theme(&theme);
        assert!(
            luminance(cc.paper) >= 0.85,
            "paper must stay light (got luminance {:.2}) so ink contrasts",
            luminance(cc.paper),
        );
        assert!(
            luminance(cc.paper) - luminance(PAPER_INK) > 0.5,
            "paper/ink contrast must be ample",
        );
    }
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let content = theme.tokens.colors.surfaces.content;
        if luminance(content) >= 0.85 {
            assert!(
                luminance(ChromeColors::for_theme(&theme).paper) >= luminance(content),
                "a light theme's page is never darker than the window behind it"
            );
        }
    }
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let (state, wp) = populated();
        let cc = editor(&mut mount_themed(state, wp, 1100, 760, &theme)).chrome;
        assert_eq!(cc.paper, ChromeColors::for_theme(&theme).paper);
        assert_eq!(
            cc.paper_accent,
            ChromeColors::for_theme(&theme).paper_accent
        );
    }
}

#[test]
fn drawing_lays_out_every_block_within_a_bounded_viewport() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Hello");
        wp.model_mut().enter(&mut edits(&mut state.doc));
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "World");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        assert_eq!(wp.laid.len(), 2, "draw laid out both paragraphs");
    }

    {
        let state = large_body();
        let wp = Editor::detached();
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        assert_eq!(wp.body_order.len(), VIEWPORT_BLOCK_WINDOW * 4);
        assert!(
            wp.blocks.len() <= VIEWPORT_BLOCK_WINDOW,
            "offscreen rich block views stay bounded: {}",
            wp.blocks.len()
        );
        assert!(
            wp.layout_cache.len() <= VIEWPORT_BLOCK_WINDOW,
            "offscreen shaped layouts stay bounded: {}",
            wp.layout_cache.len()
        );
        let all = wp.search_blocks(&state.doc);
        assert_eq!(all.len(), VIEWPORT_BLOCK_WINDOW * 4);
        assert!(all.last().is_some_and(|b| b.text.ends_with("767")));
        drop(all);

        wp.scroll_y = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 3) * ESTIMATED_BLOCK_EXTENT;
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert!(wp.body_window_start > 0, "scrolling loads a later window");
        assert!(wp.blocks.len() <= VIEWPORT_BLOCK_WINDOW);
        assert!(wp.layout_cache.len() <= VIEWPORT_BLOCK_WINDOW);
    }

    {
        let state = large_body();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        let retained_id = wp.blocks[100].id.clone();
        let retained_text = wp.blocks[100].text.as_ptr();
        for focus in [60, 80, 70, 48] {
            wp.scroll_y = num::f64_of_usize(focus) * ESTIMATED_BLOCK_EXTENT;
            wp.refresh_blocks(&state.doc);
            assert_eq!(
                wp.blocks
                    .iter()
                    .find(|b| b.id == retained_id)
                    .unwrap()
                    .text
                    .as_ptr(),
                retained_text,
                "overlapping unchanged views should be moved, not rebuilt"
            );
            let (start, end) = wp.body_window_bounds();
            assert_eq!(
                wp.blocks,
                tilo_doc::model::materialize_blocks(
                    &state.doc,
                    wp.inner.containers.as_ref().unwrap(),
                    &wp.body_order
                        .ids(start..end)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                )
            );
        }
        wp.inner.set_caret(Caret {
            block: retained_id,
            offset: 0,
        });
        wp.set_caret_block_indent_view(123.0);
        wp.set_caret_block_first_line_view(42.0);
        wp.scroll_y = 70.0 * ESTIMATED_BLOCK_EXTENT;
        wp.refresh_blocks(&state.doc);
        let (start, end) = wp.body_window_bounds();
        assert_eq!(
            wp.blocks,
            tilo_doc::model::materialize_blocks(
                &state.doc,
                wp.inner.containers.as_ref().unwrap(),
                &wp.body_order
                    .ids(start..end)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            ),
            "scrolling must discard uncommitted previews even without a live drag"
        );
    }
}

#[test]
fn deep_scrolling_keeps_visible_text_loaded() {
    let state = large_body();
    let mut wp = Editor::detached();
    wp.view.auto_fit_page = false;
    let mut framed = mount(state, wp, 1000, 700);
    for zoom in [0.5, 1.0, 2.0] {
        editor(&mut framed).zoom.set(zoom);
        pipeline_draw(&mut framed, 1000, 700);
        for fraction in [0.75, 0.5, 0.25, 0.0] {
            let wp = editor(&mut framed);
            wp.scroll_y = wp.max_scroll * fraction;
            let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
            assert!(
                wp.laid.iter().any(|l| {
                    let top =
                        wp.zoom.anchor().y + (l.y_top - wp.zoom.anchor().y) * wp.zoom.factor();
                    top + l.height * wp.zoom.factor() > wp.zoom_viewport.y0
                        && top < wp.zoom_viewport.y1
                }),
                "zoom {zoom}, scroll fraction {fraction} must retain visible text (window {}, scroll {})",
                wp.body_window_start,
                wp.scroll_y
            );
        }
    }
}

#[test]
fn returning_to_recent_text_reuses_shapes_and_refreshes_edited_words() {
    let state = large_body();
    let wp = Editor::detached();
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, _) = split(&mut framed);
    let first = wp.body_order.id_at(0).unwrap().to_owned();
    let cold_shapes = wp.dbg_layout_misses;
    assert!(cold_shapes >= VIEWPORT_BLOCK_WINDOW);
    let distant_scroll = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 3) * ESTIMATED_BLOCK_EXTENT;
    wp.scroll_y = distant_scroll;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    wp.scroll_y = 0.0;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    assert_eq!(
        wp.dbg_layout_misses, 0,
        "returning to the previous viewport should reuse its text shaping"
    );
    assert!(wp.dbg_layout_hits >= cold_shapes);
    assert!(wp.layout_cache.len() <= VIEWPORT_BLOCK_WINDOW);
    assert!(wp.recent_layout_cache.len() <= VIEWPORT_BLOCK_WINDOW);
    wp.scroll_y = distant_scroll;
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
    with_doc(state, |dm| {
        dm.splice_text(&first, 0, 0, "Updated ");
    });
    wp.scroll_y = 0.0;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    assert_eq!(
        wp.dbg_layout_misses, 1,
        "changed offscreen text must be reshaped"
    );
    assert!(wp.blocks.first().unwrap().text.starts_with("Updated "));
    assert!(
        wp.laid
            .iter()
            .any(|l| l.id == first && l.text.starts_with("Updated "))
    );
    for position in [distant_scroll / 2.0, distant_scroll, distant_scroll / 2.0] {
        editor(&mut framed).scroll_y = position;
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert!(wp.layout_cache.len() + wp.recent_layout_cache.len() <= 2 * VIEWPORT_BLOCK_WINDOW);
    }
    assert_eq!(editor(&mut framed).dbg_layout_misses, 0);
}

#[test]
fn scrolling_to_top_restores_first_blocks_without_moving_deep_caret() {
    let state = large_body();
    let wp = Editor::detached();
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, _) = split(&mut framed);
    let first = wp.body_order.id_at(0).unwrap().to_owned();
    let deep = wp
        .body_order
        .id_at(VIEWPORT_BLOCK_WINDOW * 3)
        .unwrap()
        .to_owned();
    wp.inner.set_caret(Caret {
        block: deep.clone(),
        offset: 0,
    });
    wp.scroll_y = num::f64_of_usize(VIEWPORT_BLOCK_WINDOW * 3) * ESTIMATED_BLOCK_EXTENT;
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    let mut handled = false;
    wp.handle_mouse_wheel(
        &MouseScrollDelta::LineDelta(0.0, 100_000.0),
        1.0,
        false,
        &mut handled,
    );
    assert!(handled);
    assert_eq!(wp.scroll_y, 0.0);
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    assert_eq!(
        wp.inner.caret.block, deep,
        "scrolling must not relocate the insertion point"
    );
    assert_eq!(
        wp.body_window_start, 0,
        "top viewport must load the beginning, not the distant caret"
    );
    assert_eq!(
        wp.blocks.first().map(|b| b.id.as_str()),
        Some(first.as_str())
    );
    assert!(
        wp.laid
            .iter()
            .any(|l| l.id == first && l.y_top >= 0.0 && l.y_top < 700.0)
    );
}

fn select_range(wp: &mut Editor, start: usize, end: usize) {
    let block = wp.model().caret.block.clone();
    wp.model_mut().set_caret(Caret {
        block: block.clone(),
        offset: start,
    });
    wp.model_mut().set_anchor();
    wp.model_mut().set_caret(Caret { block, offset: end });
}

fn bar_slots(wp: &Editor) -> Vec<(ui::window_toolbar::ToolbarAction, Option<BarCommand>)> {
    wp.window_bar()
        .actions
        .into_iter()
        .flat_map(|action| {
            let items = action.items.clone();
            std::iter::once(action).chain(items)
        })
        .map(|slot| {
            let event = ui::window_toolbar::ToolbarEvent::Action {
                id: slot.id.clone(),
            };
            let command = wp.bar.pressed(&event);
            (slot, command)
        })
        .collect()
}

fn bar_slot(wp: &Editor, action: ToolAction) -> Option<ui::window_toolbar::ToolbarAction> {
    bar_slots(wp)
        .into_iter()
        .find(|(_, command)| *command == Some(BarCommand::Tool(action)))
        .map(|(slot, _)| slot)
}

fn bar_items(wp: &Editor, menu: &str) -> Vec<BarCommand> {
    let bar = wp.window_bar();
    let items = bar
        .actions
        .iter()
        .find(|action| action.name == menu)
        .map(|action| action.items.clone())
        .unwrap_or_default();
    items
        .into_iter()
        .filter_map(|item| {
            wp.bar
                .pressed(&ui::window_toolbar::ToolbarEvent::Action { id: item.id })
        })
        .collect()
}

fn press_bar(wp: &mut Editor, state: &mut ui::State, name: &str) {
    let slot = bar_slots(wp)
        .into_iter()
        .find(|(slot, command)| slot.name == name && command.is_some())
        .unwrap_or_else(|| panic!("the window bar offers {name}"))
        .0;
    wp.window_toolbar_event(
        &mut edits(&mut state.doc),
        &ui::window_toolbar::ToolbarEvent::Action { id: slot.id },
    );
}

fn ask_to_replace(wp: &mut Editor) {
    let viewport = wp.floating_viewport();
    let top = Rect::new(wp.rect.x1, wp.rect.y0, wp.rect.x1, wp.rect.y0);
    wp.file_menu
        .ask_anchored(crate::Template::Report, top, viewport);
    assert!(wp.file_menu.is_open(), "the question is open");
}

fn tool_actions(wp: &Editor) -> Vec<ToolAction> {
    bar_slots(wp)
        .into_iter()
        .filter_map(|(_, command)| match command {
            Some(BarCommand::Tool(action)) => Some(action),
            _ => None,
        })
        .collect()
}

fn effect_edit(
    wp: &Editor,
    doc: &AutoCommit,
    effect: &crate::effects::WordProcessorEffect,
) -> crate::actions::RichTextAction {
    let (actions, _) = effect
        .edit(&wp.local_edit_view(), doc, wp.inner.content_store())
        .expect("an edit effect")
        .expect("the edit builds");
    assert_eq!(actions.len(), 1, "one edit");
    actions[0].clone()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn toolbar_and_palette_actions_dispatch_through_their_specs() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Intro");
        wp.refresh_blocks(&state.doc);

        let from_palette = effect_edit(
            &wp,
            &state.doc,
            &crate::effects::WordProcessorEffect::InsertTableHere {
                rows: None,
                columns: None,
                at: None,
            },
        );

        press_bar(&mut wp, &mut state, &t("toolbar-table"));
        let from_toolbar = wp
            .last_dispatched_for_test()
            .cloned()
            .expect("the toolbar dispatched an action");

        assert_eq!(
            from_toolbar, from_palette,
            "toolbar affordance and palette effect build the same edit"
        );
        wp.refresh_blocks(&state.doc);
        assert_eq!(
            wp.blocks.iter().filter(|b| b.kind == "cell").count(),
            4,
            "the toolbar affordance inserted the table"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Body text");
        wp.refresh_blocks(&state.doc);
        let before = state.doc.get_heads();

        wp.apply_panel_hit(&mut edits(&mut state.doc), PanelHit::ParaAlign("center"));
        let dispatched = wp
            .last_dispatched_for_test()
            .cloned()
            .expect("the panel dispatched an action");

        assert_ne!(
            state.doc.get_heads(),
            before,
            "the affordance mutated the document"
        );

        let from_palette = effect_edit(
            &wp,
            &state.doc,
            &crate::effects::WordProcessorEffect::Align {
                align: "center".into(),
                at: None,
            },
        );
        assert_eq!(
            dispatched, from_palette,
            "panel align and palette effect build the same edit"
        );
        wp.refresh_blocks(&state.doc);
        let aligned = wp
            .blocks
            .iter()
            .any(|b| b.str_attr("align") == Some("center"));
        assert!(aligned, "the affordance set the paragraph alignment");
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "hello world hello there");
        wp.refresh_blocks(&state.doc);

        let found =
            ui::widget::Findable::find(&mut wp, "hello", ui::widget::FindOptions::default());
        assert!(
            found.is_empty(),
            "the paragraphs are read with the document"
        );
        wp.refresh_blocks(&state.doc);
        let found =
            ui::widget::Findable::find(&mut wp, "hello", ui::widget::FindOptions::default());
        assert_eq!(found.len(), 2, "case-insensitive search finds both");

        assert_eq!(ui::widget::Findable::replace_all(&mut wp, "hi"), 2);
        wp.apply_find_replacements(&mut edits(&mut state.doc));
        assert_eq!(wp.blocks[0].text, "hi world hi there");
        assert!(
            wp.find.matches().is_empty(),
            "no matches remain after replace-all"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        assert!(
            !wp.view.styles_panel_visible,
            "the first-launch writing surface is uncluttered"
        );
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::ToggleStyles);
        assert!(
            wp.view.styles_panel_visible,
            "toolbar toggle reveals the panel"
        );
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::ToggleStyles);
        assert!(
            !wp.view.styles_panel_visible,
            "toolbar toggle dismisses the panel"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Read this");
        wp.sync(&state.doc, &state.root);

        assert!(
            !tool_actions(&wp).contains(&ToolAction::Hyperlink),
            "an empty selection cannot author a link"
        );
        select_range(&mut wp, 0, 4);
        assert!(
            tool_actions(&wp).contains(&ToolAction::Hyperlink),
            "a non-empty selection exposes the Link affordance"
        );

        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::Hyperlink);
        assert_eq!(
            wp.take_action_palette_request(),
            Some(crate::effects::WordProcessorEffect::SET_HYPERLINK.on("word-processor.caret"))
        );
        assert!(
            wp.take_action_palette_request().is_none(),
            "the host request is edge-triggered"
        );
    }

    {
        let (state, wp) = populated();
        let mut narrow = mount(state, wp, 760, 700);
        let (wp, _) = split(&mut narrow);
        let expected = tool_actions(wp);
        assert!(!expected.is_empty());
        for action in expected {
            assert!(
                bar_slot(wp, action).is_some(),
                "command {action:?} is reachable in the window bar"
            );
        }
    }
}

#[test]
fn character_marks_shape_their_spans() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "hello world");
        select_range(&mut wp, 0, 5);
        let yellow = crate::actions::MarkVal::Str("#fff35c".into());
        wp.dispatch_set_mark_value(&mut edits(&mut state.doc), "highlight", yellow);
        wp.refresh_blocks(&state.doc);
        let b = wp
            .blocks
            .iter()
            .find(|b| b.text == "hello world")
            .expect("block present");
        let (runs, _, _) = block_runs(b, false);
        let first = &runs[0];
        assert_eq!(first.text, "hello");
        assert_eq!(
            first.background,
            Some([0xff, 0xf3, 0x5c]),
            "highlight maps to RichSpan.background"
        );
        assert!(
            runs.iter()
                .any(|r| r.text.contains("world") && r.background.is_none()),
            "unhighlighted tail has no band"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut().insert_str(&mut edits(&mut state.doc), "x2");
        select_range(&mut wp, 1, 2); // the "2"
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::Super);
        wp.refresh_blocks(&state.doc);
        let b = wp
            .blocks
            .iter()
            .find(|b| b.text == "x2")
            .expect("block present");
        let (runs, _, _) = block_runs(b, false);
        let sup = runs.iter().find(|r| r.text == "2").expect("the '2' run");
        assert_eq!(
            sup.baseline,
            ui::richtext::BaselineShift::Super,
            "super mark maps to BaselineShift::Super"
        );
        let base = runs.iter().find(|r| r.text == "x").expect("the 'x' run");
        assert_eq!(
            base.baseline,
            ui::richtext::BaselineShift::None,
            "unmarked run stays on the baseline"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        let long = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua";
        wp.model_mut().insert_str(&mut edits(&mut state.doc), long);

        let mut framed = mount(state, wp, 700, 700);
        let (wp, state) = split(&mut framed);
        let single_h = wp
            .laid
            .iter()
            .find(|l| l.text == long)
            .map(|l| l.height)
            .expect("laid block");
        assert_eq!(
            block_line_spacing(wp.blocks.iter().find(|b| b.text == long).unwrap()),
            1.0,
            "default 1.0"
        );

        wp.dispatch_block_attr(
            &mut edits(&mut state.doc),
            "line_spacing",
            crate::actions::AttrVal::Float(2.0),
        );
        wp.refresh_blocks(&state.doc);
        assert_eq!(
            block_line_spacing(wp.blocks.iter().find(|b| b.text == long).unwrap()),
            2.0,
            "line_spacing=2.0 read from the block attr the layout pass uses"
        );
        let (wp, _) = pipeline_draw(&mut framed, 700, 700);
        let double_h = wp
            .laid
            .iter()
            .find(|l| l.text == long)
            .map(|l| l.height)
            .expect("laid block");
        assert!(
            double_h > single_h * 1.5,
            "double line spacing yields a much taller layout: single={single_h}, double={double_h}"
        );
    }
}

#[test]
fn super_and_sub_are_mutually_exclusive() {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut().insert_str(&mut edits(&mut state.doc), "x2");
    let c = wp.model().containers.clone().unwrap();
    let block = wp.model().caret().block.clone();
    let marked = |doc: &automerge::AutoCommit, name: &str| {
        tilo_doc::model::has_mark_over(doc, &c, &block, 1, 2, name)
    };
    select_range(&mut wp, 1, 2);
    wp.apply_tool(&mut edits(&mut state.doc), ToolAction::Super);
    assert!(marked(&state.doc, "super"), "super applied");
    assert!(!marked(&state.doc, "sub"), "no sub yet");

    select_range(&mut wp, 1, 2);
    wp.apply_tool(&mut edits(&mut state.doc), ToolAction::Sub);
    assert!(marked(&state.doc, "sub"), "sub applied");
    assert!(!marked(&state.doc, "super"), "super cleared");

    select_range(&mut wp, 1, 2);
    wp.apply_tool(&mut edits(&mut state.doc), ToolAction::Sub);
    assert!(!marked(&state.doc, "sub"), "sub toggled off");
}

#[test]
fn headings_draw_bold_and_unchanged_blocks_reuse_their_layout() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "First paragraph here");
        wp.model_mut().enter(&mut edits(&mut state.doc));
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Second paragraph here");

        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        assert_eq!(wp.dbg_layout_hits, 0, "cold draw cannot reuse anything");
        assert_eq!(
            wp.dbg_layout_misses, 2,
            "both paragraphs shaped on the first frame"
        );
        assert_eq!(wp.layout_cache.len(), 2, "both blocks cached");

        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            wp.dbg_layout_hits, 2,
            "unchanged re-draw reuses every cached layout"
        );
        assert_eq!(
            wp.dbg_layout_misses, 0,
            "no re-shaping on an unchanged frame"
        );

        wp.model_mut().insert_str(&mut edits(&mut state.doc), "!");
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            wp.dbg_layout_misses, 1,
            "only the edited block is re-shaped"
        );
        assert_eq!(wp.dbg_layout_hits, 1, "the untouched block is still reused");

        wp.margins = Edges::uniform(140.0);
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            wp.dbg_layout_misses, 2,
            "a wrap-width change re-shapes every block"
        );
        assert_eq!(
            wp.dbg_layout_hits, 0,
            "no block can be reused at a new wrap width"
        );
    }

    {
        use tilo_doc_codecs::{FormatProvider, MarkdownCodec};
        let mut state = crate::initialized_state();
        let c = crate::model::init(&mut state.doc).unwrap();
        let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            MarkdownCodec
                .import(
                    b"# Title\n\nHello **bold** world\n",
                    &mut dm,
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
        }
        let wp = Editor::detached();
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        assert!(wp.laid.len() >= 2);
        assert!(wp.blocks.iter().any(|b| b.kind == "heading"));
        assert!(
            wp.blocks
                .iter()
                .any(|b| b.marks.iter().any(|m| m.name == "strong"))
        );
    }
}

#[test]
fn the_styles_panel_picks_edits_and_commits_styles() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "A paragraph");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        assert!(wp.view.styles_panel_visible);
        assert!(
            !wp.panel_hits.is_empty(),
            "panel rendered clickable targets"
        );
        assert_eq!(wp.focused_style_key().as_deref(), Some("body"));

        wp.apply_panel_hit(&mut edits(&mut state.doc), PanelHit::Pick("quote".into()));
        wp.refresh_blocks(&state.doc);
        assert_eq!(
            wp.focused_style_key().as_deref(),
            Some("quote"),
            "block adopted the picked style"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "first");
        wp.model_mut().enter(&mut edits(&mut state.doc));
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "second");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        let before = wp.model().stylesheet(&state.doc).get("body").size;
        let slot = wp
            .num_inputs
            .iter_mut()
            .find(|slot| slot.field == NumField::Size)
            .unwrap();
        assert!(Widget::perform_action(
            &mut slot.input,
            &ui::a11y::Action::SetValue((before + 5.0).to_string()),
            &[],
            &mut edits(&mut state.doc),
            &state.root
        ));
        wp.drain_hosted_fields(&mut edits(&mut state.doc));
        let after = wp.model().stylesheet(&state.doc).get("body").size;
        assert_eq!(
            after,
            before + 5.0,
            "edited style size persisted to the document"
        );

        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(wp.laid.len(), 2);
        assert!(
            wp.laid.iter().all(|l| (l.size - after).abs() < 0.01),
            "every Body block follows the style edit"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut().insert_str(&mut edits(&mut state.doc), "x");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        wp.focus_hosted(Some(NumField::Size), false);
        let slot = wp
            .num_inputs
            .iter_mut()
            .find(|slot| slot.field == NumField::Size)
            .unwrap();
        assert!(Widget::perform_action(
            &mut slot.input,
            &ui::a11y::Action::SetValue("40".into()),
            &[],
            &mut edits(&mut state.doc),
            &state.root
        ));
        wp.drain_hosted_fields(&mut edits(&mut state.doc));
        assert_eq!(wp.sheet.get("body").size, 40.0);
        wp.focus_hosted(None, false);
        assert!(wp.editing_field.is_none(), "the field releases focus");
        assert_eq!(
            wp.model().stylesheet(&state.doc).get("body").size,
            40.0,
            "typed value persisted"
        );
    }
}

#[test]
fn custom_styles_are_created_deleted_and_sized_in_one_place() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "para");
        wp.set_styles_panel_open(true);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        let n_before = wp.sheet.order().len();
        wp.apply_panel_hit(&mut edits(&mut state.doc), PanelHit::NewStyle);
        let sheet = wp.model().stylesheet(&state.doc);
        assert_eq!(
            sheet.order().len(),
            n_before + 1,
            "a new style id was added"
        );
        wp.refresh_blocks(&state.doc);
        let key = wp.focused_style_key().unwrap();
        assert!(
            key.starts_with("custom"),
            "block adopted the duplicated style, got {key}"
        );
    }

    {
        let (mut framed, custom_id) = doc_with_custom_style();
        let (wp, state) = split(&mut framed);
        assert!(
            wp.sheet.styles.contains_key(&custom_id),
            "custom style present before delete"
        );

        wp.dispatch_style_menu(
            StyleMenuAction::Delete(custom_id.clone()),
            &mut edits(&mut state.doc),
        );
        wp.sync(&state.doc, &state.root);

        assert!(
            !wp.sheet.styles.contains_key(&custom_id),
            "custom style removed from the sheet"
        );
        assert!(
            !tilo_doc::model::style_ids(&state.doc, &wp.model().containers.clone().unwrap())
                .iter()
                .any(|s| s == &custom_id),
            "custom style removed from the document"
        );
        let c = wp.model().containers.clone().unwrap();
        let dangling = tilo_doc::model::materialize(&state.doc, &c).blocks.iter().any(
            |b| matches!(b.attrs.get("style"), Some(tilo_doc::AttrVal::Str(s)) if s == &custom_id),
        );
        assert!(!dangling, "no block still references the deleted style");
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Body text");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1800, 700);
        let (wp, _) = split(&mut framed);

        let panel_size_fields = wp
            .num_inputs
            .iter()
            .filter(|slot| slot.field == NumField::Size && slot.rect.width() > 0.0)
            .count();
        assert_eq!(
            panel_size_fields, 1,
            "the inspector is the single home for the style default"
        );

        let format = bar_items(wp, &t("toolbar-format"));
        assert!(
            format.contains(&BarCommand::Tool(ToolAction::FontSizeDec)),
            "A− remains available for direct character formatting"
        );
        assert!(
            format.contains(&BarCommand::Tool(ToolAction::FontSizeInc)),
            "A+ remains available for direct character formatting"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_toolbar_keeps_character_formatting_and_every_colour_action() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "para");
        let mut framed = mount(state, wp, 1800, 760);
        let (wp, _) = split(&mut framed);

        let actions = tool_actions(wp);
        for kept in [
            ToolAction::Bold,
            ToolAction::Italic,
            ToolAction::Underline,
            ToolAction::Strike,
            ToolAction::Super,
            ToolAction::Sub,
            ToolAction::SetFont(ui::richtext::FontFamily::Serif),
            ToolAction::BulletList,
            ToolAction::NumberedList,
            ToolAction::FontSizeInc,
            ToolAction::FontSizeDec,
        ] {
            assert!(
                actions.contains(&kept),
                "{kept:?} stays on the toolbar; toolbar={actions:?}"
            );
        }
        assert!(
            actions.contains(&ToolAction::InsertTable),
            "table insert stays on the toolbar"
        );
        assert!(
            actions.contains(&ToolAction::ToggleStyles),
            "styles-panel toggle stays on the toolbar"
        );

        let is_char_or_structural = |a: &ToolAction| {
            matches!(
                a,
                ToolAction::Bold
                    | ToolAction::Italic
                    | ToolAction::Underline
                    | ToolAction::Strike
                    | ToolAction::Super
                    | ToolAction::Sub
                    | ToolAction::Hyperlink
                    | ToolAction::SetFont(_)
                    | ToolAction::BulletList
                    | ToolAction::NumberedList
                    | ToolAction::FontSizeInc
                    | ToolAction::FontSizeDec
                    | ToolAction::TextColor(_)
                    | ToolAction::Highlight(_)
                    | ToolAction::TextColorPalette
                    | ToolAction::HighlightPalette
                    | ToolAction::InsertImage
                    | ToolAction::InsertTable
                    | ToolAction::TableRowAdd
                    | ToolAction::TableRowDel
                    | ToolAction::TableColAdd
                    | ToolAction::TableColDel
                    | ToolAction::TableDelete
                    | ToolAction::ToggleStyles
                    | ToolAction::ToggleRuler
                    | ToolAction::ToggleOutline
                    | ToolAction::InsertToc
            )
        };
        for a in &actions {
            assert!(
                is_char_or_structural(a),
                "unexpected paragraph-level control on the toolbar: {a:?}"
            );
        }
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "colour me");
        let mut framed = mount(state, wp, 520, 760);
        let (wp, state) = split(&mut framed);
        assert!(tool_actions(wp).contains(&ToolAction::TextColorPalette));
        assert!(tool_actions(wp).contains(&ToolAction::HighlightPalette));
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::HighlightPalette);
        assert!(wp.compact_swatch_palette.is_open());
        wp.compact_swatch_palette.close();

        for (action, key) in [
            (ToolAction::TextColorPalette, "toolbar-text-color"),
            (ToolAction::HighlightPalette, "toolbar-highlight"),
        ] {
            let slot = bar_slot(wp, action).expect(key);
            assert_eq!(slot.name, t(key), "the window bar's Format menu names it");
        }

        wp.open_compact_swatch_palette(PaletteKind::Text);
        assert!(wp.compact_swatch_palette.rect().width() > 0.0);
        let swatch = wp
            .compact_swatch_palette
            .swatch_rect(3)
            .expect("shared picker lays out each swatch")
            .center();
        left_click(wp, state, swatch);
        wp.refresh_blocks(&state.doc);
        assert_eq!(wp.current_text_color(), Some(STYLE_PALETTE[3]));
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "colour me");
        let mut framed = mount(state, wp, 760, 800);
        let (wp, state) = split(&mut framed);

        wp.apply_tool(
            &mut edits(&mut state.doc),
            ToolAction::TextColor(Some(STYLE_PALETTE[2])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Text);
        let pointer_clear = wp.compact_swatch_palette.clear_rect().center();
        left_click(wp, state, pointer_clear);
        assert_eq!(wp.current_text_color(), None);

        wp.apply_tool(
            &mut edits(&mut state.doc),
            ToolAction::TextColor(Some(STYLE_PALETTE[3])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Text);
        let expected_clear_bounds = wp.compact_swatch_palette.clear_rect();
        press_named(wp, state, NamedKey::Tab);
        assert_compact_clear_footer(wp, &t("picker-text-color"), expected_clear_bounds, true);
        press_named(wp, state, NamedKey::Space);
        assert_eq!(wp.current_text_color(), None);

        wp.apply_tool(
            &mut edits(&mut state.doc),
            ToolAction::TextColor(Some(STYLE_PALETTE[4])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Text);
        assert!(wp.perform_access_action(
            &ui::a11y::Action::Focus,
            &[ui::a11y::hash_key(&"compact-swatch:clear")],
            &mut edits(&mut state.doc),
            &state.root,
        ));
        assert_compact_clear_footer(
            wp,
            &t("picker-text-color"),
            wp.compact_swatch_palette.clear_rect(),
            true,
        );
        assert!(wp.perform_access_action(
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&"compact-swatch:clear")],
            &mut edits(&mut state.doc),
            &state.root,
        ));
        assert_eq!(wp.current_text_color(), None);

        wp.apply_tool(
            &mut edits(&mut state.doc),
            ToolAction::Highlight(Some(HIGHLIGHT_PALETTE[2])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Highlight);
        let highlight_pointer_clear = wp.compact_swatch_palette.clear_rect().center();
        left_click(wp, state, highlight_pointer_clear);
        assert_eq!(wp.current_highlight_color(), None);

        wp.apply_tool(
            &mut edits(&mut state.doc),
            ToolAction::Highlight(Some(HIGHLIGHT_PALETTE[3])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Highlight);
        press_named(wp, state, NamedKey::Tab);
        press_named(wp, state, NamedKey::Enter);
        assert_eq!(wp.current_highlight_color(), None);

        wp.apply_tool(
            &mut edits(&mut state.doc),
            ToolAction::Highlight(Some(HIGHLIGHT_PALETTE[4])),
        );
        wp.open_compact_swatch_palette(PaletteKind::Highlight);
        assert!(wp.perform_access_action(
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&"compact-swatch:clear")],
            &mut edits(&mut state.doc),
            &state.root,
        ));
        assert_eq!(wp.current_highlight_color(), None);
    }
}

fn assert_compact_clear_footer(wp: &Editor, title: &str, expected_bounds: Rect, focused: bool) {
    let tree = wp.accessibility_tree();
    let palette = tree
        .children
        .iter()
        .find(|child| child.node.name.as_deref() == Some(title))
        .expect("open palette is a named accessibility group");
    let clear = palette
        .node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some("compact-swatch:clear"))
        .expect("localized clear footer is exported");
    assert_eq!(clear.role, ui::a11y::Role::Button);
    assert_eq!(clear.bounds, Some(expected_bounds.into()));
    assert_eq!(clear.state.focused, focused);
}

#[test]
fn painted_paragraph_alignment_swipes_edit_the_real_document_and_undo() {
    use winit::event::TouchPhase::{Cancelled, Ended, Started};
    let mut app =
        ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
    app.effect(&WordProcessorEffect::Styles).unwrap();
    app.settle();
    let before = app.peer_document().get_heads();
    let start = app
        .with_surface::<WordProcessorWidget, _>(on_editor(|widget| {
            assert_eq!(widget.current_align(), "left");
            widget
                .panel_hits
                .iter()
                .find_map(|(rect, hit)| {
                    matches!(hit, PanelHit::ParaAlign("center")).then_some(rect.center())
                })
                .expect("the actual inspector paints Center")
        }))
        .unwrap();
    app.harness().touch(1, Started, start.x, start.y);
    app.harness().touch(1, Ended, start.x - 90.0, start.y);
    app.settle();
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.current_align()))
            .unwrap(),
        "center"
    );
    assert_ne!(app.peer_document().get_heads(), before);
    app.harness().touch(2, Started, start.x, start.y);
    app.harness().touch(2, Cancelled, start.x - 90.0, start.y);
    app.settle();
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.current_align()))
            .unwrap(),
        "center"
    );
    assert!(
        app.undo(),
        "the admitted paragraph attribute is a real undo step"
    );
    app.settle();
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.current_align()))
            .unwrap(),
        "left"
    );
    app.set_role(ui::permissions::Role::Viewer);
    assert!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(
            |widget| widget.claims_raw_touch(start)
        ))
        .unwrap(),
        "the viewer gesture reaches the actual painted strip"
    );
    let protected = app.peer_document().get_heads();
    app.harness().touch(3, Started, start.x, start.y);
    app.harness().touch(3, Ended, start.x - 90.0, start.y);
    app.settle();
    assert_eq!(
        app.peer_document().get_heads(),
        protected,
        "touch grants no edit authority to a viewer"
    );
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.current_align()))
            .unwrap(),
        "left"
    );
}

#[test]
fn compact_styles_is_a_full_window_page_with_real_controls_and_no_typing_leak() {
    for viewport in [(390, 844), (844, 390)] {
        let mut app =
            ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
        app.harness().resize(viewport).unwrap();
        app.effect(&WordProcessorEffect::Styles).unwrap();
        app.settle();
        let (heading, close) = app
            .with_surface::<WordProcessorWidget, _>(on_editor(|widget| {
                assert!(widget.styles_panel_full_window());
                assert_eq!(
                    widget.panel_rect, widget.rect,
                    "the shared compact presentation leaves no underlying document gutter"
                );
                let close = widget
                    .panel_hits
                    .iter()
                    .find_map(|(rect, hit)| matches!(hit, PanelHit::Close).then_some(*rect))
                    .expect("the actual shared close control is painted");
                assert!(
                    close.x0 >= widget.rect.x0
                        && close.x1 <= widget.rect.x1
                        && close.y0 >= widget.rect.y0
                        && close.y1 <= widget.rect.y1
                );
                (
                    Point::new(widget.panel_rect.x0 + 16.0, close.center().y),
                    close.center(),
                )
            }))
            .unwrap();
        let heads = app.peer_document().get_heads();
        app.harness().click_at(heading.x, heading.y);
        app.harness()
            .type_text("never insert this into the displaced page");
        app.settle();
        assert_eq!(app.peer_document().get_heads(), heads);
        if viewport.1 < viewport.0 {
            assert_styles_page_scrolls_to_its_last_real_action(&mut app);
        }
        assert!(
            app.with_surface::<WordProcessorWidget, _>(on_editor(
                |widget| widget.styles_panel_full_window()
            ))
            .unwrap()
        );
        app.harness().click_at(close.x, close.y);
        app.settle();
        assert!(
            !app.with_surface::<WordProcessorWidget, _>(on_editor(
                |widget| widget.styles_panel_visible()
            ))
            .unwrap()
        );
        app.effect(&WordProcessorEffect::Styles).unwrap();
        app.settle();
        app.harness().key("Escape");
        app.settle();
        assert!(
            !app.with_surface::<WordProcessorWidget, _>(on_editor(
                |widget| widget.styles_panel_visible()
            ))
            .unwrap()
        );
        assert_eq!(
            app.peer_document().get_heads(),
            heads,
            "closing either way is local presentation only"
        );
    }
}

#[test]
fn compact_styles_blocks_document_shortcuts_after_real_document_typing() {
    let mut app =
        ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
    app.harness().resize((390, 844)).unwrap();
    app.settle();
    let body = app
        .with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.zoom_viewport.center()))
        .unwrap();
    app.harness().click_at(body.x, body.y);
    app.harness().type_text("protected document words");
    app.settle();
    let text = app
        .with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.blocks[0].text.clone()))
        .unwrap();
    assert_eq!(text, "protected document words");
    app.effect(&WordProcessorEffect::Styles).unwrap();
    app.settle();
    let heads = app.peer_document().get_heads();
    for modifier in ["ctrl", "super"] {
        app.harness().set_modifiers(&[modifier.into()]);
        for key in ["Backspace", "b", "v", "Enter", "a", "Delete"] {
            app.harness().key(key);
            app.settle();
            assert_eq!(
                app.peer_document().get_heads(),
                heads,
                "{modifier}+{key} must not edit the displaced document"
            );
        }
        app.harness().set_modifiers(&[]);
    }
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.blocks[0]
            .text
            .clone()))
            .unwrap(),
        text
    );
}

#[test]
fn compact_styles_publishes_only_foreground_ui_and_retains_document_actions() {
    let mut app =
        ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
    app.harness().resize((390, 844)).unwrap();
    app.settle();
    let resources =
        |app: &mut ui::framework::testing::TestApp<crate::framework::WordProcessorApp>| {
            app.harness()
                .resources()
                .into_iter()
                .filter(|resource| resource.kind != "tilo.control")
                .map(|resource| {
                    (
                        resource.id,
                        resource
                            .actions
                            .into_iter()
                            .map(|action| action.id.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
    let catalogue = resources(&mut app);
    assert!(!catalogue.is_empty());
    app.effect(&WordProcessorEffect::Styles).unwrap();
    app.settle();
    assert_eq!(resources(&mut app), catalogue);
    assert!(
        app.harness()
            .a11y_node_named(&t("a11y-document-editor"))
            .is_none()
    );
    let heads = app.peer_document().get_heads();
    let close = app
        .harness()
        .a11y_node_named(&t("panel-close"))
        .expect("the foreground page publishes its actual Close control");
    app.harness()
        .perform_a11y_action(close, &ui::a11y::Action::Focus)
        .unwrap();
    app.harness()
        .perform_a11y_action(close, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    assert!(
        !app.with_surface::<WordProcessorWidget, _>(on_editor(
            |widget| widget.styles_panel_visible()
        ))
        .unwrap()
    );
    assert_eq!(app.peer_document().get_heads(), heads);
}

#[test]
fn compact_styles_accessibility_edits_real_style_and_refuses_viewers() {
    let mut app =
        ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
    app.harness().resize((390, 844)).unwrap();
    app.effect(&WordProcessorEffect::Styles).unwrap();
    app.settle();
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{:?}", audit.issues);
    let size = app.harness().a11y_node_named(&t("panel-num-size")).unwrap();
    app.harness()
        .perform_a11y_action(size, &ui::a11y::Action::Focus)
        .unwrap();
    app.harness()
        .perform_a11y_action(size, &ui::a11y::Action::SetValue("26".into()))
        .unwrap();
    app.settle();
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget
            .sheet
            .get("body")
            .size))
            .unwrap(),
        26.0
    );
    app.harness().key("Enter");
    app.settle();
    assert!(app.undo());
    app.settle();
    assert_ne!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget
            .sheet
            .get("body")
            .size))
            .unwrap(),
        26.0
    );
    app.set_role(ui::permissions::Role::Viewer);
    app.settle();
    let protected = app.peer_document().get_heads();
    let size = app.harness().a11y_node_named(&t("panel-num-size")).unwrap();
    assert!(
        app.harness()
            .perform_a11y_action(size, &ui::a11y::Action::SetValue("40".into()))
            .is_err()
    );
    app.settle();
    assert_eq!(app.peer_document().get_heads(), protected);
}

#[test]
fn compact_styles_accessibility_tracks_scrolled_foreground_controls() {
    let mut app =
        ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
    app.harness().resize((844, 390)).unwrap();
    app.effect(&WordProcessorEffect::Styles).unwrap();
    app.settle();
    let align = app
        .harness()
        .a11y_node_named(&t("panel-align-left"))
        .unwrap();
    assert!(
        app.harness()
            .a11y_node_named(&t("panel-new-style"))
            .is_none()
    );
    let before = app.peer_document().get_heads();
    let document_scroll = app
        .with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.scroll_y))
        .unwrap();
    let region = app
        .harness()
        .root_widget()
        .primary_vertical_scroll()
        .expect("the hosted root publishes the foreground scroll region");
    assert_eq!(region.key, "styles");
    assert!(region.metrics.max_scroll() > 0.0);
    let result = app
        .with_surface::<WordProcessorWidget, _>(on_editor(|widget| {
            widget.apply_primary_vertical_scroll(ui::widget::PrimaryScrollAction::End)
        }))
        .unwrap();
    assert!(matches!(
        result,
        ui::widget::PrimaryScrollResult::Changed(_)
    ));
    app.settle();
    assert_eq!(app.peer_document().get_heads(), before);
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget.scroll_y))
            .unwrap(),
        document_scroll
    );
    assert!(
        app.harness()
            .a11y_node_named(&t("panel-align-left"))
            .is_none()
    );
    assert!(
        app.harness()
            .a11y_node_named(&t("panel-num-indent"))
            .is_some()
    );
    assert!(
        app.harness()
            .a11y_node_named(&t("panel-new-style"))
            .is_some()
    );
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{:?}", audit.issues);
    let heads = app.peer_document().get_heads();
    assert!(
        app.harness()
            .perform_a11y_action(align, &ui::a11y::Action::Activate)
            .is_err()
    );
    assert_eq!(app.peer_document().get_heads(), heads);
}

#[test]
fn compact_styles_accessibility_focus_routes_the_real_spacing_dropdown() {
    let mut app =
        ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
    app.harness().resize((390, 844)).unwrap();
    app.effect(&WordProcessorEffect::Styles).unwrap();
    app.settle();
    let before = app.peer_document().get_heads();
    let spacing = app
        .harness()
        .a11y_node_named(&t("panel-line-spacing"))
        .unwrap();
    app.harness()
        .perform_a11y_action(spacing, &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().key("ArrowDown");
    app.settle();
    assert!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget
            .line_spacing
            .is_open()))
            .unwrap()
    );
    app.harness().key("Escape");
    app.settle();
    assert!(
        !app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| widget
            .line_spacing
            .is_open()))
            .unwrap()
    );
    assert_eq!(app.peer_document().get_heads(), before);
    let spacing = app
        .harness()
        .a11y_node_named(&t("panel-line-spacing"))
        .unwrap();
    app.harness()
        .perform_a11y_action(
            spacing,
            &ui::a11y::Action::SetValue(line_spacing_label(2.0)),
        )
        .unwrap();
    app.settle();
    assert_eq!(
        app.with_surface::<WordProcessorWidget, _>(on_editor(
            |widget| widget.current_line_spacing()
        ))
        .unwrap(),
        2.0
    );
    assert_ne!(app.peer_document().get_heads(), before);
}

fn assert_styles_page_scrolls_to_its_last_real_action(
    app: &mut ui::framework::testing::TestApp<crate::framework::WordProcessorApp>,
) {
    let body = app
        .with_surface::<WordProcessorWidget, _>(on_editor(|widget| {
            assert!(
                widget.panel_max_scroll > 0.0,
                "landscape genuinely needs scrolling"
            );
            widget.panel_body
        }))
        .unwrap();
    app.harness().send(&ui::synthetic_input::cursor_moved(
        body.center().x,
        body.center().y,
    ));
    app.harness().scroll(0.0, -20.0);
    app.settle();
    app.with_surface::<WordProcessorWidget, _>(on_editor(|widget| {
        let action = widget
            .panel_hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, PanelHit::NewStyle).then_some(*rect))
            .expect("the real final style action is painted");
        assert!(widget.panel_scroll > 0.0);
        assert!(
            action.y0 >= widget.panel_body.y0 && action.y1 <= widget.panel_body.y1,
            "the full-window inspector scrolls its last action into view: {action:?}"
        );
    }))
    .unwrap();
}

#[test]
fn the_panel_applies_paragraph_alignment_and_spacing() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "centre me");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        assert_eq!(
            wp.current_align(),
            "left",
            "paragraphs are left-aligned by default"
        );

        wp.apply_panel_hit(&mut edits(&mut state.doc), PanelHit::ParaAlign("center"));
        assert_eq!(
            wp.current_align(),
            "center",
            "the current paragraph is now centred"
        );
        assert_eq!(
            wp.model().stylesheet(&state.doc).get("body").align,
            "left",
            "the Body style default alignment is untouched — the panel edited the paragraph, not the preset"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "spaced");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        let spacing = |wp: &Editor| {
            block_line_spacing(wp.blocks.iter().find(|b| b.text == "spaced").unwrap())
        };
        assert!(
            (spacing(wp) - 1.0).abs() < 1e-9,
            "single spacing by default"
        );

        wp.line_spacing
            .set_selected(line_spacing_label(block_layout::LINE_SPACINGS[1]));
        wp.drain_hosted_fields(&mut edits(&mut state.doc));
        assert!(
            (spacing(wp) - 1.15).abs() < 1e-9,
            "the picked spacing applies to the paragraph"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "para");
        wp.set_styles_panel_open(true);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        let has = |want: &PanelHit| wp.panel_hits.iter().any(|(_, h)| h == want);
        for a in ["left", "center", "right", "justify"] {
            assert!(
                has(&PanelHit::ParaAlign(a)),
                "the panel owns paragraph alignment `{a}`"
            );
        }
        assert!(
            wp.line_spacing_rect.width() > 0.0,
            "the panel owns paragraph line spacing"
        );
        assert!(
            wp.panel_hits
                .iter()
                .any(|(_, hit)| matches!(hit, PanelHit::SetColor(_))),
            "the style editor stays (colour swatches present)"
        );
        assert!(
            wp.num_inputs
                .iter()
                .any(|slot| slot.field == NumField::Size && slot.rect.width() > 0.0),
            "the style Size field stays"
        );
    }
}

#[test]
fn paragraph_kind_font_size_and_bold_apply_from_panel_and_toolbar() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "make me a heading");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        let block_kind = |wp: &Editor, st: &ui::State| -> (String, Option<i64>) {
            let c = wp.model().containers.clone().unwrap();
            let b = tilo_doc::model::materialize(&st.doc, &c)
                .blocks
                .into_iter()
                .next()
                .unwrap();
            let level = b.int_attr("level");
            (b.kind, level)
        };

        assert_eq!(block_kind(wp, state).0, "paragraph");
        assert!(wp.outline_entries().is_empty(), "no headings yet");

        wp.apply_panel_hit(
            &mut edits(&mut state.doc),
            PanelHit::Pick("heading1".into()),
        );
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(
            block_kind(wp, state),
            ("heading".into(), Some(1)),
            "picker made it a level-1 heading"
        );
        assert_eq!(
            wp.outline_entries().len(),
            1,
            "the heading is now in the outline"
        );

        wp.apply_panel_hit(&mut edits(&mut state.doc), PanelHit::Pick("list".into()));
        assert_eq!(
            block_kind(wp, state).0,
            "list_item",
            "picker made it a list item"
        );
        assert!(
            wp.outline_entries().is_empty(),
            "a list item is not a heading"
        );

        wp.apply_panel_hit(&mut edits(&mut state.doc), PanelHit::Pick("body".into()));
        assert_eq!(
            block_kind(wp, state).0,
            "paragraph",
            "picker restored the paragraph kind"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Hello");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        let style_size = wp.model().stylesheet(&state.doc).get("body").size;
        let base = wp.current_font_size();
        assert!(
            (base - style_size).abs() < 0.01,
            "with no run mark the effective size is the style default"
        );

        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::FontSizeInc);
        wp.refresh_blocks(&state.doc);
        let up = wp.current_font_size();
        assert!(up > base, "A+ enlarges the selection: {base} -> {up}");
        assert!(
            (wp.model().stylesheet(&state.doc).get("body").size - style_size).abs() < 0.01,
            "the Body style default size is untouched — direct formatting only"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "bold me");
        wp.model_mut().select_all(&state.doc);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        assert!(!wp.mark_active_in_selection("strong"), "not bold yet");

        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::Bold);
        wp.refresh_blocks(&state.doc);
        assert!(
            wp.mark_active_in_selection("strong"),
            "the whole selection is now bold"
        );
    }
}

#[test]
fn the_window_bar_picks_the_font_and_makes_lists_and_is_the_one_place_for_emphasis() {
    use ui::richtext::FontFamily;
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "pick my font");
    wp.model_mut().select_all(&state.doc);
    wp.set_styles_panel_open(true);
    let mut framed = mount(state, wp, 1800, 760);
    let (wp, state) = split(&mut framed);

    let fonts = |wp: &Editor| -> Vec<(ToolAction, bool)> {
        [FontFamily::Sans, FontFamily::Serif, FontFamily::Mono]
            .into_iter()
            .map(|family| {
                let action = ToolAction::SetFont(family);
                let slot = bar_slot(wp, action).expect("the Format menu offers the font");
                assert_eq!(slot.name, super::controls::font_label(family));
                (action, slot.on)
            })
            .collect()
    };
    let format = bar_items(wp, &t("toolbar-format"));
    assert!(
        format.contains(&BarCommand::Tool(ToolAction::SetFont(FontFamily::Mono))),
        "the fonts are the Format menu's: {format:?}"
    );
    assert_eq!(
        fonts(wp),
        [
            (ToolAction::SetFont(FontFamily::Sans), true),
            (ToolAction::SetFont(FontFamily::Serif), false),
            (ToolAction::SetFont(FontFamily::Mono), false),
        ]
    );

    let style_font = wp.model().stylesheet(&state.doc).get("body").font;
    wp.apply_tool(
        &mut edits(&mut state.doc),
        ToolAction::SetFont(FontFamily::Serif),
    );
    wp.refresh_blocks(&state.doc);
    assert_eq!(wp.current_font(), FontFamily::Serif);
    assert_eq!(
        wp.model().stylesheet(&state.doc).get("body").font,
        style_font,
        "the style is untouched"
    );
    assert_eq!(
        fonts(wp),
        [
            (ToolAction::SetFont(FontFamily::Sans), false),
            (ToolAction::SetFont(FontFamily::Serif), true),
            (ToolAction::SetFont(FontFamily::Mono), false),
        ],
        "the menu checks the chosen font"
    );

    assert!(
        wp.panel_hits.iter().all(|(_, hit)| matches!(
            hit,
            PanelHit::Close
                | PanelHit::Pick(_)
                | PanelHit::ParaAlign(_)
                | PanelHit::SetColor(_)
                | PanelHit::OpenColorPicker
                | PanelHit::NewStyle
        )),
        "{:?}",
        wp.panel_hits
    );

    let actions = tool_actions(wp);
    assert!(actions.contains(&ToolAction::BulletList), "{actions:?}");
    assert!(actions.contains(&ToolAction::NumberedList), "{actions:?}");
    let kind = |wp: &Editor| {
        let block = wp.blocks.iter().find(|b| b.text == "pick my font").unwrap();
        (block.kind.clone(), block.flag("ordered"))
    };
    wp.apply_tool(&mut edits(&mut state.doc), ToolAction::BulletList);
    wp.refresh_blocks(&state.doc);
    assert_eq!(kind(wp), ("list_item".to_owned(), false));
    assert!(wp.list_active(false) && !wp.list_active(true));
    wp.apply_tool(&mut edits(&mut state.doc), ToolAction::NumberedList);
    wp.refresh_blocks(&state.doc);
    assert_eq!(kind(wp), ("list_item".to_owned(), true));
    wp.apply_tool(&mut edits(&mut state.doc), ToolAction::NumberedList);
    wp.refresh_blocks(&state.doc);
    assert_eq!(kind(wp).0, "paragraph", "pressed again, the list ends");
}

#[test]
fn the_window_bar_names_the_document() {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut().insert_str(
        &mut edits(&mut state.doc),
        "Quarterly report for the garden cooperative and its many volunteers",
    );
    let mut framed = mount(state, wp, 1280, 800);
    let (wp, _) = split(&mut framed);
    let label = wp.window_bar().location.map(|location| location.label);
    assert_eq!(
        label.as_deref(),
        Some("Quarterly report for the garden cooperative and its…"),
        "the bar names the document by its first words"
    );
}

fn with_doc(state: &mut ui::State, f: impl FnOnce(&mut tilo_doc::DocMut)) {
    let c = crate::model::init(&mut state.doc).unwrap();
    let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
    let mut dm = tilo_doc::DocMut {
        doc: &mut state.doc,
        c: &c,
        ids: &mut ids,
    };
    f(&mut dm);
}

static LARGE_BODY: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/large-body.automerge.hex"
    ))
    .split_ascii_whitespace()
    .map(|byte| {
        u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
    })
    .collect()
});

fn large_body() -> ui::State {
    let mut state = ui::State::load(LARGE_BODY.as_slice()).expect(
        "the large-body fixture loads; regenerate it with UPDATE_WP_LARGE_BODY=1 \
         cargo test -p word-processor --lib large_body_fixture",
    );
    ui::tilo_resource::evolution::migrate::<crate::framework::Document>(&mut state.doc)
        .expect("the large-body fixture migrates to the current storage version");
    state
}

fn build_large_body() -> ui::State {
    let mut state = crate::initialized_state();
    with_doc(&mut state, |dm| {
        let mut after = None;
        for i in 0..(VIEWPORT_BLOCK_WINDOW * 4) {
            let id = dm.insert_block(after.as_deref(), "paragraph");
            dm.splice_text(&id, 0, 0, &format!("Paragraph {i}"));
            after = Some(id);
        }
    });
    state
}

#[test]
fn large_body_fixture_holds_the_paragraphs_its_tests_expect() {
    if std::env::var_os("UPDATE_WP_LARGE_BODY").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/large-body.automerge.hex");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            build_large_body()
                .doc
                .save()
                .chunks(16)
                .map(|chunk| {
                    chunk
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                        + "\n"
                })
                .collect::<String>(),
        )
        .unwrap();
        eprintln!("updated {}", path.display());
        return;
    }
    let state = large_body();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    let blocks = wp.search_blocks(&state.doc);
    assert_eq!(blocks.len(), VIEWPORT_BLOCK_WINDOW * 4);
    for (i, block) in blocks.iter().enumerate() {
        assert_eq!(block.kind, "paragraph", "block {i}");
        assert_eq!(block.text, format!("Paragraph {i}"), "block {i}");
    }
}

#[test]
fn list_markers_indent_number_and_grow_by_level() {
    {
        let mut state = crate::initialized_state();
        with_doc(&mut state, |dm| {
            let a = dm.insert_block(None, "list_item");
            dm.splice_text(&a, 0, 0, "a");
            let b = dm.insert_block(Some(&a), "list_item");
            dm.splice_text(&b, 0, 0, "b");
            dm.set_attr(&b, "level", 1_i64);
            let cc = dm.insert_block(Some(&b), "list_item");
            dm.splice_text(&cc, 0, 0, "c");
            dm.set_attr(&cc, "level", 2_i64);
        });
        let wp = Editor::detached();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = split(&mut framed);

        let la = wp.laid.iter().find(|l| l.text == "a").unwrap();
        let lb = wp.laid.iter().find(|l| l.text == "b").unwrap();
        let lc = wp.laid.iter().find(|l| l.text == "c").unwrap();
        assert!(
            lb.text_x > la.text_x && lc.text_x > lb.text_x,
            "deeper levels indent further"
        );
        assert_eq!(la.bullet.as_deref(), Some("•"));
        assert_eq!(lb.bullet.as_deref(), Some("◦"));
        assert_eq!(lc.bullet.as_deref(), Some("▪"));
    }

    {
        let mut state = crate::initialized_state();
        with_doc(&mut state, |dm| {
            let mut prev: Option<String> = None;
            for (txt, lvl) in [
                ("one", 0i64),
                ("two", 0),
                ("two-a", 1),
                ("two-b", 1),
                ("three", 0),
            ] {
                let id = dm.insert_block(prev.as_deref(), "list_item");
                dm.splice_text(&id, 0, 0, txt);
                dm.set_attr(&id, "ordered", true);
                if lvl > 0 {
                    dm.set_attr(&id, "level", lvl);
                }
                prev = Some(id);
            }
        });
        let wp = Editor::detached();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = split(&mut framed);
        let num = |t: &str| {
            wp.laid
                .iter()
                .find(|l| l.text == t)
                .and_then(|l| l.bullet.clone())
        };
        assert_eq!(num("one").as_deref(), Some("1."));
        assert_eq!(num("two").as_deref(), Some("2."));
        assert_eq!(num("two-a").as_deref(), Some("1."));
        assert_eq!(num("two-b").as_deref(), Some("2."));
        assert_eq!(num("three").as_deref(), Some("3."));
    }

    {
        let text_x = 180.0;
        let one = list_marker_x(text_x, 13.0);
        let two = list_marker_x(text_x, 21.0);
        assert!(two < one, "the extra digit is allocated to the left gutter");
        assert_eq!(one + 13.0 + LIST_MARKER_TEXT_GAP, text_x);
        assert_eq!(two + 21.0 + LIST_MARKER_TEXT_GAP, text_x);
    }
}

#[test]
fn the_page_keys_move_the_caret_one_sheet_and_the_band_says_which() {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    for i in 0..120 {
        wp.model_mut().insert_str(
            &mut edits(&mut state.doc),
            &format!("Paragraph number {i} carries enough words to fill a line."),
        );
        wp.model_mut().enter(&mut edits(&mut state.doc));
    }
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = split(&mut framed);

    assert!(wp.pages > 1, "the document has {} page(s)", wp.pages);
    let last = wp.pages - 1;
    assert_eq!(wp.caret_page(), last, "the caret starts on the last sheet");
    assert_eq!(
        wp.page_readout(),
        format!("Page {} of {}", last + 1, wp.pages)
    );

    let end_of_document = wp.inner.caret().clone();
    wp.handle_key_press(
        &mut edits(&mut state.doc),
        &Key::Named(NamedKey::PageUp),
        None,
        false,
        false,
        &mut false,
    );
    assert_eq!(wp.caret_page(), last - 1, "one sheet back");
    assert_eq!(wp.page_readout(), format!("Page {last} of {}", wp.pages));
    assert_ne!(
        wp.inner.caret(),
        &end_of_document,
        "the caret moved to the sheet above"
    );
    wp.handle_key_press(
        &mut edits(&mut state.doc),
        &Key::Named(NamedKey::PageDown),
        None,
        false,
        false,
        &mut false,
    );
    assert_eq!(wp.caret_page(), last, "one sheet on");
    wp.handle_key_press(
        &mut edits(&mut state.doc),
        &Key::Named(NamedKey::PageDown),
        None,
        false,
        false,
        &mut false,
    );
    assert_eq!(wp.caret_page(), last, "there is no sheet below the last");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn long_documents_paginate_across_discrete_pages() {
    {
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        for i in 0..120 {
            wp.model_mut().insert_str(
                &mut edits(&mut state.doc),
                &format!("Paragraph number {i} carries enough words to fill a line."),
            );
            wp.model_mut().enter(&mut edits(&mut state.doc));
        }
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        let page_top = wp.zoom_viewport.y0 + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let pcb = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + letter_h - mar; // bottom margin
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        let max_page = wp.laid.iter().map(|l| page_of(l.y_top)).max().unwrap();
        assert!(
            max_page >= 1,
            "a long document must occupy more than one page (got {max_page})"
        );

        for l in &wp.laid {
            let i = num::usize_of_i64(page_of(l.y_top).max(0));
            assert!(
                l.y_top >= pct(i) - 1.0,
                "block starts at/below its page's top margin"
            );
            assert!(
                l.y_top + l.height <= pcb(i) + 1.0,
                "block ends above its page's bottom margin (no straddle)"
            );
        }

        let idx = wp
            .laid
            .iter()
            .position(|l| page_of(l.y_top) == 1)
            .expect("some block lands on page 2");
        assert!(idx > 0, "page 2 is preceded by page-1 content");
        assert!(
            (wp.laid[idx].y_top - pct(1)).abs() < 1.0,
            "the first page-2 block hugs the page-2 top margin"
        );
        assert_eq!(
            page_of(wp.laid[idx - 1].y_top),
            0,
            "the block before it is on page 1"
        );
    }

    {
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        let mut para = String::new();
        for i in 0..800 {
            write!(para, "word{i} ").unwrap();
        }
        let para = para.trim_end().to_string();
        wp.model_mut().insert_str(&mut edits(&mut state.doc), &para);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        let page_top = wp.zoom_viewport.y0 + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let pcb = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + letter_h - mar; // bottom margin
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        let bid = wp.blocks[0].id.clone();
        let slices: Vec<&Laid> = wp.laid.iter().filter(|l| l.id == bid).collect();
        assert!(
            slices.len() >= 2,
            "a too-tall paragraph splits into >=2 page slices (got {}, shaped height {}, page content height {})",
            slices.len(),
            wp.layout_cache[&bid].1.height(),
            letter_h - 2.0 * mar
        );
        assert_eq!(
            wp.layout_cache.len(),
            1,
            "the split block keeps exactly one cache entry"
        );
        assert!(
            wp.layout_cache.contains_key(&bid),
            "the cache entry is keyed by the single block id"
        );

        let s0 = slices[0];
        assert_eq!(page_of(s0.y_top), 0, "the first slice is on page 1");
        assert!(
            s0.y_top + s0.height <= pcb(0) + 1.0,
            "page-1 slice ends above the bottom margin (no straddle): bottom={} margin={}",
            s0.y_top + s0.height,
            pcb(0)
        );

        let s1 = slices[1];
        assert_eq!(page_of(s1.y_top), 1, "the carried-over slice is on page 2");
        assert!(
            (s1.y_top - pct(1)).abs() < 1.0,
            "the first carried line hugs the page-2 top margin: top={} margin={}",
            s1.y_top,
            pct(1)
        );

        assert!(
            s1.layout_y < s1.y_top,
            "the carried slice's layout origin sits above its page-2 band"
        );
        assert!(
            s1.clip.is_some(),
            "a split slice clips its drawing to its band"
        );
        assert!(
            s0.clip.is_some(),
            "the first slice of a split block also clips its band"
        );
    }

    {
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "First paragraph on page one.");
        wp.model_mut().enter(&mut edits(&mut state.doc));
        wp.model_mut().insert_str(
            &mut edits(&mut state.doc),
            "Second paragraph still on page one.",
        );
        wp.page_break_via_action(&mut edits(&mut state.doc));
        wp.model_mut().insert_str(
            &mut edits(&mut state.doc),
            "This paragraph is forced onto page two.",
        );
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        let page_top = wp.zoom_viewport.y0 + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        let broken = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("This paragraph is forced"))
            .expect("the broken block is laid out");
        assert_eq!(
            page_of(broken.y_top),
            1,
            "the page-break block lands on page 2"
        );
        assert!(
            (broken.y_top - pct(1)).abs() < 1.0,
            "it hugs the page-2 top margin (laid at a page top)"
        );

        let first = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("First paragraph"))
            .unwrap();
        let second = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("Second paragraph"))
            .unwrap();
        assert_eq!(
            page_of(first.y_top),
            0,
            "the first paragraph stays on page 1"
        );
        assert_eq!(
            page_of(second.y_top),
            0,
            "the second paragraph stays on page 1"
        );
    }
}

#[test]
fn caret_hit_on_a_carried_line_returns_the_right_offset() {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    let mut para = String::new();
    for i in 0..800 {
        write!(para, "word{i} ").unwrap();
    }
    let para = para.trim_end().to_string();
    wp.model_mut().insert_str(&mut edits(&mut state.doc), &para);
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, _) = split(&mut framed);

    let bid = wp.blocks[0].id.clone();
    let s1 = wp
        .laid
        .iter()
        .filter(|l| l.id == bid)
        .find(|l| l.page == 1)
        .cloned()
        .map(|l| (l.text_x, l.layout_y, l.y_top, l.height))
        .expect("the paragraph spills onto page 2");
    let (text_x, layout_y, band_top, band_h) = s1;

    let (key_byte, gx, gy) = {
        let (_, layout) = wp.layout_cache.get(&bid).expect("cached layout");
        let mut found = None;
        let mut byte = 0usize;
        while byte <= para.len() {
            let (cx, cy, _ch) = layout.caret_geometry(byte);
            let screen_y = layout_y + f64::from(cy);
            if screen_y >= band_top - 0.5 && screen_y < band_top + band_h {
                found = Some((byte, f64::from(cx), f64::from(cy)));
                break;
            }
            byte += 1;
            while byte < para.len() && !para.is_char_boundary(byte) {
                byte += 1;
            }
        }
        found.expect("some byte renders on the page-2 carried band")
    };
    let expected_char = byte_to_char(&para, key_byte);

    let click = Point::new(text_x + gx + 2.0, layout_y + gy + 4.0);
    assert!(
        click.y >= band_top - 0.5 && click.y < band_top + band_h,
        "click lands in the carried band"
    );
    wp.caret_from_pointer(click, false);

    assert_eq!(
        wp.inner.caret.block, bid,
        "hit resolves to the one logical block"
    );
    let got = num::i64_of_usize(wp.inner.caret.offset);
    assert!(
        (got - num::i64_of_usize(expected_char)).abs() <= 1,
        "carried-line hit resolves to the right offset: got {got}, expected ~{expected_char}"
    );
}

#[test]
fn page_fields_and_margins_resolve_per_page_and_persist() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        let c = wp.model().containers.clone().unwrap();
        let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let f = dm.insert_block_in(tilo_doc::model::Region::Footer, None, "paragraph");
            dm.splice_text_in(tilo_doc::model::Region::Footer, &f, 0, 0, "Page ");
            dm.insert_page_number_field(tilo_doc::model::Region::Footer, &f, 5);
        }
        for i in 0..120 {
            wp.model_mut().insert_str(
                &mut edits(&mut state.doc),
                &format!("Paragraph number {i} carries enough words to fill a line."),
            );
            wp.model_mut().enter(&mut edits(&mut state.doc));
        }
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        assert_eq!(
            wp.footer.len(),
            1,
            "the footer block is materialized into the widget"
        );
        let max_page = wp
            .laid
            .iter()
            .map(|l| {
                ((l.y_top + wp.scroll_y - (wp.zoom_viewport.y0 + PAGE_TOP))
                    / (MAX_PAGE_W * LETTER_RATIO + PAGE_GAP))
                    .floor() as i64
            })
            .max()
            .unwrap();
        assert!(
            max_page >= 1,
            "the document spans more than one page (got {max_page})"
        );

        let join = |page_no: usize| {
            running_head_runs(&wp.footer[0], page_no)
                .iter()
                .map(|r| r.text.clone())
                .collect::<String>()
        };
        assert_eq!(join(1), "Page 1", "page 1 footer resolves to 'Page 1'");
        assert_eq!(join(2), "Page 2", "page 2 footer resolves to 'Page 2'");
        assert!(
            !join(2).contains(tilo_doc::schema::FIELD_PLACEHOLDER),
            "the field placeholder is substituted, not rendered literally"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        assert!(
            (wp.margins.left - 72.0).abs() < 1e-6,
            "fresh doc uses default margins"
        );

        wp.margins = Edges {
            top: 33.0,
            right: 44.0,
            bottom: 55.0,
            left: 66.0,
        };
        wp.persist_margins(&mut edits(&mut state.doc));

        let mut wp2 = Editor::detached();
        wp2.sync(&state.doc, &state.root);
        assert!(
            (wp2.margins.top - 33.0).abs() < 1e-6,
            "top margin survived reload"
        );
        assert!(
            (wp2.margins.right - 44.0).abs() < 1e-6,
            "right margin survived reload"
        );
        assert!(
            (wp2.margins.bottom - 55.0).abs() < 1e-6,
            "bottom margin survived reload"
        );
        assert!(
            (wp2.margins.left - 66.0).abs() < 1e-6,
            "left margin survived reload"
        );
    }
}

fn header_text(wp: &Editor, state: &ui::State) -> Vec<String> {
    let c = wp.model().containers.clone().unwrap();
    tilo_doc::model::materialize_region(&state.doc, &c, tilo_doc::model::Region::Header)
        .into_iter()
        .map(|b| b.text)
        .collect()
}
fn body_text(wp: &Editor, state: &ui::State) -> Vec<String> {
    let c = wp.model().containers.clone().unwrap();
    tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .into_iter()
        .map(|b| b.text)
        .collect()
}

fn press_named(wp: &mut Editor, state: &mut ui::State, key: NamedKey) {
    press_named_with_modifiers(wp, state, key, Modifiers::default());
}

fn press_named_with_modifiers(
    wp: &mut Editor,
    state: &mut ui::State,
    key: NamedKey,
    mods: Modifiers,
) {
    let ev = ui::synthetic_input::key_event(Key::Named(key), None, ElementState::Pressed);
    Dispatch::new()
        .modifiers(mods)
        .doc(&mut state.doc)
        .path(&state.root)
        .send(&mut Lent(wp), &ui::synthetic_input::keyboard_input(ev));
}

#[test]
fn markdown_prefixes_autoformat_on_space() {
    let kind = |wp: &Editor, state: &ui::State| {
        let c = wp.model().containers.clone().unwrap();
        let block = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .into_iter()
            .find(|b| b.id == wp.model().caret().block)
            .unwrap();
        let level = match block.attrs.get("level") {
            Some(tilo_doc::AttrVal::Int(level)) => Some(*level),
            _ => None,
        };
        let ordered = block.flag("ordered");
        (block.kind, level, ordered, block.text)
    };
    for (typed, expected) in [
        ("-", ("list_item", None, false, "")),
        ("1.", ("list_item", None, true, "")),
        ("##", ("heading", Some(2), false, "")),
        ("a-", ("paragraph", None, false, "a- ")),
    ] {
        let (mut state, mut wp) = bound();
        seed_text(&mut wp, &mut state, typed);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        wp.lifecycle.focused = true;
        press_named(wp, state, NamedKey::Space);
        let (kind, level, ordered, text) = kind(wp, state);
        assert_eq!(
            (kind.as_str(), level, ordered, text.as_str()),
            expected,
            "{typed:?} then Space"
        );
    }
}

fn pinch(
    wp: &mut Editor,
    state: &mut ui::State,
    delta: f64,
    phase: winit::event::TouchPhase,
) -> bool {
    Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root)
        .send(
            &mut Lent(wp),
            &WindowEvent::PinchGesture {
                device_id: winit::event::DeviceId::dummy(),
                delta,
                phase,
            },
        )
        .handled
}

#[test]
fn mouse_selection_follows_the_pointer_across_lines_and_paragraphs() {
    {
        let (mut state, mut wp) = bound();
        wp.inner.insert_str(
            &mut edits(&mut state.doc),
            &"A paragraph with several words across visual lines. ".repeat(12),
        );
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        let laid = wp.laid[0].clone();
        let bands = wp.layout_cache[&laid.id].1.logical_line_bands(0);
        assert!(bands.len() > 2);
        for (top, _) in bands.iter().skip(1).take(3) {
            for delta in [-0.1, 0.1] {
                wp.caret_from_pointer(
                    Point::new(laid.text_x + 100.0, laid.layout_y + f64::from(*top) + delta),
                    true,
                );
                assert_eq!(wp.inner.caret.block, laid.id);
                let byte = char_to_byte(&laid.text, wp.inner.caret.offset);
                let (x, _, _) = wp.layout_cache[&laid.id].1.caret_geometry(byte);
                assert!(
                    (f64::from(x) - 100.0).abs() < 15.0,
                    "crossing a visual line boundary must retain the pointed column, got {x}"
                );
            }
        }
    }

    {
        let (mut state, mut wp) = bound();
        for i in 0..3 {
            if i != 0 {
                wp.inner.enter(&mut edits(&mut state.doc));
            }
            wp.inner
                .insert_str(&mut edits(&mut state.doc), "abcdefghijk");
        }
        let mut framed = mount(state, wp, 1000, 700);
        for zoom in [0.5, 1.0, 2.0] {
            editor(&mut framed).set_zoom(zoom);
            let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
            let first = wp.laid[0].clone();
            let second = wp.laid[1].clone();
            let gap_start = first.y_top + first.height;
            assert!(
                second.y_top > gap_start,
                "fixture needs real paragraph spacing"
            );
            let x = first.text_x + f64::from(wp.layout_cache[&first.id].1.caret_geometry(4).0);
            for (fraction, block) in [(0.25, &first.id), (0.75, &second.id)] {
                wp.inner.set_caret(Caret {
                    block: first.id.clone(),
                    offset: 1,
                });
                wp.inner.clear_anchor();
                let point = Point::new(x, gap_start + (second.y_top - gap_start) * fraction);
                let screen = wp.content_transform(Affine::IDENTITY) * point;
                wp.drag.press(screen, false);
                let mut handled = false;
                wp.handle_cursor_moved(screen.x, screen.y, 1.0, &mut handled);
                wp.drag.release();
                assert!(handled);
                assert_eq!(
                    &wp.inner.caret.block, block,
                    "gap hit must choose a neighbouring paragraph at {zoom}x"
                );
                assert_eq!(
                    wp.inner.caret.offset, 4,
                    "gap hit must preserve horizontal position"
                );
                assert_eq!(wp.inner.selection_anchor.as_ref().unwrap().offset, 1);
            }
        }
    }
}

#[test]
fn trackpad_pinch_zooms_smoothly_within_its_bounds() {
    {
        use winit::event::TouchPhase;
        let (state, mut wp) = populated();
        wp.set_zoom(1.0);
        wp.scroll_y = 100.0;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        wp.pointer = Point::new(500.0, 300.0);
        let before = wp.scroll_y + (wp.pointer.y - wp.zoom.anchor().y) / wp.zoom.factor();
        let heads = state.doc.get_heads();
        assert!(pinch(wp, state, 0.025, TouchPhase::Moved));
        assert!((wp.zoom.factor() - 1.025).abs() < 1e-9);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        let after = wp.scroll_y + (wp.pointer.y - wp.zoom.anchor().y) / wp.zoom.factor();
        assert!((before - after).abs() < 1e-6);
        assert!(!wp.view.auto_fit_page);
        assert_eq!(state.doc.get_heads(), heads);
        assert!(pinch(wp, state, -0.05, TouchPhase::Moved));
        assert!(wp.zoom.factor() < 1.025);
    }

    {
        use winit::event::TouchPhase;
        let (state, mut wp) = populated();
        wp.set_zoom(1.0);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        wp.pointer = Point::new(500.0, 300.0);
        for delta in [f64::NAN, f64::INFINITY] {
            pinch(wp, state, delta, TouchPhase::Moved);
            assert_eq!(wp.zoom.factor(), 1.0);
        }
        pinch(wp, state, 0.5, TouchPhase::Cancelled);
        assert_eq!(wp.zoom.factor(), 1.0);
        ask_to_replace(wp);
        assert!(pinch(wp, state, 0.5, TouchPhase::Moved));
        assert_eq!(wp.zoom.factor(), 1.0);
        wp.file_menu.close();
        wp.pointer = Point::new(20.0, 20.0);
        assert!(!pinch(wp, state, 0.5, TouchPhase::Moved));
        assert_eq!(wp.zoom.factor(), 1.0);
        wp.pointer = Point::new(500.0, 300.0);
        pinch(wp, state, 20.0, TouchPhase::Moved);
        assert_eq!(wp.zoom.factor(), ZOOM_MAX);
        pinch(wp, state, -2.0, TouchPhase::Moved);
        assert_eq!(wp.zoom.factor(), ZOOM_MIN);
    }
}

#[test]
fn native_text_shortcuts_move_by_words_and_lines() {
    {
        use winit::keyboard::ModifiersState;
        for (mods, expected) in [
            (ModifiersState::ALT | ModifiersState::SHIFT, "café"),
            (ModifiersState::SUPER | ModifiersState::SHIFT, "Hello café"),
            (ModifiersState::CONTROL | ModifiersState::SHIFT, "café"),
        ] {
            let (mut state, mut wp) = bound();
            wp.inner
                .insert_str(&mut edits(&mut state.doc), "Hello café");
            let mut framed = mount(state, wp, 1000, 700);
            let (wp, state) = split(&mut framed);
            wp.lifecycle.focused = true;
            ask_to_replace(wp);
            let heads = state.doc.get_heads();
            press_named(wp, state, NamedKey::Escape);
            press_named_with_modifiers(wp, state, NamedKey::ArrowLeft, mods.into());
            assert_eq!(wp.inner.selection_text(&state.doc), expected);
            assert_eq!(wp.inner.current_text(&state.doc), "Hello café");
            assert_eq!(
                state.doc.get_heads(),
                heads,
                "selection must not modify the document"
            );
        }
    }

    {
        use winit::keyboard::ModifiersState;
        for (text, selected) in [("hello   ", "  "), ("hello!!!", "!!")] {
            let (mut state, mut wp) = bound();
            wp.inner.insert_str(&mut edits(&mut state.doc), text);
            let mut framed = mount(state, wp, 1000, 700);
            let (wp, state) = split(&mut framed);
            wp.inner.caret.offset = 6;
            wp.lifecycle.focused = true;
            press_named_with_modifiers(
                wp,
                state,
                NamedKey::ArrowRight,
                (ModifiersState::ALT | ModifiersState::SHIFT).into(),
            );
            assert_eq!(wp.inner.caret.offset, 8);
            assert_eq!(wp.inner.selection_text(&state.doc), selected);
        }
    }

    {
        use winit::keyboard::ModifiersState;
        let (mut state, mut wp) = bound();
        wp.inner
            .insert_str(&mut edits(&mut state.doc), "東京京都大阪");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        wp.inner.caret.offset = 2;
        wp.lifecycle.focused = true;
        press_named_with_modifiers(
            wp,
            state,
            NamedKey::ArrowRight,
            (ModifiersState::ALT | ModifiersState::SHIFT).into(),
        );
        assert_eq!(wp.inner.selection_text(&state.doc), "京都");
    }
}

#[test]
fn native_text_shortcuts_delete_and_stop_at_visual_lines() {
    {
        for (modifier, expected) in [("alt", "Hello "), ("super", "")] {
            let (mut state, mut wp) = bound();
            wp.inner
                .insert_str(&mut edits(&mut state.doc), "Hello café");
            let containers = wp.inner.containers.clone().unwrap();
            wp.lifecycle.focused = true;
            let mut driver = ui::headless::FrameHarness::new(
                state,
                Box::new(WordProcessorWidget::over(wp)),
                (1000, 700),
            );
            driver.set_modifiers(&[modifier.into(), "shift".into()]);
            driver.key("ArrowLeft");
            driver.set_modifiers(&[]);
            driver.key("Backspace");
            assert_eq!(
                tilo_doc::model::materialize(&driver.state_mut().doc, &containers).blocks[0].text,
                expected
            );
        }
    }

    {
        use winit::keyboard::ModifiersState;
        for region in [Region::Body, Region::Header, Region::Footer] {
            let (mut state, mut wp) = bound();
            wp.inner.focus_region(&mut edits(&mut state.doc), region);
            let text = "A long line of text with several words. ".repeat(20);
            wp.inner.insert_str(&mut edits(&mut state.doc), &text);
            wp.inner.caret.offset = 2;
            wp.lifecycle.focused = true;
            let mut framed = mount(state, wp, 1000, 700);
            let (wp, state) = split(&mut framed);
            press_named_with_modifiers(
                wp,
                state,
                NamedKey::ArrowRight,
                (ModifiersState::SUPER | ModifiersState::SHIFT).into(),
            );
            assert!(
                wp.inner.caret.offset > 2 && wp.inner.caret.offset < text.chars().count(),
                "Command+Right must stop at a wrapped visual line, not the entire paragraph: {region:?}"
            );
            assert!(wp.inner.has_selection());
        }
    }
}

#[test]
fn clicking_header_band_routes_typing_into_the_header() {
    let (mut state, mut wp) = bound();
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "body text");
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = split(&mut framed);
    assert_eq!(
        wp.model().region(),
        Region::Body,
        "starts in the body context"
    );

    let hb = wp.head_band_rect;
    assert!(hb.height() > 0.0, "header band rect was cached by draw");
    let centre_content = Point::new(f64::midpoint(hb.x0, hb.x1), f64::midpoint(hb.y0, hb.y1));
    let screen = Point::new(
        wp.content_x_to_screen(centre_content.x),
        wp.zoom.anchor().y + (centre_content.y - wp.zoom.anchor().y) * wp.zoom.factor(),
    );

    left_click(wp, state, screen);
    assert_eq!(
        wp.model().region(),
        Region::Header,
        "clicking the header band focuses the Header context"
    );

    for ch in "My Title".chars() {
        type_char(wp, state, ch);
    }
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

    assert_eq!(
        header_text(wp, state),
        vec!["My Title".to_string()],
        "typed text landed in the header container"
    );
    assert_eq!(
        body_text(wp, state),
        vec!["body text".to_string()],
        "the body is untouched"
    );
    assert!(
        wp.region_laid.iter().any(|r| r.text == "My Title"),
        "the header band is laid out for the active context"
    );

    press_named(wp, state, NamedKey::Escape);
    assert_eq!(
        wp.model().region(),
        Region::Body,
        "Escape exits header editing"
    );
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
    wp.model_mut().move_end(&state.doc);
    for ch in "!".chars() {
        type_char(wp, state, ch);
    }
    assert_eq!(
        body_text(wp, state),
        vec!["body text!".to_string()],
        "after Escape, typing edits the body"
    );
    assert_eq!(
        header_text(wp, state),
        vec!["My Title".to_string()],
        "the header keeps its content"
    );
}

#[test]
fn header_edit_mode_renders_a_frame() {
    let (mut state, mut wp) = bound();
    wp.model_mut().insert_str(
        &mut edits(&mut state.doc),
        "The body paragraph recedes while the header is edited.",
    );
    wp.model_mut()
        .focus_region(&mut edits(&mut state.doc), Region::Header);
    for ch in "Draft — Confidential".chars() {
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), &ch.to_string());
    }
    let (w, h) = (1000u32, 700u32);
    let mut framed = mount(state, wp, w, h);
    let wp = editor(&mut framed);
    assert!(
        wp.region_laid.iter().any(|r| r.text.contains("Draft")),
        "header band laid for the active context"
    );

    let out = std::env::var("TILO_WP_HEADER_SHOT").unwrap_or_else(|_| {
        format!(
            "{}/header-edit-mode-{}.png",
            std::env::temp_dir().display(),
            std::process::id()
        )
    });
    ui::offscreen::OffscreenRenderer::new(w, h).render_png(
        Framed::harness(&framed).scene(),
        Color::from_rgb8(0x20, 0x22, 0x28),
        std::path::Path::new(&out),
    );
    let meta = std::fs::metadata(&out).expect("a PNG frame was written");
    assert!(
        meta.len() > 2000,
        "the rendered frame is a non-trivial PNG ({} bytes at {out})",
        meta.len()
    );
}

#[test]
fn zoom_scales_about_the_anchor_and_keeps_clicks_on_the_same_caret() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        seed_text(&mut wp, &mut state, "zoom me");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        let base = Affine::IDENTITY;
        let id_ctf = wp.content_transform(base);
        assert!(
            (id_ctf.as_coeffs()[0] - 1.0).abs() < 1e-9,
            "100% zoom does not scale"
        );

        assert!(wp.set_zoom(2.0));
        let ctf = wp.content_transform(base);
        let coeffs = ctf.as_coeffs();
        assert!(
            (coeffs[0] - 2.0).abs() < 1e-9 && (coeffs[3] - 2.0).abs() < 1e-9,
            "zoom doubles the scale"
        );
        let a = wp.zoom.anchor();
        let mapped_anchor = ctf * a;
        assert!(
            (mapped_anchor.x - a.x).abs() < 1e-6 && (mapped_anchor.y - a.y).abs() < 1e-6,
            "the anchor is fixed under zoom"
        );
        let p = Point::new(a.x, a.y + 100.0);
        let mp = ctf * p;
        assert!(
            (mp.y - (a.y + 200.0)).abs() < 1e-6,
            "content below the anchor is magnified downward"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        seed_text(&mut wp, &mut state, "first paragraph");
        wp.model_mut().enter(&mut edits(&mut state.doc));
        seed_text(&mut wp, &mut state, "second paragraph");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        let l2 = wp
            .laid
            .iter()
            .find(|l| l.text == "second paragraph")
            .expect("2nd block laid out");
        let target_block = l2.id.clone();
        let content_pt = Point::new(l2.text_x + 30.0, l2.y_top + l2.height / 2.0);

        wp.caret_from_pointer(content_pt, false);
        assert_eq!(
            wp.inner.caret.block, target_block,
            "baseline content-space click hits the 2nd block"
        );
        let off_unzoomed = wp.inner.caret.offset;

        assert!(wp.set_zoom(1.75));
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700); // recompute `zoom_anchor`
        let screen_pt = wp.content_transform(Affine::IDENTITY) * content_pt;
        let recovered = wp.pointer_to_content(screen_pt);
        assert!(
            (recovered.x - content_pt.x).abs() < 1e-6 && (recovered.y - content_pt.y).abs() < 1e-6,
            "inverse recovers the content point"
        );
        wp.caret_from_pointer(recovered, false);
        assert_eq!(
            wp.inner.caret.block, target_block,
            "zoomed click still hits the 2nd block"
        );
        assert_eq!(
            wp.inner.caret.offset, off_unzoomed,
            "zoomed click lands on the same caret offset"
        );
    }

    {
        let mut wp = Editor::detached();
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "default zoom is 100%"
        );
        for _ in 0..100 {
            wp.zoom_by(-ZOOM_STEP);
        }
        assert!(
            (wp.zoom.factor() - ZOOM_MIN).abs() < 1e-6,
            "zoom clamps to the minimum"
        );
        for _ in 0..100 {
            wp.zoom_by(ZOOM_STEP);
        }
        assert!(
            (wp.zoom.factor() - ZOOM_MAX).abs() < 1e-6,
            "zoom clamps to the maximum"
        );
        wp.set_zoom(1.0);
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "reset returns to 100%"
        );
    }
}

#[test]
fn the_zoom_field_drives_zoom_and_the_scroll_range() {
    {
        let state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        let field = wp.zoom_rect;
        assert!(field.width() > 0.0, "the zoom field is drawn");
        assert!(
            field.y0 >= wp.rect.y1 - wp.layout_bar_height,
            "the zoom field sits in the status band (field={field:?})"
        );
        let step = field.height() / 2.0;
        left_click(wp, state, Point::new(field.x1 - step, field.center().y));
        assert!(
            (wp.zoom.factor() - (1.0 + ZOOM_STEP)).abs() < 1e-6,
            "the + stepper zooms in one step: {}",
            wp.zoom.factor()
        );
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        let field = wp.zoom_rect;
        left_click(wp, state, Point::new(field.x0 + step, field.center().y));
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-6,
            "the − stepper zooms out one step: {}",
            wp.zoom.factor()
        );
    }

    {
        crate::i18n::set_locale("en-US");
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 1100, 760);
        editor(&mut framed).set_zoom(1.0);
        let (wp, state) = pipeline_draw(&mut framed, 1100, 760);

        let tree = wp.accessibility_tree();
        let toolbar = tree
            .children
            .iter()
            .find(|child| child.node.role == ui::a11y::Role::Toolbar)
            .expect("formatting controls");
        let field = &toolbar
            .children
            .iter()
            .find(|child| child.node.role == ui::a11y::Role::SpinButton)
            .expect("admitted shared zoom control")
            .node;
        assert_eq!(field.role, ui::a11y::Role::SpinButton);
        assert_eq!(field.value.as_deref(), Some("100%"));
        assert_eq!(
            field.bounds.map(ui::a11y::AccessBounds::to_rect),
            Some(wp.zoom_rect)
        );
        assert!(Widget::perform_action(
            &mut wp.zoom_input,
            &ui::a11y::Action::Increment,
            &[],
            &mut edits(&mut state.doc),
            &state.root
        ));
        wp.drain_hosted_fields(&mut edits(&mut state.doc));
        assert!((wp.zoom.factor() - 1.1).abs() < 1e-9);
        assert!(Widget::perform_action(
            &mut wp.zoom_input,
            &ui::a11y::Action::SetValue("100".into()),
            &[],
            &mut edits(&mut state.doc),
            &state.root
        ));
        wp.drain_hosted_fields(&mut edits(&mut state.doc));
        assert_eq!(wp.zoom.factor(), 1.0, "semantic reset returns to 100%");
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        for i in 0..40 {
            wp.model_mut().insert_str(
                &mut edits(&mut state.doc),
                &format!("Paragraph {i} with several words to fill the line."),
            );
            wp.model_mut().enter(&mut edits(&mut state.doc));
        }
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        let scroll_1x = wp.max_scroll;
        assert!(scroll_1x > 0.0, "the document already overflows at 100%");
        wp.set_zoom(2.0);
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert!(
            wp.max_scroll > scroll_1x,
            "zooming in enlarges the content, so there is more to scroll"
        );
    }
}

#[test]
fn the_page_starts_at_the_top_with_no_toolbar_row() {
    for width in [760, 1800] {
        let (state, mut wp) = populated();
        wp.stage_find_for_shot(&state.doc, "text");
        let mut framed = mount(state, wp, width, 700);
        let wp = editor(&mut framed);
        assert!(
            wp.zoom_viewport.y0 <= wp.rect.y0 + wp.ruler.rect().height() + 0.5,
            "{width}px: the writing page starts under the ruler, {:?}",
            wp.zoom_viewport
        );
    }
}

#[test]
fn enlarged_and_phone_layouts_keep_controls_fitting() {
    {
        let (state, wp) = populated();
        let theme = Theme::dark().with_text_scale(2.0);
        let mut wp = mount_themed(state, wp, 480, 600, &theme);
        let wp = editor(&mut wp);
        for (_, rect) in &wp.layout_chips {
            assert_eq!(rect.intersect(wp.rect), *rect, "layout modes are whole");
        }
        assert_eq!(
            wp.zoom_rect.intersect(wp.rect),
            wp.zoom_rect,
            "zoom is whole"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "A readable large-text document");
        let theme = Theme::dark().with_text_scale(2.0);
        let mut wp = mount_themed(state, wp, 390, 844, &theme);
        let wp = editor(&mut wp);
        assert_eq!(
            wp.zoom.factor(),
            2.0,
            "the editing surface follows 200% text"
        );
        assert!(
            wp.page_rect.width() * wp.zoom.factor() <= 390.0,
            "the readable 200% paper still fits the viewport"
        );
        assert!(
            wp.layout_chips.iter().all(|(_, rect)| rect.x1 <= 390.0),
            "no bottom control is half-visible at the right edge"
        );
    }
}

#[test]
fn narrow_and_zoomed_windows_keep_status_and_page_fitting() {
    {
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 700, 900);
        for (width, zoom) in [(700_u32, 0.81), (1100, 1.0)] {
            editor(&mut framed).view.auto_fit_page = false;
            editor(&mut framed).zoom.set(zoom);
            let (wp, _) = pipeline_draw(&mut framed, width, 900);
            let band_top = wp.rect.y1 - wp.layout_bar_height;
            for rect in wp
                .layout_chips
                .iter()
                .map(|(_, r)| *r)
                .chain([wp.zoom_rect])
            {
                assert!(
                    rect.y0 >= band_top && rect.y1 <= wp.rect.y1 && rect.x1 <= wp.rect.x1,
                    "{width}px: {rect:?} is inside the status band"
                );
            }
        }
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Readable narrow document");
        let mut framed = mount(state, wp, 640, 800);
        let (wp, _) = split(&mut framed);
        assert!(
            wp.zoom.factor() < 1.0,
            "narrow canvas automatically fits paper"
        );

        let ctf = wp.content_transform(Affine::IDENTITY);
        let left = ctf * Point::new(wp.page_rect.x0, wp.page_rect.y0);
        let right = ctf * Point::new(wp.page_rect.x1, wp.page_rect.y0);
        assert!(
            left.x >= wp.rect.x0 - 1.0,
            "left paper edge remains visible"
        );
        assert!(
            right.x <= wp.rect.x1 + 1.0,
            "right paper edge remains visible"
        );

        wp.set_zoom(1.0);
        let (wp, _) = pipeline_draw(&mut framed, 640, 800);
        assert_eq!(wp.zoom.factor(), 1.0, "explicit zoom disables auto-fit");
    }
}

#[test]
fn chrome_never_crowds_the_document_canvas() {
    {
        for (w, h) in [(1400_u32, 900_u32), (960, 800), (640, 700), (426, 700)] {
            let (state, mut wp) = bound();
            wp.view.styles_panel_visible = true;
            let mut framed = mount(state, wp, w, h);
            let (wp, _) = split(&mut framed);
            assert!(
                wp.view.styles_panel_visible,
                "precondition: the explicitly-open panel is the one under test",
            );
            assert!(
                !wp.outline.is_open(),
                "precondition: the left-hand outline pane is closed, so the \
                 canvas is bounded only by the panel under test",
            );
            if ui::widget::modal_dialog::uses_full_window(wp.rect, &Theme::dark()) {
                assert!(wp.styles_panel_full_window());
                assert_eq!(
                    wp.panel_rect, wp.rect,
                    "a phone uses the entire foreground page"
                );
                continue;
            }
            let panel = wp.panel_rect.width();
            let canvas = if wp.styles_panel_overlay() {
                wp.rect.width()
            } else {
                wp.rect.width() - panel
            };
            assert!(
                canvas > panel,
                "at {w}x{h} the canvas left for the document ({canvas:.0}px) \
                 must be wider than the styles panel ({panel:.0}px) — an \
                 editor whose subject is the smaller half of its own window \
                 is showing the wrong thing",
            );
        }
    }
}

#[test]
fn spellcheck_marks_misspellings_per_language_and_caches_by_block() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        seed_text(&mut wp, &mut state, "teh quick brown fox");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);

        let l = wp
            .laid
            .iter()
            .find(|l| l.text == "teh quick brown fox")
            .expect("paragraph laid out");
        let id = l.id.clone();
        let text = l.text.clone();

        let spans = wp.block_spell_spans(&id, &text);
        assert_eq!(
            spans,
            vec![(0usize, 3usize)],
            "exactly ‘teh’ at bytes [0,3) is flagged"
        );

        let (_, layout) = wp
            .layout_cache
            .get(&id)
            .expect("block layout cached after draw");
        let rects = layout.selection_rects(0, 3);
        assert!(
            !rects.is_empty(),
            "the misspelled span resolves to at least one rect"
        );
        let (x0, _, x1, _) = rects[0];
        assert!(
            x1 > x0,
            "the squiggle covers a non-empty width: x0={x0} x1={x1}"
        );
        assert!(
            x0 < 4.0,
            "the first word's squiggle starts at the line's left edge: x0={x0}"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        seed_text(&mut wp, &mut state, "teh");
        assert!(wp.spellcheck_enabled(), "spell-check defaults ON");
        wp.toggle_spellcheck();
        assert!(!wp.spellcheck_enabled(), "toggle turns it OFF");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        assert!(
            wp.dict.is_none(),
            "spell-check off skips dictionary construction"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        seed_text(&mut wp, &mut state, "teh");

        let correction = wp
            .autocorrect_before_boundary(&mut edits(&mut state.doc))
            .expect("‘teh’ is a high-confidence correction");
        seed_text(&mut wp, &mut state, " ");
        wp.last_autocorrection = Some(DocumentAutocorrection {
            boundary_end: wp.inner.caret.offset,
            ..correction
        });
        assert_eq!(wp.inner.current_text(&state.doc), "the ");

        assert!(wp.undo_document_autocorrection(&mut edits(&mut state.doc)));
        assert_eq!(wp.inner.current_text(&state.doc), "teh");
    }

    {
        let mut wp = Editor::detached();
        let english = wp.block_spell_spans("b", "schön grün");
        assert!(
            !english.is_empty(),
            "precondition: the English checker flags the German words, got {english:?}"
        );
        wp.set_spell_language("de_DE");
        assert_eq!(
            wp.block_spell_spans("b", "schön grün"),
            Vec::<(usize, usize)>::new(),
            "German words are correct in a German session"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        seed_text(&mut wp, &mut state, "wrold");
        let id = wp.blocks[0].id.clone();

        let spans = wp.block_spell_spans(&id, "wrold");
        assert_eq!(spans, vec![(0, 5)], "‘wrold’ flagged");
        assert_eq!(wp.block_spell_spans(&id, "wrold"), vec![(0, 5)]);
        assert!(
            wp.block_spell_spans(&id, "world").is_empty(),
            "the corrected text re-checks clean"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn typing_and_clipboard_edit_the_document_through_one_action() {
    const CHILD: &str = "TILO_WRITER_CLIPBOARD_TEST_CHILD";
    #[derive(Default)]
    struct TestClipboard(std::sync::Mutex<Option<String>>);
    impl ui::clipboard::ClipboardBackend for TestClipboard {
        fn set_text(&self, _: ui::clipboard::ClipboardTarget, text: &str) -> bool {
            *self.0.lock().unwrap() = Some(text.to_string());
            true
        }

        fn text(&self, _: ui::clipboard::ClipboardTarget) -> Option<String> {
            self.0.lock().unwrap().clone()
        }

        fn name(&self) -> &'static str {
            "writer-clipboard-test"
        }
    }
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "widget::tests::typing_and_clipboard_edit_the_document_through_one_action",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output_locked()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated clipboard test failed:
{}
{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "alpha");
        wp.model_mut().enter(&mut edits(&mut state.doc));
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "beta");
        let ids =
            tilo_doc::model::blocks_in_order(&state.doc, &crate::model::open(&state.doc).unwrap());
        wp.model_mut().set_caret(Caret {
            block: ids[0].clone(),
            offset: 2,
        });
        wp.model_mut().set_anchor();
        wp.model_mut().set_caret(Caret {
            block: ids[1].clone(),
            offset: 2,
        });

        wp.insert_text_via_action(&mut edits(&mut state.doc), "X");

        let action = wp
            .last_dispatched_for_test()
            .cloned()
            .expect("action dispatched");
        assert!(matches!(
            action,
            crate::actions::RichTextAction::ReplaceSelection { .. }
        ));
        let blocks =
            tilo_doc::model::materialize(&state.doc, &crate::model::open(&state.doc).unwrap())
                .blocks;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "alXta");
    }

    {
        let services = ui::host::HostServices::null()
            .with_clipboard(std::sync::Arc::new(TestClipboard::default()));
        let _host = ui::host::enter(&services);
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);

        clipboard_copy("sentinel");
        wp.clipboard_copy(&state.doc);
        assert_eq!(clipboard_paste().as_deref(), Some("sentinel"));

        wp.inner.insert_str(&mut edits(&mut state.doc), "hello");

        wp.inner.caret.offset = 0;
        wp.inner.set_anchor();
        wp.inner.caret.offset = 5;
        wp.dispatch_toggle_mark(&mut edits(&mut state.doc), "strong");
        wp.inner.caret.offset = 0;
        wp.inner.clear_anchor();
        wp.inner.set_anchor();
        wp.inner.caret.offset = 5;
        wp.clipboard_copy(&state.doc);
        assert_eq!(clipboard_paste().as_deref(), Some("hello"));

        wp.inner.clear_anchor();
        wp.inner.move_end(&state.doc);
        wp.clipboard_paste(&mut edits(&mut state.doc));
        let containers = wp.inner.containers.clone().expect("bound document");
        let block = wp.inner.caret.block.clone();
        assert_eq!(wp.inner.current_text(&state.doc), "hellohello");
        assert!(tilo_doc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            5,
            10,
            "strong"
        ));

        wp.inner.enter(&mut edits(&mut state.doc));
        wp.clipboard_paste_plain(&mut edits(&mut state.doc));
        let block = wp.inner.caret.block.clone();
        assert_eq!(wp.inner.current_text(&state.doc), "hello");
        assert!(!tilo_doc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            0,
            5,
            "strong"
        ));

        clipboard_copy("external");
        wp.clipboard_paste(&mut edits(&mut state.doc));
        assert_eq!(wp.inner.current_text(&state.doc), "helloexternal");
        assert!(!tilo_doc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            5,
            13,
            "strong"
        ));
    }

    {
        const TYPED: &str = "Hello World";

        let (mut state, mut wp) = bound();
        assert_eq!(
            body_text(&wp, &state).concat(),
            "",
            "a fresh document starts empty, so the assertion below is about \
             what typing added and nothing else",
        );

        for ch in TYPED.chars() {
            type_char(&mut wp, &mut state, ch);
        }

        assert_eq!(
            body_text(&wp, &state).concat(),
            TYPED,
            "the document holds exactly the characters that were typed, \
             capitals included",
        );
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "move");
        wp.lifecycle.focused = true;
        let heads = state.doc.get_heads();
        let before = wp.model().caret().offset;
        let mut handled = false;

        wp.handle_key_press(
            &mut edits(&mut state.doc),
            &Key::Named(NamedKey::ArrowLeft),
            None,
            false,
            false,
            &mut handled,
        );

        assert!(handled);
        assert_eq!(wp.model().caret().offset, before - 1);
        assert_eq!(
            state.doc.get_heads(),
            heads,
            "caret motion is not a content edit"
        );
    }
}

fn first_block_start_point(wp: &Editor) -> Point {
    let l = wp.laid.first().expect("at least one laid block");
    Point::new(l.text_x + 1.0, l.y_top + l.height / 2.0)
}

#[test]
fn context_menus_preserve_the_selection_and_capture_editing_keys() {
    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Hello world");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        wp.inner.select_all(&state.doc);
        let selected = wp.inner.selection_text(&state.doc);
        let caret = wp.inner.caret.clone();
        let anchor = wp.inner.selection_anchor.clone();
        let point = first_block_start_point(wp);
        wp.open_context_menu(&state.doc, point, [300.0, 300.0]);
        assert_eq!(wp.inner.selection_text(&state.doc), selected);
        assert_eq!(wp.inner.caret, caret);
        assert_eq!(wp.inner.selection_anchor, anchor);
        assert!(wp.context_actions.contains(&Some(CtxAction::Copy)));
        assert!(wp.context_actions.contains(&Some(CtxAction::Cut)));
    }

    {
        for style_menu in [false, true] {
            let (mut state, mut wp) = bound();
            wp.model_mut()
                .insert_str(&mut edits(&mut state.doc), "Keep my draft");
            let mut framed = mount(state, wp, 520, 700);
            let (wp, state) = split(&mut framed);
            wp.lifecycle.focused = true;
            wp.inner.select_all(&state.doc);
            let style_count = wp.sheet.order().len();
            if style_menu {
                wp.style_menu.open("body", Point::new(300.0, 300.0));
            }
            if !style_menu {
                ask_to_replace(editor(&mut framed));
            }
            let (wp, state) = split(&mut framed);
            assert_eq!(
                wp.inner.selection_text(&state.doc),
                "Keep my draft",
                "settling and opening the menu preserves the selected draft"
            );
            assert!(
                wp.style_menu.is_open() || wp.file_menu.is_open(),
                "the editing-key isolation subject is an actually open menu"
            );
            press_named(wp, state, NamedKey::Backspace);
            assert_eq!(
                first_block(wp, state).map(|b| b.text),
                Some("Keep my draft".into())
            );
            if style_menu {
                press_named(wp, state, NamedKey::ArrowDown);
            } else {
                let keep = wp.file_menu.cancel_row();
                for _ in 0..5 {
                    if wp.file_menu.menu_mut().hovered() == keep {
                        break;
                    }
                    press_named(wp, state, NamedKey::ArrowDown);
                }
            }
            press_named(wp, state, NamedKey::Enter);
            assert!(!wp.context_menu.is_open());
            assert!(!wp.style_menu.is_open());
            assert!(!wp.file_menu.is_open(), "Enter chose a command");
            if style_menu {
                assert_eq!(
                    wp.sheet.order().len(),
                    style_count + 1,
                    "Enter duplicates the style"
                );
            }
            assert_eq!(
                first_block(wp, state).map(|b| b.text),
                Some("Keep my draft".into())
            );
        }
    }
}

#[test]
fn context_actions_stay_in_the_viewport() {
    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Hello world");
        let mut framed = mount(state, wp, 600, 400);
        let (wp, state) = split(&mut framed);
        let point = first_block_start_point(wp);
        wp.open_context_menu(&state.doc, point, [598.0, 398.0]);
        let mut scene = Scene::new();
        wp.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark().with_text_scale(2.0),
            &PresenceCtx::empty(),
        );
        for (index, action) in wp.context_actions.iter().enumerate() {
            if action.is_some() {
                let row = wp
                    .context_menu
                    .item_rect(index)
                    .expect("menu row remains reachable");
                assert!(
                    row.x0 >= 0.0 && row.y0 >= 0.0 && row.x1 <= 600.0 && row.y1 <= 400.0,
                    "row outside viewport: {row:?}"
                );
            }
        }
    }
}

#[test]
fn host_undo_and_right_click_leave_an_open_menus_selection_alone() {
    {
        for menu_kind in 0..2 {
            let (mut state, mut wp) = bound();
            wp.inner
                .insert_str(&mut edits(&mut state.doc), "Keep this edit");
            let mut framed = mount(state, wp, 520, 700);
            match menu_kind {
                0 => ask_to_replace(editor(&mut framed)),
                _ => editor(&mut framed)
                    .style_menu
                    .open("body", Point::new(300.0, 300.0)),
            }
            let driver = Framed::harness_mut(&mut framed);
            driver.relayout();
            let (window, _) = Framed::split(&mut framed);
            assert!(
                ui::undo::handle_local_undo_request(window, ui::undo::UndoRequest::Undo),
                "host undo must be captured while a menu is open"
            );
            let wp = window.editor_mut();
            wp.file_menu.close();
            wp.context_menu.close();
            wp.style_menu.close();
            assert!(
                !ui::undo::handle_local_undo_request(window, ui::undo::UndoRequest::Undo),
                "ordinary undo resumes after dismissal"
            );
        }
    }

    {
        let (mut state, mut wp) = bound();
        wp.inner
            .insert_str(&mut edits(&mut state.doc), "Hello world");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        wp.inner.select_all(&state.doc);
        let mut point = first_block_start_point(wp);
        point.x += 400.0; // hit testing clamps the text's right edge to its end offset
        wp.open_context_menu(&state.doc, point, [500.0, 300.0]);
        assert_eq!(wp.inner.selection_text(&state.doc), "Hello world");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn right_click_spelling_offers_dictionary_suggestions_and_copy() {
    {
        crate::i18n::set_locale("en-US");
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "qwordz quick");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        let id = wp.blocks[0].id.clone();
        assert_eq!(
            wp.block_spell_spans(&id, "qwordz quick"),
            vec![(0usize, 6usize)],
            "precondition: ‘qwordz’ is flagged"
        );

        let pt = first_block_start_point(wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        let labels = wp.context_menu_labels();
        assert!(
            labels.iter().any(|l| l == "Add to dictionary"),
            "misspelled word offers ‘Add to dictionary’, got {labels:?}"
        );

        let add_idx = wp
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Add to dictionary"))
            .unwrap();
        assert_eq!(
            wp.context_actions[add_idx],
            Some(CtxAction::AddToDictionary("qwordz".to_string()))
        );
        wp.dispatch_context_action(add_idx, &mut edits(&mut state.doc));
        assert!(!wp.context_menu.is_open(), "menu closed after dispatch");

        assert!(
            wp.block_spell_spans(&id, "qwordz quick").is_empty(),
            "after Add to dictionary, ‘qwordz’ is no longer flagged"
        );
        assert!(
            wp.dictionary().contains("qwordz"),
            "the word is recorded in the (owned) dictionary"
        );
        assert_eq!(
            wp.proofing.personal_words.dictionary_words,
            ["qwordz"],
            "the word joins the person's own words"
        );
    }

    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "teh end");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        let pt = first_block_start_point(wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        let the_idx = wp.context_actions.iter().position(|a| {
            matches!(a, Some(CtxAction::ApplySuggestion { replacement, .. }) if replacement == "the")
        });
        let Some(idx) = the_idx else {
            panic!(
                "expected a ‘the’ suggestion row, labels: {:?}",
                wp.context_menu_labels()
            );
        };
        wp.dispatch_context_action(idx, &mut edits(&mut state.doc));
        let dispatched = wp
            .last_dispatched_for_test()
            .expect("the suggestion dispatched an action");
        assert!(matches!(
            dispatched,
            crate::actions::RichTextAction::SpliceText { at: 0, del: 3, ins, .. } if ins == "the"
        ));
        let id = wp.blocks[0].id.clone();
        let text = wp.model().block_texts(&state.doc);
        assert_eq!(
            text[0], "the end",
            "suggestion replaced ‘teh’ with ‘the’ (block {id})"
        );
    }

    {
        crate::i18n::set_locale("en-US");
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "hello world");
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        assert!(
            !wp.model().has_selection(),
            "precondition: nothing selected"
        );

        let pt = first_block_start_point(wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        assert!(
            wp.model().has_selection(),
            "a word was selected by the fallback"
        );
        assert_eq!(wp.model().selection_text(&state.doc), "hello");
        let labels = wp.context_menu_labels();
        assert!(labels.contains(&"Copy".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Cut".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Select all".to_string()), "got {labels:?}");

        let copy_idx = wp
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Copy"))
            .unwrap();
        assert_eq!(wp.context_actions[copy_idx], Some(CtxAction::Copy));
    }
}

fn author_doc_with_footnote(body: &str, ref_idx: usize, note_body: &str) -> ui::State {
    let mut state = crate::initialized_state();
    let c = crate::model::init(&mut state.doc).unwrap();
    let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, body);
        let note_id = dm.insert_footnote(&p, ref_idx);
        let nb = dm
            .insert_note_block(&note_id, None, "paragraph")
            .expect("note block");
        dm.splice_note_text(&note_id, &nb, 0, 0, note_body);
    }
    state.doc.commit();
    state
}

#[test]
fn footnotes_render_in_the_page_area_numbered_in_document_order() {
    {
        let state = author_doc_with_footnote("Hello world", 5, "A clarifying footnote.");
        let wp = Editor::detached();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = split(&mut framed);

        assert_eq!(wp.notes.len(), 1, "one note materialized");
        assert_eq!(
            wp.notes[0].kind,
            tilo_doc::schema::NOTE_KIND_FOOTNOTE,
            "it is a footnote"
        );

        let body = wp
            .blocks
            .iter()
            .find(|b| b.text.contains("Hello"))
            .expect("body block");
        let refs = block_note_refs(body);
        assert_eq!(refs.len(), 1, "one note-ref marker in the body");
        let (ref_ci, note_id) = &refs[0];
        assert_eq!(
            *ref_ci, 5,
            "marker sits at the reference index (the placeholder char)"
        );
        let nums = note_numbering(&wp.blocks);
        assert_eq!(
            nums.get(note_id.as_str()),
            Some(&1),
            "first-referenced note is numbered 1"
        );

        let text = note_text(&wp.notes[0]);
        assert_eq!(
            text, "A clarifying footnote.",
            "the footnote body text is materialized"
        );
        let rendered_line = format!("{}. {}", nums[note_id.as_str()], text);
        assert_eq!(
            rendered_line, "1. A clarifying footnote.",
            "the page note-area line is well-formed"
        );
    }

    {
        let mut state = crate::initialized_state();
        let c = crate::model::init(&mut state.doc).unwrap();
        let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        let (n_first, n_second);
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "alpha beta");
            n_second = dm.insert_footnote(&p, 10);
            n_first = dm.insert_footnote(&p, 5);
            let b2 = dm.insert_note_block(&n_second, None, "paragraph").unwrap();
            dm.splice_note_text(&n_second, &b2, 0, 0, "second");
            let b1 = dm.insert_note_block(&n_first, None, "paragraph").unwrap();
            dm.splice_note_text(&n_first, &b1, 0, 0, "first");
        }
        state.doc.commit();

        let wp = Editor::detached();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = split(&mut framed);

        let nums = note_numbering(&wp.blocks);
        assert_eq!(
            nums.get(n_first.as_str()),
            Some(&1),
            "the earlier marker is #1 by document order"
        );
        assert_eq!(
            nums.get(n_second.as_str()),
            Some(&2),
            "the later marker is #2"
        );
    }
}

fn doc_with_paragraph(
    text: &str,
) -> (
    Editor,
    ui::State,
    tilo_doc::Containers,
    tilo_doc::IdGen,
    String,
) {
    let mut state = crate::initialized_state();
    let c = crate::model::init(&mut state.doc).unwrap();
    let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
    let bid;
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        bid = dm.insert_block(None, "paragraph");
        dm.splice_text(&bid, 0, 0, text);
    }
    state.doc.commit();
    let wp = Editor::detached();
    (wp, state, c, ids, bid)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn comments_highlight_open_threads_and_delete_cleanly() {
    {
        let (wp, mut state, _, _, bid) = doc_with_paragraph("hello world");
        crate::model::comment_by(&mut state.doc, "Ada", &bid, (6, 11), "Reword this");
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = split(&mut framed);

        assert_eq!(
            wp.review.comments().len(),
            1,
            "the comment is materialized into the widget"
        );
        assert_eq!(wp.review.comments()[0].author, "Ada");

        let b = wp
            .blocks
            .iter()
            .find(|b| b.id == bid)
            .expect("block present");
        let ranges = block_comment_ranges(b);
        assert_eq!(ranges.len(), 1, "one comment range over the block");
        let (bs, be, cid) = &ranges[0];
        assert_eq!((*bs, *be), (6, 11), "byte range covers 'world'");
        assert_eq!(
            cid,
            &wp.review.comments()[0].id,
            "range carries the comment id"
        );

        let (_, lay) = wp.layout_cache.get(&bid).expect("shaped layout cached");
        let rects = lay.selection_rects(*bs, *be);
        assert!(
            !rects.is_empty(),
            "the commented range computes a highlight rect"
        );
        assert!(
            rects[0].2 > rects[0].0,
            "the highlight rect has positive width"
        );
    }

    {
        let (wp, mut state, c, _, bid) = doc_with_paragraph("hello world");
        let comment = crate::model::comment_by(
            &mut state.doc,
            "Ada",
            &bid,
            (0, 5),
            "Could this be clearer?",
        );
        crate::model::reply_by(
            &mut state.doc,
            &comment,
            "Bea",
            "2026-09-07",
            "I will revise it.",
        );
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);
        assert_eq!(wp.review.comments()[0].replies.len(), 1);

        wp.dispatch_review_action(
            ReviewAction::OpenCommentThread(comment.clone()),
            &mut edits(&mut state.doc),
        );
        assert_eq!(wp.thread_comment.as_deref(), Some(comment.as_str()));
        assert!(
            wp.thread_snapshot.is_none(),
            "interactive panel reads current review data"
        );

        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        {
            let mut tr = TextRenderer::new();
            wp.draw_overlay(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
        }
        for ch in "Done".chars() {
            type_char(wp, state, ch);
        }
        press_named(wp, state, NamedKey::Enter);
        for ch in "Thanks".chars() {
            type_char(wp, state, ch);
        }
        assert_eq!(
            wp.thread_reply.widget().source().plain_text(),
            "Done\nThanks"
        );
        press_named_with_modifiers(
            wp,
            state,
            NamedKey::Enter,
            Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
        );
        let after = snapshot(state, &c);
        let thread = after.comments.iter().find(|cm| cm.id == comment).unwrap();
        assert_eq!(thread.replies.len(), 2, "the reply was sent");
        let reply = thread
            .replies
            .iter()
            .find(|reply| reply.author != "Bea")
            .unwrap();
        let sent: Vec<&str> = reply
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(sent, ["Done", "Thanks"], "sent as its paragraphs");
        assert!(
            wp.thread_reply.widget().source().is_empty(),
            "the field clears"
        );
        assert_eq!(wp.thread_comment.as_deref(), Some(comment.as_str()));

        press_named(wp, state, NamedKey::Escape);
        assert!(wp.thread_comment.is_none());
    }

    {
        let (wp, mut state, c, _, bid) = doc_with_paragraph("hello world");
        let cid = crate::model::comment_by(&mut state.doc, "Ada", &bid, (6, 11), "Reword this");
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = split(&mut framed);

        let before = snapshot(state, &c);
        assert!(
            before.comments.iter().any(|cm| cm.id == cid),
            "the comment exists before resolve"
        );
        let b = before.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            block_comment_ranges(b).len(),
            1,
            "the highlight range exists before resolve"
        );

        wp.dispatch_review_action(
            ReviewAction::ResolveComment(cid.clone()),
            &mut edits(&mut state.doc),
        );

        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert!(
            !after.comments.iter().any(|cm| cm.id == cid),
            "the comment is removed from the snapshot after resolve"
        );
        assert!(
            block_comment_ranges(b).is_empty(),
            "the comment highlight is cleared after resolve"
        );
        assert_eq!(
            b.text, "hello world",
            "resolving a comment leaves the commented text intact"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn tracked_changes_render_and_accept_individually() {
    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let (ins_id, del_id);
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            ins_id = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 0, 4, &ins_id);
            del_id = dm.track_delete(&bid, 5, 9, "Ada", "");
        }
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = split(&mut framed);

        assert_eq!(
            wp.review.changes().len(),
            2,
            "both tracked changes materialized"
        );
        let ins = wp
            .review
            .changes()
            .iter()
            .find(|ch| ch.id == ins_id)
            .expect("ins change");
        let del = wp
            .review
            .changes()
            .iter()
            .find(|ch| ch.id == del_id)
            .expect("del change");
        assert_eq!(ins.kind, tilo_doc::ChangeKind::Ins);
        assert_eq!(del.kind, tilo_doc::ChangeKind::Del);

        let b = wp
            .blocks
            .iter()
            .find(|b| b.id == bid)
            .expect("block present");

        let (ins_ranges, del_ranges) = block_change_decos(b);
        assert_eq!(
            ins_ranges,
            vec![(0, 4)],
            "the insertion range underlines 'keep'"
        );
        assert_eq!(
            del_ranges,
            vec![(5, 9)],
            "the deletion range strikes 'gone'"
        );

        let (_, lay) = wp.layout_cache.get(&bid).expect("shaped layout cached");
        assert!(
            !lay.selection_rects(0, 4).is_empty(),
            "insertion underline has geometry"
        );
        assert!(
            !lay.selection_rects(5, 9).is_empty(),
            "deletion strike has geometry"
        );

        let (runs_on, _, _) = block_runs(b, true);
        assert!(
            runs_on
                .iter()
                .any(|r| r.text.contains("keep") && r.color == Some(INSERTION_RGB)),
            "the inserted run is tinted with the insertion review colour"
        );
        assert!(
            runs_on
                .iter()
                .any(|r| r.text.contains("gone") && r.color == Some(DELETION_RGB)),
            "the deleted run is tinted with the deletion review colour"
        );
        let (runs_off, _, _) = block_runs(b, false);
        assert!(
            runs_off
                .iter()
                .all(|r| r.color != Some(INSERTION_RGB) && r.color != Some(DELETION_RGB)),
            "with markup hidden, tracked-change runs render as plain text"
        );
    }

    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let ins_id = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let id = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 0, 4, &id);
            id
        };
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = split(&mut framed);

        let before = snapshot(state, &c);
        assert!(
            before.changes.iter().any(|ch| ch.id == ins_id),
            "the insertion is a tracked change before accept"
        );
        let b = before.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            block_change_decos(b).0,
            vec![(0, 4)],
            "the insertion underlines 'keep' before accept"
        );

        wp.dispatch_review_action(
            ReviewAction::AcceptChange(ins_id.clone()),
            &mut edits(&mut state.doc),
        );

        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep gone",
            "accepting an insertion keeps the inserted text"
        );
        assert!(
            !after.changes.iter().any(|ch| ch.id == ins_id),
            "the change record is dropped after accept"
        );
        assert!(
            block_change_decos(b).0.is_empty(),
            "no `ins` decoration remains after accept"
        );
        assert!(
            !b.marks.iter().any(|m| m.name == tilo_doc::schema::MARK_INS),
            "the `ins` mark is cleared from the block after accept"
        );
    }

    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let del_id = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.track_delete(&bid, 5, 9, "Ada", "")
        };
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = split(&mut framed);

        let before = snapshot(state, &c);
        assert!(
            before.changes.iter().any(|ch| ch.id == del_id),
            "the deletion is a tracked change before accept"
        );
        assert_eq!(
            before.blocks.iter().find(|b| b.id == bid).unwrap().text,
            "keep gone"
        );

        wp.dispatch_review_action(
            ReviewAction::AcceptChange(del_id.clone()),
            &mut edits(&mut state.doc),
        );

        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep ",
            "accepting a deletion removes the struck text"
        );
        assert!(
            !after.changes.iter().any(|ch| ch.id == del_id),
            "the change record is dropped after accept"
        );
        assert!(
            block_change_decos(b).1.is_empty(),
            "no `del` strike decoration remains after accept"
        );
    }
}

fn snapshot(state: &ui::State, c: &tilo_doc::Containers) -> tilo_doc::snapshot::DocSnapshot {
    let mut snapshot = tilo_doc::model::materialize(&state.doc, c);
    snapshot.comments = <crate::model::Document as ui::tilo_resource::Resource>::read(&state.doc)
        .map(|document| document.comment_views(&state.doc))
        .unwrap_or_default();
    for block in &mut snapshot.blocks {
        super::mark_comments(block, &snapshot.comments);
    }
    snapshot
}

#[test]
fn tracked_changes_reject_and_accept_all() {
    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let del_id = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.track_delete(&bid, 5, 9, "Ada", "")
        };
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = split(&mut framed);

        wp.dispatch_review_action(
            ReviewAction::RejectChange(del_id.clone()),
            &mut edits(&mut state.doc),
        );

        let after = snapshot(state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(b.text, "keep gone", "rejecting a deletion keeps the text");
        assert!(
            !after.changes.iter().any(|ch| ch.id == del_id),
            "the change record is dropped after reject"
        );
        assert!(
            block_change_decos(b).1.is_empty(),
            "the `del` strike decoration is cleared after reject"
        );
    }

    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let ins = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 0, 4, &ins);
            dm.track_delete(&bid, 5, 9, "Ada", "");
        }
        state.doc.commit();
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, state) = split(&mut framed);
        assert_eq!(
            snapshot(state, &c).changes.len(),
            2,
            "two tracked changes before Accept All"
        );

        wp.dispatch_review_action(ReviewAction::AcceptAllChanges, &mut edits(&mut state.doc));
        confirm_asked(wp, &mut state.doc);

        let after = snapshot(state, &c);
        assert!(
            after.changes.is_empty(),
            "no tracked changes remain after Accept All"
        );
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep ",
            "Accept All keeps insertions and applies deletions"
        );
        assert!(
            b.marks
                .iter()
                .all(|m| m.name != tilo_doc::schema::MARK_INS
                    && m.name != tilo_doc::schema::MARK_DEL)
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn review_markup_registers_card_hits_and_toggles_off() {
    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let (cid, ins_id);
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            ins_id = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 5, 9, &ins_id);
        }
        cid = crate::model::comment_by(&mut state.doc, "Ada", &bid, (0, 4), "look");
        state.doc.commit();
        let mut framed = mount(state, wp, 1200, 900);
        let (wp, state) = split(&mut framed);

        let has = |pred: &dyn Fn(&ReviewAction) -> bool| {
            wp.review
                .hits()
                .iter()
                .any(|(r, a)| pred(a) && r.width() > 0.0 && r.height() > 0.0)
        };
        assert!(
            has(&|a| matches!(a, ReviewAction::ResolveComment(id) if id == &cid)),
            "a Resolve affordance for the comment"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::OpenCommentThread(id) if id == &cid)),
            "a Reply affordance opens the durable comment thread"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::AcceptChange(id) if id == &ins_id)),
            "an Accept ✓ affordance for the change"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::RejectChange(id) if id == &ins_id)),
            "a Reject ✗ affordance for the change"
        );

        let tree = wp.accessibility_tree();
        let review = tree
            .children
            .iter()
            .find(|child| {
                child.node.role == ui::a11y::Role::Group
                    && child.node.name.as_deref() == Some(t("a11y-group-review").as_str())
            })
            .expect("painted review controls are exported as one semantic group");
        assert_eq!(
            review.children.len(),
            4,
            "comment cards expose Reply and Resolve; change cards expose Accept and Reject"
        );
        assert!(review.children.iter().all(|child| {
            child.node.role == ui::a11y::Role::Button
                && child.node.bounds.is_some()
                && child.node.action_target.is_some()
                && child.node.state.focusable
        }));

        let resolve_target = review_action_target(&ReviewAction::ResolveComment(cid.clone()));
        let root = state.root.clone();
        assert!(wp.perform_access_action(
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&resolve_target)],
            &mut edits(&mut state.doc),
            &root,
        ));
        assert!(
            snapshot(state, &c)
                .comments
                .iter()
                .all(|comment| comment.id != cid),
            "screen-reader activation resolves through the typed document action"
        );
    }

    {
        let (wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let ci = dm.insert_change(tilo_doc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 6, 11, &ci);
        }
        crate::model::comment_by(&mut state.doc, "Ada", &bid, (0, 5), "note");
        state.doc.commit();

        assert!(wp.markup_enabled(), "markup defaults ON");
        let mut framed = mount(state, wp, 1000, 900);
        let (wp, _) = split(&mut framed);
        let b = wp.blocks.iter().find(|b| b.id == bid).unwrap().clone();
        assert_eq!(
            block_comment_ranges(&b).len(),
            1,
            "comment range present in the data"
        );

        wp.toggle_markup();
        assert!(!wp.markup_enabled(), "toggle flips the flag");
        let (runs_off, _, _) = block_runs(&b, wp.review.is_visible());
        assert!(
            runs_off.iter().all(|r| r.color != Some(INSERTION_RGB)),
            "no review colour is applied while markup is hidden"
        );
    }
}

fn doc_with_headings(w: u32, h: u32) -> Framed<WordProcessorWidget> {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    let add_heading = |wp: &mut Editor, st: &mut ui::State, text: &str, level: i64| {
        wp.model_mut().insert_str(&mut edits(&mut st.doc), text);
        wp.dispatch_paragraph_style(&mut edits(&mut st.doc), "heading", Some(level), None);
        wp.model_mut().enter(&mut edits(&mut st.doc));
        wp.dispatch_paragraph_style(&mut edits(&mut st.doc), "paragraph", None, None);
    };
    add_heading(&mut wp, &mut state, "Introduction", 1);
    for i in 0..30 {
        wp.model_mut().insert_str(
            &mut edits(&mut state.doc),
            &format!("Intro body line {i} with enough words to fill the line."),
        );
        wp.model_mut().enter(&mut edits(&mut state.doc));
    }
    add_heading(&mut wp, &mut state, "Background", 2);
    for i in 0..30 {
        wp.model_mut().insert_str(
            &mut edits(&mut state.doc),
            &format!("Background body line {i} with enough words to fill it."),
        );
        wp.model_mut().enter(&mut edits(&mut state.doc));
    }
    add_heading(&mut wp, &mut state, "Conclusion", 1);
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "The end.");
    mount(state, wp, w, h)
}

#[test]
fn the_outline_and_toc_follow_the_headings() {
    {
        let mut wp = doc_with_headings(1000, 700);
        let wp = editor(&mut wp);
        let entries = wp.outline_entries();
        assert_eq!(
            entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["Introduction", "Background", "Conclusion"],
            "one entry per heading in document order"
        );
        assert_eq!(
            entries.iter().map(|e| e.level).collect::<Vec<_>>(),
            vec![1, 2, 1],
            "levels preserved"
        );
    }

    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = split(&mut framed);
        let heading_count = wp.outline_entries().len();
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertToc);

        let c = wp.model().containers.clone().unwrap();
        let snap = tilo_doc::model::materialize(&state.doc, &c);
        let toc: Vec<_> = snap
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .collect();
        assert_eq!(
            toc.len(),
            heading_count,
            "one toc-marked block per heading (one line each)"
        );
        assert!(
            snap.blocks[0].text.contains("Introduction"),
            "TOC begins at the top with the first heading"
        );
        assert!(
            toc.iter().any(|b| b.text.contains("Conclusion")),
            "every heading is listed"
        );
    }
}

pub(super) fn left_click(wp: &mut Editor, state: &mut ui::State, pt: Point) {
    let moved = Dispatch::new().doc(&mut state.doc).path(&state.root).send(
        &mut Lent(wp),
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: (pt.x, pt.y).into(),
        },
    );
    Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root)
        .focus_target(moved.focus_target)
        .send(&mut Lent(wp), &ui::synthetic_input::mouse_press());
}

pub(super) fn click(wp: &mut Editor, state: &mut ui::State, pt: Point) {
    left_click(wp, state, pt);
    Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root)
        .send(&mut Lent(wp), &ui::synthetic_input::mouse_release());
}

#[test]
fn clicking_an_outline_entry_scrolls_the_heading_into_view() {
    let mut framed = doc_with_headings(1000, 700);
    let (wp, state) = split(&mut framed);
    wp.apply_tool(&mut edits(&mut state.doc), ToolAction::ToggleOutline);
    assert!(wp.outline.is_open(), "outline toggled on");
    wp.outline_drawer.snap(true);
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

    let entries = wp.outline_entries();
    let (entry_index, concl) = entries
        .iter()
        .enumerate()
        .find(|(_, heading)| heading.text == "Conclusion")
        .expect("conclusion heading");
    assert_eq!(wp.scroll_y, 0.0, "starts scrolled to the top");

    let _ = entry_index;
    let pt = wp
        .outline
        .heading_rect(&concl.id)
        .expect("the heading has a row in the outline")
        .center();
    left_click(wp, state, pt);
    assert!(
        wp.scroll_y > 0.0,
        "clicking a far-down heading scrolls the document"
    );

    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    let canvas_top = wp.zoom_viewport.y0;
    let canvas_bottom = wp.rect.y1 - wp.layout_bar_height;
    let laid = wp
        .laid
        .iter()
        .find(|l| l.id == concl.id)
        .expect("conclusion is laid out after scrolling to it");
    assert!(
        laid.y_top >= canvas_top && laid.y_top <= canvas_bottom,
        "the clicked heading is visible in the canvas (y_top={}, canvas=[{canvas_top},{canvas_bottom}])",
        laid.y_top
    );
}

#[test]
fn regenerating_the_toc_replaces_it_and_shows_it() {
    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = split(&mut framed);
        let heading_count = wp.outline_entries().len();
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertToc);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertToc);

        let c = wp.model().containers.clone().unwrap();
        let snap = tilo_doc::model::materialize(&state.doc, &c);
        let toc_count = snap
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .count();
        assert_eq!(
            toc_count, heading_count,
            "regenerating replaces the prior TOC (no duplicate)"
        );
        assert_eq!(
            wp.outline_entries().len(),
            heading_count,
            "headings unchanged by TOC regeneration"
        );
    }

    {
        let (mut state, mut wp) = bound();
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertToc);
        wp.sync(&state.doc, &state.root);
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.text),
            Some(t("toast-toc-empty")),
            "the window bar says why nothing was inserted"
        );
    }

    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = split(&mut framed);
        wp.scroll_y = 144.0;
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertToc);
        assert_eq!(wp.scroll_y, 0.0, "the new table of contents is in view");
        assert!(!wp.toast.is_shown(), "no toast repeats the visible result");
        let c = wp.model().containers.clone().unwrap();
        let toc = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .count();
        assert!(toc > 0, "the TOC action inserts visible TOC lines");
    }
}

fn first_block(wp: &Editor, state: &ui::State) -> Option<BlockView> {
    let c = wp.model().containers.clone()?;
    tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .into_iter()
        .next()
}

fn bound() -> (ui::State, Editor) {
    let state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    (state, wp)
}

fn send_ime(wp: &mut Editor, state: &mut ui::State, ime: winit::event::Ime) {
    Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root)
        .send(&mut Lent(wp), &WindowEvent::Ime(ime));
}

#[test]
fn typing_composes_with_the_input_method_and_erases_whole_clusters() {
    use winit::event::Ime;
    let (mut state, mut wp) = bound();
    wp.lifecycle.focused = true;
    send_ime(&mut wp, &mut state, Ime::Commit("a".into()));
    send_ime(
        &mut wp,
        &mut state,
        Ime::Preedit("かな".into(), Some((3, 3))),
    );
    assert_eq!(wp.inner.preedit(), "かな");
    let composition = wp.inner.composition("a").expect("composing");
    assert_eq!(composition.display_text("a"), "aかな");
    assert_eq!(
        composition.display_caret("a"),
        "aか".len(),
        "the caret sits inside"
    );
    assert_eq!(body_text(&wp, &state), ["a"], "not typed until committed");
    send_ime(&mut wp, &mut state, Ime::Commit("仮名".into()));
    assert_eq!(wp.inner.preedit(), "");
    for typed in ["e\u{301}", "👍🏽", "🇩🇪"] {
        send_ime(&mut wp, &mut state, Ime::Commit(typed.into()));
    }
    assert_eq!(body_text(&wp, &state), ["a仮名e\u{301}👍🏽🇩🇪"]);

    press_named(&mut wp, &mut state, NamedKey::Backspace);
    assert_eq!(body_text(&wp, &state), ["a仮名e\u{301}👍🏽"]);
    press_named(&mut wp, &mut state, NamedKey::ArrowLeft);
    press_named(&mut wp, &mut state, NamedKey::ArrowLeft);
    press_named(&mut wp, &mut state, NamedKey::Delete);
    assert_eq!(body_text(&wp, &state), ["a仮名👍🏽"]);
    press_named(&mut wp, &mut state, NamedKey::ArrowRight);
    press_named(&mut wp, &mut state, NamedKey::Backspace);
    assert_eq!(body_text(&wp, &state), ["a仮名"]);

    select_range(&mut wp, 1, 3);
    send_ime(&mut wp, &mut state, Ime::Preedit("x".into(), None));
    send_ime(&mut wp, &mut state, Ime::Disabled);
    assert_eq!(
        wp.inner.preedit(),
        "",
        "an abandoned composition leaves nothing"
    );
    send_ime(&mut wp, &mut state, Ime::Commit("x".into()));
    assert_eq!(body_text(&wp, &state), ["ax"]);
}

#[test]
fn a_composition_lays_out_inside_its_paragraph_and_commits_in_place() {
    use winit::event::Ime;
    let (mut state, mut wp) = bound();
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "hello world");
    wp.lifecycle.focused = true;
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = split(&mut framed);
    let bid = wp.blocks[0].id.clone();
    let at = |wp: &Editor, offset: usize| {
        let l = wp.laid.iter().find(|l| l.id == bid).expect("laid out");
        let (_, layout) = wp.layout_cache.get(&bid).expect("shaped");
        let (x, y, _) = layout.caret_geometry(l.layout_byte(offset));
        (
            Point::new(l.text_x + f64::from(x), l.layout_y + f64::from(y)),
            layout.height(),
        )
    };
    let (w_before, height_before) = at(wp, 6);
    wp.inner.set_caret(Caret {
        block: bid.clone(),
        offset: 5,
    });
    send_ime(wp, state, Ime::Preedit("かな".into(), Some((3, 3))));
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

    let l = wp.laid.iter().find(|l| l.id == bid).expect("laid out");
    assert_eq!(l.text, "hello world", "the document is untouched");
    let core = l.composition.clone().expect("the composition is laid out");
    let shown = composed_block(&wp.blocks[0], &core);
    assert_eq!(shown.text, "helloかな world");
    assert!(
        shown
            .marks
            .iter()
            .any(|m| m.name == "underline" && (m.start, m.end) == (5, 7)),
        "the composition is underlined"
    );
    assert_eq!(l.caret_byte(5), "helloか".len(), "the caret sits inside");
    let (w_after, _) = at(wp, 6);
    assert!(
        (w_after.y - w_before.y).abs() < 0.5 && w_after.x > w_before.x + 10.0,
        "the text after the caret moves aside: {w_before:?} -> {w_after:?}"
    );

    let (start, _) = at(wp, 5);
    wp.caret_from_pointer(Point::new(start.x + 4.0, start.y + 4.0), false);
    assert_eq!(wp.inner.caret.offset, 5);
    wp.caret_from_pointer(Point::new(w_after.x + 2.0, w_after.y + 4.0), false);
    assert_eq!(wp.inner.caret.offset, 6, "on the 'w' of \"world\"");
    wp.inner.set_caret(Caret {
        block: bid.clone(),
        offset: 5,
    });

    send_ime(wp, state, Ime::Preedit("あ".repeat(80), None));
    let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
    let (w_wrapped, height_wrapped) = at(wp, 6);
    assert!(
        height_wrapped > height_before && w_wrapped.y > w_before.y,
        "the paragraph rewraps around the composition"
    );

    send_ime(wp, state, Ime::Preedit("かな".into(), Some((3, 3))));
    send_ime(wp, state, Ime::Commit("仮名".into()));
    assert_eq!(body_text(wp, state), ["hello仮名 world"]);
    assert_eq!(wp.inner.caret.offset, 7, "the caret follows the commit");
    let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
    let l = wp.laid.iter().find(|l| l.id == bid).expect("laid out");
    assert_eq!(l.text, "hello仮名 world");
    assert!(l.composition.is_none(), "nothing is composing any more");
}

#[test]
fn hosted_lookup_offers_caret_edits_not_a_second_document() {
    let (state, wp) = bound();
    let wp = WordProcessorWidget::over(wp);
    assert!(
        ui::resources::resources(&wp, &state.doc).is_empty(),
        "the window publishes nothing beside the framework's document"
    );
    assert!(
        wp.state().search_rows().is_empty(),
        "nothing found, no search results"
    );
    let effects: Vec<String> =
        <crate::effects::WordProcessorEffect as ui::tilo_resource::Effect>::specs()
            .into_iter()
            .map(|spec| spec.id.clone())
            .collect();
    for (id, what) in [
        (
            "type-text",
            "typing is an effect at the caret, not only a keypress",
        ),
        (
            "place-image",
            "placing an image is an effect at the caret, not only a toolbar click",
        ),
        (
            "insert-table-here",
            "inserting a table is an effect at the caret",
        ),
    ] {
        assert!(
            effects.iter().any(|effect| effect == id),
            "{what}: {effects:?}"
        );
    }
}

#[test]
fn a_selection_can_be_linked() {
    let (mut state, mut wp) = bound();
    wp.inner.insert_str(&mut edits(&mut state.doc), "hello");
    wp.sync(&state.doc, &state.root);
    wp.inner.caret.offset = 0;
    wp.inner.set_anchor();
    wp.inner.caret.offset = 5;
    let link = effect_edit(
        &wp,
        &state.doc,
        &crate::effects::WordProcessorEffect::SetHyperlink {
            target: "https://example.com".into(),
        },
    );
    assert!(
        matches!(link, crate::actions::RichTextAction::SetMark { ref name, .. } if name == "link"),
        "a selection is linked by the effect: {link:?}"
    );
}

#[test]
fn the_window_zoom_gestures_scale_the_page() {
    use ui::content_zoom::{self as shared, ZoomStep};
    let (state, wp) = bound();
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, _state) = Framed::split(&mut framed);

    assert!(
        shared::provided(wp),
        "the editor's page is content that zooms by itself"
    );
    assert_eq!(
        wp.state().zoom.factor(),
        1.0,
        "a page starts at its own size"
    );
    assert_eq!(
        shared::offer(wp, ZoomStep::In),
        Some(1.1),
        "Ctrl+Plus zooms the page in, one 10% step"
    );
    assert_eq!(shared::offer(wp, ZoomStep::In), Some(1.2));
    assert_eq!(shared::offer(wp, ZoomStep::Out), Some(1.1));
    assert_eq!(
        shared::offer(wp, ZoomStep::Reset),
        Some(1.0),
        "Ctrl+0 is the page at the size its type is set in"
    );
}

#[test]
fn the_bar_marks_work_the_file_does_not_have() {
    let (mut state, mut wp) = bound();
    let edited = |wp: &Editor| {
        wp.window_bar()
            .location
            .is_some_and(|location| location.edited)
    };
    assert!(!edited(&wp), "a new document holds nothing to lose");

    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "Work in progress");
    wp.refresh_blocks(&state.doc);
    assert!(edited(&wp), "typed words are work no file has");

    wp.lifecycle.dirty = false;
    assert!(edited(&wp), "the mark is not the repaint flag");

    crate::model::apply(
        &mut state.doc,
        &crate::actions::RichTextAction::NoteFileWritten {
            digest: tilo_content::hash_bytes(b"written"),
        },
    )
    .expect("the write is noted");
    wp.refresh_blocks(&state.doc);
    assert!(!edited(&wp), "the file took the work, and the bar says so");
}

#[test]
fn the_file_commands_are_the_window_bars_file_menu() {
    let (state, wp) = bound();
    let mut framed = mount(state, wp, 1200, 700);
    let (wp, state) = split(&mut framed);
    let items = bar_items(wp, &t("toolbar-file"));
    for choice in [FileChoice::Open, FileChoice::Save, FileChoice::SaveAs]
        .into_iter()
        .chain(
            crate::Template::all()
                .iter()
                .copied()
                .map(FileChoice::Template),
        )
    {
        assert!(
            items.contains(&BarCommand::File(choice)),
            "File offers {choice:?}: {items:?}"
        );
    }
    press_bar(wp, state, &crate::Template::Report.display_title());
    assert!(
        !wp.file_menu.is_open(),
        "nothing to replace: nothing to ask"
    );
    assert_eq!(
        first_block(wp, state).map(|b| b.text),
        Some("Report Title".into())
    );
}

#[test]
fn a_template_asks_before_replacing_and_answers_by_keyboard() {
    for theme in [Theme::dark(), Theme::light()] {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Draft");
        wp.refresh_blocks(&state.doc);
        let mut framed = mount_themed(state, wp, 1200, 700, &theme);
        let (wp, state) = split(&mut framed);
        press_bar(wp, state, &crate::Template::Report.display_title());
        assert!(wp.file_menu.is_asking(), "a template asks before replacing");
        let mut tr = TextRenderer::new();
        let mut before = Scene::new();
        wp.draw_overlay(
            &mut before,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );
        let point = wp
            .file_menu
            .choice_center(crate::file_menu::FileChoice::Template(
                crate::Template::Report,
            ))
            .expect("the Replace row is laid out");
        wp.lifecycle.dirty = false;
        Dispatch::new()
            .scale_factor(2.0)
            .doc(&mut state.doc)
            .path(&state.root)
            .send(
                &mut Lent(wp),
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (point.x * 2.0, point.y * 2.0).into(),
                },
            );
        let mut after = Scene::new();
        wp.draw_overlay(
            &mut after,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );
        assert_ne!(
            after.encoding().draw_data,
            before.encoding().draw_data,
            "hovering a row must change its painted highlight in each theme"
        );
        assert!(wp.lifecycle.dirty, "hover must request a repaint");
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Draft");
        wp.refresh_blocks(&state.doc);
        let mut framed = mount(state, wp, 1200, 700);
        let (wp, state) = split(&mut framed);
        wp.lifecycle.focused = true;
        press_bar(wp, state, &crate::Template::Report.display_title());
        assert!(wp.file_menu.is_asking());
        press_named(wp, state, NamedKey::Backspace);
        assert_eq!(first_block(wp, state).map(|b| b.text), Some("Draft".into()));
        let replace = wp
            .file_menu
            .choice_row(crate::file_menu::FileChoice::Template(
                crate::Template::Report,
            ));
        for _ in 0..5 {
            if wp.file_menu.menu_mut().hovered() == replace {
                break;
            }
            press_named(wp, state, NamedKey::ArrowDown);
        }
        press_named(wp, state, NamedKey::Enter);
        assert!(!wp.file_menu.is_open(), "Enter chooses and closes the menu");
        assert_eq!(
            first_block(wp, state).map(|b| b.text),
            Some("Report Title".into())
        );
    }
}

#[test]
fn templates_replace_the_document_only_when_chosen() {
    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Draft");
        wp.refresh_blocks(&state.doc);
        let mut framed = mount(state, wp, 1200, 700);
        let (wp, state) = split(&mut framed);
        press_bar(wp, state, &crate::Template::Report.display_title());
        let keep = wp
            .file_menu
            .cancel_center()
            .expect("the question offers Keep");
        click(wp, state, keep);
        assert!(!wp.file_menu.is_open());
        assert_eq!(first_block(wp, state).map(|b| b.text), Some("Draft".into()));

        press_bar(wp, state, &crate::Template::Report.display_title());
        assert_eq!(
            wp.file_menu.menu_mut().hovered(),
            None,
            "reopening clears the old highlight"
        );
        wp.lifecycle.focused = false;
        press_named(wp, state, NamedKey::Escape);
        assert!(
            !wp.file_menu.is_open(),
            "Escape works even without editor focus"
        );

        press_bar(wp, state, &crate::Template::Report.display_title());
        let caret = wp.inner.caret.clone();
        left_click(wp, state, Point::new(900.0, 500.0));
        assert!(!wp.file_menu.is_open());
        assert_eq!(
            wp.inner.caret, caret,
            "dismissal must not click the document beneath"
        );
        assert_eq!(first_block(wp, state).map(|b| b.text), Some("Draft".into()));
    }

    {
        let (mut state, mut wp) = bound();
        let old_path =
            std::env::temp_dir().join(format!("wp-template-original-{}.md", std::process::id()));
        std::fs::write(&old_path, "Original document").unwrap();
        wp.set_file(old_path.clone(), WpFormat::Markdown);
        wp.apply_template(&mut edits(&mut state.doc), crate::Template::Report);
        assert!(
            wp.path.is_none(),
            "New must ask for a destination on its first Save"
        );
        assert_eq!(wp.format, Some(WpFormat::Markdown));
        assert_eq!(
            std::fs::read_to_string(&old_path).unwrap(),
            "Original document"
        );
        let _ = std::fs::remove_file(old_path);
    }
}

#[test]
fn a_confirmed_template_replaces_the_draft() {
    let (mut state, mut wp) = bound();
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "Pre-existing rough draft");
    wp.refresh_blocks(&state.doc);
    let mut framed = mount(state, wp, 1200, 700);
    let (wp, state) = split(&mut framed);
    press_bar(wp, state, &crate::Template::Report.display_title());
    assert!(
        wp.file_menu.is_asking(),
        "on a document with words the editor asks before replacing it"
    );
    assert_eq!(
        first_block(wp, state).map(|b| b.text),
        Some("Pre-existing rough draft".to_string()),
        "nothing is replaced until the person says so"
    );
    wp.pointer = wp
        .file_menu
        .choice_center(crate::file_menu::FileChoice::Template(
            crate::Template::Report,
        ))
        .expect("the question offers Replace");
    let mut handled = false;
    assert!(wp.handle_left_press_menu_routing(&mut edits(&mut state.doc), &mut handled));
    assert!(
        !wp.file_menu.is_open(),
        "the question closes after a choice"
    );
    let first = first_block(wp, state).expect("the templated document has a first block");
    assert_eq!(
        first.kind, "heading",
        "the report template opens with a heading"
    );
    assert_eq!(first.text, "Report Title");
    let c = wp.model().containers.clone().unwrap();
    let all_text: String = tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .iter()
        .map(|b| b.text.clone())
        .collect();
    assert!(
        !all_text.contains("rough draft"),
        "the prior content was replaced, not appended"
    );
}

fn type_char(wp: &mut Editor, state: &mut ui::State, ch: char) {
    use winit::keyboard::SmolStr;
    let ev = ui::synthetic_input::key_event(
        Key::Character(SmolStr::new(ch.to_string())),
        Some(SmolStr::new(ch.to_string())),
        ElementState::Pressed,
    );
    Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root)
        .send(&mut Lent(wp), &ui::synthetic_input::keyboard_input(ev));
}

fn press_tab(wp: &mut Editor, state: &mut ui::State) {
    let ev = ui::synthetic_input::key_event(Key::Named(NamedKey::Tab), None, ElementState::Pressed);
    Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root)
        .send(&mut Lent(wp), &ui::synthetic_input::keyboard_input(ev));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_ruler_tracks_margins_indent_and_zoom() {
    {
        let (mut state, mut wp) = bound();
        assert!(wp.view.ruler_visible, "ruler defaults ON (discoverable)");
        press_bar(&mut wp, &mut state, &t("toolbar-ruler"));
        assert!(!wp.view.ruler_visible, "the bar's toggle hides the ruler");
        press_bar(&mut wp, &mut state, &t("toolbar-ruler"));
        assert!(wp.view.ruler_visible, "toggles back on");
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "para");
        wp.dispatch_block_attr(
            &mut edits(&mut state.doc),
            "indent",
            crate::actions::AttrVal::Float(40.0),
        );
        wp.dispatch_block_attr(
            &mut edits(&mut state.doc),
            "first_line_indent",
            crate::actions::AttrVal::Float(18.0),
        );
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, _) = split(&mut framed);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        assert!(
            (page_w - MAX_PAGE_W).abs() < 1e-6,
            "page is the fixed US-Letter width"
        );

        assert!((wp.zoom.factor() - 1.0).abs() < 1e-6, "default zoom");
        let markers = wp.ruler_markers(page_x, page_w);
        let mx = |m: RulerMarker| {
            markers
                .iter()
                .find(|(k, _)| *k == m)
                .map(|(_, x)| *x)
                .unwrap()
        };

        let m = wp.margins;
        assert!(
            (mx(RulerMarker::MarginLeft) - (page_x + m.left)).abs() < 1e-6,
            "left margin marker at the left text edge"
        );
        assert!(
            (mx(RulerMarker::MarginRight) - (page_x + page_w - m.right)).abs() < 1e-6,
            "right margin marker at the right text edge"
        );
        assert!(
            (mx(RulerMarker::IndentLeft) - (page_x + m.left + 40.0)).abs() < 1e-6,
            "left-indent marker offset by the block indent"
        );
        assert!(
            (mx(RulerMarker::IndentFirst) - (page_x + m.left + 40.0 + 18.0)).abs() < 1e-6,
            "first-line marker offset by indent + first-line indent"
        );

        wp.set_zoom(2.0);
        let (wp, _) = pipeline_draw(&mut framed, 1200, 760);
        let anchor = wp.zoom.anchor().x;
        let zoomed = wp.ruler_markers(wp.page_rect.x0, wp.page_rect.width());
        let zx = |m: RulerMarker| {
            zoomed
                .iter()
                .find(|(k, _)| *k == m)
                .map(|(_, x)| *x)
                .unwrap()
        };
        let content_left = wp.page_rect.x0 + wp.margins.left;
        let expect = anchor + (content_left - anchor) * 2.0;
        assert!(
            (zx(RulerMarker::MarginLeft) - expect).abs() < 1e-6,
            "marker scales with zoom about the anchor"
        );
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "para");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = split(&mut framed);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let before = wp.margins.left;

        let markers = wp.ruler_markers(page_x, page_w);
        let marker_x = markers
            .iter()
            .find(|(k, _)| *k == RulerMarker::MarginLeft)
            .map(|(_, x)| *x)
            .unwrap();
        wp.pointer = Point::new(
            marker_x,
            f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1),
        );
        wp.ruler_press(&mut edits(&mut state.doc));
        assert_eq!(
            wp.ruler.drag_marker(),
            Some(RulerMarker::MarginLeft),
            "grabbed the left-margin marker"
        );

        let target_content_x = page_x + 120.0; // 120 px from the page's left edge
        wp.pointer = Point::new(wp.content_x_to_screen(target_content_x), wp.pointer.y);
        wp.ruler_drag_move();
        assert!(
            (wp.margins.left - 120.0).abs() < 1.0,
            "live preview updated margins.left to ~120 (got {})",
            wp.margins.left
        );
        assert_ne!(wp.margins.left, before, "the left margin actually changed");

        wp.ruler_drag_release(&mut edits(&mut state.doc));
        assert!(!wp.ruler.is_dragging(), "drag ended on release");
        let mut wp2 = Editor::detached();
        wp2.sync(&state.doc, &state.root);
        assert!(
            (wp2.margins.left - wp.margins.left).abs() < 1e-6,
            "the dragged left margin persisted to the document"
        );
    }
}

#[test]
fn ruler_drags_and_clicks_set_indents_and_tab_stops() {
    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "indent me");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = split(&mut framed);
        let block = wp.model().caret().block.clone();
        let x_before = wp.laid.iter().find(|l| l.id == block).unwrap().text_x;

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let markers = wp.ruler_markers(page_x, page_w);
        let mxl = markers
            .iter()
            .find(|(k, _)| *k == RulerMarker::IndentLeft)
            .map(|(_, x)| *x)
            .unwrap();
        wp.pointer = Point::new(mxl, wp.ruler.rect().y1 - 2.0);
        wp.ruler_press(&mut edits(&mut state.doc));
        assert_eq!(wp.ruler.drag_marker(), Some(RulerMarker::IndentLeft));

        let target = page_x + wp.margins.left + 60.0;
        wp.pointer = Point::new(wp.content_x_to_screen(target), wp.pointer.y);
        wp.ruler_drag_move();
        wp.ruler_drag_release(&mut edits(&mut state.doc));

        let c = wp.model().containers.clone().unwrap();
        let stored = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .find(|b| b.id == block)
            .and_then(|b| match b.attrs.get("indent") {
                Some(tilo_doc::AttrVal::Float(px)) => Some(*px),
                _ => None,
            })
            .unwrap_or(0.0);
        assert!(
            (stored - 60.0).abs() < 1.0,
            "indent persisted to the block attr (got {stored})"
        );

        let (wp, _) = pipeline_draw(&mut framed, 1200, 760);
        let x_after = wp.laid.iter().find(|l| l.id == block).unwrap().text_x;
        assert!(
            (x_after - x_before - 60.0).abs() < 1.0,
            "text reflowed right by the indent (before {x_before}, after {x_after})"
        );
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut edits(&mut state.doc), "ab");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = split(&mut framed);
        let block = wp.model().caret().block.clone();
        assert!(
            wp.model().block_tab_stops(&state.doc, &block).is_empty(),
            "no tab stops initially"
        );

        let page_x = wp.page_rect.x0;
        let tab_content_x = page_x + wp.margins.left + 200.0;
        wp.pointer = Point::new(
            wp.content_x_to_screen(tab_content_x),
            f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1),
        );
        wp.ruler_press(&mut edits(&mut state.doc));
        let stops = wp.model().block_tab_stops(&state.doc, &block);
        assert_eq!(stops.len(), 1, "a tab stop was stored on the block");
        assert!(
            (stops[0] - 200.0).abs() < 2.0,
            "the stop is ~200 px from the left text margin (got {})",
            stops[0]
        );

        wp.model_mut().move_end(&state.doc);
        let off_before = wp.model().caret().offset;
        press_tab(wp, state);
        let off_after = wp.model().caret().offset;
        assert_eq!(
            off_after,
            off_before + 1,
            "Tab advanced the caret by one (to the tab stop), not four spaces"
        );
        assert!(
            wp.model().current_text(&state.doc).contains('\t'),
            "a tab character records the advance"
        );
        assert!(
            !wp.model().current_text(&state.doc).contains("    "),
            "Tab did not fall back to inserting spaces"
        );
    }
}

#[test]
fn tab_stops_insert_spaces_without_stops_and_drag_off_to_remove() {
    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut edits(&mut state.doc), "x");
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = split(&mut framed);
        press_tab(wp, state);
        assert!(
            wp.model().current_text(&state.doc).contains("    "),
            "no stops → Tab inserts spaces as before"
        );
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "para");
        let block = wp.model().caret().block.clone();
        wp.dispatch_block_attr(
            &mut edits(&mut state.doc),
            "tab_stops",
            crate::actions::AttrVal::Str("150.0".into()),
        );
        let mut framed = mount(state, wp, 1200, 760);
        let (wp, state) = split(&mut framed);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let markers = wp.ruler_markers(page_x, page_w);
        let tx = markers
            .iter()
            .find(|(k, _)| matches!(k, RulerMarker::Tab(0)))
            .map(|(_, x)| *x)
            .unwrap();
        wp.pointer = Point::new(tx, f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1));
        wp.ruler_press(&mut edits(&mut state.doc));
        assert!(
            matches!(wp.ruler.drag_marker(), Some(RulerMarker::Tab(0))),
            "grabbed the tab marker"
        );

        wp.pointer = Point::new(tx, wp.ruler.rect().y1 + 40.0);
        wp.ruler_drag_move();
        assert!(
            wp.ruler.dropping_tab(),
            "dragging below the band flags drop-off"
        );
        wp.ruler_drag_release(&mut edits(&mut state.doc));
        assert!(
            wp.model().block_tab_stops(&state.doc, &block).is_empty(),
            "the tab stop was removed"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_accessibility_tree_mirrors_the_document_structure() {
    {
        use ui::a11y::Role;
        let state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);

        let tree = wp.accessibility_tree();
        assert_eq!(tree.node.role, Role::Document);
        assert!(tree.node.state.multiline, "the document is multiline");
        assert!(tree.node.state.focusable, "the document is focusable");

        wp.set_focus(false);
        assert!(
            !wp.accessibility_tree().node.state.focused,
            "blurred editor → not focused"
        );
        wp.set_focus(true);
        assert!(
            wp.accessibility_tree().node.state.focused,
            "focused editor → focused"
        );

        let trait_root = wp.accessibility();
        assert_eq!(trait_root.role, Role::Document);
        assert_eq!(trait_root.name.as_deref(), Some("Document editor"));
        assert_eq!(
            trait_root.children.len(),
            tree.children.len(),
            "Widget::accessibility() exports every private tree child"
        );
    }

    {
        use ui::a11y::Role;
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);

        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Title");
        wp.dispatch_paragraph_style(&mut edits(&mut state.doc), "heading", Some(1), None);
        wp.model_mut().enter(&mut edits(&mut state.doc));
        wp.dispatch_paragraph_style(&mut edits(&mut state.doc), "paragraph", None, None);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "Body text here");
        wp.sync(&state.doc, &state.root);

        let tree = wp.accessibility_tree();

        let heading = tree
            .children
            .iter()
            .find(|c| matches!(c.node.role, Role::Heading { .. }))
            .expect("a heading node is present");
        assert_eq!(
            heading.node.role,
            Role::Heading { level: 1 },
            "level-1 heading"
        );
        assert_eq!(
            heading.node.name.as_deref(),
            Some("Title"),
            "heading carries its text"
        );

        let paragraph = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Paragraph)
            .expect("a paragraph node is present");
        assert_eq!(
            paragraph.node.value.as_deref(),
            Some("Body text here"),
            "paragraph carries its text",
        );
    }

    {
        use std::collections::BTreeMap;
        use tilo_doc::snapshot::{AttrVal, BlockView};
        use ui::a11y::Role;

        let mut attrs = BTreeMap::new();
        attrs.insert("blob".to_string(), AttrVal::Str("deadbeef".into()));
        attrs.insert("alt".to_string(), AttrVal::Str("a tiny dot".into()));
        let img = BlockView {
            id: "img1".into(),
            kind: "image".into(),
            text: String::new(),
            marks: Vec::new(),
            attrs,
            children: Vec::new(),
        };

        let node = Editor::block_access_node(&img).expect("image yields a node");
        assert_eq!(node.role, Role::Image);
        assert_eq!(
            node.name.as_deref(),
            Some("a tiny dot"),
            "image node carries its alt text"
        );

        let mut captioned = img;
        captioned.attrs.remove("alt");
        captioned.text = "Figure 1".into();
        let cap_node = Editor::block_access_node(&captioned).unwrap();
        assert_eq!(
            cap_node.name.as_deref(),
            Some("Figure 1"),
            "caption text is the alt fallback"
        );

        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "before");
        wp.model_mut()
            .insert_image_bytes(&mut edits(&mut state.doc), TEST_PNG);
        wp.sync(&state.doc, &state.root);
        let tree = wp.accessibility_tree();
        assert!(
            tree.children.iter().any(|c| c.node.role == Role::Image),
            "an inserted image appears as an image node in the document tree",
        );
    }

    {
        use ui::a11y::Role;
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.insert_table_via_action(&mut edits(&mut state.doc), 2, 2);
        wp.sync(&state.doc, &state.root);

        let tree = wp.accessibility_tree();
        let table = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Table)
            .expect("a table node is present");
        assert_eq!(table.children.len(), 4, "the 2×2 table has four cell nodes");
        assert!(
            table.children.iter().all(|c| c.node.role == Role::Cell),
            "every table child is a cell",
        );
        assert!(
            !tree.children.iter().any(|c| c.node.role == Role::Cell),
            "cells are not top-level document children",
        );
    }
}

#[test]
fn the_window_bar_names_its_commands_for_assistive_technology() {
    crate::i18n::set_locale("en-US");
    let (state, wp) = populated();
    let mut framed = mount(state, wp, 1100, 760);
    let (wp, state) = split(&mut framed);
    let bold = bar_slot(wp, ToolAction::Bold).expect("Format offers Bold");
    assert_eq!(bold.name, "Bold");
    assert!(!bold.on);
    let styles = bar_slot(wp, ToolAction::ToggleStyles).expect("the inspector toggle");
    assert!(!styles.on);
    press_bar(wp, state, &t("a11y-styles-panel"));
    assert!(
        wp.view.styles_panel_visible,
        "the bar's toggle shows the inspector"
    );
    assert!(
        bar_slot(wp, ToolAction::ToggleStyles).is_some_and(|slot| slot.on),
        "the toggle says it is on"
    );
}

use ui::permissions::UserId;
use ui::presence::SelectionRange;

fn first_block_id(wp: &Editor) -> String {
    wp.laid
        .first()
        .expect("at least one block was laid out")
        .id
        .clone()
}

fn drawn_focused() -> Framed<WordProcessorWidget> {
    let (state, mut wp) = populated();
    wp.set_focus(true);
    mount(state, wp, 1100, 760)
}

#[test]
fn semantic_text_caret_is_painted_without_a_selection_or_input() {
    let mut framed = drawn_focused();
    let framed = editor(&mut framed);
    let block = first_block_id(framed);
    let mut presence = PresenceCtx::empty();
    presence
        .apply_interaction(tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: tilo_actions::InteractionActor {
                id: "synthetic-semantic-run".into(),
                kind: tilo_actions::ActorKind::Agent,
                for_person: None,
            },
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: "type-text".into(),
                label: "Type text".into(),
                app: None,
                step: None,
                creates: None,
            },
            resource: tilo_actions::ActionResource {
                id: "word-processor.document".into(),
                kind: "document".into(),
                label: "Synthetic document".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Text {
                region: "body".into(),
                ranges: vec![tilo_actions::InteractionTextRange {
                    block,
                    start: 0,
                    end: 0,
                }],
            },
            group: None,
        })
        .unwrap();
    let before = (
        framed.inner.caret.clone(),
        framed.scroll_y,
        framed.lifecycle.focused,
    );
    let mut scene = Scene::new();
    framed.draw_remote_overlays(
        &mut scene,
        framed.content_transform(Affine::IDENTITY),
        framed.zoom_viewport,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &presence,
    );
    assert!(
        !scene.encoding().path_tags.is_empty(),
        "an actual collapsed semantic text range must paint its shaped caret"
    );
    assert_eq!(
        scene.encoding().n_clips,
        2,
        "semantic decoration and label share one balanced document clip"
    );
    assert_eq!(
        (
            framed.inner.caret.clone(),
            framed.scroll_y,
            framed.lifecycle.focused
        ),
        before,
        "observing semantic work must not move or focus the local editor"
    );
}

fn semantic_text_anchor(block: &str, start: u64, end: u64) -> tilo_actions::InteractionAnchor {
    tilo_actions::InteractionAnchor::Text {
        region: "body".into(),
        ranges: vec![tilo_actions::InteractionTextRange {
            block: block.into(),
            start,
            end,
        }],
    }
}

fn semantic_text_fixture(text: &str) -> Framed<WordProcessorWidget> {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut().insert_str(&mut edits(&mut state.doc), text);
    mount(state, wp, 1100, 760)
}

#[test]
fn semantic_text_geometry_uses_unicode_character_endpoints_and_real_carets() {
    let mut framed = semantic_text_fixture("a😀éZ");
    let (wp, state) = split(&mut framed);
    let block = first_block_id(wp);
    let laid = &wp.laid[0];
    let layout = &wp.layout_cache[&block].1;
    let before = (
        state.doc.save(),
        wp.inner.caret.clone(),
        wp.scroll_y,
        wp.lifecycle.focused,
    );
    let expected = layout
        .selection_rects(1, 7)
        .into_iter()
        .map(|(x0, y0, x1, y1)| {
            Rect::new(
                laid.text_x + f64::from(x0),
                laid.layout_y + f64::from(y0),
                laid.text_x + f64::from(x1),
                laid.layout_y + f64::from(y1),
            )
        })
        .collect::<Vec<_>>();
    let transform = wp.content_transform(Affine::IDENTITY);
    assert_eq!(
        wp.semantic_text_rects(
            &semantic_text_anchor(&block, 1, 3),
            transform,
            wp.zoom_viewport
        ),
        Some(expected)
    );
    for (character, byte) in [(0, 0), (3, 7), (4, 8)] {
        let (x, y, h) = layout.caret_geometry(byte);
        let expected = Rect::new(
            laid.text_x + f64::from(x),
            laid.layout_y + f64::from(y),
            laid.text_x + f64::from(x) + CARET_WIDTH,
            laid.layout_y + f64::from(y + h),
        );
        assert_eq!(
            wp.semantic_text_rects(
                &semantic_text_anchor(&block, character, character),
                transform,
                wp.zoom_viewport
            ),
            Some(vec![expected])
        );
    }
    assert_eq!(
        (
            state.doc.save(),
            wp.inner.caret.clone(),
            wp.scroll_y,
            wp.lifecycle.focused
        ),
        before
    );
    let mut empty = semantic_text_fixture("");
    let empty = editor(&mut empty);
    let rects = empty
        .semantic_text_rects(
            &semantic_text_anchor(&first_block_id(empty), 0, 0),
            empty.content_transform(Affine::IDENTITY),
            empty.zoom_viewport,
        )
        .unwrap();
    assert_eq!(rects.len(), 1);
    assert!(rects[0].width() > 0.0 && rects[0].height() > 0.0);
}

#[test]
fn semantic_text_geometry_refuses_invalid_or_unretained_targets() {
    let mut framed = semantic_text_fixture("abc");
    let framed = editor(&mut framed);
    let block = first_block_id(framed);
    let transform = framed.content_transform(Affine::IDENTITY);
    let clip = framed.zoom_viewport;
    for anchor in [
        semantic_text_anchor(&block, 2, 1),
        semantic_text_anchor(&block, 0, 4),
        semantic_text_anchor(&block, u64::MAX, u64::MAX),
        semantic_text_anchor("missing", 0, 0),
    ] {
        assert!(
            framed
                .semantic_text_rects(&anchor, transform, clip)
                .is_none()
        );
    }
    for region in ["header", "footer", "unknown"] {
        let mut anchor = semantic_text_anchor(&block, 0, 1);
        let tilo_actions::InteractionAnchor::Text { region: target, .. } = &mut anchor else {
            unreachable!()
        };
        *target = region.into();
        assert!(
            framed
                .semantic_text_rects(&anchor, transform, clip)
                .is_none()
        );
    }
    for transform in [
        Affine::scale(0.0),
        Affine::scale(f64::NAN),
        Affine::rotate(0.5),
    ] {
        assert!(
            framed
                .semantic_text_rects(&semantic_text_anchor(&block, 0, 1), transform, clip)
                .is_none()
        );
    }
    framed.layout_cache.remove(&block);
    assert!(
        framed
            .semantic_text_rects(&semantic_text_anchor(&block, 0, 1), transform, clip)
            .is_none()
    );
}

#[test]
fn semantic_text_geometry_never_turns_an_image_hit_test_layout_into_a_caret() {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut()
        .insert_image_bytes(&mut edits(&mut state.doc), TEST_PNG);
    let mut framed = mount(state, wp, 1100, 760);
    let framed = editor(&mut framed);
    let image = framed
        .laid
        .iter()
        .find(|laid| laid.kind == "image")
        .unwrap();
    assert!(
        framed.layout_cache.contains_key(&image.id),
        "image fixture has the real empty hit-test layout"
    );
    assert!(
        framed
            .semantic_text_rects(
                &semantic_text_anchor(&image.id, 0, 0),
                framed.content_transform(Affine::IDENTITY),
                framed.zoom_viewport
            )
            .is_none()
    );
}

#[test]
fn semantic_text_geometry_clips_after_pan_and_zoom_without_edge_clamping() {
    let mut framed = semantic_text_fixture("abcdefgh");
    let framed = editor(&mut framed);
    let anchor = semantic_text_anchor(&first_block_id(framed), 0, 8);
    let transform = Affine::translate((31.0, 47.0)) * Affine::scale(1.75);
    let all = framed
        .semantic_text_rects(
            &anchor,
            transform,
            Rect::new(-10000.0, -10000.0, 10000.0, 10000.0),
        )
        .unwrap();
    assert_eq!(all.len(), 1);
    let bounds = transform.transform_rect_bbox(all[0]);
    let clip = Rect::new(bounds.center().x, bounds.y0, bounds.x1, bounds.y1);
    let partial = framed
        .semantic_text_rects(&anchor, transform, clip)
        .unwrap();
    assert_eq!(partial.len(), 1);
    let visible = transform.transform_rect_bbox(partial[0]);
    assert!((visible.x0 - clip.x0).abs() < 1e-8);
    assert!((visible.x1 - clip.x1).abs() < 1e-8);
    assert!(
        framed
            .semantic_text_rects(&anchor, transform, Rect::new(-100.0, -100.0, -50.0, -50.0))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn semantic_text_geometry_uses_each_visible_page_slices_layout_origin() {
    let text = "A paragraph spanning real page slices with wrapped words. ".repeat(300);
    let mut framed = semantic_text_fixture(&text);
    pipeline_draw(&mut framed, 1100, 3000);
    let framed = editor(&mut framed);
    let block = first_block_id(framed);
    let transform = framed.content_transform(Affine::IDENTITY);
    let anchor = semantic_text_anchor(&block, 0, text.chars().count() as u64);
    let rects = framed
        .semantic_text_rects(&anchor, transform, framed.zoom_viewport)
        .unwrap();
    let visible_slices = framed
        .laid
        .iter()
        .filter(|laid| laid.id == block)
        .filter_map(|laid| laid.clip)
        .filter(|clip| {
            let visible = transform
                .transform_rect_bbox(*clip)
                .intersect(framed.zoom_viewport);
            visible.width() > 0.0 && visible.height() > 0.0
        })
        .collect::<Vec<_>>();
    assert!(
        visible_slices.len() >= 2,
        "real pagination fixture must expose multiple slices"
    );
    for slice in visible_slices {
        assert!(
            rects.iter().any(|rect| {
                let hit = rect.intersect(slice);
                hit.width() > 0.0 && hit.height() > 0.0
            }),
            "every visible slice must resolve its own text lines"
        );
    }
    for rect in &rects {
        assert!(
            framed
                .laid
                .iter()
                .filter(|laid| laid.id == block)
                .filter_map(|laid| laid.clip)
                .any(|clip| clip.intersect(*rect) == *rect),
            "selection must not cross a page gap"
        );
    }
    let later = framed
        .laid
        .iter()
        .find(|laid| laid.id == block && laid.layout_y < laid.y_top - 1.0 && laid.clip.is_some())
        .unwrap();
    let (_, layout) = &framed.layout_cache[&block];
    let expected_line = layout
        .selection_rects(0, text.len())
        .into_iter()
        .find(|(_, y0, _, y1)| {
            let top = later.layout_y + f64::from(*y0);
            let bottom = later.layout_y + f64::from(*y1);
            top >= later.y_top && bottom <= later.y_top + later.height
        })
        .unwrap();
    let exact = Rect::new(
        later.text_x + f64::from(expected_line.0),
        later.layout_y + f64::from(expected_line.1),
        later.text_x + f64::from(expected_line.2),
        later.layout_y + f64::from(expected_line.3),
    );
    let only = framed
        .semantic_text_rects(&anchor, transform, transform.transform_rect_bbox(exact))
        .unwrap();
    assert!(!only.is_empty());
    assert!(
        only.iter()
            .all(|rect| (rect.y0 - exact.y0).abs() < 1e-8 && (rect.y1 - exact.y1).abs() < 1e-8)
    );
}

#[test]
fn remote_carets_are_labelled_by_peer_name_or_id() {
    {
        let mut wp = drawn_focused();
        let wp = editor(&mut wp);
        let presence = PresenceCtx::empty();
        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(carets.is_empty(), "no remote carets without peers");
        assert!(selections.is_empty(), "no remote selections without peers");
    }

    {
        let mut wp = drawn_focused();
        let wp = editor(&mut wp);
        let block_id = first_block_id(wp);
        let peer = "peer@5537458103208628978";

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret(peer.into(), block_presence_key(&block_id), 3);
        presence.set_display_name(ui::permissions::UserId(peer.into()), "Bob");

        let (carets, _) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].label, "Bob");
        assert_eq!(
            carets[0].user.0, peer,
            "the id stays the identity — it is what the caret is coloured by",
        );
    }

    {
        let mut wp = drawn_focused();
        let wp = editor(&mut wp);
        let block_id = first_block_id(wp);
        let peer = "peer@5537458103208628978";

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret(peer.into(), block_presence_key(&block_id), 3);

        let (carets, _) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].label, peer);
        assert!(!carets[0].label.trim().is_empty());
    }
}

#[test]
fn remote_carets_and_selections_resolve_to_screen_rects() {
    {
        let mut wp = drawn_focused();
        let wp = editor(&mut wp);
        let block_id = first_block_id(wp);

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret("alice".into(), block_presence_key(&block_id), 3);

        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1, "one remote caret resolved");
        assert!(
            selections.is_empty(),
            "a bare caret yields no selection rects"
        );
        let c = &carets[0];
        assert_eq!(c.user, UserId("alice".into()));
        let block = wp.laid.iter().find(|l| l.id == block_id).unwrap();
        assert!(
            (c.rect.width() - CARET_WIDTH).abs() < 1e-6,
            "caret is CARET_WIDTH wide: {:?}",
            c.rect
        );
        assert!(
            c.rect.height() > 1.0,
            "caret has a real height: {:?}",
            c.rect
        );
        assert!(
            c.rect.x0 >= block.text_x - 0.5,
            "caret sits at/after the block's text edge"
        );
        assert!(
            c.rect.y0 >= block.y_top - 0.5,
            "caret sits within the block vertically"
        );
    }

    {
        let mut wp = drawn_focused();
        let wp = editor(&mut wp);
        let block_id = first_block_id(wp);

        let mut presence = PresenceCtx::empty();
        presence.set_remote_selection("bob".into(), block_presence_key(&block_id), 0, 4);

        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(carets.is_empty(), "a selection yields no bare caret");
        assert_eq!(selections.len(), 1, "one remote selection resolved");
        let s = &selections[0];
        assert_eq!(s.user, UserId("bob".into()));
        assert!(
            !s.rects.is_empty(),
            "the selection produced at least one highlight rect"
        );
        for r in &s.rects {
            assert!(
                r.width() > 0.0 && r.height() > 0.0,
                "highlight rect is non-degenerate: {r:?}"
            );
        }
    }

    {
        let mut wp = drawn_focused();
        let wp = editor(&mut wp);
        let block_id = first_block_id(wp);

        let mut presence = PresenceCtx::empty();
        presence.selections.insert(
            block_presence_key(&block_id),
            vec![(UserId("bob".into()), SelectionRange { start: 2, end: 2 })],
        );

        let (_carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(
            selections.is_empty(),
            "an empty selection range paints nothing"
        );
    }
}

#[test]
fn remote_peers_resolve_independently_and_draw_through_production() {
    {
        let mut wp = drawn_focused();
        let wp = editor(&mut wp);
        assert!(
            wp.laid.len() >= 2,
            "the populated doc lays out at least two blocks"
        );
        let b0 = wp.laid[0].id.clone();
        let b1 = wp.laid[1].id.clone();

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret("alice".into(), block_presence_key(&b0), 1);
        presence.set_remote_caret("bob".into(), block_presence_key(&b1), 1);

        let (carets, _selections) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 2, "both peers' carets resolved");
        let users: Vec<&str> = carets.iter().map(|c| c.user.0.as_str()).collect();
        assert!(
            users.contains(&"alice") && users.contains(&"bob"),
            "got {users:?}"
        );
        let ya = carets.iter().find(|c| c.user.0 == "alice").unwrap().rect.y0;
        let yb = carets.iter().find(|c| c.user.0 == "bob").unwrap().rect.y0;
        assert!(
            (ya - yb).abs() > 1.0,
            "carets in different blocks have distinct y positions"
        );
    }

    {
        let mut framed = drawn_focused();
        let block_id = first_block_id(editor(&mut framed));

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret("charlie".into(), block_presence_key(&block_id), 2);

        Framed::harness_mut(&mut framed).set_presence_context(presence.clone());
        let (wp, _) = pipeline_draw(&mut framed, 1100, 760);

        let (carets, _sel) = wp.remote_presence_overlays(&presence);
        assert_eq!(
            carets.len(),
            1,
            "the production draw consumed the remote caret"
        );
        assert_eq!(carets[0].user, UserId("charlie".into()));
    }
}

#[test]
fn the_presence_payload_reports_focus_caret_and_selection() {
    {
        let (state, mut wp) = populated();
        wp.set_focus(true);
        let block = wp.model().caret().block.clone();
        wp.model_mut().set_caret(Caret {
            block: block.clone(),
            offset: 3,
        });
        let mut framed = mount(state, wp, 1100, 760);

        match ui::presence::capability::presence_payload(Framed::split(&mut framed).0) {
            Some((key, ui::PresencePayload::Caret { pos })) => {
                assert_eq!(
                    key,
                    block_presence_key(&block),
                    "keyed to the caret's block"
                );
                assert_eq!(pos, 3, "caret reported at byte 3");
            }
            other => panic!("expected a Caret payload, got {other:?}"),
        }
    }

    {
        let (state, mut wp) = populated();
        wp.set_focus(true);
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, _) = split(&mut framed);
        select_range(wp, 1, 4);
        let (wp, _) = pipeline_draw(&mut framed, 1100, 760);

        let block = wp.model().caret().block.clone();
        match ui::presence::capability::presence_payload(Framed::split(&mut framed).0) {
            Some((key, ui::PresencePayload::Selection { start, end })) => {
                assert_eq!(key, block_presence_key(&block));
                assert_eq!((start, end), (1, 4), "selection reported as a byte range");
            }
            other => panic!("expected a Selection payload, got {other:?}"),
        }
    }

    {
        let (state, wp) = populated();
        let mut framed = mount(state, wp, 1100, 760);
        let (wp, _) = split(&mut framed);
        wp.set_focus(false);
        assert!(
            ui::presence::capability::presence_payload(Framed::split(&mut framed).0).is_none(),
            "no presence while unfocused"
        );
    }
}

fn doc_with_selected_image() -> (Framed<WordProcessorWidget>, String) {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "caption");
    wp.model_mut()
        .insert_image_bytes(&mut edits(&mut state.doc), TEST_PNG);
    wp.sync(&state.doc, &state.root);
    let c = wp.model().containers.clone().unwrap();
    let img_id = tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .iter()
        .find(|b| b.kind == "image")
        .expect("image inserted")
        .id
        .clone();
    wp.model_mut().set_caret(crate::word_processor::Caret {
        block: img_id.clone(),
        offset: 0,
    });
    wp.set_styles_panel_open(true);
    (mount(state, wp, 1000, 700), img_id)
}

fn doc_with_custom_style() -> (Framed<WordProcessorWidget>, String) {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "styled paragraph");
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = split(&mut framed);
    wp.apply_panel_hit(&mut edits(&mut state.doc), PanelHit::NewStyle);
    wp.refresh_blocks(&state.doc);
    let new_id = wp
        .focused_style_key()
        .expect("the paragraph took the new style");
    pipeline_draw(&mut framed, 1000, 700);
    (framed, new_id)
}

#[test]
fn the_colour_picker_applies_and_cancels_previews() {
    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "colour me");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, state) = split(&mut framed);

        let (open_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| *h == PanelHit::OpenColorPicker)
            .expect("a 'Custom…' colour affordance in the inspector")
            .clone();
        left_click(
            wp,
            state,
            Point::new(
                f64::midpoint(open_rect.x0, open_rect.x1),
                f64::midpoint(open_rect.y0, open_rect.y1),
            ),
        );
        assert!(
            wp.color_picker.is_open(),
            "the shared ColorPicker popover opened"
        );

        wp.color_picker.layout(wp.rect, wp.panel_rect);
        let sv = wp
            .color_picker
            .part("color_picker.sv")
            .expect("the picker lays out its SV square");
        let sv_pt = Point::new(sv.x0 + 20.0, sv.y0 + 20.0);
        let style_before = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        let moved = Dispatch::new().doc(&mut state.doc).path(&state.root).send(
            &mut Lent(wp),
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (sv_pt.x, sv_pt.y).into(),
            },
        );
        Dispatch::new()
            .doc(&mut state.doc)
            .path(&state.root)
            .focus_target(moved.focus_target)
            .send(&mut Lent(wp), &ui::synthetic_input::mouse_press());
        let style_after = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        assert_ne!(
            style_before, style_after,
            "dragging in the ColorPicker changed the focused style's colour"
        );
    }

    {
        let (state, wp) = bound();
        let (mut framed, before) = preview_custom_colour(state, wp);
        let (wp, state) = split(&mut framed);
        press_named(wp, state, NamedKey::Escape);
        assert!(!wp.color_picker.is_open(), "Escape closes the picker");
        assert_eq!(
            wp.focused_style_key().map(|k| wp.sheet.get(&k).color),
            before,
            "Cancel restores the style's colour"
        );
    }
}

fn preview_custom_colour(
    mut state: ui::State,
    mut wp: Editor,
) -> (Framed<WordProcessorWidget>, Option<[u8; 3]>) {
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "preview me");
    wp.view.styles_panel_visible = true;
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = split(&mut framed);
    wp.open_color_picker();
    assert!(wp.color_picker.is_open(), "Custom… opens the picker");
    wp.color_picker.layout(wp.rect, wp.panel_rect);
    let before = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
    let sv = wp
        .color_picker
        .part("color_picker.sv")
        .expect("the picker lays out its SV square");
    left_click(wp, state, Point::new(sv.x0 + 20.0, sv.y0 + 20.0));
    Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root)
        .send(&mut Lent(wp), &ui::synthetic_input::mouse_release());
    assert_ne!(
        wp.focused_style_key().map(|k| wp.sheet.get(&k).color),
        before,
        "the pending colour previews on the style"
    );
    (framed, before)
}

#[test]
fn the_colour_picker_keeps_or_dismisses_its_preview() {
    {
        let (state, wp) = bound();
        let (mut framed, before) = preview_custom_colour(state, wp);
        let (wp, state) = split(&mut framed);
        let previewed = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        let done = wp
            .color_picker
            .part("color_picker.done")
            .expect("a confirm-mode picker has a Done button");
        wp.color_picker.layout(wp.rect, wp.panel_rect);
        left_click(wp, state, done.center());
        assert!(!wp.color_picker.is_open(), "Done closes the picker");
        let after = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        assert_eq!(after, previewed, "Done keeps the preview");
        assert_ne!(after, before);
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut edits(&mut state.doc), "x");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        wp.open_color_picker();
        assert!(wp.color_picker.is_open());
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        left_click(wp, state, Point::new(80.0, 400.0));
        assert!(
            !wp.color_picker.is_open(),
            "an outside click closes the colour picker"
        );
    }
}

#[test]
#[ignore = "GPU pixel equivalence; run explicitly when changing text clipping"]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end raster scenario that reads top to bottom: seed review state, render, compare, then re-check state"
)]
fn clipped_document_text_matches_reference_raster_and_preserves_review_state() {
    let body = format!(
        "Annotated text {}",
        "wrapped words for pagination ".repeat(220)
    );
    let (wp, mut state, c, mut ids, first) = doc_with_paragraph(&body);
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        let note = dm.insert_footnote(&first, 10);
        let note_block = dm.insert_note_block(&note, None, "paragraph").unwrap();
        dm.splice_note_text(
            &note,
            &note_block,
            0,
            0,
            "Footnote remains associated with its page.",
        );
        let change = dm.insert_change(tilo_doc::ChangeKind::Ins, "Reviewer", "");
        dm.add_change_mark(tilo_doc::ChangeKind::Ins, &first, 12, 20, &change);
        let mut after = first.clone();
        for i in 0..24 {
            let id = dm.insert_block(Some(&after), "paragraph");
            dm.splice_text(
                &id,
                0,
                0,
                &format!("Paragraph {i}. {}", "Café text with wrapping. ".repeat(30)),
            );
            after = id;
        }
    }
    crate::model::comment_by(
        &mut state.doc,
        "Reviewer",
        &first,
        (0, 9),
        "Keep this comment.",
    );
    let (w, h) = (900, 1100);
    let mut framed = mount(state, wp, w, h);
    let (wp, _) = split(&mut framed);
    wp.view.auto_fit_page = false;
    let mut tr = TextRenderer::new();
    let mut renderer = ui::offscreen::OffscreenRenderer::new(w, h);
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    for (scroll, zoom) in [(0.0, 1.0), (932.25, 0.87), (5000.5, 1.25)] {
        wp.zoom.set(zoom);
        wp.scroll_y = scroll;
        let mut scenes = Vec::new();
        let mut geometry = Vec::new();
        for reference in [true, false] {
            wp.reference_text_rendering = reference;
            let mut scene = Scene::new();
            wp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            wp.draw_overlay(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            geometry.push(
                wp.laid
                    .iter()
                    .map(|l| (l.id.clone(), l.y_top, l.height, l.page))
                    .collect::<Vec<_>>(),
            );
            scenes.push(scene);
        }
        assert_eq!(
            geometry[0], geometry[1],
            "clipping must not change pagination or hit geometry"
        );
        assert_eq!(wp.notes.len(), 1);
        assert_eq!(wp.review.comments().len(), 1);
        assert_eq!(wp.review.changes().len(), 1);
        let bg = Color::from_rgb8(0x20, 0x22, 0x28);
        let mut low = renderer.render(&scenes[0], bg);
        let mut high = low.clone();
        for _ in 0..63 {
            for ((low, high), value) in low
                .iter_mut()
                .zip(&mut high)
                .zip(renderer.render(&scenes[0], bg))
            {
                *low = (*low).min(value);
                *high = (*high).max(value);
            }
        }
        let variable = low.iter().zip(&high).filter(|(a, b)| a != b).count();
        assert!(
            variable <= 256 && low.iter().zip(&high).all(|(a, b)| b - a <= 1),
            "reference raster is not stable enough for this oracle: {variable} variable bytes"
        );
        for _ in 0..4 {
            let clipped = renderer.render(&scenes[1], bg);
            let mismatches = clipped
                .iter()
                .zip(low.iter().zip(&high))
                .filter(|(value, (low, high))| value < low || value > high)
                .count();
            if mismatches > 0 {
                eprintln!(
                    "outside envelope {:?}",
                    clipped
                        .iter()
                        .zip(low.iter().zip(&high))
                        .enumerate()
                        .filter(|(_, (v, (a, b)))| v < a || v > b)
                        .take(20)
                        .map(|(i, (v, (a, b)))| (
                            i / 4 % w as usize,
                            i / 4 / w as usize,
                            i % 4,
                            v,
                            a,
                            b
                        ))
                        .collect::<Vec<_>>()
                );
            }
            assert_eq!(
                mismatches, 0,
                "document raster exceeds reference-only envelope at scroll={scroll}, zoom={zoom}"
            );
        }
        let full_glyphs = scenes[0].encoding().resources.glyphs.len();
        let clipped_glyphs = scenes[1].encoding().resources.glyphs.len();
        assert!(
            clipped_glyphs * 3 < full_glyphs,
            "offscreen glyphs must be omitted: {clipped_glyphs}/{full_glyphs}"
        );
    }
}

fn review_shot_dir() -> std::path::PathBuf {
    std::env::var_os("TILO_WP_REVIEW_SHOTS")
        .map(std::path::PathBuf::from)
        .expect("set TILO_WP_REVIEW_SHOTS=<dir> to choose where review shots are written")
}

fn render_review_png<'a>(
    framed: &'a mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
    dir: &std::path::Path,
    name: &str,
) -> (&'a mut Editor, &'a mut ui::State) {
    render_review_png_themed(framed, w, h, dir, name, &Theme::dark());
    split(framed)
}

fn render_review_png_themed(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
    dir: &std::path::Path,
    name: &str,
    theme: &Theme,
) {
    use ui::offscreen::OffscreenRenderer;
    pipeline_draw_themed(framed, w, h, theme);
    std::fs::create_dir_all(dir).ok();
    OffscreenRenderer::new(w, h).render_png(
        Framed::harness(framed).scene(),
        Color::from_rgb8(0x20, 0x22, 0x28),
        dir.join(format!("{name}.png")),
    );
}

#[test]
#[ignore = "writes review PNGs and asserts nothing; run with TILO_WP_REVIEW_SHOTS=<dir> and --ignored"]
fn menu_ux_review_shots() {
    let dir = review_shot_dir();
    let dir = dir.as_path();
    for (name, theme, width) in [
        ("dark", Theme::dark(), 1000),
        ("light", Theme::light(), 1000),
        ("large", Theme::dark().with_text_scale(2.0), 480),
    ] {
        let (mut state, mut wp) = bound();
        wp.inner
            .insert_str(&mut edits(&mut state.doc), "Keep my draft");
        let mut framed = mount_themed(state, wp, width, 600, &theme);
        let (wp, state) = split(&mut framed);
        let point = first_block_start_point(wp);
        wp.open_context_menu(&state.doc, point, [num::f32_of_u32(width) - 2.0, 598.0]);
        wp.context_menu.focus_first_action();
        render_review_png_themed(
            &mut framed,
            width,
            600,
            dir,
            &format!("context-edge-{name}"),
            &theme,
        );
    }
}

#[test]
#[ignore = "writes review PNGs and asserts nothing; run with TILO_WP_REVIEW_SHOTS=<dir> and --ignored"]
fn render_review_shots() {
    let dir = review_shot_dir();
    let (w, h) = (1200u32, 800u32);

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "The quick brown fox");
        wp.model_mut().enter(&mut edits(&mut state.doc));
        wp.model_mut().insert_str(
            &mut edits(&mut state.doc),
            "Body paragraph with some words to fill the line nicely.",
        );
        let mut framed = mount(state, wp, w, h);
        render_review_png(&mut framed, w, h, &dir, "toolbar");
    }

    {
        let (mut framed, custom_id) = doc_with_custom_style();
        let (wp, _) = split(&mut framed);
        let anchor = wp
            .panel_hits
            .iter()
            .find(|(_, hh)| matches!(hh, PanelHit::Pick(id) if id == &custom_id))
            .map_or(Point::new(950.0, 240.0), |(r, _)| {
                Point::new(r.x0 + 10.0, r.y1 + 2.0)
            });
        wp.style_menu.open(&custom_id, anchor);
        render_review_png(&mut framed, w, h, &dir, "style-menu-custom");
    }

    {
        let (mut framed, _id) = doc_with_custom_style();
        let (wp, _) = split(&mut framed);
        let anchor = wp
            .panel_hits
            .iter()
            .find(|(_, hh)| matches!(hh, PanelHit::Pick(id) if id == "body"))
            .map_or(Point::new(950.0, 240.0), |(r, _)| {
                Point::new(r.x0 + 10.0, r.y1 + 2.0)
            });
        wp.style_menu.open("body", anchor);
        render_review_png(&mut framed, w, h, &dir, "style-menu-builtin");
    }

    {
        let (mut framed, id) = doc_with_selected_image();
        let (wp, state) = split(&mut framed);
        wp.perform_action(
            &mut edits(&mut state.doc),
            &crate::actions::set_image_size_action(&id, 300, 200),
        );
        wp.perform_action(
            &mut edits(&mut state.doc),
            &crate::actions::set_block_attr_action(
                crate::actions::RegionKind::Body,
                vec![id],
                "flip_h",
                crate::actions::AttrVal::Bool(true),
            ),
        );
        render_review_png(&mut framed, w, h, &dir, "image-selected");
    }

    {
        let mut framed = doc_with_headings(w, h);
        let (wp, state) = split(&mut framed);
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertToc);
        render_review_png(&mut framed, w, h, &dir, "toc");
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "colour this paragraph");
        wp.view.styles_panel_visible = true;
        let mut framed = mount(state, wp, w, h);
        let (wp, _) = split(&mut framed);
        wp.open_color_picker();
        render_review_png(&mut framed, w, h, &dir, "color-picker");
    }

    {
        let (wp, mut state, c, mut ids, bid) =
            doc_with_paragraph("Keep this passage and review the ending.");
        {
            let mut doc = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let change = doc.insert_change(tilo_doc::ChangeKind::Ins, "Lin", "");
            doc.add_change_mark(tilo_doc::ChangeKind::Ins, &bid, 29, 39, &change);
        }
        crate::model::comment_by(&mut state.doc, "Ada", &bid, (5, 17), "This needs a source.");
        state.doc.commit();
        let mut framed = mount(state, wp, w, h);
        render_review_png(&mut framed, w, h, &dir, "review-overlay");
    }
}

#[test]
#[ignore = "writes review PNGs and asserts nothing; run with TILO_WP_REVIEW_SHOTS=<dir> and --ignored"]
fn render_responsive_review_shots() {
    let dir = review_shot_dir();
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut().insert_str(
        &mut edits(&mut state.doc),
        "A responsive Writer document with direct formatting, comments, and accessible controls.",
    );
    wp.model_mut().enter(&mut edits(&mut state.doc));
    wp.model_mut().insert_str(
        &mut edits(&mut state.doc),
        "The window bar carries the commands; the page keeps the whole window.",
    );
    wp.set_focus(true);

    let mut framed = mount(state, wp, 1280, 800);
    render_review_png(&mut framed, 1280, 800, &dir, "responsive-1280-focus");
    render_review_png(&mut framed, 1800, 900, &dir, "responsive-1800-full");
    render_review_png(&mut framed, 760, 800, &dir, "responsive-760-narrow");

    let (wp, _) = render_review_png(
        &mut framed,
        1280,
        800,
        &dir,
        "responsive-1280-before-palette",
    );
    wp.open_compact_swatch_palette(PaletteKind::Text);
    let (wp, _) = render_review_png(&mut framed, 1280, 800, &dir, "responsive-1280-text-palette");
    wp.compact_swatch_palette.close();
    let (wp, _) = render_review_png(&mut framed, 760, 800, &dir, "responsive-760-before-palette");
    wp.open_compact_swatch_palette(PaletteKind::Highlight);
    render_review_png(
        &mut framed,
        760,
        800,
        &dir,
        "responsive-760-highlight-palette",
    );
}

#[test]
fn pinch_and_ctrl_wheel_only_zoom_the_host_focused_writer() {
    use winit::{
        dpi::PhysicalPosition,
        event::{DeviceId, MouseScrollDelta, TouchPhase},
        keyboard::ModifiersState,
    };

    let drawn_at_rest = || {
        let (state, mut writer) = populated();
        writer.set_zoom(1.0);
        let mut framed = mount(state, writer, 1000, 700);
        editor(&mut framed).pointer = Point::new(500.0, 300.0);
        framed
    };
    let mut first_framed = drawn_at_rest();
    let mut second_framed = drawn_at_rest();
    let (first, first_state) = split(&mut first_framed);
    let (second, second_state) = split(&mut second_framed);

    first.set_focus(false);
    second.set_focus(true);
    assert!(
        !pinch(first, first_state, 0.025, TouchPhase::Moved),
        "an unfocused Writer must leave pinch for the selected sibling"
    );
    assert!(pinch(second, second_state, 0.025, TouchPhase::Moved));
    assert_eq!(first.zoom.factor(), 1.0);
    assert_eq!(second.zoom.factor(), 1.025);

    let ctrl: Modifiers = ModifiersState::CONTROL.into();
    let ctrl_wheel = WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 24.0)),
        phase: TouchPhase::Moved,
    };
    let first_handled = Dispatch::new()
        .modifiers(ctrl)
        .doc(&mut first_state.doc)
        .path(&first_state.root)
        .send(&mut Lent(first), &ctrl_wheel)
        .handled;
    assert!(
        !first_handled,
        "an unfocused Writer must leave browser Ctrl+wheel for the selected sibling"
    );
    assert_eq!(first.zoom.factor(), 1.0);
    let second_handled = Dispatch::new()
        .modifiers(ctrl)
        .doc(&mut second_state.doc)
        .path(&second_state.root)
        .send(&mut Lent(second), &ctrl_wheel)
        .handled;
    assert!(second_handled, "the focused Writer owns Ctrl+wheel zoom");
    assert_eq!(
        second.zoom.factor(),
        1.1,
        "Ctrl+wheel uses Writer's tenth-step zoom quantization"
    );

    first.set_focus(true);
    second.set_focus(false);
    assert!(pinch(first, first_state, 0.025, TouchPhase::Moved));
    assert_eq!(first.zoom.factor(), 1.025);
    assert_eq!(second.zoom.factor(), 1.1);

    first.set_focus(false);
    let first_before = first.zoom.factor();
    let second_before = second.zoom.factor();
    let plain_wheel = WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -24.0)),
        phase: TouchPhase::Moved,
    };
    let first_scroll_before = first.scroll_y;
    let plain_handled = Dispatch::new()
        .doc(&mut first_state.doc)
        .path(&first_state.root)
        .send(&mut Lent(first), &plain_wheel)
        .handled;
    assert!(plain_handled, "plain wheel remains pointer-routed");
    assert!(
        first.scroll_y > first_scroll_before,
        "plain wheel pans overflow"
    );
    assert!(!pinch(first, first_state, 0.025, TouchPhase::Moved));
    assert!(!pinch(second, second_state, 0.025, TouchPhase::Moved));
    assert_eq!(first.zoom.factor(), first_before);
    assert_eq!(second.zoom.factor(), second_before);
}

#[test]
fn a_digest_addressed_remote_caret_resolves_to_its_block() {
    let mut wp = drawn_focused();
    let wp = editor(&mut wp);
    let block_id = first_block_id(wp);
    let digest = block_presence_key(&block_id).digest_form().unwrap();
    assert!(is_scope_digest_key(&digest));
    let peer = "agent-publisher";
    let mut presence = PresenceCtx::empty();
    presence.set_remote_caret(peer.into(), digest.clone(), 3);
    presence.set_selection(
        &digest,
        UserId(peer.into()),
        SelectionRange { start: 0, end: 2 },
    );
    let (carets, selections) = wp.remote_presence_overlays(&presence);
    assert_eq!(carets.len(), 1, "digest caret drawn once, in its block");
    assert_eq!(selections.len(), 1);
    let mut stray = PresenceCtx::empty();
    stray.set_remote_caret(
        peer.into(),
        ui::presence::PresenceKey::from_scope_digest([7; 16]),
        1,
    );
    assert!(wp.remote_presence_overlays(&stray).0.is_empty());
}

fn confirm_asked(wp: &Editor, doc: &mut automerge::AutoCommit) {
    let dispatch = wp.dispatch.as_ref().expect("the editor is hosted");
    let mut document =
        ui::tilo_resource::Replica::<crate::framework::Document>::from_doc(doc.clone());
    for action in dispatch.take_asked().expect("what was asked builds") {
        document
            .apply_action(&action)
            .expect("a confirmed change applies");
    }
    *doc = document.doc().clone();
}

#[test]
fn the_shared_selection_is_the_place_its_actions_read() {
    let (state, mut wp) = populated();
    wp.set_focus(true);
    let mut framed = mount(state, wp, 1100, 760);
    let (wp, _) = split(&mut framed);
    select_range(wp, 1, 4);
    let (wp, _) = pipeline_draw(&mut framed, 1100, 760);
    let block = wp.model().caret().block.clone();
    let selection =
        ui::shared_selection::selection_of(Framed::split(&mut framed).0).expect("a selection");
    assert_eq!(
        selection.anchor.place,
        Some(tilo_actions::SelectionPlace::range(block.clone(), 1, 4))
    );
    assert_eq!(
        selection
            .anchor
            .excerpt
            .as_deref()
            .map(|excerpt| excerpt.chars().count()),
        Some(3),
        "the excerpt is the three selected characters"
    );
    let place: tilo_resource::Place = selection.anchor.place.unwrap().try_into().unwrap();
    assert_eq!(
        place,
        tilo_resource::Place::Range {
            item: block,
            start: 1,
            end: 4
        }
    );
    editor(&mut framed).set_focus(false);
    assert!(ui::shared_selection::selection_of(Framed::split(&mut framed).0).is_none());
}

#[test]
fn collaborator_presence_cannot_escape_the_canvas() {
    let mut framed = drawn_focused();
    let framed = editor(&mut framed);
    let block = framed.laid.last().expect("a laid block").clone();
    let mut presence = PresenceCtx::empty();
    let user = UserId("ada".into());
    let key = block_presence_key(&block.id);
    presence.set_display_name(user.clone(), "Ada");
    presence.carets.insert(key.clone(), vec![(user.clone(), 0)]);
    presence
        .selections
        .insert(key, vec![(user, SelectionRange { start: 0, end: 9 })]);
    let base = framed.content_transform(Affine::IDENTITY);
    let canvas = framed.zoom_viewport;
    let top = (base * Point::new(block.text_x, block.y_top)).y;
    let ctf = Affine::translate((0.0, canvas.y0 - top - 5.0)) * base;
    let mut scene = Scene::new();
    framed.draw_remote_overlays(
        &mut scene,
        ctf,
        canvas,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &presence,
    );
    let (width, height) = (1100_u32, 760_u32);
    let pixels = ui::offscreen::OffscreenRenderer::new(width, height)
        .render(&scene, vello::peniko::Color::TRANSPARENT);
    let mut inside = 0;
    let mut pixel = pixels.chunks_exact(4);
    for y in 0..height {
        for x in 0..width {
            let alpha = pixel.next().expect("a pixel per point")[3];
            let point = Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            if canvas.contains(point) {
                inside += usize::from(alpha > 0);
            } else {
                assert_eq!(alpha, 0, "presence painted outside the canvas at {point:?}");
            }
        }
    }
    assert!(inside > 50, "the partly visible selection is painted");
}

#[test]
fn ui_scaling_grows_the_writing_beside_the_review_cards() {
    for scale in [1.0, 1.5, 2.0] {
        let theme = Theme::dark().with_ui_scale(scale);
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        seed_text(
            &mut wp,
            &mut state,
            "A paragraph with enough words to wrap while keeping readable type.",
        );
        wp.sync(&state.doc, &state.root);
        let block = wp.blocks[0].id.clone();
        crate::model::comment_by(&mut state.doc, "Ada", &block, (0, 11), "Keep this.");
        wp.sync(&state.doc, &state.root);
        let mut framed = mount_themed(state, wp, 1440, 810, &theme);
        let framed = editor(&mut framed);
        let body = framed.laid.first().expect("the paragraph is laid");
        assert!(
            f64::from(body.size) * framed.zoom.factor() >= 16.0 * f64::from(scale) - 0.01,
            "UI scale {scale}: the writing grows with the controls"
        );
        let to_window = framed.zoom.transform(Affine::IDENTITY);
        let page = to_window.transform_rect_bbox(framed.page_rect);
        let canvas = framed.zoom_viewport;
        assert!(
            page.x0 >= canvas.x0 && page.x1 <= canvas.x1,
            "UI scale {scale}: the sheet fits its canvas: {page:?} in {canvas:?}"
        );
        let (review_x, _) = ReviewOverlay::column(
            &theme,
            &crate::review_overlay::ReviewPlacement {
                canvas,
                page_x1: page.x1,
                to_window,
            },
        );
        assert!(
            review_x >= page.x1,
            "UI scale {scale}: review cards stand beside the sheet"
        );
    }
}

#[test]
fn a_double_click_selects_the_whole_word_and_its_drag_grows_by_words() {
    let mut state = crate::initialized_state();
    let mut wp = Editor::detached();
    wp.sync(&state.doc, &state.root);
    seed_text(&mut wp, &mut state, "Meeting notes today");
    let mut framed = mount(state, wp, 1000, 700);
    let (wp, state) = split(&mut framed);
    let line = wp
        .laid
        .iter()
        .find(|l| l.text == "Meeting notes today")
        .expect("the paragraph is laid out")
        .clone();
    let y = line.y_top + line.height / 2.0;
    let x_at = |wp: &mut Editor, offset: usize| {
        (0..600)
            .map(|dx| line.text_x + f64::from(dx))
            .find(|&x| {
                wp.caret_from_pointer(Point::new(x, y), false);
                wp.inner.caret.offset == offset
            })
            .expect("a point lands on the offset")
    };
    let inside_meeting = x_at(wp, 4);
    let inside_today = x_at(wp, 16);
    let screen = |wp: &Editor, x: f64| wp.content_transform(Affine::IDENTITY) * Point::new(x, y);
    let send = |wp: &mut Editor, state: &mut ui::State, event: &WindowEvent| {
        Dispatch::new()
            .doc(&mut state.doc)
            .path(&state.root)
            .send(&mut Lent(wp), event);
    };
    let moved = |pt: Point| WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: (pt.x, pt.y).into(),
    };
    let span = |wp: &Editor| {
        let anchor = wp.inner.selection_anchor.as_ref().map(|a| a.offset);
        (anchor, wp.inner.caret.offset)
    };
    let at = screen(wp, inside_meeting);
    left_click(wp, state, at);
    send(wp, state, &ui::synthetic_input::mouse_release());
    left_click(wp, state, at);
    assert_eq!(
        span(wp),
        (Some(0), 7),
        "the double-click selects \"Meeting\""
    );
    send(wp, state, &moved(at));
    assert_eq!(
        span(wp),
        (Some(0), 7),
        "a move inside the word keeps all of it"
    );
    send(wp, state, &moved(screen(wp, inside_today)));
    assert_eq!(span(wp), (Some(0), 19), "dragging on selects whole words");
    send(wp, state, &ui::synthetic_input::mouse_release());
    assert_eq!(span(wp), (Some(0), 19));
}

#[test]
fn a_narrow_sheets_ruler_marks_the_text_where_it_stands() {
    let (mut state, mut wp) = bound();
    wp.model_mut()
        .insert_str(&mut edits(&mut state.doc), "para");
    wp.dispatch_block_attr(
        &mut edits(&mut state.doc),
        "indent",
        crate::actions::AttrVal::Float(24.0),
    );
    let mut framed = mount(state, wp, 420, 760);
    let (wp, _) = split(&mut framed);
    let (page_x, page_w) = (wp.page_rect.x0, wp.page_rect.width());
    assert!(
        page_w < MAX_PAGE_W,
        "a narrow window edits on a narrower sheet"
    );
    let (_, shown) = pagination::responsive_page_geometry(
        wp.page_rect.x0 - 12.0,
        wp.page_rect.x1 + 12.0,
        1.0,
        wp.margins,
        true,
        false,
    );
    let markers = wp.ruler_markers(page_x, page_w);
    let at = |marker: RulerMarker| {
        markers
            .iter()
            .find(|(kind, _)| *kind == marker)
            .map_or_else(|| panic!("{marker:?} is on the ruler"), |(_, x)| *x)
    };
    assert!(
        (at(RulerMarker::MarginLeft) - (page_x + shown.left)).abs() < 1e-6,
        "the left margin marker stands at the text's left edge"
    );
    assert!(
        (at(RulerMarker::MarginRight) - (page_x + page_w - shown.right)).abs() < 1e-6,
        "the right margin marker stands at the text's right edge"
    );
    assert!(
        (at(RulerMarker::IndentLeft) - (page_x + shown.left + 24.0)).abs() < 1e-6,
        "the indent marker stands at the indented text"
    );
}
