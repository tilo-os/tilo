//! Turning a launch into the process it starts.
//!
//! Two very different things are started here, and the plan says which:
//!
//! - a Tilo package, through `tilo-launcher run`, which builds the package's
//!   sandbox from its signed manifest, the room it is launched into and the
//!   grants written for it ([`Room::write_grants`]);
//! - one of the machine's own applications, through its `.desktop` entry:
//!   Flatpak, `AppImage`, a program the person added, or one of the system's.
//!   It runs unconfined, as the person, exactly as [`desktop_entry::launch`]
//!   decides — see that module for why it cannot be confined.
//!
//! A plan is pure data: the program, its arguments and the child's whole
//! environment. Nothing here starts anything, so what a launch *would* run is
//! assertable without a process appearing — that is what the tests do, and
//! what a session logs before [`crate::Spawner`] starts it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use desktop_entry::launch::{HostLaunch, LaunchEnv};

use crate::env::SessionEnvironment;
use crate::launch::{Launch, Open};
use crate::room::Room;

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
    /// The room's grants could not be written, so the app would start with
    /// fewer permissions than the person granted: refuse rather than launch
    /// it half-granted.
    #[error("the grants for {app} could not be written: {source}")]
    Grants {
        app: String,
        #[source]
        source: std::io::Error,
    },
    /// No Tilo launcher is installed.
    #[error("no tilo-launcher is installed, so Tilo apps cannot start here")]
    NoLauncher,
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
    /// The room this launch belongs to.
    pub room: &'a Room,
    /// The `tilo-launcher` to start packages through.
    pub launcher: Option<PathBuf>,
    /// The identity record this workspace's apps are given, when the session
    /// resolved one.
    pub identity: Option<PathBuf>,
    /// The DRM render node the compositor renders on, so an app renders on
    /// the same GPU.
    pub render_node: Option<PathBuf>,
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
            Launch::Package { app, open } => Self::package(app, open.as_ref(), context),
            // Which record or content it opens is the workspace's business;
            // the app itself is what starts.
            Launch::Record { app, .. } | Launch::Content { app, .. } => {
                Self::package(app, None, context)
            }
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
        context: &PlanContext<'_>,
    ) -> Result<Self, PlanError> {
        let launcher = context.launcher.clone().ok_or(PlanError::NoLauncher)?;
        let installed = tilo_installed_apps::read_one_from_env(app)
            .ok()
            .flatten()
            .ok_or_else(|| PlanError::NotInstalled(app.to_owned()))?;
        let package = Package {
            id: &installed.package_id,
            title: &installed.display_name,
            manifest: &installed.manifest_path,
            payload_root: &installed.payload_root,
        };
        Self::planned_package(package, launcher, open, context)
    }

    /// The launcher's command line and environment for `package`.
    fn planned_package(
        package: Package<'_>,
        launcher: PathBuf,
        open: Option<&Open>,
        context: &PlanContext<'_>,
    ) -> Result<Self, PlanError> {
        let capabilities =
            context
                .room
                .write_grants(package.id)
                .map_err(|source| PlanError::Grants {
                    app: package.id.to_owned(),
                    source,
                })?;
        let mut args = vec![
            "run".to_owned(),
            "--manifest".to_owned(),
            package.manifest.display().to_string(),
            "--payload-root".to_owned(),
            package.payload_root.display().to_string(),
        ];
        if context.room.is_workspace() {
            args.push("--workspace-id".to_owned());
            args.push(context.room.id.clone());
        }
        // Always passed, so the launcher checks every capability against
        // this id rather than trusting the file to already be filtered.
        args.push("--app-id".to_owned());
        args.push(package.id.to_owned());
        if let Some(socket) = &context.environment.wayland_socket {
            args.push("--wayland-socket".to_owned());
            args.push(socket.display().to_string());
        }
        if let Some(path) = &capabilities {
            args.push("--capabilities".to_owned());
            args.push(path.display().to_string());
        }
        if let Some(path) = &context.identity {
            args.push("--identity".to_owned());
            args.push(path.display().to_string());
        }
        if let Some(node) = &context.render_node {
            args.push("--render-node".to_owned());
            args.push(node.display().to_string());
        }
        match open {
            Some(Open::File(path)) => {
                args.push("--open".to_owned());
                args.push(path.display().to_string());
            }
            Some(Open::Uri(uri)) => {
                args.push("--open-uri".to_owned());
                args.push(uri.clone());
            }
            None => {}
        }
        let env = context.environment.variables();
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
        let scan = desktop_entry::cache::cached_scan();
        let entry = scan.find(desktop_id).ok_or_else(|| {
            PlanError::Host(desktop_entry::launch::HostLaunchError::NotFound(
                desktop_id.to_owned(),
            ))
        })?;
        let launch_env = opening(context.environment.host_launch_env(), open);
        let planned = desktop_entry::launch::plan(entry, &launch_env)?;
        Ok(from_host_launch(planned, Kind::of_entry(entry)))
    }

    /// The program file at `path` itself, as opening a downloaded `AppImage`
    /// or an executable in Files does. The file is made runnable first: a
    /// downloaded program arrives without the executable bit.
    ///
    /// # Errors
    /// See [`Self::of`], and the file is not a program that runs by itself.
    pub fn program(path: &Path, context: &PlanContext<'_>) -> Result<Self, PlanError> {
        desktop_entry::launch::make_runnable(path)?;
        let planned =
            desktop_entry::launch::plan_program(path, &context.environment.host_launch_env())?;
        let kind = match path.extension().and_then(std::ffi::OsStr::to_str) {
            Some("AppImage") => Kind::AppImage,
            _ => Kind::Program,
        };
        Ok(from_host_launch(planned, kind))
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

/// A host app's launch environment with what it opens: a document reaches
/// the entry's `%f`/`%F` as a path, a link its `%u`/`%U` as it is.
fn opening(mut env: LaunchEnv, open: Option<&Open>) -> LaunchEnv {
    match open {
        Some(Open::File(path)) => env.files.push(path.display().to_string()),
        Some(Open::Uri(uri)) => env.urls.push(uri.clone()),
        None => {}
    }
    env
}

/// A host plan as it was resolved.
fn from_host_launch(planned: HostLaunch, kind: Kind) -> Planned {
    Planned {
        app: planned.desktop_id.clone(),
        title: planned.display_name.clone(),
        kind,
        program: planned.program,
        args: planned.args,
        env: planned.env,
        window_identities: planned.window_identities,
    }
}

/// The strings a package's window may announce itself by: its id, and its
/// trailing component, which is what a toolkit defaults a Wayland `app_id`
/// to when the manifest does not say.
fn package_identities(package_id: &str) -> Vec<String> {
    let mut identities = vec![package_id.to_owned()];
    if let Some(tail) = package_id.rsplit('.').next()
        && tail != package_id
        && !tail.is_empty()
    {
        identities.push(tail.to_owned());
    }
    identities
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(environment: &'a SessionEnvironment, room: &'a Room) -> PlanContext<'a> {
        PlanContext {
            environment,
            room,
            launcher: Some(PathBuf::from("/usr/libexec/tilo/tilo-launcher")),
            identity: None,
            render_node: None,
        }
    }

    fn displays() -> SessionEnvironment {
        SessionEnvironment {
            wayland_socket: Some(PathBuf::from("/run/user/1000/wayland-4")),
            x11_display: Some(":11".into()),
            x11_authority: Some(PathBuf::from("/run/user/1000/tilo/Xauthority")),
            session_bus: Some("unix:path=/run/user/1000/tilo/bus".into()),
            forwarded: BTreeMap::from([("TILO_USER_ID".to_owned(), "person-3".to_owned())]),
        }
    }

    fn spotify() -> desktop_entry::DesktopEntry {
        desktop_entry::entry::parse(
            "[Desktop Entry]\nType=Application\nName=Spotify\nIcon=com.spotify.Client\n\
             Exec=/usr/bin/flatpak run --file-forwarding com.spotify.Client @@u %U @@\n\
             StartupWMClass=spotify\nX-Flatpak=com.spotify.Client\n",
            "com.spotify.Client.desktop",
            Path::new("/var/lib/flatpak/exports/share/applications/com.spotify.Client.desktop"),
            &[],
        )
        .expect("parses")
    }

    #[test]
    fn a_package_launches_through_the_launcher_with_its_room_and_the_sessions_displays() {
        let directory = tempfile::tempdir().expect("a directory");
        let room = Room::under(directory.path(), "room-9");
        let environment = displays();
        let context = context(&environment, &room);
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
                "room-9",
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
    fn a_launch_without_a_launcher_or_an_installed_package_starts_nothing() {
        let directory = tempfile::tempdir().expect("a directory");
        let room = Room::under(directory.path(), "room");
        let environment = displays();
        let launch = Launch::parse("so.tilo.notes").expect("a package launch");
        let mut context = context(&environment, &room);
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
    fn a_host_app_runs_its_entries_command_line_with_the_document_it_opens() {
        let entry = spotify();
        let launch_env = opening(
            displays().host_launch_env(),
            Some(&Open::File("/home/you/track.flac".into())),
        );
        let planned = desktop_entry::launch::plan(&entry, &launch_env).expect("plans");
        let planned = from_host_launch(planned, Kind::of_entry(&entry));
        assert_eq!(planned.kind, Kind::Flatpak);
        assert!(planned.kind.is_host());
        assert_eq!(planned.program, PathBuf::from("/usr/bin/flatpak"));
        assert_eq!(
            planned.args,
            [
                "run",
                "--socket=x11",
                "--file-forwarding",
                "com.spotify.Client",
                "@@u",
                "file:///home/you/track.flac",
                "@@"
            ],
            "the document reaches the entry's own field code"
        );
        assert_eq!(
            planned.env.get("WAYLAND_DISPLAY").map(String::as_str),
            Some("wayland-4")
        );
        assert_eq!(
            planned.env.get("TILO_HOST_APP").map(String::as_str),
            Some("1")
        );
        assert!(
            !planned.env.contains_key("TILO_USER_ID"),
            "a host app is not confined, so it is told nothing of Tilo's authority"
        );
        assert_eq!(
            planned.window_identities,
            [
                "spotify",
                "com.spotify.Client.desktop",
                "com.spotify.Client"
            ]
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
