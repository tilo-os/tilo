//! Turning a launch into the process it starts.
//!
//! Two very different things are started here, and the plan says which:
//!
//! - a Tilo package, through `tilo-launcher run`, which builds the package's
//!   sandbox from its signed manifest, the workspace it is launched into and the
//!   grants written for it ([`Workspace::write_grants`]);
//! - one of the machine's own applications, through `tilo-launcher machine-app`
//!   and its `.desktop` entry: Flatpak, `AppImage`, a program the person
//!   added, or one of the system's. It runs unconfined, as the person, exactly
//!   as [`desktop_entry::launch`] decides — see that module for why it cannot
//!   be confined — but through the same launcher as a package, so it is born
//!   in a scope of its own and the session's one memory watch covers it.
//!
//! A plan is pure data: the program, its arguments and the child's whole
//! environment. Nothing here starts anything, so what a launch *would* run is
//! assertable without a process appearing — that is what the tests do, and
//! what a session logs before [`crate::Spawner`] starts it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use desktop_entry::launch::{HostLaunch, LaunchEnv};

use crate::env::SessionEnvironment;
use crate::instance::package_identities;
use crate::launch::{Launch, Open};
use crate::workspace::Workspace;

/// Where `tilo-launcher` is installed.
const INSTALLED_LAUNCHER: &str = "/usr/libexec/tilo/tilo-launcher";
/// Where a distribution build puts it.
const DISTRIBUTION_LAUNCHER: &str = "/usr/libexec/tilo-launcher";
/// Its name beside an executable that was built next to it.
const LAUNCHER_NAME: &str = "tilo-launcher";

/// What kind of app a plan starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A Tilo package, in its sandbox.
    Package,
    /// A Flatpak, in its own.
    Flatpak,
    /// An `AppImage` the person downloaded.
    AppImage,
    /// A program the person added.
    Program,
    /// One of the operating system's applications.
    System,
}

impl Kind {
    /// Whether the plan starts an app unconfined on the host.
    #[must_use]
    pub fn is_host(self) -> bool {
        !matches!(self, Self::Package)
    }

    /// The kind of the app `entry` describes, as the person's own
    /// applications directory tells it apart from the system's.
    #[must_use]
    pub fn of_entry(entry: &desktop_entry::DesktopEntry) -> Self {
        let person = tilo_paths::xdg::data_home().map(|home| home.join("applications"));
        match entry.kind(person.as_deref()) {
            desktop_entry::EntryKind::Flatpak => Self::Flatpak,
            desktop_entry::EntryKind::AppImage => Self::AppImage,
            desktop_entry::EntryKind::Program => Self::Program,
            desktop_entry::EntryKind::System => Self::System,
        }
    }
}

/// Why a launch starts nothing.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// The package this launch names is not installed on this device.
    #[error("{0} is not installed here")]
    NotInstalled(String),
    /// The machine's app cannot be started, and why.
    #[error("{0}")]
    Host(#[from] desktop_entry::launch::HostLaunchError),
    /// The launch starts no process at all: a window already on its way, or
    /// a notice.
    #[error("{0} starts no process")]
    NothingToStart(String),
    /// The workspace's grants could not be written, so the app would start with
    /// fewer permissions than the person granted: refuse rather than launch
    /// it half-granted.
    #[error("the grants for {app} could not be written: {source}")]
    Grants {
        app: String,
        #[source]
        source: std::io::Error,
    },
    /// No Tilo launcher is installed, and every app starts through it.
    #[error("no tilo-launcher is installed, so no app can start here")]
    NoLauncher,
    /// The device's daemon, which decides every launch, refused this one.
    #[error("{0}")]
    Refused(String),
    /// A record launch names something that is not a record id: starting the
    /// app would open nothing, so the app is not started.
    #[error("{record} is not a record of {app}")]
    NotARecord { app: String, record: String },
    /// Opening content needs the bytes read through the scope of the app
    /// that handed them over, which a launch string does not carry: refuse
    /// rather than start an app that shows nothing.
    #[error("opening content in {app} is not available here")]
    ContentUnavailable { app: String },
}

impl PlanError {
    /// The app this refusal is about, as the launch named it, when it names one: what a shell
    /// showing the refusal to a person says in that app's own name instead.
    #[must_use]
    pub fn app(&self) -> Option<&str> {
        match self {
            Self::NotInstalled(app)
            | Self::Grants { app, .. }
            | Self::NotARecord { app, .. }
            | Self::ContentUnavailable { app } => Some(app),
            Self::Host(_) | Self::NothingToStart(_) | Self::NoLauncher | Self::Refused(_) => None,
        }
    }
}

