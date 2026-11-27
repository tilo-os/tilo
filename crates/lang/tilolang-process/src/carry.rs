//! What a device kept of its apps before anyone signed in, carried into the person's workspace.
//!
//! Before a person's identity has a device, an app's window keeps its documents on the device
//! alone ([`tilo_resource_store::LocalPort`]), under `<data>/tilolang/<app>/resources`. Once the
//! device is signed in, before any of those windows opens again, what they kept is written into
//! the person's own workspace (the one every device of theirs finds by their key alone) through
//! the connection such a window joins (the same connection `start_joined` opens for it): every
//! resource as the placement and fields its window wrote, every document's changes as the app's
//! own writes of it, and the app's own records as the record commands a workspace answers.
//!
//! Such a connection is served as the app it runs: the workspace keeps what it writes by the
//! grant installing the app gave it, which is why a workspace keeps a carried app's records only
//! where that app is installed, and refuses the carry there in words where it is not.
//!
//! A window that ran before sign-in ran as the person `me`, a name it had no way to know its
//! person by. What its changes say of that person is renamed before the workspace sees it: the
//! caller it is judged by becomes the app the connection runs as, and the person it acted for
//! becomes the person the device is signed in as. A name the carry cannot rename safely (a
//! record or resource id built from it, a member a text mark names, someone else a change acted
//! for) refuses that app's carry in words and leaves its documents on the device, never guessing
//! at what the name meant.
//!
//! A carry is deterministic: run again, it sends the same changes, which merge as the same
//! changes. An app's directory is deleted only once every write of it was accepted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use collab_types::store::Resource;
use collab_types::store::wire::{Bytes, Command, RECORD_PAGE_BYTES, Reply};
use runtime_portal::process_link::Descriptor;
use tilo_actions::{Actor, MemberId, PersonId};
use tilo_resource_store::{LocalPort, Port, Schema, Stamp};
use tilolang_tilo::bundle::Bundle;

use crate::connection::Route;

/// The person a window runs as before anyone signed in: the name its changes carry until the
/// carry renames them to the person the device is signed in as.
pub const ME: &str = "me";

/// What a device hands the carry for an app whose documents it kept: the package the app runs,
/// the route its window joins, and the connection its daemon made for that window, acting as the
/// person.
pub struct Handed {
    pub bundle: Arc<Bundle>,
    pub route: Route,
    pub collaboration: Descriptor,
    pub identity: Descriptor,
    /// The person the connection acts as, their key in hex.
    pub person: String,
    /// The app the connection's writes are the app's own for, by its id: a window's instance
    /// connection is served as the app it runs, whose workspaces keep its records by the grant
    /// installing it gave it. `None` for a connection whose writes are the person's.
    pub as_app: Option<String>,
}

/// What a carry did, app by app.
#[derive(Debug, Default)]
pub struct Carried {
    /// Each app whose documents the carry found, in app order, and whether they are the
    /// workspace's now: the reason in words when they are not, its documents still on the device.
    pub apps: Vec<(String, Result<(), String>)>,
}

impl Carried {
    /// Each app whose documents are still on the device, with the reason in words.
    #[must_use]
    pub fn refused(&self) -> Vec<(&str, &str)> {
        self.apps
            .iter()
            .filter_map(|(app, outcome)| {
                outcome
                    .as_ref()
                    .err()
                    .map(|why| (app.as_str(), why.as_str()))
            })
            .collect()
    }

    /// Whether every app's documents are the workspace's now.
    #[must_use]
    pub fn all_carried(&self) -> bool {
        self.apps.iter().all(|(_, outcome)| outcome.is_ok())
    }
}

/// Carry every app's documents kept under `dir` into the workspace `make` connects to for it,
/// deleting each app's directory once every write of it was accepted.
///
/// `make` is what a device's sign-in answers with for an app: the package it runs and the
/// connection its window would join. A device of any kind calls this with its own answer.
///
/// # Errors
/// `dir` cannot be read. A failure for one app is no failure of the others: it is answered in
/// the [`Carried::apps`] entry for that app, and its documents stay where they are.
pub fn signed_out(
    dir: &Path,
    mut make: impl FnMut(&str) -> Result<Handed, String>,
) -> Result<Carried, String> {
    let mut carried = Carried::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(carried),
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    };
    let mut apps = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        if !kind.is_dir() {
            continue;
        }
        let Some(app) = entry.file_name().to_str().map(str::to_owned) else {
            tracing::warn!(
                path = %entry.path().display(),
                "what an app kept before sign-in has no name this device reads"
            );
            continue;
        };
        apps.push(app);
    }
    apps.sort();
    for app in apps {
        let outcome = carry_app(dir, &app, &mut make);
        let outcome = match outcome {
            Ok(()) => match std::fs::remove_dir_all(dir.join(&app)) {
                Ok(()) => Ok(()),
                Err(error) => Err(format!(
                    "what the app kept is carried, but its directory is not deleted: {error}"
                )),
            },
            Err(why) => Err(why),
        };
        carried.apps.push((app, outcome));
    }
    Ok(carried)
}

