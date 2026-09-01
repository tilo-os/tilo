use ui::Theme;
use ui::widget::{Text, Widget};

/// The graph is validated before any widget reaches the app tree.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let root: Box<dyn Widget> = match crate::screens::router("main", &Theme::default()) {
        Ok(router) => Box::new(router),
        Err(error) => Box::new(Text::new(&format!(
            "Unable to load the generated UI: {error}"
        ))),
    };
    crate::resource_actions::immediate(
        root,
        crate::resource_actions::typed_resource(
            "generated.graph-project",
            "Project",
            "create",
            "Create project",
            vec![qos_actions::ActionParam::required_owned(
                "name", qos_actions::ParamType::Text, "Project name",
            )],
        ),
        |payload| {
            let _: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(payload)
                .map_err(|error| error.to_string())?;
            Ok(Default::default())
        },
    )
}
