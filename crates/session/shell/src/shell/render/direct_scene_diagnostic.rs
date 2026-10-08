// Opt-in native experiment, sharing the renderer gate's actual target and pixel oracle.
// No DRM/KMS, producer import fence or user scene is represented by this owned texture.
const DIAGNOSTIC_WIDTH: u32 = 2_944;
const DIAGNOSTIC_HEIGHT: u32 = 1_840;

fn report_direct_sample(label: &str, revision: u64, timing: &BridgePhaseTiming) {
    let gpu = timing.submission_gpu.expect("actual recording ownership");
    let work = timing.work.expect("actual rendering work");
    eprintln!(
        "DIRECT-SCENE {}",
        serde_json::json!({
            "case": label, "revision": revision,
            "width": work.width, "height": work.height,
            "damage_admitted": work.damage_admitted,
            "full_attempts": work.full_attempts, "sparse_attempts": work.sparse_attempts,
            "render_attempts": work.render_attempts,
            "cpu_submit_us": timing.submit_us,
            "cpu_completion_wait_us": timing.completed_readback_map_us,
            "cpu_settle_us": timing.settle_us, "cpu_backdrop_us": timing.backdrop_us,
            "bridge_total_us": timing.total_us(),
            "gpu_recording_ns": gpu.recording_ns, "gpu_post_render_nested_ns": gpu.post_render_ns,
            "renderer_generation": gpu.renderer_generation,
            "first_recording": gpu.first_recording, "last_recording": gpu.last_recording,
            "recordings": gpu.recordings, "measured": gpu.measured, "stale": gpu.stale,
            "encoder_supported": gpu.encoder_supported, "encoder_enabled": gpu.encoder_enabled,
            "external_queue_work": gpu.external_queue_work, "coverage_complete": gpu.complete(),
            "whole_frame_gpu_elapsed_ns": if gpu.complete() { Some(gpu.recording_ns) } else { None },
            "timed_readback_bytes": timing.readback_bytes,
            "timed_cpu_output_bytes": timing.cpu_output_bytes,
        })
    );
    assert!(
        gpu.encoder_supported && gpu.encoder_enabled,
        "required encoder query observation unavailable: {gpu:?}"
    );
    assert!(
        gpu.measured > 0 && gpu.measured <= gpu.recordings && gpu.stale == 0,
        "required bounded scoped recording observation unavailable: {gpu:?}"
    );
    assert!(gpu.renderer_generation.is_some());
    assert!(
        gpu.first_recording
            .zip(gpu.last_recording)
            .is_some_and(|(first, last)| first > 0 && first <= last)
    );
    if !gpu.external_queue_work {
        assert!(
            gpu.complete(),
            "non-external recording coverage incomplete: {gpu:?}"
        );
    }
    assert_eq!(timing.readback_bytes, 0);
    assert_eq!(timing.cpu_output_bytes, 0);
    assert!(gpu.recording_ns >= gpu.post_render_ns);
}

fn compare_direct_pixels(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len());
    let differences: Vec<_> = actual
        .iter()
        .zip(expected)
        .filter(|(a, b)| a != b)
        .collect();
    // Existing native dense/retained oracle's bounded independent-recording rounding tolerance.
    assert!(
        differences.len() <= 16,
        "dense parity: {} bytes differ",
        differences.len()
    );
    assert!(differences.iter().all(|(a, b)| a.abs_diff(**b) <= 1));
}

struct DirectExperiment {
    observed: VelloDrmBridge,
    oracle: VelloDrmBridge,
    destination: ui::dmabuf_import::ImportedRenderTarget,
    reference: ui::dmabuf_import::ImportedRenderTarget,
}

