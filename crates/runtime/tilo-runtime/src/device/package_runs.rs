//! The installed packages a tilolang host runs: which apps are tilolang apps, each verified as a
//! launch verifies it, and the outside service an app reaches through its origin connector.
//!
//! What a host is handed is read from the installation verified here, never from what an app
//! says of itself ([`super::processes`]).

use launcher::run::tilolang_process::VerifiedInstance;

/// Whether the installed app `app` is a tilolang app: its manifest's entrypoint runs a tilolang
/// runtime. Read only to decide how its windows are launched; what a process runs is read from the
/// installation verified as a launch verifies it.
#[must_use]
pub fn runs_in_process(app: &str) -> bool {
    app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .and_then(|installed| launcher::manifest::read_manifest(&installed.manifest_path).ok())
        .and_then(|manifest| manifest.entrypoint.runtime)
        .is_some_and(|runtime| launcher::run::tilolang_process::is_tilolang(&runtime))
}

/// An installed tilolang app as an agent task's programs reach it: its package, verified as a
/// launch verifies it.
pub struct TaskPackage {
    pub app: String,
    pub module: runtime_portal::process_link::Module,
}

/// Every installed tilolang app a task's programs name, each verified as a launch verifies it.
/// The task's process runs in the caller, so no process of the app's runtimes is started. A
/// program names an app by what its package declares (its actions, its roots), which only the
/// package itself holds: a program is compiled against its declarations, not against the surface
/// it publishes.
#[must_use]
pub fn task_packages() -> Vec<TaskPackage> {
    let apps = match app_catalogue::apps::read_from_env() {
        Ok(apps) => apps,
        Err(error) => {
            tracing::warn!(%error, "a task names no tilolang app: the installed apps are not known");
            return Vec::new();
        }
    };
    apps.iter()
        .filter(|app| runs_in_process(&app.app_id))
        .filter_map(|app| match verified_package(&app.app_id) {
            Ok(verified) => Some(TaskPackage {
                app: app.app_id.to_string(),
                module: verified.module,
            }),
            Err(error) => {
                tracing::warn!(package_id = %app.app_id, %error, "a task leaves out an app it could not verify");
                None
            }
        })
        .collect()
}

/// The tilolang and Tilo runtimes an agent task's host runs on: those of the first tilolang app
/// installed here that verifies on runtimes installed here, since the host compiles a task's
/// programs against the apps' packages.
///
/// # Errors
/// No tilolang app installed here runs on runtimes this device has.
pub fn task_runtimes() -> Result<(String, String), String> {
    let apps = app_catalogue::apps::read_from_env()
        .map_err(|error| format!("the installed apps are not known: {error}"))?;
    apps.iter()
        .filter(|app| runs_in_process(&app.app_id))
        .find_map(|app| verified_installed("", &app.app_id).ok())
        .map(|verified| (verified.runtime, verified.app_runtime))
        .ok_or_else(|| {
            "no tilolang app installed here runs on runtimes this device has, so a task's \
             programs have nowhere to run"
                .to_owned()
        })
}

/// The tilolang apps installed here, each by its id: what an instance tells another app it uses
/// by.
pub(super) fn installed_apps() -> std::collections::BTreeMap<String, String> {
    let apps = match app_catalogue::apps::read_from_env() {
        Ok(apps) => apps,
        Err(error) => {
            tracing::warn!(%error, "an instance reads no other app: the installed apps are not known");
            return std::collections::BTreeMap::new();
        }
    };
    apps.iter()
        .filter(|app| runs_in_process(&app.app_id))
        .map(|app| (app.app_id.to_string(), app.app_id.to_string()))
        .collect()
}

/// The one origin (`imap`, `youtube`) the installed app `app` reaches an outside service through,
/// when it declares one, and whether its signed manifest grants that origin's connector the
/// workspace's account sign-ins ([`tilo_app_package::manifest::ACCOUNT_SIGN_INS`]).
///
/// # Errors
/// The manifest does not read, or it declares more than one origin.
pub fn origin_of(app: &str) -> Result<Option<(String, bool)>, String> {
    let installed = app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .ok_or_else(|| format!("{app} is not installed here"))?;
    let manifest = launcher::manifest::read_manifest(&installed.manifest_path)
        .map_err(|error| format!("reading {app}'s manifest: {error}"))?;
    match manifest.entrypoint.origins.as_slice() {
        [] => Ok(None),
        [origin] => Ok(Some((
            origin.clone(),
            manifest.declares_capability(tilo_app_package::manifest::ACCOUNT_SIGN_INS),
        ))),
        more => Err(format!(
            "{app} declares {} origins; an instance reaches one connector",
            more.len()
        )),
    }
}

/// `app`, installed here, verified as a launch verifies it.
fn verified_package(app: &str) -> Result<VerifiedInstance, String> {
    let installed = package_store::find_installed(app)
        .map_err(|error| format!("the package store cannot be read: {error}"))?
        .ok_or_else(|| format!("{app} is not installed"))?;
    launcher::run::tilolang_process::verified_instance(
        &installed,
        &installed.manifest_path,
        &installed.payload_root,
        &installed.package_id_hex,
    )
    .map_err(|error| error.to_string())
}

/// `app`, installed here, verified as a launch verifies it, on runtimes installed here.
pub(super) fn verified_installed(_workspace: &str, app: &str) -> Result<VerifiedInstance, String> {
    let verified = verified_package(app)?;
    if !verified.runs_here() {
        return Err(format!(
            "{app} names runtimes not installed here ({}, {})",
            verified.runtime, verified.app_runtime
        ));
    }
    Ok(verified)
}
