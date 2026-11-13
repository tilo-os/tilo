//! The sheet's geometry, the two guarantees that made the shared chrome
//! exist (there is always a visible way out, and every label is legible),
//! and the retained dialog's input, accessibility and top-layer contract.

use super::*;
use crate::a11y::contrast_ratio;
use crate::widget::Text;
use std::cell::Cell;
use std::rc::Rc;
use vello_encoding::DrawTag;

const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1600.0, 1000.0);
const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

/// How the person reaches the window in these tests: the pointer, unless a
/// test sets another.
fn environment() -> crate::interaction::InteractionEnvironment {
    crate::interaction::InteractionEnvironment::default()
}

fn themes() -> Vec<(&'static str, Theme)> {
    vec![("dark", Theme::dark()), ("light", Theme::light())]
}

fn lay(spec: &ModalSpec<'_>, theme: &Theme) -> ModalLayout {
    let mut tr = TextRenderer::new();
    layout_for(VIEWPORT, spec, &mut tr, theme, environment())
}

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
    fn name(&self) -> &'static str {
        "TestContent"
    }
}

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
            size: Size::from_lengths(200.0, self.height),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    crate::rect_geometry!();
    fn handle_event(&mut self, _event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.events.set(self.events.get().saturating_add(1));
        *ctx.handled = self.consume;
    }
    fn name(&self) -> &'static str {
        "CountingContent"
    }
}

fn cursor_at(p: Point) -> WindowEvent {
    crate::synthetic_input::cursor_moved(p.x, p.y)
}

use crate::synthetic_input::mouse_press;

use crate::synthetic_input::named_key;

use crate::testing::dispatch;

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
fn a_coarse_pointer_grows_the_sheets_controls_to_a_fingers_target() {
    let _settled = crate::motion::testing::settled();
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
        crate::interaction::InteractionEnvironment::TOUCH,
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
        "a mouse needs no finger's room"
    );
    assert!(
        pointer.card.height() < touch.card.height(),
        "the sheet grows with them"
    );
}

