//! The agent against a real socket: the production Cap'n Proto broker on a
//! temp path, a peer that holds the document the way the word processor does
//! (same route, answers a directed snapshot request, absorbs incremental
//! changes), and the agent joining late, reading the instruction out of the
//! document, and carrying it out through the app's published action — asserted
//! on the peer's replica, never on the agent's own.

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

use tilo_automerge::ApplyIncoming;
use ui::time::Duration;

use automerge::AutoCommit;
use tilo_actions::{ActionAddress, Resource};
use tilo_agent::{Config, Session};
use tilo_content::ContentStore;
use ui::collaboration::{CollabEvent, CollabRequest, DocumentMutation, PresencePayload};
use uuid::Uuid;

use crate::typed_peer;
use typed_peer::{TypedPeer as UnixStream, document_mutation, recv, send};

const APP: &str = "so.tilo.word-processor";
const RECORD: &str = "file:/var/workspace/Demo/brief.docx";

/// The capability an inviter issues an agent for the app it was invited
/// to. The agent holds no role, so this grant is the whole of what it may
/// enumerate and perform.
fn granted(name: &str) -> ui::permissions::PermissionContext {
    ui::permissions::PermissionContext::agent_granted_at(
        name,
        &[ui::permissions::TreePath::root().app("so.tilo.word-processor".to_string())],
        ui::permissions::KindSet::READ_UNDOABLE,
    )
}

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
    let c = word_processor::model::init(&mut doc).expect("a Writer body");
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
    collab_types::CommitNamed::commit_edit(&mut doc, typed_peer::WP_DOCUMENT, "body");
    doc
}

/// A document with a title paragraph and an image, and one instruction
/// naming two operations on the app's production action surface.
fn seed_sequence_document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("quinten".as_bytes()));
    let c = word_processor::model::init(&mut doc).expect("a Writer body");
    let mut ids = tilo_doc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let title = dm.insert_block(None, "heading");
        dm.set_attr(&title, "level", 1_i64);
        dm.splice_text(&title, 0, 0, "Launch plan");
        let img = dm.insert_block(Some(&title), "image");
        let p = dm.insert_block(Some(&img), "paragraph");
        dm.splice_text(&p, 0, 0, "Scribe: centre the title, then insert a table");
    }
    collab_types::CommitNamed::commit_edit(&mut doc, typed_peer::WP_DOCUMENT, "body");
    doc
}

/// As [`seed_sequence_document`], but the second thing asked for is
/// something nothing in the room can do.
fn seed_unanswerable_sequence_document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("quinten".as_bytes()));
    let c = word_processor::model::init(&mut doc).expect("a Writer body");
    let mut ids = tilo_doc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = tilo_doc::DocMut {
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
            "Scribe: align centre and then juggle three plates",
        );
    }
    collab_types::CommitNamed::commit_edit(&mut doc, typed_peer::WP_DOCUMENT, "body");
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
    let c = word_processor::model::init(&mut doc).expect("a Writer body");
    let mut ids = tilo_doc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, "Home");
        let q = dm.insert_block(Some(&p), "paragraph");
        dm.splice_text(&q, 0, 0, "This folder is empty");
    }
    collab_types::CommitNamed::commit_edit(&mut doc, typed_peer::WP_DOCUMENT, "body");
    doc
}

/// [`seed_document`] under the colliding actor, so that merging it into
/// the foreign document fails for real instead of quietly combining two
/// unrelated pages.
fn seed_colliding_document() -> AutoCommit {
    seed_document_as(COLLIDING_ACTOR)
}

fn blocks(doc: &AutoCommit) -> Vec<tilo_doc::BlockView> {
    let c = word_processor::model::open(doc).expect("schema");
    tilo_doc::model::materialize(doc, &c).blocks
}

