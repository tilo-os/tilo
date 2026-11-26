//! An opening window's content is an arrival ([`crate::arriving`]): a first frame from this device
//! inside its frame budget shows with no placeholder on any frame; a later one, or one from another
//! device, reserves the window's place, shows the skeleton after the grace period (for at least its
//! minimum), says where it comes from, and cross-fades in without moving anything; a failure takes
//! the same place; and a window waiting between two steps asks for no frames.
use super::*;
use crate::arriving::{GRACE, MIN_SHOWN, Source, Step, frame_budget};
use crate::widget::{AppKind, TabDescriptor, TileContent};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

struct FirstFrame {
    landed: Rc<Cell<bool>>,
    from: Source,
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
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_scene, _transform, _tr, _theme, mut env) = ctx.parts();
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

#[derive(Debug)]
struct Shown {
    painted: bool,
    step: Option<Step>,
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
            self.group.draw(&mut crate::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                crate::presence::PresenceCtx::empty().draw_env(),
            ));
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
fn a_frame_from_another_device_reserves_its_place_at_once_and_never_names_its_source() {
    let mut w = opening(Source::Remote);
    assert_eq!(w.at(0, || Widget::holds_frame_until(&w.group, w.t0)), None);
    let first = w.frame(0);
    assert!(
        first.painted,
        "another device's frame never holds this one's"
    );
    assert_eq!(first.step, Some(Step::Ground));
    let waiting = w.frame(60_000);
    assert_eq!(waiting.step, Some(Step::Skeleton));
    let said = w.at(60_000, || {
        Widget::accessibility(w.host()).and_then(|node| node.description)
    });
    assert_eq!(
        said.as_deref(),
        Some("Loading"),
        "a workspace is one place: the placeholder never says which device or host it waits on"
    );
    assert_eq!(waiting.rects, first.rects);
}

#[test]
fn a_window_that_fails_to_start_shows_its_failure_in_the_place_it_reserved() {
    let mut w = opening(Source::Local);
    let grace = u64::try_from(GRACE.as_millis()).unwrap();
    let skeleton = w.frame(grace + 100);
    assert_eq!(skeleton.step, Some(Step::Skeleton));
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
