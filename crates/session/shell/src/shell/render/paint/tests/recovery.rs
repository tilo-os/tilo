use super::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn gpu_target_failed_aborted_or_poisoned_frames_hide_output_and_publish_nothing() {
    // Phase `gpu_target_failure_and_device_loss_hide_output_until_reconstruction`.
    {
        // Phase `gpu_target_failure_hides_view_until_reconstruction`.
        {
            let mut target = required_target(128, 128);
            let scene = Scene::new();
            let damage = SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: None,
            };
            target
                .render_until(&scene, damage, Instant::now() + Duration::from_secs(10))
                .expect("seed");
            assert!(target.completed_view().is_some());
            let result = target.render_until(&scene, damage, Instant::now());
            assert!(
                matches!(result, Err(BridgeError::DeadlineExceeded)),
                "expired call must fail: {result:?}"
            );
            assert!(target.completed_view().is_none());
            assert_eq!(target.last_render_attempts, 0);
        }

        // Phase `gpu_target_counter_failures_and_device_loss_hide_output`.
        {
            for error in [
                vello::BumpBudgetError::MapFailed,
                vello::BumpBudgetError::MissingDownload,
                vello::BumpBudgetError::InvalidDownload,
                vello::BumpBudgetError::Poll("injected wrong submission index".to_owned()),
            ] {
                let mut target = required_target(128, 128);
                let mut test = completion_clock();
                test.counters.push_back(Err(error));
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);
                let result = target.render_until(&Scene::new(), version(1, None, None), deadline);
                assert!(matches!(
                    result,
                    Err(BridgeError::BumpReadback(_) | BridgeError::DevicePoll(_))
                ));
                assert_eq!(target.last_render_attempts, 1);
                assert!(target.completed_view().is_none() && target.poisoned);
            }
            let mut target = required_target(128, 128);
            target
                .render_until(
                    &Scene::new(),
                    version(1, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            target.device.destroy();
            let _ = target.device.poll(wgpu::PollType::Poll);
            assert!(target.completed_view().is_none());
            assert!(matches!(
                target.render_until(
                    &Scene::new(),
                    version(2, None, None),
                    Instant::now() + Duration::from_secs(10)
                ),
                Err(BridgeError::DeviceLost(_))
            ));
            assert_eq!(target.last_render_attempts, 0);
        }

        // Phase `gpu_target_device_loss_at_marker_hides_output`.
        {
            let mut target = required_target(128, 128);
            let mut test = completion_clock();
            test.destroy_at_marker = true;
            let deadline = test.start + Duration::from_secs(1);
            target.completion_test = Some(test);
            let result = target.render_until(&Scene::new(), version(1, None, None), deadline);
            assert!(
                matches!(result, Err(BridgeError::DeviceLost(_))),
                "marker-time device loss must not publish: {result:?}"
            );
            assert!(target.completed_view().is_none());
            assert_eq!(target.completion_markers, 1);
        }
    }

    // Phase `gpu_target_aborted_or_poisoned_frames_submit_and_publish_nothing`.
    {
        // Phase `gpu_target_encoded_dense_and_sparse_abort_before_submission`.
        {
            for sparse in [false, true] {
                let mut target = required_target(128, 128);
                let params = RenderParams {
                    base_color: Color::BLACK,
                    width: 128,
                    height: 128,
                    antialiasing_method: AaConfig::Area,
                };
                let scene = Scene::new();
                let mut encoded = false;
                let result = if sparse {
                    let damage = [vello::FineDamageRect {
                        x: 16,
                        y: 16,
                        width: 16,
                        height: 16,
                    }];
                    target
                        .renderer
                        .render_to_texture_with_damage_and_post_render_checked(
                            &target.device,
                            &target.queue,
                            &scene,
                            &target.view,
                            &params,
                            &damage,
                            |_| encoded = true,
                            || false,
                        )
                        .map(|_| ())
                } else {
                    target.renderer.render_to_texture_with_post_render_checked(
                        &target.device,
                        &target.queue,
                        &scene,
                        &target.view,
                        &params,
                        |_| encoded = true,
                        || false,
                    )
                };
                assert!(
                    encoded,
                    "gate test must reach actual post-encoding boundary"
                );
                assert!(
                    matches!(result, Err(vello::Error::SubmissionAborted)),
                    "gate must cancel encoded render: {result:?}"
                );
            }
        }

        // Phase `gpu_target_expiry_after_encoding_submits_no_render_or_marker`.
        {
            for sparse in [false, true] {
                let mut target = required_target(128, 128);
                target
                    .render_until(
                        &Scene::new(),
                        version(1, None, None),
                        Instant::now() + Duration::from_secs(10),
                    )
                    .unwrap();
                let mut test = completion_clock();
                test.encode_elapsed = Duration::from_secs(2);
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);
                let regions = [vello::FineDamageRect {
                    x: 16,
                    y: 16,
                    width: 16,
                    height: 16,
                }];
                let result = target.render_until(
                    &Scene::new(),
                    version(2, Some(1), sparse.then_some(&regions)),
                    deadline,
                );
                assert!(matches!(result, Err(BridgeError::DeadlineExceeded)));
                assert_eq!(target.last_render_attempts, 0);
                assert_eq!(target.completion_markers, 0);
                assert!(target.completed_view().is_none() && target.poisoned);
            }
        }

        // Phase `gpu_target_aborted_encoding_does_not_publish_timestamps`.
        {
            let mut target = required_target_with_env("VELLO_GPU_TIMESTAMPS", "1", 128, 128);
            let mut test = completion_clock();
            test.encode_elapsed = Duration::from_secs(2);
            let deadline = test.start + Duration::from_secs(1);
            target.completion_test = Some(test);
            assert!(matches!(
                target.render_until(&Scene::new(), version(1, None, None), deadline),
                Err(BridgeError::DeadlineExceeded)
            ));
            assert_eq!(
                target.vello().gpu_timestamp_latest_submission_id(),
                None,
                "aborted recording must not advertise a submitted timestamp identity"
            );
            assert!(target.vello_mut().take_gpu_timestamp_sample().is_none());
            assert!(target.completed_view().is_none());
        }

        // Phase `gpu_target_late_counter_callback_cannot_publish_replacement`.
        {
            let mut context = RenderContext::new();
            let id = context.device(None).block_on().expect("real GPU adapter");
            let gpu = &context.devices[id];
            assert!(matches!(
                RetainedGpuTarget::new(gpu, 0, 128),
                Err(BridgeError::InvalidSize(_))
            ));
            assert!(matches!(
                RetainedGpuTarget::new(gpu, gpu.device.limits().max_texture_dimension_2d + 1, 128),
                Err(BridgeError::InvalidSize(_))
            ));
            let mut old = RetainedGpuTarget::new(gpu, 128, 128).unwrap();
            let mut test = completion_clock();
            test.failure = Some(WaitPhase::Marker);
            let deadline = test.start + Duration::from_secs(1);
            old.completion_test = Some(test);
            assert!(matches!(
                old.render_until(&Scene::new(), version(1, None, None), deadline),
                Err(BridgeError::DeadlineExceeded)
            ));
            let mut replacement = RetainedGpuTarget::new(gpu, 128, 128).unwrap();
            gpu.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(1)),
                })
                .unwrap();
            assert!(matches!(
                old.renderer.poll_bump_budget(&gpu.device),
                Ok(vello::BumpBudgetPoll::Complete { .. })
            ));
            assert!(
                old.completed_view().is_none(),
                "late callback cannot unpoison old target"
            );
            assert!(
                replacement.completed_view().is_none(),
                "late callback cannot validate fresh target"
            );
            replacement
                .render_until(
                    &Scene::new(),
                    version(2, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert!(replacement.completed_view().is_some());
            assert!(old.completed_view().is_none());
        }

        // Phase `gpu_target_poisoned_shared_coarse_diagnostic_rejects_before_encoding`.
        {
            assert_poisoned_diagnostic_rejected(|bridge, scene, left, right| {
                bridge
                    .shared_coarse_fine_diagnostic_into_xrgb8888(
                        scene,
                        &[vello::FineDamageRect {
                            x: 16,
                            y: 16,
                            width: 16,
                            height: 16,
                        }],
                        left,
                        right,
                        512,
                    )
                    .map(|_| ())
            });
        }

        // Phase `gpu_target_poisoned_geometry_diagnostic_rejects_before_encoding`.
        {
            assert_poisoned_diagnostic_rejected(|bridge, scene, left, right| {
                bridge
                    .shared_coarse_fine_geometry_diagnostic_into_xrgb8888(
                        scene,
                        &[vello::FineDamageRect {
                            x: 16,
                            y: 16,
                            width: 16,
                            height: 16,
                        }],
                        left,
                        right,
                        512,
                    )
                    .map(|_| ())
            });
        }

        // Phase `gpu_target_poisoned_segment_replay_rejects_before_encoding`.
        {
            assert_poisoned_diagnostic_rejected(|bridge, scene, left, right| {
                let input = vello::SegmentOrderReplayInput {
                    tile: 0,
                    logical_command: 0,
                    size_and_rule: 98,
                    backdrop: 0,
                    expected: [[0; 5]; 49],
                    left: [[0; 5]; 49],
                    right: [[0; 5]; 49],
                };
                bridge
                    .segment_order_replay_into_xrgb8888(scene, &input, left, right, 512)
                    .map(|_| ())
            });
        }
    }
}

