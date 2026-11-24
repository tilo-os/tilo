//! The capability list a mount enforces, kept current while it runs.
//!
//! `--acl FILE` binds a [`collab_types::FsAuthority`] to a mount: the list the session last
//! decided from the workspace's signed member list and told the mount over its tree's
//! connection ([`CapabilityFile::keep`]), kept here so a mount that starts while the session is
//! away enforces the last list it was told.
//! `/var/workspace` stays mounted for the whole session, so binding the list
//! by value at mount time would make "a later grant needs a remount" mean "a
//! later grant does not apply". [`CapabilityFile`] owns the file and an
//! [`AclHandle`]; [`CapabilityFile::refresh`] re-reads on change and swaps
//! the result in underneath the live mount, and
//! [`CapabilityFile::spawn_reload`] calls it whenever the host reports the file
//! changed.
//!
//! The three outcomes are deliberately not treated alike. A changed,
//! parseable file swaps in — the point. A **gone** file binds an empty
//! (deny-everything) ACL: removing the file is the producer saying "this
//! identity holds nothing" (distinct from never passing `--acl`, which
//! leaves the mount unenforced). An **unreadable or malformed** file keeps
//! the previous list and logs — the file is rewritten in place rather than
//! renamed over, so a reader can catch it truncated mid-write, and denying
//! everything for that transient window would make the reloader itself the outage.
//!
//! None of these can widen access beyond what some successfully-parsed file said.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use collab_types::CapOverride;
use tilo_action_policy::MemberId;

use crate::acl::{AclHandle, IdentityKey, WorkspaceAcl};
use crate::file_watch::FileChanges;
use tilo_files::native::FileStamp;
use tilo_lock::MutexRecover as _;

/// A version of the file is its [`FileStamp`] (so an unchanged file costs one `stat`): it names the
/// file itself, not only its length and modification time, which a rename-over-target producer can
/// repeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refresh {
    /// The file is byte-identical to the last one loaded. Nothing swapped.
    Unchanged,
    /// A new list is now in force, holding this many capabilities.
    Reloaded(usize),
    /// The file is gone. A deny-everything ACL is now in force.
    Revoked,
    /// The file could not be read or parsed. The previous list stands.
    Failed(String),
}

/// A capability list on disk, bound to a live mount. Construct with
/// [`CapabilityFile::load`] (fails if the file can't be read — a mount must
/// never believe it's enforcing a list it never saw), then keep it current
/// with [`CapabilityFile::spawn_reload`].
#[derive(Debug)]
pub struct CapabilityFile {
    path: PathBuf,
    identity: IdentityKey,
    members: Vec<MemberId>,
    handle: AclHandle,
    /// The version last successfully loaded. `None` once observed missing, so reappearance always reloads.
    seen: Mutex<Option<FileStamp>>,
}

impl CapabilityFile {
    /// Read `path` and bind its capabilities for `identity`. Fail-closed: a
    /// missing, unreadable or malformed file is an error the caller must refuse the mount on.
    ///
    /// # Errors
    ///
    /// Returns [`CapsError::Read`] when `path` cannot be opened or stat'd, or
    /// [`CapsError::Parse`] when its contents are not an [`collab_types::FsAuthority`].
    pub fn load(
        path: impl Into<PathBuf>,
        identity: IdentityKey,
        members: Vec<MemberId>,
    ) -> Result<Self, CapsError> {
        let path = path.into();
        let (caps, stamp) = read_capabilities(&path)?;
        let acl = build(identity, &caps, &members);
        Ok(Self {
            path,
            identity,
            members,
            handle: AclHandle::new(acl),
            seen: Mutex::new(Some(stamp)),
        })
    }

    /// The handle to give [`crate::QuickFS::with_acl`]. Cheap to clone; every clone sees each swap.
    pub fn handle(&self) -> AclHandle {
        self.handle.clone()
    }

