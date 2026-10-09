//! Two replicas of one record — a person's and an agent's — converge through
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
use tilo_agent_participant::{Replica, Seed};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

#[test]
fn read_only_resource_views_filter_roles_and_never_join_the_document_door() {
    use ui::permissions::{Actor, PermissionContext, Role};
    let catalogue = catalogue();
    let app = "so.tilo.chat";
    let mut page = Replica::open(&catalogue, app, ROOM, "ada", false, Seed::New).unwrap();
    let mut doc = page.read(Clone::clone).unwrap();
    let workspace = chat::doc::ChatDoc::create_workspace(&mut doc, "Resource view fixture");
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    let resource = format!("chat.workspace:{workspace}");
    let before = body(&page);
    let viewer = PermissionContext::from_capabilities(
        Actor::person(Some(Role::Viewer), "ada").in_room(ROOM),
        Vec::new(),
    );
    let denied = PermissionContext::from_capabilities(
        Actor::person(Some(Role::Denied), "ada").in_room(ROOM),
        Vec::new(),
    );
    let views = page.resource_views(&resource, "chat.workspace", &viewer);
    assert!(
        !views.is_empty(),
        "the actual Chat resource declares a viewer navigation door"
    );
    assert!(
        views
            .iter()
            .all(|view| view.kind == tilo_actions::OperationKind::View)
    );
    assert!(
        page.resource_views(&resource, "chat.workspace", &denied)
            .is_empty()
    );
    assert!(
        page.resource_views("missing", "chat.workspace", &viewer)
            .is_empty()
    );
    assert!(
        page.resource_views(&resource, "wrong.kind", &viewer)
            .is_empty()
    );
    assert!(
        page.survey()
            .iter()
            .all(|action| action.kind == tilo_actions::OperationKind::Document)
    );
    assert_eq!(
        body(&page),
        before,
        "metadata discovery writes no CRDT changes"
    );
}

fn body(app: &Replica) -> String {
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
    let mut page = Replica::open(&catalogue, app, ROOM, "ada", false, Seed::New).unwrap();
    let snapshot = page
        .take_mutation()
        .expect("a new record is published whole");
    assert!(matches!(snapshot, DocumentMutation::Snapshot(_)));
    let mut agent = Replica::open(
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
}

/// The record's identity alone, as a participant holds it before the
/// creator's content arrives: the shortest prefix of the creator's history
/// that names the record and its schema.
fn identity_only(page: &Replica) -> Vec<u8> {
    use automerge::{ROOT, ReadDoc as _};
    let changes = page
        .read(|doc| {
            doc.clone()
                .get_changes(&[])
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect::<Vec<_>>()
        })
        .unwrap();
    let mut prefix = automerge::AutoCommit::new();
    changes
        .into_iter()
        .find_map(|change| {
            tilo_automerge::ApplyIncoming::load_incoming(&mut prefix, &change).unwrap();
            let named = prefix
                .get(ROOT, "tilo:record")
                .unwrap()
                .is_some_and(|(_, record)| {
                    ["home", "id", "kind", "room"]
                        .iter()
                        .all(|key| prefix.get(&record, *key).unwrap().is_some())
                });
            (named && prefix.get(ROOT, "tilo:schema_version").unwrap().is_some())
                .then(|| prefix.save())
        })
        .expect("the creator's history names the record")
}

fn joined(
    catalogue: &tilo_agent_participant::AppCatalogue,
    page: &Replica,
    changes: Vec<u8>,
) -> Option<Replica> {
    Replica::open(
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
fn content(app: &Replica) -> serde_json::Value {
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
        let mut page = Replica::open(&catalogue, app, ROOM, "ada", false, Seed::New).unwrap();
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

        // Joined before it: the identity alone, then the creator's document.
        // An app may also refuse to launch until the record is initialized
        // (Notes validates it): that writes nothing either.
        let Some(mut early) = joined(&catalogue, &page, identity_only(&page)) else {
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
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn an_agents_presence_is_where_its_last_operation_left_it() {
    let catalogue = catalogue();
    let app = "so.tilo.word-processor";
    let mut agent = Replica::open(&catalogue, app, ROOM, "writer", true, Seed::New).unwrap();
    assert!(
        agent.presence().is_none(),
        "nothing done, nowhere in particular"
    );
    let append = agent
        .survey()
        .into_iter()
        .find(|operation| operation.address.action == "append-paragraph")
        .expect("the document publishes append-paragraph");
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), "Héllo".into());
    agent.invoke(&append.address, &arguments, false).unwrap();

    let (anchor, head) = agent
        .presence()
        .expect("the append leaves the agent somewhere");
    let (key, start, end) =
        tilo_browser_peer::transport::read_presence_anchors(&anchor, &head).unwrap();
    let last = agent
        .read(|doc| {
            let state = <word_processor::model::Document as ui::tilo_resource::Resource>::read(doc)
                .unwrap();
            let body = state.text().unwrap().body();
            body.id_at(body.len() - 1).unwrap().to_owned()
        })
        .unwrap();
    // The paragraph it wrote, as the record's own item: the key every
    // collaborator's Writer draws a collaborator's caret under (as its
    // digest, when the key outgrows the live plane's anchor).
    let written = ui::presence::PresenceKey::Resource {
        app: app.to_owned(),
        id: last,
        kind: "word-processor.document".to_owned(),
    };
    assert!(
        key == written || Some(&key) == written.digest_form().as_ref(),
        "{key:?}"
    );
    assert_eq!((start, end), (5, 5), "a caret after its five characters");
}
