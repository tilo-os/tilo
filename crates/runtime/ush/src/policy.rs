//! Saved permission decisions — `policy.json`.
//!
//! # There is no `ush` permission type
//!
//! A decision here is a `collab_types::caps::CapOverride`, evaluated by the
//! same `decide_capability` the compositor's permissions panel and
//! `tilo-launcher`/the FUSE mount use. `perm trust-dir ~/src` is not a
//! second kind of grant — it mints exactly:
//!
//! ```text
//! CapOverride { subject: Principal::App("so.tilo.ush"),
//!               scope:   Scope::FsPath("/var/home/you/src"),
//!               effect:  Effect::Grant,
//!               verb:    Verb::Write }
//! ```
//!
//! byte-for-byte what the compositor writes for "Always allow" on a
//! filesystem prompt, against the subject [`crate::RUNTIME_APP_ID`]. A
//! revocation from the panel removes it, and the next session doesn't
//! bind the directory.
//!
//! # Deny by default
//!
//! [`Policy::load`] returns an empty policy for a file that is missing,
//! unreadable or malformed — every failure mode grants nothing, so the
//! guest gets the base and its own layer, a working shell with no access
//! to your files. Nothing here can widen a decision the user did not make.

use std::path::{Path, PathBuf};

use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};

use crate::RUNTIME_APP_ID;

/// The saved decisions, as they sit on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Policy {
    /// Every decision the user has made, in the shared vocabulary.
    #[serde(default)]
    pub capabilities: Vec<CapOverride>,
}

impl Policy {
    /// Read `policy.json`. Never fails: see the module documentation.
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(), error = %e,
                    "policy file unreadable; the guest gets the base and its own layer only"
                );
                return Self::default();
            }
        };
        match serde_json::from_slice::<Policy>(&bytes) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(), error = %e,
                    "policy file malformed; the guest gets the base and its own layer only"
                );
                Self::default()
            }
        }
    }

    /// Write `policy.json`, creating its directory. Only decisions that
    /// outlive a restart are written — a session-scoped grant surviving into
    /// the next session would be one the user never made (mirrors
    /// `crates/session/shell/src/shell/persist.rs`).
    ///
    /// # Errors
    ///
    /// Returns an error if `path`'s parent can't be created, or the
    /// temporary file can't be written or renamed into place.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let durable = Policy {
            capabilities: self
                .capabilities
                .iter()
                .filter(|c| c.persists_across_restart())
                .cloned()
                .collect(),
        };
        tilo_files::native::atomic::write_creating_parent(
            path,
            &serde_json::to_vec_pretty(&durable)?,
        )
    }

    /// The actor every question is asked about. `role: None` is load-bearing:
    /// `collab_types::role_base_applies` returns false for `FsPath`, `Device`
    /// and `NetHost`, so no role — not even Owner — reaches a path; every bind
    /// below exists because a capability says so.
    pub fn actor() -> Actor {
        Actor::app(RUNTIME_APP_ID)
    }

    /// Trust a directory: read *and* write, for good. Returns the capability
    /// id, or `None` when the runtime refuses to bind it ([`is_trustable`]).
    pub fn trust_dir(&mut self, dir: &Path) -> Option<String> {
        let raw = dir.to_string_lossy().into_owned();
        if !is_trustable(dir) {
            return None;
        }
        let id = format!("ush-trust-{}", stable_id(&raw));
        // Same subject, scope and verb collapses onto one record rather than
        // growing the file every time the user re-runs the command.
        self.capabilities.retain(|c| c.id != id);
        self.capabilities.push(
            CapOverride::grant(
                Subject::Principal(Principal::App(RUNTIME_APP_ID.to_string())),
                Scope::FsPath(raw),
                Verb::Write,
            )
            .with_id(id.clone())
            .granted_by(Principal::Person("local".to_string())),
        );
        Some(id)
    }

    /// Drop a decision by id. Returns whether anything was there.
    pub fn revoke(&mut self, id: &str) -> bool {
        let before = self.capabilities.len();
        self.capabilities.retain(|c| c.id != id);
        self.capabilities.len() != before
    }

    /// Every directory the guest may write, as of `now`.
    pub fn trusted_dirs(&self, now: Option<u64>) -> Vec<PathBuf> {
        self.paths_where(Verb::Write, now)
    }

    /// Every directory the guest may read but not write.
    pub fn readable_dirs(&self, now: Option<u64>) -> Vec<PathBuf> {
        let writable = self.trusted_dirs(now);
        self.paths_where(Verb::Read, now)
            .into_iter()
            .filter(|p| !writable.contains(p))
            .collect()
    }

    fn paths_where(&self, verb: Verb, now: Option<u64>) -> Vec<PathBuf> {
        let actor = Self::actor();
        let mut out: Vec<PathBuf> = Vec::new();
        for cap in &self.capabilities {
            // Only a Grant nominates a path; a Restrict can take one away (through
            // `decide_capability` below) but never introduce one.
            if cap.effect != Effect::Grant || !cap.subject.matches_actor(&actor) {
                continue;
            }
            let Scope::FsPath(raw) = &cap.scope else {
                continue;
            };
            if !is_trustable(Path::new(raw)) {
                tracing::warn!(path = %raw, "ignoring a filesystem decision the runtime will not bind");
                continue;
            }
            let resource = ResourceRef::FsPath(raw.clone());
            if decide_capability(&actor, &resource, verb, &self.capabilities, now)
                != CapDecision::Allow
            {
                continue;
            }
            let path = PathBuf::from(raw);
            if !out.contains(&path) {
                out.push(path);
            }
        }
        out.sort();
        out
    }

    /// Is `path` reachable at `verb`? The question `scan` and the broker ask.
    pub fn permits(&self, path: &Path, verb: Verb, now: Option<u64>) -> bool {
        let actor = Self::actor();
        let resource = ResourceRef::FsPath(path.to_string_lossy().into_owned());
        decide_capability(&actor, &resource, verb, &self.capabilities, now) == CapDecision::Allow
    }
}

