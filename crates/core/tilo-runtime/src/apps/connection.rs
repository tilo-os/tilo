//! The apps' long-lived connections this device runs when it holds their lease
//! ([`tilo_workspace_service::work::Turn::Connection`]).
//!
//! An app says it keeps one in its signed manifest (`[entrypoint] connection = true`). The device
//! that holds the lease starts it the way the daemon starts any app with no window: a Rust app's
//! own process, launched and recognised by its exact launch record, here with no idle end; a
//! tilolang app's instance that shows nothing, with its origin's connector, which holds the
//! outside connection. Stopping it ends exactly that process, or that connector.

use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use tilo_workspace_service::work::{HostConnections, StartRefusal};

use super::{Apps, Launched, Shows, process_start_time};

impl HostConnections for Apps {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        self.installed()
            .into_iter()
            .filter(|app| {
                app.manifest
                    .entrypoint
                    .as_ref()
                    .is_some_and(|entrypoint| entrypoint.connection)
            })
            .map(|app| {
                let needs = app.manifest.capabilities.required.clone();
                (app.app_id.to_string(), needs)
            })
            .collect()
    }

    fn installed(&self, app: &str) -> bool {
        self.installed()
            .iter()
            .any(|installed| installed.app_id == app)
    }

    /// The build of `app` installed here declares `capability`, so its launch here is given it.
    fn gives(&self, _: &str, app: &str, capability: &str) -> bool {
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

    fn on_power(&self) -> bool {
        on_power()
    }

    fn start<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StartRefusal>> + Send + 'a>>
    {
        Box::pin(async move {
            // The installed package this start runs: a reinstall of it is another one, and mends
            // a failure the app stopped here.
            let package = self
                .installed()
                .into_iter()
                .find(|installed| installed.app_id == app)
                .map_or_else(String::new, |installed| {
                    installed.payload_root.display().to_string()
                });
            if let Some(refusal) = self.failures.waiting(
                workspace,
                app,
                &package,
                crate::device::package_runs::runtime_installed,
                self.retry,
            ) {
                return Err(refusal);
            }
            // A runtime the app names and this device does not have is no failure time mends:
            // the app is reported with that reason, and nothing is started for it until the
            // runtime is installed or the app is reinstalled.
            if let Some(runtime) = self.missing_runtime(app) {
                let reason = self.needs_runtime(app).unwrap_or_default();
                return Err(self.failures.failed(
                    workspace,
                    app,
                    &package,
                    reason,
                    Some(runtime.as_str()),
                    self.retry,
                ));
            }
            match self.start_connection(app, workspace).await {
                Ok(()) => {
                    self.failures.started(workspace, app);
                    Ok(())
                }
                Err(reason) => Err(self
                    .failures
                    .failed(workspace, app, &package, reason, None, self.retry)),
            }
        })
    }

    fn runs(&self, workspace: &str, app: &str) -> bool {
        if crate::device::package_runs::runs_in_process(app) && !self.keeps_its_own_process(app) {
            return self.connectors.runs(app, workspace);
        }
        self.launched.no_window_running(app, workspace, None)
    }

    fn stop(&self, workspace: &str, app: &str) {
        // A person stopping the connection asks for it again from a clean slate.
        self.failures.forget(workspace, app);
        if crate::device::package_runs::runs_in_process(app) && !self.keeps_its_own_process(app) {
            self.connectors.stop(app, workspace);
            return;
        }
        self.launched.stop_no_window(app, workspace);
    }
}

impl Apps {
    /// Start `app`'s connection in `workspace`: [`HostConnections::start`] once a failure the app
    /// stopped here is not in the way.
    async fn start_connection(&self, app: &str, workspace: &str) -> Result<(), String> {
        if crate::device::package_runs::runs_in_process(app) && !self.keeps_its_own_process(app) {
            // A tilolang app's outside connection is its origin's connector.
            if crate::device::package_runs::origin_of(app)?.is_none() {
                return Err(format!(
                    "{app} says it keeps a connection, but its manifest names no outside service \
                     (`[entrypoint] origins`) to keep it to"
                ));
            }
            let origin = self.origin_connection(app, workspace)?;
            return self
                .instances
                .start_with_no_view(app, workspace, origin, self.access_for(workspace, app))
                .map(drop);
        }
        if self.launched.no_window_running(app, workspace, None) {
            return Ok(());
        }
        self.start_watched(app, workspace, Shows::Connection, None)
            .await
            .map(drop)
    }

    /// Why this device cannot start `app` now: a runtime its signed manifest names that is not
    /// installed, which no retry mends and a person can act on.
    pub(super) fn needs_runtime(&self, app: &str) -> Option<String> {
        self.missing_runtime(app).map(|runtime| {
            format!(
                "{app} needs the runtime `{runtime}`, which is not installed; installing or \
                 updating the app brings it"
            )
        })
    }

