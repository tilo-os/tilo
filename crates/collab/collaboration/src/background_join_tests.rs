//! A native app's manager joins its workspace in the background: the app
//! builds on its local replica meanwhile, and the broker's answer, what
//! holds the join up and its refusal reach the app as events, over the
//! fake broker the tests stand up. No clock decides any of it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code: a failed step is the assertion"
)]

use super::{CollabEvent, CollaborationManager, JoiningProbe, RouteSpec, native_initial};
use crate::CollaborationState;
use std::sync::mpsc;
use uuid::Uuid;

/// The wakes the worker of a manager made in `scope` sends its owner.
fn wakes_of(scope: &mut crate::wake::WakeScope) -> mpsc::Receiver<()> {
    let (woken, wakes) = mpsc::channel();
    scope
        .bind(move |_| {
            let _ = woken.send(());
        })
        .unwrap();
    wakes
}

/// Poll `manager` on each wake its worker sends until `done`, keeping
/// what it delivered.
fn poll_on_wakes(
    manager: &mut CollaborationManager,
    wakes: &mpsc::Receiver<()>,
    what: &str,
    mut done: impl FnMut(&CollaborationManager, &[CollabEvent]) -> bool,
) -> Vec<CollabEvent> {
    let mut delivered = manager.poll();
    while !done(manager, &delivered) {
        tilo_test_env::recv(what, wakes);
        delivered.extend(manager.poll());
    }
    delivered
}

fn joining_route() -> RouteSpec {
    RouteSpec {
        app_id: "so.tilo.notes".into(),
        cluster_id: Uuid::new_v4(),
        record: None,
    }
}

/// A native app builds on its local replica while its manager joins:
/// construction takes the non-Required path and asks for no committed
/// read, a record that is not here is not created meanwhile
/// (`native_initial::joining`), and the broker's late answer, then the
/// route's committed history, reach the owner as events.
#[test]
fn a_joining_manager_builds_on_its_local_replica_and_takes_the_answer_as_an_event() {
    let mut scope = crate::wake::WakeScope::new();
    let wakes = wakes_of(&mut scope);
    let route = joining_route();
    let (manager, broker) = {
        let _entry = scope.enter();
        CollaborationManager::native_joining_fixture_for_test(
            route.clone(),
            "notes.notebook",
            automerge::AutoCommit::new().save(),
            runtime_collab::RoomStatus::Ready,
        )
    };
    assert!(manager.is_joining());
    let construction = native_initial::NativeConstruction::enter_component(manager, scope)
        .expect("a joining manager builds on the local replica");
    assert!(native_initial::joining());
    let (manager, scope) = construction.into_parts();
    let (mut manager, _scope) = (manager.unwrap(), scope.unwrap());
    assert!(!native_initial::joining());
    assert_eq!(manager.state(), CollaborationState::Connecting);

    broker.answer();
    let delivered = poll_on_wakes(
        &mut manager,
        &wakes,
        "the route's committed history",
        |manager, delivered| {
            manager.state() == CollaborationState::Ready
                && delivered
                    .iter()
                    .any(|event| matches!(event, CollabEvent::CommittedDocument { .. }))
        },
    );
    assert_eq!(
        manager
            .primary_route()
            .map(|witness| witness.route().clone()),
        Some(route)
    );
    assert!(
        delivered
            .iter()
            .any(|event| matches!(event, CollabEvent::CommittedDocument { .. }))
    );
}

/// While the broker holds the join it says why, and the manager follows
/// each change as an event: the room waits, then the broker cannot be
/// reached. The join's answer clears it.
#[test]
fn a_joining_manager_follows_the_brokers_room_status() {
    let mut scope = crate::wake::WakeScope::new();
    let wakes = wakes_of(&mut scope);
    let placing = runtime_collab::RoomStatus::Waiting {
        reason: "Placing the workspace on its host".into(),
        detail: None,
    };
    let (mut manager, broker) = {
        let _entry = scope.enter();
        CollaborationManager::native_joining_fixture_for_test(
            joining_route(),
            "notes.notebook",
            automerge::AutoCommit::new().save(),
            placing,
        )
    };
    poll_on_wakes(&mut manager, &wakes, "the room's wait", |manager, _| {
        manager.join_hold()
            == Some(crate::JoinHold::RoomWaiting {
                reason: "Placing the workspace on its host".into(),
                detail: None,
            })
    });
    broker.set_room(runtime_collab::RoomStatus::Waiting {
        reason: "Waiting for the host".into(),
        detail: Some("the host is offline".into()),
    });
    poll_on_wakes(&mut manager, &wakes, "the next wait", |manager, _| {
        matches!(
            manager.join_hold(),
            Some(crate::JoinHold::RoomWaiting {
                detail: Some(_),
                ..
            })
        )
    });
    broker.room_unreachable("the broker went away");
    poll_on_wakes(
        &mut manager,
        &wakes,
        "the broker unreachable",
        |manager, _| matches!(manager.join_hold(), Some(crate::JoinHold::Unreachable(_))),
    );
    assert_eq!(manager.state(), CollaborationState::Connecting);
    broker.answer();
    poll_on_wakes(&mut manager, &wakes, "the route joined", |manager, _| {
        manager.state() == CollaborationState::Ready
    });
    assert_eq!(manager.join_hold(), None);
}

