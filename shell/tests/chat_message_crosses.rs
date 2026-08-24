//! Two people in one workspace have to be able to hold a conversation.
//!
//! # What is real here
//!
//! The broker, the Cap'n Proto socket, the SDK client an app uses, the CRDT,
//! and both `chat::ChatApp` instances are the production ones. A message is
//! typed into a real [`ui::widget::TextInput`] through the app's own
//! `Widget::handle_event`, sent by the same Enter the composer submits on,
//! written into the same document `ui::App` broadcasts, and carried to the
//! second peer as an incremental delta over `sendUnsignedIncremental` — the
//! verb `ui::collaboration` calls for `CollabRequest::SyncIncremental`, which
//! is what `ui::App::broadcast_doc_delta` sends on every tick.
//!
//! **Cap'n Proto is the transport under test, deliberately.** The legacy
//! JSON-over-Unix-socket dialect is still compiled (`ui`'s `legacy-broker`
//! feature) and a test that sets `QUICK_COLLAB_SOCK` exercises it — but
//! `compositor/native.rs` is explicit that in a real session nothing dials it:
//! apps take the capnp descriptor. Nothing below sets that variable, and the
//! bytes are handed to a `runtime_collab::ChannelHandle` obtained by dialling
//! the broker's capnp listener.
//!
//! # What stands in
//!
//! One process, so one `ui::collaboration::CollaborationManager` — it connects
//! from the process-global `QOS_SVC_COLLAB_FD`, and two of them in one process
//! would fight over it. So the shovel thread that manager owns is the piece
//! modelled: the test moves each side's `save_incremental` onto the same
//! channel verb the manager would, and feeds what arrives back through
//! `load_incremental`. Everything on either side of that — the app, the
//! document, the socket, the broker's fan-out, the attribution the broker
//! stamps — is production code.

use std::sync::mpsc;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use automerge::ROOT;
use chat::identity::LocalIdentity;
use chat::mention::Participant;
use chat::ChatApp;
use ui::presence::PresenceCtx;
use ui::permissions::UserId;
use ui::widget::Widget;
use winit::event::{ElementState, Modifiers};
use winit::keyboard::{Key, NamedKey};

use shell::shell::collab_broker::spawn_full;
use shell::shell::collab_broker_capnp::dial_capnp_for_test;

/// A socket pair nothing else in the process will collide with.
fn unique_paths(label: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    let n = std::process::id() as u64 * 1_000_003
        + std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64;
    (
        base.join(format!("qos-{label}-{n}.json.sock")),
        base.join(format!("qos-{label}-{n}.capnp.sock")),
    )
}

