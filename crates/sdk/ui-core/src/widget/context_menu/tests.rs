use super::*;
use crate::synthetic_input::mouse_press;
use crate::synthetic_input::mouse_release;
use crate::synthetic_input::named_key;
use crate::testing::dispatch;
use automerge::AutoCommit;
use std::sync::Arc;

/// A menu is a floating Panel in the window's top layer: `draw` (the
/// in-flow pass, where later siblings paint over it) paints nothing, and
/// `draw_overlay` (run after the app and the window's chrome) paints it.
#[test]
fn an_open_menu_paints_only_in_the_top_layer() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut menu = ContextMenu::new();
    menu.push(MenuItem::action("Open", || {}));
    menu.push(MenuItem::action("Rename", || {}));
    menu.open_at([40.0, 40.0]);
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut in_flow = Scene::new();
    menu.draw(&mut in_flow, Affine::IDENTITY, &mut text, &theme, &presence);
    assert!(
        in_flow.encoding().is_empty(),
        "the in-flow pass paints nothing"
    );
    let mut top = Scene::new();
    menu.draw_overlay(&mut top, Affine::IDENTITY, &mut text, &theme, &presence);
    assert!(!top.encoding().is_empty(), "the top layer paints the menu");
}

/// A context menu a click opens comes from the pointer (`motion::Appear`):
/// its rows are hit where they are drawn while it arrives, it is drawn
/// while it leaves but takes no input then, and a nested menu that
/// follows the pointer across rows is simply there.
#[test]
fn a_clicked_menu_comes_from_the_pointer_and_fades_when_it_closes() {
    use crate::time::testing::with_monotonic_time;
    let theme = Theme::dark();
    let presence = PresenceCtx::default();
    let mut text = TextRenderer::new();
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
    let mut menu = ContextMenu::new();
    menu.push(MenuItem::action("Open", || {}));
    menu.push(MenuItem::submenu(
        "More",
        vec![MenuItem::action("Rename", || {})],
    ));
    let paint = |menu: &mut ContextMenu, text: &mut TextRenderer| {
        let mut top = Scene::new();
        menu.draw_overlay(&mut top, Affine::IDENTITY, text, &theme, &presence);
        top
    };
    with_monotonic_time(t0, || {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        menu.open_at([40.0, 40.0]);
        let _ = paint(&mut menu, &mut text);
        assert!(menu.needs_animation_frame(), "it arrives");
        // The pointer it opened at is the origin: the menu leans that way,
        // up and to the left of its centre, by its whole reach.
        let corner = Point::new(40.0, 40.0);
        let drawn = menu.drawn.to_drawn(corner);
        assert!(
            ((drawn - corner).hypot() - crate::motion::Appear::REACH).abs() < 1e-6,
            "the corner comes toward the pointer by its reach: {drawn:?}"
        );
        assert!(drawn.x < corner.x && drawn.y < corner.y, "{drawn:?}");
    });
    with_monotonic_time(at(400), || {
        let _ = paint(&mut menu, &mut text);
        assert!(!menu.needs_animation_frame(), "it rests");
        assert!(menu.drawn.is_rest());
        menu.open_submenu(1, false);
        let _ = paint(&mut menu, &mut text);
        assert!(
            !menu.needs_animation_frame(),
            "a nested menu does not animate"
        );
        menu.close();
        assert!(
            !menu.contains([50.0, 50.0]),
            "a closing menu takes no input"
        );
        let leaving = paint(&mut menu, &mut text);
        assert!(
            !leaving.encoding().is_empty(),
            "it is drawn while it leaves"
        );
        assert!(!menu.painted_bounds(&theme).is_empty());
    });
    with_monotonic_time(at(800), || {
        let gone = paint(&mut menu, &mut text);
        assert!(gone.encoding().is_empty(), "and then it has gone");
        assert!(menu.painted_bounds(&theme).is_empty());
        assert!(!menu.needs_animation_frame());
    });
}

