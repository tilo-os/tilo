//! Spare window processes: opening a tilolang app's window starts no process.
//!
//! The session keeps one spare window process per workspace, started ahead of the window it will
//! show: a sandboxed tilolang window of no app, with no package, no app data and no app identity,
//! prepared up to its window and waiting on its link (`tilolang_process::window::spare`). The
//! session makes its Wayland connection and hands it over.
//!
//! When a tilolang app's window is asked for in a workspace where no process of the app serves
//! it, the session binds the spare there to the app: it records which app and workspace the
//! spare's connection belongs to, hands the window's instance to the app's tilolang process, and
//! sends the spare the app's name and surface and the window's connections. The spare opens the
//! window; from then on it serves that app in that workspace, as a process launched for it would
//! (`tilo_app_launch::links`), and never another app. A new spare starts on the next pass, off the
//! open's path. With no spare ready, the open starts one and binds it, the same way.
//!
//! What the shell shows of a spare's windows is what its connection was bound to: never what the
//! process says of itself, and never its process id, which the system may give another process.

use std::os::fd::OwnedFd;
use std::rc::Rc;

use runtime_portal::process_link::{Packet as _, ToProcess};
use tilo_app_launch::{HeldLink, Links, Opens, Planned, Reservation, Routed, StartAlone};

use super::{ApiError, Apps, Got, Launching};
use tilo_app_launch::Workspace;

/// The tilolang runtime and the Tilo runtime a window process runs on, `tilolang/<version>` and
/// `tilo-runtime/<version>`: a spare shows only apps built for the ones it runs.
type Runtimes = (String, String);

/// The spares of a session, and what it keeps them by.
#[derive(Default)]
pub(super) struct Spares {
    /// What makes each spare's Wayland connection, once the compositor runs.
    connector: Option<tilo_compositor::ClientConnector>,
    /// The spares started and not bound yet.
    ready: Vec<SpareWindow>,
    /// The runtimes the next spare of each workspace runs on: those of the app its last spare was
    /// bound to.
    wanted: std::collections::BTreeMap<String, Runtimes>,
    /// The runtimes of the tilolang apps installed here, read once: what a workspace's first spare
    /// runs on, or none when no tilolang app is installed.
    installed: std::cell::OnceCell<Option<Runtimes>>,
    /// What each tilolang app's windows were bound with, by the app.
    packages: std::collections::BTreeMap<String, Rc<WindowPackage>>,
    /// How many spares in a row ended in each workspace before they were bound, or could not
    /// start. Each is replaced on the pass that hears it; after [`ENDED_IN_A_ROW`] none starts
    /// there again until a window is asked for, so a spare that cannot run does not start on
    /// every pass.
    ended: std::collections::BTreeMap<String, u32>,
}

/// How many spares in a row may end unbound in a workspace before none is started there again
/// until a window is asked for.
const ENDED_IN_A_ROW: u32 = 3;

/// How long a spare runs before its end counts as a new one rather than one more in a row: the
/// system may stop a spare that waited long, which says nothing about the next one.
const LIVED: std::time::Duration = std::time::Duration::from_secs(60);

/// A spare window process, started and waiting to be bound.
struct SpareWindow {
    workspace: String,
    runtimes: Runtimes,
    /// Its launch, by the spawner's id.
    launch: u64,
    /// When it started.
    started: std::time::Instant,
    pid: u32,
    /// The session's end of its link.
    link: OwnedFd,
    /// Its Wayland connection, which the session binds to the app with it.
    connection: tilo_compositor::SpareConnection,
}

impl SpareWindow {
    /// Whether this spare can be bound to an app of `workspace` that runs on `runtimes`: it is
    /// that workspace's, on those runtimes, and its process still holds its link.
    fn serves(&self, workspace: &str, runtimes: &Runtimes) -> bool {
        self.workspace == workspace
            && self.runtimes == *runtimes
            && !runtime_portal::process_link::hung_up(&self.link)
    }
}

/// What the session reads of an installed tilolang app to bind a spare to it, kept for as long as
/// the same package stays installed: a reopen of the app reads nothing again.
struct WindowPackage {
    /// The signed package it was read from.
    package_id: String,
    runtimes: Runtimes,
    /// Its compiled module, as its signed package ships it, in a sealed memfd: its windows read
    /// its name, icon and translations from it.
    module: OwnedFd,
    /// Its `surface.toml`, as its signed package ships it, in a sealed memfd, and the file's hash.
    surface: Option<(OwnedFd, String)>,
}

