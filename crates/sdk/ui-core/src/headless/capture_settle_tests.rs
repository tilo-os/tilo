//! What a capture settles: the frames a window still asks for, and none it
//! does not, and when the bound stops it.

use super::*;

struct FinalFrameWork {
    draws: std::rc::Rc<std::cell::Cell<usize>>,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    completes: bool,
    rect: Rect,
}

impl Widget for FinalFrameWork {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy
            .layout(node)
            .expect("the actual visual worker has layout");
        self.rect = Rect::from_origin_size(
            origin,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let draws = self.draws.get().saturating_add(1);
        self.draws.set(draws);
        if draws == 3 {
            self.pending
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if self.completes {
                let pending = std::sync::Arc::clone(&self.pending);
                crate::background::Background::current()
                    .spawn("capture-final-frame", move || {
                        pending.store(false, std::sync::atomic::Ordering::SeqCst);
                    })
                    .expect("the production background executor accepts the visual job");
            }
        }
    }
    fn has_pending_visual_work(&self) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// The work it starts is work it needs a frame for: a widget that owes
    /// draws asks for them, as one in a live window does.
    fn needs_animation_frame(&self) -> bool {
        self.draws.get() < 3
    }
    fn name(&self) -> &str {
        "FinalFrameWork"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[test]
fn capture_awaits_visual_work_started_by_the_final_frame() {
    let pumped = crate::background::Pumped::new();
    let _entered = pumped.background().enter();
    let draws = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
    let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut harness = FrameHarness::new(
        State::new(),
        Box::new(FinalFrameWork {
            draws: std::rc::Rc::clone(&draws),
            pending: std::sync::Arc::clone(&pending),
            completes: true,
            rect: Rect::ZERO,
        }),
        (200, 100),
    );
    draws.set(0);
    assert!(harness.settle_for_capture(tilo_test_env::hang_bound()));
    assert!(
        draws.get() > 3,
        "capture drew the completed final-frame job"
    );
    assert!(!pending.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn capture_bound_includes_the_initial_frame_pipeline() {
    let draws = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
    let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut harness = FrameHarness::new(
        State::new(),
        Box::new(FinalFrameWork {
            draws: std::rc::Rc::clone(&draws),
            pending: std::sync::Arc::clone(&pending),
            completes: false,
            rect: Rect::ZERO,
        }),
        (200, 100),
    );
    draws.set(0);
    assert!(!harness.settle_for_capture(crate::time::Duration::ZERO));
    assert_eq!(
        draws.get(),
        2,
        "an expired capture cannot flush another frame"
    );
    assert!(!pending.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn capture_final_frame_work_cannot_reset_the_original_deadline() {
    let draws = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
    let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut harness = FrameHarness::new(
        State::new(),
        Box::new(FinalFrameWork {
            draws: std::rc::Rc::clone(&draws),
            pending: std::sync::Arc::clone(&pending),
            completes: false,
            rect: Rect::ZERO,
        }),
        (200, 100),
    );
    draws.set(0);
    let bound = crate::time::Duration::from_millis(30);
    let started = crate::time::now();
    assert!(!harness.settle_for_capture(bound));
    assert!(crate::time::now().duration_since(started) >= bound);
    assert!(pending.load(std::sync::atomic::Ordering::SeqCst));
}

/// Animates for `length` from the first time it is asked whether it animates; `until` records
/// when that ends, so a slow harness start cannot end it unseen.
struct Entrance {
    length: crate::time::Duration,
    until: std::rc::Rc<std::cell::Cell<Option<crate::time::Instant>>>,
}

/// Animates until it has been drawn `frames` times, so the number of draws
/// the capture runs — not the wall clock — says whether it settled.
struct CountedFrames {
    frames: usize,
    drawn: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Widget for CountedFrames {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.drawn.set(self.drawn.get() + 1);
    }
    fn needs_animation_frame(&self) -> bool {
        self.drawn.get() < self.frames
    }
    fn name(&self) -> &str {
        "CountedFrames"
    }
}

impl Entrance {
    fn new() -> (
        Self,
        std::rc::Rc<std::cell::Cell<Option<crate::time::Instant>>>,
    ) {
        let until = std::rc::Rc::new(std::cell::Cell::new(None));
        let entrance = Self {
            length: crate::time::Duration::from_millis(150),
            until: std::rc::Rc::clone(&until),
        };
        (entrance, until)
    }
}

impl Widget for Entrance {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn needs_animation_frame(&self) -> bool {
        let now = crate::time::now();
        let until = self
            .until
            .get()
            .unwrap_or_else(|| now.checked_add(self.length).unwrap_or(now));
        self.until.set(Some(until));
        now < until
    }
    fn name(&self) -> &str {
        "Entrance"
    }
}

#[test]
fn capture_settlement_applies_the_final_push_sidebar_geometry() {
    use crate::widget::{NavItem, NavList, NavMode, NavPane, NavSection, NavShell};
    for width in [360_u32, 480_u32] {
        let shell = NavShell::new()
            .mode(NavMode::Push)
            .with_pane_state(&crate::LocalState::new(NavPane::Content))
            .sidebar(Box::new(NavList::new(vec![NavSection::untitled(
                "screens",
                vec![NavItem::new("world", "World")],
            )])))
            .content(Box::new(crate::widget::Spacer::new()));
        let mut harness = FrameHarness::for_root(State::new(), shell, (width, 820), Theme::dark());
        harness
            .perform_named("nav-shell-back", &crate::a11y::Action::Activate)
            .unwrap();
        assert!(harness.settle_for_capture(tilo_test_env::hang_bound()));
        let column = harness.root::<NavShell>().geometry().sidebar_column;
        assert_eq!(column.x0, 0.0, "the final sidebar starts at the viewport");
        assert_eq!(column.x1, f64::from(width));
        let (_, row) = harness
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("World"))
            .expect("the settled visible sidebar publishes its actual row");
        let bounds = row.bounds().unwrap();
        assert!(bounds.x0 >= 0.0 && bounds.x1 <= f64::from(width));
    }
}

/// A written capture is the window a person sees a moment after it opened, so a still-running
/// entrance is waited out; otherwise `--shots` writes a scroll thumb mid-fade or a page drawn
/// at its old width.
#[cfg(feature = "native")]
#[test]
fn writing_a_shot_runs_the_animation_out_first() {
    let drawn = std::rc::Rc::new(std::cell::Cell::new(0));
    let widget = CountedFrames {
        frames: 3,
        drawn: std::rc::Rc::clone(&drawn),
    };
    let mut harness = FrameHarness::new(State::new(), Box::new(widget), (200, 100));
    assert!(
        harness.frame_schedule().needs_animation_frame,
        "the animation is running before the capture"
    );
    let temp = tempfile::tempdir().expect("temporary output root");
    harness.write_shot(&temp.path().join("shot.png"), false);
    assert!(
        drawn.get() >= 3,
        "the capture ran the animation out ({} draw(s))",
        drawn.get()
    );
    assert!(
        !harness.frame_schedule().needs_animation_frame,
        "the written frame is a settled one"
    );
}

#[test]
fn a_capture_settles_animations_and_lands_on_the_window_ground() {
    {
        let (entrance, until) = Entrance::new();
        let mut harness = FrameHarness::new(State::new(), Box::new(entrance), (200, 100));
        assert!(harness.frame_schedule().needs_animation_frame);
        assert!(harness.settle_for_capture(tilo_test_env::hang_bound()));
        assert!(!harness.frame_schedule().needs_animation_frame);
        let until = until.get().expect("the entrance started");
        assert!(crate::time::now() >= until, "it ran the animation out");
    }

    {
        let pumped = crate::background::Pumped::new();
        let _entered = pumped.background().enter();
        let (entrance, until) = Entrance::new();
        let mut harness = FrameHarness::new(State::new(), Box::new(entrance), (200, 100));
        assert!(harness.settle_for_capture(tilo_test_env::hang_bound()));
        let until = until.get().expect("the entrance started");
        assert!(crate::time::now() >= until, "it ran the animation out");
    }

    {
        for theme in [Theme::dark(), Theme::light()] {
            let harness = FrameHarness::new_with_theme(
                State::new(),
                Box::new(crate::widget::Spacer::new()),
                (64, 48),
                theme.clone(),
            );
            let ground = theme.tokens.colors.surfaces.content.to_rgba8();
            let rgba = harness.render_rgba();
            assert_eq!(
                rgba[..4],
                [ground.r, ground.g, ground.b, 255],
                "{}: a shot lands on the window ground",
                theme.name
            );
            assert_eq!(
                harness.render_rgba_transparent()[3],
                0,
                "transparency only when asked for"
            );
        }
    }
}
