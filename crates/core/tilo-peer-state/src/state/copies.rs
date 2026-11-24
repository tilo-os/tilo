//! How many copies of a workspace its holders keep, and which of them is
//! short of what.
//!
//! A workspace says how many key holders (holders with its keys, which do
//! its work) and how many stores (holders of its ciphertext alone) it wants
//! ([`CopyPolicy`]). The policy sits in its governance beside the servers
//! that hold it, so the same owners choose it, and a workspace that never chose one has the
//! default: one of each once a server holds it, and for a group of desktops
//! with no server whatever devices it has, so such a group needs no setup and
//! is never short.
//!
//! Each holder says where its own copy stands in a row of its own in the
//! workspace's state ([`HeldCopy`]): the holders it is still taking what it
//! lacks from, and those it cannot reach and since when. Rows are per device,
//! as lease claims are, so no holder's word replaces another's. Every device
//! reads the same rows and the same policy, and so the same
//! [`CopyStatus`]: each holder, and what it is short of and why
//! ([`Shortfall`]).

use std::collections::{BTreeMap, BTreeSet};

use super::device_rows::device_of;
use super::facades::{PeerGovernance, PeerGovernanceMut, PeerWorkspace, PeerWorkspaceMut};
use super::{ROOT, SystemTime, err};
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc as _, ScalarValue, Value};
use collab_types::signer::DeviceSigner;
use serde::{Deserialize, Serialize};

/// Key of the workspace's copy policy in its governance.
pub const COPY_POLICY_KEY: &str = "copies_v1";
/// Key of the holders' rows in the workspace's state: a map, there from the
/// workspace's founding, from each holder's device key (hex) to its
/// [`HeldCopy`] (JSON).
pub const HELD_COPIES_KEY: &str = "held_copies_v1";

/// How many copies of each kind a workspace wants kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyPolicy {
    /// Holders with the workspace's keys, which also take its work.
    pub key_holders: u32,
    /// Holders of its ciphertext alone.
    pub stores: u32,
}

impl CopyPolicy {
    /// What a workspace that never chose wants: one key holder and one
    /// store once a server holds it, and nothing beyond the devices it has
    /// otherwise.
    #[must_use]
    pub const fn default_for(held_by_servers: bool) -> Self {
        if held_by_servers {
            Self {
                key_holders: 1,
                stores: 1,
            }
        } else {
            Self {
                key_holders: 0,
                stores: 0,
            }
        }
    }

    /// How many copies of `role` it wants.
    #[must_use]
    pub const fn wanted(self, role: CopyRole) -> u32 {
        match role {
            CopyRole::KeyHolder => self.key_holders,
            CopyRole::Store => self.stores,
        }
    }
}

/// How a holder holds a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyRole {
    /// It holds the keys and takes the work.
    KeyHolder,
    /// It keeps the ciphertext and reads none of it.
    Store,
}

/// One holder's word on its own copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldCopy {
    /// The holders it is still taking what it lacks from.
    pub catching_up_from: BTreeSet<[u8; 32]>,
    /// The holders it cannot reach, each since when (ns since the epoch, on
    /// this holder's clock).
    pub unreachable: BTreeMap<[u8; 32], u64>,
    /// How many blobs of the content the workspace's files name
    /// it does not keep yet.
    pub content_missing: u32,
}

/// A [`HeldCopy`] as the workspace keeps it: devices by their hex keys.
#[derive(Serialize, Deserialize)]
struct StoredCopy {
    catching_up_from: Vec<String>,
    unreachable: BTreeMap<String, u64>,
    content_missing: u32,
}

impl From<&HeldCopy> for StoredCopy {
    fn from(held: &HeldCopy) -> Self {
        Self {
            catching_up_from: held.catching_up_from.iter().map(hex::encode).collect(),
            unreachable: held
                .unreachable
                .iter()
                .map(|(device, since)| (hex::encode(device), *since))
                .collect(),
            content_missing: held.content_missing,
        }
    }
}

impl StoredCopy {
    /// The copy this row says, leaving out a device this build cannot name.
    fn held(self) -> HeldCopy {
        HeldCopy {
            catching_up_from: self
                .catching_up_from
                .iter()
                .filter_map(|device| device_of(device))
                .collect(),
            unreachable: self
                .unreachable
                .iter()
                .filter_map(|(device, since)| Some((device_of(device)?, *since)))
                .collect(),
            content_missing: self.content_missing,
        }
    }
}

/// A device key as its hex, in what every device reads.
fn hex_key<S: serde::Serializer>(key: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&hex::encode(key))
}