/// A launch as the process it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// The app, as a tab, a crash notice or a log line names it: a package
    /// id or a desktop-file id.
    pub app: String,
    /// The name the app is shown by.
    pub title: String,
    pub kind: Kind,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The child's whole environment: it is spawned with `env_clear()` and
    /// exactly this.
    pub env: BTreeMap<String, String>,
    /// Every string the app's windows may announce themselves by.
    pub window_identities: Vec<String>,
    /// The ticket of the device's launch this plan executes, when the device's daemon decided it
    /// ([`Planned::ordered_package`]): the process is reported to the daemon under it before it
    /// may go on, and every connection of it is admitted by it.
    pub ticket: Option<u64>,
}

/// A package's launch as the device's daemon ordered it: the daemon decided it and built what it
/// starts from the package's signed manifest; the session adds what is the person's own (the
/// display, its services, its environment and the grants the person gave the app), as for any
/// launch it plans.
pub struct OrderedPackage<'a> {
    pub ticket: u64,
    pub app: &'a str,
    pub title: &'a str,
    pub manifest: &'a Path,
    pub payload_root: &'a Path,
    /// The hash of the signed package the daemon decided on: the launcher refuses any other.
    pub package_hash: &'a str,
    pub open: Option<&'a Open>,
    pub visitor: bool,
}

/// What a spare window's launch is named by, in the log and to the spawner: no app's.
pub const SPARE_WINDOW: &str = "tilolang-window";

/// Whether the session serves its services where a launch with `env` looks for them
/// (`$XDG_RUNTIME_DIR/tilo/services`).
fn serves_services(env: &BTreeMap<String, String>) -> bool {
    env.get("XDG_RUNTIME_DIR")
        .map(|runtime| Path::new(runtime).join("tilo").join("services"))
        .is_some_and(|directory| directory.exists())
}

/// What a plan is built from beyond the launch itself.
pub struct PlanContext<'a> {
    /// The session's displays, bus and the person's variables.
    pub environment: &'a SessionEnvironment,
    /// The workspace this launch belongs to.
    pub workspace: &'a Workspace,
    /// The `tilo-launcher` to start packages through.
    pub launcher: Option<PathBuf>,
    /// The identity record this workspace's apps are given, when the session
    /// resolved one.
    pub identity: Option<PathBuf>,
    /// The DRM render node the compositor renders on, so an app renders on
    /// the same GPU.
    pub render_node: Option<PathBuf>,
    /// What the person gave the app a choice on in this workspace: workspace file nodes it
    /// holds a grant on, and the files it chose outside one, each bound at the reference the app
    /// was answered with.
    pub granted: &'a [launcher::run::GrantedPath],
    /// A window of the app is already showing and it opens another on
    /// request ([`crate::Answer::Start`]): ask for the new one the way its
    /// entry declares, through its New Window action where it has one.
    pub new_window: bool,
    /// The tab the window takes, when the shell holds one for it: the app is
    /// told it as its window's instance, so what it reports about the window
    /// is about that tab.
    pub continuity_instance: Option<u64>,
    /// The app's origin this launch is its connector for (`imap`, `youtube`),
    /// rather than a window (`docs/architecture/apps.md` §1): the launcher
    /// gives the process the sandbox the app's manifest declares for that
    /// origin — the network a window and the tilolang host never have — and
    /// the app's runtime runs that origin's connector there. A connector draws
    /// nothing, so the plan has no display, no render node and nothing open,
    /// which the launcher refuses beside `--tilolang-connector` anyway.
    pub connector: Option<&'a str>,
}

impl PlanContext<'_> {
    /// The launcher installed here, if any: beside this executable first,
    /// as a build tree has it, then where the image installs it.
    #[must_use]
    pub fn find_launcher() -> Option<PathBuf> {
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(LAUNCHER_NAME)));
        beside
            .into_iter()
            .chain([
                PathBuf::from(INSTALLED_LAUNCHER),
                PathBuf::from(DISTRIBUTION_LAUNCHER),
            ])
            .find(|path| path.is_file())
    }
}

/// An installed package, as its launch names it. The store lookup is what
/// [`Planned::package`] adds to this.
#[derive(Clone, Copy)]
struct Package<'a> {
    id: &'a str,
    title: &'a str,
    manifest: &'a Path,
    payload_root: &'a Path,
}

