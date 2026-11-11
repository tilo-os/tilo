//! A workspace's governance as the parts one device changes: each part is a
//! digest over the document keys holding it, so a writer names what its change
//! touches and every receiver diffing the same batch reaches the same list.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjId, ObjType, Prop, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use tilo_action_policy::roles::governance;

/// The document key holding the media policy.
const MEDIA_KEY: &str = "media_policy_v1";
/// The document key holding the hosts serving a workspace.
const HOSTS_KEY: &str = "hosts_v1";
/// The document key holding the app versions a workspace runs.
const VERSIONS_KEY: &str = "app_versions_v1";
/// The document keys holding the bots a workspace runs and those it was asked for.
const BOT_KEYS: [&str; 2] = ["bot_programs_v1", "bot_requests_v1"];
/// The document keys holding what a workspace vouches for the record history it
/// published and the checkpoint it rests on.
const CHECKPOINT_KEYS: [&str; 4] = [
    "document_imports_v1",
    "document_checkpoint_coverage_v2",
    "document_durability_receipts_v2",
    "native_checkpoint_root_v1",
];

/// One part of a workspace's governance, as one device's operation changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GovernanceRow {
    /// The structural owner: never a change anyone writes.
    Owner,
    /// Members, roles, grants, admission times, sandbox capabilities and
    /// invitation receipts: what the membership log projects.
    Membership,
    /// Device rows and the display names beside them.
    Identities,
    /// The workspace's name and its saved grant sets.
    Settings,
    /// The media policy.
    Media,
    /// The app versions the workspace runs.
    Versions,
    /// The bots it runs and the ones a member asked its host for.
    Bots,
    /// Checkpoint coverage, durability receipts and import floors.
    Checkpoints,
    /// The hosts serving the workspace.
    Hosts,
    /// The operation log itself: its growth is [`crate::members::growth`]'s.
    Log,
}

impl GovernanceRow {
    const ALL: [Self; 10] = [
        Self::Owner,
        Self::Membership,
        Self::Identities,
        Self::Settings,
        Self::Media,
        Self::Versions,
        Self::Bots,
        Self::Checkpoints,
        Self::Hosts,
        Self::Log,
    ];

    /// The digest of this part of `doc`: every value it holds, whatever shape
    /// a writer gave it, and in one order on every device.
    fn digest(self, doc: Option<&AutoCommit>) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        let Some(doc) = doc else {
            return *hasher.finalize().as_bytes();
        };
        let mut encoded = Vec::new();
        if self == Self::Log {
            for (key, bytes) in crate::members::replay::raw(doc) {
                encoded.extend_from_slice(key.as_bytes());
                encoded.push(0);
                encoded.extend_from_slice(&bytes);
                encoded.push(0xff);
            }
        } else {
            for key in self.keys() {
                encoded.extend_from_slice(key.as_bytes());
                encoded.push(0xff);
                if let Ok(Some((value, object))) = doc.get(ROOT, *key) {
                    encode(doc, &value, &object, &mut encoded);
                }
            }
        }
        hasher.update(&encoded);
        *hasher.finalize().as_bytes()
    }

    /// The document keys that hold this part, other than the operation log,
    /// which [`GovernanceRow::digest`] reads as the operations it holds.
    fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Owner => &["owner"],
            Self::Membership => &[
                "members",
                "members_joined_v1",
                "grants",
                "sandbox",
                "invitation_receipts_v1",
            ],
            Self::Identities => &["peers", "peer_identities_v1"],
            Self::Settings => &["name", "presets_v1"],
            Self::Media => &[MEDIA_KEY],
            Self::Versions => &[VERSIONS_KEY],
            Self::Bots => &BOT_KEYS,
            Self::Checkpoints => &CHECKPOINT_KEYS,
            Self::Hosts => &[HOSTS_KEY],
            Self::Log => &[],
        }
    }

    /// The governance action that decides this part, when one action does.
    /// Parts whose change is decided by their own rule have none here
    /// ([`crate::workspace::membership`]).
    #[must_use]
    pub fn action(self) -> Option<&'static str> {
        match self {
            Self::Bots => Some(governance::GRANT),
            Self::Media | Self::Checkpoints | Self::Hosts => Some(governance::SETTINGS),
            Self::Versions => Some(governance::INSTALL_APP),
            Self::Owner | Self::Membership | Self::Identities | Self::Settings | Self::Log => None,
        }
    }
}

