//! The connectors the system runs for tilolang apps whose types hold an outside service's
//! resources (`[entrypoint].origins` in the app's signed manifest: Mail's `imap`).
//!
//! A connector is the app's own launch with no window (`--tilolang-connector <origin>`): the
//! launcher gives it exactly the sandbox its manifest declares (the network and custody the
//! origin needs), which the tilolang host its instances run in never has. One runs per app,
//! origin and workspace, started the first time an instance needs it and left running when the
//! instance's window closes, so what the service says while no window is open waits in it
//! (`tilolang_mail::serve`). Each instance attaches over a connection of its own: one end in
//! the instance's descriptors, the other handed to the connector on its door
//! (`runtime_portal::origin_connector`).

use std::collections::HashMap;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Mutex;

use runtime_portal::origin_connector;

use super::{InstalledApp, LaunchExtras, RoomId};

/// Which connector: the app, its origin and the workspace.
type Key = (String, String, String);

/// A running connector: its door, and which start it is.
struct Running {
    door: OwnedFd,
    generation: u64,
}

static CONNECTORS: Mutex<Option<HashMap<Key, Running>>> = Mutex::new(None);

/// The connection an instance of `app` in `room` reaches its origin's connector over, starting
/// the connector when none runs; `None` for an app whose manifest declares no origin.
///
/// # Errors
/// The manifest cannot be read, or the connector cannot be started or reached.
pub(super) fn connection(app: &InstalledApp, room: RoomId) -> std::io::Result<Option<OwnedFd>> {
    let manifest = ::launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|error| std::io::Error::other(format!("reading the manifest: {error}")))?;
    let Some(origin) = manifest.entrypoint.origins.first() else {
        return Ok(None);
    };
    if manifest.entrypoint.origins.len() > 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "{} declares {} origins; an instance reaches one connector",
                app.package_id,
                manifest.entrypoint.origins.len()
            ),
        ));
    }
    attach(app, room, origin).map(Some)
}

fn attach(app: &InstalledApp, room: RoomId, origin: &str) -> std::io::Result<OwnedFd> {
    let (instance, theirs) = origin_connector::connection()?;
    let key = (app.package_id.clone(), origin.to_owned(), room.to_string());
    let mut connectors = super::lock(&CONNECTORS);
    let connectors = connectors.get_or_insert_with(HashMap::new);
    if let Some(running) = connectors.get(&key) {
        match origin_connector::send_attachment(&running.door, &theirs) {
            Ok(()) => return Ok(instance),
            // It ended: start another.
            Err(error) => {
                tracing::info!(%error, package_id = %app.package_id, origin, "an origin connector's door is closed");
                connectors.remove(&key);
            }
        }
    }
    let running = start(app, room, origin, key.clone())?;
    origin_connector::send_attachment(&running.door, &theirs)?;
    connectors.insert(key, running);
    Ok(instance)
}

/// Start the connector of `app` for `origin` in `room` and follow it on a thread of its own
/// until it ends.
fn start(app: &InstalledApp, room: RoomId, origin: &str, key: Key) -> std::io::Result<Running> {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let launcher =
        super::resolve_launcher_bin().map_err(|error| std::io::Error::other(error.to_string()))?;
    let identity_path = crate::identity_registry::selected_identity_path(&room.to_string())
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    // What the person granted the app: the connector runs with it and asks for nothing, having
    // no window to ask in.
    let mut grants = super::capabilities_for_app(&app.package_id);
    let capabilities = super::stage_launch_capabilities(&app.package_id, &mut grants);
    let mut command = super::build_launch_command_for(
        app,
        &launcher,
        None,
        room,
        &LaunchExtras {
            capabilities_file: capabilities.as_ref().map(tempfile::NamedTempFile::path),
            identity_path: identity_path.as_deref(),
            render_node: None,
            document: None,
            uri: None,
        },
    );
    command
        .args
        .extend(["--tilolang-connector".to_owned(), origin.to_owned()]);
    let (ours, theirs) = origin_connector::pair()?;
    let mut process = std::process::Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(std::process::Stdio::null());
    super::apply_launch_env(&mut process, &command.env);
    // The connector's door is released to the launcher as a window's is, and the launcher
    // hands it into the sandbox as the connector's.
    let gate = ::launcher::registration_gate::install(&mut process)?;
    let mut child = process.spawn()?;
    drop(process);
    gate.release_with_window_door(theirs.as_fd())?;
    drop(theirs);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let package_id = app.package_id.clone();
    let origin = origin.to_owned();
    std::thread::Builder::new()
        .name("origin-connector".into())
        .spawn(move || {
            // The staged capabilities live as long as the connector does.
            let _capabilities = capabilities;
            match child.wait() {
                Ok(status) if status.success() => {
                    tracing::info!(package_id, origin, "an origin connector ended");
                }
                Ok(status) => {
                    tracing::warn!(package_id, origin, %status, "an origin connector failed");
                }
                Err(error) => {
                    tracing::warn!(package_id, origin, %error, "an origin connector could not be followed");
                }
            }
            if let Some(connectors) = super::lock(&CONNECTORS).as_mut()
                && connectors
                    .get(&key)
                    .is_some_and(|running| running.generation == generation)
            {
                connectors.remove(&key);
            }
        })?;
    tracing::info!(package_id = %app.package_id, room = %room, "an origin connector started");
    Ok(Running {
        door: ours,
        generation,
    })
}
