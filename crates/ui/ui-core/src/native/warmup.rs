//! Cold-start warmup of the font system. Loading the font system dominates time-to-first-paint on a
//! cold process: the catalog is verified and every installed face mapped, up to ~1s on release builds.
//! Shaping the first text on a background thread, before the first frame needs it, takes that off the
//! critical path: | Configuration | median first-paint | |-------------------|--------------------| |
//! no warmup | 406 ms | | fonts warmup | **316 ms** | It wins because it never touches the GPU: it
//! shapes strings on a background thread, building the process's shared font database so the
//! compositor's own `configured_font_system` call finds it ready.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: cold-start warmup builds the shared font system on a thread"
)]

use std::thread;

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping};

// Re-export the FontSystem type so downstream tests can name it without
// pulling `cosmic-text` in as a direct dev-dep.
pub use cosmic_text as _cosmic_text;

/// Canonical strings the warmup shapes through cosmic-text so the per-face shaping caches are populated
/// before the compositor's first real widget draw.
pub const CANONICAL_STRINGS: &[&str] = &[
    "The quick brown fox jumps over the lazy dog.",
    // Latin-1 extended: accented Western European locale menu labels.
    "Voix ambiguë d'un cœur qui, au zéphyr, préfère les jattes de kiwis.",
    "我能吞下玻璃而不伤身体。",
    "いろはにほへとちりぬるを",
    "다람쥐 헌 쳇바퀴에 타고파.",
    "\u{1F600}\u{1F44D}\u{2764}\u{FE0F}\u{1F4A1}",
];

/// Shape every entry of [`CANONICAL_STRINGS`] through the supplied `FontSystem`.
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
            total_glyphs = total_glyphs.saturating_add(run.glyphs.len());
        }
    }
    total_glyphs
}

/// Spawn a detached background worker that builds the process's shared `FontSystem` and shapes
/// [`CANONICAL_STRINGS`] against it, so the fonts the first frame needs are read and mapped beside the
/// rest of the start.
pub fn spawn_warmup() -> Option<thread::JoinHandle<()>> {
    tilo_startup_metrics::mark("warmup-spawn");
    let handle = thread::Builder::new()
        .name("tilo-startup-warmup".into())
        .spawn(move || {
            tilo_startup_metrics::span("warmup-fonts", || {
                let mut fs = crate::text_render::configured_font_system();
                let _ = shape_canonical_strings(&mut fs);
            });
        })
        .ok()?;
    Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_shapes_the_bundled_fonts_the_first_frame_needs() {
        let mut font_system = crate::font_catalog::fixture_font_system();
        assert!(shape_canonical_strings(&mut font_system) > 0);
    }
}
