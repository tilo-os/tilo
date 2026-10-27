//! An app whose build speaks another interface version than this system.
//!
//! Tilo keeps no code for an older interface version
//! (docs/architecture/compatibility.md): a build speaks exactly the versions
//! this system serves, or it does not start. Every launch reads the build's
//! manifest against the image's declaration first. An app that is behind is
//! updated, its place saying "Updating Mail…", and then starts in that same
//! place. One whose developer has not published a build that fits is
//! "Waiting for an update from the developer" and does not start. One that
//! needs a newer system starts the system's update.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use registry_types::interfaces::{Behind, Unmet};
use tilo_lock::MutexRecover as _;

/// How a build fits the interface versions this system serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fit {
    /// It speaks every version this system serves.
    Fits,
    /// It was built against older versions: a newer build fits.
    AppBehind(Vec<Unmet>),
    /// It needs a version this system does not serve yet.
    SystemBehind(Vec<Unmet>),
}

/// How the build whose manifest is at `manifest_path` fits this system. A
/// host that declares no interfaces (a development machine) judges nothing.
pub(crate) fn fit(manifest_path: &Path) -> Fit {
    let provided = package_store::interfaces::system_interfaces();
    fit_on(package_store::interfaces::unmet_at(
        provided.as_ref(),
        manifest_path,
    ))
}

fn fit_on(unmet: Vec<Unmet>) -> Fit {
    if unmet.is_empty() {
        Fit::Fits
    } else if unmet.iter().all(|unmet| unmet.behind() == Behind::Package) {
        Fit::AppBehind(unmet)
    } else {
        Fit::SystemBehind(unmet)
    }
}

/// Packages being updated so they can start: their places say "Updating…".
static UPDATING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

/// Whether `package_id` is being updated before it starts.
pub(crate) fn updating(package_id: &str) -> bool {
    UPDATING.lock_recover().contains(package_id)
}

/// Mark `package_id` as being updated, or not, as [`update_then`] does.
#[cfg(test)]
pub(crate) fn mark_updating_for_test(package_id: &str, updating: bool) {
    let mut set = UPDATING.lock_recover();
    if updating {
        set.insert(package_id.to_owned());
    } else {
        set.remove(package_id);
    }
}

/// How long an update may hold the place before its window is due, on top
/// of the usual start-up allowance: as long as an install may take.
pub(crate) const UPDATE_TIMEOUT: Duration = crate::shell::catalogue::INSTALL_TIMEOUT;

/// Update `package_id` on a worker, its place saying "Updating…", then call
/// `start` with whether a build that fits is installed now. `start` runs on
/// the worker; it launches the app again, or reports why it cannot.
pub(crate) fn update_then(package_id: &str, start: impl FnOnce(UpdateOutcome) + Send + 'static) {
    update_after(package_id, || {}, start);
}

/// [`update_then`], running `first` on the worker before the update starts.
/// When `package_id` is already being updated, neither runs.
pub(crate) fn update_after(
    package_id: &str,
    first: impl FnOnce() + Send + 'static,
    start: impl FnOnce(UpdateOutcome) + Send + 'static,
) {
    if !UPDATING.lock_recover().insert(package_id.to_owned()) {
        return;
    }
    crate::shell::launcher::hold_presentation_for(package_id, UPDATE_TIMEOUT);
    let package = package_id.to_owned();
    let spawned = std::thread::Builder::new()
        .name("app-update".to_string())
        .spawn(move || {
            first();
            let outcome = crate::shell::catalogue::update_package(&package);
            crate::shell::launcher::invalidate_app_cache();
            UPDATING.lock_recover().remove(&package);
            crate::shell::launcher::hold_presentation_for(&package, Duration::ZERO);
            start(outcome);
        });
    if spawned.is_err() {
        UPDATING.lock_recover().remove(package_id);
        tracing::warn!(
            package_id,
            "could not start the update an app needs to open"
        );
    }
}

/// Apps by the surface a room's write named that this device's copy does
/// not ship.
type Sought = HashSet<(String, [u8; 32])>;
/// Each starts the app's update once.
static SOUGHT: LazyLock<Mutex<Sought>> = LazyLock::new(Mutex::default);

