use tilo_ui_base::accesskit_bridge::*;
use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;
use std::collections::HashMap;
use taffy::TaffyTree;
use tilo_ui_base::a11y::Action;
use accesskit::Action as AkAction;
use accesskit::Live as AkLive;
use accesskit::Node as AkNode;
use accesskit::NodeId as AkNodeId;
use accesskit::Role as AkRole;
use accesskit::TextPosition as AkTextPosition;
use taffy::NodeId as TaffyNodeId;
use accesskit::Toggled;
use accesskit::TreeUpdate;

struct TestWidget {
    access: Option<AccessNode>,
    rect: vello::kurbo::Rect,
    kids: Vec<Box<dyn Widget>>,
    name: String,
    represents: Vec<usize>,
}

impl TestWidget {
    fn new(access: Option<AccessNode>) -> Self {
        Self {
            access,
            rect: vello::kurbo::Rect::new(0.0, 0.0, 0.0, 0.0),
            kids: vec![],
            name: "test".into(),
            represents: vec![],
        }
    }
    fn with_rect(mut self, x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        self.rect = vello::kurbo::Rect::new(x0, y0, x1, y1);
        self
    }
    fn with_child(mut self, c: Box<dyn Widget>) -> Self {
        self.kids.push(c);
        self
    }
}

impl Widget for TestWidget {
    fn sync(&mut self, _s: &tilo_ui_base::widget::AppModel, _p: &tilo_ui_base::widget::ModelPath) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let child_nodes: Vec<taffy::NodeId> = self
            .kids
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        taffy.widget_node(taffy::Style::default(), &child_nodes, node_map, self)
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.kids.iter().map(std::convert::AsRef::as_ref).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.kids
            .iter_mut()
            .map(|c| &mut **c as &mut (dyn Widget + '_))
            .collect()
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.access.clone()
    }
    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        self.represents.contains(&child_index)
    }
}

#[repr(C)]
struct AliasWrapper {
    child: TestWidget,
}

impl Widget for AliasWrapper {
    fn sync(&mut self, _s: &tilo_ui_base::widget::AppModel, _p: &tilo_ui_base::widget::ModelPath) {}
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<TaffyNodeId, *mut dyn Widget>,
    ) -> TaffyNodeId {
        self.child.build_layout(tree, theme, nodes)
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.child]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.child]
    }
    fn name(&self) -> &str {
        "AliasWrapper"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.child.rect
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.child.access.clone()
    }
}