/// What the capnp worker thread is asked to do.
enum Cmd {
    /// Put one peer's pending CRDT changes on the wire.
    Publish { who: &'static str, delta: Vec<u8> },
    Stop,
}

enum Report {
    /// Both apps have joined and opened their op subscriptions.
    Ready,
    /// A delta reached a peer, with the attribution the broker stamped on it.
    Arrived {
        who: &'static str,
        bytes: Vec<u8>,
        from_user_id: String,
    },
}

/// Two peers on one broker, each with its own connection and its own
/// subscription. Returns the command sink, the report source, and the join
/// handle so the caller can shut the worker down.
#[allow(clippy::type_complexity)]
fn two_peers_on_one_broker(
    label: &str,
) -> (
    mpsc::Sender<Cmd>,
    mpsc::Receiver<Report>,
    std::thread::JoinHandle<()>,
    shell::shell::collab_broker::BrokerHandle,
) {
    let (json_path, capnp_path) = unique_paths(label);
    let handle = spawn_full(json_path, Some(capnp_path.clone()), None).expect("broker spawns");

    let channel_id = uuid::Uuid::new_v4().to_string();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (rep_tx, rep_rx) = mpsc::channel::<Report>();

    let worker = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            // Two apps in one room. They share a thread only because
            // capnp-rpc futures are `!Send`; on the broker they are two
            // independent peers with two independent subscriptions, which is
            // the part that matters.
            let (client_a, _drv_a) = dial_capnp_for_test(capnp_path.clone())
                .await
                .expect("app A dials the broker");
            let (client_b, _drv_b) = dial_capnp_for_test(capnp_path.clone())
                .await
                .expect("app B dials the broker");
            let sdk_a = runtime_collab::CollabClient::from_raw(client_a);
            let sdk_b = runtime_collab::CollabClient::from_raw(client_b);
            let chan_a = sdk_a
                .join_channel(&channel_id, b"identity-A")
                .await
                .expect("app A joins");
            let chan_b = sdk_b
                .join_channel(&channel_id, b"identity-B")
                .await
                .expect("app B joins");

            let ops_a = chan_a.subscribe().await.expect("A subscribes");
            let ops_b = chan_b.subscribe().await.expect("B subscribes");
            for (who, stream) in [("A", ops_a), ("B", ops_b)] {
                let tx = rep_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(evt) = stream.read().await {
                        if tx
                            .send(Report::Arrived {
                                who,
                                bytes: evt.op,
                                from_user_id: evt.from_user_id,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            let _ = rep_tx.send(Report::Ready);

            loop {
                match cmd_rx.try_recv() {
                    Ok(Cmd::Publish { who, delta }) => {
                        let ch = if who == "A" { &chan_a } else { &chan_b };
                        ch.send_unsigned_incremental(&delta).await.expect("delta goes out");
                    }
                    Ok(Cmd::Stop) => break,
                    Err(mpsc::TryRecvError::Empty) => {
                        // The command channel is synchronous and this task owns
                        // the whole LocalSet, so yield rather than block the
                        // executor the RPC futures run on.
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
        });
    });

    let ready = wait_for(&rep_rx, Duration::from_secs(10), |r| matches!(r, Report::Ready));
    assert!(ready.is_some(), "both apps must join before anything can cross");
    (cmd_tx, rep_rx, worker, handle)
}

/// A live app authoring as `id`. Live, not ephemeral: the ephemeral instance
/// seeds the demo workspace, and a conversation held in fixture content proves
/// nothing about a conversation.
fn peer(id: &str, initials: &str, color: usize) -> ChatApp {
    ChatApp::new_as(LocalIdentity {
        id: id.to_string(),
        // A peer supplies its id and nothing else. Whatever it might put here
        // must never reach a screen — the room names people. The tests below
        // pin that, so this is deliberately a name nobody would choose.
        name: format!("SELF-DECLARED-{id}"),
        initials: initials.to_string(),
        color_idx: color,
    })
}

/// The roster, as the room hands it to an app: `CollabEvent::Context` lands in
/// the presence store, and `adopt_room` is the door an app has onto it.
fn room_says(app: &mut ChatApp, entries: &[(&str, &str)]) {
    let mut ctx = PresenceCtx::empty();
    for (id, name) in entries {
        ctx.set_display_name(UserId(id.to_string()), *name);
    }
    app.adopt_room(&ctx);
}

/// Type `s` into the focused composer through the app's own event path.
fn type_text(app: &mut ChatApp, doc: &mut automerge::AutoCommit, s: &str) {
    for ch in s.chars() {
        send_key(app, doc, Key::Character(ch.to_string().into()));
    }
}

fn send_key(app: &mut ChatApp, doc: &mut automerge::AutoCommit, key: Key) {
    // The event winit itself would have delivered, built by the toolkit's own
    // headless harness rather than by a fourth hand-rolled copy of it.
    let event = ui::headless::key_event(key, ElementState::Pressed);
    let mut handled = false;
    let mut focus_target = None;
    let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
    app.handle_event(
        &event,
        1.0,
        &Modifiers::default(),
        doc,
        &ROOT,
        &mut handled,
        &mut focus_target,
        taffy::NodeId::new(0),
        &taffy,
    );
}

fn sync(app: &mut ChatApp, doc: &mut automerge::AutoCommit) {
    app.sync(doc, &ROOT);
}

/// Take reports until one matches, discarding the rest.
fn wait_for(
    rx: &mpsc::Receiver<Report>,
    deadline: Duration,
    mut matches: impl FnMut(&Report) -> bool,
) -> Option<Report> {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(r) if matches(&r) => return Some(r),
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

/// Ship whatever `from` has written since the last call to the other peer, over
/// the broker, and merge it in. Returns the attribution the broker stamped.
fn cross(
    cmds: &mpsc::Sender<Cmd>,
    reports: &mpsc::Receiver<Report>,
    from: &'static str,
    src: &mut automerge::AutoCommit,
    dst: &mut automerge::AutoCommit,
) -> String {
    let delta = src.save_incremental();
    assert!(!delta.is_empty(), "peer {from} had nothing to send");
    cmds.send(Cmd::Publish { who: from, delta }).expect("worker alive");
    let to = if from == "A" { "B" } else { "A" };
    let got = wait_for(reports, Duration::from_secs(10), |r| {
        matches!(r, Report::Arrived { who, .. } if *who == to)
    })
    .unwrap_or_else(|| panic!("nothing reached peer {to} over the capnp socket"));
    let Report::Arrived { bytes, from_user_id, .. } = got else { unreachable!() };
    dst.load_incremental(&bytes).expect("the delta merges");
    from_user_id
}

/// The headline: a message typed on one machine shows up on the other, said by
/// the person who said it, and the `@name` in it arrives as a reference rather
/// than as prose.
#[test]
fn a_message_typed_on_one_machine_reaches_the_other_attributed_to_its_author() {
    let (cmds, reports, worker, _broker) = two_peers_on_one_broker("chat-crosses");

    let mut doc_a = automerge::AutoCommit::new();
    let mut doc_b = automerge::AutoCommit::new();
    let mut a = peer("u_alice", "AL", 0);
    let mut b = peer("u_bob", "BO", 3);

    // Alice opens the app: an empty starter workspace, and her membership.
    sync(&mut a, &mut doc_a);
    cross(&cmds, &reports, "A", &mut doc_a, &mut doc_b);
    sync(&mut b, &mut doc_b);
    cross(&cmds, &reports, "B", &mut doc_b, &mut doc_a);
    sync(&mut a, &mut doc_a);

    // Nobody has named themselves. This is the property the whole attribution
    // story rests on: a peer that could write its own display name into the
    // shared document would be telling everyone else what to call it, and two
    // peers disagreeing about a third's name is a difference nobody can settle.
    for u in chat::doc::ChatDoc::list_users(&doc_a) {
        assert!(
            u.name.is_empty(),
            "participant {} declared its own name ({:?}) into the shared document",
            u.id,
            u.name
        );
    }

    // The room names them, over the same context stream the presence store is
    // fed from in a live session.
    let roster = [("u_alice", "Ada Lovelace"), ("u_bob", "Bob Kahn")];
    room_says(&mut a, &roster);
    room_says(&mut b, &roster);
    assert_eq!(a.display_name("u_bob"), "Bob Kahn");
    assert!(
        !a.display_name("u_alice").contains("SELF-DECLARED"),
        "a peer's own claim about its name reached the screen"
    );

    // Alice types, mentioning Bob by the name the room gave him, and hits
    // Enter — the composer's own submit.
    type_text(&mut a, &mut doc_a, "@Bob Kahn can you take the parser?");
    send_key(&mut a, &mut doc_a, Key::Named(NamedKey::Enter));

    let from = cross(&cmds, &reports, "A", &mut doc_a, &mut doc_b);
    // The transport carries no author for an unsigned frame — the broker
    // stamps an empty attribution on this path, which is why a message's
    // author is a field in the document and not something read off the wire.
    // Pinned so the next reader does not go looking for it there.
    assert!(
        from.is_empty(),
        "an unsigned delta arrived attributed to {from:?}; if the transport has \
         started naming senders, message attribution can stop relying solely on \
         the document — but nothing may quietly depend on both"
    );
    sync(&mut b, &mut doc_b);

    // Bob sees it, said by Alice.
    let channel = a.active_channel_id().to_string();
    let seen = chat::doc::ChatDoc::list_messages(&doc_b, &channel);
    let msg = seen
        .iter()
        .find(|m| m.text.contains("take the parser"))
        .expect("Alice's message reached Bob");
    assert_eq!(msg.author, "u_alice", "the message is attributed to Alice");
    assert_eq!(
        b.display_name(&msg.author),
        "Ada Lovelace",
        "and it is shown under the name the room gave her"
    );

    // And the mention is structure, not prose.
    assert!(
        msg.mentions_user("u_bob"),
        "the message must know it addressed Bob without anybody re-reading it"
    );
    assert_eq!(
        chat::doc::ChatDoc::message_mentions(&doc_b, &channel, &msg.id)
            .iter()
            .map(|m| m.user.clone())
            .collect::<Vec<_>>(),
        vec!["u_bob".to_string()],
        "the reference is in the document Bob's machine holds"
    );

    let _ = cmds.send(Cmd::Stop);
    let _ = worker.join();
}

/// A mention is answerable without re-reading the text — that is the whole
/// point of storing it as structure.
#[test]
fn a_consumer_enumerates_the_mentions_without_parsing_the_body() {
    let (cmds, reports, worker, _broker) = two_peers_on_one_broker("chat-mention-structure");

    let mut doc_a = automerge::AutoCommit::new();
    let mut doc_b = automerge::AutoCommit::new();
    let mut a = peer("u_alice", "AL", 0);
    let mut b = peer("u_bob", "BO", 3);
    sync(&mut a, &mut doc_a);
    cross(&cmds, &reports, "A", &mut doc_a, &mut doc_b);
    sync(&mut b, &mut doc_b);
    cross(&cmds, &reports, "B", &mut doc_b, &mut doc_a);
    sync(&mut a, &mut doc_a);

    let roster = [("u_alice", "Ada"), ("u_bob", "Bob")];
    room_says(&mut a, &roster);
    room_says(&mut b, &roster);

    type_text(&mut a, &mut doc_a, "@Bob and @Bob again, plus an email a@b.example");
    send_key(&mut a, &mut doc_a, Key::Named(NamedKey::Enter));
    cross(&cmds, &reports, "A", &mut doc_a, &mut doc_b);
    sync(&mut b, &mut doc_b);

    // The question a notification, a badge or an agent asks, answered off the
    // document Bob's machine holds — no body scanned anywhere.
    let mine = chat::doc::ChatDoc::messages_mentioning(&doc_b, "u_bob");
    assert_eq!(mine.len(), 1, "one message addresses Bob");
    let (_, m) = &mine[0];
    assert_eq!(
        m.mentioned_ids().collect::<Vec<_>>(),
        vec!["u_bob", "u_bob"],
        "two references, in the order they read"
    );
    // The offsets are the body's own, so a renderer paints the right words.
    for mention in &m.mentions {
        assert_eq!(mention.slice(&m.text), "@Bob");
    }
    assert!(
        chat::doc::ChatDoc::messages_mentioning(&doc_b, "u_alice").is_empty(),
        "the address in the text is not a mention of anybody"
    );

    let _ = cmds.send(Cmd::Stop);
    let _ = worker.join();
}

/// An agent in the room is addressed exactly the way a colleague is. This is
/// the point of the whole feature: a person hands work to software by naming
/// it, and the software finds out the same way anybody else would.
#[test]
fn mentioning_an_agent_works_the_same_as_mentioning_a_person() {
    let (cmds, reports, worker, _broker) = two_peers_on_one_broker("chat-mention-agent");

    let mut doc_a = automerge::AutoCommit::new();
    let mut doc_b = automerge::AutoCommit::new();
    let mut a = peer("u_alice", "AL", 0);
    let mut b = peer("u_bob", "BO", 3);
    sync(&mut a, &mut doc_a);
    cross(&cmds, &reports, "A", &mut doc_a, &mut doc_b);
    sync(&mut b, &mut doc_b);
    cross(&cmds, &reports, "B", &mut doc_b, &mut doc_a);
    sync(&mut a, &mut doc_a);

    // An agent is admitted to the room and says what it is. The name is still
    // the room's to give.
    chat::doc::ChatDoc::ensure_member(
        &mut doc_a,
        "agent_scribe",
        true,
        chat::model::Presence::Online,
    );
    let roster = [("u_alice", "Ada"), ("u_bob", "Bob"), ("agent_scribe", "Scribe")];
    room_says(&mut a, &roster);
    sync(&mut a, &mut doc_a);

    assert!(
        a.participants().iter().any(|p| p.id == "agent_scribe" && p.is_agent),
        "the picker offers the agent, marked as one"
    );

    type_text(&mut a, &mut doc_a, "@Scribe pick this up");
    send_key(&mut a, &mut doc_a, Key::Named(NamedKey::Enter));
    cross(&cmds, &reports, "A", &mut doc_a, &mut doc_b);
    sync(&mut b, &mut doc_b);

    let addressed = chat::doc::ChatDoc::messages_mentioning(&doc_b, "agent_scribe");
    assert_eq!(addressed.len(), 1, "the agent's own machine can find its work");
    let (_, m) = &addressed[0];
    assert_eq!(m.author, "u_alice");
    assert!(
        m.mentions.iter().any(|x| x.user == "agent_scribe" && x.is_agent),
        "the reference says it names software, not a colleague"
    );

    let _ = cmds.send(Cmd::Stop);
    let _ = worker.join();
}

/// Somebody leaving must not go on being present.
///
/// Presence forgets a peer on leave. Anything that only ever learned from it
/// keeps that peer's name and colour for the rest of the session — still
/// offered in the `@` picker, still painted behind a mention — which is a
/// screen claiming somebody is there who is not.
#[test]
fn a_participant_who_leaves_stops_being_offered() {
    let mut a = peer("u_alice", "AL", 0);
    let mut doc = automerge::AutoCommit::new();
    sync(&mut a, &mut doc);

    // Two guests the room knows about but who never wrote themselves into the
    // workspace document — the transient half of the roster.
    room_says(&mut a, &[("u_alice", "Ada"), ("u_guest", "Grace")]);
    assert!(a.participants().iter().any(|p| p.id == "u_guest"));
    assert_eq!(a.display_name("u_guest"), "Grace");

    // Grace leaves: presence drops her, and so must everything downstream.
    let mut ctx = PresenceCtx::empty();
    ctx.set_display_name(UserId("u_alice".into()), "Ada");
    a.adopt_room(&ctx);

    assert!(
        !a.participants().iter().any(|p| p.id == "u_guest"),
        "a departed guest is still offered in the picker"
    );
    assert_eq!(
        a.display_name("u_guest"),
        "u_guest",
        "a departed guest still carries the name the room gave them"
    );

    // A message that already named her keeps its reference: the mention is a
    // pointer to a participant, and that participant did exist. Leaving a room
    // does not rewrite what was said.
    let msg = chat::mention::resolve(
        "@Grace look",
        &[Participant { id: "u_guest".into(), name: "Grace".into(), is_agent: false }],
    );
    assert_eq!(msg[0].user, "u_guest");
}
