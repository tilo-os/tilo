//! The agent against a real socket: the toolkit's relay broker on a temp
//! path, a peer that holds the document the way the word processor does
//! (same route, answers `Arrived` with a full snapshot, absorbs incremental
//! changes), and the agent joining late, reading the instruction out of the
//! document, and carrying it out — asserted on the peer's replica, never on
//! the agent's own.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use qos_agent::wire::{read_frame, write_frame};
use qos_agent::{Config, Session};
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use uuid::Uuid;

const APP: &str = "dev.qos.word-processor";
const RECORD: &str = "file:/var/workspace/Demo/brief.docx";

/// A document with a heading, three bullets, and an instruction to Scribe.
fn seed_document() -> AutoCommit {
    seed_document_as("quinten")
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
        let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
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

/// The actor both documents in the multi-document test are built under.
///
/// Not a contrivance: the demo's two documents came from instances that
/// had settled on the same actor id, which is exactly why the second one
/// would not merge into the first — divergent history under one name.
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
        let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, "Home");
        let q = dm.insert_block(Some(&p), "paragraph");
        dm.splice_text(&q, 0, 0, "This folder is empty");
    }
    doc.commit();
    doc
}

/// [`seed_document`] under the colliding actor, so that merging it into
/// the foreign document fails the way it failed in the demo instead of
/// quietly combining two unrelated pages.
fn seed_colliding_document() -> AutoCommit {
    seed_document_as(COLLIDING_ACTOR)
}

fn blocks(doc: &AutoCommit) -> Vec<qdoc::BlockView> {
    let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
    qdoc::model::materialize(doc, &c).blocks
}

