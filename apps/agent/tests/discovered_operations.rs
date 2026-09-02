//! Operations the agent was never told about, discovered off the room's own
//! applications rather than a written-down list, and dispatched for real —
//! carried over the compositor's production Cap'n Proto broker. What is
//! under test is [`qos_agent::Session`]'s discovery, dispatch and capability
//! gating while it participates through the same endpoint as an app.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the arithmetic \
              here is wall-clock timeout deadlines bounded by a short-lived \
              test run, not real user data"
)]

use std::time::{Duration, Instant};

use automerge::AutoCommit;
use qos_actions::{Resource, ResourceProvider};
use qos_agent::{Config, Session};
use ui::collaboration::{CollabEvent, CollabRequest, DocumentMutation, PresencePayload};
use uuid::Uuid;

mod typed_peer;
use typed_peer::{TypedPeer as UnixStream, document_mutation, recv, send};

const APP: &str = "dev.qos.word-processor";
const RECORD: &str = "file:/var/workspace/Demo/brief.docx";
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The capability an inviter issues an agent for the app it was invited to.
fn granted(name: &str, app: &str) -> ui::permissions::PermissionContext {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    PermissionContext::from_capabilities(
        Actor::agent(name),
        vec![
            CapOverride::grant(
                Subject::agent(name),
                Scope::App(app.to_string()),
                Verb::Write,
            )
            .with_id("invited"),
        ],
    )
}

/// Nothing granted at all: the agent may enumerate and perform nothing.
fn nothing_granted(name: &str) -> ui::permissions::PermissionContext {
    use ui::permissions::{Actor, PermissionContext};
    PermissionContext::from_capabilities(Actor::agent(name), vec![])
}

/// A title paragraph, an image, and one instruction naming an operation the
/// agent was never told about: word-processor's own "delete image".
fn seed_document_with_image(instruction: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("quinten".as_bytes()));
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let title = dm.insert_block(None, "paragraph");
        dm.splice_text(&title, 0, 0, "Launch plan");
        let img = dm.insert_block(Some(&title), "image");
        let p = dm.insert_block(Some(&img), "paragraph");
        dm.splice_text(&p, 0, 0, instruction);
    }
    doc.commit();
    doc
}

fn blocks(doc: &AutoCommit) -> Vec<qdoc::BlockView> {
    let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
    qdoc::model::materialize(doc, &c).blocks
}

fn word_processor_resources(doc: &AutoCommit) -> Vec<Resource> {
    let b = blocks(doc);
    let caret = b.last().map(|block| block.id.clone()).unwrap_or_default();
    word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: caret.clone(),
            caret_offset: 0,
            target_blocks: (!caret.is_empty()).then_some(caret).into_iter().collect(),
        },
        doc,
    )
}

fn photos_resources(doc: &AutoCommit) -> Vec<Resource> {
    photos::actions::PhotosResources.resources(doc)
}

/// Bring up the relay, seed `replica`, connect a peer under it, and run the
/// agent under `cfg` against that peer — driving `poll` on every incremental
/// change until it says the run is over or `within` runs out.
///
/// Returns the peer's final replica and whether the agent reports having
/// acted.
fn run(
    cfg_of: impl FnOnce(std::path::PathBuf, Uuid) -> Config,
    mut replica: AutoCommit,
    mut poll: impl FnMut(&AutoCommit) -> bool,
    within: Duration,
) -> (AutoCommit, bool, Option<String>) {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("collab.sock");
    let _broker = typed_peer::spawn(&sock);
    let started = Instant::now();
    while !sock.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "relay never bound"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let room = typed_peer::room();
    let mut peer = UnixStream::connect(&sock);
    send(
        &mut peer,
        &CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: Some(RECORD.into()),
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let cfg = cfg_of(sock, room);
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        let until = Instant::now()
            .checked_add(within)
            .expect("test duration does not overflow Instant");
        s.run_until(until).expect("agent session")
    });

    peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now()
        .checked_add(within)
        .expect("test duration does not overflow Instant");
    let mut done = false;
    let mut invocation_actor = None;
    while Instant::now() < deadline && !done {
        let Some(event) = recv(&mut peer) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } => {
                typed_peer::publish_actions(&mut peer, APP, &word_processor_resources(&replica));
                let snap = CollabRequest::DocumentSnapshot {
                    data: replica.save(),
                };
                send(&mut peer, &snap).unwrap();
            }
            CollabEvent::Invocation {
                id,
                resource,
                action,
                argument,
                actor,
            } => {
                let resources = word_processor_resources(&replica);
                let update = typed_peer::service_document_invocation(
                    &mut peer,
                    &mut replica,
                    &resources,
                    word_processor::actions::register,
                    id,
                    resource,
                    action,
                    argument.as_deref(),
                    &actor,
                );
                send(&mut peer, &CollabRequest::UpdateInvocation { id, update }).unwrap();
                invocation_actor = Some(actor);
                done = poll(&replica);
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                replica
                    .load_incremental(&data)
                    .expect("apply the agent's change");
                done = poll(&replica);
            }
            _ => {}
        }
    }
    let acted = agent.join().unwrap();
    (replica, acted, invocation_actor)
}

