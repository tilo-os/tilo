use super::*;

fn request(dir: PathBuf) -> NewAppRequest {
    NewAppRequest {
        name: Some("reading-list".into()),
        app_id: None,
        title: None,
        template: Some(Template::Starter),
        directory: Some(dir),
        ui_path: None,
        sdk_git: None,
        sdk_rev: None,
        dry_run: false,
    }
}

fn fixture_sdk(workspace_member: bool) -> AppSdk {
    if workspace_member {
        AppSdk::Workspace {
            ui_path: PathBuf::from("../../crates/sdk/ui-core"),
        }
    } else {
        AppSdk::Standalone(standalone::Standalone {
            source: standalone::SdkSource::Checkout(PathBuf::from("../tilo")),
            baseline: standalone::Baseline::embedded().expect("embedded SDK baseline"),
        })
    }
}

fn assert_generated_tree_source(source: &str) {
    syn::parse_file(source).unwrap();
    for needle in [
        "pub struct ProjectTreeConfig",
        "M: TreeModel",
        "TreeView::from_model(model)",
        ".with_slot_delegate(",
        "TreeSlotState::Error",
        ".with_row_delegate(",
        ".reorderable()",
    ] {
        assert!(source.contains(needle), "missing {needle:?}");
    }
}

fn minimal_app(root: &Path) {
    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tilo-app.toml"),
        "[package]\nid = \"so.tilo.demo\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn app_root() {}\n").unwrap();
}

fn minimal_generated_graph_app(root: &Path) {
    minimal_app(root);
    std::fs::create_dir(root.join("ui")).unwrap();
    std::fs::write(
        root.join("ui/main.json"),
        starter_ui_document("Graph app").to_json_pretty().unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("src/screens.rs"),
        screen_registry_source(&["main".into()]),
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        format!("pub mod screens;\n\n{}", legacy_graph_app_template()),
    )
    .unwrap();
}

/// add screen resolves the app and emits a valid graph; every screen template is valid, accessible and
/// model-bound; the screen template wires locales, navigation and graph atomically.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn add_screen_emits_valid_accessible_model_bound_graphs_with_their_wiring() {
    // Phase: add_screen_resolves_app_and_emits_a_valid_graph.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        let outcome = add_app_artifact(AddRequest {
            start: app.join("src/nested"),
            artifact: AddArtifact::Screen {
                name: "account-settings".into(),
                title: None,
                template: ScreenTemplate::Blank,
                link_from: None,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(outcome.app_dir, app.canonicalize().unwrap());
        let graph_path = &outcome.files[0];
        assert_eq!(
            graph_path,
            &app.canonicalize().unwrap().join("ui/account-settings.json")
        );
        assert_eq!(outcome.files.len(), 3);
        assert_eq!(
            outcome.next_command.as_deref(),
            Some("tilo-app ui edit ui/account-settings.json")
        );
        let graph = load_ui_graph(graph_path).unwrap();
        assert!(matches!(
            graph.node("title"),
            Some(tilo_ui_graph::UiNode::Text {
                text: tilo_ui_graph::UiTextSource::Static { value },
                ..
            }) if value == "Account Settings"
        ));
        let registry = std::fs::read_to_string(app.join("src/screens.rs")).unwrap();
        assert!(registry.contains("pub const NAMES"));
        assert!(registry.contains("include_str!(\"../ui/account-settings.json\")"));
        syn::parse_file(&registry).expect("generated registry is Rust");
        assert!(
            std::fs::read_to_string(app.join("src/lib.rs"))
                .unwrap()
                .contains("pub mod screens;")
        );
    }

    // Phase: every_screen_template_is_valid_accessible_and_model_bound.
    {
        for (template, expected) in [
            (ScreenTemplate::Blank, "body"),
            (ScreenTemplate::Form, "details-input"),
            (ScreenTemplate::Settings, "notifications"),
            (ScreenTemplate::EmptyState, "primary-action"),
        ] {
            let (document, messages) =
                screen_template_document("account", "Account", template, true);
            document.validate().expect("generated graph");
            assert!(document.node(expected).is_some(), "missing {expected}");
            assert!(document.nodes().iter().all(|node| !node.id().is_empty()));
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.id.as_str())
                    .collect::<HashSet<_>>()
                    .len(),
                messages.len(),
                "message ids must be unique"
            );
            assert!(matches!(
                document.node("title"),
                Some(tilo_ui_graph::UiNode::Text {
                    text: tilo_ui_graph::UiTextSource::Static { value },
                    ..
                }) if value == "account-title"
            ));
            if template == ScreenTemplate::Form {
                assert!(matches!(
                    document.node("name-input"),
                    Some(tilo_ui_graph::UiNode::TextInput {
                        label: Some(label),
                        accessible_name,
                        ..
                    }) if label == "account-name-label" && accessible_name.is_empty()
                ));
                assert!(document.node("name-label").is_none());
            }
            if template == ScreenTemplate::Settings {
                assert!(matches!(
                    document.node("notifications"),
                    Some(tilo_ui_graph::UiNode::Toggle {
                        label: Some(label),
                        accessible_name,
                        ..
                    }) if label == "account-notifications" && accessible_name.is_empty()
                ));
                assert!(document.node("notifications-row").is_none());
                assert!(document.node("notifications-label").is_none());
            }
        }
    }

    // Phase: screen_template_wires_locales_navigation_and_graph_atomically.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        create_app(request(app.clone())).unwrap();
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "de-DE".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "main".into(),
                title: Some("Home".into()),
                template: ScreenTemplate::Blank,
                link_from: None,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let outcome = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "account-settings".into(),
                title: Some("Account \"advanced\"\nsettings".into()),
                template: ScreenTemplate::Settings,
                link_from: Some("main".into()),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert!(
            outcome
                .files
                .iter()
                .any(|path| path.ends_with("locales/en-US.ftl"))
        );
        assert!(
            outcome
                .files
                .iter()
                .any(|path| path.ends_with("locales/de-DE.ftl"))
        );

        let settings = load_ui_graph(&app.join("ui/account-settings.json")).unwrap();
        assert!(matches!(
            settings.node("notifications"),
            Some(tilo_ui_graph::UiNode::Toggle { value_path, .. })
                if value_path == "account-settings-notifications"
        ));
        assert!(matches!(
            settings.node("save"),
            Some(tilo_ui_graph::UiNode::Button {
                on_click_path: Some(path),
                style: tilo_ui_graph::UiButtonStyle::Primary,
                ..
            }) if path == "account-settings-save-requested"
        ));
        let main = load_ui_graph(&app.join("ui/main.json")).unwrap();
        assert!(matches!(
            main.node("open-account-settings"),
            Some(tilo_ui_graph::UiNode::Button { label, .. })
                if label == "account-settings-title"
        ));

        let source = std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap();
        let translation = std::fs::read_to_string(app.join("locales/de-DE.ftl")).unwrap();
        let source_contract = parse_fluent_catalog(&source).unwrap();
        let translation_contract = parse_fluent_catalog(&translation).unwrap();
        assert!(
            source_contract
                .entries
                .contains_key("account-settings-title")
        );
        assert!(
            source_contract
                .entries
                .contains_key("account-settings-notifications")
        );
        assert!(fluent_contract_drift(&source_contract, &translation_contract).is_empty());

        let report = doctor_app(&app).unwrap();
        assert!(report.is_ready(), "{}", report.summary());
    }
}

/// A malformed translation prevents all screen publication; add screen extends only a generated
/// registry; push and back navigation are wired atomically.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn add_screen_publishes_all_or_nothing_into_generated_registries_only() {
    // Phase: malformed_translation_prevents_all_screen_publication.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        create_app(request(app.clone())).unwrap();
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "de".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        std::fs::write(app.join("locales/de.ftl"), "broken = { $value\n").unwrap();
        let source_before = std::fs::read(app.join("locales/en-US.ftl")).unwrap();
        let library_before = std::fs::read(app.join("src/lib.rs")).unwrap();

        let error = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "settings".into(),
                title: None,
                template: ScreenTemplate::Settings,
                link_from: None,
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("de.ftl"), "{error:#}");
        assert!(!app.join("ui/settings.json").exists());
        assert!(!app.join("src/screens.rs").exists());
        assert_eq!(
            std::fs::read(app.join("src/lib.rs")).unwrap(),
            library_before
        );
        assert_eq!(
            std::fs::read(app.join("locales/en-US.ftl")).unwrap(),
            source_before
        );
    }

    // Phase: add_screen_extends_only_a_generated_registry.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        let mut outcomes = Vec::new();
        for name in ["main", "settings"] {
            outcomes.push(
                add_app_artifact(AddRequest {
                    start: app.clone(),
                    artifact: AddArtifact::Screen {
                        name: name.into(),
                        title: None,
                        template: ScreenTemplate::Blank,
                        link_from: None,
                    },
                    force: false,
                    dry_run: false,
                })
                .unwrap(),
            );
        }
        assert_eq!(outcomes[0].updated.len(), 1);
        assert!(outcomes[0].updated[0].ends_with("src/lib.rs"));
        assert_eq!(outcomes[1].updated.len(), 1);
        assert!(outcomes[1].updated[0].ends_with("src/screens.rs"));
        let registry = std::fs::read_to_string(app.join("src/screens.rs")).unwrap();
        assert_eq!(registry.matches("tilo-app:screen=").count(), 2);
        assert!(registry.contains("\"main\""));
        assert!(registry.contains("\"settings\""));
        assert_eq!(
            std::fs::read_to_string(app.join("src/lib.rs"))
                .unwrap()
                .matches("pub mod screens;")
                .count(),
            1
        );

        let custom = temp.path().join("custom");
        minimal_app(&custom);
        std::fs::write(custom.join("src/screens.rs"), "pub fn custom() {}\n").unwrap();
        let original_library = std::fs::read(custom.join("src/lib.rs")).unwrap();
        assert!(
            add_app_artifact(AddRequest {
                start: custom.clone(),
                artifact: AddArtifact::Screen {
                    name: "new".into(),
                    title: None,
                    template: ScreenTemplate::Blank,
                    link_from: None,
                },
                force: false,
                dry_run: false,
            })
            .unwrap_err()
            .to_string()
            .contains("app-owned")
        );
        assert!(!custom.join("ui/new.json").exists());
        assert_eq!(
            std::fs::read(custom.join("src/lib.rs")).unwrap(),
            original_library
        );
    }

    // Phase: add_screen_can_wire_push_and_back_navigation_atomically.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "main".into(),
                title: Some("Home".into()),
                template: ScreenTemplate::Blank,
                link_from: None,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let outcome = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "account-settings".into(),
                title: Some("Account settings".into()),
                template: ScreenTemplate::Blank,
                link_from: Some("main".into()),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert!(
            outcome
                .updated
                .iter()
                .any(|path| path.ends_with("ui/main.json"))
        );
        let main = load_ui_graph(&app.join("ui/main.json")).unwrap();
        assert!(matches!(
            main.node("open-account-settings"),
            Some(tilo_ui_graph::UiNode::Button {
                navigate: Some(tilo_ui_graph::UiNavigation::Push { route }),
                ..
            }) if route == "account-settings"
        ));
        let settings = load_ui_graph(&app.join("ui/account-settings.json")).unwrap();
        assert!(matches!(
            settings.node("back"),
            Some(tilo_ui_graph::UiNode::Button {
                navigate: Some(tilo_ui_graph::UiNavigation::Back),
                ..
            })
        ));

        let main_before = std::fs::read(app.join("ui/main.json")).unwrap();
        let registry_before = std::fs::read(app.join("src/screens.rs")).unwrap();
        let error = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "account-settings".into(),
                title: None,
                template: ScreenTemplate::Blank,
                link_from: Some("main".into()),
            },
            force: true,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("open-account-settings"));
        assert_eq!(
            std::fs::read(app.join("ui/main.json")).unwrap(),
            main_before
        );
        assert_eq!(
            std::fs::read(app.join("src/screens.rs")).unwrap(),
            registry_before
        );
    }
}

/// The add wizard builds a reviewed linked settings screen, cancels before writing and requires a
/// terminal, reviews and builds a typed form component, creates, registers and mounts a display
/// component, and mounts an interactive form component.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_add_wizard_reviews_before_building_screens_and_components() {
    // Phase: add_wizard_builds_a_reviewed_linked_settings_screen.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "main".into(),
                title: Some("Home".into()),
                template: ScreenTemplate::Blank,
                link_from: None,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let mut input = std::io::Cursor::new(b"\naccount-settings\n\n3\n\n\n".to_vec());
        let mut output = Vec::new();

        let outcome =
            add_app_artifact_interactive_with(&app, &mut input, &mut output, true).unwrap();

        assert!(
            outcome
                .files
                .iter()
                .any(|path| path.ends_with("ui/account-settings.json"))
        );
        let settings = load_ui_graph(&app.join("ui/account-settings.json")).unwrap();
        assert!(settings.node("notifications").is_some());
        assert!(settings.node("back").is_some());
        let main = load_ui_graph(&app.join("ui/main.json")).unwrap();
        assert!(main.node("open-account-settings").is_some());
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Grow "));
        assert!(transcript.contains("3) locale"));
        assert!(transcript.contains("Screen template:"));
        assert!(
            transcript
                .contains("Artifact: screen \"account-settings\" (settings) linked from \"main\"")
        );
        assert!(transcript.contains("Add this artifact [Y/n]:"));
    }

    // Phase: add_wizard_cancels_before_writing_and_requires_a_terminal.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        let mut input = std::io::Cursor::new(b"3\nde-DE\nno\n".to_vec());
        let mut output = Vec::new();

        let error =
            add_app_artifact_interactive_with(&app, &mut input, &mut output, true).unwrap_err();

        assert_eq!(error.to_string(), "artifact creation cancelled");
        assert!(!app.join("locales/de-DE.ftl").exists());
        let noninteractive = add_app_artifact_interactive_with(
            &app,
            &mut std::io::Cursor::new(Vec::<u8>::new()),
            &mut Vec::new(),
            false,
        )
        .unwrap_err();
        assert!(
            noninteractive
                .to_string()
                .contains("subcommand is required")
        );
    }

    // Phase: add_wizard_reviews_and_builds_a_typed_form_component.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        let mut input = std::io::Cursor::new(b"4\nprofile-card\n2\n\n".to_vec());
        let mut output = Vec::new();

        let outcome =
            add_app_artifact_interactive_with(&app, &mut input, &mut output, true).unwrap();

        assert!(app.join("src/components/profile_card.rs").is_file());
        assert!(app.join("src/components/mod.rs").is_file());
        assert_eq!(outcome.next_command.as_deref(), Some("tilo-app test"));
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("4) component"));
        assert!(transcript.contains("Component template:"));
        assert!(transcript.contains("Artifact: component \"profile-card\" (form)"));
    }

    // Phase: add_wizard_can_create_register_and_mount_a_display_component.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_generated_graph_app(&app);
        let mut input = std::io::Cursor::new(b"4\nprofile-card\n1\n\n\n\n\n".to_vec());
        let mut output = Vec::new();

        let outcome =
            add_app_artifact_interactive_with(&app, &mut input, &mut output, true).unwrap();

        assert_eq!(outcome.next_command.as_deref(), Some("tilo-app dev"));
        assert!(matches!(
            load_ui_graph(&app.join("ui/main.json")).unwrap().node("profile-card"),
            Some(tilo_ui_graph::UiNode::Component { name, props, .. })
                if name == "profile-card"
                    && props.get("detail") == Some(&serde_json::json!("Start building here."))
        ));
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Mount in screen"));
        assert!(transcript.contains("mounted in \"main\""));
    }

    // Phase: add_wizard_can_mount_an_interactive_form_component.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_generated_graph_app(&app);
        let mut input = std::io::Cursor::new(
            b"4\nprofile-form\n2\n\nprofile-name\nprofile-enabled\n\n".to_vec(),
        );
        let mut output = Vec::new();

        let outcome =
            add_app_artifact_interactive_with(&app, &mut input, &mut output, true).unwrap();

        assert_eq!(outcome.next_command.as_deref(), Some("tilo-app dev"));
        assert!(matches!(
            load_ui_graph(&app.join("ui/main.json")).unwrap().node("profile-form"),
            Some(tilo_ui_graph::UiNode::Component { name, props, .. })
                if name == "profile-form"
                    && props.get("value_path") == Some(&serde_json::json!("profile-name"))
                    && props.get("enabled_path") == Some(&serde_json::json!("profile-enabled"))
        ));
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Value document path"));
        assert!(transcript.contains("Enabled document path"));
        assert!(transcript.contains("component \"profile-form\" (form) mounted in \"main\""));
    }
}

/// The component wizard exposes the source-agnostic tree, sparse list, sparse table and advanced
/// text templates.
#[test]
fn the_component_wizard_exposes_every_component_template() {
    // Phase: component_wizard_exposes_the_source_agnostic_tree_template.
    {
        let mut input = std::io::Cursor::new(b"3\n".to_vec());
        let mut output = Vec::new();

        let template = prompt_component_template(&mut input, &mut output).unwrap();

        assert_eq!(template, ComponentTemplate::Tree);
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Any TreeModel"));
    }

    // Phase: component_wizard_exposes_the_sparse_list_template.
    {
        let mut input = std::io::Cursor::new(b"4\n".to_vec());
        let mut output = Vec::new();

        let template = prompt_component_template(&mut input, &mut output).unwrap();

        assert_eq!(template, ComponentTemplate::List);
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Any ListModel"));
    }

    // Phase: component_wizard_exposes_the_sparse_table_template.
    {
        let mut input = std::io::Cursor::new(b"5\n".to_vec());
        let mut output = Vec::new();

        let template = prompt_component_template(&mut input, &mut output).unwrap();

        assert_eq!(template, ComponentTemplate::Table);
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Any TableModel"));
    }

    // Phase: component_wizard_exposes_the_advanced_text_template.
    {
        let mut input = std::io::Cursor::new(b"6\n".to_vec());
        let mut output = Vec::new();

        let template = prompt_component_template(&mut input, &mut output).unwrap();

        assert_eq!(template, ComponentTemplate::Text);
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("custom layout engine"));
    }
}

