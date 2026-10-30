use super::*;
use crate::tokens::TextRole;

/// A button that opens a menu makes room for its chevron after the
/// label, so the label is never elided to fit it; an icon-only one
/// stays square.
#[test]
fn a_menu_button_makes_room_for_its_chevron() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let metrics = FaceMetrics::of_role(ControlSize::Small, ButtonRole::Secondary, &theme);
    let width = |label: &str, menu: bool, text: &mut TextRenderer| {
        FaceContent {
            icon: None,
            image: None,
            badge: None,
            label,
            hint: None,
            flush: false,
            filled_icon: false,
            menu,
        }
        .intrinsic_width(&metrics, text, metrics.height)
    };
    let label = "Administrator";
    let plain = width(label, false, &mut text);
    let menu = width(label, true, &mut text);
    assert!(
        (menu - plain - (metrics.icon_gap + metrics.icon)).abs() <= 1.0,
        "{plain} then {menu}"
    );
    assert!((width("", true, &mut text) - metrics.height).abs() < 1e-9);
    let button = Button::new("role", label).with_menu(Vec::new());
    assert!(button.button().opens_menu);
    assert!(!Button::new("save", "Save").opens_menu);
}
use crate::widget::Widget;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// An effect on the person's own device runs for a guest (an Editor) and
/// even a Viewer without asking anyone; a Viewer's click on one reaching
/// outside is refused, and the window says so. No click is dropped
/// unseen. (A framework app's ask is shown too: api-tester's
/// `a_guests_send_is_asked_in_the_window_saying_who_decides`.)
#[test]
fn own_device_effects_run_for_guests_and_refusals_are_said() {
    use crate::permissions::{PermissionContext, Role};
    let click = |button: Button, role: Role| {
        let mut harness = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            fit_host(1, button),
            (600, 200),
            Theme::dark(),
        );
        harness.set_permission_ctx(PermissionContext::member("ada", role));
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
        Button::new("go", "Copy").on_own_device_effect(counter()),
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
        Button::new("go", "Copy").on_own_device_effect(counter()),
        Role::Viewer,
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        2,
        "even a Viewer copies to their own clipboard: {:?}",
        harness.control_failures()
    );

    let harness = click(Button::new("go", "Send").on_effect(counter()), Role::Editor);
    assert_eq!(
        ran.load(Ordering::SeqCst),
        2,
        "a guest's effect the room asks about waits for the ask: {:?}",
        harness.control_failures()
    );
    assert!(
        harness.asking().is_some(),
        "the window asks, saying who decides: {:?}",
        harness.control_failures()
    );

    let harness = click(Button::new("go", "Send").on_effect(counter()), Role::Viewer);
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

/// An app rebuilds its buttons from state, and the host then refocuses
/// the new one: it never saw the press. The ring still follows the
/// input that focused it, so a clicked button stays ringless.
#[test]
fn a_button_rebuilt_after_a_click_shows_no_focus_ring() {
    crate::focus_visible::note(&crate::synthetic_input::mouse_press());
    let mut rebuilt = Button::new("wp.bold", "Bold");
    rebuilt.set_focus(true);
    assert!(!rebuilt.focus_visible(), "a click never rings the button");
    crate::focus_visible::note(&crate::synthetic_input::keyboard_input(
        crate::synthetic_input::key_event(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
            None,
            winit::event::ElementState::Pressed,
        ),
    ));
    assert!(rebuilt.focus_visible(), "the keyboard brings the ring back");
}

#[test]
fn a_shared_button_answers_through_an_ordinary_container() {
    let clicks = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&clicks);
    let mut column = crate::widget::Column::new().push(Box::new(
        Button::new("files.retry", "Retry").on_view(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    ));
    let mut doc = AutoCommit::new();
    let kind = tilo_actions::OperationKind::View;
    let payload = br#""files.retry""#;

    let started = crate::widget::ResourceOperation::local(
        tilo_actions::InvocationId::new(),
        kind,
        "tilo.ui.control.invoke",
        payload,
    )
    .start(
        &mut column,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    )
    .unwrap();
    assert!(
        started
            .and_then(super::super::ResourceOperationStart::completed)
            .is_some()
    );

    assert_eq!(clicks.load(Ordering::SeqCst), 1);
}

fn layout_fit_host(
    host: &mut dyn Widget,
    alignment: taffy::AlignItems,
    theme: &Theme,
    text: &mut TextRenderer,
) -> Rect {
    let mut tree = TaffyTree::new();
    let mut map = std::collections::HashMap::new();
    let root = host.build_layout(&mut tree, theme, &mut map);
    let mut style = tree.style(root).unwrap().clone();
    style.size.width = Dimension::length(600.0);
    if style.display == taffy::Display::Grid {
        style.justify_items = Some(alignment);
    } else if style.flex_direction == taffy::FlexDirection::Row {
        style.justify_content = Some(match alignment {
            taffy::AlignItems::Center => taffy::JustifyContent::Center,
            taffy::AlignItems::End => taffy::JustifyContent::End,
            _ => taffy::JustifyContent::Start,
        });
    } else {
        style.align_items = Some(alignment);
    }
    tree.set_style(root, style).unwrap();
    let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
    unsafe {
        bindings.rebuild(host, &tree, root, &map);
        crate::widget::compute_widget_layout(
            &mut tree,
            root,
            Size {
                width: taffy::AvailableSpace::Definite(600.0),
                height: taffy::AvailableSpace::Definite(200.0),
            },
            &map,
            &bindings,
            text,
        )
        .unwrap();
    }
    host.apply_layout(&tree, root, Point::ZERO);
    host.children()[0].get_rect()
}

fn fit_host(kind: usize, button: Button) -> crate::widget::Element {
    match kind {
        0 => Box::new(
            crate::widget::Column::new()
                .with_no_padding()
                .push(Box::new(button)),
        ),
        1 => Box::new(
            crate::widget::Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(button)),
        ),
        _ => Box::new(
            crate::widget::Container::new()
                .with_no_padding()
                .with_child(Box::new(button)),
        ),
    }
}

