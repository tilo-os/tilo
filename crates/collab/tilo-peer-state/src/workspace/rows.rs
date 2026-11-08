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
    /// The layout scope, which only there for stored workspaces.
    Layout,
    /// The operation log itself: its growth is [`crate::members::growth`]'s.
    Log,
}

impl GovernanceRow {
    const ALL: [Self; 11] = [
        Self::Owner,
        Self::Membership,
        Self::Identities,
        Self::Settings,
        Self::Media,
        Self::Versions,
        Self::Bots,
        Self::Checkpoints,
        Self::Hosts,
        Self::Layout,
        Self::Log,
    ];

    /// The document keys that hold this part.
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
            Self::Layout => &[tilo_desktop_model::projection::WORKSPACE_LAYOUT_KEY],
            Self::Log => &[crate::members::MEMBERS_KEY],
        }
    }

    /// The governance action that decides this part, when one action does.
    /// Parts whose change is decided by their own rule have none here
    /// ([`crate::workspace::membership`]).
    #[must_use]
    pub fn action(self) -> Option<&'static str> {
        match self {
            Self::Bots => Some(governance::GRANT),
            Self::Media | Self::Checkpoints | Self::Hosts | Self::Layout => {
                Some(governance::SETTINGS)
            }
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
        .map(|row| (row, digest(doc, row.keys())))
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
        })
        .map(|(row, _)| row)
        .collect()
}

/// The digest of what `doc` holds under `keys`: every value, whatever shape a
/// writer gave it, and in one order on every device.
fn digest(doc: Option<&AutoCommit>, keys: &[&str]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    let Some(doc) = doc else {
        return *hasher.finalize().as_bytes();
    };
    let mut encoded = Vec::new();
    for key in keys {
        encoded.extend_from_slice(key.as_bytes());
        encoded.push(0xff);
        if let Ok(Some((value, object))) = doc.get(ROOT, *key) {
            encode(doc, &value, &object, &mut encoded);
        }
    }
    hasher.update(&encoded);
    *hasher.finalize().as_bytes()
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
