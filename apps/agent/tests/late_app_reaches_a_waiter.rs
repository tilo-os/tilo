//! What a participant already in the room learns about one that joins later.
//!
//! The room's convergence handshake has always run one way in time. An
//! application publishes its whole document only in reply to somebody else's
//! `Arrived` (`ui::App::apply_remote_presence`), and joining, it announces
//! itself and asks. So the participant who was there first is the one holding
//! the wrong end: it asked when it arrived, was answered by whoever was up at
//! the time, and nothing about the newcomer's arrival sends it the newcomer's
//! state. Two windows in one room, and only the younger one knows what the
//! other is holding.
//!
//! That is what these two tests are about, and they are deliberately the same
//! test in both orders — a claim about *ordering* is worth nothing unless the
//! other ordering is exercised by the identical code.
//!
//! Everything here is the production path. The broker is the one the
//! compositor binds, with both transports up; the participants are real
//! [`ui::App`]s, each with its own [`ui::collaboration::CollaborationManager`]
//! attached to the capnp socket the way the launcher attaches a sandboxed
//! app — a connected fd, handed over by number. Nothing is stubbed and no
//! event is handed to anyone directly: what each app learns, it learns off
//! the socket.
//!
//! The two run one at a time. An app takes its room and its socket out of the
//! environment, which is process-wide, so two of these overlapping would each
//! be reading the other's room between setting a variable and the constructor
//! that reads it -- and a room the participants disagree about is the failure
//! this is trying to detect.

use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use ui::App;
use ui::state::State;
use uuid::Uuid;

const PHOTOS: &str = "dev.qos.photos";

/// An app connects through the launcher's service fd, which is named by a
/// process-wide environment variable, so building two at once would race on
/// it. Held only while one is being constructed.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The compositor's own broker, on both transports.
struct Room {
    _dir: tempfile::TempDir,
    _json: PathBuf,
    capnp: PathBuf,
    id: Uuid,
    _broker: shell::shell::collab_broker::BrokerHandle,
}

fn room() -> Room {
    let dir = tempfile::tempdir().expect("tempdir");
    let json = dir.path().join("collab-json.sock");
    let capnp = dir.path().join("collab.sock");
    let broker = shell::shell::collab_broker::spawn_full(json.clone(), Some(capnp.clone()), None)
        .expect("the broker binds both sockets");
    for path in [&json, &capnp] {
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(path).is_err() {
            assert!(Instant::now() < deadline, "broker never bound {path:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Room {
        _dir: dir,
        _json: json,
        capnp,
        id: Uuid::new_v4(),
        _broker: broker,
    }
}

/// The photo editor's document: seeded, and holding `path` if there is one.
///
/// A real app's schema rather than a bag of keys, so "did the other one's
/// state arrive" is asked the way the room asks it: of the app's own reader.
///
/// `None` is an editor with nothing open, which is what an app that has just
/// started is — and it has to be a real absence rather than a photograph with
/// an empty name, because the reader prefers a photograph over none and would
/// otherwise be choosing between two of them.
fn photo_document(actor: &str, path: Option<&str>) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from(actor.as_bytes()));
    photos::doc::PhotosDoc::seed(&mut doc);
    if let Some(path) = path {
        photos::doc::PhotosDoc::set_image(
            &mut doc,
            Some(&photos::OpenImage::from_path(path, 40, 24)),
        );
    }
    doc.commit();
    doc.save()
}

