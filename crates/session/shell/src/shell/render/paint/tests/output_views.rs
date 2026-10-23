use super::*;

#[test]
fn output_view_renders_its_subrect() {
    // A 200x100 desktop of two 100x100 outputs side by side: red on the
    // left output, blue on the right.
    let mut bridge = required_bridge(200, 100);
    let mut scene = Scene::new();
    for (x, color) in [
        (0.0, Color::from_rgb8(255, 0, 0)),
        (100.0, Color::from_rgb8(0, 0, 255)),
    ] {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &vello::kurbo::Rect::new(x, 0.0, x + 100.0, 100.0),
        );
    }
    let right = OutputView {
        id: crate::shell::render::OutputId(1),
        transform: Affine::translate((-100.0, 0.0)),
        size: (100, 100),
        orientation: OutputTransform::Normal,
    };
    let left = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (100, 100));
    // The scanout stand-in is BGRA.
    let blue: [u8; 4] = [255, 0, 0, 255];
    let red: [u8; 4] = [0, 0, 255, 255];
    for (view, expected, name) in [(right, blue, "right"), (left, red, "left")] {
        let pixels = paint_output_pixels(&mut bridge, &scene, &view, 1);
        assert_eq!(pixels.len(), 100 * 100 * 4);
        let wrong = pixels
            .chunks_exact(4)
            .filter(|pixel| *pixel != expected)
            .count();
        assert_eq!(
            wrong, 0,
            "every pixel of the {name} output is its half of the scene"
        );
    }
    assert_eq!(
        bridge.output_surfaces.len(),
        2,
        "each output keeps its own retained surface"
    );
    assert_eq!(
        bridge.size(),
        (200, 100),
        "the scene's own target is untouched by the outputs"
    );
}

const ORIENTATIONS: [OutputTransform; 4] = [
    OutputTransform::Normal,
    OutputTransform::Rotate90,
    OutputTransform::Rotate180,
    OutputTransform::Rotate270,
];

/// A desktop of `size` in which every turn of a pixel shows: a distinct colour in two
/// corners, an anti-aliased diagonal, a translucent circle and a scaled image.
fn asymmetric_scene(size: (u32, u32)) -> Scene {
    use vello::kurbo::{BezPath, Circle, Rect};
    use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let mut scene = Scene::new();
    for (color, rect) in [
        (Color::from_rgb8(30, 60, 90), Rect::new(0.0, 0.0, w, h)),
        (
            Color::from_rgb8(230, 20, 40),
            Rect::new(0.0, 0.0, w / 3.0, h / 4.0),
        ),
        (
            Color::from_rgb8(20, 210, 70),
            Rect::new(w * 0.6, h * 0.7, w, h),
        ),
    ] {
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    }
    let mut wedge = BezPath::new();
    wedge.move_to((0.0, h));
    wedge.line_to((w * 0.8, 0.0));
    wedge.line_to((w, h * 0.2));
    wedge.close_path();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(250, 200, 20, 180),
        None,
        &wedge,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(120, 80, 250, 200),
        None,
        &Circle::new((w * 0.3, h * 0.6), h * 0.21),
    );
    let texels: Vec<u8> = (0_u8..12)
        .flat_map(|i| {
            [
                i.wrapping_mul(21),
                255_u8.wrapping_sub(i.wrapping_mul(19)),
                i.wrapping_mul(7),
                255,
            ]
        })
        .collect();
    let image = ImageData {
        data: Blob::new(std::sync::Arc::new(texels)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width: 4,
        height: 3,
    };
    scene.fill(
        Fill::NonZero,
        // Five times its size, at (w/2, h/10).
        Affine::new([5.0, 0.0, 0.0, 5.0, w * 0.5, h * 0.1]),
        &ImageBrush::new(image),
        None,
        &Rect::new(0.0, 0.0, 4.0, 3.0),
    );
    scene
}

