//! A window arriving, maximizing and leaving: its place opens at once,
//! content drawn inside the grace appears whole, later content cross-fades
//! in, a new tab keeps the tab before it until it has drawn, a springing
//! window is moved and clipped but never stretched, and a key closes at once.

use super::tests::make_group;
use super::*;
use crate::widget::{AppKind, TabDescriptor, TileContent};
use std::collections::HashMap;

#[test]
fn a_window_drawn_inside_the_grace_appears_whole_and_a_late_one_cross_fades_in() {
    // One instant for the whole test: each case backdates its own
    // timestamps, so a descheduled runner cannot move them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let now = crate::time::now();
        let settle = crate::motion::CROSS_FADE.settle_time(
            crate::motion::Direction::Enter,
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
        );
        {
            // Asked for: the slot opens from nothing; the content is
            // never faded or scaled.
            let mut g = make_group();
            g.start_spawn_animation();
            assert!(g.layout_width_px() < 1.0, "the slot opens from nothing");
            assert!(g.dest_width_px() > 100.0, "towards the window's width");
            assert_eq!(g.fade_transform_params(), (1.0, 0.0, 0.0, 1.0));
            assert!(
                g.needs_animation_frame(),
                "the opening slot asks for frames"
            );
        }
        {
            // Its first frame 50 ms after it was asked for: whole.
            let mut g = make_group();
            g.start_spawn_animation();
            g.spawned_at = now.checked_sub(Duration::from_millis(50));
            g.held_back = true;
            g.take_first_paint();
            assert_eq!(g.fade_transform_params(), (1.0, 0.0, 0.0, 1.0));
            assert!(g.arrival_fade_at.is_none());
        }
        {
            // Its first frame 400 ms after: it cross-fades in over the
            // ground its place held, without moving or scaling.
            let mut g = make_group();
            g.start_spawn_animation();
            g.spawned_at = now.checked_sub(Duration::from_millis(400));
            g.held_back = true;
            g.take_first_paint();
            let (alpha, x, y, scale) = g.fade_transform_params();
            assert!(alpha < 0.05, "the late content starts clear, got {alpha}");
            assert_eq!((x, y, scale), (0.0, 0.0, 1.0));
            assert!(g.needs_animation_frame(), "the cross-fade asks for frames");
            g.arrival_fade_at = now.checked_sub(settle / 4);
            let mid = g.fade_transform_params().0;
            assert!(mid > 0.0 && mid < 1.0, "mid cross-fade, got {mid}");
            g.arrival_fade_at = now.checked_sub(settle + Duration::from_millis(16));
            g.take_first_paint();
            assert_eq!(g.fade_transform_params().0, 1.0);
            assert!(
                g.arrival_fade_at.is_none(),
                "a finished cross-fade is dropped"
            );
        }
        {
            // A shortcut lands: no slot opening, nothing to fade.
            let mut g = make_group();
            g.set_motion_source(crate::tokens::MotionSource::Keyboard);
            g.start_spawn_animation();
            assert!(
                (g.layout_width_px() - g.dest_width_px()).abs() < 0.5,
                "the slot is there at once"
            );
        }
    });
}

/// An app whose first frame arrives when the test says so.
struct Arrives {
    drawn: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Widget for Arrives {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &crate::theme::Theme,
        _m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(taffy::Style::default())
    }
    fn name(&self) -> &'static str {
        "Arrives"
    }
    fn presented(&self) -> bool {
        self.drawn.get()
    }
}

impl crate::widget::app_widget::AppWidget for Arrives {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Arrives"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("A")
    }
}

/// A new tab (Super+T) never blanks its window: the tab shown before
/// stays until the new one has drawn, then the two cross-fade.
#[test]
fn a_new_tab_shows_the_tab_before_it_until_it_draws_then_cross_fades() {
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let theme = Theme::dark();
        let mut g = make_group();
        assert_eq!(g.active_tab_id(), 1);
        let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
        g.insert_detached_tab(
            2,
            TabDescriptor::new(42, "new"),
            TileHost::new(TileContent::App(Box::new(Arrives {
                drawn: std::rc::Rc::clone(&drawn),
            }))),
        );
        assert_eq!(g.active_tab_id(), 42, "the new tab is shown at once");
        g.advance_tab_handover(&theme);
        assert_eq!(
            g.tab_standing_in(),
            Some(1),
            "until it draws, the tab shown before stands in"
        );
        drawn.set(true);
        g.advance_tab_handover(&theme);
        assert_eq!(g.tab_standing_in(), None, "it has drawn");
        let now = crate::motion::frame_now();
        let fade = g.tab_fade.as_ref().expect("the two cross-fade");
        assert_eq!(
            fade.outgoing(now).map(|(id, alpha)| (*id, alpha)),
            Some((1, 1.0)),
            "the cross-fade starts from the old content, whole"
        );
        assert!(fade.incoming(now) < 0.05, "and the new one clear");
        assert!(g.needs_animation_frame(), "the cross-fade asks for frames");
    });
}

