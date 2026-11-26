//! Installed applications for a runtime profile: what can be launched, what
//! each declared it may do on the action API, and launching one into the
//! profile's workspace.
//!
//! A launch always goes through the platform's launcher, which confines the
//! app: `tilo-launcher` (bubblewrap, Landlock, seccomp) on Linux, App
//! Sandbox or Seatbelt through `launcher-macos` on macOS. Without one the
//! launch is refused; nothing here starts an app unconfined.
//!
//! Only a process this runtime launched, for exactly that app and workspace and
//! still the same process, is attested to the broker, and the endpoints the
//! runtime serves from its own process: the Files app of each workspace whose
//! files it holds, with no window, as the desktop session attests its files
//! daemon.

use crate::daemon::Sockets;

use app_catalogue::apps::InstalledApp;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::{
    app_actions::{AppActionAccess, AppActionAccessResolver, AppResourceKindsResolver},
    services::{
        AuthenticatedApplicationLaunch, LaunchContent, LaunchPlace, LaunchableApplication,
        Launched as LaunchOutcome, LauncherServices,
    },
};

#[cfg(any(target_os = "macos", windows))]
mod carried;
mod connection;
pub(crate) mod connectors;
mod instances;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod verified;
pub(crate) use verified::verify_package;
#[cfg(windows)]
mod windows;
#[cfg(any(target_os = "macos", windows))]
pub(crate) use verified::{installed_runtime, verified_tilolang};
#[cfg(target_os = "linux")]
mod original;
/// A view the daemon opened for a caller: the descriptors its page window is started with
/// (`Processes::show_page`), and the device's end of that window's page link.
pub(crate) struct OpenedView {
    /// The window's end of its instance's patch connection.
    pub patch: runtime_portal::process_link::Descriptor,
    /// The window's end of the page link.
    pub page: runtime_portal::process_link::Descriptor,
    /// The device's end of the page link, which asks the window for the page.
    pub link: tilo_socket::Stream,
}

/// Whether a launch of an app shows a window, or runs the app's code with none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shows {
    Window,
    /// No window, for a call: it ends once it takes no call for a while.
    Nothing,
    /// No window, for the app's long-lived connection: it runs until the device stops it.
    Connection,
}

/// A start that has already happened: the answer a call gets when the process that takes it
/// runs already.
fn ready_when_started() -> tilo_workspace_service::apps::services::WindowlessStart {
    let (started, start) = tokio::sync::oneshot::channel();
    let _ = started.send(Ok(()));
    start
}

/// The profile's installed applications and the launches it made.
pub struct Apps {
    /// Whether a start waits for its app's first connection, as a device's daemon does, where
    /// the broker attests the launcher's connection. Where none is expected (a test's launch),
    /// the start only refuses a process that ended at once.
    connection_wait: bool,
    /// The stores the profile reads; `None` when no app store can be
    /// located, which is a catalogue with nothing in it.
    catalog: Option<package_store::Catalog>,
    root: PathBuf,
    /// The daemon's sockets, which a launch attaches the app to.
    sockets: Sockets,
    /// Where app containers live on macOS, beside the runtime profile.
    #[cfg(target_os = "macos")]
    data_home: PathBuf,
    /// Where `tilo-launcher` is looked for, in order.
    #[cfg(target_os = "linux")]
    launcher_candidates: Vec<PathBuf>,
    launched: Arc<Launched>,
    /// The apps this runtime's own process is the windowless endpoint of,
    /// by app and workspace.
    own: Mutex<std::collections::BTreeSet<(String, String)>>,
    /// The launches whose collaboration connection this process is opening itself, by app and
    /// workspace: each attests the one connection from this process that names it, once.
    launching: Mutex<Vec<(String, String)>>,
    /// The person's answers about this device's camera and microphone, kept in their own store
    /// once the runtime opened it.
    device_grants: tilo_peer_state::DeviceGrants,
    /// What a launch into this profile's workspace is granted, once the
    /// workspace service that holds the person's grants is open.
    grants: OnceLock<Arc<dyn LaunchGrantsSource>>,
    /// The tilolang instances this daemon starts itself, with no session.
    instances: instances::Instances,
    /// The origin connectors this device runs for its tilolang apps' instances.
    connectors: connectors::Connectors,
}
impl Apps {
    /// Hand each origin connector this device starts the connection to its dispatch through
    /// `actions` from now on.
    pub(crate) fn hand_actions(&self, actions: Box<connectors::Actions>) {
        self.connectors.hand_actions(actions);
    }
}

/// Where a launch reads what the person granted the app it starts: the
/// workspace's own sandbox, as its governance keeps it.
pub(crate) trait LaunchGrantsSource: Send + Sync {
    /// What a launch into `workspace` hands its apps
    /// ([`tilo_desktop_model::Workspace::launch_grants`]).
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            dead_code,
            reason = "only an app sandbox reads it, and this platform has none: every launch is refused"
        )
    )]
    fn launch_grants(&self, workspace: &str) -> collab_types::caps::LaunchGrants;

    /// Whether this device holds `workspace`, so an app may be launched into it here.
    fn serves(&self, workspace: &str) -> bool;
}

impl Apps {
    /// The apps of the profile at `profile`, with the daemon's `sockets`, from the
    /// stores the environment names.
    #[must_use]
    pub(crate) fn from_env(profile: &Path, sockets: &Sockets) -> Self {
        let home = tilo_paths::home();
        let mut apps = Self::new(
            package_store::Catalog::from_env().ok(),
            profile,
            sockets.clone(),
            home.as_deref().map_or_else(PathBuf::new, data_home),
            launcher_candidates(),
        );
        apps.connection_wait = true;
        #[cfg(any(target_os = "macos", windows))]
        if let Some(catalog) = apps.catalog.as_ref() {
            carried::install_carried_runtimes(catalog);
        }
        apps.register_installed_surfaces();
        apps
    }

    /// Explicit stores and places, for tests.
    #[must_use]
    pub(crate) fn new(
        catalog: Option<package_store::Catalog>,
        profile: &Path,
        sockets: Sockets,
        data_home: PathBuf,
        launcher_candidates: Vec<PathBuf>,
    ) -> Self {
        // Each place only matters on the platform whose launch path reads it.
        #[cfg(not(target_os = "macos"))]
        drop(data_home);
        #[cfg(not(target_os = "linux"))]
        drop(launcher_candidates);
        // What starts a tilolang host: `tilo-launcher` on Linux, the installed runtime on macOS
        // and Windows.
        #[cfg(target_os = "linux")]
        let finds_launcher: instances::FindsLauncher = {
            let candidates = launcher_candidates.clone();
            Box::new(move || linux::resolve_launcher(&candidates))
        };
        #[cfg(target_os = "android")]
        let finds_launcher: instances::FindsLauncher = Box::new(|| Ok(()));
        #[cfg(any(target_os = "macos", windows))]
        let finds_launcher: instances::FindsLauncher = {
            let catalog = catalog.clone();
            Box::new(move || {
                catalog
                    .clone()
                    .ok_or_else(|| "no app store is available".to_owned())
            })
        };
        // The records every process this runtime starts is written to: what admits a
        // connection from it as the app it runs.
        let launched: Arc<Launched> = Arc::default();
        Self {
            connection_wait: false,
            instances: instances::Instances::new(&sockets, finds_launcher, Arc::clone(&launched)),
            catalog,
            root: profile.to_path_buf(),
            sockets,
            #[cfg(target_os = "macos")]
            data_home,
            connectors: connectors::Connectors::default(),
            #[cfg(target_os = "linux")]
            launcher_candidates,
            launched,
            own: Mutex::default(),
            launching: Mutex::default(),
            device_grants: tilo_peer_state::DeviceGrants::unkept(),
            grants: OnceLock::new(),
        }
    }

