use ui::LocalState;
use ui::qos_actions;
use ui::tokens::TextRole;
use ui::widget::{
    AppShell, Button, ModelView, NarrowScreen, Text, TextInput, TreeNode, TreeView, Widget,
    labeled_field,
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

fn share_item(_id: u64) {}

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
        Button::new("workspace-share", "workspace-share", None)
            .on_click(move || share_item(item_id)),
    ])
}

/// A responsive professional master-detail shell with durable per-item editing.
///
/// Stable tree selection switches one retained detail pane through typed local
/// state. Each detail form binds directly to its own Automerge paths, so
/// persistence, collaboration, undo, inspection, and multiple synchronized
/// views require no app-authored notification layer.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let selected = LocalState::new(3_u64);
    let narrow_screen = LocalState::new(NarrowScreen::Content);
    let selected_from_tree = selected.clone();
    let screen_from_tree = narrow_screen.clone();
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
    .on_select(move |id| {
        selected_from_tree.set(id);
        screen_from_tree.set(NarrowScreen::Content);
    })
    .with_viewport(560.0, 36.0)
    .with_accessible_name("workspace-tree-name");
    let master = ui::column![
        Text::new("workspace-navigation-heading").with_role(TextRole::Heading),
        tree,
    ]
    .with_fill_remaining_height();
    let detail = ModelView::from_state(&selected, |item_id| detail_pane(*item_id));

    let root = Box::new(
        AppShell::new(Box::new(master), Box::new(detail))
            .with_id("workspace-shell")
            .with_back_accessible_name("workspace-back")
            .with_narrow_screen_state(&narrow_screen),
    );
    ui::widget::resource_actions::immediate(
        root,
        ui::widget::resource_actions::typed_resource(
            "generated.workspace-item",
            "Workspace item",
            "share",
            "Share item",
            vec![qos_actions::ActionParam::required_owned(
                "id",
                qos_actions::ParamType::Integer,
                "Stable item id",
            )],
        ),
        |payload| {
            let args: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            let id = args
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .ok_or("missing item id")?;
            share_item(id);
            Ok(qos_actions::ActionOutput::default())
        },
    )
}
