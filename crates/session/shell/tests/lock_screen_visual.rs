//! Production-renderer review harness for the compositor lock overlay.
//!
//! This never locks or suspends the running session. It builds the same
//! `LockView` scenes as the native compositor and renders them through Vello
//! into offscreen PNGs:
//!
//! ```text
//! TILO_LOCK_SHOT_DIR=/tmp/tilo-lock-shots \
//!   cargo test -p shell --test it lock_screen_visual::render_lock_screen_matrix -- --ignored --nocapture
//! ```
//!
//! Set `TILO_LOCK_SHOT_QUICK=1` for the portrait accessibility matrix,
//! secondary-output composition, and animation phases only.

use shell::compositor::lock_screen::{
    LockOutput, LockView, draw_lock_scene, draw_lock_scene_at_time,
};
use ui::offscreen::OffscreenRenderer;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::peniko::Color;

fn shot_dir() -> std::path::PathBuf {
    std::env::var_os("TILO_LOCK_SHOT_DIR").map_or_else(
        || std::env::temp_dir().join("tilo-lock-shots"),
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
    let quick = std::env::var("TILO_LOCK_SHOT_QUICK").is_ok_and(|value| value == "1");
    std::fs::create_dir_all(&root).expect("create lock screenshot directory");
    for (theme_label, theme) in vec![
        ("normal", Theme::dark()),
        ("light", Theme::light()),
        ("reduced-motion", Theme::dark().with_reduce_motion(true)),
        ("large-text", Theme::dark().with_text_scale(1.3)),
        ("maximum-text", Theme::dark().with_text_scale(4.0)),
        ("high-contrast", Theme::high_contrast()),
    ]
    .into_boxed_slice()
    {
        for (width, height, scale, label) in [
            (1600, 900, 1.0, ""),
            (1366, 768, 1.0, ""),
            (1280, 720, 1.0, ""),
            (1280, 800, 1.0, ""),
            // Scale is presentation density, not a second layout multiplier.
            (1600, 900, 2.0, "-hidpi"),
            // Compact but supported laptop/tablet viewport.
            (960, 540, 1.0, "-compact"),
            (800, 600, 1.0, "-small"),
            (640, 960, 1.0, "-portrait"),
        ] {
            if quick && label != "-portrait" {
                continue;
            }
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

    let mut renderer = OffscreenRenderer::new(1600, 900);
    for time in [0.0, 8.0, 16.0] {
        let scene = draw_lock_scene_at_time(
            &[LockOutput {
                origin: (0.0, 0.0),
                size: (1600.0, 900.0),
                scale: 1.0,
            }],
            &Theme::dark(),
            &mut TextRenderer::new(),
            &states()[0].1,
            None,
            time,
        );
        renderer.render_png(
            &scene,
            Color::BLACK,
            root.join(format!("motion-{time:02}.png")),
        );
    }
    println!("lock-screen review PNGs: {}", root.display());
}
