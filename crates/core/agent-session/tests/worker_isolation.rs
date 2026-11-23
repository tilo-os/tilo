//! App code the host runs for agents and workspace actions runs in confined
//! worker processes, out of reach of the host's state and keys, and what a
//! worker returns is admitted before the host would sign it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the PNG and PDF \
              helpers write fixed-layout bytes"
)]

use crate::common::{Body as _, catalogue, pdf};
use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_actions::OperationKind;
use tilo_actions::api::{ActionAddress, ActionDescriptor, ApiError};
use tilo_agent_session::worker::{ProbeReport, probe_confinement};
use tilo_agent_session::{ConfinedReplica, ReplicaActor, Seed};
use tilo_content::ContentStore;
use tilo_test_env::SpawnUnderEnvLock as _;

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

#[test]
fn a_confined_record_kept_at_an_older_storage_version_sends_its_migration() {
    use ui::tilo_resource::evolution::{VERSION_KEY, stored_version};
    // The suite's document app keeps its record at a version it migrates up to.
    let app = DOCUMENT;
    let mut page = crate::common::open(
        app,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let mut kept = automerge::AutoCommit::load(&snapshot).unwrap();
    let current = stored_version(&kept);
    automerge::transaction::Transactable::put(
        &mut kept,
        automerge::ROOT,
        VERSION_KEY,
        automerge::ScalarValue::Uint(u64::from(current - 1)),
    )
    .unwrap();

    // As a host holds a bot's record: in a worker, its changes admitted to
    // the host's mirror of the workspace's document.
    let held = crate::common::confined(
        app,
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![kept.save()],
        },
    )
    .expect("an older record opens in its worker");
    held.apply(&DocumentMutation::Snapshot(kept.save()))
        .expect("the workspace's older document merges");
    let Some(DocumentMutation::Incremental(migration)) = held
        .take_mutation()
        .expect("the worker's migration is admitted to the host's mirror")
    else {
        panic!("the migration is sent");
    };
    let mut workspace = kept.clone();
    tilo_automerge::ApplyIncoming::load_incoming(&mut workspace, &migration).unwrap();
    assert_eq!(stored_version(&workspace), current);
}

