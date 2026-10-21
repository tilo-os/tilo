//! Display-free `Backend` implementation: paints into an offscreen wgpu
//! texture (via [`ui::offscreen::OffscreenRenderer`]) instead of a
//! window. The compositor's chrome doesn't care which backend it paints
//! into, so this is the third path alongside [`super::WinitBackend`]
//! (host-window) and [`super::DrmBackend`] (TTY) — needed because
//! neither of those works in CI, in a distrobox without a display, or
//! under SSH without X-forwarding, and `TILO_TEST_CONTROL` needs a
//! backend to boot in order to drive the live binary headlessly.
//!
//! No `winit::*` imports (the entire point is to be display-free), no
//! real input devices, and no wayland-server surface bind change —
//! `native::run` still binds `wayland-N` and forwards client traffic;
//! the headless flag only affects the paint target.

use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::peniko::Color;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::{Backend, BackendEvent, Output, OutputId, combined_geometry, layout};
use crate::shell::present::{DisplayTiming, OwedRetirement, Presenter};
use crate::shell::render::paint::{
    OutputSignal, OutputTarget, OutputView, SceneDamage, VelloDrmBridge,
};
use vello::wgpu;

// Match the existing host-window fallback for blank scenes in both render paths.
const FALLBACK_BG: Color = Color::from_rgb8(0x1A, 0x1B, 0x26);

/// Env-var that overrides the synthetic output's dimensions. Format is
/// `WxH` (e.g. `1280x720`). Anything malformed or absent falls back to
/// the `shots --mode offscreen` default. Mirrors the env-var-driven
/// shape of `TILO_TEST_CONTROL` / `TILO_TEST_STATE_DUMP` so the
/// whole headless surface stays opt-in without CLI bloat.
pub const HEADLESS_SIZE_ENV: &str = "TILO_HEADLESS_SIZE";

/// Env-var that replaces the one synthetic output with a real multi-output
/// arrangement: either a named fixture from [`OUTPUT_FIXTURES`] or an
/// output spec (see [`parse_output_spec`]). Mutually exclusive with
/// [`HEADLESS_SIZE_ENV`], which only ever describes one output.
pub const HEADLESS_OUTPUTS_ENV: &str = "TILO_HEADLESS_OUTPUTS";

/// Env-var that starts a headless session watched (`1`): every frame is
/// rasterized into the offscreen target, as a viewer streaming it needs.
/// Unset, frames are laid out and handed to clients but not rasterized
/// until the control socket's `watch` attaches a viewer; `snapshot`
/// renders on demand either way.
pub const HEADLESS_WATCH_ENV: &str = "TILO_HEADLESS_WATCH";

/// Env-var that gives the headless session a render thread and a simulated
/// display: its value is the panel's refresh rate in hertz (`60`, `59.94`).
///
/// Frames then render on the real GPU into an offscreen target, wait for the
/// previous flip to retire and queue their own, which retires on the next
/// vblank of a fixed grid, the way the DRM presenter behaves on a panel. With
/// it set the loop is paced and measured as on hardware (`KEY-LATENCY`,
/// `FRAME-PACING`), minus the panel: `scripts/perf/headless_key_latency.py`
/// uses it. Unset, headless presents synchronously and only when watched.
pub const HEADLESS_VBLANK_HZ_ENV: &str = "TILO_HEADLESS_VBLANK_HZ";

/// Named output arrangements, shared by headless sessions, `shots
/// --outputs` and the integration tests, so a layout bug reproduces with a
/// name rather than a hand-typed spec.
///
/// Positions are logical: each output is its mode over its scale wide.
pub const OUTPUT_FIXTURES: &[(&str, &str)] = &[
    // A HiDPI laptop panel with a high-refresh desktop monitor to its right.
    ("docked-laptop", "2560x1600@2~60,2560x1440@1~144"),
    // A 2x laptop panel beside a 1080p monitor taller than it, logically:
    // the desktop's bottom edge and centre are both on the monitor.
    ("laptop-beside-1080p", "2944x1840@2,1920x1080@1"),
    // A monitor with the laptop centred underneath it.
    ("vertical-stack", "2560x1440@1,1920x1200@1.25+512+1440"),
    // A 1080p panel rotated to portrait beside a 2x 4K monitor.
    ("portrait-plus-4k", "1920x1080@1r90,3840x2160@2"),
    // Mixed scales whose top edges do not line up.
    ("mixed-scale-offset", "2560x1600@2,1920x1080@1+1280+200"),
];

/// The outputs of a named fixture from [`OUTPUT_FIXTURES`].
pub fn output_fixture(name: &str) -> Option<Vec<Output>> {
    OUTPUT_FIXTURES
        .iter()
        .find(|(fixture, _)| *fixture == name)
        .and_then(|(_, spec)| parse_output_spec(spec).ok())
}

/// Resolve a [`HEADLESS_OUTPUTS_ENV`] value: a fixture name, else a spec.
///
/// # Errors
///
/// Returns a message naming the fixtures and the spec grammar when the
/// value is neither.
pub fn resolve_output_arrangement(value: &str) -> Result<Vec<Output>, String> {
    let value = value.trim();
    if let Some(outputs) = output_fixture(value) {
        return Ok(outputs);
    }
    parse_output_spec(value).map_err(|error| {
        let fixtures: Vec<&str> = OUTPUT_FIXTURES.iter().map(|(name, _)| *name).collect();
        format!(
            "{error}; expected a fixture ({}) or a spec like \
             `2560x1600@2,1920x1080@1+1280+0,1920x1080@1r90` \
             (WxH mode, then optional @SCALE, ~HZ, rDEGREES, +X+Y)",
            fixtures.join(", ")
        )
    })
}

