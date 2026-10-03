#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::widget::Scrollable;

#[test]
fn scrollable_offset_clamps_reveals_and_decays() {
    // Case `scrollable_clamps_to_content_bounds`.
    {
        let mut s = Scrollable::new_vertical();
        s.set_content_size(200.0, 1000.0);
        s.set_viewport_size(300.0, 100.0);
        s.scroll_by(0.0, 50_000.0);
        assert_eq!(s.offset_y(), 900.0);
        s.scroll_by(0.0, -50_000.0);
        assert_eq!(s.offset_y(), 0.0);
    }

    // Case `scrollable_scroll_into_view_vertical`.
    {
        let mut s = Scrollable::new_vertical();
        s.set_content_size(200.0, 1000.0);
        s.set_viewport_size(200.0, 100.0);
        s.scroll_into_view(0.0, 600.0, 200.0, 20.0);
        assert!(s.offset_y() >= 520.0 && s.offset_y() <= 600.0);
    }

    // Case `scrollable_momentum_decays`.
    {
        let mut s = Scrollable::new_vertical();
        s.set_content_size(200.0, 10000.0);
        s.set_viewport_size(200.0, 500.0);
        s.fling(0.0, 1500.0);
        let before = s.offset_y();
        for _ in 0..120 {
            s.tick(1.0 / 60.0);
        }
        let after = s.offset_y();
        assert!(after > before);
        assert!(s.momentum_speed() < 10.0);
    }
}

use tilo_ui::State;
use tilo_ui::headless::Driver;
use tilo_ui::local_state::LocalState;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Button, Column, Container, Text, TextInput};