/// Carry one app's kept documents into the workspace `make` answers for it, answering why not in
/// words when a write is refused or a name cannot be renamed.
fn carry_app(
    dir: &Path,
    app: &str,
    make: &mut impl FnMut(&str) -> Result<Handed, String>,
) -> Result<(), String> {
    let kept = dir.join(app).join("resources");
    let local = LocalPort::at(kept)
        .map_err(|error| format!("{app}: what it kept does not open: {error}"))?
        .named(app);
    let handed =
        make(app).map_err(|error| format!("{app}: no connection to its workspace: {error}"))?;
    let person = handed.person.clone();
    PersonId::parse(&person).map_err(|error| {
        format!("{app}: its connection acts as `{person}`, who is no person: {error}")
    })?;
    // The store the window kept its documents in had the app's kinds declared; this one is
    // opened afresh, so it is told them before it is asked how their readers are placed.
    declare(&local, &handed.bundle);
    // The caller the workspace judges the carried writes by: the app itself, as a window's
    // instance connection is served, or the person.
    let caller = match handed.as_app.as_deref() {
        Some(app) => Actor::App(tilo_member_types::AppId::new(app)),
        None => Actor::Member(MemberId::person(person.as_str())),
    };
    let port = crate::window::hosted::joined_port(
        &handed.bundle,
        handed.route.clone(),
        (handed.collaboration, &handed.identity),
    )
    .map_err(|error| format!("{app}: its window's connection was not joined: {error}"))?;

    // The resources, each in the place its window put it: a parent before what is nested in it.
    let resources =
        order_by_parent(local.resources()).map_err(|error| format!("{app}: {error}"))?;
    let mut contents = Vec::new();
    for (kind, id, resource) in resources {
        if names_me(&kind) {
            return Err(format!(
                "{app}: the kind `{kind}` names the person the device was before sign-in \
                 (`{ME}`), and the carry cannot rename it"
            ));
        }
        // The resource and what holds it, named as the person the device is now: a person's own
        // state of an app is kept under `<app>@<person>`.
        let sent = id_as(&id, &person).map_err(|why| format!("{app}: {why}"))?;
        let parent = resource
            .parent
            .as_deref()
            .map(|parent| id_as(parent, &person))
            .transpose()
            .map_err(|why| format!("{app}: {why}"))?;
        let readers = if local.placed_by_rule(&kind) {
            // The workspace places it by its type's rule as the person the stamp names.
            String::new()
        } else {
            readers_as(&resource.readers, &person)
                .map_err(|error| format!("{app}: the readers of `{id}` are not carried: {error}"))?
        };
        // A kind only its author reads is the person's, and the store places it in their
        // document: an app's write there names the person it writes for.
        let acted_for =
            (handed.as_app.is_some() && local.read_by_author_rule(&kind)).then(|| person.clone());
        let stamp = placement_stamp(&caller, acted_for);
        let command = Command::Put {
            kind: kind.clone(),
            id: sent.clone(),
            resource: Resource {
                readers,
                parent,
                order: resource.order.clone(),
                fields: resource.fields.clone(),
            },
            stamp,
        };
        match port.command(command) {
            Ok(Reply::Written) => {}
            Ok(other) => {
                return Err(format!(
                    "{app}: the workspace answered the placement of `{id}` with {other:?}"
                ));
            }
            Err(why) => {
                return Err(format!("{app}: the workspace refused `{id}`: {why}"));
            }
        }
        if local.content_document(&kind, &id).is_some() {
            contents.push((kind, id, sent));
        }
    }

    // What each resource holds, as the app wrote it, which founds its document where the
    // workspace placed the resource.
    for (kind, id, sent) in contents {
        let doc = local
            .content_document(&kind, &id)
            .ok_or_else(|| format!("{app}: `{id}` holds no content to carry"))?;
        let bytes = local
            .save(&doc)
            .ok_or_else(|| format!("{app}: the content of `{id}` does not read"))?;
        let changes = restamp(&bytes, &caller, &person, &format!("{app}'s `{id}`"))?;
        write_record(&port, &sent, &changes)
            .map_err(|error| format!("{app}: what `{id}` holds was not written: {error}"))?;
    }

    // The records the app kept of its own, each by the id it named.
    for id in local.records() {
        if names_me(&id) {
            return Err(format!(
                "{app}: the record `{id}` names the person the device was before sign-in \
                 (`{ME}`), and the carry cannot rename it to the person it is"
            ));
        }
        let record = local
            .record(&id)
            .ok_or_else(|| format!("{app}: the record `{id}` reads nothing"))?;
        let bytes = record.save();
        let changes = restamp(&bytes, &caller, &person, &format!("{app}'s record `{id}`"))?;
        write_record(&port, &id, &changes)
            .map_err(|error| format!("{app}: its record `{id}` was not written: {error}"))?;
    }
    Ok(())
}

