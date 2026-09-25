//! Compile fixture for source emitted by every `tilo-app new` template.

pub mod locales {
    const CATALOG_0: &str = "starter-title = Deine erste Tilo-App\n\
starter-name-label = Dein Name\n\
starter-name-placeholder = Name eingeben\n\
starter-updates-label = Neuigkeiten erhalten\n\
starter-refresh-label = Aktualisierungsintervall\n\
starter-volume-label = Lautstärke\n\
starter-region-label = Region\n\
starter-greeting = Hallo, { $name }!\n\
starter-reset = Profil zurücksetzen\n";
    const CATALOG_1: &str = "starter-title = Your first Tilo app\n\
starter-name-label = Your name\n\
starter-name-placeholder = Type your name\n\
starter-updates-label = Receive updates\n\
starter-refresh-label = Refresh interval\n\
starter-volume-label = Volume\n\
starter-region-label = Region\n\
starter-greeting = Hello, { $name }!\n\
starter-reset = Reset profile\n\
data-name-column = Name\n\
data-actions-column = Actions\n\
data-accessible-name = Results\n\
data-open = Open\n\
data-recent = Recently opened: { $rows }\n\
data-none-opened = No rows opened yet\n\
list-accessible-name = Activity\n\
list-open = Open\n\
list-loading = Loading activity…\n\
list-last-opened = Last opened: { $activity }\n\
list-none-opened = Nothing opened yet\n\
tree-accessible-name = Workspace items\n\
tree-row-action = Pin\n\
tree-pinned = Pinned: { $count }\n\
graph-project-name = Project name\n\
graph-create = Create\n\
graph-create-tooltip = Create the project\n\
task-title = Background work\n\
task-run = Run task\n\
task-cancel = Cancel\n\
task-idle = Start a background task without polling\n\
task-loading = Loading…\n\
task-ready = Found { $count } entries\n\
task-folder-label = Folder\n\
document-heading = Durable document\n\
document-title-label = Title\n\
document-title-placeholder = Untitled\n\
document-body-label = Body\n\
document-body-placeholder = Start writing…\n\
document-preview-heading = Live preview\n\
qr-title = QR code generator\n\
qr-input-label = Content\n\
qr-input-placeholder = Type text or a link\n\
qr-output-alt = QR code for { $payload }\n\
qr-reset = Reset QR code\n\
workspace-root = Workspace\n\
workspace-projects = Projects\n\
workspace-archive = Archive\n\
workspace-desktop-app = Desktop app\n\
workspace-design-system = Design system\n\
workspace-first-prototype = First prototype\n\
workspace-unknown-item = Selected item\n\
workspace-tree-name = Workspace items\n\
workspace-navigation-heading = Projects\n\
workspace-title-label = Title\n\
workspace-title-placeholder = Untitled item\n\
workspace-notes-label = Notes\n\
workspace-notes-placeholder = Add project notes…\n\
workspace-preview-heading = Live preview\n\
workspace-back = Back\n\
workspace-share = Share\n";

    pub const IDS: &[&str] = &["de-DE", "en-US"];

    pub fn install() {
        if let Err(error) = ui::i18n::I18nManager::install_system(
            "en-US",
            &[("de-DE", CATALOG_0), ("en-US", CATALOG_1)],
        ) {
            eprintln!("localization catalog: {error}");
        }
    }
}

fn install_catalog() {
    locales::install();
}

/// The application id every included template runs under.
pub const APP_ID: &str = "so.tilo.template-smoke";
/// The window title every included template runs under.
pub const APP_TITLE: &str = "Template smoke";

pub mod starter {
    include!("../../tilo-app-cli/templates/starter.rs");
}

pub mod data {
    include!("../../tilo-app-cli/templates/data.rs");
}

pub mod list {
    include!("../../tilo-app-cli/templates/list.rs");
}

pub mod tree {
    include!("../../tilo-app-cli/templates/tree.rs");
}

pub mod async_task {
    include!("../../tilo-app-cli/templates/async.rs");
}

pub mod document {
    include!("../../tilo-app-cli/templates/document.rs");
}

pub mod qr {
    include!("../../tilo-app-cli/templates/qr.rs");
}

pub mod workspace {
    include!("../../tilo-app-cli/templates/workspace.rs");
}

pub mod graph {
    include!("../../tilo-app-cli/templates/graph.rs");
}

pub mod generated_form_component {
    include!(concat!(env!("OUT_DIR"), "/profile_card.rs"));
}

pub mod generated_display_component {
    include!(concat!(env!("OUT_DIR"), "/activity_card.rs"));
}

pub mod generated_tree_component {
    include!(concat!(env!("OUT_DIR"), "/project_tree.rs"));
}

pub mod generated_list_component {
    include!(concat!(env!("OUT_DIR"), "/activity_list.rs"));
}

pub mod generated_table_component {
    include!(concat!(env!("OUT_DIR"), "/results_table.rs"));
}

pub mod generated_text_component {
    include!(concat!(env!("OUT_DIR"), "/article_body.rs"));
}

pub mod screens {
    use ui::widget::Widget;
    use ui::{RouteError, Router, Theme, UiBuildContext, UiGraphError, UiGraphHandle};

