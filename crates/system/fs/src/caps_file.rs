//! The capability list a mount enforces, kept current while it runs.
//!
//! `--acl FILE` binds the [`collab_types::FsAuthority`] the governor exports to a mount.
//! `/var/workspace` stays mounted for the whole session, so binding the list
//! by value at mount time would make "a later grant needs a remount" mean "a
//! later grant does not apply". [`CapabilityFile`] owns the file and an
//! [`AclHandle`]; [`CapabilityFile::refresh`] re-reads on change and swaps
//! the result in underneath the live mount, and
//! [`CapabilityFile::spawn_reload`] calls it whenever the host reports the
//! file changed (#4224; it used to poll once a second).
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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use collab_types::{CapOverride, Principal};

use crate::acl::{AclHandle, IdentityKey, WorkspaceAcl};
use crate::file_watch::{ReloadCadence, ReloadTrigger};
use tilo_lock::MutexRecover as _;

/// Identifies a version of the file (so an unchanged file costs one `stat`), including the inode since a rename-over-target producer can collide on length + mtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    inode: u64,
    len: u64,
    mtime: Option<std::time::SystemTime>,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            inode: meta.ino(),
            len: meta.len(),
            mtime: meta.modified().ok(),
        }
    }
}

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
    principals: Vec<Principal>,
    handle: AclHandle,
    /// The version last successfully loaded. `None` once observed missing, so reappearance always reloads.
    seen: Mutex<Option<Stamp>>,
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
        principals: Vec<Principal>,
    ) -> Result<Self, CapsError> {
        let path = path.into();
        let (caps, stamp) = read_capabilities(&path)?;
        let acl = build(identity, &caps, &principals);
        Ok(Self {
            path,
            identity,
            principals,
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
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
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

        let stamp = Stamp::of(&meta);
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
                    .replace(build(self.identity, &caps, &self.principals));
                *self.seen.lock_recover() = Some(stamp);
                Refresh::Reloaded(n)
            }
            // Deliberately doesn't update `seen`, so the next tick retries rather than treating a half-written file as current.
            Err(e) => Refresh::Failed(e.to_string()),
        }
    }
}

impl CapabilityFile {
    /// Keep the live mount's list current: re-read on every change of the
    /// file, from the host's file events, and on `cadence.backstop`; poll
    /// every `cadence.poll` instead if the file cannot be watched
    /// ([`ReloadTrigger`]). The first read happens at once, so a change
    /// made between [`Self::load`] and this call is not missed.
    #[must_use]
    pub fn spawn_reload(self: Arc<Self>, cadence: ReloadCadence) -> tokio::task::JoinHandle<()> {
        let mut trigger = ReloadTrigger::for_file(&self.path, cadence);
        tokio::spawn(async move {
            loop {
                trigger.next().await;
                self.report(&self.refresh());
            }
        })
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
fn build(identity: IdentityKey, caps: &[CapOverride], principals: &[Principal]) -> WorkspaceAcl {
    WorkspaceAcl::from_capabilities(identity, caps, principals, None)
}

fn read_capabilities(path: &Path) -> Result<(Vec<CapOverride>, Stamp), CapsError> {
    let file = std::fs::File::open(path).map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    // Stamp from the same open file, not a second `stat`: a rewrite in between could pair the new stamp with the old bytes.
    let stamp = Stamp::of(&file.metadata().map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?);
    let authority: collab_types::FsAuthority =
        serde_json::from_reader(std::io::BufReader::new(file)).map_err(|source| {
            CapsError::Parse {
                path: path.to_path_buf(),
                source,
            }
        })?;
    Ok((authority.capabilities, stamp))
}

#[derive(Debug, thiserror::Error)]
pub enum CapsError {
    #[error("capability list {path} could not be read: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("capability list {path} is not the governor's file authority: {source}")]
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
    use collab_types::{Access as CapAccess, Ed25519PublicKey, Effect, Scope, Subject};

    const ALICE: IdentityKey = [0xA1; 32];

    fn read_grant(path: &str) -> CapOverride {
        CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
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

    #[test]
    fn capability_file_changes_take_effect_during_the_mount() {
        // Phase `a_grant_added_after_load_takes_effect_without_reloading_by_hand`.
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

            // The grant the user makes a minute into the session.
            write_caps(&path, &[read_grant("/w/docs"), read_grant("/w/plans")]);
            assert_eq!(file.refresh(), Refresh::Reloaded(2));

            assert!(
                handle.permits(&ALICE, "/w/plans/b.md", Access::Read),
                "a grant made after the mount came up must reach the same handle the mount holds"
            );
            assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));
        }
        // Phase `a_revoked_grant_stops_working_without_a_remount`.
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
        // Phase `an_unchanged_file_is_not_reparsed`.
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
                &[read_grant("/w/docs").expiring(collab_types::Expiry::AtUnix(now + 3600))],
            );
            let live = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
            assert!(live.handle().permits(&ALICE, "/w/docs/a.md", Access::Read));

