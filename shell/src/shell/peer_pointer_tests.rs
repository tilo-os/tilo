//! One person, one pointer — counted on the path a session actually
//! runs.
//!
//! A pointer position means nothing without the space it was measured
//! in, and the space never used to reach the far side. An app
//! published its pointer scoped to the room it collaborates in, which
//! names the audience and not the space; the capnp presence surface
//! had no keyed cursor call, so `publishCursor(x, y)` dropped even
//! that and the broker synthesised a replacement. Presence is
//! delivered to every app in a room, so each receiver supplied the
//! missing space itself and each supplied its own — one person drawn
//! once per open window, at the same offset in every one.
//!
//! Three things about how these are set up, each of which a shortcut
//! would have hidden the fault behind.
//!
//! **The capnp transport, not the JSON one.** Both dialects run
//! side by side and only one was broken: the JSON dialect delivered
//! `RemotePresenceUpdate` with the key intact all along, so a proof
//! that points itself at `QUICK_COLLAB_SOCK` exercises the path that
//! was never wrong and reports success while the product duplicates.
//! The sessions here join the way a sandboxed app joins — an fd in
//! `QOS_SVC_COLLAB_FD`, `runtime_collab::try_connect` on the other
//! side of it.
//!
//! **The peer is a remote host.** The broker attributes every *local*
//! connection to the machine's user, which is correct — two windows
//! on one desktop are one person — and it means a second local
//! session cannot stand in for a collaborator. A peer's frames arrive
//! through the inbound drain as `peer@<handle>`, which is what the
//! duplicated pill on screen was labelled with, so that is where these
//! inject.
//!
//! **Glyphs are counted, not rows.** "How many pointers will this
//! session draw" is the question a screenshot answers. The store holds
//! a peer's position for other reasons too — the compositor overlay's
//! own read, following somebody — so a count of rows is not the same
//! question and was not the thing that was wrong.

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ui::app::App;
use ui::collaboration::{CollabRequest, PresencePayload};
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::state::State;
use ui::widget::Container;

use crate::shell::collab_broker::spawn_full;
use crate::shell::network::{InboundPresence, ShellNetwork, shell_network_for_test};
use crate::shell::peer_presence::{PeerLabels, fold_inbound};
use crate::shell::state::WorkspaceTree;

/// The two surfaces on screen when this was photographed, spelled the
/// way [`ui::local_surface_id`] spells a surface: the app id.
const WORD_PROCESSOR: &str = "dev.qos.word-processor";
const FILE_MANAGER: &str = "dev.qos.files";

/// The handle a peer's frames carry. The receiving side derives the
/// identity `peer@<handle>` from it.
const XAVER: u64 = 11_895_642_303_397_647_149;
const SECOND_PEER: u64 = 42;

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

