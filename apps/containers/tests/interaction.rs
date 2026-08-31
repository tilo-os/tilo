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

use std::collections::HashMap;

use automerge::AutoCommit;
use containers::app::ContainersApp;
use containers::combobox;
use containers::doc::{CreateField, Section, ViewDoc};
use containers::model::sample_engine;
use containers::terminal;
use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

const VW: u32 = 1280;
const VH: u32 = 800;

/// A tiny driver that runs the widget through one full layout+draw cycle and
/// can dispatch synthesized events, mirroring `ui::App` / the `--shots` path.
struct Driver {
    app: ContainersApp,
    doc: AutoCommit,
    root: automerge::ObjId,
    theme: Theme,
    last_scene: Scene,
}

impl Driver {
    fn offline() -> Self {
        let state = ui::State::new();
        Driver {
            app: ContainersApp::new(false),
            doc: state.doc,
            root: state.root,
            theme: Theme::dark(),
            last_scene: Scene::new(),
        }
    }

    /// Run sync → `build_layout` → `compute_layout` → `apply_layout` → draw, caching
    /// the produced scene for inspection.
    #[allow(
        clippy::cast_precision_loss,
        reason = "VW/VH are fixed viewport constants (1280/800), both exactly \
                  representable in f32 (well under its 2^24 exact-integer range)"
    )]
    fn frame(&mut self) {
        self.app.sync(&mut self.doc, &self.root);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = self
            .app
            .build_layout(&mut taffy, &self.theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(VW as f32),
                        height: Dimension::length(VH as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let mut tr = TextRenderer::new();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(VW as f32),
                        height: AvailableSpace::Definite(VH as f32),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        self.app.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        let presence = PresenceCtx::empty();
        self.app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &self.theme,
            &presence,
        );
        self.last_scene = scene;
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
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        self.app.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut self.doc,
            &self.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// Render the cached scene to RGBA for pixel inspection.
    fn rgba(&self) -> Vec<u8> {
        let mut r = OffscreenRenderer::new(VW, VH);
        r.render_rgba(&self.last_scene)
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

#[test]
fn containers_list_renders_non_blank() {
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

#[test]
fn images_section_renders_non_blank() {
    let mut d = Driver::offline();
    d.frame(); // initial
    ViewDoc::set_section(&mut d.doc, Section::Images);
    d.frame();
    assert_eq!(ViewDoc::section(&mut d.doc), Section::Images);
    let rgba = d.rgba();
    assert!(
        painted_pixels(&rgba) > (VW * VH / 12) as usize,
        "images list paints content"
    );
}

#[test]
fn volumes_section_renders_non_blank() {
    let mut d = Driver::offline();
    d.frame();
    ViewDoc::set_section(&mut d.doc, Section::Volumes);
    d.frame();
    let rgba = d.rgba();
    assert!(
        painted_pixels(&rgba) > (VW * VH / 14) as usize,
        "volumes list paints content"
    );
}

#[test]
fn create_modal_with_open_combobox_renders() {
    let mut d = Driver::offline();
    d.frame();
    // Open create, focus image, type "n", open the dropdown.
    d.app.open_create_with_combo();
    ViewDoc::set_create_field(&mut d.doc, CreateField::Image, "n");
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

// =====================================================================
// Interaction: combobox filtering through the real key path
// =====================================================================

#[test]
fn combobox_filters_as_you_type_via_event_path() {
    let mut d = Driver::offline();
    d.frame();
    // Open the create modal and focus the image combobox.
    d.app.open_create_with_combo();

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
    let typed = ViewDoc::create_field(&mut d.doc, CreateField::Image);
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
        d.app.is_combo_open(),
        "typing keeps the suggestion dropdown open"
    );
}

#[test]
fn combobox_arrow_then_enter_picks_a_suggestion() {
    let mut d = Driver::offline();
    d.frame();
    d.app.open_create_with_combo();
    // Filter to the postgres/redis/etc set with an empty query, move down once,
    // and Enter to commit the highlighted suggestion into the field.
    d.key(winit::keyboard::NamedKey::ArrowDown);
    let all = sample_engine().image_references();
    let expected = all[1].clone(); // highlight moved to index 1
    d.key(winit::keyboard::NamedKey::Enter);
    let committed = ViewDoc::create_field(&mut d.doc, CreateField::Image);
    assert_eq!(
        committed, expected,
        "Enter commits the highlighted suggestion"
    );
    assert!(!d.app.is_combo_open(), "picking closes the dropdown");
}

#[test]
fn typing_custom_image_keeps_value_not_in_list() {
    // The whole point of a *typeable* combobox: a custom reference that is NOT
    // a local image stays as the field value.
    let mut d = Driver::offline();
    d.frame();
    d.app.open_create_with_combo();
    for ch in ["g", "h", "c", "r"] {
        d.type_char(ch);
    }
    let typed = ViewDoc::create_field(&mut d.doc, CreateField::Image);
    assert_eq!(typed, "ghcr", "custom typed value is preserved verbatim");
    let all = sample_engine().image_references();
    assert!(
        combobox::filter_options(&all, &typed).is_empty(),
        "custom value matches no local image, yet remains the field value"
    );
}

// =====================================================================
// Interaction: create assembles the right podman args
// =====================================================================

#[test]
fn create_form_assembles_full_podman_run() {
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
    let args = ViewDoc::create_run_args(&mut doc).expect("image present → args");
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

// =====================================================================
// Interaction: enter-distrobox builds the right terminal command
// =====================================================================

#[test]
fn enter_distrobox_builds_terminal_running_distrobox_enter() {
    // The inner command the qOS terminal runs must invoke `distrobox enter`.
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
    assert!(shell.contains("qos-term-enter-my-distrobox.sh"), "{shell}");
    // The launcher on disk really runs the distrobox-enter command.
    let body = std::fs::read_to_string(&shell).expect("launcher written");
    assert!(body.contains("distrobox enter 'my-distrobox'"), "{body}");
}

// =====================================================================
// Interaction: navigation + collapsible sidebar
// =====================================================================

#[test]
fn number_keys_navigate_sections() {
    let mut d = Driver::offline();
    d.frame();
    d.type_char("3"); // volumes
    assert_eq!(ViewDoc::section(&mut d.doc), Section::Volumes);
    d.type_char("4"); // distrobox
    assert_eq!(ViewDoc::section(&mut d.doc), Section::Distrobox);
    d.type_char("2"); // images
    assert_eq!(ViewDoc::section(&mut d.doc), Section::Images);
    d.type_char("1"); // containers
    assert_eq!(ViewDoc::section(&mut d.doc), Section::Containers);
}

#[test]
fn sidebar_collapses_and_expands_via_shortcut() {
    let mut d = Driver::offline();
    d.frame();
    let wide = d.app.rail_width();
    assert!(d.app.rail_expanded(), "sidebar starts expanded");
    // 'b' toggles the collapse.
    d.type_char("b");
    // Pump frames so the eased slide settles toward collapsed.
    for _ in 0..400 {
        d.frame();
        if !d.app.rail_expanded() {
            break;
        }
    }
    let narrow = d.app.rail_width();
    assert!(
        narrow < wide,
        "rail narrows when collapsed ({narrow} < {wide})"
    );
    assert!(!d.app.rail_expanded(), "labels hidden once collapsed");
    // Toggle back open.
    d.type_char("b");
    for _ in 0..400 {
        d.frame();
        if d.app.rail_expanded() {
            break;
        }
    }
    assert!(d.app.rail_expanded(), "rail re-expands on a second toggle");
}

#[test]
fn clicking_a_container_row_changes_selection() {
    let mut d = Driver::offline();
    d.frame();
    // The offline demo seeds an initial selection on first sync; clear it so
    // we observe the click producing a fresh selection.
    ViewDoc::set_selected(&mut d.doc, None);
    d.frame();
    assert!(ViewDoc::selected(&mut d.doc).is_none(), "selection cleared");
    // Click the first list row (below the header, in the list area).
    d.click(300.0, 140.0);
    d.frame();
    assert!(
        ViewDoc::selected(&mut d.doc).is_some(),
        "clicking a container row selects it and opens the detail pane"
    );
}
