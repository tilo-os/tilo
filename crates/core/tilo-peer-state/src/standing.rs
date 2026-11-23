//! What a workspace keeps with a change that takes standing away: where each
//! document's history stood for the member who lost it, and what they held
//! before.
//!
//! Every write is judged by the governance in its causal past: the roles and
//! grants its writer held when it wrote ([`collab_types::SignedOp::governance`]
//! names them). Taken alone, that would let a member who was demoted or removed
//! go on writing as they were, by building on governance from before the
//! change. So a change that takes standing away from a person (their ownership,
//! a role, a grant naming them) or from a device (the copy of the workspace a
//! server keeps) writes its *cut* in the same change: for every device of
//! theirs and every document, the number of their last write the change's
//! writer had taken, and the standing they held before.
//!
//! - A write the cut keeps is judged as written before the change: by the
//!   governance it names.
//! - Every other write of theirs that does not name the change is judged by the
//!   governance after it, whatever parents it picked.
//!
//! The cuts are one map of the workspace document, [`FIELD_V1`]. A key starting
//! `m` says a cut is written, under its subject (a person's root or a device's
//! key) and its own id; its value holds the cut's order among the subject's
//! cuts and their standing before it. A key starting `t` holds the number a
//! device of the subject had reached in a route, and one starting `c` a
//! compaction of theirs the writer had taken, by that write's id: a compaction
//! restates history under a number its author used before, so its number says
//! nothing. A cut is never lowered and never erased ([`monotonic`]).
//!
//! A workspace starts its governance over from a base at each admission
//! ([`crate::workspace::base`]); a cut lives in the governance's state, so it
//! survives that, and the standing it keeps is what judges a write whose
//! governance went into the base ([`Cut::prior`]).

use std::collections::{BTreeMap, BTreeSet};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Answer, Grant, Role, WorkspacePolicy};

use crate::removals::Held;

/// The workspace document's key for the cuts of changes that took standing
/// away.
pub const FIELD_V1: &str = "standing_cuts_v1";

/// What a person holds in a workspace's governance on their own: whether they
/// own it, their role, the workspace's own roles they are given and the grants
/// naming them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Standing {
    owner: bool,
    role: Option<Role>,
    assigned: BTreeSet<String>,
    grants: Vec<Grant>,
}

impl Standing {
    /// What every person `policy` names holds in it, by person.
    pub(crate) fn all(policy: &WorkspacePolicy) -> BTreeMap<String, Self> {
        let mut all: BTreeMap<String, Self> = BTreeMap::new();
        if let Some(owner) = &policy.owner {
            all.entry(owner.clone()).or_default().owner = true;
        }
        for (person, role) in &policy.members {
            all.entry(person.clone()).or_default().role = Some(*role);
        }
        for (person, assigned) in &policy.assigned_roles {
            all.entry(person.clone())
                .or_default()
                .assigned
                .clone_from(assigned);
        }
        for grant in &policy.grants {
            if let Some(person) = grant.grantee.person_id() {
                all.entry(person.to_owned())
                    .or_default()
                    .grants
                    .push(grant.clone());
            }
        }
        for standing in all.values_mut() {
            standing.grants.sort_by(|a, b| a.id.cmp(&b.id));
        }
        all
    }

    /// Whether `after` holds everything this standing held: no ownership, role,
    /// assigned role or allowing grant taken, and no refusal added. An owner holds
    /// everything.
    pub(crate) fn kept_by(&self, after: &Self) -> bool {
        if after.owner {
            return true;
        }
        if self.owner {
            return false;
        }
        let role = match (self.role, after.role) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(before), Some(after)) => after.includes(before),
        };
        role && self.assigned.is_subset(&after.assigned)
            && self
                .grants
                .iter()
                .all(|grant| grant.answer != Answer::Allow || after.grants.contains(grant))
            && after
                .grants
                .iter()
                .all(|grant| grant.answer == Answer::Allow || self.grants.contains(grant))
    }

    /// `policy` with `person` holding this standing in it, whatever it says of
    /// them: what judges a write of theirs that a cut keeps, when the
    /// governance it was written under went into a base. Everything else of
    /// the policy, the roles' own grants included, is the policy's.
    pub(crate) fn held_in(&self, mut policy: WorkspacePolicy, person: &str) -> WorkspacePolicy {
        if self.owner {
            policy.owner = Some(person.to_owned());
        }
        match self.role {
            Some(role) => {
                policy.members.insert(person.to_owned(), role);
            }
            None => {
                policy.members.remove(person);
            }
        }
        if self.assigned.is_empty() {
            policy.assigned_roles.remove(person);
        } else {
            policy
                .assigned_roles
                .insert(person.to_owned(), self.assigned.clone());
        }
        policy
            .grants
            .retain(|grant| grant.grantee.person_id() != Some(person));
        policy.grants.extend(self.grants.iter().cloned());
        policy
    }
}

