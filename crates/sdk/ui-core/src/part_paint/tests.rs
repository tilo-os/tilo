use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Shape, Size};
use vello::peniko::Color;
use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

use super::*;
use crate::text_render::TextRenderer;
use crate::time::Duration;
use crate::widget::{Button, Widget};

/// A painter set that draws a button as a triangle pointing up, counts its calls, springs between
/// states and leaves every other part alone.
struct Triangles {
    calls: AtomicUsize,
}

impl Triangles {
    fn set() -> (PainterSet, Arc<Self>) {
        let painter = Arc::new(Self {
            calls: AtomicUsize::new(0),
        });
        (PainterSet::new("test.triangles", painter.clone()), painter)
    }

    /// A triangle on the rect's foot, a tenth of its width in from each side, its apex at the top
    /// centre.
    fn triangle(size: Size) -> BezPath {
        let mut path = BezPath::new();
        path.move_to((size.width / 2.0, 0.0));
        path.line_to((size.width * 0.9, size.height));
        path.line_to((size.width * 0.1, size.height));
        path.close_path();
        path
    }
}

impl PartPainter for Triangles {
    fn paint(
        &self,
        part: Part,
        size: Size,
        state: PartState,
        progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        if part != Part::ButtonContainer {
            return None;
        }
        self.calls.fetch_add(1, Ordering::Relaxed);
        let shape = Self::triangle(size);
        let mut scene = PartScene::new();
        let lift = if state.now.contains(States::PRESSED) {
            progress
        } else {
            0.0
        };
        scene.fill(
            shape.clone(),
            Brush::Solid(tokens.fill.with_alpha(1.0 - lift * 0.5)),
        );
        Some(PaintedPart::new(scene).with_hit(shape))
    }

    fn transition(&self, _part: Part, _from: States, to: States) -> Transition {
        if to.contains(States::PRESSED) {
            Transition::Spring {
                response: 0.2,
                damping: 0.5,
            }
        } else {
            Transition::Inherit
        }
    }
}

fn draw(button: &mut Button, theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    button.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        &crate::presence::PresenceCtx::default(),
    );
    scene
}

fn moved_to(button: &mut Button, at: (f64, f64)) -> bool {
    crate::testing::Dispatch::new().send(
        button,
        &WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: at.into(),
        },
    );
    Widget::cursor_shape(button, Point::new(at.0, at.1)).is_some()
}

#[test]
fn a_triangle_button_takes_the_pointer_inside_its_triangle_only() {
    let (set, _) = Triangles::set();
    let theme = Theme::dark()
        .with_painter_set(SurfaceRole::Control, set)
        .unwrap();
    let fired = Arc::new(AtomicUsize::new(0));
    let clicks = fired.clone();
    let mut button = Button::new("go", "Go").on_click(move || {
        clicks.fetch_add(1, Ordering::Relaxed);
    });
    button.apply_layout_external(Rect::new(100.0, 50.0, 220.0, 90.0));
    draw(&mut button, &theme);
    assert!(moved_to(&mut button, (160.0, 60.0)));
    assert!(moved_to(&mut button, (116.0, 88.0)));
    assert!(!moved_to(&mut button, (104.0, 88.0)));
    let spoken = Widget::accessibility(&button)
        .and_then(|node| node.bounds)
        .expect("bounds");
    assert_eq!(spoken, Rect::new(112.0, 50.0, 208.0, 90.0).into());
    assert!(!moved_to(&mut button, (104.0, 54.0)));
    assert!(!moved_to(&mut button, (216.0, 54.0)));
    let mut click = |at: (f64, f64)| {
        moved_to(&mut button, at);
        let mut dispatch = crate::testing::Dispatch::new();
        for state in [ElementState::Pressed, ElementState::Released] {
            dispatch.send(
                &mut button,
                &WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state,
                    button: MouseButton::Left,
                },
            );
        }
    };
    click((104.0, 54.0));
    assert_eq!(fired.load(Ordering::Relaxed), 0);
    click((160.0, 80.0));
    assert_eq!(fired.load(Ordering::Relaxed), 1);
    let face = crate::widget::ButtonFace::new(Rect::new(100.0, 50.0, 220.0, 90.0), "Go");
    assert!(face.contains(&theme, Point::new(160.0, 60.0)));
    assert!(!face.contains(&theme, Point::new(104.0, 54.0)));
    let mut plain = Button::new("go", "Go");
    plain.apply_layout_external(Rect::new(100.0, 50.0, 220.0, 90.0));
    draw(&mut plain, &Theme::dark());
    assert!(moved_to(&mut plain, (104.0, 54.0)));
}

