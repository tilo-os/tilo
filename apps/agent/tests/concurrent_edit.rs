//! The agent working while somebody types in the same document.
//!
//! The claim the demo makes with this beat is that the agent is a peer and
//! not a batch job: it holds a replica like everyone else, and a person
//! editing the page at the same moment is not a conflict to be resolved but
//! the ordinary case. That claim is only worth making if both edits are
//! still there afterwards, so this drives the two at once and asserts on
//! the *peer's* replica — the copy a person would be looking at.
//!
//! The interleave is real rather than arranged: the peer types after applying
//! the app-hosted document action but before reporting that invocation's
//! terminal success to the agent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the arithmetic \
              here is a wall-clock timeout deadline and a typed-character \
              counter bounded by a short-lived test run, not real user data"
)]

use tilo_automerge::ApplyIncoming;
use ui::time::Duration;

use automerge::AutoCommit;
use tilo_actions::ActionAddress;
use tilo_agent::{Config, Session};
use tilo_content::ContentStore;
use ui::collaboration::{CollabEvent, CollabRequest, DocumentMutation};

use crate::typed_peer;
use typed_peer::{TypedPeer, document_mutation};

const APP: &str = "so.tilo.word-processor";
const RECORD: &str = "file:/var/workspace/Demo/brief.docx";

/// What the person types while the agent's app action is still in flight.
const TYPED: &str = "!";

fn seed_document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("ada".as_bytes()));
    let c = tilo_doc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = tilo_doc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = tilo_doc::DocMut {
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
        dm.splice_text(&p, 0, 0, "Scribe: insert a table");
    }
    doc.commit();
    doc
}

fn blocks(doc: &AutoCommit) -> Vec<tilo_doc::BlockView> {
    let c = tilo_doc::model::open(doc, &automerge::ROOT).expect("schema");
    tilo_doc::model::materialize(doc, &c).blocks
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
        let c = tilo_doc::model::open(doc, &automerge::ROOT).expect("schema");
        let at = blocks(doc)
            .into_iter()
            .find(|b| b.id == block)
            .map_or(0, |b| b.text.chars().count());
        let mut ids = tilo_doc::model::seeded_id_gen(doc, &c);
        let mut dm = tilo_doc::DocMut {
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

/// The live resource tree the Word Processor itself publishes for this
/// document. The table anchor is the final bullet, matching where a person
/// would leave the caret after entering the list.
fn word_processor_resources(doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
    let b = blocks(doc);
    let anchor = b
        .iter()
        .rev()
        .find(|block| block.kind == "list_item")
        .or_else(|| b.last())
        .map(|block| block.id.clone())
        .unwrap_or_default();
    word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: anchor.clone(),
            caret_offset: 0,
            target_blocks: (!anchor.is_empty()).then_some(anchor).into_iter().collect(),
            format_spans: Vec::new(),
            selection: None,
            region: word_processor::actions::RegionKind::Body,
            search_hits: Vec::new(),
            content: ContentStore::memory(),
        },
        doc,
    )
}

fn publish_word_processor_actions(peer: &mut TypedPeer, doc: &AutoCommit) {
    let resources = word_processor_resources(doc);
    let actions = typed_peer::exported_actions(&resources);
    assert!(
        actions.iter().any(|action| {
            action.resource == "word-processor.document" && action.action == "insert-table"
        }),
        "the production Word Processor surface must export insert-table"
    );
    typed_peer::publish_actions(peer, APP, &resources);
}

