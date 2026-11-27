//! The sheet's geometry, the two guarantees that made the shared chrome
//! exist (there is always a visible way out, and every label is legible),
//! and the retained dialog's input, accessibility and top-layer contract.
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::internals::modal_dialog::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

use std::cell::Cell;
use std::rc::Rc;
use tilo_ui_app::widget::Text;
use tilo_ui_base::a11y::contrast_ratio;
use vello_encoding::DrawTag;

const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1600.0, 1000.0);
const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

/// How the person reaches the window in these tests: the pointer, unless a
/// test sets another.
fn environment() -> tilo_ui_base::interaction::InteractionEnvironment {
    tilo_ui_base::interaction::InteractionEnvironment::default()
}

fn themes() -> Vec<(&'static str, Theme)> {
    vec![("dark", Theme::dark()), ("light", Theme::light())]
}

fn lay(spec: &ModalSpec<'_>, theme: &Theme) -> ModalLayout {
    let mut tr = TextRenderer::new();
    layout_for(VIEWPORT, spec, &mut tr, theme, environment())
}

fn cursor_at(p: Point) -> WindowEvent {
    tilo_ui_base::synthetic_input::cursor_moved(p.x, p.y)
}

use tilo_ui_base::synthetic_input::mouse_press;

use tilo_ui_base::synthetic_input::named_key;

use taffy::{Size, Style};
use tilo_ui_app::testing::dispatch;
use tilo_ui_app::testing::{Seen, TestLeaf};
use tilo_ui_base::platform::{Platform, PlatformConventions};
use tilo_ui_base::time::testing::with_monotonic_time;

fn lay_out<C: Widget + 'static>(d: &mut ModalDialog<C>, area: Rect, theme: &Theme) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = d.build_layout(&mut taffy, theme, &mut node_map);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let mut bindings = tilo_ui_base::widget::bound_nodes::BoundNodeMap::default();
    #[allow(clippy::cast_possible_truncation, reason = "test geometry")]
    let available = Size {
        width: taffy::AvailableSpace::Definite(area.width() as f32),
        height: taffy::AvailableSpace::Definite(area.height() as f32),
    };
    // SAFETY: `node_map` describes the tree just built from `d`, which this
    // function holds exclusively.
    unsafe {
        bindings.rebuild(d, &taffy, node, &node_map);
        tilo_ui_base::widget::layout::compute_widget_layout(
            &mut taffy, node, available, &node_map, &bindings, &mut text,
        )
        .unwrap();
    }
    d.apply_layout(&taffy, node, area.origin());
}

fn present<C: Widget + 'static>(d: &mut ModalDialog<C>, window: Rect, theme: &Theme) -> Scene {
    let presence = tilo_ui_base::presence::PresenceCtx::default();
    let previous = presence.push_viewport(window);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    d.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        presence.draw_env(),
    ));
    d.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        presence.draw_env(),
    ));
    presence.pop_viewport(previous);
    scene
}

fn laid_out_counting(open: bool) -> (ModalDialog<TestLeaf>, Seen) {
    let content = TestLeaf::sized(200.0, 40.0);
    let events = content.seen();
    let mut d = ModalDialog::new("Title", content).open(open);
    lay_out(&mut d, WINDOW, &Theme::dark());
    (d, events)
}

tilo_test_env::scenario! {
    fn the_sheet_is_sized_placed_and_painted;



    phase a_coarse_pointer_grows_the_sheets_controls_to_a_fingers_target;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::dark();
        let actions = [
            ModalAction::primary("Save"),
            ModalAction::secondary("Cancel"),
        ];
        let spec = ModalSpec::new("Save?").actions(&actions).body_height(40.0);
        let mut tr = TextRenderer::new();
        let touch = layout_for(
            VIEWPORT,
            &spec,
            &mut tr,
            &theme,
            tilo_ui_base::interaction::InteractionEnvironment::TOUCH,
        );
        for action in &touch.actions {
            assert!(
                action.height() >= 48.0 && action.width() >= 48.0,
                "an action is a finger's target: {action:?}"
            );
            assert!(
                VIEWPORT.contains(action.center()) && touch.card.contains(action.center()),
                "the target stands on the sheet and in the window: {action:?}"
            );
        }
        let close = touch.close.expect("the sheet closes");
        assert!(
            close.height() >= 48.0,
            "the close is a finger's target: {close:?}"
        );
        let pointer = lay(&spec, &theme);
        assert!(
            pointer.actions[0].height() < touch.actions[0].height(),
            "a mouse needs no finger's area"
        );
        assert!(
            pointer.card.height() < touch.card.height(),
            "the sheet grows with them"
        );
    }

    phase compact_modals_fill_the_window_and_keep_their_controls_on_the_foreground_page;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        for (name, theme) in themes() {
            for window in [
                Rect::new(0.0, 0.0, 360.0, 800.0),
                Rect::new(20.0, 30.0, 820.0, 390.0),
            ] {
                let actions = [
                    ModalAction::secondary("Cancel"),
                    ModalAction::primary("Save"),
                ];
                let spec = ModalSpec::new("Edit event")
                    .actions(&actions)
                    .body_height(1200.0);
                let mut text = TextRenderer::new();
                let geometry = layout_for(window, &spec, &mut text, &theme, environment());
                assert_eq!(
                    geometry.card, window,
                    "{name}: compact dialog is the whole foreground page"
                );
                assert_eq!(
                    geometry.scrim, window,
                    "the modal still isolates the whole viewport"
                );
                assert!(
                    geometry.body_overflows,
                    "the body scrolls while the footer stays reachable"
                );
                assert_eq!(
                    geometry.body.width(),
                    body_width(window, spec.max_width, &theme)
                );
                for (index, rect) in geometry.actions.iter().enumerate() {
                    assert!(window.contains_rect(*rect));
                    assert!(rect.y0 >= geometry.body.y1);
                    assert_eq!(geometry.hit(rect.center()), Some(ModalHit::Action(index)));
                }
                let close = geometry
                    .close
                    .expect("the full-window page retains its close control");
                assert!(window.contains_rect(close));
                assert_eq!(geometry.hit(close.center()), Some(ModalHit::Close));
                assert_eq!(
                    geometry.hit(window.origin() + Vec2::new(1.0, 1.0)),
                    Some(ModalHit::Card)
                );
                let mut scene = Scene::new();
                let recording = tilo_ui_base::part_paint::backdrop::BackdropRecording::begin();
                tilo_ui_base::part_paint::backdrop::layer_scene(&scene, 0);
                paint_backdrop(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &geometry,
                    &spec,
                );
                assert!(
                    recording.finish().is_empty(),
                    "the full-window foreground is opaque, not a blurred card"
                );
            }
        }
    }


    phase modal_ink_and_action_labels_are_legible_on_every_theme;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        for (name, theme) in themes() {
            let ground = tilo_ui_base::chrome::panel_ground(&theme);
            let (primary, dim) = ink(&theme);
            assert!(
                contrast_ratio(primary, ground) >= MODAL_MIN_INK_CONTRAST,
                "{name}"
            );
            assert!(contrast_ratio(dim, ground) >= 3.0, "{name}");
            for style in [
                ButtonRole::Primary,
                ButtonRole::Secondary,
                ButtonRole::Destructive,
            ] {
                let face = ButtonFace::new(Rect::new(0.0, 0.0, 100.0, 32.0), "Create").role(style);
                let face = if style == ButtonRole::Destructive {
                    face.confirming()
                } else {
                    face
                };
                let (bg, _border, painted) = face.colors(&theme);
                assert!(contrast_ratio(painted, bg) >= 4.5, "{name}/{style:?}");
            }
        }
    }

    phase the_dialog_is_presented_in_the_top_layer_over_the_whole_window;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let mut d = ModalDialog::new("Title", tilo_ui_app::testing::TestLeaf::auto()).open(true);
            lay_out(
                &mut d,
                Rect::new(300.0, 100.0, 700.0, 500.0),
                &Theme::dark(),
            );
            let mut scene = Scene::new();
            d.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut TextRenderer::new(), &Theme::dark(), tilo_ui_base::presence::PresenceCtx::default().draw_env()));
            assert!(scene.encoding().draw_tags.is_empty(), "draw paints nothing");

            let scene = present(&mut d, WINDOW, &Theme::dark());
            assert!(!scene.encoding().draw_tags.is_empty());
            assert_eq!(
                d.test_layout().as_ref().map(|l| l.scrim),
                Some(WINDOW),
                "the scrim covers the window, not the pane the dialog is mounted in"
            );
            assert!(
                (d.dialog_rect().center().x - WINDOW.center().x).abs() < 0.5,
                "the sheet is centred on the window"
            );
        }

        {
            let mut d = ModalDialog::new("Title", tilo_ui_app::testing::TestLeaf::auto()).open(false);
            lay_out(&mut d, WINDOW, &Theme::dark());
            let scene = present(&mut d, WINDOW, &Theme::dark());
            assert!(
                scene
                    .encoding()
                    .draw_tags
                    .iter()
                    .all(|t| *t != DrawTag::COLOR)
            );
        }

        {
            let mut short = ModalDialog::new("Title", TestLeaf::sized(200.0, 40.0)).open(true);
            let mut tall = ModalDialog::new(
                "Title",
                TestLeaf::sized(200.0, 240.0),
            )
            .open(true);
            present(&mut short, WINDOW, &Theme::dark());
            present(&mut tall, WINDOW, &Theme::dark());
            assert!(
                (tall.dialog_rect().height() - short.dialog_rect().height() - 200.0).abs() < 0.5,
                "no fixed height: {} vs {}",
                tall.dialog_rect().height(),
                short.dialog_rect().height()
            );
            let body = short.test_layout().as_ref().unwrap().body;
            assert!((short.content().get_rect().y0 - body.y0).abs() < 0.5);
        }

        {
            let mut dialog = ModalDialog::new(
                "Large text",
                Text::new("Scaled dialog content").with_role(TextRole::Title),
            )
            .open(true);
            let theme = Theme::dark().with_text_scale(2.0);
            lay_out(&mut dialog, WINDOW, &theme);
            assert!(dialog.content().get_rect().height() >= 49.0);
            let title_line = f64::from(theme.text(TextRole::Title).line_height_px());
            assert!(dialog.content().get_rect().y0 >= dialog.dialog_rect().y0 + title_line);
        }
    }


}

