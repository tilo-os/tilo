//! **Inspect**: reading an app's semantic tree from outside the process.
//!
//! The shell asks an app connection, over the collaboration broker, for the
//! node at a surface point, a node by id, or a depth-bounded subtree. The app
//! answers from its retained [`SemanticTree`] — the same tree AccessKit, the
//! `ui:` semantic action resources and `Ctrl+K` are built from — so a pick, a
//! recorded step and a screen-reader focus all name one node by one
//! [`SemanticId`].
//!
//! The request and answer are [`tilo_reference::inspect`]'s wire vocabulary,
//! shared with everything that reads an answer without drawing UI (the
//! session recorder, the frame masker). The app reads an [`InspectRequest`]
//! off the broker's inspection stream and answers one [`InspectReply`]; the
//! broker checks the caller may read the room before the request ever
//! reaches the app, and redacts the answer again on its way back.

use vello::kurbo::{Point, Rect};

pub use tilo_reference::inspect::{
    Content, INSPECT_VERSION, Identity, InspectQuery, InspectReply, InspectRequest, InspectedNode,
    MAX_INSPECT_DEPTH, MAX_INSPECT_NODES, NodeState, Pixels, SurfaceRect, redact_node,
};

use crate::a11y::{Audience, Role};
use crate::a11y_tree::{SemanticId, SemanticNode, SemanticTree};