#[test]
fn the_agent_turns_bullets_into_a_table_on_the_peers_replica() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("collab.sock");
    {
        let sock = sock.clone();
        std::thread::spawn(move || {
            ui::collaboration::broker::serve_path(&sock).expect("serve the relay");
        });
    }
    let started = Instant::now();
    while !sock.exists() {
        assert!(started.elapsed() < Duration::from_secs(5), "relay never bound");
        std::thread::sleep(Duration::from_millis(20));
    }

    let room = Uuid::new_v4();

    // The peer: what the word processor is on the wire.
    let mut peer = UnixStream::connect(&sock).expect("peer connects");
    let hello = serde_json::to_vec(&CollabRequest::Hello {
        app_id: APP.into(),
        cluster_id: room,
        record: Some(RECORD.into()),
        handle: Some("quinten".into()),
    })
    .unwrap();
    write_frame(&mut peer, &hello).unwrap();
    let mut replica = seed_document();
    // Let the relay register the peer's Hello before the agent's frames
    // can race it (the relay handles each connection on its own thread).
    std::thread::sleep(Duration::from_millis(100));

    // The agent, on its own thread, once.
    let cfg = Config {
        socket: Some(sock.clone()),
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
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(15))
            .expect("agent session")
    });

    // Drive the peer exactly as the app would: answer the agent's Arrived
    // with a full snapshot, apply every incremental change to the replica,
    // and watch for the table.
    peer.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_scribe_presence = false;
    let mut done = false;
    while Instant::now() < deadline && !done {
        let Some(frame) = read_frame(&mut peer) else {
            continue;
        };
        let event: CollabEvent = serde_json::from_slice(&frame).expect("decode");
        match event {
            CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Arrived,
                ..
            } => {
                assert_eq!(user_id, "Scribe", "the relay names presence by the announced handle");
                saw_scribe_presence = true;
                let snap = serde_json::to_vec(&CollabRequest::SyncCRDT { data: replica.save() }).unwrap();
                write_frame(&mut peer, &snap).unwrap();
            }
            CollabEvent::RemotePresenceUpdate { user_id, payload: PresencePayload::Caret { .. }, .. } => {
                assert_eq!(user_id, "Scribe");
            }
            CollabEvent::CRDTIncremental { data } => {
                replica.load_incremental(&data).expect("apply the agent's change");
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
    assert!(saw_scribe_presence, "the agent must announce itself to the room");
    assert!(done, "the peer's replica never became a table; blocks: {:?}", blocks(&replica).iter().map(|b| (b.kind.clone(), b.text.clone())).collect::<Vec<_>>());
    assert!(agent.join().unwrap(), "the agent reports having acted");

    // The table, row-major, carries the bullets' cells; the heading stays.
    let b = blocks(&replica);
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

    // Every change the agent made is attributed to it.
    let actors: std::collections::HashSet<String> = qdoc::history::versions(&mut replica)
        .into_iter()
        .skip(1) // the seed
        .map(|v| v.actor_hex)
        .collect();
    assert_eq!(
        actors,
        std::collections::HashSet::from([hex::encode("Scribe")]),
        "the agent's commits carry its own actor, so the history strip names it"
    );
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
/// Taking the first one to arrive is what the agent used to do, and in
/// the demo the file manager answered first: the agent adopted its
/// state, put its caret in the file manager's window, and then died —
///
///     Error: merge a peer snapshot
///     Caused by: duplicate seq 1 found for actor 66696c65732d32
///
/// — because two documents from instances that share an actor id have
/// divergent histories under the same name and Automerge refuses to
/// merge them. Both documents here are seeded that way, so the refusal
/// this asserts against is the one that was actually seen.
#[test]
fn the_room_holds_more_than_one_document_and_the_agent_finds_its_own() {
    // Precondition, checked rather than assumed: the two documents must
    // genuinely refuse to merge. If they ever reconcile — a different
    // Automerge, a changed seed — the agent would simply absorb both,
    // still find the instruction, still build the table, and this test
    // would pass while exercising none of what it exists for. Fail
    // loudly at that point instead of quietly going hollow.
    {
        let mut a = seed_foreign();
        let mut b = seed_colliding_document();
        assert!(
            a.merge(&mut b).is_err(),
            "the two seeds must collide on their shared actor id, or this \
             test proves nothing about a document the agent cannot merge"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("collab.sock");
    {
        let sock = sock.clone();
        std::thread::spawn(move || {
            ui::collaboration::broker::serve_path(&sock).expect("serve the relay");
        });
    }
    let started = Instant::now();
    while !sock.exists() {
        assert!(started.elapsed() < Duration::from_secs(5), "relay never bound");
        std::thread::sleep(Duration::from_millis(20));
    }
    let room = Uuid::new_v4();

    // The file manager: a real document, in this room, that is not the
    // agent's business and never mentions it.
    let mut files_peer = UnixStream::connect(&sock).expect("files connects");
    write_frame(
        &mut files_peer,
        &serde_json::to_vec(&CollabRequest::Hello {
            app_id: "dev.qos.file-explorer".into(),
            cluster_id: room,
            record: None,
            handle: Some("files".into()),
        })
        .unwrap(),
    )
    .unwrap();
    let mut files_doc = seed_foreign();

    // The word processor, holding the page with the instruction on it.
    let mut wp_peer = UnixStream::connect(&sock).expect("word processor connects");
    write_frame(
        &mut wp_peer,
        &serde_json::to_vec(&CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: None,
            handle: Some("quinten".into()),
        })
        .unwrap(),
    )
    .unwrap();
    let mut replica = seed_colliding_document();
    std::thread::sleep(Duration::from_millis(100));

    // The agent as the demo runs it: a room, and no record to tell the
    // room's documents apart.
    let cfg = Config {
        socket: Some(sock.clone()),
        app_id: APP.into(),
        cluster: room,
        record: None,
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::ZERO,
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(20))
            .expect("the agent must survive another app's document")
    });

    // The file manager answers first — which is the ordering that broke
    // this — and the word processor a beat later.
    files_peer
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    wp_peer
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let files_thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let Some(frame) = read_frame(&mut files_peer) else {
                continue;
            };
            if let Ok(CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            }) = serde_json::from_slice::<CollabEvent>(&frame)
            {
                let snap = serde_json::to_vec(&CollabRequest::SyncCRDT {
                    data: files_doc.save(),
                })
                .unwrap();
                write_frame(&mut files_peer, &snap).unwrap();
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut done = false;
    let mut answered = false;
    while Instant::now() < deadline && !done {
        let Some(frame) = read_frame(&mut wp_peer) else {
            continue;
        };
        let Ok(event) = serde_json::from_slice::<CollabEvent>(&frame) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } if !answered => {
                answered = true;
                // A beat after the file manager, so the agent has already
                // taken the wrong document by the time this arrives.
                std::thread::sleep(Duration::from_millis(400));
                let snap = serde_json::to_vec(&CollabRequest::SyncCRDT {
                    data: replica.save(),
                })
                .unwrap();
                write_frame(&mut wp_peer, &snap).unwrap();
            }
            CollabEvent::CRDTIncremental { data } => {
                // The agent's edits. A delta from the file manager's
                // document would not apply here, so ignore what does not.
                if replica.load_incremental(&data).is_ok() {
                    let b = blocks(&replica);
                    if b.iter().any(|x| x.kind == "cell")
                        && !b.iter().any(|x| x.text.starts_with("Scribe:"))
                    {
                        done = true;
                    }
                }
            }
            _ => {}
        }
    }

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
    {
        let sock = sock.clone();
        std::thread::spawn(move || {
            ui::collaboration::broker::serve_path(&sock).expect("serve the relay");
        });
    }
    let started = Instant::now();
    while !sock.exists() {
        assert!(started.elapsed() < Duration::from_secs(5), "relay never bound");
        std::thread::sleep(Duration::from_millis(20));
    }

    let cfg = Config {
        socket: Some(sock.clone()),
        app_id: APP.into(),
        cluster: Uuid::new_v4(),
        // The shape of the mistake: a room that exists, on a record
        // nobody in it is looking at.
        record: Some("file:/var/workspace/Demo/nobody-has-this.docx".into()),
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::from_millis(1500),
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

// ---------------------------------------------------------------------------
// A page with a hole in its history
// ---------------------------------------------------------------------------

/// The page the demo's ninth beat is written on, built change by change,
/// plus the replica a peer is left holding when exactly one of those
/// changes never reaches it.
///
/// Returns `(whole, truncated)` — the page as its author has it, and the
/// bytes a peer that missed the photograph answers a handshake with.
///
/// The missed change is a 146 KB image, which is not an arbitrary pick:
/// `shell::shell::network` refuses to publish an app snapshot over
/// `MAX_APP_SNAPSHOT_BYTES` (14 KiB), because gossipsub caps an RPC at
/// 64 KiB and the publish path writes bytes as JSON at up to four
/// characters each. So a photograph dropped into a shared page is a
/// change that provably cannot cross a room, while every keystroke after
/// it names that change as an ancestor and can.
fn seed_document_with_a_hole() -> (AutoCommit, Vec<u8>) {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("quinten".as_bytes()));
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);

    // What every replica agrees on before the big change.
    let opening = {
        let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, "the same document on both machines");
        p
    };
    doc.commit();
    let base = doc.save();

    // The change that does not fit down the pipe.
    let image = {
        let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
        let img = dm.insert_block(Some(&opening), "image");
        let hash = dm.put_blob(&vec![0x7Au8; 146 * 1024]);
        dm.set_attr(&img, "blob", hash.as_str());
        img
    };
    doc.commit();
    // Dropped on purpose: this is the frame the cap refuses.
    let refused = doc.save_incremental();
    assert!(
        refused.len() > 14 * 1024,
        "the dropped change has to be one the cap would really refuse, or \
         this seeds a hole that could not happen: {} bytes",
        refused.len()
    );

    // Everything typed after it — the list, then the line addressed to the
    // agent. Each of these names the image change as an ancestor.
    let mut tail: Vec<Vec<u8>> = Vec::new();
    let mut after = image;
    for line in ["Doors: 18:00", "Sound check: 18:30", "Seating: 240"] {
        {
            let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let b = dm.insert_block(Some(&after), "list_item");
            dm.set_attr(&b, "ordered", false);
            dm.splice_text(&b, 0, 0, line);
            after = b;
        }
        doc.commit();
        tail.push(doc.save_incremental());
    }
    {
        let mut dm = qdoc::DocMut { doc: &mut doc, c: &c, ids: &mut ids };
        let p = dm.insert_block(Some(&after), "paragraph");
        dm.splice_text(&p, 0, 0, "Scribe: turn the bullets into a table");
    }
    doc.commit();
    tail.push(doc.save_incremental());

    // The peer: the opening, every later keystroke, and not the
    // photograph in between.
    let mut lagging = AutoCommit::load(&base).expect("the base loads");
    for delta in &tail {
        lagging
            .load_incremental(delta)
            .expect("a change with an unmet dependency is queued, not refused");
    }
    (doc, lagging.save())
}