/// Recovery back-off is chosen by the error's type, never its wording: a message that reads
/// like a timeout does not start it, and a typed timeout does whatever it says.
#[test]
fn only_typed_timeouts_start_backoff() {
    let mut bridge = required_bridge(64, 64);
    for phase in [
        CompletionPhase::Fence,
        CompletionPhase::MapCallback,
        CompletionPhase::CounterSettlement,
        CompletionPhase::FrameBudget,
    ] {
        bridge.recovery_not_before = None;
        bridge.note_render_failure(&BridgeError::CompletionTimeout { phase });
        assert!(
            bridge.recovery_not_before.is_some(),
            "{phase:?} timeout starts the back-off"
        );
    }
    for error in [
        BridgeError::Map("LinuxPerPhase map_async channel: disconnected".to_owned()),
        BridgeError::Map("LinuxPerPhase fence wait: Timeout".to_owned()),
        BridgeError::BumpReadback("LinuxPerPhase counter settlement timeout (1s)".to_owned()),
        BridgeError::DevicePoll("Timeout".to_owned()),
        BridgeError::DeadlineExceeded,
    ] {
        bridge.recovery_not_before = Some(Instant::now() + Duration::from_secs(1));
        bridge.note_render_failure(&error);
        assert!(
            bridge.recovery_not_before.is_none(),
            "{error:?} is not a completion timeout and fails closed"
        );
        assert_eq!(bridge.recovery_backoff, RECOVERY_BACKOFF_INITIAL);
    }
}