/// Drive the peer as a real Word Processor host: publish its resource/action
/// tree, resolve broker invocations through its action registry, and answer
/// collaboration traffic. The person types after each agent-authored change,
/// so the app action and acknowledgement edit are genuinely interleaved.
fn drive_peer_and_type(
    peer: &mut TypedPeer,
    replica: &mut AutoCommit,
    head: &str,
) -> (usize, bool, String) {
    peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = tilo_test_env::hang_deadline();
    let mut typed = 0usize;
    let mut table_done = false;
    let mut invocation_actor = None;
    while ui::time::now() < deadline && !(table_done && typed == TYPED.chars().count()) {
        let Some(event) = peer.recv().unwrap() else {
            continue;
        };
        match event {
            CollabEvent::DocumentSnapshotRequested { .. } => {
                let snap = CollabRequest::document_snapshot(replica.save());
                peer.send(&snap).unwrap();
            }
            CollabEvent::Invocation {
                id,
                resource,
                action,
                argument,
                actor,
                context,
                ..
            } => {
                invocation_actor = Some(actor.clone());
                let resources = word_processor_resources(replica);
                let update = typed_peer::service_document_invocation(
                    peer,
                    replica,
                    &resources,
                    word_processor::actions::register,
                    typed_peer::DocumentInvocation {
                        id,
                        address: ActionAddress::new(resource, action),
                        argument: argument.as_deref(),
                        actor: &actor,
                        context: context.as_deref(),
                    },
                );

                // Land a human edit while the broker invocation is still in
                // flight. Only after that change is on the wire does the app
                // host report terminal success to the agent.
                if let Some(ch) = TYPED.chars().nth(typed) {
                    let delta = type_one(replica, head, &ch.to_string());
                    peer.send(&CollabRequest::document_incremental(delta))
                        .unwrap();
                    typed += 1;
                }
                peer.send(&CollabRequest::UpdateInvocation { id, update })
                    .unwrap();
                table_done = blocks(replica).iter().any(|block| block.kind == "cell");
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                replica
                    .load_incoming(&data)
                    .expect("apply the agent's change");

                // The person is typing through all of this. One character
                // per change the agent makes, so the two are interleaved
                // rather than merely overlapping.
                if let Some(ch) = TYPED.chars().nth(typed) {
                    let delta = type_one(replica, head, &ch.to_string());
                    let msg = CollabRequest::document_incremental(delta);
                    peer.send(&msg).unwrap();
                    typed += 1;
                }

                table_done = blocks(replica).iter().any(|block| block.kind == "cell");
            }
            _ => {}
        }
    }
    (
        typed,
        table_done,
        invocation_actor.expect("the broker delivered the Word Processor invocation"),
    )
}

/// Assert the production insert-table action landed and survived the person
/// typing during the invocation. Returns block summaries for later failures.
fn assert_agent_table(replica: &AutoCommit, table_done: bool) -> Vec<(String, String)> {
    let b = blocks(replica);
    let shown: Vec<(String, String)> = b.iter().map(|x| (x.kind.clone(), x.text.clone())).collect();

    assert!(
        table_done,
        "the agent's table never landed; blocks: {shown:?}"
    );
    let mut cells: Vec<(i64, i64, String)> = b
        .iter()
        .filter(|x| x.kind == "cell")
        .map(|x| {
            let n = |k: &str| match x.attrs.get(k) {
                Some(tilo_doc::AttrVal::Int(v)) => *v,
                _ => -1,
            };
            (n("row"), n("col"), x.text.clone())
        })
        .collect();
    cells.sort();
    assert_eq!(
        cells.len(),
        4,
        "insert-table must create its declared 2x2 table"
    );
    assert!(
        cells.iter().all(|cell| cell.2.is_empty()),
        "insert-table unexpectedly invented cell content: {cells:?}"
    );
    shown
}

/// Assert every character the person typed, in order, landed in the
/// heading — not a subset that happened to miss a collision.
fn assert_person_typed(replica: &AutoCommit, typed: usize, shown: &[(String, String)]) {
    // The count is asserted before the text: the keystroke is sent before
    // the app host reports terminal success for the agent's invocation, so
    // seeing all of it proves the human edit occurred during that lifecycle.
    assert_eq!(
        typed,
        TYPED.chars().count(),
        "the person did not type while the app action was in flight: \
         {typed} of {} characters",
        TYPED.chars().count()
    );
    let b = blocks(replica);
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
}