/// The shape the demo hung in: the only peer that answers is holding a
/// page that stops short of the instruction, and the agent takes it for
/// the document.
///
/// Automerge will not apply a change whose ancestors it lacks — it queues
/// it — so a replica missing one change in the middle materialises as the
/// page *before* that change, however many later changes it is carrying.
/// The word processor in the agent's own session was in exactly that
/// state: it answered with 3663 bytes of a page that ended above the
/// bullets. The agent adopted it, found nothing addressed to it, and went
/// quiet — it asked again only while it had no document at all, and its
/// one deadline covered the same case. The harness killed it two minutes
/// later and reported that the agent had not finished, which names the
/// only part that was working.
///
/// Asserted here: the agent notices, and says so, in bounded time.
/// Whether it can then do the work depends on somebody in the room
/// holding the whole page, which is the next test.
#[test]
fn a_page_missing_an_ancestor_is_not_mistaken_for_the_whole_page() {
    let (whole, truncated) = seed_document_with_a_hole();

    // Preconditions, checked rather than assumed: were Automerge ever to
    // apply changes with unmet dependencies, this would seed a complete
    // page and the test would pass while proving nothing.
    {
        assert!(
            blocks(&whole).iter().any(|x| x.text.starts_with("Scribe:")),
            "the author's own page must carry the instruction"
        );
        let mut lagging = AutoCommit::load(&truncated).expect("the peer's page loads");
        let seen = blocks(&lagging);
        assert!(
            !seen.iter().any(|x| x.text.starts_with("Scribe:")),
            "the peer's page must stop short of the instruction: {:?}",
            seen.iter().map(|x| (x.kind.clone(), x.text.clone())).collect::<Vec<_>>()
        );
        assert!(
            !lagging.get_missing_deps(&[]).is_empty(),
            "the peer's page must be provably missing an ancestor"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("collab.sock");
    {
        let sock = sock.clone();
        std::thread::spawn(move || {
            ui::collaboration::broker::serve_path(&sock).expect("serve the relay");
        });
    }
    let started = Instant::now();
    while !sock.exists() {
        assert!(started.elapsed() < Duration::from_secs(5), "relay never bound");
        std::thread::sleep(Duration::from_millis(20));
    }
    let room = Uuid::new_v4();

    // The word processor in the agent's own session: up, in the room, and
    // answering every handshake — with a page it cannot see all of.
    let mut peer = UnixStream::connect(&sock).expect("peer connects");
    write_frame(
        &mut peer,
        &serde_json::to_vec(&CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: None,
            handle: Some("quinten".into()),
        })
        .unwrap(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    peer.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let lagging_peer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            let Some(frame) = read_frame(&mut peer) else {
                continue;
            };
            if let Ok(CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            }) = serde_json::from_slice::<CollabEvent>(&frame)
            {
                let snap = serde_json::to_vec(&CollabRequest::SyncCRDT {
                    data: truncated.clone(),
                })
                .unwrap();
                let _ = write_frame(&mut peer, &snap);
            }
        }
    });

    let cfg = Config {
        socket: Some(sock.clone()),
        app_id: APP.into(),
        cluster: room,
        record: None,
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::from_secs(3),
    };
    let mut session = Session::connect(cfg).expect("agent connects");
    let ran = std::thread::spawn(move || session.run());

    // Bounded from outside, so "it is still waiting" fails as a result
    // rather than as a hung test binary.
    let give_up = Instant::now() + Duration::from_secs(20);
    while !ran.is_finished() && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        ran.is_finished(),
        "the agent is still waiting on a page it can prove it is not \
         seeing all of — this is the silence the harness reported as \
         'the agent did not finish'"
    );
    let err = ran
        .join()
        .expect("the agent thread must not panic")
        .expect_err("a page it cannot see all of is not a run that succeeded");
    let msg = format!("{err:#}");
    // Shown under `--nocapture`: what the agent actually says is the
    // whole point of this test, and a reader should be able to see it
    // without making the test fail on purpose.
    println!("the agent gave up saying:\n  {msg}");
    assert!(
        msg.contains("ancestor"),
        "the diagnosis must name the hole in the history rather than the \
         address, which was fine: {msg}"
    );
    let _ = lagging_peer.join();
}

