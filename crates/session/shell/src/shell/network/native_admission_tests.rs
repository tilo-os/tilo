//! The frame loop admits signed relay input off its own thread, in order.
//!
//! Admitting commits through the room's owner, which publishes its file: a
//! data and a directory sync, seconds on a loaded disk. [`ShellNetwork::tick`]
//! admits in place, for a thread that may wait; the loop's
//! [`ShellNetwork::frame_tick`] must not sync at all, and must still hand
//! the admitted write on before any event that arrived after it.
use super::*;
use crate::network::actor::{HostIdentity, native_ingress};
use ed25519_dalek::SigningKey;
use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};
use tilo_files::native::atomic::test_support::{Checkpoint, with_observer};
use tilo_joined_room::testing::{APP, ROOM, raw_fixture};

/// One frame at 60 Hz: what a tick of the frame loop may take.
const FRAME: Duration = Duration::from_millis(16);

/// One data sync on a disk under write load.
const SLOW_SYNC: Duration = Duration::from_millis(250);

struct Fixture {
    _directory: tempfile::TempDir,
    owner: tilo_joined_room::NativeWorkspaceOwner,
    room: RoomId,
    net: ShellNetwork,
    commands: mpsc::Receiver<NetCmd>,
}

/// A device enrolled in a room, a network that admits as it, and a signed
/// app write of the room's governor queued ahead of a departure.
fn queued_write_then_departure() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("owner.json")).unwrap();
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let certificate = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let message = messages
        .into_iter()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .expect("actual signed fixture document");
    let (mut net, commands) = shell_network_for_test(
        WorkspaceTree::bootstrap_default().shared(),
        neighbour_wire::peer_handle(&certificate.device_public_key),
    );
    let (hint, continuity) = owner
        .with_current(room, |view| {
            Ok((view.transport_hint()?.clone(), view.continuity()))
        })
        .unwrap();
    let input = Arc::new(native_ingress::Scope::new(
        native_ingress::Connect::new(owner.clone(), room).unwrap(),
        hint,
        certificate.clone(),
        continuity,
    ))
    .input(governor, true, message);
    net.host_identity = Some(HostIdentity {
        device_cert: certificate,
        device_signing_key: SigningKey::from_bytes(&[63; 32]),
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    });
    net.event_tx.try_send(NetEvent::NativeInput(input)).unwrap();
    net.event_tx
        .try_send(NetEvent::PeerLeft {
            ws: DEFAULT_WS_TOPIC,
            peer: iroh::SecretKey::from_bytes(&[42; 32]).public(),
            cause: PeerDepartureCause::LivenessTimedOut,
            membership_epoch: None,
        })
        .unwrap();
    Fixture {
        _directory: directory,
        owner,
        room,
        net,
        commands,
    }
}

fn admitted(fixture: &Fixture) -> bool {
    fixture
        .owner
        .with_current(fixture.room, |view| {
            Ok(view.peer().document(APP, "").is_some())
        })
        .unwrap()
}

fn native_publications(commands: &mut mpsc::Receiver<NetCmd>) -> usize {
    std::iter::from_fn(|| commands.try_recv().ok())
        .filter(|command| matches!(command, NetCmd::NativePublish { .. }))
        .count()
}

/// Run `body` with every data sync on this thread slowed to [`SLOW_SYNC`],
/// and answer what it returned and how many this thread made.
fn on_a_slow_disk<T>(body: impl FnOnce() -> T) -> (T, usize) {
    let syncs = Rc::new(Cell::new(0_usize));
    let counted = Rc::clone(&syncs);
    let value = with_observer(
        move |event| {
            if event.checkpoint == Checkpoint::BeforeDataFlush {
                counted.set(counted.get().saturating_add(1));
                std::thread::sleep(SLOW_SYNC);
            }
        },
        body,
    );
    (value, syncs.get())
}

