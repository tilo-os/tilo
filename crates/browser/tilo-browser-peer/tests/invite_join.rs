//! Inviting a different person to a workspace the host governs, page to page
//! over HTTPS: the owner's page issues an invitation; the other person's
//! page, with its own user root and a device it certified itself, redeems
//! it, which admits it by the inviter's own grant; it then holds a session
//! and subscribes, until the owner removes it.

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
use tilo_browser_peer::{HttpClient, HttpsLink};
use tilo_relay_client::https as carrier;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::workspaces::{
    JoinReply, JoinStep, Workspace, WorkspaceRole, WorkspacesReply, WorkspacesRequest,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use support::{TestHost, enrol, start_workspace_host};

async fn workspaces(link: &HttpsLink, request: WorkspacesRequest) -> WorkspacesReply {
    carrier::workspaces(link.client().as_ref(), link.session(), &request)
        .await
        .unwrap()
}

fn workspace(reply: WorkspacesReply) -> Workspace {
    match reply {
        WorkspacesReply::Workspace(workspace) => workspace,
        other => panic!("expected a workspace, got {other:?}"),
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
    let TestHost { relay, web, .. } =
        start_workspace_host(&alice, &host, state.path(), false).await;
    let https: Arc<dyn HttpClient> =
        support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());
    let connect = |cert: DeviceCert, key: SigningKey| {
        let https = https.clone();
        let host = host.cert.device_public_key;
        async move { HttpsLink::connect(https, &cert, host, &RevocationList::new(), &key).await }
    };

    // Alice's page creates a governed workspace and enters it as its owner.
    let mut owner = connect(page.cert.clone(), page.key.clone()).await.unwrap();
    assert!(
        owner.workspaces(),
        "the host administers workspaces for pages"
    );
    let created = workspace(
        workspaces(
            &owner,
            WorkspacesRequest::Create {
                name: "Design review".into(),
            },
        )
        .await,
    );
    let entered = workspace(
        workspaces(
            &owner,
            WorkspacesRequest::Enter {
                workspace: created.workspace.clone(),
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
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = workspaces(
        &owner,
        WorkspacesRequest::Invite {
            workspace: created.workspace.clone(),
            request_id: [1; 32],
            role: WorkspaceRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: Some("Alice".into()),
            wait_list_entry: None,
            recipient: None,
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
            JoinReply::Invitation { workspace, workspace_id, name, role: WorkspaceRole::Editor, inviter: Some(inviter), .. }
                if *workspace == created.workspace && *workspace_id == created.workspace_id && name == "Design review"
                    && inviter.name == "Alice" && inviter.user_public_key == alice.verifying_key().to_bytes()
        ),
        "{offered:?}"
    );

    // Not admitted yet: the host opens no session for him.
    assert!(
        connect(bob.cert.clone(), bob.key.clone()).await.is_err(),
        "a person not in the workspace has no session"
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
        workspaces(
            &owner,
            WorkspacesRequest::Requests {
                workspace: created.workspace.clone()
            }
        )
        .await,
        WorkspacesReply::Requests(vec![])
    );

    // He asks; the invitation admits him by the grant of whoever issued it,
    // so nobody waits to be let in.
    let bob_request = [2; 32];
    let asked = ask_to_join(
        https.as_ref(),
        &signed_invitation,
        &bob.cert,
        &bob.key,
        bob_request,
        "Bob",
    )
    .await
    .unwrap();
    assert!(
        matches!(
            asked,
            JoinReply::Admitted {
                role: WorkspaceRole::Editor,
                ..
            }
        ),
        "{asked:?}"
    );
    assert!(
        matches!(
            status(https.as_ref(), &bob, bob_request).await,
            JoinReply::Admitted {
                role: WorkspaceRole::Editor,
                ..
            }
        ),
        "a reloaded page hears what became of its request"
    );
    // Nobody waits for the owner, so there is nobody to admit.
    assert_eq!(
        workspaces(
            &owner,
            WorkspacesRequest::Requests {
                workspace: created.workspace.clone(),
            }
        )
        .await,
        WorkspacesReply::Requests(vec![])
    );

    // Someone else with the same invitation is turned away: he redeemed it.
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

    // He is a member by his own root, and subscribes.
    let admitted = workspace(
        workspaces(
            &owner,
            WorkspacesRequest::Enter {
                workspace: created.workspace.clone(),
            },
        )
        .await,
    );
    let bob_member = admitted
        .members
        .iter()
        .find(|member| member.user_public_key == bob.cert.user_public_key)
        .expect("Bob is in the roster");
    assert_eq!(bob_member.role, WorkspaceRole::Editor);
    assert_eq!(bob_member.name.as_deref(), Some("Bob"));
    assert!(!bob_member.owner && !bob_member.removed);
    assert!(matches!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Admitted {
            role: WorkspaceRole::Editor,
            ..
        }
    ));
    let mut member = connect(bob.cert.clone(), bob.key.clone()).await.unwrap();
    subscribe(&mut member, created.workspace_id).await.unwrap();

    // He administers nothing: the host's workspaces are its user's.
    assert!(matches!(
        workspaces(&member, WorkspacesRequest::List).await,
        WorkspacesReply::Refused(_)
    ));

    // An editor changes no role and removes nobody else: the owner stays.
    // Each request goes through the production HTTPS administration
    // endpoint.
    for request in [
        WorkspacesRequest::SetRole {
            workspace: created.workspace.clone(),
            request_id: [10; 32],
            user_public_key: bob.cert.user_public_key,
            role: WorkspaceRole::Viewer,
        },
        WorkspacesRequest::Remove {
            workspace: created.workspace.clone(),
            request_id: [11; 32],
            user_public_key: alice.verifying_key().to_bytes(),
        },
        WorkspacesRequest::Invite {
            workspace: created.workspace.clone(),
            request_id: [12; 32],
            role: WorkspaceRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
            wait_list_entry: None,
            recipient: None,
        },
        WorkspacesRequest::Requests {
            workspace: created.workspace.clone(),
        },
        WorkspacesRequest::Admit {
            workspace: created.workspace.clone(),
            request_id: bob_request,
        },
    ] {
        assert!(matches!(
            workspaces(&member, request).await,
            WorkspacesReply::Refused(_)
        ));
    }

    // Removing himself he may: leaving is the one membership change no
    // grant decides. The host answers with the workspace, which keeps his
    // entry marked removed, and stops admitting him.
    let left = workspace(
        workspaces(
            &member,
            WorkspacesRequest::Remove {
                workspace: created.workspace.clone(),
                request_id: [13; 32],
                user_public_key: bob.cert.user_public_key,
            },
        )
        .await,
    );
    assert_eq!(left.workspace, created.workspace);
    assert_eq!(left.workspace_id, created.workspace_id);
    assert!(
        left.members
            .iter()
            .any(|entry| entry.user_public_key == bob.cert.user_public_key && entry.removed),
        "{left:?}"
    );
    assert!(
        !left
            .members
            .iter()
            .any(|entry| entry.user_public_key == bob.cert.user_public_key && !entry.removed)
    );
    assert_eq!(
        status(https.as_ref(), &bob, bob_request).await,
        JoinReply::Declined
    );
    assert!(
        subscribe(&mut member, created.workspace_id).await.is_err(),
        "a person who left is not admitted"
    );
    drop(member);

    // Alice invites him back: a person who left has no way in left, so
    // returning takes a new invitation.
    let WorkspacesReply::Invitation {
        signed_invitation: again,
        ..
    } = workspaces(
        &owner,
        WorkspacesRequest::Invite {
            workspace: created.workspace.clone(),
            request_id: [14; 32],
            role: WorkspaceRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
            wait_list_entry: None,
            recipient: None,
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let bob_again = [15; 32];
    assert!(matches!(
        ask_to_join(
            https.as_ref(),
            &again,
            &bob.cert,
            &bob.key,
            bob_again,
            "Bob"
        )
        .await
        .unwrap(),
        JoinReply::Admitted { .. }
    ));
    let readmitted = workspace(
        workspaces(
            &owner,
            WorkspacesRequest::Enter {
                workspace: created.workspace.clone(),
            },
        )
        .await,
    );
    assert!(readmitted.members.iter().any(|entry| {
        entry.user_public_key == bob.cert.user_public_key
            && !entry.removed
            && entry.role == WorkspaceRole::Editor
    }));

    // A page reload opens a new session with Bob's same certificate and
    // root, and rejoins the admitted workspace without owner certification.
    let mut member = connect(bob.cert.clone(), bob.key.clone()).await.unwrap();
    subscribe(&mut member, created.workspace_id).await.unwrap();
    assert!(matches!(
        status(https.as_ref(), &bob, bob_again).await,
        JoinReply::Admitted {
            role: WorkspaceRole::Editor,
            ..
        }
    ));

    // A second invitation for Carol: Alice cancels it before anyone redeems
    // it, and it offers nothing any more.
    let WorkspacesReply::Invitation {
        signed_invitation: second,
        invite_id: second_id,
    } = workspaces(
        &owner,
        WorkspacesRequest::Invite {
            workspace: created.workspace.clone(),
            request_id: [4; 32],
            role: WorkspaceRole::Viewer,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
            wait_list_entry: None,
            recipient: None,
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let WorkspacesReply::Pending(pending) = workspaces(
        &owner,
        WorkspacesRequest::Pending {
            workspace: created.workspace.clone(),
        },
    )
    .await
    else {
        panic!("no pending invitations");
    };
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].invite_id, second_id);
    assert_eq!(
        workspaces(
            &owner,
            WorkspacesRequest::Cancel {
                workspace: created.workspace.clone(),
                request_id: [6; 32],
                invite_id: second_id,
            },
        )
        .await,
        WorkspacesReply::Pending(vec![])
    );
    assert!(matches!(
        join(https.as_ref(), &JoinStep::Inspect { invitation: second.clone() })
            .await
            .unwrap(),
        JoinReply::Refused(reason) if reason.contains("cancelled")
    ));
    assert!(matches!(
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
        JoinReply::Refused(_)
    ));
    assert!(
        connect(carol.cert.clone(), carol.key.clone())
            .await
            .is_err()
    );

    // She makes Bob a viewer, then removes him: his session loses the workspace.
    let viewer = workspace(
        workspaces(
            &owner,
            WorkspacesRequest::SetRole {
                workspace: created.workspace.clone(),
                request_id: [7; 32],
                user_public_key: bob.cert.user_public_key,
                role: WorkspaceRole::Viewer,
            },
        )
        .await,
    );
    assert!(viewer.members.iter().any(|member| {
        member.user_public_key == bob.cert.user_public_key && member.role == WorkspaceRole::Viewer
    }));
    // Status after a reload reports current governance, not the role in
    // the original admission receipt.
    assert!(matches!(
        status(https.as_ref(), &bob, bob_again).await,
        JoinReply::Admitted {
            role: WorkspaceRole::Viewer,
            ..
        }
    ));
    assert!(matches!(
        workspaces(
            &owner,
            WorkspacesRequest::Remove {
                workspace: created.workspace.clone(),
                request_id: [8; 32],
                user_public_key: alice.verifying_key().to_bytes(),
            },
        )
        .await,
        WorkspacesReply::Refused(_)
    ));
    let removed = workspace(
        workspaces(
            &owner,
            WorkspacesRequest::Remove {
                workspace: created.workspace.clone(),
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
        status(https.as_ref(), &bob, bob_again).await,
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

    async fn peer_workspaces(
        peer: &tilo_browser_peer::PeerHandle,
        request: WorkspacesRequest,
    ) -> WorkspacesReply {
        let (send, recv) = tokio::sync::oneshot::channel();
        peer.rooms_then(
            request,
            Box::new(move |reply| {
                send.send(reply).unwrap();
            }),
        )
        .unwrap();
        tilo_test_env::within("the session answers the workspace request", recv)
            .await
            .unwrap()
            .unwrap()
    }
    async fn joined(peer: &tilo_browser_peer::PeerHandle, workspace: u64) {
        ready(&[peer]).await;
        wait_for("governed subscription ready", || async {
            (peer.status().workspaces.get(&workspace) == Some(&true)).then_some(())
        })
        .await;
    }

    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "home", DeviceCapabilities::host());
    let owner = enrol(&alice, "owner page", DeviceCapabilities::member());
    let bob = guest("Bob").await;
    let state = tempfile::tempdir().unwrap();
    let running = start_workspace_host(&alice, &host, state.path(), true).await;
    let owner_peer = start_peer_with_store(
        Host::Https(running.https()),
        owner.cert.clone(),
        host.cert.clone(),
        owner.key.clone(),
        Arc::new(MemoryPeerStore::default()),
    );
    ready(&[&owner_peer]).await;
    let created = workspace(
        peer_workspaces(
            &owner_peer,
            WorkspacesRequest::Create {
                name: "Resume".into(),
            },
        )
        .await,
    );
    peer_workspaces(
        &owner_peer,
        WorkspacesRequest::Enter {
            workspace: created.workspace.clone(),
        },
    )
    .await;
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = peer_workspaces(
        &owner_peer,
        WorkspacesRequest::Invite {
            workspace: created.workspace.clone(),
            request_id: [20; 32],
            role: WorkspaceRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
            wait_list_entry: None,
            recipient: None,
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let request = [21; 32];
    // The invitation admits him by itself: no decision of the owner stands
    // between his ask and his membership.
    assert!(
        matches!(
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
            JoinReply::Admitted {
                role: WorkspaceRole::Editor,
                ..
            }
        ),
        "the invitation admits the person who holds it"
    );

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
    let running = start_workspace_host(&alice, &host, state.path(), true).await;
    assert!(matches!(
        status(running.https().as_ref(), &bob, request).await,
        JoinReply::Admitted {
            role: WorkspaceRole::Editor,
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
    // He leaves: the host answers with the workspace, keeping his entry
    // marked removed and admitting him no more.
    let left = workspace(
        peer_workspaces(
            &restored,
            WorkspacesRequest::Remove {
                workspace: created.workspace.clone(),
                request_id: [22; 32],
                user_public_key: bob.cert.user_public_key,
            },
        )
        .await,
    );
    assert_eq!(left.workspace_id, created.workspace_id);
    assert!(
        left.members
            .iter()
            .any(|entry| entry.user_public_key == bob.cert.user_public_key && entry.removed),
        "{left:?}"
    );
    assert!(
        !left
            .members
            .iter()
            .any(|entry| entry.user_public_key == bob.cert.user_public_key && !entry.removed)
    );
    assert_eq!(
        status(running.https().as_ref(), &bob, request).await,
        JoinReply::Declined
    );
    assert!(
        HttpsLink::connect(
            running.https(),
            &bob.cert,
            host.cert.device_public_key,
            &RevocationList::new(),
            &bob.key,
        )
        .await
        .is_err(),
        "a person who left has no session to resume"
    );
    restored.close();
    drop(running.web);
    running.relay.shutdown().await;
}