tilo_test_env::scenario! {
    fn the_dialogs_actions_answer_keys_presses_and_assistive_technology;

    phase modal_controls_are_hittable_and_show_their_states;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let spec = ModalSpec::new("Workspace invite").body_height(120.0);
            for (name, theme) in themes() {
                let l = lay(&spec, &theme);
                let close = l.close.unwrap_or_else(|| panic!("{name}: no close"));
                assert!(close.width() >= 32.0 && close.height() >= 32.0, "{name}");
                assert!(l.card.contains(close.center()), "{name}");
                assert_eq!(l.hit(close.center()), Some(ModalHit::Close), "{name}");
                assert!(l.dismisses(close.center()), "{name}");
                assert!(l.title.x1 <= close.x0, "{name}: title runs under close");
            }
        }

        {
            let theme = Theme::dark();
            let paint = |a: ModalAction<'_>| {
                let actions = [a];
                let spec = ModalSpec::new("Invite someone")
                    .actions(&actions)
                    .body_height(80.0);
                let mut tr = TextRenderer::new();
                let l = layout_for(VIEWPORT, &spec, &mut tr, &theme, environment());
                let mut scene = Scene::new();
                paint_actions(&mut scene, Affine::IDENTITY, &mut tr, &theme, &l, &spec);
                scene.encoding().draw_data.clone()
            };
            let rest = paint(ModalAction::secondary("Cancel"));
            for (what, face) in [
                ("hover", ModalAction::secondary("Cancel").hovered(true)),
                ("press", ModalAction::secondary("Cancel").pressed(true)),
                ("focus", ModalAction::secondary("Cancel").focused(true)),
            ] {
                assert_ne!(paint(face), rest, "a {what}ed action paints as at rest");
            }
            assert_eq!(
                paint(
                    ModalAction::secondary("Cancel")
                        .enabled(false)
                        .hovered(true)
                ),
                paint(ModalAction::secondary("Cancel").enabled(false)),
                "a disabled action does not react to the pointer",
            );
        }

        {
            let theme = Theme::light();
            let paint = |spec: &ModalSpec<'_>| {
                let mut tr = TextRenderer::new();
                let l = layout_for(VIEWPORT, spec, &mut tr, &theme, environment());
                let mut scene = Scene::new();
                paint_backdrop(&mut scene, Affine::IDENTITY, &mut tr, &theme, &l, spec);
                scene.encoding().draw_data.clone()
            };
            let base = ModalSpec::new("Invite someone").body_height(80.0);
            let rest = paint(&base);
            assert_ne!(paint(&base.clone().close_hovered(true)), rest, "hover");
            assert_ne!(paint(&base.clone().close_focused(true)), rest, "focus");
        }
    }

    phase actions_follow_platform_order_inside_the_sheet;
    {
        use tilo_ui_base::{platform::Platform, platform::PlatformConventions};

        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let actions = [
                ModalAction::secondary("Cancel"),
                ModalAction::primary("Create invite"),
            ];
            let theme = Theme::dark();
            let spec = ModalSpec::new("Workspace invite")
                .actions(&actions)
                .platform_conventions(PlatformConventions::new(Platform::Linux))
                .body_height(100.0);
            let l = lay(&spec, &theme);
            assert!(l.actions[0].x1 <= l.actions[1].x0);
            let gutter = f64::from(theme.tokens.spacing.s6);
            assert!((l.actions[1].x1 - (l.card.x1 - gutter)).abs() < 0.5);
            assert!(
                (l.actions[1].y1 - (l.card.y1 - gutter)).abs() < 0.5,
                "the footer is anchored to the bottom of the sheet"
            );
            assert_eq!(l.primary, Some(1));
        }

        {
            let actions = [
                ModalAction::primary("Save"),
                ModalAction::secondary("Cancel"),
            ];
            let mac = lay(
                &ModalSpec::new("Save changes?")
                    .actions(&actions)
                    .platform_conventions(PlatformConventions::new(Platform::MacOs)),
                &Theme::dark(),
            );
            assert!(mac.actions[0].x0 > mac.actions[1].x0);
            assert_eq!(mac.hit(mac.actions[0].center()), Some(ModalHit::Action(0)));
            let windows = lay(
                &ModalSpec::new("Save changes?")
                    .actions(&actions)
                    .platform_conventions(PlatformConventions::new(Platform::Windows)),
                &Theme::dark(),
            );
            assert!(windows.actions[0].x0 < windows.actions[1].x0);
        }

        {
            let actions = [
                ModalAction::secondary("Paste"),
                ModalAction::secondary("Cancel"),
                ModalAction::primary("Join"),
            ];
            for theme in [Theme::dark(), Theme::dark().with_text_scale(2.0)] {
                let spec = ModalSpec::new("Join a workspace")
                    .actions(&actions)
                    .body_height(120.0);
                let l = lay(&spec, &theme);
                for (i, r) in l.actions.iter().enumerate() {
                    assert!(l.card.contains(r.center()));
                    assert_eq!(l.hit(r.center()), Some(ModalHit::Action(i)));
                    assert!(!l.dismisses(r.center()));
                    assert!((r.height() - action_height(&theme)).abs() < 0.01);
                }
            }
            assert_eq!(action_height(&Theme::dark()), 32.0, "a Regular control");
        }

        {
            let actions = [ModalAction::primary("Join").enabled(false)];
            let l = lay(
                &ModalSpec::new("Join").actions(&actions).body_height(80.0),
                &Theme::dark(),
            );
            assert_eq!(l.primary, None);
        }
    }

    phase scrim_and_sheet_route_clicks_that_hit_no_control;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let corner = Point::new(4.0, 4.0);
        let l = lay(
            &ModalSpec::new("Workspace invite").body_height(100.0),
            &Theme::dark(),
        );
        assert_eq!(l.scrim, VIEWPORT, "the scrim covers the whole window");
        assert_eq!(l.hit(corner), Some(ModalHit::Scrim));
        assert!(l.dismisses(corner));
        let sticky = lay(
            &ModalSpec::new("Workspace invite")
                .body_height(100.0)
                .dismiss_on_scrim(false),
            &Theme::dark(),
        );
        assert_eq!(sticky.hit(corner), Some(ModalHit::Scrim));
        assert!(!sticky.dismisses(corner));
        assert_eq!(l.hit(l.body.center()), Some(ModalHit::Card));
        assert!(!l.dismisses(l.body.center()));
        assert_eq!(l.hit(Point::new(-10.0, -10.0)), None);
    }

    phase dialog_accessibility_exposes_open_content_and_real_actions;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let fired = Rc::new(Cell::new(0));
            let callback = fired.clone();
            let mut dialog = ModalDialog::new("Review", tilo_ui_app::testing::TestLeaf::auto())
                .open(true)
                .primary_action("Restore", move || callback.set(callback.get() + 1));
            let target = dialog.accessibility().unwrap().children[0]
                .action_target
                .clone()
                .unwrap();
            let mut doc = AutoCommit::new();
            let activate = tilo_ui_base::a11y::Action::Activate;
            assert!(dialog.perform_semantic_action(
                &target,
                &activate,
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert_eq!(fired.get(), 1);
            dialog.set_open(false);
            assert!(dialog.accessibility().unwrap().children.is_empty());
            assert!(!dialog.perform_semantic_action(
                &target,
                &activate,
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            dialog.set_open(true);
            dialog.primary_action = None;
            assert!(!dialog.perform_semantic_action(
                &target,
                &activate,
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(
                !dialog.perform_semantic_action(
                    "modal.primary",
                    &activate,
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                    &automerge::ROOT
                ),
                "an absent action is not activatable"
            );
            assert_eq!(automerge::transaction::Transactable::pending_ops(&doc), 0);
        }

        {
            let d = ModalDialog::new("Confirm delete", tilo_ui_app::testing::TestLeaf::auto())
                .open(true)
                .on_dismiss(|| {});
            let node = Widget::accessibility(&d).unwrap();
            assert_eq!(node.role, tilo_ui_base::a11y::Role::Dialog);
            assert_eq!(node.name.as_deref(), Some("Confirm delete"));
            assert!(node.state.modal && !node.state.hidden);
            assert!(
                node.children
                    .iter()
                    .any(|c| c.name.as_deref() == Some("Close")),
                "a dialog with no Cancel exposes its close control"
            );
            let closed = ModalDialog::new("Confirm delete", tilo_ui_app::testing::TestLeaf::auto()).open(false);
            assert!(Widget::accessibility(&closed).unwrap().state.hidden);
            assert_eq!(Widget::children(&d).len(), 1);
        }

        {
            use tilo_ui_base::accesskit_bridge::build_tree_update;
            use tilo_ui_app::widget::Button;
            use accesskit::Role as AccessKitRole;

            let mut d =
                ModalDialog::new("Confirm", Button::new("modal-child", "Confirm action")).open(true);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            d.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let open = build_tree_update(&d, &node_map, None, 1.0);
            assert!(
                open.nodes
                    .iter()
                    .any(|(_, node)| node.role() == AccessKitRole::Button)
            );
            d.set_open(false);
            let closed = build_tree_update(&d, &node_map, None, 1.0);
            assert!(
                closed
                    .nodes
                    .iter()
                    .all(|(_, node)| node.role() != AccessKitRole::Button)
            );
            assert!(Widget::children_mut(&mut d).is_empty());
        }
    }

    phase dialog_dismisses_and_invokes_actions_by_key_and_pointer;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let dismissed = Rc::new(Cell::new(0));
            let flag = Rc::clone(&dismissed);
            let mut d = ModalDialog::new("Title", tilo_ui_app::testing::TestLeaf::auto())
                .open(true)
                .on_dismiss(move || flag.set(flag.get() + 1));
            lay_out(&mut d, WINDOW, &Theme::dark());
            assert!(dispatch(&mut d, &named_key(NamedKey::Escape)));
            assert_eq!(dismissed.get(), 1);
            dispatch(&mut d, &cursor_at(Point::new(10.0, 10.0)));
            assert!(dispatch(&mut d, &mouse_press()), "swallowed");
            assert_eq!(dismissed.get(), 2, "a scrim click dismisses");
            let close = d.close_rect().expect("no Cancel: a close control");
            dispatch(&mut d, &cursor_at(close.center()));
            dispatch(&mut d, &mouse_press());
            assert_eq!(dismissed.get(), 3, "the close control dismisses");
            assert!(d.handle_detached(
                &named_key(NamedKey::Escape),
                1.0,
                Modifiers::default(),
                &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(dismissed.get(), 4, "a painted host routes events too");
        }

        {
            let dismissed = Rc::new(Cell::new(0));
            let flag = Rc::clone(&dismissed);
            let mut d = ModalDialog::new("Title", tilo_ui_app::testing::TestLeaf::auto())
                .open(true)
                .dismiss_on_scrim(false)
                .on_dismiss(move || flag.set(flag.get() + 1));
            lay_out(&mut d, WINDOW, &Theme::dark());
            dispatch(&mut d, &cursor_at(Point::new(10.0, 10.0)));
            assert!(dispatch(&mut d, &mouse_press()), "the scrim swallows it");
            assert_eq!(
                dismissed.get(),
                0,
                "a press beside the sheet answers nothing"
            );
            dispatch(&mut d, &cursor_at(Point::new(-20.0, -20.0)));
            assert!(dispatch(&mut d, &mouse_press()));
            assert_eq!(dismissed.get(), 0);
            assert!(dispatch(&mut d, &named_key(NamedKey::Escape)));
            assert_eq!(dismissed.get(), 1);
        }

        {
            let fired = Rc::new(Cell::new(0usize));
            let f = Rc::clone(&fired);
            let (mut d, _) = laid_out_counting(true);
            d.primary_action = Some(DialogAction::new("Save", move || f.set(f.get() + 1)));
            lay_out(&mut d, WINDOW, &Theme::dark());
            let p = d.primary_rect().center();
            dispatch(&mut d, &cursor_at(p));
            dispatch(&mut d, &mouse_press());
            assert_eq!(fired.get(), 1, "a click on the primary action fires it");
            dispatch(&mut d, &named_key(NamedKey::Enter));
            assert_eq!(fired.get(), 2, "Enter fires the primary action");
            d.content_mut().consumes = true;
            dispatch(&mut d, &named_key(NamedKey::Enter));
            assert_eq!(fired.get(), 2, "content that consumes Enter suppresses it");
        }

        {
            let mut d = ModalDialog::new("Title", tilo_ui_app::testing::TestLeaf::auto())
                .primary_action("Save", || {})
                .secondary_action("Cancel", || {})
                .open(true);
            lay_out(&mut d, WINDOW, &Theme::dark());
            d.set_platform_conventions(PlatformConventions::new(Platform::MacOs));
            assert!(d.primary_rect().x0 > d.secondary_rect().x0);
            d.set_platform_conventions(PlatformConventions::new(Platform::Windows));
            assert!(d.primary_rect().x0 < d.secondary_rect().x0);
            assert!(d.close_rect().is_none(), "Cancel is the way out");
        }
    }

    phase open_dialog_captures_input_and_closed_dialog_is_inert;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let (d, _) = laid_out_counting(true);
            assert!(Widget::modal_capture(&d).is_some());
            let (d, _) = laid_out_counting(false);
            assert!(Widget::modal_capture(&d).is_none());
        }

        {
            let (mut d, _) = laid_out_counting(true);
            assert!(dispatch(&mut d, &named_key(NamedKey::Tab)));
        }

        {
            let (mut d, events) = laid_out_counting(true);
            let inside = d.content().get_rect().center();
            dispatch(&mut d, &cursor_at(inside));
            let before = events.events();
            assert!(dispatch(&mut d, &mouse_press()));
            assert!(events.events() > before);
        }

        {
            let (mut d, events) = laid_out_counting(false);
            assert!(!dispatch(&mut d, &mouse_press()));
            assert_eq!(events.events(), 0);
        }
    }

    phase a_three_way_question_stands_in_the_platform_order;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let (saved, save) = counter();
        let (discarded, discard) = counter();
        let (cancelled, cancel) = counter();
        let mut d = ModalDialog::new("Save changes to notes.md?", tilo_ui_app::testing::TestLeaf::auto())
            .primary_action("Save", save)
            .tertiary_action("Don't save", discard)
            .with_destructive_tertiary(true)
            .secondary_action("Cancel", cancel)
            .open(true);
        lay_out(&mut d, WINDOW, &Theme::dark());
        d.set_platform_conventions(PlatformConventions::new(Platform::MacOs));
        let card = d.dialog_rect();
        let pad = f64::from(Theme::dark().tokens.spacing.s6);
        assert!(
            (d.tertiary_rect().x0 - (card.x0 + pad)).abs() < 0.5,
            "Don't save stands apart at the leading edge"
        );
        assert!(d.secondary_rect().x0 < d.primary_rect().x0);
        assert!(d.tertiary_rect().x1 < d.secondary_rect().x0);
        lay_out(&mut d, WINDOW, &Theme::dark());
        let order: Vec<Slot> = d.test_keys().into_iter().map(|(slot, _)| slot).collect();
        assert_eq!(order, [Slot::Secondary, Slot::Primary, Slot::Tertiary]);
        d.set_platform_conventions(PlatformConventions::new(Platform::Windows));
        assert!(d.primary_rect().x0 < d.tertiary_rect().x0);
        assert!(d.tertiary_rect().x0 < d.secondary_rect().x0);
        // Enter is never the alternate; Escape is Cancel when there is no
        // dismiss handler.
        dispatch(&mut d, &named_key(NamedKey::Enter));
        assert_eq!(saved.get(), 1);
        dispatch(&mut d, &named_key(NamedKey::Escape));
        assert_eq!(cancelled.get(), 1);
        let t = d.tertiary_rect().center();
        dispatch(&mut d, &cursor_at(t));
        dispatch(&mut d, &mouse_press());
        assert_eq!(discarded.get(), 1);
        assert!(
            footer_buttons(&d)
                .iter()
                .any(|c| c.name.as_deref() == Some("Don't save")),
            "the third answer is announced"
        );
    }

    phase enter_in_a_destructive_confirmation_is_the_safe_answer;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let (erased, erase) = counter();
        let (cancelled, cancel) = counter();
        let mut d = ModalDialog::new("Erase disk?", tilo_ui_app::testing::TestLeaf::auto())
            .primary_action("Erase", erase)
            .secondary_action("Cancel", cancel)
            .with_destructive(true)
            .open(true);
        lay_out(&mut d, WINDOW, &Theme::dark());
        dispatch(&mut d, &named_key(NamedKey::Enter));
        assert_eq!(cancelled.get(), 1, "Return is Cancel, as on macOS");
        assert_eq!(erased.get(), 0, "Return never erases");
        let p = d.primary_rect().center();
        dispatch(&mut d, &cursor_at(p));
        dispatch(&mut d, &mouse_press());
        assert_eq!(
            erased.get(),
            1,
            "a click still takes the destructive action"
        );

        d.set_platform_conventions(PlatformConventions::new(tilo_ui_base::platform::Platform::Windows));
        lay_out(&mut d, WINDOW, &Theme::dark());
        assert_eq!(
            d.test_keys().first().map(|(slot, _)| *slot),
            Some(Slot::Secondary)
        );

        let theme = Theme::dark();
        let actions = [
            ModalAction::secondary("Cancel"),
            ModalAction::danger("Erase"),
        ];
        let layout = lay(&ModalSpec::new("Erase disk?").actions(&actions), &theme);
        assert_eq!(
            layout.primary,
            Some(0),
            "Enter is Cancel on a painted sheet"
        );
    }

    phase a_check_may_read_state_the_content_writes_elsewhere;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let valid = Rc::new(Cell::new(false));
        let (saved, save) = counter();
        let check = Rc::clone(&valid);
        let mut d = ModalDialog::new("Back up vault", tilo_ui_app::testing::TestLeaf::auto())
            .primary_action("Choose file…", save)
            .secondary_action("Cancel", || {})
            .primary_enabled_if(move |_| check.get())
            .open(true);
        lay_out(&mut d, WINDOW, &Theme::dark());
        present(&mut d, WINDOW, &Theme::dark());
        let primary = |d: &ModalDialog<tilo_ui_app::testing::TestLeaf>| {
            footer_buttons(d)
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Choose file…"))
                .expect("the primary button")
        };
        assert!(primary(&d).state.disabled, "an invalid form cannot go on");
        dispatch(&mut d, &named_key(NamedKey::Enter));
        assert_eq!(saved.get(), 0);

        valid.set(true);
        present(&mut d, WINDOW, &Theme::dark());
        assert!(!primary(&d).state.disabled, "a valid form can");
        dispatch(&mut d, &named_key(NamedKey::Enter));
        assert_eq!(saved.get(), 1);
    }

    phase a_disabled_or_guarded_primary_action_is_inert_and_announced;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let (fired, fire) = counter();
        let mut d = ModalDialog::new("Erase disk", tilo_ui_app::testing::TestLeaf::auto())
            .primary_action("Erase", fire)
            .open(true);
        d.set_primary_enabled(false);
        lay_out(&mut d, WINDOW, &Theme::dark());
        dispatch(&mut d, &named_key(NamedKey::Enter));
        let p = d.primary_rect().center();
        dispatch(&mut d, &cursor_at(p));
        dispatch(&mut d, &mouse_press());
        assert_eq!(
            fired.get(),
            0,
            "neither Enter nor a click takes a disabled action"
        );
        assert!(footer_buttons(&d)[0].state.disabled);
        d.set_primary_enabled(true);
        d.set_primary_label("Erase 2 partitions");
        lay_out(&mut d, WINDOW, &Theme::dark());
        dispatch(&mut d, &named_key(NamedKey::Enter));
        assert_eq!(fired.get(), 1);
        assert_eq!(
            footer_buttons(&d)[0].name.as_deref(),
            Some("Erase 2 partitions")
        );

        struct ValueContent {
            value: String,
            rect: Rect,
        }

        impl Widget for ValueContent {
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                _theme: &Theme,
                node_map: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                let style = Style {
                    size: Size::from_lengths(200.0, 40.0),
                    ..Default::default()
                };
                taffy.widget_leaf(style, node_map, self)
            }
            tilo_ui_base::rect_geometry!();
            fn name(&self) -> &'static str {
                "ValueContent"
            }
        }

        let mut guarded = ModalDialog::new(
            "Erase disk",
            ValueContent {
                value: "Name".into(),
                rect: Rect::ZERO,
            },
        )
        .primary_action("Erase", || {})
        .primary_enabled_if(|body| body.value == "sdb")
        .open(true);
        assert!(!guarded.primary_enabled());
        guarded.content_mut().value = "sdb".into();
        assert!(guarded.primary_enabled());
    }

    phase an_open_list_in_the_content_takes_the_press_before_the_scrim;
    {
        struct OpenList(TestLeaf);
        impl Widget for OpenList {
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                theme: &Theme,
                node_map: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                self.0.build_layout(taffy, theme, node_map)
            }
            fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
                self.0.apply_layout(taffy, node, origin);
            }
            fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
                self.0
                    .draw(&mut tilo_ui_base::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()));
            }
            fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
                self.0.handle_event(event, ctx);
            }
            fn wants_pointer_capture(&self) -> bool {
                true
            }
            fn name(&self) -> &'static str {
                "OpenList"
            }
            fn get_rect(&self) -> Rect {
                self.0.get_rect()
            }
        }
        let _settled = tilo_ui_base::motion::testing::settled();
        let (dismissed, dismiss) = counter();
        let list = TestLeaf::sized(200.0, 40.0).consumes();
        let events = list.seen();
        let mut d = ModalDialog::new(
            "Settings",
            OpenList(list),
        )
        .on_dismiss(dismiss)
        .open(true);
        lay_out(&mut d, WINDOW, &Theme::dark());
        let below = Point::new(d.dialog_rect().center().x, d.dialog_rect().y1 + 20.0);
        dispatch(&mut d, &cursor_at(below));
        let before = events.events();
        dispatch(&mut d, &mouse_press());
        assert!(events.events() > before, "the list sees the press");
        assert_eq!(
            dismissed.get(),
            0,
            "the press is the list's, not the scrim's"
        );
    }

    phase footer_actions_are_addressable_by_name;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut d = ModalDialog::new("Delete note?", tilo_ui_app::testing::TestLeaf::auto())
            .with_test_id("notes.delete")
            .primary_action("Delete", || {})
            .secondary_action("Cancel", || {})
            .open(true);
        lay_out(&mut d, WINDOW, &Theme::dark());
        let ids: Vec<Option<String>> = Widget::children(&d)
            .iter()
            .map(|c| c.test_id().map(str::to_string))
            .collect();
        assert!(ids.contains(&Some("notes.delete.primary".into())));
        assert!(ids.contains(&Some("notes.delete.secondary".into())));
        let primary = Widget::children(&d)
            .into_iter()
            .find(|c| c.test_id() == Some("notes.delete.primary"))
            .map(Widget::get_rect)
            .unwrap();
        assert_eq!(
            primary,
            d.primary_rect(),
            "the address stands where the action is painted"
        );
    }
}

