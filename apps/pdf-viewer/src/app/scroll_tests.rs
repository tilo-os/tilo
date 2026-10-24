//! Sustained scrolling through real PDFs, measured frame by frame.

use std::time::Duration;

use ui::headless::{Framed, viewport};
use ui::theme::Theme;

use super::PdfApp;
use super::tests::open_settled;

/// One frame of a driven scroll.
#[derive(Clone, Copy, Debug)]
struct FrameSample {
    /// Wheel dispatch plus the whole frame (sync, layout, draw) on the UI
    /// thread, in milliseconds.
    ui_ms: f64,
    /// The same frame's CPU time on the UI thread: its own work, apart
    /// from time it was preempted (falls back to `ui_ms` where the
    /// platform keeps no thread clock). Reported for measurements; it
    /// varies with the machine and its load, so no test asserts on it.
    cpu_ms: f64,
    /// What the frame did on the UI thread, counted: the same on any
    /// machine under any load.
    work: FrameWork,
    /// The scene rasterised on the GPU, when measured.
    gpu_ms: Option<f64>,
    scroll_y: f64,
    /// Visible pages painted with no engine pixels at all (paper only).
    blank_pages: usize,
    /// Visible pages not yet painted at the width they want (blank or
    /// another width scaled).
    unsharp_pages: usize,
    /// Page renders the reader is waiting on, and bitmaps it holds.
    pending: usize,
    held: usize,
}

/// A PDF with `pages` pages of dense vector artwork and text, slow to
/// rasterise on purpose: a stand-in for a heavy real-world document.
fn heavy_pdf(dir: &std::path::Path, pages: usize) -> std::path::PathBuf {
    use std::fmt::Write as _;
    let mut objects: Vec<String> = Vec::new();
    // 1: catalog, 2: pages, 3: font, then page/content pairs.
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + i * 2)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {pages} >>",
        kids.join(" ")
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for p in 0..pages {
        // A full-page ground first, so no page pixel the engine drew is
        // paper-white or the canvas colour.
        let mut content = String::from("0.80 0.86 0.94 rg 0 0 612 792 re f\n");
        // A field of small shaded triangles: thousands of paths to fill,
        // none of them light enough to pass for bare paper.
        let mut seed: u32 = 0x9e37_79b9 ^ (p as u32);
        let mut rand = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            f64::from(seed % 10_000) / 10_000.0
        };
        for _ in 0..12_000 {
            let (x, y) = (rand() * 612.0, rand() * 792.0);
            let _ = writeln!(
                content,
                "{:.3} {:.3} {:.3} rg {x:.1} {y:.1} m {:.1} {:.1} l {:.1} {:.1} l f",
                rand() * 0.8,
                rand() * 0.8,
                rand() * 0.8,
                x + rand() * 14.0,
                y + rand() * 14.0,
                x - rand() * 14.0,
                y + rand() * 14.0,
            );
        }
        content.push_str("0 0 0 rg BT /F1 9 Tf 11 TL 40 760 Td\n");
        for line in 0..64 {
            let _ = writeln!(
                content,
                "(Page {} line {line}: the quick brown fox jumps over the lazy dog, again and again.) '",
                p + 1
            );
        }
        content.push_str("ET\n");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + p * 2
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{object}\nendobj\n", i + 1);
    }
    let xref = out.len();
    let _ = write!(out, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(out, "{offset:010} 00000 n ");
    }
    let _ = write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    let path = dir.join("heavy.pdf");
    std::fs::write(&path, out).expect("write the heavy fixture");
    path
}