/// A map callback that can never report (its sender dropped, as on device loss) is a
/// failure to fail closed on, not a timeout to wait out: the next frame is not held back.
#[test]
fn disconnected_map_channel_is_not_a_timeout() {
    let mut bridge = required_bridge(128, 128);
    let mut test = completion_clock();
    test.disconnect_map_callback = true;
    bridge.target.completion_test = Some(test);
    let mut bytes = vec![0xAB; 128 * 128 * 4];
    let result =
        bridge.paint_damage_into_xrgb8888(&Scene::new(), version(1, None, None), &mut bytes, 512);
    assert!(
        matches!(result, Err(BridgeError::Map(_))),
        "a dropped callback is a mapping failure: {result:?}"
    );
    assert!(bridge.recovery_not_before.is_none(), "no back-off");
    assert!(bytes.iter().all(|byte| *byte == 0xAB), "nothing published");
    let test = bridge.target.completion_test.as_ref().unwrap();
    assert_eq!(
        test.waits.last().map(|(phase, _)| *phase),
        Some(WaitPhase::MapCallback),
        "the failure is the callback wait's own"
    );
    // The next frame renders at once on a rebuilt target.
    bridge
        .paint_damage_into_xrgb8888(&Scene::new(), version(2, None, None), &mut bytes, 512)
        .expect("no back-off holds the next frame");
}

