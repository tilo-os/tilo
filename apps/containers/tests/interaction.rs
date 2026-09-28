//! End-to-end interaction + self-inspecting render tests for the containers
//! app, driven through the REAL production paths:
//!
//! * the `Widget` trait cycle (`sync` → `build_layout` → `compute_layout` →
//!   `apply_layout` → `draw`), and
//! * `handle_event` with synthesized winit events (clicks + key presses).
//!
//! The render assertions read pixels back from `OffscreenRenderer` — the same
//! renderer `--shots` uses — and assert the surface actually painted content
//! (it is not blank), so a "shot" is verified, not just produced.

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
use containers::combobox;
use containers::doc::{CreateField, DetailTab, Section, ViewDoc};
use containers::model::sample_engine;
use containers::terminal;
use taffy::prelude::*;
use ui::headless::Framed;
use ui::offscreen::OffscreenRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;

const VW: u32 = 1280;
const VH: u32 = 800;

/// A tiny driver that runs the widget through the production frame host and
/// can dispatch synthesized events, mirroring `ui::App` / the `--shots` path.
/// The app is mounted on its first frame; until then it and its document are
/// driven directly, exactly as before any frame ran.
struct Driver {
    unmounted: Option<(ContainersApp, ui::State)>,
    mounted: Option<Framed<ContainersApp>>,
    root: automerge::ObjId,
    theme: Theme,
}

impl Driver {
    fn offline() -> Self {
        let state = ui::State::new();
        let root = state.root.clone();
        Driver {
            unmounted: Some((ContainersApp::new(false), state)),
            mounted: None,
            root,
            theme: Theme::dark(),
        }
    }

    fn parts(&mut self) -> (&mut ContainersApp, &mut AutoCommit) {
        if let Some(framed) = self.mounted.as_mut() {
            let (app, state) = Framed::split(framed);
            (app, &mut state.doc)
        } else {
            let (app, state) = self.unmounted.as_mut().expect("driver owns its app");
            (app, &mut state.doc)
        }
    }

    fn app(&mut self) -> &mut ContainersApp {
        self.parts().0
    }

    fn doc(&mut self) -> &mut AutoCommit {
        self.parts().1
    }

    /// Run one full frame (sync → layout → draw) at the default viewport.
    fn frame(&mut self) {
        self.frame_sized(VW, VH);
    }

    /// Same production frame at an explicit viewport, rebuilt from scratch.
    /// Responsive tests use this instead of a mock geometry so the renderer,
    /// hit testing, and Taffy layout all see the phone/tablet dimensions.
    fn frame_sized(&mut self, width: u32, height: u32) {
        if let Some(framed) = self.mounted.as_mut() {
            Framed::harness_mut(framed)
                .relayout_themed((width, height), self.theme.clone())
                .expect("containers frame lays out");
        } else {
            let (app, state) = self.unmounted.take().expect("driver owns its app");
            self.mounted = Some(Framed::new(state, app, (width, height), self.theme.clone()));
        }
    }

    /// Move the cursor and left-click, routing through `handle_event`.
    fn click(&mut self, x: f64, y: f64) {
        self.dispatch(&cursor_at(Point::new(x, y)));
        self.dispatch(&left_press());
    }

    /// Type a single character through `handle_event` (`KeyboardInput`).
    fn type_char(&mut self, ch: &str) {
        self.dispatch(&char_press(ch));
    }

    fn key(&mut self, named: winit::keyboard::NamedKey) {
        self.dispatch(&named_press(named));
    }

    fn dispatch(&mut self, event: &winit::event::WindowEvent) {
        self.dispatch_modifiers(event, winit::event::Modifiers::default());
    }

    fn dispatch_modifiers(
        &mut self,
        event: &winit::event::WindowEvent,
        modifiers: winit::event::Modifiers,
    ) {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let root = self.root.clone();
        let (app, doc) = self.parts();
        app.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// Render the cached scene to RGBA for pixel inspection.
    fn rgba(&self) -> Vec<u8> {
        self.rgba_sized(VW, VH)
    }

    fn rgba_sized(&self, width: u32, height: u32) -> Vec<u8> {
        let framed = self.mounted.as_ref().expect("a frame has rendered");
        let mut r = OffscreenRenderer::new(width, height);
        r.render_rgba(Framed::harness(framed).scene())
    }
}

/// Count non-transparent pixels — a blank surface scores ~0.
fn painted_pixels(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|px| px[3] > 8).count()
}

// --- event scaffolding (zeroed winit structs, as the nav_split tests do) ---