#[test]
fn intrinsic_width_stays_compact_and_aligned_in_hosts() {
    fn intrinsic_width_stays_compact_in_stretched_flex_column() {
        let mut host = fit_host(0, Button::new("back", "Back to apps"));
        let rect = layout_fit_host(
            host.as_mut(),
            taffy::AlignItems::Stretch,
            &Theme::dark(),
            &mut TextRenderer::new(),
        );
        assert!(rect.width() < 300.0, "intrinsic width stretched: {rect:?}");
        assert_eq!(rect.height(), 32.0);
    }

    fn intrinsic_width_stays_compact_in_intrinsic_grid_column() {
        let mut host = fit_host(1, Button::new("back", "Back to apps"));
        let rect = layout_fit_host(
            host.as_mut(),
            taffy::AlignItems::Stretch,
            &Theme::dark(),
            &mut TextRenderer::new(),
        );
        assert!(rect.width() < 300.0, "intrinsic width stretched: {rect:?}");
        assert_eq!(rect.height(), 32.0);
    }

    fn compact_fit_width_preserves_host_center_and_end_alignment() {
        for kind in 0..3 {
            for alignment in [taffy::AlignItems::Center, taffy::AlignItems::End] {
                let mut host = fit_host(kind, Button::new("back", "Back to apps"));
                let rect = layout_fit_host(
                    host.as_mut(),
                    alignment,
                    &Theme::dark(),
                    &mut TextRenderer::new(),
                );
                assert!(rect.width() < 300.0, "host {kind}: {rect:?}");
                let edge = if alignment == taffy::AlignItems::Center {
                    rect.center().x * 2.0
                } else {
                    rect.x1
                };
                assert!(
                    (edge - 600.0).abs() <= 1.0,
                    "host {kind}, {alignment:?}: {rect:?}"
                );
            }
        }
    }

    fn compact_fit_width_is_the_prepaint_click_and_focus_geometry() {
        let fired = Arc::new(AtomicUsize::new(0));
        let clicks = Arc::clone(&fired);
        let button = Button::new("back", "Back to apps").on_click(move || {
            clicks.fetch_add(1, Ordering::SeqCst);
        });
        let mut harness = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
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

    fn explicit_fill_width_still_stretches_in_intrinsic_column() {
        let mut host = fit_host(1, Button::new("fill", "Back to apps").fill_width());
        let rect = layout_fit_host(
            host.as_mut(),
            taffy::AlignItems::Stretch,
            &Theme::dark(),
            &mut TextRenderer::new(),
        );
        assert_eq!(rect.width(), 600.0);
    }

    intrinsic_width_stays_compact_in_stretched_flex_column();
    intrinsic_width_stays_compact_in_intrinsic_grid_column();
    compact_fit_width_preserves_host_center_and_end_alignment();
    compact_fit_width_is_the_prepaint_click_and_focus_geometry();
    explicit_fill_width_still_stretches_in_intrinsic_column();
}

#[test]
fn intrinsic_buttons_remeasure_and_size_from_their_control_size() {
    fn retained_width_remeasures_scale_and_locale_after_rebuild() {
        use crate::i18n::I18nManager;
        I18nManager::install(
            "en-US",
            "en-US",
            &[
                ("en-US", "fit-refresh = Refresh"),
                ("de", "fit-refresh = Verbindungen aktualisieren"),
            ],
        )
        .unwrap();
        let mut host = fit_host(1, Button::new("refresh", "fit-refresh"));
        let mut text = TextRenderer::new();
        let normal = layout_fit_host(
            host.as_mut(),
            taffy::AlignItems::Stretch,
            &Theme::dark(),
            &mut text,
        );
        text.set_text_scale(2.0);
        let large_theme = Theme::dark().with_text_scale(2.0);
        let large = layout_fit_host(
            host.as_mut(),
            taffy::AlignItems::Stretch,
            &large_theme,
            &mut text,
        );
        assert!(
            large.width() > normal.width(),
            "renderer scale must refresh retained width: {normal:?} -> {large:?}"
        );
        I18nManager::set_locale("de");
        assert!(host.is_layout_dirty());
        let localized = layout_fit_host(
            host.as_mut(),
            taffy::AlignItems::Stretch,
            &large_theme,
            &mut text,
        );
        I18nManager::set_locale("en-US");
        assert!(
            localized.width() > large.width(),
            "locale must refresh retained width: {large:?} -> {localized:?}"
        );
        assert_eq!(
            localized.height(),
            ControlSize::Regular.height(&large_theme)
        );
    }

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
        let button = Button::icon_only("close", crate::icons::Icon::X, "Close");
        assert_eq!(button.role, ButtonRole::Plain);
        assert_eq!(Widget::tooltip(&button), Some("Close"));
        assert_eq!(
            Widget::accessibility(&button).unwrap().name.as_deref(),
            Some("Close")
        );
        let rect = measured_button_rect(button, 1.0);
        assert_eq!((rect.width(), rect.height()), (32.0, 32.0));
    }

    fn a_shortcut_hint_widens_the_button_and_paints_after_the_label() {
        let shortcut = crate::platform::Shortcut::command("Enter");
        let plain = measured_button_rect(Button::new("run", "Run query"), 1.0);
        let hinted = measured_button_rect(
            Button::new("run", "Run query").shortcut_hint(shortcut.clone()),
            1.0,
        );
        assert!(hinted.width() > plain.width(), "{plain:?} -> {hinted:?}");
        let label = shortcut.label(crate::platform::PlatformConventions::new(
            crate::platform::Platform::current(),
        ));
        if crate::platform::Platform::current() == crate::platform::Platform::Linux {
            assert!(!label.contains('⌘'), "no Apple legend on Linux: {label}");
        }
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let face = ButtonFace::new(Rect::ZERO, "Run query").shortcut_hint(shortcut);
        let width = face.intrinsic_width(&mut text, &theme);
        let face = ButtonFace::new(Rect::new(0.0, 0.0, width, 32.0), "Run query")
            .shortcut_hint(crate::platform::Shortcut::command("Enter"));
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
            crate::icons::Icon::X,
            "Close",
        );
        let hit = face.hit_rect(&theme);
        assert_eq!((hit.width(), hit.height()), (32.0, 32.0));
        assert_eq!(hit.center(), face.rect.center());
        assert!(face.contains(&theme, Point::new(8.0, 20.0)));
        assert!(!face.contains(&theme, Point::new(40.0, 20.0)));
    }

    retained_width_remeasures_scale_and_locale_after_rebuild();
    intrinsic_width_holds_the_whole_label();
    each_control_size_has_its_height_and_grows_with_large_text();
    icon_only_buttons_are_square_named_and_plain();
    a_shortcut_hint_widens_the_button_and_paints_after_the_label();
    a_face_hit_rect_is_never_under_the_pointer_target();
}

fn measured_button_rect(button: Button, scale: f32) -> Rect {
    let harness = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(crate::widget::Row::new().push(Box::new(button))),
        (1000, 200),
        Theme::dark().with_text_scale(scale),
    );
    let rect = harness.inspection().root.children[0].rect;
    Rect::new(rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3])
}

