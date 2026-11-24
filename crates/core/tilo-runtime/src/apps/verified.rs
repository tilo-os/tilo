//! What a tilolang process is started from where the daemon itself verifies it (macOS and
//! Windows; on Linux `tilo-launcher` does): the app's signed package, and the installed runtime
//! package it runs on, each proven against what was installed and its publisher's key.

/// The installed tilolang app `installed`, verified as a launch of it verifies it, on a tilolang
/// runtime installed here: what its tilolang process is handed
/// ([`crate::device::package_runs`]).
///
/// # Errors
/// It does not verify, is no tilolang app, or its runtime is not installed or does not verify.
#[cfg(any(target_os = "macos", windows))]
pub(crate) fn verified_tilolang(
    installed: &package_store::registry::InstalledPackage,
) -> Result<crate::device::package_runs::Verified, String> {
    verify_package(installed)?;
    let manifest = launcher::manifest::read_manifest(&installed.manifest_path)
        .map_err(|error| error.to_string())?;
    let runtime = launcher::manifest::code_runtime(&manifest, &installed.payload_root)
        .map(str::to_owned)
        .ok_or_else(|| format!("{} is no tilolang app", installed.id))?;
    let catalog = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
    installed_runtime(&catalog, &runtime)?;
    let module = runtime_portal::process_link::Module::read(&installed.payload_root)
        .map_err(|error| format!("its module: {error}"))?;
    Ok(crate::device::package_runs::Verified {
        app_runtime: manifest.runtime.clone().unwrap_or_default(),
        runtime,
        module,
    })
}

/// The payload of the installed runtime package that provides `runtime` (`<name>/<version>`),
/// proven as an app's own payload is: what a tilolang process runs from, and a window on that
/// runtime.
///
/// # Errors
/// No installed package provides it, or the one that does fails [`verify_package`].
#[cfg(any(target_os = "macos", windows))]
pub(crate) fn installed_runtime(
    catalog: &package_store::Catalog,
    runtime: &str,
) -> Result<std::path::PathBuf, String> {
    let provides = |package: &package_store::registry::InstalledPackage| {
        let manifest = std::fs::read_to_string(&package.manifest_path)
            .ok()
            .and_then(|source| tilo_app_package::Manifest::from_toml_str(&source).ok());
        manifest.is_some_and(|manifest| {
            manifest.package.kind == tilo_app_package::Kind::Runtime
                && manifest.package.provides.as_deref() == Some(runtime)
        })
    };
    let package = catalog
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(provides)
        .ok_or_else(|| format!("the runtime it runs on, {runtime}, is not installed"))?;
    verify_package(&package).map_err(|why| format!("its runtime {runtime}: {why}"))?;
    Ok(package.payload_root)
}

/// Prove `package` is still what was installed: its record's signature by its publisher, the
/// manifest and every payload file against the signed hashes, no file it did not ship, and — on a
/// profile that reads the person's revoked keys — a publisher key it has not revoked. The error
/// says which check failed.
pub(crate) fn verify_package(
    package: &package_store::registry::InstalledPackage,
) -> Result<(), String> {
    use package_store::integrity;
    let report =
        integrity::verify(package, &package.payload_root).map_err(|error| error.to_string())?;
    if let Some(fault) = report.faults.first() {
        return Err(format!("its payload changed after install ({fault})"));
    }
    if let Some(unexpected) = report.unexpected.first() {
        return Err(format!(
            "its payload holds a file the package did not ship ({unexpected})"
        ));
    }
    let signed = integrity::read_record(
        &integrity::record_dir(package),
        &package.id,
        &package.package_id_hex,
    )
    .map_err(|error| error.to_string())?;
    // The person's revoked keys are the profile's, read on the platform whose daemon keeps them.
    #[cfg(target_os = "macos")]
    {
        let revoked = tilo_app_package::RevocationList {
            revoked: tilo_control::packages::trust()
                .1
                .into_iter()
                .map(|key| tilo_app_package::RevokedKey {
                    pubkey_hex: key.pubkey_hex,
                    reason: key.reason,
                })
                .collect(),
        };
        if let Some(entry) = revoked.find(&signed.root.publisher_identity) {
            return Err(format!("its publisher's key is revoked ({})", entry.reason));
        }
    }
    if !hex::encode(signed.root.publisher_identity).eq_ignore_ascii_case(&package.publisher_key_hex)
    {
        return Err("its record names another publisher than the installed package's".to_owned());
    }
    Ok(())
}

/// Prove the installed package behind `app` is still the one that was
/// installed: its record's signature by its publisher, the manifest and
/// every payload file against the signed hashes, no file the package did
/// not ship, and a publisher key this profile has not revoked.
///
/// # Errors
///
/// Names the first check that failed; the launch is refused.
pub(super) fn verify_installed(
    catalog: &package_store::Catalog,
    app: &app_catalogue::apps::InstalledApp,
) -> Result<(), String> {
    let refuse = |why: String| format!("refusing to launch {}: {why}", app.app_id);
    let package = catalog
        .list()
        .map_err(|error| refuse(error.to_string()))?
        .into_iter()
        .find(|package| package.manifest_path == app.manifest_path)
        .ok_or_else(|| refuse("it is no longer installed".into()))?;
    verify_package(&package).map_err(refuse)
}