/// The wire name of `role`: its variant name in snake case (`text_input`,
/// `heading`).
pub fn role_name(role: Role) -> String {
    let name = match serde_json::to_value(role) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(serde_json::Value::Object(map)) => map.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    };
    let mut out = String::with_capacity(name.len() + 4);
    for (index, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn surface_rect(rect: Rect) -> SurfaceRect {
    SurfaceRect {
        x: rect.x0,
        y: rect.y0,
        width: rect.width(),
        height: rect.height(),
    }
}

fn inspected(node: &SemanticNode, audience: Audience) -> InspectedNode {
    let mut out = InspectedNode {
        id: node.id.0.clone(),
        identity: if node.durable_identity {
            Identity::Durable
        } else {
            Identity::Positional
        },
        role: role_name(node.role),
        label: node.name.clone(),
        value: node.value.clone(),
        sensitivity: node.sensitivity,
        content: if node.presents_value() {
            Content::Value
        } else {
            Content::Label
        },
        bounds: node.bounds.map(surface_rect),
        visible: node.visible_bounds().map(surface_rect),
        // A canvas paints what no node describes, so its pixels are not
        // accounted for by the tree.
        pixels: if matches!(node.role, Role::Canvas | Role::Unknown) {
            Pixels::Undescribed
        } else {
            Pixels::Described
        },
        state: NodeState {
            focused: node.state.focused,
            disabled: node.state.disabled,
        },
        children: node.children.iter().map(|child| child.0.clone()).collect(),
        reference: node.reference.clone(),
    };
    redact_node(&mut out, audience);
    out
}

impl SemanticTree {
    /// The path from the window root to the deepest on-screen node under
    /// `point`, root first; empty when nothing is there.
    ///
    /// A node counts only where it is visible: its bounds intersected with
    /// every ancestor clip, so a row scrolled out of its list or a child
    /// painted outside a clipped panel is not hit. Later siblings are painted
    /// over earlier ones and are tried first; a child is tried even outside
    /// its parent's bounds, because a popover may extend beyond its anchor.
    pub fn hit_path(&self, point: Point) -> Vec<SemanticId> {
        let mut path = Vec::new();
        if self.hit_into(self.root(), point, &mut path) {
            path.push(self.root().clone());
        }
        path.reverse();
        path
    }

    fn hit_into(&self, id: &SemanticId, point: Point, path: &mut Vec<SemanticId>) -> bool {
        let Some(node) = self.node(id) else {
            return false;
        };
        if node.state.hidden {
            return false;
        }
        for child in node.children.iter().rev() {
            if self.hit_into(child, point, path) {
                path.push(child.clone());
                return true;
            }
        }
        node.visible_bounds()
            .is_some_and(|visible| visible.contains(point))
    }

    /// Answer `request` from this tree, redacted for its audience.
    pub fn inspect(&self, request: &InspectRequest) -> InspectReply {
        let audience = request.audience;
        let mut reply = InspectReply::empty();
        match &request.query {
            InspectQuery::At { x, y } => {
                let path = self.hit_path(Point::new(*x, *y));
                // The window root alone is not an answer: nothing is there.
                if path.len() > 1 {
                    reply.target = path.last().map(|id| id.0.clone());
                    reply.nodes = path
                        .iter()
                        .filter_map(|id| self.node(id))
                        .map(|node| inspected(node, audience))
                        .collect();
                }
            }
            InspectQuery::Node { id } => {
                if let Some(node) = self.node(&SemanticId(id.clone())) {
                    reply.target = Some(id.clone());
                    reply.nodes.push(inspected(node, audience));
                }
            }
            InspectQuery::Subtree { root, depth } => {
                let root = root.clone().map_or_else(|| self.root().clone(), SemanticId);
                let depth = (*depth).min(MAX_INSPECT_DEPTH);
                if self.node(&root).is_some() {
                    reply.target = Some(root.0.clone());
                    let mut frontier = std::collections::VecDeque::from([(root, 0_u16)]);
                    while let Some((id, level)) = frontier.pop_front() {
                        let Some(node) = self.node(&id) else {
                            continue;
                        };
                        if reply.nodes.len() >= MAX_INSPECT_NODES {
                            reply.truncated = true;
                            break;
                        }
                        reply.nodes.push(inspected(node, audience));
                        if node.children.is_empty() {
                            continue;
                        }
                        if level >= depth {
                            reply.truncated = true;
                            continue;
                        }
                        let next = level.saturating_add(1);
                        frontier.extend(node.children.iter().map(|child| (child.clone(), next)));
                    }
                }
            }
        }
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{AccessNode, Role, Sensitivity};
    use crate::theme::Theme;
    use crate::widget::{Button, Column, TextInput, TextInputKind, Widget};
    use std::collections::HashMap;
    use taffy::TaffyTree;

    fn layout(root: &mut dyn Widget) -> HashMap<taffy::NodeId, *mut dyn Widget> {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = root.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
            )
            .unwrap();
        root.apply_layout(&taffy, node, Point::ZERO);
        node_map
    }

    fn tree_of(root: &mut dyn Widget) -> SemanticTree {
        let map = layout(root);
        let mut tree = SemanticTree::new();
        tree.refresh(root, &map, None, &[]);
        tree
    }

    fn form() -> Column {
        Column::new()
            .push(Box::new(Button::new("save", "Save", None)))
            .push(Box::new(
                TextInput::local()
                    .with_initial_value("hunter2")
                    .with_secret(true)
                    .with_accessible_name("Password"),
            ))
            .push(Box::new(
                TextInput::local()
                    .with_kind(TextInputKind::Email)
                    .with_initial_value("ada@example.org")
                    .with_accessible_name("Email"),
            ))
    }

    fn by_label<'a>(reply: &'a InspectReply, label: &str) -> &'a InspectedNode {
        reply
            .nodes
            .iter()
            .find(|node| node.label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("no node labelled {label}: {reply:#?}"))
    }

    #[test]
    fn a_secret_never_crosses_and_personal_content_keeps_its_shape_beyond_the_device() {
        let mut root = form();
        let tree = tree_of(&mut root);
        let query = InspectQuery::Subtree {
            root: None,
            depth: 8,
        };
        let local = tree.inspect(&InspectRequest::new(query.clone(), Audience::LocalSession));
        let beyond = tree.inspect(&InspectRequest::new(query, Audience::BeyondDevice));

        for reply in [&local, &beyond] {
            let password = by_label(reply, "Password");
            assert_eq!(password.sensitivity, Sensitivity::Secret);
            assert_eq!(password.value, None);
            let json = serde_json::to_string(reply).unwrap();
            assert!(!json.contains("hunter2"), "secret leaked: {json}");
        }
        assert_eq!(
            by_label(&local, "Email").value.as_deref(),
            Some("ada@example.org"),
            "personal content is readable inside the session"
        );
        let shared = by_label(&beyond, "Email").value.clone().unwrap();
        assert_eq!(shared.chars().count(), "ada@example.org".chars().count());
        assert!(!shared.contains("ada"), "personal content left the device");
        assert_eq!(
            beyond.clone().redacted_for(Audience::BeyondDevice),
            beyond,
            "redaction is idempotent"
        );
    }

    #[test]
    fn roles_cross_the_wire_by_snake_case_name() {
        assert_eq!(role_name(Role::TextInput), "text_input");
        assert_eq!(role_name(Role::Heading { level: 2 }), "heading");
        assert_eq!(role_name(Role::Button), "button");
    }

    #[test]
    fn a_node_is_found_by_id_and_a_subtree_is_depth_bounded() {
        let mut root = form();
        let tree = tree_of(&mut root);
        let save = "t:save".to_owned();
        let reply = tree.inspect(&InspectRequest::new(
            InspectQuery::Node { id: save.clone() },
            Audience::LocalSession,
        ));
        assert_eq!(reply.target.as_ref(), Some(&save));
        assert_eq!(reply.target_node().unwrap().role, "button");

        let shallow = tree.inspect(&InspectRequest::new(
            InspectQuery::Subtree {
                root: None,
                depth: 0,
            },
            Audience::LocalSession,
        ));
        assert_eq!(shallow.nodes.len(), 1, "depth 0 is the root alone");
        assert!(shallow.truncated);
        assert!(
            shallow.nodes[0].children.contains(&save),
            "children ids are listed even when the children are not carried"
        );

        let missing = tree.inspect(&InspectRequest::new(
            InspectQuery::Node {
                id: "t:absent".into(),
            },
            Audience::LocalSession,
        ));
        assert_eq!(missing.target, None);
        assert!(missing.nodes.is_empty());
    }

    /// A test widget with explicit geometry and an optional child clip, so
    /// nesting, clipping and scrolling are laid out exactly.
    struct Placed {
        name: &'static str,
        rect: Rect,
        clip: Option<Rect>,
        access: Option<AccessNode>,
        children: Vec<Box<dyn Widget>>,
    }

    impl Placed {
        fn node(name: &'static str, rect: Rect, access: Option<AccessNode>) -> Self {
            Self {
                name,
                rect,
                clip: None,
                access,
                children: Vec::new(),
            }
        }
        fn clipped(mut self, clip: Rect) -> Self {
            self.clip = Some(clip);
            self
        }
        fn child(mut self, child: Placed) -> Self {
            self.children.push(Box::new(child));
            self
        }
    }

    impl Widget for Placed {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let child_nodes: Vec<taffy::NodeId> = self
                .children
                .iter_mut()
                .map(|child| child.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &child_nodes)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: taffy::NodeId, _: Point) {}
        fn draw(
            &mut self,
            _: &mut vello::Scene,
            _: vello::kurbo::Affine,
            _: &mut crate::text_render::TextRenderer,
            _: &Theme,
            _: &crate::presence::PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _: &winit::event::WindowEvent,
            _: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
        fn name(&self) -> &str {
            self.name
        }
        fn accessibility(&self) -> Option<AccessNode> {
            self.access.clone()
        }
        fn child_clip_rect(&self) -> Option<Rect> {
            self.clip
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children.iter().map(AsRef::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.children
                .iter_mut()
                .map(|child| &mut **child as &mut (dyn Widget + '_))
                .collect()
        }
    }

    fn named(role: Role, name: &str) -> Option<AccessNode> {
        Some(AccessNode::new(role).with_name(name))
    }

    #[test]
    fn hit_testing_follows_nesting_clipping_and_scrolling() {
        // A panel holding a list scrolled by 100px inside a 0..200 viewport:
        // the first row has scrolled above the viewport, the second is
        // visible, the third is below it. A popover later in paint order
        // overlaps the panel's corner.
        let rows = Placed::node(
            "List",
            Rect::new(0.0, -100.0, 200.0, 300.0),
            named(Role::List, "Rows"),
        )
        .child(Placed::node(
            "Row",
            Rect::new(0.0, -100.0, 200.0, 0.0),
            named(Role::ListItem, "First"),
        ))
        .child(Placed::node(
            "Row",
            Rect::new(0.0, 0.0, 200.0, 100.0),
            named(Role::ListItem, "Second"),
        ))
        .child(Placed::node(
            "Row",
            Rect::new(0.0, 200.0, 200.0, 300.0),
            named(Role::ListItem, "Third"),
        ));
        let viewport = Placed::node("Scroll", Rect::new(0.0, 0.0, 200.0, 200.0), None)
            .clipped(Rect::new(0.0, 0.0, 200.0, 200.0))
            .child(rows);
        let panel = Placed::node(
            "Panel",
            Rect::new(0.0, 0.0, 400.0, 400.0),
            named(Role::Group, "Panel"),
        )
        .child(viewport)
        .child(Placed::node(
            "Popover",
            Rect::new(150.0, 50.0, 250.0, 150.0),
            named(Role::Dialog, "Menu"),
        ));
        let mut root = panel;
        let tree = tree_of(&mut root);
        let at = |x, y| {
            tree.inspect(&InspectRequest::new(
                InspectQuery::At { x, y },
                Audience::LocalSession,
            ))
        };
        let label = |reply: &InspectReply| reply.target_node().and_then(|node| node.label.clone());

        assert_eq!(
            label(&at(20.0, 50.0)).as_deref(),
            Some("Second"),
            "visible row is hit"
        );
        let path = at(20.0, 50.0);
        assert_eq!(
            path.nodes
                .iter()
                .filter_map(|node| node.label.as_deref())
                .collect::<Vec<_>>(),
            ["Tilo application window", "Panel", "Rows", "Second"],
            "the answer carries the path from the root"
        );
        assert_eq!(
            label(&at(20.0, 250.0)).as_deref(),
            Some("Panel"),
            "a row below the viewport is clipped away, so its container answers"
        );
        assert_eq!(
            label(&at(160.0, 60.0)).as_deref(),
            Some("Menu"),
            "a later sibling paints over the list"
        );
        let second = tree.find_by_name("Second").unwrap();
        assert_eq!(
            second.visible_bounds(),
            Some(Rect::new(0.0, 0.0, 200.0, 100.0))
        );
        assert_eq!(tree.find_by_name("First").unwrap().visible_bounds(), None);
        assert_eq!(at(500.0, 500.0).target, None, "outside every node");
    }
}
