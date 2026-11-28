//! The installed packages a tilolang host runs: which apps are tilolang apps, each verified as a
//! launch verifies it, and the outside service an app reaches through its origin connector.
//!
//! What a host is handed is read from the installation verified here, never from what an app
//! says of itself ([`super::processes`]).

/// What a tilolang host is handed for one installed tilolang app, read from its installation
/// verified as a launch of it verifies it.
#[derive(Debug)]
pub struct Verified {
    /// The tilolang runtime its code runs on, `tilolang/<version>`.
    pub runtime: String,
    /// The Tilo runtime it names, `tilo-runtime/<version>`: what a host on Linux runs on beside the
    /// tilolang runtime. A Mac's tilolang runtime carries everything it loads, so there it only
    /// tells hosts apart.
    pub app_runtime: String,
    /// Its module, from the verified payload.
    pub module: runtime_portal::process_link::Module,
}

/// What a workspace trusts for packages: the attestors it accepts and the publisher keys it
/// refuses.
pub type PackageTrust = (
    Vec<tilo_peer_state::TrustAttestor>,
    Vec<tilo_peer_state::RevokedPublisher>,
);

/// What `workspace` trusts for packages, as `copies` hold it now, without waiting on its lock:
/// nothing where this device holds no copy of it.
pub(crate) fn package_trust_now(
    copies: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
) -> PackageTrust {
    tilo_desktop_model::WorkspaceId::parse(workspace)
        .and_then(|id| copies.copy_now(id))
        .map(|copy| {
            (
                copy.peer.governance().trusted_attestors(),
                copy.peer.governance().revoked_publishers(),
            )
        })
        .unwrap_or_default()
}

/// The publisher keys `workspace` refuses, as `copies` hold it now: what an app read or run in
/// it is verified against.
pub(crate) fn revoked_now(
    copies: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
) -> tilo_app_package::RevocationList {
    revocations(&package_trust_now(copies, workspace).1)
}

/// The publisher keys `revoked` names, as a launch's verification takes them.
pub(crate) fn revocations(
    revoked: &[tilo_peer_state::RevokedPublisher],
) -> tilo_app_package::RevocationList {
    tilo_app_package::RevocationList {
        revoked: revoked
            .iter()
            .map(|key| tilo_app_package::RevokedKey {
                pubkey_hex: key.pubkey_hex.clone(),
                reason: key.reason.clone(),
            })
            .collect(),
    }
}

/// Whether the runtime `raw` (`<name>/<version>`) is here: installed as a bundle a launch finds,
/// or carried by this device's own runtime, as a phone carries the one its apps run on.
#[must_use]
pub fn runtime_installed(raw: &str) -> bool {
    package_store::runtime_installed(raw) || runtime_bundle::carried(raw).is_some()
}

