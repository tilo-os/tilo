//! The Windows compatibility service as this session serves it: what only the session knows,
//! handed to the service `compat_service` assembles.
//!
//! - **Who may connect.** A `tilo-launcher` process this session started (its child, running the
//!   launcher's executable) for the Windows Apps manager in the workspace its handoff names, while
//!   that launch is still alive ([`Launches`]). Each launch hands off once. The service refuses any
//!   other app id, any process the session did not start, and a second connection of the same
//!   launch.
//! - **What a program may reach.** The asking workspace's sandbox, read from the layout the broker
//!   decides against at every request ([`LayoutAuthority`]).
//! - **Where it shows its windows.** The display the session's apps start on, once the compositor
//!   has bound it ([`Display`]): asked when a program starts, never when the service starts, so a
//!   program never gets a display that does not exist yet.
//!
//! The service works on threads of its own; nothing here blocks the session's loop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use compat_service::{
    Assembly, CapabilityAuthority, CompatSessionHandle, LaunchDisplay, LaunchDisplays, SessionSide,
    TrustedLauncherRegistry,
};
use tilo_broker_core::services::WorkspaceAuthority as _;
use tilo_lock::MutexRecover as _;

use crate::Launches;
use crate::app_wire::LayoutAuthority;

/// The display the session's apps start on, as the compositor bound it, and the GPU it draws
/// with: written on the loop as they become known, read by the service as a program starts.
#[derive(Default)]
pub struct Display {
    shown: Mutex<Shown>,
}

#[derive(Default)]
struct Shown {
    wayland_socket: Option<PathBuf>,
    render_node: Option<PathBuf>,
}

impl Display {
    /// The compositor bound its Wayland socket at `socket`.
    pub(crate) fn bound(&self, socket: PathBuf) {
        self.shown.lock_recover().wayland_socket = Some(socket);
    }

    /// The compositor draws on the GPU whose render node is `node`.
    pub(crate) fn drawn_on(&self, node: Option<PathBuf>) {
        self.shown.lock_recover().render_node = node;
    }
}

impl LaunchDisplays for Display {
    fn display_for(&self, _workspace_id: &str) -> Result<LaunchDisplay, String> {
        let (wayland_socket, render_node) = {
            let shown = self.shown.lock_recover();
            (shown.wayland_socket.clone(), shown.render_node.clone())
        };
        let wayland_socket = wayland_socket
            .ok_or_else(|| "this session has not bound its display yet".to_owned())?;
        let render_node = render_node.ok_or_else(|| {
            "this session draws without a GPU, which Windows programs need".to_owned()
        })?;
        Ok(LaunchDisplay {
            uid: rustix::process::geteuid().as_raw(),
            runtime_dir: tilo_paths::xdg::runtime_dir_or_default(),
            wayland_socket,
            render_node,
        })
    }
}

/// The launches this session started, as the service asks about a connecting process.
struct SessionLaunches(Arc<Launches>);

impl TrustedLauncherRegistry for SessionLaunches {
    fn is_launch(
        &self,
        pid: u32,
        process_start_time: u64,
        app_id: &str,
        workspace_id: &str,
    ) -> bool {
        self.0.started_at(pid, app_id, workspace_id) == Some(process_start_time)
    }
}

/// Each workspace's sandbox, from the layout the broker decides against.
struct WorkspaceSandboxes(Arc<LayoutAuthority>);

impl CapabilityAuthority for WorkspaceSandboxes {
    fn capabilities_for(
        &self,
        workspace_id: &str,
        _app_id: &str,
    ) -> Result<Vec<collab_types::caps::CapOverride>, String> {
        let mut sandbox = None;
        self.0.with_workspace(workspace_id, &mut |workspace| {
            // The whole list, restrictions included: the service filters it to the program's own
            // app id.
            sandbox = workspace.map(|workspace| workspace.sandbox.clone());
        })?;
        sandbox.ok_or_else(|| "the workspace is no longer in this session".to_owned())
    }
}

/// The compatibility service while this session serves it.
pub(crate) struct Served {
    /// Filled once the service is up: loading the catalogues and the capsules reads files, so it
    /// starts on a thread of its own rather than the session's loop.
    _service: Arc<Mutex<Option<CompatSessionHandle>>>,
}

/// What the service table serves Windows programs with: the session's launches, the launcher its
/// apps start through, the display they show on, and how the service is put together.
pub(crate) struct Serving {
    launches: Arc<Launches>,
    tilo_launcher: PathBuf,
    display: Arc<Display>,
    assembly: Assembly,
}

/// What a session started with `options` serves Windows programs with: nothing when it starts no
/// apps, as no manager can start then.
pub(crate) fn serving(options: &mut crate::Options) -> Option<Serving> {
    let apps = options.launch.as_ref()?;
    Some(Serving {
        launches: Arc::clone(&options.launches),
        tilo_launcher: apps.launcher()?,
        display: apps.windows_display(),
        assembly: std::mem::take(&mut options.windows),
    })
}

/// Serve the compatibility service at the launcher's socket for it, deciding against `layout`.
pub(crate) fn serve(serving: Serving, layout: Arc<LayoutAuthority>) -> Result<Served, String> {
    let Serving {
        launches,
        tilo_launcher,
        display,
        assembly,
    } = serving;
    let capsules = tilo_paths::xdg::data_home()
        .ok_or_else(|| "this login has no data directory".to_owned())?
        .join("tilo/windows-compat");
    let side = SessionSide {
        socket: launcher::services::socket_for(launcher::services::COMPAT_SERVICE),
        launcher: tilo_launcher,
        parent_pid: std::process::id(),
        launches: Arc::new(SessionLaunches(launches)),
        sandboxes: Arc::new(WorkspaceSandboxes(layout)),
        displays: display,
        capsules,
    };
    let service = Arc::new(Mutex::new(None));
    let started = Arc::clone(&service);
    std::thread::Builder::new()
        .name("tilo-windows-start".to_owned())
        .spawn(move || {
            let served = launcher::services::ensure_services_root()
                .map_err(|error| error.to_string())
                .and_then(|_| assembly.start(side));
            match served {
                Ok(served) => {
                    tracing::info!("serving Windows programs to the Windows Apps manager");
                    *started.lock_recover() = Some(served);
                }
                Err(error) => {
                    tracing::warn!(%error, "the Windows compatibility service is unavailable");
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(Served { _service: service })
}
