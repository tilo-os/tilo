//! The owner's standing decisions to admit a person, and what the workspace did
//! under them for the owner to see.
//!
//! An owner who expects someone (a co-founder, a teammate a backend vouched
//! for) signs a [`PreAdmission`] with their user root for the SHA-256 of a
//! one-time token whoever vouches hands that person. The relay issues a
//! single-use invitation with it; the person who redeems that invitation
//! and presents the token is admitted at once, the grant is spent, and the
//! owner finds a [`WorkspaceNotice`] and may remove them like any member.
//!
//! Kept beside the workspace's record, in `<workspace>.admissions`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tilo_relay_protocol::workspaces::{PreAdmission, WorkspaceNotice};

/// Notices a workspace keeps; the oldest go first.
const MAX_NOTICES: usize = 256;
/// Grants a workspace keeps by default, spent ones included.
pub(crate) const DEFAULT_MAX_PRE_ADMISSIONS: usize = 1024;
#[cfg(test)]
const MAX_GRANTS: usize = DEFAULT_MAX_PRE_ADMISSIONS;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Granted {
    invite_id: [u8; 32],
    grant: PreAdmission,
    spent: bool,
}

/// One workspace's pre-admissions and notices.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Admissions {
    grants: Vec<Granted>,
    notices: Vec<WorkspaceNotice>,
    #[serde(skip)]
    path: PathBuf,
}

