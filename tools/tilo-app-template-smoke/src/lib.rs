//! Compile fixture for source emitted by every `tilolang new` template.

pub mod locales {
    // One catalog holding every template's messages; each template's own
    // `locales/en-US.ftl` is exactly its file here.
    const EN_US: &str = concat!(
        include_str!("../../../crates/lang/tilo-app-builder/templates/starter.ftl"),
        include_str!("../../../crates/lang/tilo-app-builder/templates/document.ftl"),
        include_str!("../../../crates/lang/tilo-app-builder/templates/collection.ftl"),
        include_str!("../../../crates/lang/tilo-app-builder/templates/graph.ftl"),
    );

    /// The declarations read their words where the crate is built, from
    /// `locales/en-US.ftl`: every word a template names its record and
    /// actions by (its catalog's last section, `## Actions`) is there as the
    /// template says it.
    #[test]
    fn the_declarations_read_every_templates_words() {
        let declared = include_str!("../locales/en-US.ftl");
        for template in [
            include_str!("../../../crates/lang/tilo-app-builder/templates/starter.ftl"),
            include_str!("../../../crates/lang/tilo-app-builder/templates/document.ftl"),
            include_str!("../../../crates/lang/tilo-app-builder/templates/collection.ftl"),
            include_str!("../../../crates/lang/tilo-app-builder/templates/graph.ftl"),
        ] {
            let (_, actions) = template
                .split_once("## Actions")
                .expect("a section of actions");
            for line in actions.lines().filter(|line| line.contains(" = ")) {
                assert!(declared.lines().any(|held| held == line), "{line}");
            }
        }
    }

    // What `tilolang add locale de-DE` and a translator make of the starter's.
    const DE_DE: &str = "starter-title = Hallo\n\
starter-name-label = Dein Name\n\
starter-name-placeholder = Name eingeben\n\
starter-greeting = Hallo, { $name }!\n\
starter-greeting-empty = Wen sollen wir begrüßen?\n\
starter-clear = Leeren\n";

    /// Two templates' history verbs share a message id; the first one wins,
    /// as when a catalog repeats a message.
    pub static CATALOG: tilo_ui_base::i18n::EmbeddedCatalog =
        tilo_ui_base::i18n::EmbeddedCatalog::new("en-US", &[("de-DE", DE_DE), ("en-US", EN_US)])
            .with_system_locale();
}

/// The application id every included template runs under.
pub const APP_ID: &str = "so.tilo.template-smoke";
/// The window title every included template runs under.
pub const APP_TITLE: &str = "Template smoke";

pub mod starter {
    include!("../../../crates/lang/tilo-app-builder/templates/starter.rs");
}

pub mod document {
    include!("../../../crates/lang/tilo-app-builder/templates/document.rs");
}

pub mod collection {
    include!("../../../crates/lang/tilo-app-builder/templates/collection.rs");
}

pub mod graph {
    include!("../../../crates/lang/tilo-app-builder/templates/graph.rs");
}

pub mod screens {
    use tilo_ui_base::widget::Widget;
    use ui::{UiBuildContext, UiGraphError, UiGraphHandle};
    use tilo_ui_base::{RouteError, Router, Theme};

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
    pub fn main(theme: &Theme) -> Result<Box<dyn Widget>, UiGraphError> {
        main_with(theme, UiBuildContext::default())
    }

    /// Build the main screen with app-owned component factories.
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
    pub fn router(initial: &str, theme: &Theme) -> Result<Router, RouteError> {
        router_with(initial, theme, UiBuildContext::default())
    }

    /// Build the route registry with app-owned component factories.
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
            include!(concat!("../../../crates/lang/tilo-app-builder/templates/tests/", $file));
            include!("../../../crates/lang/tilo-app-builder/templates/tests/native.rs");
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
    use tilo_ui_base::a11y::Role;
    use ui::headless::SemanticExpectation;
    use ui::prelude::HeadlessApp;

