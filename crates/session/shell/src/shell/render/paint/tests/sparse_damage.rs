use super::*;

/// 200x100 desktop: a blue ground with a red "window" tile and a caret drawn in
/// `caret`. What a frame typing in the window changes.
fn window_scene(caret: Color) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(0, 0, 255),
        None,
        &vello::kurbo::Rect::new(0.0, 0.0, 200.0, 100.0),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(255, 0, 0),
        None,
        &vello::kurbo::Rect::new(0.0, 0.0, 130.0, 100.0),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        caret,
        None,
        &vello::kurbo::Rect::new(40.0, 20.0, 42.0, 36.0),
    );
    scene
}

#[test]
fn the_sparse_policy_admits_a_window_and_merges_long_lists_instead_of_refusing_them() {
    let size = (2944, 1840);
    // A 1472x1840 window is half of the output; 2208 px wide is three quarters.
    for (width, admitted) in [(1472, true), (2200, true), (2208, false), (2944, false)] {
        let window = [rect(0, 0, width, 1840)];
        assert_eq!(
            VelloDrmBridge::normalize_damage(&window, size).is_some(),
            admitted,
            "a window {width} px wide of {} px",
            size.0
        );
    }
    assert!(damage_is_profitable(0, 1));
    assert!(!damage_is_profitable(1, 0));

    // 200 distinct caret-sized rectangles (what skipped frames pile up) merge to the
    // dispatch limit and still cover every one of them.
    let carets: Vec<_> = (0..200_u32)
        .map(|i| rect(16 * (i % 50), 16 * (i / 50), 16, 16))
        .collect();
    let merged = VelloDrmBridge::normalize_damage(&carets, size)
        .expect("a long list of small rectangles is repaired sparsely");
    assert!(merged.len() <= MAX_DAMAGE_RECTS, "{} rects", merged.len());
    for caret in &carets {
        assert!(
            merged.iter().any(|m| {
                m.x <= caret.x
                    && m.y <= caret.y
                    && m.x + m.width >= caret.x + caret.width
                    && m.y + m.height >= caret.y + caret.height
            }),
            "{caret:?} is covered"
        );
    }
    // The same caret repeated is one rectangle.
    let repeated = vec![rect(100, 100, 2, 16); 500];
    assert_eq!(
        coalesce_damage(repeated, MAX_DAMAGE_RECTS),
        [rect(100, 100, 2, 16)]
    );
}

#[test]
fn overlapping_damage_renders_sparsely_while_its_union_leaves_a_quarter_untouched() {
    let size = (2944, 1840);
    // Two half-output windows overlapping: their areas add up to the whole output, but together
    // they cover 2172 of its 2944 columns, under three quarters.
    let overlapping = [rect(0, 0, 1472, 1840), rect(700, 0, 1472, 1840)];
    assert_eq!(
        VelloDrmBridge::normalize_damage(&overlapping, size).as_deref(),
        Some(&overlapping[..]),
        "overlapping windows stay two exact rectangles and render sparsely"
    );
    // Covering three quarters of the columns is still a dense frame.
    let wider = [rect(0, 0, 1472, 1840), rect(736, 0, 1472, 1840)];
    assert!(VelloDrmBridge::normalize_damage(&wider, size).is_none());
    // A buffer-age history repeating one window many times over is that window.
    let repeated = vec![rect(100, 100, 1400, 1700); 40];
    assert_eq!(
        VelloDrmBridge::normalize_damage(&repeated, size).as_deref(),
        Some(&[rect(100, 100, 1400, 1700)][..])
    );
}

#[test]
fn a_window_sized_typing_update_renders_sparsely_and_equals_a_full_repaint() {
    let mut bridge = required_bridge(200, 100);
    let mut fresh = required_bridge(200, 100);
    let view = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (200, 100));
    let before = window_scene(Color::from_rgb8(255, 255, 255));
    let after = window_scene(Color::from_rgb8(0, 0, 0));
    paint_output_pixels(&mut bridge, &before, &view, 1);
    // The window tile is 65% of the output: damage as large as the window, which used to
    // pass the "at least half" gate and render the whole output.
    let window = [rect(0, 0, 130, 100)];
    let (stats, pixels) = paint_output_damage(
        &mut bridge,
        &after,
        &view,
        SceneDamage {
            revision: 2,
            previous_revision: Some(1),
            regions: Some(&window),
        },
    );
    assert_eq!(
        (
            stats.sparse_attempts,
            stats.full_attempts,
            stats.damage_admitted
        ),
        (1, 0, true),
        "typing in a window repaints the window, not the output"
    );
    let expected = paint_output_pixels(&mut fresh, &after, &view, 1);
    assert_eq!(
        pixels, expected,
        "the presented pixels equal a full repaint"
    );
}