#[test]
fn sheet_sizes_to_its_body_within_the_window() {
    let _settled = crate::motion::testing::settled();
    {
        let spec = ModalSpec::new("Workspace invite").body_height(120.0);
        let l = lay(&spec, &Theme::dark());
        assert!((l.card.center().x - 800.0).abs() < 0.5);
        assert!((l.card.center().y - 500.0).abs() < 0.5);
    }

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

    {
        let actions = [ModalAction::primary("Continue")];
        let spec = ModalSpec::new("Tall").actions(&actions).body_height(9000.0);
        let l = lay(&spec, &Theme::dark());
        assert!(l.body_overflows);
        assert!(l.card.y0 >= VIEWPORT.y0 && l.card.y1 <= VIEWPORT.y1);
        assert!(VIEWPORT.contains(l.actions[0].center()), "footer on screen");
        assert!(!lay(&ModalSpec::new("Short").body_height(100.0), &Theme::dark()).body_overflows);
    }

    {
        let small = Rect::new(0.0, 0.0, 420.0, 360.0);
        let spec = ModalSpec::new("Workspace invite").body_height(600.0);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let l = layout_for(small, &spec, &mut tr, &theme, environment());
        let gutter = f64::from(theme.tokens.spacing.s6);
        assert_eq!(l.card, small, "a compact modal fills the viewport");
        assert!(l.body.x0 >= gutter - 0.5 && l.body.x1 <= small.x1 - gutter + 0.5);
        assert!(l.title.y0 >= gutter - 0.5 && l.body.y1 <= small.y1 - gutter + 0.5);
    }

    {
        let wide = Rect::new(0.0, 0.0, 3840.0, 2160.0);
        let spec = ModalSpec::new("Workspace invite").body_height(200.0);
        let mut tr = TextRenderer::new();
        let l = layout_for(wide, &spec, &mut tr, &Theme::dark(), environment());
        assert!(l.card.width() <= MODAL_MAX_WIDTH + 0.5);
    }

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
                let l = layout_for(window, &spec, &mut tr, &theme, environment());
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
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let spec = ModalSpec::new("Rename").body_height(60.0);
    let l = lay(&spec, &theme);
    let title_line = f64::from(theme.text(TextRole::Title).line_height_px());
    assert!(l.title.height() >= title_line - 0.5);
    assert!(l.body.y0 >= l.title.y1, "the body starts under the title");
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
fn a_modal_scrim_runs_under_the_foreground_materials_recorded_backdrop() {
    let _settled = crate::motion::testing::settled();
    let window = Rect::new(0.0, 0.0, 1000.0, 700.0);
    for (name, theme) in themes() {
        let mut text = TextRenderer::new();
        let spec = ModalSpec::new("Foreground").body_height(160.0);
        let geometry = layout_for(window, &spec, &mut text, &theme, environment());
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
        assert_ne!(
            at(&below, 500, 350),
            at(&original, 500, 350),
            "{name}: the actual material samples dimmed background under the foreground"
        );
        assert_eq!(
            at(&below, 500, 350),
            at(&below, 20, 20),
            "{name}: the same dim runs under the dialog and outside it"
        );
    }
}

#[test]
fn compact_modals_fill_the_window_and_keep_their_controls_on_the_foreground_page() {
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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

#[test]
fn narrow_large_text_footer_stacks_full_actions_without_covering_the_body() {
    use crate::platform::{Platform, PlatformConventions};
    let _settled = crate::motion::testing::settled();
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
        let geometry = layout_for(window, &spec, &mut text, &theme, environment());
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
    let _settled = crate::motion::testing::settled();
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

    let _settled = crate::motion::testing::settled();
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

#[test]
fn scrim_and_sheet_route_clicks_that_hit_no_control() {
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
    {
        let mut d = ModalDialog::new("Title", TestContent).open(true);
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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

    {
        let dismissed = Rc::new(Cell::new(0));
        let flag = Rc::clone(&dismissed);
        let mut d = ModalDialog::new("Title", TestContent)
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
        d.content_mut().consume = true;
        dispatch(&mut d, &named_key(NamedKey::Enter));
        assert_eq!(fired.get(), 2, "content that consumes Enter suppresses it");
    }

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
    let _settled = crate::motion::testing::settled();
    let rest = draw_data(false, false, false);
    assert_ne!(draw_data(true, false, false), rest, "destructive differs");
    assert_ne!(draw_data(false, true, false), rest, "hover shows");
    assert_eq!(draw_data(false, true, true), rest, "leaving drops hover");
}

#[test]
fn open_dialog_captures_input_and_closed_dialog_is_inert() {
    let _settled = crate::motion::testing::settled();
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
        let before = events.get();
        assert!(dispatch(&mut d, &mouse_press()));
        assert!(events.get() > before);
    }

    {
        let (mut d, events) = laid_out_counting(false);
        assert!(!dispatch(&mut d, &mouse_press()));
        assert_eq!(events.get(), 0);
    }
}

fn wheel(dy: f32) -> WindowEvent {
    crate::synthetic_input::mouse_wheel(0.0, dy)
}

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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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

/// A strip the caller passes as the sheet's header stays where it is while
/// the body scrolls under it: the wheel moves what the body holds, never the
/// tabs a person is switching between.
#[test]
fn a_header_stays_put_while_the_body_scrolls_under_it() {
    let _settled = crate::motion::testing::settled();
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
        CountingContent {
            height: 2000.0,
            ..CountingContent::default()
        },
    )
    .with_header(CountingContent {
        height: 40.0,
        ..CountingContent::default()
    })
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

/// A sheet whose body is too short for what it holds ends above the row
/// the window's edge falls in, and the rows below are reached by scrolling:
/// a person never sees half a row under the footer.
#[test]
fn an_overflowing_body_ends_above_the_row_it_cuts() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let block = |height: f32| {
        Box::new(
            crate::widget::Container::new()
                .with_no_padding()
                .with_size(200.0, height),
        ) as Box<dyn Widget>
    };
    let pane = crate::widget::Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(block(300.0))
        .push(block(300.0))
        .push(block(300.0));
    let content = crate::widget::Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(block(60.0))
        .push(Box::new(pane));
    let mut d = ModalDialog::new("Settings", content).open(true);
    lay_out(&mut d, WINDOW, &theme);
    let body = d.body_rect();
    let rows = d.content().children();
    let pane_rows: Vec<Rect> = rows[1]
        .children()
        .iter()
        .map(|row| row.get_rect())
        .collect();
    assert_eq!(pane_rows.len(), 3);
    assert!(
        body.y1 < d.content().get_rect().y1 - 0.5,
        "the body is shorter than its content: {:?}",
        body
    );
    assert!(
        body.y1 >= rows[0].get_rect().y1 - 0.5,
        "the header row stays whole"
    );
    assert!(
        body.y1 <= pane_rows[1].y0 + 0.5,
        "the body ends above the row it would cut: body {:?}, row {:?}",
        body,
        pane_rows[1]
    );
    for row in &pane_rows {
        assert!(
            body.y1 <= row.y0 + 0.5 || body.y1 >= row.y1 - 0.5,
            "no row is cut through: body {:?}, row {row:?}",
            body
        );
    }
    dispatch(&mut d, &cursor_at(body.center()));
    for _ in 0..400 {
        dispatch(&mut d, &wheel(-3.0));
    }
    lay_out(&mut d, WINDOW, &theme);
    assert!(
        d.scroll_offset() > 0.0,
        "the rows the body holds back are scrolled to"
    );
    assert!(
        (d.content().get_rect().y1 - d.body_rect().y1).abs() < 0.5,
        "the last row stands whole at the body's edge: {:?}",
        d.body_rect()
    );
}

#[test]
fn a_three_way_question_stands_in_the_platform_order() {
    use crate::platform::{Platform, PlatformConventions};
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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

    d.set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Windows));
    lay_out(&mut d, WINDOW, &Theme::dark());
    assert_eq!(d.keys.first().map(|k| k.slot), Some(Slot::Secondary));

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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
    let overwrite = layout.actions[1].center();
    assert_eq!(sheet.handle(&cursor_at(overwrite), 1.0), None);
    assert_eq!(sheet.handle(&mouse_press(), 1.0), None);
    let release = crate::synthetic_input::mouse_release();
    assert_eq!(sheet.handle(&release, 1.0), Some(SheetResponse::Action(1)));
    sheet.handle(&cursor_at(Point::new(5.0, 5.0)), 1.0);
    assert_eq!(sheet.handle(&mouse_press(), 1.0), None);
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
    let _settled = crate::motion::testing::settled();
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
                size: Size::from_lengths(1280.0, 800.0),
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
    let _settled = crate::motion::testing::settled();
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

/// A dialog a click opens arrives from its centre over its fading scrim
/// (`motion::Appear`) and leaves the same way, quicker; one built open is at
/// rest from its first paint, so a host that rebuilds its tree never replays
/// the entrance; a key opens it at once.
#[test]
fn a_dialog_arrives_from_its_centre_and_a_rebuilt_one_does_not_replay_it() {
    use crate::motion::note_source;
    use crate::time::testing::with_monotonic_time;
    use crate::tokens::MotionSource;
    let theme = Theme::dark();
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);

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
    use crate::motion::{Appearance, note_source};
    use crate::time::testing::with_monotonic_time;
    use crate::tokens::MotionSource;
    let theme = Theme::dark();
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
    let appearance = Appearance::shared();
    let build = |open: bool| {
        ModalDialog::new("Settings", TestContent)
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
    use crate::time::testing::with_monotonic_time;
    let theme = Theme::dark().with_reduce_motion(true);
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
    let mut d = ModalDialog::new("Settings", TestContent);
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
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
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

#[test]
fn dismissal_is_a_published_view_action_while_the_dialog_is_open() {
    let (dismissed, dismiss) = counter();
    let mut d = ModalDialog::new("Delete rule?", TestContent)
        .on_dismiss(dismiss)
        .with_test_id("firewall-delete");
    let doc = AutoCommit::new();
    assert!(
        crate::resources::resources(&d, &doc).is_empty(),
        "a closed dialog offers no way to close it"
    );
    d.set_open(true);
    let resources = crate::resources::resources(&d, &doc);
    let [close] = resources.as_slice() else {
        panic!("one close control, not {}", resources.len());
    };
    assert_eq!(close.id.0, "tilo.control:firewall-delete.close");
    let [action] = close.actions.as_slice() else {
        panic!("one action, not {}", close.actions.len());
    };
    assert_eq!(action.id, "invoke");
    assert_eq!(action.kind, tilo_actions::OperationKind::View);
    assert!(
        crate::widget::controls::control_action_bindings(&d)
            .iter()
            .any(|binding| matches!(
                &binding.kind,
                crate::widget::ControlBindingKind::Action(address)
                    if address.resource == close.id.0
            )),
        "the close control is bound to the action, not a private callback"
    );
    let mut doc = AutoCommit::new();
    crate::widget::ResourceOperation::local(
        tilo_actions::InvocationId::new(),
        tilo_actions::OperationKind::View,
        super::super::activation::NATIVE_INVOKE,
        &serde_json::to_vec("firewall-delete.close").unwrap(),
    )
    .start(
        &mut d,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    )
    .unwrap()
    .expect("the open dialog answers its close control");
    assert_eq!(dismissed.get(), 1);
}

#[test]
fn an_agent_closes_a_hosted_dialog_through_its_published_close_control() {
    let (dismissed, dismiss) = counter();
    let dialog = ModalDialog::new("Delete rule?", TestContent)
        .with_test_id("firewall-delete")
        .on_dismiss(dismiss)
        .open(true);
    let mut h = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        crate::widget::Column::new().push(Box::new(dialog)),
        (800, 600),
        Theme::dark(),
    );
    h.invoke_published(
        "tilo.control:firewall-delete.close",
        "invoke",
        &serde_json::Map::new(),
    )
    .expect("the published close control is reachable");
    assert_eq!(dismissed.get(), 1);
}

#[test]
fn an_agent_presses_a_dialog_footer_through_its_published_controls() {
    let (saved, save) = counter();
    let (cancelled, cancel) = counter();
    let dialog = ModalDialog::new("Rename", TestContent)
        .with_test_id("rename")
        .primary_action("Save", save)
        .secondary_action("Cancel", cancel)
        .open(true);
    let mut h = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        crate::widget::Column::new().push(Box::new(dialog)),
        (800, 600),
        Theme::dark(),
    );
    let resources = h.resources();
    let kind = |id: &str| {
        resources
            .iter()
            .find(|resource| resource.id.0 == id)
            .map(|resource| resource.actions[0].kind)
    };
    assert_eq!(
        kind("tilo.control:rename.primary"),
        Some(tilo_actions::OperationKind::Effect)
    );
    assert_eq!(
        kind("tilo.control:rename.secondary"),
        Some(tilo_actions::OperationKind::View)
    );
    let none = serde_json::Map::new();
    h.invoke_published("tilo.control:rename.secondary", "invoke", &none)
        .expect("Cancel is reachable");
    assert_eq!(cancelled.get(), 1);
    h.invoke_published("tilo.control:rename.primary", "invoke", &none)
        .expect("Save is reachable");
    assert_eq!(saved.get(), 1);
}

#[test]
fn a_closed_sheet_names_no_target_and_an_open_one_is_where_it_shows() {
    let _settled = crate::motion::testing::settled();
    use crate::widget::Column;
    let sheet = |open: bool| {
        let mut dialog = ModalDialog::new("About", Column::new().push(Box::new(Text::new("Tilo"))))
            .with_test_id("about-sheet");
        dialog.set_open(open);
        crate::headless::FrameHarness::for_root(
            crate::State::new(),
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