/// The peer that answers first is not necessarily the peer that has the
/// page.
///
/// The agent used to stop asking the moment it held *a* document, so the
/// first answer decided the run. When that answer comes from a replica
/// that is behind — an app still opening its file, or one missing a
/// change nobody will ever be able to send it — nothing else in the loop
/// can change the outcome.
///
/// The room here holds both: a lagging word processor that answers every
/// handshake at once with the short page, and the peer holding the whole
/// page, which says nothing for the first few seconds. Nobody sends
/// anything unprompted, so asking again is the only way the agent ever
/// sees the instruction.
#[test]
fn it_keeps_asking_until_something_is_addressed_to_it() {
    let (mut whole, truncated) = seed_document_with_a_hole();

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("collab.sock");
    {
        let sock = sock.clone();
        std::thread::spawn(move || {
            ui::collaboration::broker::serve_path(&sock).expect("serve the relay");
        });
    }
    let started = Instant::now();
    while !sock.exists() {
        assert!(started.elapsed() < Duration::from_secs(5), "relay never bound");
        std::thread::sleep(Duration::from_millis(20));
    }
    let room = Uuid::new_v4();

    // The one that is behind, and always first.
    let mut lagging = UnixStream::connect(&sock).expect("the lagging peer connects");
    write_frame(
        &mut lagging,
        &serde_json::to_vec(&CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: None,
            handle: Some("behind".into()),
        })
        .unwrap(),
    )
    .unwrap();

    // The one that has the page, silent to begin with.
    let mut holder = UnixStream::connect(&sock).expect("the holder connects");
    write_frame(
        &mut holder,
        &serde_json::to_vec(&CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: None,
            handle: Some("quinten".into()),
        })
        .unwrap(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));

    lagging.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    holder.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let lagging_thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(40);
        while Instant::now() < deadline {
            let Some(frame) = read_frame(&mut lagging) else {
                continue;
            };
            if let Ok(CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            }) = serde_json::from_slice::<CollabEvent>(&frame)
            {
                let snap = serde_json::to_vec(&CollabRequest::SyncCRDT {
                    data: truncated.clone(),
                })
                .unwrap();
                let _ = write_frame(&mut lagging, &snap);
            }
        }
    });

    let cfg = Config {
        socket: Some(sock.clone()),
        app_id: APP.into(),
        cluster: room,
        record: None,
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::ZERO,
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(40))
            .expect("agent session")
    });

    // The holder ignores everything for the first few seconds — by which
    // time an agent that asks only until it has *a* document has long
    // since settled for the short page — and answers every handshake
    // after that.
    let quiet_until = Instant::now() + Duration::from_millis(2500);
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut done = false;
    while Instant::now() < deadline && !done {
        let Some(frame) = read_frame(&mut holder) else {
            continue;
        };
        let Ok(event) = serde_json::from_slice::<CollabEvent>(&frame) else {
            continue;
        };
        match event {
            CollabEvent::RemotePresenceUpdate {
                payload: PresencePayload::Arrived,
                ..
            } => {
                if Instant::now() < quiet_until {
                    continue;
                }
                let snap =
                    serde_json::to_vec(&CollabRequest::SyncCRDT { data: whole.save() }).unwrap();
                let _ = write_frame(&mut holder, &snap);
            }
            CollabEvent::CRDTIncremental { data } => {
                if whole.load_incremental(&data).is_ok() {
                    let b = blocks(&whole);
                    if b.iter().any(|x| x.kind == "cell")
                        && !b.iter().any(|x| x.text.starts_with("Scribe:"))
                    {
                        done = true;
                    }
                }
            }
            _ => {}
        }
    }

    assert!(
        agent.join().expect("the agent thread must not panic"),
        "the agent reports having acted"
    );
    assert!(
        done,
        "the whole page never became a table — the agent settled for the \
         first answer it was given and stopped asking; blocks: {:?}",
        blocks(&whole)
            .iter()
            .map(|b| (b.kind.clone(), b.text.clone()))
            .collect::<Vec<_>>()
    );
    let cells: Vec<String> = blocks(&whole)
        .iter()
        .filter(|x| x.kind == "cell")
        .map(|x| x.text.clone())
        .collect();
    assert!(
        cells.iter().any(|c| c == "Sound check"),
        "the bullets' text is in the cells: {cells:?}"
    );
    let _ = lagging_thread.join();
}
