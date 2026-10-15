//! Capture a real hosted Books reader, including a pointer-driven highlight.
//!
//! Run from the checkout root (ffmpeg with libx264 and a Vulkan adapter required):
//! ```text
//! cargo run --locked -p ebook --features test-support --example showcase -- target/ebook-showcase
//! ```
//! Writes 1440×810 showcase.png, showcase-selected.png, showcase.mp4 (210 frames
//! at 60 fps), and showcase.json with the checked passage and attributed history.
//! The verified build-time font catalog is linked beside the executable, not
//! the source metadata-only fonts directory. No display or compositor is opened.
//!
//! The app scene and input are real. Only the local arrow and the SDK's normal
//! window-host presence layer are composed over the hosted harness scene. Ada
//! is a capture fixture, not a network connection; her highlight and comment
//! are real admitted actions attributed to her in the resource's history.

#[cfg(not(feature = "test-support"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("run this example with --features test-support")
}

#[cfg(feature = "test-support")]
fn main() -> anyhow::Result<()> {
    capture::run()
}

#[cfg(feature = "test-support")]
mod capture {
    #![allow(
        clippy::arithmetic_side_effects,
        reason = "capture coordinates and the bounded 210-frame timeline use small, finite arithmetic"
    )]

    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, Stdio};

    use anyhow::{Context, Result, ensure};
    use ebook::app::showcase::{BOOK_ID, DragTarget, ShowcasePassages};
    use ebook::model::{AnnoKind, HiColor};
    use ebook::{BooksApp, Ebook, EbookShell};
    use serde_json::json;
    use ui::framework::testing::TestApp;
    use ui::icons::Icon;
    use ui::offscreen::OffscreenRenderer;
    use ui::permissions::UserId;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use ui::tilo_actions::ActionInvoker;
    use ui::tilo_resource::Resource;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect};

    const WIDTH: u32 = 1440;
    const HEIGHT: u32 = 810;
    const FPS: u32 = 60;
    const FRAMES: u32 = 210;
    const ADA: &str = "ada";
    const NOTE: &str = "Ada: A small promise, kept. Worth remembering.";
    const COLOR: HiColor = HiColor::Yellow;

    pub(super) fn run() -> Result<()> {
        let mut args = std::env::args_os().skip(1);
        let output = args
            .next()
            .map_or_else(|| PathBuf::from("target/ebook-showcase"), PathBuf::from);
        ensure!(
            args.next().is_none(),
            "expected at most one output directory"
        );
        fs::create_dir_all(&output)?;
        // Atomic image publication provisions an absolute parent directory.
        let output = fs::canonicalize(output)?;
        link_fonts()?;
        ui::i18n::I18nManager::init("en-US");
        ui::offscreen::set_capture_device_scale(1);
        tilo_clock::freeze_wall(
            tilo_clock::UNIX_EPOCH + tilo_clock::Duration::from_millis(1_767_225_600_000),
        );
        let epoch = ui::time::now();
        let (mut app, passages, drag, peer) =
            tilo_clock::testing::with_monotonic_time(epoch, || {
                let mut app = TestApp::<EbookShell>::capture_at(Some("read"), (WIDTH, HEIGHT))?;
                app.harness().set_theme(Theme::dark().with_ui_scale(1.5));
                let passages = app
                    .surface_mut::<BooksApp>()
                    .stage_showcase()
                    .map_err(anyhow::Error::msg)?;
                seed_collaborator(&mut app, &passages)?;
                app.harness().recompose()?;
                app.harness()
                    .render_png(&output.join("showcase-before.png"));
                let drag = app
                    .surface_mut::<BooksApp>()
                    .showcase_drag(&passages.reader)
                    .map_err(anyhow::Error::msg)?;
                let peer = app
                    .surface_mut::<BooksApp>()
                    .showcase_peer_point(&passages.collaborator)
                    .context("the reader painted no text for Ada's pointer")?;
                Ok::<_, anyhow::Error>((app, passages, drag, peer))
            })?;
        // Even an encoder or assertion failure must close hosted components.
        let result = record(&mut app, &output, &passages, (drag, peer), epoch);
        app.harness().close_components();
        result?;
        println!(
            "wrote {} (1440×810, 210 frames at 60 fps)",
            output.display()
        );
        Ok(())
    }

    fn seed_collaborator(app: &mut TestApp<EbookShell>, passages: &ShowcasePassages) -> Result<()> {
        let passage = &passages.collaborator;
        let before: Vec<_> = app
            .state()
            .annotations_for(BOOK_ID)
            .iter()
            .map(|mark| mark.id)
            .collect();
        app.call_by(
            ActionInvoker::actor(ADA),
            <Ebook as Resource>::KIND,
            "highlight",
            json!({
                "book": passage.book, "chapter": passage.chapter, "color": "green",
                "replacing": [], "pieces": [{
                    "block": passage.block, "start": passage.start, "end": passage.end,
                    "excerpt": passage.excerpt,
                }],
            }),
        )?;
        let highlight = app
            .state()
            .annotations_for(BOOK_ID)
            .into_iter()
            .find(|mark| mark.kind == AnnoKind::Highlight && !before.contains(&mark.id))
            .context("Ada's highlight action did not create a mark")?;
        app.call_by(
            ActionInvoker::actor(ADA),
            <Ebook as Resource>::KIND,
            "add-comment",
            json!({ "highlight": highlight.id, "text": NOTE }),
        )?;
        ensure!(
            app.state().annotations_for(BOOK_ID).iter().any(|mark| {
                mark.kind == AnnoKind::Note && mark.group == highlight.group && mark.note == NOTE
            }),
            "Ada's comment did not reach the annotation model"
        );
        let history = app.history();
        ensure!(
            history
                .iter()
                .filter(|entry| {
                    entry
                        .invoker
                        .as_ref()
                        .is_some_and(|invoker| invoker.actor == ADA)
                })
                .count()
                == 2,
            "Ada's actions lost their collaborator attribution"
        );
        Ok(())
    }

    fn record(
        app: &mut TestApp<EbookShell>,
        output: &Path,
        passages: &ShowcasePassages,
        targets: (DragTarget, Point),
        epoch: ui::time::Instant,
    ) -> Result<()> {
        let before = app.state().annotations_for(BOOK_ID).len();
        let mut renderer = OffscreenRenderer::try_new(WIDTH, HEIGHT)?;
        let mut text = TextRenderer::new();
        let mut encoder = Encoder::new(&output.join("showcase.mp4"))?;
        let mut swatch = None;
        for frame in 0..FRAMES {
            let at = epoch
                + ui::time::Duration::from_nanos(u64::from(frame) * 1_000_000_000 / u64::from(FPS));
            tilo_clock::testing::with_monotonic_time(at, || -> Result<()> {
                let pointer = advance_frame(app, frame, passages, targets, &mut swatch)?;
                if frame == 161 {
                    verify_highlight(app, passages, before)?;
                }
                let scene = compose(app, pointer, &mut text)?;
                let background = app.harness().theme().tokens.colors.surfaces.content;
                let rgba = renderer
                    .try_render_resilient(&scene, background)
                    .map_err(anyhow::Error::msg)?;
                ensure!(
                    rgba.len() == usize::try_from(WIDTH * HEIGHT * 4)?,
                    "incomplete RGBA frame"
                );
                encoder.write(&rgba)?;
                if frame == 128 || frame == FRAMES - 1 {
                    let name = if frame == 128 {
                        "showcase-selected.png"
                    } else {
                        "showcase.png"
                    };
                    image::save_buffer(
                        output.join(name),
                        &rgba,
                        WIDTH,
                        HEIGHT,
                        image::ColorType::Rgba8,
                    )?;
                }
                Ok(())
            })?;
        }
        encoder.finish()?;
        verify_highlight(app, passages, before)?;
        let history: Vec<_> = app
            .history()
            .into_iter()
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == ADA)
            })
            .collect();
        fs::write(
            output.join("showcase.json"),
            serde_json::to_vec_pretty(&json!({
                "viewport": [WIDTH, HEIGHT], "theme": "dark", "ui_scale": 1.5,
                "frames": FRAMES, "fps": FPS, "seconds": 3.5,
                "selected_passage": passages.reader,
                "collaborator_passage": passages.collaborator,
                "collaborator_history": history,
                "annotation_count_before_click": before,
                "annotation_count_after_click": app.state().annotations_for(BOOK_ID).len(),
                "limitations": [
                    "Ada is a harness presence fixture, not a network peer.",
                    "Annotation cards have no author field; attribution is in real action history.",
                    "GPU antialiasing can differ by a channel value; captures are not byte-identical goldens."
                ],
            }))?,
        )?;
        Ok(())
    }

    fn advance_frame(
        app: &mut TestApp<EbookShell>,
        frame: u32,
        passages: &ShowcasePassages,
        (drag, peer): (DragTarget, Point),
        swatch: &mut Option<Point>,
    ) -> Result<Point> {
        let pointer = pointer_at(frame, drag, *swatch)?;
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(pointer.x, pointer.y));
        match frame {
            54 | 159 => app.harness().send(&ui::synthetic_input::mouse_press()),
            113 | 160 => app.harness().send(&ui::synthetic_input::mouse_release()),
            _ => {}
        }
        if frame == 113 {
            ensure!(
                app.surface_mut::<BooksApp>().showcase_selection().as_ref()
                    == Some(&passages.reader),
                "the pointer drag did not select exactly the expected sentence"
            );
            *swatch = Some(
                app.surface_mut::<BooksApp>()
                    .showcase_swatch(COLOR)
                    .context("the real selection toolbar has no yellow colour control")?,
            );
        }
        // The headless harness carries presence but, unlike a native
        // window, does not paint its generic window overlay. Feed its
        // real context into that same SDK painter below.
        app.harness().update_presence(|presence| {
            presence.set_display_name(UserId(ADA.into()), "Ada");
            presence.upsert_remote_cursor(
                ADA.into(),
                [ui::num::f32_of_f64(peer.x), ui::num::f32_of_f64(peer.y)],
                Some("Ada"),
            );
            true
        });
        app.harness().recompose()?;
        Ok(pointer)
    }

    fn verify_highlight(
        app: &mut TestApp<EbookShell>,
        passages: &ShowcasePassages,
        before: usize,
    ) -> Result<()> {
        let annotations = app.state().annotations_for(BOOK_ID);
        let passage = &passages.reader;
        ensure!(
            annotations.len() == before + 1,
            "clicking the colour did not add exactly one annotation"
        );
        let highlight = annotations
            .iter()
            .find(|mark| {
                mark.kind == AnnoKind::Highlight
                    && mark.chapter == passage.chapter
                    && mark.block == passage.block
                    && mark.start == passage.start
                    && mark.end == passage.end
                    && mark.excerpt == passage.excerpt
                    && mark.color == COLOR
            })
            .context(
                "the saved highlight does not match the pointer-selected sentence and colour",
            )?;
        let reader = app.surface_mut::<BooksApp>();
        ensure!(
            reader.showcase_selection().is_none(),
            "the highlight did not replace the selection"
        );
        ensure!(
            reader.showcase_annotation_visible(highlight.id),
            "the new highlight has no visible card in the real annotations rail"
        );
        ensure!(
            reader.showcase_annotations_fit(),
            "the showcase clips an annotation card"
        );
        Ok(())
    }

    fn pointer_at(frame: u32, drag: DragTarget, swatch: Option<Point>) -> Result<Point> {
        let rest = Point::new(drag.start.x - 45.0, drag.start.y + 75.0);
        Ok(match frame {
            0..=23 => rest,
            24..=53 => curved(rest, drag.start, fraction(frame, 24, 53), -24.0),
            54 => drag.start,
            55..=112 => curved(drag.start, drag.end, fraction(frame, 55, 112), -12.0),
            113..=129 => drag.end,
            130..=158 => curved(
                drag.end,
                swatch.context("missing colour control")?,
                fraction(frame, 130, 158),
                28.0,
            ),
            _ => swatch.context("missing colour control")?,
        })
    }

    fn fraction(frame: u32, first: u32, last: u32) -> f64 {
        f64::from(frame - first) / f64::from(last - first)
    }

    fn min_jerk(t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
    }

    fn curved(from: Point, to: Point, t: f64, bend: f64) -> Point {
        let t = min_jerk(t);
        let delta = to - from;
        let length = delta.hypot().max(1.0);
        let arc = 4.0 * t * (1.0 - t) * bend / length;
        Point::new(
            from.x + delta.x * t - delta.y * arc,
            from.y + delta.y * t + delta.x * arc,
        )
    }

    fn compose(
        app: &mut TestApp<EbookShell>,
        pointer: Point,
        text: &mut TextRenderer,
    ) -> Result<Scene> {
        let margin = app.surface_mut::<BooksApp>().showcase_peer_margin();
        let harness = app.harness();
        let mut scene = Scene::new();
        scene.append(harness.frame_render_input().scene, None);
        let theme = harness.theme();
        let indicators = ui::presence::draw_remote_presence(
            &mut scene,
            Affine::IDENTITY,
            margin,
            text,
            theme,
            harness.presence(),
        );
        ensure!(!indicators.is_empty(), "Ada's margin indicator disappeared");
        ensure!(
            indicators.iter().all(|rect| rect.x1 <= margin.x1),
            "Ada's indicator or shadow escaped the prose-safe margin"
        );
        // Use the shared arrow glyph, with its tip (4, 3) as the input hotspot.
        let scale = f64::from(theme.ui_scale);
        let origin = Point::new(pointer.x - 4.0 * scale, pointer.y - 3.0 * scale);
        let rect = Rect::from_origin_size(origin, (24.0 * scale, 24.0 * scale));
        Icon::MousePointer2.draw_with_stroke(
            &mut scene,
            Affine::IDENTITY,
            rect,
            theme.tokens.colors.surfaces.content,
            3.0 * scale,
        );
        Icon::MousePointer2.draw_filled(&mut scene, Affine::IDENTITY, rect, theme.text_primary);
        Ok(scene)
    }

    fn link_fonts() -> Result<()> {
        let root = ui::font_catalog::checkout_root();
        // Fail rather than silently shape with whatever the host installed.
        ui::font_catalog::FontCatalog::product(&root, "en-US").load()?;
        let executable = std::env::current_exe()?;
        let link = executable
            .parent()
            .context("example executable has no parent")?
            .join("fonts");
        match fs::symlink_metadata(&link) {
            Ok(_) => {
                ui::font_catalog::FontCatalog::product(&link, "en-US").load()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(root, link)?;
                #[cfg(not(unix))]
                anyhow::bail!(
                    "link or copy {} to {} before capture",
                    root.display(),
                    link.display()
                );
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    struct Encoder {
        child: Child,
        input: Option<ChildStdin>,
        finished: bool,
    }

    impl Encoder {
        fn new(path: &Path) -> Result<Self> {
            let mut child = Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgba",
                    "-video_size",
                    "1440x810",
                    "-framerate",
                    "60",
                    "-i",
                    "pipe:0",
                    "-an",
                    "-c:v",
                    "libx264",
                    "-threads",
                    "2",
                    "-preset",
                    "medium",
                    "-crf",
                    "18",
                    "-pix_fmt",
                    "yuv420p",
                    "-frames:v",
                    "210",
                    "-map_metadata",
                    "-1",
                    "-movflags",
                    "+faststart",
                ])
                .arg(path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .context("start ffmpeg (install a build with libx264)")?;
            let input = child.stdin.take();
            Ok(Self {
                child,
                input,
                finished: false,
            })
        }

        fn write(&mut self, rgba: &[u8]) -> Result<()> {
            self.input
                .as_mut()
                .context("ffmpeg stdin is closed")?
                .write_all(rgba)
                .context("write RGBA frame to ffmpeg")
        }

        fn finish(mut self) -> Result<()> {
            drop(self.input.take());
            let status = self.child.wait()?;
            self.finished = true;
            ensure!(status.success(), "ffmpeg failed: {status}");
            Ok(())
        }
    }

    impl Drop for Encoder {
        fn drop(&mut self) {
            if !self.finished {
                drop(self.input.take());
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}