/// Whether the installed app `app` is a tilolang app: its payload carries the module its records
/// and views are, on a tilolang runtime it names. Its entrypoint may be the binary of that
/// runtime, or a program's runtime (`bun`) that the app also carries; either way its code runs in
/// a tilolang host. Read only to decide how its windows are launched; what a process runs is read
/// from the installation verified as a launch verifies it.
#[must_use]
pub fn runs_in_process(app: &str) -> bool {
    app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .and_then(|installed| {
            let manifest = launcher::manifest::read_manifest(&installed.manifest_path).ok()?;
            launcher::manifest::code_runtime(&manifest, &installed.payload_root).map(|_| ())
        })
        .is_some()
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
///
/// The package's modules are the device's compiled ones: the daemon compiled them with it
/// ([`super::packages`]) and the task's host reads them from the store it is granted read-only
/// ([`tilo_wasm_aot`]), as an app's own window does. A module the daemon did not compile runs on
/// the interpreter there.
#[cfg(target_os = "linux")]
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
        .filter_map(|app| match verified_package(&app.app_id, &desktop_revocations()) {
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
#[cfg(target_os = "android")]
pub fn task_runtimes() -> Result<(String, String), String> {
    Err(
        "Android starts no process for an app: its apps run in the Tilo app's own process"
            .to_owned(),
    )
}

/// The tilolang and Tilo runtimes an agent task's host runs on: those of the first tilolang app
/// installed here that verifies on runtimes installed here, since the host compiles a task's
/// programs against the apps' packages.
///
/// # Errors
/// No tilolang app installed here runs on runtimes this device has.
#[cfg(not(target_os = "android"))]
pub fn task_runtimes() -> Result<(String, String), String> {
    let apps = app_catalogue::apps::read_from_env()
        .map_err(|error| format!("the installed apps are not known: {error}"))?;
    apps.iter()
        .filter(|app| runs_in_process(&app.app_id))
        // Only the runtimes are read here: what a workspace refuses is checked where its app runs.
        .find_map(|app| {
            verified_installed(
                "",
                &app.app_id,
                &tilo_app_package::RevocationList::default(),
            )
            .ok()
        })
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
/// when it declares one.
///
/// # Errors
/// The manifest does not read, or it declares more than one origin.
pub fn origin_of(app: &str) -> Result<Option<String>, String> {
    let installed = app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .ok_or_else(|| format!("{app} is not installed here"))?;
    let manifest = launcher::manifest::read_manifest(&installed.manifest_path)
        .map_err(|error| format!("reading {app}'s manifest: {error}"))?;
    match manifest.entrypoint.origins.as_slice() {
        [] => Ok(None),
        [origin] => Ok(Some(origin.clone())),
        more => Err(format!(
            "{app} declares {} origins; an instance reaches one connector",
            more.len()
        )),
    }
}

/// The publisher keys the desktop's workspace refuses, as its trust reading answers them: what a
/// task's programs, which run the apps' code in the caller, are verified against.
#[cfg(target_os = "linux")]
fn desktop_revocations() -> tilo_app_package::RevocationList {
    revocations(
        &tilo_control::packages::trust()
            .1
            .into_iter()
            .map(|key| tilo_peer_state::RevokedPublisher {
                pubkey_hex: key.pubkey_hex,
                reason: key.reason,
            })
            .collect::<Vec<_>>(),
    )
}

/// `app`, installed here, verified as `tilo-launcher` verifies a launch, its publisher not in
/// `revoked`.
#[cfg(target_os = "linux")]
fn verified_package(
    app: &str,
    revoked: &tilo_app_package::RevocationList,
) -> Result<launcher::run::tilolang_process::VerifiedInstance, String> {
    let installed = package_store::find_installed(app)
        .map_err(|error| format!("the package store cannot be read: {error}"))?
        .ok_or_else(|| format!("{app} is not installed"))?;
    launcher::run::tilolang_process::verified_instance(
        &installed,
        &installed.manifest_path,
        &installed.payload_root,
        &installed.package_id_hex,
        revoked,
    )
    .map_err(|error| error.to_string())
}

/// The payload of the installed tilolang app `app`, verified as a launch of it verifies it: its
/// files against the signed record and its publisher's signature, and the runtimes it names here — installed bundles, or the exact builds this device's host carries
/// in itself, as a phone does. What a host that runs the app in its own process reads its bundle
/// from (a phone's window starts no process), on the same verified installation a launch starts.
///
/// # Errors
/// The app is not installed, is no tilolang app, does not verify, or names a runtime this device
/// neither has nor carries.
pub fn verified_payload(app: &str) -> Result<std::path::PathBuf, String> {
    let installed = package_store::find_installed(app)
        .map_err(|error| format!("the package store cannot be read: {error}"))?
        .ok_or_else(|| format!("{app} is not installed here"))?;
    // A phone's own process holds no workspace's trust list to refuse a publisher by.
    crate::apps::verify_package(&installed, &tilo_app_package::RevocationList::default())?;
    let manifest = launcher::manifest::read_manifest(&installed.manifest_path)
        .map_err(|error| format!("{app}'s manifest: {error}"))?;
    let runtime = launcher::manifest::code_runtime(&manifest, &installed.payload_root)
        .ok_or_else(|| format!("{app} is no tilolang app"))?;
    for named in [Some(runtime), manifest.runtime.as_deref()]
        .into_iter()
        .flatten()
        .filter(|runtime| !runtime.is_empty())
    {
        if package_store::runtime_installed(named) || runtime_bundle::carried(named).is_some() {
            continue;
        }
        return Err(format!(
            "{app} names the runtime {named}, and this device {}",
            if runtime_bundle::carried_builds().is_empty() {
                "does not have it".to_owned()
            } else {
                format!("carries {}", runtime_bundle::carried_builds().join(", "))
            }
        ));
    }
    Ok(installed.payload_root)
}

/// `app`, installed here, verified as a launch verifies it, on runtimes installed here, its
/// publisher not in `revoked`, the keys the workspace it runs in refuses: on Linux as
/// `tilo-launcher` verifies a launch, on macOS and Windows as the daemon verifies an app it
/// launches (`crate::apps`). An app installed for runtimes since replaced runs nowhere until it
/// is built again.
pub(super) fn verified_installed(
    _workspace: &str,
    app: &str,
    revoked: &tilo_app_package::RevocationList,
) -> Result<Verified, String> {
    #[cfg(target_os = "linux")]
    return {
        let verified = verified_package(app, revoked)?;
        let carried = runtime_bundle::carried(&verified.runtime).is_some()
            && runtime_bundle::carried(&verified.app_runtime).is_some();
        if !verified.runs_here() && !carried {
            return Err(format!(
                "{app} names runtimes not installed here ({}, {})",
                verified.runtime, verified.app_runtime
            ));
        }
        Ok(Verified {
            runtime: verified.runtime,
            app_runtime: verified.app_runtime,
            module: verified.module,
        })
    };
    #[cfg(any(target_os = "macos", windows))]
    return package_store::find_installed(app)
        .map_err(|error| format!("the package store cannot be read: {error}"))?
        .ok_or_else(|| format!("{app} is not installed"))
        .and_then(|installed| crate::apps::verified_tilolang(&installed, revoked));
    #[cfg(target_os = "android")]
    return Err(format!(
        "{app} runs in the Tilo app's own process: Android starts no process for an app"
    ));
}
