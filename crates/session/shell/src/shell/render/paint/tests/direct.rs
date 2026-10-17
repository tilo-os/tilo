use super::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn bridge_paint_fills_sized_buffers_and_xrgb_output_matches_swizzled_paint() {
    // Phase `bridge_paint_sizes_and_fills_its_buffer`.
    {
        // Phase `bridge_paint_returns_non_empty_buffer_for_non_empty_scene`.
        {
            let mut bridge = required_bridge(64, 64);
            // Fill with cyan so the buffer is definitely not the (dark) base color.
            let scene = rect_scene(64, 64, Color::from_rgb8(0, 255, 255));
            let bytes = bridge.paint(&scene).expect("paint");
            assert_eq!(bytes.len(), 64 * 64 * 4);
            // Find at least one non-background pixel (background is the bridge's default base color, ~0x1A,0x1B,0x26).
            let any_non_bg = bytes
                .chunks_exact(4)
                .any(|p| p[0] != 0x1A || p[1] != 0x1B || p[2] != 0x26);
            assert!(
                any_non_bg,
                "expected at least one non-background pixel after painting a cyan rect"
            );
            assert_eq!(bridge.last_phase_timing(), None, "profiling defaults off");
        }

        // Phase `bridge_resize_changes_buffer_size`.
        {
            let mut bridge = required_bridge(1600, 900);
            let bytes = bridge.paint(&Scene::new()).expect("paint at initial size");
            assert_eq!(bytes.len(), 1600 * 900 * 4);

            // Resize to 1280x720 and verify the next paint returns the new buffer size.
            bridge.resize(1280, 720).expect("resize");
            assert_eq!(bridge.size(), (1280, 720));
            let bytes = bridge.paint(&Scene::new()).expect("paint after resize");
            assert_eq!(bytes.len(), 1280 * 720 * 4);
        }

        // Phase `bridge_paint_uses_base_color_for_empty_scene`.
        {
            let mut bridge = required_bridge(32, 32);
            // Pick a distinctive base color so the empty background is distinguishable from any other clear.
            bridge.set_base_color(Color::from_rgb8(0x12, 0x34, 0x56));
            let bytes = bridge.paint(&Scene::new()).expect("paint empty");
            assert_eq!(bytes.len(), 32 * 32 * 4);
            for (i, p) in bytes.chunks_exact(4).enumerate() {
                assert_eq!(
                    (p[0], p[1], p[2]),
                    (0x12, 0x34, 0x56),
                    "pixel {i} expected base color, got ({:#x}, {:#x}, {:#x})",
                    p[0],
                    p[1],
                    p[2]
                );
            }
        }

        // Phase `bridge_handles_non_aligned_width_via_padding`.
        {
            // Width=33 → tight stride=132, padded to 256 — verifies the row-pitch stripping in `paint`.
            let mut bridge = required_bridge(33, 16);
            bridge.set_base_color(Color::from_rgb8(0xAA, 0xBB, 0xCC));
            let bytes = bridge.paint(&Scene::new()).expect("paint non-aligned");
            assert_eq!(bytes.len(), 33 * 16 * 4);
            for p in bytes.chunks_exact(4) {
                assert_eq!((p[0], p[1], p[2]), (0xAA, 0xBB, 0xCC));
            }
        }
    }

    // Phase `xrgb_output_matches_swizzled_paint_and_honours_pitch_and_diagnostics`.
    {
        // Phase `paint_into_xrgb8888_matches_paint_swizzled`.
        {
            // The direct XRGB path must produce the same pixels as tight-RGBA `paint()` output
            // swizzled to XRGB8888.
            let mut bridge = required_bridge(40, 24);
            let scene = rect_scene(40, 24, Color::from_rgb8(10, 200, 90));

            let rgba = bridge.paint(&scene).expect("paint").to_vec();
            let mut expected = vec![0u8; 40 * 24 * 4];
            for (i, p) in rgba.chunks_exact(4).enumerate() {
                let d = &mut expected[i * 4..i * 4 + 4];
                d[0] = p[2]; // B
                d[1] = p[1]; // G
                d[2] = p[0]; // R
                d[3] = 0xFF; // X
            }

            let pitch = 40 * 4;
            let mut dst = vec![0u8; pitch * 24];
            bridge
                .paint_into_xrgb8888(&scene, &mut dst, pitch)
                .expect("paint_into_xrgb8888");
            assert_eq!(
                dst, expected,
                "direct XRGB output must equal swizzled paint()"
            );
        }

        // Phase `paint_into_xrgb8888_respects_destination_pitch`.
        {
            // A destination whose pitch exceeds width*4 (the common DRM dumb-buffer case) must leave trailing padding untouched and place each row at `row * dst_pitch`.
            let mut bridge = required_bridge(8, 4);
            bridge.set_base_color(Color::from_rgb8(0x11, 0x22, 0x33));
            let pitch = 64;
            let mut dst = vec![0xEEu8; pitch * 4];
            bridge
                .paint_into_xrgb8888(&Scene::new(), &mut dst, pitch)
                .expect("paint into padded dst");
            for row in 0..4usize {
                for x in 0..8usize {
                    let o = row * pitch + x * 4;
                    assert_eq!(
                        (dst[o], dst[o + 1], dst[o + 2]),
                        (0x33, 0x22, 0x11),
                        "row {row} px {x} should be base color in BGR order"
                    );
                }
                assert_eq!(
                    dst[row * pitch + 8 * 4],
                    0xEE,
                    "row {row} padding untouched"
                );
            }
        }

        // Phase `diagnostic_zero_pitch_records_readback_but_no_output_bytes`.
        {
            let mut bridge = required_bridge(8, 4);
            bridge.set_phase_timing_enabled(true);
            bridge
                .paint_into_xrgb8888(&Scene::new(), &mut [], 0)
                .expect("zero-pitch diagnostic paint");
            let timing = bridge
                .last_phase_timing()
                .expect("enabled diagnostic timing");
            assert!(timing.readback_bytes > 0, "the GPU copy still completed");
            assert_eq!(timing.cpu_output_bytes, 0, "no destination was writable");
        }

        // Phase `diagnostic_xrgb_counts_written_bytes_without_changing_pixels`.
        {
            let mut bridge = required_bridge(8, 4);
            bridge.set_phase_timing_enabled(true);
            let scene = rect_scene(8, 4, Color::from_rgb8(0x12, 0x34, 0x56));
            let mut destination = vec![0_u8; 8 * 4 * 4];
            bridge
                .paint_into_xrgb8888(&scene, &mut destination, 8 * 4)
                .expect("diagnostic XRGB paint");
            let timing = bridge
                .last_phase_timing()
                .expect("enabled diagnostic timing");
            assert_eq!(timing.cpu_output_bytes, 8 * 4 * 4);
            assert_eq!(timing.readback_bytes, 8 * 4 * 4);
            for pixel in destination.chunks_exact(4) {
                assert_eq!(pixel, &[0x56, 0x34, 0x12, 0xFF]);
            }
        }

        // Phase `zero_destination_pitch_unmaps_before_the_next_frame`.
        {
            let mut bridge = required_bridge(8, 4);
            let scene = rect_scene(8, 4, Color::from_rgb8(0x12, 0x34, 0x56));
            bridge
                .paint_into_xrgb8888(&scene, &mut [], 0)
                .expect("zero-pitch paint");

            let mut destination = vec![0_u8; 8 * 4 * 4];
            bridge
                .paint_into_xrgb8888(&scene, &mut destination, 8 * 4)
                .expect("paint after zero-pitch paint");
            for pixel in destination.chunks_exact(4) {
                assert_eq!(pixel, &[0x56, 0x34, 0x12, 0xFF]);
            }
        }

        // Phase `overflow_recovery_retries_before_xrgb_output`.
        {
            let (width, height) = (384, 256);
            let mut xrgb_bridge = required_bridge(width, height);
            let scene = opaque_rect_stack_scene(width, height);
            let pitch = width as usize * 4;
            let mut xrgb = vec![0_u8; pitch * height as usize];
            xrgb_bridge
                .paint_into_xrgb8888(&scene, &mut xrgb, pitch)
                .expect("cold opaque-stack XRGB paint");
            assert!(
                xrgb_bridge.last_render_attempts() >= 2,
                "512 opaque rects at 384x256 must exercise bounded bump recovery"
            );
            for (index, pixel) in xrgb.chunks_exact(4).enumerate() {
                // Fill 511 is the visible opaque top layer: RGB=(0xFF, 0xFD, 0xA5).
                assert_eq!(pixel, &[0xA5, 0xFD, 0xFF, 0xFF], "XRGB pixel {index}");
            }

            // Use a separate cold renderer so this comparison cannot inherit the XRGB bridge's grown
            // budget. Both public outputs must agree after their own recovery path.
            let mut rgba_bridge = required_bridge(width, height);
            let rgba = rgba_bridge
                .paint(&scene)
                .expect("cold opaque-stack RGBA paint")
                .to_vec();
            assert!(rgba_bridge.last_render_attempts() >= 2);
            for (index, (rgba, xrgb)) in rgba.chunks_exact(4).zip(xrgb.chunks_exact(4)).enumerate()
            {
                assert_eq!(rgba, &[0xFF, 0xFD, 0xA5, 0xFF], "RGBA pixel {index}");
                assert_eq!(xrgb, &[rgba[2], rgba[1], rgba[0], 0xFF], "pixel {index}");
            }
        }
    }
}

