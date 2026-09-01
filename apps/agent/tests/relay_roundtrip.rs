//! The agent against a real socket: the production Cap'n Proto broker on a
//! temp path, a peer that holds the document the way the word processor does
//! (same route, answers `Arrived` with a full snapshot, absorbs incremental
//! changes), and the agent joining late, reading the instruction out of the
//! document, and carrying it out — asserted on the peer's replica, never on
//! the agent's own.

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
use qos_agent::{Config, Session};
use ui::collaboration::{CollabEvent, CollabRequest, DocumentMutation, PresencePayload};
use uuid::Uuid;

mod typed_peer;
use typed_peer::{TypedPeer as UnixStream, document_mutation, recv, send};

const APP: &str = "dev.qos.word-processor";
const RECORD: &str = "file:/var/workspace/Demo/brief.docx";

/// The capability an inviter issues an agent for the app it was invited
/// to. The agent holds no role, so this grant is the whole of what it may
/// enumerate and perform.
fn granted(name: &str) -> ui::permissions::PermissionContext {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    PermissionContext::from_capabilities(
        Actor::agent(name),
        vec![
            CapOverride::grant(
                Subject::agent(name),
                Scope::App("word-processor".to_string()),
                Verb::Write,
            )
            .with_id("invited"),
        ],
    )
}

/// A document with a heading, three bullets, and an instruction to Scribe.
fn seed_document() -> AutoCommit {
    seed_document_as("ada")
}

/// As [`seed_document`], under a named actor. The actor has to be set
/// before the first change or the changes carry the default one, and two
/// documents only refuse to merge when their histories actually share an
/// actor id.
fn seed_document_as(actor: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from(actor.as_bytes()));
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let h = dm.insert_block(None, "heading");
        dm.set_attr(&h, "level", 1i64);
        dm.splice_text(&h, 0, 0, "Launch plan");
        let mut after = h;
        for line in ["Venue - The Hall", "Date - 3 Sep", "Budget - 1200"] {
            let b = dm.insert_block(Some(&after), "list_item");
            dm.set_attr(&b, "ordered", false);
            dm.splice_text(&b, 0, 0, line);
            after = b;
        }
        let p = dm.insert_block(Some(&after), "paragraph");
        dm.splice_text(&p, 0, 0, "Scribe: turn the bullets into a table");
    }
    doc.commit();
    doc
}

/// A document with a title paragraph and an image, and one instruction
/// written as two things to do.
fn seed_sequence_document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("ada".as_bytes()));
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
        dm.splice_text(
            &p,
            0,
            0,
            "Scribe: give this a title, then caption the image",
        );
    }
    doc.commit();
    doc
}

/// As [`seed_sequence_document`], but the second thing asked for is
/// something nothing in the room can do.
fn seed_unanswerable_sequence_document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("ada".as_bytes()));
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
        let p = dm.insert_block(Some(&title), "paragraph");
        dm.splice_text(
            &p,
            0,
            0,
            "Scribe: give this a title and then juggle three plates",
        );
    }
    doc.commit();
    doc
}

/// The actor both documents in the multi-document test are built under.
///
/// Not a contrivance: two real documents came from instances that had
/// settled on the same actor id, which is exactly why the second one would
/// not merge into the first — divergent history under one name.
/// `66696c65732d32` in the reported error is this string in hex.
const COLLIDING_ACTOR: &str = "files-2";

/// Another app's document: real, in the same room, and not addressed to
/// the agent. Stands in for the file manager, whose state the agent was
/// handed and adopted.
fn seed_foreign() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from(COLLIDING_ACTOR.as_bytes()));
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, "Home");
        let q = dm.insert_block(Some(&p), "paragraph");
        dm.splice_text(&q, 0, 0, "This folder is empty");
    }
    doc.commit();
    doc
}

/// [`seed_document`] under the colliding actor, so that merging it into
/// the foreign document fails for real instead of quietly combining two
/// unrelated pages.
fn seed_colliding_document() -> AutoCommit {
    seed_document_as(COLLIDING_ACTOR)
}

fn blocks(doc: &AutoCommit) -> Vec<qdoc::BlockView> {
    let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
    qdoc::model::materialize(doc, &c).blocks
}

