//! An opening window's content is an arrival ([`crate::arriving`]): a first
//! frame from this device inside its frame budget shows with no placeholder
//! on any frame; a later one, or one from another device, reserves the
//! window's place, shows the skeleton after the grace period (for at least
//! its minimum), says where it comes from, and cross-fades in without
//! moving anything; a failure takes the same place; and a window waiting
//! between two steps asks for no frames.

use super::*;
use crate::arriving::{GRACE, MIN_SHOWN, NAME_SOURCE_AFTER, Source, Step, frame_budget};
use crate::widget::{AppKind, TabDescriptor, TileContent};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

/// A window's content whose first frame lands when the test says so.
struct FirstFrame {
    landed: Rc<Cell<bool>>,
    from: Source,
    /// How many frames drew it.
    drawn: Rc<Cell<u32>>,
}

impl Widget for FirstFrame {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &Theme,
        _m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(taffy::Style::default())
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.drawn.set(self.drawn.get() + 1);
    }
    fn name(&self) -> &'static str {
        "FirstFrame"
    }
    fn presented(&self) -> bool {
        self.landed.get()
    }
    fn arrives_from(&self) -> Source {
        self.from.clone()
    }
}

impl crate::widget::app_widget::AppWidget for FirstFrame {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "FirstFrame"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("F")
    }
}

struct Opening {
    group: WindowGroup,
    landed: Rc<Cell<bool>>,
    drawn: Rc<Cell<u32>>,
    t0: Instant,
}

/// A window asked for at `t0`, its first frame coming from `from`.
fn opening(from: Source) -> Opening {
    let t0 = crate::time::now();
    let landed = Rc::new(Cell::new(false));
    let drawn = Rc::new(Cell::new(0));
    let group = crate::time::testing::with_monotonic_time(t0, || {
        let tabs = vec![(
            TabDescriptor::new(1, "Files"),
            TileContent::App(Box::new(FirstFrame {
                landed: Rc::clone(&landed),
                from,
                drawn: Rc::clone(&drawn),
            })),
        )];
        WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
    });
    Opening {
        group,
        landed,
        drawn,
        t0,
    }
}

/// What one frame showed of the window.
#[derive(Debug)]
struct Shown {
    /// Whether the window drew anything at all.
    painted: bool,
    /// The placeholder step standing in it after the frame, if any.
    step: Option<Step>,
    /// The window's and its tile's rects.
    rects: (Rect, Rect),
}

impl Opening {
    fn at<T>(&self, ms: u64, f: impl FnOnce() -> T) -> T {
        crate::time::testing::with_monotonic_time(self.t0 + Duration::from_millis(ms), f)
    }

    fn host(&self) -> &TileHost {
        self.group.active_host().expect("the window's one tab")
    }

