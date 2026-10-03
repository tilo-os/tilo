#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Sheets — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — framework [`ResourceApp`] (`SheetsShell`).
//!   * `--shots PATH` — render one frame of the live window to a PNG and exit
//!     (headless screenshot; identical passes to the live app).
//!
//! Shots flags:
//!   * `--viewport WxH`     — override the desktop viewport (e.g. the narrow
//!     800x600 responsive layout).
//!   * `--scenario NAME`    — stage a specific surface for the frame:
//!     `file`, `format`, `more`, `docsettings`, `editing`,
//!     `pointmode`, `summary`, `multiselect`, `tabmenu`, `tabcolour`, `tabrename`,
//!     `commandpalette`, `loading`, `error`, `conflict`, `reducedmotion`.
//!     `more` opens the overflow menu, which only exists when the command bar
//!     does not fit: pair it with `--viewport 520x600` or `--text-scale 2`.
//!     `tabcolour` colours both tabs and opens the colour popover.
//!
//! The shots path always seeds the sample workbook and disables on-disk
//! persistence, so a screenshot shows formulas without touching the
//! user's stored data.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

const APP_ID: &str = "so.tilo.sheets";
const SCENARIOS: &[&str] = &[
    "file",
    "format",
    "more",
    "docsettings",
    "editing",
    "pointmode",
    "summary",
    "multiselect",
    "tabmenu",
    "tabcolour",
    "tabrename",
    "fbarselect",
    "commandpalette",
    "fillseries",
    "scrollbars",
    "pasteformula",
    "loading",
    "error",
    "conflict",
    "reducedmotion",
    "charts",
    "comments",
    "bold",
];

fn configure(app: &mut ui::App) {
    sheets::actions::register(app.action_registry_mut());
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    if room_launch() {
        ui::launch::run_or_exit(
            &LaunchSpec::from_definition(sheets::definition())
                .component_host(prepare_room_workbook)
                .native_services(),
        );
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new(APP_ID, sheets::app_root)
            .shot_root(shot_root)
            .scenarios(SCENARIOS)
            .configure_app(configure),
    );
}

/// A launch into a room (not a screenshot). There Sheets opens the room's
/// default record, the workbook every device's first Sheets window opens,
/// browsers included; standalone it keeps the one workbook it always had.
fn room_launch() -> bool {
    std::env::var("TILO_WORKSPACE_ID").is_ok_and(|room| !room.trim().is_empty())
        && !std::env::args().any(|arg| arg == "--shots" || arg == "--responsive-matrix")
}

fn prepare_room_workbook(
    definition: &ui::application::ApplicationDefinition,
    _args: &LaunchArgs,
) -> Result<ui::application::PreparedComponentInputs, LaunchError> {
    let inputs = ui::application::ComponentRunnerInputs::from_native_process();
    let mut native = ui::application::NativeProcessHost::open(definition)?;
    let instance_id = native.instance_id();
    let workbook = ui::record::RecordCheckout::new(
        ui::record::RecordId::room_default(),
        definition.record_kind.clone(),
    );
    native
        .host_mut()
        .prepare_default(definition, instance_id, workbook, None, inputs)
}

/// The root a `--shots` capture renders: the same window as a live launch,
/// seeded with the sample workbook and without on-disk persistence, with the
/// named scenario staged on it. The launcher lays it out, settles it and
/// captures it, exactly as it does the live window.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let mut app = sheets::SheetsApp::new();
    app.shots_mode();
    if let Some(scenario) = args.scenario.as_deref() {
        app.stage_scenario(scenario);
    }
    Ok(Box::new(app))
}
