//! The membership a workspace kept before this layout, brought up into it.
//!
//! The 0.1 releases kept a workspace's membership operations under
//! [`EARLIER_KEY`] and projected the membership they gave into rows of the
//! document: each member's role (`members`), when each joined
//! (`members_joined_v1`) and the workspace's grants (`grants`), each a JSON
//! value by person or grant id. A person who came in through an invitation was
//! in those rows and in no operation: those releases read an admission from
//! its redemption receipt. This layout reads a workspace's membership from its
//! operations under [`super::MEMBERS_KEY`] alone, and neither the earlier
//! operations nor the rows, so read as it is, a workspace those releases kept
//! holds its owner and nobody else.
//!
//! The grants those releases gave apps (`session-app`, `files-app`,
//! `device-panel`, `app-author:<app>`) have no grantee here: an app acts as
//! the person whose session runs it. Each is named with why ([`LeftOut`]),
//! never dropped unsaid.
//!
//! The device that governs the workspace brings it up once
//! ([`crate::StatePeer::carry_earlier_membership`]): what the rows held is
//! written as operations of its own, the workspace owner's device's, one adding
//! each member at their role at the time they joined, oldest first, then one
//! with the grants the workspace held that the roles do not already give. The
//! rows are the governing device's own projection of the earlier operations,
//! kept in its own durable copy, which is what the workspace was. A replay of
//! the new log must give the same people at the same roles, or the bring-up is
//! refused and the workspace is left as it was.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::{Change, Grant, Role, WorkspacePolicy};
use tilo_user_identity::DeviceCert;

use super::op::{MembershipOpCommand, SignedMembershipOp};

/// The prefix of the document keys the earlier layout kept its operations
/// under.
pub const EARLIER_KEY: &str = "members_v2";
/// The rows the earlier layout projected the membership into.
const MEMBERS_ROWS: &str = "members";
const JOINED_ROWS: &str = "members_joined_v1";
const GRANTS_ROWS: &str = "grants";

/// Whether `key` is one of the earlier layout's operations.
fn earlier_op(key: &str) -> bool {
    key.strip_prefix(EARLIER_KEY)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `doc` holds an earlier membership log and none of this layout:
/// what [`membership`] brings up.
#[must_use]
pub fn holds_earlier_log(doc: &AutoCommit) -> bool {
    let mut earlier = false;
    for key in doc.keys(ROOT) {
        if super::op::keyed(&key).is_some() {
            return false;
        }
        earlier |= earlier_op(&key);
    }
    earlier
}

/// The document keys the earlier layout's membership is kept under, which
/// nothing reads once it is brought up: its operations and its rows.
#[must_use]
pub fn earlier_keys(doc: &AutoCommit) -> Vec<String> {
    doc.keys(ROOT)
        .filter(|key| {
            earlier_op(key) || [MEMBERS_ROWS, JOINED_ROWS, GRANTS_ROWS].contains(&key.as_str())
        })
        .collect()
}

/// A grant as the earlier layout wrote it, as this layout writes it: those
/// releases named whom a grant is for `subject`, which this layout names
/// `grantee`, and named a person, agent or bot a grant is for (or given by)
/// on its own (`{"person": ..}`), where this layout names each a member
/// (`{"member": {"person": ..}}`). A grant those releases gave an app, an
/// issue's work or a Shortcut has no grantee in this layout, which grants
/// only members and roles: [`left_out`] says what became of it.
fn earlier_grant(mut grant: serde_json::Value) -> serde_json::Value {
    if let Some(named) = grant.as_object_mut()
        && !named.contains_key("grantee")
        && let Some(subject) = named.remove("subject")
    {
        named.insert("grantee".to_owned(), subject);
    }
    for field in ["grantee", "granted_by"] {
        let Some(grantee) = grant.get_mut(field) else {
            continue;
        };
        let names_a_member = grantee.as_object().is_some_and(|named| {
            named.len() == 1
                && named
                    .keys()
                    .all(|kind| ["person", "agent", "bot"].contains(&kind.as_str()))
        });
        if names_a_member {
            *grantee = serde_json::json!({ "member": grantee.take() });
        }
    }
    grant
}

/// The JSON values of the row map at `key`, by name, or why each does not
/// read as JSON.
fn row_values(doc: &AutoCommit, key: &str) -> BTreeMap<String, Result<serde_json::Value, String>> {
    let mut read = BTreeMap::new();
    let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(ROOT, key) else {
        return read;
    };
    for name in doc.keys(&map) {
        let value = match doc.get(&map, name.as_str()) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => {
                    serde_json::from_slice(bytes).map_err(|error| error.to_string())
                }
                other => Err(format!("a {other:?}, not JSON bytes")),
            },
            other => Err(format!("{other:?}, not a value")),
        };
        read.insert(name, value);
    }
    read
}

