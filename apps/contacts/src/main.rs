//! Contacts — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).

use std::collections::HashMap;

use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        // `--shots` renders one offscreen frame. A `--viewport WxH` arg
        // overrides the default desktop viewport so the same binary can
        // capture the narrow, single-pane responsive layout; `--detail`
        // snaps the narrow push flow to the pushed detail pane (otherwise
        // a narrow frame opens to the list, as a fresh window would).
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let show_detail = args.iter().any(|a| a == "--detail");
        let scenario = parse_scenario_arg(&args);
        // Screenshots use the hermetic sample book so a headless frame is
        // populated and never reads or writes the real address-book file.
        let mut app = contacts::ContactsApp::sample();
        if show_detail {
            app.shots_show_detail();
        }
        // A `--scenario NAME` arg snaps a specific UI surface into view so a
        // single headless frame captures it (new-contact form, export
        // selection dialog, unsaved-changes modal, right-click menu). The
        // hooks need the document seeded first, which `render_shot_to_png`
        // does before drawing — so they're applied through a closure run
        // *after* the sync pass.
        let setup: Box<dyn FnOnce(&mut contacts::ContactsApp)> = match scenario.as_deref() {
            Some("new") => Box::new(|a: &mut contacts::ContactsApp| a.shots_new_contact()),
            Some("edit") => Box::new(|a: &mut contacts::ContactsApp| a.shots_edit_contact()),
            Some("editfocus") => Box::new(|a: &mut contacts::ContactsApp| a.shots_edit_focused()),
            Some("export") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_export()),
            Some("unsaved") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_unsaved()),
            Some("context") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_context_menu()),
            Some("confirm") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_confirm_delete()),
            Some("empty") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_empty()),
            _ => Box::new(|_: &mut contacts::ContactsApp| {}),
        };
        render_shot_to_png(app, viewport, &out, setup)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, contacts::app_root(), "dev.qos.contacts"));
    }

    // Contacts kept its own address-book file before the shared store
    // existed; import it once so an existing user's book comes across.
    let (state, autosave) = match contacts::storage::default_path() {
        Some(legacy) => ui::persist::open_migrating("dev.qos.contacts", &legacy),
        None => ui::persist::open("dev.qos.contacts"),
    };
    let mut app = ui::App::new(state, contacts::app_root()).with_autosave(autosave);
    // Register the contacts actions so the shell's Ctrl+K palette can decode +
    // replay the same contact mutations the on-screen affordances dispatch.
    contacts::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

/// Parse `--viewport WxH` (e.g. `--viewport 480x820`) for `--shots`.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let spec = if arg == "--viewport" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--viewport=")
        };
        if let Some(spec) = spec {
            let (w, h) = spec.split_once(['x', 'X'])?;
            return Some((w.trim().parse().ok()?, h.trim().parse().ok()?));
        }
    }
    None
}

/// Parse `--scenario NAME` (e.g. `--scenario new`) for `--shots`, selecting
/// which non-default UI surface to snap into view for the captured frame.
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
fn render_shot_to_png(
    mut app: contacts::ContactsApp,
    viewport: (u32, u32),
    out: &std::path::Path,
    setup: Box<dyn FnOnce(&mut contacts::ContactsApp)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    // Apply the scenario hook now that the document is seeded; the hooks
    // (new-contact form, export dialog, …) operate on the live store.
    setup(&mut app);
    let widget: &mut dyn Widget = &mut app;

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;

    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("contacts: wrote shot to {}", out.display());
    Ok(())
}
