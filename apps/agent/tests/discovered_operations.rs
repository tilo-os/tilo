//! Operations the agent was never told about, discovered off the room's own
//! applications rather than a written-down list, and dispatched for real —
//! carried over the compositor's production Cap'n Proto broker. What is
//! under test is [`qos_agent::Session`]'s discovery, dispatch and capability
//! gating while it participates through the same endpoint as an app.

use std::time::{Duration, Instant};

use automerge::AutoCommit;
use qos_agent::{Config, Session};
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use uuid::Uuid;

mod typed_peer;
use typed_peer::{TypedPeer as UnixStream, recv, send};

const APP: &str = "dev.qos.word-processor";
const RECORD: &str = "file:/var/workspace/Demo/brief.docx";

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
) -> (AutoCommit, bool) {
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

    let room = Uuid::new_v4();
    let mut peer = UnixStream::connect(&sock).expect("peer connects");
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
        s.run_until(Instant::now() + within).expect("agent session")
    });

    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let deadline = Instant::now() + within;
    let mut done = false;
    while Instant::now() < deadline && !done {
        let Some(event) = recv(&mut peer) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } => {
                let snap = CollabRequest::SyncCRDT {
                    data: replica.save(),
                };
                send(&mut peer, &snap).unwrap();
            }
            CollabEvent::CRDTIncremental { data } => {
                replica
                    .load_incremental(&data)
                    .expect("apply the agent's change");
                done = poll(&replica);
            }
            _ => {}
        }
    }
    let acted = agent.join().unwrap();
    (replica, acted)
}

/// **The core claim of discovery.** An operation nowhere written down in
/// this crate — word-processor's own "delete image" — is found, performed,
/// and attributed to the agent's own actor, exactly as a catalogue action
/// is.
///
/// It also proves the re-entrancy guard: the instruction names the same
/// operation twice around an unrelated step, and dispatching it a second
/// time would find nothing left to delete and refuse — which would leave
/// the instruction paragraph standing with a refusal on it instead of being
/// answered in full. `dispatched_this_turn` is what keeps the second
/// mention from being dispatched at all.
#[test]
fn a_discovered_operation_is_performed_once_under_the_agents_own_actor() {
    let replica = seed_document_with_image(
        "Scribe: delete the image, then give this a title, then delete the image again",
    );
    let (mut replica, acted) = run(
        |sock, room| Config {
            endpoint: Some(sock),
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
            let title_done = b
                .iter()
                .any(|x| x.kind == "heading" && x.text == "Launch plan");
            let image_gone = !b.iter().any(|x| x.kind == "image");
            let answered = !b.iter().any(|x| x.text.starts_with("Scribe:"));
            title_done && image_gone && answered
        },
        Duration::from_secs(15),
    );
    let shown: Vec<(String, String)> = blocks(&replica)
        .iter()
        .map(|b| (b.kind.clone(), b.text.clone()))
        .collect();
    assert!(acted, "the agent reports having acted");

    let b = blocks(&replica);
    assert!(
        b.iter()
            .any(|x| x.kind == "heading" && x.text == "Launch plan"),
        "{shown:?}"
    );
    assert!(
        !b.iter().any(|x| x.kind == "image"),
        "the image was not deleted: {shown:?}"
    );
    assert!(
        !b.iter().any(|x| x.text.starts_with("Scribe:")),
        "the instruction was left holding a refusal instead of being answered in full \
         — the second mention of the same operation was dispatched again rather than \
         skipped: {shown:?}"
    );

    // Discovered, and attributed exactly like a catalogue action.
    let genesis = qdoc::model::schema_genesis_actor_hex();
    let seed = hex::encode("quinten");
    let actors: std::collections::HashSet<String> = qdoc::history::versions(&mut replica)
        .into_iter()
        .map(|v| v.actor_hex)
        .filter(|a| *a != genesis && *a != seed)
        .collect();
    assert_eq!(
        actors,
        std::collections::HashSet::from([hex::encode("Scribe")]),
        "the discovered operation's change must carry the agent's own actor"
    );
}

/// **Holding a second application's document.** The room carries a
/// word-processor page *and* a photograph nobody told the agent about; the
/// instruction names an operation only the photograph publishes. Answering
/// it needs the peer to have been attributed, recognised and merged under
/// its own contract ([`qos_agent::apply::attribute`],
/// [`qos_agent::apply::Editor::merge_full_any`]) — this is the live path
/// that gives them one.
#[test]
fn a_discovered_operation_reaches_a_second_application_held_as_a_peer() {
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
    let room = Uuid::new_v4();

    // The word processor: the page the agent was invited to, carrying the
    // instruction.
    let mut wp_peer = UnixStream::connect(&sock).expect("word processor connects");
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
    let mut wp_replica = seed_document_with_image("Scribe: rotate the photograph right");

    // The photo editor: a second application's document, in the same room,
    // that the agent was never told about.
    let mut photo_peer = UnixStream::connect(&sock).expect("photo editor connects");
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
    std::thread::sleep(Duration::from_millis(100));

    let permissions = {
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
    };
    let cfg = Config {
        endpoint: Some(sock),
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
    let photo_thread = std::thread::spawn(move || {
        photo_peer
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
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
                    let snap = CollabRequest::SyncCRDT {
                        data: photo_replica.save(),
                    };
                    send(&mut photo_peer, &snap).unwrap();
                    if !signalled {
                        let _ = photo_ready_tx.send(());
                        signalled = true;
                    }
                }
                CollabEvent::CRDTIncremental { data } => {
                    let _ = photo_replica.load_incremental(&data);
                    if photos::doc::PhotosDoc::edits(&photo_replica).rotate_quarters != 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        photo_replica
    });

    wp_peer
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
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
                let snap = CollabRequest::SyncCRDT {
                    data: wp_replica.save(),
                };
                send(&mut wp_peer, &snap).unwrap();
            }
            CollabEvent::CRDTIncremental { data } if wp_replica.load_incremental(&data).is_ok() => {
                done = !blocks(&wp_replica)
                    .iter()
                    .any(|x| x.text.starts_with("Scribe:"));
            }
            _ => {}
        }
    }
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
    let replica = seed_document_with_image("Scribe: delete the image");
    let (replica, acted) = run(
        |sock, room| Config {
            endpoint: Some(sock),
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
