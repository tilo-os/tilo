//! Display-free review images and a four-second, 60 fps interaction recording.
//! Run with an output directory and optional `--video`. Video uses `GStreamer`'s
//! `OpenH264` encoder; all frames and pointer events come from the live app harness.

use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use calculator::CalculatorShell;
use ui::a11y::{AccessNode, Action};
use ui::headless::InspectionNode;
use ui::offscreen::OffscreenRenderer;
use ui::prelude::TestApp;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

type Capture = TestApp<CalculatorShell>;

fn main() -> Result<(), Box<dyn Error>> {
    link_fonts()?;
    let out = PathBuf::from(std::env::args().nth(1).ok_or("expected output directory")?);
    std::fs::create_dir_all(&out)?;
    for (palette, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for mode in ["calculator", "graph2d", "graph3d"] {
            let mut app = capture(mode, theme.clone(), (1440, 810))?;
            if mode == "graph2d" {
                app.harness()
                    .send(&ui::synthetic_input::cursor_moved(832.0, 523.0));
            }
            app.harness()
                .render_png(&out.join(format!("{palette}-{mode}.png")));
        }
    }
    for mode in ["calculator", "graph2d", "graph3d"] {
        let mut app = capture(mode, Theme::dark(), (600, 900))?;
        app.harness()
            .render_png(&out.join(format!("narrow-{mode}.png")));
    }
    std::fs::copy(
        out.join("dark-calculator.png"),
        out.join("calculator-showcase.png"),
    )?;
    if std::env::args().any(|arg| arg == "--video") {
        record(&out)?;
    }
    Ok(())
}

fn link_fonts() -> Result<(), Box<dyn Error>> {
    let binary = std::env::current_exe()?;
    let fonts = binary.parent().ok_or("binary has no parent")?.join("fonts");
    if !fonts.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/sdk/ui-core/assets/fonts"),
            &fonts,
        )?;
    }
    Ok(())
}

fn capture(mode: &str, theme: Theme, viewport: (u32, u32)) -> Result<Capture, Box<dyn Error>> {
    let mut app = Capture::capture_at(Some(mode), viewport)?;
    app.harness().set_theme(theme.with_ui_scale(1.5));
    if mode == "calculator" {
        app.harness().perform_named(
            "calculator.expression",
            &Action::SetValue("(sqrt(5)+1)/2".into()),
        )?;
        app.settle();
    }
    Ok(app)
}

fn control(app: &mut Capture, target: &str) -> Result<Rect, Box<dyn Error>> {
    fn semantic(node: &AccessNode, target: &str) -> Option<Rect> {
        if node.action_target.as_deref() == Some(target) {
            return node.bounds.map(|b| Rect::new(b.x0, b.y0, b.x1, b.y1));
        }
        node.children
            .iter()
            .find_map(|child| semantic(child, target))
    }
    fn inspect(node: &InspectionNode, target: &str) -> Option<Rect> {
        node.semantics
            .as_ref()
            .and_then(|node| semantic(node, target))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| inspect(child, target))
            })
    }
    inspect(&app.harness().inspection().root, target)
        .ok_or_else(|| format!("missing control: {target}").into())
}

fn min_jerk(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}

fn curved(from: Point, to: Point, bend: Point, t: f64) -> Point {
    let u = min_jerk(t);
    Point::new(
        (1.0 - u).powi(2) * from.x + 2.0 * (1.0 - u) * u * bend.x + u * u * to.x,
        (1.0 - u).powi(2) * from.y + 2.0 * (1.0 - u) * u * bend.y + u * u * to.y,
    )
}