/// Scroll the reader on `path` down at `px_per_frame` for `frames` frames
/// of a 60 Hz display in a `w`×`h` window, as a touchpad's pixel deltas
/// arrive, and time every frame. Rasterising the scene on the GPU is
/// measured too when `gpu` is set.
fn drive_scroll(
    app: PdfApp,
    (w, h): (u32, u32),
    frames: usize,
    px_per_frame: f64,
    gpu: bool,
) -> Vec<FrameSample> {
    let period = Duration::from_micros(16_667);
    let scale: u32 = std::env::var("PDF_SCROLL_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    ui::offscreen::set_capture_device_scale(scale);
    let mut app = Framed::new(
        crate::scenarios::record_state(),
        app,
        viewport((w, h)),
        Theme::dark(),
    );
    // Settle the first screen, as a window does after open.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        Framed::harness_mut(&mut app).relayout();
        if !app.pending_visuals || ui::time::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let centre = app.canvas_rect.center();
    app.last_mouse = centre;
    Framed::harness_mut(&mut app).relayout();
    let mut renderer = gpu.then(|| ui::offscreen::OffscreenRenderer::new(w * scale, h * scale));
    let mut samples = Vec::with_capacity(frames);
    let mut next = ui::time::now();
    for _ in 0..frames {
        next += period;
        let t = ui::time::now();
        let (work, cpu_ms) = FrameWork::count(Framed::harness_mut(&mut app), |harness| {
            harness.send(&winit::event::WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: winit::event::MouseScrollDelta::PixelDelta(
                    winit::dpi::PhysicalPosition::new(0.0, -px_per_frame),
                ),
                phase: winit::event::TouchPhase::Moved,
            });
            harness.relayout();
        });
        let ui_ms = t.elapsed().as_secs_f64() * 1000.0;
        let gpu_ms = renderer.as_mut().map(|renderer| {
            let t = ui::time::now();
            renderer.render_gpu_wait(
                Framed::harness(&app).scene(),
                vello::peniko::Color::from_rgb8(10, 12, 16),
            );
            t.elapsed().as_secs_f64() * 1000.0
        });
        samples.push(FrameSample {
            ui_ms,
            cpu_ms,
            work,
            gpu_ms,
            scroll_y: app.scroll_y(),
            blank_pages: blank_pages(&app),
            unsharp_pages: unsharp_pages(&app),
            pending: app.raster_render_pending.len(),
            held: app.raster_images.len(),
        });
        let now = ui::time::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    samples
}

/// Visible pages the reader holds no engine pixels for.
fn blank_pages(app: &PdfApp) -> usize {
    let canvas = app.canvas_rect;
    let pageh = app.ph() * app.zoom();
    app.page_origins()
        .iter()
        .enumerate()
        .filter(|(_, o)| o.y <= canvas.y1 && o.y + pageh >= canvas.y0)
        .filter(|(i, _)| !app.raster_images.keys().any(|(page, _)| page == i))
        .count()
}

/// Visible pages the reader holds no bitmap at their wanted width for.
fn unsharp_pages(app: &PdfApp) -> usize {
    let canvas = app.canvas_rect;
    let pageh = app.ph() * app.zoom();
    let bucket = crate::source::RasterBackend::bucket_for(
        ((app.pw() * app.zoom() * super::RASTER_SS).round() as u32).clamp(96, 4096),
    );
    app.page_origins()
        .iter()
        .enumerate()
        .filter(|(_, o)| o.y <= canvas.y1 && o.y + pageh >= canvas.y0)
        .filter(|(i, _)| !app.raster_images.keys().any(|key| *key == (*i, bucket)))
        .count()
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let at = ((values.len() as f64 - 1.0) * p).round() as usize;
    values.get(at).copied().unwrap_or(0.0)
}

/// Print a run's numbers: per-frame UI time quantiles and the worst frames
/// with when they happened.
fn report(label: &str, samples: &[FrameSample]) {
    let mut ui: Vec<f64> = samples.iter().map(|s| s.ui_ms).collect();
    let mut cpu: Vec<f64> = samples.iter().map(|s| s.cpu_ms).collect();
    let cpu_line = format!(
        " cpu p50={:.2} p95={:.2} max={:.2}",
        percentile(&mut cpu, 0.5),
        percentile(&mut cpu, 0.95),
        cpu.last().copied().unwrap_or(0.0)
    );
    let over = ui.iter().filter(|ms| **ms > 16.667).count();
    let blank = samples.iter().filter(|s| s.blank_pages > 0).count();
    let unsharp = samples.iter().filter(|s| s.unsharp_pages > 0).count();
    let mut gpu: Vec<f64> = samples.iter().filter_map(|s| s.gpu_ms).collect();
    let (p50, p95, p99) = (
        percentile(&mut ui, 0.5),
        percentile(&mut ui, 0.95),
        percentile(&mut ui, 0.99),
    );
    let max = ui.last().copied().unwrap_or(0.0);
    let gpu_line = if gpu.is_empty() {
        String::new()
    } else {
        format!(
            " gpu p50={:.2} p95={:.2} max={:.2}",
            percentile(&mut gpu, 0.5),
            percentile(&mut gpu, 0.95),
            gpu.last().copied().unwrap_or(0.0)
        )
    };
    eprintln!(
        "SCROLL {label}: frames={} ui p50={p50:.2} p95={p95:.2} p99={p99:.2} max={max:.2} \
         over_16.7ms={over} frames_with_blank_page={blank} frames_not_sharp={unsharp}{cpu_line}{gpu_line} scrolled={:.0}px \
         most work in a frame: {:?}",
        samples.len(),
        samples.last().map_or(0.0, |s| s.scroll_y),
        FrameWork::most(samples.iter().map(|s| s.work)),
    );
    if std::env::var_os("PDF_SCROLL_TRACE").is_some() {
        for (i, s) in samples.iter().enumerate() {
            eprintln!(
                "  frame {i:4} t={:7.1}ms ui={:7.2}ms cpu={:6.2}ms gpu={:?} y={:8.1} blank={} pending={} held={} {:?}",
                i as f64 * 16.667,
                s.ui_ms,
                s.cpu_ms,
                s.gpu_ms,
                s.scroll_y,
                s.blank_pages,
                s.pending,
                s.held,
                s.work
            );
        }
    }
}

/// Measurement harness: `PDF_SCROLL_FILE` (a real PDF) or the generated
/// heavy fixture, `PDF_SCROLL_GPU=1` to rasterise every frame as well.
#[test]
fn measure_sustained_scroll() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = std::env::var_os("PDF_SCROLL_FILE")
        .map_or_else(|| heavy_pdf(dir.path(), 24), std::path::PathBuf::from);
    let Some(app) = open_settled(&path) else {
        return;
    };
    let gpu = std::env::var("PDF_SCROLL_GPU").as_deref() == Ok("1");
    let (w, h) = std::env::var("PDF_SCROLL_SIZE")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280, 800));
    let speed: f64 = std::env::var("PDF_SCROLL_SPEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24.0);
    let frames: usize = std::env::var("PDF_SCROLL_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let samples = drive_scroll(app, (w, h), frames, speed, gpu);
    report(&path.display().to_string(), &samples);
}

/// Text runs a scroll frame may shape: most shape none, and a measured run
/// shaped at most three in one frame.
const SCROLL_MAX_TEXT_SHAPED: u64 = 4;

/// Draw objects a scroll frame may encode: the chrome and the pages on
/// screen come to at most 168; far more means something draws what is not
/// on screen.
const SCROLL_MAX_DRAWS: usize = 256;

#[serial_test::serial(pdfium)]
#[test]
fn sustained_scroll_never_waits_on_rendering() {
    // Five seconds of touchpad scrolling through a document whose pages take
    // the engine far longer than a frame to rasterise. Every frame must move
    // the document without doing more on the UI thread than a scroll needs:
    // no call into the engine (rasterising happens on the worker, and the
    // window composites what it holds), no layout rebuilt, and bounded
    // measuring, shaping and drawing. The work is counted rather than timed,
    // so a loaded machine cannot fail it and a regression fails it anywhere.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = heavy_pdf(dir.path(), 24);
    let render_ms = {
        let mut raster =
            crate::source::open(&path, &crate::app::default_content()).map(|loaded| loaded.raster);
        let t = ui::time::now();
        let rendered = raster
            .as_mut()
            .ok()
            .and_then(|raster| raster.render(1, 1440));
        (rendered.is_some()).then(|| t.elapsed().as_secs_f64() * 1000.0)
    };
    let Some(app) = open_settled(&path) else {
        return;
    };
    let frames = 300;
    let samples = drive_scroll(app, (1280, 800), frames, 24.0, false);
    report("sustained scroll", &samples);
    eprintln!("SCROLL one heavy page renders in {render_ms:?} ms on the worker");

    for (i, sample) in samples.iter().enumerate() {
        let work = sample.work;
        assert!(
            work.engine_calls == 0
                && work.rebuilt_layouts == 0
                && work.measured_leaves <= MAX_MEASURED_LEAVES
                && work.text_shaped <= SCROLL_MAX_TEXT_SHAPED
                && work.draws <= SCROLL_MAX_DRAWS,
            "frame {i} did {work:?} on the UI thread, more than a scroll frame needs"
        );
    }
    for (i, pair) in samples.windows(2).enumerate() {
        assert!(
            pair[1].scroll_y > pair[0].scroll_y,
            "the document stood still at frame {}: {:.1} -> {:.1}",
            i + 1,
            pair[0].scroll_y,
            pair[1].scroll_y
        );
    }
    // Rendering did happen while the frames ran: pages far below the first
    // screen were delivered during the scroll.
    let delivered = samples.last().map_or(0, |s| s.held);
    assert!(
        delivered >= 3,
        "the worker delivered pages as the scroll reached them"
    );
}

/// A window surface of a fixed size, as the live host gives the app one.
struct Window(u32, u32);

impl ui::app::AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(self.0, self.1)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

/// Pixels of `size` that `rects` cover.
fn covered(rects: &[vello::kurbo::Rect], (w, h): (u32, u32)) -> f64 {
    let window = vello::kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    let mut rows = vec![vec![false; w as usize]; h as usize];
    for rect in rects {
        let rect = rect.intersect(window).expand();
        let (x0, x1) = (rect.x0 as usize, rect.x1 as usize);
        for row in rows
            .iter_mut()
            .take(rect.y1 as usize)
            .skip(rect.y0 as usize)
        {
            if let Some(cells) = row.get_mut(x0..x1) {
                cells.fill(true);
            }
        }
    }
    rows.iter().flatten().filter(|cell| **cell).count() as f64
}

/// What one scroll frame costs the renderer through the live window's
/// frame and damage path (the host copies what stays on screen when the
/// app offers it): pixels repainted, and with `PDF_SCROLL_GPU=1` the GPU
/// time of the presented frame against a full repaint. Measurement only;
/// `PDF_SCROLL_FILE` as above.
#[test]
fn measure_scroll_repaint() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = std::env::var_os("PDF_SCROLL_FILE")
        .map_or_else(|| heavy_pdf(dir.path(), 24), std::path::PathBuf::from);
    let painter = if std::env::var("PDF_SCROLL_GPU").as_deref() == Ok("1") {
        Painter::Gpu
    } else {
        Painter::None
    };
    let _ = scroll_repaint(&path, 240, painter);
}