#[test]
fn the_app_worker_cannot_read_the_hosts_state_directory() {
    let state = tempfile::tempdir().unwrap();
    let agents = state.path().join("agents.json");
    std::fs::write(&agents, br#"{"agents":[{"key":"secret"}]}"#).unwrap();
    let report = probe_confinement(&agents).expect("a worker starts and confines itself");
    assert_eq!(
        report,
        ProbeReport::default(),
        "a confined worker opens no file, creates none, reaches no network and starts nothing"
    );
    assert!(
        !state.path().join("agents.probe").exists(),
        "the worker created a file beside the host's state"
    );
}

/// A record a host creates under the workspace's default route is the record
/// every member's first window opens: its genesis names that id, not one
/// of its own, or a member's store refuses it as not matching its binding.
/// Only an app whose windows each show a resource of their own (the suite's
/// document app) keeps such records; Scratchpad keeps the workspace's one
/// record, its package primary.
#[test]
fn a_confined_record_created_under_a_workspace_route_names_that_route() {
    assert_seed_route(DOCUMENT, ui::resource::DocumentId::workspace_default());
}

#[test]
fn a_confined_workspace_record_is_created_under_the_package_primary_route() {
    assert_seed_route(
        "so.tilo.scratchpad",
        ui::resource::DocumentId::package_primary(),
    );
}

fn assert_seed_route(app: &str, default: ui::resource::DocumentId) {
    let catalogue = catalogue();
    let source = crate::common::confined(
        app,
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::NewAt(default.clone()),
    )
    .unwrap();
    assert_eq!(source.document_id(), &default);
    let Some(DocumentMutation::Snapshot(genesis)) = source.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let doc = automerge::AutoCommit::load(&genesis).unwrap();
    let kind = catalogue.get(app).unwrap().resource_type.clone();
    if default.is_package_primary() {
        // A package primary uses its store namespace, not a minted identity.
        assert_eq!(ui::resource::identity(&doc), None);
    } else {
        assert_eq!(
            ui::resource::workspace_scope(&doc).as_deref(),
            Some(WORKSPACE)
        );
        assert_eq!(
            ui::resource::identity(&doc),
            Some(ui::resource::DocumentCheckout::new(default, kind))
        );
    }
    // Both route shapes must reopen through the actual confined factory.
    crate::common::confined(
        app,
        &MemberId::person("member"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: source.document_id().clone(),
            changes: vec![genesis],
        },
    )
    .unwrap();
}

fn edit(text: &str) -> serde_json::Map<String, serde_json::Value> {
    crate::common::scratchpad_body(text).1
}

#[test]
fn a_confined_record_is_edited_in_its_worker_and_converges_with_a_page() {
    let app = "so.tilo.scratchpad";
    let note = crate::common::scratchpad_body("").0;
    let mut page = crate::common::open(
        app,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let agent = crate::common::confined(
        app,
        &MemberId::agent("writer"),
        &ReplicaActor::Agent {
            person: MemberId::person("writer"),
            name: String::new(),
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![snapshot],
        },
    )
    .unwrap();
    let worker = agent.worker_id().expect("the record is held in a worker");
    assert_ne!(
        worker,
        std::process::id(),
        "the app runs in another process"
    );

    assert!(
        agent.survey().iter().any(|action| action.address == note),
        "the worker publishes the record's operations"
    );
    agent
        .invoke(&note, &edit("Drafted in the worker"), false)
        .unwrap();
    let change = agent
        .take_mutation()
        .unwrap()
        .expect("the edit is the agent's own, admitted change");
    assert!(matches!(change, DocumentMutation::Incremental(_)));
    page.apply(&change).unwrap();
    assert!(
        page.body().contains("Drafted in the worker"),
        "{}",
        page.body()
    );
    assert!(
        agent.body().contains("Drafted in the worker"),
        "the host's admitted copy holds what it would sign"
    );
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "nothing is taken twice"
    );

    // And back: the page's edit reaches the worker, and is never taken as
    // the agent's own.
    page.invoke(&note, &edit("Ada's revision"), false).unwrap();
    agent
        .apply(&page.take_mutation().unwrap().unwrap())
        .unwrap();
    assert!(agent.body().contains("Ada's revision"), "{}", agent.body());
    assert!(agent.take_mutation().unwrap().is_none());
}

#[test]
fn bytes_that_are_not_automerge_never_reach_the_worker() {
    let agent = crate::common::confined(
        "so.tilo.scratchpad",
        &MemberId::agent("writer"),
        &ReplicaActor::Agent {
            person: MemberId::person("writer"),
            name: String::new(),
        },
        Seed::New,
    )
    .unwrap();
    let worker = agent.worker_id();
    assert!(
        agent
            .apply(&DocumentMutation::Incremental(b"not automerge".to_vec()))
            .is_err(),
        "the host refuses the bytes"
    );
    assert_eq!(agent.worker_id(), worker, "the worker never saw them");
    assert!(matches!(
        agent.take_mutation().unwrap(),
        Some(DocumentMutation::Snapshot(_))
    ));
}

#[test]
fn a_dead_worker_is_replaced_from_what_the_host_holds() {
    let note = crate::common::scratchpad_body("").0;
    let agent = crate::common::confined(
        "so.tilo.scratchpad",
        &MemberId::agent("writer"),
        &ReplicaActor::Agent {
            person: MemberId::person("writer"),
            name: String::new(),
        },
        Seed::New,
    )
    .unwrap();
    agent.take_mutation().unwrap();
    agent
        .invoke(&note, &edit("Before the crash"), false)
        .unwrap();
    agent.take_mutation().unwrap().unwrap();
    let first = agent.worker_id().unwrap();
    kill(first);
    // The next request finds the worker gone and is refused; the one after
    // runs in a new worker seeded from the host's admitted copy.
    let _ = agent.survey();
    assert!(
        !agent.survey().is_empty(),
        "a new worker answers for the record"
    );
    let second = agent.worker_id().unwrap();
    assert_ne!(first, second);
    agent
        .invoke(&note, &edit("After the crash"), false)
        .unwrap();
    agent.take_mutation().unwrap().unwrap();
    assert!(agent.body().contains("After the crash"), "{}", agent.body());
}

#[test]
fn records_of_many_workspaces_share_the_pools_workers_and_outlive_one_that_stops() {
    use tilo_agent_session::worker::pool_size;
    let note = crate::common::scratchpad_body("").0;
    // More workspaces than the pool has workers: their records share them.
    let records: Vec<(String, ConfinedReplica)> = (0..pool_size() * 3)
        .map(|index| {
            let workspace = format!("6f1d3a52-1c7e-4b8e-9d2a-{index:012x}");
            let record = ConfinedReplica::open(
                &catalogue(),
                "so.tilo.scratchpad",
                &workspace,
                &MemberId::agent("writer"),
                &ReplicaActor::Agent {
                    person: MemberId::person("writer"),
                    name: String::new(),
                },
                Seed::New,
                false,
            )
            .unwrap();
            record.take_mutation().unwrap();
            record
                .invoke(&note, &edit(&format!("Written in {workspace}")), false)
                .unwrap();
            record.take_mutation().unwrap().unwrap();
            (workspace, record)
        })
        .collect();
    let workers: std::collections::BTreeSet<u32> = records
        .iter()
        .filter_map(|(_, record)| record.worker_id())
        .collect();
    assert!(
        workers.len() <= pool_size(),
        "{} records in {} workers, more than the pool's {}",
        records.len(),
        workers.len(),
        pool_size()
    );
    // Each record holds its own workspace's text, never another's.
    for (workspace, record) in &records {
        let body = record.body();
        assert!(body.contains(&format!("Written in {workspace}")), "{body}");
        assert_eq!(body.matches("Written in").count(), 1, "{body}");
    }
    // A worker that stops takes every slot in it; each record is given
    // again from what the host holds, its text and its next edit intact.
    let (_, first) = &records[0];
    kill(first.worker_id().unwrap());
    let _ = first.survey();
    for (workspace, record) in &records {
        record
            .invoke(&note, &edit(&format!("Again in {workspace}")), false)
            .unwrap();
        record.take_mutation().unwrap().unwrap();
        let body = record.body();
        assert!(body.contains(&format!("Again in {workspace}")), "{body}");
    }
}

pub(crate) fn kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status_locked()
        .unwrap();
    assert!(status.success());
    // Dead once it is gone or a zombie waiting for its parent.
    tilo_test_env::wait_until(format!("worker {pid} to die"), || {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with(['Z', 'X']))
        })
    });
}

