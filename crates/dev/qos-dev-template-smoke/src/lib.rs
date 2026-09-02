//! Compile fixture for source emitted by every `qos-dev new` template.

pub mod locales {
    const CATALOG_0: &str = "starter-title = Deine erste qOS-App\n\
starter-name-label = Dein Name\n\
starter-name-placeholder = Name eingeben\n\
starter-updates-label = Neuigkeiten erhalten\n\
starter-refresh-label = Aktualisierungsintervall\n\
starter-volume-label = Lautstärke\n\
starter-region-label = Region\n\
starter-greeting = Hallo, { $name }!\n";
    const CATALOG_1: &str = "starter-title = Your first qOS app\n\
starter-name-label = Your name\n\
starter-name-placeholder = Type your name\n\
starter-updates-label = Receive updates\n\
starter-refresh-label = Refresh interval\n\
starter-volume-label = Volume\n\
starter-region-label = Region\n\
starter-greeting = Hello, { $name }!\n\
data-name-column = Name\n\
data-actions-column = Actions\n\
data-accessible-name = Results\n\
data-open = Open\n\
list-accessible-name = Activity\n\
list-open = Open\n\
list-loading = Loading activity…\n\
tree-accessible-name = Workspace items\n\
tree-row-action = More actions\n\
graph-project-name = Project name\n\
graph-create = Create\n\
graph-create-tooltip = Create the project\n\
task-title = Background work\n\
task-run = Run task\n\
task-cancel = Cancel\n\
task-idle = Start a background task without polling\n\
task-loading = Loading…\n\
task-ready = Found { $count } entries\n\
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

pub mod resource_actions {
    include!("../../qos-dev/templates/resource_actions.rs");
}

pub mod starter {
    include!("../../qos-dev/templates/starter.rs");
}

pub mod data {
    include!("../../qos-dev/templates/data.rs");
}

pub mod list {
    include!("../../qos-dev/templates/list.rs");
}

pub mod tree {
    include!("../../qos-dev/templates/tree.rs");
}

pub mod async_task {
    include!("../../qos-dev/templates/async.rs");
}

pub mod document {
    include!("../../qos-dev/templates/document.rs");
}

pub mod qr {
    include!("../../qos-dev/templates/qr.rs");
}

pub mod workspace {
    include!("../../qos-dev/templates/workspace.rs");
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
            "type": "text-input",
            "id": "name-input",
            "accessible_name": "graph-project-name",
            "value_path": "name"
          },
          {
            "type": "text",
            "id": "name-preview",
            "text": { "source": "document", "path": "name" }
          },
          {
            "type": "component",
            "id": "activity-summary",
            "name": "activity-card",
            "props": { "title": "Recent activity", "detail": "Nothing pending" }
          },
          {
            "type": "button",
            "id": "create",
            "label": "graph-create",
            "on_click_path": "create-requested"
          },
          {
            "type": "button",
            "id": "open-settings",
            "label": "Settings",
            "navigate": { "action": "push", "route": "settings" }
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

pub mod graph {
    include!("../../qos-dev/templates/graph.rs");
}

pub mod components {
    #[must_use]
    pub fn ui_graph_context() -> ui::UiBuildContext {
        crate::generated_display_component::register_ui_graph(ui::UiBuildContext::default())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ui::a11y::{Action, Role};
    use ui::automerge::{AutoCommit, ObjId};
    use ui::headless::SemanticExpectation;
    use ui::qos_actions::{InvocationId, InvocationUpdate, OperationKind};

    #[test]
    fn every_generated_template_has_a_clean_initial_accessibility_audit() {
        type TemplateCase = (&'static str, Box<dyn ui::widget::Widget>, (u32, u32));
        let templates: [TemplateCase; 9] = [
            ("starter", super::starter::app_root(), (800, 500)),
            ("data", super::data::app_root(), (800, 500)),
            ("list", super::list::app_root(), (800, 500)),
            ("tree", super::tree::app_root(), (800, 500)),
            ("graph", super::graph::app_root(), (800, 500)),
            ("async", super::async_task::app_root(), (800, 500)),
            ("document", super::document::app_root(), (800, 600)),
            ("qr", super::qr::app_root(), (800, 600)),
            ("workspace", super::workspace::app_root(), (1000, 640)),
        ];
        for (name, root, viewport) in templates {
            let driver = ui::headless::Driver::new(ui::State::new(), root, viewport);
            let audit = driver.accessibility_audit();
            assert!(audit.is_clean(), "{name}: {:#?}", audit.issues);
        }
    }

    #[test]
    fn every_generated_template_publishes_an_invocable_resource_action() {
        let mut templates: [(Box<dyn ui::widget::Widget>, &[u8]); 7] = [
            (super::starter::app_root(), br#"{"name":"Ada","updates":true,"refresh":30.0,"volume":0.5,"region":"Europe/Berlin"}"#),
            (super::data::app_root(), br#"{"id":3}"#),
            (super::list::app_root(), br#"{"id":3}"#),
            (super::tree::app_root(), br#"{"id":3}"#),
            (super::graph::app_root(), br#"{"name":"Atlas"}"#),
            (super::document::app_root(), b"null"),
            (super::qr::app_root(), br#"{"payload":"https://qos.dev"}"#),
        ];
        let doc = AutoCommit::new();
        for (root, payload) in &mut templates {
            let resources = root.resources(&doc);
            assert_eq!(resources.len(), 1);
            assert!(!resources[0].actions.is_empty());
            let result = root
                .invoke_resource_operation_v2(
                    OperationKind::Effect,
                    super::resource_actions::OPERATION_TYPE,
                    payload,
                    &doc,
                    &ObjId::Root,
                )
                .expect("generated operation succeeds");
            assert!(result.is_some(), "published operation has a live handler");
        }
    }

    #[test]
    fn generated_async_action_is_accepted_before_terminal_completion() {
        let mut root = super::async_task::app_root();
        let doc = AutoCommit::new();
        let invocation = InvocationId::new();
        let started = root
            .start_resource_operation(
                invocation,
                OperationKind::Effect,
                super::resource_actions::OPERATION_TYPE,
                b"null",
                &doc,
                &ObjId::Root,
            )
            .expect("background operation starts");
        assert_eq!(started, Some(ui::widget::ResourceOperationStart::Accepted));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(update) =
                root.poll_resource_operation(invocation, false, &doc, &ObjId::Root)
            {
                assert!(matches!(update, InvocationUpdate::Succeeded { .. }));
                break;
            }
            assert!(Instant::now() < deadline, "background operation timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn generated_starter_test_contract_runs() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::starter::app_root(), (800, 500));
        app.perform_named("name-input", &Action::SetValue("Ada".into()))
            .expect("generated input remains addressable");
        app.expect(
            &SemanticExpectation::named("greeting")
                .role(Role::StaticText)
                .name_contains("Ada"),
        )
        .expect("generated greeting semantics match");
        app.perform_named("updates-checkbox", &Action::ToggleCheck)
            .expect("generated local checkbox remains addressable");
        app.expect(
            &SemanticExpectation::named("updates-checkbox")
                .role(Role::Checkbox)
                .checked(false),
        )
        .expect("generated boolean binding updates semantics");
        app.perform_named("refresh-input", &Action::SetValue("30".into()))
            .expect("generated number input remains addressable");
        app.expect(
            &SemanticExpectation::named("refresh-input")
                .role(Role::SpinButton)
                .value("30 min"),
        )
        .expect("generated numeric binding updates semantics");
        app.perform_named("volume-slider", &Action::SetValue("75%".into()))
            .expect("generated slider remains addressable");
        app.expect(
            &SemanticExpectation::named("volume-slider")
                .role(Role::Slider)
                .value("75%"),
        )
        .expect("generated slider binding updates semantics");
        app.perform_named("region-select", &Action::SetValue("Asia/Tokyo".into()))
            .expect("generated dropdown remains addressable");
        app.expect(
            &SemanticExpectation::named("region-select")
                .role(Role::ComboBox)
                .value("Asia/Tokyo"),
        )
        .expect("generated choice binding updates semantics");
    }

    #[test]
    fn generated_qr_composes_input_state_and_accessible_visual_output() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::qr::app_root(), (800, 600));
        app.perform_named("qr-input", &Action::SetValue("你好 Rust GUI".into()))
            .expect("generated input remains addressable");
        app.expect(
            &SemanticExpectation::named("qr-output")
                .role(Role::Image)
                .name_contains("你好 Rust GUI"),
        )
        .expect("generated QR output is screen-reader visible");
    }

    #[test]
    fn generated_async_task_starts_and_dispatches_work() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::async_task::app_root(), (800, 500));
        let initial = app.inspection_json().expect("semantic snapshot");
        assert!(
            initial.contains("Start a background task without polling"),
            "{initial}"
        );
        let scenario = ui::headless::parse_spec(
            r#"{"actions":[
                {"perform":{"target":"run-task","action":"activate"}},
                {"expect":{"target":"task-result","role":"static_text","name_contains":"Found","timeout_ms":1000}}
            ]}"#,
        )
        .expect("generated saved scenario is valid");
        for action in scenario.actions {
            app.perform(&action)
                .expect("saved scenario action succeeds against the generated app");
        }
        let ready = app.inspection_json().expect("ready semantic snapshot");
        assert!(ready.contains("Found"), "{ready}");
    }

    #[test]
    fn generated_document_synchronizes_and_undoes_without_app_glue() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::document::app_root(), (800, 600));
        app.perform_named(
            "document-title-input",
            &Action::SetValue("Project Atlas".into()),
        )
        .expect("generated durable input remains addressable");
        app.expect(
            &SemanticExpectation::named("document-title-preview")
                .role(Role::Heading { level: 2 })
                .name("Project Atlas"),
        )
        .expect("generated document preview semantics match");

        app.state_mut()
            .undo(None, None)
            .expect("generated edit is undoable");
        app.relayout();
        let undone = app.inspection_json().expect("undone semantic snapshot");
        assert!(!undone.contains("Project Atlas"), "{undone}");
    }

    #[test]
    fn generated_workspace_switches_isolated_durable_detail_views() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::workspace::app_root(), (1000, 640));
        app.expect(
            &SemanticExpectation::named("workspace-selected-item")
                .role(Role::Heading { level: 1 })
                .name("Desktop app"),
        )
        .expect("default detail is addressable");
        let targets = app.targets();
        for target in [
            "workspace-selected-item",
            "workspace-title-input",
            "workspace-notes-input",
            "workspace-share",
        ] {
            let rect = targets
                .get(target)
                .unwrap_or_else(|| panic!("missing {target}"));
            assert!(
                rect.width() > 0.0 && rect.height() > 0.0,
                "{target}: {rect:?}"
            );
            assert!(rect.y0 >= 0.0 && rect.y1 <= 640.0, "{target}: {rect:?}");
        }
        app.perform_semantic_named("workspace-tree", Some("node:4"), &Action::Activate)
            .expect("select design system");
        app.expect(
            &SemanticExpectation::named("workspace-selected-item")
                .role(Role::Heading { level: 1 })
                .name("Design system"),
        )
        .expect("detail follows master selection");
        app.perform_named(
            "workspace-title-input",
            &Action::SetValue("Design tokens".into()),
        )
        .expect("edit the selected item");
        app.expect(
            &SemanticExpectation::named("workspace-title-preview")
                .role(Role::Heading { level: 2 })
                .name("Design tokens"),
        )
        .expect("bound preview follows input");

        app.perform_semantic_named("workspace-tree", Some("node:3"), &Action::Activate)
            .expect("switch item");
        app.expect(
            &SemanticExpectation::named("workspace-title-input")
                .role(Role::TextInput)
                .value(""),
        )
        .expect("the second item's binding is isolated");
        app.perform_semantic_named("workspace-tree", Some("node:4"), &Action::Activate)
            .expect("return to first item");
        app.expect(
            &SemanticExpectation::named("workspace-title-input")
                .role(Role::TextInput)
                .value("Design tokens"),
        )
        .expect("the original detail remains synchronized");
        assert!(app.dump_json().contains("workspace-item-4-title"));

        let mut compact =
            ui::headless::Driver::new(ui::State::new(), super::workspace::app_root(), (460, 820));
        compact.relayout();
        compact
            .expect(
                &SemanticExpectation::named("workspace-shell-back")
                    .role(Role::Button)
                    .name("Back"),
            )
            .expect("compact detail exposes accessible Back");
        compact
            .perform_named("workspace-shell-back", &Action::Activate)
            .expect("return to compact navigation");
        compact
            .expect(&SemanticExpectation::named("workspace-tree").role(Role::Tree))
            .expect("compact navigation uses the full-width tree pane");
        compact
            .perform_semantic_named("workspace-tree", Some("node:4"), &Action::Activate)
            .expect("choose a compact navigation row");
        compact
            .expect(
                &SemanticExpectation::named("workspace-selected-item")
                    .role(Role::Heading { level: 1 })
                    .name("Design system"),
            )
            .expect("compact selection navigates to full-width detail");
        let issues = compact.accessibility_audit();
        assert!(
            issues.issues.is_empty(),
            "compact workspace audit: {issues:#?}"
        );
    }

    #[test]
    fn generated_starter_can_switch_locale_without_rebuilding_its_widget_tree() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::starter::app_root(), (800, 500));
        ui::i18n::I18nManager::set_locale("de-DE");
        app.relayout();