/// Pending counters are settled by one blocking wait on their submission for the whole
/// remaining allowance, not sampled in millisecond sleeps until the deadline.
#[test]
fn counter_settlement_waits_on_the_submission_not_a_sleep_loop() {
    let mut target = required_target(128, 128);
    let params = RenderParams {
        base_color: Color::BLACK,
        width: 128,
        height: 128,
        antialiasing_method: AaConfig::Area,
    };
    let scene = rect_scene(128, 128, Color::WHITE);
    // A real submission whose counter readback is still in flight when settlement starts.
    let submission = target
        .renderer
        .render_to_texture_with_post_render_checked_indexed(
            &target.device,
            &target.queue,
            &scene,
            &target.view,
            &params,
            |_| {},
            || true,
        )
        .expect("submitted");
    let mut test = completion_clock();
    test.pending_elapsed = Duration::from_millis(1);
    // However the real readback stands, the first poll reports it pending.
    test.counters.push_back(Ok(vello::BumpBudgetPoll::Pending));
    let frame_deadline = test.start + LINUX_FRAME_BUDGET;
    target.completion_test = Some(test);
    let overflowed = target
        .settle_counters(
            CompletionPolicy::LinuxPerPhase { frame_deadline },
            Some(&submission),
        )
        .expect("the submission completes and its counters settle");
    assert!(!overflowed);
    let test = target.completion_test.as_ref().unwrap();
    assert_eq!(
        test.counter_waits,
        [BUMP_SETTLEMENT_WAIT],
        "one wait for the whole allowance"
    );
    assert_eq!(
        test.counter_remaining.len(),
        2,
        "pending, then complete once the submission's callbacks ran"
    );
}

/// Recovery attempts share the frame's one Linux budget: three slow overflowing attempts
/// fail as a completion timeout at the budget instead of a fourth taking the frame past it.
#[test]
fn linux_recovery_attempts_share_one_frame_budget() {
    let mut bridge = required_bridge(128, 128);
    let mut test = completion_clock();
    test.fence_elapsed = Duration::from_secs(1);
    test.callback_elapsed = Duration::from_secs(1);
    // Each attempt's counters stay pending for 800 ms of its 1 s allowance, then overflow.
    test.counters = (0..3)
        .flat_map(|_| {
            [
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Complete { overflowed: true }),
            ]
        })
        .collect();
    bridge.target.completion_test = Some(test);
    let mut bytes = vec![0xAB; 128 * 128 * 4];
    let result =
        bridge.paint_damage_into_xrgb8888(&Scene::new(), version(1, None, None), &mut bytes, 512);
    assert!(
        matches!(
            result,
            Err(BridgeError::CompletionTimeout {
                phase: CompletionPhase::CounterSettlement
            })
        ),
        "the third attempt's settlement runs out of frame budget: {result:?}"
    );
    assert_eq!(bridge.last_render_attempts(), 3);
    let test = bridge.target.completion_test.as_ref().unwrap();
    assert_eq!(test.elapsed, LINUX_FRAME_BUDGET);
    assert!(bridge.recovery_not_before.is_some(), "a timeout backs off");
    assert!(bytes.iter().all(|byte| *byte == 0xAB), "nothing published");
}