/// The appearances the context entries of `change`'s changes name; a
/// snapshot, which an edit never is, names none.
fn appearances(change: &DocumentMutation) -> Vec<Option<tilo_actions::Appearance>> {
    let DocumentMutation::Incremental(bytes) = change else {
        return Vec::new();
    };
    collab_types::parse_changes(bytes)
        .unwrap()
        .iter()
        .filter_map(automerge::Change::message)
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.invoker)
        .map(|invoker| invoker.appearance)
        .collect()
}

#[test]
fn an_appearance_given_to_the_worker_is_recorded_on_the_change() {
    let app = "so.tilo.scratchpad";
    let note = crate::common::scratchpad_body("").0;
    let mut page = crate::common::open(
        app,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let agent = crate::common::confined(
        app,
        &MemberId::agent("writer"),
        &ReplicaActor::Agent {
            person: MemberId::person("writer"),
            name: String::new(),
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![snapshot],
        },
    )
    .unwrap();
    let alice = tilo_actions::Appearance::new("alice", None, None).unwrap();
    agent
        .invoke_appearing(&note, &edit("From Discord"), false, Some(&alice))
        .unwrap();
    let shown = appearances(&agent.take_mutation().unwrap().unwrap());
    assert!(!shown.is_empty(), "the change names its invoker");
    assert!(
        shown.iter().all(|shown| shown.as_ref() == Some(&alice)),
        "{shown:?}"
    );

    // A plain invocation shows none.
    agent.invoke(&note, &edit("Plain"), false).unwrap();
    let shown = appearances(&agent.take_mutation().unwrap().unwrap());
    assert!(!shown.is_empty(), "the change names its invoker");
    assert!(shown.iter().all(Option::is_none), "{shown:?}");
}

const PDF: &str = "so.tilo.pdf-viewer";
/// The document app the suites open, in place of a product app.
const DOCUMENT: &str = test_document_app::APP_ID;

/// A new record of `app` held for an agent, lent `workspace`'s content when
/// there is a workspace.
fn agents_document(app: &str, workspace: Option<&ContentStore>) -> ConfinedReplica {
    let agent = crate::common::confined(
        app,
        &MemberId::agent("painter"),
        &ReplicaActor::Agent {
            person: MemberId::person("painter"),
            name: String::new(),
        },
        Seed::New,
    )
    .unwrap();
    let agent = match workspace {
        Some(workspace) => agent.lending(workspace.clone()),
        None => agent,
    };
    agent.take_mutation().unwrap();
    agent
}

/// The operation `action` of the record `agent` holds, as its survey lists
/// it.
fn offered(agent: &ConfinedReplica, action: &str) -> Option<ActionDescriptor> {
    agent
        .survey()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
}

fn operation(agent: &ConfinedReplica, action: &str) -> ActionDescriptor {
    offered(agent, action).unwrap_or_else(|| {
        let offered: Vec<String> = agent
            .survey()
            .into_iter()
            .map(|descriptor| {
                format!(
                    "{}#{}",
                    descriptor.address.resource, descriptor.address.action
                )
            })
            .collect();
        panic!("the record offers {action}: it offers {offered:?}")
    })
}

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

/// The one piece of content `output` names, as `workspace` keeps it.
fn produced(workspace: &ContentStore, output: &tilo_actions::ActionOutput) -> Vec<u8> {
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = tilo_actions::content_hash(address).expect("a content address");
    workspace
        .read(hash)
        .expect("the workspace keeps what the effect made")
}

/// The width and height a PNG's header gives.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
    assert_eq!(&png[12..16], b"IHDR");
    let side = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (side(16), side(20))
}

