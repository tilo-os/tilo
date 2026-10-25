use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};

use super::*;
use crate::Theme;
use crate::a11y::{AccessNode, Role};

struct Node {
    name: &'static str,
    rect: Rect,
    access: Option<AccessNode>,
    kids: Vec<Node>,
}

impl Widget for Node {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.access.clone()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.kids.iter().map(|kid| kid as &dyn Widget).collect()
    }
}

/// A message row: who said it, what they said and a Reply button.
fn window() -> Node {
    let leaf = |name, rect, access| Node {
        name,
        rect,
        access: Some(access),
        kids: Vec::new(),
    };
    Node {
        name: "Window",
        rect: Rect::new(0.0, 0.0, 400.0, 100.0),
        access: None,
        kids: vec![Node {
            name: "Row",
            rect: Rect::new(0.0, 0.0, 400.0, 50.0),
            access: Some(AccessNode::new(Role::ListItem).with_semantic_key("m1")),
            kids: vec![
                leaf(
                    "Text",
                    Rect::new(10.0, 10.0, 300.0, 40.0),
                    AccessNode::new(Role::StaticText).with_name("Thanks Ada"),
                ),
                leaf(
                    "Button",
                    Rect::new(320.0, 10.0, 390.0, 40.0),
                    AccessNode::new(Role::Button)
                        .with_name("Reply")
                        .with_interface_name(true),
                ),
            ],
        }],
    }
}

fn roundtrip<T: Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
    serde_json::from_slice(&serde_json::to_vec(value).unwrap()).unwrap()
}

#[test]
fn a_query_and_its_answer_read_back_as_they_were_written() {
    for query in [
        PickQuery::At { point: [1.5, 2.0] },
        PickQuery::Within {
            area: [0.0, 0.0, 10.0, 20.0],
            limit: 6,
        },
        PickQuery::Capture {
            parts: vec![PartToCapture {
                path: "Window[0]/Row[0]".into(),
                rect: [0.0, 0.0, 400.0, 50.0],
            }],
        },
    ] {
        assert_eq!(roundtrip(&query), query);
    }
    let wire = serde_json::to_value(PickQuery::At { point: [1.0, 2.0] }).unwrap();
    assert_eq!(wire, serde_json::json!({"kind": "at", "point": [1.0, 2.0]}));
    let captured = PickAnswer::Captured {
        parts: vec![CapturedPart {
            path: "Window[0]/Row[0]".into(),
            role: None,
            kind: None,
            label: None,
            texts: vec![CapturedText {
                text: "Reply".into(),
                interface: true,
                rect: None,
            }],
            masked: vec![[0.0, 0.0, 1.0, 1.0]],
        }],
        recent: vec![RecentAction {
            at: 7,
            resource: "chat".into(),
            action: "send".into(),
            label: None,
            done: true,
        }],
    };
    assert_eq!(roundtrip(&captured), captured);
    let wire = serde_json::to_string(&captured).unwrap();
    assert!(
        !wire.contains("\"role\"") && !wire.contains("\"label\""),
        "absent fields are left out: {wire}"
    );
}

#[test]
fn a_window_answers_what_is_at_a_point_inside_an_area_and_what_a_pick_holds() {
    let root = window();
    let PickAnswer::Targets { parts } = answer(
        &root,
        &PickQuery::At {
            point: [350.0, 20.0],
        },
    ) else {
        panic!("targets");
    };
    let roles: Vec<&str> = parts.iter().map(|part| part.role.as_str()).collect();
    assert_eq!(roles, ["ListItem", "Button"], "outermost first: {parts:?}");
    assert_eq!(parts[1].rect, [320.0, 10.0, 390.0, 40.0]);
    let button = parts[1].path.clone();

    let PickAnswer::Targets { parts } = answer(
        &root,
        &PickQuery::At {
            point: [f64::NAN, 1.0],
        },
    ) else {
        panic!("targets");
    };
    assert!(parts.is_empty(), "a point that is no point names nothing");

    let PickAnswer::Targets { parts } = answer(
        &root,
        &PickQuery::Within {
            area: [0.0, 0.0, 400.0, 60.0],
            limit: 1,
        },
    ) else {
        panic!("targets");
    };
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].role, "ListItem");
    let row = parts[0].path.clone();

    let PickAnswer::Captured { parts, .. } = answer(
        &root,
        &PickQuery::Capture {
            parts: vec![
                PartToCapture {
                    path: row,
                    rect: [0.0, 0.0, 400.0, 50.0],
                },
                PartToCapture {
                    path: "Window[0]/Gone[3]".into(),
                    rect: [0.0, 0.0, 1.0, 1.0],
                },
                PartToCapture {
                    path: button,
                    rect: [f64::INFINITY, 0.0, 1.0, 1.0],
                },
            ],
        },
    ) else {
        panic!("captured");
    };
    assert_eq!(parts.len(), 1, "a gone part or a broken rect is left out");
    let texts: Vec<(&str, bool)> = parts[0]
        .texts
        .iter()
        .map(|text| (text.text.as_str(), text.interface))
        .collect();
    assert_eq!(texts, [("Thanks Ada", false), ("Reply", true)]);
    assert_eq!(
        parts[0].masked,
        [[10.0, 10.0, 300.0, 40.0]],
        "the content is masked, the app's own copy is not"
    );
}
