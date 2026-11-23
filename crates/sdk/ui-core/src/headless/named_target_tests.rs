//! What a widget paints into its own rect is a target a tool can find and press.

use super::*;

/// A widget that paints one named affordance inside its own rect instead of composing a child
/// for it, as a strip's labels and a window's tabs do.
struct Painted {
    rect: Rect,
    presses: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Widget for Painted {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(200.0, 100.0),
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
            self.presses.set(self.presses.get().saturating_add(1));
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "Painted"
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        vec![(
            "painted_part".to_owned(),
            Rect::new(20.0, 20.0, 100.0, 80.0),
        )]
    }
}

#[test]
fn a_painted_affordance_is_a_target_and_a_press_on_it_reaches_the_widget() {
    let presses = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut harness = FrameHarness::for_root(
        State::new(),
        Painted {
            rect: Rect::ZERO,
            presses: std::rc::Rc::clone(&presses),
        },
        (320, 200),
        Theme::dark(),
    );
    harness.relayout();
    let targets = harness.targets();
    assert_eq!(
        targets.get("painted_part").copied(),
        Some(Rect::new(20.0, 20.0, 100.0, 80.0)),
        "the part the widget paints is named where it stands: {:?}",
        targets.keys().collect::<Vec<_>>()
    );
    harness
        .perform_named("painted_part", &crate::a11y::Action::Activate)
        .expect("a press on the named part");
    assert_eq!(
        presses.get(),
        1,
        "the press at the part's center reached the widget that paints it"
    );
}
