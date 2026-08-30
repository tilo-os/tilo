//! `flatpak-share-demo` — a self-contained visual demo of the
//! flatpak-share pipeline.
//!
//! For each frame: synthesise a source RGBA frame (moving text stamp,
//! bouncing shapes, rotating gradient); feed it through the production
//! [`shell::network::encoder::H264Encoder`]; push the NALU payload
//! through a [`frame_link::FrameLink`] shim configured with
//! `--latency-ms`/`--throughput-kbps`; drain the shim and feed anything
//! ready into the production [`shell::network::decoder::H264Decoder`];
//! composite source-on-left + decoded-on-right into a 2W×H canvas;
//! encode the composite via a second `H264Encoder` and write the Annex-B
//! stream through `ffmpeg -f h264 -i - -c copy out.mp4`.
//!
//! Nothing is mocked — the encoder and decoder are the same code paths
//! the compositor runs in production; the only synthetic piece is the
//! shim, the *point* of the demo. A `tracing`-subscriber accumulates
//! per-span duration and dumps a profile summary to
//! `target/latency-profile.txt`.
//!
//! ## CLI
//!
//! ```text
//! flatpak-share-demo \
//!     --duration 10 \
//!     --width 1280 --height 720 --fps 30 \
//!     --latency-ms 80 --throughput-kbps 2000 \
//!     --out target/comparison.mp4
//! ```
//!
//! ## Why the demo re-encodes
//!
//! The shim's delivered frames are only meaningful side-by-side with the
//! source. Recomposing the two into one wide frame and re-encoding
//! produces an MP4 that plays anywhere; the profile summary breaks out
//! `compose+reencode` as its own span so the reader can attribute the
//! overhead separately from the production pipeline.

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use frame_link::{FrameLink, LinkConfig, LinkStats};
use shell::network::decoder::H264Decoder;
use shell::network::encoder::H264Encoder;
use tracing::{Level, Subscriber, span};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

/// Command-line surface. `Default` picks numbers matching the task spec
/// so `flatpak-share-demo` with no flags produces a working demo.
#[derive(Debug, Clone)]
struct Args {
    duration_secs: u32,
    width: u32,
    height: u32,
    fps: u32,
    latency_ms: u32,
    throughput_kbps: u64,
    out: PathBuf,
    profile: PathBuf,
    source: Source,
}

/// Source content selector. `Pattern` is built-in generative content;
/// `Video(path)` decodes an existing MP4 for the demo. Both are
/// RGBA-normalised to the target `width × height × fps`.
#[derive(Debug, Clone)]
enum Source {
    Pattern,
    Video(PathBuf),
}

impl Default for Args {
    fn default() -> Self {
        Self {
            duration_secs: 10,
            width: 1280,
            height: 720,
            fps: 30,
            latency_ms: 80,
            throughput_kbps: 2000,
            out: PathBuf::from("target/comparison.mp4"),
            profile: PathBuf::from("target/latency-profile.txt"),
            source: Source::Pattern,
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: flatpak-share-demo \n\
         \t[--source pattern|<path.mp4>] \n\
         \t[--duration <secs>] [--width <px>] [--height <px>] [--fps <n>] \n\
         \t[--latency-ms <n>] [--throughput-kbps <n>] \n\
         \t[--out <path.mp4>] [--profile <path.txt>]\n\
         \n\
         All flags optional. Defaults match the qOS collab-viewer demo:\n\
         --duration 10 --width 1280 --height 720 --fps 30\n\
         --latency-ms 80 --throughput-kbps 2000\n\
         --out target/comparison.mp4 --profile target/latency-profile.txt"
    );
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--duration" => {
                args.duration_secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--duration expects <secs>".to_string())?;
            }
            "--width" => {
                args.width = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--width expects <px>".to_string())?;
            }
            "--height" => {
                args.height = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--height expects <px>".to_string())?;
            }
            "--fps" => {
                args.fps = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--fps expects <n>".to_string())?;
            }
            "--latency-ms" => {
                args.latency_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--latency-ms expects <n>".to_string())?;
            }
            "--throughput-kbps" => {
                args.throughput_kbps = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| "--throughput-kbps expects <n>".to_string())?;
            }
            "--out" => {
                args.out = it
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| "--out expects <path.mp4>".to_string())?;
            }
            "--profile" => {
                args.profile = it
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| "--profile expects <path.txt>".to_string())?;
            }
            "--source" => {
                let v = it
                    .next()
                    .ok_or_else(|| "--source expects `pattern` or <path.mp4>".to_string())?;
                args.source = if v == "pattern" {
                    Source::Pattern
                } else {
                    Source::Video(PathBuf::from(v))
                }
            }
            "--source-video" => {
                let v = it
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| "--source-video expects <path.mp4>".to_string())?;
                args.source = Source::Video(v);
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    // openh264 requires even dimensions.
    if args.width % 2 != 0 || args.height % 2 != 0 {
        return Err(format!(
            "openh264 requires even dimensions; got {}x{}",
            args.width, args.height
        ));
    }
    if args.duration_secs == 0 || args.fps == 0 {
        return Err(format!(
            "--duration ({}) and --fps ({}) must both be > 0",
            args.duration_secs, args.fps
        ));
    }
    Ok(args)
}

