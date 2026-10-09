#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! The resource/action/view contract on the production host path.
//!
//! A document action whose `apply` writes an outside queue is undone by
//! applying its inverse action, not by Automerge inverse patches. View and
//! effect operations are published from the resource; a document spec stuffed
//! into that list is dropped.

use tilo_actions::{
    Action, ActionError, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
    ResourceViewSpec,
};
use tilo_app::automerge::transaction::Transactable;
use tilo_app::automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value};
use tilo_app::prelude::*;
use tilo_app::record::RecordId;
use tilo_app::record::transfer::Origin;
use tilo_resource::{ActionDecl, Doc, Invoke, ModelError};

const ITEMS: &str = "items";
const OUTBOX: &str = "outbox";
const DEL_OUTBOX: &str = "del_outbox";
const ACTION_TYPE: &str = "model-contract.job.action";

#[derive(
    Clone,
    Debug,
    tilo_resource::__private::serde::Serialize,
    tilo_resource::__private::serde::Deserialize,
)]
#[serde(crate = "tilo_resource::__private::serde")]
enum JobAction {
    Create { id: String },
    Remove { id: String },
}

struct Job;

impl Resource for Job {
    const KIND: &'static str = "model-contract.job";
    const LABEL: &'static str = "Job";
    const ACTION_TYPE: &'static str = ACTION_TYPE;
    type State = Vec<String>;
    type Action = JobAction;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok(item_ids(doc))
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        if doc.get(ROOT, ITEMS)?.is_none() {
            doc.put_object(ROOT, ITEMS, ObjType::Map)?;
            doc.put_object(ROOT, OUTBOX, ObjType::List)?;
            doc.put_object(ROOT, DEL_OUTBOX, ObjType::List)?;
        }
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![ActionDecl::new(
            "create",
            "Create",
            vec![ActionParam::required("id", ParamType::Text, "Item id")],
            |args, _| {
                let id = args.text("id").unwrap_or_default().to_string();
                if id.is_empty() {
                    return Err("id required".into());
                }
                Ok(JobAction::Create { id })
            },
        )]
    }

    fn local_operations() -> Vec<ActionSpec> {
        vec![
            ActionSpec::operation(
                "show",
                "Show",
                OperationKind::View,
                "model-contract.job.view",
                &"editor",
            )
            .expect("view payload encodes"),
            ActionSpec::immediate(
                "sneak",
                "Sneak",
                ACTION_TYPE,
                &JobAction::Create { id: "nope".into() },
            )
            .expect("document payload encodes"),
        ]
    }

    fn views(_state: &Self::State) -> Vec<ResourceViewSpec> {
        vec![ResourceViewSpec::new("editor", "Editor", "show")]
    }
}

impl Action for JobAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            JobAction::Create { id } => {
                put_item(doc, id)?;
                push(doc, OUTBOX, id)?;
            }
            JobAction::Remove { id } => {
                delete_item(doc, id)?;
                push(doc, DEL_OUTBOX, id)?;
            }
        }
        Ok(())
    }

    fn inverse_encoded(&self, doc: &tilo_actions::Doc) -> Option<EncodedAction> {
        let inverse = match self {
            JobAction::Create { id } => JobAction::Remove { id: id.clone() },
            JobAction::Remove { id } if item_ids(doc).iter().any(|item| item == id) => {
                JobAction::Create { id: id.clone() }
            }
            JobAction::Remove { .. } => return None,
        };
        EncodedAction::encode(ACTION_TYPE, &inverse).ok()
    }
}

struct JobApp;

impl ResourceApp for JobApp {
    type Resource = Job;
    const ID: &'static str = "so.tilo.model-contract";
    const TITLE: &'static str = "Job";

    fn view(_cx: &mut ViewCx<Job>) -> Element {
        Box::new(Column::new())
    }
}

fn create(id: &str) -> Invoke<Job> {
    Invoke::apply(&JobAction::Create { id: id.into() }).expect("action encodes")
}

fn item_ids(doc: &tilo_app::automerge::AutoCommit) -> Vec<String> {
    let Ok(Some((_, map))) = doc.get(ROOT, ITEMS) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = doc.keys(&map).collect();
    ids.sort();
    ids
}

