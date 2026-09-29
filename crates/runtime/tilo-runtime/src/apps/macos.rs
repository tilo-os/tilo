//! Launching through `launcher-macos`: App Sandbox for a trusted signed app,
//! a deny-default Seatbelt profile for everything else.
//!
//! The launcher's contract is that its caller has verified the payload
//! against the package's signed root first ([`verify_installed`]);
//! an unverifiable payload is refused before it reaches the launcher. The
//! app inherits its service connections already handshaken with its
//! identity, so a sandbox that denies it every socket path still
//! collaborates.

use super::{Apps, Launch, Sockets};
use crate::unix::{ServiceConnections, ServiceSockets};
use std::ffi::OsString;
use std::os::fd::AsRawFd as _;
use tilo_installed_apps::InstalledApp;

pub(super) fn launch(apps: &Apps, app: &InstalledApp, sockets: &Sockets) -> Result<Launch, String> {
    let catalog = apps
        .catalog
        .as_ref()
        .ok_or("refusing to launch: no app store is available")?;
    verify_installed(catalog, app)?;
    let manifest = launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|error| format!("refusing to launch {}: {error}", app.package_id))?;
    let executable = manifest
        .entrypoint
        .gui_bin()
        .filter(|bin| !bin.is_empty())
        .ok_or_else(|| format!("{} has no window to open", app.package_id))?;
    if apps.data_home.as_os_str().is_empty() {
        return Err("refusing to launch: HOME is not set, so the app has nowhere to write".into());
    }
    let container = launcher_macos::policy::prepare_container(
        &apps.data_home,
        Some(&apps.room),
        &app.package_id,
    )
    .map_err(|error| {
        format!(
            "refusing to launch {}: its container: {error}",
            app.package_id
        )
    })?;
    let services = ServiceConnections::connect(
        &app.package_id,
        &apps.room,
        &ServiceSockets {
            collab: &sockets.collab,
            identity: &sockets.identity,
            blobs: &sockets.blobs,
        },
    )
    .map_err(|error| {
        format!(
            "could not connect {} to its services: {error}",
            app.package_id
        )
    })?;
    let chooser = Chooser::start(&app.package_id)
        .map_err(|error| format!("could not give {} a file chooser: {error}", app.package_id))?;
    let mut inherit_fds = services.fds();
    inherit_fds.push(chooser.app_end.as_raw_fd());
    let mut env: Vec<(OsString, OsString)> = services
        .env(&app.package_id, &apps.room)
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    env.push((
        OsString::from(tilo_dialogs::confined::PICKER_FD_ENV),
        OsString::from(chooser.app_end.as_raw_fd().to_string()),
    ));
    let request = launcher_macos::LaunchRequest {
        package_id: app.package_id.clone(),
        executable: app.payload_root.join(executable),
        args: Vec::new(),
        payload_dir: app.payload_root.clone(),
        runtime_dylib_dir: None,
        container_dir: container,
        capabilities: launcher::capabilities::CapabilityPlan::empty(),
        egress: launcher::netgate::EgressDecision::resolve(
            &manifest.sandbox.net.mode,
            &manifest.sandbox.net.hosts,
            &[],
        ),
        inherit_fds,
        env,
        trust: launcher_macos::TrustRequirement::DeveloperId,
    };
    let launched = launcher_macos::launch(&request).map_err(|error| error.to_string())?;
    // The app holds its own descriptors now; this process's copies go.
    drop(services);
    drop(chooser.app_end);
    let pid = u32::try_from(launched.pid())
        .map_err(|_| format!("{} started with an invalid pid", app.package_id))?;
    Ok(Launch { pid, app: launched })
}

/// The file chooser a confined app asks for over its end of a socket pair:
/// this runtime shows every panel, outside the app's sandbox, and grants the
/// app exactly what the person picked (`tilo_dialogs::confined`).
struct Chooser {
    app_end: std::os::unix::net::UnixStream,
}

impl Chooser {
    /// Serve `package_id`'s chooser requests until the app closes its end.
    fn start(package_id: &str) -> std::io::Result<Self> {
        let (app_end, runtime_end) = std::os::unix::net::UnixStream::pair()?;
        let exe = std::env::current_exe()?;
        let package = package_id.to_owned();
        std::thread::Builder::new()
            .name("tilo-app-chooser".into())
            .spawn(move || {
                let shown = tilo_dialogs::confined::serve(runtime_end, |request| {
                    let mut helper = std::process::Command::new(&exe);
                    helper.arg(crate::cli::CHOOSER_HELPER_ARG);
                    tilo_dialogs::confined::show_in_helper(helper, request)
                });
                if let Err(error) = shown {
                    tracing::warn!(%package, %error, "the app's file chooser stopped");
                }
            })?;
        Ok(Self { app_end })
    }
}

/// Prove the installed package behind `app` is still the one that was
/// installed: its record's signature by its publisher, the manifest and
/// every payload file against the signed hashes, no file the package did
/// not ship, and a publisher key this profile has not revoked.
///
/// # Errors
///
/// Names the first check that failed; the launch is refused.
pub(super) fn verify_installed(
    catalog: &package_store::Catalog,
    app: &InstalledApp,
) -> Result<(), String> {
    use package_store::integrity;
    let refuse = |why: String| format!("refusing to launch {}: {why}", app.package_id);
    let package = catalog
        .list()
        .map_err(|error| refuse(error.to_string()))?
        .into_iter()
        .find(|package| package.manifest_path == app.manifest_path)
        .ok_or_else(|| refuse("it is no longer installed".into()))?;
    let report = integrity::verify(&package, &package.payload_root)
        .map_err(|error| refuse(error.to_string()))?;
    if let Some(fault) = report.faults.first() {
        return Err(refuse(format!(
            "its payload changed after install ({fault})"
        )));
    }
    if let Some(unexpected) = report.unexpected.first() {
        return Err(refuse(format!(
            "its payload holds a file the package did not ship ({unexpected})"
        )));
    }
    let record = integrity::read_record(
        &integrity::record_dir(&package),
        &package.id,
        &package.package_id_hex,
    )
    .map_err(|error| refuse(error.to_string()))?;
    let revoked = match tilo_app_package::default_revoked_keys_path() {
        Some(path) => tilo_app_package::RevocationList::load(&path)
            .map_err(|error| refuse(format!("the revocation list is unreadable ({error})")))?,
        None => tilo_app_package::RevocationList::empty(),
    };
    if let Some(entry) = revoked.find(&record.root.publisher_identity) {
        return Err(refuse(format!(
            "its publisher's key is revoked ({})",
            entry.reason
        )));
    }
    Ok(())
}
