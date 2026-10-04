//! End-to-end interaction and render tests for the containers app, driven
//! through the production frame host (`ui::headless::Driver`): real layout,
//! real event routing, real accessibility, and pixels read back from the
//! renderer `--shots` uses.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::AutoCommit;
use containers::ContainersShell;
use containers::app::ContainersApp;
use containers::doc::{CreateField, DetailTab, Section};
use containers::model::sample_engine;
use containers::terminal;
use ui::framework::testing::TestApp;
use ui::headless::Driver;
use ui::theme::Theme;
use ui::widget::Widget;

const VIEWPORT: (u32, u32) = (1280, 800);

fn offline_at(viewport: (u32, u32), theme: Theme) -> Driver {
    let mut app = ContainersApp::sample();
    app.stage_scene(None);
    Driver::for_root(ui::State::new(), app, viewport, theme)
}

fn offline() -> Driver {
    offline_at(VIEWPORT, Theme::dark())
}

fn doc(d: &mut Driver) -> &mut AutoCommit {
    &mut d.state_mut().doc
}

fn app(d: &mut Driver) -> &mut ContainersApp {
    d.root_mut::<ContainersApp>()
}

/// Pixels that differ from the window ground: a blank surface scores ~0.
fn painted(d: &Driver) -> usize {
    let rgba = d.render_rgba();
    let Some(ground) = rgba.get(..4).map(<[u8]>::to_vec) else {
        return 0;
    };
    rgba.chunks_exact(4)
        .filter(|px| px.iter().zip(&ground).any(|(a, b)| a.abs_diff(*b) > 12))
        .count()
}

fn labels(d: &Driver) -> Vec<String> {
    d.a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

fn named(d: &Driver, name: &str, role: ui::accesskit::Role) -> ui::accesskit::NodeId {
    d.a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name) && node.role() == role)
        .map_or_else(
            || panic!("no {role:?} named {name:?}; labels: {:?}", labels(d)),
            |(id, _)| id,
        )
}

/// Every section and dialog paints a populated surface, and a phone at 200%
/// text still shows a readable page.
#[test]
fn sections_and_dialogs_render_non_blank() {
    let mut d = offline();
    let floor = (VIEWPORT.0 * VIEWPORT.1 / 20) as usize;
    assert!(painted(&d) > floor, "the containers page paints content");
    for (key, section) in [
        ("2", Section::Images),
        ("3", Section::Volumes),
        ("4", Section::Distrobox),
    ] {
        d.key(key);
        assert_eq!(app(&mut d).section(), section);
        assert!(painted(&d) > floor / 8, "{section:?} paints content");
    }

    d.key("1");
    d.key("n");
    assert!(app(&mut d).is_create_open());
    let _ = named(&d, "New container", ui::accesskit::Role::Dialog);
    assert!(painted(&d) > floor, "the form paints over the window");

    let mut phone = offline_at((390, 844), Theme::dark().with_text_scale(2.0));
    assert!(app(&mut phone).selected().is_some());
    assert!(
        labels(&phone).iter().any(|label| label == "Restart"),
        "a phone opens on the chosen container"
    );
    assert!(
        painted(&phone) > (390 * 844 / 12) as usize,
        "a phone with large text shows the container page"
    );
    let detail = phone.render_rgba();
    phone.key("Escape");
    phone.settle_for_capture(ui::time::Duration::from_secs(2));
    assert_ne!(
        phone.render_rgba(),
        detail,
        "Escape goes back from the container to the list"
    );
    assert!(painted(&phone) > (390 * 844 / 20) as usize);
}

#[test]
fn container_resources_open_terminal_and_log_detail() {
    // --- phase: enter_distrobox_builds_terminal_running_distrobox_enter ---
    {
        let inner = terminal::distrobox_enter_command("my-distrobox");
        assert!(inner.contains("distrobox enter 'my-distrobox'"), "{inner}");
        let cmd = terminal::terminal_command(&inner, "enter-my-distrobox").expect("build cmd");
        let prog = cmd.get_program().to_string_lossy().to_string();
        assert!(
            prog.ends_with("terminal"),
            "spawns the terminal binary: {prog}"
        );
        let shell = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("SHELL"))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().to_string())
            .expect("SHELL env points at the launcher");
        assert!(shell.contains("tilo-term-enter-my-distrobox.sh"), "{shell}");
        let body = std::fs::read_to_string(&shell).expect("launcher written");
        assert!(body.contains("distrobox enter 'my-distrobox'"), "{body}");
    }

    // --- phase: canonical_log_resource_opens_the_logs ---
    {
        let mut d = offline();
        d.key("2");
        let id = sample_engine().containers[1].id.clone();
        let payload = serde_json::to_vec(&vec!["logs", id.as_str()]).unwrap();
        let (surface, state) = d.root_and_state_mut::<ContainersApp>();
        assert!(
            surface
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::View,
                        "containers.logs",
                        &payload,
                    ),
                    &mut state.doc,
                    &automerge::ROOT,
                )
                .map(|started| started.is_some())
                .unwrap()
        );
        d.relayout();
        assert_eq!(app(&mut d).section(), Section::Containers);
        assert_eq!(app(&mut d).selected(), Some(id.as_str()));
        assert_eq!(app(&mut d).tab(), DetailTab::Logs);
    }
}