/// A scroll frame repaints the strip that scrolled in plus what changed —
/// the host copies the rest — and the copied frames are pixel-identical to
/// a full repaint, so nothing floating over the pages (the scroll bar) is
/// smeared along with them.
#[test]
#[serial_test::serial(pdfium)]
fn scrolling_repaints_only_the_exposed_strip_and_matches_a_full_repaint() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = heavy_pdf(dir.path(), 12);
    let Some(stats) = scroll_repaint(&path, 120, Painter::Cpu) else {
        return;
    };
    assert_eq!(
        stats.whole_window_frames, 0,
        "no scroll frame repaints the whole window"
    );
    assert!(
        stats.p50_fraction < 0.10,
        "a typical scroll frame repaints the exposed strip, not the canvas: {:.1}% of the window",
        stats.p50_fraction * 100.0
    );
    assert_eq!(
        stats.differing,
        Some(0),
        "copied scroll frames equal a full repaint"
    );
}

struct RepaintStats {
    whole_window_frames: usize,
    p50_fraction: f64,
    differing: Option<usize>,
}

/// What presents the frames of a driven scroll.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Painter {
    /// Nobody: count the damage only.
    None,
    /// The GPU, timed against a full repaint.
    Gpu,
    /// The CPU renderer, as the desktop composits without a GPU: the same
    /// pixels on any machine.
    Cpu,
}

/// A window's retained pixels as the CPU compositor keeps them: copies
/// move what stays on screen, damage is rendered again.
struct CpuSurface {
    raster: tilo_cpu_raster::CpuRasterizer,
    pixels: Vec<u8>,
    size: (u32, u32),
}

impl CpuSurface {
    fn new((w, h): (u32, u32)) -> Self {
        Self {
            raster: tilo_cpu_raster::CpuRasterizer::default(),
            pixels: vec![0; w as usize * h as usize * 4],
            size: (w, h),
        }
    }

    fn present(
        &mut self,
        scene: &vello::Scene,
        update: Option<(&[ui::scroll_copy::DeviceCopy], &[vello::kurbo::Rect])>,
    ) {
        let (width, height) = self.size;
        let mut target =
            tilo_cpu_raster::Target::new(&mut self.pixels, width, height, width as usize * 4)
                .expect("a window-sized target");
        let ground = tilo_cpu_raster::color::AlphaColor::from_rgba8(10, 12, 16, 255);
        let Some((copies, damage)) = update else {
            self.raster
                .render(scene.encoding(), &mut target, None, ground);
            return;
        };
        for copy in copies {
            // A copy names its source; the target moves pixels into a
            // destination.
            let Some((x, y)) = copy.destination(width, height) else {
                continue;
            };
            tilo_cpu_raster::move_pixels(
                &mut target,
                &[tilo_cpu_raster::Region::new(x, y, copy.width, copy.height)],
                copy.dx,
                copy.dy,
            );
        }
        let regions: Vec<tilo_cpu_raster::Region> = damage
            .iter()
            .map(|rect| rect.expand())
            .filter(|rect| rect.x1 > 0.0 && rect.y1 > 0.0)
            .map(|rect| {
                let (x0, y0) = (rect.x0.max(0.0) as u32, rect.y0.max(0.0) as u32);
                tilo_cpu_raster::Region::new(
                    x0,
                    y0,
                    (rect.x1 as u32).saturating_sub(x0),
                    (rect.y1 as u32).saturating_sub(y0),
                )
            })
            .collect();
        if !regions.is_empty() {
            self.raster
                .render(scene.encoding(), &mut target, Some(&regions), ground);
        }
    }
}

/// The reader on `path` in a live window of `surface`'s size, its first
/// screen settled, presenting through a copying host: the window, its
/// damage tracker and the last frame's scene.
fn settled_window(
    path: &std::path::Path,
    surface: &Window,
) -> Option<(ui::App, ui::app::SurfaceDamage, vello::Scene)> {
    let reader = open_settled(path)?;
    let mut app = ui::App::new(crate::scenarios::record_state(), Box::new(reader));
    let mut damage = ui::app::SurfaceDamage::copying();
    let mut scene = vello::Scene::new();
    app.init_layout();
    app.surface_input(
        surface,
        winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(640.0, 450.0),
        },
        false,
    );
    for _ in 0..60 {
        scene.reset();
        app.surface_frame_with_damage(surface, &mut scene, &mut damage);
        std::thread::sleep(Duration::from_millis(16));
    }
    let _ = damage.take();
    Some((app, damage, scene))
}

/// One touchpad wheel step of `px` down the document in a live window.
fn wheel_down(app: &mut ui::App, surface: &Window, px: f64) {
    app.surface_input(
        surface,
        winit::event::WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                0.0, -px,
            )),
            phase: winit::event::TouchPhase::Moved,
        },
        false,
    );
}

