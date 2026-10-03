//! The sheet's geometry, the two guarantees that made the shared chrome
//! exist (there is always a visible way out, and every label is legible),
//! and the retained dialog's input, accessibility and top-layer contract.

use super::*;
use crate::a11y::contrast_ratio;
use crate::widget::Text;
use std::cell::Cell;
use std::rc::Rc;
use vello_encoding::DrawTag;
use winit::event::{DeviceId, KeyEvent, MouseButton};
use winit::keyboard::{KeyLocation, PhysicalKey};

const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1600.0, 1000.0);
const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

fn themes() -> Vec<(&'static str, Theme)> {
    vec![("dark", Theme::dark()), ("light", Theme::light())]
}

fn lay(spec: &ModalSpec<'_>, theme: &Theme) -> ModalLayout {
    let mut tr = TextRenderer::new();
    layout(VIEWPORT, spec, &mut tr, theme)
}

/// Fixed empty content: no paint, no events.
struct TestContent;

impl Widget for TestContent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "TestContent"
    }
    fn get_rect(&self) -> Rect {
        Rect::ZERO
    }
}

/// Counts the events it was forwarded; 200×`height` so it lands inside the
/// sheet.
struct CountingContent {
    events: Rc<Cell<usize>>,
    rect: Rect,
    height: f32,
    consume: bool,
}

impl Default for CountingContent {
    fn default() -> Self {
        Self {
            events: Rc::default(),
            rect: Rect::ZERO,
            height: 40.0,
            consume: false,
        }
    }
}

impl Widget for CountingContent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(200.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn handle_event(&mut self, _event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.events.set(self.events.get().saturating_add(1));
        *ctx.handled = self.consume;
    }
    fn name(&self) -> &'static str {
        "CountingContent"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// SAFETY: opaque `DeviceId` whose all-zero bit pattern winit tolerates.
fn synth_device() -> DeviceId {
    unsafe { std::mem::zeroed() }
}

fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: synth_device(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn left_press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: synth_device(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

/// Built over a zeroed buffer since winit's `KeyEventExtra` is `pub(crate)`.
fn key_press_event(named: NamedKey) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(named);
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: synth_device(),
        event: ev,
        is_synthetic: false,
    }
}

fn dispatch<C: Widget + 'static>(d: &mut ModalDialog<C>, event: &WindowEvent) -> bool {
    let mut tree = TaffyTree::<()>::new();
    let node = tree.leaf_node(Style::default());
    let mut handled = false;
    let mut focus_target = None;
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    d.handle_event(
        event,
        &mut crate::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::default(),
            state: &mut doc,
            path: &root,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &tree,
        },
    );
    handled
}

/// Run the retained passes with the dialog's node filling `area`.
fn lay_out<C: Widget + 'static>(d: &mut ModalDialog<C>, area: Rect, theme: &Theme) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = d.build_layout(&mut taffy, theme, &mut node_map);
    #[allow(clippy::cast_possible_truncation, reason = "test geometry")]
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(area.width() as f32),
                height: taffy::AvailableSpace::Definite(area.height() as f32),
            },
        )
        .unwrap();
    d.apply_layout(&taffy, node, area.origin());
}

/// Paint the host's top layer over `window`, as the native and headless
/// hosts do.
fn present<C: Widget + 'static>(d: &mut ModalDialog<C>, window: Rect, theme: &Theme) -> Scene {
    let presence = PresenceCtx::default();
    let previous = presence.push_viewport(window);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    d.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    d.draw_overlay(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    presence.pop_viewport(previous);
    scene
}

fn laid_out_counting(open: bool) -> (ModalDialog<CountingContent>, Rc<Cell<usize>>) {
    let content = CountingContent::default();
    let events = content.events.clone();
    let mut d = ModalDialog::new("Title", content).open(open);
    lay_out(&mut d, WINDOW, &Theme::dark());
    (d, events)
}