/// BGRA scanout bytes as the RGBA source the readback kernels take.
fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
        .collect()
}

/// Pixels whose colour channels differ between a GPU `Bgra8Unorm` target and the CPU
/// kernel's `XRGB8888` bytes. The X byte is padding KMS ignores.
fn differing_pixels(gpu: &[u8], cpu: &[u8]) -> usize {
    assert_eq!(gpu.len(), cpu.len());
    gpu.chunks_exact(4)
        .zip(cpu.chunks_exact(4))
        .filter(|(g, c)| g[..3] != c[..3])
        .count()
}

#[test]
fn a_rotated_lone_output_matches_the_readback_byte_for_byte() {
    // The output is the desktop: the direct path renders into the bridge's own target,
    // which is what the readback presenter renders too, and turns it on the GPU. The
    // readback turns the same render on the CPU (`paint_one_scanout` without ICC or HDR).
    let size = (90, 52);
    let mut bridge = required_bridge(size.0, size.1);
    let scene = asymmetric_scene(size);
    let rgba = bridge.paint(&scene).expect("the dense render").to_vec();
    for (revision, transform) in (1..).zip(ORIENTATIONS) {
        let view =
            OutputView::at(crate::shell::render::OutputId(0), (0, 0), size).oriented(transform);
        assert!(bridge.view_is_whole_scene(&view));
        let gpu = paint_output_pixels(&mut bridge, &scene, &view, revision);
        let (pw, ph) = view.target_size();
        let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
        swizzle_subrect_rgba_to_xrgb8888_composited(
            SubrectSource {
                src_rgba: &rgba,
                src_w: size.0 as usize,
                src_h: size.1 as usize,
                src_x: 0,
                src_y: 0,
            },
            &mut cpu,
            (pw * 4) as usize,
            (pw as usize, ph as usize),
            transform,
            None,
        );
        assert_eq!(
            differing_pixels(&gpu, &cpu),
            0,
            "{transform:?}: the GPU turn is the readback's, byte for byte"
        );
    }
}

#[test]
fn a_rotated_output_beside_another_turns_its_own_surface() {
    // Two 90x52 outputs side by side; the right one is rotated and renders into its own
    // surface. Its scanout must be its unrotated image turned exactly as the readback
    // kernel turns it.
    let output = (90, 52);
    let mut bridge = required_bridge(output.0 * 2, output.1);
    let scene = asymmetric_scene((output.0 * 2, output.1));
    let id = crate::shell::render::OutputId(1);
    let upright = OutputView::at(id, (90, 0), output);
    let unrotated = bgra_to_rgba(&paint_output_pixels(&mut bridge, &scene, &upright, 1));
    for (revision, transform) in (2..).zip(ORIENTATIONS) {
        let view = upright.oriented(transform);
        assert!(!bridge.view_is_whole_scene(&view));
        let gpu = paint_output_pixels(&mut bridge, &scene, &view, revision);
        let (pw, ph) = view.target_size();
        let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
        swizzle_subrect_rgba_to_xrgb8888_transformed(
            SubrectSource {
                src_rgba: &unrotated,
                src_w: output.0 as usize,
                src_h: output.1 as usize,
                src_x: 0,
                src_y: 0,
            },
            &mut cpu,
            (pw * 4) as usize,
            pw as usize,
            ph as usize,
            transform,
        );
        assert_eq!(
            differing_pixels(&gpu, &cpu),
            0,
            "{transform:?}: the surface is turned on the GPU exactly as on the CPU"
        );
    }
    assert_eq!(
        bridge.output_surfaces.len(),
        1,
        "the rotated output keeps one surface in scene orientation, whatever its turn"
    );
}