/// add component registers typed accessible Rust without overwriting work; mount validation
/// publishes nothing on failure; generated graph apps auto-register compatible components;
/// template replacement requires compatible graph props; stateful form components mount.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn components_register_and_mount_only_with_compatible_typed_props() {
    // Phase: add_component_registers_typed_accessible_rust_without_overwriting_work.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);

        let display = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "activity-card".into(),
                template: ComponentTemplate::Display,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(display.next_command.as_deref(), Some("tilo-app test"));
        let display_source =
            std::fs::read_to_string(app.join("src/components/activity_card.rs")).unwrap();
        syn::parse_file(&display_source).unwrap();
        assert!(display_source.contains("pub struct ActivityCardProps"));
        assert!(display_source.contains("pub fn activity_card(id: &str"));
        assert!(display_source.contains("pub fn from_ui_graph("));
        assert!(display_source.contains("pub fn register_ui_graph("));
        assert!(display_source.contains("context.with_component(\"activity-card\""));
        assert!(display_source.contains("with_id(format!(\"{id}-title\"))"));
        assert!(display_source.contains("SemanticExpectation::named(\"example-title\")"));

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Form,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let form_source =
            std::fs::read_to_string(app.join("src/components/profile_card.rs")).unwrap();
        syn::parse_file(&form_source).unwrap();
        assert!(form_source.contains("pub struct ProfileCardState"));
        assert!(form_source.contains("TextInput::from_state(&state.value)"));
        assert!(form_source.contains("Checkbox::from_state(&state.enabled)"));
        assert!(form_source.contains("labeled_field(\"profile-card-value-label\""));
        assert!(form_source.contains("perform_named(\"example-value\""));

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "project-tree".into(),
                template: ComponentTemplate::Tree,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let tree_source =
            std::fs::read_to_string(app.join("src/components/project_tree.rs")).unwrap();
        assert_generated_tree_source(&tree_source);

        let registry = std::fs::read_to_string(app.join("src/components/mod.rs")).unwrap();
        assert_eq!(
            parse_component_registry(&registry).unwrap(),
            ["activity-card", "profile-card", "project-tree"]
        );
        assert!(registry.contains("pub mod activity_card;"));
        assert!(registry.contains("pub mod profile_card;"));
        assert!(registry.contains("pub mod project_tree;"));
        let library = std::fs::read_to_string(app.join("src/lib.rs")).unwrap();
        assert_eq!(library.matches("pub mod components;").count(), 1);

        let error = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("already exists"));

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
            },
            force: true,
            dry_run: false,
        })
        .unwrap();
        let replaced = std::fs::read_to_string(app.join("src/components/profile_card.rs")).unwrap();
        assert!(replaced.contains("pub struct ProfileCardProps"));
        let registry = std::fs::read_to_string(app.join("src/components/mod.rs")).unwrap();
        assert_eq!(
            registry
                .matches("// tilo-app:component=profile-card")
                .count(),
            1
        );
    }

    // Phase: graph_component_mount_validation_publishes_nothing.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("graph-app");
        minimal_generated_graph_app(&app);
        let custom_root =
            "fn custom() { let _ = crate::screens::router(\"main\", &Theme::default()); }\n";
        assert_eq!(migrate_generated_graph_context(custom_root), custom_root);
        std::fs::write(
            app.join("src/lib.rs"),
            format!("pub mod screens;\n{custom_root}"),
        )
        .unwrap();
        let custom_error = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
                mount: ComponentMount::display("main", "profile-card"),
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(
            custom_error
                .to_string()
                .contains("intact generated graph app root")
        );
        assert!(!app.join("src/components/profile_card.rs").exists());
        std::fs::write(
            app.join("src/lib.rs"),
            format!("pub mod screens;\n\n{}", legacy_graph_app_template()),
        )
        .unwrap();

        let library_before = std::fs::read(app.join("src/lib.rs")).unwrap();
        let graph_before = std::fs::read(app.join("ui/main.json")).unwrap();
        let dry_run = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
                mount: ComponentMount::display("main", "profile-card"),
            },
            force: false,
            dry_run: true,
        })
        .unwrap();
        assert!(dry_run.dry_run);
        assert!(
            dry_run
                .files
                .iter()
                .any(|path| path.ends_with("ui/main.json"))
        );
        assert!(!app.join("src/components/profile_card.rs").exists());
        assert_eq!(
            std::fs::read(app.join("ui/main.json")).unwrap(),
            graph_before
        );
        let invalid = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
                mount: ComponentMount {
                    props: "[]".into(),
                    ..ComponentMount::display("main", "profile-card")
                },
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(invalid.to_string().contains("must be a JSON object"));
        assert!(!app.join("src/components/profile_card.rs").exists());
        assert_eq!(
            std::fs::read(app.join("src/lib.rs")).unwrap(),
            library_before
        );
        assert_eq!(
            std::fs::read(app.join("ui/main.json")).unwrap(),
            graph_before
        );
    }

    // Phase: generated_graph_apps_auto_register_compatible_components.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("graph-app");
        minimal_generated_graph_app(&app);

        let outcome = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
                mount: ComponentMount::display("main", "profile-card"),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(outcome.next_command.as_deref(), Some("tilo-app dev"));
        assert!(
            outcome
                .files
                .iter()
                .any(|path| path.ends_with("ui/main.json"))
        );
        assert!(matches!(
            load_ui_graph(&app.join("ui/main.json")).unwrap().node("profile-card"),
            Some(tilo_ui_graph::UiNode::Component { name, props, .. })
                if name == "profile-card"
                    && props.get("title") == Some(&serde_json::json!("Profile Card"))
        ));

        let registry_path = app.join("src/components/mod.rs");
        let registry = std::fs::read_to_string(&registry_path).unwrap();
        assert_eq!(
            parse_component_registry(&registry).unwrap(),
            ["profile-card"]
        );
        assert_eq!(
            parse_graph_component_registry(&registry, &["profile-card".into()]).unwrap(),
            BTreeSet::from(["profile-card".to_owned()])
        );
        assert!(registry.contains("profile_card::register_ui_graph(context)"));
        let library = std::fs::read_to_string(app.join("src/lib.rs")).unwrap();
        assert!(library.contains("pub mod components;"));
        assert!(library.contains("crate::screens::router_with("));
        assert!(library.contains("crate::components::ui_graph_context()"));
        assert!(!library.contains("crate::screens::router(\"main\""));
        syn::parse_file(&library).expect("migrated generated root is valid Rust");
    }

    // Phase: component_template_replacement_requires_compatible_graph_props.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("graph-app");
        minimal_generated_graph_app(&app);
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
                mount: ComponentMount::display("main", "profile-card"),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let registry_path = app.join("src/components/mod.rs");
        let component_path = app.join("src/components/profile_card.rs");
        let component_before = std::fs::read(&component_path).unwrap();
        let registry_before = std::fs::read(&registry_path).unwrap();
        let incompatible = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Form,
            },
            force: true,
            dry_run: false,
        })
        .unwrap_err();
        assert!(
            incompatible
                .to_string()
                .contains("incompatible with the form")
        );
        assert_eq!(std::fs::read(&component_path).unwrap(), component_before);
        assert_eq!(std::fs::read(&registry_path).unwrap(), registry_before);

        edit_ui_graph_batch(
            &app.join("ui/main.json"),
            vec![
                UiGraphEdit::SetProperty {
                    id: "profile-card".into(),
                    property: "props.value_path".into(),
                    value: serde_json::json!("profile-name"),
                },
                UiGraphEdit::SetProperty {
                    id: "profile-card".into(),
                    property: "props.enabled_path".into(),
                    value: serde_json::json!("profile-enabled"),
                },
                UiGraphEdit::UnsetProperty {
                    id: "profile-card".into(),
                    property: "props.title".into(),
                },
                UiGraphEdit::UnsetProperty {
                    id: "profile-card".into(),
                    property: "props.detail".into(),
                },
            ],
        )
        .unwrap();
        add_app_artifact(AddRequest {
            start: app,
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Form,
            },
            force: true,
            dry_run: false,
        })
        .unwrap();
        let registry = std::fs::read_to_string(registry_path).unwrap();
        assert_eq!(
            parse_graph_component_registry(&registry, &["profile-card".into()]).unwrap(),
            BTreeSet::from(["profile-card".to_owned()])
        );
        assert!(registry.contains("profile_card::register_ui_graph(context)"));
    }

    // Phase: generated_graph_apps_mount_stateful_form_components.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("graph-form-app");
        let mut new_app = request(app.clone());
        new_app.template = Some(Template::Graph);
        create_app(new_app).unwrap();

        let graph_before = std::fs::read(app.join("ui/main.json")).unwrap();
        let invalid_mount = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "broken-form".into(),
                template: ComponentTemplate::Form,
                mount: ComponentMount {
                    props: serde_json::json!({"value_path": "profile-name", "enabled_path": ""})
                        .to_string(),
                    ..ComponentMount::form("main", "broken-form")
                },
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(
            invalid_mount
                .to_string()
                .contains("requires non-empty string prop \"enabled_path\"")
        );
        assert!(!app.join("src/components/broken_form.rs").exists());
        assert_eq!(
            std::fs::read(app.join("ui/main.json")).unwrap(),
            graph_before
        );

        let outcome = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "profile-form".into(),
                template: ComponentTemplate::Form,
                mount: ComponentMount::form("main", "profile-form"),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        assert_eq!(outcome.next_command.as_deref(), Some("tilo-app dev"));
        assert!(matches!(
            load_ui_graph(&app.join("ui/main.json")).unwrap().node("profile-form"),
            Some(tilo_ui_graph::UiNode::Component { name, props, .. })
                if name == "profile-form"
                    && props.get("value_path") == Some(&serde_json::json!("profile-form-value"))
                    && props.get("enabled_path") == Some(&serde_json::json!("profile-form-enabled"))
        ));
        let source = std::fs::read_to_string(app.join("src/components/profile_form.rs")).unwrap();
        assert!(source.contains("// tilo-app:graph-props=form-v2"));
        assert!(source.contains("TextInput::from_document_string(value_path)"));
        assert!(source.contains("pub fn register_ui_graph("));
        let report = doctor_app(&app).unwrap();
        assert!(report.is_ready(), "{}", report.summary());
        assert!(report.checks.iter().any(|check| {
            check.label == "UI component"
                && check.status == DoctorStatus::Pass
                && check.detail.contains("profile-form")
        }));

        edit_ui_graph(
            &app.join("ui/main.json"),
            UiGraphEdit::SetProperty {
                id: "profile-form".into(),
                property: "props.enabled_path".into(),
                value: serde_json::json!(""),
            },
        )
        .unwrap();
        let invalid = doctor_app(&app).unwrap();
        assert!(!invalid.is_ready());
        assert!(invalid.checks.iter().any(|check| {
            check.label == "UI component"
                && check
                    .detail
                    .contains("requires non-empty string prop \"enabled_path\"")
        }));
    }
}

/// The body of the registry's `ui_graph_context`, one trimmed line per
/// statement.
fn ui_graph_context_body(registry: &str) -> Vec<&str> {
    let (_, body) = registry
        .split_once("pub fn ui_graph_context() -> UiBuildContext {\n")
        .expect("registry defines ui_graph_context");
    let (body, _) = body.split_once("\n}").expect("ui_graph_context closes");
    body.lines().map(str::trim).collect()
}

#[test]
fn component_registry_returns_the_last_registration_directly() {
    // Binding the final context and returning the binding fails
    // `clippy::let_and_return` in every app that compiles the registry; the
    // smoke crate lints the two-component form, these pin the others.
    let empty = render_component_registry_source(&[]).unwrap();
    assert_eq!(ui_graph_context_body(&empty), ["UiBuildContext::default()"]);

    let unmounted =
        render_component_registry_source(&[("project-tree", ComponentTemplate::Tree)]).unwrap();
    assert!(unmounted.contains("pub mod project_tree;"));
    assert_eq!(
        ui_graph_context_body(&unmounted),
        ["UiBuildContext::default()"]
    );

    let one = render_component_registry_source(&[
        ("project-tree", ComponentTemplate::Tree),
        ("profile-card", ComponentTemplate::Display),
    ])
    .unwrap();
    assert_eq!(
        ui_graph_context_body(&one),
        [
            "let context = UiBuildContext::default();",
            "profile_card::register_ui_graph(context)",
        ]
    );

    let two = render_component_registry_source(&[
        ("profile-form", ComponentTemplate::Form),
        ("profile-card", ComponentTemplate::Display),
    ])
    .unwrap();
    assert_eq!(
        ui_graph_context_body(&two),
        [
            "let context = UiBuildContext::default();",
            "let context = profile_card::register_ui_graph(context);",
            "profile_form::register_ui_graph(context)",
        ]
    );
    assert_eq!(
        parse_component_registry(&two).unwrap(),
        ["profile-card", "profile-form"]
    );
}

/// List, table and text components preserve their generic sparse, column and model boundaries and
/// expose the standard polygon and custom engine paths.
#[test]
fn component_templates_keep_their_generic_model_boundaries() {
    // Phase: add_list_component_preserves_the_generic_sparse_model_boundary.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "activity-list".into(),
                template: ComponentTemplate::List,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let source = std::fs::read_to_string(app.join("src/components/activity_list.rs")).unwrap();
        syn::parse_file(&source).unwrap();
        assert!(source.contains("pub struct ActivityListConfig"));
        assert!(source.contains("M: ListModel"));
        assert!(source.contains("VirtualList::new_async"));
        assert!(source.contains("ListRowState<'a>"));
        assert!(source.contains("with_selection(config.selection.clone())"));
        let registry = std::fs::read_to_string(app.join("src/components/mod.rs")).unwrap();
        assert_eq!(
            parse_component_registry(&registry).unwrap(),
            ["activity-list"]
        );
        assert!(registry.contains("pub mod activity_list;"));
    }

    // Phase: add_table_component_preserves_the_generic_column_and_model_boundary.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "results-table".into(),
                template: ComponentTemplate::Table,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let source = std::fs::read_to_string(app.join("src/components/results_table.rs")).unwrap();
        syn::parse_file(&source).unwrap();
        assert!(source.contains("pub struct ResultsTableConfig"));
        assert!(source.contains("M: TableModel<Row = Row>"));
        assert!(source.contains("columns: Vec<ColumnDef<Row>>"));
        assert!(source.contains("DataTable::from_model"));
        assert!(source.contains("with_model_selection(config.selection.clone())"));
        let registry = std::fs::read_to_string(app.join("src/components/mod.rs")).unwrap();
        assert_eq!(
            parse_component_registry(&registry).unwrap(),
            ["results-table"]
        );
        assert!(registry.contains("pub mod results_table;"));
    }

    // Phase: add_text_component_exposes_standard_polygon_and_custom_engine_paths.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "article-body".into(),
                template: ComponentTemplate::Text,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let source = std::fs::read_to_string(app.join("src/components/article_body.rs")).unwrap();
        syn::parse_file(&source).unwrap();
        assert!(source.contains("pub struct ArticleBodyProps"));
        assert!(source.contains("pub fn article_body(id: &str"));
        assert!(source.contains("pub fn article_body_with_engine<E: TextLayoutEngine"));
        assert!(source.contains("pub fn article_body_in_region("));
        assert!(source.contains("PolygonTextLayout::new(region)"));
        assert!(source.contains("with_auto_hyphenation(\"en-US\")"));
    }
}

/// Component generation is dry-run safe and localises template copy, refuses an app-owned registry
/// and Rust keywords, and a generated file batch rolls back an earlier publication.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn component_generation_is_safe_localised_and_transactional() {
    // Phase: component_generation_is_dry_run_safe_and_localizes_template_copy.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("starter");
        create_app(request(app.clone())).unwrap();

        let dry = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Form,
            },
            force: false,
            dry_run: true,
        })
        .unwrap();
        assert!(dry.dry_run);
        assert!(!app.join("src/components/profile_card.rs").exists());
        assert!(!app.join("src/components/mod.rs").exists());

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Form,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let catalog = std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap();
        assert!(catalog.contains("profile-card-value-label ="));
        assert!(catalog.contains("profile-card-value-placeholder ="));
        assert!(catalog.contains("profile-card-enabled-label ="));

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "project-tree".into(),
                template: ComponentTemplate::Tree,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let catalog = std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap();
        assert!(
            catalog.contains("project-tree-accessible-name = { \"Project Tree\" }"),
            "{catalog}"
        );
        assert!(catalog.contains("project-tree-loading = { \"Loading…\" }"));
        assert!(
            catalog.contains("project-tree-load-error = { \"Could not load. Activate to retry\" }")
        );

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "activity-list".into(),
                template: ComponentTemplate::List,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let catalog = std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap();
        assert!(
            catalog.contains("activity-list-accessible-name = { \"Activity List\" }"),
            "{catalog}"
        );

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "results-table".into(),
                template: ComponentTemplate::Table,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let catalog = std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap();
        assert!(
            catalog.contains("results-table-accessible-name = { \"Results Table\" }"),
            "{catalog}"
        );

        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "article-body".into(),
                template: ComponentTemplate::Text,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let catalog = std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap();
        assert!(
            catalog.contains(
                "article-body-body = { \"Replace this copy with application content.\" }"
            ),
            "{catalog}"
        );
    }

    // Phase: component_generation_refuses_app_owned_registry_and_rust_keywords.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        std::fs::create_dir_all(app.join("src/components")).unwrap();
        std::fs::write(app.join("src/components/mod.rs"), "pub mod hand_written;\n").unwrap();

        let owned = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Component {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(owned.to_string().contains("app-owned"));
        assert!(!app.join("src/components/profile_card.rs").exists());

        let keyword = add_app_artifact(AddRequest {
            start: app,
            artifact: AddArtifact::Component {
                name: "type".into(),
                template: ComponentTemplate::Display,
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(keyword.to_string().contains("Rust identifier"));
    }

    // Phase: generated_file_batch_rolls_back_an_earlier_publication.
    {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        std::fs::write(&first, "before\n").unwrap();
        let second = temp.path().join("second.txt");
        let writes = [
            PendingWrite {
                path: first.clone(),
                source: "after\n".into(),
                description: "test artifact",
            },
            PendingWrite {
                path: second.clone(),
                source: "never published\n".into(),
                description: "test artifact",
            },
        ];
        let mut attempt = 0;
        let error = write_batch_with(&writes, |write| {
            attempt += 1;
            if attempt == 2 {
                bail!("injected publication failure");
            }
            write_atomic(&write.path, write.source.as_bytes(), write.description)
        })
        .unwrap_err();
        assert!(error.to_string().contains("rolled back"), "{error:#}");
        assert_eq!(std::fs::read_to_string(first).unwrap(), "before\n");
        assert!(!second.exists());
    }
}

/// Screen scenarios scaffold a reachable semantic workflow and reject an app root without the
/// router; planning is deterministic and prevalidates the suite; a named plan carries its viewport;
/// execution isolates state and accumulates failures.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn screen_scenarios_scaffold_plan_and_run_isolated_workflows() {
    // Phase: screen_scenario_scaffolds_a_reachable_semantic_workflow.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("preferences");
        let mut new_app = request(app.clone());
        new_app.template = Some(Template::Graph);
        create_app(new_app).unwrap();
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "settings".into(),
                title: None,
                template: ScreenTemplate::Settings,
                link_from: Some("main".into()),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let outcome = add_app_artifact(AddRequest {
            start: app,
            artifact: AddArtifact::Scenario {
                name: "settings-smoke".into(),
                screen: Some("settings".into()),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(
            outcome.next_command.as_deref(),
            Some("tilo-app scenario run settings-smoke")
        );
        let source = std::fs::read_to_string(&outcome.files[0]).unwrap();
        let actions = tilo_headless_schema::parse_actions(&source).unwrap();
        assert_eq!(actions.len(), 5);
        let value: serde_json::Value = serde_json::from_str(&source).unwrap();
        let targets = value
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["perform"]["target"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            [
                "open-settings",
                "notifications",
                "compact",
                "analytics",
                "save"
            ]
        );
    }

    // Phase: screen_scenario_rejects_an_app_root_that_does_not_use_the_router.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("starter");
        create_app(request(app.clone())).unwrap();
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Screen {
                name: "settings".into(),
                title: None,
                template: ScreenTemplate::Settings,
                link_from: None,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        let scenario = app.join("tests/scenarios/settings-smoke.json");
        let error = add_app_artifact(AddRequest {
            start: app,
            artifact: AddArtifact::Scenario {
                name: "settings-smoke".into(),
                screen: Some("settings".into()),
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("screens::router"), "{error:#}");
        assert!(!scenario.exists());
    }

    // Phase: scenario_planning_is_deterministic_and_prevalidates_the_suite.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("scenario-app");
        let mut new_app = request(app.clone());
        new_app.template = Some(Template::Graph);
        create_app(new_app).unwrap();
        let scenario_dir = app.join("tests/scenarios");
        std::fs::create_dir_all(&scenario_dir).unwrap();
        std::fs::write(scenario_dir.join("zeta.json"), "[]\n").unwrap();
        std::fs::write(scenario_dir.join("alpha.json"), "[]\n").unwrap();

        let plan = plan_scenario_suite(&ScenarioRunRequest {
            start: app.clone(),
            name: None,
            viewport: None,
            artifacts: None,
            audit: false,
        })
        .unwrap();
        assert_eq!(
            plan.cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            ["default", "alpha", "zeta"]
        );

        std::fs::write(scenario_dir.join("broken.json"), "{not json}\n").unwrap();
        let artifacts = app.join("scenario-artifacts");
        let error = plan_scenario_suite(&ScenarioRunRequest {
            start: app,
            name: None,
            viewport: None,
            artifacts: Some(artifacts.clone()),
            audit: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("broken.json"), "{error:#}");
        assert!(
            !artifacts.exists(),
            "prevalidation must finish before artifact publication"
        );
    }

    // Phase: named_scenario_plan_carries_the_requested_viewport.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("scenario-app");
        let mut new_app = request(app.clone());
        new_app.template = Some(Template::Graph);
        create_app(new_app).unwrap();
        std::fs::create_dir_all(app.join("tests/scenarios")).unwrap();
        std::fs::write(app.join("tests/scenarios/smoke.json"), "[]\n").unwrap();

        let plan = plan_scenario_suite(&ScenarioRunRequest {
            start: app,
            name: Some("smoke".into()),
            viewport: Some((1024, 768)),
            artifacts: Some(PathBuf::from("target/scenario-artifacts")),
            audit: true,
        })
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan.artifact_dir(),
            Some(plan.app_dir.join("target/scenario-artifacts").as_path())
        );
        let payload = plan.cases[0].cargo.args.last().unwrap().to_string_lossy();
        assert!(payload.contains("\"viewport\":[1024,768]"), "{payload}");
        assert!(payload.contains("\"audit\":true"), "{payload}");
        for artifact in [
            "smoke.inspect.json",
            "smoke.targets.json",
            "smoke.document.json",
            "smoke.audit.json",
        ] {
            assert!(payload.contains(artifact), "{payload}");
        }
    }

    // Phase: scenario_execution_isolates_state_and_accumulates_failures.
    {
        let temp = tempfile::tempdir().unwrap();
        let case = |name: &str, script: &str| ScenarioCasePlan {
            name: name.into(),
            source: temp.path().join(format!("{name}.json")),
            cargo: CargoPlan {
                app_dir: temp.path().to_path_buf(),
                program: OsString::from("/bin/sh"),
                args: vec![OsString::from("-c"), OsString::from(script)],
            },
        };
        let plan = ScenarioSuitePlan {
            app_dir: temp.path().to_path_buf(),
            artifact_dir: Some(temp.path().join("artifacts")),
            cases: vec![
                case(
                    "isolated",
                    "test -n \"$XDG_DATA_HOME\" && test -z \"$TILO_WORKSPACE_ID\"",
                ),
                case("failure", "exit 7"),
                case("continued", "exit 0"),
            ],
        };

        let outcome = plan.execute().unwrap();
        assert!(temp.path().join("artifacts").is_dir());
        assert_eq!(outcome.passed, ["isolated", "continued"]);
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].name, "failure");
        assert!(outcome.failed[0].detail.contains('7'));
    }
}

/// add artifact protects work with a side-effect-free dry run and rejects traversal and non-app
/// directories.
#[test]
fn add_artifact_protects_work_and_stays_inside_the_app() {
    // Phase: add_artifact_protects_work_and_dry_run_has_no_side_effects.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        let scenario = || AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Scenario {
                name: "empty-state".into(),
                screen: None,
            },
            force: false,
            dry_run: false,
        };
        let created = add_app_artifact(scenario()).unwrap();
        let created_path = &created.files[0];
        assert_eq!(std::fs::read_to_string(created_path).unwrap(), "[]\n");
        std::fs::write(created_path, "[\"keep me\"]\n").unwrap();
        assert!(add_app_artifact(scenario()).is_err());
        assert_eq!(
            std::fs::read_to_string(created_path).unwrap(),
            "[\"keep me\"]\n"
        );

        std::fs::create_dir_all(app.join("locales")).unwrap();
        std::fs::write(app.join("locales/en-US.ftl"), source_catalog()).unwrap();
        let registry = locale_registry_source(&["en-US".to_owned()]);
        std::fs::write(app.join("src/locales.rs"), &registry).unwrap();
        let preview = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "de-DE".into(),
            },
            force: false,
            dry_run: true,
        })
        .unwrap();
        assert!(preview.dry_run);
        assert!(!preview.files[0].exists());
        assert_eq!(preview.files.len(), 2);
        assert_eq!(
            std::fs::read_to_string(app.join("src/locales.rs")).unwrap(),
            registry
        );
    }

    // Phase: add_artifact_rejects_traversal_and_non_app_directories.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("demo");
        minimal_app(&app);
        for name in ["../escape", "BadName", "two--parts", "with space"] {
            assert!(
                add_app_artifact(AddRequest {
                    start: app.clone(),
                    artifact: AddArtifact::Scenario {
                        name: name.into(),
                        screen: None,
                    },
                    force: false,
                    dry_run: false,
                })
                .is_err(),
                "accepted {name:?}"
            );
        }
        for locale in ["../de", "EN_us", "en--GB", "e"] {
            assert!(
                add_app_artifact(AddRequest {
                    start: app.clone(),
                    artifact: AddArtifact::Locale {
                        locale: locale.into(),
                    },
                    force: false,
                    dry_run: false,
                })
                .is_err(),
                "accepted {locale:?}"
            );
        }

        let crate_only = temp.path().join("ordinary-crate");
        std::fs::create_dir(&crate_only).unwrap();
        std::fs::write(crate_only.join("Cargo.toml"), "[workspace]\n").unwrap();
        assert!(
            add_app_artifact(AddRequest {
                start: crate_only,
                artifact: AddArtifact::Scenario {
                    name: "test".into(),
                    screen: None,
                },
                force: false,
                dry_run: false,
            })
            .unwrap_err()
            .to_string()
            .contains("missing tilo-app.toml")
        );
    }
}

