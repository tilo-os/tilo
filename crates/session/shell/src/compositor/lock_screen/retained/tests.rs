use super::*;
use crate::compositor::lock_controller::{LockPrompt, refusal_line};
use crate::compositor::lock_screen::{LockLayout, LockView};
use crate::shell::i18n::t;
use crate::shell::osd::Osd;
use crate::shell::render::paint::{SceneDamage, VelloDrmBridge};
use std::time::{Duration, Instant};
use ui::theme::Theme;

struct Fixture {
    retained: RetainedLockScene,
    builder: FrameBuilder,
    outputs: Vec<LockOutput>,
    osd: Option<Osd>,
    wallpaper: Option<vello::peniko::ImageData>,
    /// The instant the lock paints at. Tests that prove a transition moves it
    /// on; the rest keep it fixed, so a phase never drifts under them.
    now: Instant,
}

impl Fixture {
    fn new(size: (u32, u32)) -> Self {
        Self {
            retained: RetainedLockScene::default(),
            builder: FrameBuilder::new(size),
            outputs: vec![LockOutput {
                origin: (0.0, 0.0),
                size: (f64::from(size.0), f64::from(size.1)),
                scale: 1.0,
            }],
            osd: None,
            wallpaper: None,
            now: Instant::now(),
        }
    }

    fn draw(&mut self, theme: &Theme, view: LockView<'_>) -> FramePaint {
        self.retained.draw(
            &mut self.builder,
            &LockComposition {
                outputs: &self.outputs,
                theme,
                view: &view,
                wallpaper: self.wallpaper.as_ref(),
                osd: self.osd,
                now: self.now,
            },
        )
    }

    fn advance(&mut self, millis: u64) {
        self.now = self
            .now
            .checked_add(Duration::from_millis(millis))
            .expect("bounded test instant");
    }
}

/// The lock with `length` characters typed into `prompt`: an edit, which
/// also takes a refusal down.
fn view(prompt: &LockPrompt, length: usize) -> LockView<'_> {
    {
        let mut shown = prompt.borrow_mut();
        shown.set_busy(None);
        shown.clear_error();
        if let Some(field) = shown.field_mut(ui::widget::FieldRole::Secret) {
            field.set_text(&"x".repeat(length));
        }
    }
    LockView {
        prompt: Some(prompt),
        clock: "10:42",
        date: "Saturday, 24 October",
        ..LockView::default()
    }
}

/// The password handed to the verifier, the prompt saying it checks.
fn verifying(prompt: &LockPrompt) -> LockView<'_> {
    let base = view(prompt, 0);
    prompt
        .borrow_mut()
        .set_busy(Some(t("lock-screen-verifying")));
    LockView {
        verifying: true,
        ..base
    }
}

/// The verifier refused: the prompt says so under its field.
fn failed(prompt: &LockPrompt) -> LockView<'_> {
    const REFUSED: &str = "Incorrect username or password";
    let base = view(prompt, 0);
    prompt.borrow_mut().reject(refusal_line(REFUSED));
    LockView {
        error: Some(REFUSED),
        ..base
    }
}

fn damage_pixels(frame: &FramePaint) -> u64 {
    frame
        .damage_regions
        .as_ref()
        .expect("password-only edit must be bounded")
        .iter()
        .map(|r| u64::from(r.width).saturating_mul(u64::from(r.height)))
        .sum()
}

fn damage_rect(r: &vello::FineDamageRect) -> vello::kurbo::Rect {
    vello::kurbo::Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x.saturating_add(r.width)),
        f64::from(r.y.saturating_add(r.height)),
    )
}