#[test]
fn a_long_run_of_skipped_frames_owes_merged_damage_and_stays_sparse() {
    let mut bridge = required_bridge(200, 100);
    let mut fresh = required_bridge(200, 100);
    let view = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (200, 100));
    let first = window_scene(Color::from_rgb8(255, 255, 255));
    let last = window_scene(Color::from_rgb8(0, 0, 0));
    paint_output_pixels(&mut bridge, &first, &view, 1);
    // 150 skipped frames, 100 distinct specks of the upper 40%, then the caret frame.
    for revision in 2..152_u64 {
        let (x, y) = (4 * (revision % 50), 2 * (revision % 20));
        let strip = [rect(
            u32::try_from(x).unwrap_or(0),
            u32::try_from(y).unwrap_or(0),
            4,
            2,
        )];
        assert!(
            bridge.defer_output(
                &view,
                SceneDamage {
                    revision,
                    previous_revision: revision.checked_sub(1),
                    regions: Some(&strip),
                },
            ),
            "the debt of skipped frame {revision} is kept, merged"
        );
    }
    assert!(bridge.target.owed_damage.len() <= MAX_DAMAGE_RECTS);
    let caret = [rect(32, 16, 16, 32)];
    let (stats, pixels) = paint_output_damage(
        &mut bridge,
        &last,
        &view,
        SceneDamage {
            revision: 152,
            previous_revision: Some(151),
            regions: Some(&caret),
        },
    );
    assert_eq!(
        (stats.sparse_attempts, stats.full_attempts),
        (1, 0),
        "the merged debt is repaired sparsely"
    );
    let expected = paint_output_pixels(&mut fresh, &last, &view, 1);
    assert_eq!(
        pixels, expected,
        "the presented pixels equal a full repaint"
    );
}

#[test]
fn a_deferred_output_repairs_its_owed_damage_sparsely() {
    // The right output skips revision 2 (its flip is pending), whose damage falls on it.
    // Revision 3 damages only the left output; the right one must still show revision 2.
    let mut bridge = required_bridge(100, 100);
    let right = OutputView::at(crate::shell::render::OutputId(1), (100, 0), (100, 100));
    let blue = Color::from_rgb8(0, 0, 255);
    let red = Color::from_rgb8(255, 0, 0);
    let mut before = Scene::new();
    before.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        blue,
        None,
        &vello::kurbo::Rect::new(0.0, 0.0, 200.0, 100.0),
    );
    let mut after = Scene::new();
    after.append(&before, None);
    after.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        red,
        None,
        &vello::kurbo::Rect::new(110.0, 10.0, 130.0, 30.0),
    );
    paint_output_pixels(&mut bridge, &before, &right, 1);
    let on_right = [vello::FineDamageRect {
        x: 110,
        y: 10,
        width: 20,
        height: 20,
    }];
    assert!(
        bridge.defer_output(
            &right,
            SceneDamage {
                revision: 2,
                previous_revision: Some(1),
                regions: Some(&on_right),
            },
        ),
        "a surface holding the previous revision records the skipped damage"
    );
    let on_left = [vello::FineDamageRect {
        x: 10,
        y: 50,
        width: 10,
        height: 10,
    }];
    let next = SceneDamage {
        revision: 3,
        previous_revision: Some(2),
        regions: Some(&on_left),
    };
    assert!(
        !bridge.keep_unchanged_output(&right, next),
        "owed damage makes the output render although this frame's damage is elsewhere"
    );
    let (stats, pixels) = paint_output_damage(&mut bridge, &after, &right, next);
    assert_eq!(
        (stats.sparse_attempts, stats.full_attempts),
        (1, 0),
        "the owed damage is repaired sparsely"
    );
    let pixel = |x: usize, y: usize| {
        pixels
            .chunks_exact(4)
            .nth(y.saturating_mul(100).saturating_add(x))
            .expect("the pixel lies on the output")
            .to_vec()
    };
    // The scanout stand-in is BGRA.
    assert_eq!(
        pixel(20, 20),
        [0, 0, 255, 255],
        "the skipped frame's change shows"
    );
    assert_eq!(pixel(60, 60), [255, 0, 0, 255], "the rest is unchanged");

    // Pixels that do not hold the previous revision cannot owe a frame: the output's next
    // render is full.
    assert!(!bridge.defer_output(
        &right,
        SceneDamage {
            revision: 9,
            previous_revision: Some(7),
            regions: Some(&on_right),
        },
    ));
    let (stats, _) = paint_output_damage(
        &mut bridge,
        &after,
        &right,
        SceneDamage {
            revision: 10,
            previous_revision: Some(9),
            regions: Some(&on_left),
        },
    );
    assert_eq!(
        stats.sparse_attempts, 0,
        "an output that lost its debt renders in full"
    );
}

