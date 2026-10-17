use super::*;

#[test]
fn pointer_swizzle_matches_scalar_oracle_for_unaligned_and_truncated_rows() {
    for source_offset in 0..4 {
        for destination_offset in 0..4 {
            for source_len in 0..68 {
                for destination_len in 0..68 {
                    let source_storage: Vec<u8> = (0_usize..source_offset + source_len)
                        .map(|index| {
                            u8::try_from(index.wrapping_mul(37).wrapping_add(11) & 0xff)
                                .unwrap_or_default()
                        })
                        .collect();
                    let source = &source_storage[source_offset..];
                    let mut expected_storage = vec![0x6d; destination_offset + destination_len];
                    let mut actual_storage = expected_storage.clone();
                    VelloDrmBridge::swizzle_xrgb_row_reference(
                        &mut expected_storage[destination_offset..],
                        source,
                    );
                    VelloDrmBridge::swizzle_xrgb_row(
                        &mut actual_storage[destination_offset..],
                        source,
                    );
                    assert_eq!(
                        actual_storage, expected_storage,
                        "source offset={source_offset} destination offset={destination_offset} source len={source_len} destination len={destination_len}"
                    );
                }
            }
        }
    }
}

const NATIVE_BENCHMARK_ACTION_TARGETS: [&str; 2] = ["topbar_tray_pill", "topbar_workspace_pill"];

fn dispatch_native_benchmark_click(
    frame: &mut FrameBuilder,
    target: &str,
    center: vello::kurbo::Point,
    document: &mut AutoCommit,
    focus: &mut Option<NodeId>,
) {
    let modifiers = Modifiers::default();
    let cursor = WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(center.x, center.y),
    };
    let press = WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    let release = WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    };
    let _ = frame.dispatch_event(
        &cursor,
        &modifiers,
        &mut ui::widget::Edits::lend(document),
        &ROOT,
        focus,
    );
    let pressed = frame.dispatch_event(
        &press,
        &modifiers,
        &mut ui::widget::Edits::lend(document),
        &ROOT,
        focus,
    );
    let released = frame.dispatch_event(
        &release,
        &modifiers,
        &mut ui::widget::Edits::lend(document),
        &ROOT,
        focus,
    );
    assert!(
        pressed || released,
        "native benchmark target {target:?} must handle click"
    );
}

fn assert_compact_xrgb_eq(actual: &[u8], expected: &[u8], label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label}: output length");
    let mismatch = actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected);
    assert!(
        mismatch.is_none(),
        "{label}: first mismatch at byte {mismatch:?}"
    );
}

fn layered_scene(
    w: u32,
    h: u32,
    background: Color,
    overlays: &[(vello::FineDamageRect, Color)],
) -> Scene {
    let mut scene = rect_scene(w, h, background);
    for (rect, color) in overlays {
        let rect = vello::kurbo::Rect::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.x.saturating_add(rect.width)),
            f64::from(rect.y.saturating_add(rect.height)),
        );
        scene.fill(Fill::NonZero, Affine::IDENTITY, *color, None, &rect);
    }
    scene
}

fn assert_xrgb_pixels_eq(
    actual: &[u8],
    expected: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
) {
    for row in 0..height {
        let start = row.checked_mul(pitch).expect("test row offset fits usize");
        let pixel_bytes = width.checked_mul(4).expect("test pixel row fits usize");
        let end = start
            .checked_add(pixel_bytes)
            .expect("test row end fits usize");
        assert_eq!(
            &actual[start..end],
            &expected[start..end],
            "pixel row {row} differs from dense oracle"
        );
    }
}

