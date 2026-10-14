//! Capture the real hosted editor, including file-scoped collaborator presence.
//!
//! cargo build --locked -p typst-editor --example showcase
//! target/debug/examples/showcase output.png 1.5 dark [output.mp4]
//! Put a symlink to crates/sdk/ui-core/assets/fonts beside this binary first.
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
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, CubicBez, ParamCurve, Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, WindowEvent};

const VIEWPORT: (u32, u32) = (1440, 810);
const FPS: u32 = 60;
const FRAMES: u32 = 210;
const HEADING: &str = "A gentler handoff";
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
    let caret = sample
        .source
        .find("A good transition")
        .ok_or("capture document has no abstract")?;
    app.harness().update_presence(|presence| {
        presence.set_display_name(ui::UserId(ADA.into()), "Ada");
        presence.set_caret(&key, ui::UserId(ADA.into()), caret);
        true
    });
    app.harness().relayout();
    let target = app
        .surface_mut::<TypstApp>()
        .heading_target(HEADING)
        .ok_or("the outline does not expose the capture heading")?
        .center();
    let start = Point::new(f64::from(VIEWPORT.0) * 0.48, f64::from(VIEWPORT.1) * 0.30);
    let curve = CubicBez::new(
        start,
        Point::new(start.x - 100.0, start.y + 70.0),
        Point::new(target.x + 100.0, target.y + 70.0),
        target,
    );
    let size = app.harness().frame_output().device_size;
    let mut renderer = OffscreenRenderer::new(size.0, size.1);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let mut encoder = Recording::start(video, size)?;
    let epoch = ui::time::now();
    let mut clicked = false;
    let mut edited = false;
    let source = sample.source;
    let heading_byte = source.find(HEADING).ok_or("missing capture heading")?;
    let heading_line = source
        .get(..heading_byte)
        .ok_or("invalid source offset")?
        .lines()
        .count();
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
        ui::time::testing::with_monotonic_time(at, || {
            let point = cursor_at(curve, seconds);
            // The very same position drives hit-testing and the shared peer
            // overlay. The click goes through the real hosted event route;
            // no screenshot-only preview or prearranged scroll replaces it.
            app.harness().send(&WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(point.x, point.y),
            });
            app.harness().update_presence(|presence| {
                presence.upsert_remote_cursor(ADA.into(), pointer_position(point), Some("Ada"));
                true
            });
            let just_clicked = !clicked && seconds >= 2.0;
            if just_clicked {
                app.harness().click_at(target.x, target.y);
                if app.surface_mut::<TypstApp>().caret_line_col().0 != heading_line {
                    return Err(std::io::Error::other(
                        "outline click did not move the source caret",
                    ));
                }
                app.harness().update_presence(|presence| {
                    presence.set_caret(&key, ui::UserId(ADA.into()), heading_byte);
                    true
                });
                clicked = true;
            }
            let just_edited = !edited && seconds >= 2.4;
            if just_edited {
                append_heading(app, &file, source, heading_byte)?;
                edited = true;
            }
            app.harness().relayout();
            let background = theme.tokens.colors.surfaces.content;
            // Import the harness's captured backdrop into the reusable
            // renderer when the composition changes. Ordinary cursor frames
            // need only the final render, not a fresh GPU/renderer per frame.
            if frame == 0 || just_clicked || just_edited {
                let _ = app.harness().render_rgba_with(&mut renderer, background);
            }
            let scene = capture_scene(app, &mut text, theme);
            if frame == 0 {
                renderer.render_png(app.harness().scene(), background, output);
            }
            encoder.write(&renderer.render_resilient(&scene, background))
        })?;
    }
    encoder.finish()?;
    Ok(())
}

fn append_heading(
    app: &mut TestApp<TypstEditorShell>,
    file: &ui::tilo_resource::Id,
    source: &str,
    heading_byte: usize,
) -> std::io::Result<()> {
    let end = heading_byte
        .checked_add(HEADING.len())
        .ok_or_else(|| std::io::Error::other("heading offset overflow"))?;
    // Resource text splices address Unicode scalars, not source byte offsets.
    let at = source
        .get(..end)
        .ok_or_else(|| std::io::Error::other("invalid heading offset"))?
        .chars()
        .count();
    let path = typst_editor::Workspace::paths().files.item(file).source;
    let edit = path.splice(at, 0, ", not a jump");
    let invoke = ui::tilo_resource::Invoke::edit(&edit).map_err(std::io::Error::other)?;
    app.invoke(invoke).map_err(std::io::Error::other)?;
    app.harness()
        .wait_for_visuals(ui::time::Duration::from_secs(10))
        .map_err(std::io::Error::other)?;
    if !app
        .state()
        .file(file)
        .is_some_and(|file| file.source.contains("A gentler handoff, not a jump"))
    {
        return Err(std::io::Error::other("live heading edit did not persist"));
    }
    Ok(())
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded window coordinates fit f32 with subpixel precision"
)]
fn pointer_position(point: Point) -> [f32; 2] {
    [point.x as f32, point.y as f32]
}

fn capture_scene(
    app: &mut TestApp<TypstEditorShell>,
    text: &mut TextRenderer,
    theme: &Theme,
) -> Scene {
    let harness = app.harness();
    let mut scene = harness.scene().clone();
    // The live window host, rather than an app widget, owns this overlay.
    // A headless capture uses that same shared presence painter explicitly.
    ui::presence::draw_remote_presence(
        &mut scene,
        Affine::scale(f64::from(ui::offscreen::capture_device_scale())),
        Rect::new(0.0, 0.0, f64::from(VIEWPORT.0), f64::from(VIEWPORT.1)),
        text,
        theme,
        harness.presence(),
    );
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
        let curve = CubicBez::new(start, (500.0, 250.0), (200.0, 400.0), end);
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
