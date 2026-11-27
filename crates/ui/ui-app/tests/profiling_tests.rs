use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::profiling::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
#[test]
fn profiler_records_passes_commits_and_over_budget_frames() {
    {
        let mut p = FrameProfiler::for_capture();
        p.record(Pass::Sync, Duration::from_micros(100));
        p.record(Pass::BuildLayout, Duration::from_micros(200));
        p.record(Pass::ApplyLayout, Duration::from_micros(50));
        p.record(Pass::Draw, Duration::from_micros(400));
        p.record(Pass::HandleEvent, Duration::from_micros(80));
        p.commit_frame_summed();

        let samples = p.take_capture();
        assert_eq!(samples.len(), 1);
        let f = samples[0];
        assert_eq!(f.sync_us, 100);
        assert_eq!(f.build_layout_us, 200);
        assert_eq!(f.apply_layout_us, 50);
        assert_eq!(f.draw_us, 400);
        assert_eq!(f.handle_event_us, 80);
        assert_eq!(f.total_us, 830);
        assert!(!f.over_budget());
    }

    {
        let mut profiler = FrameProfiler::for_capture();
        profiler.record(Pass::ResourceAvailability, Duration::from_micros(11));
        profiler.record(Pass::ResourceAvailability, Duration::from_micros(7));
        profiler.record(Pass::ScenePoolWait, Duration::from_micros(23));
        profiler.record(Pass::PresenceOverlay, Duration::from_micros(31));
        profiler.record(Pass::HandleEvent, Duration::from_micros(5));
        profiler.commit_frame_summed();
        profiler.commit_frame_summed();
        let samples = profiler.take_capture();
        assert_eq!(samples[0].total_us, 77);
        assert_eq!(samples[1].total_us, 0);
        let json = serde_json::to_value(samples[0]).expect("serialize frame");
        assert_eq!(json["resource_availability_us"], 18);
        assert_eq!(json["scene_pool_wait_us"], 23);
        assert_eq!(json["presence_overlay_us"], 31);
        let report = PerfReport::from_samples("diagnostic", samples.clone(), FRAME_BUDGET_US);
        assert_eq!(report.resource_availability.max_us, 18);
        assert_eq!(report.scene_pool_wait.max_us, 23);
        assert_eq!(report.presence_overlay.max_us, 31);

        let dir = tempfile::tempdir().expect("temporary profile directory");
        let path = dir.path().join("frames.csv");
        let mut writer = DumpWriter::open(&path).expect("open profile dump");
        writer.write(&samples[0]);
        writer.write(&samples[1]);
        drop(writer);
        let csv = std::fs::read_to_string(path).expect("read profile dump");
        let mut lines = csv.lines();
        let names: Vec<_> = lines.next().expect("header").split(',').collect();
        let values: Vec<_> = lines.next().expect("frame").split(',').collect();
        assert_eq!(names.len(), values.len());
        for (name, value) in names.iter().zip(values) {
            assert_eq!(
                json[*name].as_u64(),
                Some(value.parse().expect("CSV number"))
            );
        }
        assert_eq!(lines.count(), 1, "one header, two frames");
    }

    {
        let mut profiler = FrameProfiler::for_capture();
        let started = tilo_ui::time::now();
        let phases = |dispatch, commit| InputPhases {
            dispatch: Duration::from_micros(dispatch),
            hit_test: Duration::from_micros(3),
            resources: Duration::from_micros(2),
            publication: Duration::from_micros(1),
            commit: Duration::from_micros(commit),
        };
        profiler.record_input(started, Duration::from_micros(120), phases(100, 4));
        profiler.record_input(
            tilo_ui::time::now(),
            Duration::from_micros(40),
            phases(20, 9),
        );
        std::thread::sleep(Duration::from_millis(1));
        profiler.commit_frame(1_000);
        profiler.record(Pass::HandleEvent, Duration::from_micros(7));
        profiler.commit_frame(1_000);
        let samples = profiler.take_capture();
        let banked = samples[0];
        assert_eq!(banked.handle_event_us, 160);
        assert_eq!(banked.input_events, 2);
        assert_eq!(banked.input_event_max_us, 120);
        assert_eq!(banked.input_dispatch_us, 120);
        assert_eq!(banked.input_hit_test_us, 6);
        assert_eq!(banked.input_resources_us, 4);
        assert_eq!(banked.input_publication_us, 2);
        assert_eq!(banked.input_commit_us, 13);
        assert!(
            banked.input_to_commit_us >= 1_000,
            "measured from the oldest event: {banked:?}"
        );
        let next = samples[1];
        assert_eq!(next.handle_event_us, 7);
        assert_eq!(
            (
                next.input_events,
                next.input_event_max_us,
                next.input_to_commit_us
            ),
            (0, 0, 0)
        );

        let mut clock = InputPhaseClock::new(false);
        assert!(
            clock.start().is_none(),
            "no clock read while profiling is off"
        );
        clock.finish(InputPhase::Dispatch, None);
        assert_eq!(clock.phases(), InputPhases::default());
        let mut clock = InputPhaseClock::new(true);
        let start = clock.start();
        assert!(start.is_some());
        std::thread::sleep(Duration::from_millis(1));
        clock.finish(InputPhase::HitTest, start);
        clock.finish(InputPhase::HitTest, start);
        assert!(
            clock.phases().hit_test >= Duration::from_millis(2),
            "spans of one phase accumulate: {:?}",
            clock.phases()
        );
        assert_eq!(clock.phases().dispatch, Duration::ZERO);
    }

    {
        let mk = |total: u64| FrameTiming {
            total_us: total,
            draw_us: total,
            ..Default::default()
        };
        let samples = vec![mk(5_000), mk(20_000), mk(8_000), mk(30_000)];
        let r = PerfReport::from_samples("t", samples, FRAME_BUDGET_US);
        assert_eq!(r.frames, 4);
        assert_eq!(r.over_budget, 2); // 20ms and 30ms exceed 16.6ms
        assert_eq!(r.draw.max_us, 30_000);
    }

    {
        let clicks = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = clicks.clone();
        let root = tilo_ui::widget::Button::new("activate", "Activate")
            .on_click(move || observed.set(observed.get() + 1));
        let mut driver =
            tilo_ui::headless::Driver::new(tilo_ui::State::new(), Box::new(root), (400, 300));
        let scenario = PerfScenario {
            name: "real-click".into(),
            viewport: (400, 300),
            warmup: 1,
            steps: vec![PerfStep::ClickNamed("activate".into()), PerfStep::Frames(2)],
        };
        let report = run_scenario(&mut driver, &scenario);
        assert_eq!(clicks.get(), 1);
        assert!(report.frames >= 2);
    }

    {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut driver = tilo_ui::headless::Driver::new(
                tilo_ui::State::new(),
                Box::new(tilo_ui::widget::Text::new("Root")),
                (400, 300),
            );
            let scenario = PerfScenario {
                name: "missing-target".into(),
                viewport: (400, 300),
                warmup: 1,
                steps: vec![
                    PerfStep::ClickNamed("missing-control".into()),
                    PerfStep::Frames(10),
                ],
            };
            run_scenario(&mut driver, &scenario);
        }))
        .expect_err("a scenario with a missing click must not report success");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains("missing-control"),
            "panic message: {message}"
        );
    }
}
