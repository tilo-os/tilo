//! `flatpak-share-demo` — a self-contained visual demo of the
//! flatpak-share pipeline.
//!
//! For each frame: synthesise a source RGBA frame (moving text stamp,
//! bouncing shapes, rotating gradient); feed it through the production
//! [`tilo_callstream::VideoEncoder`] in the codec a share would negotiate on
//! this machine; push the access unit through a [`frame_link::FrameLink`]
//! shim configured with `--latency-ms`/`--throughput-kbps`; drain the shim
//! and feed anything ready into the production
//! [`tilo_callstream::surface_decoder::VideoDecoder`];
//! composite source-on-left + decoded-on-right into a 2W×H canvas;
//! encode the composite via a second encoder and mux it into an MP4 with
//! the compositor's recording muxer.
//!
//! The encoder and decoder are the same code paths used by the compositor;
//! the shim supplies the synthetic network conditions. A `tracing` subscriber
//! accumulates per-span duration and dumps a profile summary to
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use frame_link::{FrameLink, LinkConfig, LinkStats};
use tilo_callstream::VideoEncoder;
use tilo_callstream::mp4_recording::{Mp4Recording, recording_encoder};
use tilo_callstream::surface_decoder::VideoDecoder;
use tilo_lock::MutexRecover as _;
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
         All flags optional. Defaults match the Tilo collab-viewer demo:\n\
         --duration 10 --width 1280 --height 720 --fps 30\n\
         --latency-ms 80 --throughput-kbps 2000\n\
         --out target/comparison.mp4 --profile target/latency-profile.txt"
    );
}

