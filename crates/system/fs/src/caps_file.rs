//! The capability list a mount enforces, kept current while it runs.
//!
//! `--acl FILE` binds the [`collab_types::FsAuthority`] the governor exports to a mount.
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
    members: Vec<MemberId>,
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
    /// Returns [`CapsError::Read`] when `path` cannot be opened or stat'd,
    /// [`CapsError::Parse`] when its contents are not an [`collab_types::FsAuthority`],
    /// A list an older image seeded in a schema this build no longer reads is not a refusal: it is
    /// set aside beside itself and the image's own default ([`IMAGE_DEFAULT`]) takes its place, so
    /// an update never leaves the mount (and the session that needs it) down on state it cannot
    /// read. Only that recognised older schema is replaced; any other malformed list still fails
    /// closed, since it may be an administrator's edit.
    pub fn load(
        path: impl Into<PathBuf>,
        identity: IdentityKey,
        members: Vec<MemberId>,
    ) -> Result<Self, CapsError> {
        Self::load_with_default(path, Path::new(IMAGE_DEFAULT), identity, members)
    }

    /// [`Self::load`] with the image default read from `default`.
    ///
    /// # Errors
    ///
    /// As [`Self::load`]; also [`CapsError::Reseed`] when an older list cannot be set aside or the
    /// default cannot take its place.
    pub fn load_with_default(
        path: impl Into<PathBuf>,
        default: &Path,
        identity: IdentityKey,
        members: Vec<MemberId>,
    ) -> Result<Self, CapsError> {
        let path = path.into();
        let (caps, stamp) = match read_capabilities(&path) {
            Err(CapsError::Superseded { .. }) => {
                let kept = reseed_from_default(&path, default)?;
                eprintln!(
                    "capability list {} was in an older image's schema; kept it as {} and seeded \
                     the image's default from {}",
                    path.display(),
                    kept.display(),
                    default.display()
                );
                read_capabilities(&path)?
            }
            read => read?,
        };
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
/// Where the image keeps the capability list it seeds (`tmpfiles.d/tilo.conf` copies it to
/// `/var/lib/tilo/fs-capabilities.json` when that is absent).
pub const IMAGE_DEFAULT: &str = "/usr/share/tilo/fs-capabilities.json";

/// Move the list at `path` aside (to `<path>.older-image-<unix seconds>`) and put a copy of
/// `default` in its place, the way tmpfiles seeds it; returns where the old list went.
fn reseed_from_default(path: &Path, default: &Path) -> Result<PathBuf, CapsError> {
    let reseed = |source| CapsError::Reseed {
        path: path.to_path_buf(),
        source,
    };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut kept = path.as_os_str().to_owned();
    kept.push(format!(".older-image-{secs}"));
    let kept = PathBuf::from(kept);
    let fresh = std::fs::read(default).map_err(reseed)?;
    std::fs::rename(path, &kept).map_err(reseed)?;
    tilo_files::native::atomic::write(path, &fresh).map_err(reseed)?;
    Ok(kept)
}

fn build(identity: IdentityKey, caps: &[CapOverride], members: &[MemberId]) -> WorkspaceAcl {
    WorkspaceAcl::from_capabilities(identity, caps, members, None)
}

fn read_capabilities(path: &Path) -> Result<(Vec<CapOverride>, Stamp), CapsError> {
    let mut file = std::fs::File::open(path).map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    // Stamp from the same open file, not a second `stat`: a rewrite in between could pair the new stamp with the old bytes.
    let stamp = Stamp::of(&file.metadata().map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| CapsError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    let authority = authority(&bytes, path)?;
    Ok((authority.capabilities, stamp))
}

/// The file authority `bytes` hold, or why they are not the one this build
/// reads.
///
/// # Errors
///
/// [`CapsError::Parse`] when the bytes are not the governor's file authority,
/// and [`CapsError::Superseded`] when they name a capability's lifetime an
/// `expiry` as an older image's list did.
pub fn authority(bytes: &[u8], path: &Path) -> Result<collab_types::FsAuthority, CapsError> {
    let parse = |source| CapsError::Parse {
        path: path.to_path_buf(),
        source,
    };
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(parse)?;
    if names_a_superseded_expiry(&value) {
        return Err(CapsError::Superseded {
            path: path.to_path_buf(),
        });
    }
    serde_json::from_value(value).map_err(parse)
}

/// tmpfiles seeds `/var/lib/tilo/fs-capabilities.json` with `C`, which never
/// overwrites, so a machine installed from an image whose list said how long a
/// capability stands as an `expiry` keeps that file. The schema no longer
/// reads it, and the file is an administrator's to remove: naming the cause
/// says what to do, where the parse error alone would not.
fn names_a_superseded_expiry(authority: &serde_json::Value) -> bool {
    authority
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|capabilities| {
            capabilities.iter().any(|capability| {
                capability.get("expiry").is_some() && capability.get("lifetime").is_none()
            })
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
    #[error("capability list {path} is not the governor's file authority: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "capability list {path} was seeded by an older image and names `expiry` where it now \
         says `lifetime`; remove it so the image seeds its default again, or write the list in \
         the current schema"
    )]
    Superseded { path: PathBuf },
    #[error(
        "capability list {path} is an older image's and could not be replaced by the image's default: {source}"
    )]
    Reseed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
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
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "test arithmetic: the lint exempts the body of a test function, not the closure a phase is"
        )]
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
            {
                // What tmpfiles' `C` leaves on a machine installed before a capability said how
                // long it stands as a `lifetime`: both an image's shape with `grantee` and the one
                // a laptop installed in October kept (an older name for the grantee, and `expiry`). An update reads the
                // image's default in its place and keeps the old list beside it, so the mount (and
                // the session that needs it) comes up.
                let older = [
                    &br#"{"capabilities":[{"grantee":{"member":{"person":"local"}},
                        "scope":{"FsPath":"/"},"effect":"Grant","access":"Write",
                        "id":"seeded","expiry":"Never"}]}"#[..],
                    &br#"{"capabilities":[{"subject":{"Principal":{"Person":"local"}},
                        "scope":{"FsPath":"/"},"effect":"Grant","access":"Write",
                        "id":"local-session-workspace","expiry":"Never","granted_by":null}]}"#[..],
                ];
                for old in older {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("caps.json");
                    let default = dir.path().join("image-default.json");
                    std::fs::write(&path, old).unwrap();
                    write_caps(&default, &[read_grant("/")]);
                    let file = CapabilityFile::load_with_default(&path, &default, ALICE, Vec::new())
                        .expect("an older image's list gives way to the image's default");
                    assert!(
                        file.handle().permits(&ALICE, "/w/a.md", Access::Read),
                        "the default's grant is enforced"
                    );
                    assert_eq!(
                        std::fs::read(&path).unwrap(),
                        std::fs::read(&default).unwrap(),
                        "the list in place is the image's default"
                    );
                    let kept: Vec<_> = std::fs::read_dir(dir.path())
                        .unwrap()
                        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                        .filter(|name| name.starts_with("caps.json.older-image-"))
                        .collect();
                    assert_eq!(kept.len(), 1, "the older list is kept beside it: {kept:?}");
                    assert_eq!(
                        std::fs::read(dir.path().join(&kept[0])).unwrap(),
                        old,
                        "kept as it was"
                    );
                }
            }
            {
                // Anything else this build cannot read is still refused, never replaced: it may
                // be an administrator's edit.
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("caps.json");
                let default = dir.path().join("image-default.json");
                std::fs::write(&path, br#"{"capabilities":[{"scope":"#).unwrap();
                write_caps(&default, &[read_grant("/")]);
                let error = CapabilityFile::load_with_default(&path, &default, ALICE, Vec::new())
                    .expect_err("a malformed list fails closed");
                assert!(matches!(error, CapsError::Parse { .. }), "got {error:?}");
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    br#"{"capabilities":[{"scope":"#,
                    "a malformed list is left as it is"
                );
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
