//! Cold-start warmup for expensive first-frame work.
//!
//! Two things dominate time-to-first-paint on a cold process:
//! `cosmic_text::FontSystem::new()` mmaps every installed font face (up to
//! ~1s on release builds), and `vello::Renderer::new()` compiles ~30 WGSL
//! pipelines against the live wgpu device, stalling whichever thread runs
//! it. Only the fonts side ships enabled by default:
//!
//! | Configuration          | median first-paint | notes                 |
//! |-------------------------|--------------------|-----------------------|
//! | no warmup               | 406 ms             | reference              |
//! | fonts + vello warmup    | 483 ms             | slower than no warmup |
//! | fonts warmup only       | **316 ms**         | **-90 ms / -22 %**    |
//!
//! Vello regresses because it holds the process-wide `SHARED_RENDER_CONTEXT`
//! mutex through its scratch render, blocking main's `OffscreenRenderer::new`
//! on the same lock — left opt-in behind `QOS_STARTUP_WARM_VELLO`. Fonts
//! wins because it never touches the GPU: it shapes strings on a background
//! thread, warming the OS page cache so the compositor's own
//! `FontSystem::new` hits already-resident pages.
//!
//! `QOS_STARTUP_WARM=off` (also `0`/`false`/`no`) disables warmup for A/B
//! measurement. Stages mark through [`qos_startup_metrics::span`]:
//! `warmup-spawn`, `warmup-fonts`(-end), `warmup-vello`(-end).
//!
//! Not persistence — a persistent `wgpu::PipelineCache` would extend the
//! win across restarts, but needs driver support not ubiquitous on the
//! Mesa releases qOS ships against, and wouldn't help first boot at all.

use std::thread;

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping};

// Re-export the FontSystem type so downstream tests can name it without
// pulling `cosmic-text` in as a direct dev-dep.
pub use cosmic_text as _cosmic_text;

/// Canonical strings the warmup shapes through cosmic-text so the per-face
/// shaping caches are populated before the compositor's first real widget
/// draw. Kept small so the warmup thread stays under ~50ms wall time even
/// on a cold fontconfig cache.
///
/// Covers Latin-1, common CJK (JP+CN+KR overlap), and a four-emoji fallback
/// probe — enough to force every fallback family the shell chrome hits on
/// first paint (workspace name pill, tab titles, clock, notification body).
pub const CANONICAL_STRINGS: &[&str] = &[
    "The quick brown fox jumps over the lazy dog.",
    // Latin-1 extended: accented Western European locale menu labels.
    "Voix ambiguë d'un cœur qui, au zéphyr, préfère les jattes de kiwis.",
    // Forces the CJK fallback fonts (Noto Sans CJK, if installed).
    "我能吞下玻璃而不伤身体。",
    "いろはにほへとちりぬるを",
    "다람쥐 헌 쳇바퀴에 타고파.",
    // If no emoji font is installed the shaper reports `.notdef` for all
    // four, which is cheap — no per-face load happens.
    "\u{1F600}\u{1F44D}\u{2764}\u{FE0F}\u{1F4A1}",
];

/// Whether warmup should run in the current process.
///
/// Reads `QOS_STARTUP_WARM` once per process (cached in a `OnceLock`).
/// Disabled if the value is `off`/`0`/`false`/`no` (case-insensitive);
/// every other value, and an unset var, enables warmup.
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
/// `FontSystem`. Populates the internal shape / fallback caches so the
/// first real draw against the same `FontSystem` short-circuits at those
/// cache lookups.
///
/// Returns the number of glyph runs emitted across every buffer —
/// tests assert `> 0` to prove shaping actually happened rather than
/// silently no-op'ing on a stripped fallback font set.
pub fn shape_canonical_strings(fs: &mut FontSystem) -> usize {
    // 14pt at 1.4x line-height; larger sizes reuse the same shaped runs
    // since cosmic-text scales after shaping.
    let metrics = Metrics::new(14.0, 20.0);
    let mut total_glyphs = 0usize;
    for s in CANONICAL_STRINGS {
        let mut buffer = Buffer::new(fs, metrics);
        // Generous max-width so wrapping doesn't trigger — warm the
        // per-face shaper, not the line-breaker.
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
/// `no_vello` is set — builds a 4×4 scratch
/// [`crate::offscreen::OffscreenRenderer`] and renders one solid-colour
/// scene.
///
/// Dropping the returned `JoinHandle` before the worker finishes doesn't
/// cancel anything — the process keeps whatever warmup already completed.
/// Returns `None` when warmup is disabled via `QOS_STARTUP_WARM=off`.
pub fn spawn_warmup(no_vello: bool) -> Option<thread::JoinHandle<()>> {
    if !is_enabled() {
        return None;
    }
    qos_startup_metrics::mark("warmup-spawn");
    let handle = thread::Builder::new()
        .name("qos-startup-warmup".into())
        .spawn(move || {
            qos_startup_metrics::span("warmup-fonts", || {
                let mut fs = FontSystem::new();
                let _ = shape_canonical_strings(&mut fs);
            });
            if no_vello {
                return;
            }
            // Builds against the process-wide `SHARED_RENDER_CONTEXT` device
            // — the same one the compositor's real Renderer uses — so its
            // ~30 pipelines are already resident when the real one starts.
            //
            // `try_new`, not `new`: under `panic = "abort"` a `new()` panic
            // on a missing adapter would take the whole compositor down at
            // startup, and `catch_unwind` doesn't help under abort.
            qos_startup_metrics::span("warmup-vello", || {
                use vello::Scene;
                use vello::kurbo::{Affine, Rect};
                use vello::peniko::{Color, Fill};

                let Ok(mut renderer) = crate::offscreen::OffscreenRenderer::try_new(4, 4) else {
                    return;
                };
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
        })
        .ok()?;
    Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default is on — an unset env var must not silently skip the
    /// win. Relies on the `OnceLock`-cached value, so this must be the
    /// first test to read `QOS_STARTUP_WARM` in the process.
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