/// Parses argv into [`Args`]. `Ok(None)` means `-h`/`--help` was given:
/// usage was already printed and the caller should exit 0, not treat it
/// as an error.
fn parse_args() -> Result<Option<Args>, String> {
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
                return Ok(None);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    // 4:2:0 encoders require even dimensions.
    if args.width % 2 != 0 || args.height % 2 != 0 {
        return Err(format!(
            "video encoders require even dimensions; got {}x{}",
            args.width, args.height
        ));
    }
    if args.duration_secs == 0 || args.fps == 0 {
        return Err(format!(
            "--duration ({}) and --fps ({}) must both be > 0",
            args.duration_secs, args.fps
        ));
    }
    Ok(Some(args))
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
        let inner = self.inner.lock_recover();
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
        let mut inner = self.inner.lock_recover();
        inner.entered_at.insert(id.into_u64(), Instant::now());
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard is already taken as late as possible (after `dur`/`name` are \
                  resolved) and holds only the in-place read-modify-write it protects"
    )]
    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        let now = Instant::now();
        let key = id.into_u64();
        // Resolve the span name first so the borrow on `ctx` doesn't
        // outlive the `inner` mutable borrow.
        let name = ctx.span(id).map(|s| s.name().to_string());
        // Two short, separate critical sections rather than one held across
        // both the removal and the entry update — each lock/unlock pairs
        // tightly with the map operation it protects.
        let entered = self.inner.lock_recover().entered_at.remove(&key);
        if let Some(entered) = entered {
            let dur = now.saturating_duration_since(entered);
            if let Some(name) = name {
                let mut inner = self.inner.lock_recover();
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
    text: TextPainter,
    rgba: Vec<u8>,
}

impl SourceRenderer {
    fn new(width: u32, height: u32) -> Self {
        // Saturating rather than `*`: a pathological `--width`/`--height`
        // wrapping around usize would allocate an undersized buffer, and
        // every pixel write below would then be silently out of range
        // instead of loudly failing here.
        let rgba_len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        let rgba = vec![0u8; rgba_len];
        Self {
            width,
            height,
            text: TextPainter::new(Family::Monospace),
            rgba,
        }
    }

    /// Render a fresh frame at millisecond `t_ms` into the internal
    /// buffer. `frame_index` is used for animation phase.
    #[allow(
        clippy::many_single_char_names,
        reason = "x/y (pixel coords), u/v (normalized coords) and r/g/b (color channels) are standard graphics-code names; spelling them out would obscure the correspondence to the maths, not clarify it"
    )]
    fn render(&mut self, t_ms: u64, frame_index: u32) -> &[u8] {
        // Rotating gradient background: hue rotates through the frame index
        // so compression artifacts on smooth-motion are obvious.
        let phase = u32_as_f32(frame_index) * 0.02;
        for y in 0..self.height {
            for x in 0..self.width {
                let u = u32_as_f32(x) / u32_as_f32(self.width);
                let v = u32_as_f32(y) / u32_as_f32(self.height);
                let hue = (u + v + phase).fract();
                let (r, g, b) = hsv_to_rgb(hue, 0.35, 0.65);
                if let Some(i) = pixel_offset(i64::from(x), i64::from(y), self.width, self.height) {
                    write_pixel(&mut self.rgba, i, (r, g, b), 1.0);
                }
            }
        }
        // Three bouncing discs so motion is obvious under packet loss.
        let bounces = [
            (0.3, 0.25, 60.0, (255, 96, 96)),
            (0.6, 0.4, 45.0, (96, 200, 255)),
            (0.9, 0.6, 55.0, (255, 220, 96)),
        ];
        for &(speed_x, speed_y, radius, color) in &bounces {
            let t = u32_as_f32(frame_index) * 0.05;
            let x = bounce(t * speed_x, u32_as_f32(self.width), radius);
            let y = bounce(t * speed_y, u32_as_f32(self.height), radius);
            self.draw_disc(
                f32_as_i32_sat(x),
                f32_as_i32_sat(y),
                f32_as_i32_sat(radius),
                color,
            );
        }
        // Text stamp — top-left, "SOURCE t=NNNNms".
        let text = format!("t={t_ms:>5}ms   frame #{frame_index:>4}");
        let canvas = (self.width, self.height);
        self.text.draw(
            &mut self.rgba,
            canvas,
            (24, 24),
            48.0,
            &text,
            (255, 255, 255),
        );
        &self.rgba
    }

    fn draw_disc(&mut self, cx: i32, cy: i32, radius: i32, color: (u8, u8, u8)) {
        // Squared distances stay well within i32 for any on-screen radius;
        // saturating (rather than wrapping) means a pathological radius
        // just clips the disc to "very large" instead of flipping negative
        // and silently drawing nothing. `saturating_neg` covers the same
        // ground for the range bound itself (`-radius` would panic on
        // overflow only at `i32::MIN`, which no caller passes).
        let r2 = radius.saturating_mul(radius);
        let height_i32 = u32_to_i32_sat(self.height);
        let width_i32 = u32_to_i32_sat(self.width);
        for dy in radius.saturating_neg()..=radius {
            let py = cy.saturating_add(dy);
            if py < 0 || py >= height_i32 {
                continue;
            }
            for dx in radius.saturating_neg()..=radius {
                let d2 = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
                if d2 > r2 {
                    continue;
                }
                let px = cx.saturating_add(dx);
                if px < 0 || px >= width_i32 {
                    continue;
                }
                // Fade the edge for a soft-looking disc.
                let edge = 1.0 - (i32_as_f32(d2).sqrt() / i32_as_f32(radius)).min(1.0);
                let alpha = (edge * edge).min(1.0);
                if let Some(i) = pixel_offset(i64::from(px), i64::from(py), self.width, self.height)
                {
                    write_pixel(&mut self.rgba, i, color, alpha);
                }
            }
        }
    }
}

/// Shapes and rasterises overlay text with the toolkit's text stack:
/// cosmic-text resolves `family` through the system font configuration and
/// swash produces the coverage masks.
struct TextPainter {
    fonts: FontSystem,
    cache: SwashCache,
    family: Family<'static>,
}

impl TextPainter {
    fn new(family: Family<'static>) -> Self {
        Self {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
            family,
        }
    }