/// Parse a comma-separated output spec into outputs `OutputId(0..n)`.
///
/// Each entry is `WxH` — the mode the connector drives, before rotation —
/// followed by optional suffixes in any order:
///
/// - `@SCALE` — UI scale factor (default `1`)
/// - `~HZ` — refresh rate in hertz, fractional allowed (default: none, so
///   `wl_output` advertises the compositor's pacing rate)
/// - `rDEGREES` — rotation, one of `0`, `90`, `180`, `270`
/// - `+X+Y` — position in the logical layout, where an output is its mode
///   over its scale wide (a 2560-pixel 2x panel spans 1280); either sign may
///   be `-`. An entry without one sits to the right of everything before it.
///
/// The arrangement is translated so its top-left is the origin, as the DRM
/// backend does, outputs may not overlap, and scene positions are derived
/// with [`layout::scene_positions`] exactly as the DRM backend derives them.
///
/// # Errors
///
/// Returns a message naming the offending entry.
pub fn parse_output_spec(spec: &str) -> Result<Vec<Output>, String> {
    let mut outputs: Vec<Output> = Vec::new();
    for (index, entry) in spec.split(',').enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(format!("output {index}: empty entry"));
        }
        let parsed =
            parse_output_entry(entry).map_err(|e| format!("output {index} `{entry}`: {e}"))?;
        let id = OutputId(u32::try_from(index).map_err(|_| "too many outputs".to_string())?);
        let mut output = Output::new(id, parsed.mode, parsed.scale);
        output.transform = parsed.transform;
        // `size` is the arranged extent; `physical_size` swaps it back.
        output.size = output.physical_size();
        output.logical_position = parsed.position.unwrap_or_else(|| (right_edge(&outputs), 0));
        output.refresh_mhz = parsed.refresh_mhz;
        outputs.push(output);
    }
    let min_x = outputs
        .iter()
        .map(|o| o.logical_position.0)
        .min()
        .unwrap_or(0);
    let min_y = outputs
        .iter()
        .map(|o| o.logical_position.1)
        .min()
        .unwrap_or(0);
    for o in &mut outputs {
        o.logical_position = (
            o.logical_position.0.saturating_sub(min_x),
            o.logical_position.1.saturating_sub(min_y),
        );
    }
    for (i, a) in outputs.iter().enumerate() {
        for b in outputs.iter().skip(i.saturating_add(1)) {
            if rects_overlap(a, b) {
                return Err(format!(
                    "outputs {} and {} overlap; give one a +X+Y clear of the other",
                    a.id.0, b.id.0
                ));
            }
        }
    }
    let scene = layout::scene_positions(
        &outputs
            .iter()
            .map(|o| (o.logical_position, o.logical_size(), o.size))
            .collect::<Vec<_>>(),
    );
    for (o, position) in outputs.iter_mut().zip(scene) {
        o.position = position;
    }
    Ok(outputs)
}

/// The logical right edge of everything placed so far: where an entry without a
/// position goes.
fn right_edge(outputs: &[Output]) -> i32 {
    let right = outputs
        .iter()
        .map(|o| i64::from(o.logical_position.0).saturating_add(i64::from(o.logical_size().0)))
        .max()
        .unwrap_or(0);
    i32::try_from(right).unwrap_or(i32::MAX)
}

struct OutputEntry {
    mode: (u32, u32),
    scale: f32,
    refresh_mhz: Option<u32>,
    transform: settings_store::hardware::OutputTransform,
    position: Option<(i32, i32)>,
}

const SUFFIX_MARKERS: [char; 5] = ['@', '~', 'r', '+', '-'];

/// Split `body` at the next suffix marker: `(value, rest)`.
fn take_value(body: &str) -> (&str, &str) {
    body.split_at(body.find(SUFFIX_MARKERS).unwrap_or(body.len()))
}

fn signed(marker: char, value: &str) -> Result<i32, String> {
    let magnitude: i32 = value.parse().map_err(|_| "position must be +X+Y")?;
    Ok(if marker == '-' {
        magnitude.saturating_neg()
    } else {
        magnitude
    })
}

fn parse_output_entry(entry: &str) -> Result<OutputEntry, String> {
    let (mode, mut rest) = take_value(entry);
    let mode = parse_headless_size(Some(mode)).ok_or("mode must be WxH with both sides > 0")?;
    let mut parsed = OutputEntry {
        mode,
        scale: 1.0,
        refresh_mhz: None,
        transform: settings_store::hardware::OutputTransform::Normal,
        position: None,
    };
    while let Some(marker) = rest.chars().next() {
        let (value, tail) = take_value(rest.get(marker.len_utf8()..).unwrap_or_default());
        rest = tail;
        match marker {
            '@' => {
                parsed.scale = value
                    .parse::<f32>()
                    .ok()
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .ok_or("@SCALE must be a positive number")?;
            }
            '~' => {
                let hz = value
                    .parse::<f64>()
                    .ok()
                    .filter(|hz| hz.is_finite() && *hz > 0.0 && *hz < 1000.0)
                    .ok_or("~HZ must be a positive refresh rate below 1000")?;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "bounded to (0, 1000) Hz above, so the millihertz fit in u32"
                )]
                let mhz = (hz * 1000.0).round() as u32;
                parsed.refresh_mhz = Some(mhz);
            }
            'r' => {
                use settings_store::hardware::OutputTransform as T;
                parsed.transform = match value {
                    "0" => T::Normal,
                    "90" => T::Rotate90,
                    "180" => T::Rotate180,
                    "270" => T::Rotate270,
                    _ => return Err("rotation must be r0, r90, r180 or r270".into()),
                };
            }
            '+' | '-' => {
                let x = signed(marker, value)?;
                let y_marker = rest
                    .chars()
                    .next()
                    .filter(|c| matches!(c, '+' | '-'))
                    .ok_or("position must be +X+Y")?;
                let (y, tail) = take_value(rest.get(1..).unwrap_or_default());
                rest = tail;
                parsed.position = Some((x, signed(y_marker, y)?));
            }
            _ => return Err(format!("unexpected `{marker}`")),
        }
    }
    Ok(parsed)
}