/// Minimal tracing layer that accumulates per-span-name wall-clock
/// duration + call count, writing a text summary on drop — good enough
/// without pulling in `tracing-tree`/`tracing-flame`.
#[derive(Default)]
struct ProfileLayer {
    inner: Arc<Mutex<ProfileInner>>,
}

#[derive(Default)]
struct ProfileInner {
    // Per-span name → accumulated stats; BTreeMap so the report reads in
    // a stable order.
    per_span: BTreeMap<String, SpanStats>,
    // Entry timestamps keyed on the span id. `span::Id` isn't `Ord` but
    // is hashable, hence a HashMap here.
    entered_at: HashMap<u64, Instant>,
}

#[derive(Default, Clone, Copy)]
struct SpanStats {
    total_ns: u128,
    count: u64,
}

impl ProfileLayer {
    fn snapshot(&self) -> Vec<(String, SpanStats)> {
        let inner = self.inner.lock().expect("profile lock");
        inner
            .per_span
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }
}

impl<S> Layer<S> for ProfileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &span::Id, _ctx: Context<'_, S>) {
        let mut inner = self.inner.lock().expect("profile lock");
        inner.entered_at.insert(id.into_u64(), Instant::now());
    }
    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        let now = Instant::now();
        let key = id.into_u64();
        // Resolve the span name first so the borrow on `ctx` doesn't
        // outlive the `inner` mutable borrow.
        let name = ctx.span(id).map(|s| s.name().to_string());
        let mut inner = self.inner.lock().expect("profile lock");
        if let Some(entered) = inner.entered_at.remove(&key) {
            let dur = now.saturating_duration_since(entered);
            if let Some(name) = name {
                let entry = inner.per_span.entry(name).or_default();
                entry.total_ns = entry.total_ns.saturating_add(dur.as_nanos());
                entry.count = entry.count.saturating_add(1);
            }
        }
    }
}

/// The demo's per-frame source RGBA canvas + text overlay.
///
/// Produces `width × height × 4` bytes: bouncing coloured discs +
/// rotating gradient background with a monospace `t=NNNNms` stamp so
/// latency is directly visible from a still frame.
struct SourceRenderer {
    width: u32,
    height: u32,
    font: FontVec,
    rgba: Vec<u8>,
}