/// Why `launch` starts no process, before anyone is asked to start one: a window already on its
/// way, a notice of an app that stopped, content no plan opens here, or a record id that names no
/// record. `None` for a launch the device's daemon decides.
#[must_use]
pub fn starts_nothing(launch: &Launch) -> Option<PlanError> {
    match launch {
        Launch::Package { .. } | Launch::Host { .. } => None,
        Launch::Record { app, record, .. } => {
            crate::routing::record(record)
                .is_none()
                .then(|| PlanError::NotARecord {
                    app: app.clone(),
                    record: record.clone(),
                })
        }
        Launch::Content { app, .. } => Some(PlanError::ContentUnavailable { app: app.clone() }),
        Launch::Window { id } => Some(PlanError::NothingToStart(format!("window {id}"))),
        Launch::Crashed { app } => Some(PlanError::NothingToStart(app.clone())),
    }
}

impl Planned {
    /// The process `launch` starts in a session no daemon decides its launches for (one that runs
    /// with no link to it), from the apps installed here, or why it starts none.
    ///
    /// # Errors
    /// The app is not installed here, its entry refuses to run, its grants cannot be written, or
    /// the launch is one that starts nothing ([`starts_nothing`]).
    pub fn of(launch: &Launch, context: &PlanContext<'_>) -> Result<Self, PlanError> {
        if let Some(refused) = starts_nothing(launch) {
            return Err(refused);
        }
        match launch {
            Launch::Package { app, open } => Self::package(app, open.as_ref(), false, context),
            // The record is opened by the app the launch names: the launch asks it for that
            // record, and the app opens on it.
            Launch::Record {
                app,
                record,
                visitor,
            } => {
                let open = crate::routing::record(record);
                Self::package(app, open.as_ref(), *visitor, context)
            }
            Launch::Host { desktop_id, open } => Self::host(desktop_id, open.as_ref(), context),
            Launch::Content { app, .. } | Launch::Crashed { app } => {
                Err(PlanError::NothingToStart(app.clone()))
            }
            Launch::Window { id } => Err(PlanError::NothingToStart(format!("window {id}"))),
        }
    }

    /// The installed Tilo package `app`, starting through the launcher and opening `open`.
    fn package(
        app: &str,
        open: Option<&Open>,
        visitor: bool,
        context: &PlanContext<'_>,
    ) -> Result<Self, PlanError> {
        let launcher = context.launcher.clone().ok_or(PlanError::NoLauncher)?;
        let installed = app_catalogue::apps::read_one_from_env(app)
            .ok()
            .flatten()
            .ok_or_else(|| PlanError::NotInstalled(app.to_owned()))?;
        let package = Package {
            id: installed.app_id.as_str(),
            title: &installed.display_name,
            manifest: &installed.manifest_path,
            payload_root: &installed.payload_root,
        };
        Self::planned_package(package, launcher, open, visitor, context)
    }

    /// The process the device's daemon ordered for `ordered`: the launcher's command line for the
    /// exact package the daemon decided on, with the grants the person gave it, in this session's
    /// display and environment.
    ///
    /// # Errors
    /// No launcher is installed, or the grants cannot be written.
    pub fn ordered_package(
        ordered: &OrderedPackage<'_>,
        context: &PlanContext<'_>,
    ) -> Result<Self, PlanError> {
        let launcher = context.launcher.clone().ok_or(PlanError::NoLauncher)?;
        let package = Package {
            id: ordered.app,
            title: ordered.title,
            manifest: ordered.manifest,
            payload_root: ordered.payload_root,
        };
        let mut planned =
            Self::planned_package(package, launcher, ordered.open, ordered.visitor, context)?;
        planned.args.push("--expected-package-hash".to_owned());
        planned.args.push(ordered.package_hash.to_owned());
        planned.ticket = Some(ordered.ticket);
        Ok(planned)
    }