#[test]
fn sheet_sizes_to_its_body_within_the_window() {
    // Case `the_sheet_centres_on_the_window`.
    {
        let spec = ModalSpec::new("Workspace invite").body_height(120.0);
        let l = lay(&spec, &Theme::dark());
        assert!((l.card.center().x - 800.0).abs() < 0.5);
        assert!((l.card.center().y - 500.0).abs() < 0.5);
    }

    // Case `the_sheet_is_as_tall_as_its_content_and_no_taller`.
    {
        let theme = Theme::dark();
        let short = lay(&ModalSpec::new("Short").body_height(40.0), &theme);
        let tall = lay(&ModalSpec::new("Tall").body_height(240.0), &theme);
        assert!(
            (tall.card.height() - short.card.height() - 200.0).abs() < 0.5,
            "200 px more content makes the sheet exactly 200 px taller: {} vs {}",
            tall.card.height(),
            short.card.height()
        );
        assert!((short.body.height() - 40.0).abs() < 0.5);
    }

    // Case `an_oversized_body_clamps_the_sheet_and_reports_the_overflow`.
    {
        let actions = [ModalAction::primary("Continue")];
        let spec = ModalSpec::new("Tall").actions(&actions).body_height(9000.0);
        let l = lay(&spec, &Theme::dark());
        assert!(l.body_overflows);
        assert!(l.card.y0 >= VIEWPORT.y0 && l.card.y1 <= VIEWPORT.y1);
        assert!(VIEWPORT.contains(l.actions[0].center()), "footer on screen");
        assert!(!lay(&ModalSpec::new("Short").body_height(100.0), &Theme::dark()).body_overflows);
    }

    // Case `the_sheet_keeps_the_gutter_on_a_small_output`.
    {
        let small = Rect::new(0.0, 0.0, 420.0, 360.0);
        let spec = ModalSpec::new("Workspace invite").body_height(600.0);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let l = layout(small, &spec, &mut tr, &theme);
        let gutter = f64::from(theme.tokens.spacing.s6);
        assert!(l.card.x0 >= gutter - 0.5 && l.card.x1 <= small.x1 - gutter + 0.5);
        assert!(l.card.y0 >= gutter - 0.5 && l.card.y1 <= small.y1 - gutter + 0.5);
    }

    // Case `the_sheet_stops_growing_at_its_maximum`.
    {
        let wide = Rect::new(0.0, 0.0, 3840.0, 2160.0);
        let spec = ModalSpec::new("Workspace invite").body_height(200.0);
        let mut tr = TextRenderer::new();
        let l = layout(wide, &spec, &mut tr, &Theme::dark());
        assert!(l.card.width() <= MODAL_MAX_WIDTH + 0.5);
    }

    // Case `a_sheet_without_actions_has_no_footer`.
    {
        let actions = [ModalAction::primary("OK")];
        let theme = Theme::dark();
        let with = lay(
            &ModalSpec::new("T").actions(&actions).body_height(100.0),
            &theme,
        );
        let without = lay(&ModalSpec::new("T").body_height(100.0), &theme);
        assert!(without.actions.is_empty());
        assert!(without.card.height() < with.card.height());
    }

    // Case `body_width_is_what_the_layout_hands_back`.
    {
        let actions = [ModalAction::primary("Continue")];
        let cases = [
            ("dark", Theme::dark(), VIEWPORT),
            ("light", Theme::light(), VIEWPORT),
            ("text-1.5", Theme::dark().with_text_scale(1.5), VIEWPORT),
            ("ui-1.25", Theme::dark().with_ui_scale(1.25), VIEWPORT),
            (
                "small output",
                Theme::dark(),
                Rect::new(0.0, 0.0, 300.0, 240.0),
            ),
        ];
        for (name, theme, window) in cases {
            for max_width in [MODAL_MAX_WIDTH, 560.0] {
                let spec = ModalSpec::new("Title")
                    .actions(&actions)
                    .max_width(max_width)
                    .body_height(120.0);
                let mut tr = TextRenderer::new();
                let l = layout(window, &spec, &mut tr, &theme);
                let predicted = body_width(window, max_width, &theme);
                assert!(
                    (l.body.width() - predicted).abs() < 0.01,
                    "{name} @ {max_width}: predicted {predicted}, got {}",
                    l.body.width()
                );
                assert!((l.card.width() - card_width(window, max_width, &theme)).abs() < 0.01);
            }
        }
    }
}

