//! The overview's and a workspace switch's frames: each asks for the
//! frames it moves in, on the chord, and every frame carries the desktop
//! and repaints only the display that changed.

use super::*;

/// One frame as the compositor's loop makes it: the pump, the layout the
/// frame owes, the paint.
fn frame(fb: &mut FrameBuilder, doc: &mut AutoCommit) -> FramePaint {
    fb.pump_apps(doc, &automerge::ROOT);
    fb.relayout_if_owed();
    fb.paint_with_damage()
}

/// Bring a fresh desktop to rest as the loop does: frames until none is
/// asked for. Its hosted apps (the banners, the top bar) take what their
/// first frame handed them in the pump of the next, and the desk's team
/// glance, which comes in a moment after the desktop is first shown, has
/// come and settled, so a later frame's damage is only what the test did.
fn settle(fb: &mut FrameBuilder, doc: &mut AutoCommit) {
    let _ = fb.paint_settled();
    let deadline = tilo_test_env::hang_deadline();
    let desk_pending = |fb: &FrameBuilder| {
        let now = std::time::Instant::now();
        fb.host.desk_animating(now)
            || fb.host.desk_wake_deadline(now).is_some_and(|due| {
                due.saturating_duration_since(now) < std::time::Duration::from_secs(10)
            })
    };
    loop {
        let _ = frame(fb, doc);
        if !fb.needs_animation_frame() && !desk_pending(fb) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the desktop settles: {:?}",
            fb.animation_owners()
        );
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

fn both_directions_carry_the_desktop() {
    let mut fb = FrameBuilder::new((3200, 1080));
    let _ = fb.paint_settled();
    for step in [1, -1] {
        let mut switched = false;
        fb.with_chrome(|st| {
            switched = st.switch_workspace(step);
        });
        assert!(switched, "the demo tree switches by {step}");
        fb.host_mut()
            .start_workspace_switch(ui::tokens::MotionSource::Keyboard);
        for _ in 0..2 {
            let frame = fb.paint_with_damage();
            assert!(
                !frame.scene.encoding().is_empty(),
                "switching by {step} draws the workspace, not an empty clip"
            );
        }
    }
}

#[test]
fn overview_and_workspace_switch_request_their_frames() {
    fn overview_toggle_requests_frames_before_draw() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        fb.with_chrome(|st| {
            st.overlays.overview = true;
            st.bump();
        });
        // Latch on the host before update, as Super+Tab does.
        fb.host_mut().latch_overview_transition();
        assert!(
            fb.animation_owners()
                .iter()
                .any(|owner| owner == "root-transition"),
            "overview must request frames on the chord, not only after draw; owners {:?}",
            fb.animation_owners()
        );
        let mut doc = AutoCommit::new();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        assert!(
            fb.animation_owners()
                .iter()
                .any(|owner| owner == "root-transition"),
            "overview must request frames on the update that sees the toggle, not only after draw; owners {:?}",
            fb.animation_owners()
        );
    }

    fn workspace_switch_requests_frames_before_draw() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        let mut switched = false;
        fb.with_chrome(|st| {
            switched = st.switch_workspace(1);
        });
        assert!(switched, "the demo tree has a second workspace");
        // Latch on the host before update, as Super+PageDown does.
        fb.host_mut()
            .start_workspace_switch(ui::tokens::MotionSource::Keyboard);
        assert!(
            fb.animation_owners()
                .iter()
                .any(|owner| owner == "workspace-switch"),
            "a workspace switch must request frames on the chord, not only after draw; owners {:?}",
            fb.animation_owners()
        );
        let mut doc = AutoCommit::new();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        assert!(
            fb.animation_owners()
                .iter()
                .any(|owner| owner == "workspace-switch"),
            "a workspace switch must keep asking until its frame is painted; owners {:?}",
            fb.animation_owners()
        );
        // The switch draws in one frame: nothing of it is owed after.
        fb.relayout();
        let _ = fb.paint();
        assert!(
            !fb.animation_owners()
                .iter()
                .any(|owner| owner == "workspace-switch"),
            "a workspace switch drawn in one frame asks for no more; owners {:?}",
            fb.animation_owners()
        );
    }

    fn compositor_paint_run_is_not_host_demand() {
        let mut fb = FrameBuilder::new((1600, 900));
        settle(&mut fb, &mut AutoCommit::new());
        fb.with_chrome(|st| st.request_transition_paints(8));
        assert_eq!(fb.chrome().transition_paints_remaining(), 8);
        assert!(
            !fb.needs_animation_frame(),
            "the compositor paint run must not keep the host asking after the crossfade; owners {:?}",
            fb.animation_owners()
        );
    }

    fn latch_asks_the_compositor_for_eight_paints() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        fb.with_chrome(|st| {
            st.overlays.overview = true;
            st.bump();
        });
        fb.host_mut().latch_overview_transition();
        assert_eq!(
            fb.chrome().transition_paints_remaining(),
            8,
            "overview must keep the compositor painting until eight frames have run"
        );
    }

    // The motion clock keeps each thread's frame requests until the real
    // clock passes them and a frame at rest is drawn, past the host that
    // made them: a desk fade an earlier phase started would still ask for
    // frames in the next phase's fresh desktop, or not, by how long the
    // phases took. Each phase runs on a thread of its own.
    for phase in [
        overview_toggle_requests_frames_before_draw as fn(),
        workspace_switch_requests_frames_before_draw,
        compositor_paint_run_is_not_host_demand,
        latch_asks_the_compositor_for_eight_paints,
        both_directions_carry_the_desktop,
    ] {
        if let Err(panic) = std::thread::spawn(phase).join() {
            std::panic::resume_unwind(panic);
        }
    }
}

