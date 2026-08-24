//! One person, one pointer — proved through the surface a shipped app
//! actually has.
//!
//! A pointer position means nothing without the space it was measured
//! in, and until this the space never reached the far side. An app
//! published its pointer scoped to the room it collaborates in, the
//! capnp presence surface had no keyed cursor call so the scope was
//! dropped on the way out, and the broker synthesised a replacement.
//! Presence is delivered to every app in a room, so each receiver
//! supplied the missing space itself and each supplied its own: one
//! person drawn once per open window, at the same offset in every one.
//!
//! Four things about how these are set up. The first three are what a
//! shortcut would have hidden the fault behind; the fourth is what a
//! shortcut hid *in this file*, for a while, after the fault was fixed.
//!
//! **The capnp transport, not the JSON one.** Both dialects run side
//! by side and only one was broken: the JSON dialect delivered
//! `RemotePresenceUpdate` with the key intact all along, so a proof
//! that points itself at `QUICK_COLLAB_SOCK` exercises the path that
//! was never wrong and reports success while the product duplicates.
//! The sessions here connect the way a sandboxed app connects — an fd
//! in `QOS_SVC_COLLAB_FD`, `runtime_collab::try_connect` on the other
//! side of it — and reach the broker through nothing but its public
//! surface, so what is exercised is what ships.
//!
//! **A publisher publishes.** Where one session's pointer has to reach
//! another, it goes out through `App::broadcast_presence` and over the
//! real `publishPointer` call. That is the leg the key used to be
//! dropped on, so a test that injects the frame further downstream is
//! no longer watching the place it broke.
//!
//! **Two people are two hosts.** The broker attributes every *local*
//! capnp connection to this machine's user — `joinChannel` carries an
//! identity field and the server discards it, resolving the room
//! instead — which is correct, because two windows on one desktop
//! *are* one person. It also means a second local session cannot stand
//! in for a collaborator: two of them are one identity, and a pointer
//! is single-location per identity, so they collapse to one row no
//! matter what the test wants. Where a second *person* is needed, the
//! frame arrives as a remote host's, `peer@<handle>`, through the same
//! `fanout_event` the inbound drain calls once it has decoded one off
//! the wire. Going further back than that — a real `ShellNetwork`, so
//! the drain does the decoding too — means `ShellNetwork::spawn`,
//! which binds listening sockets and advertises over mDNS; an
//! integration test is not a place to put a node on somebody's LAN.
//! That leg is covered in-process instead, by
//! `shell/src/shell/peer_pointer_tests.rs`, which can build a network
//! that talks to nobody.
//!
//! **Nothing is asserted not to be drawn until it is shown to have
//! arrived.** "This window drew no pointer" is the whole point of the
//! file and it is also what a window that was never handed anything
//! looks like — a session whose subscription was still being
//! registered when the frame went past, a broker nobody connected to,
//! a room nobody was in. Every such assertion here is preceded by one
//! that names the very frame in the receiver's own store: the record
//! is there, under the space its publisher chose, at the position it
//! published — and the glyph list is empty anyway. Without that pair,
//! deleting the suppression rule outright leaves the test green.
//!
//! What is counted is glyphs, not stored rows. "How many pointers is
//! this session going to draw" is the question a screenshot answers,
//! and a store holds a peer's position for other reasons — following
//! them, the compositor overlay's own read — without drawing anything.

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ui::app::App;
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::state::State;
use ui::widget::Container;

use shell::shell::collab_broker::{BrokerHandle, CrdtRoute, spawn_full};

/// Where this process's word processor and file manager live, spelled
/// the way `ui::local_surface_id` spells a surface: the app id.
const WORD_PROCESSOR: &str = "dev.qos.word-processor";
const FILE_MANAGER: &str = "dev.qos.files";

/// The handles two remote hosts' frames arrive under. The receiving
/// side derives the identity `peer@<handle>` from them, which is what
/// the duplicated pill on screen was labelled with.
const XAVER: u64 = 11_895_642_303_397_647_149;
const INES: u64 = 42;

/// How long to wait for something that should happen. Generous: the
/// path crosses a shovel thread, a tokio `LocalSet` and an RPC driver,
/// and a slow machine is not a failure.
const PATIENCE: Duration = Duration::from_secs(5);