    /// The launcher's command line and environment for `package`.
    fn planned_package(
        package: Package<'_>,
        launcher: PathBuf,
        open: Option<&Open>,
        visitor: bool,
        context: &PlanContext<'_>,
    ) -> Result<Self, PlanError> {
        let capabilities = context
            .workspace
            .write_grants(package.id)
            .map_err(|source| PlanError::Grants {
                app: package.id.to_owned(),
                source,
            })?;
        // What the person gave this app a choice on: bound into its sandbox at each reference,
        // and nothing another app was given.
        let granted = launcher::capabilities::write_granted_file(
            &context.workspace.directory,
            &context.workspace.id,
            package.id,
            context.granted,
        )
        .map_err(|source| PlanError::Grants {
            app: package.id.to_owned(),
            source,
        })?;
        // What the person may do in this app here is the device's to decide
        // when the action reaches it, so the app is handed no policy to decide
        // with.
        let mut args = vec![
            "run".to_owned(),
            "--manifest".to_owned(),
            package.manifest.display().to_string(),
            "--payload-root".to_owned(),
            package.payload_root.display().to_string(),
        ];
        if context.workspace.is_workspace() {
            args.push("--workspace-id".to_owned());
            args.push(context.workspace.id.clone());
        }
        // Always passed, so the launcher checks every capability against
        // this id rather than trusting the file to already be filtered.
        args.push("--app-id".to_owned());
        args.push(package.id.to_owned());
        // What a window has and a connector has not: a connector is the app's origin half, which
        // draws nothing, opens nothing and reads no record (`docs/architecture/apps.md` §1), and
        // the launcher refuses a display or an open beside `--tilolang-connector`.
        let window = context.connector.is_none();
        if window && let Some(socket) = &context.environment.wayland_socket {
            args.push("--wayland-socket".to_owned());
            args.push(socket.display().to_string());
        }
        if let Some(path) = &capabilities {
            args.push("--capabilities".to_owned());
            args.push(path.display().to_string());
        }
        if let Some(path) = &granted {
            args.push("--grants".to_owned());
            args.push(path.display().to_string());
        }
        if let Some(path) = &context.identity {
            args.push("--identity".to_owned());
            args.push(path.display().to_string());
        }
        if window && let Some(node) = &context.render_node {
            args.push("--render-node".to_owned());
            args.push(node.display().to_string());
        }
        if window {
            args.extend(Self::open_args(open, visitor));
        }
        if let Some(origin) = context.connector {
            // The app's connector for `origin`, not a window: the process runs the origin's
            // connector in the sandbox the manifest declares for it
            // (`launcher::run::LaunchOptions::tilolang_connector`).
            args.push("--tilolang-connector".to_owned());
            args.push(origin.to_owned());
        }
        let mut env = if window {
            context.environment.variables()
        } else {
            context.environment.windowless_variables()
        };
        // The person's language, as they chose it in Settings now: the app reads its words, and
        // its window's chrome, in it from its first frame.
        crate::env::speak(
            &mut env,
            tilo_ui_base::i18n::preferred_language().as_deref(),
        );
        if let Some(instance) = context.continuity_instance.filter(|_| window) {
            // The window's instance: the id of the tab it opens in, which
            // its continuation reports are addressed by.
            env.insert("TILO_CONTINUITY_INSTANCE".to_owned(), instance.to_string());
        }
        // The launcher resolves a package's services against
        // `$XDG_RUNTIME_DIR/tilo/services`. A session that serves none asks
        // for a launch without them, so a package needing one warns instead
        // of failing to start.
        if !serves_services(&env) {
            args.push("--no-services".to_owned());
        }
        Ok(Self {
            app: package.id.to_owned(),
            title: package.title.to_owned(),
            kind: Kind::Package,
            program: launcher,
            args,
            env,
            window_identities: package_identities(package.id),
            ticket: None,
        })
    }

    /// What a window opens: the file at its path, or the URI, browsing as a visitor when it is
    /// opened for whoever works the record rather than for the person.
    fn open_args(open: Option<&Open>, visitor: bool) -> Vec<String> {
        let mut args = Vec::new();
        match open {
            Some(Open::File(path)) => {
                args.push("--open".to_owned());
                args.push(path.display().to_string());
            }
            Some(Open::Uri(uri)) => {
                args.push("--open-uri".to_owned());
                args.push(uri.clone());
                if visitor {
                    // A window opened for whoever works the record rather than
                    // for the person: it browses as a visitor, with none of
                    // their sign-ins.
                    args.push("--open-as-visitor".to_owned());
                }
            }
            None => {}
        }
        args
    }

