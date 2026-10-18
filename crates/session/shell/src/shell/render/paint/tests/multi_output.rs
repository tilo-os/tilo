//! A docked laptop: a populated desktop on two displays of different scales, rendered the way
//! the DRM presenter renders them: each display through its own view of the frame, the one at
//! the scene origin straight into the bridge's target and the other into its own surface. A
//! display the frame's damage misses keeps the image it shows.

use super::*;
use crate::shell::frame::FramePaint;
use crate::shell::output_scene::OutputGeometry;

/// A 2944x1840 laptop panel at 200 % at the origin and a 1920x1080 monitor at 100 % to its
/// right, top edges aligned.
fn docked_laptop() -> Vec<OutputGeometry> {
    vec![
        OutputGeometry {
            id: 0,
            monitor: String::new(),
            connector: String::new(),
            x: 0.0,
            y: 0.0,
            width: 1472.0,
            height: 920.0,
            scale: 2.0,
            scene: vello::kurbo::Rect::new(0.0, 0.0, 2944.0, 1840.0),
        },
        OutputGeometry {
            id: 1,
            monitor: String::new(),
            connector: String::new(),
            x: 1472.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
            scale: 1.0,
            scene: vello::kurbo::Rect::new(2944.0, 0.0, 4864.0, 1080.0),
        },
    ]
}

/// The DRM presenter's view of `output`.
fn view_of(output: &OutputGeometry) -> OutputView {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "fixture scene rectangles are whole, positive pixels"
    )]
    OutputView::at(
        crate::shell::render::OutputId(output.id),
        (output.scene.x0 as i32, output.scene.y0 as i32),
        (output.scene.width() as u32, output.scene.height() as u32),
    )
}

/// The populated desktop laid out across `outputs`, and its first frame. The fixture owns
/// the frame builder's state directory, so it lives as long as the frame is used.
fn desktop_on(outputs: Vec<OutputGeometry>) -> (PopulatedShellFixture, FramePaint) {
    let mut fixture = PopulatedShellFixture::new(
        (3392, 1080),
        ui::theme::Theme::high_contrast().with_reduce_motion(true),
    );
    fixture.frame.set_output_geometry(outputs);
    fixture.frame.relayout();
    let frame = fixture.frame.paint_with_damage();
    (fixture, frame)
}

/// A display the frame's damage misses neither renders nor flips: it keeps the image it
/// shows, and rendering the other display, which swaps that display's surface through the
/// bridge, leaves its retained pixels current, so its next change repairs only its damage.
#[test]
fn a_display_the_damage_misses_keeps_its_image() {
    let _env = tilo_test_env::ScopedEnv::new();
    let outputs = docked_laptop();
    let (_fixture, frame) = desktop_on(outputs.clone());
    let mut bridge = required_bridge(2944, 1840);
    let [panel, monitor] = [view_of(&outputs[0]), view_of(&outputs[1])];
    assert!(
        bridge.view_is_whole_scene(&panel) && !bridge.view_is_whole_scene(&monitor),
        "the panel renders into the bridge's target, the monitor into a surface of its own"
    );
    for view in [&panel, &monitor] {
        paint_output_offscreen(&mut bridge, &frame.scene, view, full_frame(1));
    }
    // A caret on the panel, then one on the monitor, then the panel again.
    let on_panel = [rect(600, 600, 16, 40)];
    let on_monitor = [rect(3500, 500, 16, 40)];
    for (revision, damaged, missed, regions) in [
        (2, &panel, &monitor, &on_panel),
        (3, &monitor, &panel, &on_monitor),
        (4, &panel, &monitor, &on_panel),
    ] {
        let damage = version(revision, Some(revision - 1), Some(regions));
        assert!(
            bridge.keep_unchanged_output(missed, damage),
            "revision {revision}: display {} keeps its image",
            missed.id.0
        );
        assert!(
            !bridge.keep_unchanged_output(damaged, damage),
            "revision {revision}: display {} renders its damage",
            damaged.id.0
        );
        let (_, stats) = paint_output_offscreen(&mut bridge, &frame.scene, damaged, damage);
        assert_eq!(
            (stats.sparse_attempts, stats.full_attempts),
            (1, 0),
            "revision {revision}: display {} repairs only its damage",
            damaged.id.0
        );
    }
}

fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples.get(samples.len() / 2).copied().unwrap_or(0)
}

/// Numbers on this machine's GPU: a dense frame of each display, and a caret blinking on
/// the panel while one blinks on the monitor (each frame renders only the display it
/// damages), docked and with the panel alone.
#[test]
#[ignore = "measurement; run with --ignored --nocapture under the GPU lock"]
fn measure_docked_desktop_cost() {
    let _env = tilo_test_env::ScopedEnv::new();
    let outputs = docked_laptop();
    let (_alone_fixture, alone) = desktop_on(outputs[..1].to_vec());
    let (_fixture, docked) = desktop_on(outputs.clone());
    let [panel, monitor] = [view_of(&outputs[0]), view_of(&outputs[1])];
    for (label, frame, views) in [
        ("panel alone", &alone, vec![&panel]),
        ("docked", &docked, vec![&panel, &monitor]),
    ] {
        let mut bridge = required_bridge(2944, 1840);
        bridge.set_phase_timing_enabled(true);
        let gpu_wait = |bridge: &VelloDrmBridge| {
            bridge
                .last_phase_timing()
                .map_or(0, |timing| timing.completed_readback_map_us)
        };
        let mut revision = 1;
        for view in &views {
            paint_output_offscreen(&mut bridge, &frame.scene, view, full_frame(revision));
        }
        for view in &views {
            let (mut wall, mut gpu) = (Vec::new(), Vec::new());
            for _ in 0..12 {
                revision += 1;
                let started = Instant::now();
                paint_output_offscreen(&mut bridge, &frame.scene, view, full_frame(revision));
                wall.push(started.elapsed().as_micros());
                gpu.push(gpu_wait(&bridge));
            }
            eprintln!(
                "{label}: display {} dense frame median {} us, gpu wait {} us",
                view.id.0,
                median(wall),
                median(gpu)
            );
        }
        revision += 1;
        for view in &views {
            paint_output_offscreen(&mut bridge, &frame.scene, view, full_frame(revision));
        }
        let on_panel = [rect(600, 600, 16, 40)];
        let on_monitor = [rect(3500, 500, 16, 40)];
        let (mut wall, mut gpu) = (Vec::new(), Vec::new());
        for i in 0..24 {
            revision += 1;
            let damage = version(revision, Some(revision - 1), Some(&on_panel));
            // As the presenter does: a display the damage misses keeps its image, and its
            // retained pixels advance to this revision, so its next frame stays sparse.
            if views.len() > 1 {
                assert!(bridge.keep_unchanged_output(&monitor, damage));
            }
            let started = Instant::now();
            paint_output_offscreen(&mut bridge, &frame.scene, &panel, damage);
            wall.push(started.elapsed().as_micros());
            gpu.push(gpu_wait(&bridge));
            if views.len() > 1 && i % 2 == 0 {
                revision += 1;
                let damage = version(revision, Some(revision - 1), Some(&on_monitor));
                assert!(bridge.keep_unchanged_output(&panel, damage));
                paint_output_offscreen(&mut bridge, &frame.scene, &monitor, damage);
            }
        }
        eprintln!(
            "{label}: caret on the panel median {} us, gpu wait {} us",
            median(wall),
            median(gpu)
        );
    }
}