#[test]
fn the_sheet_is_a_floating_panel_with_a_title() {
    let theme = Theme::dark();
    let spec = ModalSpec::new("Rename").body_height(60.0);
    let l = lay(&spec, &theme);
    // The title is a `Title`: its line box decides the title row.
    let title_line = f64::from(theme.text(TextRole::Title).line_height_px());
    assert!(l.title.height() >= title_line - 0.5);
    assert!(l.body.y0 >= l.title.y1, "the body starts under the title");
    // Painted as a floating Panel: the blurred material (a placeholder
    // image under the tint) rather than a flat fill.
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let recording = crate::backdrop::BackdropRecording::begin();
    crate::backdrop::layer_scene(&scene, 0);
    paint_backdrop(&mut scene, Affine::IDENTITY, &mut tr, &theme, &l, &spec);
    let regions = recording.finish();
    assert_eq!(regions.len(), 1, "the sheet is one material region");
    assert_eq!(regions[0].bounds, l.card, "and it is the sheet");
    assert_eq!(
        super::floating_surface::radius(&theme),
        18.0,
        "panel radius"
    );
}

#[test]
fn modal_controls_are_hittable_and_show_their_states() {
    // Case `a_closable_sheet_has_a_close_control_you_can_hit`.
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

    // Case `an_action_shows_hover_press_and_focus`.
    {
        let theme = Theme::dark();
        let paint = |a: ModalAction<'_>| {
            let actions = [a];
            let spec = ModalSpec::new("Invite someone")
                .actions(&actions)
                .body_height(80.0);
            let mut tr = TextRenderer::new();
            let l = layout(VIEWPORT, &spec, &mut tr, &theme);
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

    // Case `the_close_control_shows_hover_and_focus`.
    {
        let theme = Theme::light();
        let paint = |spec: &ModalSpec<'_>| {
            let mut tr = TextRenderer::new();
            let l = layout(VIEWPORT, spec, &mut tr, &theme);
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

#[test]
fn actions_follow_platform_order_inside_the_sheet() {
    use crate::platform::{Platform, PlatformConventions};

    // Case `linux_puts_the_primary_action_at_the_trailing_edge`.
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

    // Case `geometry_follows_platform_order_without_changing_indices`.
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

    // Case `every_action_is_a_regular_control_inside_the_sheet`.
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

    // Case `a_disabled_action_is_not_the_default_action`.
    {
        let actions = [ModalAction::primary("Join").enabled(false)];
        let l = lay(
            &ModalSpec::new("Join").actions(&actions).body_height(80.0),
            &Theme::dark(),
        );
        assert_eq!(l.primary, None);
    }
}

#[test]
fn scrim_and_sheet_route_clicks_that_hit_no_control() {
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

#[test]
fn modal_ink_and_action_labels_are_legible_on_every_theme() {
    for (name, theme) in themes() {
        let ground = crate::chrome::panel_ground(&theme);
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
            // A modal's destructive action is its confirmation step: filled.
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

#[test]
fn the_dialog_is_presented_in_the_top_layer_over_the_whole_window() {
    // Case `draw_paints_nothing_and_the_top_layer_paints_the_sheet`.
    {
        let mut d = ModalDialog::new("Title", TestContent).open(true);
        // Mounted in a pane, not the window.
        lay_out(
            &mut d,
            Rect::new(300.0, 100.0, 700.0, 500.0),
            &Theme::dark(),
        );
        let mut scene = Scene::new();
        d.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(scene.encoding().draw_tags.is_empty(), "draw paints nothing");

        let scene = present(&mut d, WINDOW, &Theme::dark());
        assert!(!scene.encoding().draw_tags.is_empty());
        assert_eq!(
            d.layout.as_ref().map(|l| l.scrim),
            Some(WINDOW),
            "the scrim covers the window, not the pane the dialog is mounted in"
        );
        assert!(
            (d.dialog_rect().center().x - WINDOW.center().x).abs() < 0.5,
            "the sheet is centred on the window"
        );
    }

    // Case `a_closed_dialog_paints_nothing`.
    {
        let mut d = ModalDialog::new("Title", TestContent).open(false);
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

    // Case `the_sheet_hugs_its_content`.
    {
        let mut short = ModalDialog::new("Title", CountingContent::default()).open(true);
        let mut tall = ModalDialog::new(
            "Title",
            CountingContent {
                height: 240.0,
                ..CountingContent::default()
            },
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
        let body = short.layout.as_ref().unwrap().body;
        assert!((short.content().get_rect().y0 - body.y0).abs() < 0.5);
    }

    // Case `large_text_is_reserved_before_the_first_paint`.
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

#[test]
fn dialog_accessibility_exposes_open_content_and_real_actions() {
    // Case `semantic_footer_uses_current_visibility_and_existing_actions`.
    {
        let fired = Rc::new(Cell::new(0));
        let callback = fired.clone();
        let mut dialog = ModalDialog::new("Review", TestContent)
            .open(true)
            .primary_action("Restore", move || callback.set(callback.get() + 1));
        let target = dialog.accessibility().unwrap().children[0]
            .action_target
            .clone()
            .unwrap();
        let mut doc = AutoCommit::new();
        let activate = crate::a11y::Action::Activate;
        assert!(dialog.perform_semantic_action(&target, &activate, &mut doc, &automerge::ROOT));
        assert_eq!(fired.get(), 1);
        dialog.set_open(false);
        assert!(dialog.accessibility().unwrap().children.is_empty());
        assert!(!dialog.perform_semantic_action(&target, &activate, &mut doc, &automerge::ROOT));
        dialog.set_open(true);
        dialog.primary_action = None;
        assert!(!dialog.perform_semantic_action(&target, &activate, &mut doc, &automerge::ROOT));
        assert!(
            !dialog.perform_semantic_action("modal.primary", &activate, &mut doc, &automerge::ROOT),
            "an absent action is not activatable"
        );
        assert_eq!(automerge::transaction::Transactable::pending_ops(&doc), 0);
    }

    // Case `role_name_state_and_close`.
    {
        let d = ModalDialog::new("Confirm delete", TestContent)
            .open(true)
            .on_dismiss(|| {});
        let node = Widget::accessibility(&d).unwrap();
        assert_eq!(node.role, crate::a11y::Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Confirm delete"));
        assert!(node.state.modal && !node.state.hidden);
        assert!(
            node.children
                .iter()
                .any(|c| c.name.as_deref() == Some("Close")),
            "a dialog with no Cancel exposes its close control"
        );
        let closed = ModalDialog::new("Confirm delete", TestContent).open(false);
        assert!(Widget::accessibility(&closed).unwrap().state.hidden);
        assert_eq!(Widget::children(&d).len(), 1);
    }

    // Case `closed_dialog_prunes_content_from_the_production_tree`.
    {
        use crate::accesskit_bridge::build_tree_update;
        use crate::widget::Button;
        use accesskit::Role as AccessKitRole;

        let mut d = ModalDialog::new(
            "Confirm",
            Button::new("modal-child", "Confirm action", None),
        )
        .open(true);
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

#[test]
fn dialog_dismisses_and_invokes_actions_by_key_and_pointer() {
    // Case `escape_and_scrim_dismiss`.
    {
        let dismissed = Rc::new(Cell::new(0));
        let flag = Rc::clone(&dismissed);
        let mut d = ModalDialog::new("Title", TestContent)
            .open(true)
            .on_dismiss(move || flag.set(flag.get() + 1));
        lay_out(&mut d, WINDOW, &Theme::dark());
        assert!(dispatch(&mut d, &key_press_event(NamedKey::Escape)));
        assert_eq!(dismissed.get(), 1);
        dispatch(&mut d, &cursor_at(Point::new(10.0, 10.0)));
        assert!(dispatch(&mut d, &left_press()), "swallowed");
        assert_eq!(dismissed.get(), 2, "a scrim click dismisses");
        let close = d.close_rect().expect("no Cancel: a close control");
        dispatch(&mut d, &cursor_at(close.center()));
        dispatch(&mut d, &left_press());
        assert_eq!(dismissed.get(), 3, "the close control dismisses");
        assert!(d.handle_detached(
            &key_press_event(NamedKey::Escape),
            1.0,
            Modifiers::default(),
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        assert_eq!(dismissed.get(), 4, "a painted host routes events too");
    }

    // Case `pointer_and_enter_fire_the_primary_action`.
    {
        let fired = Rc::new(Cell::new(0usize));
        let f = Rc::clone(&fired);
        let (mut d, _) = laid_out_counting(true);
        d.primary_action = Some(DialogAction {
            label: "Save".into(),
            on_click: Box::new(move || f.set(f.get() + 1)),
        });
        lay_out(&mut d, WINDOW, &Theme::dark());
        let p = d.primary_rect().center();
        dispatch(&mut d, &cursor_at(p));
        dispatch(&mut d, &left_press());
        assert_eq!(fired.get(), 1, "a click on the primary action fires it");
        dispatch(&mut d, &key_press_event(NamedKey::Enter));
        assert_eq!(fired.get(), 2, "Enter fires the primary action");
        d.content_mut().consume = true;
        dispatch(&mut d, &key_press_event(NamedKey::Enter));
        assert_eq!(fired.get(), 2, "content that consumes Enter suppresses it");
    }

    // Case `action_order_follows_platform_conventions`.
    {
        use crate::platform::{Platform, PlatformConventions};
        let mut d = ModalDialog::new("Title", TestContent)
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

#[test]
fn dialog_paints_destructive_and_hover_faces() {
    fn draw_data(destructive: bool, hover: bool, then_leave: bool) -> Vec<u32> {
        let mut d = ModalDialog::new("Delete?", TestContent)
            .open(true)
            .with_destructive(destructive)
            .primary_action("Delete", || {});
        lay_out(&mut d, WINDOW, &Theme::dark());
        if hover {
            let centre = d.primary_rect().center();
            dispatch(&mut d, &cursor_at(centre));
        }
        if then_leave {
            dispatch(
                &mut d,
                &WindowEvent::CursorLeft {
                    device_id: synth_device(),
                },
            );
        }
        present(&mut d, WINDOW, &Theme::dark())
            .encoding()
            .draw_data
            .clone()
    }
    let rest = draw_data(false, false, false);
    assert_ne!(draw_data(true, false, false), rest, "destructive differs");
    assert_ne!(draw_data(false, true, false), rest, "hover shows");
    assert_eq!(draw_data(false, true, true), rest, "leaving drops hover");
}

#[test]
fn open_dialog_captures_input_and_closed_dialog_is_inert() {
    // Case `modal_capture_reflects_open_state`.
    {
        let (d, _) = laid_out_counting(true);
        assert!(Widget::modal_capture(&d).is_some());
        let (d, _) = laid_out_counting(false);
        assert!(Widget::modal_capture(&d).is_none());
    }

    // Case `open_dialog_swallows_every_key`.
    {
        let (mut d, _) = laid_out_counting(true);
        assert!(dispatch(&mut d, &key_press_event(NamedKey::Tab)));
    }

    // Case `an_in_sheet_press_reaches_the_content`.
    {
        let (mut d, events) = laid_out_counting(true);
        let inside = d.content().get_rect().center();
        dispatch(&mut d, &cursor_at(inside));
        let before = events.get();
        assert!(dispatch(&mut d, &left_press()));
        assert!(events.get() > before);
    }

    // Case `closed_dialog_is_inert`.
    {
        let (mut d, events) = laid_out_counting(false);
        assert!(!dispatch(&mut d, &left_press()));
        assert_eq!(events.get(), 0);
    }
}