/// Every damage rect lands on the credential card: a credential answer may
/// never repaint the output, and the tile-rounded rect may overhang the panel
/// by one clip tile.
fn assert_credential_damage(frame: &FramePaint, theme: &Theme, outputs: &[LockOutput]) {
    let layout = LockLayout::for_output(outputs[0], theme);
    let margin = 32.0 * layout.scale + 16.0;
    let allowed = layout.panel.inflate(margin, margin);
    let rects = frame
        .damage_regions
        .as_ref()
        .expect("a credential change is a bounded frame");
    assert!(!rects.is_empty(), "the changed credential part must damage");
    for rect in rects {
        let rect = damage_rect(rect);
        assert!(
            allowed.contains(rect.origin()) && allowed.contains((rect.x1, rect.y1)),
            "damage {rect:?} escapes the credential card {allowed:?}"
        );
    }
    let budget = (layout.password.union(layout.status)).inflate(margin, margin);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scene sizes are positive physical pixels well inside u64"
    )]
    let budget = (budget.width() * budget.height()) as u64;
    assert!(
        damage_pixels(frame) <= budget,
        "a credential answer stays inside the field and status rows"
    );
}

fn assert_settled(frame: &FramePaint) {
    assert!(
        frame.damage_regions.as_ref().is_some_and(Vec::is_empty),
        "a resting lock asks for no part to be repainted"
    );
}

#[test]
fn password_changes_are_bounded_and_other_changes_break_the_proof() {
    let p = LockPrompt::new();
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let mut f = Fixture::new((3840, 2160));
        f.retained.clear();
        let first = f.draw(&theme, view(&p, 0));
        assert!(first.damage_regions.is_none());
        let mut previous = first.revision;
        for length in [1, 2, 8, 1, 0, 18, 19, 0] {
            let frame = f.draw(&theme, view(&p, length));
            assert_eq!(frame.previous_revision, Some(previous));
            assert!(
                damage_pixels(&frame) < 3840 * 2160 / 20,
                "a field must not repaint the output"
            );
            previous = frame.revision;
        }
        // The status row and the field's ring are credential parts too: an
        // answer repaints their bounds, not the output.
        for answer in [failed as fn(&LockPrompt) -> LockView<'_>, verifying] {
            f.draw(&theme, view(&p, 0));
            let frame = f.draw(&theme, answer(&p));
            assert_credential_damage(&frame, &theme, &f.outputs);
        }
        // Everything outside the credential parts cannot pass the byte-for-byte
        // proof and must still take the full frame.
        for changed in [
            LockView {
                clock: "10:43",
                ..view(&p, 0)
            },
            LockView {
                date: "Sunday, 25 October",
                ..view(&p, 0)
            },
            LockView {
                notification_count: 2,
                ..view(&p, 0)
            },
            LockView {
                keyboard: Some(ui::passcode_keyboard::PasscodeKeyboard::default()),
                ..view(&p, 0)
            },
            LockView {
                alert: Some("Low battery"),
                ..view(&p, 0)
            },
        ] {
            f.draw(&theme, view(&p, 0));
            assert!(f.draw(&theme, changed).damage_regions.is_none());
        }
        f.draw(&theme, view(&p, 0));
        f.osd = Some(Osd::volume(0.5, false));
        // The fixture clock was frozen before the new OSD existed. Advance
        // through its arrival so this assertion observes a visible readout.
        f.now = Instant::now()
            .checked_add(crate::shell::osd::osd_fade_in())
            .expect("bounded OSD arrival");
        assert!(f.draw(&theme, view(&p, 1)).damage_regions.is_none());
        let steady_osd = f.draw(&theme, view(&p, 2));
        assert_credential_damage(&steady_osd, &theme, &f.outputs);
        f.osd = None;
        assert!(f.draw(&theme, view(&p, 3)).damage_regions.is_none());
        drop(f);
    }
}

