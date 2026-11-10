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
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
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
        stop: None,
        presence: None,
        revocations: &revocations,
        carried_calls: None,
        admin: None,
        here: None,
        fetch: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        agent: Box::leak(Box::default()),
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
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
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
        stop: None,
        presence: None,
        revocations: &revocations,
        carried_calls: None,
        admin: None,
        here: None,
        fetch: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: Some(&limits),
        resumes: Box::leak(Box::default()),
        agent: Box::leak(Box::default()),
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

/// A plane that leaves every request unanswered (`None`), or answers each
/// with a `Pending` whose completer it drops.
struct SilentPlane {
    abandons: bool,
}

impl crate::app_serving::AppServingPlane for SilentPlane {
    fn serve(
        &self,
        _workspace_id: u64,
        _caller: &tilo_actions::remote::Caller,
        _message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        self.abandons
            .then(|| tilo_actions::remote::pending::<tilo_actions::remote::RemoteActionMessage>().1)
    }

    fn serve_host(
        &self,
        caller: &tilo_actions::remote::Caller,
        _certificate: &tilo_user_identity::DeviceCert,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        self.serve(0, caller, message)
    }
}

fn catalogue_request() -> Vec<u8> {
    tilo_actions::remote::RemoteActionFrame::new(
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
            correlation: 7,
            query: tilo_actions::remote::CatalogueQuery::default(),
        },
    )
    .encode()
    .unwrap()
}

/// The catalogue refusal a `HostAction` or `RemoteAction` answer carries.
fn refusal(frame: &[u8]) -> tilo_actions::remote::ActionHostError {
    match tilo_actions::remote::RemoteActionFrame::decode(frame)
        .unwrap()
        .message
    {
        tilo_actions::remote::RemoteActionMessage::CatalogueResponse {
            correlation: 7,
            result: Err(error),
        } => error,
        other => panic!("expected a refused catalogue answer, got {other:?}"),
    }
}

/// Every request a host can read is answered: one the plane leaves
/// unanswered, one it abandons, and a host operation it does not serve are
/// refusals with a reason, never silence the asker waits on forever.
#[tokio::test]
async fn requests_the_plane_does_not_answer_are_refused() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    let (a, _) = certified_device(&root, user.public_key, "a");
    let id = 13;
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    let (atx, mut arx) = mpsc::channel(4);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
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
    let silent: Arc<dyn crate::app_serving::AppServingPlane> =
        Arc::new(SilentPlane { abandons: false });
    let abandoning: Arc<dyn crate::app_serving::AppServingPlane> =
        Arc::new(SilentPlane { abandons: true });
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &host.cert,
        own_device_public_key: &host.cert.device_public_key,
        app_serving: Some(&silent),
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        stop: None,
        presence: None,
        revocations: &revocations,
        carried_calls: None,
        admin: None,
        here: None,
        fetch: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        agent: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: host.device_public_key,
    };

    // A workspace request the plane leaves unanswered is undeliverable at once.
    let answer = handle_remote_action(&a, &session, &[id], route(), catalogue_request()).await;
    let Some(Envelope::RemoteActionUndeliverable { reason, .. }) = answer else {
        panic!("an unanswered request is refused, got {answer:?}");
    };
    assert_eq!(
        UnavailableReason::from_wire_reason(&reason),
        Some(UnavailableReason::UnsupportedPeer),
        "{reason}"
    );

    // A host operation the plane does not serve is refused at once.
    let Some(Envelope::HostAction { frame }) =
        handle_host_action(&a, &session, &catalogue_request())
    else {
        panic!("an unserved host operation is answered");
    };
    assert!(matches!(
        refusal(&frame),
        tilo_actions::remote::ActionHostError::Unavailable { .. }
    ));

    // An answer the plane abandons arrives as a refusal on the connection.
    let abandoned = SessionCtx {
        app_serving: Some(&abandoning),
        carried_calls: None,
        admin: None,
        here: None,
        fetch: None,
        ..session
    };
    assert_eq!(
        handle_remote_action(&a, &abandoned, &[id], route(), catalogue_request()).await,
        None
    );
    let Some(Envelope::RemoteAction { frame, .. }) = arx.recv().await else {
        panic!("an abandoned room request is answered on the connection");
    };
    assert!(matches!(
        refusal(&frame),
        tilo_actions::remote::ActionHostError::Unavailable { .. }
    ));
    assert_eq!(
        handle_host_action(&a, &abandoned, &catalogue_request()),
        None
    );
    let Some(Envelope::HostAction { frame }) = arx.recv().await else {
        panic!("an abandoned host operation is answered on the connection");
    };
    assert!(matches!(
        refusal(&frame),
        tilo_actions::remote::ActionHostError::Unavailable { .. }
    ));

    // A frame that does not decode names no request; nothing is answered.
    assert_eq!(handle_host_action(&a, &session, b"not a frame"), None);
}
