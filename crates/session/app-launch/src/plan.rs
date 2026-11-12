//! Turning a launch into the process it starts.
//!
//! Two very different things are started here, and the plan says which:
//!
//! - a Tilo package, through `tilo-launcher run`, which builds the package's
//!   sandbox from its signed manifest, the workspace it is launched into and the
//!   grants written for it ([`Workspace::write_grants`]);
//! - one of the machine's own applications, through `tilo-launcher run-host`
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

use crate::authority::Authority;
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
    /// The person's authority in the workspace could not be published, so the
    /// app would start as a viewer of a workspace they may change: refuse
    /// rather than launch it as someone they are not.
    #[error("the person's authority for {app} could not be published: {source}")]
    Authority {
        app: String,
        #[source]
        source: std::io::Error,
    },
    /// No Tilo launcher is installed, and every app starts through it.
    #[error("no tilo-launcher is installed, so no app can start here")]
    NoLauncher,
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
    /// What publishes the person's authority in the workspace, for the app
    /// this launch starts: what the launcher binds into it so it decides the
    /// person's actions as the workspace's policy says ([`crate::authority`]).
    pub authority: Option<&'a Authority>,
    /// The DRM render node the compositor renders on, so an app renders on
    /// the same GPU.
    pub render_node: Option<PathBuf>,
    /// A window of the app is already showing and it opens another on
    /// request ([`crate::Answer::Start`]): ask for the new one the way its
    /// entry declares, through its New Window action where it has one.
    pub new_window: bool,
    /// The tab the window takes, when the desk holds one for it: the app is
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
    /// The resource address the session's document host publishes for this
    /// app, when it serves the app's model: whether the app launches hosting
    /// follows from that, and no per-app code says otherwise
    /// (`docs/architecture/apps.md` §8 step 2, `one-app-model.md` §4).
    pub hosted: Option<&'a str>,
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

impl Planned {
    /// The process `launch` starts, or why it starts none.
    ///
    /// # Errors
    /// The app is not installed here, its entry refuses to run, its grants
    /// cannot be written, or the launch is one that starts nothing.
    pub fn of(launch: &Launch, context: &PlanContext<'_>) -> Result<Self, PlanError> {
        match launch {
            Launch::Package { app, open } => Self::package(app, open.as_ref(), false, context),
            // The record is opened by the app the launch names: the launch
            // asks it for that record, and the app opens on it.
            Launch::Record {
                app,
                record,
                visitor,
            } => {
                let open = crate::routing::record(record).ok_or_else(|| PlanError::NotARecord {
                    app: app.clone(),
                    record: record.clone(),
                })?;
                Self::package(app, Some(&open), *visitor, context)
            }
            Launch::Content { app, .. } => Err(PlanError::ContentUnavailable { app: app.clone() }),
            Launch::Host { desktop_id, open } => Self::host(desktop_id, open.as_ref(), context),
            Launch::Window { id } => Err(PlanError::NothingToStart(format!("window {id}"))),
            Launch::Crashed { app } => Err(PlanError::NothingToStart(app.clone())),
        }
    }

