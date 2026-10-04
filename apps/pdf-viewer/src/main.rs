//! PDF — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic"
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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

const APP_ID: &str = "so.tilo.pdf-viewer";

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch's argument-aware root factory"
)]
fn build_widget(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let file = args.positionals().first().map(std::path::PathBuf::from);
    let root = match args.scenario.as_deref() {
        Some("edit-comment") => pdf_viewer::app_root_sample_editing(),
        Some("selection-hud") => pdf_viewer::app_root_selection_hud(),
        Some("page-bubble") => pdf_viewer::app_root_page_bubble(),
        Some("context-menu") => pdf_viewer::app_root_context_menu(),
        Some("search") => pdf_viewer::app_root_search(),
        Some("sign") => pdf_viewer::app_root_sign(),
        Some("sign-type") => pdf_viewer::app_root_sign_type(),
        Some("contents") => pdf_viewer::app_root_contents(),
        Some("marker-colours") => pdf_viewer::app_root_marker_colours(),
        Some("no-comments") => pdf_viewer::app_root_no_comments(),
        Some("reorder") => pdf_viewer::app_root_reorder_demo(file.as_deref()),
        Some("import") => pdf_viewer::app_root_import_demo(file.as_deref()),
        Some("loading") => pdf_viewer::app_root_loading(),
        Some("error") => pdf_viewer::app_root_error(),
        _ => file.as_deref().map_or_else(
            || {
                if args.has("--sample") {
                    pdf_viewer::app_root_sample()
                } else {
                    pdf_viewer::app_root()
                }
            },
            pdf_viewer::app_root_open,
        ),
    };
    Ok(root)
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn prepare_launch(args: &LaunchArgs) -> Result<ui::launch::LegacyLaunchInputs, LaunchError> {
    // Native and headless launches share the room index and each PDF's markup
    // record. Never adopt a clone of the package index as the annotation body:
    // diverging copies share an Automerge actor and cannot safely merge later.
    let records = ui::RecordSet::open(APP_ID);
    let (room, autosave) = ui::persist::open_room(APP_ID, records.home());
    let state = ui::State::new();
    let widget = build_widget(args)?;
    let mut registry = tilo_actions::ActionRegistry::new();
    pdf_viewer::actions::register(&mut registry);
    Ok(ui::launch::LegacyLaunchInputs::new(state, widget, autosave)
        .with_records(records, room.doc)
        .with_registry(registry))
}

fn shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let mut harness = ui::headless::FrameHarness::new_with_theme(
        ui::State::new(),
        build_widget(args)?,
        args.viewport.unwrap_or(ui::headless::DEFAULT_VIEWPORT),
        ui::launch::shot_theme(args),
    )
    .with_records(ui::RecordSet::in_memory(), automerge::AutoCommit::new());
    // The loading scenario intentionally demonstrates an unfinished open.
    if args.scenario.as_deref() != Some("loading") {
        harness
            .wait_for_visuals(std::time::Duration::from_secs(15))
            .map_err(LaunchError::Operation)?;
    }
    let out = args
        .shots
        .as_ref()
        .ok_or_else(|| LaunchError::Operation("Missing screenshot path".into()))?;
    harness.render_png(out);
    Ok(())
}

pub fn main() {
    if !pdf_viewer::standalone_launch() {
        ui::framework::run::<pdf_viewer::PdfViewerShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args(APP_ID, build_widget)
            .shot_root(build_widget)
            .shot_handler(shot)
            .allowed_live_flags(&["--sample"])
            .max_positionals(1)
            .scenarios(&[
                "edit-comment",
                "selection-hud",
                "page-bubble",
                "context-menu",
                "search",
                "sign",
                "sign-type",
                "contents",
                "marker-colours",
                "no-comments",
                "reorder",
                "import",
                "loading",
                "error",
            ])
            .prepare_legacy_with_args(prepare_launch),
    );
}