#[test]
fn the_status_answer_cross_fades_in_place_and_then_rests() {
    let p = LockPrompt::new();
    let mut f = Fixture::new((1600, 900));
    let theme = Theme::dark();
    let start = f.now;
    f.retained.clear();
    assert!(f.draw(&theme, view(&p, 0)).damage_regions.is_none());

    // A refusal arrives: the row and the field's availability change, and
    // both are bounded credential parts.
    f.advance(1);
    let arriving = f.draw(&theme, failed(&p));
    assert_credential_damage(&arriving, &theme, &f.outputs);
    assert!(f.retained.feedback_frame_due());
    let deadline = f
        .retained
        .next_feedback_deadline()
        .expect("the answer has an end");

    // Every frame of the run is bounded and differs from the last.
    for millis in [20, 45, 80, 130] {
        f.now = start + Duration::from_millis(millis);
        let frame = f.draw(&theme, failed(&p));
        assert_credential_damage(&frame, &theme, &f.outputs);
    }
    // The resting frame is the last one asked for; after it, the row is still
    // and the lock asks for nothing.
    f.now = deadline;
    assert_credential_damage(&f.draw(&theme, failed(&p)), &theme, &f.outputs);
    assert!(!f.retained.feedback_frame_due());
    assert_eq!(f.retained.next_feedback_deadline(), None);
    assert_settled(&f.draw(&theme, failed(&p)));

    // A retry cross-fades the refusal out; the field takes the pending mark
    // and the row says a check is running.
    f.advance(1);
    let retry = f.draw(&theme, verifying(&p));
    assert_credential_damage(&retry, &theme, &f.outputs);
    assert!(f.retained.feedback_frame_due());
    drop(f);
}

#[test]
fn a_quiet_edit_lands_at_once_and_a_fresh_lock_rests() {
    let p = LockPrompt::new();
    let mut f = Fixture::new((1280, 800));
    let theme = Theme::dark();
    f.retained.clear();
    assert!(f.draw(&theme, view(&p, 0)).damage_regions.is_none());
    f.advance(1);
    f.draw(&theme, failed(&p));
    // Typing after a refusal is an edit: the hint replaces the refusal in the
    // same frame, with nothing left running.
    f.advance(1);
    let typed = f.draw(&theme, view(&p, 1));
    assert_credential_damage(&typed, &theme, &f.outputs);
    assert!(!f.retained.feedback_frame_due());
    assert_eq!(f.retained.next_feedback_deadline(), None);
    // A lock edge forgets the session and starts at rest.
    f.advance(1);
    f.draw(&theme, failed(&p));
    assert!(f.retained.feedback_frame_due());
    f.retained.clear();
    assert!(!f.retained.feedback_frame_due());
    assert!(f.draw(&theme, view(&p, 0)).damage_regions.is_none());
    drop(f);
}

#[test]
fn relock_display_changes_and_desktop_revisions_cannot_reuse_an_old_lock() {
    let p = LockPrompt::new();
    let mut f = Fixture::new((1280, 800));
    let theme = Theme::dark();
    let desktop = f.builder.paint_with_damage();
    let first = f.draw(&theme, view(&p, 0));
    assert!(first.revision > desktop.revision);
    assert!(first.damage_regions.is_none());
    let second = f.draw(&theme, view(&p, 1));
    assert!(second.revision > first.revision);
    assert!(second.damage_regions.is_some());
    // A lock edge invalidates history even when no desktop frame intervened.
    f.retained.clear();
    assert!(f.draw(&theme, view(&p, 2)).damage_regions.is_none());
    f.outputs[0].scale = 2.0;
    assert!(f.draw(&theme, view(&p, 3)).damage_regions.is_none());
    f.outputs[0].size = (1200.0, 700.0);
    assert!(f.draw(&theme, view(&p, 4)).damage_regions.is_none());
    f.outputs.push(LockOutput {
        origin: (1200.0, 0.0),
        size: (80.0, 800.0),
        scale: 1.0,
    });
    assert!(f.draw(&theme, view(&p, 5)).damage_regions.is_none());
    assert!(
        f.draw(&Theme::light(), view(&p, 6))
            .damage_regions
            .is_none()
    );
    f.wallpaper = Some(ui::raster::rgba8(
        vello::peniko::Blob::new(std::sync::Arc::new(vec![10, 30, 70, 255])),
        1,
        1,
    ));
    assert!(f.draw(&theme, view(&p, 6)).damage_regions.is_none());
    f.wallpaper = None;
    assert!(f.draw(&theme, view(&p, 6)).damage_regions.is_none());
    f.retained.clear();
    let desktop = f.builder.paint_with_damage();
    let relock = f.draw(&theme, view(&p, 0));
    drop(f);
    assert!(relock.revision > desktop.revision);
    assert!(relock.damage_regions.is_none());
}