/// Opened from a control, the menu hangs `s1` under it with its
/// leading edge on the control's; at the window's trailing edge it lines
/// up with the control's trailing edge, and near the bottom it opens
/// above. A point-placed menu flips inside the window it is presented
/// in.
#[test]
fn a_menu_opens_at_its_trigger_and_flips_at_the_window_edges() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let window = Rect::new(0.0, 0.0, 1280.0, 800.0);
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let menu_with = || {
        let mut menu = ContextMenu::new();
        for label in ["New child note", "Rename", "Duplicate", "Delete"] {
            menu.push(MenuItem::action(label, || {}));
        }
        menu
    };

    let button = Rect::new(300.0, 16.0, 332.0, 48.0);
    let mut menu = menu_with();
    menu.open_anchored(button, window);
    menu.layout_within(&mut text, &theme);
    let rect = menu.get_rect();
    assert!((rect.x0 - button.x0).abs() < 1e-9, "{rect:?}");
    assert!(
        (rect.y0 - (button.y1 + ANCHOR_GAP)).abs() < 1e-9,
        "{rect:?}"
    );

    let more = Rect::new(1236.0, 16.0, 1268.0, 48.0);
    let mut menu = menu_with();
    menu.open_anchored(more, window);
    menu.layout_within(&mut text, &theme);
    let rect = menu.get_rect();
    assert!(
        (rect.x1 - more.x1).abs() < 1e-9,
        "trailing-aligned: {rect:?}"
    );
    assert!(rect.y0 >= more.y1, "under its trigger: {rect:?}");

    let low = Rect::new(600.0, 760.0, 632.0, 792.0);
    let mut menu = menu_with();
    menu.open_anchored(low, window);
    menu.layout_within(&mut text, &theme);
    let rect = menu.get_rect();
    assert!(rect.y1 <= low.y0, "flipped above: {rect:?}");

    let mut menu = menu_with();
    menu.open_at([1270.0, 790.0]);
    let presence = PresenceCtx::default();
    presence.viewport.set(Some(window));
    let mut scene = Scene::new();
    menu.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
    let rect = menu.get_rect();
    assert!(
        rect.x0 >= window.x0
            && rect.x1 <= window.x1
            && rect.y0 >= window.y0
            && rect.y1 <= window.y1,
        "a menu at the bottom-trailing corner stays in the window: {rect:?}"
    );
}

/// A menu assembled from optional sections paints no leading, trailing
/// or doubled separator, and its rows are the menu row height.
#[test]
fn separators_only_stand_between_actions() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut menu = ContextMenu::new();
    for item in [
        MenuItem::separator(),
        MenuItem::action("Copy", || {}),
        MenuItem::separator(),
        MenuItem::separator(),
        MenuItem::action("Paste", || {}),
        MenuItem::separator(),
    ] {
        menu.push(item);
    }
    let shown: Vec<bool> = (0..menu.items.len())
        .map(|index| item_shown(&menu.items, index))
        .collect();
    assert_eq!(shown, [false, true, true, false, true, false]);
    assert_eq!(
        menu.content_height(),
        2.0 * ROW_HEIGHT + SEPARATOR_HEIGHT,
        "hidden separators take no room"
    );
    let theme = Theme::dark();
    menu.open_anchored(
        Rect::new(10.0, 10.0, 10.0, 10.0),
        Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    menu.layout_within(&mut TextRenderer::new(), &theme);
    assert_eq!(
        menu.row_height,
        f64::from(theme.tokens.metrics.menu_row_height)
    );
}

#[test]
fn a_state_row_carries_its_checked_state_and_an_anchor_row_clears_on_close() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let item = MenuItem::action("Copy on select", || {}).with_checked(true);
    assert_eq!(item.checked(), Some(true));
    let mut menu = ContextMenu::new();
    menu.push(item);
    menu.anchor_row(Rect::new(0.0, 40.0, 300.0, 80.0));
    menu.open_at([20.0, 60.0]);
    assert!(menu.anchor_row.is_some());
    menu.close();
    assert!(menu.anchor_row.is_none());
}
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use winit::event::TouchPhase;

/// A shared counter the rows' callbacks bump.
fn open_menu() -> (ContextMenu, Arc<AtomicU32>) {
    let fired = Arc::new(AtomicU32::new(0));
    let f1 = fired.clone();
    let f2 = fired.clone();
    let mut m = ContextMenu::new();
    m.push(MenuItem::action("Cut", move || {
        f1.fetch_add(1, Ordering::SeqCst);
    }));
    m.push(MenuItem::separator());
    m.push(MenuItem::action("Copy", move || {
        f2.fetch_add(100, Ordering::SeqCst);
    }));
    m.open_at([100.0, 100.0]);
    lay_out(&mut m);
    (m, fired)
}

