//! The surface roles' finishes over a real backdrop, timed: a desktop with a
//! busy wallpaper, the top bar, a window with a sidebar and controls, a menu
//! and a notification floating over both, drawn in each theme with the
//! backdrop pass, as the shell composes a frame.
//!
//! ```text
//! cargo run -p ui-examples --release --example ui-perf -- material_bench [THEME]...
//! ```
//!
//! Without themes it runs Default, Glass and Block, light and dark. For each
//! it writes `material-<theme>.png` to `TILO_UI_SNAPSHOT_OUT` (a directory,
//! default the working directory) and prints the median and 95th
//! percentile frame time over `TILO_UI_MATERIAL_FRAMES` frames (default
//! 120): building the scene, the backdrop blur and the render, GPU completion
//! and readback included.

use std::time::{Duration, Instant};

use tilo_ui::TextRole;
use tilo_ui::backdrop::{BackdropFrame, BackdropRecording, layer_scene};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::SurfaceRole;
use tilo_ui::widget::primitives::{SurfaceLevel, paint_surface};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, ColorStop, Fill, Gradient};

const WIDTH: u32 = 1600;
const HEIGHT: u32 = 1000;
const BAR: f64 = 36.0;

/// A wallpaper with colour, edges and fine detail, so blur, saturation and
/// refraction all have something to show.
fn wallpaper(scene: &mut Scene) {
    let all = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
    let sky = Gradient::new_linear((0.0, 0.0), (f64::from(WIDTH), f64::from(HEIGHT))).with_stops(
        &[
            ColorStop {
                offset: 0.0,
                color: Color::from_rgb8(38, 70, 140).into(),
            },
            ColorStop {
                offset: 0.55,
                color: Color::from_rgb8(150, 60, 110).into(),
            },
            ColorStop {
                offset: 1.0,
                color: Color::from_rgb8(240, 170, 70).into(),
            },
        ][..],
    );
    scene.fill(Fill::NonZero, Affine::IDENTITY, &sky, None, &all);
    for (x, y, r, colour) in [
        (300.0, 700.0, 260.0, Color::from_rgb8(40, 200, 170)),
        (1250.0, 260.0, 220.0, Color::from_rgb8(250, 230, 120)),
        (1000.0, 820.0, 160.0, Color::from_rgb8(230, 80, 60)),
    ] {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            colour,
            None,
            &Circle::new(Point::new(x, y), r),
        );
    }
    for i in 0..40 {
        let x = 1100.0 + f64::from(i) * 12.0;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 140),
            None,
            &Rect::new(x, 420.0, x + 5.0, 980.0),
        );
    }
}

fn label(scene: &mut Scene, text: &mut TextRenderer, theme: &Theme, at: Point, s: &str) {
    text.draw_style(
        scene,
        s,
        theme.text_primary,
        Affine::translate((at.x, at.y)),
        None,
        &theme.text(TextRole::Label),
    );
}

/// One frame of the composition in `theme`, with what its translucent
/// surfaces need from the backdrop pass.
fn frame(theme: &Theme, text: &mut TextRenderer) -> (Scene, BackdropFrame) {
    let mut scene = Scene::new();
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    wallpaper(&mut scene);

    // The top bar.
    let bar = RoundedRect::from_rect(Rect::new(0.0, 0.0, f64::from(WIDTH), BAR), 0.0);
    if theme.tokens.finishes.bar.is_some() {
        tilo_ui::backdrop::fill_role(
            &mut scene,
            Affine::IDENTITY,
            &bar,
            SurfaceRole::Bar,
            theme,
            tilo_ui::chrome::panel_ground(theme),
        );
    }
    label(&mut scene, text, theme, Point::new(16.0, 10.0), "Workspace");
    label(&mut scene, text, theme, Point::new(760.0, 10.0), "14:32");
    window(&mut scene, text, theme);
    floating(&mut scene, text, theme);
    let backdrop = BackdropFrame::from_single_scene(recording.finish());
    (scene, backdrop)
}

