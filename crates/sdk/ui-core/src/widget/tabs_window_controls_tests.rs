//! The window's own controls at the trailing end of its bar, and the app's
//! own actions that stand beside them: what belongs to the window, and what
//! belongs to what the window shows.

use super::tests::{
    bar_with_laid_out_rects, browser_slots, built_layout, holder, lay_out_at_width_with, move_to,
};
use super::*;
use crate::testing::{Dispatch, dispatch};
use crate::widget::window_controls::{LEAD_GAP, WindowControl};
use winit::event::{ElementState, MouseButton};

/// A window someone else holds, or its person only watches, offers no
/// close among its controls, to the pointer or to assistive technology,
/// and keeps the rest.
#[test]
fn a_held_window_offers_no_close() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
    b.set_window_controls(true);
    b.set_holder(
        1,
        Some(TabHolder {
            name: "Lounge".into(),
            rgb: (1, 2, 3),
        }),
    );
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let controls = *b.window_controls();
    assert!(controls.close.area() <= 0.0);
    assert!(controls.maximize.area() > 0.0);
    let node = Widget::accessibility(&b).expect("the bar is described");
    fn targets(node: &crate::a11y::AccessNode, into: &mut Vec<String>) {
        into.extend(node.action_target.clone());
        for child in &node.children {
            targets(child, into);
        }
    }
    let mut all = Vec::new();
    targets(&node, &mut all);
    assert!(all.iter().any(|t| t == WINDOW_MAXIMIZE_TARGET), "{all:?}");
    assert!(!all.iter().any(|t| t == WINDOW_CLOSE_TARGET), "{all:?}");
    assert!(!all.iter().any(|t| t == "tab-close:1"), "{all:?}");
    // Someone who opens nothing here is offered no new tab either.
    assert!(all.iter().any(|t| t == "tab-new"), "{all:?}");
    assert!(b.set_new_tab(false));
    let mut all = Vec::new();
    targets(&Widget::accessibility(&b).expect("described"), &mut all);
    assert!(!all.iter().any(|t| t == "tab-new"), "{all:?}");
    b.set_holder(1, None);
    lay_out_at_width_with(&mut b, 900.0, &theme);
    assert!(
        b.window_controls().close.area() > 0.0,
        "its own window closes"
    );
}

/// The window's own controls stand in one pill; the app's History is an
/// action of the app's own, on the window's ground before it — never a
/// control inside it, and never the same thing as one.
#[test]
fn the_apps_history_stands_on_the_windows_ground_before_the_pill() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
    b.set_window_controls(true);
    b.set_history_control(1, Some(HistoryControl::Closed));
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let controls = *b.window_controls();
    let history = b.history_action_rect();
    assert!(history.area() > 0.0, "the app's History stands");
    assert!(
        !controls.pill.contains(history.center()) && history.x1 <= controls.pill.x0,
        "and stands before the pill, on the window's ground: {history:?} and {:?}",
        controls.pill
    );
    assert_eq!(
        controls.hit(history.center()),
        None,
        "the History toggle is not one of the window's controls",
    );
    assert!(
        (controls.pill.x0 - history.x1 - LEAD_GAP).abs() < 0.5,
        "a group's gap parts the app's action from the window's controls",
    );
    // Without History the pill keeps its place and the app's slots take
    // the room back.
    b.set_history_control(1, None);
    lay_out_at_width_with(&mut b, 900.0, &theme);
    assert!(b.history_action_rect().area() <= 0.0);
    assert_eq!(b.window_controls().pill, controls.pill);
}

