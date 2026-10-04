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

use tilo_automerge::ApplyIncoming;

use automerge::AutoCommit;
use containers::app::ContainersApp;
use containers::doc::{CreateField, DetailTab, Section, ViewDoc};
use containers::model::sample_engine;
use containers::terminal;
use ui::headless::Driver;
use ui::theme::Theme;
use ui::widget::Widget;

const VIEWPORT: (u32, u32) = (1280, 800);

fn offline_at(viewport: (u32, u32), theme: Theme) -> Driver {
    Driver::for_root(ui::State::new(), ContainersApp::new(false), viewport, theme)
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
        assert_eq!(ViewDoc::section(doc(&mut d)), section);
        assert!(painted(&d) > floor / 8, "{section:?} paints content");
    }

    d.key("1");
    d.key("n");
    assert!(app(&mut d).is_create_open());
    let _ = named(&d, "New container", ui::accesskit::Role::Dialog);
    assert!(painted(&d) > floor, "the form paints over the window");

    let mut phone = offline_at((390, 844), Theme::dark().with_text_scale(2.0));
    assert!(ViewDoc::selected(doc(&mut phone)).is_some());
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

/// The new-container form edits the shared draft through real fields and
/// assembles the podman command.
#[test]
fn create_form_input_order_and_command() {
    // --- phase: select_all_then_typing_replaces ---
    {
        let mut d = offline();
        d.key("n");
        d.type_text("nginx:latest");
        d.set_modifiers(&["ctrl".to_owned()]);
        d.key("a");
        d.set_modifiers(&[]);
        d.type_text("p");
        assert_eq!(
            ViewDoc::create_field(doc(&mut d), CreateField::Image),
            "p",
            "select-all then typing replaces the selection instead of appending"
        );
    }

    // --- phase: ime_commit_replicates_as_crdt_text ---
    {
        let mut d = offline();
        d.key("n");
        let mut peer = doc(&mut d).fork();
        d.send(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "接続".into(),
        )));
        assert_eq!(
            ViewDoc::create_field(doc(&mut d), CreateField::Image),
            "接続"
        );
        peer.merge_incoming(doc(&mut d)).expect("peer merge");
        assert_eq!(
            ViewDoc::create_field(&peer, CreateField::Image),
            "接続",
            "the field splices the shared collaborative Text"
        );
    }

    // --- phase: tab_follows_the_form_order ---
    {
        let mut d = offline();
        d.key("n");
        d.key("Tab");
        d.type_text("web");
        assert_eq!(
            ViewDoc::create_field(doc(&mut d), CreateField::Name),
            "web",
            "Tab moves from Image to Name"
        );
    }

    // --- phase: escape_cancels_and_an_empty_image_does_not_submit ---
    {
        let mut d = offline();
        d.key("n");
        d.key("Enter");
        assert!(
            app(&mut d).is_create_open(),
            "the form stays open without an image"
        );
        d.key("Escape");
        assert!(!app(&mut d).is_create_open());
    }

    // --- phase: create_form_assembles_full_podman_run ---
    {
        let mut doc = AutoCommit::new();
        ViewDoc::set_create_field(&mut doc, CreateField::Name, "web");
        ViewDoc::set_create_field(&mut doc, CreateField::Image, "ghcr.io/acme/app:edge");
        ViewDoc::set_create_field(&mut doc, CreateField::Ports, "8080:80, 8443:443");
        ViewDoc::set_create_field(&mut doc, CreateField::Env, "TZ=UTC, LOG=info");
        ViewDoc::set_create_field(
            &mut doc,
            CreateField::Volumes,
            "data:/var/lib/app, cfg:/etc/app",
        );
        let args = ViewDoc::create_run_args(&doc).expect("image present → args");
        assert_eq!(&args[0..2], &["run", "-d"]);
        assert!(args.windows(2).any(|w| w[0] == "--name" && w[1] == "web"));
        assert_eq!(args.iter().filter(|a| *a == "-p").count(), 2);
        assert_eq!(args.iter().filter(|a| *a == "-e").count(), 2);
        assert_eq!(args.iter().filter(|a| *a == "-v").count(), 2);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-v" && w[1] == "data:/var/lib/app")
        );
        assert_eq!(args.last().unwrap(), "ghcr.io/acme/app:edge");
    }

    // --- phase: the_form_is_a_modal_with_named_fields ---
    {
        let mut d = offline();
        d.key("n");
        let image = named(&d, "Image", ui::accesskit::Role::TextInput);
        d.perform_a11y_action(image, &ui::a11y::Action::SetValue("alpine:3.20".into()))
            .unwrap();
        assert_eq!(
            ViewDoc::create_field(doc(&mut d), CreateField::Image),
            "alpine:3.20"
        );
        let _ = named(&d, "New container", ui::accesskit::Role::Dialog);
    }
}

/// Entering a distrobox builds its terminal, and the canonical log resource
/// opens the container's logs.
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
        let (app, state) = d.root_and_state_mut::<ContainersApp>();
        assert!(
            app.start_operation(
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
        assert_eq!(ViewDoc::section(doc(&mut d)), Section::Containers);
        assert_eq!(ViewDoc::selected(doc(&mut d)).as_deref(), Some(id.as_str()));
        assert_eq!(ViewDoc::tab(doc(&mut d)), DetailTab::Logs);
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
        assert_eq!(ViewDoc::section(doc(&mut d)), Section::Volumes);
        d.key("4");
        assert_eq!(ViewDoc::section(doc(&mut d)), Section::Distrobox);
        d.key("2");
        assert_eq!(ViewDoc::section(doc(&mut d)), Section::Images);
        d.key("1");
        assert_eq!(ViewDoc::section(doc(&mut d)), Section::Containers);
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
        assert_eq!(ViewDoc::section(doc(&mut d)), Section::Images);

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
            ViewDoc::selected(doc(&mut d)).as_deref(),
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
        assert_eq!(ViewDoc::tab(doc(&mut d)), DetailTab::Stats);
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