    /// Wait for a start's first connection, as a daemon does.
    #[cfg(test)]
    pub(crate) fn expecting_connection(mut self) -> Self {
        self.connection_wait = true;
        self
    }

    /// Attest this runtime's own process as the windowless endpoint of `app`
    /// in `workspace` from now on, until [`Self::stop_serving`].
    pub(crate) fn serve_endpoint(&self, app: &str, workspace: &str) {
        self.own
            .lock_recover()
            .insert((app.to_owned(), workspace.to_owned()));
    }

    /// No longer attest this process as `app`'s endpoint in `workspace`.
    pub(crate) fn stop_serving(&self, app: &str, workspace: &str) {
        self.own
            .lock_recover()
            .remove(&(app.to_owned(), workspace.to_owned()));
    }

    /// Attest the next connection this process opens naming `app` in `workspace` as that launch:
    /// the one it opens for a process it is about to start, before the process runs.
    pub(crate) fn launching(&self, app: &str, workspace: &str) {
        self.launching
            .lock_recover()
            .push((app.to_owned(), workspace.to_owned()));
    }

    /// The launch of `app` in `workspace` this process opened a connection for, taken once by the
    /// connection that names it.
    fn take_launching(&self, pid: u32, app: &str, workspace: &str) -> bool {
        if pid != std::process::id() {
            return false;
        }
        let mut launching = self.launching.lock_recover();
        let Some(at) = launching
            .iter()
            .position(|(launched, into)| launched == app && into == workspace)
        else {
            return false;
        };
        launching.remove(at);
        true
    }

    /// Whether `pid` is this runtime's own process serving `app` in
    /// `workspace`.
    fn serves(&self, pid: u32, app: &str, workspace: &str) -> bool {
        pid == std::process::id()
            && self
                .own
                .lock_recover()
                .contains(&(app.to_owned(), workspace.to_owned()))
    }

    /// The person's device answers: what the capture this runtime brokers asks.
    pub(crate) const fn device_grants(&self) -> &tilo_peer_state::DeviceGrants {
        &self.device_grants
    }

    /// Read what a launch into this runtime's workspace is granted from
    /// `source` from now on: the workspace service, once it is open.
    pub(crate) fn keep_launch_grants(&self, source: Arc<dyn LaunchGrantsSource>) {
        let _ = self.grants.set(source);
    }

    /// Hand every app instance started from now on what the providers in `providers` publish,
    /// so the packages its code uses that are theirs are called where they registered them.
    pub(crate) fn read_provided_from(
        &self,
        providers: tilo_workspace_service::workspaces::providers::Providers,
    ) {
        self.instances.read_provided_from(providers);
    }

    /// Hand the tilolang process of each app's own instances started from now on its daemon line,
    /// made by `lines` ([`crate::device::processes::OwnLines`]).
    pub(crate) fn serve_own_lines(&self, lines: crate::device::processes::OwnLines) {
        self.instances.serve_own_lines(lines);
    }

    /// The connections of a window of `app` in `workspace` whose instance runs in this process
    /// (`Instances::window_here`): the member at it, the instance it is, and its collaboration
    /// and identity connections.
    ///
    /// # Errors
    /// The device is no one's yet, or a service of the daemon could not be reached.
    pub(crate) fn window_in_this_process(
        &self,
        app: &str,
        workspace: &str,
    ) -> Result<
        (
            String,
            u64,
            runtime_portal::process_link::Descriptor,
            runtime_portal::process_link::Descriptor,
        ),
        String,
    > {
        self.instances.window_here(app, workspace)
    }

    /// Run the code of an app whose calls name no view as `person`, the member the device acts
    /// as, from now on.
    pub(crate) fn keep_person(&self, person: &str) {
        self.instances.keep_person(person.to_owned());
    }

    /// Keep the files people choose for hosted apps in the device's records from now on.
    #[cfg(target_os = "macos")]
    pub(crate) fn keep_chosen_records(&self, records: Arc<crate::device::records::DeviceRecords>) {
        self.instances.keep_chosen_records(records);
    }

    /// The page this device's keeping server serves the workspaces `workspaces` at, from now on:
    /// their apps mint their resources' links under it. A device that keeps no server hands none.
    pub(crate) fn keep_serving(&self, web_entry: &str, workspaces: &[String]) {
        self.instances
            .keep_serving(web_entry.to_owned(), workspaces.to_vec());
    }

    /// This daemon's tilolang processes, which start every tilolang process it runs.
    ///
    /// # Errors
    /// No launcher is installed to start them.
    pub(crate) fn processes(&self) -> Result<Arc<crate::device::processes::Processes>, String> {
        self.instances.processes().cloned()
    }

    /// Open a view of the tilolang app `app` in `workspace` for `caller`, whom the device's
    /// dispatch admitted (a member, a member's agent, a visitor at a page): an instance of the
    /// app, started here with no session in the app's sandboxed process, shows it, on `record`
    /// (else what it opens by default) and revealing `reveal`. What the instance writes for the
    /// view is the caller's, limited to what they may do. Its window, which lays the view out
    /// and draws nothing, runs sandboxed as every tilolang process here does; the answer is the
    /// device's end of its page link (`tilo_server_protocol::page::window`), and closing it ends
    /// the window.
    ///
    /// # Errors
    /// The app is no tilolang app installed here, no launcher can start its process, or its
    /// instance or its window did not start.
    pub fn open_view(
        &self,
        caller: &tilo_actions::remote::Caller,
        app: &str,
        workspace: &str,
        record: Option<String>,
        reveal: Option<String>,
    ) -> Result<tilo_socket::Stream, String> {
        let view = self.instances.open_view(
            caller,
            (app, workspace, self.origin_connection(app, workspace)?),
            record,
            reveal,
        )?;
        self.processes()?
            .show_page(app, workspace, view)
            .map_err(|error| format!("the view's window: {error}"))
    }