impl SourceRenderer {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        // Font search: distrobox is Fedora-flavoured so Noto Sans lives at a
        // well-known path; fall back to Adwaita Mono and a bundled
        // last-resort for unusual environments.
        let font_paths = [
            "/usr/share/fonts/google-noto-vf/NotoSansMono[wght].ttf",
            "/usr/share/fonts/google-noto-vf/NotoSans[wght].ttf",
            "/usr/share/fonts/adwaita-mono-fonts/AdwaitaMono-Bold.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
            "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Bold.ttf",
        ];
        let bytes = font_paths
            .iter()
            .find_map(|p| fs::read(p).ok())
            .ok_or_else(|| format!("no font found — tried {font_paths:?}"))?;
        let font = FontVec::try_from_vec(bytes).map_err(|e| format!("font parse failed: {e:?}"))?;
        let rgba = vec![0u8; (width as usize) * (height as usize) * 4];
        Ok(Self {
            width,
            height,
            font,
            rgba,
        })
    }

    /// Render a fresh frame at millisecond `t_ms` into the internal
    /// buffer. `frame_index` is used for animation phase.
    fn render(&mut self, t_ms: u64, frame_index: u32) -> &[u8] {
        // Rotating gradient background: hue rotates through the frame index
        // so compression artifacts on smooth-motion are obvious.
        let phase = (frame_index as f32) * 0.02;
        for y in 0..self.height {
            for x in 0..self.width {
                let u = x as f32 / self.width as f32;
                let v = y as f32 / self.height as f32;
                let hue = (u + v + phase).fract();
                let (r, g, b) = hsv_to_rgb(hue, 0.35, 0.65);
                let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
                self.rgba[i] = r;
                self.rgba[i + 1] = g;
                self.rgba[i + 2] = b;
                self.rgba[i + 3] = 255;
            }
        }
        // Three bouncing discs so motion is obvious under packet loss.
        let bounces = [
            (0.3, 0.25, 60.0, (255, 96, 96)),
            (0.6, 0.4, 45.0, (96, 200, 255)),
            (0.9, 0.6, 55.0, (255, 220, 96)),
        ];
        for &(speed_x, speed_y, radius, color) in &bounces {
            let t = (frame_index as f32) * 0.05;
            let x = bounce(t * speed_x, self.width as f32, radius);
            let y = bounce(t * speed_y, self.height as f32, radius);
            self.draw_disc(x as i32, y as i32, radius as i32, color);
        }
        // Text stamp — top-left, "SOURCE t=NNNNms".
        let text = format!("t={t_ms:>5}ms   frame #{frame_index:>4}");
        self.draw_text(24, 24, 48.0, &text, (255, 255, 255));
        &self.rgba
    }

    fn draw_disc(&mut self, cx: i32, cy: i32, radius: i32, color: (u8, u8, u8)) {
        let r2 = radius * radius;
        for dy in -radius..=radius {
            let py = cy + dy;
            if py < 0 || py >= self.height as i32 {
                continue;
            }
            for dx in -radius..=radius {
                let d2 = dx * dx + dy * dy;
                if d2 > r2 {
                    continue;
                }
                let px = cx + dx;
                if px < 0 || px >= self.width as i32 {
                    continue;
                }
                // Fade the edge for a soft-looking disc.
                let edge = 1.0 - ((d2 as f32).sqrt() / radius as f32).min(1.0);
                let alpha = (edge * edge).min(1.0);
                let i = ((py as usize) * (self.width as usize) + px as usize) * 4;
                self.rgba[i] = blend(self.rgba[i], color.0, alpha);
                self.rgba[i + 1] = blend(self.rgba[i + 1], color.1, alpha);
                self.rgba[i + 2] = blend(self.rgba[i + 2], color.2, alpha);
                self.rgba[i + 3] = 255;
            }
        }
    }

    fn draw_text(&mut self, x0: i32, y0: i32, size_px: f32, text: &str, color: (u8, u8, u8)) {
        let scale = PxScale::from(size_px);
        let scaled = self.font.as_scaled(scale);
        let ascent = scaled.ascent();
        let mut caret_x = x0 as f32;
        let baseline = y0 as f32 + ascent;
        for ch in text.chars() {
            let g = self.font.glyph_id(ch);
            let glyph = g.with_scale(scale);
            let advance = self.font.as_scaled(scale).h_advance(g);
            if let Some(outlined) = self.font.outline_glyph(glyph.clone()) {
                let bounds = outlined.px_bounds();
                let ox = caret_x + bounds.min.x;
                let oy = baseline + bounds.min.y;
                outlined.draw(|dx, dy, cov| {
                    let px = ox as i32 + dx as i32;
                    let py = oy as i32 + dy as i32;
                    if px < 0 || py < 0 || px >= self.width as i32 || py >= self.height as i32 {
                        return;
                    }
                    let i = ((py as usize) * (self.width as usize) + px as usize) * 4;
                    let a = cov.min(1.0);
                    self.rgba[i] = blend(self.rgba[i], color.0, a);
                    self.rgba[i + 1] = blend(self.rgba[i + 1], color.1, a);
                    self.rgba[i + 2] = blend(self.rgba[i + 2], color.2, a);
                    self.rgba[i + 3] = 255;
                });
            }
            caret_x += advance;
        }
    }
}

