//! PDF — standard qOS application entry point.

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

const APP_ID: &str = "dev.qos.pdf-viewer";

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
        Some("search") => pdf_viewer::app_root_search(),
        Some("sign") => pdf_viewer::app_root_sign(),
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

fn live(args: &LaunchArgs) -> Result<(), LaunchError> {
    // The package document is the record index; each PDF's markup lives in a
    // separate record so annotations from two files cannot collide.
    let records = ui::RecordSet::open(APP_ID);
    let (room, autosave) = ui::persist::open_room(APP_ID, records.home());
    let mut app = ui::App::new(ui::State::new(), build_widget(args)?)
        .with_autosave(autosave)
        .with_records(records, room.doc);
    pdf_viewer::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args(APP_ID, build_widget)
            .shot_root(build_widget)
            .allowed_live_flags(&["--sample"])
            .max_positionals(1)
            .scenarios(&[
                "edit-comment",
                "selection-hud",
                "page-bubble",
                "search",
                "sign",
                "reorder",
                "import",
                "loading",
                "error",
            ])
            .live_handler(live),
    );
}