    /// What a launch into `workspace` is granted: the workspace's own
    /// sandbox, and nothing until a source is open.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            dead_code,
            reason = "only an app sandbox reads it, and this platform has none: every launch is refused"
        )
    )]
    fn launch_grants(&self, workspace: &str) -> collab_types::caps::LaunchGrants {
        self.grants
            .get()
            .map_or_else(collab_types::caps::LaunchGrants::default, |source| {
                source.launch_grants(workspace)
            })
    }

    /// What this runtime's launch of `app` into `workspace` hands it: the
    /// workspace's grants that name the app, as `tilo-launcher`'s capability
    /// file carries them.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            dead_code,
            reason = "only an app sandbox reads it, and this platform has none: every launch is refused"
        )
    )]
    fn app_grants(&self, app: &InstalledApp, workspace: &str) -> collab_types::caps::LaunchGrants {
        self.launch_grants(workspace).for_app(app.app_id.as_str())
    }

    /// The sandbox plan and outbound decision for launching `app`: what its
    /// manifest declares, narrowed by what the workspace granted it, through
    /// the launcher's own capability and egress rules — the same ones
    /// `tilo-launcher` applies to a launch on Linux.
    #[cfg(any(target_os = "macos", test))]
    fn launch_capabilities(
        &self,
        app: &InstalledApp,
        workspace: &str,
        manifest: &launcher::manifest::ManifestView,
    ) -> (
        launcher::capabilities::CapabilityPlan,
        launcher::egress_proxy::EgressDecision,
    ) {
        let declared =
            launcher::capabilities::declared(manifest, self.app_grants(app, workspace).sandbox);
        let plan = launcher::capabilities::CapabilityPlan::for_app(
            app.app_id.as_str(),
            &declared,
            Some(tilo_clock::unix_now()),
        );
        let egress = launcher::egress_proxy::egress_decision(manifest, &plan, &app.payload_root);
        (plan, egress)
    }

    /// Every installed package, GUI or not; empty without an app store or
    /// with an unreadable registry.
    fn installed(&self) -> Vec<InstalledApp> {
        self.catalog
            .as_ref()
            .and_then(|catalog| app_catalogue::apps::read(catalog).ok())
            .unwrap_or_default()
    }

    /// What the installed package `app_id` declared; nothing for an unknown
    /// package or an unreadable manifest.
    fn declared_access(&self, app_id: &str) -> AppActionAccess {
        self.installed()
            .into_iter()
            .find(|app| app.app_id == app_id)
            .map(|app| app.declared_actions())
            .unwrap_or_default()
    }

    /// Whether the build of `app` installed here declares `capability`.
    pub(crate) fn declares(&self, app: &str, capability: &str) -> bool {
        self.installed()
            .into_iter()
            .find(|installed| installed.app_id == app)
            .is_some_and(|installed| {
                installed
                    .declared_capabilities()
                    .iter()
                    .any(|declared| declared == capability)
            })
    }

    /// The resource kinds the installed package `app_id` declared its
    /// records hold; `None` when it is not installed.
    #[must_use]
    pub fn declared_resource_kinds(&self, app_id: &str) -> Option<Vec<String>> {
        self.installed()
            .into_iter()
            .find(|app| app.app_id == app_id)
            .map(|app| app.declared_resource_kinds())
    }

    /// [`Self::declared_access`] as the broker asks it.
    #[must_use]
    pub fn access_resolver(self: &Arc<Self>) -> AppActionAccessResolver {
        let apps = Arc::clone(self);
        Arc::new(move |app_id| apps.declared_access(app_id))
    }

    /// [`Self::declared_resource_kinds`] as the broker asks it.
    #[must_use]
    pub fn resource_kinds_resolver(self: &Arc<Self>) -> AppResourceKindsResolver {
        let apps = Arc::clone(self);
        Arc::new(move |app_id| apps.declared_resource_kinds(app_id).unwrap_or_default())
    }

    /// What people call `app_id`: its installed name, else one derived from
    /// the id, as the person is asked to confirm a launch grant.
    #[must_use]
    pub fn display_name(&self, app_id: &str) -> String {
        self.installed()
            .into_iter()
            .find(|app| app.app_id == app_id)
            .map_or_else(
                || package_store::display_name_for_package_id(app_id),
                |app| app.display_name,
            )
    }

    /// The surface the signed package `app` ships, when its signature still
    /// matches the bytes.
    fn signed_surface_of(&self, app: &InstalledApp) -> Option<Vec<u8>> {
        let package = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.list().ok())
            .and_then(|packages| {
                packages
                    .into_iter()
                    .find(|package| package.manifest_path == app.manifest_path)
            })?;
        package_store::integrity::signed_surface(&package)
    }

    /// The surface the installed package `app_id` publishes, in the form it
    /// is kept and addressed by ([`collab_types::surface::canonical`]),
    /// whether or not it runs here: what an agent reads its types from, and
    /// what an app's resources are decided against, before the app first opens.
    /// The one it was launched with wins, since an update the package store
    /// has not finished installing is not one.
    #[must_use]
    pub fn installed_surface(&self, app_id: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = ui::app_surface::bytes_of(app_id) {
            return Some(bytes.to_vec());
        }
        let app = self
            .installed()
            .into_iter()
            .find(|app| app.app_id == app_id)?;
        let bytes = self.signed_surface_of(&app)?;
        tilo_workspace_service::apps::register_package_surface(app_id, &bytes);
        ui::app_surface::bytes_of(app_id).map(<[u8]>::to_vec)
    }

    /// Name every installed package's signed surface now, so the app surfaces
    /// a workspace's agents read and its changes are decided against are there
    /// from the start rather than from an app's first window.
    pub fn register_installed_surfaces(&self) {
        for app in self.installed() {
            self.run_with_signed_surface(&app);
        }
    }

    /// Run the package `app` with the surface its signed package ships, so
    /// the records of the person's it declares there are kept in their workspace
    /// as a built-in app's are. A package whose surface no longer matches its
    /// signature declares none.
    fn run_with_signed_surface(&self, app: &InstalledApp) {
        if let Some(surface) = self.signed_surface_of(app) {
            tilo_workspace_service::apps::register_package_surface(&app.app_id, &surface);
        }
    }

    /// Start the instance of a window of `app` in `workspace` that this daemon launches, in the
    /// app's tilolang process and reaching its origin's connector when its app has one: the
    /// window takes its patch connection on its link ([`instances::Instances::open_window`]).
    ///
    /// # Errors
    /// As [`Self::origin_connection`] and [`instances::Instances::open_window`].
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn hosted_window(&self, app: &str, workspace: &str) -> Result<instances::HostedWindow, String> {
        let origin = self.origin_connection(app, workspace)?;
        self.instances.open_window(app, workspace, origin)
    }

    /// [`Self::hosted_window`], with the window's patch connection already put on the link the
    /// window is handed, before it starts to read it.
    ///
    /// # Errors
    /// As [`Self::hosted_window`], or the link could not be made or written.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn hosted_window_link(&self, app: &str, workspace: &str) -> Result<WindowLink, String> {
        let window = self.hosted_window(app, workspace)?;
        WindowLink::carrying(window).map_err(|error| format!("{app}'s window link: {error}"))
    }

    /// The connection an instance of `app` in `workspace` reaches its origin's connector over,
    /// starting the connector when none runs: `None` for an app whose types hold no outside
    /// service's resources, or that is no tilolang app.
    ///
    /// # Errors
    /// The app is not installed here, declares more than one origin, or its connector cannot
    /// start ([`Self::start_connector`]).
    pub(crate) fn origin_connection(
        &self,
        app: &str,
        workspace: &str,
    ) -> Result<Option<runtime_portal::process_link::Descriptor>, String> {
        // Any other app is refused where its instance would start, saying why.
        if !crate::device::package_runs::runs_in_process(app) {
            return Ok(None);
        }
        let Some(origin) = crate::device::package_runs::origin_of(app)? else {
            return Ok(None);
        };
        let (instance, theirs) = runtime_portal::origin_connector::connection()
            .map_err(|error| format!("no connection to {app}'s {origin} connector: {error}"))?;
        self.attach_origin(app, &origin, workspace, &theirs)?;
        Ok(Some(instance))
    }

    /// Attach `connection`, the connector's end of a reader's connection, to `app`'s connector of
    /// `origin` in `workspace`, starting the connector when none runs: one connector serves the
    /// app's instances and the desktop session's services alike.
    ///
    /// # Errors
    /// The app reaches no such origin, or its connector cannot be started or takes no connection.
    pub(crate) fn attach_origin(
        &self,
        app: &str,
        origin: &str,
        workspace: &str,
        connection: &runtime_portal::process_link::Descriptor,
    ) -> Result<(), String> {
        let Some(declared) = crate::device::package_runs::origin_of(app)? else {
            return Err(format!("{app} reaches no outside service called {origin}"));
        };
        if declared != origin {
            return Err(format!("{app} reaches no outside service called {origin}"));
        }
        self.connectors
            .attach((app, origin, workspace), connection, &|socket| {
                self.start_connector(app, workspace, origin, socket)
            })
    }

    /// Start `app_id`'s connector for `origin` in `workspace`, handed `socket`: the app's own
    /// launch with no window, in the sandbox its signed manifest declares for that origin.
    ///
    /// # Errors
    /// This device does not serve the workspace, the app is not installed here or its signed
    /// package does not verify, or no launcher can start it.
    pub(crate) fn start_connector(
        &self,
        app_id: &str,
        workspace: &str,
        origin: &str,
        socket: &runtime_portal::process_link::Descriptor,
    ) -> Result<connectors::Process, String> {
        #[cfg(unix)]
        use std::os::fd::AsFd as _;
        if !self
            .grants
            .get()
            .is_some_and(|source| source.serves(workspace))
        {
            return Err(format!(
                "workspace {workspace} is not served by this runtime"
            ));
        }
        let app = self
            .installed()
            .into_iter()
            .find(|app| app.app_id == app_id)
            .ok_or_else(|| format!("{app_id} is not installed"))?;
        #[cfg(target_os = "linux")]
        {
            let sockets = self.sockets.clone();
            let original = self.original_launch(&app)?;
            let child = linux::connector(
                self,
                &app,
                workspace,
                &sockets,
                &original,
                (origin, socket.as_fd()),
            )?;
            self.launched.record_showing(
                connectors::pid(&child),
                app_id,
                workspace,
                Shows::Connection,
                None,
            );
            Ok(child)
        }
        #[cfg(target_os = "macos")]
        {
            let child = macos::connector(self, &app, workspace, (origin, socket.as_fd()))?;
            if let Some(pid) = connectors::pid(&child) {
                self.launched
                    .record_showing(pid, app_id, workspace, Shows::Connection, None);
            }
            Ok(child)
        }
        #[cfg(target_os = "android")]
        {
            let _ = (app, origin, socket.as_fd());
            Err(
                "Android starts no process for an app: its apps run in the Tilo app's own process"
                    .to_owned(),
            )
        }
        #[cfg(windows)]
        {
            let _ = (app, origin, socket);
            Err(format!(
                "{app_id}'s connector to {origin} is a launch of the app itself, which this \
                 Windows device does not start: only tilolang apps run here"
            ))
        }
    }

    /// The app, and the workspace it runs in, that process `pid` is: exactly the process this
    /// runtime recorded when it started it, while its start time still matches. `None` for a
    /// process no launch here started, for one it started that runs no app, and for any process
    /// below a recorded one: an app's helpers inherit nothing of it.
    pub(crate) fn attested(&self, pid: u32) -> Option<(String, String)> {
        self.launched.of(pid)
    }

    /// What process of an app `pid` is, as [`Self::attested`] knows it: a sandboxed launch of the
    /// app's own package (and whether it is its held connection), or a tilolang process.
    pub(crate) fn process_of(&self, pid: u32) -> Option<ProcessOf> {
        self.launched.process_of(pid)
    }

    /// Record `pid` as the process this runtime started to run `app` in `workspace`, so a
    /// connection from it is admitted as that app: a tilolang instance, an origin connector or a
    /// task's host.
    pub(crate) fn record_started(&self, pid: u32, app: &str, workspace: &str) {
        let _ = self.launched.record_started(pid, app, workspace);
    }

    /// Record `pid` as the process this runtime started as `app`'s held connection in
    /// `workspace`, as a connector's launch is: what a test of an app that signs in serves its
    /// connection through, rather than starting the app's process.
    #[cfg(feature = "test-support")]
    pub(crate) fn record_connection_for_test(&self, pid: u32, app: &str, workspace: &str) {
        let _ = self
            .launched
            .record_showing(pid, app, workspace, Shows::Connection, None);
    }

    /// Record `pid` as the tilolang process that runs `app`'s own instances in `workspace`, as the
    /// device's processes record the one they start.
    #[cfg(test)]
    pub(crate) fn record_own_tilolang(&self, pid: u32, app: &str, workspace: &str) {
        let _ = self.launched.record_own_tilolang(pid, app, workspace);
    }

    /// Forget the launch recorded for `pid`, once its process ended.
    #[cfg(test)]
    pub(crate) fn forget_started(&self, pid: u32) {
        self.launched.forget(pid);
    }

    pub(crate) fn launchable(&self, app_id: &str) -> Result<InstalledApp, String> {
        let app = self.installed_app(app_id)?;
        if !app.has_gui {
            return Err(format!("{app_id} has no window to open"));
        }
        Ok(app)
    }

    /// The profile this runtime reads its apps and services in.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The package `app_id` this computer has installed, and nothing for one it has not: what a
    /// program's request to run an app resolves to, so what runs is decided here and never by
    /// the program asking.
    ///
    /// # Errors
    /// The app is not installed on this computer: how to install it.
    pub(crate) fn installed_here(&self, app_id: &str) -> Result<InstalledApp, String> {
        self.installed_app(app_id).map_err(|_| {
            format!(
                "{app_id} is not installed on this computer; install it first with \
                 `tilo app install {app_id}`"
            )
        })
    }

    /// Start the app this computer has installed, as a window launch does, and watch its
    /// process: the launch is recorded before the app's connections are made, and forgotten
    /// when the process ends.
    ///
    /// # Errors
    /// The app is not installed, has no window, or the launcher refuses it.
    pub(crate) fn start_watched_window(
        &self,
        app_id: &str,
        workspace: &str,
        guard: Option<advisory_lock::FileLock>,
    ) -> Result<u32, String> {
        let launch = self.start(app_id, workspace)?;
        let pid = launch.pid;
        let launched = Arc::clone(&self.launched);
        std::thread::Builder::new()
            .name("tilo-app-reaper".into())
            .spawn(move || {
                launch.wait();
                launched.forget(pid);
                // The default presentation's launch lock lives while its app runs: a start
                // during it waits, or shows the window this one published.
                drop(guard);
            })
            .map_err(|error| format!("could not watch {app_id}: {error}"))?;
        Ok(pid)
    }

    /// The installed package `app_id`, window or none.
    pub(crate) fn installed_app(&self, app_id: &str) -> Result<InstalledApp, String> {
        self.installed()
            .into_iter()
            .find(|app| app.app_id == app_id)
            .ok_or_else(|| format!("{app_id} is not installed"))
    }

    /// Launch `app_id` into this profile's workspace through the platform
    /// launcher; the launch's handle, recorded for attestation.
    ///
    /// # Errors
    /// The workspace is not served here, the app is not installed, or no
    /// launcher can start it.
    pub fn start(&self, app_id: &str, workspace: &str) -> Result<Launch, String> {
        self.start_showing(app_id, workspace, Shows::Window, None)
    }

    /// [`Self::start`], showing a window or running the app's code with none, on `record` when the
    /// launch names one: the app opens it, or creates it when nothing wrote it yet.
    ///
    /// # Errors
    /// As [`Self::start`].
    #[cfg_attr(
        target_os = "android",
        expect(
            unreachable_code,
            unused_variables,
            reason = "with no app sandbox here, the launch is refused before it starts"
        )
    )]
    fn start_showing(
        &self,
        app_id: &str,
        workspace: &str,
        shows: Shows,
        open: Option<&str>,
    ) -> Result<Launch, String> {
        if !self
            .grants
            .get()
            .is_some_and(|source| source.serves(workspace))
        {
            return Err(format!(
                "workspace {workspace} is not served by this runtime"
            ));
        }
        // A held connection shows nothing, so it needs no window of its own: the launcher runs
        // its entrypoint's `bin`.
        let app = if shows == Shows::Connection {
            self.installed_app(app_id)?
        } else {
            self.launchable(app_id)?
        };
        // The record the app is asked to open, as a window of the session asks for one.
        let open_uri = open
            .map(|record| {
                collab_types::document::DocumentId::parse(record)
                    .map(|record| record.uri())
                    .ok_or_else(|| format!("{record} is not a record id"))
            })
            .transpose()?;
        self.run_with_signed_surface(&app);
        let sockets = self.sockets.clone();
        #[cfg(target_os = "linux")]
        let original = self.original_launch(&app)?;
        #[cfg(target_os = "linux")]
        let mut launch = linux::launch(
            self,
            &app,
            workspace,
            &sockets,
            &original,
            shows,
            open_uri.as_deref(),
        )?;
        #[cfg(target_os = "macos")]
        let mut launch =
            macos::launch(self, &app, workspace, &sockets, shows, open_uri.as_deref())?;
        #[cfg(windows)]
        let mut launch = {
            let _ = &sockets;
            windows::launch(self, &app, workspace, shows, open_uri.as_deref())?
        };
        #[cfg(target_os = "android")]
        let mut launch: Launch = {
            let _ = (&app, &sockets, shows, open_uri);
            return Err("refusing to launch: no app sandbox on this platform".into());
        };
        if !self
            .launched
            .record_showing(launch.pid, &app.app_id, workspace, shows, open)
        {
            launch.kill();
            return Err(format!(
                "refusing to keep {app_id} running: its process could not be identified"
            ));
        }
        // Only now may the launcher go on and connect: the broker attests it
        // once, against the record just made.
        if let Err(error) = launch.release_registration() {
            self.launched.forget(launch.pid);
            launch.kill();
            return Err(format!("could not let {app_id}'s launcher go on: {error}"));
        }
        self.started(launch, app_id)
    }

    /// The start answers once the app is up: its launcher has connected, as the broker
    /// attests it. A process that ends first ends the start, with what it said, never answering
    /// that it started; one that stays alive without connecting is waited for, not cut off at a
    /// time this code invented, and said so in the log while it waits. Where no connection is
    /// expected (a test's launch), a process still running after the start grace is taken as
    /// started.
    fn started(&self, mut launch: Launch, app_id: &str) -> Result<Launch, String> {
        let started = std::time::Instant::now();
        let mut warned = started;
        loop {
            if self.launched.connected(launch.pid) {
                return Ok(launch);
            }
            match launch.ended() {
                Ok(Some(status)) => {
                    self.launched.forget(launch.pid);
                    let said = launch.said();
                    let said = said.trim();
                    tracing::error!(
                        app = app_id,
                        %status,
                        said,
                        "an app ended as its start began"
                    );
                    return Err(if said.is_empty() {
                        format!("{app_id} ended as it started ({status})")
                    } else {
                        format!("{app_id} ended as it started ({status}): {said}")
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    self.launched.forget(launch.pid);
                    launch.kill();
                    tracing::error!(app = app_id, %error, "an app's start could not be read");
                    return Err(format!("{app_id}'s start could not be read: {error}"));
                }
            }
            if self.connection_wait {
                if warned.elapsed() >= std::time::Duration::from_secs(5) {
                    warned = std::time::Instant::now();
                    tracing::warn!(
                        app = app_id,
                        waited_s = started.elapsed().as_secs(),
                        said = launch.said(),
                        "an app has not connected since its start"
                    );
                }
            } else if started.elapsed() >= std::time::Duration::from_millis(100) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(launch)
    }

    /// [`Self::start_showing`] `app` in `workspace` with `shows` on `record`, forgetting its record
    /// once the process is reaped; the launch's ticket.
    fn start_watched(
        &self,
        app: &str,
        workspace: &str,
        shows: Shows,
        record: Option<&str>,
    ) -> Result<Option<u64>, String> {
        let launch = self.start_showing(app, workspace, shows, record)?;
        let ticket = self.launched.ticket(launch.pid);
        let launched = Arc::clone(&self.launched);
        std::thread::Builder::new()
            .name("tilo-app-reaper".into())
            .spawn(move || {
                let pid = launch.pid;
                launch.wait();
                launched.forget(pid);
            })
            .map(|_| ticket)
            .map_err(|error| format!("could not watch the launched app: {error}"))
    }
}

/// What an installed app answers search about one of its records: its own binary, started by
/// `tilo-launcher` in its record mode with no services, reads the record on its input and answers
/// its entries on its output.
#[cfg(target_os = "linux")]
impl tilo_workspace_search::records::RecordReader for Apps {
    fn entries(
        &self,
        app: &str,
        doc: &[u8],
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, tilo_workspace_search::records::Unread>
    {
        use std::io::Write as _;
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::{Command, Stdio};
        use tilo_workspace_search::records::{INDEX_FLAG, Unread};

        let launcher = self
            .launcher_candidates
            .iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                Unread::Refused(
                    "no tilo-launcher is installed, so no app's records are read".into(),
                )
            })?;
        let installed = self
            .installed()
            .into_iter()
            .find(|installed| installed.app_id == app)
            .ok_or_else(|| Unread::Refused(format!("{app} is not installed here")))?;
        let mut child = Command::new(launcher)
            .arg("run")
            .arg("--manifest")
            .arg(&installed.manifest_path)
            .arg("--payload-root")
            .arg(&installed.payload_root)
            .arg("--app-id")
            .arg(installed.app_id.as_str())
            .arg("--no-services")
            .arg("--")
            .arg(INDEX_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Unread::Refused(format!("{app} cannot read its record: {error}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            // A reader stopped before it read any of the record leaves the pipe it writes to
            // closed: nothing read, rather than anything about the record.
            if let Err(error) = stdin.write_all(doc).and_then(|()| stdin.flush()) {
                return Err(if error.kind() == std::io::ErrorKind::BrokenPipe {
                    Unread::Stopped(format!("{app} was stopped before it read its record"))
                } else {
                    Unread::Refused(format!("{app} reads none of its record: {error}"))
                });
            }
        }
        let out = child.wait_with_output().map_err(|error| {
            Unread::Refused(format!("{app} stops without reading its record: {error}"))
        })?;
        if !out.status.success() {
            let said = format!(
                "{app} reads none of its record ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            // What ends a reader from outside (the daemon stopping, an app closed for memory)
            // says nothing about the record it was reading.
            return Err(match out.status.signal() {
                Some(libc::SIGTERM | libc::SIGINT | libc::SIGHUP | libc::SIGKILL) => {
                    Unread::Stopped(said)
                }
                _ => Unread::Refused(said),
            });
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|error| Unread::Refused(format!("{app} answers no entries: {error}")))
    }
}

/// Where no app sandbox runs, no app reads its records: search holds what running apps publish.
#[cfg(not(target_os = "linux"))]
impl tilo_workspace_search::records::RecordReader for Apps {
    fn entries(
        &self,
        app: &str,
        _doc: &[u8],
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, tilo_workspace_search::records::Unread>
    {
        Err(tilo_workspace_search::records::Unread::Refused(format!(
            "{app} reads no record here: this platform starts no app in its record mode"
        )))
    }
}

impl LauncherServices for Apps {
    fn applications(&self) -> Vec<LaunchableApplication> {
        let mut apps = self.installed();
        apps.retain(|app| app.has_gui);
        apps.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        apps.into_iter()
            .map(|app| LaunchableApplication {
                surface: self
                    .installed_surface(&app.app_id)
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                capabilities: Vec::new(),
                package_id: app.app_id.to_string(),
                display_name: app.display_name,
                unconfined: false,
                offers: app.offers,
            })
            .collect()
    }

    /// A standalone runtime draws no shell, so there is no place to open at;
    /// it opens the app's default record.
    fn launch(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &LaunchPlace,
    ) -> Result<(), String> {
        self.start_watched(app, &workspace.id().to_string(), Shows::Window, None)
            .map(drop)
    }

    /// A plain launch answers the ticket its process connects with; this
    /// runtime stages no launch content.
    fn launch_content(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        content: LaunchContent<'_>,
        _: &LaunchPlace,
    ) -> Result<LaunchOutcome, String> {
        if !content.hash.is_empty() {
            return Err("opening launch content is unsupported on this host".into());
        }
        Ok(self
            .start_watched(app, &workspace.id().to_string(), Shows::Window, None)?
            .map_or(LaunchOutcome::Unconfirmed, |ticket| {
                LaunchOutcome::Started { ticket }
            }))
    }

    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.launched.attest(pid, app, workspace) || self.serves(pid, app, workspace)
    }

    /// A tilolang app runs an instance here that shows nothing, in its sandboxed process, to take
    /// the call that asked for it; a Rust app runs its own process with no window, started the way
    /// its window is, handed the workspace's services and granted what the workspace granted it.
    /// Either publishes its actions where the dispatch reaches them, and takes the call there.
    /// `None` for an app that can do neither: what it is launched as a window for says why.
    fn windowless(
        &self,
        app: &str,
        workspace: &str,
        record: Option<&str>,
    ) -> Result<Option<tilo_workspace_service::apps::services::WindowlessStart>, String> {
        if crate::device::package_runs::runs_in_process(app) {
            let origin = self.origin_connection(app, workspace)?;
            return self
                .instances
                .start_with_no_view(app, workspace, origin)
                .map(Some);
        }
        // The process that runs on that record already takes the call: nothing starts. One on
        // another record serves that one (`App.state.doc`), so this call starts its own.
        if self.launched.no_window_running(app, workspace, record) {
            return Ok(Some(ready_when_started()));
        }
        let Some(installed) = self
            .installed()
            .into_iter()
            .find(|installed| installed.app_id == app)
        else {
            return Ok(None);
        };
        if !installed.has_gui {
            // No window to open either: the launch that follows says so, as it always did.
            return Ok(None);
        }
        // Started and answered: the broker waits for what the process publishes, as it does for
        // a tilolang process with no view.
        let _ = self.start_watched(app, workspace, Shows::Nothing, record)?;
        Ok(Some(ready_when_started()))
    }

    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        // A connection this process made for an instance it started: the one it names, once.
        if pid == std::process::id()
            && let Some(instance) = window_instance
            && let Some(launch) = self.instances.take(app, workspace, instance)
        {
            return Ok(Some(launch));
        }
        // A connection this process opened for a process it is starting: that launch, once.
        if window_instance.is_none() && self.take_launching(pid, app, workspace) {
            return Ok(Some(AuthenticatedApplicationLaunch {
                continuation_instance: None,
                launch_ticket: None,
                windowless: false,
                opened_for: None,
            }));
        }
        if self.serves(pid, app, workspace) {
            return Ok(Some(AuthenticatedApplicationLaunch {
                continuation_instance: None,
                launch_ticket: None,
                windowless: true,
                opened_for: None,
            }));
        }
        if !self.launched.attest(pid, app, workspace) {
            return Ok(None);
        }
        Ok(Some(AuthenticatedApplicationLaunch {
            continuation_instance: None,
            launch_ticket: self.launched.ticket(pid),
            // A launch with no window takes the app's actions and is never handed an `open`.
            windowless: self.launched.no_window(pid, app, workspace),
            opened_for: None,
        }))
    }
    #[cfg(target_os = "linux")]
    fn attest_executing_process(
        &self,
        launcher_pid: u32,
        peer: &launcher::services::ExecutingPeer,
        app: &str,
        workspace: &str,
    ) -> bool {
        let Some(start) = process_start_time(launcher_pid) else {
            return false;
        };
        self.launched.attest(launcher_pid, app, workspace)
            && peer.belongs_to(launcher_pid, start)
            && self.launched.attest(launcher_pid, app, workspace)
    }
}

impl Apps {
    /// How many processes this device runs `app` in `workspace` with no window on `record` now:
    /// one per call that started one, so a second call answered by the same process leaves the
    /// count where it was. `record` names what the launch opened, when it named one.
    #[must_use]
    pub fn no_window_processes(&self, app: &str, workspace: &str, record: Option<&str>) -> usize {
        self.launched.no_window_processes(app, workspace, record)
    }
}

/// A tilolang window's instance, running in its app's process, and the window's link, which
/// carries its patch connection: the window draws what that instance sends and runs none of the
/// app's code itself.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct WindowLink {
    /// The window's instance: the one its patch connection is for.
    instance: u64,
    /// This process's end of the window's link, held while the window runs.
    ours: runtime_portal::process_link::Descriptor,
    /// The window's end, which it is handed.
    window_end: runtime_portal::process_link::Descriptor,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl WindowLink {
    /// A link whose window end finds `window`'s patch connection waiting on it.
    fn carrying(window: instances::HostedWindow) -> std::io::Result<Self> {
        use runtime_portal::process_link::Packet as _;
        let (ours, window_end) = runtime_portal::process_link::pair()?;
        runtime_portal::process_link::ToProcess::Patch {
            instance: window.instance,
            patch: window.patch,
        }
        .send(&ours)?;
        Ok(Self {
            instance: window.instance,
            ours,
            window_end,
        })
    }
}

/// Whether a window of the app `manifest` declares is hosted: a tilolang app's instance runs in
/// its app's tilolang process, and its window shows it. A launch that shows nothing has no window.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hosts_window(manifest: &launcher::manifest::ManifestView, shows: Shows) -> bool {
    shows == Shows::Window
        && manifest
            .entrypoint
            .runtime
            .as_deref()
            .is_some_and(launcher::manifest::is_tilolang)
}

/// One launched app: the process the runtime started, which is the one the
/// broker sees connect (the launcher on Linux, the app on macOS).
pub struct Launch {
    pid: u32,
    #[cfg(target_os = "linux")]
    child: std::process::Child,
    /// Holds the launcher before it connects anywhere until the launch is
    /// recorded; `None` once released.
    #[cfg(target_os = "linux")]
    registration: Option<launcher::launch_hold::LaunchHold>,
    /// A hosted window's end of its link, handed to the launcher with its release; `None` once
    /// handed, and for any other launch.
    #[cfg(target_os = "linux")]
    window_link: Option<runtime_portal::process_link::Descriptor>,
    #[cfg(target_os = "macos")]
    app: launcher_macos::LaunchedApp,
    /// A tilolang window's link, which its patch connection came on: held while the window runs.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    _link: Option<runtime_portal::process_link::Descriptor>,
    /// The tilolang window, in its app's container.
    #[cfg(windows)]
    app: crate::device::capabilities::Confined,
    /// The window's link, which its patch connection came on: held while the window runs.
    #[cfg(windows)]
    _link: runtime_portal::process_link::Descriptor,
    /// What the program said on its standard error, bounded: a start that ends as it starts
    /// answers this, so a person is told why rather than that it started.
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Launch {
    /// Let a launcher held at its launch hold go on. Call once the
    /// launch is recorded for attestation.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            clippy::needless_pass_by_ref_mut,
            reason = "with no app sandbox here, no launcher is held back to release"
        )
    )]
    fn release_registration(&mut self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(hold) = self.registration.take() {
            // A hosted window's link goes with the release: the launcher hands it to the window,
            // whose patch connection is waiting on it.
            return match self.window_link.take() {
                Some(link) => hold.release_with_window_link(std::os::fd::AsFd::as_fd(&link)),
                None => hold.release(),
            };
        }
        Ok(())
    }

    /// Whether the program already ended, and how: a start that ends as it starts has run no
    /// app.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            clippy::needless_pass_by_ref_mut,
            reason = "only a Linux launch is a child this daemon can ask whether it ended"
        )
    )]
    fn ended(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        #[cfg(target_os = "linux")]
        {
            self.child.try_wait()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(None)
        }
    }

    /// What the program said on its standard error before it ended.
    fn said(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Wait for the process to exit; its exit code, when it exited normally.
    #[cfg_attr(
        target_os = "android",
        expect(
            clippy::unused_self,
            reason = "with no app sandbox here, no launched process waits"
        )
    )]
    pub fn wait(self) -> Option<i32> {
        #[cfg(target_os = "linux")]
        let status = {
            let mut child = self.child;
            child.wait().ok()
        };
        #[cfg(target_os = "macos")]
        let status = {
            let mut app = self.app;
            app.wait().ok()
        };
        #[cfg(windows)]
        let status = {
            let mut app = self.app;
            app.wait().ok()
        };
        #[cfg(target_os = "android")]
        let status: Option<std::process::ExitStatus> = None;
        status.and_then(|status| status.code())
    }

    #[cfg_attr(
        target_os = "android",
        expect(
            clippy::unused_self,
            reason = "with no app sandbox here, no launched process is killed"
        )
    )]
    fn kill(self) {
        #[cfg(target_os = "linux")]
        {
            let mut child = self.child;
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(target_os = "macos")]
        {
            let mut app = self.app;
            let _ = app.kill();
            let _ = app.wait();
        }
        #[cfg(windows)]
        self.app.kill();
    }
}

