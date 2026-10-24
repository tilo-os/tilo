//! Which agent caused a write, as the desktop session says.
//!
//! An agent has no filesystem access of its own. It invokes an app's action,
//! and the app, or the session for a clipboard transfer, writes through the
//! mount, so the version is this device's, signed for its person like any
//! other write. The session knows from its authenticated action context which
//! agent the action ran for. Over the session-only document media socket
//! (`SessionBlobs.attributeWrites`, which no sandbox reaches) it grants a short
//! lease naming the paths the action writes and that agent. Each version this
//! device then makes of one of those paths carries a signed
//! [`crate::record_replication::RecordOperationV1::AttributeVersion`], so
//! Files' history names the agent, on behalf of the person, on every device.
//!
//! A lease grants nothing: it changes who a version is shown as, never what
//! anybody may write, and only ever names an agent of this device's person.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use fs_protocol::FileAttributionV1;
use thiserror::Error;

/// Longest a lease lasts. An action's writes land within it; a write after
/// it is the person's again.
pub const MAX_LEASE_TTL: Duration = Duration::from_secs(60);
/// Most leases held at once; granting another drops the oldest.
pub const MAX_LEASES: usize = 64;
/// Most paths one lease names.
pub const MAX_LEASE_PATHS: usize = 256;
/// Longest path a lease names, in bytes.
pub const MAX_LEASE_PATH_BYTES: usize = 4096;

/// Why a lease was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WriteAttributionError {
    #[error("the attribution does not name a presentable agent")]
    InvalidAttribution,
    #[error("a lease names between one and {MAX_LEASE_PATHS} paths inside the mount")]
    InvalidPaths,
    #[error("a lease lasts more than nothing and at most a minute")]
    InvalidDuration,
}

/// The session's live attribution leases.
#[derive(Debug, Default)]
pub struct WriteAttributions {
    leases: Mutex<Vec<Lease>>,
}

#[derive(Debug)]
struct Lease {
    paths: Vec<String>,
    attribution: FileAttributionV1,
    until: Instant,
}

impl WriteAttributions {
    /// Attribute this device's versions of `paths` (mount-relative, `/`-rooted,
    /// each covering its subtree) to `attribution` for `ttl`.
    ///
    /// # Errors
    ///
    /// Refuses an invalid attribution, no or too many paths, a path that is
    /// the mount root or not a clean absolute path, and a duration that is
    /// zero or longer than [`MAX_LEASE_TTL`].
    pub fn grant(
        &self,
        paths: Vec<String>,
        attribution: FileAttributionV1,
        ttl: Duration,
    ) -> Result<(), WriteAttributionError> {
        attribution
            .validate()
            .map_err(|_| WriteAttributionError::InvalidAttribution)?;
        if paths.is_empty()
            || paths.len() > MAX_LEASE_PATHS
            || !paths.iter().all(|path| clean_path(path))
        {
            return Err(WriteAttributionError::InvalidPaths);
        }
        if ttl.is_zero() || ttl > MAX_LEASE_TTL {
            return Err(WriteAttributionError::InvalidDuration);
        }
        let now = Instant::now();
        let mut leases = self
            .leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        leases.retain(|lease| lease.until > now);
        if leases.len() >= MAX_LEASES {
            leases.remove(0);
        }
        leases.push(Lease {
            paths,
            attribution,
            until: now.checked_add(ttl).unwrap_or(now),
        });
        drop(leases);
        Ok(())
    }

    /// Whether no lease is live: every write is its person's alone.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        let now = Instant::now();
        self.leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .all(|lease| lease.until <= now)
    }

    /// The agent the newest live lease covering `path` names.
    #[must_use]
    pub fn for_path(&self, path: &str) -> Option<FileAttributionV1> {
        let now = Instant::now();
        let mut leases = self
            .leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        leases.retain(|lease| lease.until > now);
        leases
            .iter()
            .rev()
            .find(|lease| lease.paths.iter().any(|leased| covers(leased, path)))
            .map(|lease| lease.attribution.clone())
    }
}

/// A `/`-rooted path below the mount root, without empty, `.` or `..`
/// components.
fn clean_path(path: &str) -> bool {
    path.len() <= MAX_LEASE_PATH_BYTES
        && path.len() > 1
        && path.starts_with('/')
        && !path.contains('\0')
        && path
            .split('/')
            .skip(1)
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Whether `leased` names `path` or a folder holding it.
fn covers(leased: &str, path: &str) -> bool {
    path.strip_prefix(leased)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The mount-relative path of `absolute`, a path under `mount_point`.
#[must_use]
pub fn mount_relative(mount_point: &std::path::Path, absolute: &str) -> Option<String> {
    let relative = std::path::Path::new(absolute)
        .strip_prefix(mount_point)
        .ok()?
        .to_str()?;
    let path = format!("/{relative}");
    clean_path(&path).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scribe() -> FileAttributionV1 {
        FileAttributionV1 {
            actor: "agent:scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
        }
    }

    #[test]
    fn a_lease_names_its_paths_and_their_subtrees_until_it_ends() {
        let leases = WriteAttributions::default();
        leases
            .grant(
                vec!["/Reports".into(), "/notes.txt".into()],
                scribe(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(leases.for_path("/Reports"), Some(scribe()));
        assert_eq!(leases.for_path("/Reports/q3.txt"), Some(scribe()));
        assert_eq!(leases.for_path("/notes.txt"), Some(scribe()));
        assert_eq!(leases.for_path("/Reports2/q3.txt"), None, "a sibling");
        assert_eq!(leases.for_path("/notes.txt.bak"), None);
        assert_eq!(leases.for_path("/other.txt"), None);

        leases
            .grant(
                vec!["/brief.txt".into()],
                scribe(),
                Duration::from_millis(1),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(leases.for_path("/brief.txt"), None, "the lease ended");
    }

    #[test]
    fn a_lease_never_covers_the_whole_mount_or_escapes_it() {
        let leases = WriteAttributions::default();
        let ttl = Duration::from_secs(1);
        for paths in [
            vec![],
            vec!["/".to_string()],
            vec!["relative".to_string()],
            vec!["/a/../b".to_string()],
            vec!["/a//b".to_string()],
        ] {
            assert_eq!(
                leases.grant(paths.clone(), scribe(), ttl),
                Err(WriteAttributionError::InvalidPaths),
                "{paths:?}"
            );
        }
        let person = FileAttributionV1 {
            actor: "ada".into(),
            ..scribe()
        };
        assert_eq!(
            leases.grant(vec!["/a".into()], person, ttl),
            Err(WriteAttributionError::InvalidAttribution),
            "only an agent is attributed"
        );
        assert_eq!(
            leases.grant(vec!["/a".into()], scribe(), MAX_LEASE_TTL * 2),
            Err(WriteAttributionError::InvalidDuration)
        );
        assert_eq!(
            mount_relative(std::path::Path::new("/var/workspace"), "/var/workspace/a/b"),
            Some("/a/b".into())
        );
        assert_eq!(
            mount_relative(std::path::Path::new("/var/workspace"), "/etc/passwd"),
            None
        );
        assert_eq!(
            mount_relative(std::path::Path::new("/var/workspace"), "/var/workspace"),
            None
        );
    }
}
