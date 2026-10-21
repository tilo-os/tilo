//! The window's loading line as its window draws it: under the bar, inside
//! the content's shape, and in a part of its own.

use std::cell::Cell;
use std::rc::Rc;

use super::*;
use crate::widget::{AppKind, TaffyTreeExt};
use crate::window_loading::WindowLoading;

const TOP: f64 = 36.0;
const W: f64 = 1280.0;
const H: f64 = 800.0;

/// How often the content was laid out and drawn.
#[derive(Clone, Default)]
struct Counts {
    layouts: Rc<Cell<u32>>,
    draws: Rc<Cell<u32>>,
}

/// Window content that fills what it is given, at rest, counting its
/// layouts and draws.
struct CountingPage {
    rect: Rect,
    counts: Counts,
}

impl Widget for CountingPage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.counts.layouts.set(self.counts.layouts.get() + 1);
        taffy.fill_leaf(node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.counts.draws.set(self.counts.draws.get() + 1);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &self.rect,
        );
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "CountingPage"
    }
}

impl crate::widget::app_widget::AppWidget for CountingPage {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Files"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("F")
    }
}

fn window(counts: &Counts) -> WindowGroup {
    WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Files"),
            TileContent::App(Box::new(CountingPage {
                rect: Rect::ZERO,
                counts: counts.clone(),
            })),
        )],
        1,
        WindowWidth::Normal,
    )
}

/// Lay the window out as the strip does, under the desktop's top bar.
fn lay_out(g: &mut WindowGroup, theme: &Theme) {
    let mut taffy = TaffyTree::<()>::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = g.build_layout(&mut taffy, theme, &mut map);
    let strip = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(f32_of_f64(W)),
                    height: Dimension::length(f32_of_f64(H - TOP)),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    {
        use taffy::prelude::TaffyMaxContent;
        taffy
            .compute_layout(strip, taffy::Size::MAX_CONTENT)
            .unwrap();
    }
    g.apply_layout(&taffy, node, Point::new(0.0, TOP));
}

/// Lay out and draw until the window asks for no more layout, as the host
/// does: a title's shaped width is measured at draw and laid out next.
fn settle(g: &mut WindowGroup, theme: &Theme, text: &mut TextRenderer) {
    for _ in 0..6 {
        lay_out(g, theme);
        let _ = record(g, theme, 1.0, text);
        let structure = g.take_structure_dirty();
        if !structure && !g.is_layout_dirty() {
            return;
        }
    }
    panic!("the window never settled its layout");
}

/// Draw the window as the strip records it: a scene of its own with its
/// damage parts.
fn record(
    g: &mut WindowGroup,
    theme: &Theme,
    scale: f64,
    text: &mut TextRenderer,
) -> (Scene, crate::scene_parts::SectionParts) {
    let mut scene = Scene::new();
    let ((), parts) = crate::scene_parts::record(&mut scene, |scene| {
        g.draw(
            scene,
            Affine::scale(scale),
            text,
            theme,
            &PresenceCtx::empty(),
        );
    });
    (scene, parts.expect("the window's parts nest"))
}

fn content(g: &WindowGroup) -> Rect {
    g.active_host().map(Widget::get_rect).unwrap_or(Rect::ZERO)
}

fn band(g: &WindowGroup, theme: &Theme) -> Rect {
    g.active_host()
        .map(|host| host.loading_band(theme))
        .unwrap_or(Rect::ZERO)
}

#[cfg(feature = "native")]
fn rasterize(scene: &Scene) -> Vec<u8> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small fixed canvas"
    )]
    let (w, h) = (W as u32, H as u32);
    let mut pixels = vec![0_u8; (w * h * 4) as usize];
    let mut target =
        tilo_cpu_raster::Target::new(&mut pixels, w, h, (w * 4) as usize).expect("target");
    tilo_cpu_raster::CpuRasterizer::default().render(
        scene.encoding(),
        &mut target,
        None,
        tilo_cpu_raster::color::AlphaColor::new([0.0, 0.0, 0.0, 0.0]),
    );
    pixels
}

