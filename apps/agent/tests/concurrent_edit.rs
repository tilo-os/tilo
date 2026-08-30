//! The agent working while somebody types in the same document.
//!
//! The claim the demo makes with this beat is that the agent is a peer and
//! not a batch job: it holds a replica like everyone else, and a person
//! editing the page at the same moment is not a conflict to be resolved but
//! the ordinary case. That claim is only worth making if both edits are
//! still there afterwards, so this drives the two at once and asserts on
//! the *peer's* replica — the copy a person would be looking at.
//!
//! The interleave is real rather than arranged: the peer types the instant
//! it sees the agent's first change, which lands in the middle of the
//! agent's own sequence of edits, while the agent is between actions.

use std::time::{Duration, Instant};

use automerge::AutoCommit;
use qos_agent::{Config, Session};
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use uuid::Uuid;

mod typed_peer;
use typed_peer::TypedPeer;

const APP: &str = "dev.qos.word-processor";
const RECORD: &str = "file:/var/workspace/Demo/brief.docx";

/// What the person types while the agent is working, appended to the
/// heading a character at a time.
const TYPED: &str = " (final)";

fn seed_document() -> AutoCommit {
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

fn heading_id(doc: &AutoCommit) -> String {
    blocks(doc)
        .into_iter()
        .find(|b| b.kind == "heading")
        .expect("a heading")
        .id
}

/// Type one character into `block` at the end, and hand back the change to
/// broadcast — the shape a keystroke has in the word processor.
fn type_one(doc: &mut AutoCommit, block: &str, ch: &str) -> Vec<u8> {
    let before = doc.get_heads();
    {
        let c = qdoc::model::open(doc, &automerge::ROOT).expect("schema");
        let at = blocks(doc)
            .into_iter()
            .find(|b| b.id == block)
            .map_or(0, |b| b.text.chars().count());
        let mut ids = qdoc::model::seeded_id_gen(doc, &c);
        let mut dm = qdoc::DocMut {
            doc,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(block, at, 0, ch);
    }
    doc.commit();
    doc.save_after(&before)
}

/// The capability an inviter issues an agent for the app it was invited to.
///
/// The agent holds no role, so this grant is the whole of what it may do: with
/// none of these it enumerates nothing and performs nothing.
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

#[test]
fn a_person_typing_and_the_agent_working_both_land() {
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

    let mut peer = TypedPeer::connect(&sock).expect("peer connects");
    let hello = CollabRequest::Hello {
        app_id: APP.into(),
        cluster_id: room,
        record: Some(RECORD.into()),
        handle: Some("quinten".into()),
    };
    peer.announce(&sock, &hello).unwrap();
    let mut replica = seed_document();
    let head = heading_id(&replica);
    std::thread::sleep(Duration::from_millis(100));

    let cfg = Config {
        endpoint: Some(sock),
        app_id: APP.into(),
        cluster: room,
        record: Some(RECORD.into()),
        name: "Scribe".into(),
        // Slow enough that the person's keystrokes genuinely arrive
        // between the agent's actions rather than after all of them.
        pace: Duration::from_millis(40),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        // The instruction is in the page before the agent connects, so
        // there is no half-typed line here to wait out.
        settle: Duration::ZERO,
        await_document: Duration::ZERO,
        directory: None,
        permissions: granted("Scribe"),
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(Instant::now() + Duration::from_secs(20))
            .expect("agent session")
    });

    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut typed = 0usize;
    let mut table_done = false;
    while Instant::now() < deadline && !(table_done && typed == TYPED.chars().count()) {
        let Some(event) = peer.recv().unwrap() else {
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
                peer.send(&snap).unwrap();
            }
            CollabEvent::CRDTIncremental { data } => {
                replica
                    .load_incremental(&data)
                    .expect("apply the agent's change");

                // The person is typing through all of this. One character
                // per change the agent makes, so the two are interleaved
                // rather than merely overlapping.
                if let Some(ch) = TYPED.chars().nth(typed) {
                    let delta = type_one(&mut replica, &head, &ch.to_string());
                    let msg = CollabRequest::SyncIncremental { data: delta };
                    peer.send(&msg).unwrap();
                    typed += 1;
                }

                let b = blocks(&replica);
                table_done = b.iter().any(|x| x.kind == "cell")
                    && !b.iter().any(|x| x.kind == "list_item")
                    && !b.iter().any(|x| x.text.starts_with("Scribe:"));
            }
            _ => {}
        }
    }

    // Give the person's last keystrokes a moment to reach the agent, so a
    // pass here is not just a race the agent happened to win.
    std::thread::sleep(Duration::from_millis(300));
    assert!(agent.join().unwrap(), "the agent reports having acted");

    let b = blocks(&replica);
    let shown: Vec<(String, String)> = b.iter().map(|x| (x.kind.clone(), x.text.clone())).collect();

    // The agent's work is there.
    assert!(
        table_done,
        "the agent's table never landed; blocks: {shown:?}"
    );
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
    assert_eq!(
        cells.iter().map(|c| c.2.as_str()).collect::<Vec<_>>(),
        ["Venue", "The Hall", "Date", "3 Sep", "Budget", "1200"],
        "the agent's table survived the person typing through it"
    );

    // And so is the person's — every character, in order, not a subset
    // that happened to miss a collision.
    //
    // The count is asserted before the text: one keystroke per change the
    // agent made, so reaching the whole word is also the proof that the
    // two were genuinely interleaved and not merely both present. A pass
    // with `typed` at 1 would mean the agent did all its work in a single
    // change and nothing was ever concurrent.
    assert_eq!(
        typed,
        TYPED.chars().count(),
        "the person did not get to type through the agent's whole run, so \
         this proves less than it looks: {typed} of {} characters",
        TYPED.chars().count()
    );
    let heading = b
        .iter()
        .find(|x| x.kind == "heading")
        .expect("the heading is still there");
    let expected: String = format!(
        "Launch plan{}",
        TYPED.chars().take(typed).collect::<String>()
    );
    assert_eq!(
        heading.text, expected,
        "the person's typing was lost or reordered; blocks: {shown:?}"
    );

    // Both authors are in the history, so the strip attributes each edit
    // to whoever made it rather than folding them into one.
    let versions = qdoc::history::versions(&mut replica);
    let actors: std::collections::HashSet<String> =
        versions.iter().map(|v| v.actor_hex.clone()).collect();
    assert!(
        actors.contains(&hex::encode("Scribe")),
        "the agent's edits are not attributed to it: {actors:?}"
    );
    assert!(
        actors.contains(&hex::encode("quinten")),
        "the person's edits are not attributed to them: {actors:?}"
    );

    // And the attribution is per *change*, not per document — which is
    // the property anything drawing per-participant highlights needs.
    //
    // Worth asserting rather than assuming, because the obvious place to
    // look for an author is the wire envelope, and there it genuinely is
    // absent: `CollabEvent::CRDTIncremental` has no user field at all.
    // The author is in the delta instead. Every change carries the actor
    // that made it, the peer recovered all of them by applying the raw
    // incremental bytes, and no frame here was signed — so a receiver can
    // attribute an edit from what it already has.
    let scribe = versions
        .iter()
        .filter(|v| v.actor_hex == hex::encode("Scribe"))
        .count();
    let human = versions
        .iter()
        .filter(|v| v.actor_hex == hex::encode("quinten"))
        .count();
    assert!(
        scribe > 1 && human >= typed,
        "per-change attribution is missing: {scribe} change(s) from the agent \
         and {human} from the person, out of {} total",
        versions.len()
    );
    assert!(
        versions.iter().all(|v| !v.change_hash.is_empty()),
        "a change with no hash cannot be joined to what it touched"
    );
}