/// A GPU reset under a live session (amdgpu cancelled the context during a window-switch
/// animation at 5K) used to panic inside `Device::poll` and take the session to a black
/// screen. The reset here is the driver's: every fence wait answers `DeviceError::Lost`, and
/// wgpu-core invalidates the device and runs its lost callback as it does for a real one.
// This injects a Vulkan fence failure in the Linux DRM renderer.
// The backend-independent device-destruction tests remain available on macOS.
#[cfg(target_os = "linux")]
#[test]
fn gpu_target_a_gpu_reset_mid_session_is_survived_by_a_new_bridge() {
    const SIZE: u32 = 64;
    let pitch = SIZE as usize * 4;
    let scene = rect_scene(SIZE, SIZE, Color::WHITE);
    let mut scanout = vec![0x11; pitch * SIZE as usize];
    let mut bridge = required_bridge(SIZE, SIZE);
    bridge
        .paint_damage_into_xrgb8888(&scene, version(1, None, None), &mut scanout, pitch)
        .expect("the frame before the reset");

    // SAFETY: the hal device is only flagged; no raw handle leaves this block.
    let hal = unsafe { bridge.target.device.as_hal::<wgpu::hal::api::Vulkan>() };
    hal.expect("the compositor renders through Vulkan")
        .simulate_device_loss();

    scanout.fill(0x11);
    let result =
        bridge.paint_damage_into_xrgb8888(&scene, version(2, None, None), &mut scanout, pitch);
    assert!(
        matches!(result, Err(BridgeError::DeviceLost(_))),
        "the frame during the reset reports the loss: {result:?}"
    );
    assert!(
        scanout.iter().all(|byte| *byte == 0x11),
        "a lost frame publishes nothing, so the last good front stays on screen"
    );
    assert!(
        bridge.device_lost().is_some(),
        "the DRM backend replaces a bridge whose device_lost() is set"
    );

    let mut replacement = required_bridge(SIZE, SIZE);
    replacement
        .paint_damage_into_xrgb8888(&scene, version(1, None, None), &mut scanout, pitch)
        .expect("the replacement bridge paints on a new device");
    assert!(
        scanout
            .chunks_exact(4)
            .all(|pixel| pixel.get(..3) == Some(&[0xFF, 0xFF, 0xFF][..])),
        "the replacement repaints the whole scene"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn gpu_target_recovery_is_budgeted_and_always_waits_on_completion() {
    // Phase `gpu_target_recovery_stays_within_attempt_phase_and_deadline_budgets`.
    {
        // Phase `gpu_target_overflow_uses_four_attempt_budget`.
        {
            let mut target = required_target(384, 256);
            let region = vello::FineDamageRect {
                x: 144,
                y: 80,
                width: 96,
                height: 96,
            };
            target
                .render_until(
                    &Scene::new(),
                    version(1, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            let scene = opaque_rect_stack_in_rect_scene(region);
            let outcome = target
                .render_until(
                    &scene,
                    version(2, Some(1), Some(&[region])),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(outcome.sparse_attempts, 1);
            assert!(
                outcome.full_attempts >= 1,
                "real sparse overflow must recover densely"
            );
            assert_eq!(
                outcome.render_attempts,
                outcome.sparse_attempts + outcome.full_attempts
            );
            assert_eq!(outcome.completion_markers, outcome.render_attempts);
            assert!(outcome.render_attempts <= 4);
            let actual = target_pixels(&target);
            let mut oracle = required_target(384, 256);
            oracle
                .render_until(
                    &scene,
                    version(2, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(actual, target_pixels(&oracle));
            let mut test = completion_clock();
            test.counters = (0..4)
                .map(|_| Ok(vello::BumpBudgetPoll::Complete { overflowed: true }))
                .collect();
            let deadline = test.start + Duration::from_secs(10);
            target.completion_test = Some(test);
            let result =
                target.render_until(&scene, version(3, Some(2), Some(&[region])), deadline);
            assert!(matches!(
                result,
                Err(BridgeError::RecoveryExhausted { attempts: 4 })
            ));
            assert_eq!(target.last_sparse_render_attempts, 1);
            assert_eq!(target.last_full_render_attempts, 3);
            assert_eq!(target.completion_markers, 4);
            assert!(target.completed_view().is_none());
            // Every exhausted attempt completed and settled, so the renderer and the budget
            // those attempts grew are kept: rebuilding it would restart from the initial
            // budget and overflow the same way on every frame.
            assert!(!target.poisoned);
            target
                .render_until(&scene, version(4, None, None), deadline)
                .expect("the next frame renders on the same target");
            assert_eq!(target_pixels(&target), target_pixels(&oracle));
        }

        // Phase `gpu_target_linux_phase_limits_survive_recovery`.
        {
            let mut bridge = required_bridge(128, 128);
            let mut test = completion_clock();
            test.fence_elapsed = Duration::from_millis(750);
            test.callback_elapsed = Duration::from_millis(750);
            test.counters = [
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Complete { overflowed: true }),
                Ok(vello::BumpBudgetPoll::Complete { overflowed: false }),
            ]
            .into();
            bridge.target.completion_test = Some(test);
            let mut actual = vec![0; 128 * 128 * 4];
            bridge
                .paint_damage_into_xrgb8888(&Scene::new(), version(1, None, None), &mut actual, 512)
                .unwrap();
            let test = bridge.target.completion_test.as_ref().unwrap();
            assert_eq!(
                test.waits,
                vec![
                    (WaitPhase::Fence, Duration::from_secs(1)),
                    (WaitPhase::MapCallback, Duration::from_secs(1)),
                    (WaitPhase::Fence, Duration::from_secs(1)),
                    (WaitPhase::MapCallback, Duration::from_secs(1))
                ]
            );
            assert_eq!(
                &test.counter_remaining[..4],
                &[
                    Duration::from_millis(1000),
                    Duration::from_millis(800),
                    Duration::from_millis(600),
                    Duration::from_millis(1000)
                ]
            );
            assert!(
                test.elapsed > Duration::from_secs(1),
                "Linux has separate phase allowances"
            );
            assert_eq!(bridge.last_render_attempts(), 2);
            assert_eq!(bridge.target.completion_markers, 0);
            assert!(bridge.retained_staging_valid && bridge.target.retained_target_valid);
            let explicit_budget: Duration = test
                .waits
                .iter()
                .map(|(_, duration)| *duration)
                .sum::<Duration>()
                + BUMP_SETTLEMENT_WAIT * 2;
            assert!(explicit_budget <= Duration::from_secs(12));

            let mut exhausted = required_bridge(128, 128);
            let mut test = completion_clock();
            test.fence_elapsed = Duration::from_millis(750);
            test.callback_elapsed = Duration::from_millis(750);
            test.counters = (0..4)
                .map(|_| Ok(vello::BumpBudgetPoll::Complete { overflowed: true }))
                .collect();
            exhausted.target.completion_test = Some(test);
            assert!(matches!(
                exhausted.paint_damage_into_xrgb8888(
                    &Scene::new(),
                    version(1, None, None),
                    &mut actual,
                    512
                ),
                Err(BridgeError::RecoveryExhausted { attempts: 4 })
            ));
            let test = exhausted.target.completion_test.as_ref().unwrap();
            let requested: Duration = test
                .waits
                .iter()
                .map(|(_, duration)| *duration)
                .sum::<Duration>()
                + BUMP_SETTLEMENT_WAIT * 4;
            assert_eq!(requested, Duration::from_secs(12));
            assert_eq!(exhausted.last_render_attempts(), 4);
            assert!(exhausted.target.completed_view().is_none());
        }

        // Phase `gpu_target_linux_phase_failures_abort_without_recovery`.
        {
            // Every individual Linux phase failure aborts immediately; no completed overflow may
            // turn a failed fence/callback/counter observation into another attempt.
            for phase in [Some(WaitPhase::Fence), Some(WaitPhase::MapCallback), None] {
                let mut bridge = required_bridge(128, 128);
                let mut test = completion_clock();
                test.failure = phase;
                if phase.is_none() {
                    test.counters = (0..8).map(|_| Ok(vello::BumpBudgetPoll::Pending)).collect();
                }
                bridge.target.completion_test = Some(test);
                let mut bytes = vec![0xAB; 128 * 128 * 4];
                let result = bridge.paint_damage_into_xrgb8888(
                    &Scene::new(),
                    version(1, None, None),
                    &mut bytes,
                    512,
                );
                assert!(result.is_err());
                assert_eq!(bridge.last_render_attempts(), 1);
                assert!(!bridge.retained_staging_valid && bridge.target.completed_view().is_none());
                assert!(bytes.iter().all(|byte| *byte == 0xAB));
                let identity = bridge.xrgb_history_identity(512, 1);
                assert!(matches!(
                    bridge.paint_damage_into_xrgb8888(
                        &Scene::new(),
                        version(2, Some(1), None),
                        &mut bytes,
                        512,
                    ),
                    Err(BridgeError::RecoveryBackoff { .. })
                ));
                assert_eq!(bridge.last_render_attempts(), 1);
                assert!(bytes.iter().all(|byte| *byte == 0xAB));
                // Once the cool-down expires the poisoned target is reconstructed exactly once; the
                // failure fixture belongs to the old target and cannot poison its replacement.
                bridge.recovery_not_before = Some(
                    Instant::now()
                        .checked_sub(Duration::from_millis(1))
                        .expect("monotonic clock is past its first millisecond"),
                );
                bridge
                    .paint_damage_into_xrgb8888(
                        &Scene::new(),
                        version(3, Some(2), None),
                        &mut bytes,
                        512,
                    )
                    .expect("bridge reconstructs poisoned renderer after backoff");
                assert_ne!(bridge.xrgb_history_identity(512, 1), identity);
                assert!(bridge.target.completed_view().is_some());
            }
        }

        // Phase `gpu_target_absolute_deadline_spans_recovery`.
        {
            let mut target = required_target(128, 128);
            let mut test = completion_clock();
            test.fence_elapsed = Duration::from_millis(750);
            test.counters = [
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Pending),
                Ok(vello::BumpBudgetPoll::Complete { overflowed: true }),
            ]
            .into();
            let deadline = test.start + Duration::from_secs(1);
            target.completion_test = Some(test);
            let result = target.render_until(&Scene::new(), version(1, None, None), deadline);
            assert!(matches!(result, Err(BridgeError::DeadlineExceeded)));
            assert_eq!(target.last_render_attempts, 1);
            assert_eq!(target.completion_markers, 1);
            assert!(target.completed_view().is_none());
            let test = target.completion_test.as_ref().unwrap();
            assert_eq!(
                test.counter_remaining,
                [Duration::from_millis(250), Duration::from_millis(50)]
            );
            assert_eq!(test.elapsed, Duration::from_secs(1));

            // A completed overflow consumes the same caller deadline on the next attempt.
            let mut target = required_target(128, 128);
            let mut test = completion_clock();
            test.fence_elapsed = Duration::from_millis(750);
            test.counters
                .push_back(Ok(vello::BumpBudgetPoll::Complete { overflowed: true }));
            let deadline = test.start + Duration::from_secs(1);
            target.completion_test = Some(test);
            assert!(matches!(
                target.render_until(&Scene::new(), version(1, None, None), deadline),
                Err(BridgeError::DeadlineExceeded)
            ));
            assert_eq!(target.last_render_attempts, 2);
            assert_eq!(target.completion_markers, 2);
            assert_eq!(
                target.completion_test.as_ref().unwrap().waits,
                [
                    (WaitPhase::Marker, Duration::from_secs(1)),
                    (WaitPhase::Marker, Duration::from_millis(250))
                ]
            );
        }
    }

    // Phase `gpu_target_always_waits_on_a_completion_marker`.
    {
        // Phase `gpu_target_disabled_measurement_still_waits_for_completion`.
        {
            let mut target = required_target_with_env("VELLO_BUMP_BUDGET", "upstream", 128, 128);
            assert_eq!(
                target.renderer.poll_bump_budget(&target.device).unwrap(),
                vello::BumpBudgetPoll::Complete { overflowed: false }
            );
            let mut test = completion_clock();
            test.failure = Some(WaitPhase::Marker);
            let deadline = test.start + Duration::from_secs(1);
            target.completion_test = Some(test);
            assert!(matches!(
                target.render_until(&Scene::new(), version(1, None, None), deadline),
                Err(BridgeError::DeadlineExceeded)
            ));
            assert_eq!(target.completion_markers, 1);
            assert_eq!(
                target.renderer.poll_bump_budget(&target.device).unwrap(),
                vello::BumpBudgetPoll::Complete { overflowed: false }
            );
            assert!(target.completed_view().is_none());
            let mut replacement =
                required_target_with_env("VELLO_BUMP_BUDGET", "upstream", 128, 128);
            let stats = replacement
                .render_until(
                    &Scene::new(),
                    version(2, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(stats.completion_markers, stats.render_attempts);
            assert!(stats.completion_markers > 0 && replacement.completed_view().is_some());
        }

        // Phase `gpu_target_post_render_is_covered_by_existing_completion_marker`.
        {
            let mut target = required_target(8, 4);
            let output = target.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("retained-bgra-test-output"),
                size: wgpu::Extent3d {
                    width: 8,
                    height: 4,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = output.create_view(&wgpu::TextureViewDescriptor::default());
            let mut test = completion_clock();
            test.failure = Some(WaitPhase::Marker);
            let deadline = test.start + Duration::from_secs(1);
            target.completion_test = Some(test);

            assert!(matches!(
                target.render(
                    &Scene::new(),
                    Some(version(1, None, None)),
                    FrameOutput::GpuOnly,
                    CompletionPolicy::Absolute(deadline),
                    None,
                    |device, encoder, source| {
                        TextureBlitter::new(device, wgpu::TextureFormat::Bgra8Unorm)
                            .copy(device, encoder, source, &view);
                    },
                ),
                Err(BridgeError::DeadlineExceeded)
            ));
            assert_eq!(target.last_render_attempts, 1);
            assert_eq!(target.completion_markers, 1);
            assert!(target.completed_view().is_none() && target.poisoned);
        }
    }
}

fn assert_poisoned_diagnostic_rejected(
    diagnostic: impl Fn(&mut VelloDrmBridge, &Scene, &mut [u8], &mut [u8]) -> Result<(), BridgeError>,
) {
    let mut bridge = required_bridge(128, 128);
    let mut test = completion_clock();
    test.failure = Some(WaitPhase::Fence);
    bridge.target.completion_test = Some(test);
    let mut left = vec![0xA5; 128 * 128 * 4];
    let mut right = left.clone();
    let scene = rect_scene(128, 128, Color::WHITE);
    assert!(matches!(
        bridge.paint_damage_into_xrgb8888(&scene, version(1, None, None), &mut left, 512),
        Err(BridgeError::CompletionTimeout {
            phase: CompletionPhase::Fence
        })
    ));
    assert!(bridge.target.poisoned);
    let attempts = bridge.last_render_attempts();
    let packed = bridge.diagnostic_packed_scene().to_vec();
    let texture = bridge.target.texture.clone();
    let identity = bridge.xrgb_history_identity(512, 1);
    assert!(
        matches!(
            diagnostic(&mut bridge, &Scene::new(), &mut left, &mut right),
            Err(BridgeError::RecreateRequired)
        ),
        "diagnostic must reject before reusing the poisoned renderer"
    );
    assert_eq!(bridge.last_render_attempts(), attempts);
    assert_eq!(bridge.diagnostic_packed_scene(), packed);
    assert_eq!(bridge.target.texture, texture);
    assert_eq!(bridge.xrgb_history_identity(512, 1), identity);
    assert!(bridge.target.completed_view().is_none());
    assert!(!bridge.retained_staging_valid);
    assert!(left.iter().chain(&right).all(|byte| *byte == 0xA5));
    // The poison oracle above deliberately leaves a submitted fence unresolved. Drain
    // that fixture before testing independently observed device loss; production diagnostics
    // themselves do not poll, wait, or reuse the poisoned renderer.
    bridge
        .target
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(1)),
        })
        .expect("bounded test-only drain before device destruction");
    bridge.target.device.destroy();
    let _ = bridge.target.device.poll(wgpu::PollType::Poll);
    assert!(
        matches!(
            diagnostic(&mut bridge, &scene, &mut left, &mut right),
            Err(BridgeError::DeviceLost(_))
        ),
        "actual device loss takes precedence over reconstruction"
    );
}
