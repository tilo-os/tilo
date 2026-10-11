//! App code the host runs for agents and room actions runs in confined
//! worker processes, out of reach of the host's state and keys, and what a
//! worker returns is admitted before the host would sign it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the PNG and zip \
              helpers read and write fixed-layout bytes"
)]

mod common;

// Records are held in confined app workers (this binary, started again);
// their changes are admitted through the Automerge isolate.
tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use common::{Body as _, catalogue};
use tilo_actions::OperationKind;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_agent_participant::worker::{ProbeReport, probe_confinement};
use tilo_agent_participant::{ConfinedReplica, Replica, Seed};
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

fn edit(text: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), text.into());
    arguments
}

#[test]
fn a_confined_record_is_edited_in_its_worker_and_converges_with_a_page() {
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let note = ActionAddress::new("scratchpad.note", "edit");
    let mut page = Replica::open(&catalogue, app, ROOM, "ada", false, Seed::New).unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation() else {
        panic!("a new record is published whole");
    };
    let agent = ConfinedReplica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        true,
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
        true,
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
    let note = ActionAddress::new("scratchpad.note", "edit");
    let agent = ConfinedReplica::open(
        &catalogue,
        "so.tilo.scratchpad",
        ROOM,
        "writer",
        true,
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

const CANVAS: &str = "so.tilo.canvas";

/// Canvas's "Open picture" on the record `agent` holds.
fn open_picture(agent: &ConfinedReplica) -> ActionAddress {
    agent
        .survey()
        .into_iter()
        .find(|action| action.address.action == "open" && action.action_label == "Open picture")
        .expect("an artwork opens a picture")
        .address
}

fn picture_at(path: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("path".into(), path.into());
    arguments
}

fn new_artwork(content: Option<&ContentStore>) -> ConfinedReplica {
    let agent =
        ConfinedReplica::open(&catalogue(), CANVAS, ROOM, "painter", true, Seed::New).unwrap();
    let agent = match content {
        Some(content) => agent.with_content(content.clone()),
        None => agent,
    };
    agent.take_mutation().unwrap();
    agent
}

#[test]
fn content_the_room_holds_reaches_the_worker_an_operation_names_it_in() {
    let room = ContentStore::memory();
    let hash = room.put(&png(3, 0x80)).unwrap();
    let address = tilo_actions::content_resource(&hash).unwrap();

    let unlent = new_artwork(None);
    let open = open_picture(&unlent);
    assert!(
        unlent.invoke(&open, &picture_at(&address), false).is_err(),
        "a worker lent no store has nothing at the address"
    );

    let agent = new_artwork(Some(&room));
    agent
        .invoke(&open, &picture_at(&address), false)
        .expect("the worker reads the room's content at the address");
    agent.take_mutation().unwrap().unwrap();
    assert!(
        agent.body().contains(&hash),
        "the artwork's picture is the room's content: {}",
        agent.body()
    );

    // A worker that replaces a dead one is given what the last one held.
    kill(agent.worker_id().unwrap());
    let _ = agent.survey();
    agent
        .invoke(&open, &picture_at(&address), false)
        .expect("the new worker holds the content too");
    assert!(agent.body().contains(&hash), "{}", agent.body());
}

#[test]
fn content_an_app_stores_in_its_worker_is_kept_by_the_room() {
    use base64::Engine as _;
    let room = ContentStore::memory();
    let picture = png(2, 0x40);
    let hash = tilo_content::hash_bytes(&picture);
    let inline = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&picture)
    );
    let agent = new_artwork(Some(&room));
    agent
        .invoke(&open_picture(&agent), &picture_at(&inline), false)
        .unwrap();
    agent.take_mutation().unwrap().unwrap();
    assert!(agent.body().contains(&hash), "{}", agent.body());
    assert_eq!(
        room.read(&hash).as_deref(),
        Some(picture.as_slice()),
        "the picture the worker stored is the room's now"
    );
}

/// The effect `action` of the record `agent` holds, as an agent's survey
/// lists it.
fn effect(agent: &ConfinedReplica, action: &str) -> Option<ActionDescriptor> {
    agent
        .survey_with_effects()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
}

fn device_only(descriptor: &ActionDescriptor) -> bool {
    descriptor
        .contract
        .tags
        .iter()
        .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY)
}

