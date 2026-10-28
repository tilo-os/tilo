//! Present mode and the presenter's tools: keys, blanking, fullscreen and
//! audience outputs, the speaker timer and idle frames. View state only:
//! nothing here edits the deck.
#![allow(
    clippy::float_cmp,
    reason = "exact geometry and values the editor computes, not measurements"
)]

use super::bench::{Bench, deck_of};
use super::*;

/// The editor over the sample deck, presenting from its first slide.
fn presenting() -> Bench {
    let mut bench = Bench::sample();
    bench.with(SlidesApp::enter_present);
    bench
}

/// Press a named key through the editor's key handler.
fn press_named(app: &mut SlidesApp, key: NamedKey) -> bool {
    let mut handled = false;
    app.on_key(&Key::Named(key), None, Modifiers::default(), &mut handled);
    handled
}

/// Press a character key through the editor's key handler.
fn press_char(app: &mut SlidesApp, ch: &str) -> bool {
    let mut handled = false;
    app.on_key(
        &Key::Character(ch.into()),
        Some(ch),
        Modifiers::default(),
        &mut handled,
    );
    handled
}

/// Centre snapping inside/outside its threshold and the presenter clock format.
#[test]
fn element_snapping_and_present_clock() {
    // Case `snaps_element_center_to_slide_center`.
    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 200.0, 100.0));
        let mut bench = Bench::one_slide(s);
        bench.with(|app| {
            let (w, h) = (200.0, 100.0);
            // Drag so the element's center-x lands 4u shy of the slide center.
            let nx = SLIDE_W / 2.0 - w / 2.0 + 4.0;
            let (sx, _sy, guides) = app.snapped_move(&id, nx, 100.0, w, h, 8.0);
            assert!(
                (sx - (SLIDE_W / 2.0 - w / 2.0)).abs() < 1e-6,
                "center should snap to slide center, got left={sx}"
            );
            assert!(guides.iter().any(|g| matches!(g, Guide::Vertical(_))));
        });
    }

    // Case `no_snap_when_outside_threshold`.
    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 50.0, 50.0));
        let mut bench = Bench::open(&deck_of(vec![s]), super::bench::WINDOW);
        bench.with(|app| {
            let (sx, sy, guides) = app.snapped_move(&id, 300.0, 300.0, 50.0, 50.0, 4.0);
            assert_eq!((sx, sy), (300.0, 300.0));
            assert!(guides.is_empty());
        });
    }

    // Case `present_clock_formats_minutes_seconds`.
    {
        // No presentation started → 0:00.
        let clock = Bench::sample().read(|app| app.presentation.clock());
        assert_eq!(clock, "0:00");
    }
}

