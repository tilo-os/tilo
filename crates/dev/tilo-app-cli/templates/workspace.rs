use serde::{Deserialize, Serialize};
use ui::prelude::{
    ActionError, ActionRegistry, AppShell, Button, Doc, ExecutionLocus, LocalState, ModelView,
    NarrowScreen, Text, TextInput, TextRole, Transactable, TreeNode, TreeView, Widget,
    labeled_field, tilo_actions,
};

const WORKSPACE_ACTION_TYPE: &str = "generated.workspace.action";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorkspaceAction {
    SetTitle { id: u64, value: String },
    SetNotes { id: u64, value: String },
}

impl tilo_actions::Action for WorkspaceAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::SetTitle { id, value } => {
                doc.put(
                    ui::automerge::ROOT,
                    format!("workspace-item-{id}-title"),
                    value,
                )?;
            }
            Self::SetNotes { id, value } => {
                doc.put(
                    ui::automerge::ROOT,
                    format!("workspace-item-{id}-notes"),
                    value,
                )?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

pub fn register_actions(registry: &mut ActionRegistry) {
    registry.register::<WorkspaceAction>(WORKSPACE_ACTION_TYPE);
}

fn item_label(id: u64) -> String {
    let message = match id {
        3 => "workspace-desktop-app",
        4 => "workspace-design-system",
        6 => "workspace-first-prototype",
        _ => "workspace-unknown-item",
    };
    ui::i18n::I18nManager::lookup_plain(message)
}

#[expect(
    clippy::expect_used,
    reason = "the generated workspace action contains only serializable fields"
)]
fn item_resource() -> tilo_actions::Resource {
    let mut resource = ui::widget::resource_actions::typed_resource(
        "generated.workspace-item",
        "Workspace item",
        "share",
        "Share item",
        vec![tilo_actions::ActionParam::required_owned(
            "id",
            tilo_actions::ParamType::Integer,
            "Stable item id",
        )],
    );
    if let Some(action) = resource.actions.first_mut() {
        action.contract.result = tilo_actions::ResultSpec::Resource {
            resource_kind: "generated.workspace-item".into(),
        };
    }
    for (id, label, field) in [
        ("set-title", "Set item title", "title"),
        ("set-notes", "Set item notes", "notes"),
    ] {
        resource = resource.with_action(tilo_actions::ActionSpec::typed(
            id,
            label,
            "Enter the new value",
            ExecutionLocus::ClientLocal,
            vec![
                tilo_actions::ActionParam::required_owned(
                    "id",
                    tilo_actions::ParamType::Integer,
                    "Stable item id",
                ),
                tilo_actions::ActionParam::required_owned(
                    "value",
                    tilo_actions::ParamType::Text,
                    "Field value",
                ),
            ],
            move |args| {
                let id = args
                    .integer("id")
                    .unwrap_or_default()
                    .try_into()
                    .unwrap_or_default();
                let value = args.text("value").unwrap_or_default().to_owned();
                let action = if field == "title" {
                    WorkspaceAction::SetTitle { id, value }
                } else {
                    WorkspaceAction::SetNotes { id, value }
                };
                tilo_actions::EncodedAction::encode(WORKSPACE_ACTION_TYPE, &action)
                    .expect("generated workspace field action is serializable")
            },
        ));
    }
    resource
}

fn share_item(id: u64) -> tilo_actions::ActionOutput {
    tilo_actions::ActionOutput {
        value: None,
        resources: vec![format!("generated.workspace-item:{id}/share")],
    }
}

#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
fn detail_pane(item_id: u64) -> Box<dyn Widget> {
    let title_path = format!("workspace-item-{item_id}-title");
    let notes_path = format!("workspace-item-{item_id}-notes");
    let resource = item_resource();
    let field_binding = |action_id: &str| {
        tilo_actions::ActionBinding::new(
            &resource,
            resource
                .actions
                .iter()
                .find(|action| action.id == action_id)
                .expect("generated workspace resource exports its field action"),
            move |value| {
                serde_json::Map::from_iter([
                    ("id".into(), serde_json::json!(item_id)),
                    ("value".into(), serde_json::json!(value)),
                ])
            },
        )
        .expect("workspace field produces valid action arguments")
    };
    let title = TextInput::from_document_string(&title_path)
        .with_id("workspace-title-input")
        .with_placeholder("workspace-title-placeholder")
        .on_action(field_binding("set-title"));
    let notes = TextInput::from_document_string(&notes_path)
        .with_id("workspace-notes-input")
        .with_placeholder("workspace-notes-placeholder")
        .multiline(true)
        .on_action(field_binding("set-notes"));
    let share_binding = tilo_actions::ActionBinding::static_json_for(
        &resource,
        "share",
        serde_json::Map::from_iter([("id".into(), serde_json::json!(item_id))]),
    )
    .expect("generated item id matches the exported share action");

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
        Button::new("workspace-share", "workspace-share", None).on_action(share_binding),
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
    ui::widget::resource_actions::immediate(root, item_resource(), |payload| {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let id = args
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing item id")?;
        Ok(share_item(id))
    })
}