/// add locale registers immediately and protects app-owned setup; the Fluent contract reports
/// missing and stale entries; an invalid source is rejected before publishing; locale sync
/// preserves translations and can resolve drift, validates every catalog before publishing any;
/// only the previous generated setup is migrated.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn locales_are_registered_validated_and_synced_without_losing_translations() {
    // Phase: add_locale_is_immediately_registered_and_protects_app_owned_setup.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        create_app(request(app.clone())).unwrap();

        let outcome = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "de-DE".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(outcome.files.len(), 2);
        assert!(app.join("locales/de-DE.ftl").is_file());
        let generated_catalog = std::fs::read_to_string(app.join("locales/de-DE.ftl")).unwrap();
        assert!(generated_catalog.starts_with("### de-DE translations."));
        assert!(generated_catalog.contains("starter-greeting = Hello, { $name }!"));
        assert!(
            fluent_contract_drift(
                &parse_fluent_catalog(
                    &std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap()
                )
                .unwrap(),
                &parse_fluent_catalog(&generated_catalog).unwrap(),
            )
            .is_empty(),
            "new catalogs begin complete"
        );
        let registry_path = app.join("src/locales.rs");
        let registry = std::fs::read_to_string(&registry_path).unwrap();
        assert_eq!(
            parse_locale_registry(&registry).unwrap(),
            ["de-DE", "en-US"]
        );
        assert!(registry.contains("include_str!(\"../locales/de-DE.ftl\")"));
        assert!(registry.contains("(\"de-DE\", CATALOG_0)"));
        syn::parse_file(&registry).expect("generated locale registry is Rust");

        let repeated = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "de-DE".into(),
            },
            force: true,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(
            repeated.files.len(),
            1,
            "an unchanged registry is not rewritten"
        );
        assert_eq!(
            std::fs::read_to_string(&registry_path)
                .unwrap()
                .matches(LOCALE_ENTRY_PREFIX)
                .count(),
            2
        );
        let source_before = std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap();
        for locale in ["en-US", "de-de"] {
            let error = add_app_artifact(AddRequest {
                start: app.clone(),
                artifact: AddArtifact::Locale {
                    locale: locale.into(),
                },
                force: true,
                dry_run: false,
            })
            .unwrap_err();
            assert!(
                error.to_string().contains(if locale == "en-US" {
                    "source catalog"
                } else {
                    "already registered"
                }),
                "{error:#}"
            );
            if locale == "en-US" {
                assert_eq!(
                    std::fs::read_to_string(app.join("locales/en-US.ftl")).unwrap(),
                    source_before
                );
            } else {
                assert!(!app.join(format!("locales/{locale}.ftl")).exists());
            }
        }

        let app_owned = "pub fn install() {}\n";
        std::fs::write(&registry_path, app_owned).unwrap();
        let error = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "fr".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("app-owned"), "{error:#}");
        assert!(!app.join("locales/fr.ftl").exists());
        assert_eq!(std::fs::read_to_string(registry_path).unwrap(), app_owned);
    }

    // Phase: fluent_contract_reports_missing_and_stale_entries_and_attributes.
    {
        let source = parse_fluent_catalog(
                "title = Hello\n    .aria-label = Greeting\nbutton = Submit\ncount = { $count ->\n    [one] One\n   *[other] { NUMBER($count) }\n}\n-term = Brand\n",
            )
            .unwrap();
        let translation = parse_fluent_catalog(
            "title = Hallo\n    .tooltip = Hinweis\ncount = { $total }\nstale = Alt\n-term = Marke\n",
        )
        .unwrap();
        assert_eq!(
            fluent_contract_drift(&source, &translation),
            [
                "missing button",
                "count variables are $total instead of $count",
                "missing title.aria-label",
                "stale title.tooltip",
                "stale stale",
            ]
        );
    }

    // Phase: add_locale_rejects_an_invalid_source_before_publishing.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        create_app(request(app.clone())).unwrap();
        let registry_path = app.join("src/locales.rs");
        let registry_before = std::fs::read(&registry_path).unwrap();
        std::fs::write(app.join("locales/en-US.ftl"), "broken = { $value\n").unwrap();

        let error = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "fr".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("source catalog"), "{error:#}");
        assert!(!app.join("locales/fr.ftl").exists());
        assert_eq!(std::fs::read(registry_path).unwrap(), registry_before);
    }

    // Phase: locale_sync_preserves_translations_and_can_explicitly_resolve_all_drift.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        create_app(request(app.clone())).unwrap();
        add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "de-DE".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let source_path = app.join("locales/en-US.ftl");
        let mut source = std::fs::read_to_string(&source_path).unwrap();
        source.push_str(
                "settings-title = Settings for { $account }\n    .aria-label = Settings for { $account }\n-brand = Tilo\n",
            );
        std::fs::write(&source_path, source).unwrap();
        let translation_path = app.join("locales/de-DE.ftl");
        let translation = std::fs::read_to_string(&translation_path)
            .unwrap()
            .replace(
                "starter-title = Your first Tilo app",
                "starter-title = Meine Tilo-App",
            )
            .replace(
                "starter-greeting = Hello, { $name }!",
                "starter-greeting = Hallo, { $benutzer }!",
            );
        std::fs::write(&translation_path, format!("{translation}obsolete = Alt\n")).unwrap();

        let preserved = sync_locale_catalogs(&LocaleSyncRequest {
            start: app.clone(),
            locale: Some("de-DE".into()),
            prune: false,
            reset_variables: false,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(preserved.checked, ["de-DE"]);
        assert_eq!(
            preserved.files.as_slice(),
            std::slice::from_ref(&translation_path)
        );
        assert!(
            preserved
                .unresolved
                .iter()
                .any(|difference| difference.contains("$benutzer instead of $name"))
        );
        assert!(
            preserved
                .unresolved
                .iter()
                .any(|difference| difference.contains("stale obsolete"))
        );
        let synchronized = std::fs::read_to_string(&translation_path).unwrap();
        assert!(synchronized.contains("starter-title = Meine Tilo-App"));
        assert!(synchronized.contains("starter-greeting = Hallo, { $benutzer }!"));
        assert!(synchronized.contains("settings-title = Settings for { $account }"));
        assert!(synchronized.contains(".aria-label = Settings for { $account }"));
        assert!(synchronized.contains("-brand = Tilo"));
        assert!(synchronized.contains("obsolete = Alt"));

        let before_dry_run = std::fs::read(&translation_path).unwrap();
        let preview = sync_locale_catalogs(&LocaleSyncRequest {
            start: app.clone(),
            locale: None,
            prune: true,
            reset_variables: true,
            dry_run: true,
        })
        .unwrap();
        assert_eq!(
            preview.files.as_slice(),
            std::slice::from_ref(&translation_path)
        );
        assert!(preview.unresolved.is_empty());
        assert_eq!(std::fs::read(&translation_path).unwrap(), before_dry_run);

        let resolved = sync_locale_catalogs(&LocaleSyncRequest {
            start: app.clone(),
            locale: None,
            prune: true,
            reset_variables: true,
            dry_run: false,
        })
        .unwrap();
        assert!(resolved.unresolved.is_empty());
        let synchronized = std::fs::read_to_string(&translation_path).unwrap();
        assert!(synchronized.contains("starter-title = Meine Tilo-App"));
        assert!(synchronized.contains("starter-greeting = Hello, { $name }!"));
        assert!(!synchronized.contains("obsolete"));
        let report = doctor_app(&app).unwrap();
        assert!(report.is_ready(), "{}", report.summary());
        assert!(report.checks.iter().any(|check| {
            check.label == "locale contract"
                && check.status == DoctorStatus::Pass
                && check.detail.contains("de-DE")
        }));
    }

    // Phase: locale_sync_validates_every_catalog_before_publishing_any.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        create_app(request(app.clone())).unwrap();
        for locale in ["de", "fr"] {
            add_app_artifact(AddRequest {
                start: app.clone(),
                artifact: AddArtifact::Locale {
                    locale: locale.into(),
                },
                force: false,
                dry_run: false,
            })
            .unwrap();
        }
        let source_path = app.join("locales/en-US.ftl");
        let mut source = std::fs::read_to_string(&source_path).unwrap();
        source.push_str("new-unit = New\n");
        std::fs::write(source_path, source).unwrap();
        let de_path = app.join("locales/de.ftl");
        let de_before = std::fs::read(&de_path).unwrap();
        std::fs::write(app.join("locales/fr.ftl"), "broken = { $value\n").unwrap();

        let error = sync_locale_catalogs(&LocaleSyncRequest {
            start: app,
            locale: None,
            prune: false,
            reset_variables: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("fr.ftl"), "{error:#}");
        assert_eq!(
            std::fs::read(de_path).unwrap(),
            de_before,
            "a later parse failure must prevent earlier catalog publication"
        );
    }

    // Phase: add_locale_migrates_only_the_previous_generated_setup.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        create_app(request(app.clone())).unwrap();
        let library_path = app.join("src/lib.rs");
        let current = std::fs::read_to_string(&library_path).unwrap();
        assert_eq!(current.matches(GENERATED_LOCALE_SETUP).count(), 1);
        std::fs::write(
            &library_path,
            current.replacen(GENERATED_LOCALE_SETUP, LEGACY_GENERATED_LOCALE_SETUP, 1),
        )
        .unwrap();
        std::fs::remove_file(app.join("src/locales.rs")).unwrap();

        let migrated = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "it-IT".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(migrated.files.len(), 3);
        let library = std::fs::read_to_string(&library_path).unwrap();
        assert!(library.contains(GENERATED_LOCALE_SETUP));
        assert!(!library.contains("SOURCE_CATALOG"));
        assert_eq!(
            parse_locale_registry(&std::fs::read_to_string(app.join("src/locales.rs")).unwrap())
                .unwrap(),
            ["en-US", "it-IT"]
        );

        std::fs::remove_file(app.join("src/locales.rs")).unwrap();
        let before = std::fs::read_to_string(&library_path).unwrap();
        let error = add_app_artifact(AddRequest {
            start: app.clone(),
            artifact: AddArtifact::Locale {
                locale: "nl".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("app-owned"), "{error:#}");
        assert!(!app.join("locales/nl.ftl").exists());
        assert_eq!(std::fs::read_to_string(library_path).unwrap(), before);
    }
}

/// The UI graph builder round-trips structural edits atomically; property edits are nested, typed
/// and atomic; edit batches mix operations and roll back together; the tools insert, inspect and
/// edit app components.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn ui_graph_edits_are_typed_atomic_and_roll_back_together() {
    // Phase: ui_graph_builder_round_trips_structural_edits_atomically.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let graph = temp.path().join("ui/main.json");
        init_ui_graph(&graph, "Projects", false).expect("initialize graph");
        let initial = load_ui_graph(&graph).expect("load initialized graph");
        assert_eq!(initial.nodes().len(), 3);
        assert_eq!(
            ui_graph_tree(&graph).unwrap(),
            "root (column)\n  title (text)\n  body (text)\n"
        );

        let button =
            parse_ui_node(r#"{"type":"button","id":"create","label":"Create","style":"primary"}"#)
                .expect("button node");
        edit_ui_graph(
            &graph,
            UiGraphEdit::Insert {
                parent: "root".into(),
                index: 2,
                node: button,
            },
        )
        .expect("insert button");
        edit_ui_graph(
            &graph,
            UiGraphEdit::Move {
                id: "create".into(),
                parent: "root".into(),
                index: 1,
            },
        )
        .expect("reorder button");
        assert_eq!(
            load_ui_graph(&graph)
                .unwrap()
                .nodes()
                .iter()
                .map(|node| node.id())
                .collect::<Vec<_>>(),
            ["root", "title", "create", "body"]
        );

        let before_invalid = std::fs::read(&graph).unwrap();
        let duplicate = parse_ui_node(r#"{"type":"spacer","id":"title"}"#).unwrap();
        assert!(
            edit_ui_graph(
                &graph,
                UiGraphEdit::Insert {
                    parent: "root".into(),
                    index: 0,
                    node: duplicate,
                },
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(&graph).unwrap(),
            before_invalid,
            "failed edit must not rewrite the source"
        );

        edit_ui_graph(&graph, UiGraphEdit::Remove { id: "body".into() }).expect("remove body");
        format_ui_graph(&graph).expect("canonical format");
        assert!(load_ui_graph(&graph).unwrap().node("body").is_none());
        assert!(std::fs::read_to_string(&graph).unwrap().ends_with('\n'));
    }

    // Phase: ui_graph_property_edits_are_nested_typed_and_atomic.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let graph = temp.path().join("ui/main.json");
        init_ui_graph(&graph, "Projects", false).expect("initialize graph");

        edit_ui_graph(
            &graph,
            UiGraphEdit::SetProperty {
                id: "title".into(),
                property: "text.value".into(),
                value: serde_json::json!("Recent projects"),
            },
        )
        .expect("set nested title value");
        edit_ui_graph(
            &graph,
            UiGraphEdit::SetProperty {
                id: "title".into(),
                property: "size".into(),
                value: serde_json::json!(24.0),
            },
        )
        .expect("set typed size");
        let source = std::fs::read_to_string(&graph).unwrap();
        assert!(source.contains("Recent projects"));
        assert!(source.contains("\"size\": 24.0"));

        edit_ui_graph(
            &graph,
            UiGraphEdit::UnsetProperty {
                id: "title".into(),
                property: "size".into(),
            },
        )
        .expect("restore schema default");
        assert!(matches!(
            load_ui_graph(&graph).unwrap().node("title"),
            Some(tilo_ui_graph::UiNode::Text { size: None, .. })
        ));

        let before_invalid = std::fs::read(&graph).unwrap();
        let error = edit_ui_graph(
            &graph,
            UiGraphEdit::SetProperty {
                id: "title".into(),
                property: "size".into(),
                value: serde_json::json!("large"),
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("not valid"), "{error:#}");
        assert_eq!(std::fs::read(&graph).unwrap(), before_invalid);

        let typo = edit_ui_graph(
            &graph,
            UiGraphEdit::SetProperty {
                id: "title".into(),
                property: "typo".into(),
                value: serde_json::json!(true),
            },
        )
        .unwrap_err();
        assert!(format!("{typo:#}").contains("not valid"), "{typo:#}");
        assert_eq!(std::fs::read(&graph).unwrap(), before_invalid);
    }

    // Phase: ui_graph_edit_batches_mix_operations_and_roll_back_together.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let graph = temp.path().join("ui/main.json");
        init_ui_graph(&graph, "Projects", false).expect("initialize graph");
        let edits = parse_ui_graph_edits(
                r#"[
                  {"op":"set-property","id":"title","property":"text.value","value":"Recent projects"},
                  {"op":"insert","parent":"root","index":2,"node":{"type":"button","id":"create","label":"Create","style":"primary"}},
                  {"op":"move","id":"create","parent":"root","index":1},
                  {"op":"set-property","id":"create","property":"enabled","value":false},
                  {"op":"set-property","id":"title","property":"size","value":24},
                  {"op":"unset-property","id":"title","property":"size"}
                ]"#,
            )
            .expect("parse mixed edit batch");
        assert_eq!(edits.len(), 6);
        assert!(
            serde_json::to_string(&edits)
                .unwrap()
                .contains("\"op\":\"set-property\"")
        );
        let null_edit = parse_ui_graph_edits(
            r#"[{"op":"set-property","id":"title","property":"size","value":null}]"#,
        )
        .unwrap();
        assert!(matches!(
            null_edit.as_slice(),
            [UiGraphEdit::SetProperty {
                value: serde_json::Value::Null,
                ..
            }]
        ));
        edit_ui_graph_batch(&graph, edits).expect("apply mixed batch");

        let document = load_ui_graph(&graph).unwrap();
        assert_eq!(
            document
                .nodes()
                .iter()
                .map(|node| node.id())
                .collect::<Vec<_>>(),
            ["root", "title", "create", "body"]
        );
        assert!(matches!(
            document.node("create"),
            Some(tilo_ui_graph::UiNode::Button { enabled: false, .. })
        ));
        assert!(matches!(
            document.node("title"),
            Some(tilo_ui_graph::UiNode::Text { size: None, .. })
        ));
        assert!(
            std::fs::read_to_string(&graph)
                .unwrap()
                .contains("Recent projects")
        );

        let before_invalid = std::fs::read(&graph).unwrap();
        let invalid = parse_ui_graph_edits(
            r#"[
                  {"op":"set-property","id":"title","property":"text.value","value":"Do not publish"},
                  {"op":"set-property","id":"title","property":"size","value":"large"}
                ]"#,
        )
        .unwrap();
        let error = edit_ui_graph_batch(&graph, invalid).unwrap_err();
        assert!(error.to_string().contains("edit 2"), "{error:#}");
        assert_eq!(
            std::fs::read(&graph).unwrap(),
            before_invalid,
            "a later failure must roll back the complete batch"
        );
        assert!(parse_ui_graph_edits("[]").is_err());
        assert!(edit_ui_graph_batch(&graph, Vec::new()).is_err());
    }

    // Phase: ui_graph_tools_insert_inspect_and_edit_app_components.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let graph = temp.path().join("ui/main.json");
        init_ui_graph(&graph, "Projects", false).expect("initialize graph");
        let profile = insert_ui_component(
            &graph,
            UiComponentInsert {
                name: "profile-card".into(),
                id: Some("profile".into()),
                parent: "root".into(),
                index: None,
                props: r#"{"title":"Ada","detail":"Owner"}"#.into(),
            },
        )
        .expect("insert component");
        assert_eq!(profile, "profile");
        let second = insert_ui_component(
            &graph,
            UiComponentInsert {
                name: "profile".into(),
                id: None,
                parent: "root".into(),
                index: Some(2),
                props: "{}".into(),
            },
        )
        .expect("insert uniquely named component");
        assert_eq!(second, "profile-2");
        edit_ui_graph(
            &graph,
            UiGraphEdit::SetProperty {
                id: "profile".into(),
                property: "props.title".into(),
                value: serde_json::json!("Grace"),
            },
        )
        .expect("edit component prop");

        assert!(
            ui_graph_tree(&graph)
                .unwrap()
                .contains("profile (component)")
        );
        assert!(matches!(
            load_ui_graph(&graph).unwrap().node("profile"),
            Some(tilo_ui_graph::UiNode::Component { props, .. })
                if props.get("title") == Some(&serde_json::json!("Grace"))
        ));

        let before_invalid = std::fs::read(&graph).unwrap();
        let error = insert_ui_component(
            &graph,
            UiComponentInsert {
                name: "profile-card".into(),
                id: None,
                parent: "root".into(),
                index: None,
                props: "[]".into(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("must be a JSON object"));
        assert_eq!(std::fs::read(&graph).unwrap(), before_invalid);
    }
}