    pub fn identity(&self) -> IdentityKey {
        self.identity
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many capabilities are in force right now. For logging.
    pub fn count(&self) -> usize {
        self.handle.snapshot().capability_count()
    }

    /// Re-read the file if changed and swap the result into the live mount; cheap to call often (an unchanged file costs one `stat`).
    pub fn refresh(&self) -> Refresh {
        let stamp = match FileStamp::of(&self.path) {
            Ok(Some(stamp)) => stamp,
            Ok(None) => {
                let mut seen = self.seen.lock_recover();
                if seen.is_none() {
                    // Already observed missing and already denying.
                    return Refresh::Unchanged;
                }
                *seen = None;
                drop(seen);
                // Deny everything: an absent file is the producer saying "nothing granted" — distinct from never having asked for enforcement.
                self.handle.replace(WorkspaceAcl::new());
                return Refresh::Revoked;
            }
            Err(e) => return Refresh::Failed(format!("{}: {e}", self.path.display())),
        };

        {
            let seen = self.seen.lock_recover();
            if *seen == Some(stamp) {
                return Refresh::Unchanged;
            }
        }

        match read_capabilities(&self.path) {
            Ok((caps, stamp)) => {
                let n = caps.len();
                self.handle
                    .replace(build(self.identity, &caps, &self.members));
                *self.seen.lock_recover() = Some(stamp);
                Refresh::Reloaded(n)
            }
            // Deliberately doesn't update `seen`, so the next tick retries rather than treating a half-written file as current.
            Err(e) => Refresh::Failed(e.to_string()),
        }
    }
}

impl CapabilityFile {
    /// Put `json`, the list the session decides now, in force at once, and keep it where the
    /// next start of this mount reads it. A list that does not parse changes nothing.
    ///
    /// # Errors
    ///
    /// [`CapsError::Parse`] when `json` is no file access list, [`CapsError::Read`] when it is
    /// in force but could not be kept for the next start.
    pub fn keep(&self, json: &str) -> Result<usize, CapsError> {
        let caps = authority(json.as_bytes(), &self.path)?.capabilities;
        let n = caps.len();
        self.handle
            .replace(build(self.identity, &caps, &self.members));
        tilo_files::native::atomic::write(&self.path, json.as_bytes()).map_err(|source| {
            CapsError::Read {
                path: self.path.clone(),
                source,
            }
        })?;
        if let Ok(Some(stamp)) = FileStamp::of(&self.path) {
            *self.seen.lock_recover() = Some(stamp);
        }
        Ok(n)
    }
}

impl CapabilityFile {
    /// Keep the live mount's list current: re-read on every change of the
    /// file, from the host's file events. The first read happens at once, so
    /// a change made between [`Self::load`] and this call is not missed.
    ///
    /// # Errors
    ///
    /// The file's directory cannot be watched.
    pub fn spawn_reload(self: Arc<Self>) -> std::io::Result<tokio::task::JoinHandle<()>> {
        let mut changes = FileChanges::for_file(&self.path)?;
        Ok(tokio::spawn(async move {
            loop {
                changes.next().await;
                self.report(&self.refresh());
            }
        }))
    }

