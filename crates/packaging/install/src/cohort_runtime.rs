//! Preserve verified SDK files before a cohort can remove their system view.
//! Copying a bundle never changes publisher trust; a target view must admit it.

use std::io::Read;
use std::path::Path;

use super::Error;

const MAX_RETAINED_BYTES: u64 = 1024 * 1024 * 1024;

pub(super) fn retain(reference: &str) -> Result<(), Error> {
    if reference.is_empty() {
        return Ok(());
    }
    let version = reference
        .strip_prefix("tilo-runtime/")
        .filter(|version| {
            !version.is_empty()
                && version.len() <= 128
                && !version.starts_with('.')
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
        })
        .ok_or_else(|| Error::Policy("invalid retained SDK reference".into()))?;
    let source = runtime_bundle::resolve_bundle_path(version)
        .ok_or_else(|| Error::Policy("the selected package's SDK is unavailable".into()))?;
    let destination = runtime_bundle::default_bundle_path(version)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
    retain_at(&source, &destination, version)
}

fn retain_at(source: &Path, destination: &Path, version: &str) -> Result<(), Error> {
    super::check_runtime_bundle(source, version)?;
    let options = runtime_bundle::VerifyOptions {
        require_publisher: None,
        require_signature: true,
        require_arch: Some(runtime_bundle::host_arch().into()),
    };
    let before = runtime_bundle::verify_bundle_with(source, &options)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
    let size = before
        .files
        .iter()
        .try_fold(0u64, |sum, file| sum.checked_add(file.size))
        .filter(|size| *size <= MAX_RETAINED_BYTES)
        .ok_or_else(|| Error::Policy("retained SDK exceeds its storage bound".into()))?;
    if source == destination {
        return Ok(());
    }
    if destination.exists() {
        super::check_runtime_bundle(destination, version)?;
        let existing = runtime_bundle::verify_bundle_with(destination, &options)
            .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
        if existing.root_hash != before.root_hash {
            return Err(Error::Policy(
                "the existing SDK differs from the retained build".into(),
            ));
        }
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| Error::Policy("invalid SDK destination".into()))?;
    std::fs::create_dir_all(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    let mut copied = 0u64;
    let mut directories = std::collections::BTreeSet::new();
    directories.insert(stage.path().to_path_buf());
    for file in &before.files {
        let path = stage.path().join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            let mut directory = Some(parent);
            while let Some(parent) = directory.filter(|path| path.starts_with(stage.path())) {
                directories.insert(parent.to_path_buf());
                directory = parent.parent();
            }
        }
        let input = std::fs::File::open(source.join(&file.path))?;
        let permissions = input.metadata()?.permissions();
        let mut input = input.take(file.size.saturating_add(1));
        let mut output = std::fs::File::create(&path)?;
        output.set_permissions(permissions)?;
        let count = std::io::copy(&mut input, &mut output)?;
        if count != file.size {
            return Err(Error::Policy("SDK changed while it was retained".into()));
        }
        copied = copied
            .checked_add(count)
            .filter(|bytes| *bytes <= MAX_RETAINED_BYTES)
            .ok_or_else(|| Error::Policy("SDK changed beyond its storage bound".into()))?;
        std::fs::File::open(path)?.sync_all()?;
    }
    std::fs::copy(
        source.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME),
        stage.path().join(runtime_bundle::BUNDLE_MANIFEST_FILENAME),
    )?;
    super::check_runtime_bundle(stage.path(), version)?;
    let after = runtime_bundle::verify_bundle_with(stage.path(), &options)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
    if after != before || copied != size {
        return Err(Error::Policy("SDK changed while it was retained".into()));
    }
    std::fs::File::open(stage.path().join(runtime_bundle::BUNDLE_MANIFEST_FILENAME))?.sync_all()?;
    let mut directories = directories.into_iter().collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        std::fs::File::open(directory)?.sync_all()?;
    }
    std::fs::rename(stage.path(), destination)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_sdk_survives_system_view_removal_without_changing_trust() {
        let root = tempfile::tempdir().unwrap();
        let identity = runtime_bundle::Identity::generate();
        let identity_path = root.path().join("identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let publishers = root.path().join("publishers");
        std::fs::write(&publishers, identity.public_key_hex()).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &publishers);
        let source = root.path().join("system-sdk");
        let manifest = runtime_bundle::build_bundle_with(
            &source,
            "cohort-inverse-sdk",
            &runtime_bundle::BuildOptions {
                from: None,
                arch: None,
                identity: Some(&identity_path),
                extra_libs: &[],
            },
        )
        .unwrap();
        let destination = root.path().join("user-sdk");
        retain_at(&source, &destination, &manifest.version).unwrap();
        for file in &manifest.files {
            assert_eq!(
                std::fs::metadata(source.join(&file.path))
                    .unwrap()
                    .permissions(),
                std::fs::metadata(destination.join(&file.path))
                    .unwrap()
                    .permissions(),
            );
        }
        std::fs::remove_dir_all(&source).unwrap();
        super::super::check_runtime_bundle(&destination, &manifest.version).unwrap();
        assert_eq!(
            runtime_bundle::verify_bundle_with(
                &destination,
                &runtime_bundle::VerifyOptions {
                    require_publisher: Some(identity.public_key_hex()),
                    require_signature: true,
                    require_arch: Some(runtime_bundle::host_arch().into()),
                },
            )
            .unwrap(),
            manifest
        );
        retain_at(&destination, &destination, &manifest.version).unwrap();
        let first_file = &manifest.files[0].path;
        std::fs::write(destination.join(first_file), b"corrupt retained SDK").unwrap();
        assert!(retain_at(&destination, &destination, &manifest.version).is_err());
        assert_eq!(
            std::fs::read(destination.join(first_file)).unwrap(),
            b"corrupt retained SDK"
        );
    }

    #[test]
    fn retained_sdk_refuses_untrusted_source_before_creating_destination() {
        let root = tempfile::tempdir().unwrap();
        let identity = runtime_bundle::Identity::generate();
        let identity_path = root.path().join("identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let publishers = root.path().join("publishers");
        std::fs::write(
            &publishers,
            runtime_bundle::Identity::generate().public_key_hex(),
        )
        .unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &publishers);
        let source = root.path().join("system-sdk");
        let manifest = runtime_bundle::build_bundle_with(
            &source,
            "cohort-inverse-sdk",
            &runtime_bundle::BuildOptions {
                from: None,
                arch: None,
                identity: Some(&identity_path),
                extra_libs: &[],
            },
        )
        .unwrap();
        let destination = root.path().join("user-sdk");
        assert!(retain_at(&source, &destination, &manifest.version).is_err());
        assert!(!destination.exists());
        assert!(runtime_bundle::verify_bundle(&source).is_ok());
    }
}