fn blend(bg: u8, fg: u8, alpha: f32) -> u8 {
    let bg = f32::from(bg);
    let fg = f32::from(fg);
    (bg + (fg - bg) * alpha).clamp(0.0, 255.0) as u8
}

fn bounce(t: f32, extent: f32, margin: f32) -> f32 {
    // Triangle wave over `[margin, extent - margin]`.
    let span = (extent - 2.0 * margin).max(1.0);
    let raw = (t * 40.0).abs() % (span * 2.0);
    let folded = if raw < span { raw } else { span * 2.0 - raw };
    margin + folded
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let i = h6 as u32;
    let f = h6 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Compose the side-by-side canvas — source on the left half, streamed
/// on the right half — into `out_rgba` (2W × H), stamping the two column
/// headers + a bottom stats strip.
struct CompositeRenderer {
    src_w: u32,
    src_h: u32,
    canvas_w: u32,
    canvas_h: u32,
    header_h: u32,
    // Retained for parity with the layout math; not read from `compose`
    // since the footer text position is fixed relative to `header_h + src_h`.
    #[allow(dead_code)]
    footer_h: u32,
    out: Vec<u8>,
    font: FontVec,
    header_left: String,
    header_right: String,
}

impl CompositeRenderer {
    fn new(src_w: u32, src_h: u32, latency_ms: u32, throughput_kbps: u64) -> Result<Self, String> {
        // Round to even so the composite encoder is happy (openh264
        // rejects odd dimensions).
        let header_h = 48u32;
        let footer_h = 44u32;
        let canvas_w = round_even(src_w * 2);
        let canvas_h = round_even(src_h + header_h + footer_h);
        let out = vec![0u8; (canvas_w as usize) * (canvas_h as usize) * 4];
        let font_paths = [
            "/usr/share/fonts/google-noto-vf/NotoSans[wght].ttf",
            "/usr/share/fonts/google-noto-vf/NotoSansMono[wght].ttf",
            "/usr/share/fonts/adwaita-mono-fonts/AdwaitaMono-Bold.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
        ];
        let bytes = font_paths
            .iter()
            .find_map(|p| fs::read(p).ok())
            .ok_or_else(|| "no font found for composite text".to_string())?;
        let font = FontVec::try_from_vec(bytes).map_err(|e| format!("font parse failed: {e:?}"))?;
        let header_left = "SOURCE".to_string();
        let header_right =
            format!("STREAMED · {latency_ms}ms latency · {throughput_kbps} kbps throttle");
        Ok(Self {
            src_w,
            src_h,
            canvas_w,
            canvas_h,
            header_h,
            footer_h,
            out,
            font,
            header_left,
            header_right,
        })
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.canvas_w, self.canvas_h)
    }

    /// Compose one output frame. `src_rgba` is the source `src_w × src_h ×
    /// 4`; `decoded_rgba` is the shim-passed reconstruction (same
    /// dimensions) or `None` if none was ready yet.
    #[tracing::instrument(level = "trace", skip(self, src_rgba, decoded_rgba, footer))]
    fn compose(&mut self, src_rgba: &[u8], decoded_rgba: Option<&[u8]>, footer: &str) -> &[u8] {
        // Wipe canvas to dark grey.
        for chunk in self.out.chunks_exact_mut(4) {
            chunk[0] = 20;
            chunk[1] = 22;
            chunk[2] = 26;
            chunk[3] = 255;
        }
        // Left half — source.
        let y0 = self.header_h as usize;
        for y in 0..self.src_h as usize {
            let src_row =
                &src_rgba[y * (self.src_w as usize) * 4..(y + 1) * (self.src_w as usize) * 4];
            let dst_row_start = (y + y0) * (self.canvas_w as usize) * 4;
            let dst_row_end = dst_row_start + (self.src_w as usize) * 4;
            self.out[dst_row_start..dst_row_end].copy_from_slice(src_row);
        }
        // Right half — decoded (or an "awaiting..." placeholder).
        let x0 = self.src_w as usize;
        if let Some(dec) = decoded_rgba {
            for y in 0..self.src_h as usize {
                let src_row =
                    &dec[y * (self.src_w as usize) * 4..(y + 1) * (self.src_w as usize) * 4];
                let dst_row_start = (y + y0) * (self.canvas_w as usize) * 4 + x0 * 4;
                let dst_row_end = dst_row_start + (self.src_w as usize) * 4;
                self.out[dst_row_start..dst_row_end].copy_from_slice(src_row);
            }
        } else {
            // Fill with a distinct dark-red so the "waiting on the shim" phase
            // is obvious.
            for y in 0..self.src_h as usize {
                for x in 0..self.src_w as usize {
                    let i = ((y + y0) * (self.canvas_w as usize) + (x + x0)) * 4;
                    self.out[i] = 40;
                    self.out[i + 1] = 20;
                    self.out[i + 2] = 20;
                    self.out[i + 3] = 255;
                }
            }
        }
        // Headers. Clone the strings out first so we don't hold an immutable
        // borrow on `self` across the mutable-borrow of `draw_text` — cheap,
        // the strings are short and static.
        let header_left = self.header_left.clone();
        let header_right = self.header_right.clone();
        self.draw_text(
            (self.src_w / 2) as i32 - 40,
            8,
            28.0,
            &header_left,
            (200, 220, 255),
        );
        self.draw_text(
            (self.src_w + self.src_w / 2) as i32 - 200,
            8,
            28.0,
            &header_right,
            (255, 200, 160),
        );
        // Divider strip between the two panes.
        for y in 0..self.src_h as usize {
            let i = ((y + y0) * (self.canvas_w as usize) + self.src_w as usize) * 4;
            for k in 0..4 {
                self.out[i + k - 4] = 0;
                self.out[i + k] = 0;
            }
        }
        // Footer with rolling stats.
        let footer_y0 = (self.header_h + self.src_h) as i32 + 8;
        self.draw_text(24, footer_y0, 22.0, footer, (200, 220, 240));
        &self.out
    }

    fn draw_text(&mut self, x0: i32, y0: i32, size_px: f32, text: &str, color: (u8, u8, u8)) {
        let scale = PxScale::from(size_px);
        let scaled = self.font.as_scaled(scale);
        let ascent = scaled.ascent();
        let mut caret_x = x0 as f32;
        let baseline = y0 as f32 + ascent;
        for ch in text.chars() {
            let g = self.font.glyph_id(ch);
            let glyph = g.with_scale(scale);
            let advance = self.font.as_scaled(scale).h_advance(g);
            if let Some(outlined) = self.font.outline_glyph(glyph.clone()) {
                let bounds = outlined.px_bounds();
                let ox = caret_x + bounds.min.x;
                let oy = baseline + bounds.min.y;
                outlined.draw(|dx, dy, cov| {
                    let px = ox as i32 + dx as i32;
                    let py = oy as i32 + dy as i32;
                    if px < 0 || py < 0 || px >= self.canvas_w as i32 || py >= self.canvas_h as i32
                    {
                        return;
                    }
                    let i = ((py as usize) * (self.canvas_w as usize) + px as usize) * 4;
                    let a = cov.min(1.0);
                    self.out[i] = blend(self.out[i], color.0, a);
                    self.out[i + 1] = blend(self.out[i + 1], color.1, a);
                    self.out[i + 2] = blend(self.out[i + 2], color.2, a);
                    self.out[i + 3] = 255;
                });
            }
            caret_x += advance;
        }
    }
}