/// The live resource tree the Word Processor publishes for this document.
/// The fixture's caret rests on the final bullet when there is one (the
/// natural insertion point for a table), otherwise on the first block.
fn word_processor_resources(doc: &AutoCommit) -> Vec<Resource> {
    let b = blocks(doc);
    let anchor = b
        .iter()
        .rev()
        .find(|block| block.kind == "list_item")
        .or_else(|| b.first())
        .map(|block| block.id.clone())
        .unwrap_or_default();
    word_processor::actions::published_for(
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

fn publish_word_processor_actions(peer: &mut UnixStream, doc: &AutoCommit) {
    let resources = word_processor_resources(doc);
    let exported = typed_peer::exported_actions(&resources);
    assert!(
        exported.iter().any(|action| {
            action.resource == typed_peer::WP_DOCUMENT && action.action == "insert-table-here"
        }),
        "the production Word Processor surface must export insert-table-here"
    );
    typed_peer::publish_actions(peer, APP, &resources);
}

fn service_word_processor_invocation(
    peer: &mut UnixStream,
    replica: &mut AutoCommit,
    id: u64,
    invocation: typed_peer::DocumentInvocation<'_>,
) {
    let resources = word_processor_resources(replica);
    let update = typed_peer::service_document_invocation(
        peer,
        replica,
        &resources,
        word_processor::actions::register,
        invocation,
    );
    send(peer, &CollabRequest::UpdateInvocation { id, update }).unwrap();
}

/// Drive the peer exactly as the app would: answer the broker's directed
/// request with a full snapshot, apply every incremental change to the replica, and
/// watch for the table. Returns the peer, replica, whether it settled into the
/// table, whether Scribe's own presence was observed, and the authenticated
/// actor attached to the broker invocation.
fn drive_peer_to_table(
    mut peer: UnixStream,
    mut replica: AutoCommit,
) -> (UnixStream, AutoCommit, bool, bool, String) {
    peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = tilo_test_env::hang_deadline();
    let mut saw_scribe_presence = false;
    let mut done = false;
    let mut invocation_actor = None;
    // Presence and document events use independent typed subscriptions, so
    // their delivery is intentionally not globally ordered. Keep draining
    // until both guarantees have been observed instead of treating a fast
    // document update as permission to stop listening for presence.
    while ui::time::now() < deadline && !(done && saw_scribe_presence) {
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
                // presence as well as remote arrivals.
                if user_id == typed_peer::principal_id() {
                    continue;
                }
                assert_eq!(
                    user_id, "agent:scribe",
                    "presence names the authenticated agent actor, distinct from a person's display name"
                );
                saw_scribe_presence = true;
            }
            CollabEvent::DocumentSnapshotRequested { .. } => {
                let snap = CollabRequest::document_snapshot(replica.save());
                send(&mut peer, &snap).unwrap();
            }
            CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Caret { .. },
                ..
            } => {
                assert_eq!(user_id, "agent:scribe");
            }
            CollabEvent::Invocation {
                id,
                invocation,
                resource,
                action,
                argument,
                actor,
                context,
                ..
            } => {
                invocation_actor = Some(actor.clone());
                service_word_processor_invocation(
                    &mut peer,
                    &mut replica,
                    id,
                    typed_peer::DocumentInvocation {
                        invocation,
                        address: ActionAddress::new(resource, action),
                        argument: argument.as_deref(),
                        actor: &actor,
                        context: context.as_deref(),
                    },
                );
            }
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                replica.load_incoming(&data).unwrap();
                let b = blocks(&replica);
                let has_cells = b.iter().any(|x| x.kind == "cell");
                let has_instruction = b.iter().any(|x| x.text.starts_with("Scribe:"));
                if has_cells && !has_instruction {
                    done = true;
                }
            }
            _ => {}
        }
    }
    (
        peer,
        replica,
        done,
        saw_scribe_presence,
        invocation_actor.expect("the broker delivered the Word Processor invocation"),
    )
}

/// Assert the replica settled into exactly the production action's 2x2 table,
/// leaving unrelated content intact, and that every change is attributed to
/// the agent's own actor.
fn assert_table_and_attribution(replica: &mut AutoCommit, invocation_actor: &str) {
    // The insertion is the app's declared 2x2 table; existing content stays.
    let b = blocks(replica);
    assert_eq!(b[0].kind, "heading");
    assert_eq!(b[0].text, "Launch plan");
    let cells: Vec<&tilo_doc::BlockView> = b.iter().filter(|x| x.kind == "cell").collect();
    assert_eq!(cells.len(), 4, "insert-table declares a 2x2 table");
    assert!(cells.iter().all(|cell| cell.text.is_empty()));
    assert_eq!(
        b.iter().filter(|block| block.kind == "list_item").count(),
        3,
        "the action does not rewrite unrelated bullets"
    );

    // Every change the agent made is attributed to it. Filtered rather than
    // positionally skipped: `tilo_doc::model::init` adopts the schema from its
    // own shared genesis, under a fixed well-known actor, ahead of the
    // seed's own commit — both pre-date anything the agent did.
    let genesis = tilo_doc::model::schema_genesis_actor_hex();
    let seed = hex::encode("quinten");
    let actors: std::collections::HashSet<String> = tilo_resource::history::versions(replica)
        .into_iter()
        .map(|v| v.actor)
        .filter(|a| *a != genesis && *a != seed)
        .map(|a| ui::history_source::actor_handle(&a).unwrap_or(a))
        .collect();
    assert_eq!(
        actors,
        std::collections::HashSet::from(["Scribe".to_owned(), invocation_actor.to_owned()]),
        "the broker action carries the authenticated invocation actor and the acknowledgement edit carries Scribe's document actor"
    );
}