    /// Paint `text` in bold with its top-left corner at `origin` into a
    /// `canvas.0 × canvas.1` RGBA buffer, clipping at the canvas edges.
    fn draw(
        &mut self,
        rgba: &mut [u8],
        canvas: (u32, u32),
        origin: (i32, i32),
        size_px: f32,
        text: &str,
        color: (u8, u8, u8),
    ) {
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(size_px, size_px * 1.2));
        let mut buffer = buffer.borrow_with(&mut self.fonts);
        buffer.set_size(None, None);
        let attrs = Attrs::new().family(self.family).weight(Weight::BOLD);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        let (width, height) = canvas;
        buffer.draw(
            &mut self.cache,
            Color::rgb(255, 255, 255),
            |x, y, w, h, coverage| {
                let alpha = f32::from(coverage.a()) / 255.0;
                for dy in 0..h {
                    for dx in 0..w {
                        let px = i64::from(origin.0)
                            .saturating_add(i64::from(x))
                            .saturating_add(i64::from(dx));
                        let py = i64::from(origin.1)
                            .saturating_add(i64::from(y))
                            .saturating_add(i64::from(dy));
                        if let Some(i) = pixel_offset(px, py, width, height) {
                            write_pixel(rgba, i, color, alpha);
                        }
                    }
                }
            },
        );
    }
}

/// Byte offset of pixel `(x, y)` in a `width × height` RGBA buffer, or
/// `None` if it falls outside `[0, width) x [0, height)`. Callers derive
/// `(x, y)` from glyph/disc/gradient math that isn't provably in-range to
/// the compiler (and, for glyphs, genuinely can be off-canvas at the
/// edges) — this is the one place that turns "out of range" into "skip
/// the pixel" instead of an indexing panic.
fn pixel_offset(x: i64, y: i64, width: u32, height: u32) -> Option<usize> {
    if x < 0 || y < 0 {
        return None;
    }
    let x = usize::try_from(x).ok()?;
    let y = usize::try_from(y).ok()?;
    pixel_offset_usize(x, y, width, height)
}

/// As [`pixel_offset`], for callers already working in `usize` pixel
/// coordinates (no possibility of a negative coordinate to check for).
fn pixel_offset_usize(x: usize, y: usize, width: u32, height: u32) -> Option<usize> {
    if x >= width as usize || y >= height as usize {
        return None;
    }
    y.checked_mul(width as usize)?
        .checked_add(x)?
        .checked_mul(4)
}

/// `n` widened to `i32`, saturating rather than wrapping if it doesn't
/// fit — used for pixel-position math where a value this large would mean
/// a video many times wider than any real display, not a value clippy
/// should let silently flip negative.
fn u32_to_i32_sat(n: u32) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Lossy `u32 -> f32` / `i32 -> f32`, used throughout this file's pixel
/// math (coordinates, dimensions, radii). Every value that flows through
/// these is a screen-space pixel measurement — at most a few thousand,
/// many orders of magnitude below where `f32` (24-bit mantissa, exact up
/// to 2^24 ≈ 16.7M) would start losing integer precision.
#[allow(
    clippy::cast_precision_loss,
    reason = "pixel-space coordinates/dimensions here stay far below 2^24; see doc comment"
)]
fn u32_as_f32(n: u32) -> f32 {
    n as f32
}
#[allow(
    clippy::cast_precision_loss,
    reason = "pixel-space coordinates/dimensions here stay far below 2^24; see doc comment"
)]
fn i32_as_f32(n: i32) -> f32 {
    n as f32
}

/// `v` truncated to `i32`. `as` already saturates a float-to-int cast at
/// the target type's bounds (a language guarantee since Rust 1.45 — never
/// UB, never wraps), so this is exactly as safe as clippy's suggested
/// alternative; the exemption just names that guarantee instead of
/// re-deriving it at every pixel-coordinate cast site.
#[allow(
    clippy::cast_possible_truncation,
    reason = "float-to-int `as` casts saturate at the target's bounds by language guarantee; see doc comment"
)]
fn f32_as_i32_sat(v: f32) -> i32 {
    v as i32
}