fn round_even(v: u32) -> u32 {
    if v.is_multiple_of(2) { v } else { v + 1 }
}

/// Spawn `ffmpeg` to mux the raw H.264 Annex-B stream we're writing to
/// its stdin into a playable MP4, which the user's default video player
/// then opens.
fn spawn_muxer(
    out: &Path,
    fps: u32,
    _width: u32,
    _height: u32,
) -> Result<std::process::Child, String> {
    // Ensure the destination directory exists.
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let child = Command::new("ffmpeg")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        // Input: raw H.264 Annex-B on stdin at the caller's framerate; the
        // demuxer picks width/height straight out of the SPS.
        .arg("-f")
        .arg("h264")
        .arg("-framerate")
        .arg(format!("{fps}"))
        .arg("-i")
        .arg("-")
        // Re-encode at the caller's fps: `-c:v copy` would preserve
        // openh264's embedded 60 fps SPS timing hint and make the mp4 play
        // at 2× speed regardless of `-r`. libopenh264 is always available on
        // the ffmpeg build we ship against; libx264 is not, so we don't
        // reach for it.
        .arg("-c:v")
        .arg("libopenh264")
        .arg("-b:v")
        .arg("8000k")
        .arg("-r")
        .arg(format!("{fps}"))
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;
    Ok(child)
}

