//! Buttons the runner drives: a press a reader sends by name, the width a
//! button keeps in a host, and the names an agent finds.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::Arc;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::{Dimension, Size};
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

fn measured_button_rect(button: Button, scale: f32) -> Rect {
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(tilo_ui_app::widget::Row::new().push(Box::new(button))),
        (1000, 200),
        Theme::dark().with_text_scale(scale),
    );
    let rect = harness.inspection().root.children[0].rect;
    Rect::new(rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3])
}

fn fit_host(kind: usize, button: Button) -> tilo_ui_base::widget::Element {
    match kind {
        0 => Box::new(
            tilo_ui_app::widget::Column::new()
                .with_no_padding()
                .push(Box::new(button)),
        ),
        1 => Box::new(
            tilo_ui_app::widget::Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(button)),
        ),
        _ => Box::new(
            tilo_ui_app::widget::Container::new()
                .with_no_padding()
                .with_child(Box::new(button)),
        ),
    }
}

fn glyph_origins(scene: &Scene) -> Vec<[f32; 2]> {
    scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .map(|run| run.transform.translation)
        .collect()
}

#[test]
fn own_device_effects_run_for_guests_and_refusals_are_said() {
    use tilo_permissions::PermissionContext;
    use tilo_permissions::Role;
    let click = |root: tilo_ui_base::widget::Element, role: Role| {
        let mut harness =
            Driver::new_with_theme(tilo_ui_app::State::new(), root, (600, 200), Theme::dark());
        // The window stands in its workspace, whose grants ask about what reaches outside.
        harness.connect_peer();
        harness.set_permission_ctx(PermissionContext::member(
            &tilo_member_types::MemberId::person("ada"),
            role,
        ));
        harness.relayout();
        harness.click_named("go").unwrap();
        harness.relayout();
        harness
    };
    let ran = Arc::new(AtomicUsize::new(0));
    let counter = || {
        let ran = Arc::clone(&ran);
        move || {
            ran.fetch_add(1, Ordering::SeqCst);
        }
    };

    let harness = click(
        fit_host(1, Button::new("go", "Copy").on_click(counter())),
        Role::Editor,
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "{:?}",
        harness.control_failures()
    );
    assert_eq!(
        harness.asking(),
        None,
        "an effect on one's own device asks nobody"
    );
    assert_eq!(harness.refusal_said(), None);

    let harness = click(
        fit_host(1, Button::new("go", "Copy").on_click(counter())),
        Role::Viewer,
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        2,
        "even a Viewer copies to their own clipboard: {:?}",
        harness.control_failures()
    );

    let harness = click(
        tilo_ui_app::widget::effect_fixture::sends(counter()),
        Role::Editor,
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        2,
        "a guest's effect the area asks about waits for the ask: {:?}",
        harness.control_failures()
    );
    assert!(
        harness.asking().is_some(),
        "the window asks, saying who decides: {:?}",
        harness.control_failures()
    );

    let harness = click(
        tilo_ui_app::widget::effect_fixture::sends(counter()),
        Role::Viewer,
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        2,
        "a Viewer reaches nothing outside"
    );
    assert!(
        harness.refusal_said().is_some(),
        "a refused click is said over the window: {:?}",
        harness.control_failures()
    );
}

#[test]
fn intrinsic_buttons_remeasure_and_size_from_their_control_size() {
    fn intrinsic_width_holds_the_whole_label() {
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        for label in ["Install workspace", "Refresh connections", "View details"] {
            let rect = measured_button_rect(Button::new("action", label), 1.0);
            let (width, _) = text.measure_style(label, None, &theme.text(TextRole::Label));
            assert!(
                rect.width() >= f64::from(width + theme.tokens.spacing.s3 * 2.0),
                "the label must fit without eliding: {label}: {rect:?}, text {width}"
            );
        }
    }

    fn each_control_size_has_its_height_and_grows_with_large_text() {
        for (size, height) in [
            (ControlSize::Small, 28.0),
            (ControlSize::Regular, 32.0),
            (ControlSize::Large, 40.0),
        ] {
            let rect = measured_button_rect(Button::new("save", "Save").control_size(size), 1.0);
            assert_eq!(rect.height(), height, "{size:?}");
            assert!(
                rect.width() >= 2.0 * height,
                "{size:?} labelled minimum: {rect:?}"
            );
        }
        let theme = Theme::dark().with_text_scale(2.0);
        let rect = measured_button_rect(Button::new("save", "Save"), 2.0);
        let line = f64::from(theme.text(TextRole::Label).line_height_px());
        assert!(rect.height() > 32.0 && rect.height() >= line, "{rect:?}");
    }

    fn icon_only_buttons_are_square_named_and_plain() {
        let button = Button::icon_only("close", tilo_ui_base::icons::Icon::X, "Close");
        assert_eq!(button.role_for_tests(), ButtonRole::Plain);
        assert_eq!(Widget::tooltip(&button).as_deref(), Some("Close"));
        assert_eq!(
            Widget::accessibility(&button).unwrap().name.as_deref(),
            Some("Close")
        );
        let rect = measured_button_rect(button, 1.0);
        assert_eq!((rect.width(), rect.height()), (32.0, 32.0));
    }

    fn a_shortcut_hint_widens_the_button_and_paints_after_the_label() {
        let shortcut = tilo_ui_base::platform::Shortcut::command("Enter");
        let plain = measured_button_rect(Button::new("run", "Run query"), 1.0);
        let hinted = measured_button_rect(
            Button::new("run", "Run query").shortcut_hint(shortcut.clone()),
            1.0,
        );
        assert!(hinted.width() > plain.width(), "{plain:?} -> {hinted:?}");
        let label = shortcut.label(tilo_ui_base::platform::PlatformConventions::new(
            tilo_ui_base::platform::Platform::current(),
        ));
        if tilo_ui_base::platform::Platform::current() == tilo_ui_base::platform::Platform::Linux {
            assert!(!label.contains('⌘'), "no Apple legend on Linux: {label}");
        }
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let face = ButtonFace::new(Rect::ZERO, "Run query").shortcut_hint(shortcut);
        let width = face.intrinsic_width(&mut text, &theme);
        let face = ButtonFace::new(Rect::new(0.0, 0.0, width, 32.0), "Run query")
            .shortcut_hint(tilo_ui_base::platform::Shortcut::command("Enter"));
        let mut scene = Scene::new();
        Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
        let origins = glyph_origins(&scene);
        assert_eq!(origins.len(), 2, "label and hint are two runs");
        assert!(origins[1][0] > origins[0][0], "the hint trails the label");
    }

    fn a_face_hit_rect_is_never_under_the_pointer_target() {
        let theme = Theme::dark();
        let face = ButtonFace::icon_only(
            Rect::new(10.0, 10.0, 30.0, 30.0),
            tilo_ui_base::icons::Icon::X,
            "Close",
        );
        let hit = face.hit_rect(&theme);
        assert_eq!((hit.width(), hit.height()), (32.0, 32.0));
        assert_eq!(hit.center(), face.rect_for_tests().center());
        assert!(face.contains(&theme, Point::new(8.0, 20.0)));
        assert!(!face.contains(&theme, Point::new(40.0, 20.0)));
    }

    intrinsic_width_holds_the_whole_label();
    each_control_size_has_its_height_and_grows_with_large_text();
    icon_only_buttons_are_square_named_and_plain();
    a_shortcut_hint_widens_the_button_and_paints_after_the_label();
    a_face_hit_rect_is_never_under_the_pointer_target();
}

