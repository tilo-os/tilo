use ui::prelude::*;

/// One created project.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Project {
    pub name: String,
}

/// The app's state: the name being drafted and the projects created. The
/// serialized screens above it are presentation; this is what is stored,
/// synced and undone.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Projects {
    pub draft: String,
    pub projects: Collection<Project>,
}

/// The draft is edited at its path (`Projects::paths().draft`); creating a
/// project from it is a verb.
#[resource(kind = "generated.graph-project", label = "Project")]
impl Projects {
    /// Create a project from the draft.
    #[action]
    pub fn create(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let name = self.draft.trim().to_owned();
        if name.is_empty() {
            return Err("a project needs a name");
        }
        self.projects.push(cx.id(), Project { name });
        self.draft.clear();
        Ok(())
    }
}

/// The serialized screens under `ui/`, validated before any widget reaches
/// the app. `tilo-app ui edit ui/main.json` edits them while the app runs;
/// `tilo-app add screen` and `tilo-app add component --mount` extend them.
/// A screen's text input over `draft` edits the resource's draft field; an
/// input over any other path keeps what is typed in the screen's values until
/// the app acts on it.
fn screens(cx: &ViewCx<Projects>) -> Element {
    let context = crate::components::ui_graph_context();
    cx.route_graph_text(context.values(), "draft", Projects::paths().draft);
    match crate::screens::router_with("main", &Theme::default(), context) {
        Ok(router) => Box::new(router),
        Err(error) => Box::new(Banner::error(ui::i18n::I18nManager::format(
            "graph-load-error",
            &[("error", error.to_string().as_str().into())],
        ))),
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = Projects;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn view(cx: &mut ViewCx<Projects>) -> Element {
        let input = cx
            .input(cx.paths().draft)
            .with_id("name-input")
            .with_placeholder("graph-project-name");
        let preview = cx
            .text(|projects| projects.draft.clone())
            .with_id("name-preview");
        let create = cx.button("graph-create", Projects::CREATE.invoke());
        let created = cx.each(
            |projects| &projects.projects,
            |row| Box::new(row.text(|project| project.name.clone())),
        );
        Box::new(
            Column::new()
                .push(screens(cx))
                .push(Box::new(labeled_field("graph-project-name", input)))
                .push(Box::new(preview))
                .push(Box::new(create))
                .push(Box::new(created)),
        )
    }
}