fn scroll_repaint(path: &std::path::Path, frames: usize, painter: Painter) -> Option<RepaintStats> {
    let size = (1280, 800);
    let surface = Window(size.0, size.1);
    let (mut app, mut damage, mut scene) = settled_window(path, &surface)?;
    let mut cpu = (painter == Painter::Cpu).then(|| CpuSurface::new(size));
    if let Some(cpu) = cpu.as_mut() {
        cpu.present(&scene, None);
    }
    let mut renderers = (painter == Painter::Gpu).then(|| {
        (
            ui::offscreen::OffscreenRenderer::new(size.0, size.1),
            ui::offscreen::OffscreenRenderer::new(size.0, size.1),
        )
    });
    let ground = vello::peniko::Color::from_rgb8(10, 12, 16);
    if let Some((presented, _)) = renderers.as_mut() {
        let _ = presented.render(&scene, ground);
    }
    let whole = f64::from(size.0) * f64::from(size.1);
    let (mut pixels, mut presented_ms, mut full_ms) = (Vec::new(), Vec::new(), Vec::new());
    let mut whole_frames = 0;
    for _ in 0..frames {
        wheel_down(&mut app, &surface, 24.0);
        scene.reset();
        app.surface_frame_with_damage(&surface, &mut scene, &mut damage);
        let frame = damage.take_copying();
        pixels.push(frame.as_ref().map_or_else(
            || {
                whole_frames += 1;
                whole
            },
            |(_, rects)| covered(rects, size),
        ));
        if let Some(cpu) = cpu.as_mut() {
            cpu.present(
                &scene,
                frame
                    .as_ref()
                    .map(|(moves, rects)| (moves.as_slice(), rects.as_slice())),
            );
        }
        if let Some((presented, full)) = renderers.as_mut() {
            let t = ui::time::now();
            match &frame {
                Some((moves, rects)) => {
                    presented.render_copies_and_damage_gpu_wait(&scene, ground, moves, rects);
                }
                None => presented.render_gpu_wait(&scene, ground),
            }
            presented_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = ui::time::now();
            full.render_gpu_wait(&scene, ground);
            full_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    let presented_and_full = match (renderers.as_mut(), cpu) {
        (Some((presented, full)), _) => {
            Some((presented.readback_current(), full.render(&scene, ground)))
        }
        (None, Some(mut presented)) => {
            // The same renderer draws the whole frame afresh, so only what
            // the copies and damage left behind can differ.
            let actual = presented.pixels.clone();
            presented.present(&scene, None);
            Some((actual, presented.pixels))
        }
        (None, None) => None,
    };
    let differing = presented_and_full.map(|(actual, expected)| {
        if let Some(dir) = std::env::var_os("PDF_SCROLL_DUMP") {
            dump_diff(&std::path::PathBuf::from(dir), size, &actual, &expected);
        }
        pixels_differing(&actual, &expected)
    });
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let p50_pixels = percentile(&mut pixels.clone(), 0.5);
    eprintln!(
        "REPAINT {}: frames={} whole_window_frames={whole_frames} repainted_px p50={p50_pixels:.0} \
         mean={:.0} ({:.1}% of window) gpu_presented p50={:.2} mean={:.2} gpu_full p50={:.2} \
         differing={differing:?}",
        path.display(),
        pixels.len(),
        mean(&pixels),
        100.0 * mean(&pixels) / whole,
        percentile(&mut presented_ms.clone(), 0.5),
        mean(&presented_ms),
        percentile(&mut full_ms.clone(), 0.5),
    );
    Some(RepaintStats {
        whole_window_frames: whole_frames,
        p50_fraction: p50_pixels / whole,
        differing,
    })
}

/// Pixels of two equally sized frames that differ by more than
/// resampling does: the CPU renderer samples a scaled bitmap relative to
/// the region it draws, so the same pixel drawn as part of another region
/// can land up to 2 levels away. Anything left behind by a copy (a shadow,
/// a thumb, another page) is far beyond that.
fn pixels_differing(actual: &[u8], expected: &[u8]) -> usize {
    actual
        .chunks_exact(4)
        .zip(expected.chunks_exact(4))
        .filter(|(a, e)| a.iter().zip(e.iter()).any(|(a, e)| a.abs_diff(*e) > 4))
        .count()
}

/// Write `actual.png` and `expected.png` into `dir` and print where they
/// differ: the bounding box and the count per 50-row band.
fn dump_diff(dir: &std::path::Path, size: (u32, u32), actual: &[u8], expected: &[u8]) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let mut rows = std::collections::BTreeMap::new();
    for (i, (a, e)) in actual.chunks(4).zip(expected.chunks(4)).enumerate() {
        if a.iter().zip(e).any(|(a, e)| a.abs_diff(*e) > 4) {
            let (x, y) = (i as u32 % size.0, i as u32 / size.0);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            *rows.entry(y / 50 * 50).or_insert(0) += 1;
        }
    }
    eprintln!("DIFF bbox=({x0},{y0})-({x1},{y1}) rows={rows:?}");
    for (name, pixels) in [("expected.png", expected), ("actual.png", actual)] {
        let _ = image::RgbaImage::from_raw(size.0, size.1, pixels.to_vec())
            .map(|img| img.save(dir.join(name)));
    }
}

/// One step of a driven zoom.
#[derive(Clone, Copy, Debug)]
enum ZoomStep {
    /// A touchpad pinch frame (`PinchGesture` delta).
    Pinch(f64),
    /// A Ctrl+wheel frame (touchpad pixel delta, upward positive).
    CtrlWheel(f64),
    /// A Ctrl+= or Ctrl+- press, then frames with no input.
    Key(&'static str),
    Idle,
}

/// The work one frame did on the UI thread, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FrameWork {
    /// Calls into the PDF engine: rasterising a page or tile, reading a
    /// page's text or links. Each can take tens of milliseconds on a heavy
    /// page, which is why they belong on the worker.
    engine_calls: u64,
    /// Layouts of the window's tree rebuilt from scratch.
    rebuilt_layouts: u64,
    /// Layout leaves measured again.
    measured_leaves: usize,
    /// Text runs shaped (shape-cache misses).
    text_shaped: u64,
    /// Draw objects in the frame's scene.
    draws: usize,
}

impl FrameWork {
    /// Run `frame` on `harness`, and count what it did on this thread;
    /// with its CPU time in milliseconds, for reports.
    fn count(
        harness: &mut ui::headless::FrameHarness,
        frame: impl FnOnce(&mut ui::headless::FrameHarness),
    ) -> (Self, f64) {
        let _ = harness.take_text_shape_stats();
        let engine = crate::source::engine_calls_on_this_thread();
        let rebuilt = harness.rebuilt_frames();
        let cpu = ui::rt::thread_cpu_time();
        let t = ui::time::now();
        frame(harness);
        let cpu_ms = match (cpu, ui::rt::thread_cpu_time()) {
            (Some(before), Some(after)) => after.saturating_sub(before).as_secs_f64() * 1000.0,
            _ => t.elapsed().as_secs_f64() * 1000.0,
        };
        let (_, shaped, _) = harness.take_text_shape_stats();
        let work = Self {
            engine_calls: crate::source::engine_calls_on_this_thread() - engine,
            rebuilt_layouts: harness.rebuilt_frames() - rebuilt,
            measured_leaves: harness.last_relayout().marked_leaves,
            text_shaped: shaped,
            draws: harness.scene().encoding().draw_tags.len(),
        };
        (work, cpu_ms)
    }

    /// The most of each kind of work any one of `frames` did.
    fn most(frames: impl Iterator<Item = Self>) -> Self {
        frames.fold(Self::default(), |most, work| Self {
            engine_calls: most.engine_calls.max(work.engine_calls),
            rebuilt_layouts: most.rebuilt_layouts.max(work.rebuilt_layouts),
            measured_leaves: most.measured_leaves.max(work.measured_leaves),
            text_shaped: most.text_shaped.max(work.text_shaped),
            draws: most.draws.max(work.draws),
        })
    }
}

/// One frame of a driven zoom.
struct ZoomSample {
    /// The frame's CPU time on the UI thread, reported for measurements;
    /// it varies with the machine and its load, so no test asserts on it.
    cpu_ms: f64,
    /// What the frame did on the UI thread, counted: these are the same on
    /// any machine under any load.
    work: FrameWork,
    zoom: f64,
    /// The page's width bucket and whether it is drawn from tiles.
    bucket: u32,
    tiled: bool,
    /// Pixels inside visible pages showing bare paper (nothing the engine
    /// drew), and how many were sampled.
    paper_px: usize,
    sampled_px: usize,
}

/// The zoom script: in and back out across the whole-page widths and the
/// tile threshold, by pinch, Ctrl+wheel and the keyboard.
fn zoom_script(fit: f64, top: f64) -> Vec<ZoomStep> {
    let mut steps = Vec::new();
    let frames = |factor: f64| ((top / fit).ln() / factor.ln()).ceil() as usize;
    steps.extend(std::iter::repeat_n(ZoomStep::Pinch(0.02), frames(1.02)));
    steps.extend(std::iter::repeat_n(
        ZoomStep::Pinch(-0.02),
        frames(1.0 / 0.98),
    ));
    steps.extend(std::iter::repeat_n(ZoomStep::CtrlWheel(12.0), frames(1.05)));
    steps.extend(std::iter::repeat_n(
        ZoomStep::CtrlWheel(-12.0),
        frames(1.05),
    ));
    for key in ["=", "-"] {
        for _ in 0..frames(1.15) {
            steps.push(ZoomStep::Key(key));
            steps.extend([ZoomStep::Idle; 3]);
        }
    }
    steps
}

/// Drive `script` through the reader on `path` in a `w`×`h` window at 60
/// Hz, rasterise every frame and look at every visible page's pixels. `None` without the PDF engine.
fn drive_zoom(path: &std::path::Path, (w, h): (u32, u32), top: f64) -> Option<Vec<ZoomSample>> {
    let app = open_settled(path)?;
    ui::offscreen::set_capture_device_scale(1);
    let mut app = Framed::new(
        crate::scenarios::record_state(),
        app,
        viewport((w, h)),
        Theme::dark(),
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        Framed::harness_mut(&mut app).relayout();
        if !app.pending_visuals || ui::time::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let focal = app.canvas_rect.center();
    app.last_mouse = focal;
    Framed::harness_mut(&mut app).send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(focal.x, focal.y),
    });
    Framed::harness_mut(&mut app).relayout();
    let script = zoom_script(app.zoom(), top);
    // A native window draws on its frame clock: input between two frames
    // is dispatched as it arrives and drawn once, in the next.
    Framed::harness_mut(&mut app).set_coalesce_input_frames(true);
    // The CPU renderer the desktop draws with where it has no GPU: the
    // same pixels, on any machine, pinned to any CPU.
    let mut raster = tilo_cpu_raster::CpuRasterizer::default();
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    let ground = tilo_cpu_raster::color::AlphaColor::from_rgba8(10, 12, 16, 255);
    let probes = probe_grid(w, h);
    let period = Duration::from_micros(16_667);
    let mut samples = Vec::with_capacity(script.len());
    // Whole frames, as shown, where `PDF_ZOOM_SHOTS` names.
    let shots = std::env::var_os("PDF_ZOOM_SHOTS").map(std::path::PathBuf::from);
    let mut shot_tiled = false;
    let mut next = ui::time::now();
    let mut ctrl_held = false;
    for step in script {
        next += period;
        // Ctrl goes down before the Ctrl+wheel and keyboard steps and stays
        // down through them, as a hand holds it.
        let ctrl = matches!(step, ZoomStep::CtrlWheel(_) | ZoomStep::Key(_));
        if ctrl != ctrl_held {
            let held: &[String] = if ctrl { &["ctrl".to_owned()] } else { &[] };
            Framed::harness_mut(&mut app).set_modifiers(held);
            ctrl_held = ctrl;
        }
        let frame = samples.len();
        // The step's input is dispatched as it arrives and the window draws
        // once, on its frame clock.
        let (work, cpu_ms) = FrameWork::count(Framed::harness_mut(&mut app), |harness| {
            send_zoom_step(harness, step);
            if let Err(error) = harness.poll_frame() {
                panic!("frame {frame}: {error:?}");
            }
        });
        {
            let mut target = tilo_cpu_raster::Target::new(&mut pixels, w, h, w as usize * 4)
                .expect("a frame-sized target");
            raster.render(
                Framed::harness(&app).scene().encoding(),
                &mut target,
                Some(&probes),
                ground,
            );
        }
        let (paper_px, sampled_px) = paper_showing(&app, &pixels, w, &probes);
        let card_w = app.pw() * app.zoom();
        if let Some(dir) = shots.as_ref() {
            let tiled = card_w > crate::tiles::TILE_MIN_PAGE_PX;
            if samples.len() % 24 == 0 || tiled != shot_tiled {
                shot_tiled = tiled;
                shoot_frame(
                    &app,
                    (w, h),
                    &dir.join(format!(
                        "zoom-{:03}-{:.0}pct{}.png",
                        samples.len(),
                        app.zoom() * 100.0,
                        if tiled { "-tiled" } else { "" }
                    )),
                );
            }
        }
        samples.push(ZoomSample {
            cpu_ms,
            work,
            zoom: app.zoom(),
            bucket: crate::source::RasterBackend::bucket_for(
                ((card_w * super::RASTER_SS).round() as u32).clamp(96, 4096),
            ),
            tiled: card_w > crate::tiles::TILE_MIN_PAGE_PX,
            paper_px,
            sampled_px,
        });
        let now = ui::time::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    Some(samples)
}

/// Write the whole frame `app` shows, rendered on the CPU, to `path`.
fn shoot_frame(app: &Framed<PdfApp>, (w, h): (u32, u32), path: &std::path::Path) {
    use image::ImageEncoder as _;
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    {
        let mut target = tilo_cpu_raster::Target::new(&mut pixels, w, h, w as usize * 4)
            .expect("a frame-sized target");
        tilo_cpu_raster::CpuRasterizer::default().render(
            Framed::harness(app).scene().encoding(),
            &mut target,
            None,
            tilo_cpu_raster::color::AlphaColor::from_rgba8(10, 12, 16, 255),
        );
    }
    let file = std::fs::File::create(path).expect("create the shot");
    image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(&pixels, w, h, image::ExtendedColorType::Rgba8)
        .expect("write the shot");
}

/// Deliver one zoom step's input, as the window would.
fn send_zoom_step(harness: &mut ui::headless::FrameHarness, step: ZoomStep) {
    match step {
        ZoomStep::Pinch(delta) => harness.send(&winit::event::WindowEvent::PinchGesture {
            device_id: winit::event::DeviceId::dummy(),
            delta,
            phase: winit::event::TouchPhase::Moved,
        }),
        ZoomStep::CtrlWheel(dy) => harness.send(&winit::event::WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                0.0, dy,
            )),
            phase: winit::event::TouchPhase::Moved,
        }),
        ZoomStep::Key(key) => harness.key(key),
        ZoomStep::Idle => {}
    }
}