fn tagged(descriptor: &ActionDescriptor, tag: &str) -> bool {
    descriptor.contract.tags.iter().any(|held| held == tag)
}

/// An event on the local calendar stays in the record: an agent adds one
/// unasked. One on a remote calendar is queued for its server, which its
/// person is asked about first.
#[test]
fn an_agent_adds_a_local_event_unasked_and_is_asked_for_a_remote_one() {
    let calendar = agents_document("so.tilo.calendar", None);
    let new_event = operation(&calendar, "new-event");
    let event = |calendar_id: &str| {
        arguments(serde_json::json!({"event": {
            "title": "Runner check-in", "start": "2026-11-28T07:30",
            "end": "2026-11-28T08:30", "calendar_id": calendar_id,
        }}))
    };
    calendar
        .invoke(&new_event.address, &event(""), false)
        .expect("a local event is added unasked");
    assert!(calendar.take_mutation().unwrap().is_some());
    match calendar.invoke(&new_event.address, &event("work@caldav"), false) {
        Err(ApiError::ConfirmationRequired | ApiError::Unavailable { .. }) => {}
        other => panic!("an event for a server is not added unasked: {other:?}"),
    }
}

/// Placing a picture the workspace keeps is an ordinary edit of the document:
/// an agent is offered it, and it runs unasked.
/// What the changes of `change` did, as their history names it.
fn did(change: &DocumentMutation) -> Vec<String> {
    let DocumentMutation::Incremental(bytes) = change else {
        return vec!["the whole record".into()];
    };
    collab_types::parse_changes(bytes)
        .unwrap()
        .iter()
        .filter_map(automerge::Change::message)
        .map(|message| {
            match tilo_actions::ActionContext::from_change_message(message)
                .and_then(|context| context.what)
            {
                Some(tilo_actions::Did::Verb(verb)) => verb.action.action,
                _ => message.chars().take(80).collect(),
            }
        })
        .collect()
}

/// Pin the PDF `reference` the workspace holds as `agent`'s PDF record's
/// source, and let its view open it.
fn pin_pdf(agent: &ConfinedReplica, reference: &str, name: &str) {
    let pin = operation(agent, "pin-source");
    assert_eq!(pin.kind, OperationKind::Document);
    agent
        .invoke(
            &pin.address,
            &arguments(serde_json::json!({
                "source": tilo_actions::content_resource(reference),
                "name": name,
            })),
            false,
        )
        .expect("the record pins the workspace's PDF");
    // The pin answers once the record names the PDF: it does not wait for
    // the view to open it.
    let pinned = agent
        .take_mutation()
        .unwrap()
        .expect("the pin is the agent's change");
    assert_eq!(did(&pinned), ["pin-source"]);
    // The view opens the PDF the record names when it is next read, and the
    // record takes on what opening it adds the first time (its page plan,
    // the markup the file carries) in that frame: the app's own work, which
    // whoever reads the view sees whole.
    let _ = agent.survey();
    let opened = agent
        .take_mutation()
        .unwrap()
        .expect("opening the PDF adds to the record");
    let mut opening = did(&opened);
    opening.sort();
    assert_eq!(
        opening,
        ["adopt-existing", "start-page-plan"],
        "{opening:?}"
    );
}