/// The explicit UI builder plan is deterministic, the source-checkout builder manifest is
/// discoverable, and UI graph init protects existing work without force.
#[test]
fn the_ui_builder_is_planned_deterministically_and_found_from_a_checkout() {
    // Phase: explicit_ui_builder_plan_is_deterministic.
    {
        let plan = ui_builder_plan(
            Path::new("ui/main.json"),
            Some(OsString::from("/opt/tilo-ui-builder")),
        )
        .expect("builder plan");
        assert_eq!(plan.program, OsString::from("/opt/tilo-ui-builder"));
        assert_eq!(plan.args, [OsString::from("ui/main.json")]);
    }

    // Phase: source_checkout_builder_manifest_is_discoverable.
    {
        let current = std::env::current_exe().expect("test executable");
        let manifest = find_ui_builder_manifest(Path::new("docs/app-development.md"), &current)
            .expect("manifest search")
            .expect("workspace builder manifest");
        assert!(manifest.ends_with("crates/dev/ui-builder/Cargo.toml"));
    }

    // Phase: ui_graph_init_protects_existing_work_without_force.
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let graph = temp.path().join("screen.json");
        init_ui_graph(&graph, "First", false).unwrap();
        assert_eq!(
            std::fs::metadata(&graph).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let first = std::fs::read(&graph).unwrap();
        assert!(init_ui_graph(&graph, "Second", false).is_err());
        assert_eq!(std::fs::read(&graph).unwrap(), first);
        init_ui_graph(&graph, "Second", true).unwrap();
        assert!(std::fs::read_to_string(&graph).unwrap().contains("Second"));

        std::fs::set_permissions(&graph, std::fs::Permissions::from_mode(0o640)).unwrap();
        format_ui_graph(&graph).unwrap();
        assert_eq!(
            std::fs::metadata(&graph).unwrap().permissions().mode() & 0o777,
            0o640,
            "atomic rewrites preserve source-file permissions"
        );
    }
}

/// A complete starter is created; standalone non-interactive creation discovers the toolkit;
/// discovery follows the executable ancestry; new app preflights workspace registration, publishes
/// the tree and workspace member together, and a staged failure restores both.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn new_apps_are_created_whole_with_their_workspace_registration() {
    // Phase: creates_complete_starter.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("reading-list");
        let outcome = create_app(request(destination.clone())).expect("create app");
        assert_eq!(outcome.app_id, "local.dev.reading.list");
        assert!(destination.join("Cargo.toml").is_file());
        let cargo = std::fs::read_to_string(destination.join("Cargo.toml")).unwrap();
        let cargo: toml::Value = toml::from_str(&cargo).unwrap();
        let shader_path = cargo["patch"]["crates-io"]["vello_shaders"]["path"]
            .as_str()
            .unwrap();
        assert!(destination.join(shader_path).join("Cargo.toml").is_file());
        assert!(destination.join("src/main.rs").is_file());
        assert!(destination.join("tests/app.rs").is_file());
        assert!(destination.join("tests/headless-actions.json").is_file());
        let catalog =
            std::fs::read_to_string(destination.join("locales/en-US.ftl")).expect("catalog");
        assert!(catalog.contains("starter-greeting = Hello, { $name }!"));
        let main = std::fs::read_to_string(destination.join("src/main.rs")).expect("main");
        assert!(main.contains("ui::framework::run::<reading_list::App>()"));
        let library = std::fs::read_to_string(destination.join("src/lib.rs")).expect("library");
        assert!(library.contains("pub const APP_ID: &str = \"local.dev.reading.list\";"));
        assert!(library.contains("#[derive(Model, Debug, Clone, PartialEq)]"));
        assert!(library.contains("#[resource(kind = \"generated.profile\""));
        assert!(library.contains("impl ResourceApp for App"));
        assert!(library.contains(".input(|profile| profile.name.clone(), Profile::SET_NAME)"));
        assert!(library.contains("with_id(\"name-input\")"));
        assert!(library.contains(".checkbox("));
        assert!(library.contains("NumberInput::from_state(value)"));
        assert!(library.contains("Slider::from_state,"));
        assert!(library.contains("Dropdown::from_state("));
        assert!(library.contains("labeled_field(\"starter-name-label\", name)"));
        assert!(!library.contains("LocalState::new"));
        assert!(!library.contains("ActionForm"));
        assert!(library.contains("pub mod locales;"));
        assert!(library.contains("locales::install()"));
        let locale_registry =
            std::fs::read_to_string(destination.join("src/locales.rs")).expect("locale registry");
        assert!(locale_registry.contains("I18nManager::install_system"));
        assert_eq!(parse_locale_registry(&locale_registry).unwrap(), ["en-US"]);
        let manifest =
            std::fs::read_to_string(destination.join("tilo-app.toml")).expect("manifest");
        assert!(manifest.contains("mode = \"off\""));
        let app_test =
            std::fs::read_to_string(destination.join("tests/app.rs")).expect("generated app test");
        assert!(app_test.contains("use reading_list::{App, Profile};"));
        assert!(!app_test.contains("generated_app"));
        assert!(app_test.contains("TestApp::<App>::new()"));
        assert!(app_test.contains("Replica::<Profile>::new()"));
        assert!(app_test.contains("perform_named(\"name-input\""));
        assert!(app_test.contains("SemanticExpectation::named(\"greeting\")"));
        assert!(app_test.contains("perform_named(\"volume-slider\""));
        assert!(app_test.contains("with_role(ui::Role::Viewer)"));
        assert!(app_test.contains("app.undo()"));
        assert!(app_test.contains("reopen()"));

        let report = doctor_app(&destination).expect("doctor generated app");
        assert!(report.is_ready(), "{}", report.summary());
        assert!(report.summary().contains("Ready for `tilo-app run`"));
    }

    // Phase: standalone_noninteractive_creation_discovers_the_toolkit.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("scripted-app");
        let mut req = request(destination.clone());
        req.name = Some("scripted-app".into());
        req.ui_path = None;

        let outcome = create_app_from(temp.path(), req).expect("standalone scaffold");

        assert_eq!(outcome.destination, destination);
        assert!(destination.join("Cargo.toml").is_file());
        assert!(doctor_app(&destination).unwrap().is_ready());
    }

    // Phase: toolkit_discovery_follows_the_tilo_app_cli_executable_ancestry.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let ui = temp.path().join("crates/sdk/ui-core");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::write(ui.join("Cargo.toml"), "[workspace]\n").unwrap();

        assert_eq!(
            ui_dir_from_ancestor(&temp.path().join("target/debug/tilo-app")),
            Some(ui)
        );
    }

    // Phase: new_app_preflights_workspace_registration_before_creating_files.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        std::fs::create_dir(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("crates/sdk/ui-core")).unwrap();
        std::fs::write(
            root.join("crates/sdk/ui-core/Cargo.toml"),
            "[package]\nname = \"ui\"\n",
        )
        .unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nresolver = \"3\"\n").unwrap();
        let destination = root.join("apps/reading-list");
        let error = create_app_from(root, request(destination.clone())).unwrap_err();
        assert!(error.to_string().contains("members list"), "{error:#}");
        assert!(
            !destination.exists(),
            "workspace failure must not leave a partial app"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("Cargo.toml")).unwrap(),
            "[workspace]\nresolver = \"3\"\n"
        );
    }

    // Phase: new_app_publishes_complete_tree_and_workspace_member_together.
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        std::fs::create_dir_all(root.join("apps/reading-list")).unwrap();
        std::fs::set_permissions(
            root.join("apps/reading-list"),
            std::fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/sdk/ui-core")).unwrap();
        std::fs::write(
            root.join("crates/sdk/ui-core/Cargo.toml"),
            "[package]\nname = \"ui\"\n",
        )
        .unwrap();
        std::fs::write(
                root.join("Cargo.toml"),
                "[package.metadata.fixture]\nmembers = [\n    \"do-not-touch\",\n]\n\n[workspace]\nresolver = \"3\"\ndefault-members = [\n    \"core\",\n]\nmembers = [\n]\n",
            )
            .unwrap();
        let destination = root.join("apps/reading-list");

        let outcome = create_app_from(root, request(destination.clone())).unwrap();
        assert_eq!(outcome.destination, destination);
        for relative in &outcome.files {
            assert!(
                destination.join(relative).is_file(),
                "missing {}",
                relative.display()
            );
        }
        let workspace = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            workspace
                .starts_with("[package.metadata.fixture]\nmembers = [\n    \"do-not-touch\",\n]")
        );
        assert!(workspace.contains(
            "default-members = [\n    \"core\",\n]\nmembers = [\n    \"apps/reading-list\","
        ));
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::read_dir(root.join("apps")).unwrap().count(),
            1,
            "the staging directory must not remain visible"
        );
    }

    // Phase: staged_new_app_failure_restores_workspace_and_empty_destination.
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        let manifest = root.join("Cargo.toml");
        let original_manifest = "[workspace]\nmembers = [\n]\n";
        std::fs::write(&manifest, original_manifest).unwrap();
        let update = prepare_workspace_member(root, Path::new("apps/demo"))
            .unwrap()
            .expect("new workspace member");

        let staging = tempfile::tempdir_in(root).unwrap();
        std::fs::write(staging.path().join("Cargo.toml"), "generated").unwrap();
        let destination = root.join("apps/demo");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o750)).unwrap();
        let original_permissions = std::fs::metadata(&destination).unwrap().permissions();

        let error = publish_staged_app_with(
            staging.path(),
            &destination,
            Some(&original_permissions),
            Some(&update),
            |_, _| bail!("injected destination publication failure"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("rolled back"), "{error:#}");
        assert_eq!(
            std::fs::read_to_string(&manifest).unwrap(),
            original_manifest
        );
        assert!(destination.is_dir());
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert!(staging.path().join("Cargo.toml").is_file());
    }
}

/// The interactive wizard retries invalid fields and builds the selected template, declines before
/// publishing any files, prompts and retries the toolkit path before review, accepts the discovered
/// toolkit default, and reports end of input as cancellation.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_interactive_wizard_retries_declines_and_cancels_cleanly() {
    // Phase: interactive_wizard_retries_invalid_fields_and_builds_the_selected_template.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("reading-list");
        let mut req = request(destination.clone());
        req.name = None;
        req.title = None;
        req.app_id = None;
        req.template = None;
        let mut input = std::io::Cursor::new(
            b"Bad Name\nreading-list\n\nbad-id\norg.example.reading-list\n10\n4\n\n".to_vec(),
        );
        let mut output = Vec::new();

        let outcome = create_app_interactive_with(req, &mut input, &mut output, true).unwrap();
        assert_eq!(outcome.package_name, "reading-list");
        assert_eq!(outcome.app_id, "org.example.reading-list");
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Create a Tilo app"));
        assert!(transcript.contains("invalid package name \"Bad Name\""));
        assert!(transcript.contains("invalid app id \"bad-id\""));
        assert!(transcript.contains("1) starter"));
        assert!(transcript.contains("5) graph"));
        assert!(transcript.contains("unknown template \"10\""));
        assert!(transcript.contains("Review:"));
        assert!(transcript.contains("Template:    tree — Professional hierarchy browser"));
        assert!(transcript.contains(&format!("Destination: {}", destination.display())));
        assert!(transcript.contains("Create this app [Y/n]:"));
        let source = std::fs::read_to_string(destination.join("src/lib.rs")).unwrap();
        assert!(source.contains("TreeView::new"), "{source}");
    }

    // Phase: interactive_wizard_declines_before_publishing_any_files.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("notes");
        let mut req = request(destination.clone());
        req.name = None;
        req.title = None;
        req.app_id = None;
        req.template = None;
        let mut input = std::io::Cursor::new(b"notes\n\n\n1\nmaybe\nno\n".to_vec());
        let mut output = Vec::new();

        let error = create_app_interactive_with(req, &mut input, &mut output, true).unwrap_err();

        assert_eq!(error.to_string(), "app creation cancelled");
        assert!(!destination.exists());
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Review:"));
        assert!(transcript.contains("answer `yes` or `no`"));
    }

    // Phase: standalone_wizard_prompts_and_retries_the_toolkit_path_before_review.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let toolkit = temp.path().join("toolkit-ui");
        std::fs::create_dir(&toolkit).unwrap();
        std::fs::write(
            toolkit.join("Cargo.toml"),
            "[package]\nname = \"ui\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let sdk = temp.path().join("crates/sdk/tilo-app");
        std::fs::create_dir_all(&sdk).unwrap();
        std::fs::write(
            sdk.join("Cargo.toml"),
            "[package]\nname = \"tilo-app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let destination = temp.path().join("notes");
        let mut req = request(destination.clone());
        req.name = None;
        req.title = None;
        req.app_id = None;
        req.template = None;
        let mut input = std::io::Cursor::new(b"notes\n\n\n1\nmissing-ui\ntoolkit-ui\n\n".to_vec());
        let mut output = Vec::new();

        let outcome =
            create_app_interactive_from(temp.path(), req, &mut input, &mut output, true).unwrap();

        assert_eq!(outcome.destination, destination);
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Toolkit ui path, or `git` for the Tilo repository ["));
        assert!(transcript.contains("invalid toolkit path"));
        assert!(transcript.contains(&format!(
            "Toolkit:     {}",
            toolkit.canonicalize().unwrap().display()
        )));
        let manifest = std::fs::read_to_string(destination.join("Cargo.toml")).unwrap();
        assert!(
            manifest.contains("path = \"../crates/sdk/tilo-app\""),
            "{manifest}"
        );
    }

    // Phase: standalone_wizard_accepts_the_discovered_toolkit_default.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("notes");
        let discovered = discover_ui_dir(temp.path(), None)
            .expect("tilo-app source toolkit")
            .canonicalize()
            .unwrap();
        let mut req = request(destination.clone());
        req.name = None;
        req.title = None;
        req.app_id = None;
        req.template = None;
        let mut input = std::io::Cursor::new(b"notes\n\n\n1\n\n\n".to_vec());
        let mut output = Vec::new();

        let outcome =
            create_app_interactive_from(temp.path(), req, &mut input, &mut output, true).unwrap();

        assert_eq!(outcome.destination, destination);
        let transcript = String::from_utf8(output).unwrap();
        assert!(
            transcript.contains(&format!(
                "Toolkit ui path, or `git` for the Tilo repository [{}]:",
                discovered.display()
            )),
            "{transcript}"
        );
        assert!(transcript.contains(&format!("Toolkit:     {}", discovered.display())));
        let manifest = std::fs::read_to_string(destination.join("Cargo.toml")).unwrap();
        let sdk = sdk_path_for_ui(&discovered);
        let relative = relative_path(&destination, &sdk);
        assert!(
            manifest.contains(&format!("path = {:?}", relative.display().to_string())),
            "{manifest}"
        );
    }

    // Phase: standalone_wizard_answers_git_to_build_against_the_tilo_repository.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("notes");
        let mut req = request(destination.clone());
        req.name = None;
        req.title = None;
        req.app_id = None;
        req.template = None;
        let mut input = std::io::Cursor::new(b"notes\n\n\n1\ngit\n\n".to_vec());
        let mut output = Vec::new();

        let outcome =
            create_app_interactive_from(temp.path(), req, &mut input, &mut output, true).unwrap();

        let transcript = String::from_utf8(output).unwrap();
        assert!(
            transcript.contains("App id [local.dev.notes]:"),
            "{transcript}"
        );
        assert!(
            transcript.contains(&format!("Toolkit:     {}", standalone::DEFAULT_SDK_GIT)),
            "{transcript}"
        );
        assert!(matches!(
            outcome.sdk,
            Some(standalone::SdkSource::Git { ref url, .. }) if url == standalone::DEFAULT_SDK_GIT
        ));
        let manifest = std::fs::read_to_string(destination.join("Cargo.toml")).unwrap();
        assert!(manifest.contains(standalone::DEFAULT_SDK_GIT), "{manifest}");
    }

    // Phase: interactive_wizard_reports_end_of_input_as_cancellation.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut req = request(temp.path().join("cancelled"));
        req.name = None;
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = create_app_interactive_with(req, &mut input, &mut output, true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("input ended while reading Package name"),
            "{error:#}"
        );
        assert!(!temp.path().join("cancelled").exists());
    }
}