fn list(doc: &tilo_app::automerge::AutoCommit, key: &str) -> Vec<String> {
    let Ok(Some((_, list))) = doc.get(ROOT, key) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|index| match doc.get(&list, index).ok().flatten() {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn put_item(doc: &mut tilo_app::automerge::AutoCommit, id: &str) -> Result<(), ActionError> {
    let (_, map) = doc.get(ROOT, ITEMS)?.expect("items map");
    doc.put(&map, id, true)?;
    Ok(())
}

fn delete_item(doc: &mut tilo_app::automerge::AutoCommit, id: &str) -> Result<(), ActionError> {
    let (_, map) = doc.get(ROOT, ITEMS)?.expect("items map");
    doc.delete(&map, id)?;
    Ok(())
}

fn push(
    doc: &mut tilo_app::automerge::AutoCommit,
    key: &str,
    value: &str,
) -> Result<(), ActionError> {
    let (_, list) = doc.get(ROOT, key)?.expect("queue list");
    let index = doc.length(&list);
    doc.insert(&list, index, value)?;
    Ok(())
}

#[test]
fn undo_of_an_outside_consequence_applies_the_inverse_action() {
    let mut app = TestApp::<JobApp>::new().unwrap();
    app.invoke(create("evt")).unwrap();
    assert_eq!(app.state(), ["evt"]);
    assert_eq!(
        app.read_document(|doc| list(doc, OUTBOX)),
        ["evt".to_string()]
    );

    assert!(app.undo());
    assert!(app.state().is_empty());
    // Patch undo would drop the outbox write and write nothing in its place.
    // The inverse action deletes the item and queues the outside delete.
    assert_eq!(
        app.read_document(|doc| list(doc, OUTBOX)),
        ["evt".to_string()]
    );
    assert_eq!(
        app.read_document(|doc| list(doc, DEL_OUTBOX)),
        ["evt".to_string()]
    );
    let summaries = version_summaries(&app);
    assert!(
        summaries.iter().any(|summary| summary.contains("Remove")),
        "undo is a named history entry, not an anonymous patch: {summaries:?}"
    );

    assert!(app.redo());
    assert_eq!(app.state(), ["evt"]);
    assert_eq!(
        app.read_document(|doc| list(doc, OUTBOX)),
        ["evt".to_string(), "evt".to_string()]
    );
}

#[test]
fn publish_is_the_only_door_for_views_and_document_actions() {
    let published = tilo_resource::publish::<Job>("job", "Job", &Vec::new());
    let ids: Vec<_> = published
        .actions
        .iter()
        .map(|spec| spec.id.as_str())
        .collect();
    for id in ["create", "apply", "show"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    assert!(!ids.contains(&"sneak"), "{ids:?}");
    let show = published
        .actions
        .iter()
        .find(|spec| spec.id == "show")
        .unwrap();
    assert_eq!(show.kind, OperationKind::View);
    assert!(published.views.iter().any(|view| view.id == "editor"));
}

#[test]
fn a_second_replica_applying_the_same_action_converges() {
    let mut app = TestApp::<JobApp>::new().unwrap();
    app.invoke(create("local")).unwrap();
    let mut peer = app.peer_document();
    JobAction::Create {
        id: "remote".into(),
    }
    .apply(&mut peer)
    .unwrap();
    app.merge_peer(&mut peer).unwrap();
    let mut ids = app.state();
    ids.sort();
    assert_eq!(ids, ["local", "remote"]);
}

fn version_summaries(app: &TestApp<JobApp>) -> Vec<String> {
    let reader = tilo_history::Reader::new("en-US");
    app.versions()
        .into_iter()
        .filter_map(|version| version.context.map(|context| reader.sentence(&context)))
        .collect()
}

/// What #1300's `validate_import` allows in a record's meta map. A resource
/// document that grows any other key cannot be carried into a governed room.
const IMPORTABLE_META: &[&str] = &["id", "kind", "home", "room", "formerly", "refs", "blobs"];

#[test]
fn collaboration_and_history_survive_a_move_into_another_room() {
    let mut app = TestApp::<JobApp>::new().unwrap();
    app.invoke(create("local")).unwrap();
    let mut peer = app.peer_document();
    let remote = JobAction::Create {
        id: "remote".into(),
    };
    let encoded = EncodedAction::encode(ACTION_TYPE, &remote).unwrap();
    tilo_actions::apply_local_with_context(
        &mut peer,
        &remote,
        encoded,
        Some(b"peer-author"),
        tilo_actions::ActionContext::for_app(ACTION_TYPE, "Job"),
    )
    .unwrap();
    app.merge_peer(&mut peer).unwrap();
    let mut ids = app.state();
    ids.sort();
    assert_eq!(ids, ["local", "remote"]);
    let summaries = version_summaries(&app);
    assert!(
        summaries.iter().any(|summary| summary.contains("Create")),
        "a collaborator's action is history: {summaries:?}"
    );

    let source = app.read_document(Clone::clone);
    let carried: Vec<_> = tilo_resource::history::versions(&mut source.clone())
        .into_iter()
        .map(|version| version.hash)
        .collect();
    assert!(!carried.is_empty());
    let origin = tilo_app::record::identity(&source).expect("record identity");
    let from = tilo_app::record::room_scope(&source).expect("room scope");
    let target = RecordId::minted();
    let mut arrived = tilo_app::record::transfer::rehome(
        &source,
        &Origin::Record(origin.clone()),
        &from,
        "governed-room",
        &target,
    )
    .expect("a resource document moves with its history");

    let mut arrived_ids = Job::read(&arrived).unwrap();
    arrived_ids.sort();
    assert_eq!(arrived_ids, ["local".to_string(), "remote".to_string()]);
    assert_eq!(
        tilo_app::record::former_identities(&arrived),
        vec![origin.id]
    );
    assert!(tilo_app::record::references(&arrived).is_empty());
    assert!(
        arrived.get(ROOT, "tilo:records").unwrap().is_none(),
        "a document listing other records cannot join a room"
    );
    let (_, meta) = arrived.get(ROOT, "tilo:record").unwrap().expect("meta");
    let foreign: Vec<_> = arrived
        .keys(&meta)
        .filter(|key| !IMPORTABLE_META.contains(&key.as_str()))
        .collect();
    assert!(
        foreign.is_empty(),
        "meta keys a governed import would refuse: {foreign:?}"
    );
    let arrived_hashes: Vec<_> = tilo_resource::history::versions(&mut arrived)
        .into_iter()
        .map(|version| version.hash)
        .collect();
    for hash in &carried {
        assert!(
            arrived_hashes.contains(hash),
            "the move dropped a change from the resource history"
        );
    }
    assert!(arrived_hashes.len() > carried.len());
}