#[test]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixed test dimensions are bounded by their literal fixture values"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn tracked_swizzle_validates_before_writing_and_unmaps_on_error() {
    // Phase `tracked_full_swizzle_rejects_truncated_source_without_mutating_destination`.
    {
        let source_bytes = [0x51_u8; 15];
        let source = MappedXrgbSource {
            bytes: &source_bytes,
            padded_bytes_per_row: 8,
            width: 2,
            height: 2,
        };
        let mut destination = [0xA7_u8; 16];
        let before = destination;
        let result = VelloDrmBridge::swizzle_mapped_xrgb8888_with_plan(
            source,
            &mut destination,
            8,
            false,
            XrgbWritePlan::Full,
            true,
        );
        assert!(matches!(result, Err(BridgeError::InvalidSize(_))));
        assert_eq!(
            destination, before,
            "tracked source rejection must be atomic"
        );
    }

    // Phase `tracked_partial_swizzle_validates_every_rect_before_mutating_destination`.
    {
        let source_bytes = [0x51_u8; 16];
        let source = MappedXrgbSource {
            bytes: &source_bytes,
            padded_bytes_per_row: 16,
            width: 4,
            height: 1,
        };
        let rects = [
            vello::FineDamageRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            vello::FineDamageRect {
                x: 3,
                y: 0,
                width: 2,
                height: 1,
            },
        ];
        let mut destination = [0xA7_u8; 16];
        let before = destination;
        let result = VelloDrmBridge::swizzle_mapped_xrgb8888_with_plan(
            source,
            &mut destination,
            16,
            false,
            XrgbWritePlan::Rects(&rects),
            true,
        );
        assert!(matches!(result, Err(BridgeError::InvalidSize(_))));
        assert_eq!(
            destination, before,
            "later invalid rect must not expose an earlier write"
        );
    }

    // Phase `tracked_short_destination_and_prepared_damage_mismatch_abort_without_writes`.
    {
        let (width, height) = (32_u32, 24_u32);
        let mut bridge = required_bridge(width, height);
        let pitch = usize::try_from(width).expect("test width") * 4;
        let scene = rect_scene(width, height, Color::from_rgb8(0x11, 0x22, 0x33));
        let left = [vello::FineDamageRect {
            x: 2,
            y: 3,
            width: 5,
            height: 4,
        }];
        let right = [vello::FineDamageRect {
            x: 16,
            y: 8,
            width: 5,
            height: 4,
        }];
        let identity = bridge
            .xrgb_history_identity(pitch, 1)
            .expect("test bridge identity");
        let mut history = XrgbTargetHistory::new(1);
        let damage = SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: Some(&left),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            damage.revision,
            damage.previous_revision,
            damage.regions,
            &[],
        );
        let mut short = vec![0xA7; pitch * usize::try_from(height).expect("test height") - 1];
        let short_before = short.clone();
        let result = bridge.paint_damage_into_xrgb8888_with_history(
            &scene,
            damage,
            &transaction,
            &mut short,
            pitch,
        );
        assert!(matches!(result, Err(BridgeError::InvalidSize(_))));
        assert_eq!(
            short, short_before,
            "short tracked output must remain untouched"
        );
        transaction.abort();

        let transaction = history.prepare(0, Some(identity), 1, None, Some(&left), &[]);
        let mismatch = SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: Some(&right),
        };
        let mut destination = vec![0xB8; pitch * usize::try_from(height).expect("test height")];
        let before = destination.clone();
        let result = bridge.paint_damage_into_xrgb8888_with_history(
            &scene,
            mismatch,
            &transaction,
            &mut destination,
            pitch,
        );
        assert!(matches!(result, Err(BridgeError::XrgbHistoryMismatch)));
        assert_eq!(
            destination, before,
            "mismatched prepared damage must not write"
        );
        transaction.abort();

        let transaction = history.prepare(0, Some(identity), 1, None, Some(&right), &[]);
        bridge
            .paint_damage_into_xrgb8888_with_history(
                &scene,
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: Some(&right),
                },
                &transaction,
                &mut destination,
                pitch,
            )
            .expect("next tracked paint after aborted errors");
        transaction.commit(XrgbHistoryCommit::FullBoundary);
    }

    // Phase `tracked_swizzle_error_unmaps_before_the_next_paint`.
    {
        let (width, height) = (32_u32, 24_u32);
        let mut bridge = required_bridge(width, height);
        let pitch = usize::try_from(width).expect("test width") * 4;
        let scene = rect_scene(width, height, Color::from_rgb8(0x11, 0x22, 0x33));
        let damage = [vello::FineDamageRect {
            x: 2,
            y: 3,
            width: 5,
            height: 4,
        }];
        let identity = bridge
            .xrgb_history_identity(pitch, 1)
            .expect("test bridge identity");
        let mut history = XrgbTargetHistory::new(1);
        let mut destination = vec![0xA7; pitch * usize::try_from(height).expect("test height")];
        let before = destination.clone();
        let transaction = history.prepare(0, Some(identity), 1, None, Some(&damage), &[]);
        bridge.inject_next_tracked_swizzle_failure_for_test();
        let failed = bridge.paint_damage_into_xrgb8888_with_history(
            &scene,
            SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: Some(&damage),
            },
            &transaction,
            &mut destination,
            pitch,
        );
        assert!(matches!(failed, Err(BridgeError::InvalidSize(_))));
        assert_eq!(
            destination, before,
            "injected post-map swizzle error must be atomic"
        );
        transaction.abort();

        let transaction = history.prepare(0, Some(identity), 1, None, Some(&damage), &[]);
        bridge
            .paint_damage_into_xrgb8888_with_history(
                &scene,
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: Some(&damage),
                },
                &transaction,
                &mut destination,
                pitch,
            )
            .expect("staging must be unmapped for the next tracked paint");
        transaction.commit(XrgbHistoryCommit::FullBoundary);
    }
}