/// Small probes on a grid across a `w`×`h` window, rasterised every frame:
/// enough to see a page, or a tile of one, standing blank, cheap enough
/// that the frames keep close to a display's pace.
fn probe_grid(w: u32, h: u32) -> Vec<tilo_cpu_raster::Region> {
    (0..h / PROBE_PITCH)
        .flat_map(|row| {
            (0..w / PROBE_PITCH).map(move |col| {
                tilo_cpu_raster::Region::new(
                    col * PROBE_PITCH + PROBE_PITCH / 2,
                    row * PROBE_PITCH + PROBE_PITCH / 2,
                    PROBE,
                    PROBE,
                )
            })
        })
        .collect()
}

/// Side and spacing of the zoom test's pixel probes.
const PROBE: u32 = 12;
const PROBE_PITCH: u32 = 64;

/// Pixels of the visible pages, in the rasterised `probes`, that show bare
/// paper: the reader draws a page's paper first and the engine's pixels
/// over it, and the fixture's own ink is never that light. Excludes a
/// margin round each card edge and the scroll bars, which float over the
/// pages. Returns (paper, sampled).
fn paper_showing(
    app: &PdfApp,
    pixels: &[u8],
    width: u32,
    probes: &[tilo_cpu_raster::Region],
) -> (usize, usize) {
    let canvas = app.canvas_rect;
    let lane = 16.0;
    let view = vello::kurbo::Rect::new(canvas.x0, canvas.y0, canvas.x1 - lane, canvas.y1 - lane);
    let size = vello::kurbo::Size::new(app.pw(), app.ph()) * app.zoom();
    let (mut paper, mut sampled) = (0, 0);
    let cards: Vec<vello::kurbo::Rect> = app
        .page_origins()
        .into_iter()
        .map(|origin| {
            vello::kurbo::Rect::from_origin_size(origin, size)
                .inflate(-4.0, -4.0)
                .intersect(view)
        })
        .filter(|card| card.width() > 0.0 && card.height() > 0.0)
        .collect();
    for probe in probes {
        for y in probe.y..probe.y + probe.height {
            for x in probe.x..probe.x + probe.width {
                let at = vello::kurbo::Point::new(f64::from(x), f64::from(y));
                if !cards.iter().any(|card| card.contains(at)) {
                    continue;
                }
                let i = (y as usize * width as usize + x as usize) * 4;
                if let Some(px) = pixels.get(i..i + 3) {
                    sampled += 1;
                    if px.iter().all(|c| *c >= 250) {
                        paper += 1;
                    }
                }
            }
        }
    }
    (paper, sampled)
}