/// Copy one `width`-pixel (`width * 4` byte) RGBA row from `src` (row
/// `src_y` of a buffer with `src_w` pixels per row) into `dst` (row
/// `dst_y` of a buffer `dst_stride` bytes per row, `dst_x0` pixels in from
/// the left). No-op if either computed range falls outside its buffer —
/// every caller's `src_y`/`dst_y`/`dst_x0` are already loop-bounded, so
/// this is a safety net for the lint, not an expected path.
fn blit_row(
    dst: &mut [u8],
    dst_stride: usize,
    dst_x0: usize,
    dst_y: usize,
    src: &[u8],
    src_w: usize,
    src_y: usize,
) {
    let row_bytes = src_w.saturating_mul(4);
    let src_start = src_y.saturating_mul(row_bytes);
    let src_end = src_start.saturating_add(row_bytes);
    let dst_start = dst_y
        .saturating_mul(dst_stride)
        .saturating_add(dst_x0.saturating_mul(4));
    let dst_end = dst_start.saturating_add(row_bytes);
    let (Some(src_row), Some(dst_row)) =
        (src.get(src_start..src_end), dst.get_mut(dst_start..dst_end))
    else {
        return;
    };
    dst_row.copy_from_slice(src_row);
}

/// Blend `color` into the RGBA pixel at byte offset `i` in `buf` by
/// `alpha` (`1.0` fully replaces, matching a plain overwrite for the
/// common opaque-write case). No-op if `i..i+4` is out of range for `buf`.
#[allow(
    clippy::many_single_char_names,
    reason = "r/g/b/a are the conventional RGBA channel names and i is a conventional byte \
              offset; spelling them out would read worse, not better"
)]
fn write_pixel(buf: &mut [u8], i: usize, color: (u8, u8, u8), alpha: f32) {
    let Some(px) = buf.get_mut(i..i.saturating_add(4)) else {
        return;
    };
    let [r, g, b, a] = px else {
        return;
    };
    *r = blend(*r, color.0, alpha);
    *g = blend(*g, color.1, alpha);
    *b = blend(*b, color.2, alpha);
    *a = 255;
}

/// `v` clamped into `[0, 255]` and cast to `u8`. The clamp makes the cast
/// provably in-range; clippy can't see through `clamp`, so the exemption
/// lives here once instead of at every call site.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "v is clamped to [0.0, 255.0] on the line above; the cast can neither truncate nor lose a sign"
)]
fn clamped_u8(v: f32) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

fn blend(bg: u8, fg: u8, alpha: f32) -> u8 {
    let bg = f32::from(bg);
    let fg = f32::from(fg);
    clamped_u8(bg + (fg - bg) * alpha)
}

fn bounce(t: f32, extent: f32, margin: f32) -> f32 {
    // Triangle wave over `[margin, extent - margin]`.
    let span = (extent - 2.0 * margin).max(1.0);
    let raw = (t * 40.0).abs() % (span * 2.0);
    let folded = if raw < span { raw } else { span * 2.0 - raw };
    margin + folded
}

/// Standard HSV → RGB conversion (h/s/v in, r/g/b out via i/f/p/q/t
/// intermediates) — the names match the textbook algorithm; every cast
/// here is provably in-range from that algorithm's own invariants
/// (`h6 = rem_euclid(1.0) * 6.0` is always in `[0.0, 6.0)`, so `i` is
/// always `0..6` and exact in `f32`; `r`/`g`/`b` are always in `[0.0,
/// 1.0]`), so spelling them out or guarding each cast individually would
/// add noise without adding safety.
#[allow(
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "standard HSV-to-RGB textbook variable names and casts; see doc comment for why each cast is in-range"
)]
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
    (
        clamped_u8(r * 255.0),
        clamped_u8(g * 255.0),
        clamped_u8(b * 255.0),
    )
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
    out: Vec<u8>,
    text: TextPainter,
    header_left: String,
    header_right: String,
}