#[test]
fn label_face_geometry_follows_the_theme_scale() {
    fn retained_and_immediate_labels_share_geometry() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        text.set_text_scale(2.0);
        let label = "Refresh";
        let rect = Rect::new(10.0, 20.0, 330.0, 84.0);
        let mut button = Button::new("action", label).with_icon(crate::icons::Icon::Refresh);
        let mut tree = TaffyTree::new();
        button.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
        button.apply_layout_external(rect);
        let mut retained = Scene::new();
        button.draw(
            &mut retained,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &crate::presence::PresenceCtx::default(),
        );
        let mut immediate = Scene::new();
        Button::paint(
            &mut immediate,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &ButtonFace::new(rect, label).icon(crate::icons::Icon::Refresh),
        );
        let a = &retained.encoding().resources.glyph_runs;
        let b = &immediate.encoding().resources.glyph_runs;
        assert!(!a.is_empty());
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(b) {
            assert_eq!(a.transform, b.transform);
            assert_eq!(a.font_size, b.font_size);
        }
    }

    /// A default `ButtonFace` reads its corner from the theme and sets its
    /// label in the Regular size's role; Large sets it in Body.
    fn default_face_reads_its_corner_and_label_role_from_the_theme() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(10.0, 10.0, 130.0, 50.0);
        let mut paint = |face: ButtonFace<'_>| {
            let mut scene = Scene::new();
            Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
            scene
        };
        let themed = paint(ButtonFace::new(rect, "Save").icon(crate::icons::Icon::Refresh));
        let explicit = paint(
            ButtonFace::new(rect, "Save")
                .icon(crate::icons::Icon::Refresh)
                .control_size(ControlSize::Regular),
        );
        assert_eq!(themed.encoding().path_data, explicit.encoding().path_data);
        let a = &themed.encoding().resources.glyph_runs;
        let b = &explicit.encoding().resources.glyph_runs;
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].transform, b[0].transform);
        assert_eq!(a[0].font_size, b[0].font_size);
        assert!((a[0].font_size - theme.text(TextRole::Label).size).abs() < 1e-6);
        let large = paint(ButtonFace::new(rect, "Save").control_size(ControlSize::Large));
        assert!(
            (large.encoding().resources.glyph_runs[0].font_size - theme.text(TextRole::Body).size)
                .abs()
                < 1e-6
        );
    }

    /// The retained and immediate labels follow the theme's type scale —
    /// 2× the Label role under a 2× theme — whether the shared renderer is
    /// matched (2×, the live App and `FrameHarness`) or left at 1× (the
    /// apps' raw `--shots` loop), and whether or not a `build_layout` pass
    /// preceded the draw.
    fn retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts() {
        let theme = Theme::dark().with_text_scale(2.0);
        let label = theme.text(TextRole::Label).size;
        let rect = Rect::new(0.0, 0.0, 160.0, 40.0);
        let presence = crate::presence::PresenceCtx::default();
        let font_size = |scene: &Scene| scene.encoding().resources.glyph_runs[0].font_size;
        for renderer_scale in [1.0_f32, 2.0] {
            let mut text = TextRenderer::new();
            text.set_text_scale(renderer_scale);

            let mut laid_out = Button::new("save", "Save");
            laid_out.build_layout(
                &mut TaffyTree::new(),
                &theme,
                &mut std::collections::HashMap::new(),
            );
            laid_out.apply_layout_external(rect);
            let mut retained = Scene::new();
            laid_out.draw(
                &mut retained,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &presence,
            );
            assert!(
                (font_size(&retained) - label).abs() < 1e-3,
                "retained label at renderer {renderer_scale}×: {}",
                font_size(&retained)
            );

            // Positioned by a canvas host: no taffy pass at all.
            let mut external = Button::new("save", "Save");
            external.apply_layout_external(rect);
            let mut external_scene = Scene::new();
            external.draw(
                &mut external_scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &presence,
            );
            assert!(
                (font_size(&external_scene) - label).abs() < 1e-3,
                "externally laid-out label at renderer {renderer_scale}×: {}",
                font_size(&external_scene)
            );

            let mut immediate = Scene::new();
            Button::paint(
                &mut immediate,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &ButtonFace::new(rect, "Save"),
            );
            assert!(
                (font_size(&immediate) - label).abs() < 1e-3,
                "ButtonFace default at renderer {renderer_scale}×: {} (expected {label})",
                font_size(&immediate)
            );
        }
    }

    /// A theme text-scale change between frames re-emits the cached face at
    /// the new label size instead of replaying the old glyphs.
    fn theme_text_scale_change_invalidates_the_cached_face() {
        let rect = Rect::new(0.0, 0.0, 160.0, 40.0);
        let mut text = TextRenderer::new();
        let mut button = Button::new("save", "Save");
        let presence = crate::presence::PresenceCtx::default();
        let one = Theme::dark();
        button.build_layout(
            &mut TaffyTree::new(),
            &one,
            &mut std::collections::HashMap::new(),
        );
        button.apply_layout_external(rect);
        let mut first = Scene::new();
        button.draw(&mut first, Affine::IDENTITY, &mut text, &one, &presence);
        button.mark_drawn();
        assert!(!button.needs_redraw(), "settled at 1×");

        let larger = Theme::dark().with_text_scale(1.5);
        button.build_layout(
            &mut TaffyTree::new(),
            &larger,
            &mut std::collections::HashMap::new(),
        );
        assert!(
            button.needs_redraw(),
            "a new label size must dirty the cache"
        );
        let mut second = Scene::new();
        button.draw(&mut second, Affine::IDENTITY, &mut text, &larger, &presence);
        let before = first.encoding().resources.glyph_runs[0].font_size;
        let after = second.encoding().resources.glyph_runs[0].font_size;
        assert!((before - 13.0).abs() < 1e-3, "{before}");
        assert!((after - 19.5).abs() < 1e-3, "{after}");
    }

    retained_and_immediate_labels_share_geometry();
    default_face_reads_its_corner_and_label_role_from_the_theme();
    retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts();
    theme_text_scale_change_invalidates_the_cached_face();
}

