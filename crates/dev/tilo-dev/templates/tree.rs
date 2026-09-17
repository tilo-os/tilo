use ui::tilo_actions;
use ui::widget::{Button, Text, TreeNode, TreeView, Widget};

fn node_resource() -> tilo_actions::Resource {
    let mut resource = ui::widget::resource_actions::typed_resource(
        "generated.tree",
        "Workspace",
        "open",
        "Open node",
        vec![tilo_actions::ActionParam::required_owned(
            "id",
            tilo_actions::ParamType::Integer,
            "Stable tree node id",
        )],
    );
    if let Some(action) = resource.actions.first_mut() {
        action.contract.result = tilo_actions::ResultSpec::Resource {
            resource_kind: "generated.tree.node".into(),
        };
    }
    resource
}

fn open_node(id: u64) -> tilo_actions::ActionOutput {
    tilo_actions::ActionOutput {
        value: None,
        resources: vec![format!("generated.tree.node:{id}")],
    }
}

/// A stable-id hierarchy with expansion, keyboard navigation, selection,
/// accessibility, virtualization, reordering hooks, and structural motion.
#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let root = TreeNode::branch(
        1,
        "Workspace",
        (),
        vec![
            TreeNode::branch(
                2,
                "Projects",
                (),
                vec![
                    TreeNode::leaf(3, "Desktop app", ()),
                    TreeNode::leaf(4, "Design system", ()),
                ],
            ),
            TreeNode::branch(
                5,
                "Archive",
                (),
                vec![TreeNode::leaf(6, "First prototype", ())],
            ),
        ],
    );
    let root = Box::new(
        TreeView::new(root)
            .with_id("workspace-tree")
            .with_expanded([1, 2])
            .multi_select(true)
            .with_row_delegate(|model, id, _depth| {
                let label = model.find(id).map_or("", |node| node.label.as_str());
                let resource = node_resource();
                let binding = tilo_actions::ActionBinding::static_json_for(
                    &resource,
                    "open",
                    serde_json::Map::from_iter([("id".into(), serde_json::json!(id))]),
                )
                .expect("generated node id matches the exported action");
                Box::new(ui::row![
                    Text::new(label),
                    Button::new(&format!("more-{id}"), "tree-row-action", None)
                        .on_action(binding)
                        .fit_content(32.0),
                ])
            })
            .reorderable()
            .with_viewport(280.0, 36.0)
            .with_accessible_name("tree-accessible-name"),
    );
    ui::widget::resource_actions::immediate(root, node_resource(), |payload| {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let id = args
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing id")?;
        Ok(open_node(id))
    })
}