/// The processes this runtime launched, by pid, with the start time that
/// tells a live launch from a later process reusing its pid, and the last
/// launch ticket handed out.
#[derive(Default)]
pub(crate) struct Launched(
    Mutex<HashMap<u32, LaunchRecord>>,
    AtomicU64,
    /// The daemon's own tilolang processes by pid, and the app and workspace each runs: the
    /// instances it starts, the origin connectors and the task hosts. A connection from one is
    /// admitted as the app it runs, never as the account that started it.
    Mutex<HashMap<u32, StartedRecord>>,
);

/// A process the daemon started for a tilolang app, and the workspace it runs in.
struct StartedRecord {
    start_time: u64,
    app: String,
    workspace: String,
    /// The tilolang process that runs the app's own instances alone.
    own: bool,
}

struct LaunchRecord {
    start_time: u64,
    app: String,
    workspace: String,
    /// The app has connected: its launch is up, and the start may answer.
    connected: bool,
    /// The record the launch opened, when it named one: a process serves the one record its App
    /// holds, so a call on another record starts a process of its own.
    record: Option<String>,
    /// What the launch answered its `open` with ([`LaunchOutcome::Started`]),
    /// for the broker to tie the process's connection back to it.
    ticket: u64,
    /// The launch shows no window: its connection takes the app's actions on its records and is
    /// never handed an `open`.
    no_window: bool,
    /// The launch is the app's held connection.
    connection: bool,
}

