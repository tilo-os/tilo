//! Private host configuration; no package key or model endpoint comes from task arguments.
use launcher::{
    managed::{InstalledManagedPackage, ManagedRuntime, PackageTrust},
    services::ManagedProviderSelection,
};
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
};
use tilo_action_broker::managed_host::ManagedHostInstallation;

pub(super) struct Installation {
    pub host: ManagedHostInstallation,
    pub receipts: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    store_root: PathBuf,
    package_id: String,
    trusted_app_publishers: Vec<String>,
    trusted_attestors: PathBuf,
    revoked_keys: PathBuf,
    runtime_root: PathBuf,
    runtime_publisher: String,
    sandbox_shim: PathBuf,
    bwrap: PathBuf,
    journal_root: PathBuf,
    receipts_root: PathBuf,
    data_home: PathBuf,
    provider: ManagedProviderSelection,
}
const MAX_BYTES: u64 = 65536;
pub(super) fn load(path: &Path) -> Result<Installation, String> {
    let mut input = private_fs::read(path).map_err(|error| error.to_string())?;
    if input.metadata().map_err(|error| error.to_string())?.len() > MAX_BYTES {
        return Err("managed task installation exceeds bound".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut input)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    drop(input);
    if u64::try_from(bytes.len()).map_err(|error| error.to_string())? > MAX_BYTES {
        return Err("managed task installation grew beyond bound".into());
    }
    let config: Config = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if config.version != 1 || config.trusted_app_publishers.len() > 16 {
        return Err("unsupported managed task installation".into());
    }
    for path in [
        &config.store_root,
        &config.trusted_attestors,
        &config.revoked_keys,
        &config.runtime_root,
        &config.sandbox_shim,
        &config.bwrap,
        &config.journal_root,
        &config.receipts_root,
        &config.data_home,
    ] {
        if !path.is_absolute() {
            return Err("managed installation paths must be absolute".into());
        }
    }
    let mut trust =
        PackageTrust::from_config_paths(&config.trusted_attestors, &config.revoked_keys)?;
    for publisher in &config.trusted_app_publishers {
        let key: [u8; 32] = hex::decode(publisher)
            .map_err(|_| "invalid independent publisher trust")?
            .try_into()
            .map_err(|_| "invalid independent publisher key length")?;
        if trust.publishers.contains(&key) {
            return Err("duplicate managed publisher trust".into());
        }
        trust.publishers.push(key);
    }
    let store = package_store::PackageStore::at(&config.store_root);
    let id = config
        .package_id
        .parse()
        .map_err(|_| "invalid managed package ID")?;
    let installed = store
        .registry()
        .map_err(|error| error.to_string())?
        .find("so.tilo.agent")
        .cloned()
        .ok_or("managed agent is not installed in the normal package store")?;
    let package = InstalledManagedPackage::verify(installed, config.store_root, id, &trust)?;
    let manifest = package.revalidate(&trust)?;
    let options = launcher::run::RunOptions {
        runtime_root: Some(config.runtime_root),
        require_runtime_publisher: Some(config.runtime_publisher),
        ..Default::default()
    };
    let runtime = ManagedRuntime::from_launch_options(
        &manifest,
        &options,
        config.sandbox_shim,
        config.bwrap,
    )?;
    let receipts = private_directory(&config.receipts_root)?;
    if receipts
        == config
            .journal_root
            .canonicalize()
            .map_err(|error| error.to_string())?
    {
        return Err(
            "invocation receipts and process journal require distinct private directories".into(),
        );
    }
    Ok(Installation {
        host: ManagedHostInstallation {
            package,
            trust,
            runtime,
            journal_root: config.journal_root,
            data_home: config.data_home,
            provider: config.provider,
        },
        receipts,
    })
}
pub(super) fn private_directory(path: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !path.is_absolute()
        || !meta.is_dir()
        || meta.mode() & 0o077 != 0
        || meta.uid() != unsafe { libc::geteuid() }
    {
        return Err("managed launch receipts require a private host directory".into());
    }
    path.canonicalize().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::PermissionsExt};

    #[test]
    fn private_startup_reader_refuses_oversized_unknown_and_public_configuration() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("installation.json");
        let mut file = private_fs::create_new(&path).unwrap();
        file.write_all(b"{\"version\":1,\"actor\":\"owner\"}")
            .unwrap();
        drop(file);
        assert!(load(&path).err().unwrap().contains("unknown field `actor`"));
        std::fs::write(&path, vec![b' '; usize::try_from(MAX_BYTES + 1).unwrap()]).unwrap();
        assert_eq!(
            load(&path).err().unwrap(),
            "managed task installation exceeds bound"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load(&path)
                .err()
                .unwrap()
                .contains("identity file is not private and user-owned")
        );
        let destination = root.path().join("alias.json");
        std::os::unix::fs::symlink(&path, &destination).unwrap();
        assert!(
            load(&destination)
                .err()
                .unwrap()
                .contains("Too many levels of symbolic links")
        );
    }

    #[test]
    fn receipt_directory_must_be_a_private_owned_directory_and_not_a_link() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            private_directory(root.path()).unwrap(),
            root.path().canonicalize().unwrap()
        );
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        assert!(private_directory(&alias).is_err());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(root.path()).is_err());
    }
}
