use super::*;
use crate::app::driver::Driver;
use crate::headless::InspectionNode;
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
    let mut harness = Driver::new(
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

#[test]
fn a_titled_graph_shows_its_title_over_its_nodes() {
    let nodes = vec![GraphCanvasNode {
        x: 0.0,
        y: 0.0,
        width: 180.0,
        content: Box::new(Button::new("output", "Source output")),
    }];
    let harness = Driver::new(
        crate::State::new(),
        Box::new(GraphCanvas::new(nodes, Vec::new()).with_title(Some("Release pipeline".into()))),
        (500, 300),
    );
    let inspection = harness.inspection();
    let output = target(&inspection.root, "output").expect("the node");
    let theme = tilo_ui_base::Theme::light();
    let band = crate::widget::chart_navigation::title_band(&theme, "Release pipeline");
    assert!(
        (output.rect[1] - band).abs() < 1.0,
        "the nodes start a chart's title band under the title: {:?}, band {band}",
        output.rect
    );
    assert!(harness.a11y_node_named("Release pipeline").is_some());
}
