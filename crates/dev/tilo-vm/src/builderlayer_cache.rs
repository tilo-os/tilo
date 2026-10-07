//! Immutable public component artifacts, confined to one owned Cargo target.
//! Only a verified complete `SourceSnapshot` plus actual compiled inputs enables
//! this cache. No signing keys are retained and no device trust is changed.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::builderlayer::Packed;
use crate::error::CliError;

const MAX_LAYER: u64 = 4 << 30;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    key: String,
    packed: Packed,
    hash: String,
    bytes: u64,
}

pub(crate) struct Artifact {
    pub file: File,
    pub hash: String,
    pub bytes: u64,
}

pub(crate) struct Cache {
    root: PathBuf,
    key: String,
    owner: u32,
    _lock: advisory_lock::FileLock,
}

fn refuse() -> CliError {
    CliError::InvalidInput("owned component cache is malformed, modified or foreign".into())
}

#[cfg(unix)]
fn metadata(
    path: &Path,
    owner: u32,
    mode: u32,
    directory: bool,
) -> Result<std::fs::Metadata, CliError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let value = std::fs::symlink_metadata(path).map_err(CliError::io("inspect component cache"))?;
    if value.uid() != owner
        || value.permissions().mode() & 0o777 != mode
        || value.is_dir() != directory
        || (!directory && (!value.is_file() || value.nlink() != 1))
    {
        return Err(refuse());
    }
    Ok(value)
}

#[cfg(unix)]
fn open(path: &Path, owner: u32, mode: u32) -> Result<File, CliError> {
    use std::os::unix::fs::MetadataExt as _;
    let before = metadata(path, owner, mode, false)?;
    let file = File::open(path).map_err(CliError::io("open component cache"))?;
    let after = file
        .metadata()
        .map_err(CliError::io("inspect opened component cache"))?;
    if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
        return Err(refuse());
    }
    Ok(file)
}

