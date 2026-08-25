//! A direct message is a room with two people in it, and the third person in
//! the workspace does not receive it.
//!
//! # What is real here
//!
//! The broker, the Cap'n Proto socket, the SDK client an app uses, the CRDT and
//! all three `chat::ChatApp` instances are the production ones. A message is
//! typed into a real [`ui::widget::TextInput`] through the app's own
//! `Widget::handle_event`, sent by the same Enter the composer submits on, and
//! carried as an incremental delta over `sendUnsignedIncremental` — the verb
//! `ui::collaboration` calls for `CollabRequest::SyncIncremental`.
//!
//! The direct room's id is not invented by this test. It comes from
//! [`qos_reference::DirectRoom`], the same pure derivation the app uses, so
//! what is being exercised is the addressing production would compute — not a
//! fixture that happens to line up.
//!
//! # The half that matters
//!
//! "It arrived" is the easy half and on its own it is worth little. The
//! property this file exists for is the other one: **Carol, who is in the
//! workspace room with both of them, receives nothing.** That is asserted on
//! Carol's own subscription and on Carol's own replica, not on a rendering
//! pass or a filter.
//!
//! And an assertion that somebody heard nothing is the exact shape of a check
//! that passes because it cannot hear. So before every such assertion this
//! file proves Carol's ear works, on the same broker, in the same run, by
//! putting a message she *must* hear through it first. A Carol who is silent
//! because her subscription never opened fails that step instead of quietly
//! passing the one after it.
//!
//! # What stands in
//!
//! One process, so one `ui::collaboration::CollaborationManager` — it connects
//! from the process-global `QOS_SVC_COLLAB_FD`, and several of them in one
//! process would fight over it. So the shovel thread that manager owns is the
//! piece modelled: the test moves each side's `save_incremental` onto the same
//! channel verb the manager would, and feeds what arrives back through
//! `load_incremental`. Everything on either side of that — the apps, the
//! documents, the socket, the broker's per-channel fan-out — is production
//! code, and the fan-out is the thing under test.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use automerge::ROOT;
use chat::ChatApp;
use chat::identity::LocalIdentity;
use qos_reference::DirectRoom;
use ui::permissions::UserId;
use ui::presence::PresenceCtx;
use ui::widget::Widget;
use winit::event::{ElementState, Modifiers};
use winit::keyboard::{Key, NamedKey};

use shell::shell::collab_broker::spawn_full;
use shell::shell::collab_broker_capnp::dial_capnp_for_test;

/// Participant ids in the shape chat actually mints them: `u_` plus sixteen
/// hex characters of an Ed25519 public key (`apps/chat/src/identity.rs`).
/// Realistic on purpose — a direct room is addressed by participant id, and a
/// fixture using a friendlier id would not be exercising the values production
/// hands to the derivation.
const ALICE: &str = "u_1f4c3a9b2d7e5061";
const BOB: &str = "u_9a0b1c2d3e4f5061";
const CAROL: &str = "u_5d6e7f8091a2b3c4";

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
    /// Put one peer's pending changes on the wire, addressed to one room.
    Publish {
        who: &'static str,
        room: String,
        delta: Vec<u8>,
    },
    Stop,
}

enum Report {
    /// Everybody has joined and opened their subscriptions.
    Ready,
    /// A delta reached a peer, on the room it was subscribed to.
    Arrived {
        who: &'static str,
        room: String,
        bytes: Vec<u8>,
    },
}