#[test]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixed test dimensions are bounded by their literal fixture values"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn history_bridge_treats_saturation_gaps_and_scanout_epochs_as_full_boundaries() {
    // Phase `saturated_bridge_history_epoch_disables_tracking_but_not_full_paint`.
    {
        let mut bridge = required_bridge(16, 12);
        bridge.xrgb_history_epoch = Some(u64::MAX);
        bridge.set_base_color(Color::from_rgb8(0x20, 0x30, 0x40));
        assert!(!bridge.xrgb_history_available());
        let mut destination = vec![0xA7; 16 * 12 * 4];
        bridge
            .paint_into_xrgb8888(&Scene::new(), &mut destination, 16 * 4)
            .expect("ordinary full paint remains available after tracking epoch saturation");
    }

    // Phase `history_bridge_accepts_gap_and_oversized_damage_as_real_full_boundaries`.
    {
        let (width, height) = (32_u32, 24_u32);
        let mut bridge = required_bridge(width, height);
        let pitch = usize::try_from(width).expect("test width") * 4 + 16;
        let mut destination = vec![0xA7; pitch * usize::try_from(height).expect("test height")];
        let scene = rect_scene(width, height, Color::from_rgb8(0x20, 0x30, 0x40));
        let small = [vello::FineDamageRect {
            x: 3,
            y: 4,
            width: 6,
            height: 5,
        }];
        let identity = bridge
            .xrgb_history_identity(pitch, 1)
            .expect("test bridge identity");
        let mut history = XrgbTargetHistory::new(1);

        let seed = SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: Some(&small),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            seed.revision,
            seed.previous_revision,
            seed.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &scene,
                seed,
                &transaction,
                &mut destination,
                pitch,
            )
            .expect("seed full boundary");
        assert_eq!(write.mode, XrgbCpuWriteMode::Full);
        transaction.commit(XrgbHistoryCommit::FullBoundary);

        // This is a legitimate model gap, not an identity mismatch. It must render densely and
        // establish the new revision rather than reject before the bridge can recover.
        let gap = SceneDamage {
            revision: 4,
            previous_revision: Some(2),
            regions: Some(&small),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            gap.revision,
            gap.previous_revision,
            gap.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &scene,
                gap,
                &transaction,
                &mut destination,
                pitch,
            )
            .expect("gap full boundary");
        assert_eq!(write.mode, XrgbCpuWriteMode::Full);
        transaction.commit(XrgbHistoryCommit::FullBoundary);

        let oversized = vec![small[0]; 65];
        let oversized_damage = SceneDamage {
            revision: 5,
            previous_revision: Some(4),
            regions: Some(&oversized),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            oversized_damage.revision,
            oversized_damage.previous_revision,
            oversized_damage.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &scene,
                oversized_damage,
                &transaction,
                &mut destination,
                pitch,
            )
            .expect("oversized damage must be a full boundary, not a mismatch");
        assert_eq!(write.mode, XrgbCpuWriteMode::Full);
        transaction.commit(XrgbHistoryCommit::FullBoundary);
    }

    // Phase `history_bridge_accepts_nonzero_scanout_epoch_across_two_targets`.
    {
        let (width, height) = (32_u32, 24_u32);
        let mut bridge = required_bridge(width, height);
        let pitch = usize::try_from(width).expect("test width") * 4;
        let scene = rect_scene(width, height, Color::from_rgb8(0x20, 0x30, 0x40));
        let damage = [vello::FineDamageRect {
            x: 3,
            y: 4,
            width: 6,
            height: 5,
        }];
        let identity = bridge
            .xrgb_history_identity(pitch, 2)
            .expect("test bridge identity")
            .with_scanout_epoch(7);
        let mut history = XrgbTargetHistory::new(2);
        let mut left = vec![0xA7; pitch * usize::try_from(height).expect("test height")];
        let mut right = left.clone();

        let first = SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: Some(&damage),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            first.revision,
            first.previous_revision,
            first.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(&scene, first, &transaction, &mut left, pitch)
            .expect("nonzero scanout epoch seed");
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let second = SceneDamage {
            revision: 2,
            previous_revision: Some(1),
            regions: Some(&damage),
        };
        let transaction = history.prepare(
            1,
            Some(identity),
            second.revision,
            second.previous_revision,
            second.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &scene,
                second,
                &transaction,
                &mut right,
                pitch,
            )
            .expect("nonzero scanout epoch must not mismatch bridge identity");
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the two-target fixture keeps each ordered history transaction and exact staging comparison together"
)]
fn history_two_targets_repair_padded_destination_from_current_staging() {
    let (width, height) = (64_u32, 48_u32);
    let mut bridge = required_bridge(width, height);
    let pitch = usize::try_from(width)
        .expect("test width")
        .checked_mul(4)
        .and_then(|tight| tight.checked_add(16))
        .expect("test pitch");
    let height_usize = usize::try_from(height).expect("test height");
    let first_delta = vello::FineDamageRect {
        x: 5,
        y: 7,
        width: 9,
        height: 8,
    };
    let second_delta = vello::FineDamageRect {
        x: 31,
        y: 19,
        width: 11,
        height: 7,
    };
    let base = Color::from_rgb8(0x11, 0x22, 0x33);
    let first_scene = rect_scene(width, height, base);
    let second_scene = layered_scene(
        width,
        height,
        base,
        &[(first_delta, Color::from_rgb8(0xC0, 0x50, 0x20))],
    );
    let third_scene = layered_scene(
        width,
        height,
        base,
        &[
            (first_delta, Color::from_rgb8(0xC0, 0x50, 0x20)),
            (second_delta, Color::from_rgb8(0x20, 0x80, 0xD0)),
        ],
    );
    let first_regions = [first_delta];
    let second_regions = [second_delta];
    let identity = bridge
        .xrgb_history_identity(pitch, 2)
        .expect("test bridge identity")
        .with_scanout_epoch(9);
    let mut history = XrgbTargetHistory::new(2);
    let mut left = vec![0xA7; pitch * height_usize];
    let mut right = vec![0xB8; pitch * height_usize];

    let initial = SceneDamage {
        revision: 1,
        previous_revision: None,
        regions: Some(&first_regions),
    };
    let transaction = history.prepare(
        0,
        Some(identity),
        initial.revision,
        initial.previous_revision,
        initial.regions,
        &[],
    );
    let write = bridge
        .paint_damage_into_xrgb8888_with_history(
            &first_scene,
            initial,
            &transaction,
            &mut left,
            pitch,
        )
        .expect("seed target zero");
    transaction.commit(if write.damage_admitted {
        XrgbHistoryCommit::DamageAccepted
    } else {
        XrgbHistoryCommit::FullBoundary
    });

    let second = SceneDamage {
        revision: 2,
        previous_revision: Some(1),
        regions: Some(&first_regions),
    };
    let transaction = history.prepare(
        1,
        Some(identity),
        second.revision,
        second.previous_revision,
        second.regions,
        &[],
    );
    let write = bridge
        .paint_damage_into_xrgb8888_with_history(
            &second_scene,
            second,
            &transaction,
            &mut right,
            pitch,
        )
        .expect("seed target one");
    transaction.commit(if write.damage_admitted {
        XrgbHistoryCommit::DamageAccepted
    } else {
        XrgbHistoryCommit::FullBoundary
    });

    let third = SceneDamage {
        revision: 3,
        previous_revision: Some(2),
        regions: Some(&second_regions),
    };
    let transaction = history.prepare(
        0,
        Some(identity),
        third.revision,
        third.previous_revision,
        third.regions,
        &[],
    );
    let write = bridge
        .paint_damage_into_xrgb8888_with_history(
            &third_scene,
            third,
            &transaction,
            &mut left,
            pitch,
        )
        .expect("repair target zero");
    if bridge.target.renderer.sparse_fine_supported() {
        assert_eq!(write.mode, XrgbCpuWriteMode::Partial);
    }
    transaction.commit(if write.damage_admitted {
        XrgbHistoryCommit::DamageAccepted
    } else {
        XrgbHistoryCommit::FullBoundary
    });

    let mut staging_reference = left.clone();
    bridge
        .swizzle_current_staging_for_test(&mut staging_reference, pitch)
        .expect("same settled staging reference");
    assert_eq!(
        left, staging_reference,
        "two-target partial repair must equal the current settled staging bytes"
    );
    let tight = usize::try_from(width).expect("test width") * 4;
    for row in 0..height_usize {
        let start = row.checked_mul(pitch).expect("test row");
        let tail = start.checked_add(tight).expect("test tail");
        let end = start.checked_add(pitch).expect("test row end");
        assert!(
            left[tail..end].iter().all(|byte| *byte == 0xA7),
            "tracked swizzles must preserve target-zero row padding"
        );
        assert!(
            right[tail..end].iter().all(|byte| *byte == 0xB8),
            "tracked swizzles must preserve target-one row padding"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the native action fixture keeps one- and two-target transactions adjacent to their exact current-staging checks"
)]
fn native_4k_framebuilder_actions_match_current_staging_for_one_and_two_targets() {
    const WIDTH: u32 = 3_840;
    const HEIGHT: u32 = 2_160;
    let mut one_bridge = required_bridge(WIDTH, HEIGHT);
    let mut two_bridge = required_bridge(WIDTH, HEIGHT);
    let width = usize::try_from(WIDTH).expect("native width");
    let height = usize::try_from(HEIGHT).expect("native height");
    let pitch = width
        .checked_mul(4)
        .and_then(|tight| tight.checked_add(16))
        .expect("native padded pitch");
    let len = pitch.checked_mul(height).expect("native output bytes");
    let one_identity = one_bridge
        .xrgb_history_identity(pitch, 1)
        .expect("one-target identity");
    let two_identity = two_bridge
        .xrgb_history_identity(pitch, 2)
        .expect("two-target identity")
        .with_scanout_epoch(5);
    let mut one_history = XrgbTargetHistory::new(1);
    let mut two_history = XrgbTargetHistory::new(2);
    let mut one_destination = vec![0xA7; len];
    let mut two_destinations = [vec![0xB8; len], vec![0xC9; len]];
    let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark());
    let mut document = AutoCommit::new();
    let mut focus = None;
    let mut one_partial = 0_u64;
    let mut two_partial = 0_u64;
    let mut previous_quick_settings = None;
    let mut previous_pixels: Option<Vec<u8>> = None;

    for sample_index in 0..=12 {
        if sample_index != 0 {
            let target = *NATIVE_BENCHMARK_ACTION_TARGETS
                .get((sample_index - 1) % NATIVE_BENCHMARK_ACTION_TARGETS.len())
                .expect("native benchmark action target exists");
            let rect = frame
                .widget_registry()
                .get(target)
                .copied()
                .filter(|rect| rect.area() > 0.0)
                .expect("native benchmark target must be painted");
            dispatch_native_benchmark_click(
                &mut frame,
                target,
                rect.center(),
                &mut document,
                &mut focus,
            );
            frame.relayout();
        }
        let painted = frame.paint_with_damage();
        assert!(
            !painted.scene.encoding().is_empty(),
            "native benchmark sample {sample_index} must record a scene"
        );
        let overlays = frame.chrome().overlays;
        assert!(
            !overlays.overview,
            "the workspace pill click dismisses Quick Settings"
        );
        if let Some(previous) = previous_quick_settings {
            assert_ne!(
                overlays.quick_settings, previous,
                "native benchmark action {sample_index} must change Quick Settings state"
            );
        }
        let damage = SceneDamage {
            revision: painted.revision,
            previous_revision: painted.previous_revision,
            regions: painted.damage_regions.as_deref(),
        };

        let transaction = one_history.prepare(
            0,
            Some(one_identity),
            damage.revision,
            damage.previous_revision,
            damage.regions,
            &[],
        );
        let one_write = one_bridge
            .paint_damage_into_xrgb8888_with_history(
                &painted.scene,
                damage,
                &transaction,
                &mut one_destination,
                pitch,
            )
            .expect("one-target native history paint");
        transaction.commit(if one_write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });
        if one_write.mode == XrgbCpuWriteMode::Partial {
            one_partial = one_partial.saturating_add(1);
        }
        let mut one_reference = one_destination.clone();
        one_bridge
            .swizzle_current_staging_for_test(&mut one_reference, pitch)
            .expect("one-target current staging reference");
        assert_compact_xrgb_eq(
            &one_destination,
            &one_reference,
            "one-target native current-staging output",
        );
        if let Some(previous) = previous_pixels.as_ref() {
            let visible_row_bytes = width.checked_mul(4).expect("native visible row bytes");
            assert!(
                (0..height).any(|row| {
                    let start = row.checked_mul(pitch).expect("native row start");
                    let end = start
                        .checked_add(visible_row_bytes)
                        .expect("native visible row end");
                    previous[start..end] != one_reference[start..end]
                }),
                "native benchmark action {sample_index} must change actual output pixels"
            );
        }
        previous_quick_settings = Some(overlays.quick_settings);
        previous_pixels = Some(one_reference);

        let target_index = sample_index % two_destinations.len();
        let transaction = two_history.prepare(
            target_index,
            Some(two_identity),
            damage.revision,
            damage.previous_revision,
            damage.regions,
            &[],
        );
        let destination = two_destinations
            .get_mut(target_index)
            .expect("two-target destination");
        let two_write = two_bridge
            .paint_damage_into_xrgb8888_with_history(
                &painted.scene,
                damage,
                &transaction,
                destination,
                pitch,
            )
            .expect("two-target native history paint");
        transaction.commit(if two_write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });
        if two_write.mode == XrgbCpuWriteMode::Partial {
            two_partial = two_partial.saturating_add(1);
        }
        let mut two_reference = destination.clone();
        two_bridge
            .swizzle_current_staging_for_test(&mut two_reference, pitch)
            .expect("two-target current staging reference");
        assert_compact_xrgb_eq(
            destination,
            &two_reference,
            "two-target native current-staging output",
        );
    }
    assert!(
        one_partial > 0,
        "native one-target history must write partial XRGB"
    );
    assert!(
        two_partial > 0,
        "native two-target history must write partial XRGB"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn versioned_damage_copies_dirty_regions_until_invalidated() {
    // Phase `versioned_damage_copies_exactly_the_dirty_region`.
    {
        // Phase `l_shaped_damage_does_not_expand_to_a_bounding_box`.
        {
            // A QS popover overlapping a full-width top bar is the common case that made a bounding
            // union look like a near-full frame. Keep both copies precise; Vello unions their tiles.
            let damage = [
                vello::FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 68,
                },
                vello::FineDamageRect {
                    x: 1440,
                    y: 40,
                    width: 480,
                    height: 800,
                },
            ];
            let normalized = VelloDrmBridge::normalize_damage(&damage, (1920, 1080))
                .expect("the precise L shape is sparse eligible");
            assert_eq!(
                normalized.as_slice(),
                &damage,
                "partial overlap stays as exact copies"
            );
            let copied_pixels = normalized.iter().fold(0_u64, |pixels, rect| {
                pixels + u64::from(rect.width) * u64::from(rect.height)
            });
            assert_eq!(copied_pixels, 514_560);
            let bounding_pixels = 1920_u64 * 840;
            assert!(
                bounding_pixels > u64::from(1920_u32) * u64::from(1080_u32) / 2,
                "the old bounding box would have forced a full fallback"
            );
        }

        // Phase `shared_coarse_diagnostic_accepts_full_damage_without_relaxing_sparse_policy`.
        {
            let full = [vello::FineDamageRect {
                x: 0,
                y: 0,
                width: 64,
                height: 48,
            }];
            assert!(
                VelloDrmBridge::normalize_damage(&full, (64, 48)).is_none(),
                "ordinary sparse damage must retain its full-frame profitability fallback"
            );
            assert_eq!(
                VelloDrmBridge::validate_diagnostic_damage(&full, (64, 48))
                    .expect("the diagnostic must compare full native damage")
                    .as_slice(),
                &full,
            );

            let invalid = [vello::FineDamageRect {
                x: 63,
                y: 0,
                width: 2,
                height: 1,
            }];
            assert!(
                VelloDrmBridge::validate_diagnostic_damage(&invalid, (64, 48)).is_none(),
                "diagnostic rendering must still reject out-of-bounds rectangles"
            );
        }

        // Phase `versioned_damage_retains_full_pixels_and_copies_only_dirty_rectangles`.
        {
            let (width, height) = (64_u32, 48_u32);
            let mut bridge = required_bridge(width, height);
            let pitch = width as usize * 4 + 16;
            let first = rect_scene(width, height, Color::from_rgb8(0x11, 0x22, 0x33));
            let damage = [vello::FineDamageRect {
                x: 7,
                y: 9,
                width: 13,
                height: 11,
            }];
            let second = layered_scene(
                width,
                height,
                Color::from_rgb8(0x11, 0x22, 0x33),
                &[(damage[0], Color::from_rgb8(0xC0, 0x50, 0x20))],
            );

            bridge.set_phase_timing_enabled(true);
            let mut first_destination = vec![0xD1; pitch * height as usize];
            bridge
                .paint_damage_into_xrgb8888(
                    &first,
                    SceneDamage {
                        revision: 1,
                        previous_revision: None,
                        regions: Some(&damage),
                    },
                    &mut first_destination,
                    pitch,
                )
                .expect("first versioned paint is dense");
            assert_eq!(
                bridge.last_phase_timing().expect("timing").readback_bytes,
                u64::from(width) * u64::from(height) * 4,
                "a first versioned paint must not use stale sparse backing"
            );

            let sparse_supported = bridge.target.renderer.sparse_fine_supported();
            let mut alternating_destination = vec![0xA7; pitch * height as usize];
            bridge
                .paint_damage_into_xrgb8888(
                    &second,
                    SceneDamage {
                        revision: 2,
                        previous_revision: Some(1),
                        regions: Some(&damage),
                    },
                    &mut alternating_destination,
                    pitch,
                )
                .expect("second versioned paint");

            let mut oracle = required_bridge(width, height);
            let mut expected = vec![0x4E; pitch * height as usize];
            oracle
                .paint_into_xrgb8888(&second, &mut expected, pitch)
                .expect("dense oracle");
            assert_xrgb_pixels_eq(
                &alternating_destination,
                &expected,
                width as usize,
                height as usize,
                pitch,
            );
            if sparse_supported {
                let timing = bridge.last_phase_timing().expect("timing");
                assert_eq!(
                    timing.readback_bytes,
                    u64::from(damage[0].width) * u64::from(damage[0].height) * 4,
                    "profiled bytes are the submitted dirty copy, not a whole staging frame"
                );
                assert_eq!(
                    timing.cpu_output_bytes,
                    u64::from(width) * u64::from(height) * 4,
                    "alternating DRM destinations still receive a complete CPU XRGB image"
                );
                assert_eq!(bridge.last_sparse_render_attempts(), 1);
                assert_eq!(bridge.last_full_render_attempts(), 0);
                assert_eq!(bridge.last_render_attempts(), 1);
            }
            for row in 0..height as usize {
                assert_eq!(
                    alternating_destination[row * pitch + width as usize * 4],
                    0xA7,
                    "destination padding remains outside the XRGB image"
                );
            }
        }

        // Phase `overlapping_disjoint_and_edge_damage_matches_dense_oracle`.
        {
            let (width, height) = (64_u32, 48_u32);
            let mut bridge = required_bridge(width, height);
            let pitch = width as usize * 4 + 32;
            let first = rect_scene(width, height, Color::from_rgb8(0x08, 0x18, 0x28));
            let damage = [
                vello::FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 7,
                    height: 9,
                },
                vello::FineDamageRect {
                    x: 5,
                    y: 6,
                    width: 12,
                    height: 10,
                },
                vello::FineDamageRect {
                    x: 48,
                    y: 31,
                    width: 16,
                    height: 17,
                },
            ];
            let second = layered_scene(
                width,
                height,
                Color::from_rgb8(0x08, 0x18, 0x28),
                &[
                    (damage[0], Color::from_rgb8(0xCC, 0x11, 0x22)),
                    (damage[1], Color::from_rgb8(0x22, 0xCC, 0x11)),
                    (damage[2], Color::from_rgb8(0x11, 0x22, 0xCC)),
                ],
            );
            let mut initial = vec![0; pitch * height as usize];
            bridge
                .paint_damage_into_xrgb8888(
                    &first,
                    SceneDamage {
                        revision: 20,
                        previous_revision: None,
                        regions: Some(&damage),
                    },
                    &mut initial,
                    pitch,
                )
                .expect("dense seed");
            let mut actual = vec![0xE2; pitch * height as usize];
            bridge
                .paint_damage_into_xrgb8888(
                    &second,
                    SceneDamage {
                        revision: 21,
                        previous_revision: Some(20),
                        regions: Some(&damage),
                    },
                    &mut actual,
                    pitch,
                )
                .expect("damage paint");

            let mut oracle = required_bridge(width, height);
            let mut expected = vec![0; pitch * height as usize];
            oracle
                .paint_into_xrgb8888(&second, &mut expected, pitch)
                .expect("dense oracle");
            assert_xrgb_pixels_eq(&actual, &expected, width as usize, height as usize, pitch);
        }

        // Phase `revision_gap_and_empty_damage_force_full_readback`.
        {
            let (width, height) = (32_u32, 24_u32);
            let mut bridge = required_bridge(width, height);
            bridge.set_phase_timing_enabled(true);
            let pitch = width as usize * 4;
            let rect = [vello::FineDamageRect {
                x: 2,
                y: 3,
                width: 5,
                height: 7,
            }];
            let mut destination = vec![0; pitch * height as usize];
            bridge
                .paint_damage_into_xrgb8888(
                    &rect_scene(width, height, Color::from_rgb8(1, 2, 3)),
                    SceneDamage {
                        revision: 100,
                        previous_revision: None,
                        regions: Some(&rect),
                    },
                    &mut destination,
                    pitch,
                )
                .expect("seed");
            bridge
                .paint_damage_into_xrgb8888(
                    &rect_scene(width, height, Color::from_rgb8(4, 5, 6)),
                    SceneDamage {
                        revision: 102,
                        previous_revision: Some(101),
                        regions: Some(&rect),
                    },
                    &mut destination,
                    pitch,
                )
                .expect("revision gap uses full render");
            assert_eq!(
                bridge.last_phase_timing().expect("timing").readback_bytes,
                u64::from(width) * u64::from(height) * 4
            );

            bridge
                .paint_damage_into_xrgb8888(
                    &rect_scene(width, height, Color::from_rgb8(7, 8, 9)),
                    SceneDamage {
                        revision: 103,
                        previous_revision: Some(102),
                        regions: Some(&[]),
                    },
                    &mut destination,
                    pitch,
                )
                .expect("empty damage uses full render");
            assert_eq!(
                bridge.last_phase_timing().expect("timing").readback_bytes,
                u64::from(width) * u64::from(height) * 4
            );
        }
    }

    // Phase `failed_paint_base_color_and_resize_invalidate_sparse_backing`.
    {
        // Phase `failed_versioned_paint_invalidates_sparse_backing_before_retry`.
        {
            let (width, height) = (32_u32, 24_u32);
            let mut bridge = required_bridge(width, height);
            bridge.set_phase_timing_enabled(true);
            let pitch = width as usize * 4;
            let damage = [vello::FineDamageRect {
                x: 4,
                y: 5,
                width: 8,
                height: 7,
            }];
            let scene = rect_scene(width, height, Color::from_rgb8(0x12, 0x23, 0x34));
            let mut destination = vec![0; pitch * height as usize];
            bridge
                .paint_damage_into_xrgb8888(
                    &scene,
                    SceneDamage {
                        revision: 1,
                        previous_revision: None,
                        regions: Some(&damage),
                    },
                    &mut destination,
                    pitch,
                )
                .expect("seed complete retained frame");

            let valid_dev_id = bridge.dev_id;
            bridge.dev_id = usize::MAX;
            let failed = bridge.paint_damage_into_xrgb8888(
                &scene,
                SceneDamage {
                    revision: 2,
                    previous_revision: Some(1),
                    regions: Some(&damage),
                },
                &mut destination,
                pitch,
            );
            assert!(
                matches!(failed, Err(BridgeError::InvalidDevice(_))),
                "test-only invalid device must exercise a post-gate paint failure"
            );
            assert!(
                !bridge.target.retained_target_valid
                    && !bridge.retained_staging_valid
                    && bridge.target.last_successful_revision.is_none(),
                "a failed paint must invalidate the previously compatible retained frame before retry"
            );
            bridge.dev_id = valid_dev_id;

            bridge
                .paint_damage_into_xrgb8888(
                    &scene,
                    SceneDamage {
                        revision: 3,
                        previous_revision: Some(1),
                        regions: Some(&damage),
                    },
                    &mut destination,
                    pitch,
                )
                .expect("retry after failed paint");
            assert_eq!(
                bridge.last_phase_timing().expect("timing").readback_bytes,
                u64::from(width) * u64::from(height) * 4,
                "failed paint must erase the sparse retained revision"
            );
            assert_eq!(bridge.last_sparse_render_attempts(), 0);
            assert_eq!(bridge.last_full_render_attempts(), 1);
        }

        // Phase `base_color_and_resize_invalidate_versioned_sparse_backing`.
        {
            let (width, height) = (32_u32, 24_u32);
            let mut bridge = required_bridge(width, height);
            bridge.set_phase_timing_enabled(true);
            let pitch = width as usize * 4;
            let damage = [vello::FineDamageRect {
                x: 3,
                y: 4,
                width: 6,
                height: 5,
            }];
            let mut destination = vec![0; pitch * height as usize];
            let scene = rect_scene(width, height, Color::from_rgb8(0x20, 0x30, 0x40));
            bridge
                .paint_damage_into_xrgb8888(
                    &scene,
                    SceneDamage {
                        revision: 1,
                        previous_revision: None,
                        regions: Some(&damage),
                    },
                    &mut destination,
                    pitch,
                )
                .expect("seed");

            bridge.set_base_color(Color::from_rgb8(0x10, 0x20, 0x30));
            bridge
                .paint_damage_into_xrgb8888(
                    &scene,
                    SceneDamage {
                        revision: 2,
                        previous_revision: Some(1),
                        regions: Some(&damage),
                    },
                    &mut destination,
                    pitch,
                )
                .expect("base-color boundary");
            assert_eq!(
                bridge.last_phase_timing().expect("timing").readback_bytes,
                u64::from(width) * u64::from(height) * 4,
                "base color change must refresh the complete retained staging image"
            );

            let (resized_width, resized_height) = (width + 8, height + 4);
            bridge
                .resize(resized_width, resized_height)
                .expect("different-mode resize boundary");
            assert_eq!(bridge.size(), (resized_width, resized_height));
            let resized_pitch = resized_width as usize * 4;
            let mut resized_destination = vec![0; resized_pitch * resized_height as usize];
            let resized_scene = rect_scene(
                resized_width,
                resized_height,
                Color::from_rgb8(0x20, 0x30, 0x40),
            );
            bridge
                .paint_damage_into_xrgb8888(
                    &resized_scene,
                    SceneDamage {
                        revision: 3,
                        previous_revision: Some(2),
                        regions: Some(&damage),
                    },
                    &mut resized_destination,
                    resized_pitch,
                )
                .expect("resize boundary");
            assert_eq!(
                bridge.last_phase_timing().expect("timing").readback_bytes,
                u64::from(resized_width) * u64::from(resized_height) * 4,
                "resize must not retain a sparse backing frame"
            );
        }
    }
}

