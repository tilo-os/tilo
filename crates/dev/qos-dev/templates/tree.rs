use ui::widget::{Button, Text, TreeNode, TreeView, Widget};

fn open_node(_id: u64) {}

/// A stable-id hierarchy with expansion, keyboard navigation, selection,
/// accessibility, virtualization, reordering hooks, and structural motion.
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
                Box::new(ui::row![
                    Text::new(label),
                    Button::new(
                        &format!("more-{id}"),
                        "tree-row-action",
                        Some("tree-row-action-requested"),
                    )
                    .with_click_value(id.to_string())
                    .on_click(move || open_node(id))
                    .fit_content(32.0),
                ])
            })
            .reorderable()
            .with_viewport(280.0, 36.0)
            .with_accessible_name("tree-accessible-name"),
    );
    crate::resource_actions::immediate(
        root,
        crate::resource_actions::typed_resource(
            "generated.tree",
            "Workspace",
            "open",
            "Open node",
            vec![qos_actions::ActionParam::required_owned(
                "id", qos_actions::ParamType::Integer, "Stable tree node id",
            )],
        ),
        |payload| {
            let args: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            let id = args.get("id").and_then(serde_json::Value::as_u64).ok_or("missing id")?;
            open_node(id);
            Ok(Default::default())
        },
    )
}