#[test]
fn a_rotated_hdr_output_matches_the_readback_pq_encode() {
    // The readback encodes and turns on the CPU (`SdrToPqEncoder`); the direct path on the
    // GPU. The encodes differ by at most 2 of 1023 code values
    // (`gpu_encode_matches_the_cpu_reference`); the turn must add nothing to that.
    let size = (90, 52);
    let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
    let mut bridge = required_bridge(size.0, size.1);
    let scene = asymmetric_scene(size);
    let rgba = bridge.paint(&scene).expect("the dense render").to_vec();
    let encoder = crate::shell::color_pipeline::SdrToPqEncoder::new(white);
    for (revision, transform) in (1..).zip(ORIENTATIONS) {
        let view =
            OutputView::at(crate::shell::render::OutputId(0), (0, 0), size).oriented(transform);
        let (texture, _) = paint_output_offscreen_as(
            &mut bridge,
            &scene,
            &view,
            full_frame(revision),
            (wgpu::TextureFormat::Rgb10a2Unorm, None),
        );
        let gpu = read_target(&bridge, &texture);
        let (pw, ph) = view.target_size();
        let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
        encode_subrect_rgba_to_xbgr2101010_transformed(
            SubrectSource {
                src_rgba: &rgba,
                src_w: size.0 as usize,
                src_h: size.1 as usize,
                src_x: 0,
                src_y: 0,
            },
            &mut cpu,
            (pw * 4) as usize,
            (pw as usize, ph as usize),
            transform,
            &encoder,
            None,
        );
        let word = |bytes: &[u8]| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let max_delta = gpu
            .chunks_exact(4)
            .zip(cpu.chunks_exact(4))
            .flat_map(|(g, c)| {
                let (g, c) = (word(g), word(c));
                [0, 10, 20].map(|shift| {
                    (i64::from((g >> shift) & 0x3FF) - i64::from((c >> shift) & 0x3FF)).abs()
                })
            })
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 2,
            "{transform:?}: the GPU PQ encode differs from the readback's by {max_delta} code values"
        );
    }
}

#[test]
fn a_calibrated_output_matches_the_readback_qcms_pass() {
    // The readback calibrates on the CPU: the output's sub-rect copied out, qcms applied,
    // then turned and swizzled (`paint_one_scanout`). The direct path does the same in
    // its final GPU pass through the profile's `IccLut`, with no readback, and stays
    // within the LUT's stated tolerance of qcms (`icc_lut` module docs).
    let size = (90, 52);
    let panel = crate::shell::render::icc_lut::tests::adobe_rgb();
    let transform_icc = panel.transform();
    let lut = IccLut::from_transform(&transform_icc);
    let mut bridge = required_bridge(size.0, size.1);
    let scene = asymmetric_scene(size);
    let rgba = bridge.paint(&scene).expect("the dense render").to_vec();
    let mut calibrated = Vec::new();
    copy_rgba_subrect(
        SubrectSource {
            src_rgba: &rgba,
            src_w: size.0 as usize,
            src_h: size.1 as usize,
            src_x: 0,
            src_y: 0,
        },
        size.0 as usize,
        size.1 as usize,
        &mut calibrated,
    );
    transform_icc.apply(&mut calibrated);
    bridge.set_phase_timing_enabled(true);
    for (revision, transform) in (1..).zip(ORIENTATIONS) {
        let view =
            OutputView::at(crate::shell::render::OutputId(0), (0, 0), size).oriented(transform);
        let (texture, _) = paint_output_offscreen_as(
            &mut bridge,
            &scene,
            &view,
            full_frame(revision),
            (wgpu::TextureFormat::Bgra8Unorm, Some(&lut)),
        );
        assert_eq!(
            bridge
                .last_phase_timing()
                .expect("phase timing")
                .readback_bytes,
            0,
            "{transform:?}: a calibrated output reads nothing back"
        );
        let gpu = read_target(&bridge, &texture);
        let (pw, ph) = view.target_size();
        let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
        swizzle_subrect_rgba_to_xrgb8888_composited(
            SubrectSource {
                src_rgba: &calibrated,
                src_w: size.0 as usize,
                src_h: size.1 as usize,
                src_x: 0,
                src_y: 0,
            },
            &mut cpu,
            (pw * 4) as usize,
            (pw as usize, ph as usize),
            transform,
            None,
        );
        let max_delta = gpu
            .chunks_exact(4)
            .zip(cpu.chunks_exact(4))
            .flat_map(|(g, c)| [0, 1, 2].map(|i| i32::from(g[i].abs_diff(c[i]))))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= panel.max_code_delta,
            "{transform:?}: the GPU calibration differs from the readback's qcms pass by {max_delta} code values"
        );
        let uncalibrated = paint_output_pixels(&mut bridge, &scene, &view, revision + 100);
        let uncalibrated_delta = uncalibrated
            .chunks_exact(4)
            .zip(cpu.chunks_exact(4))
            .flat_map(|(g, c)| [0, 1, 2].map(|i| i32::from(g[i].abs_diff(c[i]))))
            .max()
            .unwrap_or(0);
        assert!(
            uncalibrated_delta > panel.max_code_delta,
            "{transform:?}: the profile is visible ({uncalibrated_delta} code values)"
        );
    }
}