    fn report(&self, outcome: &Refresh) {
        let path = self.path.display();
        match outcome {
            Refresh::Unchanged => {}
            Refresh::Reloaded(n) => println!("Capability list {path} reloaded: {n} in force"),
            Refresh::Revoked => {
                eprintln!("Capability list {path} is gone; denying every access until it returns");
            }
            Refresh::Failed(e) => eprintln!(
                "Capability list {path} could not be re-read ({e}); the previously loaded list \
                 stays in force"
            ),
        }
    }
}

/// Build the ACL for one identity from a capability list. `now: None` ⇒ every check reads the live clock, so a time-limited capability expires.
fn build(identity: IdentityKey, caps: &[CapOverride], members: &[MemberId]) -> WorkspaceAcl {
    WorkspaceAcl::from_capabilities(identity, caps, members, None)
}

fn read_capabilities(path: &Path) -> Result<(Vec<CapOverride>, FileStamp), CapsError> {
    let mut file = std::fs::File::open(path).map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    // Stamp from the same open file, not a second `stat`: a rewrite in between could pair the new stamp with the old bytes.
    let stamp = FileStamp::of_file(&file).map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| CapsError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    let authority = authority(&bytes, path)?;
    Ok((authority.capabilities, stamp))
}

/// The file authority `bytes` hold.
/// # Errors
/// [`CapsError::Parse`] for a malformed file.
pub fn authority(bytes: &[u8], path: &Path) -> Result<collab_types::FsAuthority, CapsError> {
    serde_json::from_slice(bytes).map_err(|source| CapsError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum CapsError {
    #[error("capability list {path} could not be read: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("capability list {path} is not a file access list: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::Access;
    use collab_types::{Access as CapAccess, Effect, Scope};
    use tilo_action_policy::Grantee;

    fn alice() -> Grantee {
        Grantee::person(hex::encode(ALICE))
    }

    const ALICE: IdentityKey = [0xA1; 32];

    fn read_grant(path: &str) -> CapOverride {
        CapOverride::new(
            alice(),
            Scope::FsPath(path.into()),
            Effect::Grant,
            CapAccess::Read,
        )
    }

    fn write_caps(path: &Path, caps: &[CapOverride]) {
        std::fs::write(
            path,
            serde_json::to_vec(&collab_types::FsAuthority::from(caps.to_vec())).unwrap(),
        )
        .unwrap();
        // The stamp includes mtime at filesystem resolution; don't rely on length differing within one tick.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    tilo_test_env::scenario! {
        fn the_capability_file_is_enforced_live_and_fails_closed;

        phase capability_file_changes_take_effect_during_the_mount;
        {
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                write_caps(&path, &[read_grant("/w/docs")]);

                let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
                let handle = file.handle();
                assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));
                assert!(
                    !handle.permits(&ALICE, "/w/plans/b.md", Access::Read),
                    "the second subtree must be denied before it is granted, or the test proves nothing"
                );

                write_caps(&path, &[read_grant("/w/docs"), read_grant("/w/plans")]);
                assert_eq!(file.refresh(), Refresh::Reloaded(2));

                assert!(
                    handle.permits(&ALICE, "/w/plans/b.md", Access::Read),
                    "a grant made after the mount came up must reach the same handle the mount holds"
                );
                assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                write_caps(&path, &[read_grant("/w/docs"), read_grant("/w/plans")]);

                let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
                let handle = file.handle();
                assert!(handle.permits(&ALICE, "/w/plans/b.md", Access::Read));

                write_caps(&path, &[read_grant("/w/docs")]);
                assert_eq!(file.refresh(), Refresh::Reloaded(1));

                assert!(
                    !handle.permits(&ALICE, "/w/plans/b.md", Access::Read),
                    "revoking a capability must take effect on the live mount"
                );
                assert!(
                    handle.permits(&ALICE, "/w/docs/a.md", Access::Read),
                    "revoking one capability must not disturb the others"
                );
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                write_caps(&path, &[read_grant("/w/docs")]);
                let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
                assert_eq!(file.refresh(), Refresh::Unchanged);
                assert_eq!(file.refresh(), Refresh::Unchanged);
            }
            // Phase `a_capability_that_expires_mid_mount_stops_permitting`.
            // An expiry must lapse during the mount, true only if the bound ACL reads the live clock, not the load-time instant.
            {
                let now = tilo_clock::try_unix_now().unwrap();
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");

                write_caps(
                    &path,
                    &[read_grant("/w/docs").lasting(tilo_action_policy::Lifetime::Until((now + 3600) * 1000))],
                );
                let live = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
                assert!(live.handle().permits(&ALICE, "/w/docs/a.md", Access::Read));

                write_caps(
                    &path,
                    &[read_grant("/w/docs").lasting(tilo_action_policy::Lifetime::Until((now - 1) * 1000))],
                );
                let lapsed = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
                assert!(
                    !lapsed
                        .handle()
                        .permits(&ALICE, "/w/docs/a.md", Access::Read),
                    "an expiry in the past must not permit"
                );
            }
        }

        phase missing_deleted_malformed_or_empty_capability_files_fail_closed;
        {
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                write_caps(&path, &[read_grant("/w/docs")]);
                let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
                let handle = file.handle();
                assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));