/// How long to keep pumping once everything expected has arrived,
/// before asserting that something *else* did not. Long enough that a
/// frame in flight would have landed.
const SETTLE: Duration = Duration::from_millis(300);

fn unique_paths(tag: &str) -> (PathBuf, PathBuf) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let base = std::env::temp_dir().join(format!("qos-ptr-{tag}-{}-{stamp}", std::process::id()));
    let json = base.with_extension("json.sock");
    let capnp = base.with_extension("capnp.sock");
    let _ = std::fs::remove_file(&json);
    let _ = std::fs::remove_file(&capnp);
    (json, capnp)
}

fn wait_for_socket(path: &PathBuf) -> UnixStream {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Ok(s) = UnixStream::connect(path) {
            return s;
        }
        assert!(Instant::now() < deadline, "broker never bound {path:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Connect to `path` and hand the fd to the SDK the way the launcher
/// hands one to a sandboxed app, build a session on it, and **wait
/// until the broker has registered that session's presence stream**.
///
/// `surface` goes into the process environment because that is where
/// it comes from in production. A session told its surface by any
/// other route would not be proving the thing that has to hold:
/// publisher and receiver spell a surface the same way without either
/// being handed the other's spelling.
///
/// The wait is the load-bearing part. `App::new` returns once the
/// socket is up, which is not the same moment as "this session is
/// subscribed": the SDK sends its hello onto a queue and another
/// thread joins the channel and opens the four subscriptions
/// afterwards. Presence is fanned out live and never replayed, so a
/// frame published into that gap is not late, it is gone — and a
/// session that never heard anything is indistinguishable from one
/// that heard and declined to draw, which is precisely the distinction
/// every assertion below rests on. Presence is the last subscription
/// the SDK opens, so the broker's count of them rising is evidence
/// that this connection is registered everywhere, and it is evidence
/// rather than a guess at how long that takes.
fn session_on(broker: &BrokerHandle, path: &PathBuf, room: &str, surface: &str) -> App {
    let before = broker.capnp_presence_subscribers();
    let fd = wait_for_socket(path).into_raw_fd();
    // SAFETY: every test in this file is serialised; nothing else
    // reads or writes the environment while this runs.
    unsafe {
        std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
        std::env::set_var("QOS_WORKSPACE_ID", room);
        std::env::set_var("QOS_PACKAGE_ID", surface);
        std::env::remove_var("QUICK_COLLAB_SOCK");
    }
    let app = App::new(State::new(), Box::new(Container::new()));

    let deadline = Instant::now() + PATIENCE;
    while broker.capnp_presence_subscribers() <= before {
        assert!(
            Instant::now() < deadline,
            "the {surface} session connected but the broker never registered its \
             presence subscription; anything published to it would be dropped \
             unnoticed, so there is nothing this test could honestly assert",
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    app
}

/// The identity a remote host's frames are attributed to.
fn peer_id_of(peer: u64) -> String {
    format!("peer@{peer}")
}

/// A remote host's presence frame, put on this broker the way the
/// inbound drain puts one there once it has decoded it off the wire:
/// attributed to the publishing host, addressed to the room, through
/// `fanout_to_route` with nobody excepted.
fn peer_publishes(
    broker: &BrokerHandle,
    peer: u64,
    room: &str,
    key: PresenceKey,
    payload: PresencePayload,
) {
    let event = CollabEvent::RemotePresenceUpdate {
        user_id: peer_id_of(peer),
        key,
        payload,
    };
    broker.fanout_event(&event, None, &CrdtRoute::cluster(room.to_string()));
}

/// The pointers this session would put on its own window — what a
/// screenshot of it would show. A pointer the compositor owns, or one
/// measured in another surface's content, is not drawn here and does
/// not appear.
fn glyphs_drawn_by(app: &App) -> Vec<(String, [f32; 2])> {
    app.presence_ctx()
        .remote_cursors
        .iter()
        .filter(|c| c.painted_by_window())
        .map(|c| (c.user_id.clone(), c.position))
        .collect()
}

/// Every pointer this session holds, across every space one might have
/// arrived under, as `(user, key, position)`.
///
/// This is the half of the store that says *what reached here*,
/// independent of whether anything paints it — so it is what an
/// assertion about not painting has to be paired with. Two rows for
/// one person is that person recorded twice.
fn all_pointer_rows(ctx: &PresenceCtx) -> Vec<(String, PresenceKey, (f32, f32))> {
    ctx.pointers
        .iter()
        .flat_map(|(k, list)| {
            list.iter()
                .map(move |(u, p)| (u.0.clone(), k.clone(), *p))
        })
        .collect()
}

/// Every pointer this session holds for `user`.
fn records_for(ctx: &PresenceCtx, user: &str) -> Vec<(PresenceKey, (f32, f32))> {
    let uid = UserId(user.to_string());
    ctx.pointers
        .iter()
        .flat_map(|(k, list)| {
            list.iter()
                .filter(|(u, _)| *u == uid)
                .map(move |(_, p)| (k.clone(), *p))
        })
        .collect()
}

/// Pump until `ready` holds, or give up. Presence crosses two threads
/// and an RPC driver, so a single tick is a race and a deadline is not.
fn pump_until(apps: &mut [&mut App], ready: &dyn Fn(&[&mut App]) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        for app in apps.iter_mut() {
            app.pump_collab_presence();
        }
        if ready(apps) || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Settle: keep pumping for a fixed spell so a frame that *would*
/// arrive has had every chance to. Used before asserting that nothing
/// arrived, which a short wait would pass for the wrong reason.
fn pump_for(apps: &mut [&mut App], how_long: Duration) {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        for app in apps.iter_mut() {
            app.pump_collab_presence();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Somebody else's pointer, moving on their desktop, arrives here once
/// — and this session does not draw it, because a desktop position is
/// the compositor's to place.
///
/// The counts are the assertion. Before the space travelled, the key
/// was dropped at the capnp door and the position reached the app
/// layer as a bare pair of numbers: nothing was filed under a space at
/// all, and the window drew it as though it were its own.
///
/// The record is checked before the glyph list, and that order is the
/// test: a session that never received the frame also draws no glyph,
/// and would sail past the second assertion on its way to being wrong.
#[test]
#[serial_test::serial]
fn a_desktop_pointer_crosses_the_real_broker_as_one_record_nobody_here_draws() {
    let (json, capnp) = unique_paths("desktop-once");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut peer = session_on(&broker, &capnp, &room, WORD_PROCESSOR);
    let mut here = session_on(&broker, &capnp, &room, FILE_MANAGER);

    // What a compositor republishes for the session it draws: where
    // the pointer is on the desktop, in the room's scene space. Out
    // over the real `publishPointer`, which is the call the key used
    // to be dropped on.
    peer.broadcast_presence(
        PresenceKey::desktop_pointer(&room),
        PresencePayload::Pointer { x: 900.0, y: 300.0 },
    );

    let desktop_key = PresenceKey::desktop_pointer(&room);
    pump_until(&mut [&mut here], &|apps| {
        !all_pointer_rows(apps[0].presence_ctx()).is_empty()
    });

    // Received: one row, from one person, in the space its publisher
    // chose, at the position they published. Everything after this is
    // about what is *done* with a frame that is now known to be here.
    let rows = all_pointer_rows(here.presence_ctx());
    assert_eq!(
        rows.len(),
        1,
        "one peer moved one pointer, and it must arrive as one record — \
         not one per space it was guessed to be in — got {rows:?}"
    );
    let (peer_id, key, at) = rows[0].clone();
    assert_eq!(at, (900.0, 300.0), "the position must arrive as published");
    assert_eq!(
        key, desktop_key,
        "and in the space its publisher chose, not one synthesised on the way",
    );

    pump_for(&mut [&mut here], SETTLE);
    assert_eq!(
        records_for(here.presence_ctx(), &peer_id).len(),
        1,
        "still one record after settling: a second space arriving late is \
         the same person filed twice",
    );
    assert!(
        glyphs_drawn_by(&here).is_empty(),
        "a desktop position is the compositor's to place; this window has \
         the frame — it is in the store, right above — and drawing it as \
         well would be the same person twice — {:?}",
        glyphs_drawn_by(&here),
    );

    drop(peer);
    drop(here);
    drop(broker);
}

/// Two windows open, a peer pointing inside one of them: one glyph,
/// and it is in the window they are actually over.
///
/// This is the photographed failure. Both windows drew it, at the same
/// offset into each, because both were handed the same frame and
/// neither had been told which content the numbers were measured in.
///
/// So the frame reaching the *other* window is not incidental to this
/// test, it is the premise, and it is asserted: the file manager holds
/// the word processor's pointer, at the word processor's coordinates,
/// and paints nothing. Without that assertion the empty glyph list
/// says only that the file manager was quiet, which is also what it
/// would be if it had never been sent anything — and the whole
/// suppression rule could be deleted with this test still green.
#[test]
#[serial_test::serial]
fn a_pointer_inside_one_surface_is_drawn_by_that_surface_and_no_other() {
    let (json, capnp) = unique_paths("one-surface");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut peer = session_on(&broker, &capnp, &room, WORD_PROCESSOR);
    let mut wp = session_on(&broker, &capnp, &room, WORD_PROCESSOR);
    let mut fm = session_on(&broker, &capnp, &room, FILE_MANAGER);

    let in_the_word_processor = PresenceKey::surface_pointer(WORD_PROCESSOR);
    peer.broadcast_presence(
        in_the_word_processor.clone(),
        PresencePayload::Pointer { x: 214.0, y: 34.0 },
    );

    // Both windows are in the room and both are subscribed, so both
    // are sent this. Wait for both to have it — the one that will draw
    // it, and the one that must not.
    pump_until(&mut [&mut wp, &mut fm], &|apps| {
        !glyphs_drawn_by(apps[0]).is_empty()
            && !all_pointer_rows(apps[1].presence_ctx()).is_empty()
    });

    let drawn = glyphs_drawn_by(&wp);
    assert_eq!(
        drawn.len(),
        1,
        "the word processor is the surface those coordinates were \
         measured in, and draws the pointer once — got {drawn:?}"
    );
    assert_eq!(drawn[0].1, [214.0, 34.0]);
    let peer_id = drawn[0].0.clone();

    // The premise of the assertion below: the file manager was handed
    // the same frame. Named exactly — same person, same space, same
    // position as the window that did draw it.
    let heard = all_pointer_rows(fm.presence_ctx());
    assert_eq!(
        heard,
        vec![(peer_id.clone(), in_the_word_processor.clone(), (214.0, 34.0))],
        "the file manager must have been handed this pointer for its \
         empty glyph list below to mean it refused, rather than that it \
         was never told",
    );

    pump_for(&mut [&mut wp, &mut fm], SETTLE);
    assert!(
        glyphs_drawn_by(&fm).is_empty(),
        "and having been handed it, the file manager must not place it: \
         those pixels are somewhere in a word processor, and the same \
         offset into a file list is not that place — {:?}",
        glyphs_drawn_by(&fm),
    );
    // And the window that legitimately drew it did not gain a second
    // copy while we waited.
    assert_eq!(
        glyphs_drawn_by(&wp).len(),
        1,
        "one frame is one glyph in the surface it belongs to — {:?}",
        glyphs_drawn_by(&wp),
    );

    drop(peer);
    drop(wp);
    drop(fm);
    drop(broker);
}

/// Two people are two pointers, each under their own name. The rule
/// that stops one person being drawn twice must not also collapse two
/// people into one.
///
/// Two *people*, so two hosts. A second local session is not a second
/// person and cannot be made into one from out here: the broker
/// resolves every local capnp connection to this machine's user
/// (`joinChannel`'s identity argument is read for well-formedness and
/// discarded), and a pointer is single-location per identity, so two
/// local publishers overwrite each other's row and arrive as one. That
/// is the product being right, not the test being unlucky — two
/// windows on one desktop *are* one person — so the collaborators here
/// are remote, and their frames enter where a remote host's enter.
#[test]
#[serial_test::serial]
fn two_remote_peers_in_one_surface_stay_two_pointers_with_their_own_names() {
    let (json, capnp) = unique_paths("two-peers");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut here = session_on(&broker, &capnp, &room, WORD_PROCESSOR);

    let in_the_word_processor = PresenceKey::surface_pointer(WORD_PROCESSOR);
    peer_publishes(
        &broker,
        XAVER,
        &room,
        in_the_word_processor.clone(),
        PresencePayload::Pointer { x: 10.0, y: 20.0 },
    );
    peer_publishes(
        &broker,
        INES,
        &room,
        in_the_word_processor.clone(),
        PresencePayload::Pointer {
            x: 300.0,
            y: 400.0,
        },
    );

    pump_until(&mut [&mut here], &|apps| glyphs_drawn_by(apps[0]).len() >= 2);
    pump_for(&mut [&mut here], SETTLE);

    let mut drawn = glyphs_drawn_by(&here);
    drawn.sort_by(|a, b| a.1[0].partial_cmp(&b.1[0]).expect("finite"));
    assert_eq!(drawn.len(), 2, "two people, two pointers — got {drawn:?}");
    assert_eq!(
        drawn
            .iter()
            .map(|(u, _)| u.clone())
            .collect::<Vec<_>>(),
        vec![peer_id_of(XAVER), peer_id_of(INES)],
        "each under the name of the host that sent it; two pointers drawn \
         under one name is two people collapsed into one",
    );
    assert_eq!(drawn[0].1, [10.0, 20.0]);
    assert_eq!(drawn[1].1, [300.0, 400.0]);

    // And two people are two rows in the store, not one row that the
    // second publisher moved.
    let rows = all_pointer_rows(here.presence_ctx());
    assert_eq!(
        rows.len(),
        2,
        "the single-location rule is per person, not per space — one row \
         here means one of them overwrote the other — got {rows:?}"
    );

    drop(here);
    drop(broker);
}

/// A peer who moves out of a surface leaves nothing in it.
///
/// The pointer moves rather than multiplying. It reaches a session
/// under more than one space over that session's life, and a row left
/// behind under the old one is a second dot nothing can retire:
/// retirement ages out what nobody restates, and a peer who is still
/// moving restates the new row.
///
/// "Nothing left in the word processor" is checked against where they
/// went, not against silence. A pointer that simply vanished — the
/// frame dropped, the record deleted — would empty the glyph list too,
/// so the move is asserted first and positively: one record, under the
/// file manager, at the new coordinates.
#[test]
#[serial_test::serial]
fn a_peer_that_moves_to_another_surface_leaves_nothing_in_the_first() {
    let (json, capnp) = unique_paths("moves-away");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut peer = session_on(&broker, &capnp, &room, WORD_PROCESSOR);
    let mut here = session_on(&broker, &capnp, &room, WORD_PROCESSOR);

    let in_the_word_processor = PresenceKey::surface_pointer(WORD_PROCESSOR);
    let in_the_file_manager = PresenceKey::surface_pointer(FILE_MANAGER);

    peer.broadcast_presence(
        in_the_word_processor.clone(),
        PresencePayload::Pointer { x: 214.0, y: 34.0 },
    );
    pump_until(&mut [&mut here], &|apps| {
        !glyphs_drawn_by(apps[0]).is_empty()
    });
    let drawn = glyphs_drawn_by(&here);
    assert_eq!(
        drawn.len(),
        1,
        "precondition: the peer is in this surface to begin with — got {drawn:?}"
    );
    let peer_id = drawn[0].0.clone();

    // The same person, now pointing inside the file manager instead.
    peer.broadcast_presence(
        in_the_file_manager.clone(),
        PresencePayload::Pointer { x: 230.0, y: 34.0 },
    );
    pump_until(&mut [&mut here], &|apps| {
        records_for(apps[0].presence_ctx(), &peer_id)
            .iter()
            .any(|(k, _)| *k == PresenceKey::surface_pointer(FILE_MANAGER))
    });
    pump_for(&mut [&mut here], SETTLE);

    // Where they went — asserted positively, so "nothing is left
    // behind" cannot be satisfied by nothing being anywhere.
    assert_eq!(
        records_for(here.presence_ctx(), &peer_id),
        vec![(in_the_file_manager.clone(), (230.0, 34.0))],
        "one person is one pointer record however many surfaces they have \
         crossed, and it is at the surface and offset they moved to",
    );
    assert!(
        glyphs_drawn_by(&here).is_empty(),
        "they are pointing at a file manager now, and this window is a word \
         processor; a dot left in it is one person in two places — {:?}",
        glyphs_drawn_by(&here),
    );

    drop(peer);
    drop(here);
    drop(broker);
}
