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
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use thiserror::Error;

use crate::NamedShareEnvelopeV1;
use crate::{RecordSubscription, RecordSubscriptionKey};
use tilo_lock::MutexRecover as _;

pub const RECIPIENT_MOUNT_READY_LINE: &str = "TILO_RECIPIENT_MOUNT_READY_V1";
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
    ready_timeout: Duration,
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
            .field("ready_timeout", &self.ready_timeout)
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
            ready_timeout: RECIPIENT_MOUNT_READY_TIMEOUT,
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

    fn start_payload(
        &self,
        key: RecordSubscriptionKey,
        command: &str,
        stdin_flag: &str,
        payload: &str,
    ) -> Result<RecipientMountInfo, RecipientMountError> {
        let (mount_point, cache_dir) = self.paths(key);
        // Serialize the liveness check with child registration. Reserving
        // afterwards leaves a check-then-act window where another starter can
        // register and release its reservation before this caller acquires it.
        let reservation = self.reserve_start(key)?;
        if let Some(info) = self.running_mount(key, mount_point.clone(), cache_dir.clone())? {
            return Ok(info);
        }
        self.prepare_mount_paths(&mount_point, &cache_dir)?;
        let (mut child, ready) =
            self.spawn_mount_child(command, stdin_flag, &mount_point, &cache_dir)?;
        if let Err(error) = write_private_payload(&mut child, payload) {
            terminate_child(&mut child);
            return Err(error);
        }
        if !matches!(ready.recv_timeout(self.ready_timeout), Ok(Ok(()))) {
            terminate_child(&mut child);
            return Err(RecipientMountError::ReadinessFailed);
        }
        let process_id = child.id();
        self.children.lock_recover().insert(key, child);
        drop(reservation);
        Ok(RecipientMountInfo {
            key,
            mount_point,
            cache_dir,
            process_id,
        })
    }

    fn running_mount(
        &self,
        key: RecordSubscriptionKey,
        mount_point: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<Option<RecipientMountInfo>, RecipientMountError> {
        let mut children = self.children.lock_recover();
        let process_id = match children.get_mut(&key) {
            Some(child) => match child.try_wait()? {
                None => Some(child.id()),
                Some(_) => None,
            },
            None => None,
        };
        if process_id.is_none() {
            children.remove(&key);
        }
        drop(children);
        Ok(process_id.map(|process_id| RecipientMountInfo {
            key,
            mount_point,
            cache_dir,
            process_id,
        }))
    }

    fn prepare_mount_paths(
        &self,
        mount_point: &Path,
        cache_dir: &Path,
    ) -> Result<(), RecipientMountError> {
        prepare_private_dir(&self.mounts_root)?;
        prepare_private_dir(&self.cache_root)?;
        prepare_private_dir(
            mount_point
                .parent()
                .ok_or(RecipientMountError::UnsafePath)?,
        )?;
        prepare_private_dir(cache_dir.parent().ok_or(RecipientMountError::UnsafePath)?)?;
        prepare_private_dir(mount_point)?;
        prepare_private_dir(cache_dir)?;
        ensure_empty_mountpoint(mount_point)?;
        Ok(())
    }

    fn spawn_mount_child(
        &self,
        command: &str,
        stdin_flag: &str,
        mount_point: &Path,
        cache_dir: &Path,
    ) -> Result<(Child, mpsc::Receiver<Result<(), ()>>), RecipientMountError> {
        let mut child = Command::new(&self.executable)
            .arg(command)
            .arg("--mount-point")
            .arg(mount_point)
            .arg(stdin_flag)
            .arg("--cache-dir")
            .arg(cache_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err(RecipientMountError::MissingChildStdout);
        };
        Ok((child, monitor_readiness(stdout)))
    }

    #[cfg(test)]
    fn with_ready_timeout(mut self, ready_timeout: Duration) -> Self {
        self.ready_timeout = ready_timeout;
        self
    }

    /// Return whether the supervised child remains alive.
    ///
    /// # Errors
    ///
    /// Returns an error when child status cannot be queried.
    pub fn is_running(&self, key: RecordSubscriptionKey) -> Result<bool, RecipientMountError> {
        let mut children = self.children.lock_recover();
        let running = match children.get_mut(&key) {
            Some(child) => Some(child.try_wait()?.is_none()),
            None => None,
        };
        if running == Some(false) {
            children.remove(&key);
        }
        drop(children);
        Ok(running.unwrap_or(false))
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
        let child = self.children.lock_recover().remove(&key);
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
        let child = self.children.lock_recover().remove(&key);
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

    fn reserve_start(
        &self,
        key: RecordSubscriptionKey,
    ) -> Result<StartReservation<'_>, RecipientMountError> {
        let mut starting = self.starting.lock_recover();
        if !starting.insert(key) {
            return Err(RecipientMountError::StartInProgress);
        }
        drop(starting);
        Ok(StartReservation {
            starting: &self.starting,
            key,
        })
    }
}

fn monitor_readiness(stdout: impl Read + Send + 'static) -> mpsc::Receiver<Result<(), ()>> {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
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
    ready_rx
}

fn write_private_payload(child: &mut Child, payload: &str) -> Result<(), RecipientMountError> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or(RecipientMountError::MissingChildStdin)?;
    stdin.write_all(payload.as_bytes())?;
    stdin.write_all(b"\n")?;
    Ok(())
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