#[test]
fn a_settled_part_is_painted_once_and_replayed() {
    let (set, painter) = Triangles::set();
    let theme = Theme::dark()
        .with_painter_set(SurfaceRole::Control, set.clone())
        .unwrap();
    let tokens = part_tokens(
        &theme,
        SurfaceRole::Control,
        (Color::WHITE, Color::BLACK, Color::BLACK),
        Color::BLACK,
    );
    let rest = Moment::settled(States::REST);
    let mut first = Scene::new();
    let mut second = Scene::new();
    for (scene, at) in [(&mut first, (0.0, 0.0)), (&mut second, (300.0, 40.0))] {
        let painted = paint_part(
            scene,
            Affine::IDENTITY,
            Rect::from_origin_size(at, (80.0, 30.0)),
            (&set, Part::ButtonContainer, SurfaceRole::Control),
            rest,
            &tokens,
            &theme,
        )
        .expect("painted");
        let hit = painted.hit.expect("a shape");
        assert!(hit_contains(&hit, Point::new(at.0 + 40.0, at.1 + 20.0)));
    }
    assert_eq!(painter.calls.load(Ordering::Relaxed), 1, "replayed");
    assert_eq!(
        first.encoding().draw_tags.len(),
        second.encoding().draw_tags.len()
    );
    for progress in [0.25, 0.5] {
        paint_part(
            &mut Scene::new(),
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 80.0, 30.0),
            (&set, Part::ButtonContainer, SurfaceRole::Control),
            Moment {
                state: PartState {
                    now: States::PRESSED,
                    was: States::REST,
                },
                progress,
                settled: false,
            },
            &tokens,
            &theme,
        );
    }
    assert_eq!(painter.calls.load(Ordering::Relaxed), 3);
    for _ in 0..2 {
        assert!(
            paint_part(
                &mut Scene::new(),
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 80.0, 30.0),
                (&set, Part::ScrollThumb, SurfaceRole::Control),
                rest,
                &tokens,
                &theme,
            )
            .is_none()
        );
    }
}

#[test]
fn the_toolkit_runs_the_painters_transition_on_its_clock() {
    let (set, _) = Triangles::set();
    let start = crate::time::now();
    let mut motion = PartMotion::new(States::REST);
    motion.go(
        States::PRESSED,
        (Some(&set), Part::ButtonContainer),
        Transition::None,
        false,
        start,
    );
    let begun = motion.at(start);
    assert!(!begun.settled);
    assert_eq!(begun.state.was, States::REST);
    assert_eq!(begun.state.now, States::PRESSED);
    assert!(begun.progress.abs() < 1e-6);
    let peak = (1..40)
        .map(|ms| motion.at(start + Duration::from_millis(ms * 5)).progress)
        .fold(0.0_f32, f32::max);
    assert!(peak > 1.0, "{peak}");
    assert!(motion.moving(start + Duration::from_millis(50)));
    assert_eq!(
        motion.at(start + Duration::from_secs(5)),
        Moment::settled(States::PRESSED)
    );
    motion.go(
        States::REST,
        (Some(&set), Part::ButtonContainer),
        Transition::None,
        false,
        start,
    );
    assert_eq!(motion.at(start), Moment::settled(States::REST));
    motion.go(
        States::PRESSED,
        (Some(&set), Part::ButtonContainer),
        Transition::None,
        true,
        start,
    );
    assert_eq!(motion.at(start), Moment::settled(States::PRESSED));
}