/// Drive the peer exactly as the app would: answer the agent's Arrived with
/// a full snapshot, apply every incremental change to the replica, and
/// watch for the table. Returns the replica, whether it settled into the
/// table, and whether Scribe's own presence was observed.
fn drive_peer_to_table(mut peer: UnixStream, mut replica: AutoCommit) -> (AutoCommit, bool, bool) {
    peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_scribe_presence = false;
    let mut done = false;
    // Presence and document events use independent typed subscriptions, so
    // their delivery is intentionally not globally ordered. Keep draining
    // until both guarantees have been observed instead of treating a fast
    // document update as permission to stop listening for presence.
    while Instant::now() < deadline && !(done && saw_scribe_presence) {
        let Some(event) = recv(&mut peer) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Arrived,
                ..
            } => {
                // The typed SDK reports the local participant's initial
                // presence as well as remote arrivals. Only Scribe's arrival
                // asks this app peer to offer its document.
                if user_id == "ada" {
                    continue;
                }
                assert_eq!(
                    user_id, "Scribe",
                    "the relay names presence by the announced handle"
                );
                saw_scribe_presence = true;
                let snap = CollabRequest::DocumentSnapshot {
                    data: replica.save(),
                };
                send(&mut peer, &snap).unwrap();
            }
            CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Caret { .. },
                ..
            } => {
                assert_eq!(user_id, "Scribe");
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                replica.load_incremental(&data).unwrap();
                let b = blocks(&replica);
                let has_cells = b.iter().any(|x| x.kind == "cell");
                let has_bullets = b.iter().any(|x| x.kind == "list_item");
                let has_instruction = b.iter().any(|x| x.text.starts_with("Scribe:"));
                if has_cells && !has_bullets && !has_instruction {
                    done = true;
                }
            }
            _ => {}
        }
    }
    (replica, done, saw_scribe_presence)
}

/// Assert the replica settled into a table carrying the bullets' cells with
/// the heading intact, and that every change is attributed to the agent's
/// own actor.
fn assert_table_and_attribution(replica: &mut AutoCommit) {
    // The table, row-major, carries the bullets' cells; the heading stays.
    let b = blocks(replica);
    assert_eq!(b[0].kind, "heading");
    assert_eq!(b[0].text, "Launch plan");
    let mut cells: Vec<(i64, i64, String)> = b
        .iter()
        .filter(|x| x.kind == "cell")
        .map(|x| {
            let n = |k: &str| match x.attrs.get(k) {
                Some(qdoc::AttrVal::Int(v)) => *v,
                _ => -1,
            };
            (n("row"), n("col"), x.text.clone())
        })
        .collect();
    cells.sort();
    let texts: Vec<&str> = cells.iter().map(|c| c.2.as_str()).collect();
    assert_eq!(
        texts,
        ["Venue", "The Hall", "Date", "3 Sep", "Budget", "1200"],
        "three 2-column rows, in order"
    );

    // Every change the agent made is attributed to it. Filtered rather than
    // positionally skipped: `qdoc::model::init` adopts the schema from its
    // own shared genesis, under a fixed well-known actor, ahead of the
    // seed's own commit — both pre-date anything the agent did.
    let genesis = qdoc::model::schema_genesis_actor_hex();
    let seed = hex::encode("ada");
    let actors: std::collections::HashSet<String> = qdoc::history::versions(replica)
        .into_iter()
        .map(|v| v.actor_hex)
        .filter(|a| *a != genesis && *a != seed)
        .collect();
    assert_eq!(
        actors,
        std::collections::HashSet::from([hex::encode("Scribe")]),
        "the agent's commits carry its own actor, so the history strip names it"
    );
}

