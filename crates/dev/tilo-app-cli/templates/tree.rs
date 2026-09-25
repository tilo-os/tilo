use ui::prelude::*;

/// The stable node ids of the hierarchy this template browses.
const NODES: [u64; 6] = [1, 2, 3, 4, 5, 6];

/// The app's own state: which nodes the user pinned. Stored, synced and
/// undoable; the hierarchy itself is the data being browsed.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Browser {
    /// Pinned node ids, in the order they were pinned.
    pub pinned: Vec<u64>,
}

#[resource(kind = "generated.tree", label = "Workspace")]
impl Browser {
    /// Pin a node, or unpin it when it is already pinned.
    #[action]
    pub fn toggle_pin(
        &mut self,
        #[param("Stable tree node id")] id: u64,
    ) -> Result<(), &'static str> {
        if !NODES.contains(&id) {
            return Err("no such node");
        }
        if self.pinned.contains(&id) {
            self.pinned.retain(|pinned| *pinned != id);
        } else {
            self.pinned.push(id);
        }
        Ok(())
    }
}

fn hierarchy() -> TreeNode<()> {
    TreeNode::branch(
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
    )
}

/// A stable-id hierarchy with expansion, keyboard navigation, selection,
/// accessibility, virtualization, reordering hooks, and structural motion.
pub struct App;

impl ResourceApp for App {
    type Resource = Browser;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Browser>) -> Element {
        crate::install_catalog();
        // The tree builds a row whenever one scrolls into view; each row binds
        // its button through the resource's actions.
        let actions = cx.actions();
        let tree = TreeView::new(hierarchy())
            .with_id("workspace-tree")
            .with_expanded([1, 2])
            .multi_select(true)
            .with_row_delegate(move |model, id, _depth| {
                let label = model.find(id).map_or("", |node| node.label.as_str());
                Box::new(ui::row![
                    Text::new(label),
                    actions.bind_invocation(
                        Button::new(&format!("more-{id}"), "tree-row-action", None)
                            .fit_content(32.0),
                        Browser::TOGGLE_PIN.with(id),
                    ),
                ])
            })
            .reorderable()
            .with_viewport(280.0, 36.0)
            .with_accessible_name("tree-accessible-name");
        let pinned = cx
            .text(|browser| {
                let count = browser.pinned.len().to_string();
                ui::i18n::I18nManager::format("tree-pinned", &[("count", count.as_str().into())])
            })
            .with_id("pinned-count");
        Box::new(ui::column![tree, pinned])
    }
}
