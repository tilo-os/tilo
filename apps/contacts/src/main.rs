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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;

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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // The opener imports a legacy address-book file once. The ResourceApp
    // shell does not, so an existing book would open empty.
    ui::launch::run_or_exit(&launch_spec());
}

/// Run one offscreen frame against `app` and write RGBA to `out` as PNG,
/// applying the scenario hook once the document is seeded.
fn render_shot_to_png(
    mut app: contacts::ContactsApp,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    setup: Box<dyn FnOnce(&mut contacts::ContactsApp)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme);
    frame.sync(&mut app);
    // Apply the scenario hook now that the document is seeded; the hooks
    // (new-contact form, export dialog, …) operate on the live store.
    setup(&mut app);
    frame.layout(&mut app)?;
    let scene = frame.draw(&mut app);
    frame.write_png(&scene, out);
    println!("contacts: wrote shot to {}", out.display());
    Ok(())
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.contacts", contacts::app_root)
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
        .configure_app(|app| contacts::actions::register(app.action_registry_mut()))
}

/// The production launch declares its history.
#[test]
fn production_launch_registers_history() {
    launch_spec().validate_history_declaration().unwrap();
}
