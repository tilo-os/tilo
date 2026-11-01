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

/// A painter set that draws a button as a triangle pointing up, counts
/// its calls, springs between states and leaves every other part alone.
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

    /// A triangle on the rect's foot, a tenth of its width in from each
    /// side, its apex at the top centre.
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
    // The apex column and the foot of the triangle take it; the rect's top
    // corners, outside the triangle, do not.
    assert!(moved_to(&mut button, (160.0, 60.0)));
    assert!(moved_to(&mut button, (116.0, 88.0)));
    assert!(!moved_to(&mut button, (104.0, 88.0)));
    // It is spoken where it is drawn.
    let spoken = Widget::accessibility(&button)
        .and_then(|node| node.bounds)
        .expect("bounds");
    assert_eq!(spoken, Rect::new(112.0, 50.0, 208.0, 90.0).into());
    assert!(!moved_to(&mut button, (104.0, 54.0)));
    assert!(!moved_to(&mut button, (216.0, 54.0)));
    // A press in a corner does nothing; one inside the triangle clicks.
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
    // An immediate-mode face takes it in the same shape.
    let face = crate::widget::ButtonFace::new(Rect::new(100.0, 50.0, 220.0, 90.0), "Go");
    assert!(face.contains(&theme, Point::new(160.0, 60.0)));
    assert!(!face.contains(&theme, Point::new(104.0, 54.0)));
    // Without a painter set the whole rect takes it, as before.
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
        // The hit shape comes back where the part was placed.
        let hit = painted.hit.expect("a shape");
        assert!(hit_contains(&hit, Point::new(at.0 + 40.0, at.1 + 20.0)));
    }
    assert_eq!(painter.calls.load(Ordering::Relaxed), 1, "replayed");
    assert_eq!(
        first.encoding().draw_tags.len(),
        second.encoding().draw_tags.len()
    );
    // A moving part is painted every frame, and not kept.
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
    // A part the set leaves alone is the toolkit's, and asked once.
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
    // An underdamped spring passes its end on the way.
    let peak = (1..40)
        .map(|ms| motion.at(start + Duration::from_millis(ms * 5)).progress)
        .fold(0.0_f32, f32::max);
    assert!(peak > 1.0, "{peak}");
    assert!(motion.moving(start + Duration::from_millis(50)));
    assert_eq!(
        motion.at(start + Duration::from_secs(5)),
        Moment::settled(States::PRESSED)
    );
    // The set inherits the release: the toolkit's own motion, here none.
    motion.go(
        States::REST,
        (Some(&set), Part::ButtonContainer),
        Transition::None,
        false,
        start,
    );
    assert_eq!(motion.at(start), Moment::settled(States::REST));
    // Reduce motion lands every change at once.
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
    // The toolkit's own names are not a theme's to take.
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
    // And it moves with the theme: another light is another drawing.
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

#[test]
fn forced_colours_draw_with_the_toolkits_own_painters() {
    let (set, _) = Triangles::set();
    let theme = Theme::high_contrast()
        .with_painter_set(SurfaceRole::Control, set)
        .unwrap();
    assert!(theme.forced_colors);
    assert_eq!(theme.painter(SurfaceRole::Control), builtin::default_set());
}

/// `tokens` with `name` stated at `value`, replacing what it stated before.
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

/// The straight stretches of the Liquid rim on a 200×100 floating panel lit
/// from `light_angle`: each solid fill in the theme's light, by its bounds
/// and opacity.
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

/// The Liquid rim is a thin edge lit from the theme's light angle: the
/// stretch facing the light at the full `edge_highlight`, the far one
/// faintly, the two side on dark; turning the light turns the rim.
#[test]
fn the_liquid_rim_is_lit_from_the_light_angle() {
    use vello::kurbo::Vec2;
    // The light on a normal facing it, side on and facing away.
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
    // The rim is thin: a stretch is one and a half pixels deep.
    let top_rect = above.iter().find(|(rect, _)| top(rect)).expect("top").0;
    assert!((top_rect.height() - 1.5).abs() < 1e-9);
    // Light from the left (180°): the left stretch takes it.
    let beside = liquid_rim_stretches(180.0);
    assert!((edge(&beside, left) - 0.5).abs() < 1e-4);
    assert!(edge(&beside, top) < 1e-4);
}

/// The Liquid set, counting the faces it is asked to draw.
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

/// A settled Liquid face is cached under every parameter the set reads:
/// drawn again with the same tokens it is replayed, and one whose band,
/// light angle, dispersion or refraction differs is drawn anew.
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