impl DirectExperiment {
    fn new() -> Self {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("VELLO_GPU_TIMESTAMPS", "0");
        env.set("VELLO_GPU_DISPATCH_TIMESTAMPS", "0");
        let oracle = required_bridge(DIAGNOSTIC_WIDTH, DIAGNOSTIC_HEIGHT);
        env.set("VELLO_GPU_TIMESTAMPS", "1");
        let mut observed = required_bridge(DIAGNOSTIC_WIDTH, DIAGNOSTIC_HEIGHT);
        observed.set_phase_timing_enabled(true);
        assert!(
            observed
                .target
                .device
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
            "diagnostic requires actual encoder timestamp support; no observation skip"
        );
        let adapter = observed.adapter_info().expect("actual bridge adapter");
        eprintln!(
            "DIRECT-SCENE {}",
            serde_json::json!({
                "adapter": adapter.name, "backend": adapter.backend, "device_type": adapter.device_type,
                "driver": adapter.driver, "vendor": adapter.vendor, "device": adapter.device,
                "pci_bus_id": adapter.pci_bus_id, "debug_assertions": cfg!(debug_assertions),
                "target": "owned-bgra-texture", "physical_kms": false,
            })
        );
        let destination = direct_target(&observed, DIAGNOSTIC_WIDTH, DIAGNOSTIC_HEIGHT);
        let reference = direct_target(&oracle, DIAGNOSTIC_WIDTH, DIAGNOSTIC_HEIGHT);
        Self {
            observed,
            oracle,
            destination,
            reference,
        }
    }

    fn sample(
        &mut self,
        label: &str,
        scene: &Scene,
        damage: SceneDamage<'_>,
        backdrop: Option<&std::sync::Arc<ui::backdrop::BackdropFrame>>,
    ) {
        self.observed.set_backdrop(backdrop.cloned());
        self.oracle.set_backdrop(backdrop.cloned());
        let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
        self.observed
            .paint_imported_until(
                scene,
                damage,
                &self.destination,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .expect("actual direct paint");
        let timing = self
            .observed
            .take_phase_timing()
            .expect("actual completed timing");
        report_direct_sample(label, damage.revision, &timing);
        // Oracle and both readbacks follow the measured completed transaction.
        self.oracle
            .paint_imported_until(
                scene,
                version(damage.revision, None, None),
                &self.reference,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .expect("dense direct oracle");
        compare_direct_pixels(
            &direct_scanout_pixels(&self.observed, &self.destination),
            &direct_scanout_pixels(&self.oracle, &self.reference),
        );
    }
}

#[test]
fn owned_qs_overview_and_solid_direct_target_diagnostic() {
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut experiment = DirectExperiment::new();
    let theme = ui::theme::Theme::dark().with_reduce_motion(true);
    let solid = rect_scene(DIAGNOSTIC_WIDTH, DIAGNOSTIC_HEIGHT, theme.text_primary);
    // Warm shader/bump allocations without including their startup work in repeated samples.
    let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
    experiment
        .observed
        .paint_imported_until(
            &solid,
            version(1, None, None),
            &experiment.destination,
            tilo_test_env::hang_deadline(),
            white,
            &[],
        )
        .expect("solid warmup");
    for revision in 2..=4 {
        experiment.sample("solid", &solid, version(revision, None, None), None);
    }
    let mut frame = PopulatedShellFixture::new((DIAGNOSTIC_WIDTH, DIAGNOSTIC_HEIGHT), theme);
    frame.relayout();
    let _ = frame.paint();
    let mut document = AutoCommit::new();
    let mut focus = None;
    for (label, target, quick_settings, overview) in [
        ("qs-open", "topbar_tray_pill", true, false),
        ("qs-closed", "topbar_tray_pill", false, false),
        ("overview-open", "topbar_overview_button", false, true),
        ("overview-closed", "topbar_overview_button", false, false),
        ("qs-open-repeat", "topbar_tray_pill", true, false),
        ("qs-closed-repeat", "topbar_tray_pill", false, false),
    ] {
        frame.relayout();
        let rect = frame
            .widget_registry()
            .get(target)
            .copied()
            .filter(|rect| rect.area() > 0.0)
            .expect("actual production target bounds");
        dispatch_native_benchmark_click(
            &mut frame,
            target,
            rect.center(),
            &mut document,
            &mut focus,
        );
        frame.relayout();
        assert_eq!(frame.chrome().overlays.quick_settings, quick_settings);
        assert_eq!(frame.chrome().overlays.overview, overview);
        let painted = frame.paint_with_damage();
        assert!(!painted.scene.encoding().is_empty());
        experiment.sample(
            label,
            &painted.scene,
            SceneDamage {
                revision: painted.revision,
                previous_revision: painted.previous_revision,
                regions: painted.damage_regions.as_deref(),
            },
            painted.backdrop.as_ref(),
        );
    }
}