/// Doctor accumulates actionable failures, accepts registered document actions without widget
/// glue, finds the SDK in target-specific dependencies, rejects one invalid target-specific UI
/// dependency, and never asks for the Cap'n Proto compiler the SDK no longer needs.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn doctor_reports_actionable_failures_in_app_manifests() {
    // Phase: doctor_accumulates_actionable_failures.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"broken\"\nversion = \"0.1.0\"\n",
        )
        .expect("Cargo manifest");
        std::fs::write(temp.path().join("tilo-app.toml"), "not valid toml = [")
            .expect("Tilo manifest");

        let report = doctor_app(temp.path()).expect("doctor report");
        assert!(!report.is_ready());
        assert!(
            report
                .checks
                .iter()
                .filter(|check| check.status == DoctorStatus::Fail)
                .count()
                >= 4,
            "{:#?}",
            report.checks
        );
        let summary = report.summary();
        assert!(summary.contains("missing dependency `ui`"));
        assert!(summary.contains("Not ready"));
    }

    // Phase: doctor_accepts_registered_document_actions_without_widget_operation_glue.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(temp.path().join("src")).expect("source directory");
        std::fs::write(
            temp.path().join("src/lib.rs"),
            r#"
                    fn resources() {
                        let _ = ActionSpec::immediate("clear", "Clear", "doc.action", &());
                    }
                "#,
        )
        .expect("document-only resource source");
        let mut checks = Vec::new();
        check_resource_action_surface(temp.path(), &mut checks);
        assert_eq!(checks[0].status, DoctorStatus::Pass, "{checks:#?}");
    }

    // Phase: doctor_finds_tilo_app_sdk_in_target_specific_dependencies.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let app = temp.path().join("app");
        let sdk = temp.path().join("sdk");
        std::fs::create_dir_all(&app).expect("app directory");
        std::fs::create_dir_all(&sdk).expect("SDK directory");
        std::fs::write(
            sdk.join("Cargo.toml"),
            "[package]\nname = \"tilo-app\"\nversion = \"0.1.0\"\n",
        )
        .expect("SDK manifest");
        let manifest = r#"
                [package]
                name = "target-app"
                version = "0.1.0"

                [target.'cfg(unix)'.dependencies]
                ui = { package = "tilo-app", path = "../sdk" }
            "#
        .parse::<toml::Table>()
        .expect("target-specific manifest");
        let mut checks = Vec::new();
        check_ui_dependency(&app, &manifest, &mut checks);
        assert_eq!(checks[0].status, DoctorStatus::Pass, "{checks:#?}");
        assert!(checks[0].detail.contains("sdk"));
    }

    // Phase: doctor_rejects_one_invalid_target_specific_ui_dependency.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let manifest = r#"
                [package]
                name = "target-app"
                version = "0.1.0"

                [target.'cfg(unix)'.dependencies]
                ui = { package = "not-tilo-app", path = "../sdk" }

                [target.'cfg(windows)'.dependencies]
                ui = { package = "tilo-app", version = "1" }
            "#
        .parse::<toml::Table>()
        .expect("target-specific manifest");
        let mut checks = Vec::new();
        check_ui_dependency(temp.path(), &manifest, &mut checks);
        assert_eq!(checks[0].status, DoctorStatus::Fail, "{checks:#?}");
        assert!(checks[0].detail.contains("not-tilo-app"));
    }

    // Phase: doctor_does_not_require_the_capnp_compiler.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("reading-list");
        create_app(request(destination.clone())).unwrap();

        let report = doctor_app(&destination).expect("doctor report");

        assert!(
            report
                .checks
                .iter()
                .all(|check| !check.label.contains("Cap'n Proto")),
            "{:#?}",
            report.checks
        );
    }
}

/// Doctor validates generated screens, locales and scenarios, preflights graph component factories
/// and typed props, marks app-owned component contexts unverifiable, and keeps the generated form v1
/// props contract readable.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn doctor_validates_generated_screens_components_and_contracts() {
    // Phase: doctor_validates_generated_screens_locales_and_scenarios.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("project-maker");
        let mut new_app = request(destination.clone());
        new_app.template = Some(Template::Graph);
        create_app(new_app).unwrap();
        add_app_artifact(AddRequest {
            start: destination.clone(),
            artifact: AddArtifact::Locale {
                locale: "de-DE".into(),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();
        add_app_artifact(AddRequest {
            start: destination.clone(),
            artifact: AddArtifact::Scenario {
                name: "empty-state".into(),
                screen: None,
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let healthy = doctor_app(&destination).unwrap();
        assert!(healthy.is_ready(), "{}", healthy.summary());
        assert!(
            healthy
                .checks
                .iter()
                .any(|check| check.label == "UI graph" && check.detail == "ui/main.json")
        );
        assert!(healthy.checks.iter().any(|check| {
            check.label == "locale registry" && check.detail == "2 embedded catalog(s)"
        }));

        let translated_path = destination.join("locales/de-DE.ftl");
        let complete_translation = std::fs::read_to_string(&translated_path).unwrap();
        std::fs::write(
            &translated_path,
            complete_translation.replace("graph-create = Create\n", ""),
        )
        .unwrap();
        let drifted = doctor_app(&destination).unwrap();
        assert!(
            drifted.is_ready(),
            "source fallback keeps a structurally valid app runnable"
        );
        assert!(drifted.checks.iter().any(|check| {
            check.label == "locale contract"
                && check.status == DoctorStatus::Warning
                && check.detail.contains("missing graph-create")
        }));
        std::fs::write(&translated_path, complete_translation).unwrap();

        std::fs::write(destination.join("ui/main.json"), "{not json\n").unwrap();
        std::fs::write(
            destination.join("locales/de-DE.ftl"),
            "broken = { $missing\n",
        )
        .unwrap();
        std::fs::write(
            destination.join("tests/scenarios/empty-state.json"),
            r#"[{"unknown":"operation"}]"#,
        )
        .unwrap();

        let broken = doctor_app(&destination).unwrap();
        assert!(!broken.is_ready());
        let summary = broken.summary();
        assert!(summary.contains("ui/main.json"), "{summary}");
        assert!(summary.contains("locales/de-DE.ftl"), "{summary}");
        assert!(summary.contains("unknown action"), "{summary}");
        assert!(broken.checks.iter().any(|check| {
            check.label == "headless scenario" && check.status == DoctorStatus::Warning
        }));
    }

    // Phase: doctor_preflights_graph_component_factories_and_typed_props.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("component-app");
        let mut new_app = request(destination.clone());
        new_app.template = Some(Template::Graph);
        create_app(new_app).unwrap();
        add_app_artifact(AddRequest {
            start: destination.clone(),
            artifact: AddArtifact::GraphComponent {
                name: "profile-card".into(),
                template: ComponentTemplate::Display,
                mount: ComponentMount::display("main", "profile-card"),
            },
            force: false,
            dry_run: false,
        })
        .unwrap();

        let healthy = doctor_app(&destination).unwrap();
        assert!(healthy.is_ready(), "{}", healthy.summary());
        assert!(healthy.checks.iter().any(|check| {
            check.label == "UI component"
                && check.status == DoctorStatus::Pass
                && check.detail.contains("profile-card")
        }));

        let graph = destination.join("ui/main.json");
        edit_ui_graph(
            &graph,
            UiGraphEdit::SetProperty {
                id: "profile-card".into(),
                property: "props.detail".into(),
                value: serde_json::json!(42),
            },
        )
        .unwrap();
        let invalid_props = doctor_app(&destination).unwrap();
        assert!(!invalid_props.is_ready());
        assert!(invalid_props.checks.iter().any(|check| {
            check.label == "UI component"
                && check.detail.contains("requires string prop \"detail\"")
        }));
        edit_ui_graph(
            &graph,
            UiGraphEdit::SetProperty {
                id: "profile-card".into(),
                property: "props.detail".into(),
                value: serde_json::json!("Owner"),
            },
        )
        .unwrap();

        let registry_path = destination.join("src/components/mod.rs");
        let registry = std::fs::read_to_string(&registry_path).unwrap();
        std::fs::write(
            &registry_path,
            registry.replace("// tilo-app:graph-component=profile-card\n", ""),
        )
        .unwrap();
        let missing_factory = doctor_app(&destination).unwrap();
        assert!(!missing_factory.is_ready());
        assert!(missing_factory.checks.iter().any(|check| {
            check.label == "UI component" && check.detail.contains("has no graph factory")
        }));
    }

    // Phase: doctor_marks_app_owned_component_contexts_as_unverifiable.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("custom-context");
        minimal_app(&app);
        std::fs::create_dir(app.join("ui")).unwrap();
        let document = tilo_ui_graph::UiDocument::new(tilo_ui_graph::UiNode::Component {
            id: "custom".into(),
            name: "custom-card".into(),
            props: serde_json::Map::new(),
        });
        std::fs::write(app.join("ui/main.json"), document.to_json_pretty().unwrap()).unwrap();
        std::fs::write(
                app.join("src/lib.rs"),
                "pub fn app_root() { let _context = ui::UiBuildContext::default().with_component(\"custom-card\", |_| unreachable!()); }\n",
            )
            .unwrap();

        let report = doctor_app(&app).unwrap();
        assert!(report.checks.iter().any(|check| {
            check.label == "UI components"
                && check.status == DoctorStatus::Warning
                && check.detail.contains("app-owned build context")
        }));
        assert!(!report.checks.iter().any(|check| {
            check.label == "component registry" && check.status == DoctorStatus::Fail
        }));
    }

    // Phase: doctor_keeps_the_generated_form_v1_props_contract_readable.
    {
        let contract = graph_props_contract(
            "legacy-form",
            "// tilo-app:graph-props=form-v1\npub fn register_ui_graph() {}\n",
        )
        .unwrap();
        assert_eq!(contract, GraphPropsContract::FormV1);
        let props = serde_json::json!({"value": "Ada", "enabled": true});
        assert!(contract.invalid_props(props.as_object().unwrap()).is_none());
    }
}

/// A dry run writes nothing; the summary prints copyable commands for shell-unsafe paths;
/// ambiguous or unsafe names are rejected; the workspace-relative UI path is computed; the template
/// prompt accepts names and shortcuts.
#[test]
fn creation_inputs_are_validated_and_printed_safely() {
    // Phase: dry_run_writes_nothing.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("preview");
        let mut req = request(destination.clone());
        req.dry_run = true;
        let outcome = create_app(req).expect("preview app");
        assert!(outcome.dry_run);
        assert!(!destination.exists());
        assert_eq!(outcome.files.len(), 13);
    }

    // Phase: creation_summary_prints_copyable_commands_for_shell_unsafe_paths.
    {
        let outcome = CreateOutcome {
            destination: PathBuf::from("/tmp/O'Reilly app"),
            package_name: "reading-list".into(),
            app_id: "org.example.reading-list".into(),
            files: Vec::new(),
            dry_run: false,
            workspace_member: false,
            sdk: None,
        };
        let summary = outcome.summary();
        assert!(summary.contains("tilo-app check '/tmp/O'\\''Reilly app'"));
        assert!(summary.contains("tilo-app dev '/tmp/O'\\''Reilly app'"));
        assert!(summary.contains("Start developing:"));
        assert!(summary.contains("Verify everything:"));
        assert!(summary.contains("tilo-app --help"));
    }

    // Phase: rejects_ambiguous_or_unsafe_names.
    {
        for name in ["BadName", "-leading", "two--hyphens", "with space", ""] {
            assert!(validate_package_name(name).is_err(), "accepted {name:?}");
        }
    }

    // Phase: computes_workspace_relative_ui_path.
    {
        assert_eq!(
            relative_path(
                Path::new("/repo/apps/demo"),
                Path::new("/repo/crates/sdk/ui-core")
            ),
            PathBuf::from("../../crates/sdk/ui-core")
        );
    }

    // Phase: template_prompt_accepts_names_and_shortcuts.
    {
        assert_eq!(parse_template("1").unwrap(), Template::Starter);
        assert_eq!(parse_template("starter").unwrap(), Template::Starter);
        assert_eq!(parse_template("S").unwrap(), Template::Starter);
        assert_eq!(parse_template("data").unwrap(), Template::Data);
        assert_eq!(parse_template("3").unwrap(), Template::List);
        assert_eq!(parse_template("d").unwrap(), Template::Data);
        assert_eq!(parse_template("list").unwrap(), Template::List);
        assert_eq!(parse_template("L").unwrap(), Template::List);
        assert_eq!(parse_template("tree").unwrap(), Template::Tree);
        assert_eq!(parse_template("T").unwrap(), Template::Tree);
        assert_eq!(parse_template("graph").unwrap(), Template::Graph);
        assert_eq!(parse_template("5").unwrap(), Template::Graph);
        assert_eq!(parse_template("builder").unwrap(), Template::Graph);
        assert_eq!(parse_template("async").unwrap(), Template::Async);
        assert_eq!(parse_template("6").unwrap(), Template::Async);
        assert_eq!(parse_template("task").unwrap(), Template::Async);
        assert_eq!(parse_template("document").unwrap(), Template::Document);
        assert_eq!(parse_template("7").unwrap(), Template::Document);
        assert_eq!(parse_template("editor").unwrap(), Template::Document);
        assert_eq!(parse_template("qr").unwrap(), Template::Qr);
        assert_eq!(parse_template("8").unwrap(), Template::Qr);
        assert_eq!(parse_template("showcase").unwrap(), Template::Qr);
        assert_eq!(parse_template("9").unwrap(), Template::Workspace);
        assert_eq!(
            parse_template("master-detail").unwrap(),
            Template::Workspace
        );
        assert!(parse_template("dashboard").is_err());
    }
}

/// The data, QR, workspace, graph, list, tree, async and document templates each compose the
/// toolkit patterns they are meant to demonstrate.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn each_template_composes_the_toolkit_patterns_it_teaches() {
    // Phase: data_template_renders_cells_and_uses_stable_row_ids.
    {
        let spec = AppSpec {
            package_name: "log-browser".into(),
            crate_name: "log_browser".into(),
            title: "Log Browser".into(),
            app_id: "so.tilo.log.browser".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::Data,
        };
        let source = app_lib(&spec);
        assert!(source.contains("with_text(|row| row.name.clone())"));
        assert!(source.contains("with_id(\"rows-table\")"));
        assert!(source.contains("with_accessible_name(\"data-accessible-name\")"));
        assert!(source.contains("impl TableModel for Rows"));
        assert!(source.contains("const ROW_COUNT: usize = 100_000_000"));
        assert!(source.contains("LatestRequest::new()"));
        assert!(source.contains("std::thread::spawn"));
        assert!(source.contains("fn poll_changes"));
        assert!(source.contains("TableModelChange::RowsChanged"));
        assert!(source.contains("completion.complete(rows)"));
        assert!(source.contains("with_widget"));
        assert!(source.contains("let actions = cx.actions();"));
        assert!(source.contains("Review::OPEN.with(row.id)"));
        assert!(source.contains("pub id: u64"));
        let test = app_test(&spec);
        assert!(test.contains("Role::Table"));
        assert!(test.contains("row:3:0"));
        assert!(test.contains("wait_for_named"));
        assert!(test.contains("only the sparse viewport is semantic"));
        assert!(test.contains("app.state().opened"));
    }

    // Phase: qr_template_composes_ime_input_state_visual_output_and_accessibility.
    {
        let spec = AppSpec {
            package_name: "qr-code".into(),
            crate_name: "qr_code".into(),
            title: "QR Code".into(),
            app_id: "so.tilo.qr.code".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::Qr,
        };
        let source = app_lib(&spec);
        assert!(source.contains(".input(|code| code.payload.clone(), Code::SET_PAYLOAD)"));
        assert!(source.contains("cx.value(|code| code.payload.clone())"));
        assert!(source.contains("ModelView::from_state"));
        assert!(source.contains("QrWidget::new"));
        assert!(source.contains("with_id(\"qr-output\")"));
        assert!(source.contains("with_alt(alt)"));
        let test = app_test(&spec);
        assert!(test.contains("你好 Rust GUI"));
        assert!(test.contains("Role::Image"));
        assert!(test.contains("name_contains"));
        assert!(app_readme(&spec).contains("payload-specific alternative text"));
    }

    // Phase: workspace_template_composes_navigation_and_isolated_durable_detail_views.
    {
        let spec = AppSpec {
            package_name: "project-workspace".into(),
            crate_name: "project_workspace".into(),
            title: "Project Workspace".into(),
            app_id: "so.tilo.project.workspace".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::Workspace,
        };
        let source = app_lib(&spec);
        assert!(source.contains("AppShell::new"));
        assert!(source.contains("with_narrow_screen_state"));
        assert!(source.contains("workspace-back"));
        assert!(source.contains("cx.switch(&selected"));
        assert!(source.contains(".on_select"));
        assert!(source.contains("pub projects: Collection<Project>"));
        assert!(source.contains("Workspace::SET_TITLE.map(move |title| (id, title))"));
        let test = app_test(&spec);
        assert!(test.contains("workspace_keeps_master_detail_views_isolated_and_synchronized"));
        assert!(test.contains("app.undo()"));
        assert!(app_readme(&spec).contains("responsive master-detail shell"));
    }

    // Phase: graph_template_generates_a_valid_editable_screen.
    {
        let spec = AppSpec {
            package_name: "project-maker".into(),
            crate_name: "project_maker".into(),
            title: "Project Maker".into(),
            app_id: "so.tilo.project.maker".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::Graph,
        };
        let files = render(&spec).unwrap();
        let graph = files
            .iter()
            .find(|(path, _)| path == Path::new("ui/main.json"))
            .expect("graph template emits source");
        let document = tilo_ui_graph::UiDocument::from_json(&graph.1).unwrap();
        assert_eq!(document.nodes().len(), 3);
        assert!(matches!(
            document.node("graph-introduction"),
            Some(tilo_ui_graph::UiNode::Text { .. })
        ));
        let registry = files
            .iter()
            .find(|(path, _)| path == Path::new("src/screens.rs"))
            .expect("graph template emits a compiled screen registry");
        assert!(registry.1.contains("include_str!(\"../ui/main.json\")"));
        assert!(registry.1.contains("pub const NAMES"));
        let components = files
            .iter()
            .find(|(path, _)| path == Path::new("src/components/mod.rs"))
            .expect("graph template emits an empty component registry");
        assert!(components.1.contains("pub fn ui_graph_context()"));
        assert!(!components.1.contains(GRAPH_COMPONENT_ENTRY_PREFIX));
        assert!(app_lib(&spec).contains("pub mod components;"));
        assert!(app_lib(&spec).contains("pub mod screens;"));
        assert!(app_lib(&spec).contains("screens::router_with("));
        assert!(app_lib(&spec).contains("components::ui_graph_context()"));
        assert!(app_lib(&spec).contains(GENERATED_GRAPH_ROUTER_CALL));
        assert!(app_lib(&spec).contains("Projects::CREATE.invoke()"));
        assert!(
            app_test(&spec).contains("serialized_screen_and_native_controls_share_one_resource")
        );
        assert!(app_test(&spec).contains("serialized_graph_edits_have_shared_undo_history"));
        assert!(app_readme(&spec).contains("tilo-app ui tree"));
        assert!(app_readme(&spec).contains("tilo-app ui set"));
        assert!(app_readme(&spec).contains("tilo-app ui apply"));
        assert!(app_readme(&spec).contains("--mount main"));
        assert!(app_readme(&spec).contains("available to every generated screen automatically"));
    }

    // Phase: list_template_starts_sparse_variable_and_interactive.
    {
        let spec = AppSpec {
            package_name: "activity-feed".into(),
            crate_name: "activity_feed".into(),
            title: "Activity Feed".into(),
            app_id: "so.tilo.activity.feed".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::List,
        };
        let source = app_lib(&spec);
        assert!(source.contains("ITEM_COUNT: usize = 100_000_000"));
        assert!(source.contains("impl ListModel for ActivityModel"));
        assert!(source.contains("LatestRequest::new()"));
        assert!(source.contains("VirtualList::new_async"));
        assert!(source.contains("ListRowState::Loading"));
        assert!(source.contains("ListModelChange::RowsChanged"));
        assert!(source.contains("with_semantic_key(u64::to_string)"));
        assert!(source.contains("if index % 5 == 0"));
        assert!(source.contains("Button::new"));
        assert!(source.contains("Feed::OPEN.with(*key)"));
        assert!(source.contains("with_id(\"activity-list\")"));
        let test = app_test(&spec);
        assert!(test.contains("Role::List"));
        assert!(test.contains("key:3"));
        assert!(test.contains("open-0"));
        assert!(test.contains("wait_for_named"));
        assert!(test.contains("delegate measurements drive variable rows"));
    }

    // Phase: tree_template_uses_stable_ids_and_professional_tree_defaults.
    {
        let spec = AppSpec {
            package_name: "project-browser".into(),
            crate_name: "project_browser".into(),
            title: "Project Browser".into(),
            app_id: "so.tilo.project.browser".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::Tree,
        };
        let source = app_lib(&spec);
        assert!(source.contains("TreeNode::branch"));
        assert!(source.contains("with_expanded([1, 2])"));
        assert!(source.contains("multi_select(true)"));
        assert!(source.contains("with_row_delegate"));
        assert!(source.contains("Button::new"));
        assert!(source.contains("reorderable()"));
        assert!(source.contains("with_viewport(280.0, 36.0)"));
        assert!(source.contains("with_accessible_name"));
        assert!(source.contains("with_id(\"workspace-tree\")"));
        let test = app_test(&spec);
        assert!(test.contains("Role::Tree"));
        assert!(test.contains("perform_semantic_named"));
        assert!(test.contains("node:3"));
        assert!(test.contains("more-4"));
    }

    // Phase: async_template_uses_typed_wakeup_and_keyed_result_views.
    {
        let spec = AppSpec {
            package_name: "repo-scan".into(),
            crate_name: "repo_scan".into(),
            title: "Repo Scan".into(),
            app_id: "so.tilo.repo.scan".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::Async,
        };
        let source = app_lib(&spec);
        assert!(source.contains("LatestTask::new(LoadState::Idle)"));
        assert!(source.contains("task.begin(LoadState::Loading)"));
        assert!(source.contains("completion.complete(state)"));
        assert!(source.contains("cancel_task.cancel(LoadState::Idle)"));
        assert!(source.contains("Button::new(\"cancel-task\""));
        assert!(source.contains("std::thread::spawn"));
        assert!(source.contains("ModelView::from_state_by"));
        assert!(source.contains("with_arg(\"count\""));
        assert!(source.contains("with_id(\"task-result\")"));
        let test = app_test(&spec);
        assert!(test.contains("app_runs_background_work_on_the_resource_input"));
        assert!(test.contains("generated.background-task:root"));
        assert!(test.contains("wait_for_named(\"task-result\""));
        assert!(source.contains("live.with(|scan| scan.folder.clone())"));
        let actions = headless_actions(&spec);
        assert!(actions.contains("\"action\": \"activate\""));
        assert!(actions.contains("\"expect\""));
        assert!(actions.contains("\"timeout_ms\": 1000"));
    }

    // Phase: document_template_uses_direct_durable_bindings_and_undo_contract.
    {
        let spec = AppSpec {
            package_name: "notes".into(),
            crate_name: "notes".into(),
            title: "Notes".into(),
            app_id: "so.tilo.notes".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/sdk/ui-core"),
            },
            template: Template::Document,
        };
        let source = app_lib(&spec);
        assert!(source.contains(".input(|document| document.title.clone(), Document::SET_TITLE)"));
        assert!(source.contains("cx.text(|document| document.title.clone())"));
        assert!(source.contains("multiline(true)"));
        assert!(source.contains("#[action(destructive)]"));
        let test = app_test(&spec);
        assert!(test.contains("document_fields_are_synchronized_and_undoable"));
        assert!(test.contains("app.undo()"));
        assert!(test.contains("sync_with"));
    }
}

