//! Exact selected SDK closures, not overlapping old app/tool/publisher layers.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use runtime_bundle::component_pins::RetainedBundleTrust;
use runtime_bundle::{BundleManifest, VerifyOptions};

use crate::error::DaemonError;

fn invalid(error: impl std::fmt::Display) -> DaemonError {
    DaemonError::InvalidParams(format!("selected runtime retention: {error}"))
}

fn rooted_file(root: &Path, relative: &Path, bound: u64) -> Result<Vec<u8>, DaemonError> {
    use std::io::Read as _;
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).map_err(invalid)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || !path
            .canonicalize()
            .map_err(invalid)?
            .starts_with(root.canonicalize().map_err(invalid)?)
    {
        return Err(invalid("prior trust material is not root-controlled"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(invalid)?
        .take(bound.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    if u64::try_from(bytes.len()).map_err(invalid)? > bound {
        return Err(invalid("prior trust material exceeds bound"));
    }
    Ok(bytes)
}

fn prior_trust(root: &Path, manifest: &BundleManifest) -> Result<bool, DaemonError> {
    let publisher = manifest
        .publisher_hex
        .as_deref()
        .ok_or_else(|| invalid("unsigned runtime"))?;
    let operator = match fs::read_to_string(runtime_bundle::TRUSTED_PUBLISHERS_FILE) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(invalid(error)),
    };
    let mut trusted = operator
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|key| key.eq_ignore_ascii_case(publisher));
    for name in [
        "runtime-bundle-publisher.hex",
        "browser-runtime-publisher.hex",
        "registry-runtime-publisher.hex",
    ] {
        let relative = Path::new("usr/share/tilo").join(name);
        if root.join(&relative).try_exists().map_err(invalid)? {
            let bytes = rooted_file(root, &relative, 258)?;
            let key = std::str::from_utf8(&bytes).map_err(invalid)?.trim();
            if key.is_empty() && name == "browser-runtime-publisher.hex" {
                continue;
            }
            if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid("malformed prior runtime publisher"));
            }
            trusted |= key.eq_ignore_ascii_case(publisher);
        }
    }
    let proof =
        RetainedBundleTrust::from_verified(manifest).ok_or_else(|| invalid("unsigned runtime"))?;
    let relative = proof
        .installed_path()
        .strip_prefix("/")
        .map_err(invalid)?
        .to_owned();
    if root.join(&relative).try_exists().map_err(invalid)? {
        let rows: Vec<RetainedBundleTrust> =
            serde_json::from_slice(&rooted_file(root, &relative, 8192)?).map_err(invalid)?;
        if rows.is_empty()
            || rows.len() > 32
            || rows.iter().any(|row| row.version != manifest.version)
        {
            return Err(invalid("malformed prior scoped runtime authority"));
        }
        trusted |= rows.iter().any(|row| row.matches(manifest));
    }
    Ok(trusted)
}

