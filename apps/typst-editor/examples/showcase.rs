//! Capture the real hosted editor, including file-scoped collaborator presence.
//!
//! cargo build --locked -p typst-editor --example showcase
//! target/debug/examples/showcase output.png 1.5 dark [output.mp4]
//! Link a prepared fixture font catalog beside this binary as `fonts` first.
//! Video requires ffmpeg with libx264; the default video path is beside the PNG.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use typst_editor::TypstEditorShell;
use typst_editor::app::TypstApp;
use typst_editor::samples::{DEFAULT_INDEX, SAMPLES};
use ui::framework::testing::TestApp;
use ui::offscreen::OffscreenRenderer;
use ui::presence::capability::presence_key;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, CubicBez, ParamCurve, Point, Stroke};
use vello::peniko::{Color, Fill};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, WindowEvent};
use winit::keyboard::{Key, NamedKey};

const VIEWPORT: (u32, u32) = (1440, 810);
const FPS: u32 = 60;
const FRAMES: u32 = 240;
const EQUATION: &str = "$ s(tau) = 10 tau^3 - 15 tau^4 + 6 tau^5 $";
const ADA: &str = "ada";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let output = PathBuf::from(args.next().ok_or("expected output PNG path")?);
    let scale: f32 = args.next().as_deref().unwrap_or("1.5").parse()?;
    let name = args.next().unwrap_or_else(|| "dark".to_owned());
    let video = args
        .next()
        .map_or_else(|| output.with_extension("mp4"), PathBuf::from);
    if output == video {
        return Err("screenshot and video need different paths".into());
    }
    let theme = Theme::named(&name)
        .ok_or("unknown theme")?
        .with_ui_scale(scale);
    let mut app = TestApp::<TypstEditorShell>::capture_at(None, VIEWPORT)?;
    let result = capture(&mut app, &theme, &output, &video);
    app.harness().close_components();
    result
}