/// Assert both authors are in the history — each change attributed to
/// whoever made it, per change rather than per document — and that every
/// change carries a hash.
fn assert_attribution(replica: &mut AutoCommit, typed: usize, invocation_actor: &str) {
    // Both authors are in the history, so the strip attributes each edit
    // to whoever made it rather than folding them into one.
    let versions = tilo_resource::history::versions(replica);
    let actors: std::collections::HashSet<String> =
        versions.iter().map(|v| v.actor.clone()).collect();
    let action_actor = hex::encode(invocation_actor);
    assert!(
        actors.contains(&action_actor),
        "the app action is not attributed to the broker-authenticated agent actor \
         {invocation_actor}: {actors:?}"
    );
    assert!(
        actors.contains(&hex::encode("ada")),
        "the person's edits are not attributed to them: {actors:?}"
    );

    // And the attribution is per *change*, not per document — which is
    // the property anything drawing per-participant highlights needs.
    //
    // Worth asserting rather than assuming, because the obvious place to
    // look for an author is the wire envelope, and there it genuinely is
    // Document attribution is carried separately from the Automerge bytes.
    // The author is in the delta instead. Every change carries the actor
    // that made it, the peer recovered all of them by applying the raw
    // incremental bytes, and no frame here was signed — so a receiver can
    // attribute an edit from what it already has.
    let scribe = versions.iter().filter(|v| v.actor == action_actor).count();
    let human = versions
        .iter()
        .filter(|v| v.actor == hex::encode("ada"))
        .count();
    assert!(
        scribe >= 1 && human >= typed,
        "per-change attribution is missing: {scribe} change(s) from the agent \
         and {human} from the person, out of {} total",
        versions.len()
    );
    assert!(
        versions
            .iter()
            .all(|v| v.hash.as_ref().iter().any(|byte| *byte != 0)),
        "a change with no hash cannot be joined to what it touched"
    );
}

#[test]
fn a_person_typing_and_the_agent_working_both_land() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("collab.sock");
    let _broker = typed_peer::spawn_with_agent_actions(&sock, &[(APP, "insert-table")]);
    tilo_test_env::wait_until("the relay to bind its socket", || sock.exists());

    let room = typed_peer::room();

    let mut peer = TypedPeer::connect(&sock);
    let hello = CollabRequest::Hello {
        app_id: APP.into(),
        cluster_id: room,
        record: Some(RECORD.into()),
        record_kind: None,
        handle: Some("ada".into()),
    };
    peer.announce(&sock, &hello).unwrap();
    let mut replica = seed_document();
    let head = heading_id(&replica);
    // The action host exists before the delegated agent is launched. Agent
    // construction now consumes the broker's initial catalogue snapshot, so
    // the first document decision has one deterministic routing source.
    publish_word_processor_actions(&mut peer, &replica);

    let cfg = Config {
        endpoint: Some(sock),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: APP.into(),
        cluster: room,
        record: Some(RECORD.into()),
        agent_id: "Scribe".into(),
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
        content: ContentStore::memory(),
    };
    let agent = std::thread::spawn(move || {
        let mut s = Session::connect(cfg).expect("agent connects");
        s.run_until(tilo_test_env::hang_deadline())
            .expect("agent session")
    });

    let (typed, table_done, invocation_actor) = drive_peer_and_type(&mut peer, &mut replica, &head);

    // Give the person's last keystrokes a moment to reach the agent, so a
    // pass here is not just a race the agent happened to win.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        agent.join().unwrap(),
        "the agent reports having acted (typed={typed}, table_done={table_done})"
    );

    let shown = assert_agent_table(&replica, table_done);
    assert_person_typed(&replica, typed, &shown);
    assert_attribution(&mut replica, typed, &invocation_actor);
}
