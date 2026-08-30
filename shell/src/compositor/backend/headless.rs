//! Display-free `Backend` implementation: paints into an offscreen wgpu
//! texture (via [`ui::offscreen::OffscreenRenderer`]) instead of a
//! window. The compositor's chrome doesn't care which backend it paints
//! into, so this is the third path alongside [`super::WinitBackend`]
//! (host-window) and [`super::DrmBackend`] (TTY) — needed because
//! neither of those works in CI, in a distrobox without a display, or
//! under SSH without X-forwarding, and `QOS_TEST_CONTROL` needs a
//! backend to boot in order to drive the live binary headlessly.
//!
//! No `winit::*` imports (the entire point is to be display-free), no
//! real input devices, and no wayland-server surface bind change —
//! `native::run` still binds `wayland-N` and forwards client traffic;
//! the headless flag only affects the paint target.

use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::peniko::Color;

use super::{Backend, BackendEvent, Output, OutputId};

/// Env-var that overrides the synthetic output's dimensions. Format is
/// `WxH` (e.g. `1280x720`). Anything malformed or absent falls back to
/// the `shots --mode offscreen` default. Mirrors the env-var-driven
/// shape of `QOS_TEST_CONTROL` / `NIRIOS_TEST_STATE_DUMP` so the
/// whole headless surface stays opt-in without CLI bloat.
pub const HEADLESS_SIZE_ENV: &str = "NIRIOS_HEADLESS_SIZE";

/// Default size when `NIRIOS_HEADLESS_SIZE` is unset. Matches the
/// `bin/shots` default so a fresh `--backend headless` launch produces
/// frames comparable to a `cargo run --bin shots` screenshot.
pub const DEFAULT_WIDTH: u32 = 1600;
pub const DEFAULT_HEIGHT: u32 = 900;

/// Wgpu-offscreen [`Backend`] for display-free runs.
///
/// Each [`Backend::submit_frame`] call renders the supplied scene into
/// the offscreen texture and caches the RGBA bytes on `self.last_frame`,
/// so the upcoming `snapshot` test-control command can write a PNG
/// without re-rendering. See the module-level doc comment for design
/// intent.
pub struct HeadlessBackend {
    renderer: OffscreenRenderer,
    outputs: Vec<Output>,
    events: Vec<BackendEvent>,
    /// Latest painted RGBA frame, kept so `test_control`'s `snapshot`
    /// command can write a PNG without re-rendering.
    last_frame: Vec<u8>,
    /// Whether the one startup `Redraw` has been handed over yet.
    emitted_first_redraw: bool,
}

impl HeadlessBackend {
    /// Build a headless backend at the configured size. The
    /// [`OffscreenRenderer`] eagerly acquires a wgpu adapter — this
    /// returns `Err` (rather than panics) on machines without llvmpipe
    /// or another suitable adapter so the binary entry point can
    /// surface a friendly error instead of dying at the panic site.
    pub fn new(width: u32, height: u32) -> Result<Self, Box<dyn std::error::Error>> {
        if width == 0 || height == 0 {
            return Err(format!(
                "HeadlessBackend::new: invalid size {width}x{height}: dimensions must be > 0"
            )
            .into());
        }
        // `try_new` is the fallible constructor; `new` panics, and a
        // `catch_unwind` around it would be inert in release builds
        // (`panic = "abort"`). Going through the `Result` is what lets
        // the headless backend honour the rest of the `Backend::new`
        // contract — return an error, don't take `main` down with us.
        let renderer = OffscreenRenderer::try_new(width, height).map_err(|e| {
            format!("HeadlessBackend::new: OffscreenRenderer init failed at {width}x{height}: {e}")
        })?;
        let outputs = vec![Output::new(OutputId(0), (width, height), 1.0)];
        Ok(Self {
            renderer,
            outputs,
            events: Vec::new(),
            last_frame: Vec::new(),
            emitted_first_redraw: false,
        })
    }