impl VelloDrmBridge {
    /// Test-only CPU oracle from the exact staging image settled by the preceding render.
    /// It adds no GPU render or copy, and is deliberately unavailable to normal callers.
    pub(crate) fn swizzle_current_staging_for_test(
        &self,
        dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<(), BridgeError> {
        if !self.retained_staging_valid {
            return Err(BridgeError::Map(
                "test staging reference requested before a complete paint".to_owned(),
            ));
        }
        Self::validate_complete_xrgb_destination(dst, dst_pitch, self.target.size)?;
        let mapped = Self::map_readback(&self.context, self.dev_id, &self.readback_buf)?;
        let result = Self::validate_complete_mapped_xrgb_source(MappedXrgbSource {
            bytes: &mapped,
            padded_bytes_per_row: self.padded_bytes_per_row as usize,
            width: self.target.size.0 as usize,
            height: self.target.size.1 as usize,
        })
        .map(|()| {
            Self::swizzle_mapped_xrgb8888(
                MappedXrgbSource {
                    bytes: &mapped,
                    padded_bytes_per_row: self.padded_bytes_per_row as usize,
                    width: self.target.size.0 as usize,
                    height: self.target.size.1 as usize,
                },
                dst,
                dst_pitch,
                false,
            );
        });
        drop(mapped);
        self.readback_buf.unmap();
        result
    }
}

/// A sparse frame on the combined readback (an overview fade, a hover)
/// renders the scene once inside its damage and reads back only the
/// damage, never the combined desktop. The encoded desktop it leaves is
/// the one a full render of the same scene gives.
#[test]
fn encoded_regions_read_back_only_the_damage() {
    let (w, h) = (256_u32, 128_u32);
    let backdrop = Color::from_rgb8(0x20, 0x40, 0x60);
    let frame = |shade: u8| {
        let mut scene = rect_scene(w, h, backdrop);
        let panel = vello::kurbo::Rect::new(40.0, 20.0, 88.0, 44.0);
        let color = Color::from_rgba8(0xf0, shade, 0x10, 0xa0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &panel);
        scene
    };
    let damage = [
        vello::FineDamageRect {
            x: 32,
            y: 16,
            width: 64,
            height: 32,
        },
        vello::FineDamageRect {
            x: 192,
            y: 96,
            width: 16,
            height: 16,
        },
    ];
    let damage_bytes = (64 * 32 + 16 * 16) * 4;
    let mut bridge = required_bridge(w, h);
    bridge.set_phase_timing_enabled(true);
    bridge.paint(&frame(0x10)).expect("full frame");
    let full = bridge.last_phase_timing().expect("full frame timing");
    assert_eq!(full.cpu_output_bytes, u64::from(w * h * 4));

    for shade in [0x60, 0xb0] {
        let scene = frame(shade);
        bridge
            .refresh_encoded_regions(&scene, &damage)
            .expect("sparse frame");
        let timing = bridge.last_phase_timing().expect("sparse frame timing");
        assert_eq!(
            timing.readback_bytes, damage_bytes,
            "only the damage is read"
        );
        assert_eq!(
            timing.cpu_output_bytes, damage_bytes,
            "only the damage is copied"
        );
        let encoded = bridge.encoded_rgba().expect("encoded desktop").to_vec();
        let mut reference = required_bridge(w, h);
        let dense = reference.paint(&scene).expect("reference frame").to_vec();
        assert!(encoded == dense, "the repaired desktop is the full render");
    }
    assert_eq!(
        (
            bridge.last_sparse_render_attempts(),
            bridge.last_full_render_attempts()
        ),
        (1, 0),
        "a frame after a repaired one renders only its damage on the GPU"
    );
    assert!(
        bridge
            .refresh_encoded_regions(
                &frame(0),
                &[vello::FineDamageRect {
                    x: 200,
                    y: 0,
                    width: 64,
                    height: 8,
                }],
            )
            .is_err(),
        "a region leaving the desktop is not repaired"
    );
}

/// Scratch pooled for a full frame or an old size is released once the
/// session stops rendering that way, so the shell's memory comes back
/// down after a lock screen, an overlay or a mode change.
#[test]
fn pooled_scratch_follows_the_frames_the_session_still_renders() {
    let busy = |w: u32, h: u32| {
        let mut scene = Scene::new();
        for i in 0..400_u32 {
            let x = f64::from(i * 37 % w);
            let y = f64::from(i * 53 % h);
            let rect = vello::kurbo::Rect::new(x, y, x + 24.0, y + 24.0);
            let color = Color::from_rgb8(u8::try_from(i % 251).expect("below 256"), 0x40, 0x80);
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
        }
        scene
    };
    let pooled = |bridge: &VelloDrmBridge| bridge.target.vello().memory_stats().pooled_buffer_bytes;
    let mut bridge = required_bridge(1024, 768);
    let mut pixels = vec![0; 1024 * 768 * 4];
    let scene = busy(1024, 768);
    bridge
        .paint_damage_into_xrgb8888(
            &scene,
            SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: None,
            },
            &mut pixels,
            1024 * 4,
        )
        .expect("full frame");
    let after_full = pooled(&bridge);
    assert!(after_full > 0, "a full frame pools its scratch");