tilo_test_env::scenario! {
    fn accesskit_node_ids_are_distinct_stable_and_resolve_back;

    phase node_ids_stay_distinct_and_stable_across_tree_changes;
    {
        {
            let mut wrapper = AliasWrapper {
                child: TestWidget::new(Some(AccessNode::new(Role::Button))),
            };
            assert_eq!(
                std::ptr::from_ref(&wrapper).cast::<()>(),
                std::ptr::from_ref(&wrapper.child).cast::<()>()
            );
            let (mut tree, mut nodes, child) = lay_out(&mut wrapper);
            let update = build_tree_update(&wrapper, &nodes, Some(child), 1.0);
            let ids: std::collections::HashSet<_> =
                update.nodes.iter().map(|(id, _)| *id).collect();
            assert_eq!(
                ids.len(),
                update.nodes.len(),
                "every semantic node has a distinct id"
            );
            assert_eq!(update.focus, widget_node_id(Some(child), 0));
            assert_eq!(
                update.nodes.len(),
                3,
                "window, wrapper and child all remain accessible"
            );
            for (id, node) in &update.nodes {
                assert!(
                    !node.children().contains(id),
                    "node cannot be its own child"
                );
            }
            let parent = tree.new_leaf(taffy::Style::default()).unwrap();
            nodes.insert(parent, std::ptr::from_mut::<dyn Widget>(&mut wrapper));
            for focused in [parent, child] {
                let update = build_tree_update(&wrapper, &nodes, Some(focused), 1.0);
                assert_eq!(update.focus, widget_node_id(Some(focused), 0));
                assert!(
                    update
                        .nodes
                        .iter()
                        .any(|(id, _)| *id == widget_node_id(Some(parent), 0))
                );
                assert!(
                    update
                        .nodes
                        .iter()
                        .any(|(id, _)| *id == widget_node_id(Some(child), 0))
                );
                let ids: std::collections::HashSet<_> =
                    update.nodes.iter().map(|(id, _)| *id).collect();
                assert_eq!(ids.len(), update.nodes.len());
            }
        }

        // Case `a_child_at_its_parents_address_keeps_its_own_node_id`:
        // The reverse node map must tell a container apart from the child it
        // starts with: keyed by data pointer alone, the two collided and the
        // child was exported under its parent's taffy id, so an activation
        // aimed at the child id found no node.
        {
            let mut root = FirstFieldHost {
                child: TestWidget::new(Some(AccessNode::new(Role::Button).with_name("Bump")))
                    .with_rect(0.0, 0.0, 40.0, 20.0),
                pad: 0,
            };
            let (_taffy, node_map, root_node) = lay_out(&mut root);
            assert!(
                std::ptr::eq(
                    std::ptr::from_ref::<dyn Widget>(&root).cast::<()>(),
                    std::ptr::from_ref::<dyn Widget>(&root.child).cast::<()>()
                ),
                "the fixture must reproduce the shared address"
            );
            let child_node = *node_map
                .keys()
                .find(|node| **node != root_node)
                .expect("the child has its own taffy node");
            let child_id = AkNodeId(u64::from(child_node).saturating_add(1));
            let root_id = AkNodeId(u64::from(root_node).saturating_add(1));

            let update = build_tree_update(&root, &node_map, None, 1.0);
            let role_of = |wanted: AkNodeId| {
                update
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == wanted)
                    .map(|(_, node)| node.role())
            };
            assert_eq!(
                role_of(child_id),
                Some(AkRole::Button),
                "child exported under its own id"
            );
            assert_eq!(
                role_of(root_id),
                Some(AkRole::GenericContainer),
                "parent exported under its own id"
            );
            assert!(
                matches!(
                    resolve_node_id(&root, &node_map, child_id),
                    Some(A11yTarget::Widget(node)) if node == child_node
                ),
                "the child's id resolves back to the child's taffy node"
            );
        }

        {
            // Two independent widgets with synthetic contents. Adding a clip to the
            // FIRST must not renumber the second's rows: an AT holding a reference
            // to "Beta / two" would otherwise silently start pointing elsewhere.
            let mut first = Box::new(TestWidget::new(Some(canvas(vec![clip("a", false)]))));
            let first_ptr: *mut TestWidget = &raw mut *first;
            let second = Box::new(TestWidget::new(Some(
                AccessNode::new(Role::Table)
                    .with_name("Beta")
                    .with_children(vec![
                        AccessNode::new(Role::Row).with_name("one"),
                        AccessNode::new(Role::Row).with_name("two"),
                    ]),
            )));
            let mut root = TestWidget::new(None).with_child(first).with_child(second);
            let (_taffy, node_map, _n) = lay_out(&mut root);

            let before = build_tree_update(&root, &node_map, None, 1.0);
            let two_before = label_id(&before, "two");

            unsafe {
                (*first_ptr).access = Some(canvas(vec![
                    clip("a", false),
                    clip("b", false),
                    clip("c", false),
                ]));
            }
            let after = build_tree_update(&root, &node_map, None, 1.0);

            assert_eq!(
                label_id(&after, "two"),
                two_before,
                "a synthetic id must not depend on how many synthetic nodes precede it in the tree"
            );
            assert_ne!(
                label_id(&after, "one"),
                two_before,
                "sibling ids must still be distinct"
            );
        }

        {
            // Ordinal keying is right for a table row ("row 3" is the identity the
            // user means), but wrong for a clip that can be dragged along a track.
            // An explicit key pins the id to the element instead of the slot.
            let keyed = |name: &str| {
                AccessNode::new(Role::ListItem)
                    .with_name(name)
                    .with_hashed_key(name)
            };
            let mut root = TestWidget::new(Some(canvas(vec![keyed("intro"), keyed("outro")])));
            let (_taffy, node_map, _n) = lay_out(&mut root);

            let before = build_tree_update(&root, &node_map, None, 1.0);
            let outro_before = label_id(&before, "outro");

            root.access = Some(canvas(vec![keyed("outro"), keyed("intro")]));
            let after = build_tree_update(&root, &node_map, None, 1.0);

            assert_eq!(
                label_id(&after, "outro"),
                outro_before,
                "an explicitly keyed node keeps its id across a reorder"
            );
            assert_ne!(
                label_id(&after, "intro"),
                outro_before,
                "and does not inherit its new neighbour's id"
            );
        }

        {
            let mut root = TestWidget::new(Some(canvas(vec![clip("a", false), clip("b", false)])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            assert!(!is_synthetic_id(ROOT_ID));
            for (id, node) in &update.nodes {
                let synthetic = node.role() == AkRole::ListItem;
                assert_eq!(
                    is_synthetic_id(*id),
                    synthetic,
                    "id {id:?} ({:?}) is in the wrong id space",
                    node.role()
                );
            }
            let mut ids: Vec<u64> = update.nodes.iter().map(|(id, _)| id.0).collect();
            ids.sort_unstable();
            let count = ids.len();
            ids.dedup();
            assert_eq!(count, ids.len(), "duplicate node id in the tree update");
        }
    }

    phase two_windows_that_share_a_control_name_each_keep_their_own_node;
    {
        let toggle = |x0: f64| {
            AccessNode::new(Role::Button)
                .with_name("Show sidebar")
                .with_action_target("sidebar-toggle")
                .with_bounds(vello::kurbo::Rect::new(x0, 0.0, x0 + 32.0, 32.0))
                .with_state(tilo_ui_base::a11y::AccessState::new().focusable(true))
        };
        let mut windows = TestWidget::new(Some(
            AccessNode::new(Role::Group).with_children(vec![toggle(0.0), toggle(604.0)]),
        ));
        let (_taffy, node_map, _root) = lay_out(&mut windows);
        let update = build_tree_update(&windows, &node_map, None, 1.0);
        let shown: Vec<(AkNodeId, f64)> = update
            .nodes
            .iter()
            .filter(|(_, node)| node.label() == Some("Show sidebar"))
            .map(|(id, node)| (*id, node.bounds().map_or(f64::NAN, |bounds| bounds.x0)))
            .collect();
        assert_eq!(
            shown.len(),
            2,
            "both windows' toggles are in the tree: {shown:?}"
        );
        assert_ne!(
            shown[0].0, shown[1].0,
            "each window's toggle has its own id"
        );
        assert_eq!(
            [shown[0].1, shown[1].1],
            [0.0, 604.0],
            "each stands where its window drew it"
        );
        let ids: std::collections::HashSet<_> = update.nodes.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids.len(),
            update.nodes.len(),
            "a node id addresses one node of the tree"
        );
    }

    phase resolve_node_id_maps_emitted_ids_back_or_reports_them_gone;
    {
        {
            // The action-routing contract: whatever id the bridge put in the tree,
            // an incoming ActionRequest carrying it must resolve back to the widget
            // that owns it plus the path naming the exact element.
            let mut root = TestWidget::new(Some(
                AccessNode::new(Role::Table)
                    .with_name("People")
                    .with_children(vec![
                        AccessNode::new(Role::Row)
                            .with_name("r0")
                            .with_children(vec![
                                AccessNode::new(Role::Cell).with_name("r0c0"),
                                AccessNode::new(Role::Cell).with_name("r0c1"),
                            ]),
                        AccessNode::new(Role::Row)
                            .with_name("r1")
                            .with_children(vec![
                                AccessNode::new(Role::Cell).with_name("r1c0"),
                                AccessNode::new(Role::Cell).with_name("r1c1"),
                            ]),
                    ]),
            ));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let table_taffy = taffy_id_of(&node_map, Role::Table);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            assert_eq!(
                resolve_node_id(&root, &node_map, label_id(&update, "r1")),
                Some(A11yTarget::Synthetic {
                    owner: table_taffy,
                    key_path: vec![1],
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![1],
                    semantic_target: None,
                }),
            );
            assert_eq!(
                resolve_node_id(&root, &node_map, label_id(&update, "r1c0")),
                Some(A11yTarget::Synthetic {
                    owner: table_taffy,
                    key_path: vec![1, 0],
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![1, 0],
                    semantic_target: None,
                }),
            );
            assert_eq!(
                resolve_node_id(&root, &node_map, label_id(&update, "r0c1")),
                Some(A11yTarget::Synthetic {
                    owner: table_taffy,
                    key_path: vec![0, 1],
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![0, 1],
                    semantic_target: None,
                }),
            );
            assert_eq!(
                resolve_node_id(&root, &node_map, label_id(&update, "People")),
                Some(A11yTarget::Widget(table_taffy)),
            );
            assert_eq!(
                resolve_node_id(&root, &node_map, ROOT_ID),
                Some(A11yTarget::Root),
            );
        }

        {
            // An AT can hold an id across a structural change. Resolving it to
            // "whatever now sits there" would operate on the wrong element, so an
            // unknown id must resolve to nothing at all.
            let mut root = TestWidget::new(Some(canvas(vec![clip("only", false)])));
            let (_taffy, node_map, _n) = lay_out(&mut root);

            let bogus_synthetic = AkNodeId(0x1234_5678_9abc_def0 | SYNTHETIC_ID_BIT);
            assert_eq!(resolve_node_id(&root, &node_map, bogus_synthetic), None);

            let bogus_widget = AkNodeId(u64::from(u32::MAX));
            assert_eq!(resolve_node_id(&root, &node_map, bogus_widget), None);
        }

        {
            // Without a taffy id there is no handle to dispatch against, so such a
            // node must report as unaddressable rather than resolve to a wrong owner.
            let root = TestWidget::new(Some(canvas(vec![clip("orphan", false)])));
            let empty_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();

            let update = build_tree_update(&root, &empty_map, None, 1.0);
            let orphan = label_id(&update, "orphan");
            assert!(is_synthetic_id(orphan));
            assert_eq!(resolve_node_id(&root, &empty_map, orphan), None);
        }

        {
            // A host that publishes a child's semantics itself (the shell's
            // palette inside its overlay tree) must not have the child
            // node exported beside it, and keyboard focus on the child goes
            // to the representation.
            let child = TestWidget::new(Some(
                AccessNode::new(Role::Dialog).with_name("palette widget"),
            ));
            let mut host = TestWidget::new(Some(AccessNode::new(Role::Group).with_children(vec![
                    AccessNode::new(Role::Dialog)
                        .with_name("palette")
                        .with_state(AccessState::new().focusable(true).focused(true)),
                ])))
            .with_child(Box::new(child));
            host.represents = vec![0];
            let (_taffy, node_map, _n) = lay_out(&mut host);
            let widget_id = node_map
                .iter()
                .find(|(_, w)| {
                    unsafe { (***w).accessibility() }
                        .is_some_and(|a| a.name.as_deref() == Some("palette widget"))
                })
                .map(|(id, _)| *id)
                .expect("the child is laid out");
            let update = build_tree_update(&host, &node_map, Some(widget_id), 1.0);
            assert!(
                !update
                    .nodes
                    .iter()
                    .any(|(_, n)| n.label() == Some("palette widget")),
                "the represented child is not exported again"
            );
            assert_eq!(update.focus, label_id(&update, "palette"));
            let read: Vec<_> = tilo_ui_base::screen_reader::flatten_reading_order(&host)
                .into_iter()
                .filter_map(|node| node.name)
                .collect();
            assert_eq!(read, ["palette"], "nor read twice by the built-in reader");
        }

        {
            // A query field drawn by its owner (a palette) has no widget of its
            // own. It still needs a text run, or AT-SPI exposes no Text and no
            // EditableText and a screen reader cannot type into it.
            let mut root =
                TestWidget::new(Some(AccessNode::new(Role::Dialog).with_children(vec![
                    AccessNode::new(Role::TextInput)
                        .with_name("query")
                        .with_action_target("palette:query")
                        .with_value("fi")
                        .with_text_selection(2, 2),
                ])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let owner = taffy_id_of(&node_map, Role::Dialog);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let field_id = label_id(&update, "query");
            let field = &update
                .nodes
                .iter()
                .find(|(id, _)| *id == field_id)
                .expect("the field is exported")
                .1;
            assert!(field.supports_action(AkAction::SetValue));
            let run_id = field.children()[0];
            let (_, run) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == run_id)
                .expect("the run is exported");
            assert_eq!(run.role(), AkRole::TextRun);
            assert_eq!(run.value(), Some("fi"));
            assert_eq!(
                field
                    .text_selection()
                    .map(|s| (s.anchor.node, s.focus.character_index)),
                Some((run_id, 2)),
            );
            assert_ne!(run_id, field_id);
            assert_eq!(
                resolve_node_id(&root, &node_map, run_id),
                resolve_node_id(&root, &node_map, field_id),
            );
            assert!(matches!(
                resolve_node_id(&root, &node_map, run_id),
                Some(A11yTarget::Synthetic { owner: o, ref ordinal_path, ref semantic_target, .. })
                    if o == owner
                        && ordinal_path == &[0]
                        && semantic_target.as_deref() == Some("palette:query")
            ));
        }

        {
            // The run has the widget text, so a verb aimed at it belongs
            // to the widget — dropping it would make an AT's text actions no-ops.
            let field = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_value("hello")
                    .with_text_selection(1, 1)
                    .with_state(AccessState::new().focusable(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(field));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let field_taffy = taffy_id_of(&node_map, Role::TextInput);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let (run_id, _) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::TextRun)
                .expect("the field exports a text run");
            assert!(!is_synthetic_id(*run_id));
            assert_eq!(
                resolve_node_id(&root, &node_map, *run_id),
                Some(A11yTarget::Widget(field_taffy)),
            );
        }
    }
}

fn only(update: &TreeUpdate, role: AkRole) -> AkNode {
    let mut found = update
        .nodes
        .iter()
        .map(|(_, node)| node)
        .filter(|node| node.role() == role);
    let node = found.next().unwrap_or_else(|| panic!("no {role:?}"));
    assert!(found.next().is_none(), "two {role:?} nodes");
    node.clone()
}

tilo_test_env::scenario! {
    fn a_rows_trailing_control_takes_the_rows_name;

    phase a_rows_trailing_control_takes_the_rows_name;
    // A row names the control it trails, whatever that control is: the
    // switch the row turns on and off (with its description), the field
    // that names itself, the chooser named for the row and not for its
    // value, and the slider a container wraps.
    {
        let cases = [
            (
                crate::widget::ListRow::new("remove-data", "Remove its data")
                    .secondary("Your settings and saved files go to the Trash.")
                    .trailing(Box::new(crate::widget::Toggle::from_state(
                        &tilo_ui_base::LocalState::new(false),
                    ))),
                AkRole::Switch,
                "Remove its data",
            ),
            (
                crate::widget::ListRow::new("name", "Device name").trailing(Box::new(
                    crate::widget::TextInput::new(Vec::new())
                        .with_accessible_name("Name shown to others"),
                )),
                AkRole::TextInput,
                "Name shown to others",
            ),
            (
                crate::widget::ListRow::new("currency", "Currency").trailing(Box::new(
                    crate::widget::Dropdown::from_state(
                        vec!["EUR".to_owned(), "USD".to_owned()],
                        &tilo_ui_base::LocalState::new("EUR".to_owned()),
                    ),
                )),
                AkRole::ComboBox,
                "Currency",
            ),
            (
                crate::widget::ListRow::new("sdr-brightness", "Brightness")
                    .secondary("How bright standard content is")
                    .description("How bright standard content is")
                    .trailing(Box::new(
                        crate::widget::Container::new()
                            .with_no_padding()
                            .with_child(Box::new(crate::widget::Slider::new("sdr", "sdr"))),
                    )),
                AkRole::Slider,
                "Brightness",
            ),
        ];
        for (mut row, role, label) in cases {
            let (_taffy, node_map, _root) = lay_out(&mut row);
            let update = build_tree_update(&row, &node_map, None, 1.0);
            let control = only(&update, role);
            assert_eq!(control.label(), Some(label), "{role:?}");
            match role {
                AkRole::Switch => assert_eq!(
                    control.description(),
                    Some("Your settings and saved files go to the Trash.")
                ),
                AkRole::ComboBox => assert_eq!(control.value(), Some("EUR")),
                AkRole::Slider => assert_eq!(
                    control.description(),
                    Some("How bright standard content is")
                ),
                AkRole::TextInput => {}
                other => panic!("unexpected role {other:?}"),
            }
        }
    }
}

fn lay_out(
    root: &mut dyn Widget,
) -> (
    TaffyTree<()>,
    HashMap<taffy::NodeId, *mut dyn Widget>,
    taffy::NodeId,
) {
    let mut taffy = TaffyTree::new();
    let mut node_map = HashMap::new();
    let theme = Theme::dark();
    let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
    (taffy, node_map, root_node)
}

#[repr(C)]
struct FirstFieldHost {
    child: TestWidget,
    pad: u64,
}

impl Widget for FirstFieldHost {
    fn sync(&mut self, _s: &tilo_ui_base::widget::AppModel, _p: &tilo_ui_base::widget::ModelPath) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let kid = self.child.build_layout(taffy, theme, node_map);
        taffy.widget_node(taffy::Style::default(), &[kid], node_map, self)
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.child]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.child]
    }
    fn name(&self) -> &str {
        "first-field-host"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::new(0.0, 0.0, 200.0, 100.0)
    }
    fn accessibility(&self) -> Option<AccessNode> {
        Some(AccessNode::new(Role::Generic))
    }
}

