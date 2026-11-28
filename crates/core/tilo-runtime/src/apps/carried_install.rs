//! Installing the runtime packages a program carries (`Tilo.app`'s `Resources/runtimes`, the
//! folder beside a Windows daemon) into the device's store, through the one installer.

/// Install the runtime packages under `folder` into `store`'s records, as `from` handed them
/// over: what a first start does with the folder the program carries, and what a test drives
/// without an app bundle around it.
#[cfg_attr(
    all(not(any(target_os = "macos", windows)), not(test)),
    expect(
        dead_code,
        reason = "only a host with its own folder of carried runtimes calls this"
    )
)]
pub(crate) fn install_carried_runtimes_in(
    catalog: &package_store::Catalog,
    store: &package_store::PackageStore,
    folder: &std::path::Path,
    from: &str,
) {
    use package_store::installer;
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let installed: Vec<String> = catalog
        .list()
        .map(|packages| packages.into_iter().map(|p| p.package_id_hex).collect())
        .unwrap_or_default();
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(root) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|_| path.extension().is_some_and(|ext| ext == "tapp"))
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
            store,
            catalog,
            &installer::Candidate::staged(&path, from),
            &request,
            None,
        ) {
            Ok(_) | Err(installer::Error::AlreadyInstalled(_)) => {}
            Err(error) => {
                tracing::warn!(%error, package = %root, "a runtime this program carries did not install");
            }
        }
    }
}

/// Install the app packages `folder` carries (`<root>.tapp` each) into this device's store,
/// through the one installer, as a first start does with the packages a program carries: what
/// lets a workspace this device founds run them, and this device open them, as any verified
/// install. Answers each package the store refused, with why in words; a refused one is not
/// cached.
pub(crate) fn install_carried_apps(folder: &std::path::Path) -> Vec<(String, String)> {
    use package_store::installer;
    let mut refused = Vec::new();
    let (Ok(catalog), Ok(store)) = (
        package_store::Catalog::from_env(),
        package_store::PackageStore::from_env(),
    ) else {
        return refused;
    };
    let Ok(entries) = std::fs::read_dir(folder) else {
        return refused;
    };
    let installed: Vec<String> = catalog
        .list()
        .map(|packages| packages.into_iter().map(|p| p.package_id_hex).collect())
        .unwrap_or_default();
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(root) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|_| path.extension().is_some_and(|ext| ext == "tapp"))
            .filter(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            continue;
        };
        if installed.iter().any(|id| id.eq_ignore_ascii_case(root)) {
            continue;
        }
        let request = installer::Request {
            trust: installer::Trust {
                // The package root its name gives is what this trusts: bytes that hash to
                // another package, or a name that is no root, are refused.
                require_package_root: Some(root.to_owned()),
                allow_self_signed: true,
                ..installer::Trust::default()
            },
            // This device carries the runtimes its apps run on, as its own signed manifest names
            // them; the store need not hold them beside the app.
            ignore_missing_runtime: true,
            ..installer::Request::default()
        };
        match installer::install(
            &store,
            &catalog,
            &installer::Candidate::staged(&path, "the packages this app carries"),
            &request,
            None,
        ) {
            Ok(_) | Err(installer::Error::AlreadyInstalled(_)) => {}
            Err(error) => refused.push((root.to_owned(), error.to_string())),
        }
    }
    refused
}

#[cfg(test)]
#[path = "carried_install_tests.rs"]
mod tests;