#[test]
fn targets_past_the_bin_limit_render_in_bands_of_whole_bin_rows() {
    assert_eq!(RetainedGpuTarget::band_rows((1920, 1080)), None);
    assert_eq!(
        RetainedGpuTarget::band_rows((5120, 2880)),
        None,
        "a 5K panel alone is 240 bins and renders whole"
    );
    // 5120x2880 beside 1920x1080: 28 bins across, so nine bin rows per band.
    assert_eq!(RetainedGpuTarget::band_rows((7040, 2880)), Some(2304));
    assert_eq!(RetainedGpuTarget::band_rows((7680, 4320)), Some(2048));
    assert_eq!(RetainedGpuTarget::band_rows((16384, 16384)), Some(1024));
}

/// The multi-output presenter paints one scene spanning every output. For a 5120x2880
/// panel beside a 1920x1080 one that is 7040x2880, 336 of Vello's 256x256 bins against the
/// 256 its binning and coarse stages address: the bins past 255 were clamped onto others,
/// and the bottom of the large panel drew somebody else's content. Every bin must show its
/// own.
#[test]
fn a_combined_target_past_the_bin_limit_draws_every_bin() {
    let (width, height) = (7040_u32, 2880_u32);
    let mut bridge = required_bridge(width, height);
    bridge.wait_for_gpu_completion_for_test();
    let colour = |bx: u32, by: u32| {
        Color::from_rgb8(
            u8::try_from(bx * 9).expect("fits"),
            u8::try_from(by * 20).expect("fits"),
            0xC8,
        )
    };
    let mut scene = rect_scene(width, height, Color::BLACK);
    let bins = (width.div_ceil(256), height.div_ceil(256));
    for by in 0..bins.1 {
        for bx in 0..bins.0 {
            // The last column and row of bins are partial: 7040 and 2880 are not multiples
            // of 256, so each probe sits within the first 64 pixels of its bin.
            let (x, y) = (f64::from(bx * 256 + 16), f64::from(by * 256 + 16));
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                colour(bx, by),
                None,
                &vello::kurbo::Rect::new(x, y, x + 32.0, y + 32.0),
            );
        }
    }
    let rgba = bridge.paint(&scene).expect("combined paint").to_vec();
    let mut wrong = Vec::new();
    for by in 0..bins.1 {
        for bx in 0..bins.0 {
            let (x, y) = ((bx * 256 + 32) as usize, (by * 256 + 32) as usize);
            let at = (y * width as usize + x) * 4;
            let want = colour(bx, by).to_rgba8();
            if rgba[at..at + 4] != [want.r, want.g, want.b, 0xFF] {
                wrong.push((bx, by, rgba[at..at + 4].to_vec()));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "bins drawn wrong (bx, by, pixel): {wrong:?}"
    );
    assert!(
        bridge.last_full_render_attempts() >= 2,
        "a 336-bin target renders as more than one band"
    );
}

/// The black-screen loop: a frame failed and poisoned the target, the display
/// configuration changed, and every later frame asked the poisoned target to resize, was
/// refused with "retained GPU target must be reconstructed", and presented nothing.
#[test]
fn a_poisoned_target_is_rebuilt_at_the_size_a_resize_asks_for() {
    let mut bridge = required_bridge(128, 128);
    let mut test = completion_clock();
    test.failure = Some(WaitPhase::Fence);
    bridge.target.completion_test = Some(test);
    let mut pixels = vec![0; 128 * 128 * 4];
    assert!(matches!(
        bridge.paint_into_xrgb8888(&rect_scene(128, 128, Color::WHITE), &mut pixels, 512),
        Err(BridgeError::CompletionTimeout {
            phase: CompletionPhase::Fence
        })
    ));
    assert!(
        bridge.target.poisoned,
        "the fixture must reach the poisoned state"
    );
    let generation = bridge.xrgb_history_generation;

    bridge
        .resize(256, 96)
        .expect("a poisoned target is rebuilt, not refused");
    assert_eq!(bridge.size(), (256, 96));
    assert!(!bridge.target.poisoned);
    assert_ne!(
        bridge.xrgb_history_generation, generation,
        "a rebuilt target is a new destination history"
    );
    bridge.wait_for_gpu_completion_for_test();
    bridge.recovery_not_before = Some(
        Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock is past its first millisecond"),
    );
    let red = Color::from_rgb8(0xE0, 0x10, 0x20);
    let rgba = bridge
        .paint(&rect_scene(256, 96, red))
        .expect("the rebuilt target paints")
        .to_vec();
    assert_eq!(rgba.len(), 256 * 96 * 4);
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        assert_eq!(pixel, &[0xE0, 0x10, 0x20, 0xFF], "pixel {index}");
    }
}