fn capture(
    app: &mut TestApp<TypstEditorShell>,
    theme: &Theme,
    output: &Path,
    video: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    app.harness().set_theme(theme.clone());
    let key = presence_key(app.surface_mut::<TypstApp>().source_widget())
        .ok_or("the source editor exposes no presence scope")?;
    let sample = SAMPLES.get(DEFAULT_INDEX).ok_or("missing capture sample")?;
    let caret = abstract_caret(sample.source)?;
    app.harness().update_presence(|presence| {
        presence.set_display_name(ui::UserId(ADA.into()), "Ada");
        presence.set_caret(&key, ui::UserId(ADA.into()), caret);
        true
    });
    app.harness().relayout();
    // Keep the pointer clear of the source completion menu and then guide it
    // toward the document, where the accepted math symbol will appear.
    let target = Point::new(f64::from(VIEWPORT.0) * 0.78, f64::from(VIEWPORT.1) * 0.55);
    let start = Point::new(f64::from(VIEWPORT.0) * 0.48, f64::from(VIEWPORT.1) * 0.30);
    let curve = CubicBez::new(
        start,
        Point::new(start.x - 100.0, start.y + 70.0),
        Point::new(target.x + 100.0, target.y + 70.0),
        target,
    );
    let size = app.harness().frame_output().device_size;
    // Use stable segment ordering for repeatable headless captures;
    // the fine rasterizer remains the production GPU renderer.
    let mut renderer = OffscreenRenderer::try_new_with_cpu_preprocessing(size.0, size.1)?;
    let mut encoder = Recording::start(video, size)?;
    let epoch = ui::time::now();
    let mut positioned = false;
    let mut typing = [(42, " "), (51, "+"), (60, " "), (69, "a"), (78, "l")]
        .into_iter()
        .peekable();
    let mut accepted = false;
    let source = sample.source;
    let equation_byte = source.find(EQUATION).ok_or("missing capture equation")?;
    let insertion = equation_byte
        .checked_add(EQUATION.len())
        .and_then(|end| end.checked_sub(" $".len()))
        .ok_or("equation offset overflow")?;
    let expected = format!(
        "{} + alpha{}",
        source.get(..insertion).ok_or("invalid insertion")?,
        source.get(insertion..).ok_or("invalid insertion")?
    );
    let file = app
        .state()
        .files
        .iter()
        .find(|(_, file)| file.name.0 == sample.name)
        .map(|(id, _)| id.clone())
        .ok_or("missing capture file")?;
    for frame in 0..FRAMES {
        let seconds = f64::from(frame) / f64::from(FPS);
        let at = epoch
            .checked_add(ui::time::Duration::from_secs_f64(seconds))
            .ok_or("recording clock overflow")?;
        let result = ui::time::testing::with_monotonic_time(at, || {
            let point = cursor_at(curve, seconds);
            // The local arrow and hit-testing use the same position. All edits
            // use the real hosted accessibility and keyboard routes.
            app.harness().send(&WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(point.x, point.y),
            });
            let just_positioned = !positioned && frame >= 30;
            if just_positioned {
                position_caret(app, source, insertion)?;
                positioned = true;
            }
            // One real keystroke every 150 ms, followed by a long enough hold
            // to read the production completion list's names and glyphs.
            let just_typed = typing.peek().is_some_and(|(at, _)| frame >= *at);
            if just_typed && let Some((_, character)) = typing.next() {
                app.harness().type_text(character);
            }
            let just_accepted = !accepted && frame >= 126;
            if just_accepted {
                accept_completion(app, &file, &expected)?;
                accepted = true;
            }
            app.harness().relayout();
            let presence = app.harness().presence();
            if !presence.remote_cursors.is_empty()
                || presence.caret_for(&key, &ui::UserId(ADA.into())) != Some(caret)
            {
                return Err(std::io::Error::other(
                    "local pointer changed remote author presence",
                ));
            }
            let background = theme.tokens.colors.surfaces.content;
            // Import the harness's captured backdrop into the reusable
            // renderer when the composition changes. Ordinary cursor frames
            // need only the final render, not a fresh GPU/renderer per frame.
            if frame == 0 || just_positioned || just_typed || just_accepted {
                let _ = app.harness().render_rgba_with(&mut renderer, background);
            }
            let scene = capture_scene(app, point);
            if frame == 0 {
                renderer.render_png(&scene, background, output);
            }
            encoder.write(&renderer.render_resilient(&scene, background))
        });
        if let Err(error) = result {
            // Flush even a failed take so its last visible popup can be
            // inspected rather than leaving an unreadable MP4 container.
            encoder.finish()?;
            return Err(error.into());
        }
    }
    if !accepted {
        return Err("recording did not show an accepted live edit".into());
    }
    encoder.finish()?;
    Ok(())
}

fn abstract_caret(source: &str) -> std::io::Result<usize> {
    let caret = source
        .find("A good transition")
        .ok_or_else(|| std::io::Error::other("capture document has no abstract"))?;
    Ok(caret)
}

fn position_caret(
    app: &mut TestApp<TypstEditorShell>,
    source: &str,
    byte: usize,
) -> std::io::Result<()> {
    let editor = app
        .harness()
        .a11y_node_named("Typst source")
        .ok_or_else(|| std::io::Error::other("missing accessible source editor"))?;
    app.harness()
        .perform_a11y_action(editor, &ui::a11y::Action::Focus)
        .map_err(std::io::Error::other)?;
    // The fixture prefix contains no combining sequences: its scalar and
    // accessible grapheme offsets agree, including the non-ASCII byline.
    let at = source
        .get(..byte)
        .ok_or_else(|| std::io::Error::other("invalid caret offset"))?
        .chars()
        .count();
    app.harness()
        .perform_a11y_action(
            editor,
            &ui::a11y::Action::SetSelection {
                anchor: at,
                focus: at,
            },
        )
        .map_err(std::io::Error::other)
}