fn fingerprint(file: &mut File) -> Result<(String, u64), CliError> {
    use std::os::unix::fs::MetadataExt as _;
    let before = file
        .metadata()
        .map_err(CliError::io("inspect cached layer"))?;
    let mut hash = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = vec![0_u8; 64 << 10];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(CliError::io("hash cached layer"))?;
        if read == 0 {
            break;
        }
        count = count.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if count > MAX_LAYER {
            return Err(refuse());
        }
        hash.update(buffer.get(..read).ok_or_else(refuse)?);
    }
    let after = file
        .metadata()
        .map_err(CliError::io("reinspect cached layer"))?;
    if before.len() != count
        || after.len() != count
        || (
            before.dev(),
            before.ino(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
            before.nlink(),
        ) != (
            after.dev(),
            after.ino(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
            after.nlink(),
        )
    {
        return Err(refuse());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(CliError::io("rewind cached layer"))?;
    Ok((hex::encode(hash.finalize()), count))
}

#[cfg(unix)]
impl Cache {
    pub(crate) fn new(target: &Path, key: &str) -> Result<Self, CliError> {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
        if key.len() != 64
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(refuse());
        }
        let owner = std::fs::metadata("/proc/self")
            .map_err(CliError::io("inspect cache owner"))?
            .uid();
        let target_meta =
            std::fs::symlink_metadata(target).map_err(CliError::io("inspect Cargo target"))?;
        if !target_meta.is_dir() || target_meta.uid() != owner || target_meta.mode() & 0o022 != 0 {
            return Err(refuse());
        }
        let root = target.join("tilo-vm-app-runtime-cache");
        match std::fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(CliError::io("create component cache")(error)),
        }
        metadata(&root, owner, 0o700, true)?;
        let lock_path = root.join(format!("{key}.lock"));
        let lock = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock_path)
        {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let before = metadata(&lock_path, owner, 0o600, false)?;
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .map_err(CliError::io("open component cache lock"))?;
                let after = lock
                    .metadata()
                    .map_err(CliError::io("inspect component cache lock"))?;
                if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
                    return Err(refuse());
                }
                lock
            }
            Err(error) => return Err(CliError::io("create component cache lock")(error)),
        };
        let lock = advisory_lock::FileLock::exclusive(lock)
            .map_err(CliError::io("lock component cache entry"))?;
        Ok(Self {
            root,
            key: key.to_string(),
            owner,
            _lock: lock,
        })
    }

    pub(crate) fn load(&self, packed: &Packed) -> Result<Option<Artifact>, CliError> {
        if packed.component != "apps+runtime"
            || packed.source_digest.as_ref().is_none_or(|value| {
                value.len() != 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            || packed.target.is_none()
            || packed.toolchain.as_ref().is_none_or(String::is_empty)
        {
            return Err(refuse());
        }
        let entry = self.root.join(&self.key);
        match std::fs::symlink_metadata(&entry) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(CliError::io("inspect component cache entry")(error)),
            Ok(_) => {}
        }
        metadata(&entry, self.owner, 0o700, true)?;
        let mut bytes = Vec::new();
        open(&entry.join("record.json"), self.owner, 0o400)?
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(CliError::io("read component cache record"))?;
        if bytes.len() > 8192 {
            return Err(refuse());
        }
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| refuse())?;
        if record.key != self.key || record.packed != *packed || record.bytes == 0 {
            return Err(refuse());
        }
        let mut archive = open(&entry.join("layer.tar"), self.owner, 0o400)?;
        if fingerprint(&mut archive)? != (record.hash.clone(), record.bytes) {
            return Err(refuse());
        }
        Ok(Some(Artifact {
            file: archive,
            hash: record.hash,
            bytes: record.bytes,
        }))
    }

    pub(crate) fn save(&self, packed: &Packed, tree: &Path) -> Result<Artifact, CliError> {
        use std::os::unix::fs::PermissionsExt as _;
        let staging = tempfile::Builder::new()
            .prefix("artifact-")
            .tempdir_in(&self.root)
            .map_err(CliError::io("stage component cache"))?;
        let archive = staging.path().join("layer.tar");
        crate::build::pack(tree, &archive)?;
        let mut file = File::open(&archive).map_err(CliError::io("inspect packaged layer"))?;
        let (hash, bytes) = fingerprint(&mut file)?;
        file.sync_all()
            .map_err(CliError::io("sync packaged layer"))?;
        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o400))
            .map_err(CliError::io("seal packaged layer"))?;
        let record = Record {
            key: self.key.clone(),
            packed: packed.clone(),
            hash,
            bytes,
        };
        let path = staging.path().join("record.json");
        std::fs::write(&path, serde_json::to_vec(&record).map_err(|_| refuse())?)
            .map_err(CliError::io("write component cache record"))?;
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(CliError::io("sync component cache record"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .map_err(CliError::io("seal component cache record"))?;
        std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(CliError::io("secure component cache entry"))?;
        File::open(staging.path())
            .and_then(|file| file.sync_all())
            .map_err(CliError::io("sync component cache entry"))?;
        std::fs::rename(staging.path(), self.root.join(&self.key))
            .map_err(CliError::io("publish component cache entry"))?;
        File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(CliError::io("sync component cache directory"))?;
        self.load(packed)?.ok_or_else(refuse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn packed() -> Packed {
        Packed {
            component: "apps+runtime".into(),
            target: Some("x86_64-unknown-linux-gnu".into()),
            toolchain: Some("rustc 1.97.1 (fixture)".into()),
            source_digest: Some("a".repeat(64)),
        }
    }

    fn tree(root: &Path) -> PathBuf {
        use tilo_app_package::{BlobSource, Manifest, SigningIdentity};
        let tree = root.join("tree");
        let payload = root.join("payload");
        std::fs::create_dir_all(tree.join("usr/share/tilo-apps/cohort")).unwrap();
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/fixture"), b"public fixture payload").unwrap();
        let identity = SigningIdentity::generate();
        let mut manifest = Manifest::from_toml_str(
            r#"
[package]
id = "so.tilo.fixture"
version = "0.1.0"
kind = "app"
runtime = "tilo-runtime/2026.07"
[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""
[sandbox]
namespaces = ["user", "pid", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[sandbox.portals]
[entrypoint]
bin = "bin/fixture"
type = "gui"
"#,
        )
        .unwrap();
        manifest.publisher.identity =
            format!("ed25519:{}", hex::encode(identity.public_key_bytes()));
        let package = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
        assert!(matches!(
            tilo_app_package::verify(&package.meta, &[]),
            tilo_app_package::Verification::UntrustedPublisher { .. }
        ));
        let path = tree.join("usr/share/tilo-apps/cohort/fixture.tapp");
        tilo_app_package::tapp::write(
            File::create(path).unwrap(),
            &package.meta,
            Some(&package as &dyn BlobSource),
        )
        .unwrap();
        tree
    }

    fn bytes(mut artifact: Artifact) -> Vec<u8> {
        let mut value = Vec::new();
        artifact.file.read_to_end(&mut value).unwrap();
        assert_eq!(value.len() as u64, artifact.bytes);
        assert_eq!(hex::encode(Sha256::digest(&value)), artifact.hash);
        value
    }

    #[test]
    fn exact_signed_archive_replay_avoids_new_signatures_and_timestamps() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let tree_a = tree(first.path());
        let tree_b = tree(second.path());
        let path = "usr/share/tilo-apps/cohort/fixture.tapp";
        assert_ne!(
            std::fs::read(tree_a.join(path)).unwrap(),
            std::fs::read(tree_b.join(path)).unwrap()
        );
        let cache = Cache::new(target.path(), &"1".repeat(64)).unwrap();
        assert!(cache.load(&packed()).unwrap().is_none());
        let initial = bytes(cache.save(&packed(), &tree_a).unwrap());
        assert_eq!(bytes(cache.load(&packed()).unwrap().unwrap()), initial);
        drop(cache);
        let reopened = Cache::new(target.path(), &"1".repeat(64)).unwrap();
        assert_eq!(bytes(reopened.load(&packed()).unwrap().unwrap()), initial);
        assert!(
            Cache::new(target.path(), &"2".repeat(64))
                .unwrap()
                .load(&packed())
                .unwrap()
                .is_none()
        );
        let mut foreign = packed();
        foreign.source_digest = Some("b".repeat(64));
        assert!(reopened.load(&foreign).is_err());
        foreign = packed();
        foreign.target = Some("aarch64-unknown-linux-gnu".into());
        assert!(reopened.load(&foreign).is_err());
        foreign = packed();
        foreign.toolchain = Some("different compiler".into());
        assert!(reopened.load(&foreign).is_err());
    }

    #[test]
    fn modified_mutable_and_incomplete_entries_fail_closed() {
        let target = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let cache = Cache::new(target.path(), &"3".repeat(64)).unwrap();
        cache.save(&packed(), &tree(fixture.path())).unwrap();
        let entry = cache.root.join(&cache.key);
        let archive = entry.join("layer.tar");
        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(cache.load(&packed()).is_err());
        std::fs::write(&archive, b"changed").unwrap();
        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert!(cache.load(&packed()).is_err());
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_file(&archive).unwrap();
        std::os::unix::fs::symlink(fixture.path(), &archive).unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(cache.load(&packed()).is_err());
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_file(entry.join("record.json")).unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(cache.load(&packed()).is_err());
    }

    #[test]
    fn private_root_and_lock_contract_rejects_mutability_and_aliases() {
        let target = tempfile::tempdir().unwrap();
        std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Cache::new(target.path(), &"4".repeat(64)).is_err());
        std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let cache = Cache::new(target.path(), &"4".repeat(64)).unwrap();
        let lock = cache.root.join(format!("{}.lock", cache.key));
        drop(cache);
        std::fs::hard_link(&lock, target.path().join("alias")).unwrap();
        assert!(Cache::new(target.path(), &"4".repeat(64)).is_err());
    }

    #[test]
    fn concurrent_retry_waits_for_complete_published_artifact() {
        let target = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let key = "5".repeat(64);
        let cache = Cache::new(target.path(), &key).unwrap();
        let second_lock = File::options()
            .read(true)
            .write(true)
            .open(cache.root.join(format!("{key}.lock")))
            .unwrap();
        assert!(
            matches!(
                advisory_lock::FileLock::try_exclusive(second_lock),
                Err(advisory_lock::TryLockError::WouldBlock)
            ),
            "admitted cache transaction must own the entry lock"
        );
        let initial = bytes(cache.save(&packed(), &tree(fixture.path())).unwrap());
        let path = target.path().to_path_buf();
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let retry = Cache::new(&path, &key).unwrap();
            send.send(bytes(retry.load(&packed()).unwrap().unwrap()))
                .unwrap();
        });
        drop(cache);
        assert_eq!(receive.recv().unwrap(), initial);
        thread.join().unwrap();
    }

    #[test]
    fn compiled_input_key_uses_actual_native_closure_and_refuses_incomplete_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let result = std::process::Command::new("python3")
            .arg(root.join("os/dev/test-assemble-app-runtime.py"))
            .arg("InputFingerprint")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn completed_cache_remains_removable_by_normal_owned_target_cleanup() {
        let target = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let cache = Cache::new(target.path(), &"6".repeat(64)).unwrap();
        cache.save(&packed(), &tree(fixture.path())).unwrap();
        let root = cache.root.clone();
        drop(cache);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(!root.exists());
    }
}
