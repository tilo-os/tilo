//! The capability list a mount enforces, kept current while it runs.
//!
//! `--acl FILE` binds a JSON array of [`CapOverride`] records to a mount.
//! `/var/workspace` is mounted once at boot and stays up for the session,
//! so binding the list by value at mount time would make "a later grant
//! needs a remount" mean "a later grant does not apply" — a wrong answer,
//! not a slow update. [`CapabilityFile`] owns the file and an
//! [`AclHandle`]; [`CapabilityFile::refresh`] re-reads when the file has
//! changed and swaps the result in underneath the live mount.
//!
//! The three outcomes are deliberately not treated alike. A changed,
//! parseable file swaps in — the point. A **gone** file binds an empty
//! (deny-everything) ACL: removing the file is how the producer says
//! "this identity holds nothing", so keeping the previous list would make
//! a revocation ineffective (distinct from *never passing* `--acl`,
//! which leaves the mount unenforced). An **unreadable or malformed**
//! file keeps the previous list and logs — a capability file is rewritten
//! in place rather than renamed over, so a reader can catch it truncated
//! mid-write, and revoking every capability on the machine for that
//! transient window would make the reloader itself the outage.
//!
//! None of these can *widen* access beyond what some successfully-parsed
//! file said.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use collab_types::{CapOverride, Principal};

use crate::acl::{AclHandle, IdentityKey, WorkspaceAcl};

/// Identifies a version of the file, so an unchanged file costs one
/// `stat` and no parse.
///
/// Includes the inode because a producer that writes a temporary file
/// and renames it over the target can produce a new file with the same
/// length and (at one-second granularity) the same mtime.
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

/// What a [`CapabilityFile::refresh`] did.
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

/// A capability list on disk, bound to a live mount.
///
/// Construct with [`CapabilityFile::load`] — which fails if the file
/// cannot be read, so a mount is never brought up believing it is
/// enforcing a list it never saw — then hand [`CapabilityFile::handle`]
/// to [`crate::QuickFS::with_acl`] and call [`CapabilityFile::refresh`]
/// periodically.
#[derive(Debug)]
pub struct CapabilityFile {
    path: PathBuf,
    identity: IdentityKey,
    principals: Vec<Principal>,
    handle: AclHandle,
    /// The version last successfully loaded. `None` once the file has
    /// been observed missing, so its reappearance always reloads.
    seen: Mutex<Option<Stamp>>,
}

impl CapabilityFile {
    /// Read `path` and bind its capabilities for `identity`.
    ///
    /// Fail-closed: a missing, unreadable or malformed file is an error
    /// and the caller is expected to refuse the mount. Silently mounting
    /// unenforced when enforcement was requested is how a typo in a unit
    /// file becomes a wide-open workspace.
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

    /// The handle to give [`crate::QuickFS::with_acl`]. Cloning it is
    /// cheap and every clone sees each swap.
    pub fn handle(&self) -> AclHandle {
        self.handle.clone()
    }

    /// The identity the bound capabilities answer for.
    pub fn identity(&self) -> IdentityKey {
        self.identity
    }

    /// The file being watched.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many capabilities are in force right now. For logging.
    pub fn count(&self) -> usize {
        self.handle.snapshot().capability_count()
    }

    /// Re-read the file if it has changed, and swap the result into the
    /// live mount.
    ///
    /// Cheap to call often: an unchanged file costs a single `stat`.
    pub fn refresh(&self) -> Refresh {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut seen = self.seen.lock().expect("capability stamp poisoned");
                if seen.is_none() {
                    // Already observed missing and already denying.
                    return Refresh::Unchanged;
                }
                *seen = None;
                drop(seen);
                // Deny everything. See the module docs: an absent file is
                // the producer saying "nothing is granted", and is not
                // the same as never having asked for enforcement.
                self.handle.replace(WorkspaceAcl::new());
                return Refresh::Revoked;
            }
            Err(e) => return Refresh::Failed(format!("{}: {e}", self.path.display())),
        };

        let stamp = Stamp::of(&meta);
        {
            let seen = self.seen.lock().expect("capability stamp poisoned");
            if *seen == Some(stamp) {
                return Refresh::Unchanged;
            }
        }

        match read_capabilities(&self.path) {
            Ok((caps, stamp)) => {
                let n = caps.len();
                self.handle
                    .replace(build(self.identity, &caps, &self.principals));
                *self.seen.lock().expect("capability stamp poisoned") = Some(stamp);
                Refresh::Reloaded(n)
            }
            // Deliberately does **not** update `seen`, so the next tick
            // retries rather than treating a half-written file as the
            // current version forever.
            Err(e) => Refresh::Failed(e.to_string()),
        }
    }
}