/// The JSON values of the row map at `key`, each read as `T`, by name. A row
/// that does not read is left out, as the earlier layout's own reader left it
/// out, and named in the log.
fn rows<T: serde::de::DeserializeOwned>(doc: &AutoCommit, key: &str) -> BTreeMap<String, T> {
    let mut read = BTreeMap::new();
    for (name, value) in row_values(doc, key) {
        match value.and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string())) {
            Ok(value) => {
                read.insert(name, value);
            }
            Err(error) => tracing::warn!(
                row = key,
                %name,
                %error,
                "a membership row an earlier release kept does not read and is left out"
            ),
        }
    }
    read
}

/// A grant an earlier release kept that this layout holds no grant for, and
/// why ([`Why`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftOut {
    /// The grant's id as that release kept it.
    pub grant: String,
    pub why: Why,
}

/// Why a grant an earlier release kept is held as no grant in this layout.
///
/// The 0.1 releases made each app a party of its own, which could do only
/// what the workspace granted it: every workspace granted the desktop
/// session's own app, the Files app and the device's panel what they needed,
/// and an app a person who could change the workspace opened was granted
/// authorship of its own records. In this layout an app is no party: it acts
/// as the person whose session runs it, and reaches only the resource kinds
/// it declares in its own node (`tilo_action_policy::Acting::in_app`). What
/// those grants let an app do, the person running it now does through it, as
/// their own standing decides, so no grant carries them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// `session-app`: the desktop session's own app (`shell`) publishing
    /// the session's functions, granted whatever the person may do.
    SessionApp,
    /// `files-app`: the Files app authoring the workspace's files.
    FilesApp,
    /// `device-panel`: quick settings reading what the session publishes
    /// for it.
    DevicePanel,
    /// `app-author:<app>`: `app` authoring its own records.
    AppAuthor { app: String },
    /// Any other grant to an app: what it gave beyond the person running the
    /// app is not kept.
    OtherApp { app: String },
    /// A grant to what this layout grants nothing: an issue's work, a
    /// Shortcut, a device not the person's.
    NoGrantee { subject: String },
    /// The row does not read as a grant of either layout.
    Unreadable { error: String },
}

impl Why {
    /// Whether what the grant allowed is still allowed in this layout,
    /// without a grant.
    #[must_use]
    pub const fn carried(&self) -> bool {
        matches!(
            self,
            Self::SessionApp | Self::FilesApp | Self::DevicePanel | Self::AppAuthor { .. }
        )
    }
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionApp => write!(
                f,
                "the desktop session's own app ({}) acts as the person whose session runs it, \
                 with what they may do in the workspace",
                tilo_action_policy::roles::SESSION_APP
            ),
            Self::FilesApp => write!(
                f,
                "Files ({}) acts as the person who runs it: whoever may change the workspace's \
                 content changes its files, as their role decides",
                tilo_action_policy::roles::FILES_APP
            ),
            Self::DevicePanel => write!(
                f,
                "quick settings ({}) reads the session's readings as the person whose session \
                 publishes them, which are theirs to read",
                tilo_action_policy::roles::DEVICE_PANEL
            ),
            Self::AppAuthor { app } => write!(
                f,
                "{app} acts as the person who runs it, in its own records, as their role decides"
            ),
            Self::OtherApp { app } => write!(
                f,
                "a grant to the app {app}: apps hold no grants and act as the person who runs \
                 them, so what it gave beyond that person's standing is not kept"
            ),
            Self::NoGrantee { subject } => write!(
                f,
                "a grant to {subject}: grants name only members and roles, so it is not kept"
            ),
            Self::Unreadable { error } => write!(f, "it does not read as a grant: {error}"),
        }
    }
}

/// Why the grant row `value`, kept as `name`, has no grant in this layout,
/// when it has none.
fn why_left_out(name: &str, value: &serde_json::Value, error: String) -> Why {
    let subject = value.get("subject").and_then(serde_json::Value::as_object);
    let Some((kind, named)) = subject.and_then(|subject| subject.iter().next()) else {
        return Why::Unreadable { error };
    };
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(name);
    match (kind.as_str(), named.as_str()) {
        ("app", Some(app)) => {
            use tilo_action_policy::roles::{DEVICE_PANEL, FILES_APP, SESSION_APP};
            match id {
                "session-app" if app == SESSION_APP => Why::SessionApp,
                "files-app" if app == FILES_APP => Why::FilesApp,
                "device-panel" if app == DEVICE_PANEL => Why::DevicePanel,
                _ if id.strip_prefix("app-author:") == Some(app) => Why::AppAuthor {
                    app: app.to_owned(),
                },
                _ => Why::OtherApp {
                    app: app.to_owned(),
                },
            }
        }
        ("task" | "shortcut", Some(named)) => Why::NoGrantee {
            subject: format!("{kind} {named}"),
        },
        ("foreign_device", _) => Why::NoGrantee {
            subject: "a device not the person's".to_owned(),
        },
        _ => Why::Unreadable { error },
    }
}

