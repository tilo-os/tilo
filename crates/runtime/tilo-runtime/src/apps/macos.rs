//! Launching through `launcher-macos`: App Sandbox for a trusted signed app,
//! a deny-default Seatbelt profile for everything else.
//!
//! The launcher's contract is that its caller has verified the payload
//! against the package's signed root first ([`super::verify_installed`]);
//! an unverifiable payload is refused before it reaches the launcher. The
//! app inherits its service connections already handshaken with its
//! identity, so a sandbox that denies it every socket path still
//! collaborates.

use super::{Apps, Launch, Sockets, verify_installed};
use crate::unix::{ServiceConnections, ServiceSockets};
use std::ffi::OsString;
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
        gate_socket: None,
        inherit_fds: services.fds(),
        env: services
            .env(&app.package_id, &apps.room)
            .into_iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect(),
        trust: launcher_macos::TrustRequirement::DeveloperId,
    };
    let launched = launcher_macos::launch(&request).map_err(|error| error.to_string())?;
    // The app holds its own descriptors now; this process's copies go.
    drop(services);
    let pid = u32::try_from(launched.pid())
        .map_err(|_| format!("{} started with an invalid pid", app.package_id))?;
    Ok(Launch { pid, app: launched })
}