#[test]
fn sparse_overflow_recovers_dense_without_publishing_partial_pixels() {
    let (width, height) = (384_u32, 256_u32);
    let mut bridge = required_bridge(width, height);
    if !bridge.target.renderer.sparse_fine_supported() {
        return;
    }
    // What is rendered and how often, not how fast: a dense render of the overflowing
    // stack on a loaded llvmpipe host can outlast the production fence allowance.
    bridge.wait_for_gpu_completion_for_test();
    let pitch = width as usize * 4;
    let damage = [vello::FineDamageRect {
        x: 144,
        y: 80,
        width: 96,
        height: 96,
    }];
    let overflowing = opaque_rect_stack_in_rect_scene(damage[0]);
    let mut actual = vec![0; pitch * height as usize];
    bridge
        .paint_damage_into_xrgb8888(
            &Scene::new(),
            SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: Some(&damage),
            },
            &mut actual,
            pitch,
        )
        .expect("dense seed");
    bridge
        .paint_damage_into_xrgb8888(
            &overflowing,
            SceneDamage {
                revision: 2,
                previous_revision: Some(1),
                regions: Some(&damage),
            },
            &mut actual,
            pitch,
        )
        .expect("sparse overflow recovers densely");
    assert_eq!(bridge.last_sparse_render_attempts(), 1);
    assert!(
        bridge.last_full_render_attempts() >= 1,
        "overflowing sparse work must be followed by a dense recovery"
    );
    assert!(
        bridge.last_render_attempts() <= RESILIENT_RENDER_ATTEMPTS,
        "sparse attempt plus recovery remains globally bounded"
    );

    let mut oracle = required_bridge(width, height);
    oracle.wait_for_gpu_completion_for_test();
    let mut expected = vec![0; pitch * height as usize];
    oracle
        .paint_into_xrgb8888(&overflowing, &mut expected, pitch)
        .expect("dense overflow oracle");
    assert_eq!(
        actual
            .iter()
            .zip(&expected)
            .position(|(actual, expected)| actual != expected),
        None,
        "overflowed sparse pixels must never reach the XRGB destination"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the overflow fixture keeps the complete two-target recovery chain and next-paint proof together"
)]
fn tracked_sparse_overflow_full_boundary_invalidates_the_other_target() {
    let (width, height) = (384_u32, 256_u32);
    let mut bridge = required_bridge(width, height);
    if !bridge.target.renderer.sparse_fine_supported() {
        return;
    }
    // The recovery chain's pixels and attempts, not its speed on a loaded llvmpipe host.
    bridge.wait_for_gpu_completion_for_test();
    let pitch = usize::try_from(width).expect("test width") * 4;
    let damage = [vello::FineDamageRect {
        x: 144,
        y: 80,
        width: 96,
        height: 96,
    }];
    let identity = bridge
        .xrgb_history_identity(pitch, 2)
        .expect("test bridge identity");
    let mut history = XrgbTargetHistory::new(2);
    let mut left = vec![0; pitch * usize::try_from(height).expect("test height")];
    let mut right = left.clone();
    let seed = SceneDamage {
        revision: 1,
        previous_revision: None,
        regions: Some(&damage),
    };
    let transaction = history.prepare(
        0,
        Some(identity),
        seed.revision,
        seed.previous_revision,
        seed.regions,
        &[],
    );
    let write = bridge
        .paint_damage_into_xrgb8888_with_history(
            &Scene::new(),
            seed,
            &transaction,
            &mut left,
            pitch,
        )
        .expect("seed first target");
    transaction.commit(if write.damage_admitted {
        XrgbHistoryCommit::DamageAccepted
    } else {
        XrgbHistoryCommit::FullBoundary
    });

    let second = SceneDamage {
        revision: 2,
        previous_revision: Some(1),
        regions: Some(&damage),
    };
    let transaction = history.prepare(
        1,
        Some(identity),
        second.revision,
        second.previous_revision,
        second.regions,
        &[],
    );
    let write = bridge
        .paint_damage_into_xrgb8888_with_history(
            &Scene::new(),
            second,
            &transaction,
            &mut right,
            pitch,
        )
        .expect("seed second target");
    transaction.commit(if write.damage_admitted {
        XrgbHistoryCommit::DamageAccepted
    } else {
        XrgbHistoryCommit::FullBoundary
    });

    let overflow = SceneDamage {
        revision: 3,
        previous_revision: Some(2),
        regions: Some(&damage),
    };
    let transaction = history.prepare(
        0,
        Some(identity),
        overflow.revision,
        overflow.previous_revision,
        overflow.regions,
        &[],
    );
    let write = bridge
        .paint_damage_into_xrgb8888_with_history(
            &opaque_rect_stack_in_rect_scene(damage[0]),
            overflow,
            &transaction,
            &mut left,
            pitch,
        )
        .expect("dense overflow recovery");
    assert!(
        !write.damage_admitted,
        "overflow recovery cannot preserve age history"
    );
    assert_eq!(write.mode, XrgbCpuWriteMode::Full);
    transaction.commit(XrgbHistoryCommit::FullBoundary);

    let next = SceneDamage {
        revision: 4,
        previous_revision: Some(3),
        regions: Some(&damage),
    };
    let transaction = history.prepare(
        1,
        Some(identity),
        next.revision,
        next.previous_revision,
        next.regions,
        &[],
    );
    assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
    let next_write = bridge
        .paint_damage_into_xrgb8888_with_history(
            &opaque_rect_stack_in_rect_scene(damage[0]),
            next,
            &transaction,
            &mut right,
            pitch,
        )
        .expect("other target must repaint fully after overflow boundary");
    assert_eq!(next_write.mode, XrgbCpuWriteMode::Full);
    transaction.commit(if next_write.damage_admitted {
        XrgbHistoryCommit::DamageAccepted
    } else {
        XrgbHistoryCommit::FullBoundary
    });
}
