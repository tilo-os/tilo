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

    fn triangle(size: Size) -> BezPath {
        let mut path = BezPath::new();
        path.move_to((size.width / 2.0, 0.0));
        path.line_to((size.width, size.height));
        path.line_to((0.0, size.height));
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
    let theme = Theme::dark().with_painter_set(SurfaceRole::Control, set);
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
    assert!(moved_to(&mut button, (110.0, 88.0)));
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
    // Without a painter set the whole rect takes it, as before.
    let mut plain = Button::new("go", "Go");
    plain.apply_layout_external(Rect::new(100.0, 50.0, 220.0, 90.0));
    draw(&mut plain, &Theme::dark());
    assert!(moved_to(&mut plain, (104.0, 54.0)));
}

#[test]
fn a_settled_part_is_painted_once_and_replayed() {
    let (set, painter) = Triangles::set();
    let theme = Theme::dark().with_painter_set(SurfaceRole::Control, set.clone());
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
    register_painter_set(older.clone());
    assert_eq!(painter_set("test.registry"), Some(older.clone()));
    let newer = named(&first);
    register_painter_set(newer.clone());
    assert_eq!(painter_set("test.registry"), Some(newer));
    assert_ne!(painter_set("test.registry"), Some(older));
    assert_eq!(painter_set("test.nothing"), None);
}

#[test]
fn forced_colours_draw_with_the_toolkits_own_painters() {
    let (set, _) = Triangles::set();
    let theme = Theme::high_contrast().with_painter_set(SurfaceRole::Control, set);
    assert!(theme.forced_colors);
    assert!(theme.painter(SurfaceRole::Control).is_none());
}