fn synth_device() -> winit::event::DeviceId {
    unsafe { std::mem::zeroed() }
}
fn cursor_at(p: Point) -> winit::event::WindowEvent {
    winit::event::WindowEvent::CursorMoved {
        device_id: synth_device(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}
fn left_press() -> winit::event::WindowEvent {
    winit::event::WindowEvent::MouseInput {
        device_id: synth_device(),
        state: winit::event::ElementState::Pressed,
        button: winit::event::MouseButton::Left,
    }
}
fn char_press(ch: &str) -> winit::event::WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = winit::keyboard::Key::Character(ch.into());
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = winit::event::ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(ch.into());
    winit::event::WindowEvent::KeyboardInput {
        device_id: synth_device(),
        event: ev,
        is_synthetic: false,
    }
}
fn named_press(named: winit::keyboard::NamedKey) -> winit::event::WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = winit::keyboard::Key::Named(named);
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = winit::event::ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    winit::event::WindowEvent::KeyboardInput {
        device_id: synth_device(),
        event: ev,
        is_synthetic: false,
    }
}

// =====================================================================
// Render self-inspection (verifies the shot is real, not blank)
// =====================================================================

/// Containers, images and volumes sections, the create modal with an open combobox and the compact detail drill-in render.
#[test]
fn sections_and_modals_render_non_blank() {
    // --- phase: containers_list_renders_non_blank ---
    {
        let mut d = Driver::offline();
        d.frame();
        let rgba = d.rgba();
        let painted = painted_pixels(&rgba);
        // A populated containers list paints a large fraction of the surface.
        assert!(
            painted > (VW * VH / 10) as usize,
            "containers list should paint substantial content, got {painted} px"
        );
        // The navigation rail (left strip) must be painted (it's the raised
        // surface) — sample a pixel inside the rail.
        let (_, _, _, a) = OffscreenRenderer::sample(&rgba, VW, 30, 120);
        assert!(a > 8, "navigation rail should be painted");
    }

    // --- phase: compact_container_detail_is_a_readable_drill_in_and_escape_returns_to_list ---
    {
        const PHONE_W: u32 = 390;
        const PHONE_H: u32 = 844;

        let mut d = Driver::offline();
        d.theme = Theme::dark().with_text_scale(2.0);
        d.frame_sized(PHONE_W, PHONE_H);

        let selected = ViewDoc::selected(d.doc());
        assert!(selected.is_some(), "the demo opens a representative detail");
        // This pixel assertion exercises the full production draw path at the
        // formerly broken phone + 200%-text combination. The compact rail and
        // full-width detail both contribute substantial, visible output.
        let rgba = d.rgba_sized(PHONE_W, PHONE_H);
        assert!(
            painted_pixels(&rgba) > (PHONE_W * PHONE_H / 9) as usize,
            "compact detail must paint a real full-page surface"
        );

        // Escape is the keyboard equivalent of the visible Back button. It must
        // return to the full-width list rather than leave focus in a hidden pane.
        d.key(winit::keyboard::NamedKey::Escape);
        assert!(
            ViewDoc::selected(d.doc()).is_none(),
            "Escape returns from compact detail to the container list"
        );
        d.frame_sized(PHONE_W, PHONE_H);
        assert!(
            painted_pixels(&d.rgba_sized(PHONE_W, PHONE_H)) > (PHONE_W * PHONE_H / 10) as usize,
            "the returned compact list remains visibly populated"
        );
    }

    // --- phase: images_section_renders_non_blank ---
    {
        let mut d = Driver::offline();
        d.frame(); // initial
        ViewDoc::set_section(d.doc(), Section::Images);
        d.frame();
        assert_eq!(ViewDoc::section(d.doc()), Section::Images);
        let rgba = d.rgba();
        assert!(
            painted_pixels(&rgba) > (VW * VH / 12) as usize,
            "images list paints content"
        );
    }

    // --- phase: volumes_section_renders_non_blank ---
    {
        let mut d = Driver::offline();
        d.frame();
        ViewDoc::set_section(d.doc(), Section::Volumes);
        d.frame();
        let rgba = d.rgba();
        assert!(
            painted_pixels(&rgba) > (VW * VH / 14) as usize,
            "volumes list paints content"
        );
    }

    // --- phase: create_modal_with_open_combobox_renders ---
    {
        let mut d = Driver::offline();
        d.frame();
        // Open create, focus image, type "n", open the dropdown.
        d.app().open_create_with_combo();
        ViewDoc::set_create_field(d.doc(), CreateField::Image, "n");
        d.frame();
        let rgba = d.rgba();
        // The scrim + modal + dropdown must paint over the base — expect a lot of
        // painted pixels including the centered modal region.
        let (_, _, _, a) = OffscreenRenderer::sample(&rgba, VW, VW / 2, VH / 2);
        assert!(a > 8, "create modal center should be painted");
        assert!(
            painted_pixels(&rgba) > (VW * VH / 6) as usize,
            "modal overlay paints widely"
        );
    }
}