#[test]
fn an_agent_sees_the_effects_that_only_read_its_record() {
    let agent = new_artwork(None);
    let export = effect(&agent, "export").expect("an agent sees the canvas's export");
    assert_eq!(export.kind, OperationKind::Effect);
    assert!(
        export.params.iter().all(|param| param.name != "path"),
        "an agent is offered no path to write to: {:?}",
        export.params
    );
    assert!(
        effect(&agent, "find-area").is_none(),
        "an effect that writes the record is not offered"
    );
    let offered = agent.survey_with_effects();
    assert!(offered.iter().all(|descriptor| !device_only(descriptor)));
    assert!(
        agent
            .survey()
            .iter()
            .all(|descriptor| descriptor.kind == OperationKind::Document),
        "the document survey stays document operations only"
    );
    assert!(
        offered
            .iter()
            .any(|descriptor| descriptor.kind == OperationKind::Document),
        "the record's document operations are still listed"
    );

    // A record that is not an agent's lists no effects.
    let room = ConfinedReplica::open(&catalogue(), CANVAS, ROOM, "room", false, Seed::New).unwrap();
    assert!(
        room.survey_with_effects()
            .iter()
            .all(|descriptor| descriptor.kind == OperationKind::Document)
    );

    // Effects that reach a person's device are never an agent's to run:
    // the calendar's sync and file export are neither offered nor run.
    let calendar = ConfinedReplica::open(
        &catalogue(),
        "so.tilo.calendar",
        ROOM,
        "planner",
        true,
        Seed::New,
    )
    .unwrap();
    let offered = calendar.survey_with_effects();
    assert!(offered.iter().all(|descriptor| !device_only(descriptor)));
    let calendar_kind = catalogue()
        .get("so.tilo.calendar")
        .unwrap()
        .record_kind
        .as_str()
        .to_owned();
    for device in ["sync", "export-ics", "import-ics"] {
        assert!(
            offered
                .iter()
                .all(|descriptor| descriptor.address.action != device),
            "{device} is offered"
        );
        let address = ActionAddress::new(calendar_kind.as_str(), device);
        match calendar.invoke(&address, &serde_json::Map::new(), true) {
            Err(DoorError::Unavailable { code, .. }) => assert_eq!(code, "local_device_only"),
            other => panic!("{device} is refused to an agent: {other:?}"),
        }
    }
}

#[test]
fn an_agent_exports_its_artwork_as_content_the_room_keeps() {
    let room = ContentStore::memory();
    let hash = room.put(&png(8, 0x80)).unwrap();
    let address = tilo_actions::content_resource(&hash).unwrap();
    let agent = new_artwork(Some(&room));
    agent
        .invoke(&open_picture(&agent), &picture_at(&address), false)
        .unwrap();
    agent.take_mutation().unwrap().unwrap();

    let export = effect(&agent, "export").unwrap().address;
    let output = agent
        .invoke(&export, &serde_json::Map::new(), false)
        .expect("the worker exports the artwork");
    let exported = output
        .resources
        .iter()
        .find_map(|resource| tilo_actions::content_hash(resource))
        .unwrap_or_else(|| panic!("the output names the picture's content: {output:?}"));
    let picture = room
        .read(exported)
        .expect("the room keeps the exported picture");
    assert_eq!(&picture[..8], b"\x89PNG\r\n\x1a\n", "it is a PNG");
    assert_eq!(&picture[12..16], b"IHDR");
    let side = |at: usize| u32::from_be_bytes(picture[at..at + 4].try_into().unwrap());
    assert!(
        side(16) >= 8 && side(16) == side(20),
        "the square artwork, at least the picture's size: {}x{}",
        side(16),
        side(20)
    );
    assert_eq!(tilo_content::hash_bytes(&picture), exported);
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "an export changes nothing in the record"
    );

    // After the record moves on, the effect runs on what it holds now.
    agent
        .invoke(&open_picture(&agent), &picture_at(&address), false)
        .unwrap();
    agent.take_mutation().unwrap();
    let again = agent
        .invoke(&export, &serde_json::Map::new(), false)
        .expect("the component is mounted again");
    assert!(
        again
            .resources
            .iter()
            .any(|resource| tilo_actions::content_hash(resource).is_some())
    );
}