/// A refused join takes the manager offline with the broker's reason;
/// its owner stays up.
#[test]
fn a_refused_background_join_says_why() {
    let mut scope = crate::wake::WakeScope::new();
    let wakes = wakes_of(&mut scope);
    let (mut manager, broker) = {
        let _entry = scope.enter();
        CollaborationManager::native_joining_fixture_for_test(
            joining_route(),
            "notes.notebook",
            automerge::AutoCommit::new().save(),
            runtime_collab::RoomStatus::Ready,
        )
    };
    broker.refuse();
    poll_on_wakes(&mut manager, &wakes, "the refusal", |manager, _| {
        matches!(manager.state(), CollaborationState::Offline(_))
    });
    assert!(
        manager
            .join_refusal()
            .is_some_and(|reason| reason.contains(JoiningProbe::REFUSAL)),
        "{:?}",
        manager.join_refusal()
    );
    assert!(manager.sync_lost());
}

/// A window whose record is not on this device waits for its join off
/// its thread (`NativeJoin`). The manager it waits on was made in the
/// app's scope, so once joined its worker wakes that app, whichever
/// thread waited.
#[test]
#[cfg(feature = "native")]
fn a_join_waited_for_off_its_thread_wakes_the_app_it_was_made_for() {
    let mut scope = crate::wake::WakeScope::new();
    let wakes = wakes_of(&mut scope);
    let (manager, broker) = {
        let _entry = scope.enter();
        CollaborationManager::native_joining_fixture_for_test(
            joining_route(),
            "notes.notebook",
            automerge::AutoCommit::new().save(),
            runtime_collab::RoomStatus::Ready,
        )
    };
    let join = super::NativeJoin::start(manager).unwrap();
    let (told, done) = mpsc::channel();
    join.when_done(move || {
        let _ = told.send(());
    });
    broker.answer();
    tilo_test_env::recv("the join to end", &done);
    let mut manager = join.finish().expect("joined");
    assert_eq!(manager.state(), CollaborationState::Ready);
    assert_eq!(
        manager.take_state_change(),
        Some(CollaborationState::Ready),
        "the app does its connect-time work on the joined route"
    );
    poll_on_wakes(
        &mut manager,
        &wakes,
        "the joined route's history, waking the app",
        |_, delivered| {
            delivered
                .iter()
                .any(|event| matches!(event, CollabEvent::CommittedDocument { .. }))
        },
    );
}

/// A window asks the session for its account custody as soon as it is
/// built, while its route is still joining (the live launch builds
/// local-first). The handle it captured keeps working: the request waits in
/// the worker for the route the app named, and the session's answer comes
/// back on the window's own route once it joined.
#[test]
fn account_custody_taken_while_the_route_joins_answers_once_it_is_admitted() {
    let mut scope = crate::wake::WakeScope::new();
    let wakes = wakes_of(&mut scope);
    let route = joining_route();
    let (manager, broker) = {
        let _entry = scope.enter();
        CollaborationManager::native_joining_fixture_for_test(
            route.clone(),
            "notes.notebook",
            automerge::AutoCommit::new().save(),
            runtime_collab::RoomStatus::Ready,
        )
    };
    assert!(manager.is_joining(), "the live launch builds while joining");
    let construction = native_initial::NativeConstruction::enter_component(manager, scope)
        .expect("a joining manager builds on the local replica");
    // The window's client, captured while its route is still joining.
    let client = crate::browser_service_sessions::Client::capture()
        .expect("a joining window reaches the session");
    let (mut manager, _scope) = construction.into_parts();

    let mut sealed = client
        .request(
            runtime_collab::BrowserServiceSessionCommand::StoreAccountSecret {
                account: "hiring@studio.example".into(),
                password: zeroize::Zeroizing::new("app-password".into()),
            },
            None,
        )
        .expect("the request waits for the route");

    // The route joins; the request is served on it, by the custodian.
    broker.answer();
    let manager = manager.as_mut().unwrap();
    poll_on_wakes(manager, &wakes, "the route to be admitted", |manager, _| {
        manager.primary_route().is_some()
    });
    let deadline = tilo_test_env::hang_deadline();
    let answer = loop {
        if let Some(answer) = sealed.poll() {
            break answer;
        }
        assert!(
            crate::time::now() < deadline,
            "the session answered the request it was handed while joining"
        );
        std::thread::yield_now();
    };
    assert!(
        matches!(
            answer.expect("the session sealed the account's secret"),
            runtime_collab::BrowserServiceSessionReply::AccountSecretStored { account }
                if account == "hiring@studio.example"
        ),
        "the secret is sealed on the app's own route"
    );

    // The other half: the app that keeps Mail's account type is released
    // its secret on the same client, which never had to be taken again.
    let mut released = client
        .request(
            runtime_collab::BrowserServiceSessionCommand::ReleaseAccountSecret {
                account: "hiring@studio.example".into(),
            },
            None,
        )
        .expect("the released secret is asked for on the joined route");
    let answer = loop {
        if let Some(answer) = released.poll() {
            break answer;
        }
        assert!(
            crate::time::now() < deadline,
            "the session released the account's secret"
        );
        std::thread::yield_now();
    };
    assert!(
        matches!(
            answer.expect("the session released the secret"),
            runtime_collab::BrowserServiceSessionReply::AccountSecret { account, password }
                if account == "hiring@studio.example"
                    && password.as_str() == "released-secret"
        ),
        "the app that keeps the account's type is released its secret"
    );
}
