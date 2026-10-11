//! Compile fixture for source emitted by every `tilo-app new` template.

pub mod locales {
    // One catalog holding every template's messages; each template's own
    // `locales/en-US.ftl` is exactly its file here.
    const EN_US: &str = concat!(
        include_str!("../../tilo-app-cli/templates/starter.ftl"),
        include_str!("../../tilo-app-cli/templates/document.ftl"),
        include_str!("../../tilo-app-cli/templates/collection.ftl"),
        include_str!("../../tilo-app-cli/templates/graph.ftl"),
    );
    // What `tilo-app add locale de-DE` and a translator make of the starter's.
    const DE_DE: &str = "starter-title = Hallo\n\
starter-name-label = Dein Name\n\
starter-name-placeholder = Name eingeben\n\
starter-greeting = Hallo, { $name }!\n\
starter-greeting-empty = Wen sollen wir begrüßen?\n\
starter-clear = Leeren\n";

    pub const IDS: &[&str] = &["de-DE", "en-US"];

    /// [`EN_US`] without the templates' history messages: `tilo-app new`
    /// names those after the app it makes, and two templates' verbs share
    /// names, so the combined catalog would hold them twice.
    fn en_us() -> String {
        EN_US
            .lines()
            .filter(|line| !line.starts_with("generated-"))
            .map(|line| format!("{line}\n"))
            .collect()
    }

    pub fn install() {
        let en_us = en_us();
        if let Err(error) = ui::i18n::I18nManager::install_system(
            "en-US",
            &[("de-DE", DE_DE), ("en-US", en_us.as_str())],
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

pub mod document {
    include!("../../tilo-app-cli/templates/document.rs");
}

pub mod collection {
    include!("../../tilo-app-cli/templates/collection.rs");
}

pub mod graph {
    include!("../../tilo-app-cli/templates/graph.rs");
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
        build_with("main", UI_SOURCE, theme, context)
    }

    pub fn get(name: &str, theme: &Theme) -> Option<Result<Box<dyn Widget>, UiGraphError>> {
        match name {
            "main" => Some(main(theme)),
            "settings" => Some(build("settings", SETTINGS_SOURCE, theme)),
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
                build_with(route, source, theme, context).map_err(|error| error.to_string())
            },
        )
    }

    fn build(screen: &str, source: &str, theme: &Theme) -> Result<Box<dyn Widget>, UiGraphError> {
        build_with(screen, source, theme, UiBuildContext::default())
    }

    fn build_with(
        screen: &str,
        source: &str,
        theme: &Theme,
        context: UiBuildContext,
    ) -> Result<Box<dyn Widget>, UiGraphError> {
        let graph = UiGraphHandle::from_embedded_or_live(screen, source)?;
        Ok(Box::new(graph.widget_with(theme, context)?))
    }
}

// The generated `src/components/` of an app with one component per template.
pub mod components {
    include!(concat!(env!("OUT_DIR"), "/components/mod.rs"));
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
    document_tests => document, "document.rs";
    collection_tests => collection, "collection.rs";
    graph_tests => graph, "graph.rs";
}

#[cfg(test)]
mod tests {
    use ui::a11y::Role;
    use ui::headless::SemanticExpectation;
    use ui::prelude::TestApp;

    /// The generated starter switches locale without rebuilding its widget tree, and generated graph
    /// screens reach the app component registry.
    #[test]
    fn generated_templates_localise_and_reach_the_component_registry() {
        // Phase: generated_starter_can_switch_locale_without_rebuilding_its_widget_tree.
        {
            let mut app = TestApp::<super::starter::App>::new().expect("app opens");
            ui::i18n::I18nManager::set_locale("de-DE");
            app.harness().relayout();
            let snapshot = app
                .harness()
                .inspection_json()
                .expect("localized semantic snapshot");
            ui::i18n::I18nManager::set_locale("en-US");
            assert!(snapshot.contains("Dein Name"), "{snapshot}");
            assert!(snapshot.contains("Wen sollen wir begrüßen?"), "{snapshot}");
            assert!(snapshot.contains("Leeren"), "{snapshot}");
        }

        // Phase: generated_graph_screens_reach_the_app_component_registry.
        {
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
}