/// **The core claim of discovery.** An operation nowhere written down in
/// the agent — Word Processor's own "insert table" — is found, performed,
/// and attributed to the agent's own actor, exactly as a catalogue action
/// is.
///
/// It also proves the re-entrancy guard: the instruction names the same
/// operation twice, but one repeated clause still resolves to one operation.
#[test]
fn a_discovered_operation_is_performed_once_under_the_agents_own_actor() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let replica = seed_document_with_image("Scribe: insert a table, then insert a table again");
    let (mut replica, acted, invocation_actor) = run(
        |sock, room| Config {
            endpoint: Some(sock),
            endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
            app_id: APP.into(),
            cluster: room,
            record: Some(RECORD.into()),
            name: "Scribe".into(),
            pace: Duration::from_millis(10),
            once: true,
            model: false,
            model_timeout: Duration::from_secs(1),
            await_document: Duration::ZERO,
            settle: Duration::ZERO,
            directory: None,
            permissions: granted("Scribe", "word-processor"),
        },
        replica,
        |doc| {
            let b = blocks(doc);
            let title_done = b.iter().any(|x| x.text == "Launch plan");
            let one_table = b.iter().filter(|x| x.kind == "cell").count() == 4;
            let answered = !b.iter().any(|x| x.text.starts_with("Scribe:"));
            title_done && one_table && answered
        },
        Duration::from_secs(15),
    );
    let shown: Vec<(String, String)> = blocks(&replica)
        .iter()
        .map(|b| (b.kind.clone(), b.text.clone()))
        .collect();
    assert!(acted, "the agent reports having acted");

    let b = blocks(&replica);
    assert!(b.iter().any(|x| x.text == "Launch plan"), "{shown:?}");
    assert_eq!(
        b.iter().filter(|x| x.kind == "cell").count(),
        4,
        "the repeated request must insert exactly one 2x2 table: {shown:?}"
    );
    assert!(
        !b.iter().any(|x| x.text.starts_with("Scribe:")),
        "the instruction was left holding a refusal instead of being answered: {shown:?}"
    );

    // The app-hosted action is attributed to the authenticated broker actor;
    // the agent's local acknowledgement edit may retain its display actor.
    let genesis = qdoc::model::schema_genesis_actor_hex();
    let seed = hex::encode("quinten");
    let actors: std::collections::HashSet<String> = qdoc::history::versions(&mut replica)
        .into_iter()
        .map(|v| v.actor_hex)
        .filter(|a| *a != genesis && *a != seed)
        .collect();
    let invocation_actor = invocation_actor.expect("the app received a broker invocation");
    assert!(
        actors.contains(&hex::encode(&invocation_actor)),
        "the discovered operation must carry the broker-authenticated agent actor \
         {invocation_actor}: {actors:?}"
    );
}