fn record(out: &Path) -> Result<(), Box<dyn Error>> {
    let mut app = capture("calculator", Theme::dark(), (1440, 810))?;
    let field = control(&mut app, "calculator.expression")?;
    let start = Point::new(field.x0 + field.height(), field.center().y);
    let tab2 = control(&mut app, "calculator.mode:1")?.center();
    // Resolve the second tab position after the leading plot panel appears.
    app.harness()
        .perform_named("calculator.mode:1", &Action::Activate)?;
    app.settle();
    let tab3 = control(&mut app, "calculator.mode:2")?.center();
    app.harness()
        .perform_named("calculator.mode:0", &Action::Activate)?;
    app.settle();
    let trace = Point::new(832.0, 523.0);
    let orbit = Point::new(950.0, 410.0);
    let mut encoder = Command::new("gst-launch-1.0")
        .args([
            "-q",
            "fdsrc",
            "fd=0",
            "!",
            "rawvideoparse",
            "format=rgba",
            "width=1440",
            "height=810",
            "framerate=60/1",
            "!",
            "videoconvert",
            "!",
            "openh264enc",
            "bitrate=8000000",
            "gop-size=60",
            "!",
            "h264parse",
            "!",
            "mp4mux",
            "!",
            "filesink",
        ])
        .arg(format!(
            "location={}",
            out.join("calculator-demo.mp4").display()
        ))
        .stdin(Stdio::piped())
        .spawn()?;
    let mut input = encoder.stdin.take().ok_or("encoder has no input")?;
    let mut renderer = OffscreenRenderer::new(1440, 810);
    let background = app.harness().theme().tokens.colors.surfaces.content;
    for frame in 0..240 {
        let time = f64::from(frame) / 60.0;
        let pointer = if time < 0.55 {
            start
        } else if time < 1.15 {
            curved(
                start,
                tab2,
                Point::new(start.x - 90.0, tab2.y + 30.0),
                (time - 0.55) / 0.6,
            )
        } else if time < 1.95 {
            curved(tab2, trace, Point::new(740.0, 160.0), (time - 1.2) / 0.75)
        } else if time < 2.4 {
            curved(trace, tab3, Point::new(1180.0, 310.0), (time - 1.95) / 0.4)
        } else if time < 2.85 {
            curved(tab3, orbit, Point::new(1180.0, 200.0), (time - 2.45) / 0.4)
        } else {
            curved(
                orbit,
                Point::new(1150.0, 465.0),
                Point::new(1060.0, 335.0),
                (time - 2.9) / 0.95,
            )
        };
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(pointer.x, pointer.y));
        if [69, 144, 174].contains(&frame) {
            app.harness().send(&ui::synthetic_input::mouse_press());
        }
        if [70, 145, 234].contains(&frame) {
            app.harness().send(&ui::synthetic_input::mouse_release());
        }
        app.settle();
        let mut scene = Scene::new();
        scene.append(app.harness().scene(), None);
        arrow(&mut scene, pointer);
        let rgba = renderer.render_resilient(&scene, background);
        input.write_all(&rgba)?;
        if [25, 105, 155, 225].contains(&frame) {
            renderer.render_png(&scene, background, out.join(format!("demo-{frame:03}.png")));
        }
    }
    drop(input);
    if !encoder.wait()?.success() {
        return Err("video encoding failed".into());
    }
    Ok(())
}

fn arrow(scene: &mut Scene, at: Point) {
    let mut path = BezPath::new();
    path.move_to((0.0, 0.0));
    path.line_to((0.0, 26.0));
    path.quad_to((0.0, 28.0), (2.0, 26.0));
    path.line_to((8.0, 20.0));
    path.line_to((13.0, 31.0));
    path.quad_to((14.0, 33.0), (16.0, 32.0));
    path.line_to((19.0, 30.0));
    path.quad_to((20.0, 29.0), (18.0, 26.0));
    path.line_to((14.0, 17.0));
    path.line_to((24.0, 16.0));
    path.quad_to((27.0, 15.0), (24.0, 13.0));
    path.line_to((2.0, 0.0));
    path.quad_to((0.0, -1.0), (0.0, 0.0));
    path.close_path();
    let t = Affine::translate(at.to_vec2());
    scene.fill(
        Fill::NonZero,
        Affine::translate((at.x + 2.0, at.y + 3.0)),
        Color::from_rgba8(0, 0, 0, 70),
        None,
        &path,
    );
    scene.fill(Fill::NonZero, t, Color::WHITE, None, &path);
    scene.stroke(&Stroke::new(1.4), t, Color::BLACK, None, &path);
}