impl WindowPackage {
    /// Read `installed`, the package of the app `app`, through its signed record.
    fn read(app: &str, installed: &package_store::InstalledPackage) -> Result<Self, String> {
        let manifest = launcher::manifest::read_manifest(&installed.manifest_path)
            .map_err(|error| format!("{app}'s manifest: {error}"))?;
        let runtimes = runtimes_of(&manifest).ok_or_else(|| format!("{app} is no tilolang app"))?;
        let mut files = package_store::integrity::signed_files(
            installed,
            &[
                tilolang_tilo::bundle::WASM_FILE,
                package_store::integrity::SURFACE_FILE,
            ],
        )
        .map_err(|error| format!("{app}'s package: {error}"))?
        .into_iter();
        let (module, surface) = (files.next().flatten(), files.next().flatten());
        let module =
            module.ok_or_else(|| format!("{app}'s module is not the one its package signed"))?;
        let sealed = |name: &str, bytes: &[u8]| {
            tilo_render_protocol::shm::sealed_blob(name, bytes)
                .map_err(|error| format!("{app}'s {name}: {error}"))
        };
        let surface = match surface {
            Some(bytes) => Some((
                sealed("surface", &bytes)?,
                blake3::hash(&bytes).to_hex().to_string(),
            )),
            None => None,
        };
        Ok(Self {
            package_id: installed.package_id_hex.clone(),
            runtimes,
            module: sealed("module", &module)?,
            surface,
        })
    }

    /// The descriptors one binding hands a spare: copies of the sealed files, which a spare reads
    /// at their offsets, never through a shared position.
    fn handed(&self) -> Result<(OwnedFd, Option<(OwnedFd, String)>), String> {
        let copy = |fd: &OwnedFd| fd.try_clone().map_err(|error| error.to_string());
        let surface = match &self.surface {
            Some((fd, hash)) => Some((copy(fd)?, hash.clone())),
            None => None,
        };
        Ok((copy(&self.module)?, surface))
    }
}

/// The runtimes `manifest` names, when it is a tilolang app's.
fn runtimes_of(manifest: &launcher::manifest::ManifestView) -> Option<Runtimes> {
    let runtime = manifest
        .entrypoint
        .runtime
        .clone()
        .filter(|runtime| launcher::manifest::is_tilolang(runtime))?;
    Some((runtime, manifest.runtime.clone()?))
}

/// The runtimes of a tilolang app installed here, the first the catalogue lists: what a
/// workspace's first spare runs on.
fn installed_runtimes() -> Option<Runtimes> {
    let apps = app_catalogue::apps::read_from_env()
        .map_err(
            |error| tracing::warn!(%error, "no spare window: the installed apps are not known"),
        )
        .ok()?;
    apps.iter().find_map(|app| {
        launcher::manifest::read_manifest(&app.manifest_path)
            .ok()
            .as_ref()
            .and_then(runtimes_of)
    })
}

impl Apps {
    /// Make spare windows' Wayland connections through `connector` from now on: the compositor
    /// runs.
    pub fn lend_spare_connector(&self, connector: tilo_compositor::ClientConnector) {
        self.state.borrow_mut().spares.connector = Some(connector);
    }

    /// Keep a spare window ready in the workspace in front, and in every workspace a spare was
    /// bound in: called once per pass, so the spare a binding took is replaced off the open's
    /// path.
    pub(super) fn keep_spares(&self) {
        if self.state.borrow().spares.connector.is_none()
            || self.state.borrow().launcher.is_none()
            || self.state.borrow().links.is_none()
        {
            return;
        }
        let front = self.active_workspace().id;
        let mut wanted: Vec<(String, Option<Runtimes>)> = {
            let state = self.state.borrow();
            state
                .spares
                .wanted
                .iter()
                .map(|(workspace, runtimes)| (workspace.clone(), Some(runtimes.clone())))
                .collect()
        };
        if !wanted.iter().any(|(workspace, _)| *workspace == front) {
            wanted.push((front, None));
        }
        for (workspace, runtimes) in wanted {
            let missing = {
                let state = self.state.borrow();
                state.spares.ended.get(&workspace).copied().unwrap_or(0) < ENDED_IN_A_ROW
                    && !state
                        .spares
                        .ready
                        .iter()
                        .any(|spare| spare.workspace == workspace)
            };
            if !missing {
                continue;
            }
            let Some(runtimes) = runtimes.or_else(|| self.installed_runtimes()) else {
                continue;
            };
            match self.start_spare(&workspace, runtimes) {
                Ok(spare) => self.state.borrow_mut().spares.ready.push(spare),
                Err(error) => {
                    tracing::warn!(%workspace, %error, "no spare window");
                    let mut state = self.state.borrow_mut();
                    let in_a_row = state.spares.ended.entry(workspace).or_default();
                    *in_a_row = in_a_row.saturating_add(1);
                }
            }
        }
    }