/// Zooming scales the page bitmaps already held on the GPU every frame and
/// swaps in the sharp render when it arrives, so across every width bucket
/// and the switch to tiles and back, by pinch, Ctrl+wheel and the keyboard,
/// no frame shows a page (or part of one) as bare paper, and no frame does
/// more on the UI thread than a zoom needs: no engine call, no layout
/// rebuilt for the zoom, and bounded measuring, shaping and drawing. The
/// work is counted rather than timed, so a loaded machine cannot fail it
/// and a regression fails it on any machine.
#[test]
#[serial_test::serial(pdfium)]
fn zooming_across_every_breakpoint_never_shows_a_blank_page() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = heavy_pdf(dir.path(), 6);
    // Past the tile threshold at device scale 1 (page width / 612 pt).
    let top = crate::tiles::TILE_MIN_PAGE_PX / 612.0 * 1.6;
    let Some(samples) = drive_zoom(&path, (1280, 800), top) else {
        return;
    };
    let report = zoom_report(&samples);
    eprintln!("{report}");
    let buckets: std::collections::BTreeSet<u32> = samples.iter().map(|s| s.bucket).collect();
    assert!(
        buckets.len() > 20,
        "the zoom crosses many width buckets: {buckets:?}"
    );
    assert!(
        samples
            .windows(2)
            .filter(|w| w[0].tiled != w[1].tiled)
            .count()
            >= 6,
        "the zoom crosses the tile threshold in and out, by pinch, wheel and keyboard"
    );
    for (i, s) in samples.iter().enumerate() {
        assert!(s.sampled_px > 0, "frame {i} looked at the pages");
        assert_eq!(
            s.paper_px, 0,
            "frame {i} at zoom {:.3} (bucket {}, tiled {}) shows {} px of bare paper: {report}",
            s.zoom, s.bucket, s.tiled, s.paper_px
        );
        let work = s.work;
        assert!(
            work.engine_calls == 0
                && work.measured_leaves <= MAX_MEASURED_LEAVES
                && work.text_shaped <= MAX_TEXT_SHAPED
                && work.draws <= MAX_DRAWS,
            "frame {i} did {work:?} on the UI thread, more than a zoom frame needs: {report}"
        );
    }
    // The first key press switches the window from pointer to keyboard
    // modality, which lays the tree out anew once; zooming itself never does.
    let rebuilt: u64 = samples.iter().map(|s| s.work.rebuilt_layouts).sum();
    assert!(
        rebuilt <= 1,
        "the zoom rebuilt the window's layout {rebuilt} times: {report}"
    );
}

/// Layout leaves a zoom or scroll frame may measure again: each measures one.
const MAX_MEASURED_LEAVES: usize = 2;

/// Text runs a zoom frame may shape: a zoom step shapes one, the new
/// percentage the page pill shows.
const MAX_TEXT_SHAPED: u64 = 2;

/// Draw objects a zoom frame may encode: the chrome, the visible pages and
/// their tiles at two zoom levels while the new level arrives come to about
/// 250; twice that means something draws far more than is on screen.
const MAX_DRAWS: usize = 512;

/// Measurement: the same zoom on `PDF_SCROLL_FILE` or the heavy fixture,
/// when `PDF_SCROLL_ZOOM` is set.
#[test]
fn measure_zoom() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = std::env::var_os("PDF_SCROLL_FILE")
        .map_or_else(|| heavy_pdf(dir.path(), 6), std::path::PathBuf::from);
    let top = crate::tiles::TILE_MIN_PAGE_PX / 612.0 * 1.6;
    if std::env::var_os("PDF_SCROLL_ZOOM").is_none() {
        return;
    }
    if let Some(samples) = drive_zoom(&path, (1280, 800), top) {
        eprintln!("{}", zoom_report(&samples));
    }
}

