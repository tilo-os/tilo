//! A remote action is refused once its device, member or agent may no
//! longer act, and an allowed one spends its member's change budget.

use super::*;

#[tokio::test]
async fn revoked_device_remote_action_is_refused() {
    invalidated_device_remote_action_is_refused(false).await;
}

#[tokio::test]
async fn removed_member_remote_action_is_refused() {
    invalidated_device_remote_action_is_refused(true).await;
}

/// No access audit runs here: the handler itself refuses the next frame of
/// a device revoked or removed while its subscription is still registered.
async fn invalidated_device_remote_action_is_refused(remove_membership: bool) {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    let (a, _) = certified_device(&root, user.public_key, "a");
    let id = 11;
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
        role: a.role,
        tx: atx.clone(),
        disconnect: disconnect.clone(),
    });
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let counting = Arc::new(CountingPlane::default());
    let plane: Arc<dyn crate::app_serving::AppServingPlane> = counting.clone();
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &host.cert,
        own_device_public_key: &host.cert.device_public_key,
        app_serving: Some(&plane),
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let frame = tilo_actions::remote::RemoteActionFrame::new(
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: tilo_actions::remote::CatalogueQuery::default(),
        },
    )
    .encode()
    .unwrap();
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: host.device_public_key,
    };
    let served = || counting.0.load(std::sync::atomic::Ordering::SeqCst);

    // A current member reaches the plane through this very path.
    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), frame.clone()).await,
        None
    );
    assert_eq!(served(), 1);

    if remove_membership {
        let workspace = registry.lock().await[&id].clone();
        crate::workspace_io::replace_membership(&workspace, Vec::new())
            .await
            .unwrap();
        assert!(
            workspace
                .lock()
                .await
                .subscribers
                .contains_key(&a.device_public_key)
        );
    } else {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: a.device_public_key,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        revocations.write().unwrap().insert(entry).unwrap();
    }
    let refused = handle_remote_action(&a, &session, &[id], route(), frame).await;
    let Some(Envelope::RemoteActionUndeliverable {
        reason,
        to_device_public_key,
        ..
    }) = refused
    else {
        panic!("an invalidated device is answered as undeliverable, got {refused:?}");
    };
    assert_eq!(to_device_public_key, host.device_public_key);
    assert_eq!(
        UnavailableReason::from_wire_reason(&reason),
        Some(UnavailableReason::NotAuthorised),
        "{reason}"
    );
    assert_eq!(served(), 1, "an invalidated device never reaches the plane");
}

/// An action the host runs for a member spends that member's change
/// budget, as a change they send does; reading the catalogue spends none.
#[tokio::test]
async fn a_members_actions_served_by_the_host_spend_their_change_budget() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    // A guest: another person than the host's, whose budget applies.
    let (guest_root, guest) = UserIdentity::generate("guest", 1);
    let (a, _) = certified_device(&guest_root, guest.public_key, "a");
    let id = 12;
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
        role: a.role,
        tx: atx.clone(),
        disconnect: disconnect.clone(),
    });
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let trust = Arc::new(std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let counting = Arc::new(CountingPlane::default());
    let plane: Arc<dyn crate::app_serving::AppServingPlane> = counting.clone();
    let limits =
        crate::member_limits::MemberLimits::new(crate::member_limits::MemberLimitsConfig {
            changes_per_window: 1,
            window_secs: 3600,
            burst: 2,
            ..crate::member_limits::MemberLimitsConfig::default()
        });
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &host.cert,
        own_device_public_key: &host.cert.device_public_key,
        app_serving: Some(&plane),
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: Some(&limits),
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let encode = |message| {
        tilo_actions::remote::RemoteActionFrame::new(message)
            .encode()
            .unwrap()
    };
    let invoke = || {
        encode(tilo_actions::remote::RemoteActionMessage::Invoke(
            tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: "record:so.tilo.discord:".into(),
                    address: tilo_actions::ActionAddress::new("", "link-account"),
                },
                argument: tilo_actions::remote::InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            },
        ))
    };
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: host.device_public_key,
    };
    let served = || counting.0.load(std::sync::atomic::Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(
            handle_remote_action(&a, &session, &[id], route(), invoke()).await,
            None
        );
    }
    assert_eq!(served(), 2, "within the burst, the host runs them");
    let refused = handle_remote_action(&a, &session, &[id], route(), invoke()).await;
    let Some(Envelope::RemoteActionUndeliverable { reason, .. }) = refused else {
        panic!("a spent budget is refused, got {refused:?}");
    };
    assert_eq!(
        UnavailableReason::from_wire_reason(&reason),
        Some(UnavailableReason::Backpressure),
        "{reason}"
    );
    assert_eq!(served(), 2, "a refused action never reaches the host");
    let catalogue = encode(
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: tilo_actions::remote::CatalogueQuery::default(),
        },
    );
    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), catalogue).await,
        None
    );
    assert_eq!(served(), 3, "reading spends no change budget");
}

#[tokio::test]
async fn route_private_refuses_an_agent_whose_delegation_expired() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    wr.admit_device(b.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (btx, _brx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(4);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let agents = AgentRegistry::default();
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: &agents,
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    // Signed for this room, already past its expiry: what a delegation
    // looks like once its window closed under a live session.
    let expired = |device: [u8; 32]| {
        tilo_relay_protocol::delegation::Delegation::sign(
            &root,
            device,
            tilo_relay_protocol::delegation::Delegate::Agent {
                agent_id: "helper".into(),
            },
            "Helper",
            [1, 2, 3],
            tilo_relay_protocol::delegation::DelegateScope {
                rooms: vec![room.to_string()],
                apps: Vec::new(),
            },
            1,
            2,
        )
        .unwrap()
    };

    assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
    assert!(matches!(bpr.try_recv(), Ok(PrivateOutbound::Delivery(_))));

    for agent in [a.device_public_key, b.device_public_key] {
        agents.write().unwrap().clear();
        agents.write().unwrap().insert(agent, Some(expired(agent)));
        assert_eq!(
            route_private(&action, &a, &session).await.unwrap(),
            Some(UnavailableReason::NotAuthorised),
            "an expired agent at either end is refused per frame"
        );
        assert!(bpr.try_recv().is_err(), "nothing reaches the target");
    }
}