    /// Lay the window out and draw a frame `ms` after it was asked for.
    fn frame(&mut self, ms: u64) -> Shown {
        let t = self.t0 + Duration::from_millis(ms);
        crate::time::testing::with_monotonic_time(t, || {
            use taffy::prelude::TaffyMaxContent;
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = self.group.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(
                taffy::Style {
                    size: Size {
                        width: Dimension::length(900.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            self.group.apply_layout(&taffy, node, Point::ZERO);
            let mut scene = Scene::new();
            self.group.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
            self.group.mark_drawn();
            Shown {
                painted: !scene.encoding().path_data.is_empty(),
                step: self.host().arrival_step(t),
                rects: (self.group.get_rect(), self.host().get_rect()),
            }
        })
    }

    /// Whether the window asks for a frame `ms` after it was asked for,
    /// with nothing else happening.
    fn wants_frame(&self, ms: u64) -> bool {
        self.at(ms, || {
            self.group.needs_animation_frame()
                || self.host().needs_animation_frame()
                || self.host().needs_redraw()
        })
    }

    /// When the window's content next wakes the host by itself.
    fn next_wake(&self, ms: u64) -> Option<Instant> {
        self.at(ms, || self.host().next_wake_deadline())
    }

    fn ms(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.t0).as_millis()).unwrap_or(u64::MAX)
    }
}

#[test]
fn a_local_first_frame_inside_its_budget_shows_with_no_placeholder_on_any_frame() {
    let mut w = opening(Source::Local);
    let budget = frame_budget();
    assert_eq!(
        w.at(0, || Widget::holds_frame_until(&w.group, w.t0)),
        Some(w.t0 + budget),
        "the window's first frame waits for its app's"
    );
    let held = w.frame(0);
    assert!(!held.painted, "a held frame shows nothing of the window");
    assert_eq!(w.drawn.get(), 0);

    // The app's first frame lands 8 ms later, inside the budget.
    w.landed.set(true);
    assert_eq!(w.at(8, || Widget::holds_frame_until(&w.group, w.t0)), None);
    let first = w.frame(8);
    assert!(first.painted);
    assert_eq!(first.step, None, "no placeholder stood on the first frame");
    assert_eq!(w.drawn.get(), 1, "the app drew on the window's first frame");
    assert!(
        !w.at(8, || w.host().needs_animation_frame()),
        "and nothing moves in: the window appears complete"
    );
    let next = w.frame(24);
    assert_eq!(next.step, None);
    assert_eq!(next.rects, first.rects);
}

#[test]
fn a_late_first_frame_reserves_the_ground_then_a_skeleton_then_cross_fades_in_place() {
    let mut w = opening(Source::Local);
    let after_budget = w.ms(w.t0 + frame_budget()) + 1;
    let reserved = w.frame(after_budget);
    assert!(reserved.painted, "past the budget the window stands");
    assert_eq!(reserved.step, Some(Step::Ground), "its ground alone, first");
    assert_eq!(w.drawn.get(), 0, "and not the app, which has drawn nothing");

    let grace = u64::try_from(GRACE.as_millis()).unwrap();
    let skeleton = w.frame(grace);
    assert_eq!(skeleton.step, Some(Step::Skeleton));
    assert_eq!(skeleton.rects, reserved.rects, "the skeleton moves nothing");

    // Local data is never named: still the skeleton long after.
    let long = w.frame(3_000);
    assert_eq!(long.step, Some(Step::Skeleton));

    w.landed.set(true);
    assert!(
        w.wants_frame(3_000),
        "the first frame landing asks to be shown"
    );
    let arrived = w.frame(3_000);
    assert_eq!(arrived.step, None, "the app shows");
    assert_eq!(w.drawn.get(), 1);
    assert!(
        w.at(3_000, || w.host().needs_animation_frame()),
        "and cross-fades in over the skeleton"
    );
    assert_eq!(
        arrived.rects, reserved.rects,
        "the window's geometry never changed for its content's arrival"
    );
    let settled = w.frame(3_000 + 600);
    assert!(!w.at(3_600, || w.host().needs_animation_frame()));
    assert_eq!(settled.rects, reserved.rects);
}

#[test]
fn a_frame_from_another_device_reserves_its_place_at_once_and_names_its_source() {
    let mut w = opening(Source::peer("Laptop"));
    assert_eq!(w.at(0, || Widget::holds_frame_until(&w.group, w.t0)), None);
    let first = w.frame(0);
    assert!(
        first.painted,
        "another device's frame never holds this one's"
    );
    assert_eq!(first.step, Some(Step::Ground));
    let named_at = u64::try_from(NAME_SOURCE_AFTER.as_millis()).unwrap();
    let named = w.frame(named_at);
    assert_eq!(named.step, Some(Step::Named));
    let said = w.at(named_at, || {
        Widget::accessibility(w.host()).and_then(|node| node.description)
    });
    assert!(
        said.as_deref().is_some_and(|said| said.contains("Laptop")),
        "the placeholder says where the window comes from: {said:?}"
    );
    assert_eq!(named.rects, first.rects);
}

#[test]
fn a_skeleton_stays_its_minimum_before_the_first_frame_replaces_it() {
    let mut w = opening(Source::peer("Laptop"));
    let grace = u64::try_from(GRACE.as_millis()).unwrap();
    let min = u64::try_from(MIN_SHOWN.as_millis()).unwrap();
    assert_eq!(w.frame(grace).step, Some(Step::Skeleton));
    // The first frame lands 50 ms after the skeleton showed.
    w.landed.set(true);
    let early = w.frame(grace + 50);
    assert_eq!(
        early.step,
        Some(Step::Skeleton),
        "a skeleton that showed never flashes away"
    );
    assert_eq!(w.drawn.get(), 0, "the app waits behind it");
    assert_eq!(
        w.next_wake(grace + 50),
        Some(w.t0 + GRACE + MIN_SHOWN),
        "its reveal is a deadline"
    );
    let shown = w.frame(grace + min);
    assert_eq!(shown.step, None);
    assert_eq!(w.drawn.get(), 1);
}

#[test]
fn a_window_that_fails_to_start_shows_its_failure_in_the_place_it_reserved() {
    let mut w = opening(Source::Local);
    let grace = u64::try_from(GRACE.as_millis()).unwrap();
    let skeleton = w.frame(grace + 100);
    assert_eq!(skeleton.step, Some(Step::Skeleton));
    // The shell answers a failed start by putting the failure, with its
    // retry, in the tab's place.
    let t0 = w.t0;
    crate::time::testing::with_monotonic_time(t0 + Duration::from_millis(grace + 100), || {
        w.group
            .host_for_tab_mut(1)
            .expect("the tab")
            .replace_content(TileContent::Notice {
                title: "Files".into(),
                detail: "Files could not start. Try again".into(),
            });
    });
    let min = u64::try_from(MIN_SHOWN.as_millis()).unwrap();
    let failure = w.frame(grace + min);
    assert_eq!(
        failure.step, None,
        "the failure takes the placeholder's place"
    );
    assert!(
        w.at(grace + min, || w.host().needs_animation_frame()),
        "cross-fading in over the skeleton"
    );
    assert_eq!(failure.rects, skeleton.rects, "in place: nothing moves");
    let said = w.at(grace + min, || {
        Widget::accessibility(w.host()).and_then(|node| node.description)
    });
    assert_eq!(said.as_deref(), Some("Files could not start. Try again"));
}

#[test]
fn a_window_waiting_between_two_steps_asks_for_no_frames() {
    let mut w = opening(Source::host("eu1.tilo.coffee"));
    let _ = w.frame(0);
    assert!(!w.wants_frame(1), "the reserved ground draws nothing more");
    assert_eq!(
        w.next_wake(1),
        Some(w.t0 + GRACE),
        "until the skeleton is due"
    );
    let grace = u64::try_from(GRACE.as_millis()).unwrap();
    assert!(w.wants_frame(grace), "the skeleton's step draws one frame");
    let _ = w.frame(grace);
    assert!(!w.wants_frame(grace + 1));
    assert_eq!(w.next_wake(grace + 1), Some(w.t0 + NAME_SOURCE_AFTER));
    let named = u64::try_from(NAME_SOURCE_AFTER.as_millis()).unwrap();
    let _ = w.frame(named);
    assert!(!w.wants_frame(named + 1));
    assert!(
        !w.wants_frame(60_000),
        "a window waiting a minute draws nothing"
    );
    assert_eq!(
        w.next_wake(named + 1),
        None,
        "nothing changes until its first frame lands"
    );
}