/// A device key, when there is one, as its hex.
#[expect(
    clippy::ref_option,
    reason = "serde's `serialize_with` hands the field over by reference"
)]
fn hex_key_or_none<S: serde::Serializer>(
    key: &Option<[u8; 32]>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match key {
        Some(key) => serializer.serialize_some(&hex::encode(key)),
        None => serializer.serialize_none(),
    }
}

/// What a holder lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Missing {
    /// History another holder has and it has not taken in yet.
    History {
        /// The holder it takes it from.
        #[serde(serialize_with = "hex_key")]
        from: [u8; 32],
    },
    /// Whatever was written since it was last reached.
    WritesSince {
        /// When another holder last reached it, ns since the epoch.
        since_ns: u64,
        /// The holder that cannot reach it.
        #[serde(serialize_with = "hex_key")]
        seen_by: [u8; 32],
    },
    /// Content the workspace's files name that it does not keep yet: the
    /// blobs of files, which it asks the other holders for.
    Content {
        /// How many blobs it lacks.
        blobs: u32,
    },
}

/// Something a workspace's copies fall short of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Shortfall {
    /// A holder lacks something.
    Holder {
        /// The holder.
        #[serde(serialize_with = "hex_key")]
        device: [u8; 32],
        /// What it lacks, and from or since what.
        missing: Missing,
    },
    /// Fewer holders of `role` are current than the policy wants.
    Copies {
        /// The kind of copy.
        role: CopyRole,
        /// How many the policy wants.
        wanted: u32,
        /// How many holders of it lack nothing.
        current: u32,
    },
}

/// One holder of a workspace, as every device reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HolderCopy {
    /// The holder's device.
    #[serde(serialize_with = "hex_key")]
    pub device: [u8; 32],
    /// How it holds the workspace.
    pub role: CopyRole,
    /// Whether it lacks nothing that any holder knows of.
    pub current: bool,
}

/// Where a workspace's copies stand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CopyStatus {
    /// What the workspace wants.
    pub policy: CopyPolicy,
    /// Whether its owners chose the policy, rather than it being the default.
    pub chosen: bool,
    /// Each holder, by device.
    pub holders: Vec<HolderCopy>,
    /// What falls short, holders first, then the counts.
    pub shortfall: Vec<Shortfall>,
    /// The holder that makes the workspace's compacted base
    /// ([`PeerGovernance::base_maker`]), when its governance names one.
    #[serde(serialize_with = "hex_key_or_none")]
    pub base_maker: Option<[u8; 32]>,
}

impl CopyStatus {
    /// Whether nothing falls short.
    #[must_use]
    pub fn satisfied(&self) -> bool {
        self.shortfall.is_empty()
    }
}

/// The policy `doc`'s governance chose, if it chose one.
fn chosen_policy(doc: &automerge::AutoCommit) -> Option<CopyPolicy> {
    let (Value::Scalar(value), _) = doc.get(ROOT, COPY_POLICY_KEY).ok()?? else {
        return None;
    };
    let ScalarValue::Str(json) = value.as_ref() else {
        return None;
    };
    tilo_failure::ignored!(
        "a policy another build wrote is not one this one reads: the default holds",
        serde_json::from_str(json).ok()
    )
}

/// Every holder's row in `doc`, by device.
fn held_rows(doc: &automerge::AutoCommit) -> BTreeMap<[u8; 32], HeldCopy> {
    let Ok(Some((Value::Object(ObjType::Map), rows))) = doc.get(ROOT, HELD_COPIES_KEY) else {
        return BTreeMap::new();
    };
    doc.keys(&rows)
        .filter_map(|device| {
            let key = device_of(&device)?;
            let (Value::Scalar(value), _) = doc.get(&rows, device.as_str()).ok()?? else {
                return None;
            };
            let ScalarValue::Str(json) = value.as_ref() else {
                return None;
            };
            let stored = tilo_failure::ignored!(
                "a row another build wrote is not one this one reads",
                serde_json::from_str::<StoredCopy>(json).ok()
            )?;
            Some((key, stored.held()))
        })
        .collect()
}

impl PeerGovernance<'_> {
    /// The copy policy the workspace's owners chose, if they chose one.
    #[must_use]
    pub fn chosen_copy_policy(self) -> Option<CopyPolicy> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .and_then(chosen_policy)
    }
}

impl PeerGovernanceMut<'_> {
    /// Have the workspace want `policy`, or the default again for `None`,
    /// authored by `key`. Returns whether it changed the workspace.
    ///
    /// # Errors
    ///
    /// The change cannot be written or its author may not choose it.
    pub fn author_copy_policy(
        self,
        policy: Option<CopyPolicy>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        if self.peer.governance().chosen_copy_policy() == policy {
            return Ok(false);
        }
        let json = policy
            .map(|policy| serde_json::to_string(&policy))
            .transpose()
            .map_err(|error| error.to_string())?;
        self.write(key, now, |doc| match json {
            Some(json) => doc.put(ROOT, COPY_POLICY_KEY, json.as_str()).map_err(err),
            None => doc.delete(ROOT, COPY_POLICY_KEY).map_err(err),
        })
    }
}