fn rects_overlap(first: &Output, second: &Output) -> bool {
    // `[start, end)` of an output's logical rect along one axis.
    let span = |start: i32, len: u32| {
        let start = i64::from(start);
        (start, start.saturating_add(i64::from(len)))
    };
    let axes_overlap = |axis: fn(&Output) -> (i32, u32)| {
        let (first_start, first_end) = {
            let (start, len) = axis(first);
            span(start, len)
        };
        let (second_start, second_end) = {
            let (start, len) = axis(second);
            span(start, len)
        };
        first_start < second_end && second_start < first_end
    };
    axes_overlap(|o| (o.logical_position.0, o.logical_size().0))
        && axes_overlap(|o| (o.logical_position.1, o.logical_size().1))
}

/// Default size when `TILO_HEADLESS_SIZE` is unset. Matches the
/// `bin/shots` default so a fresh `--backend headless` launch produces
/// frames comparable to a `cargo run --bin shots` screenshot.
pub const DEFAULT_WIDTH: u32 = 1600;
pub const DEFAULT_HEIGHT: u32 = 900;

#[derive(Clone, Copy)]
struct CompletedFrame {
    revision: u64,
    size: (u32, u32),
    validated: bool,
}

/// Wgpu-offscreen [`Backend`] for display-free runs.
///
/// While watched ([`HEADLESS_WATCH_ENV`], [`Backend::set_watched`]), each
/// [`Backend::submit_frame`] call renders the supplied scene into the
/// offscreen texture and caches the RGBA bytes on `self.last_frame`.
/// Unwatched, it renders nothing. See the module-level doc comment for
/// design intent.
pub struct HeadlessBackend {
    renderer: OffscreenRenderer,
    outputs: Vec<Output>,
    events: Vec<BackendEvent>,
    /// Latest painted RGBA frame, kept so `test_control`'s `snapshot`
    /// command can write a PNG without re-rendering.
    last_frame: Vec<u8>,
    /// Version of a synchronous image; only bounded validation permits reuse.
    completed_frame: Option<CompletedFrame>,
    /// Whether the one startup `Redraw` has been handed over yet.
    emitted_first_redraw: bool,
    /// Whether a viewer reads the rasterized frames. Nobody does by
    /// default, and rasterizing is most of a frame's cost on the software
    /// renderers hosted machines have.
    watched: bool,
    /// The simulated panel's frame interval ([`HEADLESS_VBLANK_HZ_ENV`]).
    vblank_interval: Option<Duration>,
}

impl HeadlessBackend {
    /// Build a headless backend at the configured size. The
    /// [`OffscreenRenderer`] eagerly acquires a wgpu adapter — this
    /// returns `Err` (rather than panics) on machines without llvmpipe
    /// or another suitable adapter so the binary entry point can
    /// surface a friendly error instead of dying at the panic site.
    ///
    /// # Errors
    ///
    /// Returns an error if `width` or `height` is `0`, or if
    /// [`OffscreenRenderer::try_new`] fails to acquire a wgpu adapter
    /// (no llvmpipe or other suitable software/hardware adapter available).
    pub fn new(width: u32, height: u32) -> Result<Self, Box<dyn std::error::Error>> {
        if width == 0 || height == 0 {
            return Err(format!(
                "HeadlessBackend::new: invalid size {width}x{height}: dimensions must be > 0"
            )
            .into());
        }
        Self::with_outputs(vec![Output::new(OutputId(0), (width, height), 1.0)])
    }

    /// Build a headless backend driving `outputs` — typically from
    /// [`resolve_output_arrangement`]. The offscreen texture spans their
    /// combined extent, exactly as the DRM presenter's paint buffer does.
    ///
    /// # Errors
    ///
    /// Returns an error if `outputs` is empty or spans no area, or if no
    /// wgpu adapter is available.
    pub fn with_outputs(outputs: Vec<Output>) -> Result<Self, Box<dyn std::error::Error>> {
        let (width, height) = combined_geometry(&outputs);
        if outputs.is_empty() || width == 0 || height == 0 {
            return Err("HeadlessBackend: the output arrangement spans no area".into());
        }
        // `try_new` is the fallible constructor; `new` panics, and a
        // `catch_unwind` around it would be inert in release builds (a
        // panic outside a `tilo_rt::panic_boundary` aborts the process).
        // Going through the `Result` is what lets
        // the headless backend honour the rest of the `Backend::new`
        // contract — return an error, don't take `main` down with us.
        let renderer = OffscreenRenderer::try_new(width, height).map_err(|e| {
            format!("HeadlessBackend::new: OffscreenRenderer init failed at {width}x{height}: {e}")
        })?;
        #[cfg(target_os = "linux")]
        if let Some(adapter) = renderer.adapter_info() {
            crate::shader_seed::observe_device(adapter);
        }
        Ok(Self {
            renderer,
            outputs,
            events: Vec::new(),
            last_frame: Vec::new(),
            completed_frame: None,
            emitted_first_redraw: false,
            watched: std::env::var(HEADLESS_WATCH_ENV).is_ok_and(|value| value.trim() == "1"),
            vblank_interval: std::env::var(HEADLESS_VBLANK_HZ_ENV)
                .ok()
                .and_then(|raw| super::super::pacing::parse_refresh_override(Some(&raw))),
        })
    }