#[test]
fn action_mode_and_availability_gate_mutation_and_callbacks() {
    fn button_test_id_is_stable_constructor_id() {
        let b = Button::new("id", "Save");
        assert_eq!(Widget::test_id(&b), Some("id"));
        assert_eq!(
            Widget::accessibility(&b).unwrap().name.as_deref(),
            Some("Save")
        );
    }

    fn action_mode_suppresses_raw_mutation_and_callback() {
        let resource = tilo_actions::ResourceSpec::new("doc:1", "doc", "Document").with_action(
            tilo_actions::ActionSpec::operation(
                "save",
                "Save",
                tilo_actions::OperationKind::Effect,
                "test.save",
                &(),
            )
            .unwrap(),
        );
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "save").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let mut button = Button::new("save", "Save")
            .on_action(binding)
            // Builder order cannot accidentally restore an imperative path.
            .on_click(move || {
                callback_calls.fetch_add(1, Ordering::SeqCst);
            });
        button.activate();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let intent =
            crate::widget::controls::take_control_invocation(&mut button).expect("one intent");
        assert_eq!(
            intent.address,
            tilo_actions::ActionAddress::new("doc:1", "save")
        );
        assert!(intent.arguments.is_empty());
        assert_eq!(
            crate::widget::controls::control_action_bindings(&button).len(),
            1
        );
    }

    fn action_availability_revokes_and_restores_without_overriding_authored_state() {
        let resource = tilo_actions::ResourceSpec::new("doc:1", "doc", "Document").with_action(
            tilo_actions::ActionSpec::operation(
                "save",
                "Save",
                tilo_actions::OperationKind::Effect,
                "test.save",
                &(),
            )
            .unwrap(),
        );
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "save").unwrap();
        let mut button = Button::new("save", "Save").on_action(binding);
        button.activate();
        crate::widget::controls::refresh_control_action_availability(
            &mut button,
            &std::collections::HashSet::new(),
        );
        assert!(!Widget::is_enabled(&button));
        assert!(
            crate::widget::controls::take_control_invocation(&mut button).is_none(),
            "revocation drops queued authority"
        );
        crate::widget::controls::refresh_control_action_availability(
            &mut button,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new("doc:1", "save")]),
        );
        assert!(Widget::is_enabled(&button));

        button.set_enabled(false);
        crate::widget::controls::refresh_control_action_availability(
            &mut button,
            &std::collections::HashSet::new(),
        );
        crate::widget::controls::refresh_control_action_availability(
            &mut button,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new("doc:1", "save")]),
        );
        assert!(
            !Widget::is_enabled(&button),
            "permission restore cannot override app-authored disabled state"
        );
    }

    fn legacy_button_paths_are_explicit_in_inventory() {
        let button = Button::new("legacy", "Legacy").on_click(|| {});
        let bindings = crate::widget::controls::control_action_bindings(&button);
        assert!(bindings.iter().any(|entry| matches!(
            entry.kind,
            crate::widget::ControlBindingKind::LegacyCallback
        )));
    }

    button_test_id_is_stable_constructor_id();
    action_mode_suppresses_raw_mutation_and_callback();
    action_availability_revokes_and_restores_without_overriding_authored_state();
    legacy_button_paths_are_explicit_in_inventory();
}

/// A theme switched under a live tree repaints a plain button's face
/// in the new theme's roles, rather than keeping the cached dark-theme
/// ink that all but vanishes on a light ground.
#[test]
fn a_cached_face_follows_a_theme_switch() {
    let mut button = Button::new("plain", "Undo").role(ButtonRole::Plain);
    button.rect = Rect::new(0.0, 0.0, 80.0, 32.0);
    let presence = crate::presence::PresenceCtx::default();
    let mut text = TextRenderer::new();
    let mut scene = Scene::new();
    for theme in [Theme::dark(), Theme::light()] {
        button.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        assert_eq!(
            button.last_theme,
            Some(theme_key(&theme)),
            "the face is painted in {}",
            theme.name
        );
    }
}

/// Only a node the layout solver placed can be measured again: a button
/// its host places never asks for a measure, placed or hidden, while one
/// in the solver's tree asks when what it shows may change its width.
#[test]
fn a_host_placed_button_never_asks_the_solver_for_a_measure() {
    let mut hosted = Button::new("history", "History");
    for rect in [Rect::ZERO, Rect::new(0.0, 0.0, 32.0, 32.0), Rect::ZERO] {
        hosted.apply_layout_external(rect);
        assert!(!hosted.layout_needs_measure(), "placed at {rect:?}");
    }

    let mut laid_out = Button::new("save", "Save");
    let mut tree = TaffyTree::new();
    let node = laid_out.build_layout(
        &mut tree,
        &Theme::dark(),
        &mut std::collections::HashMap::new(),
    );
    tree.compute_layout(
        node,
        taffy::Size {
            width: taffy::AvailableSpace::MaxContent,
            height: taffy::AvailableSpace::MaxContent,
        },
    )
    .unwrap();
    laid_out.apply_layout(&tree, node, Point::ZERO);
    assert!(!laid_out.layout_needs_measure(), "laid out and unchanged");
    laid_out.set_interaction_environment(
        crate::interaction::InteractionEnvironment::default().with_touch_modality(),
    );
    assert!(
        laid_out.layout_needs_measure(),
        "a touch target may be wider"
    );
}

/// The keyboard focus ring reads as a ring on every face: where it meets
/// a fill it does not contrast with (a primary face is your colour, as
/// the ring is), a line of the ground parts the two.
#[test]
fn the_focus_ring_stands_apart_from_every_face() {
    use crate::offscreen::OffscreenRenderer;
    use crate::tokens::PresenceHue;
    use crate::tokens::colors::NON_TEXT_MIN_CONTRAST;
    let pixel = |rgba: &[u8], y: u32| {
        let (r, g, b, a) = OffscreenRenderer::sample(rgba, 128, 64, y);
        vello::peniko::Color::from_rgba8(r, g, b, a)
    };
    // Half-pixel edges: the ring covers rows 8 and 9, the parting line
    // row 10, and the fill starts at row 11.
    let rect = Rect::new(8.5, 8.5, 120.5, 48.5);
    for base in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        for hue in [PresenceHue::Blue, PresenceHue::Amber, PresenceHue::Magenta] {
            let theme = base.clone().with_your_colour(hue);
            for role in [ButtonRole::Primary, ButtonRole::Secondary] {
                let mut scene = Scene::new();
                let face = ButtonFace::new(rect, "Save").role(role).focused(true);
                Button::paint(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &theme,
                    &face,
                );
                let rgba = OffscreenRenderer::new(128, 64)
                    .render(&scene, theme.tokens.colors.surfaces.content);
                let (ring, inside, fill) = (pixel(&rgba, 9), pixel(&rgba, 10), pixel(&rgba, 12));
                let contrast = crate::a11y::contrast_ratio;
                assert!(
                    contrast(ring, inside) >= NON_TEXT_MIN_CONTRAST,
                    "{} {hue:?} {role:?}: the ring blends into what it rings",
                    theme.name
                );
                if inside != fill {
                    assert!(contrast(inside, fill) >= NON_TEXT_MIN_CONTRAST);
                }
            }
        }
    }
}