fn zoom_report(samples: &[ZoomSample]) -> String {
    let mut cpu: Vec<f64> = samples.iter().map(|s| s.cpu_ms).collect();
    let blank = samples.iter().filter(|s| s.paper_px > 0).count();
    let worst = samples
        .iter()
        .map(|s| s.paper_px as f64 / s.sampled_px.max(1) as f64)
        .fold(0.0, f64::max);
    let most = FrameWork::most(samples.iter().map(|s| s.work));
    let line = format!(
        "ZOOM frames={} frames_with_paper={blank} worst_paper={:.1}% cpu p50={:.2} p95={:.2} max={:.2} most work in a frame: {most:?}",
        samples.len(),
        worst * 100.0,
        percentile(&mut cpu, 0.5),
        percentile(&mut cpu, 0.95),
        cpu.last().copied().unwrap_or(0.0),
    );
    if std::env::var_os("PDF_SCROLL_TRACE").is_none() {
        return line;
    }
    samples.iter().enumerate().fold(line, |mut out, (i, s)| {
        use std::fmt::Write as _;
        let _ = write!(
            out,
            "\n  frame {i:4} zoom={:.3} bucket={} tiled={} paper={}/{} cpu={:.2} {:?}",
            s.zoom, s.bucket, s.tiled, s.paper_px, s.sampled_px, s.cpu_ms, s.work
        );
        out
    })
}

/// How a person scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollInput {
    /// A mouse wheel spun steadily: a notch every fourth frame.
    Wheel,
    /// Two fingers on a touchpad flicking: ten frames of fast pixel deltas,
    /// then the fingers lift and nothing comes for fifty.
    Fling,
    /// A finger on the screen dragging the page up, lifting, and again.
    TouchDrag,
}

/// The input of frame `frame` of `input`, as the window gets it.
fn send_scroll_input(
    app: &mut ui::App,
    surface: &Window,
    fingers: &mut ui::interaction::TouchPointerEmulator,
    input: ScrollInput,
    frame: usize,
    at: vello::kurbo::Point,
) {
    use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};
    let mut send = |event| {
        app.surface_input(surface, event, false);
    };
    let wheel = |delta, phase| WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta,
        phase,
    };
    let pixel = |dy| MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy));
    match input {
        ScrollInput::Wheel => {
            if frame.is_multiple_of(4) {
                send(wheel(
                    MouseScrollDelta::LineDelta(0.0, -1.0),
                    TouchPhase::Moved,
                ));
            }
        }
        ScrollInput::Fling => match frame % 60 {
            0 => send(wheel(pixel(-36.0), TouchPhase::Started)),
            1..=9 => send(wheel(pixel(-36.0), TouchPhase::Moved)),
            10 => send(wheel(pixel(0.0), TouchPhase::Ended)),
            _ => {}
        },
        ScrollInput::TouchDrag => {
            let step = frame % 60;
            let y = at.y + 200.0 - 20.0 * step.min(20) as f64;
            match step {
                0 => finger(app, surface, fingers, TouchPhase::Started, (at.x, y)),
                1..=19 => finger(app, surface, fingers, TouchPhase::Moved, (at.x, y)),
                20 => finger(app, surface, fingers, TouchPhase::Ended, (at.x, y)),
                _ => {}
            }
        }
    }
}

/// One finger's contact update at `at`, turned into pointer events as the
/// live window's runner turns it.
fn finger(
    app: &mut ui::App,
    surface: &Window,
    fingers: &mut ui::interaction::TouchPointerEmulator,
    phase: winit::event::TouchPhase,
    at: (f64, f64),
) {
    let contact = winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(at.0, at.1),
        force: None,
        id: 0,
    };
    let events = fingers.events(&contact, |_| false);
    for event in events {
        app.surface_input(
            surface,
            event.window_event(winit::event::DeviceId::dummy()),
            false,
        );
    }
}

/// One frame of a driven scroll by some input: its UI time, the GPU time
/// of the frame as the window presents it (copying what stays on screen,
/// rendering the damage), and how far it moved the document (what the
/// host copied up; 0 for a frame repainted whole).
struct InputFrame {
    ui_ms: f64,
    gpu_ms: f64,
    moved_px: f64,
    /// Whether the frame drew anything (an idle frame between wheel
    /// notches presents nothing).
    drew: bool,
    /// Whether it repainted the whole window.
    whole: bool,
    /// The area it repainted, in pixels.
    damage_px: f64,
}