    /// Read-only access to the most recently rendered frame, RGBA8888,
    /// row-major. Empty until a frame is submitted while watched, and
    /// again whenever a frame arrives while nobody watches, so it never
    /// holds pixels older than the scene.
    pub fn last_frame(&self) -> &[u8] {
        &self.last_frame
    }

    /// Frame size as `(width, height)`: the combined extent of the outputs
    /// it was built with. The headless backend never resizes
    /// (no window, no display events to drive a resize off of).
    pub fn frame_size(&self) -> (u32, u32) {
        (self.renderer.width, self.renderer.height)
    }
}

/// Parse `TILO_HEADLESS_SIZE` if present. Returns `None` on
/// missing/empty/malformed values so `native::run` falls through to
/// the [`DEFAULT_WIDTH`] / [`DEFAULT_HEIGHT`] constants without an
/// extra `eprintln!` flush.
///
/// Pure helper so unit tests can drive the parser with fixture values
/// without poking at the process environment.
pub fn parse_headless_size(value: Option<&str>) -> Option<(u32, u32)> {
    let raw = value?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Accept either `x` or `X` as separator — case-insensitive matches
    // the `1600x900` convention used everywhere else in the repo while
    // still tolerating shell weirdness.
    let (w, h) = raw.split_once(['x', 'X'])?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// Resolve the initial headless size from the env var, falling back to
/// the [`DEFAULT_WIDTH`] / [`DEFAULT_HEIGHT`] constants. Used by
/// `native::run` so the dispatch site stays a single expression.
pub fn resolve_initial_size() -> (u32, u32) {
    let raw = std::env::var(HEADLESS_SIZE_ENV).ok();
    parse_headless_size(raw.as_deref()).unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT))
}

/// Resolve the headless outputs from the environment: the
/// [`HEADLESS_OUTPUTS_ENV`] arrangement when set, else one output at
/// [`resolve_initial_size`].
///
/// # Errors
///
/// Returns a message when the arrangement does not parse, or when both
/// env-vars are set — silently preferring one would hand a harness a
/// desktop other than the one it asked for.
pub fn resolve_initial_outputs() -> Result<Vec<Output>, String> {
    let Some(arrangement) = std::env::var(HEADLESS_OUTPUTS_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        let (w, h) = resolve_initial_size();
        return Ok(vec![Output::new(OutputId(0), (w, h), 1.0)]);
    };
    if std::env::var_os(HEADLESS_SIZE_ENV).is_some() {
        return Err(format!(
            "{HEADLESS_OUTPUTS_ENV} and {HEADLESS_SIZE_ENV} are both set; \
             {HEADLESS_OUTPUTS_ENV} describes every output, so unset {HEADLESS_SIZE_ENV}"
        ));
    }
    resolve_output_arrangement(&arrangement).map_err(|e| format!("{HEADLESS_OUTPUTS_ENV}: {e}"))
}

impl Backend for HeadlessBackend {
    fn supports_retained_scene_damage(&self) -> bool {
        self.watched || self.vblank_interval.is_some()
    }

    fn refresh_interval(&self) -> Option<Duration> {
        self.vblank_interval
    }

    fn take_presenter(&mut self) -> Option<Box<dyn Presenter>> {
        let interval = self.vblank_interval?;
        let (width, height) = self.frame_size();
        SimulatedVblankPresenter::new(width, height, interval)
            .map(|presenter| Box::new(presenter) as Box<dyn Presenter>)
    }

    fn submit_frame(&mut self, scene: &Scene, _output: OutputId) {
        self.completed_frame = None;
        // `OffscreenRenderer::render` is a wgpu round-trip per call; unlike
        // `WinitBackend` it doesn't need a `present()` step since there is
        // no swapchain. We cache the RGBA bytes so test-control's
        // `snapshot` command can write a PNG without re-painting.
        //
        // The clear colour matches `WinitBackend`'s `FALLBACK_BG` and
        // `Theme::dark().tokens.colors.surfaces.base`, so a headless PNG and a winit host window
        // clear to identical pixels for a blank scene.
        if !self.watched {
            self.last_frame = Vec::new();
            return;
        }
        self.last_frame = self.renderer.render(scene, FALLBACK_BG);
        #[cfg(target_os = "linux")]
        crate::shader_seed::after_frame();
    }

    fn submit_frame_with_damage(
        &mut self,
        frame: &crate::shell::frame::FramePaint,
        output: OutputId,
    ) {
        let retainable = self.watched
            && frame.revision != 0
            && frame.backdrop.is_none()
            && frame.hdr_layers.is_empty();
        if retainable
            && frame.damage_regions.as_ref().is_some_and(Vec::is_empty)
            && let Some(completed) = self.completed_frame
            && Some(completed.revision) == frame.previous_revision
            && completed.size == self.frame_size()
        {
            if !completed.validated {
                // A first ordinary readback may have overflowed or warmed an
                // atlas. Never freeze it: establish two clean submissions with
                // the existing bounded screenshot validator before reusing it.
                self.last_frame = self.renderer.render_resilient(&frame.scene, FALLBACK_BG);
                #[cfg(target_os = "linux")]
                crate::shader_seed::after_frame();
            }
            self.completed_frame = Some(CompletedFrame {
                revision: frame.revision,
                size: completed.size,
                validated: true,
            });
            return;
        }
        // Unproven or material-dependent frames keep the original dense path.
        self.renderer.set_backdrop(frame.backdrop.clone());
        self.submit_frame(&frame.scene, output);
        self.renderer.set_backdrop(None);
        if retainable {
            self.completed_frame = Some(CompletedFrame {
                revision: frame.revision,
                size: self.frame_size(),
                validated: false,
            });
        }
    }

    fn needs_idle_poll(&self) -> bool {
        false
    }