#[test]
fn an_hdr_output_ignores_its_icc_profile() {
    // The profile describes the panel's SDR response; in HDR the PQ encode replaces it,
    // on the GPU as on the readback.
    let size = (90, 52);
    let lut =
        IccLut::from_transform(&crate::shell::render::icc_lut::tests::adobe_rgb().transform());
    let mut bridge = required_bridge(size.0, size.1);
    let scene = asymmetric_scene(size);
    let view = OutputView::at(crate::shell::render::OutputId(0), (0, 0), size)
        .oriented(OutputTransform::Rotate90);
    let pq = |bridge: &mut VelloDrmBridge, revision, color_lut| {
        let (texture, _) = paint_output_offscreen_as(
            bridge,
            &scene,
            &view,
            full_frame(revision),
            (wgpu::TextureFormat::Rgb10a2Unorm, color_lut),
        );
        read_target(bridge, &texture)
    };
    let plain = pq(&mut bridge, 1, None);
    let profiled = pq(&mut bridge, 2, Some(&lut));
    assert_eq!(plain, profiled);
}

/// A one-strip frame as layers on a `size` output: the asymmetric scene
/// behind, a strip twice the output wide read `scroll_x` pixels in, and
/// a chip of chrome above.
fn pan_layers(size: (u32, u32), scroll_x: u32) -> crate::shell::frame::FrameCache {
    use vello::kurbo::{Circle, Rect};
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let mut strip = Scene::new();
    for (x, green, blue) in [
        (4.0, 0, 255),
        (37.0, 40, 225),
        (81.0, 80, 195),
        (122.0, 120, 165),
        (160.0, 160, 135),
    ] {
        strip.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(200, green, blue, 230),
            None,
            &Rect::new(x, 6.0, x + 29.5, h * 0.55),
        );
        strip.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(20, 20, green),
            None,
            &Circle::new((x + 14.0, h * 0.3), 6.5),
        );
    }
    let mut chrome = Scene::new();
    chrome.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(240, 240, 240, 200),
        None,
        &Rect::new(w * 0.7, 2.0, w - 3.0, 9.0),
    );
    crate::shell::frame::FrameCache {
        width: size.0.saturating_mul(2),
        strips: vec![crate::shell::frame::StripBlit {
            dest_x: 0,
            dest_y: 0,
            dest_w: size.0,
            dest_h: size.1.saturating_mul(2).checked_div(3).unwrap_or(0),
            scroll_x,
        }],
        generation: 1,
        chrome_damage: None,
        layers: crate::shell::frame::FrameLayers {
            backdrop: asymmetric_scene(size),
            workspace: Scene::new(),
            strips: vec![strip],
            strip_parts: vec![None],
            chrome,
        },
    }
}

