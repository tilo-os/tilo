use super::*;

struct Probe {
    width: f32,
    rect: Rect,
    syncs: usize,
    presses: usize,
    follows: std::cell::Cell<bool>,
}

impl Probe {
    fn new(width: f32) -> Self {
        Self {
            width,
            rect: Rect::ZERO,
            follows: std::cell::Cell::new(false),
            syncs: 0,
            presses: 0,
        }
    }
}

impl Widget for Probe {
    fn sync(&mut self, _state: &crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        self.syncs = self.syncs.saturating_add(1);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(self.width, 40.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.presses = self.presses.saturating_add(1);
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "Probe"
    }

    fn composition_follows_allocation(&self) -> bool {
        self.follows.get()
    }
}

use crate::synthetic_input::mouse_press;

#[test]
fn framed_root_runs_a_frame_on_mount_and_recomposes_after_mutable_access() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    assert!(probe.syncs > 0, "mount runs the production sync pass");
    assert!((probe.rect.width() - 120.0).abs() < f64::EPSILON);

    Framed::harness_mut(&mut probe).relayout();
    assert!(
        !Framed::harness(&probe).last_relayout().rebuilt,
        "an untouched root keeps its retained tree"
    );

    probe.width = 200.0;
    Framed::harness_mut(&mut probe).relayout();
    assert!(Framed::harness(&probe).last_relayout().rebuilt);
    assert!((probe.rect.width() - 200.0).abs() < f64::EPSILON);
}

/// A resize re-solves the retained tree against the new window size and
/// composes nothing again, unless a widget composes from its allocation.
#[test]
fn a_resize_recomposes_only_for_widgets_that_compose_from_their_allocation() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    let ids = Framed::harness(&probe).layout_node_ids();
    let frames = Framed::harness(&probe).rebuilt_frames();
    Framed::harness_mut(&mut probe).resize((360, 180)).unwrap();
    let harness = Framed::harness(&probe);
    assert!(
        !harness.last_relayout().rebuilt,
        "resize recomposed the tree"
    );
    assert!(harness.last_relayout().measured, "resize re-solved sizes");
    assert_eq!(harness.layout_node_ids(), ids, "the taffy tree was kept");
    assert_eq!(harness.rebuilt_frames(), frames);

    probe.follows.set(true);
    Framed::harness_mut(&mut probe).resize((320, 160)).unwrap();
    assert!(
        Framed::harness(&probe).last_relayout().rebuilt,
        "a widget that composes from its allocation is composed again"
    );
}

#[test]
fn recompose_rebuilds_an_unchanged_tree() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    Framed::harness_mut(&mut probe).recompose().unwrap();
    assert!(Framed::harness(&probe).last_relayout().rebuilt);
    assert!((probe.rect.width() - 120.0).abs() < f64::EPSILON);
}

#[test]
fn dispatch_to_root_reaches_the_root_and_keeps_the_retained_tree() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    let outcome = Framed::harness_mut(&mut probe)
        .dispatch_to_root_outcome(&mouse_press(), &winit::event::Modifiers::default());
    assert!(outcome.handled);
    assert_eq!(outcome.focus_target, None);
    assert_eq!(probe.presses, 1);

    Framed::harness_mut(&mut probe).relayout();
    assert!(!Framed::harness(&probe).last_relayout().rebuilt);
}

#[test]
fn host_style_positions_the_root_inside_the_viewport() {
    let probe = Framed::with_options(
        State::new(),
        Probe::new(120.0),
        (400, 200),
        FrameOptions {
            host_style: Some(Style {
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    assert!((probe.rect.x0 - 140.0).abs() < f64::EPSILON);
}

/// A leaf 300 px tall that records the heights it was offered.
struct Tall {
    rect: Rect,
    offered: Vec<AvailableSpace>,
}

impl Widget for Tall {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn measure(
        &mut self,
        _known: Size<Option<f32>>,
        available: Size<AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        self.offered.push(available.height);
        Size {
            width: 100.0,
            height: 300.0,
        }
    }

    crate::rect_geometry!();

    fn name(&self) -> &'static str {
        "Tall"
    }
}

#[test]
fn content_height_lays_the_root_out_past_the_viewport() {
    let tall = || Tall {
        rect: Rect::ZERO,
        offered: Vec::new(),
    };
    let bounded = Framed::new(State::new(), tall(), (400, 200), Theme::dark());
    assert!((bounded.rect.height() - 200.0).abs() < f64::EPSILON);
    assert!(!bounded.offered.contains(&AvailableSpace::MaxContent));

    let unbounded = Framed::with_options(
        State::new(),
        tall(),
        (400, 200),
        FrameOptions {
            content_height: true,
            ..Default::default()
        },
    );
    assert!((unbounded.rect.height() - 300.0).abs() < f64::EPSILON);
    assert!(unbounded.offered.contains(&AvailableSpace::MaxContent));
}

#[test]
#[should_panic(expected = "the root was not mounted as this type")]
fn typed_root_access_rejects_another_type() {
    let harness =
        FrameHarness::for_root(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    let _ = harness.root::<crate::widget::Text>();
}

#[test]
fn viewport_truncates_and_clamps_float_sizes() {
    assert_eq!(viewport((1200.9_f32, 760.0)), (1200, 760));
    assert_eq!(viewport((-3.0_f64, f64::NAN)), (0, 0));
}