    /// The installed Tilo package `app`, starting through the launcher and
    /// opening `open`.
    ///
    /// # Errors
    /// See [`Self::of`].
    pub fn package(
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
            id: &installed.package_id,
            title: &installed.display_name,
            manifest: &installed.manifest_path,
            payload_root: &installed.payload_root,
        };
        Self::planned_package(package, launcher, open, visitor, context)
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
        // What the person may do in this app here, published before the app
        // starts: it decides every action against it from its first frame.
        let authority = context
            .authority
            .map(|authority| authority.publish(context.workspace, package.id))
            .transpose()
            .map_err(|source| PlanError::Authority {
                app: package.id.to_owned(),
                source,
            })?
            .flatten();
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
        if let Some(path) = &authority {
            args.push("--action-authority".to_owned());
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
            match open {
                Some(Open::File(path)) => {
                    args.push("--open".to_owned());
                    args.push(path.display().to_string());
                }
                Some(Open::Uri(uri)) => {
                    args.push("--open-uri".to_owned());
                    args.push(uri.clone());
                    if visitor {
                        // A window opened for whoever works the record rather
                        // than for the person: it browses as a visitor, with none
                        // of their sign-ins.
                        args.push("--open-as-visitor".to_owned());
                    }
                }
                None => {}
            }
        }
        if let Some(origin) = context.connector {
            // The app's connector for `origin`, not a window: the process runs the origin's
            // connector in the sandbox the manifest declares for it
            // (`launcher::run::RunOptions::tilolang_connector`).
            args.push("--tilolang-connector".to_owned());
            args.push(origin.to_owned());
        }
        let mut env = if window {
            context.environment.variables()
        } else {
            context.environment.windowless_variables()
        };
        if let Some(instance) = context.continuity_instance.filter(|_| window) {
            // The window's instance: the id of the tab it opens in, which
            // its continuation reports are addressed by.
            env.insert("TILO_CONTINUITY_INSTANCE".to_owned(), instance.to_string());
        }
        if let Some(hosted) = context.hosted.filter(|_| window) {
            // The session's document host keeps this app's record: its
            // process opens none, and reads the reading published here over
            // the connection the launcher made it (`one-app-model.md` §4).
            env.insert("TILO_HOSTED_RESOURCE".to_owned(), hosted.to_owned());
        }
        // The launcher resolves a package's services against
        // `$XDG_RUNTIME_DIR/tilo/services`. A session that serves none asks
        // for a launch without them, so a package needing one warns instead
        // of failing to start.
        let serves_services = env
            .get("XDG_RUNTIME_DIR")
            .map(|runtime| Path::new(runtime).join("tilo").join("services"))
            .is_some_and(|directory| directory.exists());
        if !serves_services {
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
            ui::i18n::preferred_language().as_deref(),
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
        "run-host".to_owned(),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(
        environment: &'a SessionEnvironment,
        workspace: &'a Workspace,
    ) -> PlanContext<'a> {
        PlanContext {
            environment,
            workspace,
            launcher: Some(PathBuf::from("/usr/libexec/tilo/tilo-launcher")),
            identity: None,
            authority: None,
            render_node: None,
            new_window: false,
            continuity_instance: None,
            connector: None,
            hosted: None,
        }
    }

