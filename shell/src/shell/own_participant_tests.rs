//! A session has to be able to find *itself* in the room's list of who
//! is in it.
//!
//! Every other row in a roster describes somebody else. The only place
//! remote rows are minted is `remote_participant_id`, which runs on
//! frames arriving *from* another machine — so it runs for every
//! participant except this one, and an application was handed the room's
//! membership with no way to recognise which of them it was. Two things
//! were lost with that row, and both had visible consequences:
//!
//! - **The address.** `Collaborator::id` is `peer@<liveness handle>`,
//!   what other machines call this participant. Nothing on this side can
//!   derive it. An app that authored under an id it worked out for
//!   itself — chat's `u_<hex8>`, off the signing key — could never be
//!   found in a roster keyed the other way, so every remote message
//!   rendered `Someone (…)`.
//! - **The name.** A guest is named by whoever invited them, and that
//!   name is written into the room document on the *host's* machine. It
//!   reaches the guest only here. Without it everybody else in the room
//!   calls them Xaver while their own screen calls them by their account.
//!
//! The broker has always sent the row (`LocalIdentity::self_row`). The
//! App dropped it: `is_self` is filtered out of the remark roster and
//! out of the presence store, and nothing read it before either filter.
//!
//! Those filters are right and stay. Self-presence must not become a
//! roster row, a cursor, a caret pill or an entry in this session's own
//! `@` picker — a pill bearing your own name over your own document is
//! exactly what they exist to prevent. So the row is read for its two
//! facts and still never becomes a participant here, which is what the
//! second half of each test below pins.
//!
//! Real broker, real capnp socket, real `ui::App`. The room's answer is
//! supplied through `LocalIdentity::with_lookup` — the seam that exists
//! so attribution can be exercised against a room whose answers are
//! known — rather than by standing a libp2p node up to be told them.

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ui::app::App;
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;
use ui::state::State;
use ui::widget::Container;

use crate::shell::collab_broker::{spawn_full_with_identity, LocalIdentity};
use crate::shell::peer_presence::{PeerLabel, PeerLabels};

/// The room this session is in.
const ROOM: &str = "3f2b1c00-0000-4000-8000-0000000000a1";
/// The surface this process draws, spelled the way `ui::local_surface_id`
/// spells one.
const SURFACE: &str = "dev.qos.chat";
/// This machine's liveness handle — a hash of its persisted libp2p key in
/// production, so the address derived from it is stable across restarts.
const MY_HANDLE: u64 = 9_007_495_404_254_172_154;

fn unique_paths(tag: &str) -> (PathBuf, PathBuf) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let base = std::env::temp_dir().join(format!("qos-self-{tag}-{}-{stamp}", std::process::id()));
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

/// An app joined to the broker the way the launcher joins a sandboxed
/// one: a connected fd in `QOS_SVC_COLLAB_FD`.
fn session_on(path: &PathBuf) -> App {
    session_on_surface(path, SURFACE)
}

/// [`session_on`] for a second window of the same session, drawing
/// different content — which is what makes its pointer unplaceable in
/// the first one's coordinates.
fn session_on_surface(path: &PathBuf, surface: &str) -> App {
    let fd = wait_for_socket(path).into_raw_fd();
    // SAFETY: every test here carries `#[serial_test::serial]`, which is
    // the same lock `peer_pointer_tests` takes for the same three
    // variables; the connect inside `App::new` reads them before
    // returning.
    unsafe {
        std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
        std::env::set_var("QOS_WORKSPACE_ID", ROOM);
        std::env::set_var("QOS_PACKAGE_ID", surface);
        std::env::remove_var("QUICK_COLLAB_SOCK");
    }
    App::new(State::new(), Box::new(Container::new()))
}

