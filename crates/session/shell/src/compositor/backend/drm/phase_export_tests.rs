//! Exercise the real phase collector with actual GPU work, without claiming KMS.
use super::*;
use crate::shell::launcher::{LaunchSession, diagnostics::Options};

impl DrmPresenter {
    pub(crate) fn phase_export_fixture() -> Self {
        Self {
            scanouts: Vec::new(),
            outputs: Vec::new(),
            pointer: (0.0, 0.0),
            shared_pointer: Arc::new(Mutex::new((0.0, 0.0))),
            paint: None,
            cpu: None,
            blanked: false,
            paused: false,
            commands: None,
            present_gate: None,
            legacy_phase_trace: false,
            launch_session: LaunchSession::current(),
            profile_seq: 0,
            frame_phases: None,
            display_timing: crate::shell::present::DisplayTiming::default(),
            flip_vblanks: Vec::new(),
            paced_queue_depth: 0,
            phase_window: PhaseWindow::default(),
            readback_presenting: Arc::new(AtomicBool::new(false)),
            signal_reports: None,
        }
    }

    pub(crate) fn collect_gpu_phase_fixture(
        &mut self,
    ) -> Option<crate::shell::present::FramePhaseTiming> {
        assert!(
            ensure_bridge(&mut self.paint, (32, 32)),
            "actual GPU bridge required"
        );
        let mut scene = Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            vello::kurbo::Affine::IDENTITY,
            vello::peniko::Color::WHITE,
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, 32.0, 32.0),
        );
        self.paint
            .as_mut()
            .unwrap()
            .paint(&scene)
            .expect("actual GPU render/readback");
        let started = self.phase_export_enabled().then(Instant::now);
        self.collect_frame_phases(started, false);
        self.frame_phases
    }
}

#[test]
fn phase_export_capture_survives_render_thread_and_isolates_sessions() {
    let session = LaunchSession::current();
    session.configure_diagnostics(Options([false; 4]));
    let mut presenter = DrmPresenter::phase_export_fixture();
    session.configure_diagnostics(Options([false, true, false, false]));
    drop(session);
    std::thread::spawn(move || {
        let unrelated = LaunchSession::current();
        unrelated.configure_diagnostics(Options([false; 4]));
        assert!(
            presenter.phase_export_enabled(),
            "captured session survives thread transfer"
        );
        assert!(!DrmPresenter::phase_export_fixture().phase_export_enabled());
        presenter.collect_frame_phases(Some(Instant::now()), false);
        assert!(
            presenter.frame_phases.is_none(),
            "unsupported CPU/no bridge cannot invent phases"
        );
        presenter
            .launch_session
            .configure_diagnostics(Options([false; 4]));
        assert!(!presenter.phase_export_enabled());
        assert!(!unrelated.diagnostic_options().0[1]);
    })
    .join()
    .expect("render thread");
    assert!(!LaunchSession::current().diagnostic_options().0[1]);
}
