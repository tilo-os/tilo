//! One person, one pointer — proved over the transport a session
//! actually uses.
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
//! These run against the **real** broker over the **real** capnp
//! socket, because the fault was in that transport and only that
//! transport. There is a legacy JSON dialect alongside it which
//! carried the key correctly all along, so a proof that points itself
//! at `QUICK_COLLAB_SOCK` exercises the path that was never broken and
//! reports success while the product duplicates. The sessions here
//! connect the way a sandboxed app connects — an fd in
//! `QOS_SVC_COLLAB_FD`, `runtime_collab::try_connect` on the other
//! side of it.
//!
//! What is counted is glyphs, not stored rows. "How many pointers is
//! this session going to draw" is the question a screenshot answers,
//! and a store can hold a peer's position for other reasons —
//! following them, the compositor overlay's own read — without drawing
//! anything.

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ui::app::App;
use ui::collaboration::PresencePayload;
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::state::State;
use ui::widget::Container;

use shell::shell::collab_broker::spawn_full;

/// Where this process's word processor and file manager live, spelled
/// the way `ui::local_surface_id` spells a surface: the app id.
const WORD_PROCESSOR: &str = "dev.qos.word-processor";
const FILE_MANAGER: &str = "dev.qos.files";

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
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = UnixStream::connect(path) {
            return s;
        }
        assert!(Instant::now() < deadline, "broker never bound {path:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Connect to `path` and hand the fd to the SDK the way the launcher
/// hands one to a sandboxed app, then build a session on it.
///
/// `surface` goes into the process environment because that is where
/// it comes from in production. A session told its surface by any
/// other route would not be proving the thing that has to hold:
/// publisher and receiver spell a surface the same way without either
/// being handed the other's spelling.
fn session_on(path: &PathBuf, room: &str, surface: &str) -> App {
    let fd = wait_for_socket(path).into_raw_fd();
    // SAFETY: every test in this file is serialised; nothing else
    // reads or writes the environment while this runs.
    unsafe {
        std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
        std::env::set_var("QOS_WORKSPACE_ID", room);
        std::env::set_var("QOS_PACKAGE_ID", surface);
        std::env::remove_var("QUICK_COLLAB_SOCK");
    }
    App::new(State::new(), Box::new(Container::new()))
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

/// Every pointer this session holds for `user`, across every space it
/// might have arrived under. More than one is the same person recorded
/// twice.
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
    let deadline = Instant::now() + Duration::from_secs(3);
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

/// Somebody else's pointer, moving on their desktop, arrives here once
/// — and this session does not draw it, because a desktop position is
/// the compositor's to place.
///
/// The counts are the assertion. Before the space travelled, the key
/// was dropped at the capnp door and the position reached the app
/// layer as a bare pair of numbers: nothing was filed under a space at
/// all, and the window drew it as though it were its own.
#[test]
#[serial_test::serial]
fn a_desktop_pointer_crosses_the_real_broker_as_one_record_nobody_here_draws() {
    let (json, capnp) = unique_paths("desktop-once");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut peer = session_on(&capnp, &room, WORD_PROCESSOR);
    let mut here = session_on(&capnp, &room, FILE_MANAGER);

    // What a compositor republishes for the session it draws: where
    // the pointer is on the desktop, in the room's scene space.
    peer.broadcast_presence(
        PresenceKey::desktop_pointer(&room),
        PresencePayload::Pointer { x: 900.0, y: 300.0 },
    );

    pump_until(&mut [&mut here], &|apps| {
        !apps[0].presence_ctx().remote_cursors.is_empty()
    });

    let who: Vec<String> = here
        .presence_ctx()
        .remote_cursors
        .iter()
        .map(|c| c.user_id.clone())
        .collect();
    assert_eq!(
        who.len(),
        1,
        "one peer moved one pointer; this session holds {who:?}"
    );
    let peer_id = who[0].clone();

    let rows = records_for(here.presence_ctx(), &peer_id);
    assert_eq!(
        rows.len(),
        1,
        "one person is one pointer record, not one per space it was \
         guessed to be in — got {rows:?}"
    );
    assert_eq!(
        rows[0].1,
        (900.0, 300.0),
        "the position must arrive as published",
    );
    assert_eq!(
        rows[0].0,
        PresenceKey::desktop_pointer(&room),
        "and in the space its publisher chose, not one synthesised on the way",
    );
    assert!(
        glyphs_drawn_by(&here).is_empty(),
        "a desktop position is the compositor's to place; this window \
         drawing it as well is the same person twice — {:?}",
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
#[test]
#[serial_test::serial]
fn a_pointer_inside_one_surface_is_drawn_by_that_surface_and_no_other() {
    let (json, capnp) = unique_paths("one-surface");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut peer = session_on(&capnp, &room, WORD_PROCESSOR);
    let mut wp = session_on(&capnp, &room, WORD_PROCESSOR);
    let mut fm = session_on(&capnp, &room, FILE_MANAGER);

    peer.broadcast_presence(
        PresenceKey::surface_pointer(WORD_PROCESSOR),
        PresencePayload::Pointer { x: 214.0, y: 34.0 },
    );

    pump_until(&mut [&mut wp, &mut fm], &|apps| {
        !glyphs_drawn_by(apps[0]).is_empty()
    });

    let drawn = glyphs_drawn_by(&wp);
    assert_eq!(
        drawn.len(),
        1,
        "the word processor is the surface those coordinates were \
         measured in, and draws the pointer once — got {drawn:?}"
    );
    assert_eq!(drawn[0].1, [214.0, 34.0]);
    assert!(
        glyphs_drawn_by(&fm).is_empty(),
        "the file manager was handed the same frame and must not place \
         it: those pixels are somewhere in a word processor, and the \
         same offset into a file list is not that place — {:?}",
        glyphs_drawn_by(&fm),
    );

    drop(peer);
    drop(wp);
    drop(fm);
    drop(broker);
}

/// Two people are two pointers, each under their own name. The rule
/// that stops one person being drawn twice must not also collapse two
/// people into one.
#[test]
#[serial_test::serial]
fn two_peers_in_one_surface_stay_two_pointers_with_their_own_names() {
    let (json, capnp) = unique_paths("two-peers");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut first = session_on(&capnp, &room, WORD_PROCESSOR);
    let mut second = session_on(&capnp, &room, WORD_PROCESSOR);
    let mut here = session_on(&capnp, &room, WORD_PROCESSOR);

    first.broadcast_presence(
        PresenceKey::surface_pointer(WORD_PROCESSOR),
        PresencePayload::Pointer { x: 10.0, y: 20.0 },
    );
    second.broadcast_presence(
        PresenceKey::surface_pointer(WORD_PROCESSOR),
        PresencePayload::Pointer { x: 300.0, y: 400.0 },
    );

    pump_until(&mut [&mut here], &|apps| glyphs_drawn_by(apps[0]).len() >= 2);

    let mut drawn = glyphs_drawn_by(&here);
    drawn.sort_by(|a, b| a.1[0].partial_cmp(&b.1[0]).expect("finite"));
    assert_eq!(drawn.len(), 2, "two people, two pointers — got {drawn:?}");
    assert_ne!(
        drawn[0].0, drawn[1].0,
        "two pointers drawn under one name is two people collapsed into one",
    );
    assert_eq!(drawn[0].1, [10.0, 20.0]);
    assert_eq!(drawn[1].1, [300.0, 400.0]);

    drop(first);
    drop(second);
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
#[test]
#[serial_test::serial]
fn a_peer_that_moves_to_another_surface_leaves_nothing_in_the_first() {
    let (json, capnp) = unique_paths("moves-away");
    let broker = spawn_full(json.clone(), Some(capnp.clone()), None).expect("spawn broker");
    let room = uuid::Uuid::new_v4().to_string();

    let mut peer = session_on(&capnp, &room, WORD_PROCESSOR);
    let mut here = session_on(&capnp, &room, WORD_PROCESSOR);

    peer.broadcast_presence(
        PresenceKey::surface_pointer(WORD_PROCESSOR),
        PresencePayload::Pointer { x: 214.0, y: 34.0 },
    );
    pump_until(&mut [&mut here], &|apps| {
        !glyphs_drawn_by(apps[0]).is_empty()
    });
    let drawn = glyphs_drawn_by(&here);
    assert_eq!(
        drawn.len(),
        1,
        "precondition: the peer is in this surface to begin with"
    );
    let peer_id = drawn[0].0.clone();

    // The same person, now pointing inside the file manager instead.
    peer.broadcast_presence(
        PresenceKey::surface_pointer(FILE_MANAGER),
        PresencePayload::Pointer { x: 230.0, y: 34.0 },
    );
    pump_until(&mut [&mut here], &|apps| {
        glyphs_drawn_by(apps[0]).is_empty()
    });

    assert!(
        glyphs_drawn_by(&here).is_empty(),
        "they are pointing at a file manager now; a dot left in the \
         word processor is one person in two places — {:?}",
        glyphs_drawn_by(&here),
    );
    let rows = records_for(here.presence_ctx(), &peer_id);
    assert_eq!(
        rows.len(),
        1,
        "one person is one pointer record however many surfaces they \
         have crossed — got {rows:?}"
    );

    drop(peer);
    drop(here);
    drop(broker);
}