/// Build a runtime-only snapshot from the canonical selections. Each byte is
/// reverified after copying; user-owned input cannot change an admitted closure.
pub(super) fn prepare(
    staging: &Path,
    prior: &Path,
    candidate: &Path,
    selected: &crate::cohort::SelectedApps,
) -> Result<Option<PathBuf>, DaemonError> {
    let versions: BTreeSet<_> = selected
        .apps
        .iter()
        .filter_map(|row| row.runtime.strip_prefix("tilo-runtime/"))
        .collect();
    if versions.len() > 512 {
        return Err(invalid("selected runtime count exceeds bound"));
    }
    let stage = staging.join(format!(
        "runtime-retention-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&stage).map_err(invalid)?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).map_err(invalid)?;
    let result = prepare_into(&stage, prior, candidate, selected, &versions);
    match result {
        Ok(true) => Ok(Some(stage)),
        Ok(false) => {
            fs::remove_dir_all(stage).map_err(invalid)?;
            Ok(None)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(stage);
            Err(error)
        }
    }
}

fn prepare_into(
    stage: &Path,
    prior: &Path,
    candidate: &Path,
    selected: &crate::cohort::SelectedApps,
    versions: &BTreeSet<&str>,
) -> Result<bool, DaemonError> {
    let mut records: BTreeMap<PathBuf, Vec<RetainedBundleTrust>> = BTreeMap::new();
    for version in versions {
        if Path::new(version).components().count() != 1
            || !matches!(
                Path::new(version).components().next(),
                Some(Component::Normal(_))
            )
        {
            return Err(invalid(
                "selected version is not one confined path component",
            ));
        }
        let relative = Path::new("usr/lib/tilo-runtime").join(version);
        let user = selected.runtime_paths.get(*version);
        // Use the maintained account resolver, including nondefault XDG roots.
        let old = if let Some(path) = user {
            path.clone()
        } else if candidate.join(&relative).exists() {
            verify_admitted(candidate, &candidate.join(&relative), version)?;
            continue;
        } else {
            prior.join(&relative)
        };
        let manifest = runtime_bundle::verify_bundle_with(
            &old,
            &VerifyOptions {
                require_signature: true,
                require_arch: Some(std::env::consts::ARCH.into()),
                ..Default::default()
            },
        )
        .map_err(invalid)?;
        if manifest.version != *version || !prior_trust(prior, &manifest)? {
            return Err(invalid(
                "selected runtime lacks exact prior admitted authority",
            ));
        }
        let proof = RetainedBundleTrust::from_verified(&manifest)
            .ok_or_else(|| invalid("unsigned runtime"))?;
        records
            .entry(
                proof
                    .installed_path()
                    .strip_prefix("/")
                    .map_err(invalid)?
                    .to_owned(),
            )
            .or_default()
            .push(proof);
        if !candidate.join(&relative).exists() {
            let target = stage.join(&relative);
            fs::create_dir_all(&target).map_err(invalid)?;
            crate::fsutil::copy_tree(&old, &target).map_err(invalid)?;
            // Signed file bytes become immutable system data; mutable user
            // permission bits are not carried into the root-owned namespace.
            let mut paths = Vec::new();
            tilo_vm_protocol::layer_hash::walk(&target, Path::new(""), &mut paths)
                .map_err(invalid)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).map_err(invalid)?;
            for relative in paths {
                let path = target.join(relative);
                let metadata = fs::symlink_metadata(&path).map_err(invalid)?;
                let mode = if metadata.is_dir() {
                    0o755
                } else if metadata.is_file() {
                    if metadata.mode() & 0o111 == 0 {
                        0o644
                    } else {
                        0o755
                    }
                } else {
                    return Err(invalid(
                        "runtime retention cannot contain symlinks or special files",
                    ));
                };
                fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(invalid)?;
            }
            let copied = runtime_bundle::verify_bundle_with(
                &target,
                &VerifyOptions {
                    require_publisher: manifest.publisher_hex.clone(),
                    require_signature: true,
                    require_arch: Some(std::env::consts::ARCH.into()),
                },
            )
            .map_err(invalid)?;
            if copied.root_hash != manifest.root_hash || copied.version != manifest.version {
                return Err(invalid("selected runtime changed during retention"));
            }
        }
    }
    for (relative, records) in &records {
        let path = stage.join(relative);
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| invalid("missing retention parent"))?,
        )
        .map_err(invalid)?;
        fs::write(&path, serde_json::to_vec(records).map_err(invalid)?).map_err(invalid)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).map_err(invalid)?;
    }
    Ok(!records.is_empty())
}

/// Revalidate actual post-selection references in the admitted combined view.
/// An intervening runtime replacement cannot borrow an earlier exact proof.
pub(super) fn validate_current(
    view: &Path,
    selected: &crate::SelectedApps,
) -> Result<(), DaemonError> {
    let versions: BTreeSet<_> = selected
        .apps
        .iter()
        .filter_map(|row| row.runtime.strip_prefix("tilo-runtime/"))
        .collect();
    for version in versions {
        if Path::new(version).components().count() != 1
            || !matches!(
                Path::new(version).components().next(),
                Some(Component::Normal(_))
            )
        {
            return Err(invalid("selected runtime version is not confined"));
        }
        let path = selected
            .runtime_paths
            .get(version)
            .cloned()
            .unwrap_or_else(|| view.join("usr/lib/tilo-runtime").join(version));
        verify_admitted(view, &path, version)?;
    }
    Ok(())
}

