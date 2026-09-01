//! Supervision for isolated recipient mounts.
//!
//! A record received through a personal share must not be inserted into the
//! owner's ordinary workspace namespace: that namespace is replicated to its
//! workspace peers and would silently widen the share. Each subscription is
//! therefore mounted by a dedicated read-only `fs mount-share` child. The
//! bearer URI is written to the child's stdin and never appears in argv.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use thiserror::Error;

use crate::NamedShareEnvelopeV1;
use crate::{RecordSubscription, RecordSubscriptionKey};

pub const RECIPIENT_MOUNT_READY_LINE: &str = "QOS_RECIPIENT_MOUNT_READY_V1";
const RECIPIENT_MOUNT_READY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RECIPIENT_CHILD_STDOUT_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientMountInfo {
    pub key: RecordSubscriptionKey,
    pub mount_point: PathBuf,
    pub cache_dir: PathBuf,
    pub process_id: u32,
}

pub struct RecipientMountSupervisor {
    executable: PathBuf,
    mounts_root: PathBuf,
    cache_root: PathBuf,
    children: Mutex<BTreeMap<RecordSubscriptionKey, Child>>,
    starting: Mutex<BTreeSet<RecordSubscriptionKey>>,
}

impl std::fmt::Debug for RecipientMountSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecipientMountSupervisor")
            .field("executable", &self.executable)
            .field("mounts_root", &self.mounts_root)
            .field("cache_root", &self.cache_root)
            .field(
                "active_children",
                &self.children.lock().map(|c| c.len()).ok(),
            )
            .field(
                "starting_children",
                &self.starting.lock().map(|s| s.len()).ok(),
            )
            .finish()
    }
}

