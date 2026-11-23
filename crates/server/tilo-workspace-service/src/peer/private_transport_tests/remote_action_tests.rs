//! A remote action is refused once its device, member or agent may no
//! longer act.

use super::*;
use tilo_server_protocol::WorkspaceId;

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
#[expect(
    clippy::too_many_lines,
    reason = "one session's whole setup, its calls and their answers"
)]
async fn invalidated_device_remote_action_is_refused(remove_membership: bool) {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    let (a, _) = certified_device(&root, user.public_key, "a");
    let id = WorkspaceId::from_u128(11);
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
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
        content_waiting: Arc::default(),
        peer: a.clone(),
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
    let counting = Arc::new(CountingServer::default());
    let server: Arc<dyn crate::app_serving::AppActionServer> = counting.clone();
    let session = SessionCtx {
        workspaces: &registry,
        outbound: &atx,
        content: Box::leak(Box::default()),
        own_user_public_key: &user.public_key,
        own_device_public_key: &host.cert.device_public_key,
        app_serving: Some(&server),
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        stop: None,
        presence: None,
        revocations: &revocations,
        admin: None,
        fetch: None,
        agents: Box::leak(Box::default()),
        environments: Box::leak(Box::default()),
        agent: Box::leak(Box::default()),
        execution: Box::leak(Box::new(tilo_actions::remote::ExecutionLease::mint())),
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
    let counted = || counting.0.load(std::sync::atomic::Ordering::SeqCst);

    // A current member reaches the app action server through this very path.
    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), frame.clone()).await,
        None
    );
    assert_eq!(counted(), 1);

    if remove_membership {
        let workspace = registry.lock().await[&id].clone();
        crate::workspace_io::replace_membership(&workspace, Vec::new())
            .await
            .unwrap();
        assert!(workspace.lock().await.has_subscriber(&a.device_public_key));
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
        refusal,
        to_device_public_key,
        ..
    }) = refused
    else {
        panic!("an invalidated device is answered as undeliverable, got {refused:?}");
    };
    assert_eq!(to_device_public_key, host.device_public_key);
    assert!(
        matches!(
            refusal,
            tilo_server_protocol::refusal::Refusal::NotAMember(_)
        ),
        "{refusal}"
    );
    assert_eq!(
        counted(),
        1,
        "an invalidated device never reaches the app action server"
    );
}

/// An app action server that leaves every request unanswered (`None`), or answers each
/// with a `Pending` whose completer it drops.
struct SilentServer {
    abandons: bool,
}

impl crate::app_serving::AppActionServer for SilentServer {
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
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
        self.serve(WorkspaceId::nil(), caller, message)
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
fn refusal(frame: &[u8]) -> tilo_actions::remote::ActionCallError {
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

/// Every request a host can read is answered: one the app action server leaves
/// unanswered, one it abandons, and a host operation it does not serve are
/// refusals with a reason, never silence the asker waits on forever.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one session's whole setup, its calls and their answers"
)]
async fn requests_the_server_does_not_answer_are_refused() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    let (a, _) = certified_device(&root, user.public_key, "a");
    let id = WorkspaceId::from_u128(13);
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
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
        content_waiting: Arc::default(),
        peer: a.clone(),
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
    let silent: Arc<dyn crate::app_serving::AppActionServer> =
        Arc::new(SilentServer { abandons: false });
    let abandoning: Arc<dyn crate::app_serving::AppActionServer> =
        Arc::new(SilentServer { abandons: true });
    let session = SessionCtx {
        workspaces: &registry,
        outbound: &atx,
        content: Box::leak(Box::default()),
        own_user_public_key: &user.public_key,
        own_device_public_key: &host.cert.device_public_key,
        app_serving: Some(&silent),
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        stop: None,
        presence: None,
        revocations: &revocations,
        admin: None,
        fetch: None,
        agents: Box::leak(Box::default()),
        environments: Box::leak(Box::default()),
        agent: Box::leak(Box::default()),
        execution: Box::leak(Box::new(tilo_actions::remote::ExecutionLease::mint())),
    };
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: host.device_public_key,
    };

    // A workspace request the app action server leaves unanswered is undeliverable at once.
    let answer = handle_remote_action(&a, &session, &[id], route(), catalogue_request()).await;
    let Some(Envelope::RemoteActionUndeliverable {
        refusal: undelivered,
        ..
    }) = answer
    else {
        panic!("an unanswered request is refused, got {answer:?}");
    };
    assert!(
        matches!(
            undelivered,
            tilo_server_protocol::refusal::Refusal::Other(_)
        ),
        "{undelivered}"
    );

    // A host operation the app action server does not serve is refused at once.
    let Some(Envelope::HostAction { frame }) =
        handle_host_action(&a, &session, &catalogue_request())
    else {
        panic!("an unserved host operation is answered");
    };
    assert!(matches!(
        refusal(&frame),
        tilo_actions::remote::ActionCallError::Unavailable { .. }
    ));

    // An answer the app action server abandons arrives as a refusal on the connection.
    let abandoned = SessionCtx {
        app_serving: Some(&abandoning),
        admin: None,
        fetch: None,
        ..session
    };
    assert_eq!(
        handle_remote_action(&a, &abandoned, &[id], route(), catalogue_request()).await,
        None
    );
    let Some(Envelope::RemoteAction { frame, .. }) = arx.recv().await else {
        panic!("an abandoned workspace request is answered on the connection");
    };
    assert!(matches!(
        refusal(&frame),
        tilo_actions::remote::ActionCallError::Unavailable { .. }
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
        tilo_actions::remote::ActionCallError::Unavailable { .. }
    ));

    // A frame that does not decode names no request; nothing is answered.
    assert_eq!(handle_host_action(&a, &session, b"not a frame"), None);
}