/// Hover and press follow what they are on: a window control inside the
/// pill, the app's History on its own, and the pill's own padding
/// neither — which a press still claims for the window, so nothing
/// under the glass receives it.
#[test]
fn the_pills_padding_is_the_windows_and_its_controls_hover_alone() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    let mut b = TabBar::new(
        "g1",
        vec![
            TabDescriptor::new(1, "Notes"),
            TabDescriptor::new(2, "Files"),
        ],
        1,
    );
    b.set_window_controls(true);
    b.set_history_control(1, Some(HistoryControl::Closed));
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let controls = *b.window_controls();
    let history = b.history_action_rect();

    let hover = |b: &mut TabBar, p: Point| {
        let (taffy, node) = built_layout(b);
        let event = crate::synthetic_input::cursor_moved(p.x, p.y);
        Dispatch::new().layout(&taffy, node).send(b, &event);
    };

    hover(&mut b, controls.close.center());
    assert_eq!(b.hover_control, Some(WindowControl::Close));
    assert!(!b.history_action_hover, "one point lights one thing");

    hover(&mut b, history.center());
    assert_eq!(b.hover_control, None);
    assert!(b.history_action_hover);

    let padding = Point::new(controls.pill.x0 + 1.0, controls.pill.center().y);
    hover(&mut b, padding);
    assert_eq!(b.hover_control, None);
    assert!(
        !b.history_action_hover,
        "the pill's padding belongs to no control"
    );

    let press = crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left);
    let (taffy, node) = built_layout(&mut b);
    assert!(
        Dispatch::new()
            .layout(&taffy, node)
            .send(&mut b, &press)
            .handled,
        "a press on the pill is the window's, never the app's underneath",
    );
    assert_eq!(b.pressed_control, None);
    assert!(!b.history_action_pressed);
}

/// `TILO_WINDOW_BAR_SHOTS=<dir>` shoots a bar that carries an action of
/// the app's besides the window's own controls: the app's History stands
/// on the window's ground and looks like the app, so the glass pill
/// around the window's controls reads as the window's.
#[test]
fn window_bar_app_action_shots() {
    let Ok(directory) = std::env::var("TILO_WINDOW_BAR_SHOTS") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    for (scheme, theme) in [
        ("dark", crate::Theme::dark()),
        ("light", crate::Theme::light()),
    ] {
        let theme = theme.with_reduce_motion(true);
        for at in [
            "rest",
            "hover-close",
            "hover-maximize",
            "hover-history",
            "press-close",
        ] {
            let mut b = TabBar::new(
                "shots",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                ],
                1,
            );
            b.set_window_controls(true);
            b.set_history_control(1, Some(HistoryControl::Closed));
            lay_out_at_width_with(&mut b, 900.0, &theme);
            let controls = *b.window_controls();
            let point = match at {
                "hover-close" | "press-close" => Some(controls.close.center()),
                "hover-maximize" => Some(controls.maximize.center()),
                "hover-history" => Some(b.history_action_rect().center()),
                _ => None,
            };
            if let Some(point) = point {
                let (taffy, node) = built_layout(&mut b);
                let event = crate::synthetic_input::cursor_moved(point.x, point.y);
                Dispatch::new().layout(&taffy, node).send(&mut b, &event);
                if at == "press-close" {
                    let (taffy, node) = built_layout(&mut b);
                    let press = crate::synthetic_input::mouse_input(
                        ElementState::Pressed,
                        MouseButton::Left,
                    );
                    Dispatch::new().layout(&taffy, node).send(&mut b, &press);
                }
            }
            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            b.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let mut renderer = crate::offscreen::OffscreenRenderer::new(900, 120);
            renderer.render_png(
                &scene,
                theme.tokens.colors.surfaces.content,
                directory.join(format!("window-bar-actions-{at}-{scheme}.png")),
            );
        }
    }
}

