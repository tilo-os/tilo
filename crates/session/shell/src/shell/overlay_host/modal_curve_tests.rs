//! The command palette appears and leaves on the standard motion
//! (`ui::motion::Appear`), and pumps frames only while it moves.

use super::*;

fn pointer_motion() -> ui::tokens::EffectiveMotion {
    ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false)
}

#[test]
fn the_palette_appears_on_the_standard_motion_and_settles() {
    let start = ui::time::now();
    let mut t = ui::motion::Presence::closed();
    t.show(
        true,
        ui::motion::Appear::SPEC,
        pointer_motion(),
        ui::tokens::MotionSource::Pointer,
        start,
    );
    let card = Rect::new(400.0, 96.0, 1000.0, 500.0);
    let first = ui::motion::Appear::pose(t.progress(start), card.center(), pointer_motion());
    assert!(!first.is_visible(), "it starts from nothing, not a pop");
    let settled = t.settles_at().expect("appearing");
    let rest = ui::motion::Appear::pose(t.progress(settled), card.center(), pointer_motion());
    assert!(rest.is_rest(), "and rests exactly where it is laid out");
    // Summoned by a key it is there on the next frame.
    let mut keyed = ui::motion::Presence::closed();
    keyed.show(
        true,
        ui::motion::Appear::SPEC,
        pointer_motion(),
        ui::tokens::MotionSource::Keyboard,
        start,
    );
    assert!(!keyed.is_moving(start));
    assert_eq!(keyed.progress(start), 1.0);
}

#[test]
fn modal_transitions_pump_only_the_frames_they_need() {
    fn palette_exit_interpolates_not_pops() {
        // Deterministic proof (no wall-clock sleep) that the palette fades
        // out rather than popping, by sampling openness at explicit instants.
        let mut t = ui::motion::Presence::opened();
        let start = ui::time::now();
        t.show(
            false,
            ui::motion::Appear::SPEC,
            pointer_motion(),
            ui::tokens::MotionSource::Pointer,
            start,
        );
        let end = t.settles_at().expect("closing");
        let at_start = t.progress(start);
        let at_mid = t.progress(start + (end - start) / 3);
        let at_end = t.progress(end);
        assert!(at_start > 0.95, "close begins near fully-open: {at_start}");
        assert!(
            at_mid > 0.01 && at_mid < 0.99,
            "mid-close openness must be a partial fade, not a pop: {at_mid}"
        );
        assert!(at_end < 0.01, "close settles fully closed: {at_end}");
        assert!(at_mid < at_start, "openness decreases while closing");
    }

    fn palette_open_transition_pumps_frames_and_is_visible() {
        use ui::widget::Container;
        let mut h = ShellOverlayHost::new(Box::new(Container::new()));
        // One instant for the whole test, so it is a statement about the
        // motion rather than about the machine's load.
        let now = ui::time::now();
        assert!(!h.palette_transition.is_visible(now));
        h.palette_transition.show(
            true,
            ui::motion::Appear::SPEC,
            pointer_motion(),
            ui::tokens::MotionSource::Pointer,
            now,
        );
        assert!(h.palette_transition.is_visible(now));
        assert!(
            h.needs_animation_frame_at(now),
            "an in-flight palette open must pump redraws"
        );
        let settled = h.palette_transition.settles_at().expect("opening");
        assert!(
            !h.needs_animation_frame_at(settled),
            "a settled palette must let the event loop go idle"
        );
    }

    fn osd_sleeps_while_opaque_and_pumps_only_its_fade_tail() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::osd::{OSD_HOLD, Osd, osd_fade, osd_fade_in};
        use crate::shell::state::WorkspaceTree;
        use ui::widget::Container;

        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let host = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        let osd = Osd::brightness(0.4);
        let raised_at = osd.raised_at;
        chrome.borrow_mut().show_osd(osd);

        assert!(
            host.needs_animation_frame_at(raised_at),
            "the fade-in pumps its frames"
        );
        assert!(
            !host.needs_animation_frame_at(raised_at + osd_fade_in()),
            "the static hold must not repaint continuously"
        );
        let expiry = raised_at.checked_add(OSD_HOLD).unwrap();
        assert!(host.needs_animation_frame_at(expiry.checked_sub(osd_fade()).unwrap()));
        assert!(
            host.needs_animation_frame_at(expiry.checked_sub(Duration::from_millis(1)).unwrap())
        );
        assert!(
            !host.needs_animation_frame_at(raised_at + OSD_HOLD),
            "the expired readout must let the compositor idle"
        );
    }

    /// The draw path consults `self.last_cursor` against each row rect, so
    /// the hit-test must stay honest with that geometry; `hit_test_overview`
    /// and `overview_row_rect` are the shared oracle.
    fn overview_hit_test_returns_card_index_when_cursor_is_over_card() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;
        use ui::widget::Container;

        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.append_empty_workspace("Second");
        tree.append_empty_workspace("Third");
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome);
        h.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

        // Aim at the first row's centre via the same geometry helper the
        // draw path uses, so this test doesn't re-derive row math.
        let first = h.overview_row_rect(0);
        assert_eq!(h.hit_test_overview(first.center()), Some(0));

        // Second row should hit with index 1.
        let second = h.overview_row_rect(1);
        assert_eq!(h.hit_test_overview(second.center()), Some(1));

        // A point far outside the column (top-left corner) must return None.
        assert_eq!(h.hit_test_overview(Point::new(5.0, 5.0)), None);
    }

    palette_exit_interpolates_not_pops();
    palette_open_transition_pumps_frames_and_is_visible();
    osd_sleeps_while_opaque_and_pumps_only_its_fade_tail();
    overview_hit_test_returns_card_index_when_cursor_is_over_card();
}