// =====================================================================
// Interaction: combobox filtering through the real key path
// =====================================================================

/// The image combobox filters through the event path, picks with arrow and Enter, and keeps a custom typed value.
#[test]
fn image_combobox_filters_picks_and_keeps_custom() {
    // --- phase: combobox_filters_as_you_type_via_event_path ---
    {
        let mut d = Driver::offline();
        d.frame();
        // Open the create modal and focus the image combobox.
        d.app().open_create_with_combo();

        // Baseline: empty filter shows every local image.
        let all = sample_engine().image_references();
        assert_eq!(
            combobox::filter_options(&all, ""),
            (0..all.len()).collect::<Vec<_>>()
        );

        // Type "n" then "g" then "i" → narrows toward nginx via the REAL key path.
        d.type_char("n");
        d.type_char("g");
        d.type_char("i");
        let typed = ViewDoc::create_field(d.doc(), CreateField::Image);
        assert_eq!(
            typed, "ngi",
            "typed chars spliced into the CRDT image field"
        );

        let filtered = combobox::filter_options(&all, &typed);
        assert!(
            all[filtered[0]].contains("nginx"),
            "'ngi' filters to nginx: {:?}",
            filtered.iter().map(|i| &all[*i]).collect::<Vec<_>>()
        );
        // Typing keeps the dropdown open.
        assert!(
            d.app().is_combo_open(),
            "typing keeps the suggestion dropdown open"
        );
    }

    // --- phase: combobox_arrow_then_enter_picks_a_suggestion ---
    {
        let mut d = Driver::offline();
        d.frame();
        d.app().open_create_with_combo();
        // Filter to the postgres/redis/etc set with an empty query, move down once,
        // and Enter to commit the highlighted suggestion into the field.
        d.key(winit::keyboard::NamedKey::ArrowDown);
        let all = sample_engine().image_references();
        let expected = all[1].clone(); // highlight moved to index 1
        d.key(winit::keyboard::NamedKey::Enter);
        let committed = ViewDoc::create_field(d.doc(), CreateField::Image);
        assert_eq!(
            committed, expected,
            "Enter commits the highlighted suggestion"
        );
        assert!(!d.app().is_combo_open(), "picking closes the dropdown");
    }

    // --- phase: typing_custom_image_keeps_value_not_in_list ---
    {
        // The whole point of a *typeable* combobox: a custom reference that is NOT
        // a local image stays as the field value.
        let mut d = Driver::offline();
        d.frame();
        d.app().open_create_with_combo();
        for ch in ["g", "h", "c", "r"] {
            d.type_char(ch);
        }
        let typed = ViewDoc::create_field(d.doc(), CreateField::Image);
        assert_eq!(typed, "ghcr", "custom typed value is preserved verbatim");
        let all = sample_engine().image_references();
        assert!(
            combobox::filter_options(&all, &typed).is_empty(),
            "custom value matches no local image, yet remains the field value"
        );
    }
}

