//! Installing the runtime packages a program carries (`Tilo.app`'s `Resources/runtimes`, the
//! folder beside a Windows daemon) into the device's store, through the one installer.

/// Install the runtime packages under `folder` into `store`'s records, as `from` handed them
/// over: what a first start does with the folder the program carries, and what a test drives
/// without an app bundle around it.
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

#[cfg(test)]
#[path = "carried_install_tests.rs"]
mod tests;
