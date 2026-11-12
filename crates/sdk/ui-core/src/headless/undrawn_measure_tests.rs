//! The layout pass answers a widget's measure, drawn or not: a part a container lays out and
//! does not paint asks once for the change it measures, not on every frame after.
use super::*;
use crate::widget::Text;

/// A container that lays out both its parts and paints, and rests, only the first: a shut
/// door's contents, a collapsed band's body.
struct HiddenPart {
    shown: Text,
    hidden: Text,
    rect: Rect,
}

impl Widget for HiddenPart {
    fn name(&self) -> &str {
        "HiddenPart"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let shown = self.shown.build_layout(taffy, theme, nodes);
        let hidden = self.hidden.build_layout(taffy, theme, nodes);
        let style = Style {
            flex_direction: FlexDirection::Column,
            ..Style::default()
        };
        let node = taffy.parent_node(style, &[shown, hidden]);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let children = taffy.children(node).unwrap_or_default();
        let origin = self.rect.origin();
        if let [shown, hidden] = children.as_slice() {
            self.shown.apply_layout(taffy, *shown, origin);
            self.hidden.apply_layout(taffy, *hidden, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shown.draw(scene, transform, text, theme, presence);
    }

    fn mark_drawn(&mut self) {
        self.shown.mark_drawn();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.shown, &self.hidden]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.shown, &mut self.hidden]
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[test]
fn a_part_laid_out_and_never_drawn_asks_once_for_its_measure() {
    let label = crate::LocalState::new(String::from("Draft"));
    let mut harness = FrameHarness::new(
        State::new(),
        Box::new(HiddenPart {
            shown: Text::new("Title"),
            hidden: Text::from_state(&label, Clone::clone),
            rect: Rect::ZERO,
        }),
        (400, 200),
    );
    harness.relayout();
    harness.relayout();
    label.set(String::from("A draft that says a good deal more"));
    harness.relayout();
    assert!(
        harness.last_relayout().marked_leaves >= 1,
        "the change is measured where it is laid out: {:?}",
        harness.last_relayout()
    );
    harness.relayout();
    assert_eq!(
        harness.last_relayout().marked_leaves,
        0,
        "the layout pass answered it; no draw has to: {:?}",
        harness.last_relayout()
    );
}