/// Each case spawns its own relay on its own socket.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the end-to-end proofs keep the broker, agent, and peer assertions together"
)]
fn the_agent_inserts_a_table_on_its_own_documents_replica_even_among_others() {
    // Case `the_agent_inserts_a_table_on_the_peers_replica`.
    {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("collab.sock");
        let _broker = typed_peer::spawn(&sock);
        // The broker binds on the spawning thread.
        assert!(sock.exists(), "relay never bound");

        let room = typed_peer::room();

        // The peer: what the word processor is on the wire.
        let mut peer = UnixStream::connect(&sock);
        let hello = CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: Some(RECORD.into()),
            record_kind: None,
            handle: Some("quinten".into()),
        };
        send(&mut peer, &hello).unwrap();
        let replica = seed_document();
        // A real app publishes its action surface when the document opens, not
        // after a prospective invoker announces itself. Publishing here also
        // removes the race where the agent can inspect the snapshot before the
        // peer has processed its arrival event.
        // The Hello returned once the broker acknowledged the peer's route,
        // and publishing returned once the catalogue offered its actions, so
        // the agent cannot race either.
        publish_word_processor_actions(&mut peer, &replica);

        // The agent, on its own thread, once.
        let cfg = Config {
            endpoint: Some(sock),
            endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
            app_id: APP.into(),
            cluster: room,
            record: Some(RECORD.into()),
            agent_id: "Scribe".into(),
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
            permissions: granted("Scribe"),
            content: ContentStore::memory(),
        };
        let agent = std::thread::spawn(move || {
            let mut s = Session::connect(cfg).expect("agent connects");
            s.run_until(tilo_test_env::hang_deadline())
                .expect("agent session")
        });

        let (mut peer, mut replica, done, saw_scribe_presence, invocation_actor) =
            drive_peer_to_table(peer, replica);
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

        // The successful task is subsequently recorded as a marker in the same
        // real document stream. Keep consuming after the visible table result:
        // history evidence must arrive even though the user-facing work already
        // looks complete.
        let deadline = tilo_test_env::hang_deadline();
        while ui::time::now() < deadline && ui::read_runs(&replica).is_empty() {
            let Some(event) = recv(&mut peer) else {
                continue;
            };
            if let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) {
                replica.load_incoming(&data).unwrap();
            }
        }
        let runs = ui::read_runs(&replica);
        let run = runs.last().expect("the completed agent task is recorded");
        assert_eq!(run.instruction.as_deref(), Some("insert a table"));
        let issuer = hex::encode("quinten");
        assert_eq!(run.issued_by.as_deref(), Some(issuer.as_str()));
        assert!(
            run.decision
                .as_deref()
                .is_some_and(|decision| decision.contains("switched off")),
            "the fallback decision remains reviewable: {run:?}"
        );
        assert!(
            !run.steps.is_empty(),
            "the run must retain an exact change range for undo/review"
        );
        let history = ui::HistorySource::new().entries(&mut replica);
        assert!(
            history.iter().any(|entry| {
                entry.context.as_ref().is_some_and(|context| {
                    context.purpose.as_deref() == Some("insert a table")
                        && context
                            .group
                            .as_ref()
                            .is_some_and(|group| group.kind == "agent-run")
                })
            }),
            "the live history source exposes the task context for review"
        );
        let mut open = [(APP.to_string(), RECORD.to_string(), &mut replica)];
        let report = ui::undo_run(
            run,
            Some(&hex::encode("Scribe")),
            &ui::AuthorDirectory::new(),
            &mut open,
        );
        assert!(
            report.is_complete(),
            "the recorded task can be undone as one run: {report:?}"
        );

        assert_table_and_attribution(&mut replica, &invocation_actor);
    }

    // Case `the_room_holds_more_than_one_document_and_the_agent_finds_its_own`.
    {
        assert_seeds_collide();

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("collab.sock");
        let _broker = typed_peer::spawn(&sock);
        // The broker binds on the spawning thread.
        assert!(sock.exists(), "relay never bound");
        let room = typed_peer::room();

        // Both Hellos return only once the broker acknowledged each route.
        let (files_thread, files_stop, wp_peer, replica) =
            connect_files_and_word_processor(&sock, room);

        // A room, and no record to tell the room's documents apart.
        let cfg = Config {
            endpoint: Some(sock),
            endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
            app_id: APP.into(),
            cluster: room,
            record: None,
            agent_id: "Scribe".into(),
            name: "Scribe".into(),
            pace: Duration::from_millis(10),
            once: true,
            model: false,
            model_timeout: Duration::from_secs(1),
            await_document: Duration::ZERO,
            settle: Duration::ZERO,
            permissions: granted("Scribe"),
            content: ContentStore::memory(),
        };
        let agent = std::thread::spawn(move || {
            let mut s = Session::connect(cfg).expect("agent connects");
            s.run_until(tilo_test_env::hang_deadline())
                .expect("the agent must survive another app's document")
        });

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

        // The production insertion landed without consuming the existing list.
        let b = blocks(&replica);
        assert_eq!(
            b.iter().filter(|x| x.kind == "cell").count(),
            4,
            "the app's insert-table action makes one 2x2 table"
        );
        assert!(
            b.iter().any(|block| block.text == "Venue - The Hall"),
            "the unrelated bullets survive: {b:?}"
        );

        drop(files_stop);
        files_thread
            .join()
            .expect("the file manager thread must not panic");
    }
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
        a.merge_incoming(&mut b).is_err(),
        "the two seeds must collide on their shared actor id, or this \
         test proves nothing about a document the agent cannot merge"
    );
}

