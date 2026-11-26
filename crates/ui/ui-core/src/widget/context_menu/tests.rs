use super::*;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::synthetic_input::mouse_press;
use tilo_ui_base::synthetic_input::mouse_release;
use tilo_ui_base::synthetic_input::named_key;
use crate::testing::dispatch;
use automerge::AutoCommit;
use std::sync::Arc;

tilo_test_env::scenario! {
    /// What a menu paints and how wide, tall and where it is.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_menu_paints_sizes_and_places_itself;

    // Opened from a control, the menu hangs one anchor gap under it with its
    // leading edge on the control's; at the window's trailing edge it lines up
    // with the control's trailing edge, and near the bottom it opens above. A
    // point-placed menu flips inside the window it is presented in. Where it
    // lands is checked in `placement_tests`.
    phase an_open_menu_paints_only_in_the_top_layer;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Open", || {}));
        menu.push(MenuItem::action("Rename", || {}));
        menu.open_at([40.0, 40.0]);
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut in_flow = Scene::new();
        menu.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut in_flow, Affine::IDENTITY, &mut text, &theme, presence.draw_env()));
        assert!(
            in_flow.encoding().is_empty(),
            "the in-flow pass paints nothing"
        );
        let mut top = Scene::new();
        menu.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(&mut top, Affine::IDENTITY, &mut text, &theme, presence.draw_env()));
        assert!(!top.encoding().is_empty(), "the top layer paints the menu");
    }

    // A menu floats in the window's top layer and paints nothing in place,
    // so a menu nothing touches must not keep the window drawing: it asks
    // for a frame when what it paints changes, and not once it has settled.
    // A menu is as wide as its longest label and its longest trailing text
    // together. A row that carries both used to pay for its trailing text twice —
    // once against the label it sits beside and once more in the column every row
    // shares — so one sentence beside a disabled row grew the panel across half
    // the strip.
    phase a_menu_is_as_wide_as_its_longest_label_and_longest_trailing_text_together;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
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

    // A menu that takes typing shows its hint whole: the field is drawn as wide
    // as the hint plus its own inset on both sides, so a hint longer than every
    // row widens the panel rather than running out of the field mid-word.
    phase the_field_that_takes_typing_shows_its_hint_whole;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Kopieren", || {}));
        menu.set_owner_asks(true);
        menu.open_at([100.0, 100.0]);
        assert!(menu.can_ask(), "fixture: the menu can send what is typed");
        assert!(menu.query.is_some(), "fixture: the menu takes typing");
        menu.layout_within(&mut text, &theme);
        let hint = tilo_ui_base::i18n::I18nManager::lookup_plain("menu-query-placeholder");
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

    // A menu opened a second time stands as it did the first, the host having told it how the person
    // works since (a mouse, then a key pressed): its caption and rules keep their own heights, never
    // a pressable row's each.
    phase a_menu_opened_again_keeps_its_rows_heights;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::light();
        let presence = PresenceCtx::default();
        let mut text = TextRenderer::new();
        let items = || {
            vec![
                MenuItem::caption("Hosted by example.org"),
                MenuItem::separator(),
                MenuItem::selection("Join a workspace"),
                MenuItem::separator(),
                MenuItem::selection("Rename workspace").with_icon(tilo_ui_base::icons::Icon::Pencil),
                MenuItem::selection("Overview"),
                MenuItem::selection("New strip").with_icon(tilo_ui_base::icons::Icon::Plus),
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
            menu.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(&mut top, Affine::IDENTITY, text, &theme, presence.draw_env()));
            (0..menu.items().len())
                .filter_map(|index| menu.item_rect(index).map(|rect| (rect.y0, rect.height())))
                .collect::<Vec<_>>()
        };
        let first = open(&mut menu, &mut text);
        menu.close();
        Widget::set_interaction_environment(&mut menu, tilo_ui_base::InteractionEnvironment::POINTER);
        Widget::set_interaction_environment(
            &mut menu,
            tilo_ui_base::InteractionEnvironment {
                modality: tilo_ui_base::interaction::InputModality::Keyboard,
                ..tilo_ui_base::InteractionEnvironment::POINTER
            },
        );
        let mut top = Scene::new();
        menu.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(&mut top, Affine::IDENTITY, &mut text, &theme, presence.draw_env()));
        let second = open(&mut menu, &mut text);
        assert_eq!(
            first, second,
            "where the second opening's rows stand, and how tall"
        );
    }
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
        menu.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut top,
            Affine::IDENTITY,
            text,
            &theme,
            presence.draw_env(),
        ));
        top
    };
    with_monotonic_time(t0, || {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        menu.open_at([40.0, 40.0]);
        let _ = paint(&mut menu, &mut text);
        assert!(menu.needs_animation_frame(), "it arrives");
        // The pointer it opened at is the origin: the menu leans that way,
        // up and to the left of its centre, by its whole reach.
        let corner = Point::new(40.0, 40.0);
        let drawn = menu.drawn.to_drawn(corner);
        assert!(
            ((drawn - corner).hypot() - tilo_ui_base::motion::Appear::REACH).abs() < 1e-6,
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

tilo_test_env::scenario! {
    /// The rows of a menu: separators, state, activation and the rows that are no implicit target.
    /// Each phase was a test of its own and builds its own fixture.
    fn menu_rows_carry_state_and_run_by_pointer_and_keyboard;

    // A menu assembled from optional sections paints no leading, trailing
    // or doubled separator, and its rows are the menu row height.
    phase separators_only_stand_between_actions;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
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
            "hidden separators take up nothing"
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

    phase a_state_row_carries_its_checked_state_and_an_anchor_row_clears_on_close;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
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

    phase pointer_activation_fires_actions_and_dismisses;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
        // A press on a row lights it and the menu stays up until the button
        // comes up on it: a click is a press and the release that ends it.
        let (mut m, fired) = open_menu();
        let r = m.get_rect();
        let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
        let p = Point::new(r.x0 + 20.0, row0_y);
        dispatch(&mut m, &cursor_at(p));
        let handled = dispatch(&mut m, &mouse_press());
        assert!(handled, "a row click is consumed");
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
        // Hover follows the pointer row and leaves it over a separator.
        let (mut m, _f) = open_menu();
        let r = m.get_rect();
        let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
        dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, row0_y)));
        assert_eq!(m.hovered(), Some(0), "cursor over row 0 highlights it");
        let sep_y = r.y0 + PADDING + ROW_HEIGHT + SEPARATOR_HEIGHT / 2.0;
        dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, sep_y)));
        assert_eq!(m.hovered(), None, "cursor over a separator clears hover");
        // A finger down and up on a row activates it once.
        let (mut menu, fired) = open_menu();
        let rect = menu.get_rect();
        let point = Point::new(rect.x0 + 20.0, rect.y0 + PADDING + ROW_HEIGHT / 2.0);
        assert!(dispatch(&mut menu, &touch(TouchPhase::Started, 7, point)));
        assert!(dispatch(&mut menu, &touch(TouchPhase::Ended, 7, point)));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(menu.take_selection(), Some(0));
        assert!(!menu.is_open());
        // Anything outside the menu dismisses it and runs nothing: a click
        // there is swallowed, Escape closes it, and a window losing the
        // keyboard closes its menu.
        let (mut m, fired) = open_menu();
        let r = m.get_rect();
        let p = Point::new(r.x1 + 50.0, r.y1 + 50.0);
        dispatch(&mut m, &cursor_at(p));
        assert!(
            dispatch(&mut m, &mouse_press()),
            "outside click is swallowed (not passed through)"
        );
        assert!(!m.is_open(), "outside click dismisses the menu");
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "no action fires on an outside click"
        );
        let (mut m, _f) = open_menu();
        assert!(dispatch(&mut m, &named_key(NamedKey::Escape)));
        assert!(!m.is_open(), "Esc closes the menu");
        let (mut m, fired) = open_menu();
        dispatch(&mut m, &WindowEvent::Focused(false));
        assert!(
            !m.is_open(),
            "the window losing the keyboard closes its menu"
        );
        assert_eq!(fired.load(Ordering::SeqCst), 0, "no action fires");
        // An open menu is a modal that takes the keyboard; a closed one is not.
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

    // Every widget that opens a menu at the pointer shares this: the
    // host's replay of the still pointer after a rebuild is not a hover,
    // so the keyboard keeps its row; the pointer's first real move takes
    // hover from there.
    phase a_still_pointer_replayed_where_the_menu_opened_keeps_the_keyboard_row;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
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

    phase disabled_and_destructive_rows_are_not_implicit_targets;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
        // The keyboard walks the actions a person may run: down from the first
        // action skips the separator, and Enter runs the row it landed on.
        let (mut m, fired) = open_menu();
        dispatch(&mut m, &named_key(NamedKey::ArrowDown));
        assert_eq!(m.hovered(), Some(0));
        dispatch(&mut m, &named_key(NamedKey::ArrowDown));
        assert_eq!(m.hovered(), Some(2), "ArrowDown skips the separator");
        dispatch(&mut m, &named_key(NamedKey::Enter));
        assert_eq!(
            fired.load(Ordering::SeqCst),
            100,
            "Enter fired the second action"
        );
        assert_eq!(m.take_selection(), Some(2));
        // A disabled row takes neither the pointer nor the keyboard.
        let (mut menu, fired) = open_menu();
        menu.set_item_enabled(0, false);
        let point = menu.item_rect(0).expect("first action row").center();
        dispatch(&mut menu, &cursor_at(point));
        assert_eq!(menu.hovered(), None, "disabled rows are not hover targets");
        assert!(dispatch(&mut menu, &mouse_press()));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert_eq!(menu.take_selection(), None);
        assert!(menu.is_open(), "a rejected activation keeps the menu open");
        let (mut menu, fired) = open_menu();
        menu.set_item_enabled(0, false);
        dispatch(&mut menu, &named_key(NamedKey::ArrowDown));
        assert_eq!(menu.hovered(), Some(2));
        dispatch(&mut menu, &named_key(NamedKey::Enter));
        assert_eq!(fired.load(Ordering::SeqCst), 100);
        assert_eq!(menu.take_selection(), Some(2));
        // A destructive first row never takes the keyboard implicit target.
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::destructive_selection("Delete").with_icon(Icon::Trash));
        menu.push(MenuItem::selection("Archive"));
        menu.open_at([10.0, 20.0]);
        assert_eq!(menu.hovered(), Some(1));
        // Assistive technology activates through the same selection channel,
        // and a disabled row refuses it just as the pointer is refused.
        let (mut menu, fired) = open_menu();
        let mut doc = AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut menu,
            "context-menu:item:2",
            &tilo_ui_base::a11y::Action::Activate,
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(fired.load(Ordering::SeqCst), 100);
        assert_eq!(menu.take_selection(), Some(2));
        assert!(!menu.is_open());
        let (mut menu, fired) = open_menu();
        menu.set_item_enabled(2, false);
        let mut doc = AutoCommit::new();
        assert!(!Widget::perform_semantic_action(
            &mut menu,
            "context-menu:item:2",
            &tilo_ui_base::a11y::Action::Activate,
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert_eq!(menu.take_selection(), None);
        assert!(menu.is_open());
    }
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
    tilo_ui_base::synthetic_input::cursor_moved(p.x, p.y)
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

/// A row near the window's trailing edge opens its menu where the pointer
/// is, and the box its send row opens stands in the window the menu stood in —
/// not past its edge, where a person cannot reach it.
#[test]
fn a_menu_near_the_trailing_edge_opens_its_send_to_box_in_the_window() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    crate::shared_selection::set_send_host_available(true);
    let window = Rect::new(0.0, 0.0, 604.0, 708.0);
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut menu = ContextMenu::with_items(vec![
        MenuItem::action("Open issue", || {}),
        MenuItem::action("Complete", || {}),
    ]);
    menu.set_about(Some(
        tilo_ui_base::shared_subject::SharedSelection::of_items([(
            "tasks.tracker/issues/one",
            "Ship the launch notes",
        )])
        .in_kind("tasks.tracker"),
    ));
    // Right-click on a row whose right end is well inside the window.
    note_input(&tilo_ui_base::synthetic_input::mouse_input(
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
    assert!(
        !menu.open && menu.is_open(),
        "the box took the menu's place"
    );
    let presence = PresenceCtx::default();
    presence.viewport.set(Some(window));
    let mut scene = Scene::new();
    menu.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        presence.draw_env(),
    ));
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
        .find(|child| child.role == tilo_ui_base::a11y::Role::Dialog)
        .expect("the box is read");
    assert_eq!(
        dialog.bounds.map(tilo_ui_base::a11y::AccessBounds::to_rect),
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
            .map(tilo_ui_base::a11y::AccessBounds::to_rect)
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

/// The person's own menu on a bar is their avatar, whole (`canvas.md` §7, "The canvas is the whole
/// screen"): one face is drawn as the avatar itself at an avatar's size, with the menu behind it,
/// where several stay a group of faces.
#[test]
fn one_persons_menu_is_their_whole_avatar() {
    use tilo_ui_base::{component::Described, component::Person};
    let own = MenuButton::from_props(
        "me",
        MenuButtonProps {
            label: "Sam Rivera: style and workspaces".into(),
            faces: vec![Person::named("Sam Rivera")],
            ..MenuButtonProps::default()
        },
    );
    assert_eq!(
        own.button.people_shown(),
        (Some("Sam Rivera"), false),
        "their avatar, not a group of faces"
    );
    let group = MenuButton::from_props(
        "people",
        MenuButtonProps {
            label: "Who is here".into(),
            faces: vec![Person::named("Sam Rivera"), Person::named("Ana Ruiz")],
            ..MenuButtonProps::default()
        },
    );
    assert_eq!(
        group.button.people_shown(),
        (None, true),
        "several people are a group"
    );
}
