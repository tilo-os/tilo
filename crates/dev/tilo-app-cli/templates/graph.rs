use ui::tilo_actions;
use ui::widget::{Button, Text, TextInput, Widget, labeled_field};
use ui::{LocalState, ModelView, Theme};

fn project_resource() -> tilo_actions::Resource {
    let draft = tilo_actions::ActionSpec::typed_operation(
        "set-draft",
        "Set project draft",
        "Enter the project name",
        tilo_actions::OperationKind::Effect,
        ui::widget::resource_actions::OPERATION_TYPE,
        vec![tilo_actions::ActionParam::required_owned(
            "draft",
            tilo_actions::ParamType::Text,
            "Project name draft",
        )],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                ui::widget::resource_actions::OPERATION_TYPE,
                &args.to_json(),
            )
            .expect("generated project draft is serializable")
        },
    );
    let mut resource = ui::widget::resource_actions::typed_resource(
        "generated.graph-project",
        "Project",
        "create",
        "Create project",
        vec![tilo_actions::ActionParam::required_owned(
            "name",
            tilo_actions::ParamType::Text,
            "Project name",
        )],
    );
    if let Some(action) = resource.actions.first_mut() {
        action.contract.result = tilo_actions::ResultSpec::Resource {
            resource_kind: "generated.graph-project".into(),
        };
    }
    resource.actions.insert(0, draft);
    resource
}

fn create_project(name: &str) -> tilo_actions::ActionOutput {
    let stable_name = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    tilo_actions::ActionOutput {
        value: None,
        resources: vec![format!("generated.graph-project:{stable_name}")],
    }
}

/// The graph is validated before any widget reaches the app tree.
#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let root: Box<dyn Widget> = match crate::screens::router_with(
        "main",
        &Theme::default(),
        crate::components::ui_graph_context(),
    ) {
        Ok(router) => Box::new(router),
        Err(error) => Box::new(Text::new(&format!(
            "Unable to load the generated UI: {error}"
        ))),
    };
    let resource = project_resource();
    let draft = LocalState::new(String::new());
    let draft_binding = tilo_actions::ActionBinding::new(
        &resource,
        resource
            .actions
            .iter()
            .find(|action| action.id == "set-draft")
            .expect("generated graph resource exports set-draft"),
        |value| serde_json::Map::from_iter([("draft".into(), serde_json::json!(value))]),
    )
    .expect("project input produces valid draft arguments");
    let create_draft = draft.clone();
    let create = tilo_actions::ActionBinding::new(
        &resource,
        resource
            .actions
            .iter()
            .find(|action| action.id == "create")
            .expect("generated graph resource exports create"),
        move |()| {
            serde_json::Map::from_iter([("name".into(), serde_json::json!(create_draft.get()))])
        },
    )
    .expect("generated project draft matches the exported create action");
    let input = TextInput::from_state(&draft)
        .with_id("name-input")
        .with_placeholder("graph-project-name")
        .on_action(draft_binding);
    let preview = ModelView::from_state(&draft, |value| {
        Box::new(Text::new(value).with_id("name-preview"))
    });
    let root = Box::new(
        ui::widget::Column::new()
            .push(root)
            .push(Box::new(labeled_field("graph-project-name", input)))
            .push(Box::new(preview))
            .push(Box::new(
                Button::new("create-action", "graph-create", None).on_action(create),
            )),
    );
    ui::widget::resource_actions::immediate(root, resource, move |payload| {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        if let Some(value) = args.get("draft").and_then(serde_json::Value::as_str) {
            draft.set(value.to_owned());
            return Ok(tilo_actions::ActionOutput::default());
        }
        let name = args
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing project name")?;
        Ok(create_project(name))
    })
}