/// The create form uses shared input and CRDT IME text, keeps dynamic tab order and footer policy, hides the background and assembles the podman run.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn create_form_input_order_validation_and_command() {
    // --- phase: create_field_selection_replacement_uses_shared_input_path ---
    {
        let mut d = Driver::offline();
        d.frame();
        d.app().open_create_with_combo();
        ViewDoc::set_create_field(d.doc(), CreateField::Image, "nginx:latest");
        d.frame();

        d.dispatch_modifiers(
            &char_press("a"),
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
        );
        d.type_char("p");
        assert_eq!(
            ViewDoc::create_field(d.doc(), CreateField::Image),
            "p",
            "Ctrl+A then typing replaces the selection instead of appending"
        );
    }

    // --- phase: create_field_ime_commit_replicates_as_crdt_text ---
    {
        let mut d = Driver::offline();
        d.frame();
        d.app().open_create_with_combo();
        let mut peer = d.doc().fork();

        d.dispatch(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "接続".into(),
        )));
        assert_eq!(ViewDoc::create_field(d.doc(), CreateField::Image), "接続");
        peer.merge_incoming(d.doc()).expect("peer merge");
        assert_eq!(
            ViewDoc::create_field(&peer, CreateField::Image),
            "接続",
            "the shared input splices the existing collaborative Text object"
        );
    }

    // --- phase: tab_keeps_dynamic_form_order_and_image_combo_policy ---
    {
        let mut d = Driver::offline();
        d.frame();
        d.app().open_create_with_combo();
        assert!(d.app().is_combo_open());

        d.key(winit::keyboard::NamedKey::Tab);
        assert!(
            !d.app().is_combo_open(),
            "leaving Image closes its suggestions"
        );
        d.type_char("8");
        assert_eq!(
            ViewDoc::create_field(d.doc(), CreateField::Ports),
            "8",
            "Tab advances from Image to Ports in the existing six-field order"
        );
    }

    // --- phase: create_footer_preserves_click_validation_and_keyboard_policy ---
    {
        // Exercise Containers' existing hit/action policy; only paint is shared.
        let mut d = Driver::offline();
        d.frame();
        d.app().open_create_with_combo();
        d.frame();

        // Button centres for the 520×540 modal in this 1280×800 driver.
        d.click(828.0, 636.0);
        assert!(d.app().is_create_open());

        ViewDoc::set_create_field(d.doc(), CreateField::Image, "nginx:latest");
        d.frame();
        d.key(winit::keyboard::NamedKey::Enter);
        assert!(!d.app().is_create_open());

        d.app().open_create_with_combo();
        d.frame();
        d.click(722.0, 636.0);
        assert!(!d.app().is_create_open());

        d.app().open_create_with_combo();
        d.key(winit::keyboard::NamedKey::Escape);
        assert!(d.app().is_create_open() && !d.app().is_combo_open());
        d.key(winit::keyboard::NamedKey::Escape);
        assert!(!d.app().is_create_open());
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
        // Exact shape: run -d --name web -p ... -p ... -e ... -e ... -v ... -v ... <image>
        assert_eq!(&args[0..2], &["run", "-d"]);
        assert!(args.windows(2).any(|w| w[0] == "--name" && w[1] == "web"));
        assert_eq!(args.iter().filter(|a| *a == "-p").count(), 2);
        assert_eq!(args.iter().filter(|a| *a == "-e").count(), 2);
        assert_eq!(args.iter().filter(|a| *a == "-v").count(), 2);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-v" && w[1] == "data:/var/lib/app")
        );
        // Image is the last positional (no command override here).
        assert_eq!(args.last().unwrap(), "ghcr.io/acme/app:edge");
    }

    // --- phase: create_dialog_hides_background_and_exposes_editable_fields ---
    {
        let mut d = Driver::offline();
        d.frame();
        d.app().open_create_with_combo();
        d.frame();
        let tree = d.app().accessibility().expect("create dialog is semantic");
        let nodes = semantic_nodes(&tree);
        assert!(nodes.iter().any(|node| {
            node.role == ui::a11y::Role::Dialog && node.name.as_deref() == Some("Create container")
        }));
        assert!(nodes.iter().any(|node| {
            node.action_target.as_deref() == Some("containers.create.field:image")
                && node.role == ui::a11y::Role::TextInput
        }));
        assert!(
            !nodes.iter().any(|node| {
                node.action_target
                    .as_deref()
                    .is_some_and(|target| target.starts_with("containers.section:"))
            }),
            "modal semantics do not leak the background navigation"
        );

        let (app, doc) = d.parts();
        assert!(app.perform_semantic_action(
            "containers.create.field:image",
            &ui::a11y::Action::SetValue("alpine:3.20".into()),
            doc,
            &automerge::ROOT,
        ));
        assert_eq!(
            ViewDoc::create_field(d.doc(), CreateField::Image),
            "alpine:3.20"
        );
    }
}

// =====================================================================
// Interaction: create assembles the right podman args
// =====================================================================

// =====================================================================
// Interaction: enter-distrobox builds the right terminal command
// =====================================================================

/// Entering a distrobox builds its terminal and the canonical log resource opens the collaborative detail.
#[test]
fn container_resources_open_terminal_and_log_detail() {
    // --- phase: enter_distrobox_builds_terminal_running_distrobox_enter ---
    {
        // The inner command the Tilo terminal runs must invoke `distrobox enter`.
        let inner = terminal::distrobox_enter_command("my-distrobox");
        assert!(inner.contains("distrobox enter 'my-distrobox'"), "{inner}");
        // And the spawned Command must target the terminal binary with SHELL set
        // to a launcher that runs that inner command.
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
        // The launcher on disk really runs the distrobox-enter command.
        let body = std::fs::read_to_string(&shell).expect("launcher written");
        assert!(body.contains("distrobox enter 'my-distrobox'"), "{body}");
    }

    // --- phase: canonical_log_resource_opens_collaborative_detail_state ---
    {
        let mut d = Driver::offline();
        let id = sample_engine().containers[1].id.clone();
        let payload = serde_json::to_vec(&vec!["logs", id.as_str()]).unwrap();
        let (app, doc) = d.parts();
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    ui::tilo_actions::OperationKind::View,
                    "containers.logs",
                    &payload,
                ),
                doc,
                &automerge::ROOT,
            )
            .map(|started| started.is_some())
            .unwrap()
        );
        let (app, doc) = d.parts();
        app.sync(doc, &automerge::ROOT);
        assert_eq!(ViewDoc::section(d.doc()), Section::Containers);
        assert_eq!(ViewDoc::selected(d.doc()).as_deref(), Some(id.as_str()));
        assert_eq!(ViewDoc::tab(d.doc()), DetailTab::Logs);
    }
}

