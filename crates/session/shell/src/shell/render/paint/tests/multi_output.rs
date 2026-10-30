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

/// A deadline no test frame meets on a working renderer.
fn far_deadline() -> Instant {
    Instant::now()
        .checked_add(std::time::Duration::from_secs(30))
        .unwrap_or_else(Instant::now)
}

/// How `image` differs from the same pixels of the combined desktop: the pixels that
/// differ, and the largest channel difference.
fn differences(desktop: &[u8], image: &[u8]) -> (usize, u8) {
    let crop = crop(desktop, 4864, (2944, 0, 1920, 1080));
    assert_eq!(crop.len(), image.len(), "the image is the display's size");
    let mut pixels = 0_usize;
    let mut largest = 0;
    for (a, b) in crop.chunks_exact(4).zip(image.chunks_exact(4)) {
        let worst = a
            .iter()
            .zip(b)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        if worst > 0 {
            pixels = pixels.saturating_add(1);
            largest = largest.max(worst);
        }
    }
    (pixels, largest)
}

/// Rendered through its own view, a display's pixels may round a channel the other way
/// where a path's edge crosses a pixel: a handful of pixels, by one level. Nothing else.
fn assert_rounding_only((pixels, largest): (usize, u8), what: &str) {
    assert!(
        largest <= 1 && pixels <= 64,
        "{what}: {pixels} pixels differ, by up to {largest}"
    );
}

/// The readback bytes of the bridge's last render.
fn read_bytes(bridge: &VelloDrmBridge) -> u64 {
    bridge
        .last_phase_timing()
        .map_or(0, |timing| timing.readback_bytes)
}

/// `width`x`height` pixels at (`x`, `y`) of a tight RGBA image `stride` pixels wide.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixture rectangles lie inside images a few thousand pixels wide"
)]
fn crop(
    rgba: &[u8],
    stride: usize,
    (x, y, width, height): (usize, usize, usize, usize),
) -> Vec<u8> {
    (y..y + height)
        .flat_map(|row| {
            let start = (row * stride + x) * 4;
            rgba[start..start + width * 4].iter().copied()
        })
        .collect()
}

/// A display presented by readback (its port is on another GPU) beside a direct one renders
/// alone and reads back only what changed on it: the whole display once, then nothing for a
/// frame whose damage falls on the other display, a caret's pixels for a caret on it, and
/// the damage of a frame it skipped with the next. Its image is the combined readback's
/// pixels of that display, which used to render and read back every display.
#[test]
fn a_display_on_the_readback_reads_back_only_what_changed_on_it() {
    let _env = tilo_test_env::ScopedEnv::new();
    let outputs = docked_laptop();
    let (_fixture, frame) = desktop_on(outputs.clone());
    let mut bridge = required_bridge(2944, 1840);
    bridge.set_phase_timing_enabled(true);
    let [panel, monitor] = [view_of(&outputs[0]), view_of(&outputs[1])];
    paint_output_offscreen(&mut bridge, &frame.scene, &panel, full_frame(1));
    let refresh = |bridge: &mut VelloDrmBridge, damage| {
        bridge
            .refresh_output_image(&frame.scene, &monitor, damage, far_deadline())
            .expect("the monitor's image refreshes")
    };

    assert_eq!(
        refresh(&mut bridge, full_frame(1)),
        OutputImageChange::Whole
    );
    assert_eq!(
        read_bytes(&bridge),
        1920 * 1080 * 4,
        "the first frame reads it whole"
    );
    let mut combined = required_bridge(4864, 1840);
    let desktop = combined
        .paint(&frame.scene)
        .expect("combined readback")
        .to_vec();
    let image = bridge
        .output_image(monitor.id)
        .expect("the monitor's image");
    assert_rounding_only(
        differences(&desktop, image),
        "the display's own image is the combined readback's pixels of it",
    );

    let on_panel = [rect(600, 600, 16, 40)];
    assert_eq!(
        refresh(&mut bridge, version(2, Some(1), Some(&on_panel))),
        OutputImageChange::Unchanged,
        "damage on the other display renders and reads nothing here"
    );

    let on_monitor = [rect(3500, 500, 16, 40)];
    assert_eq!(
        refresh(&mut bridge, version(3, Some(2), Some(&on_monitor))),
        OutputImageChange::Region(rect(3500 - 2944, 500, 16, 40))
    );
    assert_eq!(
        read_bytes(&bridge),
        16 * 40 * 4,
        "a caret reads back its own pixels"
    );

    // A frame skipped while the display's flip was pending is owed to the next one.
    let skipped = [rect(4000, 100, 8, 8)];
    bridge.defer_output_image(&monitor, Some(version(4, Some(3), Some(&skipped))));
    assert_eq!(
        refresh(&mut bridge, version(5, Some(4), Some(&on_monitor))),
        OutputImageChange::Region(rect(3500 - 2944, 100, 4000 + 8 - 3500, 500 + 40 - 100))
    );
    assert_eq!(read_bytes(&bridge), (16 * 40 + 8 * 8) * 4);
    let image = bridge
        .output_image(monitor.id)
        .expect("the monitor's image");
    assert_rounding_only(
        differences(&desktop, image),
        "repaired regions leave the image equal to a whole readback",
    );
}