#[test]
fn overview_fade_damages_only_its_display_on_two_heads() {
    use crate::shell::output_scene::OutputGeometry;

    /// Every frame of a fade carries the desktop and a sparse proof, and
    /// after the chord's own frame repaints only the first display. Each
    /// frame is the compositor loop's: the pump that shows the overview's
    /// app what the chord asked, the layout it owes, the paint.
    fn fade(fb: &mut FrameBuilder, doc: &mut AutoCommit, open: bool) {
        let direction = if open { "open" } else { "close" };
        fb.with_chrome(|st| {
            st.overlays.overview = open;
            st.bump();
        });
        fb.host_mut().latch_overview_transition();
        let mut repainted = false;
        for index in 0..8 {
            // The fade samples the wall clock in whole milliseconds.
            std::thread::sleep(std::time::Duration::from_millis(20));
            let painted = frame(fb, doc);
            assert!(
                !painted.scene.encoding().is_empty(),
                "{direction} frame {index} draws the desktop, not an empty clip"
            );
            if index == 0 {
                // The chord may change chrome beyond the overview (the
                // top bar's button) once.
                continue;
            }
            let damage = painted.damage_regions.unwrap_or_else(|| {
                panic!("{direction} frame {index} is sparse, not both displays")
            });
            for rect in &damage {
                let right = rect.x.saturating_add(rect.width);
                assert!(
                    right <= 1920,
                    "{direction} frame {index} stays on the first display ({rect:?})"
                );
            }
            repainted |= !damage.is_empty();
        }
        assert!(repainted, "the {direction} fade repaints its display");
    }

    let mut fb = FrameBuilder::new((3200, 1080));
    fb.set_output_geometry(vec![
        OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0).with_id(0),
        OutputGeometry::unscaled(1920.0, 0.0, 1280.0, 800.0).with_id(1),
    ]);
    let mut doc = AutoCommit::new();
    settle(&mut fb, &mut doc);
    let _ = frame(&mut fb, &mut doc);
    fade(&mut fb, &mut doc, true);
    fade(&mut fb, &mut doc, false);
}

#[test]
fn single_head_transition_frames_carry_the_desktop() {
    let mut fb = FrameBuilder::new((1600, 900));
    let _ = fb.paint_settled();
    let _ = fb.paint_with_damage();
    fb.with_chrome(|st| {
        st.overlays.overview = true;
        st.bump();
    });
    fb.host_mut().latch_overview_transition();
    for index in 0..8 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let frame = fb.paint_with_damage();
        assert!(
            !frame.scene.encoding().is_empty(),
            "overview frame {index} on one display draws the desktop"
        );
    }
    let mut switched = false;
    fb.with_chrome(|st| {
        switched = st.switch_workspace(1);
    });
    assert!(switched, "the demo tree has a second workspace");
    fb.host_mut()
        .start_workspace_switch(ui::tokens::MotionSource::Keyboard);
    for index in 0..8 {
        let frame = fb.paint_with_damage();
        assert!(
            !frame.scene.encoding().is_empty(),
            "switch frame {index} on one display draws the desktop"
        );
    }
}

#[test]
fn workspace_switch_frames_carry_the_desktop_and_settle_sparse() {
    use crate::shell::output_scene::OutputGeometry;

    let mut fb = FrameBuilder::new((3200, 1080));
    fb.set_output_geometry(vec![
        OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0).with_id(0),
        OutputGeometry::unscaled(1920.0, 0.0, 1280.0, 800.0).with_id(1),
    ]);
    let _ = fb.paint_settled();
    let _ = fb.paint_with_damage();
    for step in [1, -1] {
        let mut switched = false;
        fb.with_chrome(|st| {
            switched = st.switch_workspace(step);
        });
        assert!(switched, "the demo tree switches by {step}");
        fb.host_mut()
            .start_workspace_switch(ui::tokens::MotionSource::Keyboard);
        for index in 0..8 {
            let frame = fb.paint_with_damage();
            assert!(
                !frame.scene.encoding().is_empty(),
                "switch {step} frame {index} draws the workspace it shows"
            );
            if index > 0 {
                assert!(
                    frame.damage_regions.is_some(),
                    "switch {step} frame {index} repaints only what changed, not both displays"
                );
            }
        }
    }
}
