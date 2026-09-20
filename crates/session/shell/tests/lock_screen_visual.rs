//! Production-renderer review harness for the compositor lock overlay.
//!
//! This never locks or suspends the running session. It builds the same
//! `LockView` scenes as the native compositor and renders them through Vello
//! into offscreen PNGs:
//!
//! ```text
//! QOS_LOCK_SHOT_DIR=/tmp/qos-lock-shots \
//!   cargo test -p shell --test it lock_screen_visual::render_lock_screen_matrix -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a visual review harness these are assertions, not recoverable failures"
)]

use crate::support::offscreen::OffscreenRenderer;
use shell::compositor::lock_screen::{LockOutput, LockView, draw_lock_scene};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::peniko::Color;

fn shot_dir() -> std::path::PathBuf {
    std::env::var_os("QOS_LOCK_SHOT_DIR").map_or_else(
        || std::env::temp_dir().join("qos-lock-shots"),
        std::path::PathBuf::from,
    )
}

fn states() -> [(&'static str, LockView<'static>); 4] {
    [
        (
            "normal",
            LockView {
                clock: "21:47",
                date: "Saturday, September 5",
                ..LockView::default()
            },
        ),
        (
            "error",
            LockView {
                clock: "21:47",
                date: "Saturday, September 5",
                error: Some("Incorrect username or password"),
                ..LockView::default()
            },
        ),
        (
            "verifying",
            LockView {
                clock: "21:47",
                date: "Saturday, September 5",
                password_len: 12,
                verifying: true,
                ..LockView::default()
            },
        ),
        (
            "notifications",
            LockView {
                clock: "21:47",
                date: "Saturday, September 5",
                password_len: 8,
                notification_count: 12,
                ..LockView::default()
            },
        ),
    ]
}

#[test]
#[ignore = "review tool: needs an offscreen wgpu adapter and writes PNGs"]
fn render_lock_screen_matrix() {
    let root = shot_dir();
    std::fs::create_dir_all(&root).expect("create lock screenshot directory");
    for (theme_label, theme) in [
        ("normal", Theme::dark()),
        ("large-text", Theme::dark().with_text_scale(1.3)),
        ("high-contrast", Theme::high_contrast()),
    ] {
        for (width, height, scale, label) in [
            (1600, 900, 1.0, ""),
            (1366, 768, 1.0, ""),
            (1280, 720, 1.0, ""),
            // Scale is presentation density, not a second layout multiplier.
            (1600, 900, 2.0, "-hidpi"),
            // Compact but supported laptop/tablet viewport.
            (960, 540, 1.0, "-compact"),
        ] {
            let output = [LockOutput {
                origin: (0.0, 0.0),
                size: (f64::from(width), f64::from(height)),
                scale,
            }];
            let mut renderer = OffscreenRenderer::new(width, height);
            let mut text = TextRenderer::new();
            for (state, view) in states() {
                let scene = draw_lock_scene(&output, &theme, &mut text, &view);
                renderer.render_png(
                    &scene,
                    Color::BLACK,
                    root.join(format!("{width}x{height}{label}-{theme_label}-{state}.png")),
                );
            }
        }
    }

    // One combined scene, one credential surface on the designated primary
    // output, and an intentionally non-interactive secondary indication.
    // This asymmetric matrix catches both bezel anchoring and accidental
    // duplicate password cards.
    let outputs = [
        LockOutput {
            origin: (0.0, 0.0),
            size: (1366.0, 768.0),
            scale: 1.25,
        },
        LockOutput {
            origin: (1366.0, 48.0),
            size: (1280.0, 720.0),
            scale: 1.0,
        },
    ];
    let mut renderer = OffscreenRenderer::new(2646, 768);
    let mut text = TextRenderer::new();
    let scene = draw_lock_scene(&outputs, &Theme::dark(), &mut text, &states()[3].1);
    renderer.render_png(&scene, Color::BLACK, root.join("multi-output.png"));

    println!("lock-screen review PNGs: {}", root.display());
}