/// Every part of `doc`'s governance, each as a digest of what it holds.
#[must_use]
pub fn governance_rows(doc: Option<&AutoCommit>) -> BTreeMap<GovernanceRow, [u8; 32]> {
    GovernanceRow::ALL
        .into_iter()
        .map(|row| (row, row.digest(doc)))
        .collect()
}

/// The parts of `before`'s governance `after` changes, other than the operation
/// log and — unless `membership` — the membership rows that log projects.
#[must_use]
pub fn changed_rows(
    before: Option<&AutoCommit>,
    after: Option<&AutoCommit>,
    membership: bool,
) -> Vec<GovernanceRow> {
    let held = governance_rows(before);
    let written = governance_rows(after);
    held.into_iter()
        .filter(|(row, digest)| {
            *row != GovernanceRow::Log
                && (membership || *row != GovernanceRow::Membership)
                && written.get(row) != Some(digest)
                && !(*row == GovernanceRow::Media && refreshed_media(before, after))
        })
        .map(|(row, _)| row)
        .collect()
}

/// [`changed_rows`] of `after`, which is `before` with `changes` applied on
/// top of it.
///
/// A value of the document changes only where an operation of `changes`
/// wrote, so only the parts holding a key those operations name are compared,
/// and of a part's map only the entries they name. A change to a workspace of
/// hundreds then costs what it wrote rather than an encoding of every
/// member's row, twice. Where the operations do not say where they wrote
/// (an object this reading cannot place), every part is compared as
/// [`changed_rows`] compares it.
#[must_use]
pub fn changed_rows_by(
    before: &AutoCommit,
    after: &AutoCommit,
    changes: &[automerge::Change],
    membership: bool,
) -> Vec<GovernanceRow> {
    let Some(touched) = touched(after, changes) else {
        return changed_rows(Some(before), Some(after), membership);
    };
    GovernanceRow::ALL
        .into_iter()
        .filter(|row| {
            *row != GovernanceRow::Log
                && (membership || *row != GovernanceRow::Membership)
                && row.keys().iter().any(|key| {
                    touched
                        .get(*key)
                        .is_some_and(|wrote| differs(before, after, key, wrote))
                })
                && !(*row == GovernanceRow::Media && refreshed_media(Some(before), Some(after)))
        })
        .collect()
}

/// Where operations wrote under one document key.
#[derive(Debug)]
pub(crate) enum Wrote {
    /// The key's value as a whole: the key itself, or something nested
    /// deeper than its map's entries.
    Whole,
    /// These entries of the map the key holds.
    Entries(std::collections::BTreeSet<String>),
}

/// The document key an object lies under, and whether directly (its entries
/// are that key's map's) or deeper.
type Placed = (String, bool);

/// The document keys `changes` wrote under, each with where, read against
/// `after`, the document holding them. `None` when an operation names an
/// object that cannot be placed under a key.
pub(crate) fn touched(
    after: &AutoCommit,
    changes: &[automerge::Change],
) -> Option<BTreeMap<String, Wrote>> {
    use automerge::legacy::{Key, ObjectId};
    // Where each object an operation names lies: the document key it is held
    // under, and whether directly (its entries are that key's map's) or
    // deeper. Placed through its own parents, never by reading every key of
    // the document, whose operation log grows with every admission.
    let mut placed: BTreeMap<(u64, Vec<u8>), Option<Placed>> = BTreeMap::new();
    let mut place = |id: &automerge::legacy::OpId| -> Option<Placed> {
        placed
            .entry((id.0, id.1.to_bytes().to_vec()))
            .or_insert_with(|| {
                let (object, _) = after.import(&format!("{}@{}", id.0, id.1)).ok()?;
                let parents: Vec<_> = after.parents(&object).ok()?.collect();
                let top = parents.last()?;
                let Prop::Map(key) = &top.prop else {
                    return None;
                };
                Some((key.clone(), parents.len() == 1))
            })
            .clone()
    };
    let mut touched: BTreeMap<String, Wrote> = BTreeMap::new();
    let whole = |touched: &mut BTreeMap<String, Wrote>, key: String| {
        touched.insert(key, Wrote::Whole);
    };
    for change in changes {
        for op in change.decode().operations {
            match (&op.obj, &op.key) {
                (ObjectId::Root, Key::Map(key)) => whole(&mut touched, key.to_string()),
                (ObjectId::Root, Key::Seq(_)) => return None,
                (ObjectId::Id(id), key) => {
                    let (root, direct) = place(id)?;
                    match (direct, key, touched.get_mut(&root)) {
                        (_, _, Some(Wrote::Whole)) => {}
                        (true, Key::Map(entry), Some(Wrote::Entries(entries))) => {
                            entries.insert(entry.to_string());
                        }
                        (true, Key::Map(entry), None) => {
                            touched.insert(root, Wrote::Entries([entry.to_string()].into()));
                        }
                        // A list's element, or something nested deeper: the
                        // key it lies under, rewritten whole.
                        _ => whole(&mut touched, root),
                    }
                }
            }
        }
    }
    Some(touched)
}

