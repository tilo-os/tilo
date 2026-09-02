use ui::Theme;
use ui::qos_actions;
use ui::widget::{Button, Text, Widget};

fn project_resource() -> qos_actions::Resource {
    let mut resource = ui::widget::resource_actions::typed_resource(
        "generated.graph-project",
        "Project",
        "create",
        "Create project",
        vec![qos_actions::ActionParam::required_owned(
            "name",
            qos_actions::ParamType::Text,
            "Project name",
        )],
    );
    if let Some(action) = resource.actions.first_mut() {
        action.contract.result = qos_actions::ResultSpec::Resource {
            resource_kind: "generated.graph-project".into(),
        };
    }
    resource
}

fn create_project(name: &str) -> qos_actions::ActionOutput {
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
    qos_actions::ActionOutput {
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
    let create = qos_actions::ActionBinding::static_json_for(
        &resource,
        "create",
        serde_json::Map::from_iter([("name".into(), serde_json::json!("New project"))]),
    )
    .expect("generated project name matches the exported create action");
    let root = Box::new(
        ui::widget::Column::new()
            .push(root)
            .push(Box::new(
                Button::new("create-action", "graph-create", None).on_action(create),
            )),
    );
    ui::widget::resource_actions::immediate(
        root,
        resource,
        |payload| {
            let args: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            let name = args
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing project name")?;
            Ok(create_project(name))
        },
    )
}