fn wheel(dy: f32) -> WindowEvent {
    tilo_ui_base::synthetic_input::mouse_wheel(0.0, dy)
}

fn footer_buttons<C: Widget + 'static>(d: &ModalDialog<C>) -> Vec<tilo_ui_base::a11y::AccessNode> {
    Widget::children(d)
        .into_iter()
        .filter(|child| child.name() == "ModalAction")
        .filter_map(Widget::accessibility)
        .collect()
}

fn counter() -> (Rc<Cell<usize>>, impl FnMut() + 'static) {
    let count = Rc::new(Cell::new(0));
    let c = Rc::clone(&count);
    (count, move || c.set(c.get() + 1))
}

#[test]
fn a_hosted_dialog_focuses_its_fields_by_pointer_and_its_footer_by_tab() {
    use tilo_ui_app::app::driver::Framed;
    use tilo_ui_app::widget::{Column, TextInput};
    let _settled = tilo_ui_base::motion::testing::settled();
    let (cancelled, cancel) = counter();
    let (connected, connect) = counter();
    let content = Column::new()
        .push(Box::new(TextInput::local().with_placeholder("Name")))
        .push(Box::new(TextInput::local().with_placeholder("Host")));
    let dialog = ModalDialog::new("Connection", content)
        .primary_action("Connect", connect)
        .secondary_action("Cancel", cancel)
        .open(true);
    let mut framed = Framed::new(tilo_ui_app::State::new(), dialog, (800, 600), Theme::dark());
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    let focused = |framed: &Framed<ModalDialog<Column>>| -> Vec<bool> {
        framed
            .content()
            .children()
            .iter()
            .map(|field| field.is_focused())
            .collect()
    };
    let host = framed.content().children()[1].get_rect().center();
    let name = framed.content().children()[0].get_rect().center();
    assert!(
        framed.dialog_rect().contains(host),
        "the field is laid out inside the sheet: {host:?} in {:?}",
        framed.dialog_rect()
    );
    Framed::harness_mut(&mut framed).click_at(host.x, host.y);
    assert_eq!(
        focused(&framed),
        [false, true],
        "a click focuses the field it lands on"
    );
    Framed::harness_mut(&mut framed).click_at(name.x, name.y);
    assert_eq!(focused(&framed), [true, false], "and blurs the one before");
    Framed::harness_mut(&mut framed).type_text("db1");
    assert_eq!(
        framed.content().children()[0]
            .accessibility()
            .and_then(|node| node.value),
        Some("db1".to_string()),
        "typing lands in the focused field"
    );
    let harness = Framed::harness_mut(&mut framed);
    harness.tab(false);
    harness.tab(false);
    let keys: Vec<(Slot, bool)> = framed.test_keys();
    assert_eq!(
        keys,
        [(Slot::Secondary, true), (Slot::Primary, false)],
        "after the fields, Tab reaches Cancel first"
    );
    Framed::harness_mut(&mut framed).key("Enter");
    assert_eq!(
        cancelled.get(),
        1,
        "Enter on a focused footer action takes it"
    );
    assert_eq!(connected.get(), 0);
}