/// **Holding a second application's document.** The room carries a
/// word-processor page *and* a photograph nobody told the agent about; the
/// instruction names an operation only the photograph publishes. Answering
/// it needs the peer to have been attributed, recognised and merged under
/// its own contract ([`qos_agent::apply::attribute`],
/// [`qos_agent::apply::Editor::merge_full_any`]) — this is the live path
/// that gives them one.
/// Connect the word-processor peer to `sock`, announce it, and seed its
/// replica with the instruction naming a discoverable operation.
fn connect_word_processor(sock: &std::path::Path, room: Uuid) -> (UnixStream, AutoCommit) {
    let mut wp_peer = UnixStream::connect(sock);
    send(
        &mut wp_peer,
        &CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: Some(RECORD.into()),
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    let wp_replica = seed_document_with_image("Scribe: rotate the photograph right");
    (wp_peer, wp_replica)
}

/// Connect the photo-editor peer to `sock` under the same room, announce it,
/// and seed its replica with one open image — a second application's
/// document the agent was never told about.
fn connect_photo_editor(sock: &std::path::Path, room: Uuid) -> (UnixStream, AutoCommit) {
    let mut photo_peer = UnixStream::connect(sock);
    send(
        &mut photo_peer,
        &CollabRequest::Hello {
            app_id: "dev.qos.photos".into(),
            cluster_id: room,
            record: None,
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    let mut photo_replica = AutoCommit::new();
    photo_replica.set_actor(automerge::ActorId::from("photos-peer".as_bytes()));
    photos::doc::PhotosDoc::seed(&mut photo_replica);
    photos::doc::PhotosDoc::set_image(
        &mut photo_replica,
        Some(&photos::OpenImage::from_path("photo.png", 40, 24)),
    );
    photo_replica.commit();
    (photo_peer, photo_replica)
}

/// The capability the agent needs to touch both applications in this room.
fn word_processor_and_photos_permissions() -> ui::permissions::PermissionContext {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    PermissionContext::from_capabilities(
        Actor::agent("Scribe"),
        vec![
            CapOverride::grant(
                Subject::agent("Scribe"),
                Scope::App("word-processor".to_string()),
                Verb::Write,
            )
            .with_id("wp"),
            CapOverride::grant(
                Subject::agent("Scribe"),
                Scope::App("photos".to_string()),
                Verb::Write,
            )
            .with_id("photos"),
        ],
    )
}

/// Drive the photo peer's side of the exchange: answer the agent's presence
/// with a snapshot, signal `photo_ready_tx` once that snapshot is sent, and
/// stop once the discovered rotate lands or the 15s deadline passes.
fn drive_photo_peer(
    mut photo_peer: UnixStream,
    mut photo_replica: AutoCommit,
    photo_ready_tx: &std::sync::mpsc::SyncSender<()>,
) -> AutoCommit {
    photo_peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut signalled = false;
    while Instant::now() < deadline {
        let Some(event) = recv(&mut photo_peer) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } => {
                typed_peer::publish_actions(
                    &mut photo_peer,
                    "dev.qos.photos",
                    &photos_resources(&photo_replica),
                );
                let snap = CollabRequest::DocumentSnapshot {
                    data: photo_replica.save(),
                };
                send(&mut photo_peer, &snap).unwrap();
                if !signalled {
                    let _ = photo_ready_tx.send(());
                    signalled = true;
                }
            }
            CollabEvent::Invocation {
                id,
                resource,
                action,
                argument,
                actor,
            } => {
                let resources = photos_resources(&photo_replica);
                let update = typed_peer::service_document_invocation(
                    &mut photo_peer,
                    &mut photo_replica,
                    &resources,
                    photos::actions::register,
                    id,
                    resource,
                    action,
                    argument.as_deref(),
                    &actor,
                );
                send(
                    &mut photo_peer,
                    &CollabRequest::UpdateInvocation { id, update },
                )
                .unwrap();
                if photos::doc::PhotosDoc::edits(&photo_replica).rotate_quarters != 0 {
                    break;
                }
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                let _ = photo_replica.load_incremental(&data);
                if photos::doc::PhotosDoc::edits(&photo_replica).rotate_quarters != 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    photo_replica
}

/// Drive the word-processor peer's side: wait for the photo peer's own
/// `Arrived` handshake before answering (pins reply order so `offers` is
/// stable when the agent acts), then answer with a snapshot and watch for
/// the instruction to be resolved.
fn drive_word_processor_peer(
    mut wp_peer: UnixStream,
    mut wp_replica: AutoCommit,
    photo_ready_rx: &std::sync::mpsc::Receiver<()>,
) -> (AutoCommit, bool) {
    wp_peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut done = false;
    while Instant::now() < deadline && !done {
        let Some(event) = recv(&mut wp_peer) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } => {
                photo_ready_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the photo peer never answered its own Arrived");
                // `send` queues the photo snapshot asynchronously. Let the
                // broker deliver it before revealing the instruction.
                std::thread::sleep(Duration::from_millis(250));
                typed_peer::publish_actions(
                    &mut wp_peer,
                    APP,
                    &word_processor_resources(&wp_replica),
                );
                let snap = CollabRequest::DocumentSnapshot {
                    data: wp_replica.save(),
                };
                send(&mut wp_peer, &snap).unwrap();
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                if wp_replica.load_incremental(&data).is_err() {
                    continue;
                }
                done = !blocks(&wp_replica)
                    .iter()
                    .any(|x| x.text.starts_with("Scribe:"));
            }
            _ => {}
        }
    }
    (wp_replica, done)
}

#[test]
fn a_discovered_operation_reaches_a_second_application_held_as_a_peer() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("collab.sock");
    let _broker = typed_peer::spawn(&sock);
    let started = Instant::now();
    while !sock.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "relay never bound"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let room = typed_peer::room();

    // The word processor: the page the agent was invited to, carrying the
    // instruction.
    let (wp_peer, wp_replica) = connect_word_processor(&sock, room);

    // The photo editor: a second application's document, in the same room,
    // that the agent was never told about.
    let (photo_peer, photo_replica) = connect_photo_editor(&sock, room);
    std::thread::sleep(Duration::from_millis(100));

    let permissions = word_processor_and_photos_permissions();
    let cfg = Config {
        endpoint: Some(sock),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: APP.into(),
        cluster: room,
        record: Some(RECORD.into()),
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::ZERO,
        settle: Duration::ZERO,
        directory: None,
        permissions,
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(15))
            .expect("agent session")
    });

    // Nothing here yet waits for a late-arriving application — that is
    // `late_app_reaches_a_waiter.rs`'s concern, not this commit's — so the
    // photo editor's document has to be *fully held* before the instruction
    // naming its operation is even readable, or this races: whichever
    // peer's reply the agent happens to read first decides what `offers`
    // holds when it acts, once, on this single non-sequence instruction.
    // The photo peer answers first and only then signals the word
    // processor to answer, which is what pins the order rather than hoping
    // for it.
    let (photo_ready_tx, photo_ready_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let photo_thread =
        std::thread::spawn(move || drive_photo_peer(photo_peer, photo_replica, &photo_ready_tx));

    let (wp_replica, done) = drive_word_processor_peer(wp_peer, wp_replica, &photo_ready_rx);

    assert!(agent.join().unwrap(), "the agent reports having acted");
    assert!(
        done,
        "the instruction was never answered; blocks: {:?}",
        blocks(&wp_replica)
            .iter()
            .map(|b| (b.kind.clone(), b.text.clone()))
            .collect::<Vec<_>>()
    );

    let photo_replica = photo_thread
        .join()
        .expect("the photo peer's thread must not panic");
    assert_eq!(
        photos::doc::PhotosDoc::edits(&photo_replica).rotate_quarters,
        1,
        "the discovered operation never reached the second application's document"
    );
}