/// Every template uses the toolkit resource action host and generates valid Rust and actions, a
/// legacy graph app still migrates, and generated README commands work in a workspace or standalone.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn every_template_generates_valid_rust_actions_and_working_readme_commands() {
    // Phase: every_template_uses_the_toolkit_resource_action_host.
    {
        for template in [
            Template::Starter,
            Template::Data,
            Template::List,
            Template::Tree,
            Template::Graph,
            Template::Async,
            Template::Document,
            Template::Qr,
            Template::Workspace,
        ] {
            let spec = AppSpec {
                package_name: "generated-app".into(),
                crate_name: "generated_app".into(),
                title: "Generated App".into(),
                app_id: "so.tilo.generated.app".into(),
                sdk: AppSdk::Workspace {
                    ui_path: PathBuf::from("../../crates/sdk/ui-core"),
                },
                template,
            };
            let manifest = cargo_toml(&spec).expect("render Cargo.toml");
            assert!(!manifest.contains("tilo-actions ="), "{template:?}");
            let source = app_lib(&spec);
            assert!(source.contains("#[derive(Model"), "{template:?}");
            assert!(source.contains("#[resource(kind = "), "{template:?}");
            assert!(source.contains("#[action"), "{template:?}");
            assert!(source.contains("impl ResourceApp for App"), "{template:?}");
            assert!(
                source.contains("const ID: &'static str = crate::APP_ID;"),
                "{template:?}"
            );
            assert!(!source.contains("pub fn app_root"), "{template:?}");
            assert!(!source.contains("fn resources("), "{template:?}");
            assert!(
                app_main(&spec).contains("ui::framework::run::<generated_app::App>()"),
                "{template:?}"
            );
            let test = app_test(&spec);
            assert!(test.contains("TestApp::<"), "{template:?}");
            assert!(!test.contains("app_root"), "{template:?}");
        }
    }

    // Phase: a_legacy_generated_graph_app_still_migrates_to_the_component_context.
    {
        let legacy = legacy_graph_app_template();
        assert!(legacy.contains(LEGACY_GRAPH_ROUTER_CALL));
        let source = format!("pub mod screens;\n\n{legacy}");
        let migrated = migrate_generated_graph_context(&source);
        assert!(migrated.contains(GENERATED_GRAPH_ROUTER_CALL));
        assert!(!migrated.contains(LEGACY_GRAPH_ROUTER_CALL));
        let current = format!("pub mod screens;\n\n{GRAPH_APP_TEMPLATE}");
        assert_eq!(migrate_generated_graph_context(&current), current);
    }

    // Phase: generated_readme_commands_work_inside_workspace_or_standalone.
    {
        for workspace_member in [false, true] {
            let spec = AppSpec {
                package_name: "notes".into(),
                crate_name: "notes".into(),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                sdk: fixture_sdk(workspace_member),
                template: Template::Starter,
            };
            let readme = app_readme(&spec);
            assert!(readme.contains("tilo-app check --fix\n"));
            assert!(readme.contains("tilo-app new notes --template starter --dev"));
            assert!(readme.contains("\"resource\":\"generated.profile\""));
            assert!(readme.contains("`#[derive(Model)]` resource"));
            assert!(readme.contains("tilo-app test\n"));
            assert!(readme.contains("tilo-app dev\n"));
            assert!(readme.contains("tilo-app run\n"));
            assert!(readme.contains("tilo-app add component profile-card --template form"));
            assert!(readme.contains("tilo-app add component project-tree --template tree"));
            assert!(readme.contains("tilo-app add component activity-list --template list"));
            assert!(readme.contains("tilo-app add component results-table --template table"));
            assert!(readme.contains("tilo-app add component article-body --template text"));
            assert!(readme.contains("tilo-app scenario run\n"));
            assert!(readme.contains("tilo-app scenario run --artifacts target/scenario-artifacts"));
            assert!(
                readme.contains(
                    "tilo-app scenario run --audit --artifacts target/scenario-artifacts"
                )
            );
            assert!(readme.contains("tilo-app shot\n"));
            assert!(readme.contains("tilo-app inspect --actions tests/headless-actions.json"));
            assert!(readme.contains(
                    "tilo-app inspect --audit --audit-report target/audit/report.json --actions tests/headless-actions.json"
                ));
            assert!(readme.contains("tilo-app package\n"));
            assert!(readme.contains("`cargo test` and `cargo run`"));
        }
    }

    // Phase: only_workspace_members_depend_on_the_workspace_hack.
    {
        for workspace_member in [false, true] {
            let spec = AppSpec {
                package_name: "notes".into(),
                crate_name: "notes".into(),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                sdk: fixture_sdk(workspace_member),
                template: Template::Starter,
            };
            let manifest: toml::Table =
                toml::from_str(&cargo_toml(&spec).expect("render Cargo.toml"))
                    .expect("generated Cargo.toml");
            let hack = manifest
                .get("dependencies")
                .and_then(|dependencies| dependencies.get("workspace-hack"));
            assert_eq!(hack.is_some(), workspace_member, "{manifest:#?}");
        }
    }

    // Phase: every_template_generates_valid_rust_and_actions.
    {
        for template in [
            Template::Starter,
            Template::Data,
            Template::List,
            Template::Tree,
            Template::Graph,
            Template::Async,
            Template::Document,
            Template::Qr,
            Template::Workspace,
        ] {
            let spec = AppSpec {
                package_name: "demo".into(),
                crate_name: "demo".into(),
                title: "Demo".into(),
                app_id: "so.tilo.demo".into(),
                sdk: AppSdk::Workspace {
                    ui_path: PathBuf::from("../../crates/sdk/ui-core"),
                },
                template,
            };
            let actions: serde_json::Value =
                serde_json::from_str(&headless_actions(&spec)).expect("valid action JSON");
            assert!(actions.is_array());
            assert!(!actions.as_array().expect("actions").is_empty());
            tilo_headless_schema::parse_actions(&headless_actions(&spec))
                .expect("generated actions satisfy the runtime grammar");
            syn::parse_file(&app_lib(&spec)).expect("generated library is valid Rust syntax");
            syn::parse_file(&app_main(&spec)).expect("generated binary is valid Rust syntax");
            let generated_test = app_test(&spec);
            assert!(generated_test.contains("initial_native_app_contract_is_certified"));
            assert!(generated_test.contains("native_app_audit"));
            syn::parse_file(&generated_test).expect("generated test is valid Rust syntax");
            let rendered = render(&spec).expect("render complete app");
            let locale_registry = rendered
                .iter()
                .find(|(path, _)| path == Path::new("src/locales.rs"))
                .expect("generated locale registry");
            syn::parse_file(&locale_registry.1)
                .expect("generated locale registry is valid Rust syntax");
            if template == Template::Graph {
                let registry = screen_registry_source(&["main".into()]);
                syn::parse_file(&registry).expect("generated screen registry is valid Rust syntax");
                assert!(registry.contains("pub fn main_with("));
                assert!(registry.contains("pub fn router_with("));
                assert!(registry.contains("context.clone().with_router"));
            }
        }
    }
}

/// The standalone UI path is discovered and overridable; task plans discover the app and forward
/// after the separator; quality plans scope format and clippy to the package; tasks refuse to spawn
/// when doctor fails.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn app_task_plans_are_scoped_to_the_app_and_gated_by_doctor() {
    // Phase: standalone_ui_path_is_discovered_and_can_be_overridden.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let discovered = resolve_ui_dir(temp.path(), None, None).expect("discovered toolkit");
        assert!(discovered.join("Cargo.toml").is_file());

        let error = resolve_ui_dir(temp.path(), None, Some(Path::new("missing-ui"))).unwrap_err();
        assert!(error.to_string().contains("does not contain Cargo.toml"));

        let ui = temp.path().join("toolkit-ui");
        std::fs::create_dir(&ui).expect("ui dir");
        std::fs::write(ui.join("Cargo.toml"), "[package]\nname='ui'\n").expect("manifest");
        assert_eq!(
            resolve_ui_dir(temp.path(), None, Some(Path::new("toolkit-ui"))).unwrap(),
            ui.canonicalize().unwrap()
        );
    }

    // Phase: app_task_plan_discovers_generated_app_and_forwards_after_separator.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("reading-list");
        create_app(request(destination.clone())).expect("create app");
        let descendant = destination.join("src/lib.rs");
        let forwarded = vec![OsString::from("--shots"), OsString::from("preview.png")];

        let plan = plan_app_task(&descendant, AppTask::Run, &forwarded).expect("run plan");
        assert_eq!(plan.app_dir, destination.canonicalize().unwrap());
        assert_eq!(
            plan.args,
            vec![
                OsString::from("run"),
                OsString::from("--manifest-path"),
                destination.join("Cargo.toml").into_os_string(),
                OsString::from("--"),
                OsString::from("--shots"),
                OsString::from("preview.png"),
            ]
        );

        let test = plan_app_task(&destination, AppTask::Test, &[]).expect("test plan");
        assert_eq!(test.args[0], "test");
        assert!(!test.args.iter().any(|arg| arg == "--"));
    }

    // Phase: app_quality_plan_scopes_format_and_clippy_to_the_app_package.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("reading-list");
        create_app(request(destination.clone())).expect("create app");
        let forwarded = [OsString::from("--nocapture")];

        let plan = plan_app_quality(
            &destination.join("src/lib.rs"),
            &forwarded,
            AppFormatMode::Check,
        )
        .expect("quality plan");
        let manifest = destination.join("Cargo.toml").into_os_string();
        assert_eq!(
            plan.format.args,
            [
                OsString::from("fmt"),
                OsString::from("--manifest-path"),
                manifest.clone(),
                OsString::from("--package"),
                OsString::from("reading-list"),
                OsString::from("--check"),
            ]
        );
        assert_eq!(
            plan.clippy.args,
            [
                OsString::from("clippy"),
                OsString::from("--manifest-path"),
                manifest.clone(),
                OsString::from("--package"),
                OsString::from("reading-list"),
                OsString::from("--all-targets"),
                OsString::from("--no-deps"),
                OsString::from("--"),
                OsString::from("-D"),
                OsString::from("warnings"),
            ]
        );
        assert_eq!(
            plan.test.args,
            [
                OsString::from("test"),
                OsString::from("--manifest-path"),
                manifest,
                OsString::from("--package"),
                OsString::from("reading-list"),
                OsString::from("--"),
                OsString::from("--nocapture"),
            ]
        );

        let fix = plan_app_quality(&destination, &[], AppFormatMode::Apply).expect("fix plan");
        assert_eq!(
            fix.format.args,
            [
                OsString::from("fmt"),
                OsString::from("--manifest-path"),
                destination.join("Cargo.toml").into_os_string(),
                OsString::from("--package"),
                OsString::from("reading-list"),
            ]
        );
    }

    // Phase: app_task_refuses_to_spawn_when_doctor_fails.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname='broken'\nversion='0.1.0'\n",
        )
        .expect("manifest");

        let error = plan_app_task(temp.path(), AppTask::Run, &[]).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("app preflight failed"), "{message}");
        assert!(message.contains("missing dependency `ui`"), "{message}");
        assert!(message.contains("tilo-app.toml"), "{message}");
    }
}

/// Shot plans use the standard launch flags and forward app options; inspection plans build a strict
/// payload from an actions file and reject a non-array file.
#[test]
fn shot_and_inspection_plans_are_built_strictly() {
    // Phase: shot_plan_uses_standard_launch_flags_and_forwards_app_options.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("gallery");
        create_app(request(destination.clone())).expect("create app");

        let plan = plan_app_shot(
            &destination.join("src"),
            ShotRequest {
                output: PathBuf::from("artifacts/dark.png"),
                viewport: Some((1440, 900)),
                scenario: Some("populated".into()),
                forwarded: vec![OsString::from("--theme"), OsString::from("dark")],
            },
        )
        .expect("shot plan");

        assert_eq!(
            &plan.args[3..],
            [
                OsString::from("--"),
                OsString::from("--shots"),
                OsString::from("artifacts/dark.png"),
                OsString::from("--viewport"),
                OsString::from("1440x900"),
                OsString::from("--scenario"),
                OsString::from("populated"),
                OsString::from("--theme"),
                OsString::from("dark"),
            ]
        );
    }

    // Phase: inspection_plan_builds_strict_payload_from_an_actions_file.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("inspector");
        create_app(request(destination.clone())).expect("create app");
        let actions = destination.join("tests/actions.json");
        std::fs::write(
            &actions,
            r#"[{"perform":{"target":"name-input","action":"set_value","value":"Ada"}}]"#,
        )
        .expect("actions file");

        let plan = plan_app_inspection(
            &destination.join("src/lib.rs"),
            InspectRequest {
                output: PathBuf::from("artifacts/inspect.json"),
                viewport: Some((1280, 800)),
                actions: Some(PathBuf::from("tests/actions.json")),
                dump: Some(PathBuf::from("artifacts/state.json")),
                targets: Some(PathBuf::from("artifacts/targets.json")),
                audit: true,
                audit_report: Some(PathBuf::from("artifacts/audit.json")),
            },
        )
        .expect("inspection plan");

        assert_eq!(plan.args[3], "--");
        assert_eq!(plan.args[4], "--headless-drive");
        let payload: serde_json::Value =
            serde_json::from_str(plan.args[5].to_str().expect("inspection payload is UTF-8"))
                .expect("inspection payload JSON");
        assert_eq!(payload["inspect"], "artifacts/inspect.json");
        assert_eq!(payload["viewport"], serde_json::json!([1280, 800]));
        assert_eq!(payload["dump"], "artifacts/state.json");
        assert_eq!(payload["targets"], "artifacts/targets.json");
        assert_eq!(payload["audit"], true);
        assert_eq!(payload["audit_report"], "artifacts/audit.json");
        assert_eq!(payload["actions"].as_array().map(Vec::len), Some(1));
        assert_eq!(payload["actions"][0]["perform"]["target"], "name-input");
    }

    // Phase: inspection_plan_rejects_a_non_array_actions_file.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("inspector");
        create_app(request(destination.clone())).expect("create app");
        let actions = temp.path().join("actions.json");
        std::fs::write(&actions, r#"{"actions":[]}"#).expect("actions file");

        let error = plan_app_inspection(
            &destination,
            InspectRequest {
                output: PathBuf::from("inspect.json"),
                viewport: None,
                actions: Some(actions),
                dump: None,
                targets: None,
                audit: false,
                audit_report: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("must contain a JSON array"));
    }
}