fn editor_driver(text: &LocalState<String>, fill: bool, scale: f32) -> Driver {
    let mut scroll = Scrollable::new_vertical().fill_parent_height();
    if fill {
        scroll = scroll.with_content_min_viewport_height();
    }
    let root = Column::new()
        .with_no_padding()
        .with_gap(0.0)
        .push(Box::new(
            scroll.with_child(Box::new(
                TextInput::from_state(text)
                    .multiline(true)
                    .with_id("editor"),
            )),
        ))
        .push(Box::new(Button::new("footer", "Save", None)));
    let theme = Theme::dark().with_text_scale(scale);
    Driver::new_with_theme(State::new(), Box::new(root), (600, 400), theme)
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn writing_surface_fills_the_viewport_and_settles_across_changes() {
    // Case `empty_writing_surface_fills_viewport_and_accepts_lower_click`.
    {
        for scale in [1.0, 2.0] {
            let text = LocalState::new(String::new());
            let mut driver = editor_driver(&text, true, scale);
            let targets = driver.targets();
            let editor = *targets.get("editor").unwrap();
            let footer = *targets.get("footer").unwrap();
            assert!(
                (editor.y1 - footer.y0).abs() < 1.0,
                "editor {editor:?}, footer {footer:?}"
            );
            assert!(editor.height() > 250.0);
            driver.click_at(editor.x0 + 30.0, editor.y1 - 30.0);
            driver.type_text("lower click");
            assert_eq!(text.get(), "lower click");
            driver.click_at(editor.x1 - 30.0, editor.y1 - 30.0);
            driver.type_text(" at end");
            assert_eq!(text.get(), "lower click at end");
            assert!(driver.targets().get("footer").unwrap().y1 <= 400.0);
        }
    }

    // Case `default_short_multiline_remains_content_sized`.
    {
        let text = LocalState::new(String::new());
        let driver = editor_driver(&text, false, 1.0);
        let targets = driver.targets();
        assert!(targets.get("editor").unwrap().height() < 60.0);
        assert!(targets.get("footer").unwrap().y0 > 300.0);
    }

    // Case `long_writing_surface_retains_overflow_and_footer`.
    {
        let text = LocalState::new("A line of the document.\n".repeat(80));
        let mut driver = editor_driver(&text, true, 1.0);
        let initial = *driver.targets().get("editor").unwrap();
        let footer = *driver.targets().get("footer").unwrap();
        assert!(initial.height() > 1000.0, "{initial:?}");
        driver.click_at(30.0, 100.0);
        driver.scroll(0.0, -800.0);
        let moved = *driver.targets().get("editor").unwrap();
        assert!(
            moved.y0 < initial.y0 - 100.0,
            "before {initial:?}, after {moved:?}"
        );
        assert_eq!(*driver.targets().get("footer").unwrap(), footer);
        assert!(footer.y1 <= 400.0);
    }

    // Case `writing_surface_settles_resize_and_short_long_short_changes`.
    {
        let text = LocalState::new(String::new());
        let mut driver = editor_driver(&text, true, 1.0);
        for width in [280, 760, 280, 760] {
            driver.resize((width, 400)).unwrap();
            for body in [
                String::new(),
                "Wrapping document text with some words. ".repeat(100),
                String::new(),
            ] {
                let long = !body.is_empty();
                text.set(body);
                driver.relayout();
                let targets = driver.targets();
                let editor = targets.get("editor").unwrap();
                let footer = targets.get("footer").unwrap();
                assert!(
                    (editor.width() - f64::from(width)).abs() < 1.0,
                    "{editor:?}"
                );
                if long {
                    assert!(editor.height() > 400.0, "{editor:?}");
                } else {
                    assert!(
                        (editor.y1 - footer.y0).abs() < 1.0,
                        "editor {editor:?}, footer {footer:?}"
                    );
                }
            }
        }
    }

    // Case `writing_surface_preserves_explicit_child_height`.
    {
        // The explicit Container remains 72px even though its wrapper fills 300px.
        let root = Scrollable::new_vertical()
            .with_height(300.0)
            .with_content_min_viewport_height()
            .with_child(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_height(72.0)
                    .with_child(Box::new(Text::new("Fixed content").with_id("fixed"))),
            ));
        let driver = Driver::new(State::new(), Box::new(root), (500, 300));
        assert!((driver.targets().get("fixed").unwrap().height() - 72.0).abs() < 1.0);
    }

    // Case `writing_surface_retained_tree_settles_without_changing_child_style`.
    {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Dimension, Size, TaffyTree};
        use tilo_ui::bound_dispatch::BoundNodeMap;
        use tilo_ui::text_render::TextRenderer;
        use tilo_ui::widget::{Widget, compute_widget_layout};
        use vello::kurbo::Point;

        let text = LocalState::new(String::new());
        // Also exercise child-before-opt-in builder ordering and repeated opt-in.
        let mut scroll = Scrollable::new_vertical()
            .with_child(Box::new(TextInput::from_state(&text).multiline(true)))
            .with_content_min_viewport_height()
            .with_content_min_viewport_height();
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = scroll.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        let wrapper = *tree.children(root).unwrap().first().unwrap();
        let editor = *tree.children(wrapper).unwrap().first().unwrap();
        let child_style = tree.style(editor).unwrap().clone();
        let mut bindings = BoundNodeMap::new();
        // SAFETY: one stable tree, exclusively owned throughout the test.
        unsafe {
            bindings.rebuild(&scroll, &tree, root, &nodes);
        }
        let mut renderer = TextRenderer::new();
        let mut doc = automerge::AutoCommit::new();
        for (width, height) in [(280.0, 180.0), (760.0, 420.0), (280.0, 300.0)] {
            let mut style = tree.style(root).unwrap().clone();
            style.size.height = Dimension::length(height);
            tree.set_style(root, style).unwrap();
            for body in [
                String::new(),
                "A document line.\n".repeat(80),
                String::new(),
            ] {
                let long = !body.is_empty();
                text.set(body);
                scroll.sync(&mut doc, &automerge::ROOT);
                tree.mark_dirty(editor).unwrap();
                // SAFETY: no widgets move or change topology during settlement.
                unsafe {
                    compute_widget_layout(
                        &mut tree,
                        root,
                        Size {
                            width: AvailableSpace::Definite(width),
                            height: AvailableSpace::Definite(height),
                        },
                        &nodes,
                        &bindings,
                        &mut renderer,
                    )
                }
                .unwrap();
                scroll.apply_layout(&tree, root, Point::ZERO);
                assert_eq!(tree.style(editor).unwrap(), &child_style);
                let editor_height = tree.layout(editor).unwrap().size.height;
                if long {
                    assert!(editor_height > 1000.0);
                    assert!(scroll.max_offset_y() > 500.0);
                } else {
                    assert!((editor_height - height).abs() < 1.0);
                    assert_eq!(scroll.max_offset_y(), 0.0);
                }
            }
        }
    }
}