/// Scroll the reader on `path` for `frames` 60 Hz frames by `input` in a
/// live 1280×800 window presenting through a copying host on the GPU.
fn drive_input(
    path: &std::path::Path,
    input: ScrollInput,
    frames: usize,
    painter: Painter,
) -> Option<Vec<InputFrame>> {
    let size = (1280, 800);
    let surface = Window(size.0, size.1);
    let (mut app, mut damage, mut scene) = settled_window(path, &surface)?;
    let at = vello::kurbo::Point::new(640.0, 450.0);
    let ground = vello::peniko::Color::from_rgb8(10, 12, 16);
    let mut cpu = (painter == Painter::Cpu).then(|| CpuSurface::new(size));
    let mut presented =
        (painter == Painter::Gpu).then(|| ui::offscreen::OffscreenRenderer::new(size.0, size.1));
    if let Some(presented) = presented.as_mut() {
        presented.render_gpu_wait(&scene, ground);
    }
    if let Some(cpu) = cpu.as_mut() {
        cpu.present(&scene, None);
    }
    let period = Duration::from_micros(16_667);
    let mut out = Vec::with_capacity(frames);
    let mut fingers = ui::interaction::TouchPointerEmulator::default();
    let mut next = ui::time::now();
    // Each frame's input and work see the 60 Hz clock a host's frames keep,
    // so a loaded machine that oversleeps a frame cannot age the finger's
    // last move out of the release velocity's window.
    let mut frame_clock = next;
    for frame in 0..frames {
        next += period;
        frame_clock += period;
        let t = ui::time::now();
        ui::time::testing::with_monotonic_time(frame_clock, || {
            send_scroll_input(&mut app, &surface, &mut fingers, input, frame, at);
            scene.reset();
            app.surface_frame_with_damage(&surface, &mut scene, &mut damage);
        });
        let ui_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = ui::time::now();
        let frame = damage.take_copying();
        if let Some(presented) = presented.as_mut() {
            match &frame {
                Some((moves, rects)) => {
                    presented.render_copies_and_damage_gpu_wait(&scene, ground, moves, rects);
                }
                None => presented.render_gpu_wait(&scene, ground),
            }
        }
        if let Some(cpu) = cpu.as_mut() {
            cpu.present(
                &scene,
                frame
                    .as_ref()
                    .map(|(moves, rects)| (moves.as_slice(), rects.as_slice())),
            );
        }
        let (moved_px, drew) = frame.as_ref().map_or((0.0, true), |(moves, rects)| {
            let moved = moves
                .first()
                .map_or(0.0, |copy| f64::from(copy.dy.unsigned_abs()));
            (moved, !moves.is_empty() || !rects.is_empty())
        });
        // Repainted whole: no damage list, or damage over most of the
        // window (the canvas without the chrome around it).
        let window = f64::from(size.0) * f64::from(size.1);
        let damage_px = frame.as_ref().map_or(window, |(_, rects)| {
            rects.iter().map(vello::kurbo::Rect::area).sum::<f64>()
        });
        let whole = damage_px >= 0.6 * window;
        let gpu_ms = t.elapsed().as_secs_f64() * 1000.0;
        out.push(InputFrame {
            ui_ms,
            gpu_ms,
            moved_px,
            drew,
            whole,
            damage_px,
        });
        let now = ui::time::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    Some(out)
}

/// p50 / p95 / max of `values`, in a line.
fn quantiles(mut values: Vec<f64>) -> String {
    let p50 = percentile(&mut values, 0.5);
    let p95 = percentile(&mut values, 0.95);
    let max = values.last().copied().unwrap_or(0.0);
    format!("p50 {p50:5.2} p95 {p95:5.2} max {max:5.2}")
}

/// Measurement: ten seconds of scrolling by wheel, touchpad fling and touch
/// drag through the heavy fixture, each frame's UI work and its scene
/// rasterised on the GPU, the first 0.3 s apart from the rest. Set
/// `PDF_SCROLL_INPUTS=1`; run it on lavapipe, pinned to the cores to test.
#[test]
#[serial_test::serial(pdfium)]
fn measure_scroll_inputs() {
    const FRAMES: usize = 600;
    const EARLY: usize = 18;
    if std::env::var_os("PDF_SCROLL_INPUTS").is_none() {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let path = heavy_pdf(dir.path(), 40);
    for input in [
        ScrollInput::Wheel,
        ScrollInput::Fling,
        ScrollInput::TouchDrag,
    ] {
        // The GPU (lavapipe where there is none), or with
        // `PDF_SCROLL_PAINTER=cpu` the CPU renderer the desktop draws with
        // on a machine without one.
        let painter = if std::env::var("PDF_SCROLL_PAINTER").as_deref() == Ok("cpu") {
            Painter::Cpu
        } else {
            Painter::Gpu
        };
        let Some(frames) = drive_input(&path, input, FRAMES, painter) else {
            return;
        };
        // Frames that drew: an idle frame between wheel notches presents
        // nothing and costs nothing.
        let total = |f: &&InputFrame| f.ui_ms + f.gpu_ms;
        let drawn = |frames: &[InputFrame]| -> Vec<f64> {
            frames
                .iter()
                .filter(|f| f.drew)
                .map(|f| total(&f))
                .collect()
        };
        let (early, late) = frames.split_at(EARLY);
        let moving_late = late.iter().filter(|f| f.moved_px > 0.0).count();
        let per_second: Vec<String> = frames
            .chunks(60)
            .map(|second| {
                let mut ms = drawn(second);
                format!("{:.1}", percentile(&mut ms, 0.95))
            })
            .collect();
        eprintln!(
            "INPUT {input:?}: scrolled {:.0}px, drew {} frames, moved on {moving_late} of the {} frames after 0.3 s\n  first 0.3 s  {}\n  after 0.3 s  {}\n  p95 per second: {}",
            frames.iter().map(|f| f.moved_px).sum::<f64>(),
            frames.iter().filter(|f| f.drew).count(),
            late.len(),
            quantiles(drawn(early)),
            quantiles(drawn(late)),
            per_second.join(" "),
        );
        if std::env::var_os("PDF_SCROLL_TRACE").is_some() {
            for (i, f) in frames.iter().enumerate().take(120) {
                eprintln!(
                    "  {i:3} ui {:6.2} gpu {:6.2} moved {:5.1}",
                    f.ui_ms, f.gpu_ms, f.moved_px
                );
            }
        }
    }
}

/// A touchpad flick or a finger lifted while moving leaves the page gliding
/// to a stop, as every Tilo scroll surface does, and every frame of the
/// drag and the glide only scrolls: the window repaints what scrolled in,
/// never all of itself.
#[test]
#[serial_test::serial(pdfium)]
fn a_lifted_gesture_glides_and_every_frame_only_scrolls() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = heavy_pdf(dir.path(), 12);
    for input in [ScrollInput::TouchDrag, ScrollInput::Fling] {
        let Some(frames) = drive_input(&path, input, 60, Painter::None) else {
            return;
        };
        // Fingers on the surface: frames 0..10 (a flick), 0..20 (a drag).
        let lifted = if input == ScrollInput::Fling { 11 } else { 21 };
        let gliding = frames[lifted..].iter().filter(|f| f.moved_px > 0.0).count();
        assert!(
            gliding >= 10,
            "{input:?}: the page glides on after the lift ({gliding} frames moved)"
        );
        let glide: Vec<f64> = frames[lifted..]
            .iter()
            .map(|f| f.moved_px)
            .filter(|px| *px > 0.0)
            .collect();
        assert!(
            glide.last() < glide.first(),
            "{input:?}: and slows to a stop: {glide:?}"
        );
        // The first frame takes in the pointer arriving over the page.
        let whole = frames[1..].iter().filter(|f| f.drew && f.whole).count();
        let wholes: Vec<String> = (1..frames.len())
            .filter(|i| frames[*i].drew && frames[*i].whole)
            .map(|i| {
                format!(
                    "{i}: moved {} damage {}",
                    frames[i].moved_px, frames[i].damage_px
                )
            })
            .collect();
        assert_eq!(
            whole, 0,
            "{input:?}: no frame repaints the whole window: {wholes:?}"
        );
    }
}

/// Text left selected does not stop the pages scrolling by copying: the
/// selection's bands move with the text, and its bar repaints where it was
/// and where it is.
#[test]
#[serial_test::serial(pdfium)]
fn scrolling_with_text_selected_still_only_scrolls() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = heavy_pdf(dir.path(), 12);
    let Some(mut reader) = open_settled(&path) else {
        return;
    };
    assert!(reader.select_all(), "the document's text is selected");
    let size = (1280, 800);
    let surface = Window(size.0, size.1);
    let mut app = ui::App::new(crate::scenarios::record_state(), Box::new(reader));
    let mut damage = ui::app::SurfaceDamage::copying();
    let mut scene = vello::Scene::new();
    app.init_layout();
    // The wheel scrolls what is under the pointer: the pages.
    app.surface_input(
        &surface,
        winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(640.0, 450.0),
        },
        false,
    );
    for _ in 0..60 {
        scene.reset();
        app.surface_frame_with_damage(&surface, &mut scene, &mut damage);
        std::thread::sleep(Duration::from_millis(16));
    }
    let _ = damage.take();
    let mut copied = 0;
    for _ in 0..20 {
        wheel_down(&mut app, &surface, 24.0);
        scene.reset();
        app.surface_frame_with_damage(&surface, &mut scene, &mut damage);
        if damage
            .take_copying()
            .is_some_and(|(moves, _)| !moves.is_empty())
        {
            copied += 1;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    assert!(
        copied >= 18,
        "scroll frames with a selection copy what stays on screen ({copied} of 20)"
    );
}
