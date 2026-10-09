//! The sheet's geometry, the two guarantees that made the shared chrome
//! exist (there is always a visible way out, and every label is legible),
//! and the retained dialog's input, accessibility and top-layer contract.

use super::*;
use crate::a11y::contrast_ratio;
use crate::widget::Text;
use std::cell::Cell;
use std::rc::Rc;
use vello_encoding::DrawTag;
use winit::event::MouseButton;

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
        taffy.widget_leaf(Style::default(), node_map, self)
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
        let style = Style {
            size: Size {
                width: Dimension::length(200.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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

fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

use crate::synthetic_input::mouse_press;

use crate::synthetic_input::named_key;

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
            state: crate::widget::Edits::lend(&mut doc),
            path: &root,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &tree,
        },
    );
    handled
}

/// Run the retained passes with the dialog's node filling `area`, through
/// the production solver (measure functions and layout adjustments), as a
/// host does.
fn lay_out<C: Widget + 'static>(d: &mut ModalDialog<C>, area: Rect, theme: &Theme) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = d.build_layout(&mut taffy, theme, &mut node_map);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let mut bindings = crate::bound_dispatch::BoundNodeMap::default();
    #[allow(clippy::cast_possible_truncation, reason = "test geometry")]
    let available = Size {
        width: taffy::AvailableSpace::Definite(area.width() as f32),
        height: taffy::AvailableSpace::Definite(area.height() as f32),
    };
    // SAFETY: `node_map` describes the tree just built from `d`, which this
    // function holds exclusively.
    unsafe {
        bindings.rebuild(d, &taffy, node, &node_map);
        crate::widget::layout::compute_widget_layout(
            &mut taffy, node, available, &node_map, &bindings, &mut text,
        )
        .unwrap();
    }
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

    // Case `a_small_output_becomes_a_page_and_keeps_the_content_gutter`.
    {
        let small = Rect::new(0.0, 0.0, 420.0, 360.0);
        let spec = ModalSpec::new("Workspace invite").body_height(600.0);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let l = layout(small, &spec, &mut tr, &theme);
        let gutter = f64::from(theme.tokens.spacing.s6);
        assert_eq!(l.card, small, "a compact modal fills the viewport");
        assert!(l.body.x0 >= gutter - 0.5 && l.body.x1 <= small.x1 - gutter + 0.5);
        assert!(l.title.y0 >= gutter - 0.5 && l.body.y1 <= small.y1 - gutter + 0.5);
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
fn a_modal_scrim_does_not_dim_the_foreground_materials_recorded_backdrop() {
    let window = Rect::new(0.0, 0.0, 1000.0, 700.0);
    for (name, theme) in themes() {
        let mut text = TextRenderer::new();
        let spec = ModalSpec::new("Foreground").body_height(160.0);
        let geometry = layout(window, &spec, &mut text, &theme);
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.tokens.colors.surfaces.content,
            None,
            &window,
        );
        let original = scene.clone();
        let recording = crate::backdrop::BackdropRecording::begin();
        crate::backdrop::layer_scene(&scene, 0);
        paint_backdrop(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &geometry,
            &spec,
        );
        let surfaces = recording.finish();
        let surface = surfaces
            .first()
            .expect("the real floating material records its backdrop");
        assert_eq!(surface.bounds, geometry.card);
        let mut renderer =
            crate::offscreen::OffscreenRenderer::try_new_with_cpu_preprocessing(1000, 700)
                .expect("the production renderer can observe the backdrop");
        let original = renderer.render_rgba(&original);
        let below = renderer.render_rgba(&surface.below);
        let at = |pixels: &[u8], x: usize, y: usize| {
            let offset = y.saturating_mul(1000).saturating_add(x).saturating_mul(4);
            pixels
                .get(offset..offset.saturating_add(4))
                .expect("a sample inside the image")
                .to_vec()
        };
        assert_eq!(
            at(&below, 500, 350),
            at(&original, 500, 350),
            "{name}: the actual material samples undimmed background under the foreground"
        );
        assert_ne!(
            at(&below, 20, 20),
            at(&original, 20, 20),
            "{name}: the same scrim still dims the page outside the dialog"
        );
    }
}