    fn installed_runtimes(&self) -> Option<Runtimes> {
        self.state
            .borrow()
            .spares
            .installed
            .get_or_init(installed_runtimes)
            .clone()
    }

    /// Start a spare window in `workspace` on `runtimes`, its Wayland connection made and sent.
    fn start_spare(&self, workspace: &str, runtimes: Runtimes) -> Result<SpareWindow, String> {
        let connector = self
            .state
            .borrow()
            .spares
            .connector
            .clone()
            .ok_or("this session makes no display connection for a window")?;
        let (wayland, connection) = connector
            .connect_spare()
            .map_err(|error| format!("its display connection: {error}"))?;
        let (link, theirs) =
            runtime_portal::process_link::pair().map_err(|error| format!("its link: {error}"))?;
        // Waits on the link until the process reads it.
        ToProcess::Display {
            wayland: OwnedFd::from(wayland),
        }
        .send(&link)
        .map_err(|error| format!("its display: {error}"))?;
        let planned = self
            .with_plan_context(
                None,
                &self.workspace_named(workspace),
                false,
                None,
                |context| Planned::spare_window(workspace, (&runtimes.0, &runtimes.1), context),
            )
            .map_err(|error| error.to_string())?;
        let (started, _) = self
            .state
            .borrow_mut()
            .spawner
            .start_with(
                &planned,
                Some(HeldLink::unrouted(theirs)),
                &|_| {},
                workspace,
                None,
            )
            .map_err(|error| error.to_string())?;
        connection.started(started.pid);
        tracing::info!(%workspace, pid = started.pid, "a spare window started");
        Ok(SpareWindow {
            workspace: workspace.to_owned(),
            runtimes,
            launch: started.id,
            started: std::time::Instant::now(),
            pid: started.pid,
            link,
            connection,
        })
    }