fn laid_out(g: &mut WindowGroup, theme: &Theme, w: f32, h: f32) -> Rect {
    use taffy::prelude::TaffyMaxContent;
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = g.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy.parent_node(
        taffy::Style {
            size: Size {
                width: Dimension::length(w),
                height: Dimension::length(h),
            },
            ..Default::default()
        },
        &[node],
    );
    taffy
        .compute_layout(root, taffy::Size::MAX_CONTENT)
        .unwrap();
    g.apply_layout(&taffy, node, Point::ZERO);
    g.get_rect()
}

/// Maximizing by pointer springs the window's frame; what it holds (the
/// bar and the app's buffer) is drawn at the size it was laid out at,
/// moved with the frame and clipped to it, never scaled.
#[test]
fn a_springing_window_is_moved_and_clipped_and_never_stretched() {
    let theme = Theme::dark();
    let t0 = crate::time::now();
    let mut g = make_group();
    g.set_strip_usable_px(1200.0);
    let tiled =
        crate::time::testing::with_monotonic_time(t0, || laid_out(&mut g, &theme, 1600.0, 900.0));
    crate::time::testing::with_monotonic_time(t0, || g.toggle_maximized());
    let laid = crate::time::testing::with_monotonic_time(t0, || {
        let laid = laid_out(&mut g, &theme, 1600.0, 900.0);
        g.step_rect_spring_for_tests();
        laid
    });
    crate::time::testing::with_monotonic_time(t0 + Duration::from_millis(48), || {
        g.step_rect_spring_for_tests();
        let drawn = g.drawn_rect();
        assert!(
            drawn.width() > tiled.width() + 1.0 && drawn.width() < laid.width() - 1.0,
            "mid-spring: {drawn:?} between {tiled:?} and {laid:?}"
        );
        let [a, b, c, d, ..] = g.rect_draw_affine(Affine::IDENTITY).as_coeffs();
        assert_eq!([a, b, c, d], [1.0, 0.0, 0.0, 1.0], "moved, never scaled");
        assert_eq!(
            g.content_width_in_flight(),
            None,
            "widening, the app is given its final size as the motion starts"
        );
        let clip = g.frame_clip().expect("the frame clips what it holds");
        assert!((clip.width() - drawn.width()).abs() < 1e-6);
        assert!((clip.height() - drawn.height()).abs() < 1e-6);
    });
    // Restoring: it keeps the wide layout, clipped by the shrinking frame,
    // and is given its tiled size once the frame rests.
    let back = t0 + Duration::from_secs(2);
    crate::time::testing::with_monotonic_time(back, || {
        g.step_rect_spring_for_tests();
        g.toggle_maximized();
        laid_out(&mut g, &theme, 1600.0, 900.0);
        g.step_rect_spring_for_tests();
        assert_eq!(g.content_width_in_flight(), Some(laid.width()));
    });
    crate::time::testing::with_monotonic_time(back + Duration::from_secs(2), || {
        g.step_rect_spring_for_tests();
        assert!(
            g.needs_animation_frame(),
            "the layout at rest is owed a frame"
        );
        laid_out(&mut g, &theme, 1600.0, 900.0);
        assert_eq!(g.content_width_in_flight(), None);
        assert!((g.get_rect().width() - tiled.width()).abs() < 1.0);
    });
}

/// Super+W closes a window in the frame it is pressed: nothing of it
/// lingers. A click on its close button lets it go once its fade is over.
#[test]
fn a_key_closes_a_window_at_once_and_a_click_after_its_fade() {
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let mut g = make_group();
        g.set_motion_source(crate::tokens::MotionSource::Keyboard);
        g.start_closing();
        assert!(g.close_complete(), "a key's close is complete at once");

        let mut g = make_group();
        g.set_motion_source(crate::tokens::MotionSource::Pointer);
        g.start_closing();
        assert!(!g.close_complete(), "a click's close fades first");
        assert!(
            g.needs_animation_frame(),
            "and asks for the frames of its fade"
        );
    });
}

/// A spring let go mid-way (a shortcut landing a maximize or a resize it
/// started, a close completing) is drawn no more, so it asks the motion
/// clock for no more frames; a spring still in flight keeps asking.
#[test]
fn a_spring_let_go_mid_way_asks_the_clock_for_no_frames() {
    let now = crate::motion::frame_now();
    let mut spring = super::FrameSpring::new(0.0).with_tolerance_px();
    spring.set_target(400.0);
    assert!(
        crate::motion::frames_wanted(now),
        "a springing slot asks for its frames"
    );
    drop(spring);
    assert!(
        !crate::motion::frames_wanted(now),
        "let go, it is drawn no more and asks for nothing"
    );
}