fn lay_out(m: &mut ContextMenu) {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = m.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(800.0),
                height: taffy::AvailableSpace::Definite(600.0),
            },
        )
        .unwrap();
    m.apply_layout(&taffy, node, Point::ZERO);
}

fn cursor_at(p: Point) -> WindowEvent {
    crate::synthetic_input::cursor_moved(p.x, p.y)
}
fn touch(phase: TouchPhase, id: u64, point: Point) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(point.x, point.y),
        force: None,
        id,
    })
}
fn wheel_lines(y: f32) -> WindowEvent {
    crate::synthetic_input::mouse_wheel(0.0, y)
}

#[test]
fn pointer_activation_fires_actions_and_dismisses() {
    fn click_on_row_fires_action_and_records_selection() {
        let (mut m, fired) = open_menu();
        // First action row centre: y0 + PADDING + ROW_HEIGHT/2.
        let r = m.get_rect();
        let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
        let p = Point::new(r.x0 + 20.0, row0_y);
        dispatch(&mut m, &cursor_at(p));
        let handled = dispatch(&mut m, &mouse_press());
        assert!(handled, "a row click is consumed");
        // The press lights the row and the menu stays up until the button
        // comes up on it: a click is a press and the release that ends it.
        assert_eq!(fired.load(Ordering::SeqCst), 0, "the press runs nothing");
        assert!(m.is_open(), "the menu waits for the release");
        assert!(dispatch(&mut m, &mouse_release()));
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the first action's callback fired"
        );
        assert_eq!(m.take_selection(), Some(0), "selection index is exposed");
        assert!(!m.is_open(), "the menu closes after a selection");
        assert_eq!(m.take_selection(), None, "selection drains exactly once");
    }

    fn click_outside_dismisses_and_is_swallowed() {
        let (mut m, fired) = open_menu();
        let r = m.get_rect();
        let p = Point::new(r.x1 + 50.0, r.y1 + 50.0);
        dispatch(&mut m, &cursor_at(p));
        let handled = dispatch(&mut m, &mouse_press());
        assert!(handled, "outside click is swallowed (not passed through)");
        assert!(!m.is_open(), "outside click dismisses the menu");
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "no action fires on an outside click"
        );
    }

    fn esc_dismisses() {
        let (mut m, _f) = open_menu();
        let handled = dispatch(&mut m, &named_key(NamedKey::Escape));
        assert!(handled);
        assert!(!m.is_open(), "Esc closes the menu");
    }

    fn hover_tracks_cursor_row() {
        let (mut m, _f) = open_menu();
        let r = m.get_rect();
        let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
        dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, row0_y)));
        assert_eq!(m.hovered(), Some(0), "cursor over row 0 highlights it");
        // Move onto the separator → no hover.
        let sep_y = r.y0 + PADDING + ROW_HEIGHT + SEPARATOR_HEIGHT / 2.0;
        dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, sep_y)));
        assert_eq!(m.hovered(), None, "cursor over a separator clears hover");
    }

    fn touch_release_activates_once_and_keeps_the_capture() {
        let (mut menu, fired) = open_menu();
        let rect = menu.get_rect();
        let point = Point::new(rect.x0 + 20.0, rect.y0 + PADDING + ROW_HEIGHT / 2.0);

        assert!(dispatch(&mut menu, &touch(TouchPhase::Started, 7, point)));
        assert!(dispatch(&mut menu, &touch(TouchPhase::Ended, 7, point)));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(menu.take_selection(), Some(0));
        assert!(!menu.is_open());
    }

    fn modal_capture_when_open() {
        let (m, _f) = open_menu();
        assert!(Widget::traps_focus(&m), "open menu traps keyboard focus");
        assert!(
            Widget::modal_capture(&m).is_some(),
            "open menu reports capture"
        );
        let closed = ContextMenu::new();
        assert!(!Widget::traps_focus(&closed));
        assert!(Widget::modal_capture(&closed).is_none());
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    click_on_row_fires_action_and_records_selection();
    click_outside_dismisses_and_is_swallowed();
    esc_dismisses();
    hover_tracks_cursor_row();
    touch_release_activates_once_and_keeps_the_capture();
    modal_capture_when_open();
}