#[test]
fn an_agent_may_not_write_where_an_effect_would_or_run_one_that_edits() {
    let state = tempfile::tempdir().unwrap();
    let target = state.path().join("export.png");
    let agent = new_artwork(Some(&ContentStore::memory()));
    let export = effect(&agent, "export").unwrap().address;
    let mut arguments = serde_json::Map::new();
    arguments.insert("path".into(), target.to_string_lossy().into_owned().into());
    match agent.invoke(&export, &arguments, false) {
        Err(DoorError::Unavailable { code, .. }) => assert_eq!(code, "effect_path"),
        other => panic!("an export to a path is refused: {other:?}"),
    }
    assert!(!target.exists());

    let find = ActionAddress {
        action: "find-area".into(),
        ..export
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert("subject".into(), true.into());
    match agent.invoke(&find, &arguments, false) {
        Err(DoorError::Unavailable { code, .. }) => assert_eq!(code, "effect_writes"),
        other => panic!("an effect that edits is refused: {other:?}"),
    }
}

#[test]
fn an_agent_reads_its_artwork_through_the_canvass_views() {
    let room = ContentStore::memory();
    let picture = ui::png::encode_rgba8(120, 40, &vec![0x80; 120 * 40 * 4]).unwrap();
    let address = tilo_actions::content_resource(&room.put(&picture).unwrap()).unwrap();
    let agent = new_artwork(Some(&room));
    let inspect = effect(&agent, "inspect").expect("an agent's survey lists the canvas's inspect");
    assert_eq!(inspect.kind, OperationKind::View);
    let look = effect(&agent, "look").expect("and its look");
    assert_eq!(look.kind, OperationKind::View);
    assert!(
        agent
            .survey()
            .iter()
            .all(|descriptor| descriptor.address != inspect.address),
        "the document survey stays document operations only"
    );
    agent
        .invoke(&open_picture(&agent), &picture_at(&address), false)
        .unwrap();
    agent.take_mutation().unwrap().unwrap();

    let inspected = agent
        .invoke(&inspect.address, &serde_json::Map::new(), false)
        .expect("inspect runs in the mounted component for the agent");
    let value = inspected.value.clone().expect("inspect answers");
    assert_eq!(value["artwork"]["width"].as_f64(), Some(120.0), "{value}");
    assert_eq!(value["artwork"]["height"].as_f64(), Some(40.0), "{value}");
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "a view changes nothing in the record"
    );
    // What the next plan is shown: the artwork's size, as inspect found it.
    let seen =
        tilo_agent_participant::planner::observation(CANVAS, &inspect.address, &inspected).unwrap();
    assert!(
        seen.contains(r#""width":120"#) && seen.contains(r#""height":40"#),
        "{seen}"
    );

    // A look renders the artwork as content the room keeps; the answer
    // names it by address beside its dimensions, the image never inlined.
    let looked = agent
        .invoke(&look.address, &serde_json::Map::new(), false)
        .expect("look runs for the agent");
    let [kept] = looked.resources.as_slice() else {
        panic!("one rendered picture: {looked:?}");
    };
    let hash = tilo_actions::content_hash(kept).expect("a content address");
    let rendered = room
        .read(hash)
        .expect("the room keeps the rendered picture");
    assert!(rendered.len() > 100, "a picture: {} bytes", rendered.len());
    let value = looked.value.clone().unwrap();
    assert_eq!(value["content"], kept.as_str(), "{value}");
    assert!(value["width"].as_f64().is_some_and(|width| width > 0.0));
    let seen =
        tilo_agent_participant::planner::observation(CANVAS, &look.address, &looked).unwrap();
    assert!(seen.contains(kept.as_str()), "{seen}");
    assert!(
        seen.len() <= tilo_agent::tools::MOST_RESULT_CHARS + 200,
        "a compact answer, not the picture: {seen}"
    );
    assert!(agent.take_mutation().unwrap().is_none());
}

const NOTES: &str = "so.tilo.notes";
const WRITER: &str = "so.tilo.word-processor";

/// A new record of `app` held for an agent, lent `room`'s content.
fn new_record(app: &str, room: &ContentStore) -> ConfinedReplica {
    let agent = ConfinedReplica::open(&catalogue(), app, ROOM, "carrier", true, Seed::New)
        .unwrap()
        .with_content(room.clone());
    agent.take_mutation().unwrap();
    agent
}

/// The first operation of `agent`'s record named `action`, effects too.
fn operation(agent: &ConfinedReplica, action: &str) -> ActionDescriptor {
    agent
        .survey_with_effects()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
        .unwrap_or_else(|| panic!("the record publishes {action}"))
}

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
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

#[test]
fn effects_a_records_tree_carries_run_in_its_component_for_an_agent() {
    let room = ContentStore::memory();

    // A row's effect: the note's own export, offered on the note.
    let notes = new_record(NOTES, &room);
    let new_page = operation(&notes, "new-page");
    assert_eq!(new_page.kind, OperationKind::Document);
    notes
        .invoke(
            &new_page.address,
            &arguments(serde_json::json!({tilo_actions::PROMPTED_PARAM: "Lamp inventory"})),
            false,
        )
        .expect("a note is created on the replica");
    notes.take_mutation().unwrap().unwrap();
    let export = operation(&notes, "export-markdown-content");
    assert_eq!(export.kind, OperationKind::Effect);
    assert_ne!(
        export.address.resource, new_page.address.resource,
        "the export is the note's, not the notebook's"
    );
    let output = notes
        .invoke(&export.address, &serde_json::Map::new(), false)
        .expect("a row's effect runs in the mounted component");
    let markdown = String::from_utf8(produced(&room, &output)).unwrap();
    assert!(markdown.contains("Lamp inventory"), "{markdown}");
    assert!(
        notes.take_mutation().unwrap().is_none(),
        "an export changes nothing"
    );

    // A document's read, published on the document resource beside its
    // document operations.
    let writer = new_record(WRITER, &room);
    let append = operation(&writer, "append-paragraph");
    writer
        .invoke(
            &append.address,
            &arguments(serde_json::json!({"text": "The lamp is lit"})),
            false,
        )
        .unwrap();
    writer.take_mutation().unwrap().unwrap();
    let read = operation(&writer, "read-text");
    assert_eq!(read.kind, OperationKind::Effect);
    let output = writer
        .invoke(&read.address, &serde_json::Map::new(), false)
        .expect("read-text runs for the agent");
    let text = output
        .value
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(text.contains("The lamp is lit"), "{output:?}");

    // Document operations still run on the replica alone.
    writer
        .invoke(
            &append.address,
            &arguments(serde_json::json!({"text": "Second line"})),
            false,
        )
        .unwrap();
    assert!(writer.take_mutation().unwrap().is_some());
}

#[test]
fn an_effect_is_given_the_content_its_record_refers_to() {
    let room = ContentStore::memory();
    let picture = png(6, 0x50);
    let hash = room.put(&picture).unwrap();

    // One agent places the room's picture in a Writer record.
    let placing = new_record(WRITER, &room);
    let append = operation(&placing, "append-paragraph");
    placing
        .invoke(
            &append.address,
            &arguments(serde_json::json!({"text": "The lamp, pictured"})),
            false,
        )
        .unwrap();
    let insert = operation(&placing, "insert-image");
    placing
        .invoke(
            &insert.address,
            &arguments(serde_json::json!({
                tilo_actions::PROMPTED_PARAM: tilo_actions::content_resource(&hash).unwrap(),
            })),
            false,
        )
        .expect("the picture is placed");
    placing.take_mutation().unwrap().unwrap();
    assert!(placing.body().contains(&hash), "{}", placing.body());

    // Another worker holds the same record and was given no content: its
    // export still carries the picture the record refers to.
    let exporting = ConfinedReplica::open(
        &catalogue(),
        WRITER,
        ROOM,
        "exporter",
        true,
        Seed::Existing {
            record: placing.record_id().clone(),
            changes: vec![placing.read(|doc| doc.clone().save()).unwrap()],
        },
    )
    .unwrap()
    .with_content(room.clone());
    let export = operation(&exporting, "export-content");
    let output = exporting
        .invoke(
            &export.address,
            &arguments(serde_json::json!({"format": "docx"})),
            false,
        )
        .expect("the docx is exported");
    let docx = produced(&room, &output);
    assert!(docx.starts_with(b"PK"), "a .docx is a zip package");
    let media: Vec<(String, u32, usize)> = zip_entries(&docx)
        .into_iter()
        .filter(|(name, ..)| name.starts_with("word/media/"))
        .collect();
    assert!(
        media
            .iter()
            .any(|&(_, crc, size)| crc == crc32(&picture) && size == picture.len()),
        "the placed PNG travels byte for byte: {media:?}"
    );
}

/// Every entry of a zip's central directory: its name, CRC-32 and
/// uncompressed size.
fn zip_entries(zip: &[u8]) -> Vec<(String, u32, usize)> {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes(zip[at..at + 2].try_into().unwrap()));
    let u32_at = |at: usize| u32::from_le_bytes(zip[at..at + 4].try_into().unwrap());
    let mut entries = Vec::new();
    let mut at = 0;
    while at + 46 <= zip.len() {
        if zip[at..at + 4] != [0x50, 0x4b, 0x01, 0x02] {
            at += 1;
            continue;
        }
        let name = u16_at(at + 28);
        let skip = name + u16_at(at + 30) + u16_at(at + 32);
        entries.push((
            String::from_utf8_lossy(&zip[at + 46..at + 46 + name]).into_owned(),
            u32_at(at + 16),
            usize::try_from(u32_at(at + 24)).unwrap(),
        ));
        at += 46 + skip;
    }
    entries
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

/// CRC-32 (IEEE), as PNG chunks and zip entries carry it.
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

fn kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    std::thread::sleep(std::time::Duration::from_millis(200));
}