tilo_test_env::scenario! {
    fn the_tree_update_maps_roles_actions_shape_and_text_to_accesskit;

    phase roles_states_bounds_and_labels_map_to_accesskit;
    {
        {
            let cases = [
                (Role::Window, AkRole::Window),
                (Role::Dialog, AkRole::Dialog),
                (Role::TabBar, AkRole::TabList),
                (Role::ToggleButton, AkRole::Button),
                (Role::Checkbox, AkRole::CheckBox),
                (Role::Switch, AkRole::Switch),
                (Role::Spinner, AkRole::ProgressIndicator),
                (Role::ProgressBar, AkRole::ProgressIndicator),
                (Role::StaticText, AkRole::Label),
                (Role::Heading { level: 2 }, AkRole::Heading),
                (Role::MultilineTextInput, AkRole::MultilineTextInput),
                (Role::Terminal, AkRole::Terminal),
                (Role::Generic, AkRole::GenericContainer),
            ];
            for (ours, theirs) in cases {
                assert_eq!(map_role(ours), theirs, "role {ours:?} mapped wrong");
            }
        }

        {
            let checkbox = TestWidget::new(Some(
                AccessNode::new(Role::Checkbox).with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(true)
                        .disabled(true),
                ),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(checkbox));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::CheckBox)
                .unwrap();
            assert_eq!(node.toggled(), Some(Toggled::True));
            assert!(node.is_disabled());
            assert!(!node.supports_action(AkAction::Focus));
            assert!(!node.supports_action(AkAction::Click));
        }

        {
            let h = TestWidget::new(Some(
                AccessNode::new(Role::Heading { level: 3 }).with_name("Section"),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(h));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::Heading)
                .unwrap();
            assert_eq!(node.level(), Some(3));
        }

        {
            let btn = TestWidget::new(Some(AccessNode::new(Role::Button).with_name("B")))
                .with_rect(10.0, 20.0, 110.0, 60.0);
            let mut root = TestWidget::new(None).with_child(Box::new(btn));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);

            // Logical (scale 1.0).
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, button) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::Button)
                .unwrap();
            let b = button.bounds().expect("button has bounds");
            assert_eq!((b.x0, b.y0, b.x1, b.y1), (10.0, 20.0, 110.0, 60.0));

            // Physical (scale 2.0).
            let update2 = build_tree_update(&root, &node_map, None, 2.0);
            let (_, button2) = update2
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::Button)
                .unwrap();
            let b2 = button2.bounds().expect("button has bounds");
            assert_eq!((b2.x0, b2.y0, b2.x1, b2.y1), (20.0, 40.0, 220.0, 120.0));
        }

        {
            for role in [Role::Label, Role::StaticText] {
                let text = "SQLite · Grün 🌍 · 1 rows";
                let mut root = TestWidget::new(Some(AccessNode::new(role).with_name(text)));
                let (_taffy, node_map, _) = lay_out(&mut root);
                let update = build_tree_update(&root, &node_map, None, 1.0);
                let (_, label) = update
                    .nodes
                    .iter()
                    .find(|(_, node)| node.role() == AkRole::Label)
                    .unwrap();
                assert_eq!(
                    label.value(),
                    Some(text),
                    "AccessKit native label names come from their value"
                );
            }
        }

        {
            let mut root = TestWidget::new(None);
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert!(update.tree.is_some());
            assert_eq!(update.tree.as_ref().unwrap().root, ROOT_ID);
            assert!(update.nodes.iter().any(|(id, _)| *id == ROOT_ID));
        }
    }

    phase sidebar_shared_separator_exports_resize_verbs_to_accesskit;
    {
        use crate::widget::{SplitHandle, SplitOrientation, SplitRatio};
        let handle = SplitHandle::new(SplitOrientation::Horizontal, SplitRatio::default());
        let mut root = TestWidget::new(None).with_child(Box::new(handle));
        let (_taffy, map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == AkRole::Splitter)
            .expect("actual shared divider exported");
        for action in [AkAction::Increment, AkAction::Decrement, AkAction::SetValue] {
            assert!(node.supports_action(action));
        }
        assert_eq!(node.value(), Some("50%"));
    }

    phase supported_actions_are_advertised_and_round_trip;
    {
        {
            for a in [
                Action::Activate,
                Action::Focus,
                Action::Increment,
                Action::Decrement,
                Action::Expand,
                Action::Collapse,
                Action::ScrollIntoView,
                Action::ShowContextMenu,
            ] {
                let ak = map_action(&a);
                let back = accesskit_action_to_ours(ak, None).unwrap();
                assert_eq!(back, a, "action {a:?} did not round-trip");
            }

            let set = Action::SetValue("hello".into());
            let ak = map_action(&set);
            assert_eq!(ak, AkAction::SetValue);
            let data = accesskit::ActionData::Value("hello".into());
            assert_eq!(
                accesskit_action_to_ours(ak, Some(&data)),
                Some(Action::SetValue("hello".into()))
            );

            assert_eq!(map_action(&Action::ToggleCheck), AkAction::Click);
        }

        {
            let btn = TestWidget::new(Some(
                AccessNode::new(Role::Button)
                    .with_name("Go")
                    .with_state(AccessState::new().focusable(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(btn));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::Button)
                .unwrap();
            assert!(node.supports_action(AkAction::Focus));
            assert!(node.supports_action(AkAction::Click));
        }

        {
            let row = TestWidget::new(Some(
                AccessNode::new(Role::ListItem)
                    .with_name("Ada Lovelace")
                    .with_state(AccessState::new().focusable(true).context_menu(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(row));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::ListItem)
                .expect("list item reaches AccessKit");
            assert!(node.supports_action(AkAction::ShowContextMenu));
        }

        {
            let slider = TestWidget::new(Some(
                AccessNode::new(Role::Slider)
                    .with_value("50%")
                    .with_state(AccessState::new().focusable(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(slider));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::Slider)
                .unwrap();
            assert!(node.supports_action(AkAction::Increment));
            assert!(node.supports_action(AkAction::Decrement));
            assert_eq!(node.value(), Some("50%"));
        }

        {
            let immediate = tilo_actions::ActionSpec::operation(
                "refresh",
                "Refresh",
                tilo_actions::OperationKind::Effect,
                "test.refresh",
                &serde_json::json!({}),
            )
            .unwrap();
            let prompted = tilo_actions::ActionSpec::prompted_operation(
                "find",
                "Find",
                "Text to find",
                "query",
                tilo_actions::OperationKind::View,
                "test.find",
                |value| tilo_actions::EncodedAction::encode_operation("test.find", &value).unwrap(),
            );
            let resources =
                [
                    tilo_actions::ResourceSpec::new("document:1", "test.document", "Document")
                        .with_action(immediate)
                        .with_action(prompted),
                ];
            let mut root = TestWidget::new(None);
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update_with_resources(&root, &node_map, None, 1.0, &resources);

            let resource_id = resource_node_id("document:1");
            let refresh_id = resource_action_node_id("document:1", "refresh");
            let find_id = resource_action_node_id("document:1", "find");
            let root_node = update.nodes.iter().find(|(id, _)| *id == ROOT_ID).unwrap();
            assert!(root_node.1.children().contains(&resource_id));
            let resource = update
                .nodes
                .iter()
                .find(|(id, _)| *id == resource_id)
                .unwrap();
            assert_eq!(resource.1.role(), AkRole::Group);
            assert_eq!(resource.1.children(), &[refresh_id, find_id]);
            let refresh = update
                .nodes
                .iter()
                .find(|(id, _)| *id == refresh_id)
                .unwrap();
            let find = update.nodes.iter().find(|(id, _)| *id == find_id).unwrap();
            assert!(refresh.1.supports_action(AkAction::Click));
            assert!(!refresh.1.supports_action(AkAction::SetValue));
            assert!(find.1.supports_action(AkAction::SetValue));
            assert!(!find.1.supports_action(AkAction::Click));
            assert_eq!(
                resource_action_node_id("document:1", "refresh"),
                refresh_id,
                "action identity must remain stable between frames"
            );
        }
    }

    phase tree_shape_collapses_presentational_nodes_and_carries_synthetic_and_live_regions;
    {
        {
            let btn = TestWidget::new(Some(AccessNode::new(Role::Button).with_name("Go")));
            let group = TestWidget::new(Some(AccessNode::new(Role::Group).with_name("Form")))
                .with_child(Box::new(btn));
            let text = TestWidget::new(Some(AccessNode::new(Role::StaticText).with_name("Hi")));
            let wrapper = TestWidget::new(None).with_child(Box::new(text)); // presentational
            let mut root = TestWidget::new(None) // presentational root
                .with_child(Box::new(group))
                .with_child(Box::new(wrapper));

            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let roles: Vec<AkRole> = update.nodes.iter().map(|(_, n)| n.role()).collect();
            assert!(roles.contains(&AkRole::Window));
            assert!(roles.contains(&AkRole::Group));
            assert!(roles.contains(&AkRole::Button));
            assert!(roles.contains(&AkRole::Label));
            assert!(
                !roles.contains(&AkRole::GenericContainer),
                "presentational widgets must not emit a node"
            );

            let (_, window) = update.nodes.iter().find(|(id, _)| *id == ROOT_ID).unwrap();
            assert_eq!(
                window.children().len(),
                2,
                "window should own Group + reparented Label"
            );

            let group_id = window
                .children()
                .iter()
                .copied()
                .find(|cid| {
                    update
                        .nodes
                        .iter()
                        .any(|(id, n)| id == cid && n.role() == AkRole::Group)
                })
                .expect("group child of window");
            let (_, group_node) = update.nodes.iter().find(|(id, _)| *id == group_id).unwrap();
            assert_eq!(group_node.children().len(), 1, "group owns the button");
        }

        {
            let row = |pos: usize, size: usize| {
                AccessNode::new(Role::Row)
                    .with_position(pos, size)
                    .with_children(vec![
                        AccessNode::new(Role::Cell)
                            .with_name("Name")
                            .with_position(1, 2),
                        AccessNode::new(Role::Cell)
                            .with_name("Age")
                            .with_position(2, 2),
                    ])
            };
            let table_access = AccessNode::new(Role::Table)
                .with_name("People")
                .with_children(vec![row(1, 2), row(2, 2)]);
            let mut root = TestWidget::new(Some(table_access));

            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let (_, table) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::Table)
                .expect("table node present");
            assert_eq!(table.children().len(), 2, "table owns two synthetic rows");

            // Rows carry their 1-based position and own two header-named cells.
            let row_nodes: Vec<_> = update
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == AkRole::Row)
                .collect();
            assert_eq!(row_nodes.len(), 2, "two row nodes reached the tree");
            for (_, r) in &row_nodes {
                assert_eq!(r.size_of_set(), Some(2), "row knows the table has 2 rows");
                assert!(
                    r.position_in_set().is_some(),
                    "row carries its 1-based position"
                );
                assert_eq!(r.children().len(), 2, "each row owns two cells");
            }

            let cell_labels: Vec<Option<&str>> = update
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == AkRole::Cell)
                .map(|(_, n)| n.label())
                .collect();
            assert!(
                cell_labels.contains(&Some("Name")),
                "a cell is named for the Name column"
            );
            assert!(
                cell_labels.contains(&Some("Age")),
                "a cell is named for the Age column"
            );
            assert_eq!(cell_labels.len(), 4, "two cells per row across two rows");
        }

        {
            let polite = TestWidget::new(Some(
                AccessNode::new(Role::StaticText)
                    .with_name("Saved")
                    .with_live(tilo_ui_base::a11y::Politeness::Polite),
            ));
            let assertive = TestWidget::new(Some(
                AccessNode::new(Role::StaticText)
                    .with_name("Connection lost")
                    .with_live(tilo_ui_base::a11y::Politeness::Assertive),
            ));
            let mut root = TestWidget::new(None)
                .with_child(Box::new(polite))
                .with_child(Box::new(assertive));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let node_named = |name: &str| {
                update
                    .nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some(name))
                    .map(|(_, node)| node)
                    .expect("live region is exported")
            };
            let polite = node_named("Saved");
            assert_eq!(polite.live(), Some(AkLive::Polite));
            assert!(polite.is_live_atomic());
            let assertive = node_named("Connection lost");
            assert_eq!(assertive.live(), Some(AkLive::Assertive));
            assert!(assertive.is_live_atomic());
        }
    }

    phase text_values_expose_editable_runs_but_never_protected_content;
    {
        {
            let terminal = TestWidget::new(Some(
                AccessNode::new(Role::Terminal)
                    .with_name("Terminal")
                    .with_value("$ cargo test")
                    .with_state(AccessState::new().focusable(true).read_only(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(terminal));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::Terminal)
                .expect("terminal reaches AccessKit with its native role");
            assert!(node.is_read_only());
            assert!(node.supports_action(AkAction::Focus));
            assert!(node.supports_action(AkAction::SetTextSelection));
            assert!(!node.supports_action(AkAction::SetValue));
            assert_eq!(node.value(), Some("$ cargo test"));
            let run_id = *node
                .children()
                .first()
                .expect("unselected terminal has a readable text run");
            let run = update
                .nodes
                .iter()
                .find(|(id, _)| *id == run_id)
                .expect("terminal text run reaches the platform tree");
            assert_eq!(run.1.role(), AkRole::TextRun);
            assert_eq!(run.1.value(), Some("$ cargo test"));
            assert_eq!(run.1.character_lengths().len(), 12);
            let selection = node
                .text_selection()
                .expect("unselected transcript has a collapsed caret");
            assert_eq!(selection.anchor.node, run_id);
            assert_eq!(selection.anchor.character_index, 0);
            assert_eq!(selection.focus, selection.anchor);
        }

        {
            let pw = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_name("Protected input")
                    .with_value("hunter2")
                    .with_state(AccessState::new().protected(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(pw));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.label() == Some("Protected input"))
                .unwrap();
            assert_eq!(node.role(), AkRole::PasswordInput);
            assert_eq!(
                node.value(),
                None,
                "protected value must not leak to the AT"
            );
        }

        {
            // AccessKit's AT-SPI adapter offers Text and EditableText only on a
            // text input with a text run; without them a screen reader cannot
            // focus a password as text or type into it. The run never has
            // the value, its length, or a caret that moved with typing.
            for (role, secret) in [
                (Role::TextInput, ""),
                (Role::TextInput, "hunter2"),
                (Role::MultilineTextInput, "a much longer password"),
            ] {
                let pw = TestWidget::new(Some(
                    AccessNode::new(role)
                        .with_name("Passphrase")
                        .with_value(secret)
                        .with_text_selection(secret.len(), secret.len())
                        .with_state(AccessState::new().focusable(true).protected(true)),
                ));
                let mut root = TestWidget::new(None).with_child(Box::new(pw));
                let (_taffy, node_map, _root_node) = lay_out(&mut root);
                let update = build_tree_update(&root, &node_map, None, 1.0);
                let (field_id, field) = update
                    .nodes
                    .iter()
                    .find(|(_, n)| n.label() == Some("Passphrase"))
                    .unwrap();
                assert_eq!(field.role(), AkRole::PasswordInput);
                assert!(field.supports_action(AkAction::SetValue));
                let [run_id] = field.children() else {
                    panic!("one text run: {:?}", field.children());
                };
                let (_, run) = update.nodes.iter().find(|(id, _)| id == run_id).unwrap();
                assert_eq!(run.role(), AkRole::TextRun);
                assert_eq!(run.value(), Some(""));
                assert!(run.character_lengths().is_empty());
                let selection = field.text_selection().unwrap();
                assert_eq!(
                    (selection.anchor, selection.focus),
                    (
                        AkTextPosition {
                            node: *run_id,
                            character_index: 0
                        },
                        AkTextPosition {
                            node: *run_id,
                            character_index: 0
                        }
                    )
                );
                assert_eq!(
                    resolve_node_id(&root, &node_map, *run_id),
                    resolve_node_id(&root, &node_map, *field_id),
                    "a verb aimed at the run reaches the field"
                );
            }
        }

        {
            let mut root =
                TestWidget::new(Some(AccessNode::new(Role::Dialog).with_children(vec![
                    AccessNode::new(Role::TextInput)
                        .with_name("Sheet password")
                        .with_action_target("sheet:password")
                        .with_state(AccessState::new().protected(true)),
                ])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let field_id = label_id(&update, "Sheet password");
            let field = &update
                .nodes
                .iter()
                .find(|(id, _)| *id == field_id)
                .unwrap()
                .1;
            assert_eq!(field.role(), AkRole::PasswordInput);
            let [run_id] = field.children() else {
                panic!("one text run: {:?}", field.children());
            };
            let (_, run) = update.nodes.iter().find(|(id, _)| id == run_id).unwrap();
            assert_eq!(run.value(), Some(""));
            assert!(matches!(
                resolve_node_id(&root, &node_map, *run_id),
                Some(A11yTarget::Synthetic { ref semantic_target, .. })
                    if semantic_target.as_deref() == Some("sheet:password")
            ));
        }

        {
            let mut root = TestWidget::new(Some(
                AccessNode::new(Role::Label)
                    .with_name("Recovery key")
                    .with_state(AccessState::new().protected(true)),
            ));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert!(
                update
                    .nodes
                    .iter()
                    .all(|(_, node)| node.role() != AkRole::TextRun)
            );
        }

        {
            let family = "👩‍👩‍👧‍👦";
            let value = format!("a{family}b");
            let field = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_value(value.clone())
                    .with_text_selection(1 + family.len(), 1)
                    .with_state(AccessState::new().focusable(true).focused(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(field));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (field_id, field_node) = update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::TextInput)
                .unwrap();
            let (run_id, run) = update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::TextRun)
                .unwrap();

            assert_eq!(run.value(), Some(value.as_str()));
            assert_eq!(
                run.character_lengths(),
                [1, u8::try_from(family.len()).unwrap_or(u8::MAX), 1].as_slice(),
                "AccessKit characters match the editor's extended graphemes"
            );
            assert_eq!(field_node.children(), &[*run_id]);
            let selection = field_node.text_selection().unwrap();
            assert_eq!(selection.anchor.node, *run_id);
            assert_eq!(selection.anchor.character_index, 2);
            assert_eq!(selection.focus.node, *run_id);
            assert_eq!(selection.focus.character_index, 1);
            assert_ne!(*field_id, *run_id);
        }
    }

    phase a_table_rows_press_and_menu_reach_an_assistive_technology;
    {
        let row = TestWidget::new(Some(
            AccessNode::new(Role::Row)
                .with_name("Quarterly report.pdf")
                .with_action_target("row:a:0")
                .with_bounds(vello::kurbo::Rect::new(0.0, 0.0, 200.0, 24.0))
                .with_state(AccessState::new().focusable(true).context_menu(true)),
        ))
        .with_rect(0.0, 0.0, 200.0, 24.0);
        let mut root = TestWidget::new(None).with_child(Box::new(row));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Row)
            .expect("a row reaches AccessKit");
        assert_eq!(node.label(), Some("Quarterly report.pdf".into()));
        assert!(
            node.supports_action(AkAction::Click),
            "the row's activation is offered"
        );
        assert!(
            node.supports_action(AkAction::ShowContextMenu),
            "the row's menu is offered"
        );

        let inert = TestWidget::new(Some(
            AccessNode::new(Role::Row)
                .with_name("Budget")
                .with_bounds(vello::kurbo::Rect::new(0.0, 0.0, 200.0, 24.0)),
        ))
        .with_rect(0.0, 0.0, 200.0, 24.0);
        let mut root = TestWidget::new(None).with_child(Box::new(inert));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Row)
            .expect("a row reaches AccessKit");
        assert!(
            !node.supports_action(AkAction::Click),
            "a row nothing can reach is not offered a press"
        );
    }
}

tilo_test_env::scenario! {
    fn accesskit_focus_follows_the_focused_widget_or_marked_node;

    phase focus_targets_the_focused_widget_or_falls_back_to_root;
    {
        {
            let btn = TestWidget::new(Some(
                AccessNode::new(Role::Button)
                    .with_name("Save")
                    .with_state(AccessState::new().focusable(true).focused(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(btn));

            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let focused_taffy = node_map
                .iter()
                .find(|(_, ptr)| {
                    let w = unsafe { &***ptr };
                    w.accessibility().is_some_and(|n| n.role == Role::Button)
                })
                .map(|(&id, _)| id);
            assert!(focused_taffy.is_some());

            let update = build_tree_update(&root, &node_map, focused_taffy, 1.0);
            assert_ne!(
                update.focus, ROOT_ID,
                "focus should be the button, not root"
            );
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(id, n)| *id == update.focus && n.role() == AkRole::Button),
                "focus must point at the Button node"
            );
        }

        {
            let mut root = TestWidget::new(Some(AccessNode::new(Role::Document)));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert_eq!(update.focus, ROOT_ID);
        }

        {
            // A stale taffy id (left over from before a relayout) must not be
            // forwarded — AccessKit panics the AT if `focus` isn't emitted.
            let btn = TestWidget::new(Some(
                AccessNode::new(Role::Button)
                    .with_name("Save")
                    .with_state(AccessState::new().focusable(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(btn));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let stale = taffy::NodeId::from(u64::MAX - 7);
            let update = build_tree_update(&root, &node_map, Some(stale), 1.0);
            assert_eq!(update.focus, ROOT_ID);
            assert!(update.nodes.iter().any(|(id, _)| *id == update.focus));
        }

        {
            // The other half of the emitted-node rule the active-descendant
            // preference is layered on: a live taffy id whose widget is
            // presentational emits no node at all, so forwarding it would name a
            // node absent from this update — which panics the consuming AT.
            let inner = TestWidget::new(None).with_child(Box::new(TestWidget::new(Some(
                AccessNode::new(Role::Button).with_name("Go"),
            ))));
            let mut root = TestWidget::new(None).with_child(Box::new(inner));
            let (_taffy, node_map, _n) = lay_out(&mut root);

            // The presentational wrapper: in the node_map, but never exported.
            let collapsed = node_map
                .iter()
                .find(|(_, ptr)| unsafe { &***ptr }.accessibility().is_none())
                .map(|(&id, _)| id)
                .expect("a presentational widget was laid out");

            let update = build_tree_update(&root, &node_map, Some(collapsed), 1.0);
            assert_eq!(update.focus, ROOT_ID);
            assert!(update.nodes.iter().any(|(id, _)| *id == update.focus));
        }
    }

    phase focus_and_active_descendant_follow_the_marked_synthetic_node;
    {
        {
            // A canvas widget has keyboard focus and nominates its second clip as
            // the active descendant. The reader must land on THAT clip — a canvas
            // whose focus can only ever be the whole canvas is precisely why a
            // screen-reader user cannot navigate one.
            let mut root = TestWidget::new(Some(canvas(vec![
                clip("Intro", false),
                clip("Interview", true),
                clip("Outro", false),
            ])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let canvas_taffy = taffy_id_of(&node_map, Role::Canvas);

            let update = build_tree_update(&root, &node_map, Some(canvas_taffy), 1.0);

            let canvas_ak = widget_ak_id(&node_map, canvas_taffy).unwrap();
            assert_ne!(
                update.focus, canvas_ak,
                "focus must not fall back to the whole canvas when a descendant is marked"
            );
            assert_ne!(update.focus, ROOT_ID);
            let (_, focused) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .expect("the focused id must name a node in this very update");
            assert_eq!(
                focused.label(),
                Some("Interview"),
                "focus must be the clip the widget marked, not one of its siblings"
            );
            assert!(
                is_synthetic_id(update.focus),
                "the focused node is synthetic, so its id must live in the synthetic id space"
            );
        }

        {
            let idle = TestWidget::new(Some(canvas(vec![clip("Ghost", true)])));
            let btn = TestWidget::new(Some(
                AccessNode::new(Role::Button)
                    .with_name("Save")
                    .with_state(AccessState::new().focusable(true)),
            ));
            let mut root = TestWidget::new(None)
                .with_child(Box::new(idle))
                .with_child(Box::new(btn));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let button_taffy = taffy_id_of(&node_map, Role::Button);

            let update = build_tree_update(&root, &node_map, Some(button_taffy), 1.0);

            assert_eq!(
                update.focus,
                widget_ak_id(&node_map, button_taffy).unwrap(),
                "focus stays on the widget that holds the keyboard"
            );
            let (_, focused) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .unwrap();
            assert_eq!(focused.label(), Some("Save"));
        }

        {
            // A widget that marks two nodes focused is buggy, but the reader must
            // still land somewhere deterministic rather than on whichever node the
            // walk happened to reach last.
            let mut root = TestWidget::new(Some(canvas(vec![
                clip("Intro", false),
                clip("Interview", true),
                clip("Outro", true),
            ])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let canvas_taffy = taffy_id_of(&node_map, Role::Canvas);

            let update = build_tree_update(&root, &node_map, Some(canvas_taffy), 1.0);
            let (_, focused) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .expect("the focused id must name a node in this very update");
            assert_eq!(focused.label(), Some("Interview"));
        }

        {
            let mut root = TestWidget::new(Some(
                AccessNode::new(Role::Canvas)
                    .with_name("Timeline")
                    .with_state(AccessState::new().focusable(true))
                    .with_children(vec![
                        AccessNode::new(Role::Group)
                            .with_name("Track 1")
                            .with_children(vec![clip("Intro", false), clip("Interview", true)]),
                        AccessNode::new(Role::Group).with_name("Track 2"),
                    ]),
            ));
            // Second clip of the first track: [track 0, clip 1].
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let canvas_taffy = taffy_id_of(&node_map, Role::Canvas);
            let update = build_tree_update(&root, &node_map, Some(canvas_taffy), 1.0);
            assert_eq!(
                resolve_node_id(&root, &node_map, update.focus),
                Some(A11yTarget::Synthetic {
                    owner: canvas_taffy,
                    key_path: vec![0, 1],
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![0, 1],
                    semantic_target: None,
                }),
                "the exported focus resolves to the marked clip"
            );
        }
    }
}

#[cfg(windows)]
#[test]
fn native_password_entry_uses_only_a_fixed_empty_placeholder() {
    for secret in ["", "hunter2", "a much longer password"] {
        let mut root = TestWidget::new(Some(
            AccessNode::new(Role::TextInput)
                .with_name("Secret")
                .with_value(secret)
                .with_state(AccessState::new().protected(true)),
        ));
        let (_, node_map, _) = lay_out(&mut root);
        let semantic = build_tree_update(&root, &node_map, None, 1.0);
        assert!(
            semantic
                .nodes
                .iter()
                .all(|(_, node)| node.value().is_none())
        );
        let native = native_tree_update(semantic);
        let (_, password) = native
            .nodes
            .iter()
            .find(|(_, node)| node.role() == AkRole::PasswordInput)
            .unwrap();
        assert_eq!(password.value(), Some(""));
        assert!(password.supports_action(AkAction::SetValue));
        assert!(password.children().iter().all(|child| {
            native
                .nodes
                .iter()
                .any(|(id, node)| id == child && node.value() == Some(""))
        }));
    }
}

fn canvas(children: Vec<AccessNode>) -> AccessNode {
    AccessNode::new(Role::Canvas)
        .with_name("Timeline")
        .with_state(AccessState::new().focusable(true))
        .with_children(children)
}

fn clip(name: &str, focused: bool) -> AccessNode {
    AccessNode::new(Role::ListItem)
        .with_name(name)
        .with_state(AccessState::new().focusable(true).focused(focused))
}

fn taffy_id_of(
    node_map: &HashMap<taffy::NodeId, *mut dyn Widget>,
    role: Role,
) -> taffy::NodeId {
    node_map
        .iter()
        .find(|(_, ptr)| {
            let w = unsafe { &***ptr };
            w.accessibility().is_some_and(|n| n.role == role)
        })
        .map(|(&id, _)| id)
        .expect("a widget with that role was laid out")
}

fn label_id(update: &TreeUpdate, label: &str) -> AkNodeId {
    update
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some(label))
        .map_or_else(
            || panic!("no node labelled {label} in the update"),
            |(id, _)| *id,
        )
}