    /// A spare tilolang window in `workspace`, on the tilolang runtime and the Tilo runtime
    /// `runtimes` name: a window process of no app yet, which the session binds to the app whose
    /// window is asked for (`launcher::run::tilolang_window`).
    ///
    /// # Errors
    /// There is no launcher.
    pub fn spare_window(
        workspace: &str,
        (runtime, app_runtime): (&str, &str),
        context: &PlanContext<'_>,
    ) -> Result<Self, PlanError> {
        let launcher = context.launcher.clone().ok_or(PlanError::NoLauncher)?;
        let mut env = context.environment.variables();
        crate::env::speak(
            &mut env,
            tilo_ui_base::i18n::preferred_language().as_deref(),
        );
        let mut args = [
            launcher::run::tilolang_window::SUBCOMMAND,
            "--runtime",
            runtime,
            "--app-runtime",
            app_runtime,
            "--workspace-id",
            workspace,
        ]
        .map(str::to_owned)
        .to_vec();
        if !serves_services(&env) {
            args.push("--no-services".to_owned());
        }
        Ok(Self {
            app: SPARE_WINDOW.to_owned(),
            title: String::new(),
            kind: Kind::Package,
            program: launcher,
            args,
            env,
            window_identities: Vec::new(),
            // A spare is no app's launch yet: the window that binds it is the app's.
            ticket: None,
        })
    }

    /// One of the machine's own applications, from its desktop entry.
    ///
    /// # Errors
    /// See [`Self::of`].
    pub fn host(
        desktop_id: &str,
        open: Option<&Open>,
        context: &PlanContext<'_>,
    ) -> Result<Self, PlanError> {
        let scan = desktop_entry::cache::cached_scan(&desktop_entry::locale::language_candidates(
            tilo_ui_base::i18n::preferred_language().as_deref(),
        ));
        let entry = scan.find(desktop_id).ok_or_else(|| {
            PlanError::Host(desktop_entry::launch::HostLaunchError::NotFound(
                desktop_id.to_owned(),
            ))
        })?;
        let launch_env = opening(
            context.environment.host_launch_env(),
            open,
            context.new_window,
        );
        // Planned here as well as in the launcher: this is where an entry
        // that cannot run says so before anything is started, and what its
        // window will announce itself by.
        let planned = desktop_entry::launch::plan(entry, &launch_env)?;
        Ok(run_host(
            planned,
            None,
            &launch_env,
            context.launcher.clone().ok_or(PlanError::NoLauncher)?,
            Kind::of_entry(entry),
        ))
    }

    /// The program file at `path` itself, as opening a downloaded `AppImage`
    /// or an executable in Files does. The file is made runnable first: a
    /// downloaded program arrives without the executable bit.
    ///
    /// # Errors
    /// See [`Self::of`], and the file is not a program that runs by itself.
    pub fn program(path: &Path, context: &PlanContext<'_>) -> Result<Self, PlanError> {
        desktop_entry::launch::make_runnable(path)?;
        let launch_env = context.environment.host_launch_env();
        let planned = desktop_entry::launch::plan_program(path, &launch_env)?;
        let kind = match path.extension().and_then(std::ffi::OsStr::to_str) {
            Some("AppImage") => Kind::AppImage,
            _ => Kind::Program,
        };
        Ok(run_host(
            planned,
            Some(path),
            &launch_env,
            context.launcher.clone().ok_or(PlanError::NoLauncher)?,
            kind,
        ))
    }

    /// Whether this launch starts behind an app keeper, so its windows stay open while the desktop
    /// restarts in place ([`crate::kept`]): every app whose windows the Tilo runtime does not draw,
    /// which is every app that is not a Tilo package. A package's windows are reopened instead.
    #[must_use]
    pub fn kept(&self) -> bool {
        self.kind.is_host()
    }

    /// One line saying what this launch starts, for a log before it starts.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut line = self.program.display().to_string();
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        line
    }
}

/// A host app's launch environment with what it opens and whether a window
/// of it is already showing: a document reaches the entry's `%f`/`%F` as a
/// path, a link its `%u`/`%U` as it is, and an app that opens another window
/// on request is asked for it through its own New Window action.
fn opening(mut env: LaunchEnv, open: Option<&Open>, new_window: bool) -> LaunchEnv {
    env.prefer_new_window = new_window;
    match open {
        Some(Open::File(path)) => env.files.push(path.display().to_string()),
        Some(Open::Uri(uri)) => env.urls.push(uri.clone()),
        None => {}
    }
    env
}