#[test]
fn face_colors_per_role_track_state_and_stay_readable() {
    use crate::a11y::contrast_ratio;
    let hover = ButtonInteract {
        hovered: true,
        ..Default::default()
    };
    let press = ButtonInteract {
        pressed: true,
        ..Default::default()
    };
    let rest = ButtonInteract::default();
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let colors = &theme.tokens.colors;
        let raised = colors.surfaces.raised;

        // Primary is inverted: primary ink as the fill, never a hue.
        let (fill, edge, ink) = face_colors(&theme, ButtonRole::Primary, rest);
        assert_eq!(fill, colors.intent.accent, "{}", theme.name);
        assert_eq!(edge, None);
        assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
        let (hover_fill, _, hover_ink) = face_colors(&theme, ButtonRole::Primary, hover);
        assert_eq!(hover_fill, colors.intent.accent_hover);
        assert_eq!(
            hover_ink, ink,
            "the label does not change under the pointer"
        );
        // Pressed reads past hover: rest, hover and press are three
        // distinct steps, and the label holds across all of them.
        let (press_fill, _, press_ink) = face_colors(&theme, ButtonRole::Primary, press);
        assert_eq!(press_fill, colors.intent.accent_pressed, "{}", theme.name);
        assert_ne!(press_fill, hover_fill, "{}", theme.name);
        let step = |a, b| crate::chrome::shell::lightness_step(a, b);
        assert!(
            step(press_fill, fill) > step(hover_fill, fill),
            "{}: press must move further from rest than hover",
            theme.name
        );
        assert_eq!(press_ink, ink);
        assert!(contrast_ratio(press_ink, press_fill) >= ON_FILL_MIN_CONTRAST);

        // Secondary is the raised control; the raised level has no edge,
        // its step off the ground separates it (design language,
        // "Surfaces").
        let (fill, edge, ink) = face_colors(&theme, ButtonRole::Secondary, rest);
        assert_eq!(fill, raised);
        assert_eq!(edge, None);
        assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
        assert_eq!(
            face_colors(&theme, ButtonRole::Secondary, hover).0,
            colors.lift_on(raised, 0.5)
        );
        assert_eq!(
            face_colors(&theme, ButtonRole::Secondary, press).0,
            colors.lift_on(raised, 1.0)
        );

        // Destructive is the same neutral control lettered in danger ink.
        for interact in [rest, hover, press] {
            let (fill, edge, ink) = face_colors(&theme, ButtonRole::Destructive, interact);
            assert_eq!(fill, face_colors(&theme, ButtonRole::Secondary, interact).0);
            assert_eq!(edge, face_colors(&theme, ButtonRole::Secondary, interact).1);
            assert!(
                contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST,
                "{}: danger ink on the neutral face",
                theme.name
            );
            assert_ne!(
                ink, colors.text_primary,
                "{}: reads as destructive",
                theme.name
            );
        }

        // Plain paints nothing at rest and lifts its ground under the pointer.
        let (fill, edge, ink) = face_colors(&theme, ButtonRole::Plain, rest);
        assert!(fill.components[3] <= 0.0);
        assert_eq!(edge, None);
        assert!(contrast_ratio(ink, colors.surfaces.content) >= ON_FILL_MIN_CONTRAST);
        assert_eq!(
            face_colors(&theme, ButtonRole::Plain, hover).0,
            colors.intent.selection_hover
        );
        // Press is the neutral lift; only a selection is tinted.
        assert_eq!(
            face_colors(&theme, ButtonRole::Plain, press).0,
            colors.lift_on(colors.surfaces.content, 1.0)
        );
    }
}

/// The filled danger face exists only in a confirmation step.
#[test]
fn only_the_confirmation_step_fills_a_destructive_button() {
    use crate::a11y::contrast_ratio;
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let rect = Rect::new(0.0, 0.0, 120.0, 32.0);
        let plain = ButtonFace::new(rect, "Delete").role(ButtonRole::Destructive);
        assert_ne!(plain.colors(&theme).0, theme.tokens.colors.semantic.danger);
        let confirm = ButtonFace::new(rect, "Delete")
            .role(ButtonRole::Destructive)
            .confirming();
        for face in [
            confirm.hovered(false),
            ButtonFace::new(rect, "Delete")
                .role(ButtonRole::Destructive)
                .confirming()
                .hovered(true),
        ] {
            let (fill, edge, ink) = face.colors(&theme);
            assert_eq!(edge, None);
            assert!(
                contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST,
                "{}: confirmation label {:.2}:1",
                theme.name,
                contrast_ratio(ink, fill)
            );
        }
        assert_eq!(
            ButtonFace::new(rect, "Delete")
                .role(ButtonRole::Destructive)
                .confirming()
                .colors(&theme)
                .0,
            theme.tokens.colors.semantic.danger
        );
    }
}