/// The grants the earlier layout's rows held, as this layout holds them, and
/// those it holds none for, each with why.
fn grants(doc: &AutoCommit) -> (Vec<Grant>, Vec<LeftOut>) {
    let mut held = Vec::new();
    let mut left_out = Vec::new();
    for (name, value) in row_values(doc, GRANTS_ROWS) {
        let why = match value {
            Ok(value) => match serde_json::from_value::<Grant>(earlier_grant(value.clone())) {
                Ok(grant) => {
                    held.push(grant);
                    continue;
                }
                Err(error) => why_left_out(&name, &value, error.to_string()),
            },
            Err(error) => Why::Unreadable { error },
        };
        left_out.push(LeftOut { grant: name, why });
    }
    (held, left_out)
}

/// What `doc`'s earlier layout held, read into this layout.
#[derive(Clone, Debug)]
pub struct Earlier {
    /// Its owner as this layout reads it, and the members, join times and
    /// grants its rows held.
    pub policy: WorkspacePolicy,
    /// The grants its rows held that this layout holds none for, each with
    /// why.
    pub left_out: Vec<LeftOut>,
}

/// The membership `doc`'s earlier layout held for `workspace`. `None` when the
/// document holds no earlier log, or one this layout's log already took over.
#[must_use]
pub fn membership(doc: &AutoCommit, workspace: &str) -> Option<Earlier> {
    if !holds_earlier_log(doc) {
        return None;
    }
    let mut policy = super::replay::start(doc, workspace);
    let owner = policy.owner.clone();
    policy.members = rows::<Role>(doc, MEMBERS_ROWS);
    policy
        .members
        .retain(|person, _| owner.as_ref() != Some(person));
    policy.joined = rows::<u64>(doc, JOINED_ROWS);
    let (grants, left_out) = grants(doc);
    if !grants.is_empty() {
        policy.grants = grants;
    }
    super::replay::order(&mut policy);
    Some(Earlier { policy, left_out })
}

/// This layout's operations that found `earlier` in `doc`, which holds none
/// yet, written by `device`, the workspace owner's, with `key`: one adding each
/// member at their role when they joined, oldest first, then one with every
/// grant the workspace held that the members' roles do not already give it.
/// How many it wrote.
///
/// # Errors
///
/// An operation cannot be signed or written.
pub fn found(
    doc: &mut AutoCommit,
    workspace: &str,
    earlier: &WorkspacePolicy,
    (device, key): (&DeviceCert, &dyn DeviceSigner),
    now_ns: u64,
) -> Result<usize, String> {
    let mut written = 0_usize;
    let mut parents = Vec::new();
    let mut write = |doc: &mut AutoCommit, at_ns: u64, changes: Vec<Change>| {
        let op = SignedMembershipOp::signed(
            device,
            key,
            std::mem::take(&mut parents),
            at_ns,
            MembershipOpCommand::Project { changes },
        )?;
        parents = vec![super::put_op(doc, &op)?];
        written = written.saturating_add(1);
        Ok::<_, String>(())
    };
    let mut people: Vec<(&String, &Role)> = earlier.members.iter().collect();
    people.sort_by_key(|(person, _)| (earlier.joined.get(*person).copied(), (*person).clone()));
    for (person, role) in people {
        let at_ns = earlier
            .joined
            .get(person)
            .map_or(now_ns, |ms| ms.saturating_mul(1_000_000));
        write(
            doc,
            at_ns,
            vec![Change::AddMember {
                person: person.clone(),
                role: *role,
            }],
        )?;
    }
    let held = super::replay(doc, workspace);
    let grants: Vec<Change> = earlier
        .grants
        .iter()
        .filter(|grant| !held.grants.contains(grant))
        .cloned()
        .map(Change::Grant)
        .collect();
    if !grants.is_empty() {
        write(doc, now_ns, grants)?;
    }
    Ok(written)
}

/// The people `policy` holds at their roles, for comparing two memberships.
#[must_use]
pub fn roles(policy: &WorkspacePolicy) -> BTreeMap<String, Option<Role>> {
    policy
        .members
        .keys()
        .map(|person| (person.clone(), policy.active_role(person)))
        .collect()
}
