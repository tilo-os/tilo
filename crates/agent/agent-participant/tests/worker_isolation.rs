//! App code the host runs for agents and room actions runs in confined
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

mod common;

// Records are held in confined app workers (this binary, started again);
// their changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use common::{Body as _, catalogue, pdf};
use tilo_actions::OperationKind;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_agent_participant::worker::{ProbeReport, probe_confinement};
use tilo_agent_participant::{ConfinedReplica, Replica, ReplicaActor, Seed};
use tilo_content::ContentStore;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

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

/// A record a host creates under the room's default route is the record
/// every member's first window opens: its genesis names that id, not one
/// of its own, or a member's store refuses it as not matching its binding.
/// Only an app whose windows each show a resource of their own (Sheets)
/// keeps such records; Scratchpad keeps the workspace's one record, its
/// package primary, like Notes.
#[test]
fn a_confined_record_created_under_a_room_route_names_that_route() {
    assert_seed_route("so.tilo.sheets", ui::record::RecordId::room_default());
}

#[test]
fn a_confined_room_record_is_created_under_the_package_primary_route() {
    assert_seed_route("so.tilo.notes", ui::record::RecordId::package_primary());
}

fn assert_seed_route(app: &str, default: ui::record::RecordId) {
    let catalogue = catalogue();
    let source = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "host",
        &tilo_agent_participant::ReplicaActor::Owner,
        Seed::NewAt(default.clone()),
    )
    .unwrap();
    assert_eq!(source.record_id(), &default);
    let Some(DocumentMutation::Snapshot(genesis)) = source.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let doc = automerge::AutoCommit::load(&genesis).unwrap();
    let kind = catalogue.get(app).unwrap().record_kind.clone();
    if default.is_package_primary() {
        // A package primary uses its store namespace, not a minted identity.
        assert_eq!(ui::record::identity(&doc), None);
    } else {
        assert_eq!(ui::record::room_scope(&doc).as_deref(), Some(ROOM));
        assert_eq!(
            ui::record::identity(&doc),
            Some(ui::record::RecordCheckout::new(default, kind))
        );
    }
    // Both route shapes must reopen through the actual confined factory.
    ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "member",
        &tilo_agent_participant::ReplicaActor::Owner,
        Seed::Existing {
            record: source.record_id().clone(),
            changes: vec![genesis],
        },
    )
    .unwrap();
}

fn edit(text: &str) -> serde_json::Map<String, serde_json::Value> {
    common::scratchpad_body(text).1
}