    fn set_watched(&mut self, watched: bool) -> bool {
        let attached = watched && !self.watched;
        if watched != self.watched {
            self.completed_frame = None;
        }
        self.watched = watched;
        attached
    }

    fn watched(&self) -> bool {
        self.watched
    }

    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent)) {
        // The headless backend has no input devices and no display events,
        // so it emits one `BackendEvent::Redraw` at startup to get the
        // first frame on screen, and nothing after that: the main loop
        // only paints when something changed, so a synthetic event every
        // tick would mean something always has, running headless at a
        // permanent 60 fps against an idle desktop. Emitting nothing
        // after startup lets the loop's own pacing and keepalive drive it,
        // exactly as under winit.
        if !self.emitted_first_redraw {
            self.emitted_first_redraw = true;
            self.events.push(BackendEvent::Redraw);
        }
        for ev in self.events.drain(..) {
            callback(ev);
        }
    }

    fn outputs(&self) -> &[Output] {
        &self.outputs
    }
}

/// A flip the simulated panel has been asked for.
struct QueuedFlip {
    queued_at: Instant,
    vblank: Instant,
}

/// Scanout buffers the simulated panel flips between, as the DRM presenter's
/// direct targets: one shown, one queued, one rendered.
const SIMULATED_SCANOUT_SLOTS: usize = 3;

/// The longest one simulated frame may take on the GPU before its render is
/// reported failed, as the DRM presenter's direct frame deadline.
const SIMULATED_FRAME_DEADLINE: Duration = Duration::from_secs(1);

/// Renders on the real GPU and shows frames on a simulated vblank grid.
///
/// It behaves as the DRM presenter does on one output that renders straight
/// into its scanout buffers: the same renderer ([`VelloDrmBridge`]) draws the
/// frame into its retained surface, damage-limited, with client buffers drawn
/// from their GPU textures, and the final pass copies it into a scanout-sized
/// buffer. The frame renders while the previous flip waits for its vblank,
/// its own flip is queued once that one retired (a CRTC takes one flip at a
/// time), and a queued flip retires on the first vblank after it was queued.
/// Nothing is read back.
struct SimulatedVblankPresenter {
    bridge: VelloDrmBridge,
    view: OutputView,
    slots: Vec<(wgpu::Texture, wgpu::TextureView)>,
    next_slot: usize,
    interval: Duration,
    /// Any vblank of the grid; the rest follow at whole intervals.
    epoch: Instant,
    owed: VecDeque<QueuedFlip>,
    /// Flips that retired while a later frame waited to queue its own.
    ready: Vec<OwedRetirement>,
    /// The frame just presented queued a flip.
    queued_last: bool,
    /// How long the last frame took to render, GPU wait included.
    last_render: Duration,
    /// What the last frame repainted.
    last_painted: PaintedArea,
    /// The client frame the simulated display scans out, when it shows one
    /// instead of a composited frame.
    scanned_out: Option<u64>,
    /// `wl_shm` bytes uploaded to client-frame textures as of the last frame.
    shm_uploaded: u64,
}

/// What one frame repainted and what it cost.
#[derive(Clone, Copy, Default)]
struct PaintedArea {
    /// Pixels the frame's damage covers: the whole target for a dense frame,
    /// none for one kept unchanged.
    damage: u64,
    /// Pixels the sparse render wrote; the whole target for a dense frame.
    raster: u64,
    /// The frame rendered the whole target.
    dense: bool,
    /// The display showed a client's own buffer: nothing was rendered.
    scanout: bool,
    /// Encoding, uploads and submission on the CPU, and the wait for the GPU
    /// to finish the render and the copy into the scanout buffer.
    submit_us: u128,
    settle_us: u128,
}