tilo_test_env::scenario! {
    fn the_sheets_body_scrolls_under_its_header;

    phase a_tall_body_scrolls_inside_a_sheet_that_takes_the_window;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::dark();
        let mut d = ModalDialog::new(
            "Shortcuts",
            TestLeaf::sized(200.0, 2000.0),
        )
        .open(true);
        lay_out(&mut d, WINDOW, &theme);
        present(&mut d, WINDOW, &theme);
        let layout = d.test_layout().cloned().expect("laid out");
        assert!(layout.body_overflows);
        let pad = f64::from(theme.tokens.spacing.s6);
        assert!(
            layout.card.y0 >= WINDOW.y0 + pad - 0.5 && layout.card.y1 <= WINDOW.y1 - pad + 0.5,
            "a tall sheet stays inside the window's gutter: {:?}",
            layout.card
        );
        dispatch(&mut d, &cursor_at(layout.body.center()));
        dispatch(&mut d, &wheel(-3.0));
        let scrolled = d.scroll_offset();
        assert!(scrolled > 0.0, "the wheel scrolls the body");
        lay_out(&mut d, WINDOW, &theme);
        assert!(
            (d.content().get_rect().y0 - (d.body_rect().y0 - scrolled)).abs() < 0.5,
            "the content moves up by the scroll"
        );
        // Scrolling never runs past either end.
        for _ in 0..200 {
            dispatch(&mut d, &wheel(-3.0));
        }
        let max = 2000.0 - d.body_rect().height();
        assert!(d.scroll_offset() <= max + 0.5);
        for _ in 0..200 {
            dispatch(&mut d, &wheel(3.0));
        }
        assert!(d.scroll_offset().abs() < 1e-9);
    }

    phase a_rebuilt_sheet_keeps_where_its_body_was_scrolled;
    // An app builds a new tree whenever its state changes: a choice made far down a long sheet
    // builds a new dialog, which starts where the person was, not back at its top.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::dark();
        let kept = Rc::new(Cell::new(0.0));
        let tall = || TestLeaf::sized(200.0, 2000.0);
        let mut d = ModalDialog::new("Agents", tall())
            .open(true)
            .keeping_scroll(Rc::clone(&kept));
        lay_out(&mut d, WINDOW, &theme);
        present(&mut d, WINDOW, &theme);
        let body = d.test_layout().cloned().expect("laid out").body;
        dispatch(&mut d, &cursor_at(body.center()));
        for _ in 0..4 {
            dispatch(&mut d, &wheel(-3.0));
        }
        let scrolled = d.scroll_offset();
        assert!(scrolled > 0.0 && (kept.get() - scrolled).abs() < 0.5);
        let mut rebuilt = ModalDialog::new("Agents", tall())
            .open(true)
            .keeping_scroll(Rc::clone(&kept));
        lay_out(&mut rebuilt, WINDOW, &theme);
        present(&mut rebuilt, WINDOW, &theme);
        lay_out(&mut rebuilt, WINDOW, &theme);
        assert!(
            (rebuilt.scroll_offset() - scrolled).abs() < 0.5,
            "the rebuilt sheet stands where the person was: {} of {scrolled}",
            rebuilt.scroll_offset()
        );
        assert!(
            (rebuilt.content().get_rect().y0 - (rebuilt.body_rect().y0 - scrolled)).abs() < 0.5,
            "and its content is placed there"
        );
        // A sheet without one starts at its top.
        let mut fresh = ModalDialog::new("Agents", tall()).open(true);
        lay_out(&mut fresh, WINDOW, &theme);
        assert!(fresh.scroll_offset().abs() < 1e-9);
    }

    phase a_header_stays_put_while_the_body_scrolls_under_it;
    // A strip the caller passes as the sheet's header stays where it is while
    // the body scrolls under it: the wheel moves what the body holds, never the
    // tabs a person is switching between.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::dark();

        let strip = 40.0;
        let plain = lay(&ModalSpec::new("Tabs").body_height(600.0), &theme);
        let headed = lay(
            &ModalSpec::new("Tabs")
                .body_height(600.0)
                .header_height(strip),
            &theme,
        );
        assert!(
            plain.header.is_none(),
            "a sheet that asked for none has none"
        );
        let header = headed.header.expect("the spec asked for a header");
        assert!(
            (header.height() - strip).abs() < 0.5,
            "the header takes the height it asked for: {header:?}"
        );
        assert!(
            (header.width() - headed.body.width()).abs() < 0.5,
            "and the body's width"
        );
        assert!(
            header.y0 >= headed.title.y1 - 0.5 && headed.body.y0 >= header.y1,
            "the header stands below the title and the body below it: {:?} {:?}",
            header,
            headed.body
        );
        let gap = plain.body.y0 - plain.title.y1;
        assert!(
            (headed.card.height() - plain.card.height() - strip - gap).abs() < 0.5,
            "the sheet grows by the header and its gap: {} vs {}",
            plain.card.height(),
            headed.card.height()
        );
        assert!(
            (plain.body.height() - headed.body.height()).abs() < 0.5,
            "and the body keeps the height it asked for"
        );

        let mut d = ModalDialog::new(
            "Tabs",
            TestLeaf::sized(200.0, 2000.0),
        )
        .with_header(TestLeaf::sized(200.0, 40.0))
        .open(true);
        lay_out(&mut d, WINDOW, &theme);
        present(&mut d, WINDOW, &theme);
        let header_before = d.header_rect().expect("the sheet shows its header");
        let body = d.body_rect();
        dispatch(&mut d, &cursor_at(body.center()));
        dispatch(&mut d, &wheel(-3.0));
        let scrolled = d.scroll_offset();
        assert!(scrolled > 0.0, "the wheel scrolls the body");
        lay_out(&mut d, WINDOW, &theme);
        present(&mut d, WINDOW, &theme);
        assert_eq!(
            d.header_rect(),
            Some(header_before),
            "the header stays where it stands"
        );
        assert!(
            (d.content().get_rect().y0 - (d.body_rect().y0 - scrolled)).abs() < 0.5,
            "while the content under it moves up by the scroll"
        );
    }


}