    /// The launch `id` that ended was a spare's that was never bound: it is let go, and the pass
    /// that heard it starts another in its workspace, unless too many ended there in a row.
    /// Whether it was one.
    pub(super) fn spare_ended(&self, id: u64) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(at) = state
            .spares
            .ready
            .iter()
            .position(|spare| spare.launch == id)
        else {
            return false;
        };
        let spare = state.spares.ready.remove(at);
        tracing::warn!(workspace = %spare.workspace, pid = spare.pid, "a spare window ended unbound");
        let in_a_row = state.spares.ended.entry(spare.workspace).or_default();
        if spare.started.elapsed() >= LIVED {
            *in_a_row = 0;
        }
        *in_a_row = in_a_row.saturating_add(1);
        true
    }

    /// Open the window `instance` of the tilolang app `asked` names in `workspace`, showing what
    /// `opens` names: in the process bound to the app there when one serves it, else in a spare
    /// bound to it now. `timed` is the open's launch timing.
    pub(super) fn open_tilolang_window(
        &self,
        asked: &tilo_app_launch::Launch,
        workspace: &Workspace,
        instance: u64,
        opens: &Opens,
        timed: u64,
        links: &Rc<Links>,
    ) -> Result<bool, ApiError> {
        let app = asked.app();
        // The window asks for its instance by its tag: that is the window this open is timed to.
        tilo_startup_metrics::launch_shows(timed, instance);
        let failed = |error: String| {
            tilo_startup_metrics::launch_failed(timed);
            ApiError::Failed(error)
        };
        // A process bound to an app says it serves its windows as it is bound: no window of it
        // ever waits to hear whether it does.
        let unasked = || -> StartAlone {
            Box::new(|| tracing::error!("a tilolang window waited on a process that never said"))
        };
        let reservation =
            match links.route_or_reserve(app, &workspace.id, true, opens, &unasked, Some(instance))
            {
                Err(Routed::Opened) => {
                    tilo_startup_metrics::launch_failed(timed);
                    tracing::info!(app, "the app's window process opens the window");
                    return Ok(true);
                }
                Ok(Some(reservation)) => reservation,
                Err(Routed::NotRunning) | Ok(None) => {
                    return Err(failed(format!("{app} has no window link here")));
                }
            };
        tilo_startup_metrics::launch_phase(app, "routed");
        self.bind_spare(asked, workspace, instance, opens, reservation, timed)
            .map_err(failed)
    }

    /// What `app`'s windows are bound with: read once for the package installed now.
    fn window_package(&self, app: &str) -> Result<Rc<WindowPackage>, String> {
        let installed = package_store::find_installed(app)
            .map_err(|error| format!("the installed apps are not known: {error}"))?
            .ok_or_else(|| format!("{app} is not installed"))?;
        if let Some(package) = self.state.borrow().spares.packages.get(app)
            && package.package_id == installed.package_id_hex
        {
            return Ok(Rc::clone(package));
        }
        let package = Rc::new(WindowPackage::read(app, &installed)?);
        self.state
            .borrow_mut()
            .spares
            .packages
            .insert(app.to_owned(), Rc::clone(&package));
        Ok(package)
    }

    /// Bind a spare in `workspace` to `asked`'s app for the window `instance`: the one ready
    /// there, or one started now. `timed` is the open's launch timing, which the spare's first
    /// frame ends.
    fn bind_spare(
        &self,
        asked: &tilo_app_launch::Launch,
        workspace: &Workspace,
        instance: u64,
        opens: &Opens,
        reservation: Reservation,
        timed: u64,
    ) -> Result<bool, String> {
        let app = asked.app();
        let package = self.window_package(app)?;
        let ready = {
            let mut state = self.state.borrow_mut();
            state.spares.ended.remove(&workspace.id);
            let at = state
                .spares
                .ready
                .iter()
                .position(|spare| spare.serves(&workspace.id, &package.runtimes));
            at.map(|at| state.spares.ready.remove(at))
        };
        let spare = match ready {
            Some(spare) => spare,
            None => self.start_spare(&workspace.id, package.runtimes.clone())?,
        };
        // The compositor names the spare's windows by what its connection is bound to: it is
        // bound before the spare is told, so no window of it is ever shown as anything else.
        if !spare.connection.bind(app, &workspace.id) {
            return Err("the spare window was bound already".to_owned());
        }
        // Written down before the spare reaches a service as the app's window.
        let launches = self.state.borrow().launches.clone();
        if let Some(launches) = &launches {
            launches.started(spare.pid, app, &workspace.id, true);
        }
        let (module, surface) = package.handed()?;
        let held = reservation
            .bind_spare(
                tilo_app_launch::Spare {
                    link: spare.link,
                    launcher_pid: spare.pid,
                },
                instance,
                opens,
                module,
                surface,
            )
            .map_err(|error| format!("the spare window was not bound: {error}"))?;
        tilo_startup_metrics::launch_spawned(timed, spare.pid);
        let mut state = self.state.borrow_mut();
        state.held.insert(spare.launch, held);
        let asked = asked.id();
        // This open replaces the last one of the app in the workspace, once that one ended: what a
        // caller reads is how the app's latest open went.
        state.launched.retain(|_, launch| {
            !(launch.app == app
                && launch.workspace == workspace.id
                && matches!(launch.got, Got::Closed | Got::CouldNotOpen { .. }))
        });
        state.launched.insert(
            spare.launch,
            Launching {
                asked,
                app: app.to_owned(),
                workspace: workspace.id.clone(),
                pid: Some(spare.pid),
                instance: Some(instance),
                window: None,
                tab: None,
                started: std::time::Instant::now(),
                got: Got::Arriving,
            },
        );
        // The workspace's next spare runs on this app's runtimes, started on the next pass.
        state
            .spares
            .wanted
            .insert(workspace.id.clone(), package.runtimes.clone());
        drop(state);
        tracing::info!(
            app,
            pid = spare.pid,
            instance,
            "a spare window was bound to the app"
        );
        Ok(true)
    }
}
