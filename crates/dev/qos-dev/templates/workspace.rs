use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{
    Button, ModelView, ResizableSplit, Text, TextInput, TreeNode, TreeView, Widget, labeled_field,
};

fn item_label(id: u64) -> String {
    let message = match id {
        3 => "workspace-desktop-app",
        4 => "workspace-design-system",
        6 => "workspace-first-prototype",
        _ => "workspace-unknown-item",
    };
    ui::i18n::I18nManager::lookup_plain(message)
}

fn detail_pane(item_id: u64) -> Box<dyn Widget> {
    let title_path = format!("workspace-item-{item_id}-title");
    let notes_path = format!("workspace-item-{item_id}-notes");
    let title = TextInput::from_document_string(&title_path)
        .with_id("workspace-title-input")
        .with_placeholder("workspace-title-placeholder");
    let notes = TextInput::from_document_string(&notes_path)
        .with_id("workspace-notes-input")
        .with_placeholder("workspace-notes-placeholder")
        .multiline(true);

    Box::new(ui::column![
        Text::new(&item_label(item_id))
            .with_id("workspace-selected-item")
            .with_role(TextRole::Title),
        labeled_field("workspace-title-label", title),
        labeled_field("workspace-notes-label", notes),
        Text::new("workspace-preview-heading").with_role(TextRole::Heading),
        Text::from_document_string(&title_path)
            .with_id("workspace-title-preview")
            .with_role(TextRole::Heading),
        Text::from_document_string(&notes_path).with_id("workspace-notes-preview"),
        Button::new(
            "workspace-share",
            "workspace-share",
            Some("workspace-share-requested"),
        )
        .with_click_value(item_id.to_string()),
    ])
}

/// A professional master-detail shell with durable per-item editing.
///
/// Stable tree selection switches one retained detail pane through typed local
/// state. Each detail form binds directly to its own Automerge paths, so
/// persistence, collaboration, undo, inspection, and multiple synchronized
/// views require no app-authored notification layer.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let selected = LocalState::new(3_u64);
    let selected_from_tree = selected.clone();
    let label = |message| ui::i18n::I18nManager::lookup_plain(message);
    let tree = TreeView::new(TreeNode::branch(
        1,
        label("workspace-root"),
        (),
        vec![
            TreeNode::branch(
                2,
                label("workspace-projects"),
                (),
                vec![
                    TreeNode::leaf(3, label("workspace-desktop-app"), ()),
                    TreeNode::leaf(4, label("workspace-design-system"), ()),
                ],
            ),
            TreeNode::branch(
                5,
                label("workspace-archive"),
                (),
                vec![TreeNode::leaf(6, label("workspace-first-prototype"), ())],
            ),
        ],
    ))
    .with_id("workspace-tree")
    .with_expanded([1, 2])
    .with_selected(Some(3))
    .on_select(move |id| selected_from_tree.set(id))
    .with_viewport(560.0, 36.0)
    .with_accessible_name("workspace-tree-name");
    let master = ui::column![
        Text::new("workspace-navigation-heading").with_role(TextRole::Heading),
        tree,
    ]
    .with_fill_remaining_height();
    let detail = ModelView::from_state(&selected, |item_id| detail_pane(*item_id));

    Box::new(
        ResizableSplit::new(master, detail, SplitOrientation::Horizontal).with_split_position(0.32),
    )
}
