//! A grant a person gives an app is in force before the call it was given
//! for goes on: on the device hosting the room through its own host, on
//! another device once the room's host granted it and the grant reached
//! that device. A refused grant is said, and nothing is granted. The
//! receipt noting the grant is the room's own, so it outlives every reading
//! of the room's document.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_action_broker::app_actions::{AppActionConsent, AppActionConsentRequest};
use tilo_action_broker::services::AppActionGrants as _;
use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, TreePath};
use tilo_user_identity::DeviceCapabilities;

use super::idle_quiet_tests::device_reaching;
use super::{NetCmd, Reach, ShellNetwork};
use crate::shell::broker_services::NativeAppActionGrants;
use crate::shell::state::{RoomId, WorkspaceTree, WorkspaceTreeHandle};

const INVOKER: &str = "so.tilo.invoker";
const TARGET: &str = "so.tilo.target";

fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The person whose root key is `root`.
fn person_of(root: &SigningKey) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    }
}

/// A device whose own host holds a new workspace's room, and that room.
fn hosting_device(
    dir: &std::path::Path,
    person_key: &SigningKey,
) -> (Arc<ShellNetwork>, WorkspaceTreeHandle, RoomId) {
    let mut tree = WorkspaceTree::default();
    let (_, room) = tree.create_hosted_workspace("Home");
    tree.persist_path = Some(dir.join("workspaces.json"));
    let tree = tree.shared();
    let device = device_reaching(
        tree.clone(),
        dir,
        person_key,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(device.join_default_workspace());
    until("the workspace to be hosted here", || {
        device.tick();
        (device.administered_room(room).is_some() && tree.read().native_authority.holds(room))
            .then_some(())
    });
    (device, tree, room)
}

/// Another device of `person_key`'s person, reaching `host` and holding `room`
/// from it.
fn member_device(
    tree: &WorkspaceTreeHandle,
    dir: &std::path::Path,
    person_key: &SigningKey,
    host: &Arc<ShellNetwork>,
    room: RoomId,
) -> Arc<ShellNetwork> {
    let member = device_reaching(
        tree.clone(),
        dir,
        person_key,
        DeviceCapabilities::member(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(member.join_default_workspace());
    let host_addr = until("the hosting device to listen", || host.dial_addr());
    member
        .cmd_tx
        .try_send(NetCmd::Connect { addr: host_addr })
        .unwrap();
    until("the room to reach the other device", || {
        host.tick();
        member.tick();
        (tree.read().native_authority.holds(room) && member.administered_room(room).is_some())
            .then_some(())
    });
    member
}

/// The question the person answered: `INVOKER` using `TARGET`'s action.
fn question(room: RoomId, person: &str) -> AppActionConsentRequest {
    AppActionConsentRequest {
        handle: "app-action:test".into(),
        room: room.to_string(),
        person: person.to_owned(),
        app_id: INVOKER.into(),
        target_app: TARGET.into(),
        resource: "inbox".into(),
        action: "draft".into(),
        resource_label: "Inbox".into(),
        action_label: "Write draft".into(),
        risk: tilo_actions::RiskLevel::Low,
        needs_confirmation: false,
        confirming: false,
        moderation: None,
        scope: None,
        draft: None,
        preview: None,
    }
}

/// The grant `answer` gives `INVOKER` on `TARGET` in `room`, as `person`.
fn grant(id: &str, room: RoomId, person: &str, lifetime: Lifetime) -> Grant {
    Grant::new(
        id,
        Subject::App(INVOKER.into()),
        TreePath::room(room.to_string()).app(TARGET),
        KindSet::READ,
        Answer::Allow,
        Subject::Person(person.to_owned()),
    )
    .with_lifetime(lifetime)
}

/// What recording the grant came to, driving `devices` while it waits as
/// a desktop's loop would.
fn recorded(
    on: &Arc<ShellNetwork>,
    devices: &[&Arc<ShellNetwork>],
    recording: tilo_action_broker::services::GrantRecording,
) -> Result<String, String> {
    let runtime = on.runtime_handle().expect("the network's runtime");
    let pending = runtime.spawn(recording);
    until("the grant to be recorded or refused", || {
        for device in devices {
            device.tick();
        }
        pending.is_finished().then_some(())
    });
    runtime.block_on(pending).expect("the recording finished")
}

/// Whether `room`'s workspace in `tree` holds the grant `id`, and whether
/// the room's admitted document here does.
fn holds(tree: &WorkspaceTreeHandle, room: RoomId, id: &str) -> (bool, bool) {
    let held = |grants: &[Grant]| grants.iter().any(|grant| grant.id.as_str() == id);
    let tree = tree.read();
    let workspace = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .is_some_and(|workspace| held(&workspace.policy.grants));
    let document = tree.room_docs.get(&room).is_some_and(|doc| {
        held(&tilo_desktop_model::projection::room_policy_of(doc, &room.to_string()).grants)
    });
    drop(tree);
    (workspace, document)
}

/// The receipts in `room`'s workspace that `person` made.
fn receipts_by(tree: &WorkspaceTreeHandle, room: RoomId, person: &str) -> usize {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map_or(0, |workspace| {
            workspace
                .effect_receipts
                .iter()
                .filter(|receipt| receipt.actor == person)
                .count()
        })
}

/// On the device hosting the room, "Allow once" and "Always" each put
/// their grant in the room's document through the device's own host before
/// the call goes on, and each grant's receipt is the room's: reading the
/// room's document into the workspace again keeps it.
#[test]
#[serial_test::serial]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_grant_given_where_its_room_is_hosted_is_in_force_before_the_call() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let person_key = SigningKey::from_bytes(&[61; 32]);
    let person = person_of(&person_key);
    home.env().set("TILO_USER_ID", &person);
    let (device, tree, room) = hosting_device(&dir.path().join("host"), &person_key);
    let grants = NativeAppActionGrants::new(tree.clone(), Some(Arc::clone(&device)));

    for (id, answer, lifetime) in [
        ("app-action:once", AppActionConsent::Once, Lifetime::Once),
        (
            "app-action:always",
            AppActionConsent::Always,
            Lifetime::UntilRevoked,
        ),
    ] {
        let recording = grants.record(
            &question(room, &person),
            grant(id, room, &person, lifetime),
            answer,
        );
        assert_eq!(
            recorded(&device, &[&device], recording).as_deref(),
            Ok(id),
            "{answer:?} is granted"
        );
        assert_eq!(
            holds(&tree, room, id),
            (true, true),
            "{answer:?}'s grant is in the room's document before the call goes on"
        );
    }

    until("both grants' receipts to reach the room here", || {
        device.tick();
        (tree.read().room_outbox.is_empty() && receipts_by(&tree, room, &person) >= 2).then_some(())
    });
    let mut held = tree.write();
    let document = held
        .room_docs
        .get(&room)
        .expect("the room's document")
        .clone();
    let workspace = held
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room() == room)
        .expect("the room's workspace");
    tilo_peer_state::room::adopt_room(workspace, &document, &[]);
    drop(held);
    assert_eq!(
        receipts_by(&tree, room, &person),
        2,
        "the receipts are the room's own, so reading its document keeps them"
    );
    drop(device);
}

/// On another device of the person, a grant waits for the room's host and
/// then for the room to bring it here: once the call may go on, the grant
/// is in the room as this device holds it. A grant the room refuses is
/// said as any refused change is, and nothing is granted.
#[test]
#[serial_test::serial]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_grant_given_on_another_device_waits_until_the_room_brings_it_there() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let person_key = SigningKey::from_bytes(&[62; 32]);
    let person = person_of(&person_key);
    home.env().set("TILO_USER_ID", &person);
    let (host, _, room) = hosting_device(&dir.path().join("host"), &person_key);

    let tree = WorkspaceTree {
        persist_path: Some(dir.path().join("member/workspaces.json")),
        ..WorkspaceTree::default()
    }
    .shared();
    let member = member_device(&tree, &dir.path().join("member"), &person_key, &host, room);
    let grants = NativeAppActionGrants::new(tree.clone(), Some(Arc::clone(&member)));
    let id = "app-action:member";
    let recording = grants.record(
        &question(room, &person),
        grant(id, room, &person, Lifetime::UntilRevoked),
        AppActionConsent::Always,
    );
    assert_eq!(holds(&tree, room, id), (false, false));
    assert_eq!(
        recorded(&member, &[&host, &member], recording).as_deref(),
        Ok(id)
    );
    assert_eq!(
        holds(&tree, room, id),
        (true, true),
        "the call goes on only once the grant is in the room here"
    );

    // A grant on another room's app is not this room's to give.
    let elsewhere = Grant::new(
        "app-action:elsewhere",
        Subject::App(INVOKER.into()),
        TreePath::room(RoomId::new().to_string()).app(TARGET),
        KindSet::READ,
        Answer::Allow,
        Subject::Person(person.clone()),
    );
    let recording = grants.record(
        &question(room, &person),
        elsewhere,
        AppActionConsent::Always,
    );
    let refused = recorded(&member, &[&host, &member], recording)
        .expect_err("the room refuses a grant on another room");
    assert!(!refused.is_empty(), "the refusal has a reason");
    let said = tree.write().take_governance_refusals();
    assert!(
        said.iter()
            .any(|refusal| refusal.room == room && refusal.reason == refused),
        "the refusal is said: {said:?}"
    );
    assert_eq!(holds(&tree, room, "app-action:elsewhere"), (false, false));
    drop((host, member));
}