        let snapshot = app.inspection_json().expect("localized semantic snapshot");
        assert!(snapshot.contains("Deine erste qOS-App"), "{snapshot}");
        assert!(snapshot.contains("Dein Name"), "{snapshot}");
        assert!(snapshot.contains("Neuigkeiten erhalten"), "{snapshot}");
        assert!(snapshot.contains("Aktualisierungsintervall"), "{snapshot}");
        assert!(snapshot.contains("Lautstärke"), "{snapshot}");
        assert!(snapshot.contains("Hallo, world!"), "{snapshot}");
        app.expect(
            &SemanticExpectation::named("name-input")
                .role(Role::TextInput)
                .name("Dein Name"),
        )
        .expect("the visible label and input name switch locale together");
    }

    #[test]
    fn generated_data_test_contract_runs() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::data::app_root(), (800, 500));
        app.perform_named("rows-table", &Action::Focus)
            .expect("generated table remains addressable");
        app.wait_for_named("open-3", std::time::Duration::from_secs(1))
            .expect("background page becomes ready");
        app.perform_semantic_named("rows-table", Some("row:3:0"), &Action::Activate)
            .expect("select stable row id");
        app.expect(
            &SemanticExpectation::named("rows-table")
                .semantic_target("row:3:0")
                .role(Role::Row)
                .selected(true),
        )
        .expect("generated row semantics match");
        let semantics = app.inspection().root.semantics.expect("table semantics");
        assert_eq!(semantics.role, Role::Table);
        assert!(semantics.state.focused);
        assert!(!semantics.children.is_empty());
        let selected = semantics
            .children
            .iter()
            .find(|row| row.action_target.as_deref() == Some("row:3:0"))
            .expect("selected row remains semantic");
        assert!(selected.state.selected);
        assert_eq!(
            selected
                .children
                .first()
                .and_then(|cell| cell.value.as_deref()),
            Some("Row 3")
        );
        assert!(semantics.children.len() < 40);
        assert_eq!(
            semantics.children.first().and_then(|row| row.set_size),
            Some(100_000_000)
        );
        app.perform_named("open-3", &Action::Activate)
            .expect("retained table cell control remains addressable");
        assert!(app.dump_json().contains("open-requested"));
    }

    #[test]
    fn generated_data_app_adapts_rows_and_selection_to_direct_touch() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::data::app_root(), (800, 500));
        app.wait_for_named("open-3", std::time::Duration::from_secs(1))
            .expect("background page becomes ready");
        app.set_interaction_environment(ui::InteractionEnvironment::TOUCH);
        let semantics = app.inspection().root.semantics.expect("table semantics");
        let first = semantics.children.first().expect("visible touch row");
        let bounds = first.bounds.expect("row bounds").to_rect();
        assert!((bounds.height() - 48.0).abs() < f64::EPSILON);

        let center = bounds.center();
        app.tap_at(center.x, center.y);
        let semantics = app.inspection().root.semantics.expect("table semantics");
        assert!(
            semantics.children.iter().any(|row| row.state.selected),
            "the generated table selects through host touch routing"
        );

        app.swipe_at((400.0, 300.0), (400.0, 100.0));
        let semantics = app.inspection().root.semantics.expect("table semantics");
        let first_position = semantics
            .children
            .first()
            .and_then(|row| row.pos_in_set)
            .expect("absolute visible row position");
        assert!(
            first_position > 1,
            "an upward direct-touch drag scrolls the generated virtual table"
        );
    }

    #[test]
    fn generated_list_keeps_a_sparse_variable_interactive_window() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::list::app_root(), (800, 500));
        app.set_interaction_environment(ui::InteractionEnvironment::TOUCH);
        app.perform_named("activity-list", &Action::Focus)
            .expect("generated list remains addressable");
        app.wait_for_named("open-0", std::time::Duration::from_secs(1))
            .expect("background page becomes ready");
        app.perform_semantic_named("activity-list", Some("key:3"), &Action::Activate)
            .expect("select stable model key");
        app.expect(
            &SemanticExpectation::named("activity-list")
                .semantic_target("key:3")
                .role(Role::ListItem)
                .selected(true),
        )
        .expect("generated list row semantics match");
        app.perform_named("open-0", &Action::Activate)
            .expect("interactive row delegate remains addressable");

        let semantics = app.inspection().root.semantics.expect("list semantics");
        assert_eq!(semantics.role, Role::List);
        assert!(semantics.state.focused);
        assert!(semantics.children.len() < 30);
        assert_eq!(semantics.children[0].set_size, Some(100_000_000));
        assert!(semantics.children.iter().any(|row| row.state.selected));
        let first_height = semantics.children[0].bounds.unwrap().to_rect().height();
        let second_height = semantics.children[1].bounds.unwrap().to_rect().height();
        assert!((first_height - second_height).abs() > 1.0);
        assert!(app.dump_json().contains("open-requested"));

        app.swipe_at((400.0, 360.0), (400.0, 120.0));
        let semantics = app.inspection().root.semantics.expect("list semantics");
        assert!(
            semantics.children[0]
                .pos_in_set
                .is_some_and(|position| position > 1)
        );
    }

    #[test]
    fn generated_tree_test_contract_runs() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::tree::app_root(), (800, 500));
        app.perform_named("workspace-tree", &Action::Focus)
            .expect("generated tree remains addressable");
        app.perform_semantic_named("workspace-tree", Some("node:2"), &Action::Collapse)
            .expect("collapse stable model node");
        app.perform_semantic_named("workspace-tree", Some("node:2"), &Action::Expand)
            .expect("expand stable model node");
        app.perform_semantic_named("workspace-tree", Some("node:3"), &Action::Activate)
            .expect("select stable model node");
        app.expect(
            &SemanticExpectation::named("workspace-tree")
                .semantic_target("node:3")
                .role(Role::TreeItem)
                .selected(true),
        )
        .expect("generated tree row semantics match");
        app.perform_named("more-4", &Action::Activate)
            .expect("retained tree row action remains independently addressable");
        let semantics = app.inspection().root.semantics.expect("tree semantics");
        assert_eq!(semantics.role, Role::Tree);
        assert!(semantics.state.focused);
        assert_eq!(semantics.name.as_deref(), Some("Workspace items"));
        assert_eq!(semantics.value.as_deref(), Some("Desktop app"));
        let projects = semantics
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("node:2"))
            .expect("Projects row");
        let desktop = semantics
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("node:3"))
            .expect("Desktop app row");
        let design = semantics
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("node:4"))
            .expect("Design system row");
        assert_eq!(projects.state.expanded, Some(true));
        assert!(desktop.state.selected);
        assert!(!design.state.selected);
        assert!(app.inspection_json().unwrap().contains("more-4"));
        assert!(app.dump_json().contains("tree-row-action-requested"));
    }

    #[test]
    fn generated_graph_test_contract_runs() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::graph::app_root(), (800, 500));
        app.perform_named("name-input", &Action::SetValue("Atlas".into()))
            .expect("serialized input remains addressable");
        app.perform_named("create", &Action::Activate)
            .expect("serialized button remains addressable");
        app.expect(
            &SemanticExpectation::named("name-preview")
                .role(Role::StaticText)
                .name("Atlas"),
        )
        .expect("generated graph preview semantics match");
        app.expect(
            &SemanticExpectation::named("activity-summary-title")
                .role(Role::Heading { level: 2 })
                .name("Recent activity"),
        )
        .expect("generated component registry reaches the screen router");
        assert!(app.dump_json().contains("create-requested"));
    }

    #[test]
    fn generated_graph_router_uses_process_local_back_history() {
        let mut app =
            ui::headless::Driver::new(ui::State::new(), super::graph::app_root(), (800, 500));
        let document_before = app.dump_json();
        app.perform_named("open-settings", &Action::Activate)
            .expect("generated navigation button remains addressable");
        assert!(app.inspection_json().unwrap().contains("Settings screen"));
        app.perform_named("back", &Action::Activate)
            .expect("generated back button remains addressable");
        assert!(app.inspection_json().unwrap().contains("Project name"));
        assert_eq!(app.dump_json(), document_before);
    }
}