/// What process of an app this daemon started a pid is, while it is still that process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProcessOf {
    /// A launch of the app's own package in its sandbox, and whether it is its held connection.
    Launched {
        app: String,
        workspace: String,
        connection: bool,
    },
    /// A process the daemon started for the app: a tilolang process, an origin connector or a
    /// task host. `own` when it is the tilolang process that runs the app's own instances alone.
    Started {
        app: String,
        workspace: String,
        own: bool,
    },
}

impl Launched {
    /// Record `pid` as `app` in `workspace` showing `shows` on `record`; `false` when its start
    /// time cannot be read.
    fn record_showing(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        shows: Shows,
        record: Option<&str>,
    ) -> bool {
        let Some(start_time) = process_start_time(pid) else {
            return false;
        };
        let ticket = self.1.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.0.lock_recover().insert(
            pid,
            LaunchRecord {
                start_time,
                app: app.to_owned(),
                workspace: workspace.to_owned(),
                connected: false,
                record: record.map(str::to_owned),
                ticket,
                no_window: shows != Shows::Window,
                connection: shows == Shows::Connection,
            },
        );
        true
    }

    /// Whether a process this device started with no window runs `app` in `workspace` on
    /// `record` now.
    fn no_window_running(&self, app: &str, workspace: &str, on: Option<&str>) -> bool {
        let launches = self.0.lock_recover();
        launches.iter().any(|(pid, record)| {
            record.no_window
                && record.app == app
                && record.workspace == workspace
                && record.record.as_deref() == on
                && process_start_time(*pid) == Some(record.start_time)
        })
    }