impl CompositeRenderer {
    fn new(src_w: u32, src_h: u32, latency_ms: u32, throughput_kbps: u64) -> Self {
        // Round to even so the composite encoder is happy (4:2:0
        // encoders reject odd dimensions).
        let header_h = 48u32;
        let footer_h = 44u32;
        let canvas_w = round_even(src_w.saturating_mul(2));
        let canvas_h = round_even(src_h.saturating_add(header_h).saturating_add(footer_h));
        let out_len = (canvas_w as usize)
            .saturating_mul(canvas_h as usize)
            .saturating_mul(4);
        let out = vec![0u8; out_len];
        let header_left = "SOURCE".to_string();
        let header_right =
            format!("STREAMED · {latency_ms}ms latency · {throughput_kbps} kbps throttle");
        Self {
            src_w,
            src_h,
            canvas_w,
            canvas_h,
            header_h,
            out,
            text: TextPainter::new(Family::SansSerif),
            header_left,
            header_right,
        }
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
            if let [r, g, b, a] = chunk {
                *r = 20;
                *g = 22;
                *b = 26;
                *a = 255;
            }
        }
        let src_w = self.src_w as usize;
        let src_h = self.src_h as usize;
        let canvas_stride = (self.canvas_w as usize).saturating_mul(4);
        // Left half — source.
        let y0 = self.header_h as usize;
        for y in 0..src_h {
            blit_row(
                &mut self.out,
                canvas_stride,
                0,
                y.saturating_add(y0),
                src_rgba,
                src_w,
                y,
            );
        }
        // Right half — decoded (or an "awaiting..." placeholder).
        let x0 = src_w;
        if let Some(dec) = decoded_rgba {
            for y in 0..src_h {
                blit_row(
                    &mut self.out,
                    canvas_stride,
                    x0,
                    y.saturating_add(y0),
                    dec,
                    src_w,
                    y,
                );
            }
        } else {
            // Fill with a distinct dark-red so the "waiting on the shim" phase
            // is obvious.
            for y in 0..src_h {
                for x in 0..src_w {
                    if let Some(i) = pixel_offset_usize(
                        x.saturating_add(x0),
                        y.saturating_add(y0),
                        self.canvas_w,
                        self.canvas_h,
                    ) {
                        write_pixel(&mut self.out, i, (40, 20, 20), 1.0);
                    }
                }
            }
        }
        // Headers.
        let canvas = (self.canvas_w, self.canvas_h);
        let half_src_w = u32_to_i32_sat(self.src_w / 2);
        self.text.draw(
            &mut self.out,
            canvas,
            (half_src_w.saturating_sub(40), 8),
            28.0,
            &self.header_left,
            (200, 220, 255),
        );
        let one_and_half_src_w = u32_to_i32_sat(self.src_w.saturating_add(self.src_w / 2));
        self.text.draw(
            &mut self.out,
            canvas,
            (one_and_half_src_w.saturating_sub(200), 8),
            28.0,
            &self.header_right,
            (255, 200, 160),
        );
        // Divider strip between the two panes: two fully-zeroed (including
        // alpha) pixels straddling the boundary — the last source-side
        // column and the first decoded-side column.
        for y in 0..src_h {
            for x in [src_w.saturating_sub(1), src_w] {
                if let Some(i) =
                    pixel_offset_usize(x, y.saturating_add(y0), self.canvas_w, self.canvas_h)
                    && let Some(px) = self.out.get_mut(i..i.saturating_add(4))
                {
                    px.fill(0);
                }
            }
        }
        // Footer with rolling stats.
        let footer_y0 = u32_to_i32_sat(self.header_h.saturating_add(self.src_h)).saturating_add(8);
        self.text.draw(
            &mut self.out,
            canvas,
            (24, footer_y0),
            22.0,
            footer,
            (200, 220, 240),
        );
        &self.out
    }
}

fn round_even(v: u32) -> u32 {
    if v.is_multiple_of(2) {
        v
    } else {
        v.saturating_add(1)
    }
}

/// Lossy `u128 -> f64`, used for the profiler's nanosecond-scale
/// cumulative-duration stats. Display-only: a session would need to run
/// for longer than the age of the universe before this lost precision
/// that mattered to a human reading the report.
#[allow(
    clippy::cast_precision_loss,
    reason = "display-only profiling stat; see doc comment"
)]
fn u128_as_f64(n: u128) -> f64 {
    n as f64
}