impl PeerWorkspace<'_> {
    /// Where the workspace's copies stand, the same on every device that
    /// holds the same state: its servers and every holder that said where its
    /// copy stands, what each lacks, and how far the copies fall short of
    /// what the workspace wants.
    #[must_use]
    pub fn copies(self) -> CopyStatus {
        let governance = self.peer.governance();
        let servers = governance.keeping_devices();
        let chosen = governance.chosen_copy_policy();
        let policy = chosen.unwrap_or_else(|| CopyPolicy::default_for(!servers.is_empty()));
        let rows = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .map(held_rows)
            .unwrap_or_default();

        // Each holder holds the workspace as its signed entry says; a holder
        // that says where its copy stands writes into the workspace's state,
        // which only a key holder does. A row naming another device names a
        // holder beside it, but only one the workspace still has: a device it
        // names, or one that says where its own copy stands. A row can only
        // name a device the workspace dropped while it named it, or one that
        // stopped keeping it since, so such a device is no holder of the
        // report, and nothing falls short about it.
        let mut roles: BTreeMap<[u8; 32], CopyRole> = BTreeMap::new();
        for hint in &servers {
            roles.insert(hint.host.device_public_key, super::holder_role(hint));
        }
        for device in rows.keys() {
            roles.entry(*device).or_insert(CopyRole::KeyHolder);
        }

        let mut shortfall = Vec::new();
        for (device, row) in &rows {
            if row.content_missing > 0 {
                shortfall.push(Shortfall::Holder {
                    device: *device,
                    missing: Missing::Content {
                        blobs: row.content_missing,
                    },
                });
            }
            for from in &row.catching_up_from {
                shortfall.push(Shortfall::Holder {
                    device: *device,
                    missing: Missing::History { from: *from },
                });
            }
        }
        for (seen_by, row) in &rows {
            for (device, since_ns) in &row.unreachable {
                if !roles.contains_key(device) {
                    continue;
                }
                shortfall.push(Shortfall::Holder {
                    device: *device,
                    missing: Missing::WritesSince {
                        since_ns: *since_ns,
                        seen_by: *seen_by,
                    },
                });
            }
        }
        let short: BTreeSet<[u8; 32]> = shortfall
            .iter()
            .filter_map(|short| match short {
                Shortfall::Holder { device, .. } => Some(*device),
                Shortfall::Copies { .. } => None,
            })
            .collect();
        let holders: Vec<HolderCopy> = roles
            .into_iter()
            .map(|(device, role)| HolderCopy {
                device,
                role,
                current: !short.contains(&device),
            })
            .collect();
        for role in [CopyRole::KeyHolder, CopyRole::Store] {
            let wanted = policy.wanted(role);
            let current = holders
                .iter()
                .filter(|holder| holder.role == role && holder.current)
                .count();
            let current = u32::try_from(current).unwrap_or(u32::MAX);
            if current < wanted {
                shortfall.push(Shortfall::Copies {
                    role,
                    wanted,
                    current,
                });
            }
        }
        CopyStatus {
            policy,
            chosen: chosen.is_some(),
            holders,
            shortfall,
            base_maker: governance.base_maker(),
        }
    }
}