#[test]
fn disabled_faces_are_neutral_and_reject_input() {
    fn disabled_faces_are_neutral_bordered_and_readable_in_every_theme() {
        use crate::a11y::contrast_ratio;

        let disabled = ButtonInteract {
            hovered: true,
            pressed: true,
            focused: true,
            disabled: true,
        };
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            for style in [
                ButtonRole::Primary,
                ButtonRole::Secondary,
                ButtonRole::Destructive,
            ] {
                let (bg, border, fg) = face_colors(&theme, style, disabled);
                assert_eq!(
                    bg, theme.tokens.colors.surfaces.raised,
                    "{name} {style:?}: disabled face uses the neutral raised surface"
                );
                assert_eq!(
                    border,
                    Some(crate::surface::edge_on(theme.tokens.colors.surfaces.raised)),
                    "{name} {style:?}: disabled face keeps the derived rest edge, no semantic fill"
                );
                let ratio = contrast_ratio(fg, bg);
                assert!(
                    ratio >= ON_FILL_MIN_CONTRAST,
                    "{name} {style:?}: disabled label is only {ratio:.2}:1 on {bg:?}"
                );
            }
            let (bg, border, _) = face_colors(&theme, ButtonRole::Plain, disabled);
            assert!(
                bg.components[3] <= 0.0,
                "{name}: a disabled plain button stays bare"
            );
            assert_eq!(border, None);
        }
    }

    /// Writes a small disabled-button matrix for human visual review. The
    /// controls intentionally carry hover/focus input too: disabled state
    /// must win over stale pointer/focus state and never recover a semantic
    /// action colour or focus ring.
    #[ignore = "developer visual audit writes /tmp/tilo-button-disabled-audit"]
    fn disabled_button_visual_audit() {
        use std::path::Path;

        let output = Path::new("/tmp/tilo-button-disabled-audit");
        std::fs::create_dir_all(output).expect("visual-audit output directory");
        for (name, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("high-contrast", Theme::high_contrast()),
        ] {
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let faces = [
                (ButtonRole::Primary, "Primary", None),
                (ButtonRole::Secondary, "Secondary", None),
                (ButtonRole::Destructive, "Destructive", None),
                (ButtonRole::Primary, "", Some(crate::icons::Icon::Check)),
            ];
            for (index, (style, label, icon)) in faces.into_iter().enumerate() {
                let y = 88.0 + f64::from(u32::try_from(index).expect("four audit rows")) * 86.0;
                let mut face = ButtonFace::new(Rect::new(80.0, y, 360.0, y + 52.0), label)
                    .role(style)
                    .disabled(true)
                    .hovered(true)
                    .focused(true);
                if let Some(icon) = icon {
                    face = face.icon(icon).accessible_name("Confirm");
                }
                Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
            }
            let mut renderer = crate::offscreen::OffscreenRenderer::new(440, 480);
            renderer.render_png(
                &scene,
                theme.tokens.colors.surfaces.content,
                output.join(format!("{name}.png")),
            );
        }
    }

    fn with_enabled_false_is_reported_by_is_enabled() {
        let b = Button::new("id", "Go").with_enabled(false);
        assert!(!Widget::is_enabled(&b));
        let b2 = Button::new("id", "Go");
        assert!(Widget::is_enabled(&b2), "default is enabled");
    }

    fn disabled_button_rejects_input() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go")
                .on_click(move || fired.set(fired.get() + 1))
                .with_enabled(false)
        };

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        let device = winit::event::DeviceId::dummy();
        let mut at_node = crate::testing::Dispatch::new().layout(&tree, node);
        let mut focus = None;
        let mut handled = false;

        for ev in [
            WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ] {
            let sent = at_node.send(&mut b, &ev);
            handled |= sent.handled;
            focus = sent.focus_target.or(focus);
        }

        assert_eq!(fired.get(), 0, "disabled button must not fire on_click");
        assert!(focus.is_none(), "disabled button must not request focus");
        assert!(!handled, "disabled button must not swallow input");
    }

    disabled_faces_are_neutral_bordered_and_readable_in_every_theme();
    disabled_button_visual_audit();
    with_enabled_false_is_reported_by_is_enabled();
    disabled_button_rejects_input();
}

#[test]
fn button_face_builder_paints_fill_label_and_icon_only_faces() {
    fn button_face_builder_sets_style_and_state() {
        let f = ButtonFace::new(Rect::new(0.0, 0.0, 80.0, 32.0), "Send")
            .role(ButtonRole::Primary)
            .hovered(true)
            .control_size(ControlSize::Small);
        let theme = Theme::dark();
        let (bg, border, fg) = f.colors(&theme);
        assert_eq!(bg, theme.tokens.colors.intent.accent_hover);
        assert_eq!(border, None);
        // Resolves through `face_colors`, so it carries the same derived
        // label rather than a literal of its own.
        assert_eq!(
            fg,
            face_colors(
                &theme,
                ButtonRole::Primary,
                ButtonInteract {
                    hovered: true,
                    ..Default::default()
                }
            )
            .2,
        );
        assert!(crate::a11y::contrast_ratio(fg, bg) >= ON_FILL_MIN_CONTRAST);
        assert_eq!(f.spoken_name(), "Send", "visible label is the spoken name");
    }

    fn icon_only_face_spoken_name_falls_back_to_accessible_name() {
        let f = ButtonFace::new(Rect::new(0.0, 0.0, 32.0, 32.0), "")
            .icon(crate::icons::Icon::Plus)
            .accessible_name("Add page");
        assert_eq!(f.spoken_name(), "Add page");
    }

    fn paint_emits_fill_and_label_geometry() {
        use vello_encoding::DrawTag;
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut scene = Scene::new();
        Button::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &ButtonFace::new(Rect::new(10.0, 10.0, 110.0, 46.0), "Go").role(ButtonRole::Primary),
        );
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            fills > 1,
            "body fill + label glyphs ⇒ several COLOR tags, got {fills}"
        );
    }

    /// An icon-only face has an empty label key. Under the expanded
    /// pseudo-locale `i18n!("")` came back as `⟦⟧`, painting a bracket pair
    /// beside the glyph; the resolved label must stay empty.
    fn icon_only_face_paints_no_label_under_the_pseudo_locale() {
        use crate::i18n::{I18nManager, PseudoLocale};
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(0.0, 0.0, 40.0, 40.0);

        let mut icon_only = Scene::new();
        Button::paint(
            &mut icon_only,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &ButtonFace::new(rect, "").icon(crate::icons::Icon::X),
        );
        let icon_only_runs = glyph_origins(&icon_only).len();

        let mut labelled = Scene::new();
        Button::paint(
            &mut labelled,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &ButtonFace::new(rect, "Go").icon(crate::icons::Icon::X),
        );
        let labelled_runs = glyph_origins(&labelled).len();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);

        assert_eq!(icon_only_runs, 0, "an icon-only face paints no glyph run");
        assert!(
            labelled_runs > 0,
            "the labelled control still paints its text"
        );
    }

    /// The same defect in the accessibility tree: `⟦⟧` is not empty, so the
    /// icon-only button lost its tooltip fallback and was spoken as brackets.
    fn icon_only_button_speaks_its_tooltip_under_the_pseudo_locale() {
        use crate::i18n::{I18nManager, PseudoLocale};
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let button = Button::new("close", "")
            .with_icon(crate::icons::Icon::X)
            .tooltip("Close");
        let name = Widget::accessibility(&button).and_then(|node| node.name);
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        let name = name.expect("icon-only button with a tooltip has a spoken name");
        assert_ne!(
            name, "⟦⟧",
            "the spoken name must not be the empty pseudo-locale markers"
        );
        assert!(
            name.contains('C') && name.contains('s'),
            "the tooltip is what gets spoken, got {name:?}"
        );
    }

    /// A fixed face narrower than its label (a long German label in a
    /// half-width footer button) elides inside the face instead of
    /// painting across both edges.
    fn a_label_wider_than_its_face_stays_inside_it() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(100.0, 0.0, 220.0, 40.0);
        let label = "Standard wiederherstellen";
        let (natural, _) = text.measure_style(label, None, &theme.text(TextRole::Body));
        assert!(
            f64::from(natural) > rect.width(),
            "the fixture must overflow"
        );
        let mut scene = Scene::new();
        Button::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &ButtonFace::new(rect, label).control_size(ControlSize::Large),
        );
        let origins = glyph_origins(&scene);
        assert!(!origins.is_empty(), "the elided label still paints");
        let inset = f64::from(theme.tokens.spacing.s2);
        for [x, _] in origins {
            assert!(
                f64::from(x) >= rect.x0 + inset,
                "label starts inside the face: {x}"
            );
        }
    }

    button_face_builder_sets_style_and_state();
    icon_only_face_spoken_name_falls_back_to_accessible_name();
    paint_emits_fill_and_label_geometry();
    a_label_wider_than_its_face_stays_inside_it();
    icon_only_face_paints_no_label_under_the_pseudo_locale();
    icon_only_button_speaks_its_tooltip_under_the_pseudo_locale();
}