#[test]
fn scrollable_editor_routes_wheel_and_keeps_focus() {
    // Case `fractional_height_editor_routes_first_wheel_to_outer_scrollable`.
    {
        // 47 nonempty rows at 22.625px plus padding end in .375px; rounding
        // down must not create an inner scroll range in a fully laid-out input.
        let root = Scrollable::new_vertical()
            .with_height(300.0)
            .with_content_min_viewport_height()
            .with_child(Box::new(
                TextInput::local()
                    .with_id("fractional-editor")
                    .multiline(true)
                    .with_text_role(tilo_ui::TextRole::Reading)
                    .with_initial_value(&("A line of text.\n".repeat(46) + "Final line")),
            ));
        let mut driver = Driver::new(State::new(), Box::new(root), (416, 300));
        driver.click_at(30.0, 30.0);
        let before = *driver.targets().get("fractional-editor").unwrap();
        driver.scroll(0.0, -3.0);
        let after = *driver.targets().get("fractional-editor").unwrap();
        assert!(
            after.y0 < before.y0 - 100.0,
            "first wheel must move outer content: before {before:?}, after {after:?}"
        );
    }

    // Case `repeated_click_keeps_default_scrollable_editor_focused`.
    {
        let text = LocalState::new(String::new());
        let mut driver = editor_driver(&text, false, 1.0);
        let editor = *driver.targets().get("editor").unwrap();
        driver.click_at(editor.x0 + 25.0, editor.center().y);
        driver.type_text("first");
        driver.click_at(editor.x1 - 25.0, editor.center().y);
        driver.type_text(" again");
        assert_eq!(text.get(), "first again");
    }

    // Case `pane_background_and_scrollbar_press_still_focus_the_scrollable`.
    {
        for x in [250.0, 394.0] {
            let root = Scrollable::new_vertical()
                .with_height(300.0)
                .with_child(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_size(100.0, 900.0)
                        .with_child(Box::new(
                            Text::new("Tall narrow content").with_id("content"),
                        )),
                ));
            let mut driver = Driver::new(State::new(), Box::new(root), (400, 300));
            // x=250 hits empty pane background; x=394 hits the scrollbar thumb.
            driver.click_at(x, 20.0);
            assert_eq!(
                driver.focused_access_node().unwrap().role,
                tilo_ui::a11y::Role::Group
            );
            let before = driver.targets().get("content").unwrap().y0;
            driver.key("ArrowDown");
            assert!(driver.targets().get("content").unwrap().y0 < before);
        }
    }
}

fn tall_scroll_driver(theme: Theme) -> Driver {
    let root = Scrollable::new_vertical()
        .with_height(300.0)
        .with_child(Box::new(
            Container::new()
                .with_no_padding()
                .with_size(100.0, 20_000.0)
                .with_child(Box::new(Text::new("Tall content").with_id("content"))),
        ));
    let mut driver = Driver::new_with_theme(State::new(), Box::new(root), (400, 300), theme);
    driver.send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(150.0, 150.0),
    });
    driver
}

/// A two-finger touchpad stroke: pixel deltas bracketed by its phases.
fn touchpad_flick(driver: &mut Driver, strokes: usize, step: f64) {
    use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};
    let wheel = |delta: f64, phase| WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, delta)),
        phase,
    };
    driver.send(&wheel(-step, TouchPhase::Started));
    for _ in 1..strokes {
        driver.send(&wheel(-step, TouchPhase::Moved));
    }
    driver.send(&wheel(0.0, TouchPhase::Ended));
    driver.relayout();
}

fn content_top(driver: &Driver) -> f64 {
    driver.targets().get("content").unwrap().y0
}

#[test]
fn touchpad_flick_keeps_gliding_after_release_then_settles() {
    let mut driver = tall_scroll_driver(Theme::dark().with_reduce_motion(false));
    let start = content_top(&driver);
    touchpad_flick(&mut driver, 6, 20.0);
    let released = content_top(&driver);
    assert!(
        start - released >= 120.0,
        "pixel deltas move the content at least as far as the fingers: {start} -> {released}"
    );
    driver.relayout();
    driver.relayout();
    let gliding = content_top(&driver);
    assert!(
        gliding < released - 1.0,
        "a fast lift keeps the content moving: released {released}, then {gliding}"
    );
    let mut last = gliding;
    let mut settled = false;
    for _ in 0..600 {
        driver.relayout();
        let now = content_top(&driver);
        assert!(now <= last, "the glide never reverses: {last} -> {now}");
        if now.to_bits() == last.to_bits() {
            settled = true;
            break;
        }
        last = now;
    }
    assert!(settled, "the glide decelerates to rest, still at {last}");
    assert!(
        last > -19_000.0,
        "the glide settled on its own rather than at the content end: {last}"
    );
}

#[test]
fn reduced_motion_touchpad_flick_stops_on_release() {
    let mut driver = tall_scroll_driver(Theme::dark().with_reduce_motion(true));
    let start = content_top(&driver);
    touchpad_flick(&mut driver, 6, 20.0);
    let released = content_top(&driver);
    assert!(
        (start - released - 120.0).abs() < 1.0,
        "{start} -> {released}"
    );
    for _ in 0..10 {
        driver.relayout();
    }
    assert_eq!(content_top(&driver), released);
}
