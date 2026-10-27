//! The runtime bundle an entrypoint executes a binary of (`[entrypoint].runtime`):
//! resolved beside the optional runtimes, and its binary's libraries checked as the
//! sandboxed loader looks for them.
use std::path::{Path, PathBuf};

use super::{
    ManifestView, RunError, RunOptions, find_runtime_bundle, launch_entry_bin,
    resolve_optional_runtimes,
};

/// The runtime bundle whose binary the entrypoint executes
/// (`[entrypoint].runtime`), installed and verified. A launch cannot start
/// without it, so a missing or refused bundle refuses the launch.
pub(super) fn resolve_entrypoint_runtime(
    manifest: &ManifestView,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    let Some(raw) = manifest.entrypoint.runtime.as_deref() else {
        return Ok(None);
    };
    match find_runtime_bundle(raw, opts)? {
        (Some(bundle), _) => Ok(Some(bundle)),
        (None, searched) => Err(RunError::EntrypointRuntimeMissing {
            package: manifest.package_id.clone(),
            runtime: raw.to_owned(),
            searched,
        }),
    }
}

/// Every further runtime bundle a launch mounts: the optional runtimes that
/// are installed and verify, and the entrypoint's runtime, which must be.
pub(super) fn resolve_launch_runtimes(
    manifest: &ManifestView,
    opts: &RunOptions,
) -> Result<Vec<(PathBuf, String)>, RunError> {
    let mut bundles = resolve_optional_runtimes(manifest, opts);
    bundles.extend(resolve_entrypoint_runtime(manifest, opts)?);
    Ok(bundles)
}

/// [`preflight_runtime_libraries`] for an entrypoint that executes a binary
/// of its runtime bundle: the binary must find its libraries in that bundle,
/// the app's runtime or the system, exactly as the sandboxed loader looks.
pub(super) fn preflight_entrypoint_runtime(
    manifest: &ManifestView,
    opts: &RunOptions,
    runtime: Option<(&Path, &str)>,
    bundles: &[(PathBuf, String)],
) -> Result<(), RunError> {
    let Some(raw) = manifest.entrypoint.runtime.as_deref() else {
        return Ok(());
    };
    let Some((_, entry_version)) = raw.split_once('/') else {
        return Err(RunError::RuntimeReferenceMalformed(raw.to_owned()));
    };
    let Some((entry_root, _)) = bundles.iter().find(|(_, version)| version == entry_version) else {
        return Err(RunError::EntrypointRuntimeMissing {
            package: manifest.package_id.clone(),
            runtime: raw.to_owned(),
            searched: Vec::new(),
        });
    };
    let Some(entry_bin) = launch_entry_bin(manifest, &opts.mode) else {
        return Ok(());
    };
    let entry_mount = crate::sandbox::runtime_mount_for(entry_version);
    let mut mounts = vec![(entry_mount.clone(), entry_root.clone())];
    let mut library_path = Vec::new();
    if let Some((root, version)) = runtime {
        // The app's own runtime: what `LD_LIBRARY_PATH` points into inside
        // the sandbox.
        mounts.push((
            crate::sandbox::runtime_mount_for(version),
            root.to_path_buf(),
        ));
        library_path.push(crate::sandbox::runtime_library_path(version));
    }
    let view = crate::loader_preflight::LoaderView::new(
        mounts,
        library_path,
        crate::loader_preflight::LdCache::load(Path::new(crate::loader_preflight::LD_SO_CACHE)),
        crate::loader_preflight::SYSTEM_LIBRARY_DIRS
            .iter()
            .map(PathBuf::from)
            .collect(),
    );
    view.check(&entry_mount.join(entry_bin)).map_err(|missing| {
        tracing::error!(
            package = %manifest.package_id,
            library = %missing.library,
            needed_by = %missing.needed_by.display(),
            "the entrypoint runtime's binary links against a library nothing provides"
        );
        RunError::RuntimeLibraryMissing {
            package: manifest.package_id.clone(),
            runtime: raw.to_owned(),
            library: missing.library,
            searched: missing.searched,
        }
    })
}
