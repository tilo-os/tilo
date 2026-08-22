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
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("quinten".as_bytes()));
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