#[cfg(feature = "native")]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel coordinates inside the canvas"
)]
fn pixel(pixels: &[u8], x: f64, y: f64) -> [u8; 4] {
    let at = ((y as usize) * (W as usize) + (x as usize)) * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

#[cfg(feature = "native")]
fn near(pixel: [u8; 4], color: vello::peniko::Color) -> bool {
    let want = color.to_rgba8();
    let want = [want.r, want.g, want.b, want.a];
    pixel
        .iter()
        .zip(want)
        .all(|(got, want)| got.abs_diff(want) <= 12)
}

// USER REQUIREMENT (2026-10-21): "it shows under the window bar and is
// clipped to the window's shape"
#[test]
fn the_loading_line_shows_under_the_window_bar_clipped_to_the_window() {
    let theme = Theme::dark();
    let counts = Counts::default();
    let mut g = window(&counts);
    let mut text = TextRenderer::new();
    settle(&mut g, &theme, &mut text);
    let bar = g.bar_rect();
    let content = content(&g);
    let line = band(&g, &theme);
    assert!(bar.height() > 0.0, "the window shows its bar");
    assert!(
        (line.y0 - content.y0).abs() < 1e-6 && line.y0 >= bar.y1 - 0.5,
        "directly under the bar {bar:?}, at the top of the content {content:?}: {line:?}"
    );
    assert!(
        (line.x0 - content.x0).abs() < 1e-6 && (line.x1 - content.x1).abs() < 1e-6,
        "across the content's width: {line:?}"
    );
    assert!((line.height() - 2.0).abs() < 1e-6, "2 px at 1x");

    let t0 = crate::time::now();
    crate::time::testing::with_monotonic_time(t0, || {
        assert!(g.set_tab_loading(1, WindowLoading::progress(1.0)));
    });
    assert_eq!(g.tab_loading(1), Some(WindowLoading::progress(1.0)));
    #[cfg(feature = "native")]
    {
        let (scene, _) = crate::time::testing::with_monotonic_time(
            t0 + crate::time::Duration::from_secs(2),
            || record(&mut g, &theme, 1.0, &mut text),
        );
        let pixels = rasterize(&scene);
        let accent = theme.tokens.colors.loading_line();
        assert!(
            near(pixel(&pixels, content.center().x, line.y0 + 1.0), accent),
            "the full bar is drawn along the top of the content"
        );
        assert!(
            !near(pixel(&pixels, content.x0 + 0.5, line.y0 + 0.5), accent),
            "but not in the window's rounded corner"
        );
        assert!(
            !near(pixel(&pixels, content.x1 - 0.5, line.y0 + 0.5), accent),
            "at either end"
        );
        assert!(
            !near(pixel(&pixels, content.center().x, line.y0 - 1.0), accent),
            "nor over the bar"
        );
        assert!(
            !near(pixel(&pixels, content.center().x, line.y1 + 1.0), accent),
            "and it is a thin line"
        );
    }
}

// USER REQUIREMENT (2026-10-21): "In fullscreen, with no window bar, it sits
// at the top edge."
#[test]
fn in_full_screen_the_loading_line_sits_at_the_top_edge() {
    let theme = Theme::dark();
    let counts = Counts::default();
    let mut g = window(&counts);
    g.set_fullscreen_presentation(true);
    let mut text = TextRenderer::new();
    settle(&mut g, &theme, &mut text);
    let content = content(&g);
    let line = band(&g, &theme);
    assert!(
        (content.y0 - g.get_rect().y0).abs() < 1e-6,
        "no bar above the content"
    );
    assert!(
        (line.y0 - content.y0).abs() < 1e-6,
        "at the top edge: {line:?}"
    );
    assert!(
        (line.width() - content.width()).abs() < 1e-6,
        "edge to edge"
    );
}

// USER REQUIREMENT (2026-10-21): "animating it doesn't relayout or repaint
// the window content (counters)"
#[test]
fn animating_the_loading_line_neither_lays_out_nor_repaints_the_window_content() {
    let theme = Theme::dark();
    let counts = Counts::default();
    let mut g = window(&counts);
    let mut text = TextRenderer::new();
    settle(&mut g, &theme, &mut text);
    let t0 = crate::time::now();
    crate::time::testing::with_monotonic_time(t0, || {
        g.set_tab_loading(1, WindowLoading::Busy);
    });
    let line = band(&g, &theme);
    let content = content(&g);
    let scale = 2.0;
    let line_device = Affine::scale(scale).transform_rect_bbox(line);
    let below_line = Affine::scale(scale).transform_rect_bbox(Rect::new(
        content.x0,
        line.y1 + 1.0,
        content.x1,
        content.y1,
    ));
    let layouts = counts.layouts.get();
    let mut relayouts = 0_u32;
    let mut previous: Option<(Scene, crate::scene_parts::SectionParts)> = None;
    let mut damaged = 0_u32;
    // Two dozen frames of the sweep, well past the line's delay and arrival.
    for frame in 0..24_u64 {
        let now = t0 + crate::time::Duration::from_millis(400 + frame * 16);
        let drawn = crate::time::testing::with_monotonic_time(now, || {
            assert!(g.needs_animation_frame(), "the sweep animates");
            if g.take_structure_dirty() || g.is_layout_dirty() {
                relayouts += 1;
                lay_out(&mut g, &theme);
            }
            record(&mut g, &theme, scale, &mut text)
        });
        if let Some((before, before_parts)) = &previous {
            let damage = crate::scene_parts::damage(before, before_parts, &drawn.0, &drawn.1)
                .expect("the frames compare part by part");
            assert!(!damage.is_empty(), "frame {frame}: the line moved");
            for rect in &damage {
                assert!(
                    line_device.inflate(2.0, 2.0).contains_rect(*rect),
                    "frame {frame}: {rect:?} repaints beyond the line {line_device:?}"
                );
                assert!(
                    !rect.overlaps(below_line),
                    "frame {frame}: {rect:?} repaints the content"
                );
            }
            damaged += 1;
        }
        previous = Some(drawn);
    }
    assert_eq!(damaged, 23);
    assert_eq!(relayouts, 0, "the line takes no layout");
    assert_eq!(
        counts.layouts.get(),
        layouts,
        "the content was not laid out"
    );
}

#[test]
fn a_report_reaches_only_its_own_tab_and_a_hidden_line_costs_nothing() {
    let theme = Theme::dark().with_reduce_motion(true);
    let counts = Counts::default();
    let mut g = window(&counts);
    let mut text = TextRenderer::new();
    settle(&mut g, &theme, &mut text);
    let t0 = crate::time::now();
    crate::time::testing::with_monotonic_time(t0, || {
        assert!(
            !g.needs_animation_frame(),
            "nothing loads, nothing animates"
        );
        assert!(!g.set_tab_loading(7, WindowLoading::Busy), "no such tab");
        assert!(g.set_tab_loading(1, WindowLoading::Busy));
        assert!(
            !g.set_tab_loading(1, WindowLoading::Busy),
            "an equal report"
        );
        assert!(
            !g.needs_animation_frame(),
            "the line's delay is waited out asleep"
        );
        assert_eq!(
            g.next_wake_deadline(),
            Some(t0 + crate::motion::Loading::DELAY)
        );
    });
}