/// Write `changes`, a document's changes concatenated, as the record `record` of the joined
/// workspace, in runs of at most [`RECORD_PAGE_BYTES`]: the runs before the last are kept until
/// the last arrives, and the workspace takes them as one write.
fn write_record(port: &impl Port, record: &str, changes: &[u8]) -> Result<(), String> {
    let runs: Vec<&[u8]> = changes.chunks(RECORD_PAGE_BYTES).collect();
    for (index, run) in runs.iter().enumerate() {
        let command = Command::RecordChange {
            record: Some(record.to_owned()),
            changes: Bytes(run.to_vec()),
            more: index < runs.len().saturating_sub(1),
            commit: None,
        };
        match port.command(command)? {
            Reply::Written => {}
            other => return Err(format!("the workspace answered the write with {other:?}")),
        }
    }
    Ok(())
}

/// The resources in an order a store places them in: each after the resource it is nested in.
fn order_by_parent(
    resources: Vec<(String, String, Resource)>,
) -> Result<Vec<(String, String, Resource)>, String> {
    let by_id: BTreeSet<String> = resources.iter().map(|(_, id, _)| id.clone()).collect();
    let mut ordered = Vec::with_capacity(resources.len());
    let mut placed: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<(String, String, Resource)> = resources;
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|(kind, id, resource)| {
            let ready = resource
                .parent
                .as_ref()
                .is_none_or(|parent| !by_id.contains(parent) || placed.contains(parent));
            if ready {
                placed.insert(id.clone());
                ordered.push((kind.clone(), id.clone(), resource.clone()));
            }
            !ready
        });
        if pending.len() == before {
            let mut stuck: Vec<String> = pending.iter().map(|(_, id, _)| id.clone()).collect();
            stuck.sort();
            return Err(format!(
                "the resources {} are nested in one another, so no store could place them",
                stuck.join(", ")
            ));
        }
    }
    Ok(ordered)
}

/// Tell `store`, the port the window kept its documents in, the kinds its app declares and the
/// state it keeps for each person, as the app's own store was told them: an app's kind names who
/// reads each of its resources, and a store asks that to place a carried one.
fn declare(store: &LocalPort, bundle: &Bundle) {
    use tilolang_tilo::meta::MetaExt;
    let meta = &bundle.meta;
    store.declare(Schema {
        readers: meta
            .types
            .iter()
            .filter_map(|ty| Some((MetaExt::kind_of(meta, &ty.name), ty.readers.clone()?)))
            .collect(),
        ..Schema::default()
    });
    store.declare(tilolang_tilo::personal::readers());
}

/// `id`, a resource id the carry sends, naming the person the device is signed in as where its
/// window named the person it was before sign-in: a person's own state of an app is kept under
/// `<app>@<person>`.
///
/// # Errors
/// The id names the person the device was in a place the carry cannot rename.
fn id_as(id: &str, person: &str) -> Result<String, String> {
    if let Some(app) = id.strip_suffix(&format!("@{ME}")) {
        return Ok(format!("{app}@{person}"));
    }
    if names_me(id) {
        return Err(format!(
            "`{id}` names the person the device was before sign-in (`{ME}`), and the carry \
             cannot rename it to the person it is"
        ));
    }
    Ok(id.to_owned())
}