/// Number keys switch sections, the sidebar and the list respond to the
/// pointer and to assistive technology, and the tabs switch the detail.
#[test]
fn keyboard_pointer_and_semantic_navigation() {
    // --- phase: number_keys_navigate_sections ---
    {
        let mut d = offline();
        d.key("3");
        assert_eq!(app(&mut d).section(), Section::Volumes);
        d.key("4");
        assert_eq!(app(&mut d).section(), Section::Distrobox);
        d.key("2");
        assert_eq!(app(&mut d).section(), Section::Images);
        d.key("1");
        assert_eq!(app(&mut d).section(), Section::Containers);
    }

    // --- phase: the_sidebar_and_the_list_take_assistive_activation ---
    {
        let mut d = offline();
        let images = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("Images"))
            .map(|(id, _)| id)
            .expect("the Images destination is named");
        d.perform_a11y_action(images, &ui::a11y::Action::Activate)
            .unwrap();
        assert_eq!(app(&mut d).section(), Section::Images);

        d.key("1");
        let postgres = sample_engine()
            .containers
            .into_iter()
            .find(|c| c.name == "postgres")
            .unwrap();
        let row = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("postgres"))
            .map(|(id, _)| id)
            .expect("the postgres row is named");
        d.perform_a11y_action(row, &ui::a11y::Action::Activate)
            .unwrap();
        assert_eq!(
            app(&mut d).selected(),
            Some(postgres.id.as_str()),
            "activating a row opens that container"
        );
    }

    // --- phase: tabs_switch_the_detail ---
    {
        let mut d = offline();
        let stats = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("Stats"))
            .map(|(id, _)| id)
            .expect("the Stats tab is named");
        d.perform_a11y_action(stats, &ui::a11y::Action::Activate)
            .unwrap();
        assert_eq!(app(&mut d).tab(), DetailTab::Stats);
        assert!(labels(&d).iter().any(|label| label == "Memory"));
    }
}

#[test]
fn the_root_is_a_named_window() {
    let d = offline();
    let tree = d
        .root_widget()
        .accessibility()
        .expect("the root is semantic");
    assert_eq!(tree.role, ui::a11y::Role::Window);
    assert_eq!(tree.name.as_deref(), Some("Containers"));
}
fn hosted() -> TestApp<ContainersShell> {
    TestApp::<ContainersShell>::capture(None).expect("the containers capture launches")
}

fn draft(app: &TestApp<ContainersShell>, field: CreateField) -> String {
    app.state().draft.get(field).to_owned()
}

/// Typing in the create form splices the draft's texts: plain keys, a
/// replaced selection, an IME commit, and Tab to the next field; undo takes
/// a typed word back.
#[test]
fn typing_in_the_create_form_edits_the_draft() {
    let mut app = hosted();
    // `n` opens the form on its image field.
    app.harness().key("n");
    app.harness().type_text("ngi");
    assert_eq!(draft(&app, CreateField::Image), "ngi");
    let all = sample_engine().image_references();
    let filtered = containers::combobox::filter_options(&all, "ngi");
    assert!(
        all[filtered[0]].contains("nginx"),
        "'ngi' narrows the suggestions to nginx"
    );
    assert!(app.undo(), "the typed word is one undo step");
    assert_eq!(draft(&app, CreateField::Image), "");

    app.harness().type_text("nginx");
    app.harness().set_modifiers(&["ctrl".to_owned()]);
    app.harness().key("a");
    app.harness().set_modifiers(&[]);
    app.harness().type_text("p");
    assert_eq!(
        draft(&app, CreateField::Image),
        "p",
        "Ctrl+A then typing replaces the selection instead of appending"
    );

    app.harness().commit_text("接続");
    assert_eq!(draft(&app, CreateField::Image), "p接続");

    app.harness().key("Tab");
    app.harness().type_text("8");
    assert_eq!(
        draft(&app, CreateField::Ports),
        "8",
        "Tab advances from Image to Ports in the six-field order"
    );
}

/// The image suggestions pick with the arrow keys and Enter; Enter on a
/// complete form creates the container and empties the draft.
#[test]
fn image_suggestions_pick_and_enter_creates() {
    let mut app = hosted();
    app.harness().key("n");
    // Typing opens the suggestions; with the field empty again they list
    // every local image.
    app.harness().type_text("x");
    app.harness().key("Backspace");
    app.harness().key("ArrowDown");
    app.harness().key("Enter");
    let all = sample_engine().image_references();
    assert_eq!(
        draft(&app, CreateField::Image),
        all[1],
        "Enter takes the highlighted suggestion"
    );

    app.harness().key("Enter");
    assert!(
        app.state().draft.is_empty(),
        "creating the container empties the draft"
    );
    assert!(app.undo(), "emptying the draft is undoable");
    assert_eq!(draft(&app, CreateField::Image), all[1]);
}

#[test]
fn create_dialog_hides_background_and_exposes_editable_fields() {
    let mut app = hosted();
    app.harness().key("n");
    let inspection = app.harness().inspection_json().unwrap();
    assert!(inspection.contains("New container"));
    assert!(inspection.contains("container_create_image"));
    assert!(
        !app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some("Distrobox")),
        "modal semantics hide background navigation"
    );
}