/// A path the runtime is willing to bind into a guest: absolute, not the
/// root, and not a base subtree the overlay owns — binding `/usr` would put
/// the host's copy back on top of the layer and undo the read-only base;
/// binding `/` would replace the namespace itself.
pub fn is_trustable(path: &Path) -> bool {
    let Some(raw) = path.to_str() else {
        return false;
    };
    if raw.trim().is_empty() || !path.is_absolute() || raw == "/" {
        return false;
    }
    if path.components().any(|c| c.as_os_str() == "..") {
        return false;
    }
    for subtree in crate::layer::BASE_SUBTREES {
        let owned = PathBuf::from("/").join(subtree);
        if path == owned || path.starts_with(&owned) {
            return false;
        }
    }
    true
}

/// A short, stable id for a path, so re-trusting the same directory rewrites
/// one record instead of appending another.
fn stable_id(raw: &str) -> String {
    // FNV-1a. Not a security property — this only has to be deterministic and
    // short enough to read in `perm list`.
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, raw.as_bytes());
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::Expiry;

    #[test]
    fn a_trusted_directory_grant_is_recursive_unique_and_revocable() {
        // phase: trusting_a_directory_mints_the_grant_the_compositor_would_have_written
        {
            let mut policy = Policy::default();
            let id = policy.trust_dir(Path::new("/var/home/you/src")).unwrap();

            assert_eq!(policy.capabilities.len(), 1);
            let cap = &policy.capabilities[0];
            assert_eq!(cap.id, id);
            assert_eq!(cap.effect, Effect::Grant);
            assert_eq!(cap.verb, Verb::Write);
            assert_eq!(cap.scope, Scope::FsPath("/var/home/you/src".to_string()));
            assert_eq!(
                cap.subject,
                Subject::Principal(Principal::App(RUNTIME_APP_ID.to_string())),
                "the grant must name the runtime as an app, or the permissions \
             panel has no subject to show and nothing to revoke"
            );
        }
        // phase: a_trusted_directory_is_writable_and_an_untrusted_one_is_not
        {
            let mut policy = Policy::default();
            policy.trust_dir(Path::new("/var/home/you/src")).unwrap();

            assert_eq!(
                policy.trusted_dirs(Some(1_000)),
                vec![PathBuf::from("/var/home/you/src")]
            );
            assert!(policy.permits(Path::new("/var/home/you/src"), Verb::Write, Some(1_000)));
            assert!(
                policy.permits(
                    Path::new("/var/home/you/src/deep/file.rs"),
                    Verb::Write,
                    Some(1_000)
                ),
                "a filesystem grant is recursive; that is what `path_covers` means"
            );
            assert!(
                !policy.permits(Path::new("/var/home/you/.ssh"), Verb::Read, Some(1_000)),
                "a sibling directory must not come along with the grant"
            );
            assert!(
                !policy.permits(
                    Path::new("/var/home/you/srcextra"),
                    Verb::Write,
                    Some(1_000)
                ),
                "a component-boundary check must not be a prefix check"
            );
        }
        // phase: re_trusting_a_directory_rewrites_one_record
        {
            let mut policy = Policy::default();
            let a = policy.trust_dir(Path::new("/var/home/you/src")).unwrap();
            let b = policy.trust_dir(Path::new("/var/home/you/src")).unwrap();
            assert_eq!(a, b);
            assert_eq!(policy.capabilities.len(), 1);
        }
        // phase: revoking_removes_the_decision
        {
            let mut policy = Policy::default();
            let id = policy.trust_dir(Path::new("/var/home/you/src")).unwrap();
            assert!(policy.revoke(&id));
            assert!(!policy.revoke(&id));
            assert!(policy.trusted_dirs(Some(1_000)).is_empty());
        }
    }

    #[test]
    fn restrictions_and_expiry_narrow_a_trusted_directory() {
        // phase: a_restriction_from_the_shared_vocabulary_removes_a_trusted_directory
        // The one that makes this not a second permission system: a `Restrict`
        // written by the compositor removes the bind here.
        {
            let mut policy = Policy::default();
            policy.trust_dir(Path::new("/var/home/you/src")).unwrap();
            policy.capabilities.push(CapOverride::new(
                Subject::Principal(Principal::App(RUNTIME_APP_ID.to_string())),
                Scope::FsPath("/var/home/you/src".to_string()),
                Effect::Restrict,
                Verb::Read,
            ));

            assert!(
                policy.trusted_dirs(Some(1_000)).is_empty(),
                "a Restrict capped at Read must remove the writable bind"
            );
            assert_eq!(
                policy.readable_dirs(Some(1_000)),
                vec![PathBuf::from("/var/home/you/src")],
                "and must leave the directory readable, not invisible"
            );
        }
        // phase: an_expired_grant_binds_nothing
        {
            let mut policy = Policy::default();
            policy.trust_dir(Path::new("/var/home/you/src")).unwrap();
            policy.capabilities[0].expiry = Expiry::AtUnix(500);

            assert_eq!(
                policy.trusted_dirs(Some(499)),
                vec![PathBuf::from("/var/home/you/src")]
            );
            assert!(
                policy.trusted_dirs(Some(501)).is_empty(),
                "an expired grant must not survive into the next session"
            );
        }
    }

    /// Binding a base subtree would put the host's copy back on top of the
    /// layer and quietly undo the read-only base.
    #[test]
    fn the_runtime_refuses_to_trust_the_paths_the_overlay_owns() {
        for bad in [
            "/",
            "/usr",
            "/usr/bin",
            "/etc",
            "/etc/ssh",
            "relative/path",
            "",
            // A dotdot path is refused.
            "/var/home/you/../../etc",
        ] {
            assert!(
                !is_trustable(Path::new(bad)),
                "{bad:?} must not be trustable"
            );
        }
        for good in ["/var/home/you/src", "/var/workspace/notes", "/opt/thing"] {
            assert!(
                is_trustable(Path::new(good)),
                "{good:?} should be trustable"
            );
        }

        let mut policy = Policy::default();
        assert!(policy.trust_dir(Path::new("/usr")).is_none());
        assert!(policy.capabilities.is_empty());
    }

    #[test]
    fn the_policy_file_persists_only_durable_grants() {
        // phase: a_missing_or_malformed_policy_grants_nothing
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(
                Policy::load(&dir.path().join("absent.json"))
                    .capabilities
                    .is_empty()
            );

            let bad = dir.path().join("bad.json");
            std::fs::write(&bad, b"{ not json ").unwrap();
            assert!(Policy::load(&bad).capabilities.is_empty());

            let wrong = dir.path().join("wrong.json");
            std::fs::write(&wrong, br"[1,2,3]").unwrap();
            assert!(Policy::load(&wrong).capabilities.is_empty());
        }
        // phase: policy_round_trips_and_drops_session_scoped_decisions
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("policy.json");

            let mut policy = Policy::default();
            policy.trust_dir(Path::new("/var/home/you/src")).unwrap();
            let session_id = policy
                .trust_dir(Path::new("/var/home/you/scratch"))
                .unwrap();
            let idx = policy
                .capabilities
                .iter()
                .position(|c| c.id == session_id)
                .unwrap();
            policy.capabilities[idx].expiry = Expiry::EndOfSession;

            policy.save(&path).unwrap();
            let reloaded = Policy::load(&path);
            assert_eq!(
                reloaded.trusted_dirs(Some(1_000)),
                vec![PathBuf::from("/var/home/you/src")],
                "a session-scoped decision must not survive a restart"
            );
        }
    }
}