#[test]
fn a_painted_backdrop_is_its_fallback_where_nothing_may_be_translucent() {
    let theme = Theme::dark();
    let path = Rect::new(0.0, 0.0, 40.0, 40.0).to_path(0.1);
    let mut scene = PartScene::new();
    scene.fill(
        path,
        Brush::Backdrop {
            blur: 20.0,
            lens: Lens::default(),
            fallback: Color::from_rgb8(9, 9, 9),
            glaze: Vec::new(),
        },
    );
    struct Panel(PartScene);
    impl PartPainter for Panel {
        fn paint(
            &self,
            _: Part,
            _: Size,
            _: PartState,
            _: f32,
            _: &PartTokens,
        ) -> Option<PaintedPart> {
            Some(PaintedPart::new(self.0.clone()))
        }
    }
    let set = PainterSet::new("test.panel", Arc::new(Panel(scene)));
    let tokens = part_tokens(
        &theme,
        SurfaceRole::Floating,
        (Color::BLACK, Color::WHITE, Color::BLACK),
        Color::BLACK,
    );
    // No backdrop recording is active, and a control never samples: one
    // solid fill either way.
    for role in [SurfaceRole::Floating, SurfaceRole::Control] {
        let mut out = Scene::new();
        paint_part(
            &mut out,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 40.0, 40.0),
            (&set, Part::FloatingPanel, role),
            Moment::settled(States::REST),
            &tokens,
            &theme,
        )
        .expect("painted");
        assert_eq!(out.encoding().draw_tags.len(), 1, "{role:?}");
    }
}

#[test]
fn a_registered_set_is_found_by_name_and_replaced_by_a_newer_one() {
    let (first, _) = Triangles::set();
    let named = |set: &PainterSet| PainterSet::new("test.registry", set.painter.clone());
    let older = named(&first);
    assert!(register_painter_set(older.clone()));
    assert_eq!(painter_set("test.registry"), Some(older.clone()));
    let newer = named(&first);
    assert!(register_painter_set(newer.clone()));
    assert_eq!(painter_set("test.registry"), Some(newer));
    assert_ne!(painter_set("test.registry"), Some(older));
    assert_eq!(painter_set("test.nothing"), None);
    assert!(!register_painter_set(PainterSet::new(
        builtin::GLASS,
        first.painter.clone()
    )));
    assert_eq!(
        painter_set(builtin::GLASS).as_ref(),
        Some(builtin::glass_set())
    );
}

#[test]
fn the_glass_sheen_and_rim_take_the_themes_light() {
    // Glass draws its specular sheen and its lit rim in `tokens.light`: the
    // colour the theme names for the light a surface catches, never a
    // literal white in the painter.
    let theme = Theme::named("glass").expect("the Glass preset");
    let color = Color::from_rgb8(0x40, 0x60, 0x80);
    let tokens = part_tokens(&theme, SurfaceRole::Bar, (color, color, color), color);
    assert_eq!(
        tokens.light, theme.tokens.colors.light,
        "the theme's light reaches the painter"
    );
    let painted = builtin::glass_set()
        .painter()
        .paint(
            Part::BarBody,
            Size::new(320.0, 32.0),
            PartState::settled(States::REST),
            1.0,
            &tokens,
        )
        .expect("glass draws a bar body");
    // Every stop of the sheen and of the rim is the tokens' light at some
    // opacity; a literal white would not be. The fade's last stop is the
    // light at zero, which is transparent whatever the light's channels.
    let lit = |stop: &Stop| {
        stop.color.components[3] == 0.0
            || stop.color.components[..3] == tokens.light.components[..3]
    };
    let stops: Vec<Stop> = painted
        .scene
        .ops
        .iter()
        .flat_map(|op| match op {
            Op::Fill {
                brush: Brush::Linear { stops, .. },
                ..
            }
            | Op::Stroke {
                brush: Brush::Linear { stops, .. },
                ..
            } => stops.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert!(!stops.is_empty(), "a glass bar catches light");
    assert!(stops.iter().all(lit), "every lit stop is the theme's light");
    let other = PartTokens {
        light: Color::from_rgb8(0xff, 0xe0, 0xb0),
        ..tokens.clone()
    };
    assert_ne!(
        builtin::glass_set().painter().paint(
            Part::BarBody,
            Size::new(320.0, 32.0),
            PartState::settled(States::REST),
            1.0,
            &other,
        ),
        Some(painted),
        "the painter reads the light it is handed"
    );
}

fn strokes(painted: &PaintedPart) -> Vec<(vello::kurbo::BezPath, f32)> {
    painted
        .scene
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Stroke { path, width, .. } => Some((path.clone(), *width)),
            _ => None,
        })
        .collect()
}