/// Whether what `wrote` names under `key` reads differently in `before` and
/// `after`, encoded as [`GovernanceRow::digest`] encodes it.
fn differs(before: &AutoCommit, after: &AutoCommit, key: &str, wrote: &Wrote) -> bool {
    let at = |doc: &AutoCommit, object: &ObjId, prop: Prop| {
        let mut encoded = Vec::new();
        if let Ok(Some((value, child))) = doc.get(object, prop) {
            encoded.push(1);
            encode(doc, &value, &child, &mut encoded);
        }
        encoded
    };
    let map = |doc: &AutoCommit| match doc.get(ROOT, key) {
        Ok(Some((Value::Object(ObjType::Map | ObjType::Table), object))) => Some(object),
        _ => None,
    };
    match (wrote, map(before), map(after)) {
        (Wrote::Entries(entries), Some(held), Some(written)) if held == written => {
            entries.iter().any(|entry| {
                at(before, &held, Prop::Map(entry.clone()))
                    != at(after, &written, Prop::Map(entry.clone()))
            })
        }
        _ => {
            at(before, &ROOT, Prop::Map(key.to_owned()))
                != at(after, &ROOT, Prop::Map(key.to_owned()))
        }
    }
}

/// Whether `after`'s media policy is `before`'s with only the authorization
/// basis and revision renewed: what [`super::media::refresh_authorization`]
/// writes beside the change to the rows that basis is made of, and never a
/// policy anyone chose. It is the change's own doing, so it is no part of what
/// the change has to be permitted for.
fn refreshed_media(before: Option<&AutoCommit>, after: Option<&AutoCommit>) -> bool {
    let (Some(before), Some(after)) = (before, after) else {
        return false;
    };
    let (Ok(Some(held)), Ok(Some(written))) =
        (super::media::parse(before), super::media::parse(after))
    else {
        return false;
    };
    let renewed = super::media::WorkspaceMediaV1 {
        authorization_basis: written.authorization_basis,
        authorization_revision: written.authorization_revision,
        ..held.clone()
    };
    renewed == written
        && held.authorization_revision.checked_add(1) == Some(written.authorization_revision)
        && super::media::security_basis(after, &[])
            .is_ok_and(|basis| basis == written.authorization_basis)
}

/// `value` at `object`, as bytes that are the same on every device holding it.
fn encode(doc: &AutoCommit, value: &Value<'_>, object: &ObjId, out: &mut Vec<u8>) {
    match value {
        Value::Object(ObjType::Map | ObjType::Table) => {
            out.push(1);
            let mut keys: Vec<String> = doc.keys(object).collect();
            keys.sort();
            for key in keys {
                out.extend_from_slice(key.as_bytes());
                out.push(0);
                if let Ok(Some((value, child))) = doc.get(object, key.as_str()) {
                    encode(doc, &value, &child, out);
                }
            }
        }
        Value::Object(ObjType::List) => {
            out.push(2);
            for index in 0..doc.length(object) {
                out.push(0);
                if let Ok(Some((value, child))) = doc.get(object, Prop::Seq(index)) {
                    encode(doc, &value, &child, out);
                }
            }
        }
        Value::Object(ObjType::Text) => {
            out.push(3);
            if let Ok(text) = doc.text(object) {
                out.extend_from_slice(text.as_bytes());
            }
        }
        Value::Scalar(scalar) => {
            out.push(4);
            match scalar.as_ref() {
                ScalarValue::Bytes(bytes) => {
                    out.push(0);
                    out.extend_from_slice(bytes);
                }
                ScalarValue::Str(text) => {
                    out.push(1);
                    out.extend_from_slice(text.as_bytes());
                }
                ScalarValue::Boolean(flag) => {
                    out.push(2);
                    out.push(u8::from(*flag));
                }
                other => {
                    out.push(3);
                    out.extend_from_slice(format!("{other:?}").as_bytes());
                }
            }
        }
    }
}