#[test]
fn a_tick_off_the_loop_publishes_its_admission_on_its_own_thread() {
    // What the frame-loop test below must not see, seen here: the admission
    // syncs on whichever thread admits it.
    let fixture = queued_write_then_departure();
    let ((), syncs) = on_a_slow_disk(|| fixture.net.tick());
    assert!(admitted(&fixture), "the signed write was admitted in place");
    assert!(
        syncs > 0,
        "admitting in place made no sync on the ticking thread, so the frame-loop test cannot observe one"
    );
}

#[test]
fn the_frame_loop_admits_signed_input_off_its_thread_in_arrival_order() {
    let mut fixture = queued_write_then_departure();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("deadline");
    let ((slowest, overtaken), syncs) = on_a_slow_disk(|| {
        let mut slowest = Duration::ZERO;
        let mut overtaken = false;
        while fixture.net.peer_departures().is_empty() {
            assert!(
                Instant::now() < deadline,
                "the frame loop never handled the queued events"
            );
            let started = Instant::now();
            fixture.net.frame_tick();
            slowest = slowest.max(started.elapsed());
            // The departure arrived after the write: it may not be handled
            // before the write reached the broker.
            overtaken |= !fixture.net.peer_departures().is_empty()
                && fixture.net.inbound_documents.lock().unwrap().is_empty();
            std::thread::sleep(Duration::from_millis(1));
        }
        (slowest, overtaken)
    });

    assert_eq!(
        syncs, 0,
        "the frame loop published the admission of signed input itself"
    );
    assert!(
        slowest <= FRAME,
        "a frame-loop network tick took {slowest:?} admitting signed input, over one frame ({FRAME:?})"
    );
    assert!(admitted(&fixture), "the signed write was admitted");
    assert!(
        !overtaken,
        "an event that arrived after the signed write was handled before it"
    );
    assert_eq!(
        fixture.net.inbound_documents.lock().unwrap().len(),
        1,
        "the admitted write reached the broker once"
    );
    assert_eq!(
        native_publications(&mut fixture.commands),
        1,
        "the committed change drove the owner's publication"
    );
    assert_eq!(fixture.net.event_rx.lock().unwrap().len(), 0);
}

/// A room replays its records to a member as it joins, often before the
/// app that opens them has named its surface. Such an input is held, not
/// dropped, and admitted again, in the order it arrived, once a surface is
/// named: dropping it lost the record's history, and every later change to
/// it then failed in the app.
#[test]
fn inputs_held_for_an_app_surface_are_admitted_again_once_one_is_named() {
    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("owner.json")).unwrap();
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let (certificate, hint, continuity) = owner
        .with_current(room, |view| {
            Ok((
                view.device().clone(),
                view.transport_hint()?.clone(),
                view.continuity(),
            ))
        })
        .unwrap();
    let scope = Arc::new(native_ingress::Scope::new(
        native_ingress::Connect::new(owner, room).unwrap(),
        hint,
        certificate,
        continuity,
    ));
    let mut documents = messages
        .into_iter()
        .filter(|message| matches!(message, NeighbourMsg::AppDocument { .. }));
    let first = scope.input(governor, true, documents.next().expect("a document"));
    let second = scope.input(governor, true, first.message().clone());
    let state = native_admission::State::default();

    state.hold(first);
    state.hold(second);
    ui::app_surface::register_package(
        "so.tilo.test.held-input",
        b"[[resources]]\nkind = \"held.input\"\n",
    );
    assert!(
        state.next_again().is_some(),
        "a surface was named: what was held is admitted again"
    );
    assert!(state.next_again().is_some());
    assert!(state.next_again().is_none(), "both were taken, in order");

    let third = scope.input(
        governor,
        true,
        documents
            .next()
            .unwrap_or(NeighbourMsg::Heartbeat { peer: 0, ts: 0 }),
    );
    state.hold(third);
    assert!(
        state.next_again().is_none(),
        "held again with no surface named since: it waits"
    );
    ui::app_surface::register_package(
        "so.tilo.test.held-input-later",
        b"[[resources]]\nkind = \"held.later\"\n",
    );
    assert!(
        state.next_again().is_some(),
        "another surface was named: it is admitted again"
    );
    assert!(state.next_again().is_none());
}