    fn displays() -> SessionEnvironment {
        SessionEnvironment {
            wayland_socket: Some(PathBuf::from("/run/user/1000/wayland-4")),
            x11_display: Some(":11".into()),
            session_bus: Some("unix:path=/run/user/1000/tilo/bus".into()),
            forwarded: BTreeMap::from([("TILO_USER_ID".to_owned(), "person-3".to_owned())]),
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
    fn a_package_launches_through_the_launcher_with_its_workspace_and_the_sessions_displays() {
        let directory = tempfile::tempdir().expect("a directory");
        let workspace = Workspace::under(directory.path(), "workspace-9");
        let environment = displays();
        let context = context(&environment, &workspace);
        let package = Package {
            id: "so.tilo.notes",
            title: "Notes",
            manifest: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/manifest.toml"),
            payload_root: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/payload"),
        };
        let planned = Planned::planned_package(
            package,
            PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
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
            planned.env.get("TILO_USER_ID").map(String::as_str),
            Some("person-3"),
            "the person reaches the app through its launcher"
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
    }

    #[test]
    fn a_package_is_handed_the_persons_authority_in_its_workspace() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut workspace = Workspace::under(directory.path(), "workspace-9");
        workspace.policy = Some(tilo_action_policy::WorkspacePolicy::new(
            "workspace-9",
            "person-3",
        ));
        workspace.person = "person-3".to_owned();
        let environment = displays();
        let authority = Authority::new();
        let handed = PlanContext {
            authority: Some(&authority),
            ..context(&environment, &workspace)
        };
        let package = Package {
            id: "so.tilo.notes",
            title: "Notes",
            manifest: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/manifest.toml"),
            payload_root: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/payload"),
        };
        let planned = Planned::planned_package(
            package,
            PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
            None,
            false,
            &handed,
        )
        .expect("a plan");
        let at = planned
            .args
            .iter()
            .position(|arg| arg == "--action-authority")
            .expect("the launcher is told the person's authority");
        let file = Path::new(&planned.args[at + 1]);
        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some("current.json"),
            "the file the launcher binds"
        );
        assert!(
            tilo_files::native::held::read(file, 64 * 1024)
                .expect("readable")
                .is_some(),
            "published and held before the app starts"
        );
        // A workspace whose policy the session does not hold hands none: the
        // app takes the launcher's participation.
        let mut unsupported = workspace.clone();
        unsupported.policy = None;
        let blind = PlanContext {
            authority: Some(&authority),
            ..context(&environment, &unsupported)
        };
        assert!(
            !Planned::planned_package(
                package,
                PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
                None,
                false,
                &blind,
            )
            .expect("a plan")
            .args
            .iter()
            .any(|arg| arg == "--action-authority")
        );
    }

    /// A connector is the app's origin half, not a window (`docs/architecture/apps.md` §1): its
    /// launch carries the origin and nothing a window has. The launcher refuses a display or an
    /// open beside `--tilolang-connector`, so a plan that names both starts no connector at all
    /// and the app whose records it keeps shows nothing.
    #[test]
    fn a_connector_is_planned_without_a_display_or_anything_open() {
        let directory = tempfile::tempdir().expect("a directory");
        let workspace = Workspace::under(directory.path(), "workspace-9");
        let socket = directory.path().join("wayland-4");
        let environment = SessionEnvironment {
            wayland_socket: Some(socket.clone()),
            x11_display: Some(":11".into()),
            session_bus: Some("unix:path=/run/user/1000/tilo/bus".into()),
            forwarded: BTreeMap::from([("TILO_USER_ID".to_owned(), "person-3".to_owned())]),
        };
        let connector = PlanContext {
            connector: Some("imap"),
            render_node: Some(PathBuf::from("/dev/dri/renderD128")),
            ..context(&environment, &workspace)
        };
        let planned = Planned::planned_package(
            Package {
                id: "so.tilo.lang.mail",
                title: "Mail",
                manifest: Path::new("/home/you/.local/share/tilo/apps/so.tilo.lang.mail/m.toml"),
                payload_root: Path::new("/home/you/.local/share/tilo/apps/so.tilo.lang.mail/p"),
            },
            PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
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
                "/home/you/.local/share/tilo/apps/so.tilo.lang.mail/m.toml",
                "--payload-root",
                "/home/you/.local/share/tilo/apps/so.tilo.lang.mail/p",
                "--workspace-id",
                "workspace-9",
                "--app-id",
                "so.tilo.lang.mail",
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
    }

    #[test]
    fn a_launch_without_a_launcher_or_an_installed_package_starts_nothing() {
        let directory = tempfile::tempdir().expect("a directory");
        let workspace = Workspace::under(directory.path(), "workspace");
        let environment = displays();
        let launch = Launch::parse("so.tilo.notes").expect("a package launch");
        let mut context = context(&environment, &workspace);
        context.launcher = None;
        assert!(matches!(
            Planned::of(&launch, &context),
            Ok(_) | Err(PlanError::NoLauncher | PlanError::NotInstalled(_))
        ));
        // A window on its way and a crash notice are not processes.
        for launch in ["window:12", "crashed:so.tilo.canvas"] {
            let launch = Launch::parse(launch).expect("a launch");
            assert!(matches!(
                Planned::of(&launch, &context),
                Err(PlanError::NothingToStart(_))
            ));
        }
    }

    #[test]
    fn a_host_app_runs_through_the_launcher_with_the_document_it_opens() {
        let entry = spotify();
        let launch_env = opening(
            displays().host_launch_env(),
            Some(&Open::File("/home/you/track.flac".into())),
            false,
        );
        let planned = desktop_entry::launch::plan(&entry, &launch_env).expect("plans");
        let launcher = PathBuf::from("/usr/libexec/tilo/tilo-launcher");
        let planned = run_host(
            planned,
            None,
            &launch_env,
            launcher.clone(),
            Kind::of_entry(&entry),
        );
        assert_eq!(planned.kind, Kind::Flatpak);
        assert!(planned.kind.is_host());
        assert_eq!(
            planned.program, launcher,
            "the launcher starts it, so it is born in a scope of its own"
        );
        assert_eq!(
            planned.args,
            [
                "run-host",
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
            !planned.env.contains_key("TILO_USER_ID")
                && !planned.env.keys().any(|key| key.starts_with("TILO_")),
            "a host app is not confined, so it is told nothing of Tilo's authority"
        );
        assert_eq!(planned.window_identities, ["spotify", "com.spotify.Client"]);
    }

    #[test]
    fn a_record_launch_opens_the_app_on_that_record() {
        let directory = tempfile::tempdir().expect("a directory");
        let workspace = Workspace::under(directory.path(), "workspace-9");
        let environment = displays();
        let context = context(&environment, &workspace);
        let record = collaboration::record::RecordId::for_file("/home/you/Notes.note");
        let launch = Launch::parse(&format!("record:so.tilo.notes:{}", record.as_str()))
            .expect("a record launch");
        let open = match launch {
            Launch::Record { record, .. } => crate::routing::record(&record),
            other => panic!("a record launch: {other:?}"),
        };
        let package = Package {
            id: "so.tilo.notes",
            title: "Notes",
            manifest: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/manifest.toml"),
            payload_root: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/payload"),
        };
        let planned = Planned::planned_package(
            package,
            PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
            open.as_ref(),
            false,
            &context,
        )
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

        // A window opened for whoever works the record — an agent's — browses
        // as a visitor: the launcher hands the app the switch that says so.
        let visitor = Planned::planned_package(
            Package {
                id: "so.tilo.notes",
                title: "Notes",
                manifest: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/manifest.toml"),
                payload_root: Path::new("/home/you/.local/share/tilo/apps/so.tilo.notes/payload"),
            },
            PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
            open.as_ref(),
            true,
            &context,
        )
        .expect("a plan");
        assert!(
            visitor.args.iter().any(|arg| arg == "--open-as-visitor"),
            "a window for an agent browses as a visitor: {:?}",
            visitor.args
        );

        // A record the launch string misspells is refused before any app is
        // started, rather than starting one that would show nothing.
        let launch = Launch::parse("record:so.tilo.notes:not a record").expect("a launch");
        assert!(matches!(
            Planned::of(&launch, &context),
            Err(PlanError::NotARecord { .. })
        ));
        // A record it names correctly goes on to the app, and stops only
        // where every launch would: the app is not installed here.
        let launch =
            Launch::parse(&format!("record:so.tilo.notes:{}", record.as_str())).expect("a launch");
        assert!(matches!(
            Planned::of(&launch, &context),
            Err(PlanError::NoLauncher | PlanError::NotInstalled(_))
        ));
        let launch = Launch::parse(&format!("content:so.tilo.notes:{}", "0".repeat(64)))
            .expect("a content launch");
        assert!(matches!(
            Planned::of(&launch, &context),
            Err(PlanError::ContentUnavailable { .. })
        ));
    }

    #[test]
    fn an_app_already_showing_is_asked_for_a_new_window_through_the_launcher() {
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
                PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
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
    }

    #[test]
    fn an_entry_that_cannot_run_says_why_instead_of_starting_something_else() {
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