fn runs_along(path: &vello::kurbo::BezPath, rect: Rect, side: crate::part_paint::Sides) -> bool {
    use vello::kurbo::PathEl;
    let half = 0.51;
    let (top, trailing, bottom, leading) = (rect.y0, rect.x1, rect.y1, rect.x0);
    let on = |p: vello::kurbo::Point| {
        if side == crate::part_paint::Sides::TOP {
            (p.y - top).abs() <= half
        } else if side == crate::part_paint::Sides::BOTTOM {
            (p.y - bottom).abs() <= half
        } else if side == crate::part_paint::Sides::LEADING {
            (p.x - leading).abs() <= half
        } else {
            (p.x - trailing).abs() <= half
        }
    };
    // The rim runs along the side when a segment of it does: both ends of
    // that segment lie on the side's line. A path that only leaves or
    // arrives there at a corner, or a flush body's rim that turns before it,
    // does not.
    let mut pen = None;
    for element in path.elements() {
        match *element {
            PathEl::MoveTo(p) => pen = Some(p),
            PathEl::LineTo(p) => {
                if pen.is_some_and(|from| on(from) && on(p)) {
                    return true;
                }
                pen = Some(p);
            }
            PathEl::CurveTo(p1, p2, p) => {
                if pen.is_some_and(|from| on(from) && on(p1) && on(p2) && on(p)) {
                    return true;
                }
                pen = Some(p);
            }
            PathEl::ClosePath => pen = None,
            _ => {}
        }
    }
    false
}

/// A bar flush with the display draws no lit rim along the screen's own edges, and keeps its rim on the
/// side that faces what it covers. On a bar edge to edge with the screen its top row and end columns
/// are the display's own, so a rim there is a line of light a pixel in from the bezel: the bar reads as
/// floating rather than as reaching the edge.
#[test]
fn a_flush_bar_lights_no_rim_on_the_display_edges() {
    use crate::part_paint::Sides;
    let theme = Theme::named("glass").expect("the Glass preset");
    let color = Color::from_rgb8(0x40, 0x60, 0x80);
    let size = Size::new(320.0, 32.0);
    let body = Rect::new(0.0, 0.0, 320.0, 32.0);
    let paint = |flush: Sides| {
        let mut tokens = part_tokens(&theme, SurfaceRole::Bar, (color, color, color), color);
        tokens.flush = flush;
        tokens.radius = 0.0;
        let tokens = tokens;
        builtin::glass_set()
            .painter()
            .paint(
                Part::BarBody,
                size,
                PartState::settled(States::REST),
                1.0,
                &tokens,
            )
            .expect("glass draws a bar body")
    };
    let all = strokes(&paint(Sides::NONE));
    assert_eq!(all.len(), 1, "a body not flush lights one rim path");
    for side in [Sides::TOP, Sides::BOTTOM, Sides::LEADING, Sides::TRAILING] {
        assert!(
            runs_along(&all[0].0, body, side),
            "an unflushed body's rim runs along {side:?}"
        );
    }
    // A bar on the screen's top edge lights only the side it covers.
    let flush = Sides::TOP.union(Sides::LEADING).union(Sides::TRAILING);
    let drawn = strokes(&paint(flush));
    assert_eq!(drawn.len(), 1, "a flush body lights one rim path");
    for side in [Sides::TOP, Sides::LEADING, Sides::TRAILING] {
        assert!(
            !runs_along(&drawn[0].0, body, side),
            "a flush body draws nothing along the display's {side:?} edge"
        );
    }
    assert!(
        runs_along(&drawn[0].0, body, Sides::BOTTOM),
        "and keeps its rim on the side that faces what it covers"
    );
}

#[test]
fn forced_colours_draw_with_the_toolkits_own_painters() {
    let (set, _) = Triangles::set();
    let theme = Theme::high_contrast()
        .with_painter_set(SurfaceRole::Control, set)
        .unwrap();
    assert!(theme.forced_colors);
    assert_eq!(theme.painter(SurfaceRole::Control), builtin::default_set());
}

fn with_param(tokens: &PartTokens, name: &str, value: f32) -> PartTokens {
    let mut params: Vec<Param> = tokens.params.iter().cloned().collect();
    params.retain(|param| param.name != name);
    params.push(Param {
        name: name.into(),
        value,
    });
    PartTokens {
        params: params.into(),
        ..tokens.clone()
    }
}