impl SimulatedVblankPresenter {
    fn new(width: u32, height: u32, interval: Duration) -> Option<Self> {
        let mut bridge = match VelloDrmBridge::new(width, height) {
            Ok(bridge) => bridge,
            Err(error) => {
                tracing::warn!(%error, "headless: no renderer for the simulated display");
                return None;
            }
        };
        bridge.set_phase_timing_enabled(true);
        let slots = {
            let (device, _) = bridge.gpu();
            (0..SIMULATED_SCANOUT_SLOTS)
                .map(|_| {
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("simulated-scanout-buffer"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Bgra8Unorm,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    });
                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    (texture, view)
                })
                .collect()
        };
        Some(Self {
            bridge,
            view: OutputView::at(OutputId(0), (0, 0), (width, height)),
            slots,
            next_slot: 0,
            interval,
            epoch: Instant::now(),
            owed: VecDeque::new(),
            ready: Vec::new(),
            queued_last: false,
            last_render: Duration::ZERO,
            last_painted: PaintedArea::default(),
            scanned_out: None,
            shm_uploaded: crate::shell::gpu_frames::shm_upload_bytes(),
        })
    }

    fn target_pixels(&self) -> u64 {
        let (width, height) = self.view.target_size();
        u64::from(width).saturating_mul(u64::from(height))
    }

    /// The first vblank strictly after `at`.
    fn vblank_after(&self, at: Instant) -> Instant {
        let since = at.saturating_duration_since(self.epoch).as_nanos();
        let interval = self.interval.as_nanos().max(1);
        let ticks = since.checked_div(interval).unwrap_or(0).saturating_add(1);
        let nanos = interval.saturating_mul(ticks);
        let offset = Duration::new(
            u64::try_from(nanos / 1_000_000_000).unwrap_or(u64::MAX),
            u32::try_from(nanos % 1_000_000_000).unwrap_or(0),
        );
        self.epoch.checked_add(offset).unwrap_or(at)
    }

    fn retirement(flip: &QueuedFlip) -> OwedRetirement {
        OwedRetirement {
            physical: None,
            display: DisplayTiming {
                vblank_paced: true,
                vblank: Some(flip.vblank),
                flip_queued_at: Some(flip.queued_at),
            },
        }
    }

    /// Sleep to `until` unless it has passed.
    fn wait_until(until: Instant) {
        let remaining = until.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
    }

    /// Render `frame` into the next scanout buffer, as the DRM presenter's
    /// direct path does; `false` when it kept the image already shown.
    fn render(&mut self, frame: &crate::shell::frame::FramePaint) -> bool {
        let damage = (frame.revision != 0).then_some(SceneDamage {
            revision: frame.revision,
            previous_revision: frame.previous_revision,
            regions: frame.damage_regions.as_deref(),
        });
        self.bridge.set_backdrop(frame.backdrop.clone());
        self.bridge.stage_scroll_copies(
            damage.map(|damage| damage.revision),
            frame.scroll_copies.as_ref(),
        );
        if let Some(shown) = self.scan_out(frame, damage) {
            return shown;
        }
        if let Some(damage) = damage
            && damage.regions.is_some_and(<[_]>::is_empty)
            && frame.hdr_layers.is_empty()
            && self.bridge.keep_unchanged_output(&self.view, damage)
        {
            self.last_painted = PaintedArea::default();
            return false;
        }
        let damage = damage.unwrap_or(SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: None,
        });
        let Some((_, slot)) = self.slots.get(self.next_slot) else {
            return false;
        };
        self.next_slot = self
            .next_slot
            .saturating_add(1)
            .checked_rem(self.slots.len())
            .unwrap_or(0);
        let started = Instant::now();
        let painted = self.bridge.paint_output_into(
            &frame.scene,
            &self.view,
            damage,
            OutputTarget {
                view: slot,
                format: wgpu::TextureFormat::Bgra8Unorm,
                size: self.view.target_size(),
                orientation: settings_store::hardware::OutputTransform::Normal,
            },
            started
                .checked_add(SIMULATED_FRAME_DEADLINE)
                .unwrap_or(started),
            OutputSignal {
                sdr_white_nits: crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                hdr_layers: &frame.hdr_layers,
                color_lut: None,
                // Headless has no physical display brightness to control.
                dim: None,
                layers: frame.cache.as_ref(),
            },
        );
        self.bridge.after_paint();
        let target = self.target_pixels();
        let timing = self.bridge.last_phase_timing().unwrap_or_default();
        self.last_painted = match painted {
            Ok(stats) => {
                let sparse = stats.sparse_attempts > 0 && stats.full_attempts == 0;
                PaintedArea {
                    damage: if sparse {
                        damage
                            .regions
                            .map_or(target, crate::shell::render::paint::damage_area)
                    } else {
                        target
                    },
                    raster: if sparse {
                        self.bridge
                            .last_sparse_render_stats()
                            .map_or(target, |stats| stats.written_pixels)
                    } else {
                        target
                    },
                    dense: !sparse,
                    scanout: false,
                    submit_us: timing.submit_us,
                    settle_us: timing.completed_readback_map_us,
                }
            }
            Err(error) => {
                tracing::warn!(%error, "headless: simulated frame failed to render");
                PaintedArea::default()
            }
        };
        true
    }
}

impl SimulatedVblankPresenter {
    /// Show the frame's client buffer by itself, as the DRM presenter flips
    /// a fullscreen or maximized client's buffer onto the primary plane:
    /// `Some(true)` when a flip of it is queued, `Some(false)` when that
    /// buffer is already shown, `None` to composite. The rules are the DRM
    /// presenter's, less the plane's own `TEST_ONLY` verdict: an opaque
    /// dma-buf covering the display exactly.
    fn scan_out(
        &mut self,
        frame: &crate::shell::frame::FramePaint,
        damage: Option<SceneDamage<'_>>,
    ) -> Option<bool> {
        let candidate = frame.scanout.as_ref().filter(|candidate| {
            candidate.covers((0, 0), self.view.size) && frame.hdr_layers.is_empty()
        });
        let client = candidate
            .and_then(|candidate| crate::shell::gpu_frames::client_frame(&candidate.image))
            .filter(|client| client.dmabuf().is_some() && client.opaque());
        let Some((candidate, _client)) = candidate.zip(client) else {
            self.scanned_out = None;
            return None;
        };
        // What changed below the client is owed to the next composited frame.
        match damage {
            Some(damage) => {
                self.bridge.defer_output(&self.view, damage);
            }
            None => self.bridge.invalidate_output_view(&self.view),
        }
        let id = candidate.image.data.id();
        let shown = self.scanned_out.replace(id) == Some(id);
        self.last_painted = PaintedArea {
            scanout: !shown,
            ..PaintedArea::default()
        };
        Some(!shown)
    }
}

impl Presenter for SimulatedVblankPresenter {
    fn present(&mut self, scene: &Scene, output: OutputId) {
        self.present_frame(
            &crate::shell::frame::FramePaint::full(scene.clone()),
            output,
        );
    }

    fn present_frame(&mut self, frame: &crate::shell::frame::FramePaint, _: OutputId) {
        self.queued_last = false;
        let started = Instant::now();
        let rendered = self.render(frame);
        self.last_render = started.elapsed();
        if !rendered {
            return;
        }
        self.queued_last = true;
        // Queued once the previous flip retired.
        if let Some(previous) = self.owed.pop_front() {
            Self::wait_until(previous.vblank);
            self.ready.push(Self::retirement(&previous));
        }
        let queued_at = Instant::now();
        let vblank = self.vblank_after(queued_at);
        self.owed.push_back(QueuedFlip { queued_at, vblank });
    }