// =====================================================================
// Interaction: navigation + collapsible sidebar
// =====================================================================

/// Number keys switch sections, the sidebar toggles via shortcut, row clicks select, and painted controls have named semantics reusing hit dispatch.
#[test]
fn keyboard_pointer_and_semantic_navigation() {
    // --- phase: number_keys_navigate_sections ---
    {
        let mut d = Driver::offline();
        d.frame();
        d.type_char("3"); // volumes
        assert_eq!(ViewDoc::section(d.doc()), Section::Volumes);
        d.type_char("4"); // distrobox
        assert_eq!(ViewDoc::section(d.doc()), Section::Distrobox);
        d.type_char("2"); // images
        assert_eq!(ViewDoc::section(d.doc()), Section::Images);
        d.type_char("1"); // containers
        assert_eq!(ViewDoc::section(d.doc()), Section::Containers);
    }

    // --- phase: sidebar_collapses_and_expands_via_shortcut ---
    {
        let mut d = Driver::offline();
        d.frame();
        let wide = d.app().rail_width();
        assert!(d.app().rail_expanded(), "sidebar starts expanded");
        // 'b' toggles the collapse.
        d.type_char("b");
        // Pump frames so the eased slide settles toward collapsed.
        for _ in 0..400 {
            d.frame();
            if !d.app().rail_expanded() {
                break;
            }
        }
        let narrow = d.app().rail_width();
        assert!(
            narrow < wide,
            "rail narrows when collapsed ({narrow} < {wide})"
        );
        assert!(!d.app().rail_expanded(), "labels hidden once collapsed");
        // Toggle back open.
        d.type_char("b");
        for _ in 0..400 {
            d.frame();
            if d.app().rail_expanded() {
                break;
            }
        }
        assert!(
            d.app().rail_expanded(),
            "rail re-expands on a second toggle"
        );
    }

    // --- phase: clicking_a_container_row_changes_selection ---
    {
        let mut d = Driver::offline();
        d.frame();
        // The offline demo seeds an initial selection on first sync; clear it so
        // we observe the click producing a fresh selection.
        ViewDoc::set_selected(d.doc(), None);
        d.frame();
        assert!(ViewDoc::selected(d.doc()).is_none(), "selection cleared");
        // Click the first list row (below the header, in the list area).
        d.click(300.0, 140.0);
        d.frame();
        assert!(
            ViewDoc::selected(d.doc()).is_some(),
            "clicking a container row selects it and opens the detail pane"
        );
    }

    // --- phase: painted_controls_have_named_semantics_and_activation_reuses_hit_dispatch ---
    {
        let mut d = Driver::offline();
        d.frame(); // consume the one-shot demo selection
        ViewDoc::set_selected(d.doc(), None);
        d.frame();
        let expected = sample_engine().containers[0].id.clone();
        let target = format!("containers.container:{expected}:select");
        let tree = d
            .app()
            .accessibility()
            .expect("custom canvas exports a semantic window");
        assert_eq!(tree.role, ui::a11y::Role::Window);
        let nodes = semantic_nodes(&tree);
        assert!(nodes.iter().any(|node| {
            node.action_target.as_deref() == Some(target.as_str())
                && node.role == ui::a11y::Role::ListItem
                && node.name.as_deref().is_some_and(|name| !name.is_empty())
                && node.bounds.is_some()
        }));
        let (app, doc) = d.parts();
        assert!(app.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            doc,
            &automerge::ROOT,
        ));
        assert_eq!(
            ViewDoc::selected(d.doc()).as_deref(),
            Some(expected.as_str())
        );
    }
}

fn semantic_nodes(node: &ui::a11y::AccessNode) -> Vec<&ui::a11y::AccessNode> {
    fn walk<'a>(node: &'a ui::a11y::AccessNode, out: &mut Vec<&'a ui::a11y::AccessNode>) {
        out.push(node);
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}
