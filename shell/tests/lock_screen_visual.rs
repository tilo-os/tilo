//! Production-renderer review harness for the compositor lock overlay.
//!
//! This never locks or suspends the running session. It builds the same
//! `LockView` scenes as the native compositor and renders them through Vello
//! into offscreen PNGs:
//!
//! ```text
//! QOS_LOCK_SHOT_DIR=/tmp/qos-lock-shots \
//!   cargo test -p shell --test lock_screen_visual -- --ignored --nocapture
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
    let theme = Theme::dark();

    for (width, height) in [(1600, 900), (1366, 768), (1280, 720)] {
        let output = [LockOutput {
            origin: (0.0, 0.0),
            size: (f64::from(width), f64::from(height)),
            scale: 1.0,
        }];
        let mut renderer = OffscreenRenderer::new(width, height);
        let mut text = TextRenderer::new();
        for (state, view) in states() {
            let scene = draw_lock_scene(&output, &theme, &mut text, &view);
            renderer.render_png(
                &scene,
                Color::BLACK,
                root.join(format!("{width}x{height}-{state}.png")),
            );
        }
    }

    // Production uses one combined scene and stamps a usable lock UI on
    // every output. This asymmetric matrix catches anchoring across a bezel.
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
    let scene = draw_lock_scene(&outputs, &theme, &mut text, &states()[3].1);
    renderer.render_png(&scene, Color::BLACK, root.join("multi-output.png"));

    println!("lock-screen review PNGs: {}", root.display());
}