#[test]
fn the_agent_turns_bullets_into_a_table_on_the_peers_replica() {
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

    // The peer: what the word processor is on the wire.
    let mut peer = UnixStream::connect(&sock);
    let hello = CollabRequest::Hello {
        app_id: APP.into(),
        cluster_id: room,
        record: Some(RECORD.into()),
        handle: Some("ada".into()),
    };
    send(&mut peer, &hello).unwrap();
    let replica = seed_document();
    // Let the relay register the peer's Hello before the agent's frames
    // can race it (the relay handles each connection on its own thread).
    std::thread::sleep(Duration::from_millis(100));

    // The agent, on its own thread, once.
    let cfg = Config {
        endpoint: Some(sock),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: APP.into(),
        cluster: room,
        record: Some(RECORD.into()),
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        // This test proves the relay and the edit path, not the decision, so
        // it pins the choice to the keyword vocabulary: no network, no key,
        // and the same answer every run.
        model: false,
        model_timeout: Duration::from_secs(1),
        // `run_until` bounds this test; the wait-for-a-document deadline
        // belongs to `run`, and is exercised on its own below.
        await_document: Duration::ZERO,
        settle: Duration::ZERO,
        directory: None,
        permissions: granted("Scribe"),
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(15))
            .expect("agent session")
    });

    let (mut replica, done, saw_scribe_presence) = drive_peer_to_table(peer, replica);
    assert!(
        saw_scribe_presence,
        "the agent must announce itself to the room"
    );
    assert!(
        done,
        "the peer's replica never became a table; blocks: {:?}",
        blocks(&replica)
            .iter()
            .map(|b| (b.kind.clone(), b.text.clone()))
            .collect::<Vec<_>>()
    );
    assert!(agent.join().unwrap(), "the agent reports having acted");

    assert_table_and_attribution(&mut replica);
}

/// Another app's document in the same room must not end the run.
///
/// A room is not one document. Every participant answers an arrival with
/// its own state, and the broker hands a frame to any subscriber whose
/// address it matches — which, for a participant that announced no app
/// id, is everyone in the room. An agent given a room but no `--record`
/// is therefore offered the file manager's state alongside the page it
/// was invited to, and nothing in the frame says which is which.
///
/// Taking the first one to arrive is what the agent used to do, and when
/// the file manager answered first the agent adopted its state, put its
/// caret in the file manager's window, and then died —
///
///     Error: merge a peer snapshot
///     Caused by: duplicate seq 1 found for actor 66696c65732d32
///
/// — because two documents from instances that share an actor id have
/// divergent histories under the same name and Automerge refuses to
/// merge them. Both documents here are seeded that way, so the refusal
/// this asserts against is the one that was actually seen.
/// Precondition, checked rather than assumed: the two documents must
/// genuinely refuse to merge. If they ever reconcile — a different
/// Automerge, a changed seed — the agent would simply absorb both, still
/// find the instruction, still build the table, and the test that calls
/// this would pass while exercising none of what it exists for. Fail
/// loudly at that point instead of quietly going hollow.
fn assert_seeds_collide() {
    let mut a = seed_foreign();
    let mut b = seed_colliding_document();
    assert!(
        a.merge(&mut b).is_err(),
        "the two seeds must collide on their shared actor id, or this \
         test proves nothing about a document the agent cannot merge"
    );
}

/// Connect the file manager and the word processor to the same room —
/// neither with a record, so nothing but the app id tells their documents
/// apart.
fn connect_files_and_word_processor(
    sock: &std::path::Path,
    room: Uuid,
) -> (UnixStream, AutoCommit, UnixStream, AutoCommit) {
    // The file manager: a real document, in this room, that is not the
    // agent's business and never mentions it.
    let mut files_peer = UnixStream::connect(sock);
    send(
        &mut files_peer,
        &CollabRequest::Hello {
            app_id: "dev.qos.file-explorer".into(),
            cluster_id: room,
            record: None,
            handle: Some("files".into()),
        },
    )
    .unwrap();
    let files_doc = seed_foreign();

    // The word processor, holding the page with the instruction on it.
    let mut wp_peer = UnixStream::connect(sock);
    send(
        &mut wp_peer,
        &CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: None,
            handle: Some("ada".into()),
        },
    )
    .unwrap();
    let replica = seed_colliding_document();
    (files_peer, files_doc, wp_peer, replica)
}

/// The file manager answers every `Arrived` with its own document — the
/// ordering that broke this once, since it answers before the word
/// processor does.
fn drive_files_peer(mut files_peer: UnixStream, mut files_doc: AutoCommit) {
    files_peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let Some(event) = recv(&mut files_peer) else {
            continue;
        };
        if let CollabEvent::RemotePresenceUpdate {
            payload: PresencePayload::Arrived,
            ..
        } = event
        {
            let snap = CollabRequest::DocumentSnapshot {
                data: files_doc.save(),
            };
            send(&mut files_peer, &snap).unwrap();
        }
    }
}