    /// Only for a frame that queued no flip (an unchanged frame): its flip
    /// timing is [`Self::retire_owed`]'s otherwise. Vblank-paced with no
    /// vblank of its own, as the DRM presenter reports such a frame, so the
    /// frame clock keeps its vblank grid instead of dropping it and pacing by
    /// retirement time.
    fn display_timing(&self) -> DisplayTiming {
        DisplayTiming {
            vblank_paced: true,
            vblank: None,
            flip_queued_at: None,
        }
    }

    /// As the DRM presenter on a vblank-paced output: no frame waits behind
    /// the one rendering. The loop builds the next frame once this one's
    /// render returned, from the newest state, instead of queueing one that
    /// would start only after this render and show input that much older.
    fn queue_depth(&self) -> usize {
        0
    }

    fn frame_detail(&mut self) -> Option<String> {
        let painted = self.last_painted;
        // What clients' wl_shm commits sent to the GPU since the last frame.
        let uploaded = crate::shell::gpu_frames::shm_upload_bytes();
        let shm_upload =
            uploaded.saturating_sub(std::mem::replace(&mut self.shm_uploaded, uploaded));
        Some(format!(
            "gpu_us={} damage_px={} raster_px={} dense={} scanout={} submit_us={} settle_us={} \
             shm_upload_bytes={shm_upload}",
            self.last_render.as_micros(),
            painted.damage,
            painted.raster,
            u8::from(painted.dense),
            u8::from(painted.scanout),
            painted.submit_us,
            painted.settle_us,
        ))
    }

    fn overlaps_presentation(&self) -> bool {
        true
    }

    fn owes_retirement(&self) -> bool {
        self.queued_last
    }

    fn owed_retirement_due(&self) -> Option<Instant> {
        self.owed.front().map(|flip| flip.vblank)
    }