#[test]
fn compact_modals_fill_the_window_and_keep_their_controls_on_the_foreground_page() {
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
            let geometry = layout(window, &spec, &mut text, &theme);
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
            let recording = crate::backdrop::BackdropRecording::begin();
            crate::backdrop::layer_scene(&scene, 0);
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
fn narrow_large_text_footer_stacks_full_actions_without_covering_the_body() {
    use crate::platform::{Platform, PlatformConventions};
    let window = Rect::new(0.0, 0.0, 360.0, 640.0);
    let actions = [
        ModalAction::alternate("Keep separate"),
        ModalAction::secondary("Cancel"),
        ModalAction::danger("Merge contacts"),
    ];
    for (_, base) in themes() {
        let theme = base.with_text_scale(2.0);
        let mut text = crate::font_catalog::fixture_renderer();
        text.set_text_scale(theme.text_scale);
        let spec = ModalSpec::new("Merge contacts")
            .platform_conventions(PlatformConventions::new(Platform::Linux))
            .actions(&actions)
            .body_height(600.0);
        let geometry = layout(window, &spec, &mut text, &theme);
        assert_eq!(geometry.actions.len(), 3);
        assert!(geometry.body_overflows, "the real body must scroll");
        assert_eq!(geometry.primary, Some(1), "Enter remains the safe Cancel");
        for (index, rect) in geometry.actions.iter().enumerate() {
            assert!(rect.x0 >= geometry.card.x0 && rect.x1 <= geometry.card.x1);
            assert!(rect.y0 >= geometry.body.y1 && rect.y1 <= geometry.card.y1);
            assert!((rect.height() - action_height(&theme)).abs() < 0.5);
            let (label_width, _) = text.measure_style(
                actions[index].label,
                None,
                &ControlSize::Regular.label_style(&theme),
            );
            assert!(
                f64::from(label_width) + 2.0 * f64::from(theme.tokens.spacing.s4)
                    <= rect.width() + 0.5,
                "the complete action label fits: {}",
                actions[index].label,
            );
        }
        let gap = f64::from(theme.tokens.spacing.s2);
        assert!(geometry.actions[0].y1 + gap <= geometry.actions[1].y0 + 0.5);
        assert!(geometry.actions[1].y1 + gap <= geometry.actions[2].y0 + 0.5);
        assert!(geometry.body.height() > 0.0);
    }
}

#[test]
fn retained_narrow_dialog_footer_targets_follow_the_stacked_geometry() {
    let window = Rect::new(0.0, 0.0, 360.0, 640.0);
    let theme = Theme::dark().with_text_scale(2.0);
    let confirmed = Rc::new(Cell::new(false));
    let cancelled = Rc::new(Cell::new(false));
    let yes = confirmed.clone();
    let no = cancelled.clone();
    let mut dialog = ModalDialog::new("Merge contacts", TestContent)
        .open(true)
        .with_destructive(true)
        .primary_action("Merge contacts", move || yes.set(true))
        .secondary_action("Cancel", move || no.set(true));
    lay_out(&mut dialog, window, &theme);
    let _ = present(&mut dialog, window, &theme);
    let secondary = dialog.secondary_rect();
    let primary = dialog.primary_rect();
    assert!(secondary.width() > 0.0 && primary.width() > 0.0);
    assert!(
        secondary.y1 <= primary.y0,
        "the safe answer precedes the primary"
    );
    for key in &dialog.keys {
        let actual = key.get_rect();
        assert_eq!(actual, dialog.slot_rect(key.slot));
        assert!(actual.x0 >= window.x0 && actual.x1 <= window.x1);
        assert!(actual.y0 >= window.y0 && actual.y1 <= window.y1);
    }
    assert!(dispatch(&mut dialog, &named_key(NamedKey::Enter)));
    assert!(
        cancelled.get(),
        "Enter cancels the destructive confirmation"
    );
    assert!(!confirmed.get());
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
        assert!(dialog.perform_semantic_action(
            &target,
            &activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(fired.get(), 1);
        dialog.set_open(false);
        assert!(dialog.accessibility().unwrap().children.is_empty());
        assert!(!dialog.perform_semantic_action(
            &target,
            &activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        dialog.set_open(true);
        dialog.primary_action = None;
        assert!(!dialog.perform_semantic_action(
            &target,
            &activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(
            !dialog.perform_semantic_action(
                "modal.primary",
                &activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ),
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
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(dismissed.get(), 4, "a painted host routes events too");
    }

    // Case `pointer_and_enter_fire_the_primary_action`.
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
        d.content_mut().consume = true;
        dispatch(&mut d, &named_key(NamedKey::Enter));
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
                    device_id: winit::event::DeviceId::dummy(),
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
        assert!(dispatch(&mut d, &named_key(NamedKey::Tab)));
    }

    // Case `an_in_sheet_press_reaches_the_content`.
    {
        let (mut d, events) = laid_out_counting(true);
        let inside = d.content().get_rect().center();
        dispatch(&mut d, &cursor_at(inside));
        let before = events.get();
        assert!(dispatch(&mut d, &mouse_press()));
        assert!(events.get() > before);
    }

    // Case `closed_dialog_is_inert`.
    {
        let (mut d, events) = laid_out_counting(false);
        assert!(!dispatch(&mut d, &mouse_press()));
        assert_eq!(events.get(), 0);
    }
}

fn wheel(dy: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, dy),
        phase: winit::event::TouchPhase::Moved,
    }
}

/// The buttons a laid-out dialog's footer focus nodes expose.
fn footer_buttons<C: Widget + 'static>(d: &ModalDialog<C>) -> Vec<crate::a11y::AccessNode> {
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
    use crate::headless::Framed;
    use crate::widget::{Column, TextInput};
    let (cancelled, cancel) = counter();
    let (connected, connect) = counter();
    let content = Column::new()
        .push(Box::new(TextInput::local().with_placeholder("Name")))
        .push(Box::new(TextInput::local().with_placeholder("Host")));
    let dialog = ModalDialog::new("Connection", content)
        .primary_action("Connect", connect)
        .secondary_action("Cancel", cancel)
        .open(true);
    let mut framed = Framed::new(crate::State::new(), dialog, (800, 600), Theme::dark());
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
    // Tab runs from the fields into the footer, in the order it stands.
    let harness = Framed::harness_mut(&mut framed);
    harness.tab(false);
    harness.tab(false);
    let keys: Vec<(Slot, bool)> = framed.keys.iter().map(|k| (k.slot, k.focused)).collect();
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

#[test]
fn a_tall_body_scrolls_inside_a_sheet_that_takes_the_window() {
    let theme = Theme::dark();
    let mut d = ModalDialog::new(
        "Shortcuts",
        CountingContent {
            height: 2000.0,
            ..CountingContent::default()
        },
    )
    .open(true);
    lay_out(&mut d, WINDOW, &theme);
    present(&mut d, WINDOW, &theme);
    let layout = d.layout.clone().expect("laid out");
    assert!(layout.body_overflows);
    // Too tall to stand below the header row, the sheet takes the whole
    // window inside the gutter, footer and all.
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

#[test]
fn a_three_way_question_stands_in_the_platform_order() {
    use crate::platform::{Platform, PlatformConventions};
    let (saved, save) = counter();
    let (discarded, discard) = counter();
    let (cancelled, cancel) = counter();
    let mut d = ModalDialog::new("Save changes to notes.md?", TestContent)
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
    // Focus opens on Cancel and reaches Don't save last, wherever it stands.
    lay_out(&mut d, WINDOW, &Theme::dark());
    let order: Vec<Slot> = d.keys.iter().map(|k| k.slot).collect();
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

#[test]
fn enter_in_a_destructive_confirmation_is_the_safe_answer() {
    let (erased, erase) = counter();
    let (cancelled, cancel) = counter();
    let mut d = ModalDialog::new("Erase disk?", TestContent)
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

    // Where the platform stands the primary action first, the keyboard
    // still starts on Cancel.
    d.set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Windows));
    lay_out(&mut d, WINDOW, &Theme::dark());
    assert_eq!(d.keys.first().map(|k| k.slot), Some(Slot::Secondary));

    // The painted sheet answers the same way.
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

#[test]
fn a_gate_may_read_state_the_content_writes_elsewhere() {
    // A form whose fields write into an app's shared core is gated on that
    // core, and its footer button is announced with the state it is painted
    // with.
    let valid = Rc::new(Cell::new(false));
    let (saved, save) = counter();
    let gate = Rc::clone(&valid);
    let mut d = ModalDialog::new("Back up vault", TestContent)
        .primary_action("Choose file…", save)
        .secondary_action("Cancel", || {})
        .primary_enabled_if(move |_| gate.get())
        .open(true);
    lay_out(&mut d, WINDOW, &Theme::dark());
    present(&mut d, WINDOW, &Theme::dark());
    let primary = |d: &ModalDialog<TestContent>| {
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

#[test]
fn a_disabled_or_gated_primary_action_is_inert_and_announced() {
    let (fired, fire) = counter();
    let mut d = ModalDialog::new("Erase disk", TestContent)
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

    // A gate reads the content every time.
    let mut gated = ModalDialog::new("Erase disk", crate::widget::PromptBody::new("Name", "sdb"))
        .primary_action("Erase", || {})
        .primary_enabled_if(|body| body.value() == "sdb")
        .open(true);
    assert!(!gated.primary_enabled());
    gated.content_mut().set_value("sdb");
    assert!(gated.primary_enabled());
}

#[test]
fn a_long_title_wraps_to_two_lines() {
    let theme = Theme::dark();
    let short = lay(&ModalSpec::new("Rename"), &theme);
    let long = lay(
        &ModalSpec::new(
            "Move the 14 selected photos from the Summer in Lisbon album to the shared family album?",
        )
        .max_width(360.0),
        &theme,
    );
    let line = f64::from(theme.text(TextRole::Title).line_height_px());
    assert!(
        long.title.height() >= line * 2.0 - 0.5,
        "two lines: {:?}",
        long.title
    );
    assert!(short.title.height() < line * 2.0, "one line stays one line");
    assert!(long.body.y0 > long.title.y1, "the body follows the title");
}

#[test]
fn an_open_list_in_the_content_takes_the_press_before_the_scrim() {
    /// Content whose list is open: it captures the pointer and takes every
    /// press.
    struct OpenList(CountingContent);
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
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.0
                .draw(scene, transform, text_renderer, theme, presence);
        }
        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
    let (dismissed, dismiss) = counter();
    let events = Rc::new(Cell::new(0));
    let mut d = ModalDialog::new(
        "Settings",
        OpenList(CountingContent {
            events: Rc::clone(&events),
            consume: true,
            ..CountingContent::default()
        }),
    )
    .on_dismiss(dismiss)
    .open(true);
    lay_out(&mut d, WINDOW, &Theme::dark());
    // Below the sheet, where the list hangs.
    let below = Point::new(d.dialog_rect().center().x, d.dialog_rect().y1 + 20.0);
    dispatch(&mut d, &cursor_at(below));
    let before = events.get();
    dispatch(&mut d, &mouse_press());
    assert!(events.get() > before, "the list sees the press");
    assert_eq!(
        dismissed.get(),
        0,
        "the press is the list's, not the scrim's"
    );
}

#[test]
fn an_immediate_sheet_paints_hit_tests_and_answers_without_a_renderer() {
    let theme = Theme::dark();
    let mut sheet = ModalSheet::new();
    let actions = [
        ModalAction::primary("Reload"),
        ModalAction::alternate_danger("Overwrite"),
        ModalAction::secondary("Cancel"),
    ];
    let spec = ModalSpec::new("notes.md changed on disk")
        .actions(&actions)
        .closable(false)
        .dismiss_on_scrim(false);
    let mut scene = Scene::new();
    sheet.paint_message(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        WINDOW,
        &spec,
        "Something else wrote this file after you opened it.",
    );
    let layout = sheet.layout().cloned().expect("the paint keeps its layout");
    assert!(
        layout.body.height() > 0.0,
        "the message is measured into the body"
    );
    // The pointer: press and release on an action answer with it.
    let overwrite = layout.actions[1].center();
    assert_eq!(sheet.handle(&cursor_at(overwrite), 1.0), None);
    assert_eq!(sheet.handle(&mouse_press(), 1.0), None);
    let release = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    };
    assert_eq!(sheet.handle(&release, 1.0), Some(SheetResponse::Action(1)));
    // The scrim swallows without answering when the spec says so.
    sheet.handle(&cursor_at(Point::new(5.0, 5.0)), 1.0);
    assert_eq!(sheet.handle(&mouse_press(), 1.0), None);
    // The keyboard: Enter is the default, Tab walks the actions in the
    // order they stand, Escape dismisses.
    assert_eq!(
        sheet.handle(&named_key(NamedKey::Enter), 1.0),
        Some(SheetResponse::Action(0))
    );
    sheet.handle(&named_key(NamedKey::Tab), 1.0);
    let first = sheet.focused_action().expect("Tab focuses an action");
    let leftmost = (0..3)
        .min_by(|a, b| layout.actions[*a].x0.total_cmp(&layout.actions[*b].x0))
        .unwrap();
    assert_eq!(first, leftmost, "Tab starts at the leading action");
    assert_eq!(
        sheet.handle(&named_key(NamedKey::Enter), 1.0),
        Some(SheetResponse::Action(first))
    );
    assert_eq!(
        sheet.handle(&named_key(NamedKey::Escape), 1.0),
        Some(SheetResponse::Dismiss)
    );
}

#[test]
fn footer_actions_are_addressable_by_name() {
    let mut d = ModalDialog::new("Delete note?", TestContent)
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

#[test]
fn space_flips_a_focused_switch_and_enter_is_the_sheets_default() {
    use crate::headless::Framed;
    use crate::widget::{Column, TextInput, Toggle};
    let (saved, save) = counter();
    let on = crate::LocalState::new(false);
    let content = Column::new()
        .push(Box::new(TextInput::local().with_placeholder("Name")))
        .push(Box::new(
            Toggle::from_state(&on).with_accessible_name("Sync"),
        ));
    let dialog = ModalDialog::new("Account", content)
        .primary_action("Save", save)
        .secondary_action("Cancel", || {})
        .open(true);
    let mut framed = Framed::new(crate::State::new(), dialog, (800, 600), Theme::dark());
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

    // Return is the sheet's default answer, as on macOS, even on a switch.
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
    use crate::headless::Framed;
    use crate::widget::{Column, TextInput};
    let (connected, connect) = counter();
    let content = Column::new().push(Box::new(TextInput::local().with_placeholder("Name")));
    let dialog = ModalDialog::new("Connection", content)
        .with_test_id("connect")
        .primary_action("Connect", connect)
        .secondary_action("Cancel", || {})
        .open(true);
    let mut framed = Framed::new(crate::State::new(), dialog, (800, 600), Theme::dark());
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
    // Tab onto a footer action and the reader is on that button.
    let mut focused = None;
    for _ in 0..3 {
        Framed::harness_mut(&mut framed).tab(false);
        focused = Framed::harness(&framed)
            .focused_access_node()
            .filter(|node| node.role == crate::a11y::Role::Button);
        if focused.is_some() {
            break;
        }
    }
    let focused = focused.expect("Tab reaches a footer button");
    assert_eq!(focused.name.as_deref(), Some("Cancel"));
    assert!(focused.state.focused, "the reader is on the focused action");
    // The footer's address still operates the action.
    Framed::harness_mut(&mut framed)
        .click_named("connect.primary")
        .expect("the primary action has an address");
    assert_eq!(connected.get(), 1, "its address still operates the action");
    // A reader activating the footer button runs the action itself.
    let primary = footer_buttons(&framed)
        .into_iter()
        .find(|node| node.name.as_deref() == Some("Connect"))
        .and_then(|node| node.action_target)
        .expect("the primary button has a semantic address");
    Framed::harness_mut(&mut framed)
        .perform_semantic_named(
            "connect.primary",
            Some(&primary),
            &crate::a11y::Action::Activate,
        )
        .expect("the reader's activation lands");
    assert_eq!(
        connected.get(),
        2,
        "the button's activation runs the action"
    );
}

#[test]
fn an_unpainted_sheet_centres_on_the_window_not_its_parent() {
    // A dialog mounted beside a sidebar is centred on the window it will be
    // painted over, so its content is placed and hit there from the first
    // frame instead of under the parent's centre.
    let mut taffy = TaffyTree::<()>::new();
    let pane = taffy
        .new_leaf(Style {
            flex_grow: 1.0,
            ..Default::default()
        })
        .unwrap();
    let root = taffy
        .new_with_children(
            Style {
                padding: taffy::Rect {
                    left: taffy::LengthPercentage::length(140.0),
                    right: taffy::LengthPercentage::length(0.0),
                    top: taffy::LengthPercentage::length(0.0),
                    bottom: taffy::LengthPercentage::length(0.0),
                },
                size: Size {
                    width: Dimension::length(1280.0),
                    height: Dimension::length(800.0),
                },
                ..Default::default()
            },
            &[pane],
        )
        .unwrap();
    taffy
        .compute_layout(root, taffy::Size::max_content())
        .unwrap();
    assert_eq!(
        tree_window(&taffy, pane),
        Some(Rect::new(0.0, 0.0, 1280.0, 800.0)),
        "the pane's window is the root of its tree"
    );
}

#[test]
fn what_stands_behind_an_open_sheet_never_takes_its_input() {
    // A page lists its content before its dialog. A greedy sibling (a table
    // that takes every press) must not see the press meant for the sheet.
    use crate::headless::Framed;
    use crate::widget::Column;
    let behind = CountingContent {
        consume: true,
        height: 500.0,
        ..CountingContent::default()
    };
    let seen = behind.events.clone();
    let (confirmed, confirm) = counter();
    let page = Column::new().push(Box::new(behind)).push(Box::new(
        ModalDialog::new("Delete this rule?", TestContent)
            .with_test_id("confirm")
            .primary_action("Delete rule", confirm)
            .secondary_action("Cancel", || {})
            .open(true),
    ));
    let mut framed = Framed::new(crate::State::new(), page, (800, 600), Theme::dark());
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    let before = seen.get();
    Framed::harness_mut(&mut framed)
        .click_named("confirm.primary")
        .expect("the action has an address");
    assert_eq!(confirmed.get(), 1, "the press reaches the sheet's action");
    assert_eq!(seen.get(), before, "and nothing behind the sheet sees it");
}