/// The stamp a carry places a resource under: the move verb, which every store admits for any
/// field of a declared kind, named as `caller`, and for `acted_for` when the kind is a person's.
fn placement_stamp(caller: &Actor, acted_for: Option<String>) -> Stamp {
    Stamp {
        verb: tilo_resource_store::MOVE_VERB.to_owned(),
        invoker: Stamp::by(caller),
        acted_for,
        invocation: uuid::Uuid::new_v4().to_string(),
        at: tilolang_tilo::clock::system_time(),
        ..Stamp::default()
    }
}

/// `readers`, a reader set, with the person the device was before sign-in read as `person`.
/// `*` reads as everyone and names no one.
fn readers_as(readers: &str, person: &str) -> Result<String, String> {
    if readers == collab_types::EVERY_MEMBER || readers.is_empty() {
        return Ok(readers.to_owned());
    }
    let mut out = Vec::new();
    for reader in readers.split(',') {
        let reader = reader.trim();
        if reader == ME {
            out.push(person.to_owned());
        } else if reader.contains(&format!("\"{ME}\"")) {
            return Err(format!(
                "`{reader}` names the person the device was before sign-in in a form the carry \
                 cannot rename"
            ));
        } else {
            out.push(reader.to_owned());
        }
    }
    Ok(out.join(","))
}