fn accept_completion(
    app: &mut TestApp<TypstEditorShell>,
    file: &ui::tilo_resource::Id,
    expected: &str,
) -> std::io::Result<()> {
    let choice = "alpha";
    if !completion_visible(app, choice) {
        return Err(std::io::Error::other(
            "typing did not expose the real math completion popup",
        ));
    }
    app.harness().send(&ui::synthetic_input::key_press(
        Key::Named(NamedKey::Tab),
        None,
    ));
    app.harness()
        .wait_for_visuals(ui::time::Duration::from_secs(10))
        .map_err(std::io::Error::other)?;
    let state = app.state();
    if state.file(file).is_none_or(|file| file.source != expected) {
        let equation = state
            .file(file)
            .and_then(|file| file.source.lines().find(|line| line.starts_with("$ s(")));
        return Err(std::io::Error::other(format!(
            "Tab did not accept alpha into the live source: {equation:?}",
        )));
    }
    if completion_visible(app, choice) {
        return Err(std::io::Error::other("accepted completion stayed open"));
    }
    Ok(())
}

fn completion_visible(app: &mut TestApp<TypstEditorShell>, choice: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .iter()
        .any(|(_, node)| node.label() == Some(choice) && !node.is_hidden())
}

fn capture_scene(app: &mut TestApp<TypstEditorShell>, point: Point) -> Scene {
    let mut scene = app.harness().scene().clone();
    // Headless captures have no OS cursor. Paint only the local arrow here;
    // Ada remains the file-scoped caret painted by draw_peer_caret_labeled.
    let mut arrow = BezPath::new();
    arrow.move_to((0.0, 0.0));
    arrow.line_to((0.0, 20.0));
    arrow.line_to((5.5, 15.0));
    arrow.line_to((10.0, 23.0));
    arrow.line_to((14.0, 21.0));
    arrow.line_to((9.5, 13.0));
    arrow.line_to((17.0, 13.0));
    arrow.close_path();
    let transform = Affine::translate((point.x, point.y))
        .then_scale(f64::from(ui::offscreen::capture_device_scale()));
    scene.stroke(
        &Stroke::new(2.5),
        transform,
        Color::from_rgb8(24, 24, 24),
        None,
        &arrow,
    );
    scene.fill(Fill::NonZero, transform, Color::WHITE, None, &arrow);
    scene
}

fn min_jerk(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}

fn cursor_at(curve: CubicBez, seconds: f64) -> Point {
    // One curve, one minimum-jerk time profile, with quiet holds at both ends.
    curve.eval(min_jerk((seconds - 0.45) / 1.55))
}

struct Recording {
    process: Child,
    input: Option<ChildStdin>,
}

impl Recording {
    fn start(path: &Path, size: (u32, u32)) -> std::io::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut process = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "warning",
                "-f",
                "rawvideo",
            ])
            .args(["-pixel_format", "rgba", "-video_size"])
            .arg(format!("{}x{}", size.0, size.1))
            .args(["-framerate", &FPS.to_string(), "-i", "pipe:0", "-an"])
            .args([
                "-c:v", "libx264", "-threads", "2", "-preset", "medium", "-crf", "18",
            ])
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;
        let input = process.stdin.take();
        Ok(Self { process, input })
    }

    fn write(&mut self, rgba: &[u8]) -> std::io::Result<()> {
        self.input
            .as_mut()
            .ok_or_else(|| std::io::Error::other("video encoder input is closed"))?
            .write_all(rgba)
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.input.take();
        let status = self.process.wait()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "ffmpeg exited with {status}"
            )));
        }
        Ok(())
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.input.take();
        if !matches!(self.process.try_wait(), Ok(Some(_))) {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_has_one_smooth_stroke_and_stable_endpoint_holds() {
        let start = Point::new(600.0, 200.0);
        let end = Point::new(100.0, 350.0);
        let curve = CubicBez::new(start, (500.0, 250.0).into(), (200.0, 400.0).into(), end);
        assert_eq!(cursor_at(curve, 0.0), start);
        assert_eq!(cursor_at(curve, 0.45), start);
        assert_eq!(cursor_at(curve, 2.0), end);
        assert_eq!(cursor_at(curve, 3.5), end);
        let mut previous = 0.0;
        for frame in 0..=FRAMES {
            let t = f64::from(frame) / f64::from(FRAMES);
            let progress = min_jerk(t);
            assert!(progress >= previous);
            previous = progress;
        }
        let epsilon = 0.0001;
        assert!(min_jerk(epsilon) / epsilon < 0.0001);
        assert!((1.0 - min_jerk(1.0 - epsilon)) / epsilon < 0.0001);
    }
}