/// The straight stretches of the Liquid rim on a 200×100 floating panel lit from `light_angle`: each
/// solid fill in the theme's light, by its bounds and opacity.
fn liquid_rim_stretches(light_angle: f32) -> Vec<(Rect, f32)> {
    let theme = Theme::preset(crate::theme::Preset::LiquidGlass);
    let color = Color::from_rgb8(0x30, 0x34, 0x40);
    let mut tokens = part_tokens(&theme, SurfaceRole::Floating, (color, color, color), color);
    tokens.radius = 12.0;
    let tokens = with_param(&tokens, "edge_highlight", 0.5);
    let tokens = with_param(&tokens, "light_angle", light_angle);
    let painted = builtin::liquid_set()
        .painter()
        .paint(
            Part::FloatingPanel,
            Size::new(200.0, 100.0),
            PartState::settled(States::REST),
            1.0,
            &tokens,
        )
        .expect("liquid draws a floating panel");
    painted
        .scene
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Fill {
                path,
                brush: Brush::Solid(color),
                ..
            } if color.components[..3] == tokens.light.components[..3] => {
                Some((path.bounding_box(), color.components[3]))
            }
            _ => None,
        })
        .collect()
}

/// The Liquid rim is a thin edge lit from the theme's light angle: the stretch facing the light at the
/// full `edge_highlight`, the far one faintly, the two side on dark; turning the light turns the rim.
#[test]
fn the_liquid_rim_is_lit_from_the_light_angle() {
    use vello::kurbo::Vec2;
    let up = Vec2::new(0.0, -1.0);
    assert!((builtin::rim_light(up, up) - 1.0).abs() < 1e-6);
    assert!(builtin::rim_light(Vec2::new(1.0, 0.0), up).abs() < 1e-6);
    let far = builtin::rim_light(Vec2::new(0.0, 1.0), up);
    assert!(far > 0.0 && far < 0.5, "the far side is faint: {far}");

    let edge = |stretches: &[(Rect, f32)], which: fn(&Rect) -> bool| {
        stretches
            .iter()
            .find(|(rect, _)| which(rect))
            .map(|(_, alpha)| *alpha)
            .expect("a rim stretch there")
    };
    let top = |rect: &Rect| rect.y0 < 1.0 && rect.width() > 100.0;
    let bottom = |rect: &Rect| rect.y1 > 99.0 && rect.width() > 100.0;
    let left = |rect: &Rect| rect.x0 < 1.0 && rect.height() > 50.0;
    // Light from above (90°): the top stretch is the brightest, the bottom
    // faint, the sides unlit.
    let above = liquid_rim_stretches(90.0);
    assert!((edge(&above, top) - 0.5).abs() < 1e-4, "{above:?}");
    assert!(edge(&above, bottom) < edge(&above, top) / 2.0);
    assert!(
        edge(&above, bottom) > 0.0,
        "light leaves through the far side"
    );
    assert!(edge(&above, left) < 1e-4);
    let top_rect = above.iter().find(|(rect, _)| top(rect)).expect("top").0;
    assert!((top_rect.height() - 1.5).abs() < 1e-9);
    // Light from the left (180°): the left stretch takes it.
    let beside = liquid_rim_stretches(180.0);
    assert!((edge(&beside, left) - 0.5).abs() < 1e-4);
    assert!(edge(&beside, top) < 1e-4);
}

struct CountedLiquid {
    calls: AtomicUsize,
}

impl PartPainter for CountedLiquid {
    fn paint(
        &self,
        part: Part,
        size: Size,
        state: PartState,
        progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        builtin::liquid_set()
            .painter()
            .paint(part, size, state, progress, tokens)
    }
}

