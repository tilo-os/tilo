//! Two mounts of one record — a person's and an agent's — converge through
//! the changes each takes and the other applies, with no transport between
//! them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use collab_types::DocumentMutation;
use common::catalogue;
use tilo_actions::door::ActionAddress;
use tilo_agent_participant::{HostedApp, Seed};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

fn body(app: &HostedApp) -> String {
    app.read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
        .unwrap()
}

fn bytes(mutation: DocumentMutation) -> Vec<u8> {
    match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    }
}

#[test]
fn an_agent_mount_and_a_person_mount_converge() {
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let mut page = HostedApp::open(&catalogue, app, ROOM, "ada", false, Seed::New).unwrap();
    let snapshot = page
        .take_mutation()
        .expect("a new record is published whole");
    assert!(matches!(snapshot, DocumentMutation::Snapshot(_)));
    let mut agent = HostedApp::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        true,
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![bytes(snapshot)],
        },
    )
    .unwrap();
    // Whatever mounting wrote, the person's mount takes it in.
    if let Some(opened) = agent.take_mutation() {
        page.apply(&opened).unwrap();
    }
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), "Drafted by the agent".into());
    agent
        .invoke(
            &ActionAddress::new("scratchpad.note", "edit"),
            &arguments,
            false,
        )
        .unwrap();
    let edit = agent
        .take_mutation()
        .expect("the edit is the agent's own change");
    page.apply(&edit).unwrap();
    assert!(
        body(&page).contains("Drafted by the agent"),
        "{}",
        body(&page)
    );
    assert!(agent.take_mutation().is_none(), "nothing is taken twice");

    // And back: the person's edit reaches the agent, and the agent never
    // republishes it as its own.
    arguments.insert("text".into(), "Ada's revision".into());
    page.invoke(
        &ActionAddress::new("scratchpad.note", "edit"),
        &arguments,
        false,
    )
    .unwrap();
    agent.apply(&page.take_mutation().unwrap()).unwrap();
    assert!(body(&agent).contains("Ada's revision"), "{}", body(&agent));
    assert!(
        agent.take_mutation().is_none(),
        "a received change is not the agent's"
    );
    agent.close();
    page.close();
}

/// The record's identity alone, as a participant holds it before the
/// creator's content arrives.
fn identity_only(page: &HostedApp) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT, ReadDoc as _};
    let (identity, version) = page
        .read(|doc| {
            let (_, record) = doc.get(ROOT, "tilo:record").unwrap().unwrap();
            let fields: Vec<(String, String)> = ["home", "id", "kind", "room"]
                .iter()
                .map(|key| {
                    let (value, _) = doc.get(&record, *key).unwrap().unwrap();
                    ((*key).to_owned(), value.into_string().unwrap())
                })
                .collect();
            let (version, _) = doc.get(ROOT, "tilo:schema_version").unwrap().unwrap();
            (fields, version.as_i64().unwrap())
        })
        .unwrap();
    let mut doc = automerge::AutoCommit::new();
    let record = doc.put_object(ROOT, "tilo:record", ObjType::Map).unwrap();
    for (key, value) in identity {
        doc.put(&record, key, value).unwrap();
    }
    doc.put_object(&record, "refs", ObjType::List).unwrap();
    doc.put(ROOT, "tilo:schema_version", version).unwrap();
    doc.save()
}

fn joined(
    catalogue: &tilo_agent_participant::AppCatalogue,
    page: &HostedApp,
    changes: Vec<u8>,
) -> Option<HostedApp> {
    HostedApp::open(
        catalogue,
        page.app_id(),
        ROOM,
        "writer",
        true,
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![changes],
        },
    )
    .ok()
}

/// Every app's content, without the record identity both sides write.
fn content(app: &HostedApp) -> serde_json::Value {
    let mut body: serde_json::Value = serde_json::from_str(&body(app)).unwrap();
    body.as_object_mut().unwrap().remove("tilo:record");
    body
}

/// Only the record's creator writes its initial content: a participant that
/// joins, before or after that content arrives, adds nothing of its own, so
/// nobody's later edits land in a copy the merge hides.
#[test]
fn only_the_creator_seeds_a_record() {
    let catalogue = catalogue();
    let mut failures = Vec::new();
    // The room's own record (Chat's) is not handed over as a copy.
    for definition in tilo_agent_participant::component_apps()
        .into_iter()
        .filter(|definition| !definition.keeps_room_record())
    {
        let app = definition.app_id;
        let mut page = HostedApp::open(&catalogue, app, ROOM, "ada", false, Seed::New).unwrap();
        let snapshot = page.take_mutation().expect("a new record is published");
        let created = content(&page);

        // Joined after the content was published.
        let mut after = joined(&catalogue, &page, bytes(snapshot)).expect("the record opens");
        if let Some(opened) = after.take_mutation() {
            page.apply(&opened).unwrap();
        }
        if content(&page) != created {
            failures.push(format!(
                "{app}: a joined mount changed the record: {}",
                body(&page)
            ));
        }
        after.close();

        // Joined before it: the identity alone, then the creator's document.
        // An app may also refuse to launch until the record is initialized
        // (Notes validates it): that writes nothing either.
        let Some(mut early) = joined(&catalogue, &page, identity_only(&page)) else {
            page.close();
            continue;
        };
        let whole = page.read(|doc| doc.clone().save()).unwrap();
        early.apply(&DocumentMutation::Snapshot(whole)).unwrap();
        if let Some(opened) = early.take_mutation() {
            page.apply(&opened).unwrap();
        }
        if content(&page) != created {
            failures.push(format!(
                "{app}: an early joined mount changed the record: {}",
                body(&page)
            ));
        }
        if content(&early) != content(&page) {
            failures.push(format!(
                "{app}: the joined mount sees {} and the creator {}",
                body(&early),
                body(&page)
            ));
        }
        early.close();
        page.close();
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