#[test]
fn compact_fit_width_is_the_prepaint_click_and_focus_geometry() {
    let fired = Arc::new(AtomicUsize::new(0));
    let clicks = Arc::clone(&fired);
    let button = Button::new("back", "Back to apps").on_click(move || {
        clicks.fetch_add(1, Ordering::SeqCst);
    });
    let mut harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        fit_host(1, button),
        (600, 200),
        Theme::dark(),
    );
    let before = harness.inspection().root.children[0].rect;
    harness.click_at(550.0, before[1] + before[3] / 2.0);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "blank grid track must not activate compact button"
    );
    harness.click_at(before[0] + before[2] / 2.0, before[1] + before[3] / 2.0);
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    assert_eq!(
        harness
            .focused_access_node()
            .unwrap()
            .semantic_key
            .as_deref(),
        Some("back")
    );
    harness.resize((800, 200)).unwrap();
    assert_eq!(harness.inspection().root.children[0].rect[2], before[2]);
}

#[test]
fn an_icon_only_effects_spoken_name_is_the_press_an_agent_finds() {
    let named = Button::new("private", "")
        .with_icon(tilo_ui_base::icons::Icon::Shield)
        .with_accessible_name("New Private Window")
        .on_click(|| {});
    let tooltip_after = Button::new("search", "")
        .with_icon(tilo_ui_base::icons::Icon::Search)
        .on_click(|| {})
        .tooltip("Search");
    // What a screen reader says, and what an agent shown the window finds the press by.
    for (button, name) in [(named, "New Private Window"), (tooltip_after, "Search")] {
        let harness = Driver::new(
            tilo_ui_app::State::new(),
            Box::new(tilo_ui_app::widget::Row::new().push(Box::new(button))),
            (400, 100),
        );
        let pressable: Vec<_> = harness
            .controls()
            .into_iter()
            .filter(|control| control.verbs.iter().any(|verb| verb == "activate"))
            .collect();
        assert_eq!(pressable.len(), 1, "{pressable:?}");
        assert_eq!(pressable[0].name.as_deref(), Some(name));
    }
}

#[test]
fn a_plain_label_sits_on_its_layout_edge() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let label = f64::from(
        text.measure_style("Show details", None, &theme.text(TextRole::Label))
            .0,
    );
    let plain = measured_button_rect(
        Button::new("details", "Show details").role(ButtonRole::Plain),
        1.0,
    );
    let secondary = measured_button_rect(Button::new("details", "Show details"), 1.0);
    assert!(
        (plain.width() - label.ceil()).abs() <= 1.0,
        "a plain button is as wide as its ink: {plain:?} vs {label}"
    );
    assert!(secondary.width() > plain.width());
    let mut button = Button::new("details", "Show details").role(ButtonRole::Plain);
    button.set_stock_metrics_for_tests(ControlSize::Regular, &theme);
    button.apply_layout_external(plain);
    let hit = button.hit_rect_for_tests();
    assert!(
        hit.x0 < plain.x0 && hit.x1 > plain.x1,
        "the hit area reaches out"
    );
}

#[test]
fn a_shared_button_answers_through_an_ordinary_container() {
    let clicks = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&clicks);
    let column = tilo_ui_app::widget::Column::new().push(Box::new(
        Button::new("files.retry", "Retry").on_open(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    ));
    let mut harness = Driver::new(tilo_ui_app::State::new(), Box::new(column), (400, 120));
    harness
        .perform_named("files.retry", &tilo_ui_base::a11y::Action::Activate)
        .expect("the press reaches the button through its container");

    assert_eq!(clicks.load(Ordering::SeqCst), 1);
}