    /// The runtime the signed manifest of `app` names that this device does not have, or `None`
    /// when it has every one it names.
    pub(super) fn missing_runtime(&self, app: &str) -> Option<String> {
        let installed = self
            .installed()
            .into_iter()
            .find(|installed| installed.app_id == app)?;
        let package = &installed.manifest.package;
        let entrypoint = installed
            .manifest
            .entrypoint
            .as_ref()
            .and_then(|entrypoint| entrypoint.runtime.clone());
        package
            .runtime
            .iter()
            .chain(&package.required_runtimes)
            .chain(entrypoint.iter())
            .find(|runtime| !crate::device::package_runs::runtime_installed(runtime))
            .cloned()
    }

    /// Whether `app`'s held connection is its own process: a package that carries a program
    /// (`[entrypoint] script`) runs that program as the connection, beside the module its code
    /// runs from, where a tilolang app's connection is its origin's connector.
    fn keeps_its_own_process(&self, app: &str) -> bool {
        self.installed()
            .into_iter()
            .find(|installed| installed.app_id == app)
            .and_then(|installed| installed.manifest.entrypoint)
            .is_some_and(|entrypoint| entrypoint.script.is_some())
    }
}

impl Launched {
    /// End every process this device started with no window for `app` in `workspace` on no
    /// record, each recognised by its launch record: the reaper that waits for it forgets it.
    pub(super) fn stop_no_window(&self, app: &str, workspace: &str) {
        let pids: Vec<u32> = self
            .0
            .lock_recover()
            .iter()
            .filter(|(pid, record)| {
                record.no_window
                    && record.app == app
                    && record.workspace == workspace
                    && record.record.is_none()
                    && process_start_time(**pid) == Some(record.start_time)
            })
            .map(|(pid, _)| *pid)
            .collect();
        // No app process runs with no window on Windows: only tilolang apps run there, in
        // their processes, whose connections are their connectors.
        #[cfg(windows)]
        drop(pids);
        #[cfg(unix)]
        for pid in pids {
            let Ok(raw) = libc::pid_t::try_from(pid) else {
                continue;
            };
            tracing::info!(app, workspace, pid, "stopping an app's connection");
            // SAFETY: `pid` is a child this daemon launched and has not reaped: its reaper still
            // waits for it, and its start time is the one recorded.
            unsafe {
                libc::kill(raw, libc::SIGTERM);
            }
        }
    }
}

/// How long a reading of the device's power stands.
const POWER_READ: Duration = Duration::from_secs(10);

/// Whether this device runs on external power, or has no battery to run on.
fn on_power() -> bool {
    static LAST: std::sync::Mutex<Option<(Instant, bool)>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock_recover();
    if let Some((at, on)) = *last
        && at.elapsed() < POWER_READ
    {
        return on;
    }
    let on = read_power();
    *last = Some((Instant::now(), on));
    on
}

/// The power supplies the kernel reports, Linux's and Android's alike: on power while one of
/// mains or USB is online, or when the device has no battery at all.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_power() -> bool {
    let read = |path: std::path::PathBuf| {
        std::fs::read_to_string(path)
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    };
    let Ok(supplies) = std::fs::read_dir("/sys/class/power_supply") else {
        return true;
    };
    let mut battery = false;
    for supply in supplies.flatten() {
        let path = supply.path();
        match read(path.join("type")).as_str() {
            "Battery" => battery = true,
            "Mains" | "USB" if read(path.join("online")) == "1" => return true,
            _ => {}
        }
    }
    !battery
}

/// What `pmset` says powers the Mac.
#[cfg(target_os = "macos")]
fn read_power() -> bool {
    match std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "ps"])
        .output()
    {
        Ok(output) => !String::from_utf8_lossy(&output.stdout).contains("'Battery Power'"),
        Err(error) => {
            tracing::warn!(%error, "pmset did not say what powers this Mac, so it counts as on power");
            true
        }
    }
}

/// What Windows says powers this machine: on power while it reports AC power online, or cannot
/// tell, as a machine with no battery runs on power.
#[cfg(windows)]
fn read_power() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: a live output struct of the size the call writes.
    if unsafe { GetSystemPowerStatus(&raw mut status) } == 0 {
        tracing::warn!("Windows did not say what powers this machine, so it counts as on power");
        return true;
    }
    // 1 is mains, 0 is battery, 255 says it cannot tell: a machine it cannot tell about runs on
    // power, as one with no battery does.
    status.ACLineStatus != 0
}
