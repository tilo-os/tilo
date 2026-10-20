//! tilolang: the host app that runs every tilolang app package.
//!
//! One Tilo app, `so.tilo.lang`, hosts every package in process. Each package is compiled at
//! startup from the source embedded with the app, runs in its own wasm instance, and keeps its
//! resources in the room's record. The window shows a home view of the packages and any
//! package's `main` view, rendered onto the shared layout primitives.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "layout weights, counts and sizes are small values; see crates/platform/tilolang-tilo"
)]

pub mod i18n;
pub mod names;
pub mod packages;
pub mod page;
pub mod reconcile;

use std::sync::OnceLock;

use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::launch::LaunchError;
use ui::record::RecordKind;
use ui::widget::IconDescriptor;

/// The app id of the host.
pub const APP_ID: &str = "so.tilo.lang";

/// The record kind of the room's tilolang record.
pub const RECORD_KIND: &str = "tilolang.room";

/// The surface file the host registers for its app id: the host resource and every built-in
/// package's kinds and verbs, generated from the packages' metadata.
fn surface_bytes() -> Vec<u8> {
    let sources: Vec<(&str, &str)> = packages::EMBEDDED
        .iter()
        .map(|package| (package.source, package.ids))
        .collect();
    tilolang_tilo::host::builtin_surface(&sources)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the component factory signature of `ApplicationDefinition::create`"
)]
fn create(
    mut context: HostContext,
    target: LaunchTarget,
    _restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    i18n::ensure();
    if matches!(target, LaunchTarget::New | LaunchTarget::PrimaryNew)
        || context.creation_workspace().is_some()
    {
        context
            .initialize_new_record(|_| Ok(()))
            .map_err(LaunchError::Record)?;
    }
    let me = context.author().0.clone();
    // A capture names the scenario it stages; the live path never reads one.
    let scenario = context
        .capture()
        .and_then(|capture| capture.scenario())
        .map(str::to_owned);
    let mut registry = ui::tilo_actions::ActionRegistry::new();
    tilolang_tilo::register_actions(&mut registry);
    Ok(AppComponent::new(
        context,
        Box::new(match scenario {
            Some(scenario) => page::TilolangPage::staged(me, &scenario),
            None => page::TilolangPage::new(me),
        }),
        registry,
    ))
}

/// The app as every host mounts it.
pub fn definition() -> &'static ApplicationDefinition {
    static DEFINITION: OnceLock<ApplicationDefinition> = OnceLock::new();
    DEFINITION.get_or_init(|| {
        // Receivers decide each change against this surface; name it before the first signed
        // operation.
        ui::app_surface::register_package(APP_ID, &surface_bytes());
        ApplicationDefinition {
            app_id: APP_ID,
            title: "tilolang",
            icon: IconDescriptor::Named("code-editor".into()),
            record_kind: RecordKind::new(RECORD_KIND),
            required: vec![],
            optional: vec![],
            opens: &[],
            room_record: true,
            create,
        }
    })
}

/// What a capture does after the window is mounted and before it is drawn: let the staged state
/// reach the record and the frames settle.
#[allow(clippy::unnecessary_wraps, reason = "the capture hook's signature")]
fn settle_capture(
    driver: &mut ui::headless::FrameHarness,
    _args: &ui::launch::LaunchArgs,
) -> Result<(), LaunchError> {
    for _ in 0..4 {
        driver.settle_for_capture(ui::time::Duration::from_secs(10));
    }
    Ok(())
}

/// How the host launches: the live window, or a capture of one of the staged scenarios.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::definition_launch_spec(definition())
        .scenarios(page::SCENARIOS)
        .capture_scenario(settle_capture)
}