/// One value of the map, as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Stored {
    /// Under a cut's key: the cut is written, `order`th among its subject's,
    /// and its subject held `prior` (a [`Standing`] as JSON) before it.
    Cut { order: u64, prior: Vec<u8> },
    /// Under a device's and a route's key: the number of the device's last
    /// write to the route the writer had taken.
    Taken(u64),
    /// Under the key of a compaction: the writer had taken it.
    Recorded,
}

/// A cut as the workspace holds it.
#[derive(Clone, Debug)]
pub(crate) struct Cut {
    id: String,
    subject: [u8; 32],
    /// Its place among its subject's cuts: one more than the cuts its writer
    /// held of them.
    pub(crate) order: u64,
    /// What its subject held before it.
    pub(crate) prior: Standing,
    /// The governance changes that wrote it: the change that took the
    /// standing away, or the base it went into.
    pub(crate) written_in: Vec<ChangeHash>,
}

fn cut_key(subject: &[u8; 32], id: &str) -> String {
    format!("m\0{}\0{id}", hex::encode(subject))
}

fn cut_prefix(subject: &[u8; 32]) -> String {
    format!("m\0{}\0", hex::encode(subject))
}

fn taken_key(subject: &[u8; 32], id: &str, device: &[u8; 32], route: &str) -> String {
    format!(
        "t\0{}\0{id}\0{}\0{route}",
        hex::encode(subject),
        hex::encode(device)
    )
}

fn compaction_key(
    subject: &[u8; 32],
    id: &str,
    device: &[u8; 32],
    compaction: &[u8; 32],
    route: &str,
) -> String {
    format!(
        "c\0{}\0{id}\0{}\0{}\0{route}",
        hex::encode(subject),
        hex::encode(device),
        hex::encode(compaction)
    )
}

const NOT_A_MAP: &str = "the cuts of standing taken away are not a map";
const NOT_A_CUT: &str = "a cut of standing taken away does not decode";

/// Every map the workspace keeps the cuts in: none until one is written, and
/// more than one only where two devices wrote the first one at once.
fn maps(doc: &AutoCommit) -> Result<Vec<ObjId>, String> {
    doc.get_all(ROOT, FIELD_V1)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(value, id)| match value {
            Value::Object(ObjType::Map) => Ok(id),
            _ => Err(NOT_A_MAP.to_owned()),
        })
        .collect()
}

/// Every value `key` holds in `maps`, with the operation that wrote it.
fn stored(
    doc: &AutoCommit,
    maps: &[ObjId],
    key: &str,
) -> Result<Vec<(Stored, automerge::ObjId)>, String> {
    let mut values = Vec::new();
    for map in maps {
        for (value, op) in doc.get_all(map, key).map_err(|error| error.to_string())? {
            let Value::Scalar(value) = value else {
                return Err(NOT_A_CUT.into());
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                return Err(NOT_A_CUT.into());
            };
            values.push((
                crate::governed_routes::exact::<Stored>(bytes).ok_or(NOT_A_CUT)?,
                op,
            ));
        }
    }
    Ok(values)
}