/// Arrow, Space, Home/End and number keys move through the talk; Escape
/// leaves it; B and W blank the screen until a key resumes it.
#[test]
fn present_mode_navigation_keys_and_blanking() {
    // Case `present_advances_and_reverses_via_real_handler`.
    presenting().with(|app| {
        assert_eq!(app.mode, Mode::Present);
        assert_eq!(app.cur_slide, 0);
        assert!(press_named(app, NamedKey::ArrowRight));
        assert_eq!(app.cur_slide, 1, "ArrowRight advances one slide");
        assert!(press_named(app, NamedKey::Space));
        assert_eq!(app.cur_slide, 2, "Space advances one slide");
        assert!(press_named(app, NamedKey::ArrowLeft));
        assert_eq!(app.cur_slide, 1, "ArrowLeft goes back one slide");
    });

    // Case `present_clamps_at_both_ends`.
    presenting().with(|app| {
        press_named(app, NamedKey::ArrowLeft);
        assert_eq!(app.cur_slide, 0, "clamped at the first slide");
        for _ in 0..20 {
            press_named(app, NamedKey::ArrowRight);
        }
        assert_eq!(
            app.cur_slide,
            app.deck.slides.len() - 1,
            "clamped at the last slide"
        );
    });

    // Case `present_home_and_end_jump_to_first_and_last`.
    presenting().with(|app| {
        app.cur_slide = 2;
        assert!(press_named(app, NamedKey::End));
        assert_eq!(app.cur_slide, app.deck.slides.len() - 1, "End → last slide");
        assert!(press_named(app, NamedKey::Home));
        assert_eq!(app.cur_slide, 0, "Home → first slide");
    });

    // Case `present_number_key_jumps_to_slide`.
    presenting().with(|app| {
        assert!(press_char(app, "3"));
        assert_eq!(app.cur_slide, 2, "'3' jumps to slide index 2");
        assert!(press_char(app, "1"));
        assert_eq!(app.cur_slide, 0, "'1' jumps to slide index 0");
    });

    // Case `present_escape_exits_to_editor`.
    presenting().with(|app| {
        app.cur_slide = 2;
        assert!(press_named(app, NamedKey::Escape));
        assert_eq!(app.mode, Mode::Edit, "Esc leaves present mode");
        // The editor opens where the talk stopped.
        assert_eq!(app.cur_slide, 2);
    });

    // Case `blackout_and_whiteout_toggle_and_navigation_resumes`.
    presenting().with(|app| {
        assert!(press_char(app, "b"));
        assert_eq!(app.presentation.blank(), Blank::Black);
        assert!(press_char(app, "b"));
        assert_eq!(app.presentation.blank(), Blank::None);
        // W whites out; pressing B switches to black (mutually exclusive).
        assert!(press_char(app, "w"));
        assert_eq!(app.presentation.blank(), Blank::White);
        assert!(press_char(app, "b"));
        assert_eq!(app.presentation.blank(), Blank::Black);
        // A navigation key resumes the slide *without* advancing.
        let before = app.cur_slide;
        assert!(press_named(app, NamedKey::ArrowRight));
        assert_eq!(
            app.presentation.blank(),
            Blank::None,
            "navigation resumes a blanked screen"
        );
        assert_eq!(
            app.cur_slide, before,
            "the resuming keypress does not also advance"
        );
    });
}

#[test]
fn present_mode_outputs_fullscreen_and_audience_lifecycle() {
    // Case `fullscreen_intent_tracks_present_mode`.
    Bench::sample().with(|app| {
        assert_eq!(FullscreenProvider::fullscreen_intent(app), Some(false));
        app.enter_present();
        assert_eq!(FullscreenProvider::fullscreen_intent(app), Some(true));
        app.exit_present();
        assert_eq!(FullscreenProvider::fullscreen_intent(app), Some(false));
    });

    // Case `present_uses_the_shared_mechanism_targeting_the_largest_output`.
    presenting().with(|app| {
        use ui::fullscreen::{FullscreenTarget, FullscreenView};
        // Present routes through the one fullscreen mechanism the video player
        // also uses: a distinct presentation view on the largest monitor.
        let params = FullscreenProvider::fullscreen_params(app);
        assert_eq!(
            params.target,
            FullscreenTarget::Largest,
            "Present targets the big screen"
        );
        assert_eq!(
            params.view,
            FullscreenView::Distinct,
            "Present shows the slideshow, not the editor"
        );
        assert_eq!(
            FullscreenProvider::fullscreen_intent(app),
            Some(true),
            "and it drives the shared reconcile"
        );
    });

    // Case `audience_lifecycle_keeps_dashboard_local_and_preserves_single_output_fallback`.
    presenting().with(|app| {
        use ui::fullscreen::{AudiencePresentationRequest, FullscreenTarget};
        assert_eq!(
            AudiencePresentationProvider::audience_presentation_request(app),
            Some(AudiencePresentationRequest::new(FullscreenTarget::Largest))
        );
        AudiencePresentationProvider::on_audience_presentation_changed(app, true);
        assert!(app.presentation.has_external_audience());
        assert_eq!(
            FullscreenProvider::fullscreen_intent(app),
            Some(false),
            "the original local window must not become the audience surface"
        );
        FullscreenProvider::on_fullscreen_changed(app, false);
        assert_eq!(
            app.mode,
            Mode::Present,
            "the host's intentional presenter unfullscreen must not end the talk"
        );
        AudiencePresentationProvider::on_audience_presentation_changed(app, false);
        assert_eq!(
            FullscreenProvider::fullscreen_intent(app),
            Some(true),
            "loss of the external output returns to the established one-output slideshow"
        );
    });

    // Case `presentation_output_resource_policy_is_local_and_selects_primary_external`.
    {
        // The output policy is one of the editor's view effects.
        let mut bench = Bench::sample();
        bench
            .app
            .effect(&crate::effects::SlidesEffect::PresentPrimaryExternal)
            .expect("the presentation output effect is answered");
        bench.frame();
        bench.read(|app| {
            assert_eq!(app.mode, Mode::Present);
            assert_eq!(
                app.presentation_target,
                ui::fullscreen::FullscreenTarget::PrimaryExternal,
                "the external-display policy must not collapse to the first output, often the laptop"
            );
        });
    }

    // Case `compositor_exit_of_fullscreen_leaves_present_mode`.
    presenting().with(|app| {
        assert_eq!(app.mode, Mode::Present);
        // A compositor-driven fullscreen exit drops out of present mode so the
        // editor chrome returns rather than a windowed borderless slideshow.
        FullscreenProvider::on_fullscreen_changed(app, false);
        assert_eq!(
            app.mode,
            Mode::Edit,
            "losing fullscreen leaves present mode"
        );
        // A redundant notification is a no-op, and gaining fullscreen never
        // forces present mode on.
        FullscreenProvider::on_fullscreen_changed(app, false);
        assert_eq!(app.mode, Mode::Edit);
        FullscreenProvider::on_fullscreen_changed(app, true);
        assert_eq!(
            app.mode,
            Mode::Edit,
            "a fullscreen grant alone doesn't start a talk"
        );
    });

    // Case `presenting_wants_fullscreen`.
    presenting().with(|app| assert!(app.wants_fullscreen()));

    // Case `entering_present_does_not_request_fullscreen_until_presenting`.
    let editing = Bench::sample().read(SlidesApp::wants_fullscreen);
    assert!(!editing, "editor mode is not fullscreen");
}

