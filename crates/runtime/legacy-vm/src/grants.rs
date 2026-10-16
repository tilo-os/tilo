//! Which host directories a legacy VM sees.
//!
//! A share is a `CapOverride` like every other filesystem grant in Tilo:
//!
//! ```text
//! CapOverride { subject: Principal::App("so.tilo.legacy"),
//!               scope:   Scope::FsPath("/var/home/you/src"),
//!               effect:  Grant, verb: Write | Read }
//! ```
//!
//! evaluated with the same `decide_capability` as the permissions panel, so
//! a `Restrict` there narrows a share here. Nothing is shared by default.

use std::path::{Path, PathBuf};

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, Principal, ResourceRef, Scope, Subject,
    decide_capability,
};
use legacy_agent_protocol::{GuestShare, share_path_allowed};

/// The subject every legacy VM grant names.
pub const LEGACY_APP_ID: &str = "so.tilo.legacy";

/// One host directory the VM should see, and how.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct HostShare {
    pub path: PathBuf,
    pub read_only: bool,
}

impl HostShare {
    /// A stable virtio-fs tag for this path (36 bytes at most).
    #[must_use]
    pub fn tag(&self) -> String {
        let mut hasher = fnv::FnvHasher::default();
        std::hash::Hasher::write(&mut hasher, self.path.as_os_str().as_encoded_bytes());
        format!("fs-{:016x}", std::hash::Hasher::finish(&hasher))
    }

    #[must_use]
    pub fn guest(&self) -> GuestShare {
        GuestShare {
            tag: self.tag(),
            path: self.path.display().to_string(),
            read_only: self.read_only,
        }
    }
}

/// The saved grants: `{ "capabilities": [CapOverride…] }`, the same shape
/// as an environment's `policy.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Grants {
    #[serde(default)]
    pub capabilities: Vec<CapOverride>,
}

impl Grants {
    /// A missing or unreadable file grants nothing.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let durable = Self {
            capabilities: self
                .capabilities
                .iter()
                .filter(|cap| cap.persists_across_restart())
                .cloned()
                .collect(),
        };
        tilo_files::native::atomic::write_creating_parent(
            path,
            &serde_json::to_vec_pretty(&durable)?,
        )
    }

    /// Share `dir` with the VM. Returns the grant id, or `None` for a path
    /// that can never be shared (relative, the root, the guest's system).
    pub fn grant(&mut self, dir: &Path, read_only: bool) -> Option<String> {
        if !share_path_allowed(dir) {
            return None;
        }
        let raw = dir.display().to_string();
        let id = format!(
            "legacy-share-{}",
            HostShare {
                path: dir.into(),
                read_only
            }
            .tag()
        );
        self.capabilities.retain(|cap| cap.id != id);
        self.capabilities.push(
            CapOverride::grant(
                Subject::Principal(Principal::App(LEGACY_APP_ID.to_string())),
                Scope::FsPath(raw),
                if read_only {
                    Access::Read
                } else {
                    Access::Write
                },
            )
            .with_id(id.clone())
            .granted_by(Principal::Person("local".to_string())),
        );
        Some(id)
    }

    /// Drop the grant for `dir` (or with id `dir`). Returns whether one existed.
    pub fn revoke(&mut self, dir_or_id: &str) -> bool {
        let before = self.capabilities.len();
        self.capabilities.retain(|cap| {
            cap.id != dir_or_id
                && !(cap.subject.matches_actor(&actor())
                    && cap.scope == Scope::FsPath(dir_or_id.to_string()))
        });
        before != self.capabilities.len()
    }

    /// Every directory the VM sees at `now`, writable ones before read-only.
    #[must_use]
    pub fn shares(&self, now: Option<u64>) -> Vec<HostShare> {
        let actor = actor();
        let mut out: Vec<HostShare> = Vec::new();
        for cap in &self.capabilities {
            if cap.effect != Effect::Grant || !cap.subject.matches_actor(&actor) {
                continue;
            }
            let Scope::FsPath(raw) = &cap.scope else {
                continue;
            };
            let path = PathBuf::from(raw);
            if !share_path_allowed(&path) || out.iter().any(|share| share.path == path) {
                continue;
            }
            let resource = ResourceRef::FsPath(raw.clone());
            let allows = |verb| {
                decide_capability(&actor, &resource, verb, &self.capabilities, now)
                    == CapDecision::Allow
            };
            if allows(Access::Write) {
                out.push(HostShare {
                    path,
                    read_only: false,
                });
            } else if allows(Access::Read) {
                out.push(HostShare {
                    path,
                    read_only: true,
                });
            }
        }
        out.sort();
        out
    }
}

fn actor() -> Actor {
    Actor::app(LEGACY_APP_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_are_exactly_the_granted_directories() {
        // phase: nothing is shared until granted, and grants persist
        {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("grants.json");
            assert!(Grants::load(&file).shares(Some(0)).is_empty());
            let mut grants = Grants::default();
            grants.grant(Path::new("/var/home/you/src"), false).unwrap();
            grants
                .grant(Path::new("/var/home/you/Music"), true)
                .unwrap();
            grants.save(&file).unwrap();
            let shares = Grants::load(&file).shares(Some(0));
            assert_eq!(
                shares,
                [
                    HostShare {
                        path: "/var/home/you/Music".into(),
                        read_only: true
                    },
                    HostShare {
                        path: "/var/home/you/src".into(),
                        read_only: false
                    },
                ]
            );
            assert!(shares[0].tag().len() <= 36);
            assert_ne!(shares[0].tag(), shares[1].tag());
            assert!(shares[1].guest().validate().is_ok());
        }
        // phase: system paths and other subjects' grants are never shared
        {
            let mut grants = Grants::default();
            assert_eq!(grants.grant(Path::new("/usr/lib"), false), None);
            assert_eq!(grants.grant(Path::new("relative"), false), None);
            grants.capabilities.push(CapOverride::grant(
                Subject::Principal(Principal::App("so.tilo.ush".into())),
                Scope::FsPath("/var/home/you/other".into()),
                Access::Write,
            ));
            assert!(grants.shares(Some(0)).is_empty());
        }
        // phase: a restrict narrows a share, and revoking removes it
        {
            let mut grants = Grants::default();
            grants.grant(Path::new("/var/home/you/src"), false).unwrap();
            grants.capabilities.push(CapOverride::new(
                Subject::Principal(Principal::App(LEGACY_APP_ID.into())),
                Scope::FsPath("/var/home/you/src".into()),
                Effect::Restrict,
                Access::Read,
            ));
            assert!(grants.shares(Some(0))[0].read_only);
            assert!(grants.revoke("/var/home/you/src"));
            assert!(grants.shares(Some(0)).is_empty());
            assert!(!grants.revoke("/var/home/you/src"));
        }
    }
}