/// The word processor answers its own `Arrived` shortly after the file
/// manager does — by which point the agent has already taken the wrong
/// document, if it was going to — then watches for its instruction to
/// resolve into a table.
fn drive_word_processor_to_table(
    mut wp_peer: UnixStream,
    mut replica: AutoCommit,
) -> (AutoCommit, bool) {
    wp_peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut done = false;
    let mut answered = false;
    while Instant::now() < deadline && !done {
        let Some(event) = recv(&mut wp_peer) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } if !answered => {
                answered = true;
                // Shortly after the file manager, so the agent has already
                // taken the wrong document by the time this arrives.
                std::thread::sleep(Duration::from_millis(400));
                let snap = CollabRequest::DocumentSnapshot {
                    data: replica.save(),
                };
                send(&mut wp_peer, &snap).unwrap();
            }
            // The agent's edits. A delta from the file manager's document
            // would not apply here, so ignore what does not.
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                if replica.load_incremental(&data).is_err() {
                    continue;
                }
                let b = blocks(&replica);
                if b.iter().any(|x| x.kind == "cell")
                    && !b.iter().any(|x| x.text.starts_with("Scribe:"))
                {
                    done = true;
                }
            }
            _ => {}
        }
    }
    (replica, done)
}

#[test]
fn the_room_holds_more_than_one_document_and_the_agent_finds_its_own() {
    assert_seeds_collide();

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

    let (files_peer, files_doc, wp_peer, replica) = connect_files_and_word_processor(&sock, room);
    std::thread::sleep(Duration::from_millis(100));

    // A room, and no record to tell the room's documents apart.
    let cfg = Config {
        endpoint: Some(sock),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: APP.into(),
        cluster: room,
        record: None,
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::ZERO,
        settle: Duration::ZERO,
        directory: None,
        permissions: granted("Scribe"),
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(20))
            .expect("the agent must survive another app's document")
    });

    let files_thread = std::thread::spawn(move || drive_files_peer(files_peer, files_doc));
    let (replica, done) = drive_word_processor_to_table(wp_peer, replica);

    assert!(
        agent.join().expect("the agent thread must not panic"),
        "the agent reports having acted"
    );
    assert!(
        done,
        "the word processor's document never became a table — the agent \
         stayed on the other document in the room; blocks: {:?}",
        blocks(&replica)
            .iter()
            .map(|b| (b.kind.clone(), b.text.clone()))
            .collect::<Vec<_>>()
    );

    // The content survived into the table, not just the grid.
    let b = blocks(&replica);
    let cells: Vec<String> = b
        .iter()
        .filter(|x| x.kind == "cell")
        .map(|x| x.text.clone())
        .collect();
    assert!(
        cells.iter().any(|c| c == "Venue"),
        "the bullets' text is in the cells: {cells:?}"
    );

    let _ = files_thread.join();
}

/// A room where nobody answers must end in a diagnosis, not a hang.
///
/// The broker matches `(app, room, record)` and drops what does not
/// match, silently and by design. So the commonest way to misconfigure
/// an agent — point it at a room but not at the record the app has
/// open — produces a process that connects, announces, logs that it
/// joined, and then waits forever. A harness that hits this reports
/// "the agent did not finish", which names the one part that was
/// working and sends the reader to `apps/agent`.
///
/// The relay used here broadcasts to every peer regardless of route, so
/// what it reproduces is the *symptom* — nobody answers — rather than
/// the route mismatch itself, which is the shell broker's to enforce
/// and to test. What is asserted is the agent's half: a bounded wait,
/// a failure rather than a success, and a message that names both the
/// address that went unanswered and the flag that fixes it.
#[test]
fn an_unanswered_address_is_reported_rather_than_waited_on() {
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

    let cfg = Config {
        endpoint: Some(sock),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: APP.into(),
        cluster: typed_peer::room(),
        // The shape of the mistake: a room that exists, on a record
        // nobody in it is looking at.
        record: Some("file:/var/workspace/Demo/nobody-has-this.docx".into()),
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::from_millis(1500),
        settle: Duration::ZERO,
        directory: None,
        permissions: granted("Scribe"),
    };
    let mut session = Session::connect(cfg).expect("agent connects");

    let began = Instant::now();
    let err = session
        .run()
        .expect_err("an unanswered address must not report success");
    let waited = began.elapsed();

    assert!(
        waited < Duration::from_secs(10),
        "the agent waited {waited:?} — the deadline did not bound it"
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("nobody-has-this.docx"),
        "the diagnosis must name the record that went unanswered: {msg}"
    );
    assert!(
        msg.contains("--record"),
        "the diagnosis must name the flag that fixes it: {msg}"
    );
}

