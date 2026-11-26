use super::*;
use tilo_ui_base::tokens::TextRole;
use automerge::AutoCommit;

use tilo_ui_base::widget::Widget;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// An effect on the person's own device runs for a guest (an Editor) and
/// even a Viewer without asking anyone; a Viewer's click on one reaching
/// outside is refused, and the window says so. No click is dropped
/// unseen. (A framework app's ask is shown too: API-tester's
/// `a_guests_send_is_asked_in_the_window_saying_who_decides`.)
#[test]
fn own_device_effects_run_for_guests_and_refusals_are_said() {
    use ::collaboration::permissions::{PermissionContext, Role};
    let click = |root: tilo_ui_base::widget::Element, role: Role| {
        let mut harness = crate::app::driver::Driver::new_with_theme(
            crate::State::new(),
            root,
            (600, 200),
            Theme::dark(),
        );
        // The window stands in its workspace, whose grants ask about what reaches outside.
        harness.connect_peer();
        harness.set_permission_ctx(PermissionContext::member(
            &tilo_action_policy::MemberId::person("ada"),
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
        crate::widget::effect_fixture::sends(counter()),
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
        crate::widget::effect_fixture::sends(counter()),
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
    let mut bindings = tilo_ui_base::widget::bound_nodes::BoundNodeMap::new();
    unsafe {
        bindings.rebuild(host, &tree, root, &map);
        tilo_ui_base::widget::compute_widget_layout(
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

fn fit_host(kind: usize, button: Button) -> tilo_ui_base::widget::Element {
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
        let mut harness = crate::app::driver::Driver::new_with_theme(
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
        assert_eq!(button.role, ButtonRole::Plain);
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
        assert_eq!(hit.center(), face.rect.center());
        assert!(face.contains(&theme, Point::new(8.0, 20.0)));
        assert!(!face.contains(&theme, Point::new(40.0, 20.0)));
    }

    intrinsic_width_holds_the_whole_label();
    each_control_size_has_its_height_and_grows_with_large_text();
    icon_only_buttons_are_square_named_and_plain();
    a_shortcut_hint_widens_the_button_and_paints_after_the_label();
    a_face_hit_rect_is_never_under_the_pointer_target();
}

fn measured_button_rect(button: Button, scale: f32) -> Rect {
    let harness = crate::app::driver::Driver::new_with_theme(
        crate::State::new(),
        Box::new(crate::widget::Row::new().push(Box::new(button))),
        (1000, 200),
        Theme::dark().with_text_scale(scale),
    );
    let rect = harness.inspection().root.children[0].rect;
    Rect::new(rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3])
}

#[test]
fn the_focus_ring_stands_apart_from_every_face() {
    use tilo_ui_base::offscreen::OffscreenRenderer;
    use tilo_ui_base::tokens::PresenceHue;
    use tilo_ui_base::tokens::colors::NON_TEXT_MIN_CONTRAST;
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
                let contrast = tilo_ui_base::a11y::contrast_ratio;
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

tilo_test_env::scenario! {
    /// Button faces: theme switches, role colours and the destructive confirmation.
    /// Each phase was a test of its own and builds its own fixture.
    fn button_faces_follow_role_state_and_theme;

    phase a_cached_face_follows_a_theme_switch;
    // A theme switched under a live tree repaints a plain button's face
    // in the new theme's roles, rather than keeping the cached dark-theme
    // ink that all but vanishes on a light ground.
    {
        let mut button = Button::new("plain", "Undo").role(ButtonRole::Plain);
        button.rect = Rect::new(0.0, 0.0, 80.0, 32.0);
        let presence = tilo_ui_base::presence::PresenceCtx::default();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        for theme in [Theme::dark(), Theme::light()] {
            button.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme, presence.draw_env()));
            assert_eq!(
                button.last_theme,
                Some(theme_key(&theme)),
                "the face is painted in {}",
                theme.name
            );
        }
    }

    phase face_colors_per_role_track_state_and_stay_readable;
    {
        use tilo_ui_base::a11y::contrast_ratio;
        let hover = Interaction {
            hovered: true,
            ..Default::default()
        };
        let press = Interaction {
            pressed: true,
            ..Default::default()
        };
        let rest = Interaction::default();
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
            let step = |a, b| tilo_ui_base::chrome::shell::lightness_step(a, b);
            assert!(
                step(press_fill, fill) > step(hover_fill, fill),
                "{}: press must move further from rest than hover",
                theme.name
            );
            assert_eq!(press_ink, ink);
            assert!(contrast_ratio(press_ink, press_fill) >= ON_FILL_MIN_CONTRAST);

            let (fill, edge, ink) = face_colors(&theme, ButtonRole::Secondary, rest);
            assert_eq!(fill, raised);
            // High contrast keeps every control's boundary: its face is its ground's black.
            if colors.palette.high_contrast {
                let edge = edge.expect("a high-contrast face keeps its boundary");
                assert!(contrast_ratio(edge, fill) >= 3.0, "{}", theme.name);
            } else {
                assert_eq!(edge, None, "{}", theme.name);
            }
            assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
            assert_eq!(
                face_colors(&theme, ButtonRole::Secondary, hover).0,
                colors.lift_on(raised, 0.5)
            );
            assert_eq!(
                face_colors(&theme, ButtonRole::Secondary, press).0,
                colors.lift_on(raised, 1.0)
            );

            for interact in [rest, hover, press] {
                let (fill, edge, ink) = face_colors(&theme, ButtonRole::Destructive, interact);
                let neutral = face_colors(&theme, ButtonRole::Secondary, interact).0;
                // High contrast keeps the black face, so its danger lettering alone marks it.
                if !colors.palette.high_contrast {
                    assert_ne!(fill, neutral, "{}: a destructive face wears a wash", theme.name);
                    let hue = |c| crate::color::Oklch::from_color(c).h;
                    let off = (hue(fill) - hue(colors.semantic.danger)).rem_euclid(360.0);
                    assert!(
                        off.min(360.0 - off) <= 20.0,
                        "{}: the wash is the danger hue",
                        theme.name
                    );
                }
                assert_eq!(edge, face_colors(&theme, ButtonRole::Secondary, interact).1);
                assert!(
                    contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST,
                    "{}: danger ink on the danger wash",
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

    phase only_the_confirmation_step_fills_a_destructive_button;
    // The filled danger face exists only in a confirmation step.
    {
        use tilo_ui_base::a11y::contrast_ratio;
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
}

#[test]
fn disabled_faces_are_neutral_and_reject_input() {
    /// The edge of a disabled face is drawn under the same group alpha as its face, so the
    /// button reads as itself, disabled, not as an outlined button of another style.
    fn a_disabled_face_dims_its_edge_with_it() {
        use vello_encoding::DrawTag;
        let mut tr = TextRenderer::new();
        let theme = Theme::light();
        let mut scene = Scene::new();
        Button::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &ButtonFace::new(Rect::new(10.0, 10.0, 110.0, 46.0), "Archived").disabled(true),
        );
        let tags = &scene.encoding().draw_tags;
        assert!(
            tags.last() == Some(&DrawTag::END_CLIP),
            "the dimming layer closes over everything the face draws"
        );
    }

    fn disabled_faces_are_neutral_bordered_and_readable_in_every_theme() {
        use tilo_ui_base::a11y::contrast_ratio;

        let disabled = Interaction {
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
                    Some(tilo_ui_base::surface::edge_on(theme.tokens.colors.surfaces.raised)),
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

        assert_eq!(fired.get(), 0, "disabled button must not fire its press");
        assert!(focus.is_none(), "disabled button must not request focus");
        assert!(!handled, "disabled button must not swallow input");
    }

    disabled_faces_are_neutral_bordered_and_readable_in_every_theme();
    a_disabled_face_dims_its_edge_with_it();
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
                Interaction {
                    hovered: true,
                    ..Default::default()
                }
            )
            .2,
        );
        assert!(tilo_ui_base::a11y::contrast_ratio(fg, bg) >= ON_FILL_MIN_CONTRAST);
        assert_eq!(f.spoken_name(), "Send", "visible label is the spoken name");
    }

    fn icon_only_face_spoken_name_falls_back_to_accessible_name() {
        let f = ButtonFace::new(Rect::new(0.0, 0.0, 32.0, 32.0), "")
            .icon(tilo_ui_base::icons::Icon::Plus)
            .accessible_name("Add page");
        assert_eq!(f.spoken_name(), "Add page");
    }

    /// An icon-only face has an empty label key. Under the expanded
    /// pseudo-locale `widget_text("")` came back as `⟦⟧`, painting a bracket pair
    /// beside the glyph; the resolved label must stay empty.
    fn icon_only_face_paints_no_label_under_the_pseudo_locale() {
        use tilo_ui_base::{i18n::I18nManager, i18n::PseudoLocale};
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
            &ButtonFace::new(rect, "").icon(tilo_ui_base::icons::Icon::X),
        );
        let icon_only_runs = glyph_origins(&icon_only).len();

        let mut labelled = Scene::new();
        Button::paint(
            &mut labelled,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &ButtonFace::new(rect, "Go").icon(tilo_ui_base::icons::Icon::X),
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
        use tilo_ui_base::{i18n::I18nManager, i18n::PseudoLocale};
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let button = Button::new("close", "")
            .with_icon(tilo_ui_base::icons::Icon::X)
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

tilo_test_env::scenario! {
    /// Activation, availability, accessibility and naming.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_button_acts_and_is_announced;

    phase a_button_rebuilt_after_a_click_shows_no_focus_ring;
    // An app rebuilds its buttons from state, and the host then refocuses
    // the new one: it never saw the press. The ring still follows the
    // input that focused it, so a clicked button stays ringless.
    {
        tilo_ui_base::focus_visible::note(&tilo_ui_base::synthetic_input::mouse_press());
        let mut rebuilt = Button::new("wp.bold", "Bold");
        rebuilt.set_focus(true);
        assert!(!rebuilt.focus_visible(), "a click never rings the button");
        tilo_ui_base::focus_visible::note(&tilo_ui_base::synthetic_input::keyboard_input(
            tilo_ui_base::synthetic_input::key_event(
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                None,
                winit::event::ElementState::Pressed,
            ),
        ));
        assert!(rebuilt.focus_visible(), "the keyboard brings the ring back");
    }

    // A button inside an ordinary container answers the press a screen reader sends it by name,
    // the route that reaches it through every container it stands in.
    phase a_shared_button_answers_through_an_ordinary_container;
    {
        let clicks = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&clicks);
        let column = crate::widget::Column::new().push(Box::new(
            Button::new("files.retry", "Retry").on_open(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        ));
        let mut harness =
            crate::app::driver::Driver::new(crate::State::new(), Box::new(column), (400, 120));
        harness
            .perform_named("files.retry", &tilo_ui_base::a11y::Action::Activate)
            .expect("the press reaches the button through its container");

        assert_eq!(clicks.load(Ordering::SeqCst), 1);
    }

    phase pointer_click_fires_the_callback;
    {
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
            assert_eq!(fired.get(), 1);
        }

        cursor_shape_is_pointer_over_button();
        on_click_callback_fires_on_release_over_button();
    }

    phase accessibility_reports_role_name_state_and_icon_only_naming;
    {
        fn accessibility_reports_button_role_name_and_state() {
            use tilo_ui_base::a11y::Role;
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
            let b = Button::new("id", "").with_icon(tilo_ui_base::icons::Icon::Check);
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name, None);
        }

        fn accessibility_icon_only_button_uses_accessible_name() {
            let b = Button::new("id", "")
                .with_icon(tilo_ui_base::icons::Icon::Check)
                .with_accessible_name("Confirm");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Confirm"));
        }

        fn accessibility_stated_name_wins_over_the_visible_label() {
            let b = Button::new("id", "Save").with_accessible_name("Persist");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Persist"));
        }

        accessibility_reports_button_role_name_and_state();
        accessibility_reflects_disabled_and_focus();
        accessibility_icon_only_button_without_name_is_silent();
        accessibility_icon_only_button_uses_accessible_name();
        accessibility_stated_name_wins_over_the_visible_label();
    }

    phase tooltip_naming_precedence_and_compact_toolbar_semantics;
    {
        fn tooltip_is_exposed_to_the_host() {
            let b = Button::new("id", "")
                .with_icon(tilo_ui_base::icons::Icon::Search)
                .tooltip("Search");
            assert_eq!(Widget::tooltip(&b).as_deref(), Some("Search"));
            let plain = Button::new("id", "Save");
            assert_eq!(Widget::tooltip(&plain), None, "no tooltip unless attached");
        }

        fn compact_toolbar_presentation_round_trips_authored_semantics() {
            let mut button = Button::new("publish", "Publish")
                .with_icon(tilo_ui_base::icons::Icon::CloudDownload)
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
                Widget::tooltip(&button).as_deref(),
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
                Some("Publish document"),
                "the name the app stated still says it after expansion"
            );
            assert_eq!(
                Widget::tooltip(&button).as_deref(),
                Some("Publish this document now")
            );
        }

        fn icon_only_tooltip_doubles_as_accessible_name() {
            let b = Button::new("id", "")
                .with_icon(tilo_ui_base::icons::Icon::Search)
                .tooltip("Search");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("Search"));
        }

        fn explicit_accessible_name_wins_over_tooltip() {
            let b = Button::new("id", "")
                .with_icon(tilo_ui_base::icons::Icon::MoreHorizontal)
                .with_accessible_name("View options")
                .tooltip("More");
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(node.name.as_deref(), Some("View options"));
        }

        fn a_compacted_faced_button_shows_one_face_and_keeps_it_under_new_words() {
            let mut text = TextRenderer::new();
            let faces = || ["Ada".to_owned(), "Bo".to_owned()];
            let mut button = Button::new("mark", "2 changes by Ada and Bo").with_faces(faces());
            let full = button.intrinsic_width(&mut text);
            button.set_compact_toolbar_presentation(true);
            let compact = button.intrinsic_width(&mut text);
            assert!(
                compact * 2.0 < full,
                "one face stands where the names stood: {compact} of {full}"
            );
            assert!(
                button.label.is_empty(),
                "the row gave up the words for the face"
            );
            // A change arrives while the row is short: the mark takes the new words for when the row
            // gives it width again, and stays one face where it stands now.
            let fresh = Button::new("mark", "3 changes by Ada and Bo").with_faces(faces());
            assert!(
                button.take_words(&fresh),
                "the mark keeps its place and its face"
            );
            assert!(
                button.label.is_empty(),
                "the compact pill shows its face, not the new words"
            );
            assert_eq!(
                button.intrinsic_width(&mut text),
                compact,
                "the new words do not widen the pill the row made compact"
            );
            button.set_compact_toolbar_presentation(false);
            assert_eq!(
                button.label, "3 changes by Ada and Bo",
                "the area back gives the mark its newest words"
            );
        }

        fn icon_only_effect_publishes_its_spoken_name() {
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
                let harness = crate::app::driver::Driver::new(
                    crate::State::new(),
                    Box::new(crate::widget::Row::new().push(Box::new(button))),
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

        tooltip_is_exposed_to_the_host();
        compact_toolbar_presentation_round_trips_authored_semantics();
        icon_only_tooltip_doubles_as_accessible_name();
        explicit_accessible_name_wins_over_tooltip();
        icon_only_effect_publishes_its_spoken_name();
        a_compacted_faced_button_shows_one_face_and_keeps_it_under_new_words();
    }

    phase keys_plain_ink_and_bound_state;
    {
        fn a_key_rests_on_a_faint_lift_with_a_title_label() {
            let theme = Theme::dark();
            let ground = theme.tokens.colors.surfaces.content;
            let (rest, edge, _) = face_colors(&theme, ButtonRole::Key, Interaction::default());
            assert_ne!(rest, ground, "a key is lifted off its ground");
            assert!(edge.is_none());
            let (hover, _, _) = face_colors(
                &theme,
                ButtonRole::Key,
                Interaction {
                    hovered: true,
                    ..Interaction::default()
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
            assert!(fills_when_on(tilo_ui_base::icons::Icon::Star));
            assert!(fills_when_on(tilo_ui_base::icons::Icon::Pin));
            assert!(!fills_when_on(tilo_ui_base::icons::Icon::Refresh));
        }

        fn icons_grow_with_the_text_scale() {
            let one = FaceMetrics::of(ControlSize::Regular, &Theme::dark());
            let two = FaceMetrics::of(ControlSize::Regular, &Theme::dark().with_text_scale(2.0));
            assert!(two.icon > one.icon);
            assert!(two.icon <= two.height);
        }

        fn enabled_follows_its_state_and_bounds_are_spoken() {
            let can_save = tilo_ui_base::LocalState::new(false);
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

    phase a_button_named_beyond_its_label_is_announced_with_that_name;
    // A button's visible label may say less than its name: the charge reads "61%" beside the
    // charging glyph, and assistive technology hears the charging too.
    {
        use tilo_ui_base::widget::Widget as _;
        let named = Button::new("charge", "61%").with_accessible_name("61%, charging");
        let heard = named.accessibility().expect("a button is announced");
        assert_eq!(heard.name.as_deref(), Some("61%, charging"));
        let shown = Button::new("charge", "61%");
        assert_eq!(shown.accessibility().unwrap().name.as_deref(), Some("61%"));
        let glyph = Button::icon_only("charge", tilo_ui_base::icons::Icon::Battery, "61%, charging");
        assert_eq!(
            glyph.accessibility().unwrap().name.as_deref(),
            Some("61%, charging"),
            "an icon-only button keeps the name its label only states"
        );
    }
}

tilo_test_env::scenario! {
    /// Button width and height: chevrons, intrinsic size, host placement, faces and counts.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_button_measures_from_its_label_and_control_size;

    phase a_menu_button_makes_way_for_its_chevron;
    // A button that opens a menu keeps a chevron after the
    // label, so the label is never elided to fit it; an icon-only one
    // stays square.
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let metrics = FaceMetrics::of_role(ControlSize::Small, ButtonRole::Secondary, &theme);
        let width = |label: &str, menu: bool, text: &mut TextRenderer| {
            FaceContent {
                icon: None,
                image: None,
                faces: &[],
                avatar: None,
                placeholder: false,
                tone: None,
                badge: None,
                label,
                detail: None,
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

    phase a_host_placed_button_never_asks_the_solver_for_a_measure;
    // Only a node the layout solver placed can be measured again: a button
    // its host places never asks for a measure, placed or hidden, while one
    // in the solver's tree asks when what it shows may change its width.
    {
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
            tilo_ui_base::interaction::InteractionEnvironment::default().with_touch_modality(),
        );
        assert!(
            laid_out.layout_needs_measure(),
            "a touch target may be wider"
        );
    }

    phase the_intrinsic_footprint_follows_the_label_and_never_moves_it;
    {
        fn a_short_label_is_at_the_labelled_minimum() {
            let rect = measured_button_rect(Button::new("save", "Save"), 1.0);
            assert_eq!((rect.width(), rect.height()), (64.0, 32.0), "{rect:?}");
        }

        /// A long label widens the button to hold it, never elided while the
        /// parent fits.
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
                ("rest", Interaction::default()),
                (
                    "hovered",
                    Interaction {
                        hovered: true,
                        ..Interaction::default()
                    },
                ),
                (
                    "pressed",
                    Interaction {
                        pressed: true,
                        ..Interaction::default()
                    },
                ),
                (
                    "focused",
                    Interaction {
                        focused: true,
                        ..Interaction::default()
                    },
                ),
                (
                    "disabled",
                    Interaction {
                        disabled: true,
                        ..Interaction::default()
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

    phase a_faces_button_is_as_wide_as_the_faces_it_shows;
    // A button of who is here is exactly as wide as the faces it shows: no label behind them at
    // any width, one face wider than a square for one person, and a fourth person counted in a
    // face of its own rather than a fifth face. The label names the people, never the glyph.
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let button = |people: &[&str]| {
            let mut button = Button::new("people", "")
                .with_accessible_name("Mia Clayton and others are here")
                .with_faces(people.iter().map(|name| (*name).to_owned()))
                .role(ButtonRole::Bar);
            button.metrics = FaceMetrics::of_role(ControlSize::Regular, ButtonRole::Bar, &theme);
            button
        };
        let width =
            |people: &[&str], text: &mut TextRenderer| f64::from(button(people).intrinsic_width(text));
        let stack = tilo_ui_base::presence::FaceStack::in_face(button(&[]).face_height());
        let pad = 2.0 * button(&[]).metrics.pad_x;
        let one = width(&["Mia Clayton"], &mut text);
        let three = width(&["Mia Clayton", "Sam Okafor", "Ana Ruiz"], &mut text);
        let four = width(
            &["Mia Clayton", "Sam Okafor", "Ana Ruiz", "Ben Ito"],
            &mut text,
        );
        assert!(
            (one - (stack.group_width(1) + pad)).abs() <= 1.0,
            "one face and the pad: {one}"
        );
        assert!(three > one, "three faces are wider than one: {three}");
        assert!(
            (four - (stack.group_width(4) + pad)).abs() <= 1.0,
            "the fourth person is a counted face of its own: {four} vs {}",
            stack.group_width(4) + pad
        );
        assert!(four > three, "counting the rest takes a slot: {four}");
        assert_eq!(stack.group(4), (3, 1), "three faces, then +1");
        assert!(
            four < 4.0 * button(&[]).face_height(),
            "still a bar control"
        );

        let node = Widget::accessibility(&button(&["Mia Clayton", "Sam Okafor"]))
            .expect("a group of faces is announced");
        assert_eq!(
            node.name.as_deref(),
            Some("Mia Clayton and others are here"),
            "the name is the people, in words"
        );
    }

    phase a_count_stands_inside_the_face_and_leaves_its_parents_margins_alone;
    // A node's margins are its parent's (a layout puts its gaps there), and a count stands
    // inside the button's face: a button with a count leaves its parent's margins as they are,
    // and a second layout pass finds nothing to change, so the layout settles. The count itself
    // never leaves the face on any side, so no edge that cuts the control can cut it.
    {
        let theme = Theme::light();
        let mut text = TextRenderer::new();
        let parents = taffy::Rect {
            left: taffy::LengthPercentageAuto::length(8.0),
            right: taffy::LengthPercentageAuto::length(8.0),
            top: taffy::LengthPercentageAuto::length(4.0),
            bottom: taffy::LengthPercentageAuto::length(4.0),
        };
        let laid = |mut button: Button, text: &mut TextRenderer| {
            let mut tree = TaffyTree::new();
            let mut map = std::collections::HashMap::new();
            let node = button.build_layout(&mut tree, &theme, &mut map);
            let mut style = tree.style(node).unwrap().clone();
            style.margin = parents;
            tree.set_style(node, style).unwrap();
            button.adjust_layout_style(&mut tree, node, text);
            let first = tree.style(node).unwrap().margin;
            let changed = button.adjust_layout_style(&mut tree, node, text);
            (first, changed, tree.style(node).unwrap().margin)
        };

        let counted = || {
            Button::icon_only("bell", tilo_ui_base::icons::Icon::Bell, "Notifications, 9 unread")
                .role(ButtonRole::Bar)
                .with_count(
                    crate::widget::BadgeContent::Count(9),
                    crate::widget::ChipStyle::Live,
                )
        };
        let (first, changed, after) = laid(counted(), &mut text);
        assert_eq!(first, parents, "a count leaves its parent's margins alone");
        assert!(
            !changed && after == parents,
            "and finds nothing to change again"
        );

        let mut button = counted();
        let mut tree = TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let node = button.build_layout(&mut tree, &theme, &mut map);
        button.adjust_layout_style(&mut tree, node, &mut text);
        button.apply_layout_external(Rect::new(100.0, 0.0, 128.0, 28.0));
        let badge = button.count_rect().expect("a count where there is unread");
        let icon = button.metrics.icon.min(button.rect.height());
        assert!(
            (badge.center().x - (button.rect.center().x + icon / 2.0)).abs() < 0.5,
            "across, on the icon's corner where the face holds it: {badge:?} in {:?}",
            button.rect
        );
        assert_eq!(
            Widget::paint_bounds(&button),
            button.rect.union(badge),
            "the count is painted, not culled"
        );
        let held = |badge: Rect, face: Rect| {
            badge.x0 >= face.x0
                && badge.x1 <= face.x1
                && badge.y0 >= face.y0
                && badge.y1 <= face.y1
        };
        assert!(
            held(badge, button.rect),
            "inside the face on every side: {badge:?} in {:?}",
            button.rect
        );
        // A face no wider than its icon cannot hold the count on the corner's outer half, so
        // the count gives up the corner rather than leave the face.
        button.apply_layout_external(Rect::new(100.0, 0.0, 116.0, 28.0));
        let badge = button.count_rect().expect("a count where there is unread");
        assert!(
            badge.center().x < button.rect.center().x + icon / 2.0,
            "past the corner's outer half, the face holds the count back: {badge:?} in {:?}",
            button.rect
        );
        assert!(
            held(badge, button.rect),
            "inside a face no wider than its icon: {badge:?} in {:?}",
            button.rect
        );
    }

    phase a_button_the_solver_placed_answers_its_measure;
    // A control the solver has placed answers its measure there, not in a draw: a control that is
    // laid out and never drawn (a shut overflow API's button among a row's controls) finds no draw
    // to answer it, so one switch of input would keep it — and its path to the root — asking to be
    // measured on every frame after.
    {
        let mut harness = crate::app::driver::Driver::new(
            crate::State::new(),
            Box::new(crate::widget::Row::new().push(Box::new(Button::icon_only(
                "pin",
                tilo_ui_base::icons::Icon::Pin,
                "Pin",
            )))),
            (400, 100),
        );
        harness.relayout();
        let keyboard = harness.interaction_environment().with_keyboard_modality();
        harness.set_interaction_environment(keyboard);
        assert!(
            harness.last_relayout().measured,
            "the switch lays the window out once: {:?}",
            harness.last_relayout()
        );
        harness.relayout();
        assert_eq!(
            harness.last_relayout().marked_leaves,
            0,
            "a control the solver placed asks for no second measure: {:?}",
            harness.last_relayout()
        );
    }
}

#[test]
fn a_persons_face_leads_a_chip_a_step_larger_than_an_icon() {
    let theme = Theme::dark();
    let ada = tilo_ui_base::component::Person::new(tilo_actions::MemberId::person("ada"), "Ada");
    let with_icon = measured_button_rect(
        Button::new("owner", "Ada").with_icon(tilo_ui_base::icons::Icon::User),
        1.0,
    );
    let with_face = measured_button_rect(Button::new("owner", "Ada").with_avatar(ada), 1.0);
    let step = f64::from(theme.tokens.metrics.icon_md - theme.tokens.metrics.icon_control);
    assert!(step > 0.0);
    assert!(
        (with_face.width() - with_icon.width() - step).abs() < 0.5,
        "the face is {step} px wider than an icon: {with_face:?} against {with_icon:?}"
    );
}

#[test]
fn a_view_declared_key_shows_as_the_buttons_shortcut_hint() {
    use super::component::ButtonProps;
    use tilo_ui_base::component::Described;
    let button = Button::from_props(
        "go",
        ButtonProps {
            label: "Run".into(),
            key: Some("Ctrl+Enter".into()),
            ..ButtonProps::default()
        },
    );
    assert_eq!(
        button.declared_key(),
        Some(tilo_ui_base::platform::Shortcut::from("Ctrl+Enter")),
        "the prop the view set is the hint the face draws"
    );
    let none = Button::from_props(
        "go",
        ButtonProps {
            label: "Run".into(),
            key: Some(String::new()),
            ..ButtonProps::default()
        },
    );
    assert_eq!(
        none.declared_key(),
        None,
        "a key the view computed to nothing draws no hint"
    );
}

#[test]
fn an_icon_only_button_without_words_is_named_by_its_accessible_name() {
    use super::component::ButtonProps;
    use tilo_ui_base::component::Described;
    let button = Button::from_props(
        "sync",
        ButtonProps {
            label: String::new(),
            icon: Some(tilo_ui_base::icons::Icon::Plus),
            icon_only: true,
            accessible_name: Some("Sync calendars".into()),
            ..ButtonProps::default()
        },
    );
    assert_eq!(Widget::tooltip(&button).as_deref(), Some("Sync calendars"));
    assert_eq!(
        Widget::accessibility(&button).unwrap().name.as_deref(),
        Some("Sync calendars")
    );
}