/// Package specs use the manifest entrypoint and package name, artifact collection is scoped to the
/// manifest, output is not replaced before building, the payload overlay copies nested files and
/// rejects symlinks, and profile names are safe Cargo arguments.
#[test]
fn packaging_is_scoped_safe_and_never_clobbers_output() {
    // Phase: package_spec_uses_manifest_entrypoint_and_cargo_package_name.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("reading-list");
        create_app(request(destination.clone())).expect("create app");

        let spec = load_app_package_spec(destination.clone()).expect("package spec");
        assert_eq!(spec.app_dir, destination);
        assert_eq!(spec.cargo_package, "reading-list");
        assert_eq!(
            spec.entrypoints,
            [("reading-list".into(), PathBuf::from("bin/reading-list"))]
        );
    }

    // Phase: cargo_artifact_collection_is_scoped_to_the_app_manifest.
    {
        let manifest = PathBuf::from("/workspace/demo/Cargo.toml");
        let executable = PathBuf::from("/workspace/target/release/demo");
        let line = serde_json::json!({
            "reason": "compiler-artifact",
            "manifest_path": manifest,
            "target": {"name": "demo", "kind": ["bin"]},
            "executable": executable,
        })
        .to_string();
        let mut artifacts = HashMap::new();
        collect_cargo_message(&line, &manifest, &mut artifacts);
        assert_eq!(artifacts.get("demo"), Some(&executable));

        let dependency = serde_json::json!({
            "reason": "compiler-artifact",
            "manifest_path": "/workspace/dependency/Cargo.toml",
            "target": {"name": "other", "kind": ["bin"]},
            "executable": "/workspace/target/release/other",
        })
        .to_string();
        collect_cargo_message(&dependency, &manifest, &mut artifacts);
        assert!(!artifacts.contains_key("other"));
    }

    // Phase: package_refuses_to_replace_output_before_building.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("reading-list");
        create_app(request(destination.clone())).expect("create app");
        std::fs::create_dir(destination.join("dist")).expect("dist directory");
        std::fs::write(destination.join("dist/reading-list.tapp"), "existing")
            .expect("existing package");

        let error = package_app(
            &destination.join("src/lib.rs"),
            &PackageRequest {
                output: None,
                identity: None,
                profile: "release".into(),
                target: None,
                features: Vec::new(),
                no_default_features: false,
                force: false,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("pass --force"));
    }

    // Phase: payload_overlay_copies_nested_files_and_rejects_symlinks.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::create_dir_all(source.join("share/demo")).expect("source tree");
        std::fs::create_dir(&destination).expect("destination");
        std::fs::write(source.join("share/demo/message.txt"), "hello").expect("asset");
        copy_payload_tree(&source, &destination).expect("copy overlay");
        assert_eq!(
            std::fs::read_to_string(destination.join("share/demo/message.txt")).expect("copy"),
            "hello"
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("share/demo/message.txt", source.join("link"))
                .expect("symlink");
            let error = copy_payload_tree(&source, &destination).unwrap_err();
            assert!(error.to_string().contains("unsupported symlink"));
        }
    }

    // Phase: package_profile_names_are_safe_cargo_arguments.
    {
        assert!(validate_profile_name("release").is_ok());
        assert!(validate_profile_name("distribution-fast_2").is_ok());
        for invalid in ["", "../release", "release build", "debug;echo"] {
            assert!(
                validate_profile_name(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }
}

/// The dev loop watches app and local UI source, ignores build and editor noise, detects a newer
/// edit while draining, selects the default binary and reports failures, and passes app arguments and
/// working directory to the rebuilt binary.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn the_dev_loop_watches_rebuilds_and_restarts_only_on_real_edits() {
    // Phase: dev_loop_watches_app_and_local_ui_source.
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let destination = temp.path().join("watchable");
        create_app(request(destination.clone())).expect("create app");

        let roots = dev_watch_roots(&destination, true).expect("app and UI watch roots");
        assert_eq!(roots[0], destination.canonicalize().unwrap());
        assert_eq!(
            roots[1],
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../sdk/tilo-app")
                .canonicalize()
                .unwrap()
        );
        assert_eq!(
            roots[2],
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../sdk/ui-core")
                .canonicalize()
                .unwrap()
        );

        let app_only = dev_watch_roots(&destination, false).expect("app-only roots");
        assert_eq!(app_only, roots[..1]);

        std::fs::create_dir_all(destination.join("assets/icons")).expect("asset directories");
        std::fs::create_dir_all(destination.join("target/debug/deps")).expect("target directories");
        std::fs::create_dir_all(destination.join(".git/hooks")).expect("Git directories");
        let watched = source_watch_directories(&destination).expect("source watch directories");
        assert!(watched.contains(&destination));
        assert!(watched.contains(&destination.join("src")));
        assert!(watched.contains(&destination.join("assets/icons")));
        assert!(
            !watched
                .iter()
                .any(|path| path.starts_with(destination.join("target")))
        );
        assert!(
            !watched
                .iter()
                .any(|path| path.starts_with(destination.join(".git")))
        );
    }

    // Phase: dev_restart_filter_ignores_build_and_editor_noise.
    {
        let root = PathBuf::from("/workspace/apps/demo");
        let roots = [root.clone()];
        let changed = |path: &str| {
            Event::new(EventKind::Modify(notify::event::ModifyKind::Any)).add_path(root.join(path))
        };

        assert!(event_requires_restart(&changed("src/main.rs"), &roots));
        assert!(event_requires_restart(&changed("assets/icon.png"), &roots));
        assert!(event_requires_restart(
            &changed("assets/new-directory"),
            &roots
        ));
        assert!(
            !event_requires_restart(&changed("ui/main.json"), &roots),
            "serialized screens hot-reload without a process restart"
        );
        assert!(!event_requires_restart(
            &changed("target/debug/demo"),
            &roots
        ));
        assert!(!event_requires_restart(&changed("Cargo.lock"), &roots));
        assert!(!event_requires_restart(&changed("src/main.rs.swp"), &roots));
        assert!(!event_requires_restart(
            &Event::new(EventKind::Modify(notify::event::ModifyKind::Any)).add_path(root.clone()),
            &roots
        ));

        let access = Event::new(EventKind::Access(notify::event::AccessKind::Any))
            .add_path(root.join("src/main.rs"));
        assert!(!event_requires_restart(&access, &roots));
    }

    // Phase: dev_build_drain_detects_a_newer_edit_without_reacting_to_noise.
    {
        let root = PathBuf::from("/workspace/apps/demo");
        let roots = [root.clone()];
        let changed = |path: &str| {
            Event::new(EventKind::Modify(notify::event::ModifyKind::Any)).add_path(root.join(path))
        };
        let (sender, events) = mpsc::channel::<notify::Result<Event>>();
        sender
            .send(Ok(changed("target/debug/demo")))
            .expect("queue build noise");
        sender
            .send(Ok(changed("src/app.rs")))
            .expect("queue newer source edit");
        drop(sender);

        let mut observed = Vec::new();
        assert!(drain_pending_restart_events(&events, &roots, |event| {
            observed.extend(event.paths.iter().cloned());
        }));
        assert_eq!(observed.len(), 2, "the watcher observes every queued event");

        let (sender, events) = mpsc::channel::<notify::Result<Event>>();
        sender
            .send(Ok(changed("Cargo.lock")))
            .expect("queue ignored lock update");
        drop(sender);
        assert!(!drain_pending_restart_events(&events, &roots, |_| {}));
    }

    // Phase: dev_candidate_build_selects_default_binary_and_reports_failures.
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let app = temp.path().join("app");
        std::fs::create_dir(&app).unwrap();
        let manifest = app.join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\ndefault-run = \"desktop\"\n",
        )
        .unwrap();
        let manifest = manifest.canonicalize().unwrap();
        let executable = app.join("target/debug/desktop");
        let message = serde_json::json!({
            "reason": "compiler-artifact",
            "manifest_path": manifest,
            "target": {"name": "desktop", "kind": ["bin"]},
            "executable": executable,
        });
        let cargo = app.join("fake-cargo");
        let argument_log = app.join("cargo-args.txt");
        std::fs::write(
            &cargo,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat <<'TILO_JSON'\n{message}\nTILO_JSON\n",
                argument_log.display()
            ),
        )
        .unwrap();
        seal_script(&cargo);
        let plan = CargoPlan {
            app_dir: app.clone(),
            program: cargo.into_os_string(),
            args: Vec::new(),
        };
        assert_eq!(build_dev_candidate(&plan).unwrap(), executable);
        assert_eq!(dev_binary_name(&manifest).unwrap(), "desktop");
        let arguments = std::fs::read_to_string(argument_log).unwrap();
        assert!(arguments.contains("--bin\ndesktop\n"), "{arguments}");

        let failing_cargo = app.join("failing-cargo");
        std::fs::write(&failing_cargo, "#!/bin/sh\nexit 9\n").unwrap();
        seal_script(&failing_cargo);
        let error = build_dev_candidate(&CargoPlan {
            app_dir: app,
            program: failing_cargo.into_os_string(),
            args: Vec::new(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("exit status: 9"), "{error:#}");
    }

    // Phase: rebuilt_dev_binary_receives_app_arguments_and_app_working_directory.
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().expect("temp dir");
        let output = temp.path().join("invocation.txt");
        let executable = temp.path().join("app-bin");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n' \"$PWD\" \"$1\" \"$2\" > '{}'\n",
                output.display()
            ),
        )
        .unwrap();
        seal_script(&executable);
        let mut child = spawn_dev_binary(
            temp.path(),
            &executable,
            &[OsString::from("--document"), OsString::from("A file.qdoc")],
        )
        .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            format!("{}\n--document\nA file.qdoc\n", temp.path().display())
        );
    }
}

#[test]
fn dev_session_dir_is_stable_and_outside_the_app_tree() {
    let app = PathBuf::from("/workspace/apps/demo");
    let session = dev_session_dir(&app);
    assert_eq!(session, dev_session_dir(&app));
    assert!(session.starts_with(std::env::temp_dir()));
    assert_ne!(session, app);
}

#[test]
#[cfg(unix)]
fn dev_build_cancels_when_a_newer_source_edit_arrives() {
    let temp = tempfile::tempdir().expect("temp dir");
    let app = temp.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\ndefault-run = \"desktop\"\n",
    )
    .unwrap();
    let leader = app.join("leader.pid");
    let sleeper = app.join("sleeper.pid");
    let cargo = app.join("slow-cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nset +m\nprintf '%s\\n' $$ > '{}'\nsleep 30 &\nprintf '%s\\n' $! > '{}'\nwait\n",
            leader.display(),
            sleeper.display()
        ),
    )
    .unwrap();
    seal_script(&cargo);
    let plan = CargoPlan {
        app_dir: app.clone(),
        program: cargo.into_os_string(),
        args: Vec::new(),
    };
    let roots = [app.clone()];
    let (sender, events) = mpsc::channel();
    let leader_watch = leader.clone();
    let sleeper_watch = sleeper.clone();
    let source = app.join("src/main.rs");
    let edited = std::sync::Arc::new(std::sync::OnceLock::new());
    let edited_at = edited.clone();
    let watcher = std::thread::spawn(move || {
        tilo_test_env::wait_until("the fake cargo and its child to start", || {
            leader_watch.is_file() && sleeper_watch.is_file()
        });
        let _ = edited_at.set(Instant::now());
        let _ = sender.send(Ok(Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(source)));
    });
    let outcome = build_dev_candidate_interruptible(&plan, Some(&events), &roots, |_| {}).unwrap();
    watcher.join().unwrap();
    assert!(
        matches!(outcome, DevCandidate::Superseded),
        "a source edit during the build must cancel it"
    );
    let cancel = edited.get().expect("the edit was sent").elapsed();
    assert!(
        cancel < Duration::from_secs(2),
        "cancelling a stale build took {cancel:?}"
    );
    let leader_pid = std::fs::read_to_string(&leader).unwrap_or_default();
    let sleeper_pid = std::fs::read_to_string(&sleeper).unwrap_or_default();
    assert!(
        !command_still_running(leader_pid.trim(), "sh"),
        "cargo process {leader_pid} still running"
    );
    assert!(
        !command_still_running(sleeper_pid.trim(), "sleep"),
        "rustc child {sleeper_pid} still running ({})",
        command_file_name(sleeper_pid.trim()).unwrap_or_else(|| "gone".into())
    );
}

#[test]
#[cfg(unix)]
fn dev_build_keeps_compiling_through_editor_noise() {
    let temp = tempfile::tempdir().expect("temp dir");
    let app = temp.path().join("app");
    std::fs::create_dir_all(app.join("target")).unwrap();
    let manifest = app.join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\ndefault-run = \"desktop\"\n",
    )
    .unwrap();
    let manifest = manifest.canonicalize().unwrap();
    let executable = app.join("target/debug/desktop");
    let message = serde_json::json!({
        "reason": "compiler-artifact",
        "manifest_path": manifest,
        "target": {"name": "desktop", "kind": ["bin"]},
        "executable": executable,
    });
    let cargo = app.join("noisy-cargo");
    std::fs::write(
        &cargo,
        format!("#!/bin/sh\nsleep 0.2\nprintf '%s\\n' '{message}'\n"),
    )
    .unwrap();
    seal_script(&cargo);
    let plan = CargoPlan {
        app_dir: app.clone(),
        program: cargo.into_os_string(),
        args: Vec::new(),
    };
    let roots = [app.clone()];
    let (sender, events) = mpsc::channel();
    sender
        .send(Ok(Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(app.join("target/debug/desktop"))))
        .unwrap();
    let outcome = build_dev_candidate_interruptible(&plan, Some(&events), &roots, |_| {}).unwrap();
    assert_eq!(outcome_path(outcome), executable);
}

#[test]
#[cfg(unix)]
fn dev_restart_hands_the_checkpoint_to_the_next_process() {
    let temp = tempfile::tempdir().expect("temp dir");
    let app = temp.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let executable = app.join("demo");
    std::fs::write(
        &executable,
        "#!/bin/bash\n\
dir=\"$TILO_DEV_SESSION\"\n\
if [ -z \"$dir\" ]; then echo missing session >&2; exit 2; fi\n\
if [ -f \"$dir/checkpoint.bin\" ]; then\n\
  cp \"$dir/checkpoint.bin\" \"$dir/restored\"\n\
fi\n\
trap 'printf document-route-caret > \"$dir/checkpoint.bin\"; exit 0' TERM\n\
touch \"$dir/ready\"\n\
while true; do\n\
  read -t 0.2 || true\n\
done\n",
    )
    .unwrap();
    seal_script(&executable);
    let session = dev_session_dir(&app);
    let _ = std::fs::remove_dir_all(&session);

    let mut child = Some(spawn_dev_binary(&app, &executable, &[]).unwrap());
    wait_for_file(&session.join("ready"));
    stop_dev_child(&mut child);
    let checkpoint = std::fs::read(session.join("checkpoint.bin")).unwrap_or_default();
    assert_eq!(checkpoint, b"document-route-caret");

    let mut next = Some(spawn_dev_binary(&app, &executable, &[]).unwrap());
    wait_for_file(&session.join("restored"));
    let restored = std::fs::read(session.join("restored")).unwrap_or_default();
    stop_dev_child(&mut next);
    let _ = std::fs::remove_dir_all(&session);
    assert_eq!(restored, b"document-route-caret");
}

/// True while `pid` is still the named command.
///
/// `kill -0` stays true for a zombie and for a pid the next test reused.
/// An empty `/proc/<pid>/cmdline` is a zombie or a recycled slot, not the
/// compiler this build started.
#[cfg(unix)]
fn command_still_running(pid: &str, name: &str) -> bool {
    let Some(deadline) = Instant::now().checked_add(Duration::from_millis(500)) else {
        return true;
    };
    loop {
        if command_file_name(pid).as_deref() != Some(name) {
            return false;
        }
        if Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn command_file_name(pid: &str) -> Option<String> {
    let pid = pid.parse::<i32>().ok()?;
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let first = bytes
        .split(|byte| *byte == 0)
        .next()
        .filter(|arg| !arg.is_empty())?;
    let name = first.rsplit(|byte| *byte == b'/').next().unwrap_or(first);
    Some(String::from_utf8_lossy(name).into_owned())
}

#[cfg(unix)]
fn outcome_path(outcome: DevCandidate) -> PathBuf {
    match outcome {
        DevCandidate::Built(path) => path,
        DevCandidate::Superseded => panic!("editor noise cancelled the build"),
    }
}

/// Wait for a file a spawned child writes. The deadline only bounds a child
/// that never writes it; a loaded runner can take seconds to start one.
#[cfg(unix)]
fn wait_for_file(path: &Path) {
    tilo_test_env::wait_until(path.display(), || path.is_file());
}

#[test]
fn doctor_accepts_the_cli_only_agent_without_a_published_surface() {
    // The agent is a `type = "cli"` participant: it is never handed a
    // surface, so it has no controls to publish, and packaging it must pass
    // the same preflight as every graphical app.
    let agent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/agent");
    let report = doctor_app(&agent).expect("doctor report");
    assert!(report.is_ready(), "{}", report.summary());
    assert!(report.checks.iter().any(|check| {
        check.label == "resource/action surface" && check.status == DoctorStatus::Pass
    }));
}

/// A standalone app builds what the workspace tests: every `[patch]` the SDK's graph uses, the
/// lockfile, compiler, target flags and profiles, with the SDK from a checkout or from git.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: the checkout and git phases assert the same generated files"
)]
fn standalone_apps_build_against_the_sdk_the_workspace_tests() {
    let read = |path: &Path| std::fs::read_to_string(path).unwrap();
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let workspace: toml::Table = toml::from_str(&read(&checkout.join("Cargo.toml"))).unwrap();
    let workspace_patches = workspace["patch"]["crates-io"].as_table().unwrap();
    let forks = [
        "automerge",
        "winit",
        "vello",
        "wayland-backend",
        "rustls-native-certs",
    ];
    // Forks pinned in public git repositories reach the app exactly as the workspace pins them.
    let git_forks = ["accesskit", "atspi", "rustypipe"];
    let pinned_as_workspace = |patches: &toml::Table| {
        for name in git_forks {
            assert_eq!(patches.get(name), workspace_patches.get(name), "{name}");
            assert!(
                workspace_patches[name]["git"]
                    .as_str()
                    .is_some_and(|url| url.starts_with("https://")),
                "{name}"
            );
        }
    };

    // Phase: a checkout nearby is used in place, by relative path.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        let outcome = create_app_from(temp.path(), request(app.clone())).unwrap();
        assert_eq!(outcome.app_id, "local.dev.reading.list");
        assert!(outcome.summary().contains("namespace you control"));
        let manifest: toml::Table = toml::from_str(&read(&app.join("Cargo.toml"))).unwrap();
        let ui_path = manifest["dependencies"]["ui"]["path"].as_str().unwrap();
        assert!(Path::new(ui_path).is_relative(), "{ui_path}");
        assert_eq!(
            app.join(ui_path).canonicalize().unwrap(),
            checkout.join("crates/sdk/tilo-app")
        );
        let patches = manifest["patch"]["crates-io"].as_table().unwrap();
        for name in forks {
            let path = patches
                .get(name)
                .and_then(|patch| patch.get("path"))
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("{name} is not patched by path: {patches:#?}"));
            assert!(
                app.join(path).join("Cargo.toml").is_file(),
                "{name} -> {path}"
            );
        }
        pinned_as_workspace(patches);
        assert!(!patches.contains_key("workspace-hack"));
        assert!(
            patches
                .keys()
                .all(|name| workspace_patches.contains_key(name))
        );
        let profile = &manifest["profile"];
        assert_eq!(profile["dev"]["debug"].as_str(), Some("line-tables-only"));
        assert_eq!(
            profile["dev"]["package"]["automerge"]["opt-level"].as_integer(),
            Some(3)
        );
        assert_eq!(profile["thin-app"]["panic"].as_str(), Some("unwind"));
        assert_eq!(profile["thin-app"]["inherits"].as_str(), Some("release"));

        assert_eq!(
            read(&app.join("Cargo.lock")),
            read(&checkout.join("Cargo.lock"))
        );
        let toolchain: toml::Table =
            toml::from_str(&read(&app.join("rust-toolchain.toml"))).unwrap();
        let workspace_toolchain: toml::Table =
            toml::from_str(&read(&checkout.join("rust-toolchain.toml"))).unwrap();
        assert_eq!(
            toolchain["toolchain"]["channel"],
            workspace_toolchain["toolchain"]["channel"]
        );
        let config: toml::Table = toml::from_str(&read(&app.join(".cargo/config.toml"))).unwrap();
        let linux = &config["target"]["cfg(all(target_arch = \"x86_64\", target_os = \"linux\"))"];
        assert!(
            linux["rustflags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|flag| flag.as_str() == Some("target-cpu=x86-64-v3"))
        );
        assert!(linux.get("runner").is_none(), "{config:#?}");
        assert!(config.get("source").is_none() && config.get("build").is_none());
        assert_eq!(read(&app.join(".gitignore")), "/target\n");
        assert!(doctor_app(&app).unwrap().is_ready());
    }

    // Phase: without a checkout the SDK and every fork come from one pinned git commit.
    {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("reading-list");
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let mut req = request(app.clone());
        req.sdk_git = Some("https://example.invalid/tilo.git".into());
        req.sdk_rev = Some(rev.into());
        let outcome = create_app_from(temp.path(), req).unwrap();
        assert_eq!(
            outcome.sdk,
            Some(standalone::SdkSource::Git {
                url: "https://example.invalid/tilo.git".into(),
                rev: Some(rev.into()),
            })
        );
        let manifest: toml::Table = toml::from_str(&read(&app.join("Cargo.toml"))).unwrap();
        let dependencies = &manifest["dependencies"];
        for dependency in ["ui", "runtime-dylib"] {
            assert_eq!(
                dependencies[dependency]["git"].as_str(),
                Some("https://example.invalid/tilo.git")
            );
            assert_eq!(dependencies[dependency]["rev"].as_str(), Some(rev));
        }
        assert_eq!(
            dependencies["runtime-dylib"]["optional"].as_bool(),
            Some(true)
        );
        let patches = manifest["patch"]["crates-io"].as_table().unwrap();
        for name in forks {
            assert_eq!(patches[name]["rev"].as_str(), Some(rev), "{name}");
        }
        pinned_as_workspace(patches);
        assert!(patches.values().all(|patch| patch.get("path").is_none()));
        let report = doctor_app(&app).unwrap();
        assert!(report.is_ready(), "{}", report.summary());
        assert!(report.summary().contains(rev), "{}", report.summary());
    }

    // Phase: Cargo's caches are never mistaken for a checkout to build against.
    {
        let home = Path::new("/home/ada/.cargo");
        assert!(standalone::in_cargo_cache(
            &home.join("git/checkouts/tilo-1a2b/0123456/crates/sdk/ui-core")
        ));
        assert!(standalone::in_cargo_cache(
            &home.join("registry/src/index.crates.io-1a2b/tilo-app-cli-0.1.0")
        ));
        assert!(!standalone::in_cargo_cache(Path::new(
            "/home/ada/src/tilo/crates/sdk/ui-core"
        )));
    }
}

