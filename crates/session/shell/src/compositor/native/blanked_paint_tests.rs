use super::{NativeRedrawReasons, TickPaint, release_idle_client_callback, tick_paint};
use crate::compositor::backend::OutputId;
use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
use crate::compositor::present::{PresentPipe, Presenter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Counts what reaches the GPU.
struct CountingPresenter(Arc<AtomicUsize>);

impl Presenter for CountingPresenter {
    fn present(&mut self, _scene: &vello::Scene, _output: OutputId) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// One tick of a terminal that commits a frame every tick, through the
/// loop's own paint decision; returns whether a frame was built and handed
/// to the presenter.
fn tick(
    session: &SessionManager,
    pipe: &mut PresentPipe,
    owed: &mut bool,
    session_changed: bool,
) -> (TickPaint, bool) {
    let reasons = NativeRedrawReasons {
        apps: true,
        session: session_changed,
        owed: *owed,
        ..NativeRedrawReasons::default()
    };
    let paint = tick_paint(
        reasons,
        session,
        true,
        pipe.has_capacity() || pipe.can_replace_queued(),
    );
    *owed = paint.owed;
    if paint.should_paint {
        assert!(pipe.submit(vello::Scene::new(), OutputId(0)));
    }
    let released = release_idle_client_callback(
        session.is_locked() || session.is_blanked(),
        paint.content_changed,
        paint.should_paint,
    );
    (paint, released)
}

#[test]
fn a_blanked_display_draws_and_submits_nothing_until_it_wakes() {
    let start = Instant::now();
    // Lock "Never": only the blank hides the desktop.
    let mut session = SessionManager::new(
        IdleConfig {
            blank_timeout: Some(Duration::from_secs(5)),
            lock_timeout: None,
            suspend_timeout: None,
        },
        start,
    );
    let presented = Arc::new(AtomicUsize::new(0));
    let mut pipe = PresentPipe::spawn(
        Box::new(CountingPresenter(Arc::clone(&presented))),
        Box::new(|| {}),
    );
    let mut owed = false;

    // Awake, a committing terminal is drawn.
    let (awake, _) = tick(&session, &mut pipe, &mut owed, false);
    assert!(awake.should_paint);

    let blank_at = start + Duration::from_secs(5);
    assert!(session.tick(blank_at).contains(&SessionTransition::Blanked));
    assert!(session.is_blanked() && !session.is_locked());
    let submitted_before = pipe.submitted();

    // Two seconds of a terminal committing at 60 Hz while the display is dark.
    let mut frames = 0;
    for _ in 0..120 {
        let (paint, released) = tick(&session, &mut pipe, &mut owed, false);
        frames += usize::from(paint.should_paint);
        assert!(!paint.content_changed, "nothing is shown on a dark display");
        assert!(
            !released,
            "the client waits for the frame that wakes the display"
        );
    }
    assert_eq!(frames, 0, "no frame is built while blanked");
    assert_eq!(
        pipe.submitted(),
        submitted_before,
        "nothing is handed to the GPU"
    );
    assert!(owed, "what changed is drawn when the display wakes");

    // Input wakes it: the first tick draws the desktop as it now is.
    let woke = session.note_activity(blank_at + Duration::from_secs(2));
    assert!(woke.contains(&SessionTransition::Woke));
    let (first, released) = tick(&session, &mut pipe, &mut owed, true);
    assert!(first.should_paint && released);
    assert_eq!(pipe.submitted(), submitted_before + 1);
    pipe.shutdown();
    assert_eq!(
        presented.load(Ordering::SeqCst),
        2,
        "one frame before the blank and one on waking reached the GPU"
    );
}
