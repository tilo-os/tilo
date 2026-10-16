//! Inviting a different person to a room the host governs, page to page
//! over HTTPS: the owner's page issues an invitation; the other person's
//! page, with its own user root and a device it certified itself, redeems
//! it and waits; the owner admits it; it then holds a session and
//! subscribes, until the owner removes it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::invite::{ask_to_join, guest_certificate, join};
use tilo_browser_peer::{HttpClient, HttpsLink, carrier};
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::rooms::{JoinReply, JoinStep, Room, RoomRole, RoomsReply, RoomsRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use support::{TestHost, enrol, start_room_host};

async fn rooms(link: &HttpsLink, request: RoomsRequest) -> RoomsReply {
    carrier::rooms(link.client().as_ref(), link.session(), &request)
        .await
        .unwrap()
}

fn room(reply: RoomsReply) -> Room {
    match reply {
        RoomsReply::Room(room) => room,
        other => panic!("expected a room, got {other:?}"),
    }
}

/// A person's page: their own root, and a device it certified with it.
struct Guest {
    key: SigningKey,
    cert: DeviceCert,
}

async fn guest(name: &str) -> Guest {
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = guest_certificate(
        &root,
        key.verifying_key().to_bytes(),
        name,
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
    Guest { key, cert }
}

async fn status(client: &dyn HttpClient, guest: &Guest, request_id: [u8; 32]) -> JoinReply {
    join(
        client,
        &JoinStep::Status {
            request_id,
            device_public_key: guest.cert.device_public_key,
        },
    )
    .await
    .unwrap()
}

/// Subscribe on `link`: `Err` with the host's reason when it refuses, or
/// when it has ended the session (a removed member's, once the host audits
/// its access).
async fn subscribe(link: &mut HttpsLink, workspace_id: u64) -> Result<(), String> {
    if link.send(Envelope::Subscribe { workspace_id }).is_err() {
        return Err("session ended".into());
    }
    tilo_test_env::within("the host answers the subscription", async {
        loop {
            match link.recv().await {
                Some(Envelope::Ready { .. }) => return Ok(()),
                Some(Envelope::NotAuthorised { reason, .. }) => return Err(reason),
                Some(_) => {}
                None => return Err("session ended".into()),
            }
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_admits_and_removes_a_different_person_over_https() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let page = enrol(&alice, "alice-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web } = start_room_host(&alice, &host, state.path(), false).await;
    let https: Arc<dyn HttpClient> =
        support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());
    let connect = |cert: DeviceCert, key: SigningKey| {
        let https = https.clone();
        let host = host.cert.device_public_key;
        async move { HttpsLink::connect(https, &cert, host, &RevocationList::new(), &key).await }
    };

    // Alice's page creates a governed workspace and enters it as its owner.
    let mut owner = connect(page.cert.clone(), page.key.clone()).await.unwrap();
    assert!(owner.rooms(), "the host administers rooms for pages");
    let created = room(
        rooms(
            &owner,
            RoomsRequest::Create {
                name: "Design review".into(),
            },
        )
        .await,
    );
    let entered = room(
        rooms(
            &owner,
            RoomsRequest::Enter {
                room: created.room.clone(),
            },
        )
        .await,
    );
    let alice_member = entered
        .members
        .iter()
        .find(|member| member.user_public_key == alice.verifying_key().to_bytes())
        .unwrap();
    assert!(alice_member.owner);
    subscribe(&mut owner, entered.workspace_id).await.unwrap();

    // She invites; Bob, a different person, sees what it offers.
    let RoomsReply::Invitation {
        signed_invitation, ..
    } = rooms(
        &owner,
        RoomsRequest::Invite {
            room: created.room.clone(),
            request_id: [1; 32],
            role: RoomRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: Some("Alice".into()),
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let bob = guest("Bob").await;
    let offered = join(
        https.as_ref(),
        &JoinStep::Inspect {
            invitation: signed_invitation.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(
            &offered,
            JoinReply::Invitation { room, workspace_id, name, role: RoomRole::Editor, inviter: Some(inviter), .. }
                if *room == created.room && *workspace_id == created.workspace_id && name == "Design review"
                    && inviter.name == "Alice" && inviter.user_public_key == alice.verifying_key().to_bytes()
        ),
        "{offered:?}"
    );

    // Not admitted yet: the host opens no session for him.
    assert!(
        connect(bob.cert.clone(), bob.key.clone()).await.is_err(),
        "a person not in the room has no session"
    );

    // A certificate without proof of this device's secret admits nothing.
    let JoinReply::Challenge { challenge } = join(
        https.as_ref(),
        &JoinStep::Ask {
            invitation: signed_invitation.clone(),
            device_cert: bob.cert.clone(),
            request_id: [99; 32],
        },
    )
    .await
    .unwrap() else {
        panic!("no challenge");
    };
    assert!(matches!(
        join(
            https.as_ref(),
            &JoinStep::Prove {
                challenge,
                signature: [0; 64],
                name: "Bob".into(),
            }
        )
        .await
        .unwrap(),
        JoinReply::Refused(_)
    ));
    assert_eq!(
        rooms(
            &owner,
            RoomsRequest::Requests {
                room: created.room.clone()
            }
        )
        .await,
        RoomsReply::Requests(vec![])
    );

    // He asks; the request waits for Alice.
    let bob_request = [2; 32];
    assert_eq!(
        ask_to_join(
            https.as_ref(),
            &signed_invitation,
            &bob.cert,
            &bob.key,
            bob_request,
            "Bob"
        )
        .await
        .unwrap(),
        JoinReply::Waiting
    );
    assert_eq!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Waiting
    );
    let RoomsReply::Requests(waiting) = rooms(
        &owner,
        RoomsRequest::Requests {
            room: created.room.clone(),
        },
    )
    .await
    else {
        panic!("no requests");
    };
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].name, "Bob");
    assert_eq!(waiting[0].user_public_key, bob.cert.user_public_key);
    assert_eq!(waiting[0].role, RoomRole::Editor);

    // A reloaded page uses the retained request and certified device to
    // recover Waiting, without consuming a second invitation.
    assert_eq!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Waiting
    );

    // Someone else with the same invitation is turned away while he waits.
    let carol = guest("Carol").await;
    assert!(matches!(
        ask_to_join(
            https.as_ref(),
            &signed_invitation,
            &carol.cert,
            &carol.key,
            [3; 32],
            "Carol"
        )
        .await
        .unwrap(),
        JoinReply::Refused(_)
    ));

    // Alice admits him: he is a member by his own root, and subscribes.
    let admitted = room(
        rooms(
            &owner,
            RoomsRequest::Admit {
                room: created.room.clone(),
                request_id: bob_request,
            },
        )
        .await,
    );
    // Losing the admission response must not require a second invitation.
    assert_eq!(
        room(
            rooms(
                &owner,
                RoomsRequest::Admit {
                    room: created.room.clone(),
                    request_id: bob_request,
                }
            )
            .await
        ),
        admitted,
    );
    let bob_member = admitted
        .members
        .iter()
        .find(|member| member.user_public_key == bob.cert.user_public_key)
        .expect("Bob is in the roster");
    assert_eq!(bob_member.role, RoomRole::Editor);
    assert_eq!(bob_member.name.as_deref(), Some("Bob"));
    assert!(!bob_member.owner && !bob_member.removed);
    assert!(matches!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Admitted {
            role: RoomRole::Editor,
            ..
        }
    ));
    let mut member = connect(bob.cert.clone(), bob.key.clone()).await.unwrap();
    subscribe(&mut member, created.workspace_id).await.unwrap();

    // He administers nothing: the host's rooms are its user's.
    assert!(matches!(
        rooms(&member, RoomsRequest::List).await,
        RoomsReply::Refused(_)
    ));

    // An editor cannot change roles or remove a member. Both
    // requests go through the production HTTPS administration endpoint.
    for request in [
        RoomsRequest::SetRole {
            room: created.room.clone(),
            request_id: [10; 32],
            user_public_key: bob.cert.user_public_key,
            role: RoomRole::Viewer,
        },
        RoomsRequest::Remove {
            room: created.room.clone(),
            request_id: [11; 32],
            user_public_key: bob.cert.user_public_key,
        },
        RoomsRequest::Invite {
            room: created.room.clone(),
            request_id: [12; 32],
            role: RoomRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
        },
        RoomsRequest::Requests {
            room: created.room.clone(),
        },
        RoomsRequest::Admit {
            room: created.room.clone(),
            request_id: bob_request,
        },
    ] {
        assert!(matches!(
            rooms(&member, request).await,
            RoomsReply::Refused(_)
        ));
    }

    // A page reload opens a new session with Bob's same certificate and
    // root, and rejoins the admitted workspace without owner certification.
    drop(member);
    let mut member = connect(bob.cert.clone(), bob.key.clone()).await.unwrap();
    subscribe(&mut member, created.workspace_id).await.unwrap();
    assert!(matches!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Admitted {
            role: RoomRole::Editor,
            ..
        }
    ));

    // A second invitation: Carol asks and Alice declines.
    let RoomsReply::Invitation {
        signed_invitation: second,
        invite_id: second_id,
    } = rooms(
        &owner,
        RoomsRequest::Invite {
            room: created.room.clone(),
            request_id: [4; 32],
            role: RoomRole::Viewer,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let RoomsReply::Pending(pending) = rooms(
        &owner,
        RoomsRequest::Pending {
            room: created.room.clone(),
        },
    )
    .await
    else {
        panic!("no pending invitations");
    };
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].invite_id, second_id);
    assert_eq!(
        ask_to_join(
            https.as_ref(),
            &second,
            &carol.cert,
            &carol.key,
            [5; 32],
            "Carol"
        )
        .await
        .unwrap(),
        JoinReply::Waiting
    );
    assert_eq!(
        rooms(
            &owner,
            RoomsRequest::Decline {
                room: created.room.clone(),
                request_id: [5; 32],
            },
        )
        .await,
        RoomsReply::Requests(vec![])
    );
    assert_eq!(
        status(https.as_ref(), &carol, [5; 32]).await,
        JoinReply::Declined
    );
    assert!(
        connect(carol.cert.clone(), carol.key.clone())
            .await
            .is_err()
    );

    // Alice cancels the invitation: it offers nothing any more.
    assert_eq!(
        rooms(
            &owner,
            RoomsRequest::Cancel {
                room: created.room.clone(),
                request_id: [6; 32],
                invite_id: second_id,
            },
        )
        .await,
        RoomsReply::Pending(vec![])
    );
    assert!(matches!(
        join(https.as_ref(), &JoinStep::Inspect { invitation: second })
            .await
            .unwrap(),
        JoinReply::Refused(reason) if reason.contains("cancelled")
    ));

    // She makes Bob a viewer, then removes him: his session loses the room.
    let viewer = room(
        rooms(
            &owner,
            RoomsRequest::SetRole {
                room: created.room.clone(),
                request_id: [7; 32],
                user_public_key: bob.cert.user_public_key,
                role: RoomRole::Viewer,
            },
        )
        .await,
    );
    assert!(viewer.members.iter().any(|member| {
        member.user_public_key == bob.cert.user_public_key && member.role == RoomRole::Viewer
    }));
    // Status after a reload reports current governance, not the role in
    // the original admission receipt.
    assert!(matches!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Admitted {
            role: RoomRole::Viewer,
            ..
        }
    ));
    assert!(matches!(
        rooms(
            &owner,
            RoomsRequest::Remove {
                room: created.room.clone(),
                request_id: [8; 32],
                user_public_key: alice.verifying_key().to_bytes(),
            },
        )
        .await,
        RoomsReply::Refused(_)
    ));
    let removed = room(
        rooms(
            &owner,
            RoomsRequest::Remove {
                room: created.room.clone(),
                request_id: [9; 32],
                user_public_key: bob.cert.user_public_key,
            },
        )
        .await,
    );
    assert!(
        removed
            .members
            .iter()
            .any(|member| { member.user_public_key == bob.cert.user_public_key && member.removed })
    );
    assert_eq!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Declined
    );
    assert!(
        subscribe(&mut member, created.workspace_id).await.is_err(),
        "a removed member is not admitted again"
    );
    drop(web);
    relay.shutdown().await;
}

