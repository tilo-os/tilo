//! Installed applications for a runtime profile: what can be launched, what
//! each declared it may do on the action door, and launching one into the
//! profile's workspace.
//!
//! A launch always goes through the platform's launcher, which confines the
//! app: `tilo-launcher` (bubblewrap, Landlock, seccomp) on Linux, App
//! Sandbox or Seatbelt through `launcher-macos` on macOS. Without one the
//! launch is refused; nothing here starts an app unconfined.
//!
//! Only a process this runtime launched, for exactly that app and room and
//! still the same process, is attested to the broker.

use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET};

/// Where the profile keeps the person's camera and microphone decisions.
#[cfg(target_os = "macos")]
const DEVICE_GRANTS: &str = "state/device-grants";
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};
use tilo_action_broker::{
    app_actions::{AppActionAccess, AppActionAccessResolver},
    services::{LaunchableApplication, LauncherServices},
};
use tilo_installed_apps::InstalledApp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// The profile's installed applications and the launches it made.
pub struct Apps {
    /// The stores the profile reads; `None` when no app store can be
    /// located, which is a catalogue with nothing in it.
    catalog: Option<package_store::Catalog>,
    room: String,
    root: PathBuf,
    /// Where app containers live on macOS, beside the runtime profile.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "macOS containers only")
    )]
    data_home: PathBuf,
    /// Where `tilo-launcher` is looked for, in order.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "Linux launcher only")
    )]
    launcher_candidates: Vec<PathBuf>,
    launched: Arc<Launched>,
    /// Who may capture from which device, in `state/device-grants` of the
    /// profile; opened on the first macOS launch.
    #[cfg(target_os = "macos")]
    device_grants: std::sync::OnceLock<Arc<tilo_capture::grants::DeviceGrants>>,
}

impl Apps {
    /// The apps of the profile at `profile`, serving `room`, from the stores
    /// the environment names.
    #[must_use]
    pub fn from_env(profile: &Path, room: &str) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::new(
            package_store::Catalog::from_env().ok(),
            profile,
            room,
            home.as_deref().map_or_else(PathBuf::new, data_home),
            launcher_candidates(),
        )
    }

    /// Explicit stores and places, for tests.
    #[must_use]
    pub fn new(
        catalog: Option<package_store::Catalog>,
        profile: &Path,
        room: &str,
        data_home: PathBuf,
        launcher_candidates: Vec<PathBuf>,
    ) -> Self {
        Self {
            catalog,
            room: room.to_owned(),
            root: profile.to_path_buf(),
            data_home,
            launcher_candidates,
            launched: Arc::default(),
            #[cfg(target_os = "macos")]
            device_grants: std::sync::OnceLock::new(),
        }
    }

    /// This profile's device grants.
    ///
    /// # Errors
    ///
    /// The grants file cannot be read; no capture is brokered then.
    #[cfg(target_os = "macos")]
    pub(crate) fn device_grants(&self) -> std::io::Result<Arc<tilo_capture::grants::DeviceGrants>> {
        if let Some(grants) = self.device_grants.get() {
            return Ok(Arc::clone(grants));
        }
        let grants = Arc::new(tilo_capture::grants::DeviceGrants::open(
            &self.root.join(DEVICE_GRANTS),
        )?);
        Ok(Arc::clone(self.device_grants.get_or_init(|| grants)))
    }

    /// Every installed package, GUI or not; empty without an app store or
    /// with an unreadable registry.
    fn installed(&self) -> Vec<InstalledApp> {
        self.catalog
            .as_ref()
            .and_then(|catalog| tilo_installed_apps::read(catalog).ok())
            .unwrap_or_default()
    }

    /// What the installed package `app_id` declared; nothing for an unknown
    /// package or an unreadable manifest.
    fn declared_access(&self, app_id: &str) -> AppActionAccess {
        self.installed()
            .into_iter()
            .find(|app| app.package_id == app_id)
            .map(|app| {
                let declared = app.declared_actions();
                AppActionAccess {
                    query: declared.query,
                    invoke: declared.invoke,
                }
            })
            .unwrap_or_default()
    }

    /// The resource kinds the installed package `app_id` declared its
    /// records hold; `None` when it is not installed.
    #[must_use]
    pub fn declared_resource_kinds(&self, app_id: &str) -> Option<Vec<String>> {
        self.installed()
            .into_iter()
            .find(|app| app.package_id == app_id)
            .map(|app| app.declared_resource_kinds())
    }

    /// [`Self::declared_access`] as the broker asks it.
    #[must_use]
    pub fn access_resolver(self: &Arc<Self>) -> AppActionAccessResolver {
        let apps = Arc::clone(self);
        Arc::new(move |app_id| apps.declared_access(app_id))
    }

    /// What people call `app_id`: its installed name, else one derived from
    /// the id.
    #[must_use]
    pub fn display_name(&self, app_id: &str) -> String {
        self.installed()
            .into_iter()
            .find(|app| app.package_id == app_id)
            .map_or_else(
                || package_store::display_name_for_package_id(app_id),
                |app| app.display_name,
            )
    }

    /// The installed GUI app `app_id`.
    fn launchable(&self, app_id: &str) -> Result<InstalledApp, String> {
        let app = self
            .installed()
            .into_iter()
            .find(|app| app.package_id == app_id)
            .ok_or_else(|| format!("{app_id} is not installed"))?;
        if !app.has_gui {
            return Err(format!("{app_id} has no window to open"));
        }
        Ok(app)
    }

    /// Launch `app_id` into this profile's room through the platform
    /// launcher; the launch's handle, recorded for attestation.
    ///
    /// # Errors
    ///
    /// Refuses an app that is not installed or has no window, a room this
    /// profile does not serve, and a platform with no launcher or a payload
    /// it cannot verify; returns spawn errors.
    pub fn start(&self, app_id: &str, room: &str) -> Result<Launch, String> {
        if room != self.room {
            return Err(format!("workspace {room} is not served by this runtime"));
        }
        let app = self.launchable(app_id)?;
        let sockets = Sockets {
            collab: self.root.join(COLLAB_SOCKET),
            identity: self.root.join(IDENTITY_SOCKET),
            blobs: self.root.join(BLOB_SOCKET),
        };
        #[cfg(target_os = "linux")]
        let launch = linux::launch(self, &app, &sockets)?;
        #[cfg(target_os = "macos")]
        let launch = macos::launch(self, &app, &sockets)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let launch: Launch = {
            let _ = (&app, &sockets);
            return Err("refusing to launch: no app sandbox on this platform".into());
        };
        if !self.launched.record(launch.pid, &app.package_id, room) {
            launch.kill();
            return Err(format!(
                "refusing to keep {app_id} running: its process could not be identified"
            ));
        }
        Ok(launch)
    }
}

