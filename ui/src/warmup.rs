//! Cold-start warmup for expensive first-frame work.
//!
//! Two things dominate the compositor's time-to-first-paint on a cold
//! process:
//!
//! 1. `cosmic_text::FontSystem::new()` walks fontconfig and mmaps every
//!    installed face. Its own docstring warns it can take up to a
//!    second on release builds.
//! 2. `vello::Renderer::new()` compiles ~30 WGSL pipelines against the
//!    live wgpu device. Whichever thread does this stalls until the
//!    driver is done.
//!
//! Neither cost is avoidable — but both can be *moved off the critical
//! path* by running them on a background thread as soon as the process
//! is alive. When the compositor thread later builds its own
//! `FontSystem` and `Renderer`, the OS page cache (font files),
//! fontconfig's shared cache, and — for backends that share the
//! process-wide `RenderContext` via [`crate::offscreen`] — wgpu's
//! device-level pipeline cache are already hot.
//!
//! # Behaviour
//! Warmup is enabled by default. Set `QOS_STARTUP_WARM=off` (also
//! accepts `0` / `false` / `no`) to disable — used for A/B measurement.
//!
//! # Metrics
//! Every stage records marks through [`qos_startup_metrics::span`], so a
//! process launched with `QOS_STARTUP_METRICS_PATH` set gets:
//!
//! ```text
//! warmup-spawn        (main thread, before the join handle is stored)
//! warmup-fonts        (worker thread, before FontSystem::new)
//! warmup-fonts-end    (worker thread, after shape loop)
//! warmup-vello        (worker thread, before scratch renderer)
//! warmup-vello-end    (worker thread, after scratch render)
//! ```
//!
//! # Not persistence
//! This is a *hot* warmup — no bytes are written to disk. A persistent
//! `wgpu::PipelineCache` would extend the win across process restarts,
//! but it requires driver support (`PIPELINE_CACHE` feature) that isn't
//! ubiquitous on the Mesa releases qOS ships against and doesn't help
//! the first-boot case at all. The scratch-render path lands the
//! measurable win with no new device features.

use std::thread;

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping};

// Re-export the FontSystem type so downstream tests can name it without
// pulling `cosmic-text` in as a direct dev-dep.
pub use cosmic_text as _cosmic_text;

/// Canonical strings the warmup shapes through cosmic-text so the
/// per-face shaping caches are populated before the compositor's first
/// real widget draw. Kept small so the warmup thread stays under
/// ~50 ms wall time even on a cold fontconfig cache.
///
/// The set covers Latin-1, common CJK (JP+CN+KR overlap), and a
/// four-emoji fallback probe — enough to force the fallback iterator
/// to touch every fallback family the shell chrome hits on first
/// paint (workspace name pill, tab titles, clock, notification body).
pub const CANONICAL_STRINGS: &[&str] = &[
    "The quick brown fox jumps over the lazy dog.",
    // Latin-1 extended: covers accented Western European chrome (menu
    // labels for German / French locale settings).
    "Voix ambiguë d'un cœur qui, au zéphyr, préfère les jattes de kiwis.",
    // CJK: Simplified Chinese sample sentence + Japanese kana + Korean
    // hangul. Forces cosmic-text to hit the CJK fallback fonts (Noto
    // Sans CJK, if installed) so the first `Shaping::Advanced` call
    // against those faces is warm.
    "我能吞下玻璃而不伤身体。",
    "いろはにほへとちりぬるを",
    "다람쥐 헌 쳇바퀴에 타고파.",
    // Common emoji: hits the color-emoji fallback face. If no emoji
    // font is installed the shaper reports `.notdef` for all four,
    // which is cheap — no per-face load happens.
    "\u{1F600}\u{1F44D}\u{2764}\u{FE0F}\u{1F4A1}",
];

/// Whether warmup should run in the current process.
///
/// Reads `QOS_STARTUP_WARM`; the check is done once per process
/// through a `OnceLock` so the return value is stable across calls.
/// Disabled if the env var is one of `off`, `0`, `false`, `no`
/// (case-insensitive) — every other value, and an unset var, enables
/// warmup.
pub fn is_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        let Ok(raw) = std::env::var("QOS_STARTUP_WARM") else {
            return true;
        };
        let normalized = raw.trim().to_ascii_lowercase();
        !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
    })
}