/// Stand up the broker and put people on it, each subscribed to exactly the
/// rooms named for them.
///
/// Membership is the argument because membership is the subject: the whole
/// claim being tested is that being in a room is what decides delivery, so a
/// harness that hard-coded who was where would be assuming the answer.
///
/// Two rooms on one connection is not something this test adds: the capnp
/// service mints a fresh channel handle per `join_channel` and derives every
/// route from that handle's own channel id, so a peer holding two channels is
/// already two rooms on one socket.
#[allow(clippy::type_complexity)]
fn peers_on_one_broker(
    label: &str,
    membership: Vec<(&'static str, Vec<String>)>,
) -> (
    mpsc::Sender<Cmd>,
    mpsc::Receiver<Report>,
    std::thread::JoinHandle<()>,
    shell::shell::collab_broker::BrokerHandle,
) {
    let (json_path, capnp_path) = unique_paths(label);
    let handle = spawn_full(json_path, Some(capnp_path.clone()), None).expect("broker spawns");

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (rep_tx, rep_rx) = mpsc::channel::<Report>();

    let worker = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            // The apps share a thread only because capnp-rpc futures are
            // `!Send`; on the broker they are independent peers with
            // independent subscriptions, which is the part that matters.
            let mut channels: HashMap<(&'static str, String), runtime_collab::ChannelHandle> =
                HashMap::new();
            for (who, rooms) in &membership {
                let (client, _drv) = dial_capnp_for_test(capnp_path.clone())
                    .await
                    .unwrap_or_else(|_| panic!("{who} dials the broker"));
                // The driver has to outlive the connection; leaking it is what
                // the two-peer test does for the same reason.
                std::mem::forget(_drv);
                let sdk = runtime_collab::CollabClient::from_raw(client);
                for room in rooms {
                    let ch = sdk
                        .join_channel(room, who.as_bytes())
                        .await
                        .unwrap_or_else(|_| panic!("{who} joins {room}"));
                    let stream = ch
                        .subscribe()
                        .await
                        .unwrap_or_else(|_| panic!("{who} subscribes to {room}"));
                    let tx = rep_tx.clone();
                    let room_name = room.clone();
                    let who = *who;
                    tokio::task::spawn_local(async move {
                        while let Ok(evt) = stream.read().await {
                            if tx
                                .send(Report::Arrived {
                                    who,
                                    room: room_name.clone(),
                                    bytes: evt.op,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                    channels.insert((who, room.clone()), ch);
                }
            }
            let _ = rep_tx.send(Report::Ready);

            loop {
                match cmd_rx.try_recv() {
                    Ok(Cmd::Publish { who, room, delta }) => {
                        let ch = channels
                            .get(&(who, room.clone()))
                            .unwrap_or_else(|| panic!("{who} is not in {room}"));
                        ch.send_unsigned_incremental(&delta)
                            .await
                            .expect("delta goes out");
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

    let ready = wait_for(&rep_rx, Duration::from_secs(20), |r| {
        matches!(r, Report::Ready)
    });
    assert!(
        ready.is_some(),
        "everybody must join before anything can cross"
    );
    (cmd_tx, rep_rx, worker, handle)
}

/// A live app authoring as `id`. Live, not ephemeral: the ephemeral instance
/// seeds the demo workspace, and a conversation held in fixture content proves
/// nothing about a conversation.
fn peer(id: &str) -> ChatApp {
    ChatApp::new_as(LocalIdentity {
        id: id.to_string(),
        // A peer supplies its id and nothing else. Whatever it puts here must
        // never reach a screen — the room names people.
        name: format!("SELF-DECLARED-{id}"),
        initials: "ZZ".to_string(),
        color_idx: 0,
    })
}

/// The roster, as the room hands it to an app.
fn room_says(app: &mut ChatApp, entries: &[(&str, &str)]) {
    let mut ctx = PresenceCtx::empty();
    for (id, name) in entries {
        ctx.set_display_name(UserId(id.to_string()), *name);
    }
    app.adopt_room(&ctx);
}

fn type_text(app: &mut ChatApp, doc: &mut automerge::AutoCommit, s: &str) {
    for ch in s.chars() {
        send_key(app, doc, Key::Character(ch.to_string().into()));
    }
}

fn send_key(app: &mut ChatApp, doc: &mut automerge::AutoCommit, key: Key) {
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

/// Publish `src`'s pending changes into `room`, and merge whatever reaches each
/// listed recipient into the document they hold for that room.
///
/// Returns how many recipients received something, so a caller can tell "it was
/// delivered to nobody" from "nobody was listening".
fn publish(
    cmds: &mpsc::Sender<Cmd>,
    reports: &mpsc::Receiver<Report>,
    from: &'static str,
    room: &str,
    src: &mut automerge::AutoCommit,
    recipients: &mut [(&'static str, &mut automerge::AutoCommit)],
) -> usize {
    let delta = src.save_incremental();
    assert!(!delta.is_empty(), "peer {from} had nothing to send");
    cmds.send(Cmd::Publish {
        who: from,
        room: room.to_string(),
        delta,
    })
    .expect("worker alive");

    let mut delivered = 0;
    for (who, doc) in recipients.iter_mut() {
        let want = *who;
        let got = wait_for(reports, Duration::from_secs(10), |r| {
            matches!(r, Report::Arrived { who, room: r2, .. } if *who == want && r2 == room)
        });
        if let Some(Report::Arrived { bytes, .. }) = got {
            doc.load_incremental(&bytes).expect("the delta merges");
            delivered += 1;
        }
    }
    delivered
}

/// Whatever reached `who` on `room` within `patience`, if anything. Used for
/// the assertions about somebody hearing *nothing*, which is why it returns
/// rather than panicking.
fn heard_anything(
    reports: &mpsc::Receiver<Report>,
    who: &'static str,
    room: &str,
    patience: Duration,
) -> bool {
    wait_for(reports, patience, |r| {
        matches!(r, Report::Arrived { who: w, room: r2, .. } if *w == who && r2 == room)
    })
    .is_some()
}

/// Find a body in a room's conversation on the document `doc` holds.
fn conversation(doc: &automerge::AutoCommit) -> Vec<String> {
    chat::doc::ChatDoc::list_messages(doc, qos_reference::ROOM_CONVERSATION)
        .into_iter()
        .map(|m| m.text)
        .collect()
}

/// The headline, both halves. A message Alice writes to Bob directly reaches
/// Bob's replica, and does not reach Carol — who is in the workspace room with
/// both of them and is listening on it throughout.
#[test]
fn a_direct_message_reaches_the_other_person_and_nobody_else_in_the_workspace() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let pair = DirectRoom::between(ALICE, BOB).expect("two people name a room");
    let direct = pair.room_id();
    assert_ne!(
        direct, workspace,
        "a direct room must be a different room from the workspace it was started in"
    );

    // Alice and Bob are in the workspace *and* in their direct room; Carol is
    // in the workspace only. That last line is the fixture: she is a colleague,
    // present, listening, and not in this conversation.
    let (cmds, reports, worker, _broker) = peers_on_one_broker(
        "dm-only-the-pair",
        vec![
            ("alice", vec![workspace.clone(), direct.clone()]),
            ("bob", vec![workspace.clone(), direct.clone()]),
            ("carol", vec![workspace.clone()]),
        ],
    );

    // The workspace: all three of them, each with the document for that room.
    let mut ws_alice = automerge::AutoCommit::new();
    let mut ws_carol = automerge::AutoCommit::new();
    let mut a_ws = peer(ALICE);
    let mut c_ws = peer(CAROL);

    // The direct room: a chat window on a room whose membership is two. It is
    // an ordinary chat app on an ordinary room — that is the whole design.
    let mut dm_alice = automerge::AutoCommit::new();
    let mut dm_bob = automerge::AutoCommit::new();
    let mut a_dm = peer(ALICE);
    let mut b_dm = peer(BOB);

    let roster = [(ALICE, "Ada Lovelace"), (BOB, "Bob Kahn"), (CAROL, "Carol Shaw")];
    for app in [&mut a_ws, &mut c_ws, &mut a_dm, &mut b_dm] {
        room_says(app, &roster);
    }

    // --- Carol's ear, proved working -------------------------------------
    //
    // Before asserting that Carol hears nothing, prove she can hear. A silent
    // subscription and a correctly-scoped one look identical from here, and
    // that is exactly how a check comes to pass without being able to see.
    sync(&mut a_ws, &mut ws_alice);
    type_text(&mut a_ws, &mut ws_alice, "morning all");
    send_key(&mut a_ws, &mut ws_alice, Key::Named(NamedKey::Enter));
    let heard = publish(
        &cmds,
        &reports,
        "alice",
        &workspace,
        &mut ws_alice,
        &mut [("carol", &mut ws_carol)],
    );
    assert_eq!(
        heard, 1,
        "Carol's workspace subscription delivered nothing, so every assertion \
         below about her hearing nothing would pass for the wrong reason"
    );
    sync(&mut c_ws, &mut ws_carol);
    assert!(
        conversation(&ws_carol).iter().any(|t| t == "morning all"),
        "Carol's replica must hold what was said in the room she is in"
    );

    // --- the direct message ----------------------------------------------
    sync(&mut a_dm, &mut dm_alice);
    sync(&mut b_dm, &mut dm_bob);
    type_text(&mut a_dm, &mut dm_alice, "can we talk about the offer");
    send_key(&mut a_dm, &mut dm_alice, Key::Named(NamedKey::Enter));
    let heard = publish(
        &cmds,
        &reports,
        "alice",
        &direct,
        &mut dm_alice,
        &mut [("bob", &mut dm_bob)],
    );
    assert_eq!(heard, 1, "the direct message never reached Bob at all");
    sync(&mut b_dm, &mut dm_bob);

    // Bob has it, attributed to Alice, under the name the room gave her.
    let seen = chat::doc::ChatDoc::list_messages(&dm_bob, qos_reference::ROOM_CONVERSATION);
    let msg = seen
        .iter()
        .find(|m| m.text.contains("about the offer"))
        .expect("Alice's direct message reached Bob's replica");
    assert_eq!(msg.author, ALICE, "the message is attributed to Alice");
    assert_eq!(b_dm.display_name(&msg.author), "Ada Lovelace");

    // --- and Carol has nothing -------------------------------------------
    //
    // Nothing on her subscription, and nothing in her replica. Both, because
    // they fail differently: a delta that reached her and was not merged would
    // pass the second check alone.
    assert!(
        !heard_anything(&reports, "carol", &workspace, Duration::from_secs(2)),
        "a direct message between two other people arrived on Carol's workspace subscription"
    );
    sync(&mut c_ws, &mut ws_carol);
    let carols = conversation(&ws_carol);
    assert!(
        !carols.iter().any(|t| t.contains("about the offer")),
        "Carol's replica holds a message she was never addressed in: {carols:?}"
    );

    let _ = cmds.send(Cmd::Stop);
    let _ = worker.join();
}

/// The reply comes back the same way, so the room carries a conversation
/// rather than one person shouting into it. Same room id, computed from the
/// other side of the pair — which is the property that stops two people
/// holding two different rooms with each other.
#[test]
fn the_other_person_replies_into_the_same_room_they_were_written_to() {
    let workspace = uuid::Uuid::new_v4().to_string();
    // Bob names Ada; Ada named Bob. Whichever way round, one room.
    let from_bob = DirectRoom::between(BOB, ALICE).unwrap().room_id();
    let from_alice = DirectRoom::between(ALICE, BOB).unwrap().room_id();
    assert_eq!(
        from_bob, from_alice,
        "each of them opened a conversation with the other and they are not in one room"
    );

    let (cmds, reports, worker, _broker) = peers_on_one_broker(
        "dm-reply",
        vec![
            ("alice", vec![workspace.clone(), from_alice.clone()]),
            ("bob", vec![workspace.clone(), from_bob.clone()]),
        ],
    );

    let mut dm_alice = automerge::AutoCommit::new();
    let mut dm_bob = automerge::AutoCommit::new();
    let mut a = peer(ALICE);
    let mut b = peer(BOB);
    let roster = [(ALICE, "Ada Lovelace"), (BOB, "Bob Kahn")];
    room_says(&mut a, &roster);
    room_says(&mut b, &roster);

    sync(&mut a, &mut dm_alice);
    type_text(&mut a, &mut dm_alice, "are you free");
    send_key(&mut a, &mut dm_alice, Key::Named(NamedKey::Enter));
    assert_eq!(
        publish(&cmds, &reports, "alice", &from_alice, &mut dm_alice, &mut [("bob", &mut dm_bob)]),
        1,
    );
    sync(&mut b, &mut dm_bob);

    type_text(&mut b, &mut dm_bob, "after three");
    send_key(&mut b, &mut dm_bob, Key::Named(NamedKey::Enter));
    assert_eq!(
        publish(&cmds, &reports, "bob", &from_bob, &mut dm_bob, &mut [("alice", &mut dm_alice)]),
        1,
        "Bob's reply did not reach Alice — the two of them are not in one room"
    );
    sync(&mut a, &mut dm_alice);

    let both = conversation(&dm_alice);
    assert!(
        both.iter().any(|t| t == "are you free") && both.iter().any(|t| t == "after three"),
        "one conversation, both sides of it: {both:?}"
    );

    let _ = cmds.send(Cmd::Stop);
    let _ = worker.join();
}

/// A third person's direct room is a third room.
///
/// Alice holds two conversations at once. What she says to Carol must not
/// appear in the one she is holding with Bob — and Bob is in the workspace
/// with all three of them throughout, so "he did not see it" is a fact about
/// the room he is in and not about what happened to be on screen.
#[test]
fn what_alice_says_to_carol_does_not_reach_bob() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let alice_bob = DirectRoom::between(ALICE, BOB).unwrap().room_id();
    let alice_carol = DirectRoom::between(ALICE, CAROL).unwrap().room_id();
    assert_ne!(
        alice_bob, alice_carol,
        "two of Alice's conversations resolved to one room"
    );

    let (cmds, reports, worker, _broker) = peers_on_one_broker(
        "dm-third-room",
        vec![
            (
                "alice",
                vec![workspace.clone(), alice_bob.clone(), alice_carol.clone()],
            ),
            ("bob", vec![workspace.clone(), alice_bob.clone()]),
            ("carol", vec![workspace.clone(), alice_carol.clone()]),
        ],
    );

    let mut ab_alice = automerge::AutoCommit::new();
    let mut ab_bob = automerge::AutoCommit::new();
    let mut ac_alice = automerge::AutoCommit::new();
    let mut ac_carol = automerge::AutoCommit::new();
    let mut a_to_bob = peer(ALICE);
    let mut a_to_carol = peer(ALICE);
    let mut b = peer(BOB);
    let mut c = peer(CAROL);
    let roster = [(ALICE, "Ada"), (BOB, "Bob"), (CAROL, "Carol")];
    for app in [&mut a_to_bob, &mut a_to_carol, &mut b, &mut c] {
        room_says(app, &roster);
    }

    // Bob's ear, proved working, on the room he is actually in. Without this
    // the assertion below would pass just as well for a subscription that
    // never opened.
    sync(&mut a_to_bob, &mut ab_alice);
    type_text(&mut a_to_bob, &mut ab_alice, "just between us");
    send_key(&mut a_to_bob, &mut ab_alice, Key::Named(NamedKey::Enter));
    assert_eq!(
        publish(&cmds, &reports, "alice", &alice_bob, &mut ab_alice, &mut [("bob", &mut ab_bob)]),
        1,
        "Bob heard nothing on his own direct room, so nothing below proves anything"
    );
    sync(&mut b, &mut ab_bob);
    assert!(conversation(&ab_bob).iter().any(|t| t == "just between us"));

    // Alice now writes to Carol, in the room those two share.
    sync(&mut a_to_carol, &mut ac_alice);
    type_text(&mut a_to_carol, &mut ac_alice, "bob must not read this");
    send_key(&mut a_to_carol, &mut ac_alice, Key::Named(NamedKey::Enter));
    assert_eq!(
        publish(&cmds, &reports, "alice", &alice_carol, &mut ac_alice, &mut [("carol", &mut ac_carol)]),
        1,
        "the message to Carol never reached her"
    );
    sync(&mut c, &mut ac_carol);
    assert!(conversation(&ac_carol).iter().any(|t| t == "bob must not read this"));

    // Bob has it on neither of the rooms he is in.
    for room in [&alice_bob, &workspace] {
        assert!(
            !heard_anything(&reports, "bob", room, Duration::from_secs(2)),
            "Alice's conversation with Carol arrived on a room Bob is in ({room})"
        );
    }
    sync(&mut b, &mut ab_bob);
    let bobs = conversation(&ab_bob);
    assert!(
        !bobs.iter().any(|t| t.contains("must not read this")),
        "Bob's replica holds a message from a conversation he is not in: {bobs:?}"
    );

    let _ = cmds.send(Cmd::Stop);
    let _ = worker.join();
}
