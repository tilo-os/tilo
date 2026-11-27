use super::*;
use automerge::AutoCommit;
use tilo_ui_base::tokens::TextRole;

use tilo_ui_base::widget::Widget;

/// An effect on the person's own device runs for a guest (an Editor) and
/// even a Viewer without asking anyone; a Viewer's click on one reaching
/// outside is refused, and the window says so. No click is dropped
/// unseen. (A framework app's ask is shown too: API-tester's
/// `a_guests_send_is_asked_in_the_window_saying_who_decides`.)

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

    intrinsic_width_stays_compact_in_intrinsic_grid_column();
    compact_fit_width_preserves_host_center_and_end_alignment();
    explicit_fill_width_still_stretches_in_intrinsic_column();
}

tilo_test_env::scenario! {
    /// Button faces: theme switches, role colours and the destructive confirmation.
    /// Each phase was a test of its own and builds its own fixture.
    fn button_faces_follow_role_state_and_theme;


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
            // The hovered plate is the state a person sees the control by, so it is a step of
            // the ground that clears the non-text contrast floor.
            let plate = face_colors(&theme, ButtonRole::Plain, hover).0;
            assert!(
                contrast_ratio(plate, colors.surfaces.content)
                    >= 3.0 - 0.05,
                "{}: a plain control's hover plate is a {:.2}:1 step",
                theme.name,
                contrast_ratio(plate, colors.surfaces.content)
            );
            // The glyph is resolved against the resting ground and stays legible on the plate,
            // which is a lift of it: a dark theme keeps its light glyph instead of collapsing to
            // black on the dark ground.
            assert!(
                contrast_ratio(ink, plate) >= 3.0 - 0.05,
                "{}: a plain control's glyph on its plate is {:.2}:1",
                theme.name,
                contrast_ratio(ink, plate)
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
                    Some(tilo_ui_base::surface::edge_on(
                        theme.tokens.colors.surfaces.raised
                    )),
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
    with_enabled_false_is_reported_by_is_enabled();
    disabled_button_rejects_input();
}

#[test]
fn button_face_builder_paints_fill_label_and_icon_only_faces() {
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


        tooltip_is_exposed_to_the_host();
        compact_toolbar_presentation_round_trips_authored_semantics();
        icon_only_tooltip_doubles_as_accessible_name();
        explicit_accessible_name_wins_over_tooltip();
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

/// A press a rule opens after a wait (a slow player's turn running out) stands dimmed with the
/// time left on its face, counts down each second, and opens by itself when the moment comes,
/// enabled as the view asked; a button the view disabled stays disabled once it opens.
#[test]
fn a_button_that_opens_later_counts_down_and_opens_by_itself() {
    let now = crate::time::unix_now_i64();
    let mut button = Button::new("skip", "Fold for the slow player").opening_at(now + 75);
    assert!(
        !button.enabled,
        "a button whose moment has not come cannot be pressed"
    );
    assert_eq!(button.detail(), Some("1:15"));
    assert!(
        button.next_wake_deadline().is_some(),
        "it wakes to count the next second"
    );

    assert!(button.follow_opening(now + 74));
    assert_eq!(button.detail(), Some("0:01"));
    assert!(!button.enabled);

    assert!(button.follow_opening(now + 75));
    assert!(button.enabled, "at its moment it opens");
    assert_eq!(button.detail(), None, "and the countdown leaves its face");
    assert!(
        button.next_wake_deadline().is_none(),
        "an open button wakes nothing"
    );
    assert!(
        !button.follow_opening(now + 80),
        "nothing changes after it opened"
    );

    let mut held = Button::new("deal", "Deal")
        .with_enabled(false)
        .opening_at(now + 5);
    held.follow_opening(now + 5);
    assert!(!held.enabled, "the view's own disabling outlasts the wait");

    let gone = Button::new("deal", "Deal").opening_at(now - 1);
    assert!(
        gone.enabled && gone.detail().is_none(),
        "a moment already past is an open button"
    );
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