// Reload/reconnect coverage uses the production PeerHandle and PeerStore,
// not just a fresh low-level HTTPS link.
#[expect(
    clippy::significant_drop_tightening,
    reason = "async Relay::shutdown consumes the relay; the suggested later drop is a use-after-move"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invited_peer_resumes_with_its_own_identity_after_page_and_host_reload() {
    use support::{ready, start_peer_with_store, wait_for};
    use tilo_browser_peer::{Host, MemoryPeerStore, PeerStore};

    async fn peer_rooms(peer: &tilo_browser_peer::PeerHandle, request: RoomsRequest) -> RoomsReply {
        let (send, recv) = tokio::sync::oneshot::channel();
        peer.rooms_then(
            request,
            Box::new(move |reply| {
                send.send(reply).unwrap();
            }),
        )
        .unwrap();
        tilo_test_env::within("the session answers the room request", recv)
            .await
            .unwrap()
            .unwrap()
    }
    async fn joined(peer: &tilo_browser_peer::PeerHandle, workspace: u64) {
        ready(&[peer]).await;
        wait_for("governed subscription ready", || async {
            (peer.status().rooms.get(&workspace) == Some(&true)).then_some(())
        })
        .await;
    }

    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "home", DeviceCapabilities::host());
    let owner = enrol(&alice, "owner page", DeviceCapabilities::member());
    let bob = guest("Bob").await;
    let state = tempfile::tempdir().unwrap();
    let running = start_room_host(&alice, &host, state.path(), true).await;
    let owner_peer = start_peer_with_store(
        Host::Https(running.https()),
        owner.cert.clone(),
        host.cert.clone(),
        owner.key.clone(),
        Arc::new(MemoryPeerStore::default()),
    );
    ready(&[&owner_peer]).await;
    let created = room(
        peer_rooms(
            &owner_peer,
            RoomsRequest::Create {
                name: "Resume".into(),
            },
        )
        .await,
    );
    peer_rooms(
        &owner_peer,
        RoomsRequest::Enter {
            room: created.room.clone(),
        },
    )
    .await;
    let RoomsReply::Invitation {
        signed_invitation, ..
    } = peer_rooms(
        &owner_peer,
        RoomsRequest::Invite {
            room: created.room.clone(),
            request_id: [20; 32],
            role: RoomRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let request = [21; 32];
    assert_eq!(
        ask_to_join(
            running.https().as_ref(),
            &signed_invitation,
            &bob.cert,
            &bob.key,
            request,
            "Bob"
        )
        .await
        .unwrap(),
        JoinReply::Waiting
    );
    // Decisions on the same request serialize even while admission awaits
    // publication. A decline cannot report success after admission committed.
    let authority = running.relay.rooms().unwrap();
    let (admission, decline) = tokio::join!(
        authority.page_rooms(
            &owner.cert,
            RoomsRequest::Admit {
                room: created.room.clone(),
                request_id: request,
            }
        ),
        authority.page_rooms(
            &owner.cert,
            RoomsRequest::Decline {
                room: created.room.clone(),
                request_id: request,
            }
        ),
    );
    room(admission);
    assert!(matches!(decline, RoomsReply::Refused(_)), "{decline:?}");

    // A profile record belonging to another certified device/root is a
    // hard error. Its identity must never be silently replaced on reload.
    let mismatched = Arc::new(MemoryPeerStore::default());
    let recorded = tilo_browser_peer::PeerRecord {
        cert: Some(owner.cert.clone()),
        host_cert: Some(host.cert.clone()),
        outbox: vec![],
    };
    mismatched.save(&recorded).unwrap();
    let wrong = start_peer_with_store(
        Host::Https(running.https()),
        bob.cert.clone(),
        host.cert.clone(),
        bob.key.clone(),
        mismatched.clone(),
    );
    wait_for("mismatched identity refused", || async {
        let status = wrong.status();
        (status.authenticated || status.phase == tilo_browser_peer::TransportPhase::Failed)
            .then_some(())
    })
    .await;
    assert_eq!(
        wrong.status().phase,
        tilo_browser_peer::TransportPhase::Failed
    );
    assert_eq!(mismatched.load().as_ref(), Some(&recorded));
    wrong.close();

    let store = Arc::new(MemoryPeerStore::default());
    let peer = start_peer_with_store(
        Host::Https(running.https()),
        bob.cert.clone(),
        host.cert.clone(),
        bob.key.clone(),
        store.clone(),
    );
    peer.join(created.workspace_id).unwrap();
    joined(&peer, created.workspace_id).await;
    peer.reconnect().unwrap();
    joined(&peer, created.workspace_id).await;
    assert_eq!(store.load().unwrap().cert.as_ref(), Some(&bob.cert));
    assert_ne!(bob.cert.user_public_key, alice.verifying_key().to_bytes());
    peer.close();
    wait_for("invited page closed", || async {
        (peer.status().phase == tilo_browser_peer::TransportPhase::Closed).then_some(())
    })
    .await;
    owner_peer.close();
    drop(running.web);
    running.relay.shutdown().await;

    // Both the host's committed governance and the page's saved peer
    // record survive. No reissue, certification by Alice, or owner root.
    let running = start_room_host(&alice, &host, state.path(), true).await;
    assert!(matches!(
        status(running.https().as_ref(), &bob, request).await,
        JoinReply::Admitted {
            role: RoomRole::Editor,
            ..
        }
    ));
    let restored = start_peer_with_store(
        Host::Https(running.https()),
        bob.cert.clone(),
        host.cert.clone(),
        bob.key.clone(),
        store.clone(),
    );
    restored.join(created.workspace_id).unwrap();
    joined(&restored, created.workspace_id).await;
    assert_eq!(store.load().unwrap().cert.as_ref(), Some(&bob.cert));
    assert!(matches!(
        peer_rooms(
            &restored,
            RoomsRequest::Remove {
                room: created.room,
                request_id: [22; 32],
                user_public_key: bob.cert.user_public_key,
            }
        )
        .await,
        RoomsReply::Refused(_)
    ));
    restored.close();
    drop(running.web);
    running.relay.shutdown().await;
}