/// Pump until `ready`, or give up. The row crosses a drain thread, the
/// fan-out and an RPC driver, so one tick is a race and a deadline is not.
fn pump_until(app: &mut App, ready: &dyn Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.pump_collab_presence();
        if ready(app) || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Make the session talk, so the broker has a request to answer beside.
/// The self row is resolved per request against the route the app
/// announced, which it only has once the app has said something.
fn speak(app: &mut App) {
    app.broadcast_presence(
        PresenceKey::Workspace {
            id: ROOM.to_string(),
        },
        PresencePayload::Arrived,
    );
}

/// A room that has named this machine's participant, as a host does when
/// it admits a guest by the name on the invite.
fn room_calls_me(name: &'static str) -> Arc<LocalIdentity> {
    LocalIdentity::with_lookup(MY_HANDLE, move |_cluster| {
        let mut labels = PeerLabels::new();
        labels.insert(MY_HANDLE, PeerLabel::person(name));
        labels
    })
}

/// A room that has admitted this machine but named nobody.
fn room_names_nobody() -> Arc<LocalIdentity> {
    LocalIdentity::with_lookup(MY_HANDLE, |_cluster| PeerLabels::new())
}

/// The guest's own screen learns the name the host gave them.
#[test]
#[serial_test::serial]
fn a_session_learns_the_name_the_room_gave_it() {
    let (json, capnp) = unique_paths("named");
    let _broker = spawn_full_with_identity(
        json,
        Some(capnp.clone()),
        None,
        room_calls_me("Xaver"),
    )
    .expect("the broker binds");

    let mut app = session_on(&capnp);
    speak(&mut app);
    pump_until(&mut app, &|a| a.presence_ctx().self_id().is_some());

    let me = app
        .presence_ctx()
        .self_id()
        .expect("the room never said which participant this session is")
        .clone();
    assert_eq!(
        me.0,
        format!("peer@{MY_HANDLE}"),
        "the address handed to the app is not the one other machines use \
         for this participant"
    );
    assert_eq!(
        app.presence_ctx().self_name(),
        Some("Xaver"),
        "the guest's own screen does not know the name the host gave them"
    );

    // And it is still not a participant here. Each of these would be a
    // visible defect: a row in this session's own picker, a name pill or
    // caret chip painted for the person holding the mouse, a second
    // cursor at their own cursor.
    assert!(
        !app.presence_ctx().names.contains_key(&me),
        "this session's own participant was seeded into the presence \
         roster; every caret chip and mention colour reads from that map"
    );
    assert!(
        app.remark_destination_ids().iter().all(|id| *id != me.0),
        "this session's own participant is offered as somewhere to send \
         a remark"
    );
    assert!(
        app.presence_ctx().remote_cursors.is_empty(),
        "a cursor was minted for this session's own participant"
    );

    // Knowing the address means frames stamped with it are recognised as
    // this session's own rather than painted as a stranger's.
    assert!(
        app.presence_ctx().is_own_identity(&me.0),
        "a frame coming back stamped with this session's own room address \
         would be drawn as somebody else"
    );
}

/// A participant the room has admitted but not named has an address and
/// no name — and the honest answer is no name, not a guessed one.
#[test]
#[serial_test::serial]
fn an_unnamed_participant_still_learns_its_address() {
    let (json, capnp) = unique_paths("unnamed");
    let _broker =
        spawn_full_with_identity(json, Some(capnp.clone()), None, room_names_nobody())
            .expect("the broker binds");

    let mut app = session_on(&capnp);
    speak(&mut app);
    pump_until(&mut app, &|a| a.presence_ctx().self_id().is_some());

    let me = app
        .presence_ctx()
        .self_id()
        .expect("the room never said which participant this session is")
        .clone();
    assert_eq!(me.0, format!("peer@{MY_HANDLE}"));
    assert_ne!(
        app.presence_ctx().self_name(),
        Some(me.0.as_str()),
        "an address is not a name; handing the id back as one is how a \
         routing hash ends up rendered where a name belongs"
    );
}

/// Two windows of one person are still one participant — under the
/// spelling the broker actually stamps their traffic with.
///
/// The row this machine announces about its own participant carries the
/// *address* other machines use for it, `peer@<handle>`
/// ([`LocalIdentity::self_row`]). What it stamps on traffic leaving a
/// local application is the room's *name* for that same participant
/// ([`LocalIdentity::resolve`]). One participant, two strings, on one
/// socket — and only the first had ever been handed to an application,
/// so a session whose account is `quinten` and whom the host invited as
/// `Xaver` got its own pointer back stamped `Xaver`, matched nothing,
/// and painted it.
///
/// What that looks like is not a stray cursor. The position was measured
/// in a *different* window, so in this one it lands outside the viewport
/// and comes out as the off-screen wayfinding band — a 140x44 block in
/// the peer's colour hugging an edge, name tag and all, lying across a
/// file manager's toolbar.
///
/// The stamp is read from the same [`LocalIdentity`] the broker is
/// holding rather than written down here, so this cannot pass by
/// asserting on a spelling the broker has stopped using.
#[test]
#[serial_test::serial]
fn a_second_window_of_this_session_is_not_a_peer() {
    let (json, capnp) = unique_paths("two-windows");
    let identity = room_calls_me("Xaver");
    let _broker = spawn_full_with_identity(
        json,
        Some(capnp.clone()),
        None,
        Arc::clone(&identity),
    )
    .expect("the broker binds");

    let mut files = session_on_surface(&capnp, "dev.qos.file-explorer");
    let mut words = session_on_surface(&capnp, "dev.qos.word-processor");
    speak(&mut files);
    speak(&mut words);
    pump_until(&mut files, &|a| a.presence_ctx().self_id().is_some());
    pump_until(&mut words, &|a| a.presence_ctx().self_id().is_some());

    // What a frame leaving one of these windows is stamped with, asked
    // of the resolver the broker is using.
    let route = crate::shell::collab_broker::CrdtRoute::cluster(ROOM.to_string());
    let stamp = identity.resolve(Some(&route), None);
    assert_eq!(
        stamp, "Xaver",
        "the fixture no longer exercises the spelling it exists for",
    );
    assert!(
        files.presence_ctx().is_own_identity(&stamp),
        "a frame this session publishes comes back stamped `{stamp}`, and \
         this window does not recognise it as its own",
    );

    // The person moves their mouse in the word processor.
    words.broadcast_presence(
        PresenceKey::Pointer {
            space: ui::presence::PointerSpace::Surface {
                surface: "dev.qos.word-processor".to_string(),
            },
        },
        PresencePayload::Pointer { x: 900.0, y: 512.0 },
    );

    // Pump the file manager until the frame has demonstrably reached it,
    // so an empty overlay cannot be a rig that delivered nothing.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut delivered = false;
    while Instant::now() < deadline {
        delivered |= files.pump_collab_presence();
        if delivered && Instant::now() > deadline - Duration::from_secs(4) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        delivered,
        "nothing crossed between the two windows, so this proves nothing",
    );
    assert!(
        files.presence_ctx().remote_cursors.is_empty(),
        "the file manager took this person's own pointer — measured in \
         another of their windows — for a peer's: {:?}",
        files.presence_ctx().remote_cursors,
    );
    assert!(
        !files.presence_ctx().names.contains_key(&ui::permissions::UserId(
            stamp.clone(),
        )),
        "this session's own participant was given a roster name, which is \
         what every chip and pill is labelled from",
    );
}