    let rects = [vello::FineDamageRect {
        x: 16,
        y: 16,
        width: 32,
        height: 32,
    }];
    bridge
        .paint_damage_into_xrgb8888(
            &scene,
            SceneDamage {
                revision: 2,
                previous_revision: Some(1),
                regions: Some(&rects),
            },
            &mut pixels,
            1024 * 4,
        )
        .expect("damage frame");
    assert_eq!(bridge.last_sparse_render_attempts(), 1);
    let after_damage = pooled(&bridge);
    assert!(
        after_damage < after_full,
        "damage frames must not keep the full frame's scratch: {after_damage} >= {after_full}"
    );

    bridge
        .paint_damage_into_xrgb8888(
            &scene,
            SceneDamage {
                revision: 3,
                previous_revision: Some(2),
                regions: None,
            },
            &mut pixels,
            1024 * 4,
        )
        .expect("full frame again");
    let before_resize = pooled(&bridge);
    bridge.resize(320, 240).expect("resize");
    let mut small = vec![0; 320 * 240 * 4];
    bridge
        .paint_damage_into_xrgb8888(
            &Scene::new(),
            SceneDamage {
                revision: 4,
                previous_revision: None,
                regions: None,
            },
            &mut small,
            320 * 4,
        )
        .expect("frame at the new size");
    let after_resize = pooled(&bridge);
    assert!(
        after_resize < before_resize,
        "a mode change must not keep the old size's scratch: {after_resize} >= {before_resize}"
    );
}