/// A pointer sent to the centre of a window control's accessibility
/// node lands on the control, not somewhere in the bar.
#[test]
fn each_window_control_reports_the_rect_it_is_drawn_in() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
    b.set_window_controls(true);
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let node = Widget::accessibility(&b).expect("the bar is described");
    fn find<'a>(
        node: &'a crate::a11y::AccessNode,
        target: &str,
    ) -> Option<&'a crate::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    let controls = b.window_controls();
    for (target, drawn) in [
        ("tab:1", b.tab_rects()[0]),
        (WINDOW_MAXIMIZE_TARGET, controls.maximize),
        (WINDOW_CLOSE_TARGET, controls.close),
    ] {
        let bounds = find(&node, target)
            .and_then(|control| control.bounds)
            .unwrap_or_else(|| panic!("{target} reports where it is"));
        assert!(drawn.area() > 0.0, "{target} is drawn");
        assert_eq!(bounds.to_rect(), drawn, "{target}");
    }
    // A pill's close chip, drawn or not, reports exactly what is drawn.
    let mut b = TabBar::new(
        "g2",
        vec![
            TabDescriptor::new(1, "Notes"),
            TabDescriptor::new(2, "Chat"),
        ],
        1,
    );
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let node = Widget::accessibility(&b).expect("the bar is described");
    for (index, id) in [(0, 1), (1, 2)] {
        let close = find(&node, &format!("tab-close:{id}")).expect("a pill can be closed");
        let drawn = b.close_rects[index];
        assert_eq!(
            close.bounds.map(crate::a11y::AccessBounds::to_rect),
            (drawn.area() > 0.0).then_some(drawn),
            "tab-close:{id}"
        );
    }
}

#[test]
fn a_middle_press_closes_the_tab_under_it() {
    let mut b = bar_with_laid_out_rects();
    move_to(&mut b, 95.0, 20.0);
    let press = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Middle,
    };
    assert!(dispatch(&mut b, &press), "the bar takes the middle press");
    assert_eq!(b.pending_close, Some(2), "closing pill B asks to close B");
    assert!(b.closing_tabs.iter().any(|c| c.desc.id == 2));
    assert_eq!(b.active_id, 1, "another tab closing leaves the active one");

    // A tab someone else holds is not this person's to close, by the ×
    // or by a middle press.
    let mut b = bar_with_laid_out_rects();
    b.set_holder(2, Some(holder()));
    move_to(&mut b, 95.0, 20.0);
    dispatch(&mut b, &press);
    assert_eq!(b.pending_close, None);
    assert!(b.closing_tabs.is_empty());

    // A pinned tab stays where it was put.
    let mut b = bar_with_laid_out_rects();
    b.pin(2);
    move_to(&mut b, 95.0, 20.0);
    dispatch(&mut b, &press);
    assert_eq!(b.pending_close, None);

    // A window's only tab is drawn as its icon alone when the app puts
    // its slots in the title's row, and a middle press still closes it.
    let mut b = solo_bar_with_slots();
    assert!(b.icon_only(1));
    b.tab_rects = vec![Rect::new(0.0, 6.0, 40.0, 34.0)];
    move_to(&mut b, 20.0, 20.0);
    assert!(dispatch(&mut b, &press));
    assert_eq!(b.pending_close, Some(1), "the window's only tab closes");
}

/// A window of one tab whose app puts its slots in the title's row.
fn solo_bar_with_slots() -> TabBar {
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
    b.set_window_controls(true);
    assert!(b.set_toolbar(1, Some(browser_slots())));
    b
}

/// A secondary press on a bar whose app put its slots there opens the window's menu wherever it
/// does not land on one of the app's controls: on the location, which names the window as a title
/// does, and on the row's empty stretch. One on the app's own control is the app's.
#[test]
fn a_secondary_press_on_the_location_opens_the_windows_menu() {
    let mut b = solo_bar_with_slots();
    lay_out_at_width_with(&mut b, 900.0, &crate::theme::Theme::dark());
    let location = b.location_rect();
    assert!(
        location.area() > 0.0,
        "the app's location stands in the row"
    );
    let reload = b.toolbar_rects().3[0];
    let right = crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
    move_to(&mut b, location.center().x, location.center().y);
    assert!(dispatch(&mut b, &right), "the bar takes the press");
    assert_eq!(
        b.take_context_menu().map(|(tab, _)| tab),
        Some(1),
        "the location opens its window's menu"
    );
    move_to(&mut b, reload.center().x, reload.center().y);
    dispatch(&mut b, &right);
    assert_eq!(b.take_context_menu(), None, "the app's control keeps it");
}