fn assert_pixels(retained: &mut VelloDrmBridge, dense: &mut VelloDrmBridge, frame: &FramePaint) {
    let pitch = 1280 * 4;
    let mut actual = vec![0; pitch * 800];
    let mut expected = actual.clone();
    retained
        .paint_damage_into_xrgb8888(
            &frame.scene,
            SceneDamage {
                revision: frame.revision,
                previous_revision: frame.previous_revision,
                regions: frame.damage_regions.as_deref(),
            },
            &mut actual,
            pitch,
        )
        .expect("retained lock render");
    if frame
        .damage_regions
        .as_ref()
        .is_some_and(|rects| !rects.is_empty())
    {
        let stats = retained
            .last_sparse_render_stats()
            .expect("bounded password edit must use sparse fine");
        assert!(stats.written_pixels <= damage_pixels(frame));
    }
    dense
        .paint_into_xrgb8888(&frame.scene, &mut expected, pitch)
        .expect("dense lock oracle");
    let differences: Vec<_> = actual
        .iter()
        .zip(&expected)
        .filter(|(a, b)| a != b)
        .collect();
    // The same native-pixel oracle allowance as frame_retained_tests: GPU
    // blend-edge rounding can differ by one unit in a handful of bytes.
    assert!(
        differences.len() <= 16 && differences.iter().all(|(a, b)| a.abs_diff(**b) <= 1),
        "retained lock pixels diverged: {} bytes",
        differences.len()
    );
}

#[test]
fn retained_password_pixels_match_full_renders_in_every_theme_and_after_dropped_frames() {
    let p = LockPrompt::new();
    let mut retained = VelloDrmBridge::new(1280, 800).expect("GPU required for lock pixel oracle");
    let mut dense = VelloDrmBridge::new(1280, 800).expect("GPU required for dense lock oracle");
    retained.wait_for_gpu_completion_for_test();
    dense.wait_for_gpu_completion_for_test();
    let mut f = Fixture::new((1280, 800));
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        f.retained.clear();
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 0)));
        for length in [1, 8, 2, 0, 18, 19, 0] {
            let frame = f.draw(&theme, view(&p, length));
            assert!(frame.damage_regions.is_some());
            assert_pixels(&mut retained, &mut dense, &frame);
        }
        let skipped = f.draw(&theme, view(&p, 8));
        let latest = f.draw(&theme, view(&p, 1)).superseding(&skipped);
        assert!(latest.damage_regions.is_some());
        assert_pixels(&mut retained, &mut dense, &latest);
        let changed_clock = f.draw(
            &theme,
            LockView {
                clock: "11:00",
                ..view(&p, 1)
            },
        );
        assert!(changed_clock.damage_regions.is_none());
        assert_pixels(&mut retained, &mut dense, &changed_clock);
        // Return to the old clock and clear the placeholder/dots again.
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 0)));
        f.retained.clear();
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 1)));
        // Mixed-scale outputs, including moving the credential surface to
        // the other display: no old field or placeholder may survive there.
        f.outputs = vec![
            LockOutput {
                origin: (0.0, 0.0),
                size: (640.0, 800.0),
                scale: 1.0,
            },
            LockOutput {
                origin: (640.0, 0.0),
                size: (640.0, 800.0),
                scale: 2.0,
            },
        ];
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 0)));
        let typed = f.draw(&theme, view(&p, 1));
        assert!(typed.damage_regions.is_some());
        assert_pixels(&mut retained, &mut dense, &typed);
        f.outputs.swap(0, 1);
        let moved = f.draw(&theme, view(&p, 1));
        assert!(moved.damage_regions.is_none());
        assert_pixels(&mut retained, &mut dense, &moved);
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 2)));
        f.outputs = vec![LockOutput {
            origin: (0.0, 0.0),
            size: (1280.0, 800.0),
            scale: 1.0,
        }];
        f.wallpaper = Some(ui::raster::rgba8(
            vello::peniko::Blob::new(std::sync::Arc::new(vec![40, 70, 100, 255])),
            1,
            1,
        ));
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 0)));
        let over_wallpaper = f.draw(&theme, view(&p, 1));
        assert!(over_wallpaper.damage_regions.is_some());
        assert_pixels(&mut retained, &mut dense, &over_wallpaper);
        f.wallpaper = None;
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 2)));
    }
}