/// The owner's docked laptop: a 1920x1080 panel at 125 % at the origin and a 3840x2160
/// monitor at 150 % on another GPU's port to its right.
fn laptop_and_4k_on_another_gpu() -> Vec<OutputGeometry> {
    vec![
        OutputGeometry {
            id: 0,
            monitor: String::new(),
            connector: String::new(),
            x: 0.0,
            y: 0.0,
            width: 1536.0,
            height: 864.0,
            scale: 1.25,
            scene: vello::kurbo::Rect::new(0.0, 0.0, 1920.0, 1080.0),
        },
        OutputGeometry {
            id: 1,
            monitor: String::new(),
            connector: String::new(),
            x: 1536.0,
            y: 0.0,
            width: 2560.0,
            height: 1440.0,
            scale: 1.5,
            scene: vello::kurbo::Rect::new(1920.0, 0.0, 5760.0, 2160.0),
        },
    ]
}

/// Numbers on this machine's GPU for the display on the readback, each frame as the
/// presenter shows it there (render, readback, and the swizzle into its dumb buffer): the
/// combined readback the presenter used, which renders and reads the box around both
/// displays, against the display's own image.
#[test]
#[ignore = "measurement; run with --ignored --nocapture under the GPU lock"]
fn measure_readback_display_cost() {
    let _env = tilo_test_env::ScopedEnv::new();
    let outputs = laptop_and_4k_on_another_gpu();
    let (_fixture, frame) = desktop_on(outputs.clone());
    let monitor = view_of(&outputs[1]);
    let (width, height) = (3840_usize, 2160_usize);
    let mut dumb = vec![0_u8; width * height * 4];
    let caret_monitor = [rect(3500, 900, 16, 40)];
    let caret_panel = [rect(600, 600, 16, 40)];
    let whole_panel = [rect(0, 32, 1920, 1048)];
    let whole_monitor = [rect(1920, 48, 3840, 2112)];
    let cases: [(&str, Option<&[vello::FineDamageRect]>); 5] = [
        ("dense frame", None),
        ("caret on the monitor", Some(&caret_monitor)),
        ("caret on the panel", Some(&caret_panel)),
        ("strip pan on the panel", Some(&whole_panel)),
        ("strip pan on the monitor", Some(&whole_monitor)),
    ];
    for (label, regions) in cases {
        for own in [false, true] {
            let mut bridge = if own {
                required_bridge(1920, 1080)
            } else {
                required_bridge(5760, 2160)
            };
            bridge.set_phase_timing_enabled(true);
            let (mut wall, mut bytes) = (Vec::new(), Vec::new());
            for revision in 1_u64..=16 {
                let damage = if revision == 1 {
                    full_frame(1)
                } else {
                    version(revision, Some(revision - 1), regions)
                };
                let started = Instant::now();
                let mut read = 0;
                if own {
                    let change = bridge
                        .refresh_output_image(&frame.scene, &monitor, damage, far_deadline())
                        .expect("own image");
                    if change != OutputImageChange::Unchanged {
                        read = read_bytes(&bridge);
                        let image = bridge.output_image(monitor.id).expect("image");
                        swizzle_subrect_rgba_to_xrgb8888(
                            SubrectSource {
                                src_rgba: image,
                                src_w: width,
                                src_h: height,
                                src_x: 0,
                                src_y: 0,
                            },
                            &mut dumb,
                            width * 4,
                            width,
                            height,
                        );
                    }
                } else {
                    // `render_combined_readback`: the damage when the desktop image is
                    // whole, else a full render; the monitor's buffer is written whole on
                    // every frame, as a page-flipped dumb pair is.
                    let sparse = match damage.regions {
                        Some([]) => true,
                        Some(regions) if bridge.has_complete_readback() => bridge
                            .refresh_encoded_regions(&frame.scene, regions)
                            .is_ok(),
                        _ => false,
                    };
                    if !sparse {
                        bridge.paint(&frame.scene).expect("combined readback");
                    }
                    read = read_bytes(&bridge);
                    let desktop = bridge.encoded_rgba().expect("combined image");
                    swizzle_subrect_rgba_to_xrgb8888(
                        SubrectSource {
                            src_rgba: desktop,
                            src_w: 5760,
                            src_h: 2160,
                            src_x: 1920,
                            src_y: 0,
                        },
                        &mut dumb,
                        width * 4,
                        width,
                        height,
                    );
                }
                if revision > 2 {
                    wall.push(started.elapsed().as_micros());
                    bytes.push(u128::from(read));
                }
            }
            eprintln!(
                "{label}: {} median {} us, readback {} bytes",
                if own {
                    "own image"
                } else {
                    "combined readback"
                },
                median(wall),
                median(bytes)
            );
        }
    }
}