/// Glyph-run origins painted into `scene`, in paint order.
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
fn the_intrinsic_footprint_follows_the_label_and_never_moves_it() {
    /// A short label stands at the labelled minimum, twice the height.
    fn a_short_label_is_at_the_labelled_minimum() {
        let rect = measured_button_rect(Button::new("save", "Save"), 1.0);
        assert_eq!((rect.width(), rect.height()), (64.0, 32.0), "{rect:?}");
    }

    /// A long label widens the button to hold it, never elided while the
    /// parent has room.
    fn a_wide_label_widens_the_button_instead_of_overflowing_it() {
        let label = "Remove every remote connection";
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let (label_w, _) = text.measure_style(label, None, &theme.text(TextRole::Label));
        let expected = (f64::from(label_w) + 2.0 * f64::from(theme.tokens.spacing.s3)).ceil();
        let rect = measured_button_rect(Button::new("remove", label), 1.0);
        assert!(
            (rect.width() - expected).abs() < 1e-6,
            "{rect:?} vs {expected}"
        );
        assert_eq!(rect.height(), 32.0);
    }

    /// Hover, press, focus and disabled recolour the face; none of them may
    /// move it. The focus ring is stroked on the outline, not inset, so the
    /// label origin is identical in every state.
    fn interaction_states_do_not_move_the_face_or_its_label() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(10.0, 10.0, 130.0, 50.0);
        let states = [
            ("rest", ButtonInteract::default()),
            (
                "hovered",
                ButtonInteract {
                    hovered: true,
                    ..ButtonInteract::default()
                },
            ),
            (
                "pressed",
                ButtonInteract {
                    pressed: true,
                    ..ButtonInteract::default()
                },
            ),
            (
                "focused",
                ButtonInteract {
                    focused: true,
                    ..ButtonInteract::default()
                },
            ),
            (
                "disabled",
                ButtonInteract {
                    disabled: true,
                    ..ButtonInteract::default()
                },
            ),
        ];
        for style in [
            ButtonRole::Secondary,
            ButtonRole::Primary,
            ButtonRole::Destructive,
            ButtonRole::Plain,
        ] {
            let mut reference: Option<[f32; 2]> = None;
            for (name, interact) in states {
                let mut scene = Scene::new();
                let face = ButtonFace::new(rect, "Delete")
                    .role(style)
                    .hovered(interact.hovered)
                    .pressed(interact.pressed)
                    .focused(interact.focused)
                    .disabled(interact.disabled);
                Button::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
                let origins = glyph_origins(&scene);
                let origin = *origins.first().expect("label painted");
                match reference {
                    None => reference = Some(origin),
                    Some(expected) => {
                        assert_eq!(origin, expected, "{style:?}/{name}: the label moved")
                    }
                }
            }
        }
    }

    a_short_label_is_at_the_labelled_minimum();
    a_wide_label_widens_the_button_instead_of_overflowing_it();
    interaction_states_do_not_move_the_face_or_its_label();
}

#[test]
fn pointer_click_fires_the_callback() {
    fn cursor_shape_is_pointer_over_button() {
        let mut b = Button::new("id", "Go");
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        assert_eq!(
            Widget::cursor_shape(&b, Point::new(10.0, 10.0)),
            Some(winit::window::CursorIcon::Pointer),
            "pointer over the button requests the pointing-hand cursor",
        );
        assert_eq!(
            Widget::cursor_shape(&b, Point::new(500.0, 500.0)),
            None,
            "pointer well outside the button defers (no cursor claim)",
        );
    }

    fn on_click_callback_fires_on_release_over_button() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go").on_click(move || fired.set(fired.get() + 1))
        };

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        let device = winit::event::DeviceId::dummy();
        let mut at_node = crate::testing::Dispatch::new().layout(&tree, node);
        let mut dispatch = |b: &mut Button, ev: WindowEvent| {
            at_node.send(b, &ev);
        };

        dispatch(
            &mut b,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
        );
        dispatch(
            &mut b,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert_eq!(fired.get(), 0, "not fired until release");
        dispatch(
            &mut b,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(fired.get(), 1, "fired once on release over the button");
    }

    cursor_shape_is_pointer_over_button();
    on_click_callback_fires_on_release_over_button();
}

#[test]
fn accessibility_reports_role_name_state_and_icon_only_naming() {
    fn accessibility_reports_button_role_name_and_state() {
        use crate::a11y::Role;
        let b = Button::new("id", "Save");
        let node = Widget::accessibility(&b).expect("button is not presentational");
        assert_eq!(node.role, Role::Button);
        assert_eq!(node.name.as_deref(), Some("Save"));
        assert!(node.state.focusable);
        assert!(!node.state.focused);
        assert!(!node.state.disabled);
        assert_eq!(node.state.checked, None, "a button is not checkable");
    }

    fn accessibility_reflects_disabled_and_focus() {
        let mut b = Button::new("id", "Save").with_enabled(false);
        b.set_focus(true);
        let node = Widget::accessibility(&b).unwrap();
        assert!(node.state.disabled);
        assert!(node.state.focused);
    }

    fn accessibility_icon_only_button_without_name_is_silent() {
        let b = Button::new("id", "").with_icon(crate::icons::Icon::Check);
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name, None);
    }

    fn accessibility_icon_only_button_uses_accessible_name() {
        let b = Button::new("id", "")
            .with_icon(crate::icons::Icon::Check)
            .with_accessible_name("Confirm");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Confirm"));
    }

    fn accessibility_visible_label_wins_over_accessible_name() {
        let b = Button::new("id", "Save").with_accessible_name("Persist");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Save"));
    }

    accessibility_reports_button_role_name_and_state();
    accessibility_reflects_disabled_and_focus();
    accessibility_icon_only_button_without_name_is_silent();
    accessibility_icon_only_button_uses_accessible_name();
    accessibility_visible_label_wins_over_accessible_name();
}