/// One place says what a window is: its only tab is drawn as its icon alone when the bar's
/// slots carry the app's location, so the title is not painted over the location beside it.
#[test]
fn a_windows_only_tab_says_nothing_beside_the_bars_location() {
    let mut b = solo_bar_with_slots();
    let theme = crate::theme::Theme::light();
    let mut text = crate::text_render::TextRenderer::new();
    b.tab_rects = vec![Rect::new(0.0, 6.0, 40.0, 34.0)];
    b.visible_titles = vec![b.visible_title(0, &mut text, &theme)];
    assert!(
        b.visible_titles[0].is_none(),
        "the solo pill of a window with slots shows its icon alone"
    );
    assert_eq!(
        b.pill_label(0),
        None,
        "and draws no words there: the bar's location names the window"
    );
    // A bar of one tab with no slots of its own names the window by its title.
    let mut plain = TabBar::new("g2", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
    plain.set_window_controls(true);
    plain.visible_titles = vec![Some("DuckDuckGo".to_owned())];
    assert_eq!(plain.pill_label(0), Some("DuckDuckGo"));
}

/// The words the bar paints itself are reported where their ink stands: no widget lays them
/// out, so nothing else would tell a reader where the window's name is drawn.
#[test]
fn the_bar_reports_the_words_it_paints_where_they_stand() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::light().with_reduce_motion(true);
    let mut b = solo_bar_with_slots();
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let mut scene = Scene::new();
    b.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &PresenceCtx::default(),
    );
    let labels = b.bar_layout().labels;
    let location = labels
        .iter()
        .find(|label| label.widget == "BarLocation")
        .unwrap_or_else(|| panic!("the bar reports where it is: {labels:?}"));
    assert_eq!(location.text, "duckduckgo.com");
    let ink = Rect::new(
        location.rect[0],
        location.rect[1],
        location.rect[0] + location.rect[2],
        location.rect[1] + location.rect[3],
    );
    let slot = b.location_rect();
    assert!(
        ink.width() > 0.0
            && ink.height() > 0.0
            && ink.x0 >= slot.x0 - 0.5
            && ink.x1 <= slot.x1 + 0.5,
        "its words stand in the location's slot {slot:?}: {ink:?}"
    );
    assert!(
        labels.iter().all(|label| label.widget != "TabTitle"),
        "a window's only tab, its slots in the title's row, shows its icon alone: {labels:?}"
    );
}

/// A window's only tab draws its icon alone where the bar's location names the window, so the
/// bar reports one word there. The words the pill drew before [`TabBar::pill_label`] took them
/// from the decision that measured them are reported too, at the title's line beside the icon —
/// past the pill's own box, on the bar's ground where the location's words stand: what a layout
/// check reads as two words with nothing of their own between them.
#[test]
fn the_words_the_old_pill_painted_beside_its_mark_are_reported_where_they_stood() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::light().with_reduce_motion(true);
    let mut b = solo_bar_with_slots();
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let mut text = TextRenderer::new();
    b.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::default(),
    );
    let pill = b.tab_rects[0];
    let style = tab_title_style(&theme).with_weight(TextWeight::Semibold);
    let (width, height) = text.measure_style("DuckDuckGo", None, &style);
    let x = title_x0(pill, ICON_W + ICON_GAP);
    let y = pill.center().y - f64::from(height) / 2.0;
    b.record_label(
        "TabTitle",
        "DuckDuckGo",
        Rect::new(x, y, x + f64::from(width), y + f64::from(height)),
    );
    let labels = b.painted_labels();
    let title = labels
        .iter()
        .find(|label| label.widget == "TabTitle")
        .unwrap_or_else(|| panic!("the pill's words are reported: {labels:?}"));
    let box_ = Rect::new(
        title.rect[0],
        title.rect[1],
        title.rect[0] + title.rect[2],
        title.rect[1] + title.rect[3],
    );
    assert_eq!(title.text, "DuckDuckGo");
    assert!(
        box_.area() > 0.0 && box_.x0 >= pill.x1,
        "the words stand at the title's line past the pill's own {pill:?}: {box_:?}"
    );
}