    fn retire_owed(&mut self, wait: Duration) -> Vec<OwedRetirement> {
        if self.ready.is_empty()
            && let Some(front) = self.owed.front()
        {
            let due_within_wait = front.vblank.saturating_duration_since(Instant::now()) <= wait;
            if due_within_wait {
                Self::wait_until(front.vblank);
            }
        }
        let now = Instant::now();
        while self.owed.front().is_some_and(|flip| flip.vblank <= now) {
            if let Some(flip) = self.owed.pop_front() {
                self.ready.push(Self::retirement(&flip));
            }
        }
        std::mem::take(&mut self.ready)
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    //! Unit coverage for the headless backend. Each test acquires its
    //! own wgpu adapter (via `OffscreenRenderer::try_new`); on machines
    //! without llvmpipe these surface as an `Err` from
    //! `HeadlessBackend::new`, so we run them serially under
    //! `cargo test -- --test-threads 1` (the default for the
    //! tilo-desktop lib suite).
    use super::*;

    #[test]
    fn headless_backend_sizes_frames_and_keeps_the_loop_warm() {
        // Phase `headless_backend_new_at_default_size`.
        {
            let backend = HeadlessBackend::new(DEFAULT_WIDTH, DEFAULT_HEIGHT)
                .expect("HeadlessBackend::new at default size");
            let outs = backend.outputs();
            assert_eq!(
                outs.len(),
                1,
                "headless backend exposes one synthetic output"
            );
            assert_eq!(outs[0].size, (DEFAULT_WIDTH, DEFAULT_HEIGHT));
            assert_eq!(outs[0].id, OutputId(0));
        }
        // Phase `frame_size_matches_constructor_arg`.
        {
            let backend = HeadlessBackend::new(800, 600).expect("HeadlessBackend::new at 800x600");
            assert_eq!(backend.frame_size(), (800, 600));
        }
        // Phase `submit_frame_writes_non_uniform_rgba`.
        {
            // Paint a real FrameBuilder scene so the readback is something
            // the chrome actually emits. A blank scene with the cleared
            // background colour would be uniform — we want to assert
            // there are at least 3 distinct pixel values, which proves
            // (a) the offscreen pipeline ran end-to-end and (b) the
            // FrameBuilder's scene reached the texture (not a no-op).
            let mut fb = crate::shell::frame::FrameBuilder::new((400, 300));
            // Paint twice — the first paint primes layout caches, the
            // second is what we actually capture. Mirrors the
            // `paint_settled` pattern used by `bin/shots`.
            let _ = fb.paint();
            let scene = fb.paint_settled();
            let mut backend = HeadlessBackend::new(400, 300)
                .expect("HeadlessBackend::new at 400x300 for submit_frame test");
            backend.submit_frame(&scene, OutputId(0));
            assert!(
                backend.last_frame().is_empty(),
                "nobody watches a fresh headless backend, so nothing is rasterized"
            );
            assert!(!backend.watched());
            assert!(
                backend.set_watched(true),
                "attaching a viewer asks for a fresh frame"
            );
            assert!(!backend.set_watched(true), "already watched");
            assert!(backend.watched());
            backend.submit_frame(&scene, OutputId(0));
            let frame = backend.last_frame();
            assert_eq!(
                frame.len(),
                400 * 300 * 4,
                "RGBA frame buffer length must be width * height * 4"
            );
            let mut seen = std::collections::HashSet::new();
            for chunk in frame.chunks_exact(4) {
                seen.insert([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if seen.len() >= 3 {
                    break;
                }
            }
            assert!(
                seen.len() >= 3,
                "expected at least 3 distinct RGBA values in the rendered frame \
                 (got {}); the offscreen pipeline either ran a no-op paint or \
                 the FrameBuilder's chrome didn't reach the texture",
                seen.len()
            );
        }
        // Phase `poll_events_emits_redraw_to_keep_loop_warm`.
        {
            let mut backend = HeadlessBackend::new(640, 480)
                .expect("HeadlessBackend::new at 640x480 for poll_events test");
            let mut seen = Vec::new();
            backend.poll_events(&mut |ev| seen.push(ev));
            // Exactly one Redraw on this first call — a heartbeat so anything
            // downstream sees the backend wake up. Subsequent calls emit
            // nothing; see the source comment on `poll_events` for why.
            assert_eq!(
                seen.len(),
                1,
                "poll_events emits exactly one event per tick"
            );
            assert!(
                matches!(seen[0], BackendEvent::Redraw),
                "headless poll_events emits Redraw to keep the loop warm; got {:?}",
                seen[0]
            );
            seen.clear();
            backend.poll_events(&mut |ev| seen.push(ev));
            assert!(seen.is_empty(), "nothing arrives after startup");
            assert!(
                !backend.needs_idle_poll(),
                "a backend that never has news must not keep an idle desktop polling"
            );
        }
    }

    // ---- env-var parser ---------------------------------------------

    #[test]
    fn parse_headless_size_accepts_wxh_and_rejects_malformed_values() {
        // Phase `parse_headless_size_accepts_valid_wxh`.
        assert_eq!(parse_headless_size(Some("1280x720")), Some((1280, 720)));
        assert_eq!(parse_headless_size(Some("800X600")), Some((800, 600)));
        // Tolerate surrounding whitespace — shell expansions sometimes
        // sneak in trailing spaces.
        assert_eq!(parse_headless_size(Some("  640x480  ")), Some((640, 480)));
        // Phase `parse_headless_size_rejects_malformed_values`.
        assert_eq!(parse_headless_size(None), None);
        assert_eq!(parse_headless_size(Some("")), None);
        assert_eq!(parse_headless_size(Some("garbage")), None);
        assert_eq!(parse_headless_size(Some("800")), None);
        assert_eq!(parse_headless_size(Some("0x600")), None);
        assert_eq!(parse_headless_size(Some("800x0")), None);
        assert_eq!(parse_headless_size(Some("-1x100")), None);
    }

    // ---- output spec ------------------------------------------------

    #[test]
    fn output_spec_reads_mode_scale_position_rotation_and_refresh() {
        use settings_store::hardware::OutputTransform;
        let outs = parse_output_spec("2560x1600@2,1920x1080@1+1280+0~143.912,1080x1920r90")
            .expect("valid spec");
        assert_eq!(outs.len(), 3);
        assert_eq!(outs[0].id, OutputId(0));
        assert_eq!(outs[0].size, (2560, 1600));
        assert_eq!(outs[0].logical_size(), (1280, 800));
        assert_eq!(outs[0].scale, 2.0);
        assert_eq!(outs[0].refresh_mhz, None);
        // Positions are logical: the 1x monitor sits at the 2x panel's
        // logical right edge, and past its 2560 pixels in the scene.
        assert_eq!(outs[1].logical_position, (1280, 0));
        assert_eq!(outs[1].position, (2560, 0));
        assert_eq!(outs[1].refresh_mhz, Some(143_912));
        // No position: right of everything before it. Rotated: the mode
        // stays 1080x1920, the arranged extent swaps.
        assert_eq!(outs[2].transform, OutputTransform::Rotate90);
        assert_eq!(outs[2].logical_position, (3200, 0));
        assert_eq!((outs[2].position, outs[2].size), ((4480, 0), (1920, 1080)));
        assert_eq!(outs[2].physical_size(), (1080, 1920));
    }

    #[test]
    fn output_spec_translates_negative_positions_to_the_origin() {
        let outs = parse_output_spec("1920x1080,1280x1024-1280+56").expect("valid spec");
        assert_eq!(outs[0].logical_position, (1280, 0));
        assert_eq!(outs[1].logical_position, (0, 56));
        // At scale 1 the scene is the logical layout.
        assert_eq!(outs[0].position, (1280, 0));
        assert_eq!(outs[1].position, (0, 56));
    }

    #[test]
    fn output_spec_overlap_is_judged_in_logical_pixels() {
        // Physically the panel is 2560 wide, but at 2x it ends at logical
        // 1280, so a monitor there is beside it, not on top of it.
        assert!(parse_output_spec("2560x1600@2,1920x1080+1280+0").is_ok());
        assert!(parse_output_spec("2560x1600@2,1920x1080+1279+0").is_err());
    }

    #[test]
    fn output_spec_rejects_what_it_cannot_place() {
        for bad in [
            "",
            "1920x1080,",
            "1920",
            "1920x1080@0",
            "1920x1080@x",
            "1920x1080r45",
            "1920x1080+10",
            "1920x1080~0",
            "1920x1080?",
            // The second output lands inside the first.
            "1920x1080,1920x1080+100+100",
        ] {
            assert!(parse_output_spec(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn every_fixture_parses() {
        for (name, spec) in OUTPUT_FIXTURES {
            let outs = parse_output_spec(spec).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(outs.len() >= 2, "{name} is a multi-output fixture");
            assert_eq!(output_fixture(name).map(|o| o.len()), Some(outs.len()));
        }
    }

    #[test]
    fn an_unknown_arrangement_names_the_fixtures() {
        let error = resolve_output_arrangement("dockd-laptop").expect_err("typo");
        assert!(error.contains("docked-laptop"), "{error}");
        assert!(resolve_output_arrangement("docked-laptop").is_ok());
        assert!(resolve_output_arrangement("800x600").is_ok());
    }

    #[test]
    fn a_multi_output_backend_spans_the_combined_extent() {
        let outputs = output_fixture("docked-laptop").expect("fixture");
        let backend = HeadlessBackend::with_outputs(outputs.clone()).expect("headless backend");
        assert_eq!(backend.outputs().len(), outputs.len());
        assert_eq!(backend.frame_size(), combined_geometry(&outputs));
    }
}

#[cfg(test)]
#[path = "headless/retained_tests.rs"]
mod retained_tests;
