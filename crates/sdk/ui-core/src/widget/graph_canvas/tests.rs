use super::*;
use crate::headless::{FrameHarness, InspectionNode};
use crate::widget::Button;

fn target<'a>(node: &'a InspectionNode, id: &str) -> Option<&'a InspectionNode> {
    if node.test_id.as_deref() == Some(id) {
        return Some(node);
    }
    node.children.iter().find_map(|child| target(child, id))
}

#[test]
fn retained_ports_have_real_accessibility_and_authored_positions() {
    let nodes = vec![
        GraphCanvasNode {
            x: 10.0,
            y: 20.0,
            width: 180.0,
            content: Box::new(Button::new("output", "Source output")),
        },
        GraphCanvasNode {
            x: 240.0,
            y: 120.0,
            width: 180.0,
            content: Box::new(Button::new("input", "Destination input")),
        },
    ];
    let mut harness = FrameHarness::new(
        crate::State::new(),
        Box::new(GraphCanvas::new(
            nodes,
            vec![GraphCanvasConnection {
                from: "output".into(),
                to: "input".into(),
            }],
        )),
        (500, 300),
    );
    let inspection = harness.inspection();
    let output = target(&inspection.root, "output").expect("retained source port");
    let input = target(&inspection.root, "input").expect("retained destination port");
    assert_eq!(&output.rect[..3], &[10.0, 20.0, 180.0]);
    assert_eq!(&input.rect[..3], &[240.0, 120.0, 180.0]);
    assert!(output.focusable && input.focusable);
    assert!(harness.a11y_node_named("Source output").is_some());
    assert!(harness.a11y_node_named("Destination input").is_some());
    harness.click_at(300.0, 135.0);
    assert!(harness.a11y_node_named("Destination input").is_some());
}