#[test]
fn space_flips_a_focused_switch_and_enter_is_the_sheets_default() {
    use tilo_ui_app::widget::{Column, TextInput, Toggle};
    let _settled = tilo_ui_base::motion::testing::settled();
    let (saved, save) = counter();
    let on = tilo_ui_base::LocalState::new(false);
    let content = Column::new()
        .push(Box::new(TextInput::local().with_placeholder("Name")))
        .push(Box::new(
            Toggle::from_state(&on).with_accessible_name("Sync"),
        ));
    let dialog = ModalDialog::new("Account", content)
        .primary_action("Save", save)
        .secondary_action("Cancel", || {})
        .open(true);
    let mut framed = Framed::new(tilo_ui_app::State::new(), dialog, (800, 600), Theme::dark());
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    let switch_focused =
        |framed: &Framed<ModalDialog<Column>>| framed.content().children()[1].is_focused();
    for _ in 0..4 {
        if switch_focused(&framed) {
            break;
        }
        Framed::harness_mut(&mut framed).tab(false);
    }
    assert!(switch_focused(&framed), "Tab reaches the switch");

    Framed::harness_mut(&mut framed).key("Space");
    assert!(on.get(), "Space flips the focused switch");
    assert_eq!(saved.get(), 0, "and does not save");

    Framed::harness_mut(&mut framed).key("Enter");
    assert!(on.get(), "Enter leaves the switch as it is");
    assert_eq!(saved.get(), 1, "Enter on a switch saves");

    let name = framed.content().children()[0].get_rect().center();
    Framed::harness_mut(&mut framed).click_at(name.x, name.y);
    Framed::harness_mut(&mut framed).key("Enter");
    assert_eq!(saved.get(), 2, "Enter in a field saves");
}