/// A PDF the workspace holds is pinned in an agent's PDF record and its text
/// read page by page, extracted inside the confined worker from the bytes
/// the workspace lent it: no file, network or helper process.
#[test]
fn an_agent_reads_a_pdf_the_workspace_holds_in_its_worker() {
    // Sealed, as the host keeps what is put in a workspace.
    let workspace = ContentStore::memory().sealing();
    let reference = workspace
        .put(&pdf(&[
            "Lamps in the hall",
            "Lamps in the attic",
            "Lamps in the cellar",
        ]))
        .unwrap();
    let agent = agents_document(PDF, Some(&workspace));
    assert_ne!(agent.worker_id(), Some(std::process::id()));
    pin_pdf(&agent, &reference, "inventory.pdf");

    let read = operation(&agent, "read-text");
    assert_eq!(read.kind, OperationKind::Effect);
    assert!(tilo_agent_session::apps::readable(&read), "{read:?}");
    let output = agent
        .invoke(
            &read.address,
            &arguments(serde_json::json!({ "from_page": 2, "to_page": 2 })),
            false,
        )
        .expect("read-text runs in the worker for the agent");
    let value = output.value.expect("read-text answers");
    assert_eq!(value["pages"][0]["page"], 2, "{value}");
    assert!(
        value["pages"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Lamps in the attic")),
        "{value}"
    );
    let changed = agent.take_mutation().unwrap();
    assert!(
        changed.is_none(),
        "a read changes nothing in the record: {:?}",
        changed.as_ref().map(did)
    );

    // Another worker on the same record, lent the workspace, asks for the PDF
    // it lacks and reads it; one lent nothing says it cannot.
    let open = |lent: Option<&ContentStore>| {
        let replica = crate::common::confined(
            PDF,
            &MemberId::agent("reader"),
            &ReplicaActor::Agent {
                person: MemberId::person("reader"),
                name: String::new(),
            },
            Seed::Existing {
                record: agent.document_id().clone(),
                changes: vec![agent.read(|doc| doc.clone().save()).unwrap()],
            },
        )
        .unwrap();
        match lent {
            Some(workspace) => replica.lending(workspace.clone()),
            None => replica,
        }
    };
    let whole = serde_json::Map::new();
    let again = open(Some(&workspace))
        .invoke(&read.address, &whole, false)
        .expect("the PDF the workspace holds reaches the other worker");
    assert!(
        again
            .value
            .unwrap()
            .to_string()
            .contains("Lamps in the cellar")
    );
    assert!(open(None).invoke(&read.address, &whole, false).is_err());
}

/// A page of a PDF the workspace holds is drawn as a picture inside the
/// confined worker and kept as content the workspace keeps.
#[test]
fn an_agent_looks_at_a_pdf_page_in_its_worker() {
    // Sealed, as the host keeps what is put in a workspace.
    let workspace = ContentStore::memory().sealing();
    let reference = workspace
        .put(&pdf(&["Floor plan: hall", "Floor plan: attic"]))
        .unwrap();
    let agent = agents_document(PDF, Some(&workspace));
    pin_pdf(&agent, &reference, "plan.pdf");

    let look = operation(&agent, "look-page");
    assert!(tilo_agent_session::apps::readable(&look), "{look:?}");
    let looked = agent
        .invoke(
            &look.address,
            &arguments(serde_json::json!({ "page": 2, "size": 512 })),
            false,
        )
        .expect("look-page draws in the worker for the agent");
    let (width, height) = png_size(&produced(&workspace, &looked));
    assert_eq!(height, 512, "the page's long side");
    assert!(width < height, "a portrait page: {width}x{height}");
    let value = looked.value.unwrap();
    assert_eq!(value["page"], 2, "{value}");
    let changed = agent.take_mutation().unwrap();
    assert!(
        changed.is_none(),
        "a look changes nothing: {:?}",
        changed.as_ref().map(did)
    );
}

/// A document the workspace holds as content is read with the document app's
/// read-content, in the worker, from the bytes the workspace lent it.
#[test]
fn an_agent_reads_a_document_the_workspace_holds_with_the_document_app() {
    let workspace = ContentStore::memory();
    let hash = workspace
        .put(b"# Lamps\n\nThe cellar lamp flickers.\n")
        .unwrap();
    let agent = agents_document(DOCUMENT, Some(&workspace));
    let read = operation(&agent, "read-content");
    assert!(tilo_agent_session::apps::readable(&read), "{read:?}");
    let output = agent
        .invoke(
            &read.address,
            &arguments(serde_json::json!({
                "content": tilo_actions::content_resource(&hash).unwrap(),
                "name": "lamps.md",
            })),
            false,
        )
        .expect("read-content runs for the agent");
    let text = output
        .value
        .map(|value| value.to_string())
        .unwrap_or_default();
    assert!(text.contains("The cellar lamp flickers"), "{text}");
    assert!(agent.take_mutation().unwrap().is_none());
}

