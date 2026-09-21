#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Contacts — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//! * default        — run the live winit event loop via `ui::App`.
//! * `--shots PATH` — render one offscreen frame to a PNG and exit
//!   (headless screenshot; identical passes to the live app).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
    // selection dialog, unsaved-changes modal, group filter, right-click menu). The
    // hooks need the document seeded first, which `render_shot_to_png`
    // does before drawing — so they're applied through a closure run
    // *after* the sync pass.
    let setup: Box<dyn FnOnce(&mut contacts::ContactsApp)> = match scenario {
        Some("new") => Box::new(|a: &mut contacts::ContactsApp| a.shots_new_contact()),
        Some("edit") => Box::new(|a: &mut contacts::ContactsApp| a.shots_edit_contact()),
        Some("editfocus") => Box::new(|a: &mut contacts::ContactsApp| a.shots_edit_focused()),
        Some("edit-lower") => Box::new(|a: &mut contacts::ContactsApp| a.shots_edit_lower()),
        Some("export") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_export()),
        Some("unsaved") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_unsaved()),
        Some("groups") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_group_filter()),
        Some("context") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_context_menu()),
        Some("confirm") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_confirm_delete()),
        Some("import-conflict") => {
            Box::new(|a: &mut contacts::ContactsApp| a.shots_show_import_conflict())
        }
        Some("duplicate-review") => {
            Box::new(|a: &mut contacts::ContactsApp| a.shots_show_duplicate_review())
        }
        Some("duplicates") => {
            Box::new(|a: &mut contacts::ContactsApp| a.shots_show_duplicate_discovery())
        }
        Some("empty") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_empty()),
        Some("search") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_search()),
        Some("loading") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_loading()),
        Some("error") => Box::new(|a: &mut contacts::ContactsApp| a.shots_show_error()),
        _ => Box::new(|_: &mut contacts::ContactsApp| {}),
    };
    render_shot_to_png(app, viewport, ui::launch::shot_theme(args), out, setup)
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
    ui::launch::run_or_exit(&launch_spec());
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut app: contacts::ContactsApp,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    setup: Box<dyn FnOnce(&mut contacts::ContactsApp)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
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

    let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the bound nodes come from this exact exclusively owned build.
    unsafe {
        bindings.rebuild(widget, &taffy, child, &node_map);
        ui::widget::compute_widget_layout(
            &mut taffy,
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            &node_map,
            &bindings,
            &mut text_renderer,
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

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new("dev.tilo.contacts", contacts::app_root)
        .history(&contacts::framework_history::HISTORY)
        .shot_handler(shot_handler)
        .allowed_flags(&["--detail"])
        .scenarios(&[
            "new",
            "edit",
            "editfocus",
            "export",
            "unsaved",
            "groups",
            "context",
            "confirm",
            "import-conflict",
            "empty",
        ])
        .open_app_global_state(open_state)
        .configure_app(|app| contacts::actions::register(app.action_registry_mut()))
}

#[test]
fn production_launch_registers_history() {
    launch_spec().validate_history_declaration().unwrap();
}

#[test]
fn actual_migrating_opener_keeps_saved_graph() {
    use automerge::{ReadDoc, transaction::Transactable};
    const APP: &str = "dev.tilo.contacts";
    if let Ok(stage) = std::env::var("TILO_CONTACTS_HISTORY_STARTUP") {
        let legacy = contacts::storage::default_path().unwrap();
        if stage == "write" {
            let mut doc = automerge::AutoCommit::new();
            let mut contact = contacts::model::sample_contacts().remove(0);
            contact.first = "Historical imported contact".into();
            contacts::doc::ContactsDoc::upsert_contact(&mut doc, &contact);
            let historical = doc.get_heads()[0];
            contact.first = "Current imported contact".into();
            contacts::doc::ContactsDoc::upsert_contact(&mut doc, &contact);
            let bytes = doc.save();
            std::fs::write(&legacy, &bytes).unwrap();
            let (mut state, mut saver) = open_state(APP);
            assert!(saver.is_enabled());
            assert!(state.doc.get_change_by_hash(&historical).is_some());
            assert_eq!(
                contacts::doc::ContactsDoc::list_contacts(&state.doc)[0].first,
                "Current imported contact"
            );
            let current = state.doc.get_heads();
            let prepared = ui::history_mode::restore::prepare_content_restore(
                &state.doc,
                &contacts::framework_history::SCHEMA,
                &current,
                &[historical],
            )
            .unwrap();
            ui::history_mode::restore::apply_prepared_content_restore(&mut state.doc, &prepared)
                .unwrap();
            state.commit_action(None, None);
            saver.request(&mut state.doc);
            saver.try_flush().unwrap();
            assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
            std::fs::write(legacy.with_extension("heads"), historical.to_string()).unwrap();
        } else {
            let (state, saver) = open_state(APP);
            assert!(saver.is_enabled());
            assert_eq!(
                contacts::doc::ContactsDoc::list_contacts(&state.doc)[0].first,
                "Historical imported contact"
            );
            let historical = std::fs::read_to_string(legacy.with_extension("heads"))
                .unwrap()
                .parse()
                .unwrap();
            assert!(state.doc.get_change_by_hash(&historical).is_some());
            assert_eq!(state.doc.pending_ops(), 0);
            assert!(
                state
                    .doc
                    .get(automerge::ROOT, "contacts")
                    .unwrap()
                    .is_some()
            );
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for stage in ["write", "read"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "actual_migrating_opener_keeps_saved_graph",
                "--nocapture",
            ])
            .env("TILO_CONTACTS_HISTORY_STARTUP", stage)
            .env("XDG_DATA_HOME", dir.path())
            .env("TILO_PACKAGE_ID", APP)
            .env("TILO_CONTACTS_DATA", dir.path().join("legacy.automerge"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{stage}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
