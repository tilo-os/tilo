//! Root-admitted receiving-namespace qualification; no live installer entry.
//! Reuses the existing receiving namespace guard/held-directory contract.
use super::{Connector, PasswordSealedAccounts, SourceRoots, discover};
use crate::setup::service_sessions::official_format_native as custody;
use accounts::{
    AccountStore, AccountSyncHealth, AccountSyncStatusStore, SecretCustody, SecretVault,
};
use cloud_source_dav::fake::{FakeDav, USER};
use fs::cloud::{CloudService, ProcessLauncher, ServiceConfig};
use serde::Deserialize;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::fd::FromRawFd,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const FS: &str = "/usr/bin/fs";
const HELPER: &str = "/usr/libexec/tilo-account-credential";
const ARTIFACTS: &str = "/run/tilo-cloud-destination-artifacts";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifacts {
    schema: u32,
    fs_sha256: String,
    helper_sha256: String,
}
fn executable(path: &str, expected: &str) -> File {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .expect("admitted cloud executable absent");
    assert_eq!(custody::held_executable_digest(&file), expected);
    file
}
fn artifacts() -> (Artifacts, File, File) {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(ARTIFACTS)
        .expect("cloud artifact admission absent");
    let before = file.metadata().unwrap();
    assert!(before.is_file() && before.uid() == 0 && before.nlink() == 1);
    assert_eq!(before.mode() & 0o7777, 0o444);
    assert!(before.len() > 0 && before.len() <= 1024);
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1025)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 1024);
    let after = file.metadata().unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime_nsec(),
            before.ctime_nsec()
        ),
        (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime_nsec(),
            after.ctime_nsec()
        )
    );
    let admitted: Artifacts = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(admitted.schema, 1);
    let fs = executable(FS, &admitted.fs_sha256);
    let helper = executable(HELPER, &admitted.helper_sha256);
    (admitted, fs, helper)
}
struct Service(CloudService);
impl Drop for Service {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
fn mounted(path: &Path) -> bool {
    let encoded = path
        .to_str()
        .unwrap()
        .replace('\\', "\\134")
        .replace(' ', "\\040");
    let file = File::open("/proc/self/mountinfo").unwrap();
    assert!(file.metadata().unwrap().len() <= 2 * 1024 * 1024);
    let mut bytes = String::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 2 * 1024 * 1024);
    bytes
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(encoded.as_str()))
}

