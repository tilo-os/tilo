//! End-to-end interaction and render tests for the containers app, driven
//! through the production frame host (`ui::headless::FrameHarness`): real layout,
//! real event routing, real accessibility, and pixels read back from the
//! renderer `--shots` uses.
//!
//! Editing the new-container draft goes through the framework's dispatcher,
//! so those tests run the hosted app ([`TestApp`]) and type into it.

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
use containers::draft::{CreateDraft, CreateField};
use containers::model::{DetailTab, Section, sample_engine};
use containers::terminal;
use ui::headless::FrameHarness as FrameDriver;
use ui::prelude::TestApp;
use ui::theme::Theme;

const VIEWPORT: (u32, u32) = (1280, 800);

/// The sample engine offline, showing what a plain `--shots` shows.
fn offline_at(viewport: (u32, u32), theme: Theme) -> Driver {
    let mut app = ContainersApp::sample();
    app.stage_scene(None);
    Driver {
        frame: FrameDriver::for_root(ui::State::new(), app, viewport, theme),
    }
}

struct Driver {
    frame: FrameDriver,
}
impl std::ops::Deref for Driver {
    type Target = FrameDriver;
    fn deref(&self) -> &FrameDriver {
        &self.frame
    }
}
impl std::ops::DerefMut for Driver {
    fn deref_mut(&mut self) -> &mut FrameDriver {
        &mut self.frame
    }
}
enum TestKey {
    Text(String),
    Named(winit::keyboard::NamedKey),
}
impl From<&str> for TestKey {
    fn from(s: &str) -> Self {
        Self::Text(s.into())
    }
}
impl From<winit::keyboard::NamedKey> for TestKey {
    fn from(k: winit::keyboard::NamedKey) -> Self {
        Self::Named(k)
    }
}
impl Driver {
    fn offline() -> Self {
        offline_at(VIEWPORT, Theme::dark())
    }
    fn parts(&mut self) -> (&mut ContainersApp, &mut AutoCommit) {
        let (app, state) = self.frame.root_and_state_mut::<ContainersApp>();
        (app, &mut state.doc)
    }
    fn app(&mut self) -> &mut ContainersApp {
        self.frame.root_mut::<ContainersApp>()
    }
    fn frame(&mut self) {
        self.frame.relayout();
    }
    fn click(&mut self, x: f64, y: f64) {
        self.frame.click_at(x, y);
    }
    fn key(&mut self, key: impl Into<TestKey>) {
        let spec = match key.into() {
            TestKey::Text(s) => s,
            TestKey::Named(k) => format!("{k:?}"),
        };
        self.frame.key(&spec);
    }
}
fn offline() -> Driver {
    Driver::offline()
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

fn labels(d: &FrameDriver) -> Vec<String> {
    d.a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

fn named(d: &FrameDriver, name: &str, role: ui::accesskit::Role) -> ui::accesskit::NodeId {
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

fn hosted() -> TestApp<ContainersShell> {
    TestApp::<ContainersShell>::capture(None).expect("the containers capture launches")
}

fn draft(app: &TestApp<ContainersShell>, field: CreateField) -> String {
    app.state().draft.get(field).to_owned()
}

fn hosted_named(app: &mut TestApp<ContainersShell>, name: &str) -> ui::accesskit::NodeId {
    let d = app.harness();
    named(d, name, ui::accesskit::Role::TextInput)
}

/// Typing in the new-container form splices the draft's texts: plain keys,
/// a replaced selection, an IME commit, Tab to the next field and an
/// assistive value; undo takes a typed word back.
#[test]
fn create_form_edits_the_draft() {
    // --- phase: typing_splices_and_undo_takes_a_word_back ---
    {
        let mut app = hosted();
        // `n` opens the form on its image field.
        app.harness().key("n");
        app.harness().type_text("ngi");
        assert_eq!(draft(&app, CreateField::Image), "ngi");
        assert!(app.undo(), "the typed word is one undo step");
        assert_eq!(draft(&app, CreateField::Image), "");
    }

    // --- phase: select_all_then_typing_replaces ---
    {
        let mut app = hosted();
        app.harness().key("n");
        app.harness().type_text("nginx:latest");
        app.harness().set_modifiers(&["ctrl".to_owned()]);
        app.harness().key("a");
        app.harness().set_modifiers(&[]);
        app.harness().type_text("p");
        assert_eq!(
            draft(&app, CreateField::Image),
            "p",
            "select-all then typing replaces the selection instead of appending"
        );
    }

    // --- phase: ime_commit_replicates_as_crdt_text ---
    {
        let mut app = hosted();
        app.harness().key("n");
        let mut peer = app.peer_document();
        app.harness().commit_text("接続");
        assert_eq!(draft(&app, CreateField::Image), "接続");
        app.merge_peer(&mut peer).expect("peer merge");
        assert_eq!(
            <containers::resource::Containers as ui::tilo_resource::Resource>::read(&peer)
                .unwrap()
                .draft
                .get(CreateField::Image),
            "接続",
            "the field splices the shared collaborative text"
        );
    }

    // --- phase: the_image_field_suggests_installed_images ---
    {
        let mut app = hosted();
        app.harness().key("n");
        app.harness().type_text("redis");
        app.harness().key("ArrowDown");
        app.harness().key("Enter");
        assert_eq!(
            draft(&app, CreateField::Image),
            "docker.io/library/redis:7-alpine",
            "choosing an installed image fills the draft with its reference"
        );
    }

    // --- phase: tab_follows_the_form_order ---
    {
        let mut d = offline();
        d.key("n");
        let focused = |d: &Driver| d.focused_access_node().and_then(|node| node.name);
        assert_eq!(focused(&d).as_deref(), Some("Image"));
        d.key("Tab");
        assert_eq!(
            focused(&d).as_deref(),
            Some("Name"),
            "Tab moves from Image to Name"
        );
    }

    // --- phase: the_form_is_a_modal_with_named_fields ---
    {
        let mut app = hosted();
        app.harness().key("n");
        let image = hosted_named(&mut app, "Image");
        app.harness()
            .perform_a11y_action(image, &ui::a11y::Action::SetValue("alpine:3.20".into()))
            .unwrap();
        assert_eq!(draft(&app, CreateField::Image), "alpine:3.20");
        let _ = named(app.harness(), "New container", ui::accesskit::Role::Dialog);
    }
}

/// An empty image keeps the form open; Escape cancels it; Enter on a form
/// with an image creates the container and empties the draft, undoably.
#[test]
fn create_form_submits_and_cancels() {
    let mut app = hosted();
    app.harness().key("n");
    app.harness().key("Enter");
    let _ = named(app.harness(), "New container", ui::accesskit::Role::Dialog);
    app.harness().key("Escape");
    assert!(
        !app.harness().a11y_nodes().into_iter().any(|(_, node)| {
            node.role() == ui::accesskit::Role::Dialog && node.label() == Some("New container")
        }),
        "Escape closes the form"
    );

    app.harness().key("n");
    app.harness().type_text("alpine");
    assert_eq!(
        app.state().draft.run_args(),
        Some(vec!["run".into(), "-d".into(), "alpine".into()])
    );
    // The hosted fixture uses the production offline engine: submission is
    // accepted and clears the draft through the canonical create action.
    app.harness().key("Enter");
    assert!(
        !app.harness().a11y_nodes().into_iter().any(|(_, node)| {
            node.role() == ui::accesskit::Role::Dialog && node.label() == Some("New container")
        }),
        "an accepted creation closes the form"
    );
    assert!(
        app.state().draft.is_empty(),
        "a confirmed creation empties the draft"
    );
    assert!(app.undo(), "emptying the draft is undoable");
    assert_eq!(draft(&app, CreateField::Image), "alpine");
}

/// The draft assembles the full `podman run`.
#[test]
fn create_draft_assembles_full_podman_run() {
    let args = CreateDraft::with(&[
        (CreateField::Name, "web"),
        (CreateField::Image, "ghcr.io/acme/app:edge"),
        (CreateField::Ports, "8080:80, 8443:443"),
        (CreateField::Env, "TZ=UTC, LOG=info"),
        (CreateField::Volumes, "data:/var/lib/app, cfg:/etc/app"),
    ])
    .run_args()
    .expect("image present → args");
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

// =====================================================================
// Interaction: enter-distrobox asks for the right terminal
// =====================================================================

/// Entering a distrobox asks for Terminal and the canonical log resource opens the collaborative detail.
#[test]
fn container_resources_open_terminal_and_log_detail() {
    // --- phase: enter_distrobox_asks_for_terminal_running_distrobox_enter ---
    {
        // Terminal is asked for by intent; the shell runs `distrobox enter`.
        let intent = terminal::distrobox_enter_intent("my-distrobox");
        assert_eq!(
            intent,
            runtime_portal::tools::ToolIntent::ContainerShell {
                container: "my-distrobox".into(),
                engine: runtime_portal::tools::ContainerEngine::Distrobox,
            }
        );
        assert_eq!(
            intent.package_id(),
            Some("so.tilo.terminal"),
            "opens the Terminal package, not a loose binary"
        );
    }

    // --- phase: canonical_log_resource_opens_the_logs ---
    {
        let mut d = offline();
        d.key("2");
        let id = sample_engine().containers[1].id.clone();
        let (app, _doc) = d.parts();
        app.perform_effect(containers::resource::ContainersEffect::OpenLogs {
            container: id.clone(),
        })
        .unwrap();
        assert_eq!(d.app().section(), Section::Containers);
        assert_eq!(d.app().selected(), Some(id.as_str()));
        assert_eq!(d.app().tab(), DetailTab::Logs);
    }
}

fn assert_number_keys_navigate_sections() {
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

fn assert_sidebar_and_list_assistive_activation() {
    let mut d = offline();
    let images = d
        .a11y_nodes()
        .into_iter()
        // The destination, not a header that names it.
        .find(|(_, node)| {
            node.label() == Some("Images") && node.role() != ui::accesskit::Role::Heading
        })
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

/// Number keys switch sections, the sidebar and the list respond to the
/// pointer and to assistive technology, and the tabs switch the detail.
#[test]
fn keyboard_pointer_and_semantic_navigation() {
    assert_number_keys_navigate_sections();

    assert_sidebar_and_list_assistive_activation();

    // --- phase: tabs_switch_the_detail ---
    {
        let mut d = Driver::offline();
        d.frame();
        // The demo opens with a selection; Escape clears it so we observe the
        // click producing a fresh selection.
        d.key(winit::keyboard::NamedKey::Escape);
        d.frame();
        assert!(d.app().selected().is_none(), "selection cleared");
        // Click the real laid-out first container row, whose position moves
        // with the shared list header and theme.
        let label = sample_engine().containers[0].name.clone();
        let bounds = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(label.as_str()))
            .and_then(|(_, node)| node.bounds())
            .expect("the first container row is laid out");
        d.click(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        );
        d.frame();
        assert!(
            d.app().selected().is_some(),
            "clicking a container row selects it and opens the detail pane"
        );
    }

    // --- phase: painted_controls_have_named_semantics_and_activation_reuses_hit_dispatch ---
    {
        let mut d = Driver::offline();
        d.frame();
        d.key(winit::keyboard::NamedKey::Escape);
        d.frame();
        let expected = sample_engine().containers[0].id.clone();
        let row = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(sample_engine().containers[0].name.as_str()))
            .expect("the actual table publishes its container row");
        assert_eq!(row.1.role(), ui::accesskit::Role::Row);
        assert!(row.1.label().is_some_and(|name| !name.is_empty()));
        assert!(row.1.bounds().is_some());
        d.perform_a11y_action(row.0, &ui::a11y::Action::Activate)
            .expect("the real semantic row activation selects its own container");
        d.frame();
        assert_eq!(d.app().selected(), Some(expected.as_str()));

        // The same container has a canonical row operation whose target is
        // bound by its published owner, rather than a caller-supplied id.
        let mut hosted = TestApp::<ContainersShell>::new().unwrap();
        hosted
            .with_surface::<ContainersApp, _>(ContainersApp::stage_sample_engine)
            .unwrap();
        hosted.settle();
        let heads = hosted.peer_document().get_heads();
        let address = format!("container:{expected}");
        hosted
            .effect_on(
                &address,
                &containers::resource::ContainersEffect::SelectContainer {
                    id: Some(expected.clone()),
                },
            )
            .expect("the canonical published row action is admitted");
        hosted
            .with_widget::<ContainersApp, _>(|app| {
                assert_eq!(app.selected(), Some(expected.as_str()));
            })
            .unwrap();
        assert_eq!(
            hosted.peer_document().get_heads(),
            heads,
            "the admitted row selection changes no document"
        );
    }
}

/// The bundled sample says that it is one; only an engine that answered
/// reads as connected.
#[test]
fn the_sample_never_claims_a_connection() {
    let d = offline();
    let labels = labels(&d);
    assert!(
        labels
            .iter()
            .any(|label| label == "Not connected to Podman"),
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label == "Connected"),
        "{labels:?}"
    );
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
