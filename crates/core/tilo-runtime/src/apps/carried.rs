//! The runtime packages the daemon's program carries, installed on its first start: Tilo.app's
//! on a Mac, the ones beside `tilo-runtime.exe` on Windows.

/// Where the running program carries its runtime packages: `Contents/Resources/runtimes` of
/// Tilo.app, the `runtimes` folder beside `tilo-runtime.exe`.
fn carried() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()?;
    #[cfg(target_os = "macos")]
    return Some(exe.parent()?.parent()?.join("Resources/runtimes"));
    #[cfg(windows)]
    return Some(exe.parent()?.join("runtimes"));
}

/// What the installer names as the place a carried package came from.
#[cfg(target_os = "macos")]
const CARRIER: &str = "Tilo.app";
#[cfg(windows)]
const CARRIER: &str = "tilo-runtime";

/// Install the runtime packages this daemon's program carries (`<package root>.tapp`, the
/// tilolang runtime and the Tilo runtime itself among them, in [`carried`]) that this profile
/// does not have yet, through the one installer, as the Store installs any package: each must be
/// the exact package its sealed name says and its bundle signed by a publisher the app lists.
/// Once installed, a runtime updates from the registry like every package; a later program does
/// not put back an older one.
pub(super) fn install_carried_runtimes(catalog: &package_store::Catalog) {
    let Some(carried) = carried().filter(|dir| dir.is_dir()) else {
        return;
    };
    let store = match package_store::PackageStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(%error, "no package store for the runtimes this program carries");
            return;
        }
    };
    super::carried_install::install_carried_runtimes_in(catalog, &store, &carried, CARRIER);
}
