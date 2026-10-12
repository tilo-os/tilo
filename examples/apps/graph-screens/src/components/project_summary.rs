// tilo-app:graph-props=display-v1
use ui::tokens::TextRole;
use ui::widget::{Element, Text};
use ui::{UiBuildContext, UiComponentRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSummaryProps {
    pub title: String,
    pub detail: String,
}

impl ProjectSummaryProps {
    #[must_use]
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
        }
    }
}

/// Build one reusable `project-summary` instance. `id` keeps semantic identities unique.
pub fn project_summary(id: &str, props: &ProjectSummaryProps) -> ui::widget::Column {
    ui::column![
        Text::new(&props.title)
            .with_role(TextRole::Heading)
            .with_id(format!("{id}-title")),
        Text::new(&props.detail)
            .with_role(TextRole::Body)
            .with_id(format!("{id}-detail")),
    ]
    .with_no_padding()
}

/// Build this component from a serialized UI graph node.
///
/// # Errors
///
/// Returns a field-specific error when `title` or `detail` is not a string.
pub fn from_ui_graph(request: UiComponentRequest<'_>) -> Result<Element, String> {
    let string = |key| {
        request
            .props
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("{key} must be a string"))
    };
    let props = ProjectSummaryProps::new(string("title")?, string("detail")?);
    Ok(Box::new(project_summary(request.id, &props)))
}

/// Register this component's stable graph factory name.
#[must_use]
pub fn register_ui_graph(context: UiBuildContext) -> UiBuildContext {
    context.with_component("project-summary", from_ui_graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::Role;
    use ui::headless::SemanticExpectation;

    #[test]
    fn component_exposes_typed_props_and_stable_semantics() {
        let props = ProjectSummaryProps::new("Example title", "Example detail");
        let mut app = ui::headless::FrameHarness::new(
            ui::State::new(),
            Box::new(project_summary("example", &props)),
            (480, 240),
        );
        app.expect(
            &SemanticExpectation::named("example-title")
                .role(Role::Heading { level: 2 })
                .name("Example title"),
        )
        .expect("component heading semantics");
        app.expect(
            &SemanticExpectation::named("example-detail")
                .role(Role::StaticText)
                .name("Example detail"),
        )
        .expect("component detail semantics");
    }

    #[test]
    fn graph_factory_validates_props_and_builds_the_same_component() {
        let document = ui::UiDocument::from_json(
            r#"{"version":1,"root":{"type":"component","id":"example","name":"project-summary","props":{"title":"Graph title","detail":"Graph detail"}}}"#,
        )
        .expect("valid component graph");
        let context = register_ui_graph(UiBuildContext::default());
        let widget = ui::UiDocumentExt::build_with(&document, &ui::Theme::default(), &context)
            .expect("registered component");
        let mut app = ui::headless::FrameHarness::new(ui::State::new(), widget, (480, 240));
        app.expect(
            &SemanticExpectation::named("example-title")
                .role(Role::Heading { level: 2 })
                .name("Graph title"),
        )
        .expect("graph component heading");

        let invalid = ui::UiDocument::from_json(
            r#"{"version":1,"root":{"type":"component","id":"invalid","name":"project-summary","props":{"title":"Missing detail"}}}"#,
        )
        .expect("structurally valid component graph");
        assert!(matches!(
            ui::UiDocumentExt::build_with(&invalid, &ui::Theme::default(), &context),
            Err(ui::UiGraphError::ComponentBuild { message, .. })
                if message == "detail must be a string"
        ));
    }
}
