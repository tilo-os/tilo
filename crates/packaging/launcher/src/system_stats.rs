//! The `system-info` and `processes` capabilities: what an app is granted of
//! the session's system information service (`tilo_system_stats`).
//!
//! An app sandbox has a process namespace of its own, so `/proc` inside it
//! shows the app and nothing else, and binding the host's would hand every
//! program's command line and environment to it. An app that declares
//! `system-info` instead receives a connection to the session's service,
//! opened here and authenticated before the sandbox starts: the launcher
//! writes the app's id, its room and its grants as the first frame, so the
//! app cannot claim more than it was given.
//!
//! What it is given:
//!
//! - an app the image ships (its payload is in the image's read-only store)
//!   is granted what it declares, `system-info` and `processes`, by default;
//! - any other app is granted what the room's capabilities name
//!   (`Scope::Device("system-info")`, `"processes"`, `"process-commands"`),
//!   and asks the person through the permission prompt for the rest;
//! - a command line beside the process list is never a default: it is its
//!   own grant (`process-commands`).
//!
//! The manifest is the ceiling of all three: what an app does not declare
//! it is neither given nor asked about. `processes` alone keeps its other
//! meaning (starting programs of its own) and opens no connection.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use tilo_system_stats::protocol::{Grant, Grants, Handoff, PROTOCOL, write_handoff};

use crate::capabilities::CapabilityPlan;
use crate::manifest::ManifestView;
use crate::services::{ResolvedService, ServiceError, socket_for};

/// The manifest capability that opens the connection.
pub const SYSTEM_INFO: &str = "system-info";
/// The manifest capability that allows the process list on it.
pub const PROCESSES: &str = "processes";

#[must_use]
pub fn ceiling(manifest: &ManifestView) -> Grants {
    let system_info = manifest.declares_capability(SYSTEM_INFO);
    let processes = system_info && manifest.declares_capability(PROCESSES);
    Grants {
        system_info,
        processes,
        command_lines: processes,
    }
}

/// What a launch of `manifest` is granted, given the room's capabilities
/// (`plan`) and whether the image ships it.
#[must_use]
pub fn grants(manifest: &ManifestView, plan: &CapabilityPlan, image_shipped: bool) -> Grants {
    let mut grants = if image_shipped {
        Grants::IMAGE_DEFAULT
    } else {
        Grants::default()
    };
    for device in &plan.devices {
        if let Some(grant) = Grant::from_device_scope(device) {
            grants = grants.with(grant);
        }
    }
    grants.within(ceiling(manifest))
}

/// Whether `payload_root` lies in the image's own store, which only an image
/// update writes.
#[must_use]
pub fn image_shipped(payload_root: &Path) -> bool {
    let Ok(payload) = payload_root.canonicalize() else {
        return false;
    };
    let roots = [
        Some(package_store::layout::system_payload_dir()),
        package_store::layout::system_data_dir(),
    ];
    roots
        .into_iter()
        .flatten()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| payload.starts_with(root))
}

/// In-sandbox descriptor slot of the system information connection.
pub const CHILD_FD: i32 = 33;

/// Open and authenticate the app's connection, when it declares
/// `system-info` and the session serves it. A session without the service
/// costs the app its readings, never its launch.
///
/// # Errors
/// The handoff cannot be written on a connection that was opened.
pub fn resolve_bind(
    manifest: &ManifestView,
    plan: &CapabilityPlan,
    payload_root: &Path,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    let ceiling = ceiling(manifest);
    if !ceiling.system_info || no_services {
        return Ok(None);
    }
    let handoff = Handoff {
        protocol: PROTOCOL,
        app_id: manifest.package_id.clone(),
        workspace_id: workspace_id.map(str::to_owned),
        grants: grants(manifest, plan, image_shipped(payload_root)),
        ceiling,
    };
    resolve_bind_at(&socket_for(tilo_system_stats::SERVICE), &handoff)
}

fn resolve_bind_at(
    path: &Path,
    handoff: &Handoff,
) -> Result<Option<ResolvedService>, ServiceError> {
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "system information service not reachable");
            return Ok(None);
        }
    };
    write_handoff(&mut stream, handoff).map_err(|source| ServiceError::OpenFailed {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(ResolvedService {
        name: tilo_system_stats::SERVICE.to_owned(),
        host: OwnedFd::from(stream),
        child_fd: CHILD_FD,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn manifest(capabilities: &[&str]) -> ManifestView {
        let mut manifest = crate::manifest::test_manifest(
            "id = \"com.example.tool\"\nversion = \"1.0.0\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/tool\"\n",
        );
        manifest.capabilities = capabilities.iter().map(|c| (*c).to_owned()).collect();
        manifest
    }

    fn plan(devices: &[&str]) -> CapabilityPlan {
        CapabilityPlan {
            devices: devices.iter().map(|d| (*d).to_owned()).collect(),
            ..CapabilityPlan::empty()
        }
    }

    #[test]
    fn the_image_grants_its_own_apps_and_others_get_what_the_room_names() {
        let monitor = manifest(&["system-info", "processes"]);
        assert_eq!(
            grants(&monitor, &plan(&[]), true),
            Grants::IMAGE_DEFAULT,
            "an app the image ships: what it declares, never command lines"
        );
        assert_eq!(
            grants(&monitor, &plan(&[]), false),
            Grants::default(),
            "a user-installed app starts with nothing"
        );
        assert_eq!(
            grants(&monitor, &plan(&["system-info"]), false),
            Grants {
                system_info: true,
                ..Grants::default()
            },
            "what the room granted"
        );
        assert_eq!(
            grants(&monitor, &plan(&["process-commands"]), true),
            Grants::ALL,
            "command lines only by their own grant"
        );

        let readings_only = manifest(&["system-info"]);
        assert_eq!(
            grants(
                &readings_only,
                &plan(&["processes", "process-commands"]),
                true
            ),
            Grants {
                system_info: true,
                ..Grants::default()
            },
            "the manifest is the ceiling"
        );
        let spawner = manifest(&["processes"]);
        assert_eq!(
            ceiling(&spawner),
            Grants::default(),
            "starting programs opens nothing"
        );
        assert!(
            resolve_bind(&spawner, &plan(&[]), Path::new("/"), None, false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_connection_carries_the_launchers_handoff_before_the_app_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system-stats.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let handoff = Handoff {
            protocol: PROTOCOL,
            app_id: "so.tilo.monitor".into(),
            workspace_id: Some("room".into()),
            grants: Grants::IMAGE_DEFAULT,
            ceiling: Grants::ALL,
        };
        let resolved = resolve_bind_at(&path, &handoff).unwrap().expect("bound");
        assert_eq!(resolved.child_fd, CHILD_FD);
        assert!(crate::services::RESERVED_CHILD_FDS.contains(&CHILD_FD));
        let (mut service, _) = listener.accept().unwrap();
        assert_eq!(
            tilo_system_stats::protocol::read_handoff(&mut service).unwrap(),
            handoff
        );
        drop(resolved);
        let mut rest = Vec::new();
        service.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "nothing but the handoff");

        assert!(
            resolve_bind_at(&dir.path().join("absent.sock"), &handoff)
                .unwrap()
                .is_none(),
            "no service costs the readings, not the launch"
        );
    }
}