/// A strip pan on a turned, calibrated or HDR output is composited from
/// its layers, as on a plain one, and shows what rendering the frame's
/// scene through that output shows: the final pass turns, calibrates or
/// PQ-encodes the composite as it would the render.
#[test]
fn a_turned_calibrated_or_hdr_output_composites_a_strip_pan_from_its_layers() {
    let size = (90, 52);
    let lut =
        IccLut::from_transform(&crate::shell::render::icc_lut::tests::adobe_rgb().transform());
    let sdr = wgpu::TextureFormat::Bgra8Unorm;
    let hdr = wgpu::TextureFormat::Rgb10a2Unorm;
    // Channel steps for 8-bit outputs, code values for the 10-bit PQ
    // encode: a composite differs from a single render by a step of
    // rounding, which the PQ curve stretches in the darks.
    let cases: [(&str, wgpu::TextureFormat, Option<&IccLut>, i64); 3] = [
        ("SDR", sdr, None, 2),
        ("calibrated", sdr, Some(&lut), 3),
        ("HDR", hdr, None, 12),
    ];
    for (name, format, color_lut, tolerance) in cases {
        for transform in ORIENTATIONS {
            let label = format!("{name}, {transform:?}");
            let view =
                OutputView::at(crate::shell::render::OutputId(0), (0, 0), size).oriented(transform);
            let mut layered = required_bridge(size.0, size.1);
            let mut rendered = required_bridge(size.0, size.1);
            let mut shown = Vec::new();
            for (revision, scroll_x) in [(1, 0), (2, 7)] {
                let cache = pan_layers(size, scroll_x);
                let scene = crate::shell::render::strip_layers::compose(&cache);
                let (texture, _) = paint_output_offscreen_with(
                    &mut layered,
                    (&scene, Some(&cache)),
                    &view,
                    full_frame(revision),
                    (format, color_lut),
                );
                shown = read_target(&layered, &texture);
                let work = layered
                    .target
                    .last_layer_work_for_test()
                    .unwrap_or_else(|| panic!("{label}: the frame came as layers"));
                if revision == 2 {
                    assert!(
                        matches!(
                            work.workspace,
                            crate::shell::render::strip_layers::WorkspaceWork::Pan { .. }
                        ),
                        "{label}: the pan copies the strip ({work:?})"
                    );
                }
            }
            let cache = pan_layers(size, 7);
            let scene = crate::shell::render::strip_layers::compose(&cache);
            let (texture, _) = paint_output_offscreen_with(
                &mut rendered,
                (&scene, None),
                &view,
                full_frame(1),
                (format, color_lut),
            );
            let expected = read_target(&rendered, &texture);
            assert_eq!(shown.len(), expected.len(), "{label}");
            let channels = |bytes: &[u8]| -> [i64; 3] {
                if format == hdr {
                    let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    [0, 10, 20].map(|shift| i64::from((word >> shift) & 0x3FF))
                } else {
                    [0, 1, 2].map(|index| i64::from(bytes[index]))
                }
            };
            let max_delta = shown
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .flat_map(|(a, b)| {
                    let (a, b) = (channels(a), channels(b));
                    [0, 1, 2].map(|index| (a[index] - b[index]).abs())
                })
                .max()
                .unwrap_or(0);
            assert!(
                max_delta <= tolerance,
                "{label}: the composited pan differs from the rendered scene by {max_delta}"
            );
        }
    }
}