#[test]
fn migrated_dav_grant_default_helper_promotes_system_custody_and_mounts_real_files() {
    let owner = custody::guarded_owner();
    for forbidden in [runtime_identity::FILE_SECRETS_ENV, "TILO_SECRETS_DIR"] {
        assert!(
            std::env::var_os(forbidden).is_none(),
            "file-secret override refused"
        );
    }
    assert_eq!(
        std::env::var("TILO_SECRETS_SYSTEM_TEST").as_deref(),
        Ok("1")
    );
    let store = tilo_secrets::SecretStore::for_app("Tilo");
    assert_eq!(store.protection(), tilo_secrets::Protection::OsKeychain);
    store
        .probe_blocking()
        .expect("actual private Secret Service unavailable");
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let fixture = home.parent().unwrap();
    assert!(fixture.parent() == Some(Path::new("/var/tmp")));
    assert!(
        fixture
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("tilo-receiver-")
    );
    let facts = std::fs::symlink_metadata(fixture).unwrap();
    assert!(
        facts.is_dir()
            && facts.uid() == 0
            && facts.gid() == owner
            && facts.mode() & 0o7777 == 0o750
    );
    let target = accounts::config_root().unwrap();
    assert_eq!(target, fixture.join("config/tilo"));
    let destination = custody::held_directory(86, &target, owner);
    assert!(AccountStore::load().is_empty());
    let (admitted, fs_file, helper_file) = artifacts();
    let fuse = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .expect("actual FUSE device unavailable");
    assert!(std::os::unix::fs::FileTypeExt::is_char_device(
        &fuse.metadata().unwrap().file_type()
    ));

    let server = FakeDav::generic();
    server.put("Documents/hello.txt", b"actual receiving read");
    let source = tempfile::tempdir_in(&home).unwrap();
    let config = source.path().join(".config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let source_file = config.join("rclone/rclone.conf");
    let source_bytes = format!(
        "[Work files]\ntype=webdav\nurl={}\nuser={USER}\npass=AAECAwQFBgcICQoLDA0OD2LwFVdisTSqpU56FmE\n",
        server.root_url()
    );
    std::fs::write(&source_file, &source_bytes).unwrap();
    let roots = SourceRoots {
        home: source.path().to_owned(),
        config,
    };
    let inventory = discover(&roots);
    assert_eq!(inventory.candidates.len(), 1);
    let stage = source.path().join("stage");
    let connector = Connector::new(stage.clone());
    let id = connector
        .import_rclone(&roots, &inventory.candidates[0])
        .unwrap();
    let before = std::fs::read(accounts::vault_dir_in(&stage).join("vault.enc")).unwrap();
    let artifact = connector
        .seal_selected_accounts_with_password(
            std::slice::from_ref(&id),
            b"native-dav-transport-fixture-password",
            b"native-receiving-dav-fixture",
        )
        .unwrap();
    let artifact = PasswordSealedAccounts::from_bytes(artifact.as_bytes()).unwrap();
    Connector::new(target.clone())
        .import_password_sealed_accounts_matching(
            &artifact,
            b"native-dav-transport-fixture-password",
            b"native-receiving-dav-fixture",
            std::slice::from_ref(&id),
        )
        .unwrap();
    // Observe the genuine offline-import boundary before default-session use.
    assert_eq!(
        SecretVault::open_read_only_in(&target).custody(),
        SecretCustody::LocalFile
    );
    let local_key = accounts::vault_dir_in(&target).join("vault.key");
    assert!(local_key.is_file());
    let cipher_before = std::fs::read(accounts::vault_dir_in(&target).join("vault.enc")).unwrap();
    let handoff = accounts::HandoffStore::open().unwrap();
    assert!(handoff.read(&id).unwrap().is_none());
    let mount_root = home.join("Remote");
    let state_root = home.join("cloud-state");
    let mount = mount_root.join("Work files");
    assert!(!mounted(&mount));
    let mut service = Service(CloudService::new(
        ServiceConfig {
            config_root: None,
            mounts_root: mount_root,
            state_root: state_root.clone(),
            control_socket: None,
            file_socket: None,
            refresher: Arc::new(accounts::CommandRefresher::new(None).with_program(HELPER)),
        },
        Box::new(ProcessLauncher {
            executable: FS.into(),
            ready_timeout: Duration::from_secs(30),
        }),
    ));
    service.0.reconcile();
    assert_eq!(service.0.mounted(), vec![(id.to_string(), mount.clone())]);
    assert!(service.0.wait_for_health(
        id.as_str(),
        fs::cloud::volume::Health::Healthy,
        Duration::from_secs(30)
    ));
    assert!(mounted(&mount));
    assert!(
        !local_key.exists(),
        "default helper must retire the migrated local key"
    );
    let live = SecretVault::open();
    assert_eq!(live.custody(), SecretCustody::System);
    assert!(live.recovery_error().is_none());
    assert!(account_custody::is_copied_secret(&live.get(&id).unwrap()));
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&target).join("vault.enc")).unwrap(),
        cipher_before
    );
    assert!(handoff.read(&id).unwrap().is_some());
    assert!(
        !accounts::handoff::handoff_dir_in(&target)
            .join("vault.key")
            .exists()
    );
    assert_eq!(
        std::fs::read(mount.join("Documents/hello.txt")).unwrap(),
        b"actual receiving read"
    );
    std::fs::write(
        mount.join("Documents/from-tilo.txt"),
        b"actual receiving write",
    )
    .unwrap();
    server.wait_for(|request| {
        request.method == "PUT" && request.target.ends_with("/Documents/from-tilo.txt")
    });
    assert_eq!(
        server.get("Documents/from-tilo.txt").unwrap(),
        b"actual receiving write"
    );
    service.0.pump();
    assert_eq!(
        AccountSyncStatusStore::load().get(&id).unwrap().health,
        AccountSyncHealth::Healthy
    );
    let mut accounts = AccountStore::load();
    accounts.remove(&id);
    accounts.save().unwrap();
    service.0.reconcile();
    assert!(service.0.mounted().is_empty());
    assert!(!mounted(&mount));
    assert!(!mount.exists());
    assert!(state_root.read_dir().unwrap().next().is_none());
    service.0.shutdown();
    assert_eq!(
        std::fs::read(&source_file).unwrap(),
        source_bytes.as_bytes()
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&stage).join("vault.enc")).unwrap(),
        before
    );
    assert_eq!(
        custody::held_executable_digest(&fs_file),
        admitted.fs_sha256
    );
    assert_eq!(
        custody::held_executable_digest(&helper_file),
        admitted.helper_sha256
    );
    let named = File::open(&target).unwrap().metadata().unwrap();
    let held = destination.metadata().unwrap();
    assert_eq!((held.dev(), held.ino()), (named.dev(), named.ino()));
    assert!(
        unsafe { libc::fcntl(87, libc::F_GETFD) } >= 0,
        "closed cloud receipt handle absent"
    );
    let mut receipt = unsafe { File::from_raw_fd(87) };
    let public = serde_json::json!({"schema":1,"qualification":"migrated-dav-default-session",
        "migrated_grant_verified":true,"default_system_custody_verified":true,"local_key_retired":true,
        "real_helper_verified":true,"kernel_read_write_verified":true,"account_removal_unmounted":true,
        "source_unchanged":true,"provider_authentication_verified":false});
    let bytes = serde_json::to_vec(&public).unwrap();
    assert!(bytes.len() <= 1024);
    receipt.write_all(&bytes).unwrap();
    receipt.flush().unwrap();
}