#[test]
fn every_footer_action_is_one_focusable_button() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (connected, connect) = counter();
    let content = Column::new().push(Box::new(TextInput::local().with_placeholder("Name")));
    let dialog = ModalDialog::new("Connection", content)
        .with_test_id("connect")
        .primary_action("Connect", connect)
        .secondary_action("Cancel", || {})
        .open(true);
    let mut framed = Framed::new(tilo_ui_app::State::new(), dialog, (800, 600), Theme::dark());
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    let issues = Framed::harness(&framed).accessibility_audit().issues;
    assert!(issues.is_empty(), "the open sheet audits clean: {issues:?}");
    let nodes = Framed::harness(&framed).a11y_nodes();
    for label in ["Connect", "Cancel"] {
        let buttons = nodes
            .iter()
            .filter(|(_, node)| {
                format!("{:?}", node.role()) == "Button" && node.label() == Some(label)
            })
            .count();
        assert_eq!(buttons, 1, "{label} is exactly one button");
    }
    let mut focused = None;
    for _ in 0..3 {
        Framed::harness_mut(&mut framed).tab(false);
        focused = Framed::harness(&framed)
            .focused_access_node()
            .filter(|node| node.role == tilo_ui_base::a11y::Role::Button);
        if focused.is_some() {
            break;
        }
    }
    let focused = focused.expect("Tab reaches a footer button");
    assert_eq!(focused.name.as_deref(), Some("Cancel"));
    assert!(focused.state.focused, "the reader is on the focused action");
    Framed::harness_mut(&mut framed)
        .click_named("connect.primary")
        .expect("the primary action has an address");
    assert_eq!(connected.get(), 1, "its address still operates the action");
    let primary = footer_buttons(&framed)
        .into_iter()
        .find(|node| node.name.as_deref() == Some("Connect"))
        .and_then(|node| node.action_target)
        .expect("the primary button has a semantic address");
    Framed::harness_mut(&mut framed)
        .perform_semantic_named(
            "connect.primary",
            Some(&primary),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .expect("the reader's activation lands");
    assert_eq!(
        connected.get(),
        2,
        "the button's activation runs the action"
    );
}