/// As [`u128_as_f64`], for the per-span call-count stat (`u64`).
#[allow(
    clippy::cast_precision_loss,
    reason = "display-only profiling stat; see doc comment"
)]
fn u64_as_f64(n: u64) -> f64 {
    n as f64
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
    rows.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.total_ns));
    let wall_ns = total_wall.as_nanos().max(1);
    for (name, stats) in rows {
        let total_ms = u128_as_f64(stats.total_ns) / 1_000_000.0;
        let mean_us = if stats.count == 0 {
            0.0
        } else {
            u128_as_f64(stats.total_ns) / u64_as_f64(stats.count) / 1_000.0
        };
        let pct = 100.0 * u128_as_f64(stats.total_ns) / u128_as_f64(wall_ns);
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

/// Installs the tracing subscriber (env-filter controlled, `trace` by
/// default) with a [`ProfileLayer`] wired in, and returns the shared
/// per-span accumulator so `main` can later hand it to
/// [`write_profile_report`].
fn init_tracing() -> Arc<Mutex<ProfileInner>> {
    let profile_layer: Arc<Mutex<Option<ProfileLayer>>> =
        Arc::new(Mutex::new(Some(ProfileLayer::default())));
    let inner = {
        // `profile_layer` was just constructed with `Some(..)` above and
        // nothing else touches it yet, so `None` here can't happen in
        // practice; `get_or_insert_with` still gives a real fallback
        // (a fresh layer) instead of an `unwrap`.
        let mut slot = profile_layer.lock_recover();
        slot.get_or_insert_with(ProfileLayer::default).inner.clone()
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
    inner
}

/// The shim in the middle of the pipeline. Payload is `Vec<u8>` (one NAL
/// unit) so the throttle sees individual-NAL granularity.
fn build_shim(args: &Args) -> FrameLink<Vec<u8>> {
    FrameLink::with_config(&LinkConfig {
        latency_ms: args.latency_ms,
        jitter_ms: 5,
        throughput_kbps: args.throughput_kbps,
        queue_capacity: 512,
        jitter_seed: 0xC0DE,
        // Sized to a couple of frames' budget; keyframes still fit through
        // via the oversize-drain rule.
        token_bucket_bytes: (args.throughput_kbps.max(1).saturating_mul(1000) / 8 / 2)
            .max(64 * 1024),
    })
}

/// Bundles every long-lived per-frame resource so `main`'s loop body can
/// be a short call into [`Pipeline::process_frame`] instead of inlining
/// the whole encode → shim → decode → compose → mux chain.
struct Pipeline {
    source_renderer: SourceRenderer,
    send_encoder: VideoEncoder,
    shim: FrameLink<Vec<u8>>,
    recv_decoder: VideoDecoder,
    composite: CompositeRenderer,
    composite_encoder: VideoEncoder,
    muxer: Mp4Recording,
    started: Instant,
    sent_frames: u64,
    decoded_frames: u64,
    dropped_frames: u64,
    // Rolling decoded-frame cache — the shim can hold a decoded frame "in
    // flight" for tens of ms, so we keep the last known-decoded to show
    // while waiting for the next real one.
    last_decoded: Option<Vec<u8>>,
}

impl Pipeline {
    /// Builds every encoder/decoder/shim/renderer and the MP4 muxer.
    ///
    /// # Errors
    ///
    /// Returns `Err` if a font can't be found or an encoder fails to
    /// initialise.
    fn new(args: &Args) -> Result<Self, String> {
        // The codec a share negotiates on this machine with no viewers yet.
        let codec = media::realtime::negotiate(&media::realtime::local_capabilities(), &[])
            .map(|n| n.codec)
            .ok_or_else(|| "this machine can encode no real-time codec".to_string())?;
        // Production encoder — same crate the compositor uses to publish.
        // Sized to 4 Mbps so 720p content has budget for detail; `FrameLink`
        // then throttles down to `--throughput-kbps`.
        let send_encoder = VideoEncoder::with_options(
            codec,
            args.width,
            args.height,
            args.fps,
            4_000_000,
            args.fps,
            true,
        )
        .map_err(|e| format!("send encoder ({}): {e:?}", codec.wire_label()))?;
        // Production decoder — same crate PeerView constructs.
        let mut recv_decoder = VideoDecoder::new(codec, false);
        recv_decoder.set_display_size(Some((args.width, args.height)));
        eprintln!("flatpak-share-demo: streaming {}", codec.wire_label());
        let shim = build_shim(args);

        let source_renderer = SourceRenderer::new(args.width, args.height);
        let composite = CompositeRenderer::new(
            args.width,
            args.height,
            args.latency_ms,
            args.throughput_kbps,
        );
        let (canvas_w, canvas_h) = composite.dimensions();

        // Encoder for the side-by-side canvas: bigger canvas → bigger
        // bitrate (composite is 2×W×H, so 2× the send-side budget).
        let composite_encoder =
            recording_encoder(canvas_w, canvas_h, args.fps, 8_000_000, args.fps, false)?;
        let out_codec = composite_encoder.codec();
        if let Some(parent) = args.out.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let muxer = Mp4Recording::new(
            args.out.clone(),
            out_codec,
            canvas_w,
            canvas_h,
            args.fps,
            Vec::new(),
        );

        // Source video path (--source-video) — decode to raw
        // RGBA into a channel. `--source pattern` is the default; video-in
        // is a future extension, warned and falls back if requested.
        if let Source::Video(path) = &args.source {
            eprintln!(
                "warning: --source-video ({}) not yet wired to a raw-RGBA decoder in this demo; \
                 falling back to the pattern generator.",
                path.display()
            );
        }

        let pipeline = Self {
            source_renderer,
            send_encoder,
            shim,
            recv_decoder,
            composite,
            composite_encoder,
            muxer,
            started: Instant::now(),
            sent_frames: 0,
            decoded_frames: 0,
            dropped_frames: 0,
            last_decoded: None,
        };
        Ok(pipeline)
    }

    /// Encode, shim, (maybe) decode, compose and mux one source frame at
    /// `frame_index`/`t_ms`. Returns the wall-time this call actually
    /// took, for the caller's real-time pacing.
    ///
    /// # Errors
    ///
    /// Returns `Err` if either encoder or the muxer fails.
    fn process_frame(&mut self, t_ms: u64, frame_index: u32) -> Result<Duration, String> {
        let src = self.source_renderer.render(t_ms, frame_index);

        // Encode the source with the production encoder — same code
        // path SharedFlatpakSession uses.
        let units = self
            .send_encoder
            .encode(src)
            .map_err(|e| format!("send_encoder.encode: {e:?}"))?;
        self.sent_frames = self.sent_frames.saturating_add(1);

        // Push each frame's full NAL blob as a single shim payload — the
        // throttle sees per-frame granularity, matching how a real streaming
        // path bundles keyframe SPS+PPS+IDR together. Splitting into
        // individual NALs would let the throttle pace within a frame,
        // understating real burst behaviour.
        let now = Instant::now();
        for unit in units {
            let size = unit.data.len() as u64;
            self.shim.push(now, size, unit.data);
        }

        // Drain everything the shim has decided is ready, feeding each
        // frame's access unit into the decoder in emitted order. Units
        // before a keyframe decode to `None` — expected; the counter only
        // advances on Some(frame).
        for nal_blob in self.shim.drain_ready(now) {
            let span = span!(
                Level::TRACE,
                "recv.decoder.decode",
                nal_bytes = nal_blob.len()
            );
            let _entered = span.enter();
            if let Some(frame) = self.recv_decoder.decode(&nal_blob) {
                // Post-HDR decoder now returns typed DecodedPixels; the
                // demo only handles SDR RGBA, so extract those bytes.
                if let tilo_callstream::surface_decoder::DecodedPixels::Rgba8(rgba) = frame.pixels {
                    self.last_decoded = Some(rgba);
                }
                self.decoded_frames = self.decoded_frames.saturating_add(1);
            }
        }

        // Update running stats.
        let shim_stats = self.shim.stats();
        self.dropped_frames = shim_stats.frames_dropped;
        let footer = format_footer(
            &shim_stats,
            self.started.elapsed(),
            self.sent_frames,
            self.decoded_frames,
            self.dropped_frames,
        );

        // Compose the side-by-side canvas.
        let composite_rgba = self
            .composite
            .compose(src, self.last_decoded.as_deref(), &footer);

        // Encode the composite via the second encoder and mux it.
        let composite_units = self
            .composite_encoder
            .encode(composite_rgba)
            .map_err(|e| format!("composite_encoder.encode: {e:?}"))?;
        for unit in composite_units {
            self.muxer.write_access_unit(&unit.data)?;
        }

        Ok(now.elapsed())
    }

    /// Drain anything still in the shim so late-arriving frames count
    /// toward `decoded_frames` in the final summary (they're not composed
    /// or muxed — the composite tail already closed by the time this
    /// runs). Given a "far future" instant so everything queued is
    /// deliverable.
    fn flush(&mut self) {
        // `checked_add` + a same-instant fallback: this can only fail if
        // `Instant` arithmetic is already at its representable limit, in
        // which case flushing "now" instead of "one hour from now" is a
        // harmless fallback, not a panic.
        let flush_now = Instant::now()
            .checked_add(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        for nal_blob in self.shim.drain_ready(flush_now) {
            if let Some(frame) = self.recv_decoder.decode(&nal_blob) {
                if let tilo_callstream::surface_decoder::DecodedPixels::Rgba8(rgba) = frame.pixels {
                    self.last_decoded = Some(rgba);
                }
                self.decoded_frames = self.decoded_frames.saturating_add(1);
            }
        }
    }
}

fn main() -> Result<(), String> {
    // Install the tracing subscriber early so every span from the
    // encoder/decoder/shim is captured.
    let inner = init_tracing();
    let Some(args) = parse_args().inspect_err(|_e| {
        print_usage();
    })?
    else {
        // `-h`/`--help`: usage already printed, exit 0.
        return Ok(());
    };
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

    let total_frames = args.duration_secs.saturating_mul(args.fps);
    // `args.fps > 0` is validated in `parse_args`, but that guarantee
    // doesn't cross the function boundary for clippy — fall back to a 1 µs
    // interval (fastest possible pacing) rather than a division panic.
    let frame_interval =
        Duration::from_micros(1_000_000u64.checked_div(u64::from(args.fps)).unwrap_or(1));

    let mut pipeline = Pipeline::new(&args)?;

    for i in 0..total_frames {
        // Same "validated in `parse_args` but not visible to clippy here" case
        // as `frame_interval` above — fall back to 0 rather than a division panic.
        let t_ms = u64::from(i)
            .saturating_mul(1000)
            .checked_div(u64::from(args.fps))
            .unwrap_or(0);
        let elapsed_this_frame = pipeline.process_frame(t_ms, i)?;

        // Real-time-ish: rest of the frame interval budget goes to sleep so
        // the demo's wall clock reflects the streaming rate. Skipped on the
        // last frame.
        if elapsed_this_frame < frame_interval && i.saturating_add(1) < total_frames {
            std::thread::sleep(
                frame_interval
                    .checked_sub(elapsed_this_frame)
                    .unwrap_or(Duration::ZERO),
            );
        }
    }

    // Drain anything still in the shim so late-arriving frames land in
    // the final counters.
    pipeline.flush();
    let sent_frames = pipeline.sent_frames;
    let decoded_frames = pipeline.decoded_frames;
    let dropped_frames = pipeline.dropped_frames;
    let shim_stats = pipeline.shim.stats();
    let started = pipeline.started;

    // Mux the composite frames still in the encoder, then finish the MP4
    // (writes its trailer).
    let tail = pipeline
        .composite_encoder
        .finish()
        .map_err(|e| format!("composite_encoder.finish: {e:?}"))?;
    for unit in tail {
        pipeline.muxer.write_access_unit(&unit.data)?;
    }
    pipeline.muxer.finish()?;

    let total_wall = started.elapsed();
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