fn verify_admitted(view: &Path, path: &Path, version: &str) -> Result<BundleManifest, DaemonError> {
    let manifest = runtime_bundle::verify_bundle_with(
        path,
        &VerifyOptions {
            require_signature: true,
            require_arch: Some(std::env::consts::ARCH.into()),
            ..Default::default()
        },
    )
    .map_err(invalid)?;
    if manifest.version != version || !prior_trust(view, &manifest)? {
        return Err(invalid("selected runtime is not an admitted closure"));
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use package_store::registry::Registry;
    use std::process::Command;

    fn bundle(root: &Path, seed: &str, payload: &[u8]) -> BundleManifest {
        let host = root.join("host");
        fs::create_dir_all(&host).unwrap();
        let library = root.join("library.so");
        fs::write(&library, payload).unwrap();
        let key = root.join("key.json");
        fs::write(
            &key,
            serde_json::json!({"secret_key_hex":seed.repeat(32)}).to_string(),
        )
        .unwrap();
        let (manifest, _) = runtime_bundle::build_abi_bundle(
            &root.join("usr/lib/tilo-runtime"),
            "2026.07",
            &runtime_bundle::BuildOptions {
                from: Some(&host),
                identity: Some(&key),
                extra_libs: &[library],
                ..Default::default()
            },
        )
        .unwrap();
        let pin = root.join("usr/share/tilo/runtime-bundle-publisher.hex");
        fs::create_dir_all(pin.parent().unwrap()).unwrap();
        fs::write(pin, manifest.publisher_hex.as_ref().unwrap()).unwrap();
        manifest
    }

    fn same_abi_case(
        root: &Path,
        prior: &Path,
        old: &BundleManifest,
        partial: &crate::SelectedApps,
    ) {
        // Same ABI bytes signed by a different legitimate publisher:
        // preserve the selected user bundle's exact authority without
        // shadowing the candidate system bundle at that same version.
        let same = root.join("same-candidate");
        let same_manifest = bundle(&same, "22", b"old exact SDK");
        assert_eq!(same_manifest.version, old.version);
        assert_ne!(same_manifest.publisher_hex, old.publisher_hex);
        let home = root.join("user-home");
        let user_bundle = home.join(".local/share/tilo-runtime").join(&old.version);
        fs::create_dir_all(&user_bundle).unwrap();
        crate::fsutil::copy_tree(
            &prior.join("usr/lib/tilo-runtime").join(&old.version),
            &user_bundle,
        )
        .unwrap();
        let selected_user = crate::cohort::SelectedApps {
            apps: partial.apps[..1].to_vec(),
            runtime_paths: BTreeMap::from([(old.version.clone(), user_bundle)]),
        };
        let retained = prepare(&root.join("staging"), prior, &same, &selected_user)
            .unwrap()
            .unwrap();
        assert!(!retained.join("usr/lib/tilo-runtime").exists());
        let old_proof = RetainedBundleTrust::from_verified(old).unwrap();
        let new_proof = RetainedBundleTrust::from_verified(&same_manifest).unwrap();
        assert_ne!(old_proof.installed_path(), new_proof.installed_path());
        assert!(
            retained
                .join(old_proof.installed_path().strip_prefix("/").unwrap())
                .is_file()
        );
        assert!(
            !retained
                .join(new_proof.installed_path().strip_prefix("/").unwrap())
                .exists()
        );
    }

    fn selected_fixture(
        root: &Path,
    ) -> (
        PathBuf,
        PathBuf,
        BundleManifest,
        BundleManifest,
        crate::SelectedApps,
    ) {
        let prior = root.join("prior");
        let candidate = root.join("candidate");
        let old = bundle(&prior, "11", b"old exact SDK");
        let new = bundle(&candidate, "22", b"new exact SDK");
        assert_ne!(old.version, new.version);
        assert_ne!(old.publisher_hex, new.publisher_hex);
        crate::cohort::tests::fixture_pair(&root.join("packages"));
        let mut rows =
            Registry::load(&root.join("packages/layer/usr/share/tilo-apps/registry.toml"))
                .unwrap()
                .into_entries();
        rows[0].held = true;
        rows[0].runtime = format!("tilo-runtime/{}", old.version);
        rows[1].runtime = format!("tilo-runtime/{}", new.version);
        let user = root.join("selected");
        fs::create_dir_all(&user).unwrap();
        let mut registry = Registry::default();
        for row in rows {
            registry.upsert(row);
        }
        crate::cohort::tests::save(&registry, &user.join("registry.toml"));
        let selected = crate::cohort::SelectedApps {
            apps: package_store::Catalog::at(&user, None).list().unwrap(),
            runtime_paths: BTreeMap::new(),
        };
        (prior, candidate, old, new, selected)
    }

    fn run_retention_cases(root: &Path) {
        let (prior, candidate, old, new, selected) = selected_fixture(root);
        assert!(selected.apps[0].held);
        let stage = prepare(&root.join("staging"), &prior, &candidate, &selected)
            .unwrap()
            .unwrap();
        assert!(
            stage
                .join("usr/lib/tilo-runtime")
                .join(&old.version)
                .is_dir()
        );
        assert!(
            !stage
                .join("usr/lib/tilo-runtime")
                .join(&new.version)
                .exists()
        );
        assert!(!stage.join("usr/share/tilo-apps").exists());
        assert!(
            !stage
                .join("usr/share/tilo/runtime-bundle-publisher.hex")
                .exists()
        );
        let proof = RetainedBundleTrust::from_verified(&old).unwrap();
        let retained: Vec<RetainedBundleTrust> = serde_json::from_slice(
            &fs::read(stage.join(proof.installed_path().strip_prefix("/").unwrap())).unwrap(),
        )
        .unwrap();
        assert_eq!(retained, vec![proof]);
        // An interrupted selection keeps its still-running old nonheld row,
        // and the same exact closure survives regardless of sysext order.
        let mut partial = selected;
        partial.apps[0].held = false;
        let second = prepare(&root.join("staging"), &prior, &candidate, &partial)
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::layers::tree_hash(&stage).unwrap(),
            crate::layers::tree_hash(&second).unwrap()
        );
        same_abi_case(root, &prior, &old, &partial);
        fs::write(
            prior.join("usr/share/tilo/runtime-bundle-publisher.hex"),
            new.publisher_hex.as_ref().unwrap(),
        )
        .unwrap();
        assert!(prepare(&root.join("staging"), &prior, &candidate, &partial).is_err());
        fs::write(
            prior.join("usr/share/tilo/runtime-bundle-publisher.hex"),
            old.publisher_hex.as_ref().unwrap(),
        )
        .unwrap();
        let unauthorized = candidate.join("usr/lib/tilo-runtime").join(&old.version);
        fs::create_dir_all(&unauthorized).unwrap();
        crate::fsutil::copy_tree(
            &prior.join("usr/lib/tilo-runtime").join(&old.version),
            &unauthorized,
        )
        .unwrap();
        assert!(
            prepare(&root.join("staging"), &prior, &candidate, &partial).is_err(),
            "unadmitted extra runtime in a candidate layer must fail before mutation"
        );
        fs::remove_dir_all(&unauthorized).unwrap();
        // Actual combined readonly-view contents admit both retained old
        // and candidate new closures, and reject an intervening forgery.
        let mut paths = Vec::new();
        tilo_vm_protocol::layer_hash::walk(&stage, Path::new(""), &mut paths).unwrap();
        for relative in paths {
            let from = stage.join(&relative);
            let to = candidate.join(&relative);
            if from.is_dir() {
                fs::create_dir_all(to).unwrap();
            } else {
                fs::create_dir_all(to.parent().unwrap()).unwrap();
                fs::copy(from, to).unwrap();
            }
        }
        validate_current(&candidate, &partial).unwrap();
        let mut replaced = crate::SelectedApps {
            apps: partial.apps.clone(),
            runtime_paths: partial.runtime_paths.clone(),
        };
        replaced.runtime_paths.insert(
            old.version,
            candidate.join("usr/lib/tilo-runtime").join(&new.version),
        );
        assert!(validate_current(&candidate, &replaced).is_err());
    }

    #[test]
    fn held_and_partial_selections_retain_only_their_exact_runtime_and_trust() {
        const CASE: &str = "TILO_TEST_EXACT_RUNTIME_RETENTION";
        if let Ok(root) = std::env::var(CASE) {
            run_retention_cases(&PathBuf::from(root));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = Command::new("timeout").args(["30", "bwrap", "--die-with-parent", "--unshare-all", "--uid", "0", "--gid", "0", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"])
            .arg("--bind").arg(root.path()).arg(root.path()).args(["--setenv", CASE]).arg(root.path())
            .arg("--").arg(std::env::current_exe().unwrap()).args(["--exact", "daemon::runtime_retention::tests::held_and_partial_selections_retain_only_their_exact_runtime_and_trust", "--nocapture"]).output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