/// Start a real application in `room`, holding `document`.
///
/// The environment is what a launched app is given: the room to collaborate
/// in, the package it is, and a connected descriptor to the session's broker.
/// [`App::new`] reads all three — it is what announces the app's arrival — so
/// they have to be in place before it is called and are serialised against
/// the sibling that does the same.
fn app_in(room: &Room, document: &[u8]) -> App {
    let _guard = env_lock();
    let fd = UnixStream::connect(&room.capnp)
        .expect("connect the capnp socket")
        .into_raw_fd();
    // SAFETY: serialised by `env_lock`, and everything below reads these
    // before it returns.
    unsafe {
        std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
        std::env::set_var("QOS_PACKAGE_ID", PHOTOS);
        std::env::set_var("QOS_WORKSPACE_ID", room.id.to_string());
        // The legacy path wins when this is set, and it must not: an app in a
        // session takes the capnp descriptor.
        std::env::remove_var("QUICK_COLLAB_SOCK");
    }
    let state = State::load(document).expect("the app's document loads");
    let app = App::new(state, Box::new(ui::widget::Text::new("photo")));
    assert!(
        app.presence_broadcast_scope().is_some(),
        "an app in a workspace announces itself; without a scope this test \
         would be about a participant that never joined"
    );
    app
}

/// The file the photo editor `app` currently has open, as its own reader
/// answers it — never as a string search over the bytes.
fn open_file(app: &mut App) -> Option<String> {
    photos::doc::PhotosDoc::image(&app.state_mut_for_test().doc).map(|i| i.path)
}

/// Drain both apps until `done`, or until `within` runs out.
///
/// Pumping is what an app's event loop does every wakeup
/// (`ui::App::pump_collab_presence`, from `about_to_wait`), and it is the only
/// thing that moves an inbound frame into a document. Both are pumped on every
/// turn because either one going quiet would decide the answer.
fn settle(a: &mut App, b: &mut App, within: Duration, mut done: impl FnMut(&mut App, &mut App) -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        a.pump_collab_presence();
        b.pump_collab_presence();
        if done(a, b) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    a.pump_collab_presence();
    b.pump_collab_presence();
    done(a, b)
}

/// How long the two are given. Generous, because a failure here has to mean
/// "it never came" rather than "the machine was busy": everything this waits
/// on is local, and a run that needs seconds of it has already told the truth.
const WITHIN: Duration = Duration::from_secs(20);

/// **The defect.** The first application in the room ends up holding what the
/// second one brought with it.
///
/// The second app announcing its arrival is what makes the first reply with
/// its own state — that direction has always worked and is the control below.
/// This is the other direction, and there was nothing on it: the newcomer
/// asked and volunteered nothing, so a participant that had already asked,
/// and had been answered by an emptier room, stayed with that answer.
#[test]
#[serial_test::serial]
fn the_app_already_here_receives_what_a_later_one_brought() {
    let room = room();
    // Deliberately empty: the point is what it *gains*, and an app that
    // already had a picture could pass this by having merged nothing.
    let mut early = app_in(&room, &photo_document("early", None));
    // Give the first one time to finish arriving, so this is an application
    // joining a room that is already up rather than two starting at once.
    for _ in 0..25 {
        early.pump_collab_presence();
        std::thread::sleep(Duration::from_millis(20));
    }

    let late_file = "/late/harbour.png";
    let mut late = app_in(&room, &photo_document("late", Some(late_file)));

    let arrived = settle(&mut early, &mut late, WITHIN, |early, _| {
        open_file(early).as_deref() == Some(late_file)
    });
    assert!(
        arrived,
        "the application that was here first never received the document of \
         the one that started after it; it still has {:?} open",
        open_file(&mut early),
    );
}

/// **The control, and it is not a formality.** The same two applications, the
/// same socket, the same pump — started the other way round.
///
/// A run where both fail is a broken harness rather than a defect, and the
/// two are indistinguishable from the test above on its own.
#[test]
#[serial_test::serial]
fn the_app_that_starts_second_receives_what_was_already_here() {
    let room = room();
    let early_file = "/early/harbour.png";
    let mut early = app_in(&room, &photo_document("early", Some(early_file)));
    for _ in 0..25 {
        early.pump_collab_presence();
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut late = app_in(&room, &photo_document("late", None));

    let arrived = settle(&mut early, &mut late, WITHIN, |_, late| {
        open_file(late).as_deref() == Some(early_file)
    });
    assert!(
        arrived,
        "the application that started second never received the document that \
         was already in the room; it has {:?} open",
        open_file(&mut late),
    );
}