#[test]
fn tooltip_naming_precedence_and_compact_toolbar_semantics() {
    fn tooltip_is_exposed_to_the_host() {
        let b = Button::new("id", "")
            .with_icon(crate::icons::Icon::Search)
            .tooltip("Search");
        assert_eq!(Widget::tooltip(&b), Some("Search"));
        let plain = Button::new("id", "Save");
        assert_eq!(Widget::tooltip(&plain), None, "no tooltip unless attached");
    }

    fn compact_toolbar_presentation_round_trips_authored_semantics() {
        let mut button = Button::new("publish", "Publish")
            .with_icon(crate::icons::Icon::CloudDownload)
            .with_accessible_name("Publish document")
            .tooltip("Publish this document now");

        button.set_compact_toolbar_presentation(true);
        assert!(button.label.is_empty(), "only the icon should be painted");
        assert_eq!(
            Widget::accessibility(&button).unwrap().name.as_deref(),
            Some("Publish document"),
            "compaction never makes an icon-only control silent"
        );
        assert_eq!(
            Widget::tooltip(&button),
            Some("Publish this document now"),
            "the authored tooltip survives compaction"
        );

        button.set_compact_toolbar_presentation(false);
        assert_eq!(button.label, "Publish");
        assert_eq!(
            button.accessible_name.as_deref(),
            Some("Publish document"),
            "expansion restores the authored accessible name rather than retaining a fallback"
        );
        assert_eq!(
            Widget::accessibility(&button).unwrap().name.as_deref(),
            Some("Publish"),
            "the visible label again takes precedence after expansion"
        );
        assert_eq!(Widget::tooltip(&button), Some("Publish this document now"));
    }

    fn icon_only_tooltip_doubles_as_accessible_name() {
        let b = Button::new("id", "")
            .with_icon(crate::icons::Icon::Search)
            .tooltip("Search");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Search"));
    }

    fn explicit_accessible_name_wins_over_tooltip() {
        let b = Button::new("id", "")
            .with_icon(crate::icons::Icon::MoreHorizontal)
            .with_accessible_name("View options")
            .tooltip("More");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("View options"));
    }

    fn icon_only_effect_publishes_its_spoken_name() {
        let doc = AutoCommit::new();
        let named = Button::new("private", "")
            .with_icon(crate::icons::Icon::Shield)
            .with_accessible_name("New Private Window")
            .on_effect(|| {});
        let tooltip_after = Button::new("search", "")
            .with_icon(crate::icons::Icon::Search)
            .on_effect(|| {})
            .tooltip("Search");
        for (button, name) in [(named, "New Private Window"), (tooltip_after, "Search")] {
            let resources = crate::resources::resources(&button, &doc);
            assert_eq!(resources.len(), 1);
            assert_eq!(resources[0].label, name);
            assert!(
                resources[0]
                    .actions
                    .iter()
                    .all(|action| action.label == name)
            );
        }
    }

    tooltip_is_exposed_to_the_host();
    compact_toolbar_presentation_round_trips_authored_semantics();
    icon_only_tooltip_doubles_as_accessible_name();
    explicit_accessible_name_wins_over_tooltip();
    icon_only_effect_publishes_its_spoken_name();
}

#[test]
fn keys_plain_ink_and_bound_state() {
    fn a_key_rests_on_a_faint_lift_with_a_title_label() {
        let theme = Theme::dark();
        let ground = theme.tokens.colors.surfaces.content;
        let (rest, edge, _) = face_colors(&theme, ButtonRole::Key, ButtonInteract::default());
        assert_ne!(rest, ground, "a key is lifted off its ground");
        assert!(edge.is_none());
        let (hover, _, _) = face_colors(
            &theme,
            ButtonRole::Key,
            ButtonInteract {
                hovered: true,
                ..ButtonInteract::default()
            },
        );
        assert_ne!(hover, rest);
        let metrics = FaceMetrics::of_role(ControlSize::Regular, ButtonRole::Key, &theme);
        assert_eq!(metrics.label_style, theme.text(TextRole::Title));
    }

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
        button.metrics = FaceMetrics::of(ControlSize::Regular, &theme);
        button.apply_layout_external(plain);
        let hit = button.hit_rect();
        assert!(
            hit.x0 < plain.x0 && hit.x1 > plain.x1,
            "the hit area reaches out"
        );
    }

    fn closed_glyphs_fill_when_on() {
        assert!(fills_when_on(crate::icons::Icon::Star));
        assert!(fills_when_on(crate::icons::Icon::Pin));
        assert!(!fills_when_on(crate::icons::Icon::Refresh));
    }

    fn icons_grow_with_the_text_scale() {
        let one = FaceMetrics::of(ControlSize::Regular, &Theme::dark());
        let two = FaceMetrics::of(ControlSize::Regular, &Theme::dark().with_text_scale(2.0));
        assert!(two.icon > one.icon);
        assert!(two.icon <= two.height);
    }

    fn enabled_follows_its_state_and_bounds_are_spoken() {
        let can_save = crate::LocalState::new(false);
        let mut button = Button::new("save", "Save").enabled_from(&can_save);
        assert!(!Widget::is_enabled(&button));
        can_save.set(true);
        Widget::sync(&mut button, &AutoCommit::new(), &automerge::ROOT);
        assert!(Widget::is_enabled(&button));
        button.apply_layout_external(Rect::new(4.0, 8.0, 84.0, 40.0));
        let node = Widget::accessibility(&button).expect("semantics");
        assert!(node.bounds.is_some());
    }

    a_key_rests_on_a_faint_lift_with_a_title_label();
    a_plain_label_sits_on_its_layout_edge();
    closed_glyphs_fill_when_on();
    icons_grow_with_the_text_scale();
    enabled_follows_its_state_and_bounds_are_spoken();
}