/// Connect the file manager and the word processor to the same room —
/// neither with a record, so nothing but the app id tells their documents
/// apart. The file manager keeps answering on its own thread until the
/// returned sender is dropped.
fn connect_files_and_word_processor(
    sock: &std::path::Path,
    room: Uuid,
) -> (
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Sender<()>,
    UnixStream,
    AutoCommit,
) {
    // The file manager: a real document, in this room, that is not the
    // agent's business and never mentions it.
    // A typed collaboration manager stays on its owning thread.
    let files_socket = sock.to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let files_thread = std::thread::spawn(move || {
        let mut files_peer = UnixStream::connect(&files_socket);
        send(
            &mut files_peer,
            &CollabRequest::Hello {
                app_id: "so.tilo.file-explorer".into(),
                cluster_id: room,
                record: None,
                record_kind: None,
                handle: Some("files".into()),
            },
        )
        .unwrap();
        ready_tx.send(()).unwrap();
        drive_files_peer(files_peer, seed_foreign(), &stop_rx);
    });
    ready_rx.recv_timeout(tilo_test_env::hang_bound()).unwrap();

    // The word processor, holding the page with the instruction on it.
    let mut wp_peer = UnixStream::connect(sock);
    send(
        &mut wp_peer,
        &CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: None,
            record_kind: None,
            handle: Some("quinten".into()),
        },
    )
    .unwrap();
    let replica = seed_colliding_document();
    (files_thread, stop_tx, wp_peer, replica)
}