/// A window: its ground, a sidebar, controls.
fn window(scene: &mut Scene, text: &mut TextRenderer, theme: &Theme) {
    let window = Rect::new(80.0, 80.0, 1080.0, 900.0);
    let on_window = paint_surface(scene, Affine::IDENTITY, window, SurfaceLevel::Window, theme);
    let radius = f64::from(theme.tokens.radii.tile);
    let content = window.inset(-0.0);
    tilo_ui::surface::region(
        scene,
        Affine::IDENTITY,
        Rect::new(content.x0 + 240.0, content.y0, content.x1, content.y1),
        tilo_ui::surface::Region::Content,
        &on_window,
    );
    tilo_ui::surface::region(
        scene,
        Affine::IDENTITY,
        Rect::new(
            content.x0,
            content.y0 + radius,
            content.x0 + 240.0,
            content.y1 - radius,
        ),
        tilo_ui::surface::Region::Sidebar,
        &on_window,
    );
    for (i, name) in (0u8..).zip(["Inbox", "Drafts", "Sent", "Archive"]) {
        let y = 120.0 + 36.0 * f64::from(i);
        label(scene, text, &on_window, Point::new(104.0, y), name);
    }
    for (i, name) in (0u8..).zip(["Reply", "Forward", "Archive", "Delete"]) {
        let x = 360.0 + 150.0 * f64::from(i);
        let face = Rect::new(x, 110.0, x + 132.0, 142.0);
        paint_surface(
            scene,
            Affine::IDENTITY,
            face,
            SurfaceLevel::Raised,
            &on_window,
        );
        label(scene, text, &on_window, Point::new(x + 18.0, 118.0), name);
    }
    let field = Rect::new(360.0, 170.0, 1040.0, 206.0);
    paint_surface(
        scene,
        Affine::IDENTITY,
        field,
        SurfaceLevel::Raised,
        &on_window,
    );
    label(
        scene,
        text,
        &on_window,
        Point::new(376.0, 180.0),
        "Search mail",
    );
    for i in 0..12 {
        let y = 240.0 + 52.0 * f64::from(i);
        label(
            scene,
            text,
            &on_window,
            Point::new(360.0, y),
            "A message whose subject runs on for a while to show body text",
        );
    }
}

/// A menu over the window's edge and the wallpaper, and a notification.
fn floating(scene: &mut Scene, text: &mut TextRenderer, theme: &Theme) {
    let menu = Rect::new(940.0, 260.0, 1300.0, 600.0);
    let panel_radius = f64::from(theme.tokens.radii.panel);
    tilo_ui::surface::floating_shadow(scene, Affine::IDENTITY, menu, panel_radius, 1.0, theme);
    tilo_ui::surface::panel(scene, Affine::IDENTITY, menu, panel_radius, theme);
    let on_menu = tilo_ui::chrome::shell::on_panel(theme);
    for (i, name) in (0u8..).zip([
        "Open",
        "Open in new window",
        "Copy link",
        "Move to…",
        "Mark as read",
        "Delete",
    ]) {
        label(
            scene,
            text,
            &on_menu,
            Point::new(964.0, 284.0 + 50.0 * f64::from(i)),
            name,
        );
    }

    // A notification at the top right.
    let note = Rect::new(1180.0, 56.0, 1580.0, 156.0);
    tilo_ui::surface::floating_shadow(scene, Affine::IDENTITY, note, panel_radius, 1.0, theme);
    tilo_ui::surface::panel(scene, Affine::IDENTITY, note, panel_radius, theme);
    label(
        scene,
        text,
        &on_menu,
        Point::new(1204.0, 80.0),
        "Ada Lovelace",
    );
    label(
        scene,
        text,
        &on_menu,
        Point::new(1204.0, 110.0),
        "The engine ran the whole table tonight.",
    );
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an index into a short list of samples"
    )]
    let index = ((sorted.len().saturating_sub(1)) as f64 * p).round() as usize;
    sorted.get(index).copied().unwrap_or_default()
}

pub fn run(args: &[String]) {
    let names: Vec<String> = if args.is_empty() {
        [
            "dark",
            "light",
            "glass",
            "glass light",
            "block",
            "block light",
        ]
        .map(String::from)
        .to_vec()
    } else {
        args.to_vec()
    };
    let out = tilo_ui::env::var("TILO_UI_SNAPSHOT_OUT").unwrap_or_else(|_| ".".into());
    let frames: usize = tilo_ui::env::var("TILO_UI_MATERIAL_FRAMES")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(120);
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT);
    if let Some(info) = renderer.adapter_info() {
        println!("adapter: {} ({:?})", info.name, info.backend);
    }
    let mut text = TextRenderer::new();
    for name in names {
        let Some(theme) = Theme::named(&name) else {
            crate::registry::fatal(format!("no theme named {name:?}"));
        };
        let theme = theme.with_reduce_motion(true);
        let base = theme.tokens.colors.surfaces.base;
        // The shot.
        let (scene, backdrop) = frame(&theme, &mut text);
        let regions = backdrop.regions.len();
        renderer.set_backdrop(Some(std::sync::Arc::new(backdrop)));
        let slug: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        renderer.render_png(&scene, base, format!("{out}/material-{slug}.png"));
        // The timing: every frame built and rendered from scratch.
        let mut samples = Vec::with_capacity(frames);
        for _ in 0..frames {
            let started = Instant::now();
            let (scene, backdrop) = frame(&theme, &mut text);
            renderer.set_backdrop(Some(std::sync::Arc::new(backdrop)));
            let _pixels = renderer.render(&scene, base);
            samples.push(started.elapsed());
        }
        samples.sort_unstable();
        println!(
            "{name:<12} regions {regions}  median {:>6.2} ms  p95 {:>6.2} ms  ({frames} frames, {WIDTH}x{HEIGHT})",
            percentile(&samples, 0.5).as_secs_f64() * 1000.0,
            percentile(&samples, 0.95).as_secs_f64() * 1000.0,
        );
    }
}
