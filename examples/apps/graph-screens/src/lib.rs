//! Graph screens: presentation as data, state as a resource.
//!
//! The top of the window is not Rust. It is a serialized UI graph under
//! `ui/`, validated when the app starts, that `tilo-app ui edit` changes
//! while the app runs, with its own undo history. Screens reach Rust in two
//! ways: a component node mounts a widget an app-owned factory builds
//! (`src/components/`), and the screen router hosts every screen
//! registered in `src/screens.rs`, which `tilo-app add screen` maintains.
//! A navigation button in a screen (the About link) is published as a view
//! action, so an agent reaches the About screen the way a click does.
//!
//! Anything durable still goes through the resource. The native controls
//! under the screens bind to its actions like in any other app, so the
//! palette, agents and undo see the same surface.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.graph-screens";
/// The window and launcher title.
pub const APP_TITLE: &str = "Graph Screens";

pub mod locales;

fn install_catalog() {
    locales::install();
}

pub mod components;
pub mod screens;

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

#[resource(kind = "example.projects", label = "Project")]
impl Projects {
    /// Set the project name draft.
    #[action]
    pub fn set_draft(&mut self, #[param("Project name draft")] draft: String) {
        self.draft = draft;
    }

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
fn screens() -> Element {
    match crate::screens::router_with(
        "main",
        &Theme::default(),
        crate::components::ui_graph_context(),
    ) {
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

    fn view(cx: &mut ViewCx<Projects>) -> Element {
        install_catalog();
        let input = cx
            .input_with(|projects| projects.draft.clone(), Projects::SET_DRAFT)
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
                .push(screens())
                .push(Box::new(labeled_field("graph-project-name", input)))
                .push(Box::new(preview))
                .push(Box::new(create))
                .push(Box::new(created)),
        )
    }
}

ui::dev_reload_entry!(App);