impl LauncherServices for Apps {
    fn applications(&self) -> Vec<LaunchableApplication> {
        let mut apps = self.installed();
        apps.retain(|app| app.has_gui);
        apps.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        apps.into_iter()
            .map(|app| LaunchableApplication {
                package_id: app.package_id,
                display_name: app.display_name,
                unconfined: false,
            })
            .collect()
    }

    fn launch(&self, app: &str, workspace: &tilo_desktop_model::Workspace) -> Result<(), String> {
        let launch = self.start(app, &workspace.room.to_string())?;
        let launched = Arc::clone(&self.launched);
        std::thread::Builder::new()
            .name("tilo-app-reaper".into())
            .spawn(move || {
                let pid = launch.pid;
                launch.wait();
                launched.forget(pid);
            })
            .map(|_| ())
            .map_err(|error| format!("could not watch the launched app: {error}"))
    }

    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        self.launched.attest(pid, app, room)
    }
}

/// The profile's service sockets, as an app is connected to them.
struct Sockets {
    collab: PathBuf,
    identity: PathBuf,
    blobs: PathBuf,
}

/// One launched app: the process the runtime started, which is the one the
/// broker sees connect (the launcher on Linux, the app on macOS).
pub struct Launch {
    pid: u32,
    #[cfg(target_os = "linux")]
    child: std::process::Child,
    #[cfg(target_os = "macos")]
    app: launcher_macos::LaunchedApp,
}

impl Launch {
    /// Wait for the process to exit; its exit code, when it exited normally.
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
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let status: Option<std::process::ExitStatus> = None;
        status.and_then(|status| status.code())
    }

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
    }
}

/// The processes this runtime launched, by pid, with the start time that
/// tells a live launch from a later process reusing its pid.
#[derive(Default)]
struct Launched(Mutex<HashMap<u32, LaunchRecord>>);

struct LaunchRecord {
    start_time: u64,
    app: String,
    room: String,
}

impl Launched {
    /// Record `pid` as `app` in `room`; `false` when its start time cannot
    /// be read.
    fn record(&self, pid: u32, app: &str, room: &str) -> bool {
        let Some(start_time) = process_start_time(pid) else {
            return false;
        };
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                pid,
                LaunchRecord {
                    start_time,
                    app: app.to_owned(),
                    room: room.to_owned(),
                },
            );
        true
    }

    /// Whether `pid` is still the process recorded for exactly `app` and
    /// `room`.
    fn attest(&self, pid: u32, app: &str, room: &str) -> bool {
        let launches = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(record) = launches.get(&pid) else {
            return false;
        };
        let (recorded, same) = (record.start_time, record.app == app && record.room == room);
        drop(launches);
        same && process_start_time(pid) == Some(recorded)
    }

    /// Forget `pid`, once its process has been reaped.
    fn forget(&self, pid: u32) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&pid);
    }
}

/// When process `pid` started, in the kernel's units: clock ticks since boot
/// on Linux (`/proc/<pid>/stat` field 22), microseconds since the epoch on
/// macOS. `None` for a process that does not exist.
fn process_start_time(pid: u32) -> Option<u64> {
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
    if launcher::dev_env::DEV_ENV_OVERRIDES
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
    out.push(PathBuf::from("/usr/libexec/tilo-launcher"));
    out.push(PathBuf::from("/usr/bin/tilo-launcher"));
    out
}

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;