/// Inside the Tilo workspace the toolkit is built on its own; a standalone app cannot select it,
/// so every artifact for its target is linked dynamically and the toolkit comes in as a dependency.
#[test]
fn thin_dev_builds_link_the_toolkit_dynamically_inside_and_outside_the_workspace() {
    let manifest = Path::new("/apps/notes/Cargo.toml");
    let args = |step: &ThinDevStep| {
        step.cargo
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };

    let member = thin_dev_steps(manifest, "notes", &ThinDevLinkage::WorkspaceToolkit, true);
    assert_eq!(
        member.iter().map(|step| step.label).collect::<Vec<_>>(),
        [
            "the shared toolkit",
            "the reload host",
            "the reload library"
        ]
    );
    assert_eq!(args(&member[0])[..3], ["rustc", "-p", "runtime-dylib"]);
    assert!(member.iter().all(|step| step.cargo.env.is_empty()));
    assert!(!args(&member[1]).contains(&"--target".to_owned()));

    let triple = "x86_64-unknown-linux-gnu";
    let standalone = thin_dev_steps(
        manifest,
        "notes",
        &ThinDevLinkage::Target(triple.into()),
        false,
    );
    assert_eq!(
        standalone.iter().map(|step| step.label).collect::<Vec<_>>(),
        ["the reload library"]
    );
    let library = args(&standalone[0]);
    let target = library.iter().position(|arg| arg == "--target").unwrap();
    assert_eq!(library[target + 1], triple);
    assert!(
        library
            .windows(2)
            .any(|pair| pair == ["--crate-type", "cdylib"])
    );
    let [(key, flags)] = standalone[0].cargo.env.as_slice() else {
        panic!("one rustflags variable: {:?}", standalone[0].cargo.env);
    };
    assert_eq!(key, "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS");
    assert!(flags.to_string_lossy().ends_with("-C prefer-dynamic"));

    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    assert!(app_is_workspace_member(&repo.join("apps/hot-reload-demo")));
    let temp = tempfile::tempdir().unwrap();
    assert!(!app_is_workspace_member(temp.path()));
}

#[test]
fn every_template_scaffolds_a_portable_app() {
    for template in [
        Template::Starter,
        Template::Data,
        Template::List,
        Template::Tree,
        Template::Graph,
        Template::Async,
        Template::Document,
        Template::Qr,
        Template::Workspace,
    ] {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(template.name());
        let mut new_app = request(app.clone());
        new_app.template = Some(template);
        create_app(new_app).unwrap();
        let report = lint_portability(&app).unwrap();
        assert!(report.passed(), "{}: {}", template.name(), report.render());
        assert!(report.files > 0);
        assert!(report.render().starts_with("portability: ok"));
    }
}

#[test]
fn native_declaration_replaces_line_exceptions_and_must_be_needed() {
    let root = tempfile::tempdir().unwrap();
    let native = platform_specific_app(root.path());
    let declare = |app: &Path| {
        let manifest = std::fs::read_to_string(app.join("tilo-app.toml")).unwrap();
        std::fs::write(
            app.join("tilo-app.toml"),
            format!("{manifest}\n[platform]\nnative = \"Needs the host's process table.\"\n"),
        )
        .unwrap();
    };
    declare(&native);
    let report = lint_portability(&native).unwrap();
    assert!(report.passed(), "{}", report.render());
    assert!(
        report
            .render()
            .contains("declared native in tilo-app.toml: \"Needs the host's process table.\"")
    );

    let portable = root.path().join("portable");
    create_app(request(portable.clone())).unwrap();
    declare(&portable);
    let report = lint_portability(&portable).unwrap();
    assert!(!report.passed());
    assert!(report.render().contains("remove the [platform] table"));
}

fn platform_specific_app(root: &Path) -> PathBuf {
    let app = root.join("platform");
    create_app(request(app.clone())).unwrap();
    let cargo = std::fs::read_to_string(app.join("Cargo.toml")).unwrap();
    let cargo = cargo
        .replacen("[dependencies]\n", "[dependencies]\nlibc = \"0.2\"\n", 1)
        .replacen("[features]\n", "[features]\nmedia = [\"ui/mpris\"]\n", 1);
    std::fs::write(
        app.join("Cargo.toml"),
        format!("{cargo}\n[dev-dependencies]\nnix = \"0.29\"\n"),
    )
    .unwrap();
    let mut lib = std::fs::read_to_string(app.join("src/lib.rs")).unwrap();
    lib.push_str(
        r#"
#[cfg(unix)]
pub fn owner_only(file: &std::fs::File) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
}

pub fn pid() -> i32 {
    unsafe { libc::getpid() }
}

pub fn separator() -> char {
    if cfg!(windows) { '\\' } else { '/' }
}

unsafe extern "C" {
    fn abs(value: i32) -> i32;
}

mod platform;

#[cfg(test)]
mod platform_tests;

#[cfg(all(test, unix))]
fn test_symlink() {
    std::os::unix::fs::symlink("a", "b").unwrap();
}
"#,
    );
    std::fs::write(app.join("src/lib.rs"), lib).unwrap();
    std::fs::write(
        app.join("src/platform.rs"),
        "pub use std::{fs, os::fd::AsRawFd};\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/platform_tests.rs"),
        "use std::os::unix::net::UnixStream;\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/orphan.rs"),
        "use std::os::unix::net::UnixStream;\n",
    )
    .unwrap();
    let main = std::fs::read_to_string(app.join("src/main.rs")).unwrap();
    std::fs::write(
        app.join("src/main.rs"),
        format!("#![cfg_attr(windows, windows_subsystem = \"windows\")]\n{main}"),
    )
    .unwrap();
    app
}

#[test]
fn portability_lint_reports_shipped_platform_code_with_remedies() {
    use portability::FindingKind::{
        Ffi, NativeCrate, NativeDependency, NativeSdkFeature, OsStd, TargetCfg,
    };
    let root = tempfile::tempdir().unwrap();
    let app = platform_specific_app(root.path());
    let report = lint_portability(&app).unwrap();
    assert!(!report.passed());
    let sites: Vec<(String, portability::FindingKind)> = report
        .findings
        .iter()
        .map(|finding| (finding.path.display().to_string(), finding.kind))
        .collect();
    for expected in [
        ("Cargo.toml", NativeDependency),
        ("Cargo.toml", NativeSdkFeature),
        ("src/lib.rs", TargetCfg),
        ("src/lib.rs", OsStd),
        ("src/lib.rs", NativeCrate),
        ("src/lib.rs", Ffi),
        ("src/platform.rs", OsStd),
    ] {
        assert!(
            sites.contains(&(expected.0.to_owned(), expected.1)),
            "missing {expected:?} in {sites:?}"
        );
    }
    // Tests, unreachable files, dev-dependencies and the Windows subsystem
    // linker setting are not shipped platform code.
    assert!(sites.iter().all(|(path, _)| path != "src/platform_tests.rs"
        && path != "src/orphan.rs"
        && path != "src/main.rs"));
    assert_eq!(
        report
            .findings
            .iter()
            .filter(|finding| finding.kind == NativeDependency)
            .count(),
        1,
        "dev-dependencies are exempt"
    );
    let rendered = report.render();
    assert!(rendered.contains("error[portability]: src/lib.rs:"));
    assert!(rendered.contains("`std::os::unix::fs` is an OS-specific standard library module"));
    assert!(rendered.contains("= help: call a platform crate instead"));
    assert!(rendered.contains("enable the semantic SDK feature `ui/media-session`"));
    assert!(rendered.contains("[platform]\n    native = \"<one-line reason>\""));
}

#[test]
#[cfg(unix)]
fn rebuilt_dev_binary_receives_app_arguments_and_app_working_directory() {
    let temp = tempfile::tempdir().expect("temp dir");
    let output = temp.path().join("invocation.txt");
    let executable = temp.path().join("app-bin");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n' \"$PWD\" \"$1\" \"$2\" > '{}'\n",
            output.display()
        ),
    )
    .unwrap();
    seal_script(&executable);
    let mut child = spawn_dev_binary(
        temp.path(),
        &executable,
        &[OsString::from("--document"), OsString::from("A file.qdoc")],
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        format!("{}\n--document\nA file.qdoc\n", temp.path().display())
    );
}

#[test]
fn source_snapshot_notices_an_edit_inotify_did_not_deliver() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::create_dir(root.join("ui")).unwrap();
    let source = root.join("src/lib.rs");
    std::fs::write(&source, "fn main() {}\n").unwrap();
    std::fs::write(root.join("target/stale.rs"), "fn ignored() {}\n").unwrap();
    std::fs::write(root.join("ui/screen.json"), "{}\n").unwrap();
    let roots = vec![root.to_path_buf()];
    let before = snapshot_restart_sources(&roots).expect("scan");
    assert!(before.contains_key(&source));
    assert!(!before.keys().any(|path| path.ends_with("stale.rs")));
    assert!(!before.keys().any(|path| path.ends_with("screen.json")));
    std::fs::write(&source, "fn main() { let _ = 1; }\n").unwrap();
    let after = snapshot_restart_sources(&roots).expect("rescan");
    assert_ne!(before, after);
}

/// One open window. Reload replaces `contents` and leaves `id` alone.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DevWindow {
    id: &'static str,
    contents: &'static str,
}

/// A reload builds new contents for each open window. It does not open,
/// close, or reorder them. One failure leaves every window untouched.
fn reload_open_windows(
    windows: &[DevWindow],
    mut updated: impl FnMut(&DevWindow) -> Result<&'static str, ()>,
) -> Result<Vec<DevWindow>, ()> {
    let mut next = Vec::with_capacity(windows.len());
    for window in windows {
        let contents = updated(window)?;
        next.push(DevWindow {
            id: window.id,
            contents,
        });
    }
    Ok(next)
}

#[test]
fn reload_replaces_the_contents_of_every_open_window() {
    let open = [
        DevWindow {
            id: "primary",
            contents: "hello",
        },
        DevWindow {
            id: "notes",
            contents: "draft",
        },
    ];
    let reloaded = reload_open_windows(&open, |window| {
        Ok(match window.id {
            "primary" => "hello-next",
            "notes" => "draft-next",
            _ => return Err(()),
        })
    })
    .expect("both windows reload");
    assert_eq!(
        reloaded.iter().map(|window| window.id).collect::<Vec<_>>(),
        vec!["primary", "notes"],
        "reload keeps each window and its order"
    );
    assert_eq!(reloaded[0].contents, "hello-next");
    assert_eq!(reloaded[1].contents, "draft-next");

    let failed = reload_open_windows(&open, |window| {
        if window.id == "notes" {
            Err(())
        } else {
            Ok("unused")
        }
    });
    assert!(failed.is_err(), "one failure reloads nothing");
    assert_eq!(open[0].contents, "hello");
    assert_eq!(open[1].contents, "draft");
}

#[test]
#[cfg(unix)]
fn dev_reload_signal_updates_every_window_without_a_new_process() {
    let temp = tempfile::tempdir().expect("temp dir");
    let state = temp.path().join("state");
    let script = temp.path().join("app");
    std::fs::write(
        &script,
        "#!/bin/bash\n\
state=\"$1\"\n\
printf '%s\\n' primary notes > \"$state.windows\"\n\
printf '%s\\n' 'primary=hello' 'notes=draft' > \"$state.contents\"\n\
printf '%s\\n' $$ > \"$state.pid\"\n\
trap 'printf \"%s\\n\" \"primary=hello-next\" \"notes=draft-next\" > \"$state.contents\"' USR1\n\
touch \"$state.ready\"\n\
while true; do sleep 0.2; done\n",
    )
    .unwrap();
    seal_script(&script);
    let mut child = std::process::Command::new(&script)
        .arg(&state)
        .spawn()
        .expect("app process");
    let ready = state.with_extension("ready");
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(2)) else {
        panic!("clock");
    };
    while Instant::now() < deadline && !ready.is_file() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ready.is_file(), "the app did not open its windows");
    let pid = std::fs::read_to_string(state.with_extension("pid")).unwrap();
    assert!(
        reload_running_dev_app(&child),
        "the running app accepts an in-place reload"
    );
    let contents_path = state.with_extension("contents");
    let mut contents = String::new();
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(2)) else {
        panic!("clock");
    };
    while Instant::now() < deadline {
        contents = std::fs::read_to_string(&contents_path).unwrap_or_default();
        if contents.contains("hello-next") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        std::fs::read_to_string(state.with_extension("windows")).unwrap(),
        "primary\nnotes\n"
    );
    assert_eq!(contents, "primary=hello-next\nnotes=draft-next\n");
    assert_eq!(
        std::fs::read_to_string(state.with_extension("pid")).unwrap(),
        pid,
        "reload stays in the process that owns the windows"
    );
}

#[test]
fn source_snapshot_keeps_a_readable_root_when_another_cannot_be_read() {
    let temp = tempfile::tempdir().expect("temp dir");
    let app = temp.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    let source = app.join("src/lib.rs");
    std::fs::write(&source, "fn main() {}\n").unwrap();
    let roots = vec![temp.path().join("missing"), app];
    let stamps = snapshot_restart_sources(&roots).expect("partial scan");
    assert!(stamps.contains_key(&source));
}

#[test]
fn dev_watch_tick_sees_a_save_while_access_events_are_pending() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path().join("app");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let source = root.join("src/lib.rs");
    std::fs::write(&source, "fn main() {}\n").unwrap();
    let roots = vec![root.clone()];
    let mut sources = snapshot_restart_sources(&roots).expect("scan");
    let mut next_poll = Instant::now() + Duration::from_secs(60);
    let mut watcher = DevWatcher::new(notify::recommended_watcher(|_| {}).expect("watcher"));
    watcher.watch_tree(&root).expect("watch");

    std::fs::write(&source, "fn main() { let _ = 1; }\n").unwrap();
    let (sender, events) = mpsc::channel();
    let access = Event::new(EventKind::Access(notify::event::AccessKind::Open(
        notify::event::AccessMode::Any,
    )))
    .add_path(source);
    for _ in 0..32 {
        sender.send(Ok(access.clone())).expect("queue access");
    }

    let waiting = watch_tick(&events, &roots, &mut watcher, &mut sources, &mut next_poll);
    assert!(
        matches!(waiting, WatchTick::Idle),
        "the poll interval must elapse before a stamp change restarts"
    );

    next_poll = Instant::now();
    let tick = watch_tick(&events, &roots, &mut watcher, &mut sources, &mut next_poll);
    assert!(
        matches!(
            tick,
            WatchTick::Rebuild {
                bump_revision: true
            }
        ),
        "a save must restart while open events are still queued, got {tick:?}"
    );
    drop(sender);
}

#[test]
fn opening_a_watched_directory_does_not_rewatch_it() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path().join("app");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let mut watcher = DevWatcher::new(notify::recommended_watcher(|_| {}).expect("watcher"));
    watcher.watch_tree(&root).expect("watch");
    let directory_count = watcher.watched.len();
    assert!(directory_count >= 2, "root and src are watched");

    watcher.observe(
        &Event::new(EventKind::Access(notify::event::AccessKind::Open(
            notify::event::AccessMode::Any,
        )))
        .add_path(root.clone()),
    );
    assert_eq!(
        watcher.watched.len(),
        directory_count,
        "an open event must not walk the tree again"
    );

    let created = root.join("locales");
    std::fs::create_dir(&created).unwrap();
    watcher.observe(
        &Event::new(EventKind::Create(notify::event::CreateKind::Folder)).add_path(created.clone()),
    );
    assert!(
        watcher.watched.contains(&created),
        "a new directory is watched"
    );
}

#[test]
fn build_tick_leaves_a_non_restart_flood_queued() {
    let root = PathBuf::from("/workspace/apps/demo");
    let roots = [root.clone()];
    let (sender, events) = mpsc::channel();
    let access = Event::new(EventKind::Access(notify::event::AccessKind::Open(
        notify::event::AccessMode::Any,
    )))
    .add_path(root.join("src/main.rs"));
    for _ in 0..=DEV_EVENT_DRAIN_LIMIT {
        sender.send(Ok(access.clone())).expect("queue access");
    }
    assert!(!newer_source_during_build(&events, &roots, &mut |_| {}));
    assert!(
        events.try_recv().is_ok(),
        "Cargo must be reaped before an open flood is drained"
    );
    drop(sender);
}

#[test]
fn in_place_reload_requires_the_shared_toolkit_and_the_entry_macro() {
    let temp = tempfile::tempdir().expect("temp dir");
    let app = temp.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("Cargo.toml"),
        "[features]\nthin = [\"dep:runtime-dylib\"]\n",
    )
    .unwrap();
    std::fs::write(app.join("src/lib.rs"), "ui::dev_reload_entry!(App);\n").unwrap();
    assert!(app_supports_in_place_reload(&app));
    std::fs::write(app.join("src/lib.rs"), "fn main() {}\n").unwrap();
    assert!(!app_supports_in_place_reload(&app));
}

#[test]
#[cfg(unix)]
fn in_place_build_stages_a_library_the_open_process_can_load() {
    let temp = tempfile::tempdir().expect("temp dir");
    let app = temp.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    let manifest = app.join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[features]\nthin = [\"dep:runtime-dylib\"]\n",
    )
    .unwrap();
    std::fs::write(app.join("src/lib.rs"), "ui::dev_reload_entry!(App);\n").unwrap();
    let manifest = manifest.canonicalize().unwrap();
    let executable = app.join("target/debug/demo");
    let library = app.join("target/debug/libdemo.so");
    std::fs::create_dir_all(library.parent().unwrap()).unwrap();
    std::fs::write(&library, "library").unwrap();
    let log = app.join("cargo-args.txt");
    let cargo = app.join("fake-cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nargs=\"$*\"\ncase \"$args\" in\n*runtime-dylib*) exit 0 ;;\nesac\ncase \"$args\" in\n*cdylib*)\nprintf '%s\\n' '{{\"reason\":\"compiler-artifact\",\"manifest_path\":{},\"target\":{{\"name\":\"demo\"}},\"filenames\":[{}]}}'\nexit 0 ;;\nesac\nprintf '%s\\n' '{{\"reason\":\"compiler-artifact\",\"manifest_path\":{},\"target\":{{\"name\":\"demo\"}},\"executable\":{}}}'\n",
            log.display(),
            serde_json::to_string(&manifest).unwrap(),
            serde_json::to_string(&library).unwrap(),
            serde_json::to_string(&manifest).unwrap(),
            serde_json::to_string(&executable).unwrap(),
        ),
    )
    .unwrap();
    seal_script(&cargo);
    let plan = CargoPlan {
        app_dir: app.clone(),
        program: cargo.into_os_string(),
        args: Vec::new(),
    };
    let built = build_in_place_dev_app(&plan, None, &[], |_| {}, true).expect("in-place build");
    let DevCandidate::Built(host) = built else {
        panic!("the host should be built");
    };
    assert_eq!(host, executable);
    let staged = std::fs::read_to_string(dev_session_dir(&app).join("reload.path")).unwrap();
    let staged = PathBuf::from(staged.trim());
    assert_eq!(std::fs::read(&staged).unwrap(), b"library");
    // The fixture is outside the Tilo workspace: the toolkit is a dependency
    // of the host, not a package Cargo could select.
    let args = std::fs::read_to_string(&log).unwrap();
    assert!(!args.contains("runtime-dylib"), "{args}");
    assert!(args.contains("--target"), "{args}");
    assert!(args.contains("cdylib"), "{args}");
    assert!(args.contains("prefer-dynamic"), "{args}");
}

/// Make a test-written script executable without leaving a writable
/// descriptor on the executed inode: a sibling test thread that forks between
/// our write and its exec would otherwise make `execve` fail with ETXTBSY.
fn seal_script(path: &Path) {
    let draft = path.with_extension("draft");
    std::fs::rename(path, &draft).unwrap();
    let status = std::process::Command::new("install")
        .arg("-m")
        .arg("755")
        .arg(&draft)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "install {}", path.display());
    std::fs::remove_file(&draft).unwrap();
}

/// `--ui-path` may name either SDK crate of a checkout; both resolve to the same checkout.
#[test]
fn ui_path_accepts_either_sdk_crate() {
    for crate_dir in ["ui-core", "tilo-app"] {
        let ui = Path::new("../tilo/crates/sdk").join(crate_dir);
        assert_eq!(checkout_root_for_ui(&ui), Path::new("../tilo"));
        assert_eq!(
            sdk_path_for_ui(&ui),
            Path::new("../tilo/crates/sdk/tilo-app")
        );
    }
}