/// Build the ACL for one identity from a capability list.
///
/// `now: None` ⇒ every check reads the live clock, so a time-limited
/// capability really does lapse during a long-lived mount.
fn build(identity: IdentityKey, caps: &[CapOverride], principals: &[Principal]) -> WorkspaceAcl {
    WorkspaceAcl::from_capabilities(identity, caps, principals, None)
}

fn read_capabilities(path: &Path) -> Result<(Vec<CapOverride>, Stamp), CapsError> {
    let file = std::fs::File::open(path).map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    // Stamp from the same open file, not a second `stat`: a rewrite
    // between the two would otherwise record the new version's stamp
    // against the old version's bytes and never reload.
    let stamp = Stamp::of(&file.metadata().map_err(|source| CapsError::Read {
        path: path.to_path_buf(),
        source,
    })?);
    let caps: Vec<CapOverride> =
        serde_json::from_reader(std::io::BufReader::new(file)).map_err(|source| {
            CapsError::Parse {
                path: path.to_path_buf(),
                source,
            }
        })?;
    Ok((caps, stamp))
}

/// Why a capability list could not be bound.
#[derive(Debug, thiserror::Error)]
pub enum CapsError {
    #[error("capability list {path} could not be read: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("capability list {path} is not a JSON array of capabilities: {source}")]
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
    use collab_types::{Ed25519PublicKey, Effect, Scope, Subject, Verb};

    const ALICE: IdentityKey = [0xA1; 32];

    fn read_grant(path: &str) -> CapOverride {
        CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath(path.into()),
            Effect::Grant,
            Verb::Read,
        )
    }

    fn write_caps(path: &Path, caps: &[CapOverride]) {
        std::fs::write(path, serde_json::to_vec(caps).unwrap()).unwrap();
        // The stamp includes mtime at whatever resolution the filesystem
        // reports; two writes inside one tick would otherwise be
        // indistinguishable. Length differs in these tests, but make the
        // ordering unambiguous rather than relying on that.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    #[test]
    fn a_grant_added_after_load_takes_effect_without_reloading_by_hand() {
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

    #[test]
    fn a_revoked_grant_stops_working_without_a_remount() {
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

    #[test]
    fn an_unchanged_file_is_not_reparsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        write_caps(&path, &[read_grant("/w/docs")]);
        let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("loads");
        assert_eq!(file.refresh(), Refresh::Unchanged);
        assert_eq!(file.refresh(), Refresh::Unchanged);
    }

    #[test]
    fn a_deleted_file_denies_everything() {
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

    #[test]
    fn a_malformed_file_keeps_the_last_good_list_and_retries() {
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
        // Not latched: the next tick tries again rather than treating the
        // truncated bytes as the current version.
        assert!(matches!(file.refresh(), Refresh::Failed(_)));

        write_caps(&path, &[read_grant("/w/plans")]);
        assert_eq!(file.refresh(), Refresh::Reloaded(1));
        assert!(handle.permits(&ALICE, "/w/plans/b.md", Access::Read));
        assert!(!handle.permits(&ALICE, "/w/docs/a.md", Access::Read));
    }

    #[test]
    fn a_missing_file_refuses_to_load_rather_than_binding_nothing() {
        let err = CapabilityFile::load("/nonexistent/qos-caps.json", ALICE, Vec::new())
            .expect_err("a missing list must not load as an unenforced mount");
        assert!(matches!(err, CapsError::Read { .. }), "got {err:?}");
    }

    #[test]
    fn an_empty_list_is_a_valid_total_denial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        std::fs::write(&path, b"[]").unwrap();
        let file = CapabilityFile::load(&path, ALICE, Vec::new()).expect("an empty list is valid");
        assert!(!file.handle().permits(&ALICE, "/", Access::Read));
    }

    // An expiry must lapse *during* the mount, which is only true if the
    // bound ACL reads the live clock rather than the load-time instant.
    #[test]
    fn a_capability_that_expires_mid_mount_stops_permitting() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
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
            !lapsed.handle().permits(&ALICE, "/w/docs/a.md", Access::Read),
            "an expiry in the past must not permit"
        );
    }
}