#[test]
fn what_stands_behind_an_open_sheet_never_takes_its_input() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // A page lists its content before its dialog. A greedy sibling (a table
    // that takes every press) must not see the press meant for the sheet.
    use tilo_ui_app::widget::Column;
    let behind = TestLeaf::sized(200.0, 500.0).consumes();
    let seen = behind.seen();
    let (confirmed, confirm) = counter();
    let page = Column::new().push(Box::new(behind)).push(Box::new(
        ModalDialog::new("Delete this rule?", tilo_ui_app::testing::TestLeaf::auto())
            .with_test_id("confirm")
            .primary_action("Delete rule", confirm)
            .secondary_action("Cancel", || {})
            .open(true),
    ));
    let mut framed = Framed::new(tilo_ui_app::State::new(), page, (800, 600), Theme::dark());
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    let before = seen.events();
    Framed::harness_mut(&mut framed)
        .click_named("confirm.primary")
        .expect("the action has an address");
    assert_eq!(confirmed.get(), 1, "the press reaches the sheet's action");
    assert_eq!(
        seen.events(),
        before,
        "and nothing behind the sheet sees it"
    );
}

/// A dialog a click opens arrives from its centre over its fading scrim
/// (`motion::Appear`) and leaves the same way, quicker; one built open is at
/// rest from its first paint, so a host that rebuilds its tree never replays
/// the entrance; a key opens it at once.
#[test]
fn a_dialog_arrives_from_its_centre_and_a_rebuilt_one_does_not_replay_it() {
    use tilo_ui_app::time::testing::with_monotonic_time;
    use tilo_ui_base::motion::note_source;
    use tilo_ui_base::tokens::MotionSource;
    let theme = Theme::dark();
    let t0 = tilo_ui_app::time::now();
    let at = |ms: u64| t0 + tilo_ui_app::time::Duration::from_millis(ms);

    let (mut built, _) = with_monotonic_time(t0, || laid_out_counting(true));
    with_monotonic_time(t0, || {
        let _ = present(&mut built, WINDOW, &theme);
        assert!(
            !built.appear.is_moving(),
            "a dialog built open does not arrive"
        );
        assert!(built.appear.progress() >= 1.0);
    });

    let (mut d, _) = with_monotonic_time(t0, || laid_out_counting(false));
    with_monotonic_time(t0, || {
        note_source(MotionSource::Pointer);
        d.set_open(true);
        lay_out(&mut d, WINDOW, &theme);
        let _ = present(&mut d, WINDOW, &theme);
        assert!(d.needs_animation_frame(), "it arrives");
        assert!(d.appear.progress() < 0.05);
    });
    with_monotonic_time(at(80), || {
        if d.is_layout_dirty() {
            lay_out(&mut d, WINDOW, &theme);
        }
        let _ = present(&mut d, WINDOW, &theme);
        let p = d.appear.progress();
        assert!(p > 0.0 && p < 1.0, "mid-arrival: {p}");
    });
    with_monotonic_time(at(400), || {
        if d.is_layout_dirty() {
            lay_out(&mut d, WINDOW, &theme);
        }
        let _ = present(&mut d, WINDOW, &theme);
        assert!(
            !d.needs_animation_frame(),
            "it rests: appear moving {} (progress {}), stale {}, remeasure {}, content wants frames {}, content size {:?}, body {:?}",
            d.appear.is_moving(),
            d.appear.progress(),
            d.stale,
            d.remeasure,
            d.content.needs_animation_frame(),
            d.content_size,
            d.body_rect()
        );
        d.set_open(false);
        assert!(!d.is_open());
        let leaving = present(&mut d, WINDOW, &theme);
        assert!(
            !leaving.encoding().is_empty(),
            "it is drawn while it leaves"
        );
    });
    with_monotonic_time(at(800), || {
        let gone = present(&mut d, WINDOW, &theme);
        assert!(gone.encoding().is_empty(), "and then it has gone");
        note_source(MotionSource::Keyboard);
        d.set_open(true);
        lay_out(&mut d, WINDOW, &theme);
        let _ = present(&mut d, WINDOW, &theme);
        assert!(!d.appear.is_moving(), "a shortcut lands at once");
    });
}