/// A target keeps a frame proven unchanged relative to the one it holds,
/// as the DRM presenter keeps a lone output's image, so the next change
/// renders sparse; it keeps nothing else.
#[test]
fn gpu_target_keeps_an_unchanged_frame_and_the_next_change_renders_sparse() {
    let mut target = required_target(128, 128);
    let deadline = tilo_test_env::hang_deadline;
    target
        .render_until(&Scene::new(), version(1, None, None), deadline())
        .expect("seed");
    let changed = [vello::FineDamageRect {
        x: 16,
        y: 16,
        width: 16,
        height: 16,
    }];
    assert!(
        !target.keep_unchanged(version(2, Some(1), Some(&changed))),
        "a frame that changes pixels is rendered"
    );
    assert!(
        !target.keep_unchanged(version(2, Some(7), Some(&[]))),
        "a frame unchanged from a revision the target does not hold is rendered"
    );
    assert!(
        !target.keep_unchanged(version(2, Some(1), None)),
        "a frame without damage authority is rendered"
    );
    assert!(target.keep_unchanged(version(2, Some(1), Some(&[]))));
    let stats = target
        .render_until(
            &Scene::new(),
            version(3, Some(2), Some(&changed)),
            deadline(),
        )
        .expect("change after the kept frame");
    assert_eq!(
        (stats.sparse_attempts, stats.full_attempts),
        (1, 0),
        "the change after a kept frame repairs only its damage"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn gpu_target_refreshes_dense_whenever_retained_damage_cannot_be_trusted() {
    // Phase `gpu_target_unchanged_clear_color_preserves_retained_damage`.
    {
        let mut bridge = required_bridge(128, 128);
        let mut pixels = vec![0; 128 * 128 * 4];
        let scene = Scene::new();
        bridge
            .paint_damage_into_xrgb8888(
                &scene,
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: None,
                },
                &mut pixels,
                512,
            )
            .expect("seed");
        let identity = bridge.xrgb_history_identity(512, 1);
        bridge.set_base_color(bridge.target.base_color);
        let rects = [vello::FineDamageRect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        }];
        bridge
            .paint_damage_into_xrgb8888(
                &scene,
                SceneDamage {
                    revision: 2,
                    previous_revision: Some(1),
                    regions: Some(&rects),
                },
                &mut pixels,
                512,
            )
            .expect("retained repaint");
        assert_eq!(bridge.last_sparse_render_attempts(), 1);
        assert_eq!(bridge.xrgb_history_identity(512, 1), identity);
    }

    // Phase `gpu_target_sparse_without_staging_matches_dense`.
    {
        let mut target = required_target(129, 128);
        let stats = target
            .render_until(
                &Scene::new(),
                version(1, None, None),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        assert_eq!(stats.framebuffer_staging_bytes, 0);
        assert_eq!(stats.framebuffer_copy_bytes, 0);
        let seed = target_pixels(&target);
        let allocation = target.texture.clone();
        let region = vello::FineDamageRect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        };
        let scene = region_scene(region, Color::WHITE);
        let outcome = target
            .render_until(
                &scene,
                version(2, Some(1), Some(&[region])),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        assert!(outcome.damage_admitted);
        assert_eq!(outcome.framebuffer_staging_bytes, 0);
        assert_eq!(outcome.framebuffer_copy_bytes, 0);
        assert_eq!(
            outcome.render_attempts,
            outcome.sparse_attempts + outcome.full_attempts
        );
        assert_eq!(outcome.completion_markers, outcome.render_attempts);
        assert_eq!(
            target.texture, allocation,
            "sparse render must reuse actual target allocation"
        );
        let actual = target_pixels(&target);
        for y in 0..128 {
            for x in 0..129 {
                if !(16..32).contains(&x) || !(16..32).contains(&y) {
                    let offset = (y * 129 + x) * 4;
                    assert_eq!(
                        &actual[offset..offset + 4],
                        &seed[offset..offset + 4],
                        "untouched pixel {x},{y}"
                    );
                }
            }
        }
        let mut oracle = required_target(129, 128);
        oracle
            .render_until(
                &scene,
                version(2, None, None),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        assert_eq!(actual, target_pixels(&oracle));
    }

    // Phase `gpu_target_revision_gap_and_resize_force_dense`.
    {
        let mut target = required_target(128, 128);
        let regions = [vello::FineDamageRect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        }];
        let scene = region_scene(regions[0], Color::WHITE);
        target
            .render_until(
                &scene,
                version(1, None, None),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        let outcome = target
            .render_until(
                &scene,
                version(3, Some(2), Some(&regions)),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        assert!(!outcome.damage_admitted);
        assert_eq!(outcome.sparse_attempts, 0);
        target.resize(128, 128).unwrap();
        assert!(target.completed_view().is_none());
        let outcome = target
            .render_until(
                &scene,
                version(4, Some(3), Some(&regions)),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        assert_eq!(outcome.sparse_attempts, 0);
        assert!(matches!(
            target.resize(0, 128),
            Err(BridgeError::InvalidSize(_))
        ));
        assert!(target.completed_view().is_none());
        let limit = target.device.limits().max_texture_dimension_2d;
        assert!(matches!(
            target.resize(limit + 1, 128),
            Err(BridgeError::InvalidSize(_))
        ));
        let old = target.texture.clone();
        target.resize(160, 128).unwrap();
        assert_ne!(old, target.texture);
        assert_eq!(target.size(), (160, 128));
        let outcome = target
            .render_until(
                &scene,
                version(5, Some(4), Some(&regions)),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        assert_eq!(outcome.sparse_attempts, 0);
        target.set_base_color(Color::WHITE);
        assert!(target.completed_view().is_none());
    }

    // Phase `gpu_target_xrgb_staging_gap_gets_complete_refresh`.
    {
        let mut bridge = required_bridge(129, 128);
        bridge.set_phase_timing_enabled(true);
        let mut actual = vec![0; 129 * 128 * 4];
        bridge
            .paint_damage_into_xrgb8888(&Scene::new(), version(1, None, None), &mut actual, 129 * 4)
            .unwrap();
        let first = vello::FineDamageRect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        };
        let second = vello::FineDamageRect {
            x: 64,
            y: 64,
            width: 16,
            height: 16,
        };
        let mut scene = region_scene(first, Color::WHITE);
        bridge
            .target
            .render_until(
                &scene,
                version(2, Some(1), Some(&[first])),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        scene.append(&region_scene(second, Color::from_rgb8(200, 30, 50)), None);
        bridge
            .paint_damage_into_xrgb8888(
                &scene,
                version(3, Some(2), Some(&[second])),
                &mut actual,
                129 * 4,
            )
            .unwrap();
        assert_eq!(
            bridge.last_sparse_render_attempts(),
            1,
            "target continuity survives GPU-only middle frame"
        );
        assert_eq!(
            bridge.last_phase_timing().unwrap().readback_bytes,
            129 * 128 * 4,
            "stale staging needs complete refresh in same submission"
        );
        let mut oracle = required_bridge(129, 128);
        let mut expected = vec![0; actual.len()];
        oracle
            .paint_into_xrgb8888(&scene, &mut expected, 129 * 4)
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            bridge.staging_content,
            Some((bridge.target.geometry_generation, Some(3)))
        );
    }
}

#[test]
fn default_area_frames_keep_stable_pixels_after_counter_settlement() {
    use crate::shell::output_scene::OutputGeometry;

    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("VELLO_CANONICAL_SEGMENT_ORDER");
    env.remove("VELLO_BUMP_BUDGET");
    env.set("TILO_CLOCK", "09:41");
    let mut target = required_target(3_840, 2_160);
    assert!(target.renderer.canonical_segment_order_available());
    let mut frame = PopulatedShellFixture::new(
        (3_840, 2_160),
        ui::theme::Theme::high_contrast().with_reduce_motion(true),
    );
    frame.set_output_geometry(vec![
        OutputGeometry::unscaled(0.0, 0.0, 1920.0, 2160.0).with_id(0),
        OutputGeometry::unscaled(1920.0, 0.0, 1920.0, 2160.0).with_id(1),
    ]);
    frame.relayout();
    let scene = frame.paint();
    target
        .render_until(
            &scene,
            version(1, None, None),
            tilo_test_env::hang_deadline(),
        )
        .unwrap();
    let expected = target_pixels(&target);
    // Repaint the high-contrast label edges implicated by the overview failure.
    // A whole half-desktop is deliberately ineligible under the profitability gate.
    let damage = [vello::FineDamageRect {
        x: 640,
        y: 128,
        width: 128,
        height: 64,
    }];
    for revision in 2_u64..=9 {
        let sparse = revision.is_multiple_of(2);
        let started = Instant::now();
        target
            .render_until(
                &scene,
                version(
                    revision,
                    revision.checked_sub(1),
                    sparse.then_some(&damage[..]),
                ),
                tilo_test_env::hang_deadline(),
            )
            .unwrap();
        eprintln!(
            "default Area revision {revision}: sparse={sparse}, completed_render_us={}",
            started.elapsed().as_micros()
        );
        assert_eq!(
            target.renderer.poll_bump_budget(&target.device).unwrap(),
            vello::BumpBudgetPoll::Complete { overflowed: false },
            "the next adaptive frame must not inherit a pending measurement"
        );
        assert_eq!(target.last_render_attempts, 1);
        if sparse {
            assert_eq!(target.last_canonical_segment_order_sparse_attempts, 1);
            assert_eq!(target.last_full_render_attempts, 0);
        } else {
            assert_eq!(target.last_canonical_segment_order_full_attempts, 1);
            assert_eq!(target.last_sparse_render_attempts, 0);
        }
        let actual = target_pixels(&target);
        let first = actual
            .iter()
            .zip(&expected)
            .position(|(actual, expected)| actual != expected);
        assert!(
            actual == expected,
            "revision {revision}: {} versus {} bytes; first differing byte {first:?}",
            actual.len(),
            expected.len()
        );
    }
}