fn write_profile_report(
    path: &Path,
    layer: &ProfileLayer,
    total_wall: Duration,
) -> Result<(), String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "flatpak-share-demo profile — total wall-clock {:.3} s\n",
        total_wall.as_secs_f64()
    ));
    lines.push(
        "span                                            calls      total_ms     mean_us   %wall\n"
            .to_string(),
    );
    lines.push(
        "---------------------------------------------- ------  ------------  ----------  ------\n"
            .to_string(),
    );
    let mut rows = layer.snapshot();
    rows.sort_by(|a, b| b.1.total_ns.cmp(&a.1.total_ns));
    let wall_ns = total_wall.as_nanos().max(1);
    for (name, stats) in rows {
        let total_ms = stats.total_ns as f64 / 1_000_000.0;
        let mean_us = if stats.count == 0 {
            0.0
        } else {
            (stats.total_ns as f64) / (stats.count as f64) / 1_000.0
        };
        let pct = 100.0 * (stats.total_ns as f64) / (wall_ns as f64);
        lines.push(format!(
            "{name:<46} {:>6}  {total_ms:>12.3}  {mean_us:>10.2}  {pct:>5.2}%\n",
            stats.count
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir profile parent: {e}"))?;
    }
    let mut f = fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    for line in &lines {
        f.write_all(line.as_bytes())
            .map_err(|e| format!("write profile: {e}"))?;
    }
    // Also print to stderr so the demo's console output is self-
    // describing.
    for line in &lines {
        eprint!("{line}");
    }
    Ok(())
}

fn format_footer(
    stats: &LinkStats,
    elapsed: Duration,
    sent: u64,
    decoded: u64,
    dropped: u64,
) -> String {
    let bps = stats.effective_bps(elapsed);
    let mbps = bps / 1_000_000.0;
    let mean_delay = stats.mean_delay_ms();
    format!(
        "frames sent: {sent}  decoded: {decoded}  drops: {dropped}  \
         mean shim delay: {mean_delay:.1} ms  effective bitrate: {mbps:.2} Mbps",
    )
}

/// A minimal H.264 Annex-B parser: chops a `Vec<u8>` back into
/// individual NAL units so the shim can throttle at the correct
/// granularity.
///
/// The encoder emits Annex-B (start code `00 00 00 01` or `00 00 01`);
/// splitting at start codes and re-emitting them keeps the bytes
/// bit-identical for the decoder.
///
/// Not currently called from the demo — the whole per-frame NAL blob is
/// pushed into the shim as one payload so the throttle sees a realistic
/// per-frame packet size (real streaming stacks bundle SPS+PPS+IDR
/// together on keyframes). Kept for downstream callers wanting a
/// finer-grained throttle.
#[allow(dead_code)]
fn split_annex_b(nalus: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let starts = find_start_codes(nalus);
    for w in starts.windows(2) {
        out.push(nalus[w[0]..w[1]].to_vec());
    }
    if let Some(&last) = starts.last() {
        out.push(nalus[last..].to_vec());
    }
    // If we found no start codes, the whole slice is one blob.
    if out.is_empty() && !nalus.is_empty() {
        out.push(nalus.to_vec());
    }
    out
}

#[allow(dead_code)]
fn find_start_codes(bytes: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == 0 && bytes[i + 1] == 0 {
            if bytes[i + 2] == 1 {
                out.push(i);
                i += 3;
                continue;
            }
            if i + 3 < bytes.len() && bytes[i + 2] == 0 && bytes[i + 3] == 1 {
                out.push(i);
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn main() -> Result<(), String> {
    // Install the tracing subscriber early so every span from the shell
    // encoder/decoder/shim is captured.
    let profile_layer: Arc<Mutex<Option<ProfileLayer>>> =
        Arc::new(Mutex::new(Some(ProfileLayer::default())));
    let inner = {
        let slot = profile_layer.lock().unwrap();
        slot.as_ref().unwrap().inner.clone()
    };
    // Build a `ProfileLayer` variant referencing the shared `Arc` so it
    // can be snapshotted after `tracing_subscriber::set_global_default`.
    let shared_layer = ProfileLayer {
        inner: inner.clone(),
    };
    tracing_subscriber::registry()
        .with(shared_layer)
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace")),
        )
        .init();
    let args = parse_args().inspect_err(|_e| {
        print_usage();
    })?;
    eprintln!(
        "flatpak-share-demo: {}x{} @ {}fps for {}s, shim latency={}ms throttle={}kbps → {}",
        args.width,
        args.height,
        args.fps,
        args.duration_secs,
        args.latency_ms,
        args.throughput_kbps,
        args.out.display()
    );

    let total_frames = args.duration_secs * args.fps;
    let frame_interval = Duration::from_micros(1_000_000 / u64::from(args.fps));

    // Production encoder — same crate the compositor uses to publish.
    // Sized to 4 Mbps so 720p content has budget for detail; `FrameLink`
    // then throttles down to `--throughput-kbps`.
    let mut send_encoder =
        H264Encoder::new_with_bitrate_bps(args.width, args.height, args.fps, 4_000_000)
            .map_err(|e| format!("H264Encoder(send): {e:?}"))?;
    // Production decoder — same crate PeerView constructs.
    let mut recv_decoder = H264Decoder::new();

    // The shim in the middle. Payload is `Vec<u8>` (one NAL unit) so the
    // throttle sees individual-NAL granularity.
    let mut shim: FrameLink<Vec<u8>> = FrameLink::with_config(LinkConfig {
        latency_ms: args.latency_ms,
        jitter_ms: 5,
        throughput_kbps: args.throughput_kbps,
        queue_capacity: 512,
        jitter_seed: 0xC0DE,
        // Sized to a couple of frames' budget; keyframes still fit through
        // via the oversize-drain rule.
        token_bucket_bytes: (args.throughput_kbps.max(1) * 1000 / 8 / 2).max(64 * 1024),
    });

    let mut source_renderer = SourceRenderer::new(args.width, args.height)?;
    let mut composite = CompositeRenderer::new(
        args.width,
        args.height,
        args.latency_ms,
        args.throughput_kbps,
    )?;
    let (canvas_w, canvas_h) = composite.dimensions();

    // Encoder for the side-by-side canvas: bigger canvas → bigger
    // bitrate (composite is 2×W×H, so 2× the send-side budget).
    let mut composite_encoder =
        H264Encoder::new_with_bitrate_bps(canvas_w, canvas_h, args.fps, 8_000_000)
            .map_err(|e| format!("H264Encoder(composite): {e:?}"))?;

    // Spawn ffmpeg for muxing.
    let mut muxer = spawn_muxer(&args.out, args.fps, canvas_w, canvas_h)?;
    let mut mux_stdin = muxer
        .stdin
        .take()
        .ok_or_else(|| "ffmpeg stdin missing".to_string())?;

    // Source video path (--source-video) — decode via ffmpeg piping raw
    // RGBA into a channel. `--source pattern` is the default; video-in
    // is a future extension, warned and falls back if requested.
    if let Source::Video(path) = &args.source {
        eprintln!(
            "warning: --source-video ({}) not yet wired to a raw-RGBA decoder in this demo; \
             falling back to the pattern generator.",
            path.display()
        );
    }

    let started = Instant::now();
    let mut sent_frames: u64 = 0;
    let mut decoded_frames: u64 = 0;
    let mut dropped_frames: u64 = 0;
    // Rolling decoded-frame cache — the shim can hold a decoded frame
    // "in flight" for tens of ms, so we keep the last known-decoded to
    // show while waiting for the next real one.
    let mut last_decoded: Option<Vec<u8>> = None;

    for i in 0..total_frames {
        let t_ms = (u64::from(i) * 1000) / u64::from(args.fps);
        let src = source_renderer.render(t_ms, i);

        // Encode the source with the production encoder — same code
        // path SharedFlatpakSession uses.
        let nalus = send_encoder
            .encode(src)
            .map_err(|e| format!("send_encoder.encode: {e:?}"))?;
        sent_frames += 1;

        // Push the full per-frame NAL blob as a single shim payload — the
        // throttle sees per-frame granularity, matching how a real streaming
        // path bundles keyframe SPS+PPS+IDR together. Splitting into
        // individual NALs would let the throttle pace within a frame,
        // understating real burst behaviour.
        let now = Instant::now();
        let size = nalus.len() as u64;
        shim.push(now, size, nalus);

        // Drain everything the shim has decided is ready, feeding each
        // frame's NAL blob into the decoder in emitted order. openh264's
        // decoder may return `None` for the first few frames while
        // buffering SPS/PPS metadata — expected; the counter only advances
        // on Some(frame).
        for nal_blob in shim.drain_ready(now) {
            let span = span!(
                Level::TRACE,
                "recv.decoder.decode",
                nal_bytes = nal_blob.len()
            );
            let _entered = span.enter();
            if let Some(frame) = recv_decoder.decode(&nal_blob) {
                // Post-HDR decoder now returns typed DecodedPixels; the
                // demo only handles SDR RGBA, so extract those bytes.
                if let shell::network::decoder::DecodedPixels::Rgba8(rgba) = frame.pixels {
                    last_decoded = Some(rgba);
                }
                decoded_frames += 1;
            }
        }

        // Update running stats.
        let shim_stats = shim.stats();
        dropped_frames = shim_stats.frames_dropped;
        let footer = format_footer(
            &shim_stats,
            started.elapsed(),
            sent_frames,
            decoded_frames,
            dropped_frames,
        );

        // Compose the side-by-side canvas.
        let composite_rgba = composite.compose(src, last_decoded.as_deref(), &footer);

        // Encode the composite via the second encoder + stream into
        // ffmpeg's stdin. `send_all` blocks until the write completes;
        // ffmpeg keeps up trivially at 1280x720 @ 30 fps.
        let composite_nalus = composite_encoder
            .encode(composite_rgba)
            .map_err(|e| format!("composite_encoder.encode: {e:?}"))?;
        mux_stdin
            .write_all(&composite_nalus)
            .map_err(|e| format!("ffmpeg stdin write: {e}"))?;

        // Real-time-ish: rest of the frame interval budget goes to sleep so
        // the demo's wall clock reflects the streaming rate. Skipped on the
        // last frame.
        let elapsed_this_frame = now.elapsed();
        if elapsed_this_frame < frame_interval && i + 1 < total_frames {
            std::thread::sleep(frame_interval.checked_sub(elapsed_this_frame).unwrap());
        }
    }

    // Drain anything still in the shim so late-arriving frames land in
    // the composite tail — given a "far future" instant so everything
    // queued is deliverable.
    let flush_now = Instant::now() + Duration::from_hours(1);
    for nal_blob in shim.drain_ready(flush_now) {
        if let Some(frame) = recv_decoder.decode(&nal_blob) {
            // last_decoded is dropped at end of scope — the flush loop exists to
            // count late-arriving decodes into `decoded_frames` for the summary
            // line, not to display them.
            let _ = &last_decoded;
            if let shell::network::decoder::DecodedPixels::Rgba8(rgba) = frame.pixels {
                last_decoded = Some(rgba);
            }
            decoded_frames += 1;
        }
    }

    // Close ffmpeg stdin so it flushes + finalises the MP4.
    drop(mux_stdin);
    let status = muxer.wait().map_err(|e| format!("wait ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg exited non-zero: {status}"));
    }

    let total_wall = started.elapsed();
    let shim_stats = shim.stats();
    let bps = shim_stats.effective_bps(total_wall);
    eprintln!(
        "\ndemo done: {sent} src frames → {decoded} decoded ({drop} shim drops), \
         wrote {}, effective shim throughput {:.2} Mbps",
        args.out.display(),
        bps / 1_000_000.0,
        sent = sent_frames,
        decoded = decoded_frames,
        drop = dropped_frames,
    );

    // Snapshot the profile-layer accumulator + write the report.
    let layer_ref = ProfileLayer { inner };
    write_profile_report(&args.profile, &layer_ref, total_wall)?;

    Ok(())
}