    /// The generated starter switches locale without rebuilding its widget tree, and generated graph
    /// screens reach the app component registry.
    #[test]
    fn generated_templates_localise_and_reach_the_component_registry() {
        // Phase: generated_starter_can_switch_locale_without_rebuilding_its_widget_tree.
        {
            let mut app = HeadlessApp::<super::starter::App>::new().expect("app opens");
            tilo_ui_base::i18n::I18nManager::set_locale("de-DE");
            app.harness().relayout();
            let snapshot = app
                .harness()
                .inspection_json()
                .expect("localized semantic snapshot");
            tilo_ui_base::i18n::I18nManager::set_locale("en-US");
            assert!(snapshot.contains("Dein Name"), "{snapshot}");
            assert!(snapshot.contains("Wen sollen wir begrüßen?"), "{snapshot}");
            assert!(snapshot.contains("Leeren"), "{snapshot}");
        }

        // Phase: generated_graph_screens_reach_the_app_component_registry.
        {
            let mut app = HeadlessApp::<super::graph::App>::new().expect("app opens");
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

/// Each template's committed surface against the code it is written from: the
/// comparison `tilolang check` makes for an app `tilolang new` made from it, so
/// a resource the framework grows a verb on cannot leave a new app's first
/// `check` failing.
#[cfg(test)]
mod surface_tests {
    use serde_json::Value;
    use ui::tilo_resource::surface::file::filed;

    /// The line every surface file starts with.
    const HEADER: &str = "# Written by `tilolang surface`; review changes like an API change.\n\n";

    /// Each template's name, the surface report its app prints for `--surface`,
    /// and the committed surface it ships.
    fn templates() -> [(&'static str, Value, &'static str); 4] {
        [
            (
                "starter",
                ui::framework::headless::surface_report::<super::starter::App>(),
                include_str!(
                    "../../../crates/lang/tilo-app-builder/templates/starter.surface.toml"
                ),
            ),
            (
                "document",
                ui::framework::headless::surface_report::<super::document::App>(),
                include_str!(
                    "../../../crates/lang/tilo-app-builder/templates/document.surface.toml"
                ),
            ),
            (
                "collection",
                ui::framework::headless::surface_report::<super::collection::App>(),
                include_str!(
                    "../../../crates/lang/tilo-app-builder/templates/collection.surface.toml"
                ),
            ),
            (
                "graph",
                ui::framework::headless::surface_report::<super::graph::App>(),
                include_str!("../../../crates/lang/tilo-app-builder/templates/graph.surface.toml"),
            ),
        ]
    }

    /// The surface `file` holds, as the report names it.
    fn committed(file: &str) -> Value {
        serde_json::to_value(
            toml::from_str::<toml::Value>(file).expect("a committed surface is TOML"),
        )
        .expect("a surface file as JSON")
    }

    fn resources(surface: &Value) -> &[Value] {
        surface["resources"]
            .as_array()
            .expect("a surface names its resources")
    }

    #[test]
    fn every_templates_surface_is_what_its_code_publishes() {
        for (name, report, file) in templates() {
            let committed = committed(file);
            let (now, before) = (resources(&report), resources(&committed));
            assert_eq!(
                now.len(),
                before.len(),
                "the {name} template's code publishes {} resources and its surface.toml names {}",
                now.len(),
                before.len()
            );
            for expected in before {
                let kind = expected["kind"].as_str().expect("a resource kind");
                let published = now
                    .iter()
                    .find(|resource| resource["kind"] == *kind)
                    .unwrap_or_else(|| panic!("the {name} template's code publishes no `{kind}`"));
                assert_eq!(
                    filed(published),
                    filed(expected),
                    "the {name} template's `{kind}` surface is behind its code; write it with `tilolang surface`"
                );
            }
        }
    }

    /// Print each template's surface as its code publishes it now, for writing
    /// the committed files: `t cargo test -p tilo-app-template-smoke --lib
    /// print_template_surfaces -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints each template's surface instead of comparing it"]
    fn print_template_surfaces() {
        for (name, report, _) in templates() {
            let surface = ui::tilo_resource::surface::file::render(HEADER, resources(&report), &[])
                .expect("a rendered surface");
            println!("== {name}\n{surface}");
        }
    }
}