/// A settled Liquid face is cached under every parameter the set reads: drawn again with the same
/// tokens it is replayed, and one whose band, light angle, dispersion or refraction differs is drawn
/// anew.
#[test]
fn the_part_cache_keys_on_every_liquid_parameter() {
    let painter = Arc::new(CountedLiquid {
        calls: AtomicUsize::new(0),
    });
    let set = PainterSet::new("test.counted-liquid", painter.clone());
    let theme = Theme::preset(crate::theme::Preset::LiquidGlass);
    let color = Color::from_rgb8(0x50, 0x54, 0x60);
    let base = part_tokens(&theme, SurfaceRole::Control, (color, color, color), color);
    let draw = |tokens: &PartTokens| {
        paint_part(
            &mut Scene::new(),
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 91.0, 29.0),
            (&set, Part::ControlFace, SurfaceRole::Control),
            Moment::settled(States::REST),
            tokens,
            &theme,
        );
        painter.calls.load(Ordering::Relaxed)
    };
    let first = with_param(&base, "band", 10.0);
    let once = draw(&first);
    assert_eq!(draw(&first), once, "the same tokens replay");
    for (name, value) in [
        ("band", 11.0),
        ("light_angle", 45.0),
        ("dispersion", 0.2),
        ("refraction", 3.0),
    ] {
        let tokens = with_param(&first, name, value);
        let before = painter.calls.load(Ordering::Relaxed);
        assert_eq!(
            draw(&tokens),
            before + 1,
            "another {name} is another drawing"
        );
        assert_eq!(draw(&tokens), before + 1, "and is kept under it");
    }
}

fn pixelated_patterns(tokens: &PartTokens, part: Part) -> Vec<f32> {
    let painted = builtin::pixelated_set()
        .painter()
        .paint(
            part,
            Size::new(200.0, 120.0),
            PartState {
                was: States::REST,
                now: States::REST,
            },
            1.0,
            tokens,
        )
        .expect("pixelated draws the four surface parts");
    painted
        .scene
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Fill {
                brush: Brush::Pattern { image, scale },
                ..
            } => {
                assert_eq!((image.width, image.height), (4, 4), "a 4 by 4 tile");
                Some(*scale)
            }
            _ => None,
        })
        .collect()
}

/// Pixelated draws its cell grid as its own ops: a pattern fill of the body in the fill stepped towards
/// the ink, one texel per cell.
#[test]
fn the_pixelated_set_draws_its_cell_grid() {
    let theme = Theme::preset(crate::theme::Preset::Pixelated);
    let color = Color::from_rgb8(0x18, 0x20, 0x2c);
    let tokens = part_tokens(
        &theme,
        SurfaceRole::Floating,
        (color, color, color),
        crate::chrome::panel_ground(&theme),
    );
    let cell = tokens.param("cell", 8.0);
    for part in [
        Part::WindowGround,
        Part::FloatingPanel,
        Part::BarBody,
        Part::ControlFace,
    ] {
        assert_eq!(
            pixelated_patterns(&tokens, part),
            vec![cell],
            "{part:?} carries the grid, one pattern fill, one texel per cell"
        );
    }
    // A grid of 0 draws none.
    let plain = with_param(&tokens, "grid", 0.0);
    assert!(
        pixelated_patterns(&plain, Part::FloatingPanel).is_empty(),
        "a grid of 0 draws no cells"
    );
}

/// The grid's pattern paints cells on the frame: a 4 by 4 dither tile drawn at `cell` logical pixels
/// per texel, so a row of the body steps in a Bayer order rather than shading evenly.
#[test]
fn the_cell_grid_paints_cells_on_the_frame() {
    use crate::offscreen::OffscreenRenderer;
    let (width, height) = (160, 120);
    let theme = Theme::preset(crate::theme::Preset::Pixelated);
    let color = Color::from_rgb8(0x20, 0x28, 0x34);
    let ink = Color::from_rgb8(0xf0, 0xf2, 0xf6);
    let tokens = part_tokens(
        &theme,
        SurfaceRole::Floating,
        (color, ink, color),
        crate::chrome::panel_ground(&theme),
    );
    let mut scene = Scene::new();
    paint_part(
        &mut scene,
        Affine::IDENTITY,
        Rect::new(20.0, 20.0, 140.0, 100.0),
        (
            builtin::pixelated_set(),
            Part::FloatingPanel,
            SurfaceRole::Floating,
        ),
        Moment::settled(States::REST),
        &tokens,
        &theme,
    );
    let mut renderer = OffscreenRenderer::new(width, height);
    let rgba = renderer.render(&scene, Color::BLACK);
    let row: Vec<(u8, u8, u8)> = (30..130)
        .map(|x| {
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, width, x, 60);
            (r, g, b)
        })
        .collect();
    let changes = row.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert!(
        changes >= 4,
        "the grid paints cells across the body: {changes} changes in {row:?}"
    );
}