/// Every widget that opens a menu at the pointer shares this: the
/// host's replay of the still pointer after a rebuild is not a hover,
/// so the keyboard keeps its row; the pointer's first real move takes
/// hover from there.
#[test]
fn a_still_pointer_replayed_where_the_menu_opened_keeps_the_keyboard_row() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let (mut m, fired) = open_menu();
    dispatch(&mut m, &named_key(NamedKey::ArrowDown));
    dispatch(&mut m, &named_key(NamedKey::ArrowDown));
    assert_eq!(m.hovered(), Some(2));
    dispatch(&mut m, &cursor_at(Point::new(100.0, 100.0)));
    assert_eq!(
        m.hovered(),
        Some(2),
        "the replayed pointer where the menu opened leaves the keyboard row"
    );
    let first = m.item_rect(0).expect("first action row").center();
    dispatch(&mut m, &cursor_at(first));
    assert_eq!(
        m.hovered(),
        Some(0),
        "hover follows the pointer once it moves"
    );
    dispatch(&mut m, &named_key(NamedKey::Enter));
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    assert_eq!(m.take_selection(), Some(0));
}

#[test]
fn disabled_and_destructive_rows_are_not_implicit_targets() {
    fn keyboard_nav_skips_separator_and_enter_selects() {
        let (mut m, fired) = open_menu();
        // Down → first action (idx 0).
        dispatch(&mut m, &named_key(NamedKey::ArrowDown));
        assert_eq!(m.hovered(), Some(0));
        // Down again → must skip the separator (idx 1) and land on idx 2.
        dispatch(&mut m, &named_key(NamedKey::ArrowDown));
        assert_eq!(m.hovered(), Some(2), "ArrowDown skips the separator");
        dispatch(&mut m, &named_key(NamedKey::Enter));
        assert_eq!(
            fired.load(Ordering::SeqCst),
            100,
            "Enter fired the second action"
        );
        assert_eq!(m.take_selection(), Some(2));
    }

    fn disabled_rows_reject_pointer_activation() {
        let (mut menu, fired) = open_menu();
        menu.set_item_enabled(0, false);
        let rect = menu.item_rect(0).expect("first action row");
        let point = rect.center();

        dispatch(&mut menu, &cursor_at(point));
        assert_eq!(menu.hovered(), None, "disabled rows are not hover targets");
        assert!(dispatch(&mut menu, &mouse_press()));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert_eq!(menu.take_selection(), None);
        assert!(menu.is_open(), "a rejected activation keeps the menu open");
    }

    fn keyboard_navigation_skips_disabled_rows() {
        let (mut menu, fired) = open_menu();
        menu.set_item_enabled(0, false);

        dispatch(&mut menu, &named_key(NamedKey::ArrowDown));
        assert_eq!(menu.hovered(), Some(2));
        dispatch(&mut menu, &named_key(NamedKey::Enter));

        assert_eq!(fired.load(Ordering::SeqCst), 100);
        assert_eq!(menu.take_selection(), Some(2));
    }

    fn destructive_first_row_is_not_the_implicit_keyboard_target() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::destructive_selection("Delete"));
        menu.push(MenuItem::selection("Archive"));
        menu.open_at([10.0, 20.0]);

        assert_eq!(menu.hovered(), Some(1));
    }

    fn item_builders_preserve_icon_and_destructive_intent() {
        let item = MenuItem::destructive_selection("Delete").with_icon(Icon::Trash);
        let MenuItem::Action {
            icon, destructive, ..
        } = item
        else {
            panic!("selection builder must produce an action");
        };

        assert_eq!(icon, Some(Icon::Trash));
        assert!(destructive);
    }

    fn semantic_activation_uses_the_same_selection_channel() {
        let (mut menu, fired) = open_menu();
        let mut doc = AutoCommit::new();

        assert!(Widget::perform_semantic_action(
            &mut menu,
            "context-menu:item:2",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(fired.load(Ordering::SeqCst), 100);
        assert_eq!(menu.take_selection(), Some(2));
        assert!(!menu.is_open());
    }

    fn semantic_activation_rejects_disabled_rows() {
        let (mut menu, fired) = open_menu();
        menu.set_item_enabled(2, false);
        let mut doc = AutoCommit::new();

        assert!(!Widget::perform_semantic_action(
            &mut menu,
            "context-menu:item:2",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert_eq!(menu.take_selection(), None);
        assert!(menu.is_open());
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    keyboard_nav_skips_separator_and_enter_selects();
    disabled_rows_reject_pointer_activation();
    keyboard_navigation_skips_disabled_rows();
    destructive_first_row_is_not_the_implicit_keyboard_target();
    item_builders_preserve_icon_and_destructive_intent();
    semantic_activation_uses_the_same_selection_channel();
    semantic_activation_rejects_disabled_rows();
}

#[test]
fn bounded_menu_scrolls_and_keeps_targets_reachable() {
    fn bounded_wheel_hit_testing_and_keyboard_activation_share_scroll_offset() {
        let mut menu = ContextMenu::new();
        for index in 0..20 {
            menu.push(MenuItem::action(format!("Command {index}"), || {}));
        }
        menu.open_anchored(
            Rect::new(10.0, 10.0, 30.0, 30.0),
            Rect::new(0.0, 0.0, 180.0, 100.0),
        );
        menu.layout_within(&mut TextRenderer::new(), &Theme::dark());
        let point = menu.get_rect().center();
        dispatch(&mut menu, &cursor_at(point));
        let wheel = |y| crate::synthetic_input::mouse_wheel(0.0, y);
        assert!(dispatch(&mut menu, &wheel(-100.0)));
        assert_eq!(menu.scroll_offset(), menu.list.max_scroll_offset());
        let last = menu.item_rect(19).unwrap().center();
        assert_eq!(menu.item_at([last.x, last.y]), Some(19));
        dispatch(&mut menu, &cursor_at(last));
        dispatch(&mut menu, &mouse_press());
        dispatch(&mut menu, &mouse_release());
        assert_eq!(menu.take_selection(), Some(19));
        assert_eq!(menu.take_selection(), None);
        menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 180.0, 100.0));
        menu.focus_first_action();
        for _ in 0..19 {
            dispatch(&mut menu, &named_key(NamedKey::ArrowDown));
        }
        assert_eq!(menu.hovered(), Some(19));
        assert!(menu.item_rect(19).is_some());
        dispatch(&mut menu, &named_key(NamedKey::Enter));
        assert_eq!(menu.take_selection(), Some(19));
        menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 180.0, 100.0));
        dispatch(&mut menu, &wheel(100.0));
        assert_eq!(menu.scroll_offset(), 0.0);
    }

    fn viewport_bounds_menu_and_keeps_keyboard_target_reachable() {
        let mut menu = ContextMenu::new();
        for label in ["First", "Second", "Third", "Fourth"] {
            menu.push(MenuItem::action(label, || {}));
        }
        menu.set_text_scale(2.0);
        // Two rows fit in this viewport. A bottom-right invocation flips the
        // popup upward, so the remaining rows stay reachable via scrolling.
        let viewport = Rect::new(
            0.0,
            0.0,
            menu.menu_width(),
            menu.padding() * 2.0 + menu.row_height() * 2.0,
        );
        menu.set_viewport(Some(viewport));
        menu.open_at([390.0, 160.0]);
        assert!(
            menu.list.overflows(),
            "clipped rows expose a scroll affordance"
        );
        assert_eq!(menu.get_rect().x1, viewport.x1);
        assert_eq!(menu.get_rect().y0, viewport.y0);
        assert_eq!(menu.get_rect().y1, viewport.y1);
        for _ in 0..4 {
            menu.move_hover(1);
        }
        assert_eq!(menu.hovered(), Some(3));
        let last = menu.item_rect(3).expect("last action is laid out");
        assert!(last.y0 >= menu.rect.y0 + menu.padding());
        assert!(last.y1 <= menu.rect.y1 - menu.padding());
    }

    fn wheel_scrolls_rows_inside_a_bounded_menu() {
        let mut menu = ContextMenu::new();
        for label in ["First", "Second", "Third", "Fourth"] {
            menu.push(MenuItem::action(label, || {}));
        }
        let viewport = Rect::new(0.0, 0.0, menu.menu_width(), 68.0);
        menu.set_viewport(Some(viewport));
        menu.open_at([0.0, 0.0]);
        let pointer = Point::new(
            16.0,
            menu.rect.y0 + menu.padding() + menu.row_height() / 2.0,
        );
        dispatch(&mut menu, &cursor_at(pointer));
        assert!(dispatch(&mut menu, &wheel_lines(-2.0)));
        assert_eq!(menu.item_at([pointer.x, pointer.y]), Some(2));
    }

    fn text_scale_updates_layout_and_hit_testing_together() {
        let (mut m, _fired) = open_menu();
        let before = m.get_rect();
        m.set_text_scale(2.0);
        let after = m.get_rect();
        assert_eq!(after.width(), before.width() * 2.0);
        assert_eq!(
            after.height(),
            PADDING * 2.0 + ROW_HEIGHT * 4.0 + SEPARATOR_HEIGHT
        );

        let second_action = m.item_rect(2).expect("second action row");
        let p = Point::new(
            f64::midpoint(second_action.x0, second_action.x1),
            second_action.y0 + 1.0,
        );
        assert_eq!(m.item_at([p.x, p.y]), Some(2));
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    bounded_wheel_hit_testing_and_keyboard_activation_share_scroll_offset();
    viewport_bounds_menu_and_keeps_keyboard_target_reachable();
    wheel_scrolls_rows_inside_a_bounded_menu();
    text_scale_updates_layout_and_hit_testing_together();
}

#[test]
fn legacy_open_at_geometry_scales_with_text() {
    /// At 1× a point-placed menu paints exactly where `open_at` put it, at
    /// the reference width and row height: measuring the labels changes
    /// nothing a reference-scale menu already had.
    fn legacy_open_at_geometry_is_unchanged_at_1x_after_paint() {
        let (mut m, _f) = open_menu();
        let before = m.get_rect();
        let mut scene = Scene::new();
        m.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert_eq!(m.get_rect(), before);
        assert_eq!(m.row_height, ROW_HEIGHT);
    }

    /// A point-placed (`open_at`) menu keeps its origin and never shrinks
    /// below the reference width, but its rows follow the text. At a 200 %
    /// text setting the labels used to be painted at 4× — the theme's
    /// already-scaled size sent through the renderer's own multiplier —
    /// into fixed 28 px rows (file-explorer `context-menu` at
    /// `--text-scale 2`); now every glyph line lands inside its own row.
    fn legacy_open_at_rows_grow_with_the_text_scale_and_glyphs_stay_in_their_rows() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let mut m = ContextMenu::new();
        m.push(MenuItem::action("Neuer Ordner", || {}));
        m.push(MenuItem::separator());
        m.push(MenuItem::action("Verborgene Dateien anzeigen", || {}));
        m.push(MenuItem::action("Nach Name sortieren", || {}));
        m.open_at([100.0, 100.0]);
        let before = m.get_rect();
        let mut scene = Scene::new();
        m.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
        let after = m.get_rect();
        assert_eq!((after.x0, after.y0), (before.x0, before.y0), "origin kept");
        assert!(
            after.width() >= MENU_WIDTH,
            "never narrower than the reference"
        );
        assert!(
            m.row_height > ROW_HEIGHT,
            "rows grew to the text: {}",
            m.row_height
        );

        let rows: Vec<Rect> = [0, 2, 3].iter().filter_map(|&i| m.item_rect(i)).collect();
        assert_eq!(rows.len(), 3, "every action row is painted");
        let runs = &scene.encoding().resources.glyph_runs;
        assert!(!runs.is_empty(), "labels were painted");
        // Measure the painted size with an unscaled renderer: `font_size`
        // is the final glyph size, and it must be the theme's 2× — not 4×.
        let mut probe = TextRenderer::new();
        for run in runs {
            assert!(
                (run.font_size - theme.tokens.typography.size(TextRole::Body)).abs() < 0.01,
                "glyphs painted at {} px; the theme's scaled size is {}",
                run.font_size,
                theme.tokens.typography.size(TextRole::Body),
            );
            let top = f64::from(run.transform.translation[1]);
            let (_, h) = probe.measure("Ag", run.font_size, None);
            assert!(
                rows.iter()
                    .any(|r| top >= r.y0 - 0.01 && top + f64::from(h) <= r.y1 + 0.01),
                "a glyph line at y={top} (h={h}) escapes every row: {rows:?}",
            );
        }
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    legacy_open_at_geometry_is_unchanged_at_1x_after_paint();
    legacy_open_at_rows_grow_with_the_text_scale_and_glyphs_stay_in_their_rows();
}

/// A menu floats in the window's top layer and paints nothing in place,
/// so a menu nothing touches must not keep the window drawing: it asks
/// for a frame when what it paints changes, and not once it has settled.
#[test]
fn a_settled_menu_asks_for_no_frame() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut menu = ContextMenu::with_items(vec![
        MenuItem::action("Open", || {}),
        MenuItem::action("Close", || {}),
    ]);
    assert!(
        !menu.needs_redraw(),
        "a menu nobody opened paints nothing, whatever it paints once it opens"
    );
    menu.open_at([40.0, 60.0]);
    assert!(menu.needs_redraw(), "it was opened");
    menu.mark_drawn();
    assert!(!menu.needs_redraw(), "nothing changed since it was painted");
    menu.focus_item(1);
    assert!(menu.needs_redraw(), "the keyboard moved to another row");
    menu.mark_drawn();
    assert!(!menu.needs_redraw());
    menu.set_items(vec![MenuItem::action("Rename", || {})]);
    assert!(menu.needs_redraw(), "its rows were replaced");
    menu.mark_drawn();
    assert!(!menu.needs_redraw());
    menu.close();
    assert!(
        !menu.needs_redraw(),
        "motion reads at its end: the menu it closed paints nothing"
    );
}

