//! tilolang: the runtime binary that runs one installed tilolang app.
//!
//! A tilolang package is an app of its own: its signed manifest names the tilolang runtime
//! bundle as its entrypoint (`[entrypoint] runtime = "tilolang/<version>"`), and the launcher
//! starts this binary from that bundle with `TILO_PACKAGE_ID` (the package's signed id) and
//! `TILO_APP_MODULE` (its payload: the compiled module, its metadata and its surface). The
//! binary builds the app's definition from them, so each package has its own id, window, record,
//! custody and permissions. It compiles nothing: the package was compiled by `tilolang build`.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "layout weights, counts and sizes are small values; see crates/platform/tilolang-tilo"
)]

pub mod i18n;
pub mod names;
#[cfg(test)]
pub mod packages;
pub mod page;
pub mod reconcile;
pub mod search;

use std::sync::{Arc, OnceLock};

use tilolang_tilo::bundle::{Bundle, BundleError};
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::launch::LaunchError;
use ui::record::RecordKind;
use ui::widget::IconDescriptor;

/// The id the runtime answers to when no package is installed in it: only tests and captures of
/// the built-in packages run like that.
pub const UNBUNDLED_ID: &str = "so.tilo.lang";

/// The record kind of an app's room record.
pub const RECORD_KIND: &str = "tilolang.room";

/// The variable the launcher names the package's signed id with.
pub const PACKAGE_ID_ENV: &str = "TILO_PACKAGE_ID";

static INSTALLED: OnceLock<Arc<Bundle>> = OnceLock::new();

/// Why the runtime cannot run the package it was started for.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error(
        "the package was signed as `{signed}`, but its module declares the app `{declared}`; \
         the runtime runs a module only under the id it was signed for"
    )]
    IdMismatch { signed: String, declared: String },
    #[error("a package is already installed in this process")]
    AlreadyInstalled,
}

/// Run `bundle` in this process: the definition every host mounts is its app's. Once per
/// process, before the definition is first asked for.
///
/// # Errors
/// A package is already installed.
pub fn install(bundle: Bundle) -> Result<(), InstallError> {
    INSTALLED
        .set(Arc::new(bundle))
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Install the package the launcher started this process for: the payload `TILO_APP_MODULE`
/// names, under the id `TILO_PACKAGE_ID` names.
///
/// # Errors
/// The payload is missing or broken, or its module declares another app than it was signed as.
pub fn install_from_env() -> Result<(), InstallError> {
    let bundle = Bundle::from_env()?;
    if let Some(signed) = std::env::var_os(PACKAGE_ID_ENV).filter(|id| !id.is_empty()) {
        let signed = signed.to_string_lossy();
        if signed != bundle.meta.app.as_str() {
            return Err(InstallError::IdMismatch {
                signed: signed.into_owned(),
                declared: bundle.meta.app,
            });
        }
    }
    install(bundle)
}

/// The package this process runs, once installed.
#[must_use]
pub fn installed() -> Option<Arc<Bundle>> {
    INSTALLED.get().cloned()
}

/// The icon a definition shows for `bundle`: the platform icon its `app` declaration names.
fn icon_of(bundle: Option<&Bundle>) -> IconDescriptor {
    let name = bundle
        .and_then(|bundle| bundle.meta.icon.as_deref())
        .map_or("code-editor", |name| name);
    IconDescriptor::Named(name.replace('_', "-"))
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
    let device = context.device_id().to_owned();
    // A capture names the scenario it stages; the live path never reads one.
    let scenario = context
        .capture()
        .and_then(|capture| capture.scenario())
        .map(str::to_owned);
    let mut registry = ui::tilo_actions::ActionRegistry::new();
    tilolang_tilo::register_actions(&mut registry);
    let page = match scenario {
        Some(scenario) => page::TilolangPage::staged(me, &scenario),
        None => page::TilolangPage::new(me).on_device(&device),
    };
    let page = match installed() {
        Some(bundle) => page.running(bundle),
        None => page,
    };
    Ok(AppComponent::new(context, Box::new(page), registry))
}

/// The app as every host mounts it: the installed package's ([`install`]).
pub fn definition() -> &'static ApplicationDefinition {
    static DEFINITION: OnceLock<ApplicationDefinition> = OnceLock::new();
    DEFINITION.get_or_init(|| {
        let bundle = installed();
        let app_id = bundle
            .as_ref()
            .map_or_else(|| UNBUNDLED_ID.to_owned(), |bundle| bundle.meta.app.clone());
        let title = bundle
            .as_ref()
            .map_or_else(|| "tilolang".to_owned(), |bundle| bundle.meta.title.clone());
        // Receivers decide each change against the surface; the launcher names the package's
        // own file (`TILO_SURFACE`), which the host registers. Without a launcher (a test, a
        // capture) the surface is generated from the same metadata here, before the first
        // signed operation.
        if std::env::var_os(ui::app_surface::SURFACE_FILE_ENV).is_none() {
            let surface = bundle.as_ref().map_or_else(
                || {
                    #[cfg(test)]
                    {
                        packages::surface()
                    }
                    #[cfg(not(test))]
                    {
                        tilolang_tilo::surface::surface_toml(&[]).unwrap_or_default()
                    }
                },
                |bundle| bundle.surface(),
            );
            ui::app_surface::register_package(&app_id, &surface);
        }
        ApplicationDefinition {
            icon: icon_of(bundle.as_deref()),
            app_id: app_id.into(),
            title: title.into(),
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

/// How the runtime launches: the live window, or a capture of one of the staged scenarios.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::definition_launch_spec(definition())
        .scenarios(page::SCENARIOS)
        .capture_scenario(settle_capture)
}
