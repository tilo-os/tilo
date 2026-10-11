//! The shell's own scenes — the desktop and the lock screen — rendered through vello's GPU pipeline and through the CPU
//! replay (`tilo-cpu-raster`), compared pixel by pixel.
//!
//! Both renders, their difference and a capture of each scene land in `TILO_SHOT_DIR`, or
//! `$TMPDIR/tilo-cpu-raster`; the captures feed `tilo-cpu-raster`'s
//! benchmark on other machines.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;

use shell::compositor::frame::FrameBuilder;
use shell::compositor::lock_screen::{LockOutput, LockView, draw_lock_scene};
use tilo_cpu_raster::compare::{Diff, compare, premultiply};
use tilo_cpu_raster::{CpuRasterizer, Options, Target};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::peniko::Color;

use crate::support::offscreen::OffscreenRenderer;

const W: u32 = 1600;
const H: u32 = 900;

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("TILO_SHOT_DIR").map_or_else(
        || std::env::temp_dir().join("tilo-cpu-raster"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(name: &str, rgba: &[u8]) {
    ui::png::write_rgba8(out_dir().join(name), W, H, rgba).unwrap();
}

/// Render through both pipelines, keep both, their difference and a capture.
fn parity(gpu: &mut OffscreenRenderer, name: &str, scene: &Scene) -> Diff {
    let mut reference = gpu.render_resilient(scene, Color::BLACK);
    let mut ours = vec![0u8; (W * H * 4) as usize];
    {
        let mut target = Target::new(&mut ours, W, H, W as usize * 4).unwrap();
        CpuRasterizer::new(Options::default()).render(
            scene.encoding(),
            &mut target,
            None,
            Color::BLACK,
        );
    }
    write_png(&format!("{name}.gpu.png"), &reference);
    write_png(&format!("{name}.cpu.png"), &ours);
    premultiply(&mut reference);
    let mut heat = Vec::with_capacity(ours.len());
    for (a, b) in reference.chunks_exact(4).zip(ours.chunks_exact(4)) {
        let d = a
            .iter()
            .zip(b)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        let grey = (u16::from(a[0]) + u16::from(a[1]) + u16::from(a[2])) / 12;
        heat.extend_from_slice(&if d == 0 {
            [grey as u8, grey as u8, grey as u8, 255]
        } else {
            [(64 + u16::from(d) * 6).min(255) as u8, 0, 0, 255]
        });
    }
    write_png(&format!("{name}.diff.png"), &heat);
    let file = std::fs::File::create(out_dir().join(format!("{name}.tscene"))).unwrap();
    tilo_cpu_raster::capture::write(scene.encoding(), file).unwrap();
    compare(&reference, &ours)
}

#[test]
fn shell_scenes_match_vello_gpu() {
    let mut gpu = OffscreenRenderer::new(W, H);
    let mut scenes: Vec<(String, Scene)> = Vec::new();

    let mut fb = FrameBuilder::new((W, H));
    fb.relayout();
    scenes.push(("desktop".into(), fb.paint_settled()));
    drop(fb);

    let output = [LockOutput {
        origin: (0.0, 0.0),
        size: (f64::from(W), f64::from(H)),
        scale: 1.0,
    }];
    let mut text = TextRenderer::new();
    for (name, view) in [
        (
            "lock",
            LockView {
                clock: "21:47",
                date: "Saturday, September 5",
                ..LockView::default()
            },
        ),
        (
            "lock-password",
            LockView {
                clock: "21:47",
                date: "Saturday, September 5",
                password_len: 8,
                notification_count: 3,
                ..LockView::default()
            },
        ),
    ] {
        let scene = draw_lock_scene(&output, &Theme::dark(), &mut text, &view);
        scenes.push((name.into(), scene));
    }

    let mut failures = Vec::new();
    for (name, scene) in &scenes {
        let diff = parity(&mut gpu, name, scene);
        println!("{name:14} {diff}");
        // Edge pixels of two anti-aliasing implementations, as in
        // tilo-cpu-raster's own feature parity test.
        if diff.psnr < 33.0 || diff.over_8_ratio() > 0.02 || diff.over_32_ratio() > 0.005 {
            failures.push(format!("{name}: {diff}"));
        }
    }
    println!("renders and captures in {}", out_dir().display());
    assert!(
        failures.is_empty(),
        "outside the bound:\n{}",
        failures.join("\n")
    );
}