/// A menu is as wide as its longest label and its longest trailing text
/// together. A row that carries both used to pay for its trailing text twice —
/// once against the label it sits beside and once more in the column every row
/// shares — so one sentence beside a disabled row grew the panel across half
/// the strip.
#[test]
fn a_menu_is_as_wide_as_its_longest_label_and_longest_trailing_text_together() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let label = "Aurora Skies, the whole discography";
    let detail = "No other workspace to move this window to";
    let mut both =
        ContextMenu::with_items(vec![MenuItem::action(label, || {}).with_detail(detail)]);
    let mut label_only = ContextMenu::with_items(vec![MenuItem::action(label, || {})]);
    let mut detail_only =
        ContextMenu::with_items(vec![MenuItem::action("Go", || {}).with_detail(detail)]);
    for menu in [&mut both, &mut label_only, &mut detail_only] {
        menu.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), viewport);
        menu.layout_within(&mut text, &theme);
    }
    let (label_alone, detail_alone) = (label_only.measured_width, detail_only.measured_width);
    assert!(
        both.measured_width < label_alone + detail_alone,
        "the row with a sentence beside it is {} px wide, more than its label's menu \
         ({label_alone} px) and its sentence's menu ({detail_alone} px) together",
        both.measured_width
    );
    assert!(both.measured_width >= label_alone.max(detail_alone));
}