/// How often frames of common motions on the owner's docked laptop carry damage the
/// presenter can repair sparsely, and how much: pointer motion over the monitor, wheel
/// scrolls on it, and the same on the panel alone.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn probe_docked_damage() {
    let _env = tilo_test_env::ScopedEnv::new();
    for (label, outputs) in [
        ("docked", laptop_and_4k_on_another_gpu()),
        ("panel alone", laptop_and_4k_on_another_gpu()[..1].to_vec()),
    ] {
        let (mut fixture, _) = desktop_on(outputs.clone());
        let mut document = AutoCommit::new();
        let mut focus: Option<NodeId> = None;
        let last = outputs.last().expect("an output");
        let (x0, width, height) = (last.x, last.width, last.height);
        for (motion, events) in [
            (
                "pointer",
                (0..40)
                    .map(|step| {
                        ui::synthetic_input::cursor_moved(
                            x0 + width * (0.1 + 0.02 * f64::from(step)),
                            height * 0.5,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            (
                "wheel",
                std::iter::once(ui::synthetic_input::cursor_moved(
                    x0 + width * 0.3,
                    height * 0.5,
                ))
                .chain((0..40).map(|_| ui::synthetic_input::mouse_wheel(0.0, -1.0)))
                .collect(),
            ),
        ] {
            let (mut dense, mut sparse, mut empty, mut area) = (0, 0, 0, 0_u64);
            for event in &events {
                fixture.frame.dispatch_event(
                    event,
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut document),
                    &ROOT,
                    &mut focus,
                );
                fixture.frame.relayout();
                let paint = fixture.frame.paint_with_damage();
                match paint.damage_regions.as_deref() {
                    None => dense += 1,
                    Some([]) => empty += 1,
                    Some(regions) => {
                        sparse += 1;
                        area += regions
                            .iter()
                            .map(|r| u64::from(r.width) * u64::from(r.height))
                            .sum::<u64>();
                    }
                }
            }
            eprintln!(
                "{label} {motion}: dense {dense} sparse {sparse} unchanged {empty}, \
                 sparse px/frame {}",
                area / u64::try_from(sparse.max(1)).unwrap_or(1)
            );
        }
    }
}