impl RecipientMountSupervisor {
    /// Construct a supervisor rooted at private mount and cache directories.
    pub fn new(
        executable: impl Into<PathBuf>,
        mounts_root: impl Into<PathBuf>,
        cache_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            executable: executable.into(),
            mounts_root: mounts_root.into(),
            cache_root: cache_root.into(),
            children: Mutex::new(BTreeMap::new()),
            starting: Mutex::new(BTreeSet::new()),
        }
    }

    /// Start or return the existing mount for a bearer subscription.
    ///
    /// # Errors
    ///
    /// Returns an error when the link is invalid or the child cannot be
    /// prepared, started, supplied its secret, or observed ready.
    pub fn start(
        &self,
        subscription: &RecordSubscription,
    ) -> Result<RecipientMountInfo, RecipientMountError> {
        let payload = subscription
            .link()
            .encode_uri()
            .map_err(|_| RecipientMountError::InvalidOrExpiredLink)?;
        self.start_payload(subscription.key, "mount-share", "--link-stdin", &payload)
    }

    /// Start or return the existing mount for a named-share envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when the envelope is invalid or the child cannot be
    /// prepared, started, supplied its secret, or observed ready.
    pub fn start_named(
        &self,
        envelope: &NamedShareEnvelopeV1,
    ) -> Result<RecipientMountInfo, RecipientMountError> {
        let key = RecordSubscriptionKey {
            workspace_id: envelope.invite.workspace_id,
            record_id: envelope.invite.record_id,
        };
        let payload = hex::encode(
            collab_types::canonical_bytes(envelope)
                .map_err(|_| RecipientMountError::InvalidOrExpiredLink)?,
        );
        self.start_payload(key, "mount-named-share", "--envelope-stdin", &payload)
    }

    #[allow(clippy::too_many_lines)]
    fn start_payload(
        &self,
        key: RecordSubscriptionKey,
        command: &str,
        stdin_flag: &str,
        payload: &str,
    ) -> Result<RecipientMountInfo, RecipientMountError> {
        let (mount_point, cache_dir) = self.paths(key);
        let mut children = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = children.get_mut(&key) {
            match child.try_wait()? {
                None => {
                    return Ok(RecipientMountInfo {
                        key,
                        mount_point,
                        cache_dir,
                        process_id: child.id(),
                    });
                }
                Some(_) => {
                    children.remove(&key);
                }
            }
        }
        drop(children);
        let reservation = self.reserve_start(key)?;

        prepare_private_dir(&self.mounts_root)?;
        prepare_private_dir(&self.cache_root)?;
        prepare_private_dir(
            mount_point
                .parent()
                .ok_or(RecipientMountError::UnsafePath)?,
        )?;
        prepare_private_dir(cache_dir.parent().ok_or(RecipientMountError::UnsafePath)?)?;
        prepare_private_dir(&mount_point)?;
        prepare_private_dir(&cache_dir)?;
        ensure_empty_mountpoint(&mount_point)?;

        let mut child = Command::new(&self.executable)
            .arg(command)
            .arg("--mount-point")
            .arg(&mount_point)
            .arg(stdin_flag)
            .arg("--cache-dir")
            .arg(&cache_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RecipientMountError::MissingChildStdout);
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout.take(MAX_RECIPIENT_CHILD_STDOUT_BYTES));
            let mut signalled = false;
            for line in reader.lines() {
                match line {
                    Ok(line) if line == RECIPIENT_MOUNT_READY_LINE => {
                        if !signalled {
                            let _ = ready_tx.send(Ok(()));
                            signalled = true;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if !signalled {
                let _ = ready_tx.send(Err(()));
            }
        });
        let write_result = child
            .stdin
            .take()
            .ok_or(RecipientMountError::MissingChildStdin)
            .and_then(|mut stdin| {
                stdin
                    .write_all(payload.as_bytes())
                    .and_then(|()| stdin.write_all(b"\n"))
                    .map_err(RecipientMountError::Io)
            });
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if !matches!(
            ready_rx.recv_timeout(RECIPIENT_MOUNT_READY_TIMEOUT),
            Ok(Ok(()))
        ) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RecipientMountError::ReadinessFailed);
        }
        let process_id = child.id();
        self.children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, child);
        drop(reservation);
        Ok(RecipientMountInfo {
            key,
            mount_point,
            cache_dir,
            process_id,
        })
    }

    /// Return whether the supervised child remains alive.
    ///
    /// # Errors
    ///
    /// Returns an error when child status cannot be queried.
    #[allow(clippy::significant_drop_tightening)]
    pub fn is_running(&self, key: RecordSubscriptionKey) -> Result<bool, RecipientMountError> {
        let mut children = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(child) = children.get_mut(&key) else {
            return Ok(false);
        };
        if child.try_wait()?.is_none() {
            return Ok(true);
        }
        children.remove(&key);
        Ok(false)
    }

    pub fn mount_point_for(&self, key: RecordSubscriptionKey) -> PathBuf {
        self.paths(key).0
    }

    /// Stop and reap one supervised mount child.
    ///
    /// # Errors
    ///
    /// Returns an error when child status, termination, or reaping fails.
    pub fn stop(&self, key: RecordSubscriptionKey) -> Result<bool, RecipientMountError> {
        let child = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        let Some(mut child) = child else {
            return Ok(false);
        };
        if child.try_wait()?.is_none() {
            child.kill()?;
            child.wait()?;
        }
        Ok(true)
    }

    /// Wait for one known child to exit. Service code normally polls
    /// [`Self::is_running`]; deterministic harnesses use this event barrier.
    ///
    /// # Errors
    ///
    /// Returns an error when waiting for the child fails.
    pub fn wait(
        &self,
        key: RecordSubscriptionKey,
    ) -> Result<Option<std::process::ExitStatus>, RecipientMountError> {
        let child = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        let Some(mut child) = child else {
            return Ok(None);
        };
        Ok(Some(child.wait()?))
    }

    fn paths(&self, key: RecordSubscriptionKey) -> (PathBuf, PathBuf) {
        let workspace = key.workspace_id.simple().to_string();
        let record = key.record_id.0.simple().to_string();
        (
            self.mounts_root.join(&workspace).join(&record),
            self.cache_root.join(workspace).join(record),
        )
    }

    #[allow(clippy::significant_drop_tightening)]
    fn reserve_start(
        &self,
        key: RecordSubscriptionKey,
    ) -> Result<StartReservation<'_>, RecipientMountError> {
        let mut starting = self
            .starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !starting.insert(key) {
            return Err(RecipientMountError::StartInProgress);
        }
        Ok(StartReservation {
            starting: &self.starting,
            key,
        })
    }
}

struct StartReservation<'a> {
    starting: &'a Mutex<BTreeSet<RecordSubscriptionKey>>,
    key: RecordSubscriptionKey,
}