/// A row near the window's trailing edge opens its menu where the pointer
/// is, and the box its send row opens stands in the window the menu stood in —
/// not past its edge, where a person cannot reach it.
#[test]
fn a_menu_near_the_trailing_edge_opens_its_send_to_box_in_the_window() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    crate::shared_selection::set_send_host_available(true);
    let window = Rect::new(0.0, 0.0, 604.0, 708.0);
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut menu = ContextMenu::with_items(vec![
        MenuItem::action("Open issue", || {}),
        MenuItem::action("Complete", || {}),
    ]);
    menu.set_about(Some(
        crate::shared_selection::SharedSelection::of_items([(
            "tasks.tracker/issues/one",
            "Ship the launch notes",
        )])
        .in_kind("tasks.tracker"),
    ));
    // Right-click on a row whose right end is well inside the window.
    note_input(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Right,
    ));
    menu.open_at([560.0, 178.0]);
    menu.set_viewport(Some(window));
    assert!(
        !menu.send_to().is_open(),
        "the menu is one surface: the box opens when its send row is chosen"
    );
    // The row that sends what the menu is about, which the menu adds itself.
    let send_at = menu.items().len() - 1;
    menu.click_item(send_at);
    assert!(menu.is_open(), "the menu stays beside its box");
    let presence = PresenceCtx::default();
    presence.viewport.set(Some(window));
    let mut scene = Scene::new();
    menu.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
    let box_rect = menu.send_to().rect();
    assert!(menu.send_to().is_open(), "the send row opened the box");
    assert!(
        box_rect.x0 >= window.x0
            && box_rect.x1 <= window.x1
            && box_rect.y0 >= window.y0
            && box_rect.y1 <= window.y1,
        "the box {box_rect:?} stands past the window {window:?} where the menu stood at 560"
    );
    // What a screen reader reads is what is drawn: the box and each of its
    // parts where the person sees them, so the session's own targets reach
    // them too.
    let node = Widget::accessibility(&menu).expect("the menu is read");
    let dialog = node
        .children
        .iter()
        .find(|child| child.role == crate::a11y::Role::Dialog)
        .expect("the box is read beside the menu");
    assert_eq!(
        dialog.bounds.map(crate::a11y::AccessBounds::to_rect),
        Some(box_rect),
        "the box is read where it stands"
    );
    // Each part of the box — who it goes to, the words, Send — is read where
    // it is drawn, inside the box the person sees, so the keyboard, a screen
    // reader and the session's own targets all reach the same control in the
    // same place.
    for target in ["send-to:recipients", "send-to:message", "send-to:send"] {
        let part = dialog
            .children
            .iter()
            .find(|child| child.action_target.as_deref() == Some(target))
            .unwrap_or_else(|| panic!("{target} is read"));
        let bounds = part
            .bounds
            .map(crate::a11y::AccessBounds::to_rect)
            .unwrap_or_else(|| panic!("{target} is read where it is drawn"));
        assert!(
            bounds.x0 >= box_rect.x0
                && bounds.x1 <= box_rect.x1
                && bounds.y0 >= box_rect.y0
                && bounds.y1 <= box_rect.y1
                && bounds.width() > 0.0
                && bounds.height() > 0.0,
            "{target} {bounds:?} is not drawn inside the box {box_rect:?}"
        );
    }
    crate::shared_selection::set_send_host_available(false);
}