/// A file-manager connection on another route must never receive the directed
/// request for the word-processor document. It answers any request for its
/// own until `stop` disconnects, or a hang bound passes.
fn drive_files_peer(
    mut files_peer: UnixStream,
    mut files_doc: AutoCommit,
    stop: &std::sync::mpsc::Receiver<()>,
) {
    files_peer.set_read_timeout(Some(Duration::from_millis(50)));
    let deadline = tilo_test_env::hang_deadline();
    while ui::time::now() < deadline
        && matches!(stop.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
    {
        let Some(event) = recv(&mut files_peer) else {
            continue;
        };
        if let CollabEvent::DocumentSnapshotRequested { .. } = event {
            let snap = CollabRequest::document_snapshot(files_doc.save());
            send(&mut files_peer, &snap).unwrap();
        }
    }
}

/// The word processor answers the directed request, then watches for its
/// instruction to resolve into a table.
fn drive_word_processor_to_table(
    mut wp_peer: UnixStream,
    mut replica: AutoCommit,
) -> (AutoCommit, bool) {
    wp_peer.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = tilo_test_env::hang_deadline();
    let mut done = false;
    let mut answered = false;
    while ui::time::now() < deadline && !done {
        let Some(event) = recv(&mut wp_peer) else {
            continue;
        };
        match event {
            CollabEvent::DocumentSnapshotRequested { .. } if !answered => {
                answered = true;
                publish_word_processor_actions(&mut wp_peer, &replica);
                let snap = CollabRequest::document_snapshot(replica.save());
                send(&mut wp_peer, &snap).unwrap();
            }
            CollabEvent::Invocation {
                id,
                invocation,
                resource,
                action,
                argument,
                actor,
                context,
                ..
            } => {
                service_word_processor_invocation(
                    &mut wp_peer,
                    &mut replica,
                    id,
                    typed_peer::DocumentInvocation {
                        invocation,
                        address: ActionAddress::new(resource, action),
                        argument: argument.as_deref(),
                        actor: &actor,
                        context: context.as_deref(),
                    },
                );
            }
            // The agent's edits. A delta from the file manager's document
            // would not apply here, so ignore what does not.
            event @ CollabEvent::Document { .. } => {
                let Some(DocumentMutation::Incremental(data)) = document_mutation(&event) else {
                    continue;
                };
                if replica.load_incoming(&data).is_err() {
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
    // The broker binds on the spawning thread.
    assert!(sock.exists(), "relay never bound");

    let cfg = Config {
        endpoint: Some(sock),
        endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
        app_id: APP.into(),
        cluster: typed_peer::room(),
        // The shape of the mistake: a room that exists, on a record
        // nobody in it is looking at.
        record: Some("file:/var/workspace/Demo/nobody-has-this.docx".into()),
        agent_id: "Scribe".into(),
        name: "Scribe".into(),
        pace: Duration::from_millis(10),
        once: true,
        model: false,
        model_timeout: Duration::from_secs(1),
        await_document: Duration::from_millis(1500),
        settle: Duration::ZERO,
        permissions: granted("Scribe"),
        content: ContentStore::memory(),
    };
    let mut session = Session::connect(cfg).expect("agent connects");

    let began = ui::time::now();
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

/// Each case spawns its own relay on its own socket.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the scenarios assert ordered application effects, broker attribution and untouched refusal"
)]
fn a_sequence_in_one_instruction_runs_whole_in_order_or_is_refused_untouched() {
    // Case `the_agent_carries_out_a_sequence_written_as_one_instruction`.
    // One request, two things to do: each step runs in order against the
    // document the step before it left, and the instruction paragraph goes
    // away only once every step in it is done.
    {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("collab.sock");
        let _broker = typed_peer::spawn(&sock);
        // The broker binds on the spawning thread.
        assert!(sock.exists(), "relay never bound");
        let room = typed_peer::room();
        let mut peer = UnixStream::connect(&sock);
        let hello = CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: Some(RECORD.into()),
            record_kind: None,
            handle: Some("quinten".into()),
        };
        // Returns once the broker acknowledged the peer's route.
        send(&mut peer, &hello).unwrap();
        let mut replica = seed_sequence_document();

        let cfg = Config {
            endpoint: Some(sock),
            endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
            app_id: APP.into(),
            cluster: room,
            record: Some(RECORD.into()),
            agent_id: "Scribe".into(),
            name: "Scribe".into(),
            pace: Duration::from_millis(10),
            once: true,
            // The deterministic floor splits the two clauses; see `Session::decide`.
            model: false,
            model_timeout: Duration::from_secs(1),
            await_document: Duration::ZERO,
            settle: Duration::ZERO,
            permissions: granted("Scribe"),
            content: ContentStore::memory(),
        };
        let agent = std::thread::spawn(move || {
            let mut s = Session::connect(cfg).expect("agent connects");
            s.run_until(tilo_test_env::hang_deadline())
                .expect("agent session")
        });

        peer.set_read_timeout(Some(Duration::from_millis(200)));
        let deadline = tilo_test_env::hang_deadline();
        let mut done = false;
        while ui::time::now() < deadline && !done {
            let Some(event) = recv(&mut peer) else {
                continue;
            };
            match event {
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    publish_word_processor_actions(&mut peer, &replica);
                    let snap = CollabRequest::document_snapshot(replica.save());
                    send(&mut peer, &snap).unwrap();
                }
                CollabEvent::Invocation {
                    id,
                    invocation,
                    resource,
                    action,
                    argument,
                    actor,
                    context,
                    ..
                } => {
                    service_word_processor_invocation(
                        &mut peer,
                        &mut replica,
                        id,
                        typed_peer::DocumentInvocation {
                            invocation,
                            address: ActionAddress::new(resource, action),
                            argument: argument.as_deref(),
                            actor: &actor,
                            context: context.as_deref(),
                        },
                    );
                }
                event @ CollabEvent::Document { .. } => {
                    let Some(DocumentMutation::Incremental(data)) = document_mutation(&event)
                    else {
                        continue;
                    };
                    replica.load_incoming(&data).unwrap();
                    let b = blocks(&replica);
                    let title_aligned = b.first().is_some_and(|x| {
                        matches!(x.attrs.get("align"), Some(tilo_doc::AttrVal::Str(v)) if v == "center")
                    });
                    let has_table = b.iter().filter(|x| x.kind == "cell").count() == 4;
                    let has_instruction = b.iter().any(|x| x.text.starts_with("Scribe:"));
                    if title_aligned && has_table && !has_instruction {
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

        // Both production actions ran; only the completed plan removes the
        // instruction.
        let b = blocks(&replica);
        assert_eq!(b[0].kind, "heading");
        assert_eq!(b[0].text, "Launch plan");
        assert!(
            matches!(b[0].attrs.get("align"), Some(tilo_doc::AttrVal::Str(v)) if v == "center"),
            "the first catalogue action aligned the title it named"
        );

        assert_eq!(
            b.iter().filter(|x| x.kind == "cell").count(),
            4,
            "the second catalogue action inserted exactly one 2x2 table"
        );
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

    // Case `an_unanswerable_step_refuses_the_whole_sequence_untouched`.
    // A sequence naming something nothing in the room can do is refused
    // *whole*, before either step is touched — never the first step applied
    // and the second one silently dropped.
    {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("collab.sock");
        let _broker = typed_peer::spawn(&sock);
        // The broker binds on the spawning thread.
        assert!(sock.exists(), "relay never bound");

        let room = typed_peer::room();

        let mut peer = UnixStream::connect(&sock);
        let hello = CollabRequest::Hello {
            app_id: APP.into(),
            cluster_id: room,
            record: Some(RECORD.into()),
            record_kind: None,
            handle: Some("quinten".into()),
        };
        // Returns once the broker acknowledged the peer's route.
        send(&mut peer, &hello).unwrap();
        let mut replica = seed_unanswerable_sequence_document();

        let cfg = Config {
            endpoint: Some(sock),
            endpoint_identity: Some(typed_peer::endpoint_identity("Scribe")),
            app_id: APP.into(),
            cluster: room,
            record: Some(RECORD.into()),
            agent_id: "Scribe".into(),
            name: "Scribe".into(),
            pace: Duration::from_millis(10),
            once: true,
            model: false,
            model_timeout: Duration::from_secs(1),
            await_document: Duration::ZERO,
            settle: Duration::ZERO,
            permissions: granted("Scribe"),
            content: ContentStore::memory(),
        };
        let agent = std::thread::spawn(move || {
            let mut s = Session::connect(cfg).expect("agent connects");
            s.run_until(tilo_test_env::hang_deadline())
                .expect("agent session")
        });

        peer.set_read_timeout(Some(Duration::from_millis(200)));
        let deadline = tilo_test_env::hang_deadline();
        let mut done = false;
        while ui::time::now() < deadline && !done {
            let Some(event) = recv(&mut peer) else {
                continue;
            };
            match event {
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    publish_word_processor_actions(&mut peer, &replica);
                    let snap = CollabRequest::document_snapshot(replica.save());
                    send(&mut peer, &snap).unwrap();
                }
                CollabEvent::Invocation {
                    resource, action, ..
                } => {
                    panic!(
                        "the whole sequence must be refused before dispatch, but invoked {resource}#{action}"
                    );
                }
                event @ CollabEvent::Document { .. } => {
                    let Some(DocumentMutation::Incremental(data)) = document_mutation(&event)
                    else {
                        continue;
                    };
                    replica
                        .load_incoming(&data)
                        .expect("apply the agent's change");
                    let b = blocks(&replica);
                    if b.iter().any(|x| x.text.starts_with("Scribe: I can't")) {
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
            !b[0].attrs.contains_key("align"),
            "the first catalogue action was not dispatched before refusal"
        );
        assert!(
            b.iter().any(|x| x.text.starts_with("Scribe: I can't")),
            "the refusal names what could not be done: {:?}",
            b.iter().map(|x| x.text.clone()).collect::<Vec<_>>()
        );
    }
}