impl Admissions {
    /// Those of the workspace recorded at `room_path` (its `.room` file).
    pub(super) fn open(room_path: &Path) -> Result<Self, String> {
        let path = room_path.with_extension("admissions");
        let mut admissions: Self = match std::fs::read(&path) {
            Ok(bytes) => postcard::from_bytes(&bytes).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error.to_string()),
        };
        admissions.path = path;
        Ok(admissions)
    }

    pub(super) fn save(&self) -> Result<(), String> {
        let bytes = postcard::to_stdvec(self).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())
    }

    /// How long until the door takes another grant, or `None` when it does
    /// now: the table has room under `max_grants`, or a spent or expired grant
    /// to evict ([`Self::record`]). Otherwise the earliest open grant's expiry,
    /// so the person who asked knows when to come back.
    pub(super) fn full_for(&self, now_ns: u64, max_grants: usize) -> Option<u64> {
        let evictable = self.grants.iter().any(|known| known.spent)
            || self
                .grants
                .iter()
                .any(|known| now_ns >= known.grant.expires_at_ns);
        if self.grants.len() < max_grants || evictable {
            return None;
        }
        let soonest = self
            .grants
            .iter()
            .map(|known| known.grant.expires_at_ns)
            .min()
            .unwrap_or(now_ns);
        Some(
            soonest
                .saturating_sub(now_ns)
                .div_ceil(1_000_000_000)
                .max(1),
        )
    }

    /// Record `grant` for the invitation `invite_id` issued with it. The
    /// same grant again is a retry. A full table evicts a spent grant, or
    /// else one expired by `now_ns`, so visitors who never redeemed their
    /// invitation cannot lock a public workspace's joins.
    /// Unwritten: the workspace's next write saves it.
    pub(super) fn record(
        &mut self,
        invite_id: [u8; 32],
        grant: PreAdmission,
        now_ns: u64,
        max_grants: usize,
    ) -> Result<(), String> {
        if let Some(known) = self
            .grants
            .iter()
            .find(|known| known.invite_id == invite_id)
        {
            return if known.grant == grant {
                Ok(())
            } else {
                Err("the invitation carries another pre-admission".into())
            };
        }
        if self.grants.len() >= max_grants {
            let evictable = self
                .grants
                .iter()
                .position(|known| known.spent)
                .or_else(|| {
                    self.grants
                        .iter()
                        .position(|known| now_ns >= known.grant.expires_at_ns)
                });
            let Some(index) = evictable else {
                return Err("too many pre-admissions are open".into());
            };
            self.grants.remove(index);
        }
        self.grants.push(Granted {
            invite_id,
            grant,
            spent: false,
        });
        Ok(())
    }

    /// Whether `invite_id` already carries a grant, spent or not: a door
    /// handing back an invitation a member pre-admitted keeps that grant,
    /// whose own token admits them.
    pub(super) fn carries(&self, invite_id: [u8; 32]) -> bool {
        self.grants.iter().any(|known| known.invite_id == invite_id)
    }

    /// The unspent, unexpired grant of `invite_id` whose token is `token`.
    pub(super) fn grant_for(
        &self,
        invite_id: [u8; 32],
        token: &[u8],
        now_ns: u64,
    ) -> Result<&PreAdmission, String> {
        let granted = self
            .grants
            .iter()
            .find(|known| known.invite_id == invite_id)
            .ok_or("the invitation carries no pre-admission")?;
        let digest: [u8; 32] = Sha256::digest(token).into();
        if granted.spent || now_ns >= granted.grant.expires_at_ns {
            return Err("the pre-admission was used or expired".into());
        }
        if !constant_time_eq(&digest, &granted.grant.token_sha256) {
            return Err("the admission token does not match".into());
        }
        Ok(&granted.grant)
    }

    /// Spend the grant of each invitation and tell the owner with its
    /// notice, in one write.
    pub(super) fn spend(&mut self, spent: Vec<([u8; 32], WorkspaceNotice)>) -> Result<(), String> {
        for (invite_id, notice) in spent {
            for granted in &mut self.grants {
                if granted.invite_id == invite_id {
                    granted.spent = true;
                }
            }
            if self.notices.len() >= MAX_NOTICES {
                self.notices.remove(0);
            }
            self.notices.push(notice);
        }
        self.save()
    }

    /// Tell the owner `notice`, in one write.
    pub(super) fn notice(&mut self, notice: WorkspaceNotice) -> Result<(), String> {
        if self.notices.len() >= MAX_NOTICES {
            self.notices.remove(0);
        }
        self.notices.push(notice);
        self.save()
    }

    /// Change the grant recorded for `invite_id` in place, as an attacker
    /// with the workspace's files would.
    #[cfg(test)]
    pub(super) fn tamper(&mut self, invite_id: [u8; 32], change: impl FnOnce(&mut PreAdmission)) {
        if let Some(granted) = self
            .grants
            .iter_mut()
            .find(|granted| granted.invite_id == invite_id)
        {
            change(&mut granted.grant);
        }
    }

    pub(super) fn notices(&self) -> Vec<WorkspaceNotice> {
        self.notices.clone()
    }
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |differ, (left, right)| differ | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_files::native::migration::assert_golden_bytes;
    use tilo_relay_protocol::workspaces::Role;

    fn grant(token: &[u8], expires_at_ns: u64) -> PreAdmission {
        PreAdmission {
            workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
            role: Role::Editor,
            token_sha256: Sha256::digest(token).into(),
            label: "cy@example.com".into(),
            expires_at_ns,
            owner_user_public_key: [1; 32],
            signature: [0; 64],
        }
    }

    /// The admissions file this build writes: a grant the owner made, spent,
    /// and the notice of who it admitted. Kept so the file a workspace holds
    /// goes on reading the same way; a field added, removed or reordered
    /// changes these bytes.
    const KEPT: &str = "0107070707070707070707070707070707070707070707070707070707070707072436663164336135322d316337652d346238652d396432612d306634613863336535623731023c469e9d6c5875d37a43f353d4f88e61fcf812c66eee3457465a40b0da4153e00e6379406578616d706c652e636f6d8080a8b1e39fe7cb170101010101010101010101010101010101010101010101010101010101010101400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001010002020202020202020202020202020202020202020202020202020202020202020243790e6379406578616d706c652e636f6d0209";

    fn kept() -> Admissions {
        Admissions {
            grants: vec![Granted {
                invite_id: [7; 32],
                grant: grant(b"token", 1_700_000_000_000_000_000),
                spent: true,
            }],
            notices: vec![WorkspaceNotice::PreAdmitted {
                user_public_key: [2; 32],
                name: "Cy".into(),
                label: "cy@example.com".into(),
                role: Role::Editor,
                at_ns: 9,
            }],
            path: PathBuf::new(),
        }
    }

    #[test]
    fn the_admissions_file_kept_here_reads_and_writes_as_it_is() {
        assert_golden_bytes(
            "a workspace's admissions",
            &hex::decode(KEPT).unwrap(),
            |bytes| {
                let stored: Admissions = postcard::from_bytes(bytes).unwrap();
                assert_eq!(stored.grants.len(), 1);
                assert!(stored.grants[0].spent);
                assert_eq!(stored.grants[0].grant.label, "cy@example.com");
                assert!(matches!(
                    &stored.notices[0],
                    WorkspaceNotice::PreAdmitted { name, .. } if name == "Cy"
                ));
            },
            || postcard::to_stdvec(&kept()).unwrap(),
        );
    }

    #[test]
    fn expired_unspent_grants_make_room_for_new_ones() {
        let dir = tempfile::tempdir().unwrap();
        let mut admissions = Admissions::open(&dir.path().join("x.room")).unwrap();
        for index in 0..MAX_GRANTS {
            let id = *blake3::hash(&index.to_le_bytes()).as_bytes();
            admissions
                .record(id, grant(b"old", 100), 50, MAX_GRANTS)
                .unwrap();
        }
        // Every grant is unspent; at time 50 none has expired, so the table is full.
        let full = admissions.record([9; 32], grant(b"new", 1_000), 50, MAX_GRANTS);
        assert!(full.unwrap_err().contains("too many"));
        // At time 200 they all have: one is evicted for the new grant.
        admissions
            .record([9; 32], grant(b"new", 1_000), 200, MAX_GRANTS)
            .unwrap();
        assert_eq!(admissions.grants.len(), MAX_GRANTS);
    }

    #[test]
    fn a_grant_admits_once_with_its_token_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("room.workspace");
        let mut admissions = Admissions::open(&workspace).unwrap();
        admissions
            .record([7; 32], grant(b"token", 100), 1, MAX_GRANTS)
            .unwrap();
        admissions
            .record([7; 32], grant(b"token", 100), 1, MAX_GRANTS)
            .unwrap();
        assert!(
            admissions
                .record([7; 32], grant(b"other", 100), 1, MAX_GRANTS)
                .is_err()
        );
        assert!(admissions.grant_for([8; 32], b"token", 1).is_err());
        assert!(admissions.grant_for([7; 32], b"tokem", 1).is_err());
        assert!(admissions.grant_for([7; 32], b"token", 100).is_err());
        assert_eq!(
            admissions.grant_for([7; 32], b"token", 1).unwrap().label,
            "cy@example.com"
        );

        let notice = WorkspaceNotice::PreAdmitted {
            user_public_key: [2; 32],
            name: "Cy".into(),
            label: "cy@example.com".into(),
            role: Role::Editor,
            at_ns: 1,
        };
        admissions.spend(vec![([7; 32], notice.clone())]).unwrap();
        let restored = Admissions::open(&workspace).unwrap();
        assert!(restored.grant_for([7; 32], b"token", 1).is_err());
        assert_eq!(restored.notices(), vec![notice]);
    }

    #[test]
    fn a_full_table_says_when_it_has_room_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut admissions = Admissions::open(&dir.path().join("x.room")).unwrap();
        assert_eq!(admissions.full_for(1, 1), None, "empty");
        admissions
            .record([7; 32], grant(b"token", 600_000_000_000), 0, 1)
            .unwrap();
        assert_eq!(
            admissions.full_for(0, 1),
            Some(600),
            "the earliest open grant expires in ten minutes"
        );
        assert_eq!(
            admissions.full_for(600_000_000_000, 1),
            None,
            "expired grants are evicted for a new one"
        );
        assert_eq!(admissions.full_for(0, 2), None, "room under the cap");
    }
}