#[test]
fn a_confined_record_is_edited_in_its_worker_and_converges_with_a_page() {
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let note = common::scratchpad_body("").0;
    let mut page = Replica::open(
        &catalogue,
        app,
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation() else {
        panic!("a new record is published whole");
    };
    let agent = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        &tilo_agent_participant::ReplicaActor::Agent,
        Seed::Existing {
            record: page.record_id().clone(),
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
    agent.apply(&page.take_mutation().unwrap()).unwrap();
    assert!(agent.body().contains("Ada's revision"), "{}", agent.body());
    assert!(agent.take_mutation().unwrap().is_none());
}

#[test]
fn bytes_that_are_not_automerge_never_reach_the_worker() {
    let catalogue = catalogue();
    let agent = ConfinedReplica::open(
        &catalogue,
        "so.tilo.scratchpad",
        ROOM,
        "writer",
        &tilo_agent_participant::ReplicaActor::Agent,
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
    let catalogue = catalogue();
    let note = common::scratchpad_body("").0;
    let agent = ConfinedReplica::open(
        &catalogue,
        "so.tilo.scratchpad",
        ROOM,
        "writer",
        &tilo_agent_participant::ReplicaActor::Agent,
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

fn kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
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
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let note = common::scratchpad_body("").0;
    let mut page = Replica::open(
        &catalogue,
        app,
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation() else {
        panic!("a new record is published whole");
    };
    let agent = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        &tilo_agent_participant::ReplicaActor::Agent,
        Seed::Existing {
            record: page.record_id().clone(),
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

const CANVAS: &str = "so.tilo.canvas";
const PDF: &str = "so.tilo.pdf-viewer";
const WRITER: &str = "so.tilo.word-processor";
const SHEETS: &str = "so.tilo.sheets";

/// A new record of `app` held for an agent, lent `room`'s content when
/// there is a room.
fn agents_record(app: &str, room: Option<&ContentStore>) -> ConfinedReplica {
    let agent = ConfinedReplica::open(
        &catalogue(),
        app,
        ROOM,
        "painter",
        &ReplicaActor::Agent,
        Seed::New,
    )
    .unwrap();
    let agent = match room {
        Some(room) => agent.lending(room.clone()),
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

/// Canvas's open-picture on `agent`'s artwork, of the picture at `path`.
fn open_picture(
    agent: &ConfinedReplica,
    path: &str,
) -> Result<tilo_actions::ActionOutput, DoorError> {
    let open = operation(agent, "open-picture");
    agent.invoke(
        &open.address,
        &arguments(serde_json::json!({ "path": path })),
        false,
    )
}

/// The one piece of content `output` names, as `room` keeps it.
fn produced(room: &ContentStore, output: &tilo_actions::ActionOutput) -> Vec<u8> {
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = tilo_actions::content_hash(address).expect("a content address");
    room.read(hash)
        .expect("the room keeps what the effect made")
}

/// The width and height a PNG's header gives.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
    assert_eq!(&png[12..16], b"IHDR");
    let side = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (side(16), side(20))
}

#[test]
fn content_the_room_holds_reaches_the_worker_an_operation_names_it_in() {
    let room = ContentStore::memory();
    let hash = room.put(&png(3, 0x80)).unwrap();
    let address = tilo_actions::content_resource(&hash).unwrap();

    let unlent = agents_record(CANVAS, None);
    assert!(
        open_picture(&unlent, &address).is_err(),
        "a worker lent no store has nothing at the address"
    );

    let agent = agents_record(CANVAS, Some(&room));
    open_picture(&agent, &address).expect("the worker reads the room's content at the address");
    agent.take_mutation().unwrap().unwrap();
    assert!(
        agent.body().contains(&hash),
        "the artwork's picture is the room's content: {}",
        agent.body()
    );

    // A worker that replaces a dead one is given what the last one held.
    kill(agent.worker_id().unwrap());
    let _ = agent.survey();
    open_picture(&agent, &address).expect("the new worker holds the content too");
    assert!(agent.body().contains(&hash), "{}", agent.body());
}

#[test]
fn content_an_app_stores_in_its_worker_is_kept_by_the_room() {
    let room = ContentStore::memory();
    let picture = png(2, 0x40);
    let hash = tilo_content::hash_bytes(&picture);
    let inline = format!("data:image/png;base64,{}", base64(&picture));
    let agent = agents_record(CANVAS, Some(&room));
    open_picture(&agent, &inline).unwrap();
    agent.take_mutation().unwrap().unwrap();
    assert!(agent.body().contains(&hash), "{}", agent.body());
    assert_eq!(
        room.read(&hash).as_deref(),
        Some(picture.as_slice()),
        "the picture the worker stored is the room's now"
    );
}

fn tagged(descriptor: &ActionDescriptor, tag: &str) -> bool {
    descriptor.contract.tags.iter().any(|held| held == tag)
}

#[test]
fn an_agent_is_offered_the_effects_and_views_that_answer() {
    let agent = agents_record(CANVAS, None);
    let export = offered(&agent, "export").expect("an agent sees the canvas's export");
    assert_eq!(export.kind, OperationKind::Effect);
    assert!(
        export.params.iter().all(|param| param.name != "path"),
        "an agent is offered no path to write to: {:?}",
        export.params
    );
    let look = offered(&agent, "look").expect("and its look");
    assert_eq!(look.kind, OperationKind::Effect);
    assert!(
        tilo_agent_participant::apps::readable(&look),
        "a look is a read a plan may take: {look:?}"
    );
    let inspect = offered(&agent, "inspect").expect("and its inspect");
    assert_eq!(inspect.kind, OperationKind::View);
    assert!(tilo_agent_participant::apps::readable(&inspect));
    let open = operation(&agent, "open-picture");
    assert_eq!(open.kind, OperationKind::Document);
    assert!(
        !tilo_agent_participant::apps::readable(&open),
        "an edit is no read: {open:?}"
    );
    let offered_all = agent.survey();
    assert!(
        offered_all
            .iter()
            .any(|descriptor| descriptor.kind == OperationKind::Document),
        "the record's document operations are still listed"
    );
    assert!(offered_all.iter().all(|descriptor| {
        !tagged(descriptor, tilo_actions::OWN_DEVICE_SCOPE)
            && !tagged(descriptor, tilo_actions::ATTENDED_TAG)
            && (descriptor.kind != OperationKind::View
                || descriptor.contract.result != tilo_actions::ResultSpec::None)
    }));

    // A record held for the room's owner lists no views.
    let owners = ConfinedReplica::open(
        &catalogue(),
        CANVAS,
        ROOM,
        "room",
        &ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    assert!(
        owners
            .survey()
            .iter()
            .all(|descriptor| descriptor.kind != OperationKind::View)
    );

    // Effects that need a person attending, or answer nothing, are neither
    // offered to an agent nor run for it: the calendar's file import and
    // export, and its account sync.
    let calendar = agents_record("so.tilo.calendar", None);
    let offered_all = calendar.survey();
    let kind = catalogue()
        .get("so.tilo.calendar")
        .unwrap()
        .record_kind
        .as_str()
        .to_owned();
    for (effect, code) in [
        ("export-ics", "attended"),
        ("import-ics", "attended"),
        ("sync", "effect_not_offered"),
    ] {
        assert!(
            offered_all
                .iter()
                .all(|descriptor| descriptor.address.action != effect),
            "{effect} is offered"
        );
        let address = ActionAddress::new(kind.as_str(), effect);
        match calendar.invoke(&address, &serde_json::Map::new(), true) {
            Err(DoorError::Unavailable { code: refused, .. }) => assert_eq!(refused, code),
            other => panic!("{effect} is refused to an agent: {other:?}"),
        }
    }
}

#[test]
fn an_agent_exports_its_artwork_as_content_the_room_keeps() {
    let room = ContentStore::memory();
    let address = tilo_actions::content_resource(&room.put(&png(8, 0x80)).unwrap()).unwrap();
    let agent = agents_record(CANVAS, Some(&room));
    open_picture(&agent, &address).unwrap();
    agent.take_mutation().unwrap().unwrap();

    let export = operation(&agent, "export").address;
    // An export the room keeps (no `path`) is a read: an agent's grants
    // allow it without asking its person.
    let output = agent
        .invoke(&export, &serde_json::Map::new(), false)
        .expect("the worker exports the artwork unasked");
    let picture = produced(&room, &output);
    let (width, height) = png_size(&picture);
    assert!(
        width >= 8 && width == height,
        "the square artwork, at least the picture's size: {width}x{height}"
    );
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "an export changes nothing in the record"
    );

    // Another worker holding the same record, given no content but the
    // room's, exports the same picture: what the record refers to is put
    // into its worker before the export runs.
    let exporting = ConfinedReplica::open(
        &catalogue(),
        CANVAS,
        ROOM,
        "exporter",
        &ReplicaActor::Agent,
        Seed::Existing {
            record: agent.record_id().clone(),
            changes: vec![agent.read(|doc| doc.clone().save()).unwrap()],
        },
    )
    .unwrap()
    .lending(room.clone());
    let again = exporting
        .invoke(&export, &serde_json::Map::new(), false)
        .expect("the other worker exports it");
    assert_eq!(
        produced(&room, &again),
        picture,
        "the picture the record refers to reached the other worker"
    );
}

/// Importing a workbook the room keeps edits the record and reaches nothing
/// outside the room: an agent does it unasked, as it exports one.
#[test]
fn an_agent_imports_a_workbook_the_room_keeps_unasked() {
    let room = ContentStore::memory();
    let source = agents_record(SHEETS, Some(&room));
    let export = operation(&source, "export-xlsx-content");
    let exported = source
        .invoke(&export.address, &serde_json::Map::new(), false)
        .expect("the agent exports the workbook unasked");
    let [address] = exported.resources.as_slice() else {
        panic!("one content resource: {exported:?}");
    };
    // The bare hash, as a step carrying the content gives it.
    let hash = tilo_actions::content_hash(address).unwrap().to_owned();
    let target = agents_record(SHEETS, Some(&room));
    let import = operation(&target, "import-xlsx-content");
    target
        .invoke(
            &import.address,
            &arguments(serde_json::json!({ "content": hash })),
            false,
        )
        .expect("the agent imports the workbook unasked");
}

/// An event on the local calendar stays in the record: an agent adds one
/// unasked. One on a remote calendar is queued for its server, which its
/// person is asked about first.
#[test]
fn an_agent_adds_a_local_event_unasked_and_is_asked_for_a_remote_one() {
    let calendar = agents_record("so.tilo.calendar", None);
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
        Err(DoorError::ConfirmationRequired | DoorError::Unavailable { .. }) => {}
        other => panic!("an event for a server is not added unasked: {other:?}"),
    }
}

/// Placing a picture the room keeps is an ordinary edit of the document:
/// an agent is offered it, and it runs unasked, under the heading named.
#[test]
fn an_agent_places_a_picture_the_room_keeps_under_a_heading() {
    let room = ContentStore::memory();
    // The address an export answered with, as `tilo.content:latest` binds.
    let hash = room.put(&png(8, 0x40)).unwrap();
    let address = tilo_actions::content_resource(&hash).unwrap();
    let writer = agents_record(WRITER, Some(&room));
    let replace = operation(&writer, "replace-body");
    writer
        .invoke(
            &replace.address,
            &arguments(serde_json::json!({
                "markdown": "# Summary\nSee the results below.\n# Results\nNumbers by region"
            })),
            false,
        )
        .expect("the agent writes the document");
    writer.take_mutation().unwrap().unwrap();
    let place = offered(&writer, "place-image").expect("an agent is offered place-image");
    writer
        .invoke(
            &place.address,
            &arguments(serde_json::json!({ "content": address, "after": "Results" })),
            false,
        )
        .expect("the agent places the picture unasked");
    assert!(
        writer.take_mutation().unwrap().is_some(),
        "placing the picture edits the document"
    );
    let body = writer
        .read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
        .unwrap();
    // Where it goes under the heading is the word processor's own test.
    assert!(
        body.contains(&hash),
        "the document holds the picture: {body}"
    );
}

#[test]
fn an_agent_may_not_write_where_an_effect_would_or_run_one_that_answers_nothing() {
    let state = tempfile::tempdir().unwrap();
    let target = state.path().join("export.png");
    let agent = agents_record(CANVAS, Some(&ContentStore::memory()));
    let export = operation(&agent, "export").address;
    let to_path = arguments(serde_json::json!({ "path": target.to_string_lossy() }));
    match agent.invoke(&export, &to_path, true) {
        Err(DoorError::Unavailable { code, .. }) => assert_eq!(code, "effect_path"),
        other => panic!("an export to a path is refused: {other:?}"),
    }
    assert!(!target.exists());

    // Replacing the document from a file answers nothing an agent could
    // use and edits the document out of sight of any plan: refused.
    let writer = agents_record(WRITER, None);
    let kind = catalogue()
        .get(WRITER)
        .unwrap()
        .record_kind
        .as_str()
        .to_owned();
    let open = ActionAddress::new(kind.as_str(), "open-document");
    match writer.invoke(&open, &serde_json::Map::new(), true) {
        Err(DoorError::Unavailable { code, .. }) => assert_eq!(code, "effect_not_offered"),
        other => panic!("an effect that answers nothing is refused: {other:?}"),
    }
    assert!(writer.take_mutation().unwrap().is_none());
}

#[test]
fn an_agent_reads_and_looks_at_its_artwork_through_the_canvass_views() {
    let room = ContentStore::memory();
    let picture = png(40, 0x80);
    let address = tilo_actions::content_resource(&room.put(&picture).unwrap()).unwrap();
    let agent = agents_record(CANVAS, Some(&room));
    open_picture(&agent, &address).unwrap();
    agent.take_mutation().unwrap().unwrap();

    let inspect = operation(&agent, "inspect");
    let inspected = agent
        .invoke(&inspect.address, &serde_json::Map::new(), false)
        .expect("inspect runs in the record's view for the agent");
    let value = inspected.value.expect("inspect answers");
    assert_eq!(value["artwork"]["width"].as_f64(), Some(40.0), "{value}");
    assert_eq!(value["artwork"]["height"].as_f64(), Some(40.0), "{value}");
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "a view changes nothing in the record"
    );

    // A look renders the artwork, on the CPU in the confined worker, as
    // content the room keeps; the answer names it, never inlines it.
    let look = operation(&agent, "look");
    let looked = agent
        .invoke(
            &look.address,
            &arguments(serde_json::json!({ "format": "png", "size": 64 })),
            false,
        )
        .expect("look runs for the agent");
    let rendered = produced(&room, &looked);
    assert_eq!(png_size(&rendered), (64, 64));
    let value = looked.value.clone().expect("a look answers");
    assert_eq!(value["content"], looked.resources[0].as_str(), "{value}");
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "a look changes nothing"
    );
}

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

/// Pin the PDF `hash` the room holds as `agent`'s PDF record's source, and
/// let its view open it.
fn pin_pdf(agent: &ConfinedReplica, room: &ContentStore, hash: &str, name: &str) {
    let pin = operation(agent, "pin-source");
    assert_eq!(pin.kind, OperationKind::Document);
    agent
        .invoke(
            &pin.address,
            &arguments(serde_json::json!({
                "hash": hash,
                "len": room.read(hash).unwrap().len(),
                "name": name,
            })),
            false,
        )
        .expect("the record pins the room's PDF");
    agent
        .take_mutation()
        .unwrap()
        .expect("the pin is the agent's change");
    // The view opens the PDF it now names, and the record takes on what
    // opening it adds the first time (its page plan, the markup the file
    // carries): the app's own work, not a read's.
    let read = operation(agent, "read-text");
    agent
        .invoke(&read.address, &serde_json::Map::new(), false)
        .expect("the pinned PDF opens");
    if let Some(opened) = agent.take_mutation().unwrap() {
        let opening = did(&opened);
        assert!(
            opening
                .iter()
                .all(|verb| verb == "start-page-plan" || verb == "adopt-existing"),
            "{opening:?}"
        );
    }
}

/// A PDF the room holds is pinned in an agent's PDF record and its text
/// read page by page, extracted inside the confined worker from the bytes
/// the room lent it: no file, network or helper process.
#[test]
fn an_agent_reads_a_pdf_the_room_holds_in_its_worker() {
    let room = ContentStore::memory();
    let hash = room
        .put(&pdf(&[
            "Lamps in the hall",
            "Lamps in the attic",
            "Lamps in the cellar",
        ]))
        .unwrap();
    let agent = agents_record(PDF, Some(&room));
    assert_ne!(agent.worker_id(), Some(std::process::id()));
    pin_pdf(&agent, &room, &hash, "inventory.pdf");

    let read = operation(&agent, "read-text");
    assert_eq!(read.kind, OperationKind::Effect);
    assert!(tilo_agent_participant::apps::readable(&read), "{read:?}");
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

    // Another worker on the same record, lent the room, asks for the PDF
    // it lacks and reads it; one lent nothing says it cannot.
    let open = |lent: Option<&ContentStore>| {
        let replica = ConfinedReplica::open(
            &catalogue(),
            PDF,
            ROOM,
            "reader",
            &ReplicaActor::Agent,
            Seed::Existing {
                record: agent.record_id().clone(),
                changes: vec![agent.read(|doc| doc.clone().save()).unwrap()],
            },
        )
        .unwrap();
        match lent {
            Some(room) => replica.lending(room.clone()),
            None => replica,
        }
    };
    let whole = serde_json::Map::new();
    let again = open(Some(&room))
        .invoke(&read.address, &whole, false)
        .expect("the PDF the room holds reaches the other worker");
    assert!(
        again
            .value
            .unwrap()
            .to_string()
            .contains("Lamps in the cellar")
    );
    assert!(open(None).invoke(&read.address, &whole, false).is_err());
}

/// A page of a PDF the room holds is drawn as a picture inside the
/// confined worker and kept as content the room keeps.
#[test]
fn an_agent_looks_at_a_pdf_page_in_its_worker() {
    let room = ContentStore::memory();
    let hash = room
        .put(&pdf(&["Floor plan: hall", "Floor plan: attic"]))
        .unwrap();
    let agent = agents_record(PDF, Some(&room));
    pin_pdf(&agent, &room, &hash, "plan.pdf");

    let look = operation(&agent, "look-page");
    assert!(tilo_agent_participant::apps::readable(&look), "{look:?}");
    let looked = agent
        .invoke(
            &look.address,
            &arguments(serde_json::json!({ "page": 2, "size": 512 })),
            false,
        )
        .expect("look-page draws in the worker for the agent");
    let (width, height) = png_size(&produced(&room, &looked));
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

/// A document the room holds as content is read with the word processor's
/// read-content, in the worker, from the bytes the room lent it.
#[test]
fn an_agent_reads_a_document_the_room_holds_with_the_word_processor() {
    let room = ContentStore::memory();
    let hash = room.put(b"# Lamps\n\nThe cellar lamp flickers.\n").unwrap();
    let agent = agents_record(WRITER, Some(&room));
    let read = operation(&agent, "read-content");
    assert!(tilo_agent_participant::apps::readable(&read), "{read:?}");
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

/// A `side`-pixel square grey PNG, its pixels stored uncompressed.
fn png(side: u8, grey: u8) -> Vec<u8> {
    fn chunk(png: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
        png.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
        let start = png.len();
        png.extend(kind);
        png.extend(data);
        let sum = crc32(&png[start..]);
        png.extend(sum.to_be_bytes());
    }
    let mut rows = Vec::new();
    for _ in 0..side {
        rows.push(0);
        rows.extend(std::iter::repeat_n(grey, usize::from(side)));
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &rows {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    let length = u16::try_from(rows.len()).unwrap();
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend(length.to_le_bytes());
    zlib.extend((!length).to_le_bytes());
    zlib.extend(&rows);
    zlib.extend(((b << 16) | a).to_be_bytes());
    let mut header = Vec::new();
    header.extend(u32::from(side).to_be_bytes());
    header.extend(u32::from(side).to_be_bytes());
    header.extend([8, 0, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, *b"IHDR", &header);
    chunk(&mut png, *b"IDAT", &zlib);
    chunk(&mut png, *b"IEND", &[]);
    png
}

/// `bytes` in standard, padded base64.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for group in bytes.chunks(3) {
        let word = group.iter().enumerate().fold(0u32, |word, (at, &byte)| {
            word | u32::from(byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            if at <= group.len() {
                out.push(char::from(
                    ALPHABET[((word >> (18 - 6 * at)) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// CRC-32 (IEEE), as PNG chunks carry it.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