#[test]
fn presenter_dashboard_timer_and_idle_frames() {
    // Case `presenter_dashboard_toggles_with_p`.
    presenting().with(|app| {
        assert!(!app.presentation.is_presenter_view());
        assert!(press_char(app, "p"));
        assert!(
            app.presentation.is_presenter_view(),
            "P shows the presenter dashboard"
        );
        assert!(press_char(app, "p"));
        assert!(!app.presentation.is_presenter_view(), "P hides it again");
        assert_eq!(app.mode, Mode::Present);
    });

    // Case `speaker_timer_pause_and_reset`.
    presenting().with(|app| {
        use ui::time::testing::with_monotonic_time;
        let paused = ui::time::now();
        assert!(!app.presentation.is_paused());
        assert!(with_monotonic_time(paused, || press_char(app, "k")));
        assert!(app.presentation.is_paused(), "K pauses the timer");
        let frozen = with_monotonic_time(paused, || app.presentation.elapsed());
        let later = paused + std::time::Duration::from_secs(60);
        assert_eq!(
            with_monotonic_time(later, || app.presentation.elapsed()),
            frozen,
            "elapsed is frozen while paused"
        );
        assert!(with_monotonic_time(later, || press_char(app, "k")));
        assert!(!app.presentation.is_paused(), "K resumes the timer");
        assert_eq!(
            with_monotonic_time(later + std::time::Duration::from_secs(1), || {
                app.presentation.elapsed()
            }),
            frozen + std::time::Duration::from_secs(1),
            "the paused minute is not counted"
        );
        assert!(with_monotonic_time(later, || press_char(app, "0")));
        assert_eq!(
            with_monotonic_time(later, || app.presentation.elapsed()),
            std::time::Duration::ZERO,
            "0 resets the clock"
        );
    });

    // Case `present_mode_rests_when_idle_and_wakes_for_a_transition`.
    presenting().with(|app| {
        for _ in 0..3 {
            app.sync_flaps(Some(&Theme::dark()));
        }
        app.presentation.expire_transition(true);
        assert!(
            !app.needs_animation_frame(),
            "a settled present mode does not request frames at vsync"
        );
        app.next_slide(1);
        assert!(
            app.needs_animation_frame(),
            "a live cross-fade pumps frames"
        );
    });

    // Case `reduced_motion_settles_presentation_navigation_in_one_pass`.
    presenting().with(|app| {
        app.next_slide(1);
        assert!(app.presentation.needs_animation_frame());
        Widget::set_motion_reduced(app, true);
        assert!(!app.presentation.needs_animation_frame());
        assert_eq!(app.presentation.transition_alpha(true), 1.0);
        assert!(!app.needs_animation_frame());
    });
}