/// `doc`, saved whole, with every change's stamp naming `person` where it named the person the
/// device was before sign-in: the same operations, under the stamps they were made with, as the
/// person the device now is. Answers the changes concatenated, as a record write carries them.
///
/// It is deterministic: run again, it answers the same changes, which merge as the same changes.
fn restamp(doc: &[u8], caller: &Actor, person: &str, what: &str) -> Result<Vec<u8>, String> {
    let doc = automerge::Automerge::load(doc)
        .map_err(|error| format!("{what} does not read: {error}"))?;
    let mut renamed = BTreeMap::new();
    let mut out = Vec::new();
    for change in doc.get_changes(&[]) {
        let was = change.hash();
        let mut expanded = change.decode();
        expanded.hash = None;
        expanded.deps = expanded
            .deps
            .iter()
            .map(|dep| {
                renamed
                    .get(dep)
                    .copied()
                    .ok_or_else(|| format!("{what}: a change is missing the change it builds on"))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if let Some(message) = expanded.message.take() {
            expanded.message = Some(rename_message(&message, caller, person, what)?);
        }
        rename_marks(&mut expanded.operations, person, what)?;
        let change = automerge::Change::from(expanded);
        renamed.insert(was, change.hash());
        out.extend_from_slice(change.raw_bytes());
    }
    Ok(out)
}

/// `message`, a change's label, naming `person` where it named the person the device was before
/// sign-in: the same label in every other respect.
fn rename_message(
    message: &str,
    caller: &Actor,
    person: &str,
    what: &str,
) -> Result<String, String> {
    let Ok(mut stamp) = serde_json::from_str::<Stamp>(message) else {
        // A label in no store's form names no caller: the workspace reads none to judge it by.
        return Ok(message.to_owned());
    };
    rename_stamp(&mut stamp, caller, person).map_err(|why| format!("{what}: {why}"))?;
    Ok(stamp.message())
}

/// Rename the person the device was before sign-in wherever `stamp` names them: as `caller` in
/// the invoker, and as `person` in the one it acted for. Whether it named them at all.
///
/// # Errors
/// The change names someone else to act for, which the carry cannot know how to rename.
fn rename_stamp(stamp: &mut Stamp, caller: &Actor, person: &str) -> Result<bool, String> {
    let mut changed = false;
    let me = Some(Actor::Member(MemberId::person(ME)));
    if stamp.invoked_by() == me {
        stamp.invoker = Stamp::by(caller);
        match stamp.acted_for.as_deref() {
            None | Some(ME) => stamp.acted_for = Some(person.to_owned()),
            Some(other) => {
                return Err(format!(
                    "a change of the device's person names `{other}` to act for, whom the carry                      cannot rename"
                ));
            }
        }
        changed = true;
    } else if stamp.acted_for.as_deref() == Some(ME) {
        stamp.acted_for = Some(person.to_owned());
        changed = true;
    }
    if stamp.suggested_by == ME {
        person.clone_into(&mut stamp.suggested_by);
        changed = true;
    }
    if let Some(context) = stamp.context.as_mut()
        && let Some(invoker) = context.invoker.as_mut()
    {
        changed |= rename_acting(&mut invoker.acting, person);
    }
    Ok(changed)
}

/// Rename the person the device was before sign-in wherever `acting` names them.
fn rename_acting(acting: &mut tilo_actions::Acting, person: &str) -> bool {
    let mut changed = rename_actor(&mut acting.by, person);
    for inner in &mut acting.acted_for {
        changed |= rename_acting(inner, person);
    }
    changed
}

/// Rename the person the device was before sign-in wherever `actor` names them.
fn rename_actor(actor: &mut Actor, person: &str) -> bool {
    let Actor::Member(MemberId::Person(id)) = actor else {
        return false;
    };
    if id.as_str() != ME {
        return false;
    }
    let Ok(person) = PersonId::parse(person) else {
        return false;
    };
    *id = person;
    true
}

/// Rename the person the device was before sign-in wherever a text mark names them, in
/// `operations`: a mark value is the member it names, as JSON, so it renames as one.
fn rename_marks(
    operations: &mut [automerge::legacy::Op],
    person: &str,
    what: &str,
) -> Result<(), String> {
    for operation in operations {
        let automerge::legacy::OpType::MarkBegin(mark) = &mut operation.action else {
            continue;
        };
        let automerge::ScalarValue::Str(value) = &mark.value else {
            continue;
        };
        let Ok(mut member) = serde_json::from_str::<MemberId>(value) else {
            if value.starts_with('{') && value.contains(&format!("\"{ME}\"")) {
                return Err(format!(
                    "{what}: a text mark names the person the device was before sign-in in a \
                     form the carry cannot rename"
                ));
            }
            continue;
        };
        let MemberId::Person(id) = &mut member else {
            continue;
        };
        if id.as_str() != ME {
            continue;
        }
        *id = PersonId::parse(person).map_err(|_| {
            format!("{what}: `{person}` is no person's id, so a mark naming them cannot carry")
        })?;
        mark.value = automerge::ScalarValue::Str(member.mark_value().into());
    }
    Ok(())
}

/// Whether `text`, an id or a name the carry sends, names the person the device was before
/// sign-in as a whole part of itself: `me`, or a step of an id or reader set that is `me`.
fn names_me(text: &str) -> bool {
    text == ME || text.split(['/', ',', ':']).any(|part| part.trim() == ME)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a test's unwrap and panic are its assertions"
    )]

    use automerge::transaction::Transactable as _;
    use automerge::{ROOT, ReadDoc as _, ScalarValue};

    use super::*;

    /// The person the device is signed in as in these tests: a key in hex, as a connection names
    /// one.
    const ADA: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// The stamp a window that ran as `me` made its change under.
    fn mine(verb: &str) -> Stamp {
        let me = Actor::Member(MemberId::person(ME));
        Stamp {
            verb: verb.to_owned(),
            invoker: Stamp::by(&me),
            acted_for: Some(ME.to_owned()),
            invocation: "an-invocation".to_owned(),
            at: "2026-01-01T00:00:00Z".to_owned(),
            ..Stamp::default()
        }
    }

    /// A document holding one note, made under `stamp`, saved whole as the local store keeps it.
    fn note(stamp: &Stamp) -> Vec<u8> {
        let mut doc = automerge::AutoCommit::new();
        doc.put(ROOT, "title", "a note").unwrap();
        doc.commit_with(
            automerge::transaction::CommitOptions::default().with_message(stamp.message()),
        );
        doc.save()
    }

    /// A change of a document kept before sign-in reaches the workspace naming the caller the
    /// connection is judged by — the app, whose records the workspace keeps for the person — and
    /// the person it was for, holding what it held, and carrying the same changes twice.
    #[test]
    fn a_change_naming_the_person_before_sign_in_is_the_caller_s_now() {
        let caller = Actor::App(tilo_member_types::AppId::new("so.tilo.notes"));
        let bytes = note(&mine("new_note"));
        let once = restamp(&bytes, &caller, ADA, "a note").expect("the change carries");
        let again = restamp(&bytes, &caller, ADA, "a note").expect("the change carries");
        assert_eq!(once, again, "a carry sends the same changes each time");

        let carried = automerge::Automerge::load(&once).expect("the carried change reads");
        let changes = carried.get_changes(&[]);
        assert_eq!(changes.len(), 1);
        let stamp = Stamp::of(&changes[0]).expect("the change keeps its stamp");
        assert_eq!(
            stamp.invoked_by(),
            Some(caller),
            "the change names the caller the workspace judges it by"
        );
        assert_eq!(
            stamp.acted_for.as_deref(),
            Some(ADA),
            "and the person the app wrote it for"
        );
        assert_eq!(stamp.verb, "new_note", "the verb it was made under stays");
        let title = match carried.get(ROOT, "title").unwrap().unwrap().0 {
            automerge::Value::Scalar(scalar) => match scalar.as_ref() {
                ScalarValue::Str(text) => text.to_string(),
                other => panic!("the title reads as a scalar: {other:?}"),
            },
            other @ automerge::Value::Object(_) => {
                panic!("the title reads as a value: {other:?}");
            }
        };
        assert_eq!(title, "a note", "the change holds what it held");
    }

    /// A connection whose writes are the person's names the person, and a change that names
    /// someone else to act for is refused rather than guessed at.
    #[test]
    fn a_person_s_write_names_the_person_and_a_foreign_one_is_refused() {
        let person = Actor::Member(MemberId::person(ADA));
        let bytes = note(&mine("type"));
        let carried = restamp(&bytes, &person, ADA, "a note").expect("the change carries");
        let doc = automerge::Automerge::load(&carried).expect("the carried change reads");
        let stamp = Stamp::of(&doc.get_changes(&[])[0]).expect("the change keeps its stamp");
        assert_eq!(stamp.invoked_by(), Some(person));

        let mut foreign = mine("type");
        foreign.acted_for = Some("someone-else".to_owned());
        let bytes = note(&foreign);
        let caller = Actor::App(tilo_member_types::AppId::new("so.tilo.notes"));
        assert!(
            restamp(&bytes, &caller, ADA, "a note").is_err(),
            "a change naming someone else to act for is refused"
        );
    }

    /// A reader set names the person a device was before sign-in only as a whole member: `*` and
    /// other people's keys read as they did, and a shape the carry cannot rename is refused.
    #[test]
    fn a_reader_set_is_read_by_the_person_the_device_is_now() {
        assert_eq!(readers_as("me", ADA).unwrap(), ADA);
        assert_eq!(readers_as("111,me", ADA).unwrap(), format!("111,{ADA}"));
        assert_eq!(readers_as(collab_types::EVERY_MEMBER, ADA).unwrap(), "*");
        assert_eq!(readers_as("", ADA).unwrap(), "");
        assert!(
            readers_as("{\"person\":\"me\"}", ADA).is_err(),
            "a reader set naming the person another way is refused, not guessed at"
        );
    }

    /// A person's own state of an app is kept under `<app>@<person>`: the id is renamed to the
    /// person the device is now, and an id naming the person another way is refused.
    #[test]
    fn a_persons_own_state_is_kept_under_the_person_the_device_is_now() {
        assert_eq!(
            id_as("so.tilo.notes@me", ADA).unwrap(),
            format!("so.tilo.notes@{ADA}")
        );
        assert_eq!(id_as("root-notebook", ADA).unwrap(), "root-notebook");
        assert!(
            id_as("root/me/note", ADA).is_err(),
            "an id naming the person in a way the carry cannot rename is refused"
        );
    }

    /// An id or kind names the person the device was before sign-in only as a whole part.
    #[test]
    fn an_id_names_the_person_before_sign_in_only_as_a_part() {
        assert!(names_me("me"));
        assert!(names_me("root/mine/me"));
        assert!(names_me("tl:so.tilo.notes:me/x"));
        assert!(!names_me("memo"), "a word holding `me` is no person");
        assert!(!names_me("root-one/note-two"));
    }

    /// A resource is placed after the resource it is nested in, whatever order the store lists
    /// them in, and resources nested in one another are refused in words.
    #[test]
    fn resources_are_placed_after_what_holds_them() {
        let resource = |parent: Option<&str>| Resource {
            readers: collab_types::EVERY_MEMBER.to_owned(),
            parent: parent.map(str::to_owned),
            order: None,
            fields: BTreeMap::new(),
        };
        let listed = vec![
            (
                "kind".to_owned(),
                "note".to_owned(),
                resource(Some("notebook")),
            ),
            ("kind".to_owned(), "notebook".to_owned(), resource(None)),
        ];
        let ordered = order_by_parent(listed).expect("the resources are placed");
        assert_eq!(
            ordered
                .iter()
                .map(|(_, id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["notebook", "note"],
            "a note is placed after the notebook holding it"
        );

        let stuck = vec![
            ("kind".to_owned(), "a".to_owned(), resource(Some("b"))),
            ("kind".to_owned(), "b".to_owned(), resource(Some("a"))),
        ];
        assert!(
            order_by_parent(stuck).is_err(),
            "resources nested in one another are refused, not placed in a made-up order"
        );
    }
}