impl Drop for StartReservation<'_> {
    fn drop(&mut self) {
        self.starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

fn prepare_private_dir(path: &Path) -> Result<(), RecipientMountError> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(RecipientMountError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn ensure_empty_mountpoint(path: &Path) -> Result<(), RecipientMountError> {
    if std::fs::read_dir(path)?.next().transpose()?.is_some() {
        return Err(RecipientMountError::MountpointNotEmpty);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum RecipientMountError {
    #[error("recipient mount I/O failed")]
    Io(#[from] std::io::Error),
    #[error("recipient mount path is not a private directory")]
    UnsafePath,
    #[error("recipient mountpoint is not empty")]
    MountpointNotEmpty,
    #[error("recipient share link is invalid or expired")]
    InvalidOrExpiredLink,
    #[error("recipient mount child has no private stdin")]
    MissingChildStdin,
    #[error("recipient mount child has no readiness channel")]
    MissingChildStdout,
    #[error("recipient mount child failed to become ready")]
    ReadinessFailed,
    #[error("recipient mount is already starting")]
    StartInProgress,
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        FileInviteSpec, FileInviteSubject, FilePermission, FileRecordId, mint_bearer_file_invite,
    };
    use rand::{SeedableRng, rngs::StdRng};
    use uuid::Uuid;

    fn subscription(path: &Path) -> RecordSubscription {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x41; 32]);
        let mut rng = StdRng::seed_from_u64(4);
        let bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: Uuid::new_v4(),
                record_id: FileRecordId::new(),
                subject: FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                },
                permission: FilePermission::View,
                expires_at_ms: None,
                max_redemptions: Some(1),
            },
            &key,
            &mut rng,
        )
        .unwrap();
        let endpoint =
            iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x31; 32]).public())
                .with_relay_url("https://relay.example.invalid".parse().unwrap());
        let link = crate::FileShareLinkV2::bind_at(endpoint, bearer, &key, 1).unwrap();
        let store = crate::RecordSubscriptionStore::open_at(path, 1).unwrap();
        store.insert_at(link, 1).unwrap();
        store.subscriptions().remove(0)
    }

    #[cfg(unix)]
    #[test]
    fn child_argv_is_secret_free_and_uri_is_delivered_only_on_stdin() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let args_path = temp.path().join("args");
        let stdin_path = temp.path().join("stdin");
        let script = temp.path().join("fake-fs");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n/bin/cat > '{}'\nprintf '%s\\n' '{}'\n",
                args_path.display(),
                stdin_path.display(),
                RECIPIENT_MOUNT_READY_LINE,
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let subscribed = subscription(&temp.path().join("subscriptions.json"));
        let uri = subscribed.link().encode_uri_at(1).unwrap();
        let secret = hex::encode(subscribed.link().bearer_secret());
        let supervisor = RecipientMountSupervisor::new(
            &script,
            temp.path().join("mounts"),
            temp.path().join("cache"),
        );
        let info = supervisor.start(&subscribed).unwrap();
        assert!(supervisor.wait(subscribed.key).unwrap().unwrap().success());
        let args = std::fs::read_to_string(args_path).unwrap();
        let stdin = std::fs::read_to_string(stdin_path).unwrap();
        assert!(!args.contains(&uri));
        assert!(!args.contains(&secret));
        assert!(args.contains("--link-stdin"));
        assert_eq!(stdin.trim_end(), uri);
        assert!(info.mount_point.starts_with(temp.path().join("mounts")));
    }

    #[test]
    fn nonempty_or_symlink_mountpoints_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let subscribed = subscription(&temp.path().join("subscriptions.json"));
        let supervisor = RecipientMountSupervisor::new(
            "/does/not/matter",
            temp.path().join("mounts"),
            temp.path().join("cache"),
        );
        let (mount, _) = supervisor.paths(subscribed.key);
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(mount.join("unexpected"), b"no").unwrap();
        assert!(matches!(
            supervisor.start(&subscribed),
            Err(RecipientMountError::MountpointNotEmpty)
        ));

        #[cfg(unix)]
        {
            let other = tempfile::tempdir().unwrap();
            let subscribed = subscription(&other.path().join("subscriptions.json"));
            let supervisor = RecipientMountSupervisor::new(
                "/does/not/matter",
                other.path().join("mounts"),
                other.path().join("cache"),
            );
            let (mount, _) = supervisor.paths(subscribed.key);
            std::fs::create_dir_all(other.path().join("mounts")).unwrap();
            std::os::unix::fs::symlink(other.path(), mount.parent().unwrap()).unwrap();
            assert!(matches!(
                supervisor.start(&subscribed),
                Err(RecipientMountError::UnsafePath)
            ));
        }
    }
}
