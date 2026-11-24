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
/// tilolang runtime among them, in [`carried`]) that this profile does not have yet, through the
/// one installer, as the Store installs any package: each must be the exact package its sealed
/// name says and its bundle signed by a publisher the app lists. Once installed, a runtime updates
/// from the registry like every package; a later program does not put back an older one.
pub(super) fn install_carried_runtimes(catalog: &package_store::Catalog) {
    use package_store::installer;
    let Some(carried) = carried().filter(|dir| dir.is_dir()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&carried) else {
        return;
    };
    let installed: Vec<String> = catalog
        .list()
        .map(|packages| packages.into_iter().map(|p| p.package_id_hex).collect())
        .unwrap_or_default();
    let store = match package_store::PackageStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(%error, "no package store for the runtimes this program carries");
            return;
        }
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(root) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| path.extension().is_some_and(|ext| ext == "tapp"))
            .filter(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            continue;
        };
        if installed.iter().any(|id| id.eq_ignore_ascii_case(root)) {
            continue;
        }
        let request = installer::Request {
            trust: installer::Trust {
                require_package_root: Some(root.to_owned()),
                // No attestor vouches for the app's own publisher: the package root, sealed in the
                // app's signature, is what this trusts.
                allow_self_signed: true,
                ..installer::Trust::default()
            },
            ..installer::Request::default()
        };
        match installer::install(
            &store,
            catalog,
            &installer::Candidate::staged(&path, CARRIER),
            &request,
            None,
        ) {
            Ok(_) | Err(installer::Error::AlreadyInstalled(_)) => {}
            Err(error) => {
                tracing::warn!(%error, package = %root, "a runtime this program carries did not install")
            }
        }
    }
}