#[test]
fn a_shared_appearance_arrives_once_and_leaves_until_it_is_gone() {
    use tilo_ui_base::{motion::Appearance, motion::note_source};
    let theme = Theme::dark();
    let t0 = tilo_ui_app::time::now();
    let at = |ms: u64| t0 + tilo_ui_app::time::Duration::from_millis(ms);
    let appearance = Appearance::shared();
    let build = |open: bool| {
        ModalDialog::new("Settings", tilo_ui_app::testing::TestLeaf::auto())
            .appearing(appearance.clone())
            .open(open)
    };

    note_source(MotionSource::Pointer);
    let mut d = with_monotonic_time(t0, || build(true));
    with_monotonic_time(t0, || {
        lay_out(&mut d, WINDOW, &theme);
        let _ = present(&mut d, WINDOW, &theme);
        assert!(
            d.appear.is_moving() && d.appear.progress() < 0.05,
            "built open on a shared appearance, it arrives: moving {}, progress {}",
            d.appear.is_moving(),
            d.appear.progress()
        );
    });
    with_monotonic_time(at(80), || {
        let mut rebuilt = build(true);
        lay_out(&mut rebuilt, WINDOW, &theme);
        let _ = present(&mut rebuilt, WINDOW, &theme);
        let progress = rebuilt.appear.progress();
        assert!(
            progress > 0.0 && progress < 1.0,
            "the rebuild carries the arrival on: {progress}"
        );
        d = rebuilt;
    });
    with_monotonic_time(at(400), || {
        lay_out(&mut d, WINDOW, &theme);
        let _ = present(&mut d, WINDOW, &theme);
        assert!(!d.needs_animation_frame(), "it rests");
        assert!(d.presents_modal());
    });
    note_source(MotionSource::Pointer);
    with_monotonic_time(at(420), || {
        let mut leaving = build(false);
        lay_out(&mut leaving, WINDOW, &theme);
        let shown = present(&mut leaving, WINDOW, &theme);
        assert!(!shown.encoding().is_empty(), "drawn while it leaves");
        assert!(leaving.presents_modal(), "and still the sheet's modal");
        d = leaving;
    });
    with_monotonic_time(at(900), || {
        let mut gone = build(false);
        lay_out(&mut gone, WINDOW, &theme);
        let after = present(&mut gone, WINDOW, &theme);
        assert!(after.encoding().is_empty(), "and then it has gone");
        assert!(!gone.presents_modal());
    });
}

#[test]
fn a_dialog_moves_on_the_motion_the_person_allows() {
    let theme = Theme::dark().with_reduce_motion(true);
    let t0 = tilo_ui_app::time::now();
    let at = |ms: u64| t0 + tilo_ui_app::time::Duration::from_millis(ms);
    let mut d = ModalDialog::new("Settings", tilo_ui_app::testing::TestLeaf::auto());
    with_monotonic_time(t0, || {
        d.set_open(true);
        lay_out(&mut d, WINDOW, &theme);
        let _ = present(&mut d, WINDOW, &theme);
    });
    with_monotonic_time(at(600), || {
        lay_out(&mut d, WINDOW, &theme);
        let _ = present(&mut d, WINDOW, &theme);
        assert!(!d.appear.is_moving(), "it rests");
    });
    tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
    with_monotonic_time(at(620), || d.set_open(false));
    with_monotonic_time(at(660), || {
        lay_out(&mut d, WINDOW, &theme);
        let _ = present(&mut d, WINDOW, &theme);
        let card = Rect::new(0.0, 0.0, 400.0, 300.0);
        let pose = d.appear.pose(card, Point::new(400.0, 0.0));
        assert_eq!(
            pose.transform,
            Affine::IDENTITY,
            "reduced motion: it does not travel"
        );
        assert!(
            pose.opacity > 0.0 && pose.opacity < 1.0,
            "but it fades: {}",
            pose.opacity
        );
    });
}

/// The published controls of a frame, by the words a person reads on them.
fn published_controls(
    h: &tilo_ui_app::headless::Driver,
) -> std::collections::HashMap<String, tilo_actions::ResourceSpec> {
    h.resources()
        .into_iter()
        .filter(|resource| {
            resource
                .id
                .as_str()
                .starts_with(tilo_ui_app::headless::CONTROL_RESOURCE_PREFIX)
                && resource
                    .actions
                    .iter()
                    .any(|action| action.id == "activate")
        })
        .map(|resource| (resource.label.clone(), resource))
        .collect()
}

#[test]
fn an_agent_closes_a_hosted_dialog_through_its_published_close_control() {
    let (dismissed, dismiss) = counter();
    let (deleted, delete) = counter();
    let dialog = ModalDialog::new("Delete rule?", tilo_ui_app::testing::TestLeaf::auto())
        .with_test_id("firewall-delete")
        .primary_action("Delete rule", delete)
        .on_dismiss(dismiss)
        .open(true);
    let mut h = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        tilo_ui_app::widget::Column::new().push(Box::new(dialog)),
        (800, 600),
        Theme::dark(),
    );
    let published = published_controls(&h);
    assert!(
        !published.contains_key("Delete rule"),
        "what the dialog confirms stays the person's"
    );
    let close = published
        .get("Close")
        .expect("closing grants nothing, so an agent reaches it")
        .id
        .as_str()
        .to_owned();
    h.invoke_published(&close, "activate", &serde_json::Map::new())
        .expect("the published close control is reachable");
    assert_eq!(dismissed.get(), 1);
    assert_eq!(deleted.get(), 0);
}

#[test]
fn an_agent_presses_a_dialog_footer_through_its_published_controls() {
    let (saved, save) = counter();
    let (cancelled, cancel) = counter();
    let dialog = ModalDialog::new("Rename", tilo_ui_app::testing::TestLeaf::auto())
        .with_test_id("rename")
        .primary_action("Save", save)
        .secondary_action("Cancel", cancel)
        .open(true);
    let mut h = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        tilo_ui_app::widget::Column::new().push(Box::new(dialog)),
        (800, 600),
        Theme::dark(),
    );
    let published = published_controls(&h);
    assert!(
        !published.contains_key("Save"),
        "the answer that confirms is never an agent's to press"
    );
    let cancel = published
        .get("Cancel")
        .expect("Cancel grants nothing, so an agent reaches it")
        .id
        .as_str()
        .to_owned();
    h.invoke_published(&cancel, "activate", &serde_json::Map::new())
        .expect("Cancel is reachable");
    assert_eq!(cancelled.get(), 1);
    assert_eq!(saved.get(), 0);
    h.click_named("rename.primary")
        .expect("the person still saves through the footer");
    assert_eq!(saved.get(), 1);
}

#[test]
fn a_closed_sheet_names_no_target_and_an_open_one_is_where_it_shows() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let sheet = |open: bool| {
        let mut dialog = ModalDialog::new("About", Column::new().push(Box::new(Text::new("Tilo"))))
            .with_test_id("about-sheet");
        dialog.set_open(open);
        tilo_ui_app::app::driver::Driver::for_root(
            tilo_ui_app::State::new(),
            Column::new().push(Box::new(dialog)),
            (800, 600),
            Theme::dark(),
        )
    };
    let closed = sheet(false);
    let targets = closed.targets();
    assert!(
        !targets.contains_key("about-sheet"),
        "a hidden sheet stands nowhere: {:?}",
        targets.keys().collect::<Vec<_>>()
    );
    assert!(
        !targets.keys().any(|name| name.contains("ModalDialog")),
        "nor does its widget path: {:?}",
        targets.keys().collect::<Vec<_>>()
    );
    let open = sheet(true);
    assert!(
        open.targets().contains_key("about-sheet"),
        "an open sheet is where it shows"
    );
}