    const UI_SOURCE: &str = r#"{
      "version": 1,
      "root": {
        "type": "column",
        "id": "root",
        "children": [
          {
            "type": "text",
            "id": "graph-introduction",
            "text": { "source": "static", "value": "Action-native graph screen" }
          },
          {
            "type": "component",
            "id": "activity-summary",
            "name": "activity-card",
            "props": { "title": "Recent activity", "detail": "Nothing pending" }
          }
        ]
      }
    }"#;

    const SETTINGS_SOURCE: &str = r#"{
      "version": 1,
      "root": {
        "type": "column",
        "id": "settings-root",
        "children": [
          {
            "type": "text",
            "id": "settings-title",
            "text": { "source": "static", "value": "Settings screen" }
          },
          {
            "type": "button",
            "id": "back",
            "label": "Back",
            "navigate": { "action": "back" }
          }
        ]
      }
    }"#;

    /// Build the generated main screen.
    ///
    /// # Errors
    ///
    /// Returns a graph parse, validation, or construction error.
    pub fn main(theme: &Theme) -> Result<Box<dyn Widget>, UiGraphError> {
        main_with(theme, UiBuildContext::default())
    }

    /// Build the main screen with app-owned component factories.
    ///
    /// # Errors
    ///
    /// Returns a graph parse, validation, component, or construction error.
    pub fn main_with(
        theme: &Theme,
        context: UiBuildContext,
    ) -> Result<Box<dyn Widget>, UiGraphError> {
        build_with(UI_SOURCE, theme, context)
    }

    pub fn get(name: &str, theme: &Theme) -> Option<Result<Box<dyn Widget>, UiGraphError>> {
        match name {
            "main" => Some(main(theme)),
            "settings" => Some(build(SETTINGS_SOURCE, theme)),
            _ => None,
        }
    }

    /// Build the generated route registry at `initial`.
    ///
    /// # Errors
    ///
    /// Returns an error when the route is absent or one of its graphs cannot
    /// be parsed, validated, or constructed.
    pub fn router(initial: &str, theme: &Theme) -> Result<Router, RouteError> {
        router_with(initial, theme, UiBuildContext::default())
    }

    /// Build the route registry with app-owned component factories.
    ///
    /// # Errors
    ///
    /// Returns an error when the initial route or a screen is invalid.
    pub fn router_with(
        initial: &str,
        theme: &Theme,
        context: UiBuildContext,
    ) -> Result<Router, RouteError> {
        Router::new(
            initial,
            ["main", "settings"],
            theme,
            move |route, theme, navigation| {
                let source = if route == "main" {
                    UI_SOURCE
                } else {
                    SETTINGS_SOURCE
                };
                let context = context.clone().with_router(navigation.clone());
                build_with(source, theme, context).map_err(|error| error.to_string())
            },
        )
    }

    fn build(source: &str, theme: &Theme) -> Result<Box<dyn Widget>, UiGraphError> {
        build_with(source, theme, UiBuildContext::default())
    }

    fn build_with(
        source: &str,
        theme: &Theme,
        context: UiBuildContext,
    ) -> Result<Box<dyn Widget>, UiGraphError> {
        let graph = UiGraphHandle::from_json(source)?;
        Ok(Box::new(graph.widget_with(theme, context)?))
    }
}

pub mod components {
    #[must_use]
    pub fn ui_graph_context() -> ui::UiBuildContext {
        crate::generated_display_component::register_ui_graph(ui::UiBuildContext::default())
    }
}

// Each template's generated `tests/app.rs`, run against the template itself.
macro_rules! generated_tests {
    ($($module:ident => $template:ident, $file:literal;)*) => {$(
        #[cfg(test)]
        mod $module {
            use crate::$template as generated_app;
            include!(concat!("../../tilo-app-cli/templates/tests/", $file));
            include!("../../tilo-app-cli/templates/tests/native.rs");
        }
    )*};
}

generated_tests! {
    starter_tests => starter, "starter.rs";
    data_tests => data, "data.rs";
    list_tests => list, "list.rs";
    tree_tests => tree, "tree.rs";
    graph_tests => graph, "graph.rs";
    async_tests => async_task, "async.rs";
    document_tests => document, "document.rs";
    qr_tests => qr, "qr.rs";
    workspace_tests => workspace, "workspace.rs";
}

#[cfg(test)]
mod tests {
    use ui::a11y::Role;
    use ui::headless::SemanticExpectation;
    use ui::prelude::TestApp;

    #[test]
    fn generated_starter_can_switch_locale_without_rebuilding_its_widget_tree() {
        let mut app = TestApp::<super::starter::App>::new().expect("app opens");
        ui::i18n::I18nManager::set_locale("de-DE");
        app.harness().relayout();
        let snapshot = app
            .harness()
            .inspection_json()
            .expect("localized semantic snapshot");
        ui::i18n::I18nManager::set_locale("en-US");
        assert!(snapshot.contains("Deine erste Tilo-App"), "{snapshot}");
        assert!(snapshot.contains("Dein Name"), "{snapshot}");
        assert!(snapshot.contains("Neuigkeiten erhalten"), "{snapshot}");
        assert!(snapshot.contains("Aktualisierungsintervall"), "{snapshot}");
        assert!(snapshot.contains("Lautstärke"), "{snapshot}");
        assert!(snapshot.contains("Hallo, world!"), "{snapshot}");
    }

    #[test]
    fn generated_graph_screens_reach_the_app_component_registry() {
        let mut app = TestApp::<super::graph::App>::new().expect("app opens");
        app.harness()
            .expect(
                &SemanticExpectation::named("activity-summary-title")
                    .role(Role::Heading { level: 2 })
                    .name("Recent activity"),
            )
            .expect("generated component registry reaches the screen router");
    }
}