/// A denied operation is *absent* from what the agent discovers, not merely
/// refused — so an instruction naming it is answered exactly as one naming
/// nothing at all would be, and never leaks an address into the document.
#[test]
fn without_the_capability_a_discovered_operation_is_never_performed() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let replica = seed_document_with_image("Scribe: delete the image");
    let (replica, acted, invocation_actor) = run(
        |sock, room| Config {
            endpoint: Some(sock),
            endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
            app_id: APP.into(),
            cluster: room,
            record: Some(RECORD.into()),
            name: "Scribe".into(),
            pace: Duration::from_millis(10),
            once: true,
            model: false,
            model_timeout: Duration::from_secs(1),
            await_document: Duration::ZERO,
            settle: Duration::ZERO,
            directory: None,
            permissions: nothing_granted("Scribe"),
        },
        replica,
        |doc| {
            blocks(doc)
                .iter()
                .any(|x| x.text.starts_with("Scribe: I can't"))
        },
        Duration::from_secs(15),
    );
    assert!(acted, "the agent reports having acted");
    assert!(
        invocation_actor.is_none(),
        "a denied operation reached the application host"
    );

    let b = blocks(&replica);
    let shown: Vec<(String, String)> = b.iter().map(|x| (x.kind.clone(), x.text.clone())).collect();
    assert!(
        b.iter().any(|x| x.kind == "image"),
        "without the capability the image must not be deleted: {shown:?}"
    );
    let reply = b
        .iter()
        .find(|x| x.text.starts_with("Scribe:"))
        .unwrap_or_else(|| panic!("no reply was written at all: {shown:?}"));
    assert!(
        !reply.text.contains('#'),
        "a refusal must never carry an address a person cannot act on: {:?}",
        reply.text
    );
}