    /// How many processes this device started with no window for `app` in `workspace` on `record`
    /// run now: one per launch, so a second call that finds the first still answers it and starts
    /// nothing.
    fn no_window_processes(&self, app: &str, workspace: &str, on: Option<&str>) -> usize {
        let launches = self.0.lock_recover();
        launches
            .iter()
            .filter(|(pid, record)| {
                record.no_window
                    && record.app == app
                    && record.workspace == workspace
                    && record.record.as_deref() == on
                    && process_start_time(**pid) == Some(record.start_time)
            })
            .count()
    }

    /// The ticket of the launch recorded for `pid`, whatever it attests.
    fn ticket(&self, pid: u32) -> Option<u64> {
        self.0.lock_recover().get(&pid).map(|record| record.ticket)
    }

    /// Whether `pid` is still the process recorded for exactly `app` and
    /// `workspace`.
    fn attest(&self, pid: u32, app: &str, workspace: &str) -> bool {
        let mut launches = self.0.lock_recover();
        let Some(record) = launches.get_mut(&pid) else {
            return false;
        };
        if record.app != app || record.workspace != workspace {
            return false;
        }
        record.connected = true;
        drop(launches);
        true
    }

    /// Whether the app launched as `pid` has connected.
    fn connected(&self, pid: u32) -> bool {
        self.0
            .lock_recover()
            .get(&pid)
            .is_some_and(|record| record.connected)
    }