/// A menu that takes typing shows its hint whole: the field is drawn as wide
/// as the hint plus its own inset on both sides, so a hint longer than every
/// row widens the panel rather than running out of the field mid-word.
#[test]
fn the_field_that_takes_typing_shows_its_hint_whole() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut menu = ContextMenu::new();
    menu.push(MenuItem::action("Kopieren", || {}));
    menu.set_owner_asks(true);
    menu.open_at([100.0, 100.0]);
    assert!(menu.can_ask(), "fixture: the menu can send what is typed");
    assert!(menu.query.is_some(), "fixture: the menu takes typing");
    menu.layout_within(&mut text, &theme);
    let hint = crate::i18n::I18nManager::lookup_plain("menu-query-placeholder");
    let (hint_width, _) = text.measure_style(&hint, None, &theme.text(TextRole::Body));
    let field = menu
        .query_rect()
        .expect("fixture: the field stands on the menu");
    let shown = field.width() - 2.0 * (label_inset(&theme) - PADDING);
    assert!(
        shown + 0.5 >= f64::from(hint_width),
        "the field shows {shown} px of the {hint_width} px hint {hint:?}"
    );
}

/// A menu opened a second time stands as it did the first, the host having told it how the person
/// works since (a mouse, then a key pressed): its caption and rules keep their own heights, never
/// a pressable row's each.
#[test]
fn a_menu_opened_again_keeps_its_rows_heights() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::light();
    let presence = PresenceCtx::default();
    let mut text = TextRenderer::new();
    let items = || {
        vec![
            MenuItem::caption("Hosted by example.org"),
            MenuItem::separator(),
            MenuItem::selection("Join a workspace"),
            MenuItem::separator(),
            MenuItem::selection("Rename workspace").with_icon(crate::icons::Icon::Pencil),
            MenuItem::selection("Overview"),
            MenuItem::selection("New strip").with_icon(crate::icons::Icon::Plus),
        ]
    };
    let mut menu = ContextMenu::new();
    let open = |menu: &mut ContextMenu, text: &mut TextRenderer| {
        menu.set_items(items());
        menu.open_anchored(
            Rect::new(50.0, 4.0, 160.0, 28.0),
            Rect::new(0.0, 0.0, 1280.0, 800.0),
        );
        menu.layout_within(text, &theme);
        let mut top = Scene::new();
        menu.draw_overlay(&mut top, Affine::IDENTITY, text, &theme, &presence);
        (0..menu.items().len())
            .filter_map(|index| menu.item_rect(index).map(|rect| (rect.y0, rect.height())))
            .collect::<Vec<_>>()
    };
    let first = open(&mut menu, &mut text);
    menu.close();
    Widget::set_interaction_environment(&mut menu, crate::InteractionEnvironment::POINTER);
    Widget::set_interaction_environment(
        &mut menu,
        crate::InteractionEnvironment {
            modality: crate::interaction::InputModality::Keyboard,
            ..crate::InteractionEnvironment::POINTER
        },
    );
    let mut top = Scene::new();
    menu.draw_overlay(&mut top, Affine::IDENTITY, &mut text, &theme, &presence);
    let second = open(&mut menu, &mut text);
    assert_eq!(
        first, second,
        "where the second opening's rows stand, and how tall"
    );
}