/// The cuts `doc` holds of `subject`, a person's root or a device's key.
///
/// # Errors
///
/// The map does not read.
pub(crate) fn cuts(doc: &AutoCommit, subject: &[u8; 32]) -> Result<Vec<Cut>, String> {
    let maps = maps(doc)?;
    let prefix = cut_prefix(subject);
    let keys: BTreeSet<String> = maps
        .iter()
        .flat_map(|map| {
            doc.map_range(map, prefix.clone()..)
                .map(|item| item.key.into_owned())
                .take_while(|key| key.starts_with(&prefix))
        })
        .collect();
    let mut cuts = Vec::new();
    for key in keys {
        let mut cut: Option<Cut> = None;
        for (value, op) in stored(doc, &maps, &key)? {
            let Stored::Cut { order, prior } = value else {
                return Err(NOT_A_CUT.into());
            };
            let prior: Standing = serde_json::from_slice(&prior).map_err(|_| NOT_A_CUT)?;
            let written = doc.hash_for_opid(&op).ok_or(NOT_A_CUT)?;
            let cut = cut.get_or_insert_with(|| Cut {
                id: key
                    .strip_prefix(prefix.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                subject: *subject,
                order,
                prior,
                written_in: Vec::new(),
            });
            cut.order = cut.order.min(order);
            cut.written_in.push(written);
        }
        cuts.extend(cut);
    }
    Ok(cuts)
}

/// Whether `cut` keeps `device`'s write number `sequence` to `document` of
/// `app`: the cut's writer had taken it. `compaction` is the write's id when it
/// is a compaction: then that id decides, never the number.
///
/// # Errors
///
/// The map does not read.
pub(crate) fn keeps(
    doc: &AutoCommit,
    cut: &Cut,
    device: &[u8; 32],
    (app, document): (&str, &str),
    sequence: u64,
    compaction: Option<&[u8; 32]>,
) -> Result<bool, String> {
    let maps = maps(doc)?;
    let route = crate::coverage::route_key(app, document);
    if let Some(id) = compaction {
        let key = compaction_key(&cut.subject, &cut.id, device, id, &route);
        return Ok(!stored(doc, &maps, &key)?.is_empty());
    }
    let taken = stored(
        doc,
        &maps,
        &taken_key(&cut.subject, &cut.id, device, &route),
    )?
    .into_iter()
    .filter_map(|(value, _)| match value {
        Stored::Taken(sequence) => Some(sequence),
        _ => None,
    })
    .max()
    .unwrap_or(0);
    Ok(sequence <= taken)
}

/// Write a cut of `subject`'s standing into `doc`, under `id`: they held
/// `prior` before the change it goes with, and the writer had taken `held` of
/// each of their devices, by route ([`crate::coverage::route_key`]). The
/// caller commits it with the change that takes the standing away.
///
/// # Errors
///
/// The map does not read, or the workspace document refuses the write.
pub(crate) fn write(
    doc: &mut AutoCommit,
    subject: &[u8; 32],
    id: &str,
    prior: &Standing,
    held: &BTreeMap<[u8; 32], BTreeMap<String, Held>>,
) -> Result<(), String> {
    let order = u64::try_from(cuts(doc, subject)?.len())
        .map_err(|error| error.to_string())?
        .saturating_add(1);
    let map = match maps(doc)?.into_iter().next() {
        Some(map) => map,
        None => doc
            .put_object(ROOT, FIELD_V1, ObjType::Map)
            .map_err(|error| error.to_string())?,
    };
    let mut put = |key: String, value: Stored| -> Result<(), String> {
        let bytes = postcard::to_allocvec(&value).map_err(|error| error.to_string())?;
        doc.put(&map, key, ScalarValue::Bytes(bytes))
            .map_err(|error| error.to_string())
    };
    for (device, routes) in held {
        for (route, taken) in routes {
            if taken.last > 0 {
                put(
                    taken_key(subject, id, device, route),
                    Stored::Taken(taken.last),
                )?;
            }
            for compaction in &taken.compactions {
                put(
                    compaction_key(subject, id, device, compaction, route),
                    Stored::Recorded,
                )?;
            }
        }
    }
    let prior = serde_json::to_vec(prior).map_err(|error| error.to_string())?;
    put(cut_key(subject, id), Stored::Cut { order, prior })
}

/// What the map of `doc` holds, key by key: every value written under it.
fn rows(doc: &AutoCommit) -> Result<BTreeMap<String, Vec<Stored>>, String> {
    let maps = maps(doc)?;
    let mut rows = BTreeMap::new();
    for key in maps.iter().flat_map(|map| doc.keys(map)) {
        if rows.contains_key(&key) {
            continue;
        }
        let values = stored(doc, &maps, &key)?
            .into_iter()
            .map(|(value, _)| value)
            .collect();
        rows.insert(key, values);
    }
    Ok(rows)
}

/// Whom `after` holds a cut of that `before` does not: whom the change from
/// one to the other cut.
///
/// # Errors
///
/// Either map does not read.
pub fn subjects_cut(before: &AutoCommit, after: &AutoCommit) -> Result<BTreeSet<[u8; 32]>, String> {
    let held = rows(before)?;
    Ok(rows(after)?
        .into_keys()
        .filter(|key| !held.contains_key(key))
        .filter_map(|key| {
            let (subject, _) = key.strip_prefix("m\0")?.split_once('\0')?;
            hex::decode(subject).ok()?.try_into().ok()
        })
        .collect())
}

/// The agents whose delegation the change from `before` to `after` withdraws:
/// those whose identity row `after` holds withdrawn and `before` did not. Only
/// the rows the change wrote are read, so a change in a workspace of many
/// members decodes no more than one in a small one.
pub(crate) fn newly_withdrawn(before: &AutoCommit, after: &AutoCommit) -> BTreeSet<[u8; 32]> {
    let withdrawn = |bytes: &[u8]| {
        match crate::state::GovernedIdentity::decode(bytes) {
            Ok(identity) => Some(identity),
            // An agent the row withdrew for would act with its standing if the row were
            // read as one that withdrew nothing, so a row that does not decode is said.
            Err(error) => {
                tracing::warn!(%error, "a governance identity row does not decode; its withdrawn delegation is not read");
                None
            }
        }
        .filter(|identity| {
            identity
                .delegate
                .as_ref()
                .is_some_and(|delegate| delegate.withdrawn_at_ns.is_some())
        })
        .map(|identity| identity.device_cert.device_public_key)
    };
    let was = crate::workspace::policy::read(before);
    crate::workspace::policy::read(after)
        .identities
        .iter()
        .filter(|(key, bytes)| was.identities.get(*key) != Some(*bytes))
        .filter(|(key, _)| {
            was.identities
                .get(*key)
                .is_none_or(|bytes| withdrawn(bytes).is_none())
        })
        .filter_map(|(_, bytes)| withdrawn(bytes))
        .collect()
}

/// Whom the change from the governance `before` to `after` of `workspace`
/// takes standing from: each person whose standing `after` does not keep,
/// each server it no longer has keep the workspace, and each agent whose
/// delegation it withdraws. A change cuts only them.
#[must_use]
pub fn taken_from(workspace: &str, before: &AutoCommit, after: &AutoCommit) -> BTreeSet<[u8; 32]> {
    let was = Standing::all(&crate::workspace::policy_of(before, workspace));
    let is = Standing::all(&crate::workspace::policy_of(after, workspace));
    let people = was
        .into_iter()
        .filter(|(person, prior)| !prior.kept_by(&is.get(person).cloned().unwrap_or_default()))
        .filter_map(|(person, _)| hex::decode(person).ok()?.try_into().ok());
    let kept: BTreeSet<[u8; 32]> = crate::keeping_devices_of(after)
        .iter()
        .map(|hint| hint.host.device_public_key)
        .collect();
    let servers = crate::keeping_devices_of(before)
        .into_iter()
        .map(|hint| hint.host.device_public_key)
        .filter(|server| !kept.contains(server));
    people
        .chain(servers)
        .chain(newly_withdrawn(before, after))
        .collect()
}

/// Check that `after` keeps every cut `before` holds: none is erased, changed
/// or lowered, since either would let a member write again as they were.
///
/// # Errors
///
/// A cut is erased, changed or lowered, or `after`'s map does not read.
pub fn monotonic(before: Option<&AutoCommit>, after: &AutoCommit) -> Result<(), String> {
    let next = rows(after)?;
    let Some(before) = before else {
        return Ok(());
    };
    if maps(before)?.is_empty() {
        return Ok(());
    }
    for (key, values) in rows(before)? {
        let still = next.get(&key).map(Vec::as_slice).unwrap_or_default();
        for value in values {
            let kept = match &value {
                Stored::Taken(taken) => still
                    .iter()
                    .any(|value| matches!(value, Stored::Taken(now) if now >= taken)),
                value => still.contains(value),
            };
            if !kept {
                return Err("the cut of a change that took standing away cannot be undone".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming as _;

    const PERSON: [u8; 32] = [7; 32];
    const DEVICE: [u8; 32] = [8; 32];

    fn editor() -> Standing {
        Standing {
            role: Some(Role::Editor),
            ..Standing::default()
        }
    }

    fn held(last: u64) -> BTreeMap<[u8; 32], BTreeMap<String, Held>> {
        BTreeMap::from([(
            DEVICE,
            BTreeMap::from([(
                crate::coverage::route_key("app", "note"),
                Held {
                    last,
                    compactions: vec![[3; 32]],
                },
            )]),
        )])
    }

    fn keeps_write(doc: &AutoCommit, cut: &Cut, device: &[u8; 32], sequence: u64) -> bool {
        keeps(doc, cut, device, ("app", "note"), sequence, None).unwrap()
    }

    #[test]
    fn a_cut_keeps_what_its_writer_had_taken_and_says_what_its_subject_held() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "name", "Workspace").unwrap();
        doc.commit();
        assert!(cuts(&doc, &PERSON).unwrap().is_empty());
        write(&mut doc, &PERSON, "one", &editor(), &held(3)).unwrap();
        doc.commit();
        let [cut] = cuts(&doc, &PERSON).unwrap().try_into().unwrap();
        assert_eq!(cut.order, 1);
        assert_eq!(cut.prior, editor());
        assert_eq!(cut.written_in, doc.get_heads());
        assert!(keeps_write(&doc, &cut, &DEVICE, 3));
        assert!(!keeps_write(&doc, &cut, &DEVICE, 4));
        // A device the cut does not list had nothing taken.
        assert!(!keeps_write(&doc, &cut, &[9; 32], 1));
        assert!(keeps(&doc, &cut, &DEVICE, ("app", "note"), 3, Some(&[3; 32])).unwrap());
        assert!(!keeps(&doc, &cut, &DEVICE, ("app", "note"), 3, Some(&[4; 32])).unwrap());
        assert!(cuts(&doc, &DEVICE).unwrap().is_empty());

        // A second cut of the same person comes after the first.
        let before = doc.clone();
        write(&mut doc, &PERSON, "two", &Standing::default(), &held(5)).unwrap();
        doc.commit();
        let orders: Vec<u64> = cuts(&doc, &PERSON)
            .unwrap()
            .iter()
            .map(|cut| cut.order)
            .collect();
        assert_eq!(orders, [1, 2]);
        assert_eq!(monotonic(Some(&before), &doc), Ok(()));

        // Taking it back would let the person write as they were.
        let map = maps(&doc).unwrap()[0].clone();
        let mut erased = doc.clone();
        erased
            .delete(&map, cut_key(&PERSON, "one").as_str())
            .unwrap();
        assert!(monotonic(Some(&doc), &erased).is_err());
        let mut lowered = doc.clone();
        lowered
            .put(
                &map,
                taken_key(
                    &PERSON,
                    "one",
                    &DEVICE,
                    &crate::coverage::route_key("app", "note"),
                ),
                ScalarValue::Bytes(postcard::to_allocvec(&Stored::Taken(1)).unwrap()),
            )
            .unwrap();
        assert!(monotonic(Some(&doc), &lowered).is_err());
    }

    #[test]
    fn two_first_cuts_written_at_once_both_stand() {
        let mut base = AutoCommit::new();
        base.put(ROOT, "name", "Workspace").unwrap();
        base.commit();
        let (mut first, mut second) = (base.fork(), base.fork());
        write(&mut first, &PERSON, "one", &editor(), &held(2)).unwrap();
        write(&mut second, &DEVICE, "two", &Standing::default(), &held(4)).unwrap();
        first.commit();
        second.commit();
        first.merge_incoming(&mut second).unwrap();
        assert_eq!(cuts(&first, &PERSON).unwrap().len(), 1);
        let [cut] = cuts(&first, &DEVICE).unwrap().try_into().unwrap();
        assert!(keeps_write(&first, &cut, &DEVICE, 4));
    }

    #[test]
    fn a_standing_is_kept_only_by_one_that_holds_all_of_it() {
        let editor = editor();
        let viewer = Standing {
            role: Some(Role::Viewer),
            ..Standing::default()
        };
        let owner = Standing {
            owner: true,
            ..Standing::default()
        };
        let policy = |members: &[(&str, Role)]| WorkspacePolicy {
            members: members
                .iter()
                .map(|(person, role)| ((*person).to_owned(), *role))
                .collect(),
            ..WorkspacePolicy::new("w", "owner")
        };
        let all = Standing::all(&policy(&[("ada", Role::Editor)]));
        assert_eq!(all.get("ada"), Some(&editor));
        assert!(all["owner"].owner);
        assert!(viewer.kept_by(&editor));
        assert!(!editor.kept_by(&viewer));
        assert!(!editor.kept_by(&Standing::default()));
        assert!(!owner.kept_by(&editor));
        assert!(editor.kept_by(&owner));
        let refused = Standing {
            grants: vec![Grant::new(
                "refused",
                tilo_action_policy::Grantee::person("7".repeat(64)),
                tilo_action_policy::TreePath::workspace("w"),
                tilo_action_policy::KindSet::ALL,
                Answer::Deny,
                tilo_action_policy::Grantee::person("8".repeat(64)),
            )],
            ..editor.clone()
        };
        assert!(
            !editor.kept_by(&refused),
            "a refusal given takes standing away"
        );
        assert!(refused.kept_by(&editor));
    }
}