struct StartReservation<'a> {
    starting: &'a Mutex<BTreeSet<RecordSubscriptionKey>>,
    key: RecordSubscriptionKey,
}

impl Drop for StartReservation<'_> {
    fn drop(&mut self) {
        self.starting.lock_recover().remove(&self.key);
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
    use fs_protocol::{
        FileInviteSpec, FileInviteSubject, FilePermission, FileRecordId, mint_bearer_file_invite,
    };
    use rand::{SeedableRng, rngs::StdRng};
    use std::sync::Arc;
    use uuid::Uuid;

    #[cfg(unix)]
    fn write_executable_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

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
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn recipient_mount_children_get_secrets_once_and_starts_are_exclusive() {
        // Phase `child_argv_is_secret_free_and_uri_is_delivered_only_on_stdin`.
        {
            let temp = tempfile::tempdir().unwrap();
            let args_path = temp.path().join("args");
            let stdin_path = temp.path().join("stdin");
            let script = temp.path().join("fake-fs");
            write_executable_script(
                &script,
                &format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n/bin/cat > '{}'\nprintf '%s\\n' '{}'\n",
                    args_path.display(),
                    stdin_path.display(),
                    RECIPIENT_MOUNT_READY_LINE,
                ),
            );
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
        // Phase `a_live_mount_is_reused_without_redelivering_its_bearer_secret`.
        {
            let temp = tempfile::tempdir().unwrap();
            let calls_path = temp.path().join("calls");
            let payloads_path = temp.path().join("payloads");
            let script = temp.path().join("fake-fs");
            write_executable_script(
                &script,
                &format!(
                    "#!/bin/sh\nprintf 'call\\n' >> '{}'\nIFS= read -r payload\nprintf '%s\\n' \"$payload\" >> '{}'\nprintf '%s\\n' '{}'\nsleep 30\n",
                    calls_path.display(),
                    payloads_path.display(),
                    RECIPIENT_MOUNT_READY_LINE,
                ),
            );
            let subscribed = subscription(&temp.path().join("subscriptions.json"));
            let expected_payload = subscribed.link().encode_uri_at(1).unwrap();
            let supervisor = RecipientMountSupervisor::new(
                &script,
                temp.path().join("mounts"),
                temp.path().join("cache"),
            );

            let first = supervisor.start(&subscribed).unwrap();
            let second = supervisor.start(&subscribed).unwrap();

            assert_eq!(second, first);
            assert_eq!(std::fs::read_to_string(calls_path).unwrap(), "call\n");
            assert_eq!(
                std::fs::read_to_string(payloads_path).unwrap(),
                format!("{expected_payload}\n")
            );
            assert!(supervisor.stop(subscribed.key).unwrap());
        }
        // Phase `concurrent_start_is_rejected_before_a_duplicate_child_can_launch`.
        {
            let temp = tempfile::tempdir().unwrap();
            let release_path = temp.path().join("release");
            let calls_path = temp.path().join("calls");
            let script = temp.path().join("fake-fs");
            write_executable_script(
                &script,
                &format!(
                    "#!/bin/sh\nprintf 'call\\n' >> '{}'\n/bin/cat >/dev/null\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf '%s\\n' '{}'\nsleep 30\n",
                    calls_path.display(),
                    release_path.display(),
                    RECIPIENT_MOUNT_READY_LINE,
                ),
            );
            let subscribed = Arc::new(subscription(&temp.path().join("subscriptions.json")));
            let supervisor = Arc::new(RecipientMountSupervisor::new(
                &script,
                temp.path().join("mounts"),
                temp.path().join("cache"),
            ));
            let starter = {
                let subscribed = Arc::clone(&subscribed);
                let supervisor = Arc::clone(&supervisor);
                std::thread::spawn(move || supervisor.start(&subscribed))
            };
            let deadline = tilo_test_env::hang_deadline();
            while !supervisor.starting.lock_recover().contains(&subscribed.key)
                && std::time::Instant::now() < deadline
            {
                std::thread::park_timeout(Duration::from_millis(5));
            }
            assert!(
                supervisor.starting.lock_recover().contains(&subscribed.key),
                "mount start did not acquire its reservation"
            );

            assert!(matches!(
                supervisor.start(&subscribed),
                Err(RecipientMountError::StartInProgress)
            ));
            std::fs::write(release_path, b"release").unwrap();
            let info = starter.join().unwrap().unwrap();
            assert_eq!(std::fs::read_to_string(calls_path).unwrap(), "call\n");
            assert_eq!(info.key, subscribed.key);
            assert!(supervisor.stop(subscribed.key).unwrap());
        }
        // Phase `readiness_timeout_releases_the_start_reservation_for_retry`.
        {
            let temp = tempfile::tempdir().unwrap();
            let script = temp.path().join("fake-fs");
            write_executable_script(&script, "#!/bin/sh\n/bin/cat >/dev/null\nsleep 30\n");
            let subscribed = subscription(&temp.path().join("subscriptions.json"));
            let supervisor = RecipientMountSupervisor::new(
                &script,
                temp.path().join("mounts"),
                temp.path().join("cache"),
            )
            .with_ready_timeout(Duration::from_millis(50));

            for _ in 0..2 {
                assert!(matches!(
                    supervisor.start(&subscribed),
                    Err(RecipientMountError::ReadinessFailed)
                ));
            }

            assert!(!supervisor.is_running(subscribed.key).unwrap());
        }
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