/// A stand-in for an imported scanout image: an SDR target the test can read back.
#[cfg(target_os = "linux")]
fn direct_target(
    bridge: &VelloDrmBridge,
    width: u32,
    height: u32,
) -> ui::dmabuf_import::ImportedRenderTarget {
    let (device, _) = bridge.gpu();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("direct-scanout-test-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    ui::dmabuf_import::ImportedRenderTarget {
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        texture,
        format: wgpu::TextureFormat::Bgra8Unorm,
        width,
        height,
        stride: width.saturating_mul(4),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn direct_paint_phase_timing_observes_real_gpu_wait_without_pixel_readback() {
    let mut bridge = required_bridge(128, 96);
    let target = direct_target(&bridge, 128, 96);
    let scene = rect_scene(128, 96, Color::WHITE);
    let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
    bridge
        .paint_imported_until(
            &scene,
            version(1, None, None),
            &target,
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .unwrap();
    assert!(
        bridge.last_phase_timing().is_none(),
        "ordinary bridge profiling stays off"
    );
    bridge.set_phase_timing_enabled(true);
    let stats = bridge
        .paint_imported_until(
            &scene,
            version(2, None, None),
            &target,
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .unwrap();
    let timing = bridge
        .last_phase_timing()
        .expect("actual direct GPU paint must expose its phases");
    assert!(
        timing.submit_us > 0,
        "actual CPU encoding and queue submission"
    );
    assert!(
        timing.completed_readback_map_us > 0,
        "actual GPU completion poll"
    );
    assert_eq!(timing.readback_bytes, 0);
    assert_eq!(timing.cpu_output_bytes, 0);
    assert_eq!(timing.cpu_copy_swizzle_us, 0);
    assert_eq!(stats.framebuffer_copy_bytes, 0);
    assert_eq!(stats.completion_markers, 1);
    assert_eq!(bridge.take_phase_timing(), Some(timing));
    assert!(
        bridge.take_phase_timing().is_none(),
        "timing is consumed exactly once"
    );
    assert!(
        bridge
            .paint_imported_until(
                &scene,
                version(3, None, None),
                &target,
                Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .expect("monotonic clock permits an expired deadline"),
                white,
                &[]
            )
            .is_err()
    );
    assert!(
        bridge.last_phase_timing().is_none(),
        "early refusal never reuses preceding frame timing"
    );
    bridge.set_phase_timing_enabled(false);
    bridge
        .paint_imported_until(
            &scene,
            version(4, None, None),
            &target,
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .unwrap();
    assert!(bridge.last_phase_timing().is_none());
}

/// Runs the ordinary direct paint, not a diagnostic helper or a fake GPU sample.
#[cfg(target_os = "linux")]
#[test]
fn direct_gpu_stage_records_keep_actual_submission_identity_and_coverage() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_PROFILE_GPU_STAGES", "0");
    let ordinary = required_bridge(128, 96);
    assert!(ordinary.dispatch_stage_observer.is_none());
    drop(ordinary);
    env.set("TILO_PROFILE_GPU_STAGES", "1");
    env.set("VELLO_GPU_TIMESTAMPS", "1");
    env.set("VELLO_GPU_DISPATCH_TIMESTAMPS", "1");
    let mut bridge = required_bridge(128, 96);
    let target = direct_target(&bridge, 128, 96);
    let mut samples = 0;
    for revision in 1..=12 {
        bridge
            .paint_imported_until(
                &rect_scene(128, 96, Color::WHITE),
                version(revision, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                &[],
            )
            .expect("production direct GPU paint");
        let row = bridge
            .dispatch_stage_observer
            .as_ref()
            .unwrap()
            .last_record
            .as_ref()
            .unwrap();
        assert_eq!(row["process_id"], std::process::id());
        assert_eq!(
            row["renderer_generation"],
            bridge.xrgb_history_generation.unwrap()
        );
        assert_eq!(row["revision"], revision);
        assert_eq!(row["coverage"]["attempts"], revision);
        assert_eq!(row["render_succeeded"], true);
        assert_eq!(
            row["coverage"]["matched_current"].as_u64().unwrap()
                + row["coverage"]["missing_current"].as_u64().unwrap(),
            revision
        );
        if let Some(sample_id) = row["sample_submission_id"].as_u64() {
            samples += 1;
            let latest = row["latest_attempted_submission_id"].as_u64().unwrap();
            assert!(sample_id <= latest);
            assert_eq!(row["matched_current"], sample_id == latest);
            assert_eq!(row["stale_sample"], sample_id != latest);
            assert!(!row["stages"].as_array().unwrap().is_empty());
        } else {
            assert_eq!(row["missing_current"], true);
            assert!(row["stages"].as_array().unwrap().is_empty());
        }
        bridge.wait_for_gpu_completion_for_test();
    }
    assert!(
        samples > 0,
        "the diagnostic must observe an actual GPU stage sample"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn direct_gpu_stage_failed_paints_and_recovery_keep_renderer_identity() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_PROFILE_GPU_STAGES", "1");
    env.set("VELLO_GPU_TIMESTAMPS", "1");
    env.set("VELLO_GPU_DISPATCH_TIMESTAMPS", "1");
    let mut bridge = required_bridge(128, 96);
    let target = direct_target(&bridge, 128, 96);
    let scene = rect_scene(128, 96, Color::WHITE);
    let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
    bridge
        .paint_imported_until(
            &scene,
            version(1, None, None),
            &target,
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .expect("initial ordinary direct paint");
    let old_generation = bridge.xrgb_history_generation;
    let expired = bridge.paint_imported_until(
        &scene,
        version(2, None, None),
        &target,
        Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("monotonic clock permits an expired deadline"),
        white,
        &[],
    );
    assert!(expired.is_err(), "an expired ordinary paint must fail");
    let row = bridge
        .dispatch_stage_observer
        .as_ref()
        .unwrap()
        .last_record
        .as_ref()
        .unwrap();
    assert_eq!(row["revision"], 2);
    assert_eq!(row["render_succeeded"], false);
    assert!(row["expected_current_submission_id"].is_null());
    assert_eq!(row["matched_current"], false);
    assert_eq!(row["missing_current"], true);
    // An actual wgpu validation error enters the ordinary recovery path.
    let (device, _) = bridge.gpu();
    let refused = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stage-observer-recovery-test"),
        size: device.limits().max_buffer_size.saturating_add(4),
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    drop(refused);
    assert!(
        bridge
            .paint_imported_until(
                &scene,
                version(3, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .is_err()
    );
    bridge
        .paint_imported_until(
            &scene,
            version(4, None, None),
            &target,
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .expect("ordinary paint reconstructs after the reported GPU error");
    let row = bridge
        .dispatch_stage_observer
        .as_ref()
        .unwrap()
        .last_record
        .as_ref()
        .unwrap();
    assert_eq!(row["revision"], 4);
    assert_ne!(bridge.xrgb_history_generation, old_generation);
    assert_eq!(
        row["renderer_generation"],
        bridge.xrgb_history_generation.unwrap()
    );
}

/// The direct swapchain alternates between two scanout images, so the image a frame lands in
/// holds the revision before last. The renderer's retained target holds the previous one, and
/// the image receives a whole copy of it: a typed character must repaint sparsely, not the
/// full panel (a dense 2944x1840 frame took ~45 ms on a Snapdragon X Elite).
#[cfg(target_os = "linux")]
#[test]
fn direct_frames_stay_sparse_across_the_swapchain_and_show_the_whole_scene() {
    let (width, height) = (128, 96);
    let mut bridge = required_bridge(width, height);
    bridge.wait_for_gpu_completion_for_test();
    let slots = [
        direct_target(&bridge, width, height),
        direct_target(&bridge, width, height),
    ];
    let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
    let region = vello::FineDamageRect {
        x: 16,
        y: 16,
        width: 16,
        height: 16,
    };
    let red = Color::from_rgb8(0xE0, 0x10, 0x20);
    let mut typed = rect_scene(width, height, Color::WHITE);
    typed.append(&region_scene(region, red), None);

    bridge
        .paint_imported_until(
            &rect_scene(width, height, Color::WHITE),
            version(1, None, None),
            &slots[0],
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .expect("the first direct frame renders");
    let stats = bridge
        .paint_imported_until(
            &typed,
            version(2, Some(1), Some(&[region])),
            &slots[1],
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .expect("the sparse direct frame renders");
    if bridge.target.renderer.sparse_fine_supported() {
        assert!(
            stats.sparse_attempts > 0 && stats.full_attempts == 0,
            "a damaged region repaints sparsely: {stats:?}"
        );
    }

    let (device, queue) = bridge.gpu();
    let (buffer, stride) = VelloDrmBridge::allocate_staging(device, width, height);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    VelloDrmBridge::append_full_copy(
        &mut encoder,
        &slots[1].texture,
        &buffer,
        stride,
        width,
        height,
    );
    queue.submit([encoder.finish()]);
    let mapped = VelloDrmBridge::map_device_readback(device, &buffer).expect("slot readback");
    let pixel = |x: u32, y: u32| {
        let at = (y * stride + x * 4) as usize;
        mapped[at..at + 4].to_vec()
    };
    assert_eq!(
        pixel(20, 20),
        vec![0x20, 0x10, 0xE0, 0xFF],
        "the typed region"
    );
    assert_eq!(
        pixel(100, 80),
        vec![0xFF; 4],
        "the rest of the slot comes from the retained frame, not the slot's stale image"
    );
    drop(mapped);
    buffer.unmap();
}

/// A client frame kept on the GPU is drawn by the presenting renderer from its texture, and
/// by a renderer on another device from pixels read back on demand: both show the client.
#[cfg(target_os = "linux")]
#[test]
fn a_client_frame_on_the_gpu_reaches_every_renderer() {
    let (width, height) = (64, 48);
    let mut presenting = required_bridge(width, height);
    presenting.wait_for_gpu_completion_for_test();
    let (device, queue) = presenting.gpu();
    let red = [0xE0, 0x10, 0x20, 0xFF];
    let generation = presenting
        .target
        .present_generation
        .expect("a presenting device");
    let frame =
        crate::shell::gpu_frames::frame_for_test(device, queue, generation, (40, 30), red, 9_001);
    let image = frame.image((4, 2, 32, 24));
    assert!(image.data.is_empty(), "the frame carries no CPU pixels");
    let mut scene = rect_scene(width, height, Color::WHITE);
    scene.fill(
        Fill::NonZero,
        Affine::translate((8.0, 8.0)),
        &vello::peniko::ImageBrush::new(image),
        None,
        &vello::kurbo::Rect::new(0.0, 0.0, 32.0, 24.0),
    );
    let target = direct_target(&presenting, width, height);
    presenting
        .paint_imported_until(
            &scene,
            version(1, None, None),
            &target,
            tilo_test_env::hang_deadline(),
            crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
            &[],
        )
        .expect("the presenting renderer draws the frame");
    let (device, queue) = presenting.gpu();
    let (buffer, stride) = VelloDrmBridge::allocate_staging(device, width, height);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    VelloDrmBridge::append_full_copy(
        &mut encoder,
        &target.texture,
        &buffer,
        stride,
        width,
        height,
    );
    queue.submit([encoder.finish()]);
    let mapped = VelloDrmBridge::map_device_readback(device, &buffer).expect("readback");
    let at = (20 * stride + 20 * 4) as usize;
    assert_eq!(
        &mapped[at..at + 4],
        &[0x20, 0x10, 0xE0, 0xFF],
        "BGRA target"
    );
    drop(mapped);
    buffer.unmap();

    let mut elsewhere = required_bridge(width, height);
    elsewhere.wait_for_gpu_completion_for_test();
    let rgba = elsewhere
        .paint(&scene)
        .expect("a renderer without the texture still draws the frame")
        .to_vec();
    let at = ((20 * width + 20) * 4) as usize;
    assert_eq!(&rgba[at..at + 4], &red);
}

/// One direct frame that missed its deadline -- a cold shader cache, a busy GPU -- poisons
/// the target. The retry after the readback presenter stood in must render again, or a single
/// slow frame keeps the output on the per-frame readback for the rest of the session.
#[cfg(target_os = "linux")]
#[test]
fn a_direct_frame_after_a_missed_deadline_renders_again() {
    let mut bridge = required_bridge(128, 96);
    let target = direct_target(&bridge, 128, 96);
    let scene = rect_scene(128, 96, Color::WHITE);
    let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;

    let mut test = completion_clock();
    test.failure = Some(WaitPhase::Marker);
    let deadline = test.start + Duration::from_secs(1);
    bridge.target.completion_test = Some(test);
    assert!(matches!(
        bridge.paint_imported_until(
            &scene,
            version(1, None, None),
            &target,
            deadline,
            white,
            &[]
        ),
        Err(BridgeError::DeadlineExceeded)
    ));
    assert!(
        bridge.target.poisoned,
        "the fixture must reach the poisoned state"
    );

    let stats = bridge
        .paint_imported_until(
            &scene,
            version(2, None, None),
            &target,
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .expect("the retried direct frame renders");
    assert_eq!(stats.revision, 2);
    assert!(!bridge.target.poisoned);
}

/// wgpu answers an uncaptured validation error by panicking, and the compositor aborts on
/// panic: one oversized binding on the present thread ended the session and every client
/// in it. The bridge's device must hand such an error to the bridge instead, which rebuilds
/// its renderer and keeps presenting.
#[test]
fn an_uncaptured_wgpu_error_costs_a_renderer_not_the_session() {
    let mut bridge = required_bridge(64, 64);
    bridge.wait_for_gpu_completion_for_test();
    let limits = bridge.target.device.limits();
    let refused = bridge.target.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("past max_buffer_size"),
        size: limits.max_buffer_size.saturating_add(4),
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    drop(refused);
    assert!(
        bridge.device_errors.0.lock().expect("latch").is_some(),
        "the validation error reached the bridge, not a panic"
    );
    let generation = bridge.xrgb_history_generation;
    let green = Color::from_rgb8(0x10, 0xC0, 0x40);
    let rgba = bridge
        .paint(&rect_scene(64, 64, green))
        .expect("the next frame renders on a rebuilt renderer")
        .to_vec();
    assert_ne!(bridge.xrgb_history_generation, generation, "rebuilt");
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        assert_eq!(pixel, &[0x10, 0xC0, 0x40, 0xFF], "pixel {index}");
    }
}

/// wgpu's default limits cap a storage binding at 128 MiB whatever the hardware binds; a
/// 5120x2880 output's scratch buffers need more. The bridge's device must carry what its
/// adapter supports.
#[test]
fn the_bridge_device_binds_what_its_adapter_supports() {
    let bridge = required_bridge(64, 64);
    let adapter = bridge.context.devices[bridge.dev_id].adapter.limits();
    let device = bridge.target.device.limits();
    assert_eq!(
        device.max_storage_buffer_binding_size,
        adapter.max_storage_buffer_binding_size
    );
    assert_eq!(device.max_buffer_size, adapter.max_buffer_size);
}