#[test]
fn dropped_direct_frame_makes_the_output_render_in_full() {
    // Two 100x100 outputs side by side; damage falls only on the left one.
    let mut bridge = required_bridge(200, 100);
    let scene = Scene::new();
    let left = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (100, 100));
    let right = OutputView::at(crate::shell::render::OutputId(1), (100, 0), (100, 100));
    paint_output_pixels(&mut bridge, &scene, &left, 1);
    paint_output_pixels(&mut bridge, &scene, &right, 1);
    let left_only = [vello::FineDamageRect {
        x: 10,
        y: 10,
        width: 20,
        height: 20,
    }];
    let damage = |revision| SceneDamage {
        revision,
        previous_revision: Some(revision - 1),
        regions: Some(&left_only),
    };
    assert!(
        !bridge.keep_unchanged_output(&left, damage(2)),
        "the damaged output renders"
    );
    assert!(
        bridge.keep_unchanged_output(&right, damage(2)),
        "an undamaged output holding the previous revision keeps its image"
    );
    // A busy flip dropped the right output's frame: its surface is ahead of KMS.
    bridge.invalidate_output_view(&right);
    assert!(
        !bridge.keep_unchanged_output(&right, damage(3)),
        "after a dropped flip the output must not count as unchanged"
    );
}

#[test]
fn per_output_view_never_bands_two_4k_heads() {
    // Two 3840x2160 heads side by side span 7680x2160: 30x9 = 270 Vello bins, past the
    // 256 one render can address, so the combined target rendered in two bands. While
    // both render direct the bridge's target is the head at the origin (the DRM
    // presenter's `bridge_extent`), and each head is one render.
    let head = (3840, 2160);
    assert_eq!(
        RetainedGpuTarget::band_rows((7680, 2160)),
        Some(2048),
        "the combined desktop is past the bin limit"
    );
    assert_eq!(
        RetainedGpuTarget::band_rows(head),
        None,
        "one head fits one render"
    );
    let mut bridge = required_bridge(head.0, head.1);
    let mut scene = Scene::new();
    // A window spanning both heads.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(40, 120, 200),
        None,
        &vello::kurbo::Rect::new(3000.0, 400.0, 4800.0, 1600.0),
    );
    let views = [
        OutputView::at(crate::shell::render::OutputId(0), (0, 0), head),
        OutputView::at(crate::shell::render::OutputId(1), (3840, 0), head),
    ];
    for (index, view) in views.iter().enumerate() {
        let (_, stats) = paint_output_offscreen(&mut bridge, &scene, view, full_frame(1));
        assert_eq!(
            (stats.render_attempts, bridge.last_render_attempts()),
            (1, 1),
            "head {index} renders in one pass, not in bands"
        );
    }
    assert!(
        bridge.view_is_whole_scene(&views[0]),
        "the head at the origin renders straight into the bridge's target"
    );
    assert!(bridge.target.band.is_none(), "no band target was needed");
    assert!(
        bridge
            .output_surfaces
            .iter()
            .all(|(_, surface)| surface.size == head && surface.band.is_none()),
        "the other head's surface is its own size and never banded"
    );
    assert_eq!(bridge.size(), head, "no target spans both heads");
}

#[test]
fn an_undamaged_origin_output_keeps_its_image_in_the_bridge_target() {
    // The bridge's target is the left output's: damage on the right one leaves it as it is.
    let mut bridge = required_bridge(100, 100);
    let scene = Scene::new();
    let left = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (100, 100));
    let right = OutputView::at(crate::shell::render::OutputId(1), (100, 0), (100, 100));
    assert!(bridge.view_is_whole_scene(&left));
    paint_output_pixels(&mut bridge, &scene, &left, 1);
    paint_output_pixels(&mut bridge, &scene, &right, 1);
    let right_only = [vello::FineDamageRect {
        x: 120,
        y: 10,
        width: 20,
        height: 20,
    }];
    let damage = SceneDamage {
        revision: 2,
        previous_revision: Some(1),
        regions: Some(&right_only),
    };
    assert!(
        bridge.keep_unchanged_output(&left, damage),
        "damage on the other output neither renders nor flips this one"
    );
    assert!(
        !bridge.keep_unchanged_output(&right, damage),
        "the damaged output renders"
    );
    assert_eq!(
        bridge.target.last_successful_revision,
        Some(2),
        "the kept target holds the new revision, so the next frame repairs it sparsely"
    );
}