    /// Read-only access to the most recently rendered frame, RGBA8888,
    /// row-major. Used by `compositor::test_control` for the upcoming
    /// `snapshot` command. Empty until the first
    /// [`Backend::submit_frame`] call.
    pub fn last_frame(&self) -> &[u8] {
        &self.last_frame
    }

    /// Frame size as `(width, height)`. Reflects the value passed to
    /// [`HeadlessBackend::new`] — the headless backend never resizes
    /// (no window, no display events to drive a resize off of).
    pub fn frame_size(&self) -> (u32, u32) {
        (self.renderer.width, self.renderer.height)
    }
}

/// Parse `NIRIOS_HEADLESS_SIZE` if present. Returns `None` on
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

impl Backend for HeadlessBackend {
    fn submit_frame(&mut self, scene: &Scene, _output: OutputId) {
        // `OffscreenRenderer::render` is a wgpu round-trip per call; unlike
        // `WinitBackend` it doesn't need a `present()` step since there is
        // no swapchain. We cache the RGBA bytes so test-control's
        // `snapshot` command can write a PNG without re-painting.
        //
        // The clear colour matches `WinitBackend`'s `FALLBACK_BG` and
        // `Theme::dark().bg_app`, so a headless PNG and a winit host window
        // clear to identical pixels for a blank scene.
        let bg = Color::from_rgb8(0x1A, 0x1B, 0x26);
        self.last_frame = self.renderer.render(scene, bg);
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

#[cfg(test)]
mod tests {
    //! Unit coverage for the headless backend. Each test acquires its
    //! own wgpu adapter (via `OffscreenRenderer::try_new`); on machines
    //! without llvmpipe these surface as an `Err` from
    //! `HeadlessBackend::new`, so we run them serially under
    //! `cargo test -- --test-threads 1` (the default for the
    //! quick-desktop lib suite).
    use super::*;

    #[test]
    fn headless_backend_new_at_default_size() {
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

    #[test]
    fn frame_size_matches_constructor_arg() {
        let backend = HeadlessBackend::new(800, 600).expect("HeadlessBackend::new at 800x600");
        assert_eq!(backend.frame_size(), (800, 600));
    }

    #[test]
    fn submit_frame_writes_non_uniform_rgba() {
        // Paint a real FrameBuilder scene so the readback is something
        // the chrome actually emits. A blank scene with the cleared
        // background colour would be uniform — we want to assert
        // there are at least 3 distinct pixel values, which proves
        // (a) the offscreen pipeline ran end-to-end and (b) the
        // FrameBuilder's scene reached the texture (not a no-op).
        let mut fb = crate::compositor::frame::FrameBuilder::new((400, 300));
        // Paint twice — the first paint primes layout caches, the
        // second is what we actually capture. Mirrors the
        // `paint_settled` pattern used by `bin/shots`.
        let _ = fb.paint();
        let scene = fb.paint_settled();
        let mut backend = HeadlessBackend::new(400, 300)
            .expect("HeadlessBackend::new at 400x300 for submit_frame test");
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

    #[test]
    fn poll_events_emits_redraw_to_keep_loop_warm() {
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
    }

    // ---- env-var parser ---------------------------------------------

    #[test]
    fn parse_headless_size_accepts_valid_wxh() {
        assert_eq!(parse_headless_size(Some("1280x720")), Some((1280, 720)));
        assert_eq!(parse_headless_size(Some("800X600")), Some((800, 600)));
        // Tolerate surrounding whitespace — shell expansions sometimes
        // sneak in trailing spaces.
        assert_eq!(parse_headless_size(Some("  640x480  ")), Some((640, 480)));
    }

    #[test]
    fn parse_headless_size_rejects_malformed_values() {
        assert_eq!(parse_headless_size(None), None);
        assert_eq!(parse_headless_size(Some("")), None);
        assert_eq!(parse_headless_size(Some("garbage")), None);
        assert_eq!(parse_headless_size(Some("800")), None);
        assert_eq!(parse_headless_size(Some("0x600")), None);
        assert_eq!(parse_headless_size(Some("800x0")), None);
        assert_eq!(parse_headless_size(Some("-1x100")), None);
    }
}