    /// Whether `pid` is the process this device started with no window for `app` in `workspace`.
    fn no_window(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.no_window_of(pid, app, workspace) == Some(true)
    }

    /// Whether `pid` is still the process recorded for exactly `app` and `workspace`, and
    /// whether that launch shows no window.
    fn no_window_of(&self, pid: u32, app: &str, workspace: &str) -> Option<bool> {
        let launches = self.0.lock_recover();
        let record = launches.get(&pid)?;
        let (recorded, same, no_window) = (
            record.start_time,
            record.app == app && record.workspace == workspace,
            record.no_window,
        );
        drop(launches);
        (same && process_start_time(pid) == Some(recorded)).then_some(no_window)
    }

    /// Forget `pid`, once its process has been reaped.
    pub(crate) fn forget(&self, pid: u32) {
        self.0.lock_recover().remove(&pid);
        self.2.lock_recover().remove(&pid);
    }

    /// Record `pid` as the process this runtime started to run `app` in `workspace`; `false`
    /// when its start time cannot be read.
    pub(crate) fn record_started(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.record_start(pid, app, workspace, false)
    }

    /// [`Self::record_started`] for the tilolang process that runs `app`'s own instances alone,
    /// and nothing of any other app's, a task's or a page's: the process its own secrets are kept
    /// for and read back by ([`ProcessOf::Started::own`]).
    pub(crate) fn record_own_tilolang(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.record_start(pid, app, workspace, true)
    }