/// A host launch run through the launcher: the program is `launcher`, and
/// the app it starts is the one `planned` resolved. `program` is the file
/// itself for a launch with no desktop entry.
fn run_host(
    planned: HostLaunch,
    program: Option<&Path>,
    env: &LaunchEnv,
    launcher: PathBuf,
    kind: Kind,
) -> Planned {
    let mut args = vec![
        "machine-app".to_owned(),
        "--desktop-id".to_owned(),
        planned.desktop_id.clone(),
    ];
    if let Some(program) = program {
        args.push("--program".to_owned());
        args.push(program.display().to_string());
    }
    if let Some(socket) = &env.wayland_socket {
        args.push("--wayland-socket".to_owned());
        args.push(socket.display().to_string());
    }
    if let Some(display) = &env.x11_display {
        args.push("--x11-display".to_owned());
        args.push(display.clone());
    }
    for file in &env.files {
        args.push("--open".to_owned());
        args.push(file.clone());
    }
    for uri in &env.urls {
        args.push("--open-url".to_owned());
        args.push(uri.clone());
    }
    if env.prefer_new_window {
        args.push("--new-window".to_owned());
    }
    Planned {
        app: planned.desktop_id.clone(),
        title: planned.display_name.clone(),
        kind,
        program: launcher,
        args,
        // The launcher builds the app's environment from the same allowlist
        // of the session's own: a host app is told nothing of Tilo's.
        env: desktop_entry::launch::inherited_environment(),
        window_identities: planned.window_identities,
        ticket: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAUNCHER: &str = "/usr/libexec/tilo/tilo-launcher";

    fn launcher() -> PathBuf {
        PathBuf::from(LAUNCHER)
    }

    fn planning<'a>(
        environment: &'a SessionEnvironment,
        workspace: &'a Workspace,
    ) -> PlanContext<'a> {
        PlanContext {
            environment,
            workspace,
            launcher: Some(launcher()),
            identity: None,
            render_node: None,
            granted: &[],
            new_window: false,
            continuity_instance: None,
            connector: None,
        }
    }

    fn displays() -> SessionEnvironment {
        SessionEnvironment {
            wayland_socket: Some(PathBuf::from("/run/user/1000/wayland-4")),
            x11_display: Some(":11".into()),
            session_bus: Some("unix:path=/run/user/1000/tilo/bus".into()),
            forwarded: BTreeMap::from([(
                "TILO_IDENTITY_ROOT".to_owned(),
                "/home/you/.local/share/tilo/identity".to_owned(),
            )]),
        }
    }

    fn notes() -> Package<'static> {
        Package {
            id: "so.tilo.notes",
            title: "Notes",
            manifest: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/manifest.toml"),
            payload_root: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/payload"),
        }
    }

    fn mail() -> Package<'static> {
        Package {
            id: "so.tilo.mail",
            title: "Mail",
            manifest: Path::new("/home/you/.local/share/tilo/apps/so.tilo.mail/m.toml"),
            payload_root: Path::new("/home/you/.local/share/tilo/apps/so.tilo.mail/p"),
        }
    }

    fn spotify() -> desktop_entry::DesktopEntry {
        desktop_entry::entry::parse(
            "[Desktop Entry]\nType=Application\nName=Spotify\nIcon=com.spotify.Client\n\
             Exec=/usr/bin/flatpak run --file-forwarding com.spotify.Client @@u %U @@\n\
             StartupWMClass=spotify\nX-Flatpak=com.spotify.Client\n",
            "com.spotify.Client",
            Path::new("/var/lib/flatpak/exports/share/applications/com.spotify.Client.desktop"),
            &[],
        )
        .expect("parses")
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one flow through planning every kind of launch, which is what it proves"
    )]
    fn a_launch_is_planned_as_the_process_the_launcher_starts_or_says_why_it_starts_none() {
        let directory = tempfile::tempdir().expect("a directory");
        let workspace = Workspace::under(directory.path(), "workspace-9");
        let environment = displays();
        let context = planning(&environment, &workspace);

        // A package launches through the launcher with its workspace and the session's displays.
        let planned = Planned::planned_package(
            notes(),
            launcher(),
            Some(&Open::File("/home/you/notes.tilo-note".into())),
            false,
            &context,
        )
        .expect("a plan");
        assert_eq!(planned.kind, Kind::Package);
        assert!(!planned.kind.is_host());
        assert_eq!(planned.app, "so.tilo.notes");
        assert_eq!(planned.title, "Notes");
        assert_eq!(
            planned.args,
            [
                "run",
                "--manifest",
                "/home/you/.local/share/tilo/apps/so.tilo.notes/manifest.toml",
                "--payload-root",
                "/home/you/.local/share/tilo/apps/so.tilo.notes/payload",
                "--workspace-id",
                "workspace-9",
                "--app-id",
                "so.tilo.notes",
                "--wayland-socket",
                "/run/user/1000/wayland-4",
                "--open",
                "/home/you/notes.tilo-note",
                "--no-services",
            ]
        );
        assert_eq!(
            planned.env.get("TILO_IDENTITY_ROOT").map(String::as_str),
            Some("/home/you/.local/share/tilo/identity"),
            "the person's identity root reaches the app through its launcher"
        );
        assert_eq!(
            planned.env.get("WAYLAND_DISPLAY").map(String::as_str),
            Some("wayland-4")
        );
        assert_eq!(
            planned.env.get("DISPLAY").map(String::as_str),
            Some(":11"),
            "an X11 toolkit finds the session's Xwayland"
        );
        assert_eq!(
            planned.window_identities,
            ["so.tilo.notes", "notes"],
            "a toolkit's own app_id is the trailing component"
        );

        // A connector is the app's origin half, not a window: its launch carries the origin and
        // nothing a window has, so it gets no display and nothing open.
        let socket = directory.path().join("wayland-4");
        let connector_environment = SessionEnvironment {
            wayland_socket: Some(socket.clone()),
            x11_display: Some(":11".into()),
            session_bus: Some("unix:path=/run/user/1000/tilo/bus".into()),
            forwarded: BTreeMap::from([(
                "TILO_IDENTITY_ROOT".to_owned(),
                "/home/you/.local/share/tilo/identity".to_owned(),
            )]),
        };
        let connector = PlanContext {
            connector: Some("imap"),
            render_node: Some(PathBuf::from("/dev/dri/renderD128")),
            ..planning(&connector_environment, &workspace)
        };
        let planned = Planned::planned_package(
            mail(),
            launcher(),
            // Nothing a connector names to open is handed on either.
            Some(&Open::File("/home/you/mail.tilo-mail".into())),
            false,
            &connector,
        )
        .expect("a plan");
        assert_eq!(
            planned.args,
            [
                "run",
                "--manifest",
                "/home/you/.local/share/tilo/apps/so.tilo.mail/m.toml",
                "--payload-root",
                "/home/you/.local/share/tilo/apps/so.tilo.mail/p",
                "--workspace-id",
                "workspace-9",
                "--app-id",
                "so.tilo.mail",
                "--tilolang-connector",
                "imap",
                "--no-services",
            ],
            "the launcher's connector launch, and none of a window's flags"
        );
        assert_eq!(
            planned.env.get("XDG_RUNTIME_DIR").map(String::as_str),
            socket.parent().and_then(Path::to_str),
            "the connector runs in this session's runtime directory"
        );
        for display in ["WAYLAND_DISPLAY", "DISPLAY"] {
            assert!(
                !planned.env.contains_key(display),
                "a connector reaches no compositor: {display}"
            );
        }

        // A package launch is the daemon's to decide, and a window on its way and a crash notice
        // are not processes.
        let launched = Launch::parse("so.tilo.notes").expect("a package launch");
        assert!(starts_nothing(&launched).is_none());
        // A session no daemon decides for starts it from the apps installed here, or says why not.
        let mut without_launcher = planning(&environment, &workspace);
        without_launcher.launcher = None;
        assert!(matches!(
            Planned::of(&launched, &without_launcher),
            Err(PlanError::NoLauncher)
        ));
        for launch in ["window:12", "crashed:so.tilo.canvas"] {
            let launch = Launch::parse(launch).expect("a launch");
            assert!(matches!(
                starts_nothing(&launch),
                Some(PlanError::NothingToStart(_))
            ));
        }

        // A record launch opens the app on that record, and a window opened for whoever works
        // it browses as a visitor.
        let record = collab_types::document::DocumentId::for_file("/home/you/Notes.note");
        let launch = Launch::parse(&format!("record:so.tilo.notes:{}", record.as_str()))
            .expect("a record launch");
        let open = match launch {
            Launch::Record { record, .. } => crate::routing::record(&record),
            other => panic!("a record launch: {other:?}"),
        };
        let planned = Planned::planned_package(notes(), launcher(), open.as_ref(), false, &context)
            .expect("a plan");
        assert!(
            planned
                .args
                .windows(2)
                .any(|pair| pair[0] == "--open-uri" && pair[1] == record.uri()),
            "the app is asked for the record itself: {:?}",
            planned.args
        );
        assert!(
            !planned.args.iter().any(|arg| arg == "--open-as-visitor"),
            "the person's own window browses with their sign-ins: {:?}",
            planned.args
        );
        let visitor = Planned::planned_package(notes(), launcher(), open.as_ref(), true, &context)
            .expect("a plan");
        assert!(
            visitor.args.iter().any(|arg| arg == "--open-as-visitor"),
            "a window for an agent browses as a visitor: {:?}",
            visitor.args
        );
        // A record the launch string misspells is refused before any app is started, rather
        // than starting one that would show nothing.
        let launch = Launch::parse("record:so.tilo.notes:not a record").expect("a launch");
        assert!(matches!(
            starts_nothing(&launch),
            Some(PlanError::NotARecord { .. })
        ));
        // A record it names correctly goes on to the daemon, which decides it.
        let launch =
            Launch::parse(&format!("record:so.tilo.notes:{}", record.as_str())).expect("a launch");
        assert!(starts_nothing(&launch).is_none());
        let launch = Launch::parse(&format!("content:so.tilo.notes:{}", "0".repeat(64)))
            .expect("a content launch");
        assert!(matches!(
            starts_nothing(&launch),
            Some(PlanError::ContentUnavailable { .. })
        ));

        // A host app runs through the launcher with the document it opens, and is told nothing
        // of Tilo's authority.
        let entry = spotify();
        let launch_env = opening(
            displays().host_launch_env(),
            Some(&Open::File("/home/you/track.flac".into())),
            false,
        );
        let planned = desktop_entry::launch::plan(&entry, &launch_env).expect("plans");
        let planned = run_host(
            planned,
            None,
            &launch_env,
            launcher(),
            Kind::of_entry(&entry),
        );
        assert_eq!(planned.kind, Kind::Flatpak);
        assert!(planned.kind.is_host());
        assert_eq!(
            planned.program,
            launcher(),
            "the launcher starts it, so it is born in a scope of its own"
        );
        assert_eq!(
            planned.args,
            [
                "machine-app",
                "--desktop-id",
                "com.spotify.Client",
                "--wayland-socket",
                "/run/user/1000/wayland-4",
                "--x11-display",
                ":11",
                "--open",
                "/home/you/track.flac",
            ],
            "the entry's command line and the document are the launcher's to resolve"
        );
        assert!(
            !planned.env.contains_key("TILO_IDENTITY_ROOT")
                && !planned.env.keys().any(|key| key.starts_with("TILO_")),
            "a host app is not confined, so it is told nothing of Tilo's authority"
        );
        assert_eq!(planned.window_identities, ["spotify", "com.spotify.Client"]);

        // An app already showing is asked for a new window through the action its entry
        // declares.
        let editor = desktop_entry::entry::parse(
            "[Desktop Entry]\nType=Application\nName=Editor\nExec=/usr/bin/editor %F\n\
             Actions=new-window;\n\n[Desktop Action new-window]\nName=New Window\n\
             Exec=/usr/bin/editor --new-window %F\n",
            "org.example.Editor.desktop",
            Path::new("/usr/share/applications/org.example.Editor.desktop"),
            &[],
        )
        .expect("parses");
        let opened = |new_window| {
            let launch_env = opening(displays().host_launch_env(), None, new_window);
            let planned = desktop_entry::launch::plan(&editor, &launch_env).expect("plans");
            run_host(
                planned,
                None,
                &launch_env,
                launcher(),
                Kind::of_entry(&editor),
            )
            .args
        };
        assert!(
            !opened(false).contains(&"--new-window".to_string()),
            "a cold launch runs the entry's own command line"
        );
        assert!(
            opened(true).contains(&"--new-window".to_string()),
            "one already showing gets another through the action the entry declares"
        );

        // An entry that cannot run says why instead of starting something else.
        let terminal = desktop_entry::entry::parse(
            "[Desktop Entry]\nType=Application\nName=htop\nExec=/usr/bin/htop\nTerminal=true\n",
            "htop",
            Path::new("/usr/share/applications/htop.desktop"),
            &[],
        )
        .expect("parses");
        let refused = desktop_entry::launch::plan(&terminal, &LaunchEnv::default());
        assert!(matches!(
            refused,
            Err(desktop_entry::launch::HostLaunchError::NeedsTerminal { .. })
        ));
        let link = desktop_entry::entry::parse(
            "[Desktop Entry]\nType=Link\nName=Site\nURL=https://example.com\n",
            "site",
            Path::new("/usr/share/applications/site.desktop"),
            &[],
        )
        .expect("parses");
        assert!(matches!(
            desktop_entry::launch::plan(&link, &LaunchEnv::default()),
            Err(desktop_entry::launch::HostLaunchError::NotAnApplication { .. })
        ));
    }
}