impl PeerWorkspaceMut<'_> {
    /// Say where `device`'s own copy stands: `held`. Writes nothing when the
    /// workspace says so already. Returns whether it changed the workspace.
    ///
    /// # Errors
    ///
    /// `device` is not the exact current certificate of a device that may
    /// change this workspace, or the row cannot be written. A holder that is
    /// no member writes nothing, this row included.
    pub fn report_copy(
        mut self,
        device: &tilo_user_identity::DeviceCert,
        held: &HeldCopy,
        now: SystemTime,
    ) -> Result<bool, String> {
        let workspace = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("the workspace is absent")?;
        if held_rows(workspace).get(&device.device_public_key) == Some(held) {
            return Ok(false);
        }
        self.check_writer(device, now)?;
        let json =
            serde_json::to_string(&StoredCopy::from(held)).map_err(|error| error.to_string())?;

        self.write_row(
            HELD_COPIES_KEY,
            &hex::encode(device.device_public_key),
            &device.device_public_key,
            Some(&json),
            now,
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::testing::now;
    use crate::state::test_desktops::{cert_of, desktops, sync};

    /// The owners take a holder out: its row says where its copy stood no
    /// more, and a row naming it no longer makes it one of the report's
    /// holders, nor makes anything fall short about it.
    #[test]
    fn a_holder_the_workspace_no_longer_names_is_no_holder() {
        let (mut left, left_key, mut right, right_key) = desktops();
        let (left_cert, right_cert) = (cert_of(&left), cert_of(&right));
        let right_device = right_cert.device_public_key;
        for (cert, key, at) in [
            (&left_cert, &left_key, "left addr"),
            (&right_cert, &right_key, "right addr"),
        ] {
            let hint =
                tilo_workspace_join::sign_transport_hint("iroh", at, cert.clone(), [1; 32], key)
                    .unwrap();
            left.governance_mut()
                .author_keeping_device(&hint, &left_key, now())
                .unwrap();
        }
        let away = HeldCopy {
            unreachable: BTreeMap::from([(right_device, 7)]),
            ..HeldCopy::default()
        };
        left.workspace_mut()
            .report_copy(&left_cert, &away, now())
            .unwrap();
        sync(&left, &mut right);
        right
            .workspace_mut()
            .report_copy(&right_cert, &HeldCopy::default(), now())
            .unwrap();
        sync(&right, &mut left);
        let status = left.workspace().copies();
        assert_eq!(status.holders.len(), 2, "{status:?}");

        assert!(
            left.governance_mut()
                .author_keeping_device_removed(&right_device, &left_key, now())
                .unwrap()
        );
        sync(&left, &mut right);
        let status = right.workspace().copies();
        assert!(
            status
                .holders
                .iter()
                .all(|holder| holder.device != right_device),
            "a device the workspace no longer names is no holder: {status:?}"
        );
        assert!(
            !status.shortfall.iter().any(|short| matches!(
                short,
                Shortfall::Holder { device, .. } if *device == right_device
            )),
            "and nothing falls short about it: {status:?}"
        );
    }

    /// A group of desktops with no server wants nothing beyond the devices
    /// it has: it is never short, however its desktops come and go.
    #[test]
    fn a_group_of_desktops_is_never_short_by_default() {
        let (left, _, _, _) = desktops();
        let status = left.workspace().copies();
        assert!(!status.chosen);
        assert_eq!(status.policy, CopyPolicy::default_for(false));
        assert!(status.satisfied(), "{status:?}");
    }

    /// The owners choose how many copies the workspace wants; a holder that
    /// cannot reach another, or still takes in what another has, makes that
    /// one short, and every device reads the same shortfall, cleared once
    /// the holders say they are current again.
    #[test]
    fn a_shortfall_is_reported_alike_and_cleared_once_holders_are_current() {
        let (mut left, left_key, mut right, _) = desktops();
        let (left_cert, right_cert) = (cert_of(&left), cert_of(&right));
        let (left_device, right_device) =
            (left_cert.device_public_key, right_cert.device_public_key);
        let wanted = CopyPolicy {
            key_holders: 2,
            stores: 0,
        };
        assert!(
            left.governance_mut()
                .author_copy_policy(Some(wanted), &left_key, now())
                .unwrap()
        );
        let went_away = 7;
        let away = HeldCopy {
            unreachable: BTreeMap::from([(right_device, went_away)]),
            ..HeldCopy::default()
        };
        assert!(
            left.workspace_mut()
                .report_copy(&left_cert, &away, now())
                .unwrap()
        );
        assert!(
            !left
                .workspace_mut()
                .report_copy(&left_cert, &away, now())
                .unwrap(),
            "saying the same again changes nothing"
        );
        sync(&left, &mut right);
        let catching_up = HeldCopy {
            catching_up_from: BTreeSet::from([left_device]),
            ..HeldCopy::default()
        };
        right
            .workspace_mut()
            .report_copy(&right_cert, &catching_up, now())
            .unwrap();
        sync(&right, &mut left);

        let status = left.workspace().copies();
        assert_eq!(status, right.workspace().copies(), "both read the same");
        assert!(status.chosen);
        assert_eq!(status.policy, wanted);
        assert_eq!(
            status.shortfall,
            [
                Shortfall::Holder {
                    device: right_device,
                    missing: Missing::History { from: left_device },
                },
                Shortfall::Holder {
                    device: right_device,
                    missing: Missing::WritesSince {
                        since_ns: went_away,
                        seen_by: left_device,
                    },
                },
                Shortfall::Copies {
                    role: CopyRole::KeyHolder,
                    wanted: 2,
                    current: 1,
                },
            ]
        );

        for (peer, cert) in [(&mut left, &left_cert), (&mut right, &right_cert)] {
            peer.workspace_mut()
                .report_copy(cert, &HeldCopy::default(), now())
                .unwrap();
        }
        sync(&left, &mut right);
        sync(&right, &mut left);
        let status = left.workspace().copies();
        assert!(status.satisfied(), "{status:?}");
        assert_eq!(status.holders.len(), 2);
        assert!(status.holders.iter().all(|holder| holder.current));
    }
}