    fn record_start(&self, pid: u32, app: &str, workspace: &str, own: bool) -> bool {
        let Some(start_time) = process_start_time(pid) else {
            return false;
        };
        self.2.lock_recover().insert(
            pid,
            StartedRecord {
                start_time,
                app: app.to_owned(),
                workspace: workspace.to_owned(),
                own,
            },
        );
        true
    }

    /// What process `pid` is, while it is still that process: one this runtime launched in the
    /// app's sandbox, or one it started for a tilolang app.
    pub(crate) fn process_of(&self, pid: u32) -> Option<ProcessOf> {
        let started = self.2.lock_recover().get(&pid).map(|record| {
            (
                record.start_time,
                record.app.clone(),
                record.workspace.clone(),
                record.own,
            )
        });
        if let Some((recorded, app, workspace, own)) = started {
            return (process_start_time(pid) == Some(recorded)).then_some(ProcessOf::Started {
                app,
                workspace,
                own,
            });
        }
        let launched = self.0.lock_recover().get(&pid).map(|record| {
            (
                record.start_time,
                ProcessOf::Launched {
                    app: record.app.clone(),
                    workspace: record.workspace.clone(),
                    connection: record.connection,
                },
            )
        });
        let (recorded, launched) = launched?;
        (process_start_time(pid) == Some(recorded)).then_some(launched)
    }

    /// The app and workspace `pid` is the process of, while it is still that process: one this
    /// runtime launched, or one it started for a tilolang app.
    fn of(&self, pid: u32) -> Option<(String, String)> {
        let started = self.2.lock_recover().get(&pid).map(|record| {
            (
                record.start_time,
                record.app.clone(),
                record.workspace.clone(),
            )
        });
        if let Some((recorded, app, workspace)) = started
            && process_start_time(pid) == Some(recorded)
        {
            return Some((app, workspace));
        }
        let launched = self.0.lock_recover().get(&pid).map(|record| {
            (
                record.start_time,
                record.app.clone(),
                record.workspace.clone(),
            )
        });
        let (recorded, app, workspace) = launched?;
        (process_start_time(pid) == Some(recorded)).then_some((app, workspace))
    }
}

/// When process `pid` started, in the kernel's units: clock ticks since boot
/// on Linux (`/proc/<pid>/stat` field 22), microseconds since the epoch on
/// macOS. `None` for a process that does not exist.
pub(crate) fn process_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name is parenthesised and may itself hold spaces or
        // parentheses; fields resume after its last `)`, at field 3.
        let (_, rest) = stat.rsplit_once(')')?;
        rest.split_whitespace().nth(22 - 3)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        let pid = libc::c_int::try_from(pid).ok()?;
        // SAFETY: an all-zero proc_bsdinfo is a valid value for this plain C
        // struct; proc_pidinfo writes at most `size` bytes into it.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` is a writable buffer of exactly `size` bytes.
        let written = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        if written != size {
            return None;
        }
        info.pbi_start_tvsec
            .checked_mul(1_000_000)?
            .checked_add(info.pbi_start_tvusec)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Where Tilo keeps per-user state for `home` on macOS.
fn data_home(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        launcher_macos::policy::data_home(home)
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.to_path_buf()
    }
}

/// Where `tilo-launcher` is looked for: an explicit binary in a developer
/// build only, then beside this executable, then the system paths. A shipped
/// build never lets the environment name the binary that builds every app's
/// sandbox.
fn launcher_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if package_store::dev_env::DEV_ENV_OVERRIDES
        && let Some(explicit) = std::env::var_os("TILO_LAUNCHER_BIN").filter(|v| !v.is_empty())
    {
        out.push(PathBuf::from(explicit));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        out.push(dir.join("tilo-launcher"));
    }
    out.push(PathBuf::from("/usr/libexec/tilo/tilo-launcher"));
    out.push(PathBuf::from("/usr/libexec/tilo-launcher"));
    out
}

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;

/// The device of the machine a capture device is.
#[cfg(target_os = "macos")]
pub(crate) const fn device_cap(device: tilo_capture::DeviceKind) -> collab_types::caps::DeviceCap {
    match device {
        tilo_capture::DeviceKind::Camera => collab_types::caps::DeviceCap::Camera,
        tilo_capture::DeviceKind::Microphone => collab_types::caps::DeviceCap::Microphone,
    }
}

#[cfg(target_os = "linux")]
impl crate::device::processes::Origins for Apps {
    fn connection(
        &self,
        app: &str,
        workspace: &str,
    ) -> Result<Option<runtime_portal::process_link::Descriptor>, String> {
        self.origin_connection(app, workspace)
    }

    fn attach(
        &self,
        app: &str,
        origin: &str,
        workspace: &str,
        connection: &runtime_portal::process_link::Descriptor,
    ) -> Result<(), String> {
        self.attach_origin(app, origin, workspace, connection)
    }
}
