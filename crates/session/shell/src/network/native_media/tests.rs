use super::*;
use crate::network::HostIdentity;
use crate::shell::native_workspace_owner::test_enrolled_owner;

#[tokio::test]
async fn actual_actor_custody_opens_only_pinned_discovery_and_fences_late_reply() {
    let (_directory, owner, room, _now, key) = test_enrolled_owner();
    let cert = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .unwrap();
    let identity = HostIdentity {
        device_cert: cert,
        device_signing_key: key,
        workspace_device_certs: Default::default(),
        workspace_revocations: Default::default(),
        identity_paths: None,
    };
    let target = CallTarget {
        workspace: room.to_string(),
        app: "dev.qos.chat".into(),
        record: "".into(),
        channel: "voice".into(),
    };
    let mut registry = Registry::default();
    let (tx, rx) = tokio::sync::oneshot::channel();
    registry.handle(
        Command {
            operation: Operation::Open {
                owner: owner.clone(),
                target,
            },
            reply: tx,
        },
        Some(&identity),
    );
    let Reply::Opened(ticket, work) = rx
        .await
        .unwrap()
        .expect("actual actor prepares pinned discovery")
    else {
        panic!("discovery request required")
    };
    assert_eq!(work.request.origin, "https://native.example");
    assert_eq!(work.request.target, "/peer/v3/descriptor");
    assert!(work.request.body.is_empty());
    assert!(work.request.proof.is_none());
    owner.invalidate_identity(room);
    assert!(ticket.live().is_err());
    let (tx, rx) = tokio::sync::oneshot::channel();
    registry.handle(
        Command {
            operation: Operation::Advance {
                ticket,
                serial: work.serial,
                body: vec![1],
            },
            reply: tx,
        },
        Some(&identity),
    );
    assert!(
        rx.await.unwrap().is_err(),
        "revoked owner cannot complete an old discovery"
    );
}

async fn open_actual(
    owner: NativeWorkspaceOwner,
    room: RoomId,
    identity: &HostIdentity,
) -> Arc<Ticket> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut registry = Registry::default();
    registry.handle(
        Command {
            operation: Operation::Open {
                owner,
                target: CallTarget {
                    workspace: room.to_string(),
                    app: "dev.qos.chat".into(),
                    record: "".into(),
                    channel: "voice".into(),
                },
            },
            reply: tx,
        },
        Some(identity),
    );
    let Reply::Opened(ticket, _) = rx.await.unwrap().unwrap() else {
        panic!("exact actual discovery required")
    };
    ticket
}
fn identity_for(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    key: ed25519_dalek::SigningKey,
) -> HostIdentity {
    HostIdentity {
        device_cert: owner
            .with_current(room, |view| Ok(view.device().clone()))
            .unwrap(),
        device_signing_key: key,
        workspace_device_certs: Default::default(),
        workspace_revocations: Default::default(),
        identity_paths: None,
    }
}
#[tokio::test]
async fn own_bootstrap_ack_is_correlated_but_leave_during_owner_fence_wins() {
    use crate::shell::native_workspace_owner::{test_bootstrap_response, test_descriptor};
    for leave in [false, true] {
        let (_directory, owner, room, now, key) = test_enrolled_owner();
        let identity = identity_for(&owner, room, key);
        let ticket = open_actual(owner.clone(), room, &identity).await;
        let hint = test_descriptor(&owner, 1, now);
        let pending = owner
            .begin_bootstrap(room, &hint, &identity.device_signing_key, now)
            .unwrap();
        let response = test_bootstrap_response(&pending, &hint, now);
        let weak = Arc::downgrade(&ticket);
        let _leave = owner.observe_fences(Arc::new(move |changed, _| {
            if changed == room && leave {
                if let Some(ticket) = weak.upgrade() {
                    ticket.stop();
                }
            }
        }));
        let result = registry::finish_owner_bootstrap(&ticket, pending, &response);
        assert_eq!(result.is_ok(), !leave);
        assert_eq!(
            ticket.live().is_ok(),
            !leave,
            "an accepted floor cannot revive local Leave"
        );
        assert!(!ticket.own_bootstrap.load(Ordering::SeqCst));
        if !leave {
            // Equal exact floor acknowledgement has no security-continuity
            // change. It cannot open a window that accepts another fence.
            let pending = owner
                .begin_bootstrap(room, &hint, &identity.device_signing_key, now)
                .unwrap();
            let response = test_bootstrap_response(&pending, &hint, now);
            registry::finish_owner_bootstrap(&ticket, pending, &response).unwrap();
            owner.invalidate_identity(room);
            assert!(ticket.live().is_err());
        }
    }
}
#[tokio::test]
async fn revoked_owner_or_wrong_actual_key_cannot_finish_pending_bootstrap() {
    use crate::shell::native_workspace_owner::{test_bootstrap_response, test_descriptor};
    let (_directory, owner, room, now, key) = test_enrolled_owner();
    let wrong = identity_for(
        &owner,
        room,
        ed25519_dalek::SigningKey::from_bytes(&[17; 32]),
    );
    let mut registry = Registry::default();
    let (tx, rx) = tokio::sync::oneshot::channel();
    registry.handle(
        Command {
            operation: Operation::Open {
                owner: owner.clone(),
                target: CallTarget {
                    workspace: room.to_string(),
                    app: "dev.qos.chat".into(),
                    record: "".into(),
                    channel: "voice".into(),
                },
            },
            reply: tx,
        },
        Some(&wrong),
    );
    assert!(rx.await.unwrap().is_err());
    let identity = identity_for(&owner, room, key);
    let ticket = open_actual(owner.clone(), room, &identity).await;
    let hint = test_descriptor(&owner, 1, now);
    let pending = owner
        .begin_bootstrap(room, &hint, &identity.device_signing_key, now)
        .unwrap();
    let response = test_bootstrap_response(&pending, &hint, now);
    owner.invalidate_identity(room);
    assert!(registry::finish_owner_bootstrap(&ticket, pending, &response).is_err());
    assert!(ticket.live().is_err());
}