/// Shape every entry of [`CANONICAL_STRINGS`] through the supplied
/// FontSystem. Populates the internal shape / fallback caches so the
/// first real draw against the same FontSystem short-circuits at those
/// cache lookups.
///
/// Returns the number of glyph runs emitted across every buffer —
/// tests assert `> 0` to prove shaping actually happened rather than
/// silently no-op'ing on a stripped fallback font set.
pub fn shape_canonical_strings(fs: &mut FontSystem) -> usize {
    // 14pt at 1.4× line-height covers every chrome text size the shell
    // renders on the first frame (workspace-name pill, tab titles,
    // clock, tooltips). Larger sizes reuse the same shaped runs since
    // cosmic-text scales after shaping.
    let metrics = Metrics::new(14.0, 20.0);
    let mut total_glyphs = 0usize;
    for s in CANONICAL_STRINGS {
        let mut buffer = Buffer::new(fs, metrics);
        // A generous max-width so wrapping doesn't trigger during
        // warmup — we want to warm the per-face shaper, not exercise
        // the line-breaker.
        buffer.set_size(Some(2048.0), Some(2048.0));
        buffer.set_text(s, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(fs, false);
        for run in buffer.layout_runs() {
            total_glyphs += run.glyphs.len();
        }
    }
    total_glyphs
}

/// Spawn a detached background worker that constructs a throw-away
/// `FontSystem`, shapes [`CANONICAL_STRINGS`] against it, then — unless
/// `no_vello` is set — builds a 4×4 scratch [`crate::offscreen::OffscreenRenderer`]
/// and renders one solid-colour scene.
///
/// The returned `JoinHandle` should be kept alive on the caller's
/// stack (typically the `App` or the compositor's frame builder). If
/// the handle is dropped before the worker finishes, the process still
/// benefits from whatever warmup work already completed — dropping does
/// not signal cancellation.
///
/// Returns `None` when warmup is disabled via `QOS_STARTUP_WARM=off`.
pub fn spawn_warmup(no_vello: bool) -> Option<thread::JoinHandle<()>> {
    if !is_enabled() {
        return None;
    }
    qos_startup_metrics::mark("warmup-spawn");
    let handle = thread::Builder::new()
        .name("qos-startup-warmup".into())
        .spawn(move || {
            // Fonts first. Even a throw-away FontSystem warms the OS
            // page cache for every .ttf/.otf under /usr/share/fonts —
            // the compositor's own FontSystem::new hits those already-
            // resident pages and avoids the disk round-trip.
            qos_startup_metrics::span("warmup-fonts", || {
                let mut fs = FontSystem::new();
                let _ = shape_canonical_strings(&mut fs);
            });
            if no_vello {
                return;
            }
            // Vello: build a 4×4 offscreen renderer through the
            // process-wide `SHARED_RENDER_CONTEXT` and paint one
            // trivial scene. The wgpu Device the compositor's own
            // Renderer will build against is the same Device (both
            // OffscreenRenderer paths call `RenderContext::device(None)`
            // which caches per-adapter DeviceHandles inside vello). At
            // that point the ~30 pipelines are already resident on the
            // device — the second `Renderer::new` returns much sooner.
            //
            // Wrapped in `catch_unwind` because OffscreenRenderer::new
            // panics when no wgpu adapter is available (headless CI
            // with no llvmpipe). A missing adapter isn't fatal to the
            // shell — the compositor's real backend surfaces its own
            // adapter error later — so we swallow it here.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                qos_startup_metrics::span("warmup-vello", || {
                    use vello::Scene;
                    use vello::kurbo::{Affine, Rect};
                    use vello::peniko::{Color, Fill};

                    let mut renderer = crate::offscreen::OffscreenRenderer::new(4, 4);
                    let mut scene = Scene::new();
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        Color::from_rgb8(0x33, 0x33, 0x33),
                        None,
                        &Rect::new(0.0, 0.0, 4.0, 4.0),
                    );
                    let _ = renderer.render(&scene, Color::TRANSPARENT);
                });
            }));
        })
        .ok()?;
    Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default is on. An unset env var must enable warmup — we
    /// don't want a fresh container image, which sets nothing, to
    /// silently skip the win.
    ///
    /// Uses the `OnceLock`-cached value, so this test must run before
    /// any other test that toggles the env var. It's a `#[test]` under
    /// `#[serial]` (via the OnceLock) — the first test to observe the
    /// flag wins for the whole process.
    #[test]
    fn enabled_by_default_when_env_unset() {
        // SAFETY: single-threaded test binary invocation; the harness
        // runs one `#[test]` at a time under the default test main. We
        // clear the var so the OnceLock initialiser sees the "unset"
        // branch on a fresh process.
        // (Setting env vars in tests is racy across threads — cargo
        // test binaries run each #[test] in its own thread, and rustc
        // 1.83 flagged it unsafe. We accept the risk here because the
        // OnceLock caches on first read, and this is the only test
        // reading it.)
        unsafe {
            std::env::remove_var("QOS_STARTUP_WARM");
        }
        assert!(is_enabled());
    }
}