/// A room's write for `package_id` names `surface`, which neither the build
/// this device runs nor the one it has installed ships: its writer runs a
/// newer build of the app. That is the app's update, the same one an app
/// behind takes as it starts. Once it is in, the surface the updated package
/// ships is readable ([`installed_surface`]) and the writes held for it are
/// decided again. An app this device does not have is not installed here;
/// its writes wait as any held write does.
pub(crate) fn surface_named(package_id: &str, surface: [u8; 32]) {
    if ui::app_surface::hash_of(package_id) == Some(surface) {
        return;
    }
    match installed_package_surface(package_id) {
        Installed::Absent => return,
        Installed::Ships(Some((hash, _))) if hash == surface => {
            // Installed since the write was decided: decide it again.
            ui::app_surface::arrived();
            return;
        }
        Installed::Ships(_) => {}
    }
    if !SOUGHT
        .lock_recover()
        .insert((package_id.to_owned(), surface))
    {
        return;
    }
    tracing::info!(
        package_id,
        surface = %hex::encode(surface),
        "a room names a newer surface of an app: updating it"
    );
    let package = package_id.to_owned();
    update_then(package_id, move |outcome| {
        if matches!(installed_package_surface(&package), Installed::Ships(Some((hash, _))) if hash == surface)
        {
            tracing::info!(package_id = %package, "the app's update brought the surface its room names");
        } else {
            tracing::warn!(
                package_id = %package,
                ?outcome,
                "the app's update did not bring the surface its room names; its writes wait"
            );
        }
        ui::app_surface::arrived();
    });
}

/// A signed surface by its hash.
type Surface = ([u8; 32], Arc<[u8]>);

#[derive(Clone)]
enum Installed {
    /// The app is not installed.
    Absent,
    /// The app is installed, shipping this signed surface, or none.
    Ships(Option<Surface>),
}

/// Each installed app as of the installed store's epoch it was read at.
type InstalledSurfaces = HashMap<String, (u64, Installed)>;
static INSTALLED_SURFACES: LazyLock<Mutex<InstalledSurfaces>> = LazyLock::new(Mutex::default);

/// The surface the installed package `package_id` ships, by its hash: what
/// a change of that app naming it is decided against, before the app has
/// started with it. Signed by the package's publisher, as the surface the
/// app runs with is ([`ui::app_surface::register_package`]).
pub(crate) fn installed_surface(package_id: &str, hash: &str) -> Option<Vec<u8>> {
    let Installed::Ships(Some((installed, bytes))) = installed_package_surface(package_id) else {
        return None;
    };
    (hex::encode(installed) == hash).then(|| bytes.to_vec())
}

fn installed_package_surface(package_id: &str) -> Installed {
    let epoch = crate::shell::launcher::installed_apps_epoch();
    if let Some((read_at, installed)) = INSTALLED_SURFACES.lock_recover().get(package_id)
        && *read_at == epoch
    {
        return installed.clone();
    }
    let installed = package_store::find_installed(package_id)
        .ok()
        .flatten()
        .map_or(Installed::Absent, |installed| {
            Installed::Ships(
                package_store::integrity::signed_surface(&installed)
                    .map(|bytes| (*blake3::hash(&bytes).as_bytes(), Arc::from(bytes))),
            )
        });
    INSTALLED_SURFACES
        .lock_recover()
        .insert(package_id.to_owned(), (epoch, installed.clone()));
    installed
}

/// What updating a package that was behind came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateOutcome {
    /// `app update` ran; whether the newest build fits is read from it.
    Updated,
    /// The update could not run or reach the store: `detail` says why.
    Failed(String),
}

/// Start the system's own update now, rather than at its next scheduled
/// check: an app or a collaborator needs a newer version than this system
/// serves. The update service stages the new system; applying it is part of
/// the next restart (`crate::shell::system_update`).
pub(crate) fn update_system_now() {
    crate::shell::system_update::update_now_and_ask();
}

/// A running app said another shell-ui version than this shell's when it
/// connected (`None`: it said none). The shell closed that connection; the
/// side that is behind updates now. An app's launch already refuses a build
/// whose manifest names another version, so this is a build whose binary and
/// manifest disagree: the newest build replaces it for its next start.
pub(crate) fn spoke_another_shell_version(app_id: &str, app_version: Option<u16>) {
    let shell = ui::floating_surface::FLOATING_SURFACE_PROTOCOL_VERSION;
    tracing::warn!(
        app_id,
        ?app_version,
        shell,
        "an app connected at another shell-ui version"
    );
    if app_version.is_some_and(|app| app > shell) {
        update_system_now();
    } else {
        update_then(app_id, |_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unmet(requirement: &str, provided: u32) -> Unmet {
        Unmet {
            requirement: requirement.to_owned(),
            provided: Some(provided),
        }
    }

    #[test]
    fn an_app_behind_is_updated_and_one_ahead_waits_for_the_system() {
        assert_eq!(fit_on(Vec::new()), Fit::Fits);
        let behind = vec![unmet("shell-ui@5", 6), unmet("collab@1", 2)];
        assert_eq!(fit_on(behind.clone()), Fit::AppBehind(behind));
        let ahead = vec![unmet("shell-ui@5", 6), unmet("collab@3", 2)];
        assert_eq!(fit_on(ahead.clone()), Fit::SystemBehind(ahead));
        let unknown = vec![Unmet {
            requirement: "displays@1".into(),
            provided: None,
        }];
        assert_eq!(fit_on(unknown.clone()), Fit::SystemBehind(unknown));
    }
}