/// A network with a room to be in, and the room's id.
fn network_in_a_room() -> (ShellNetwork, String) {
    let tree = WorkspaceTree::bootstrap_demo().shared();
    let room = {
        let mut t = tree.write();
        t.adopt_roomless_workspaces();
        let active = t.active;
        t.workspaces
            .iter()
            .find(|w| w.id == active)
            .map(|w| w.room.to_string())
            .expect("the fixture has an active workspace")
    };
    let (net, _cmds) = shell_network_for_test(tree, 0xA11CE);
    // `_cmds` is dropped: nothing here reads what the session would
    // have published, and the drain does not depend on it.
    (net, room)
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
/// it comes from in production. A session told its surface by some
/// other route would not be proving the thing that has to hold:
/// publisher and receiver spell a surface the same way without either
/// being handed the other's spelling.
fn session_on(path: &PathBuf, room: &str, surface: &str) -> App {
    let fd = wait_for_socket(path).into_raw_fd();
    // SAFETY: every test in this module is serialised, and the
    // connect below reads these before returning.
    unsafe {
        std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
        std::env::set_var("QOS_WORKSPACE_ID", room);
        std::env::set_var("QOS_PACKAGE_ID", surface);
        std::env::remove_var("QUICK_COLLAB_SOCK");
    }
    App::new(State::new(), Box::new(Container::new()))
}

/// Put a peer's presence frame on this host the way its host puts it
/// there: the peer's own serialised `CollabRequest::Presence`, stamped
/// with the room and the handle it arrived under.
fn peer_publishes(net: &ShellNetwork, peer: u64, room: &str, key: PresenceKey, x: f32, y: f32) {
    let frame = serde_json::to_vec(&CollabRequest::Presence {
        key,
        payload: PresencePayload::Pointer { x, y },
    })
    .expect("a presence frame serialises");
    net.handle_app_presence_message(peer, WORD_PROCESSOR.to_string(), room.to_string(), frame);
}

/// A caret from the same peer, on the same queue.
///
/// Sent wherever a test is about to assert that a *pointer* did not
/// arrive. "Nothing arrived" and "the drain is not running" look
/// identical from here, and a proof that cannot tell them apart
/// passes against a broker that was never wired up at all. A caret is
/// a position in a document, which does cross machines, so it is the
/// control: it must land.
fn peer_publishes_caret(net: &ShellNetwork, peer: u64, room: &str, key: PresenceKey, pos: usize) {
    let frame = serde_json::to_vec(&CollabRequest::Presence {
        key,
        payload: PresencePayload::Caret { pos },
    })
    .expect("a presence frame serialises");
    net.handle_app_presence_message(peer, WORD_PROCESSOR.to_string(), room.to_string(), frame);
}

/// The same frame as the compositor bridge receives it: a position on
/// the peer's desktop, which their session republished because only it
/// knows where their pointer is on screen.
fn peer_moves_on_their_desktop(room: &str, peer: u64, at: (f64, f64)) -> InboundPresence {
    InboundPresence {
        peer,
        user: format!("session-{peer}"),
        key: serde_json::to_string(&PresenceKey::Workspace {
            id: room.to_string(),
        })
        .expect("a workspace key serialises"),
        point: at,
        ts: 0,
    }
}

/// The pointers a session would put on its own window — what a
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

/// Every pointer `ctx` holds, across every space one might have
/// arrived under, as `(user, key, position)`. Two rows for one person
/// is that person recorded twice.
fn all_pointer_rows(ctx: &PresenceCtx) -> Vec<(String, PresenceKey, (f32, f32))> {
    ctx.pointers
        .iter()
        .flat_map(|(k, list)| {
            list.iter()
                .map(move |(u, p)| (u.0.clone(), k.clone(), *p))
        })
        .collect()
}

/// Pump until `ready` holds, or give up. A frame crosses the drain
/// thread, the fan-out and an RPC driver, so a single tick is a race
/// and a deadline is not.
fn pump_until(apps: &mut [&mut App], ready: &dyn Fn(&[&mut App]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
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

/// Settle: pump for a fixed spell so a frame that *would* arrive has
/// had every chance to. Used where the assertion is that nothing
/// arrives, which a short wait would pass for the wrong reason.
fn pump_for(apps: &mut [&mut App], how_long: Duration) {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        for app in apps.iter_mut() {
            app.pump_collab_presence();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn no_labels(_: &str) -> PeerLabels {
    PeerLabels::new()
}

/// A peer's pointer reaches this host once, in desktop coordinates,
/// and no window draws it.
///
/// The counts are the assertion. Before the space travelled with the
/// position, the key was dropped at the capnp door and the pointer
/// reached the app layer as a bare pair of numbers in no stated space
/// — which every open window then read as its own.
#[test]
#[serial_test::serial]
fn a_peers_pointer_arrives_once_over_the_real_capnp_transport() {
    let (json, capnp) = unique_paths("arrives-once");
    let (net, room) = network_in_a_room();
    let net = std::sync::Arc::new(net);
    let broker =
        spawn_full(json.clone(), Some(capnp.clone()), Some(net.clone())).expect("spawn broker");

    let mut here = session_on(&capnp, &room, FILE_MANAGER);

    // The peer's desktop position, folded in the way the compositor
    // folds it — the real `fold_inbound`, not a stand-in.
    let mut desktop = PresenceCtx::empty();
    let landed = fold_inbound(
        vec![peer_moves_on_their_desktop(&room, XAVER, (900.0, 300.0))],
        &mut desktop,
        &no_labels,
        &|r| r == room,
    );
    assert!(landed, "the peer's desktop position must land");

    let rows = all_pointer_rows(&desktop);
    assert_eq!(
        rows.len(),
        1,
        "one person moved one pointer; the desktop holds {rows:?}"
    );
    assert_eq!(rows[0].0, format!("peer@{XAVER}"));
    assert_eq!(
        rows[0].2,
        (900.0, 300.0),
        "at the position their own session reported"
    );
    assert_eq!(
        desktop
            .remote_cursors
            .iter()
            .filter(|c| c.painted_by_window())
            .count(),
        0,
        "a desktop position is the compositor's to place; anything else \
         drawing it too is the same person twice",
    );

    // And nothing of the peer's own in-window pointer is drawn here.
    // The caret is the control: it proves the drain, the fan-out and
    // the RPC stream are all carrying this peer's frames, so the empty
    // pointer list below is a refusal and not a dead pipe.
    let doc = PresenceKey::Tab {
        id: "shared-doc".to_owned(),
    };
    peer_publishes_caret(&net, XAVER, &room, doc.clone(), 12);
    peer_publishes(
        &net,
        XAVER,
        &room,
        PresenceKey::surface_pointer(WORD_PROCESSOR),
        214.0,
        34.0,
    );
    let peer_id = UserId(format!("peer@{XAVER}"));
    pump_until(&mut [&mut here], &|apps| {
        apps[0].presence_ctx().caret_for(&doc, &peer_id).is_some()
    });
    assert_eq!(
        here.presence_ctx().caret_for(&doc, &peer_id),
        Some(12),
        "the peer's frames reach this session — a document position \
         crosses machines and must land",
    );
    pump_for(&mut [&mut here], Duration::from_millis(300));
    assert!(
        glyphs_drawn_by(&here).is_empty(),
        "and their in-window pointer does not: those pixels are inside \
         a window on another machine, and no window here is the one \
         they describe — {:?}",
        glyphs_drawn_by(&here),
    );

    drop(here);
    drop(broker);
}

/// Two windows open and a peer over one of them: one glyph, in the
/// window they are actually over.
///
/// This is the photographed failure. Both windows drew the peer, at
/// the same offset into each, because both were handed one frame and
/// neither had been told which content the numbers were measured in.
/// Now neither window draws it and the compositor draws it once, at
/// the peer's real position — which is what puts it inside the window
/// they are over rather than at a matching offset in every window.
#[test]
#[serial_test::serial]
fn two_open_windows_draw_no_second_copy_of_a_peer_over_one_of_them() {
    let (json, capnp) = unique_paths("two-windows");
    let (net, room) = network_in_a_room();
    let net = std::sync::Arc::new(net);
    let broker =
        spawn_full(json.clone(), Some(capnp.clone()), Some(net.clone())).expect("spawn broker");

    let mut wp = session_on(&capnp, &room, WORD_PROCESSOR);
    let mut fm = session_on(&capnp, &room, FILE_MANAGER);

    // The peer is pointing inside their word processor, and their
    // session says where that is on their desktop. Both facts travel;
    // only the second one is a place on a screen.
    let doc = PresenceKey::Tab {
        id: "shared-doc".to_owned(),
    };
    peer_publishes_caret(&net, XAVER, &room, doc.clone(), 12);
    peer_publishes(
        &net,
        XAVER,
        &room,
        PresenceKey::surface_pointer(WORD_PROCESSOR),
        214.0,
        34.0,
    );
    let peer_id = UserId(format!("peer@{XAVER}"));
    pump_until(&mut [&mut wp, &mut fm], &|apps| {
        apps[0].presence_ctx().caret_for(&doc, &peer_id).is_some()
            && apps[1].presence_ctx().caret_for(&doc, &peer_id).is_some()
    });
    // The control: both windows are receiving this peer. Without it,
    // two empty pointer lists would also be what a broker nobody
    // connected to looks like.
    assert_eq!(
        (
            wp.presence_ctx().caret_for(&doc, &peer_id),
            fm.presence_ctx().caret_for(&doc, &peer_id),
        ),
        (Some(12), Some(12)),
        "both windows must be hearing this peer for the counts below to mean anything",
    );
    pump_for(&mut [&mut wp, &mut fm], Duration::from_millis(300));

    assert!(
        glyphs_drawn_by(&wp).is_empty() && glyphs_drawn_by(&fm).is_empty(),
        "one frame, two windows, and neither may place it: those pixels \
         are somewhere in a word processor on another machine, and the \
         same offset here is not that place — wp {:?}, fm {:?}",
        glyphs_drawn_by(&wp),
        glyphs_drawn_by(&fm),
    );

    // The one glyph, and where it goes. The tiles are the ones on
    // screen when this was photographed: a file manager from x=292 and
    // a word processor from x=1113.
    let mut desktop = PresenceCtx::empty();
    fold_inbound(
        vec![peer_moves_on_their_desktop(&room, XAVER, (1327.0, 299.0))],
        &mut desktop,
        &no_labels,
        &|r| r == room,
    );
    let rows = all_pointer_rows(&desktop);
    assert_eq!(rows.len(), 1, "one peer, one glyph — got {rows:?}");
    let (_, _, at) = rows[0];
    let in_file_manager = (292.0..1113.0).contains(&at.0);
    let in_word_processor = at.0 >= 1113.0;
    assert!(
        in_word_processor && !in_file_manager,
        "the glyph belongs in the window the peer is over, not at a \
         matching offset in each — landed at {at:?}",
    );

    drop(wp);
    drop(fm);
    drop(broker);
}

/// Two people are two pointers under their own names. The rule that
/// stops one person being drawn twice must not collapse two people
/// into one.
#[test]
#[serial_test::serial]
fn two_distinct_peers_stay_two_pointers_with_their_own_identities() {
    let (_net, room) = network_in_a_room();

    let mut desktop = PresenceCtx::empty();
    fold_inbound(
        vec![
            peer_moves_on_their_desktop(&room, XAVER, (1327.0, 299.0)),
            peer_moves_on_their_desktop(&room, SECOND_PEER, (522.0, 299.0)),
        ],
        &mut desktop,
        &no_labels,
        &|r| r == room,
    );

    let mut rows = all_pointer_rows(&desktop);
    rows.sort_by(|a, b| (a.2).0.partial_cmp(&(b.2).0).expect("finite"));
    assert_eq!(rows.len(), 2, "two people, two pointers — got {rows:?}");
    assert_ne!(
        rows[0].0, rows[1].0,
        "two pointers under one name is two people collapsed into one",
    );
    assert_eq!(rows[0].0, format!("peer@{SECOND_PEER}"));
    assert_eq!(rows[0].2, (522.0, 299.0));
    assert_eq!(rows[1].0, format!("peer@{XAVER}"));
    assert_eq!(rows[1].2, (1327.0, 299.0));
}

/// A peer who moves from one window to another leaves nothing behind.
///
/// A pointer moves rather than multiplying. It reaches a session under
/// more than one space over that session's life, and a row left under
/// the old one is a second dot nothing can retire: retirement ages out
/// what nobody restates, and a peer who is still moving restates the
/// new row.
#[test]
#[serial_test::serial]
fn a_peer_that_moves_from_one_window_to_another_leaves_nothing_behind() {
    let (_net, room) = network_in_a_room();
    let me = UserId(format!("peer@{XAVER}"));

    let mut desktop = PresenceCtx::empty();
    fold_inbound(
        vec![peer_moves_on_their_desktop(&room, XAVER, (522.0, 299.0))],
        &mut desktop,
        &no_labels,
        &|r| r == room,
    );
    assert_eq!(
        all_pointer_rows(&desktop).len(),
        1,
        "precondition: they are over the file manager to begin with"
    );

    // The same person, now over the word processor.
    fold_inbound(
        vec![peer_moves_on_their_desktop(&room, XAVER, (1327.0, 299.0))],
        &mut desktop,
        &no_labels,
        &|r| r == room,
    );

    let rows = all_pointer_rows(&desktop);
    assert_eq!(
        rows.len(),
        1,
        "one person is one pointer however many windows they have \
         crossed — got {rows:?}"
    );
    assert_eq!(rows[0].2, (1327.0, 299.0), "and it is at the new place");
    assert_eq!(
        desktop.pointer_for(&PresenceKey::Workspace { id: room }, &me),
        Some((1327.0, 299.0)),
        "nothing of theirs is left where they were",
    );
}