/// A grant given on a device that just started, while its session with the
/// room's host is still being established, waits for that session rather
/// than being refused, and is in force once it is up.
#[test]
#[serial_test::serial]
fn a_grant_given_while_the_hosts_session_connects_waits_for_it() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let person_key = SigningKey::from_bytes(&[63; 32]);
    let person = person_of(&person_key);
    home.env().set("TILO_USER_ID", &person);
    let (host, _, room) = hosting_device(&dir.path().join("host"), &person_key);
    let tree = WorkspaceTree {
        persist_path: Some(dir.path().join("member/workspaces.json")),
        ..WorkspaceTree::default()
    }
    .shared();
    drop(member_device(
        &tree,
        &dir.path().join("member"),
        &person_key,
        &host,
        room,
    ));

    // The device starts again: its room's session is dialled as the grant
    // is given.
    let member = device_reaching(
        tree.clone(),
        &dir.path().join("member"),
        &person_key,
        DeviceCapabilities::member(),
        Reach::LoopbackHoldingRelays,
    );
    let host_addr = until("the hosting device to listen", || host.dial_addr());
    member
        .cmd_tx
        .try_send(NetCmd::Connect { addr: host_addr })
        .unwrap();
    let grants = NativeAppActionGrants::new(tree.clone(), Some(Arc::clone(&member)));
    let id = "app-action:after-start";
    let recording = grants.record(
        &question(room, &person),
        grant(id, room, &person, Lifetime::UntilRevoked),
        AppActionConsent::Always,
    );
    assert_eq!(
        recorded(&member, &[&host, &member], recording).as_deref(),
        Ok(id)
    );
    assert_eq!(holds(&tree, room, id), (true, true));
    drop((grants, host, member));
}

/// A room's host going away ends what waits on its sessions at once, with
/// nothing else happening on the device that waits.
#[test]
#[serial_test::serial]
fn a_wait_on_the_rooms_host_ends_when_its_session_does() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let person_key = SigningKey::from_bytes(&[64; 32]);
    let person = person_of(&person_key);
    home.env().set("TILO_USER_ID", &person);
    let (host, _, room) = hosting_device(&dir.path().join("host"), &person_key);
    let tree = WorkspaceTree {
        persist_path: Some(dir.path().join("member/workspaces.json")),
        ..WorkspaceTree::default()
    }
    .shared();
    let member = member_device(&tree, &dir.path().join("member"), &person_key, &host, room);
    let (watch, mut ended) = tokio::sync::oneshot::channel();
    member
        .cmd_tx
        .try_send(NetCmd::WatchHostSessions {
            ws: super::room_topic_id(room),
            ended: watch,
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(ended.try_recv().is_err(), "the room's session is up");

    drop(host);
    until("the wait to end with the room's session", || {
        ended.try_recv().ok()
    });
    drop(member);
}
