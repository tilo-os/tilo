//! Photos — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default              — the live winit event loop via `ui::App`.
//!   * `--open PATH`        — start with that photograph open.
//!   * `--shots PATH`       — render one settled frame to a PNG and exit.
//!     `--scenario NAME` picks the pose (see [`photos::SHOT_SCENARIOS`]),
//!     `--viewport WxH` the size and `--theme light|dark` the palette.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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

use tilo_content::ContentStore;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Default live path: framework-hosted ResourceApp. Shots, imports and
    // explicit file opens keep the standalone widget launcher.
    if !photos::standalone_launch() {
        ui::framework::run::<photos::PhotosShell>();
        return Ok(());
    }
    ui::launch::run(
        &LaunchSpec::new_with_args("so.tilo.photos", live_root)
            .history(&photos::framework_history::HISTORY)
            .open_app_global_state_with_args(open_state)
            .shot_root(shot_root)
            .shot_viewport(|_| (1280, 800))
            .allowed_live_flags(&["--analyze-imports", "--library"])
            .allowed_live_options(&["--open", "--import-directory"])
            .max_positionals(1)
            .scenarios(photos::SHOT_SCENARIOS)
            .configure_app(configure_app),
    )?;
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by the `LaunchSpec::new_with_args` root constructor callback"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    // Live and headless launches restore the same persistent image content.
    photos::strings::install();
    let mut app = photos::PhotosApp::new_persistent(ContentStore::for_app());
    if args.value("--import-directory").is_some() || args.has("--library") {
        app.show_library();
    }
    Ok(Box::new(app))
}

fn requested_path(args: &LaunchArgs) -> Option<&str> {
    args.value("--open")
        .or_else(|| args.positionals().first().map(String::as_str))
}

fn open_state(app_id: &'static str, args: &LaunchArgs) -> (ui::State, ui::persist::DocWriter) {
    let content = ContentStore::for_app();
    let (mut state, autosave) = ui::persist::open(app_id);
    if let Some(path) = requested_path(args) {
        let mut opener = photos::PhotosApp::new_persistent(content.clone());
        photos::strings::install();
        opener.open_path(&mut state.doc, path);
    }
    if let Some(directory) = args.value("--import-directory") {
        import_directory_at_startup(
            &mut state.doc,
            &content,
            directory,
            args.has("--analyze-imports"),
        );
    }
    (state, autosave)
}

/// An explicit launch import is useful for first-run migration and acceptance
/// testing. It uses the production bounded indexer and genuine local analysis;
/// no fixture labels, dates, locations, descriptions, or people are injected.
fn import_directory_at_startup(
    doc: &mut ui::automerge::AutoCommit,
    content: &ContentStore,
    directory: &str,
    analyze: bool,
) {
    use std::sync::atomic::AtomicBool;

    let cancelled = AtomicBool::new(false);
    let Ok(report) = photos::import::index_directory(
        std::path::Path::new(directory),
        content,
        photos::import::ImportOptions::default(),
        &cancelled,
        |_| {},
    ) else {
        return;
    };
    let capability = if analyze {
        tilo_vision::discover_local_capability(&agent_provider_config::ProviderConfig::load())
    } else {
        None
    };
    let mut library = photos::doc::PhotosDoc::library(doc);
    for mut indexed in report.photos {
        if analyze {
            let result = capability
                .as_ref()
                .and_then(|model| {
                    photos::analysis::analyze_image(&indexed.photo.image, model, content).ok()
                })
                .or_else(|| {
                    photos::analysis::recognize_text(&indexed.photo.image, "eng", content).ok()
                });
            if let Some(mut analysis) = result {
                "shared".clone_into(&mut analysis.provenance.scope);
                if let Some(provenance) = &mut analysis.ocr_provenance {
                    "shared".clone_into(&mut provenance.scope);
                }
                if let Some(provenance) = &mut analysis.vision_provenance {
                    "shared".clone_into(&mut provenance.scope);
                }
                if !analysis.searchable_text().is_empty() {
                    indexed.photo.analysis = Some(analysis);
                }
            }
        }
        let _ = library.add_photo(indexed.photo);
    }
    photos::doc::PhotosDoc::set_library(doc, &library);
}

fn configure_app(app: &mut ui::App) {
    photos::actions::register(app.action_registry_mut());
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
}

/// The app posed for `--scenario`: it seeds the scenario's library into the
/// capture's document on its first sync, writing its generated photographs
/// beside the output so a shot run leaves nothing where the live app looks.
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by the `LaunchSpec::shot_root` factory callback"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    photos::strings::install();
    let scenario = args.scenario.as_deref().unwrap_or("opened").to_owned();
    let staging = args
        .shots
        .as_deref()
        .and_then(std::path::Path::parent)
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("photos-shots");
    let mut app = photos::PhotosApp::new();
    app.stage_scenario(&scenario, &staging);
    Ok(Box::new(app))
}