                std::fs::remove_file(&path).unwrap();
                assert_eq!(file.refresh(), Refresh::Revoked);
                assert!(
                    !handle.permits(&ALICE, "/w/docs/a.md", Access::Read),
                    "an absent capability list must deny, not keep serving the last one it saw"
                );
                // Idempotent: still missing, still denying, and not re-reported.
                assert_eq!(file.refresh(), Refresh::Unchanged);

                write_caps(&path, &[read_grant("/w/docs")]);
                assert_eq!(file.refresh(), Refresh::Reloaded(1));
                assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                write_caps(&path, &[read_grant("/w/docs")]);
                let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
                let handle = file.handle();

                std::fs::write(&path, b"[{\"subject\"").unwrap();
                assert!(matches!(file.refresh(), Refresh::Failed(_)));
                assert!(
                    handle.permits(&ALICE, "/w/docs/a.md", Access::Read),
                    "a half-written file must not revoke every capability on the machine"
                );
                // Not latched: the next tick retries rather than treating truncated bytes as current.
                assert!(matches!(file.refresh(), Refresh::Failed(_)));

                write_caps(&path, &[read_grant("/w/plans")]);
                assert_eq!(file.refresh(), Refresh::Reloaded(1));
                assert!(handle.permits(&ALICE, "/w/plans/b.md", Access::Read));
                assert!(!handle.permits(&ALICE, "/w/docs/a.md", Access::Read));
            }
            {
                let err = CapabilityFile::load("/nonexistent/tilo-caps.json", ALICE, Vec::new())
                    .expect_err("a missing list must not load as an unenforced mount");
                assert!(matches!(err, CapsError::Read { .. }), "got {err:?}");
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                std::fs::write(&path, br#"{"capabilities":[]}"#).unwrap();
                let file =
                    CapabilityFile::load(&path, ALICE, Vec::new()).expect("an empty list is valid");
                assert!(!file.handle().permits(&ALICE, "/", Access::Read));
            }
        }
    }

    /// Waits until `holds` is true, which only the file watch can make so.
    async fn through_the_watch(what: &str, holds: impl Fn() -> bool + Sync) {
        tilo_test_env::within(what, async {
            while !holds() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await;
    }

    /// The reloader the daemon runs delivers each change through the file watch. A revocation and a removal each reach the live mount, and a
    /// half-written file still keeps the previous list.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_revocation_reaches_the_live_mount_through_the_file_watch_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        write_caps(&path, &[read_grant("/w/docs"), read_grant("/w/plans")]);
        let file = Arc::new(CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads"));
        let handle = file.handle();
        let reload = Arc::clone(&file)
            .spawn_reload()
            .expect("the capability list is watchable");
        assert!(handle.permits(&ALICE, "/w/plans/b.md", Access::Read));

        write_caps(&path, &[read_grant("/w/docs")]);
        through_the_watch("the revocation to reach the live mount", || {
            !handle.permits(&ALICE, "/w/plans/b.md", Access::Read)
        })
        .await;
        assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));

        // What a reader sees if it catches an in-place rewrite midway, read
        // here at once rather than whenever the watch gets to it.
        std::fs::write(&path, b"[{\"subject\"").unwrap();
        file.refresh();
        assert!(
            handle.permits(&ALICE, "/w/docs/a.md", Access::Read),
            "a half-written file must not revoke every capability"
        );

        std::fs::remove_file(&path).unwrap();
        through_the_watch("removing the list to deny everything", || {
            !handle.permits(&ALICE, "/w/docs/a.md", Access::Read)
        })
        .await;

        write_caps(&path, &[read_grant("/w/plans")]);
        through_the_watch("a list that comes back to be enforced again", || {
            handle.permits(&ALICE, "/w/plans/b.md", Access::Read)
        })
        .await;
        reload.abort();
    }
}