            write_caps(
                &path,
                &[read_grant("/w/docs").expiring(collab_types::Expiry::AtUnix(now - 1))],
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

    /// How long the mount may keep serving a revoked capability: what the
    /// one-second poll gave before the reload watched the file.
    const REVOKE_BOUND: std::time::Duration = std::time::Duration::from_secs(1);

    /// How long `holds` took to become true, or `None` if it did not
    /// within [`REVOKE_BOUND`].
    async fn within_bound(holds: impl Fn() -> bool) -> Option<std::time::Duration> {
        let started = std::time::Instant::now();
        while started.elapsed() <= REVOKE_BOUND {
            if holds() {
                return Some(started.elapsed());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        None
    }

    /// #4224. The reloader the daemon runs, with its poll fallback and its
    /// backstop both an hour away, so only the file watch can deliver a
    /// change. A revocation and a removal each reach the live mount within
    /// the old one-second bound, and a half-written file still keeps the
    /// previous list.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_revocation_reaches_the_live_mount_through_the_file_watch_alone() {
        let hour = std::time::Duration::from_secs(3600);
        let cadence = ReloadCadence {
            poll: hour,
            backstop: hour,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        write_caps(&path, &[read_grant("/w/docs"), read_grant("/w/plans")]);
        let file = Arc::new(CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads"));
        let handle = file.handle();
        assert!(
            ReloadTrigger::for_file(&path, cadence).is_watching(),
            "the capability list must be watchable here, or this proves nothing"
        );
        let reload = Arc::clone(&file).spawn_reload(cadence);
        assert!(handle.permits(&ALICE, "/w/plans/b.md", Access::Read));

        write_caps(&path, &[read_grant("/w/docs")]);
        let revoked = within_bound(|| !handle.permits(&ALICE, "/w/plans/b.md", Access::Read))
            .await
            .expect("a revocation must reach the live mount within the bound");
        eprintln!(
            "revocation applied {} ms after the write",
            revoked.as_millis()
        );
        assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));

        // What a reader sees if it catches an in-place rewrite midway.
        std::fs::write(&path, b"[{\"subject\"").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            handle.permits(&ALICE, "/w/docs/a.md", Access::Read),
            "a half-written file must not revoke every capability"
        );

        std::fs::remove_file(&path).unwrap();
        within_bound(|| !handle.permits(&ALICE, "/w/docs/a.md", Access::Read))
            .await
            .expect("removing the list must deny everything within the bound");

        write_caps(&path, &[read_grant("/w/plans")]);
        within_bound(|| handle.permits(&ALICE, "/w/plans/b.md", Access::Read))
            .await
            .expect("a list that comes back must be enforced again");
        reload.abort();
    }

    /// The list tmpfiles seeded on a machine installed on 2026-10-02, byte
    /// for byte. `C` never overwrites it, so after the update that wrapped
    /// the list in a file authority object it is still what fs.service
    /// reads; refusing it crash-looped the service and left
    /// `/var/workspace` unmounted.
    #[test]
    fn the_list_an_older_image_seeded_still_mounts_with_its_grant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fs-capabilities.json");
        std::fs::write(
            &path,
            r#"[
  {
    "subject": { "Principal": { "Person": "local" } },
    "scope": { "FsPath": "/" },
    "effect": "Grant",
    "verb": "Write",
    "id": "local-session-workspace",
    "expiry": "Never",
    "granted_by": null
  }
]
"#,
        )
        .unwrap();
        let local = vec![Principal::Person("local".into())];
        let file = CapabilityFile::load(&path, ALICE, local).expect("the old list must load");
        let handle = file.handle();
        assert!(handle.permits(&ALICE, "/notes/today.md", Access::Write));
        let outsider = CapabilityFile::load(&path, ALICE, Vec::new())
            .unwrap()
            .handle();
        assert!(
            !outsider.permits(&ALICE, "/notes/today.md", Access::Read),
            "migrating must not widen the grant beyond its principal"
        );
    }

    #[test]
    fn missing_deleted_malformed_or_empty_capability_files_fail_closed() {
        // Phase `a_deleted_file_denies_everything`.
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

            // And it recovers when the file comes back.
            write_caps(&path, &[read_grant("/w/docs")]);
            assert_eq!(file.refresh(), Refresh::Reloaded(1));
            assert!(handle.permits(&ALICE, "/w/docs/a.md", Access::Read));
        }
        // Phase `a_malformed_file_keeps_the_last_good_list_and_retries`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("caps.json");
            write_caps(&path, &[read_grant("/w/docs")]);
            let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
            let handle = file.handle();

            // What a reader sees if it catches an in-place rewrite midway.
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
        // Phase `a_missing_file_refuses_to_load_rather_than_binding_nothing`.
        {
            let err = CapabilityFile::load("/nonexistent/tilo-caps.json", ALICE, Vec::new())
                .expect_err("a missing list must not load as an unenforced mount");
            assert!(matches!(err, CapsError::Read { .. }), "got {err:?}");
        }
        // Phase `an_empty_list_is_a_valid_total_denial`.
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
