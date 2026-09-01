//! Contacts — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//! * default        — run the live winit event loop via `ui::App`.
//! * `--shots PATH` — render one offscreen frame to a PNG and exit
//!   (headless screenshot; identical passes to the live app).

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::HashMap;

use taffy::prelude::*;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    // `--shots` renders one offscreen frame. A `--viewport WxH` arg
    // overrides the default desktop viewport so the same binary can
    // capture the narrow, single-pane responsive layout; `--detail`
    // snaps the narrow push flow to the pushed detail pane (otherwise
    // a narrow frame opens to the list, as a fresh window would).
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let show_detail = args.has("--detail");
    let scenario = args.scenario.as_deref();
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
    let setup: Box<dyn FnOnce(&mut contacts::ContactsApp)> = match scenario {
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
    render_shot_to_png(app, viewport, out, setup)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

fn open_state(app_id: &'static str) -> (ui::State, ui::persist::Autosave) {
    // Contacts kept its own address-book file before the shared store
    // existed; import it once so an existing user's book comes across.
    match contacts::storage::default_path() {
        Some(legacy) => ui::persist::open_migrating(app_id, &legacy),
        None => ui::persist::open(app_id),
    }
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("dev.qos.contacts", contacts::app_root)
            .shot_handler(shot_handler)
            .allowed_flags(&["--detail"])
            .scenarios(&[
                "new",
                "edit",
                "editfocus",
                "export",
                "unsaved",
                "context",
                "confirm",
                "empty",
            ])
            .open_state(open_state)
            .configure_app(|app| contacts::actions::register(app.action_registry_mut())),
    );
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut app: contacts::ContactsApp,
    viewport: (u32, u32),
    out: &std::path::Path,
    setup: Box<dyn FnOnce(&mut contacts::ContactsApp)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
    // theme-driven chrome can be proven across palettes; defaults to dark.
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
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
