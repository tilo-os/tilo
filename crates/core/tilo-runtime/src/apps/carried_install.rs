//! Installing the packages a program carries (`Tilo.app`'s `Resources/runtimes`, the folder beside
//! a Windows daemon, a phone's APK assets) into the device's store, through the one installer.

use package_store::installer;

/// The package root `path`'s name gives, for a package a program carries: `<root>.tapp`, its root
/// 64 hex digits.
fn root_of(path: &std::path::Path) -> Option<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|_| path.extension().is_some_and(|ext| ext == "tapp"))
        .filter(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Install the packages `folder` carries into `store`, as `from` handed them over and a first
/// start does, through the one installer: each must be the exact package its sealed name says and
/// its bundle signed by a publisher the app lists. A package the store refuses goes to `refused`,
/// with the root its name gave and why in words; `ignore_missing_runtime` takes a package whose
/// runtime this device carries itself, as its own signed manifest names it.
pub(super) fn install_carried_in(
    catalog: &package_store::Catalog,
    store: &package_store::PackageStore,
    folder: &std::path::Path,
    from: &str,
    ignore_missing_runtime: bool,
    mut refused: impl FnMut(&str, String),
) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let installed: Vec<String> = catalog
        .list()
        .map(|packages| packages.into_iter().map(|p| p.package_id_hex).collect())
        .unwrap_or_default();
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(root) = root_of(&path) else {
            continue;
        };
        if installed.iter().any(|id| id.eq_ignore_ascii_case(root)) {
            continue;
        }
        let request = installer::Request {
            trust: installer::Trust {
                require_package_root: Some(root.to_owned()),
                // No attestor vouches for the app's own publisher: the package root, sealed in
                // the app's signature, is what this trusts. Bytes that hash to another package,
                // or a name that is no root, are refused.
                allow_self_signed: true,
                ..installer::Trust::default()
            },
            ignore_missing_runtime,
            ..installer::Request::default()
        };
        match installer::install(
            store,
            catalog,
            &installer::Candidate::staged(&path, from),
            &request,
            None,
        ) {
            Ok(_) | Err(installer::Error::AlreadyInstalled(_)) => {}
            Err(error) => refused(root, error.to_string()),
        }
    }
}

/// Install the app packages `folder` carries (`<root>.tapp` each) into this device's store,
/// through the one installer, as a first start does with the packages a program carries: what
/// lets a workspace this device founds run them, and this device open them, as any verified
/// install. Answers each package the store refused, with why in words; a refused one is not
/// cached.
pub(crate) fn install_carried_apps(folder: &std::path::Path) -> Vec<(String, String)> {
    let mut refused = Vec::new();
    let (Ok(catalog), Ok(store)) = (
        package_store::Catalog::from_env(),
        package_store::PackageStore::from_env(),
    ) else {
        return refused;
    };
    install_carried_in(
        &catalog,
        &store,
        folder,
        "the packages this app carries",
        true,
        |root, why| refused.push((root.to_owned(), why)),
    );
    refused
}

#[cfg(test)]
#[path = "carried_install_tests.rs"]
mod tests;