/// One request, two things to do: each step runs in order against the
/// document the step before it left, and the instruction paragraph goes
/// away only once every step in it is done.
#[test]
fn the_agent_carries_out_a_sequence_written_as_one_instruction() {
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
    let hello = CollabRequest::Hello {
        app_id: APP.into(),
        cluster_id: room,
        record: Some(RECORD.into()),
        handle: Some("ada".into()),
    };
    send(&mut peer, &hello).unwrap();
    let mut replica = seed_sequence_document();
    std::thread::sleep(Duration::from_millis(100));

    let cfg = Config {
        endpoint: Some(sock),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: APP.into(),
        cluster: room,
        record: Some(RECORD.into()),
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        // The deterministic floor splits the two clauses; see `Session::decide`.
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::ZERO,
        settle: Duration::ZERO,
        directory: None,
        permissions: granted("Scribe"),
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(15))
            .expect("agent session")
    });

    peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(15);
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
                let snap = CollabRequest::DocumentSnapshot {
                    data: replica.save(),
                };
                send(&mut peer, &snap).unwrap();
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                replica.load_incremental(&data).unwrap();
                let b = blocks(&replica);
                let has_heading = b
                    .iter()
                    .any(|x| x.kind == "heading" && x.text == "Launch plan");
                let has_caption = b.iter().any(|x| x.text == "Figure 1");
                let has_instruction = b.iter().any(|x| x.text.starts_with("Scribe:"));
                if has_heading && has_caption && !has_instruction {
                    done = true;
                }
            }
            _ => {}
        }
    }
    assert!(
        done,
        "the sequence never finished both steps; blocks: {:?}",
        blocks(&replica)
            .iter()
            .map(|b| (b.kind.clone(), b.text.clone()))
            .collect::<Vec<_>>()
    );
    assert!(agent.join().unwrap(), "the agent reports having acted");

    // Both steps ran in order; only one removes the instruction.
    let b = blocks(&replica);
    assert_eq!(b[0].kind, "heading", "the title step ran");
    assert_eq!(b[0].text, "Launch plan");
    assert!(
        b.iter().any(|x| x.kind == "image"),
        "the image the caption belongs to is still there"
    );
    assert_eq!(
        b.iter().filter(|x| x.text.starts_with("Scribe:")).count(),
        0,
        "the instruction is answered once, not once per step"
    );
}

/// A sequence naming something nothing in the room can do is refused
/// *whole*, before either step is touched — never the first step applied
/// and the second one silently dropped.
#[test]
fn an_unanswerable_step_refuses_the_whole_sequence_untouched() {
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
    let hello = CollabRequest::Hello {
        app_id: APP.into(),
        cluster_id: room,
        record: Some(RECORD.into()),
        handle: Some("ada".into()),
    };
    send(&mut peer, &hello).unwrap();
    let mut replica = seed_unanswerable_sequence_document();
    std::thread::sleep(Duration::from_millis(100));

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
        permissions: granted("Scribe"),
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(15))
            .expect("agent session")
    });

    peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(15);
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
                let snap = CollabRequest::DocumentSnapshot {
                    data: replica.save(),
                };
                send(&mut peer, &snap).unwrap();
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                replica
                    .load_incremental(&data)
                    .expect("apply the agent's change");
                let b = blocks(&replica);
                if b.iter().any(|x| x.text.contains("juggle three plates")) {
                    done = true;
                }
            }
            _ => {}
        }
    }
    assert!(
        done,
        "the agent never answered the sequence; blocks: {:?}",
        blocks(&replica)
            .iter()
            .map(|b| (b.kind.clone(), b.text.clone()))
            .collect::<Vec<_>>()
    );
    assert!(agent.join().unwrap(), "the agent reports having acted");

    // Nothing was touched: the title is still a plain paragraph, and the
    // refusal was written in place of the instruction rather than after
    // half the request had already been carried out.
    let b = blocks(&replica);
    assert_eq!(
        b[0].kind, "paragraph",
        "the title step never ran: the whole sequence was refused first"
    );
    assert_eq!(b[0].text, "Launch plan");
    assert!(
        b.iter().any(|x| x.text.starts_with("Scribe: I can't")),
        "the refusal names what could not be done: {:?}",
        b.iter().map(|x| x.text.clone()).collect::<Vec<_>>()
    );
}