#[test]
fn retained_status_answers_match_dense_pixels_at_every_phase_and_under_reduced_motion() {
    let p = LockPrompt::new();
    let mut retained = VelloDrmBridge::new(1280, 800).expect("GPU required for lock pixel oracle");
    let mut dense = VelloDrmBridge::new(1280, 800).expect("GPU required for dense lock oracle");
    retained.wait_for_gpu_completion_for_test();
    dense.wait_for_gpu_completion_for_test();
    for theme in [
        Theme::dark(),
        Theme::light(),
        Theme::high_contrast(),
        Theme::dark().with_reduce_motion(true),
    ] {
        let mut f = Fixture::new((1280, 800));
        let start = f.now;
        f.retained.clear();
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 0)));

        // The refusal's entrance: every phase must match a full render while
        // damaging only the credential card.
        f.now = start + Duration::from_millis(1);
        let refused = f.draw(&theme, failed(&p));
        assert_credential_damage(&refused, &theme, &f.outputs);
        assert_pixels(&mut retained, &mut dense, &refused);
        for millis in [20, 45, 80, 130, 400] {
            f.now = start + Duration::from_millis(millis);
            let frame = f.draw(&theme, failed(&p));
            assert!(
                frame.damage_regions.is_some(),
                "a declining phase still needs its frame"
            );
            if frame
                .damage_regions
                .as_ref()
                .is_some_and(|rects| !rects.is_empty())
            {
                assert_credential_damage(&frame, &theme, &f.outputs);
            }
            assert_pixels(&mut retained, &mut dense, &frame);
        }
        if let Some(rest) = f.retained.next_feedback_deadline() {
            f.now = f.now.max(rest);
            let resting = f.draw(&theme, failed(&p));
            assert_credential_damage(&resting, &theme, &f.outputs);
            assert_pixels(&mut retained, &mut dense, &resting);
        }
        assert!(!f.retained.feedback_frame_due());
        assert_eq!(f.retained.next_feedback_deadline(), None);

        // Retrying: the pending mark and the new row, at the same phases.
        let retry_start = f.now + Duration::from_millis(1);
        f.now = retry_start;
        let retry = f.draw(&theme, verifying(&p));
        assert_credential_damage(&retry, &theme, &f.outputs);
        assert_pixels(&mut retained, &mut dense, &retry);
        for millis in [20, 45, 80, 130, 400] {
            f.now = retry_start + Duration::from_millis(millis);
            let frame = f.draw(&theme, verifying(&p));
            assert!(frame.damage_regions.is_some());
            if frame
                .damage_regions
                .as_ref()
                .is_some_and(|rects| !rects.is_empty())
            {
                assert_credential_damage(&frame, &theme, &f.outputs);
            }
            assert_pixels(&mut retained, &mut dense, &frame);
        }
        if let Some(rest) = f.retained.next_feedback_deadline() {
            f.now = f.now.max(rest);
            let resting = f.draw(&theme, verifying(&p));
            assert_credential_damage(&resting, &theme, &f.outputs);
            assert_pixels(&mut retained, &mut dense, &resting);
        }
        assert!(!f.retained.feedback_frame_due());
        assert_eq!(f.retained.next_feedback_deadline(), None);
        f.retained.clear();
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, failed(&p)));
        assert_pixels(&mut retained, &mut dense, &f.draw(&theme, view(&p, 0)));
    }
}
