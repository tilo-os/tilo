//! An invocation this host serves runs as the key that signed it: unsigned,
//! changed, stale, revoked, expired, replayed or foreign signatures never
//! reach the app action server.

use super::*;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::invocation::sign_invocation_now;
use tilo_server_protocol::refusal::Refusal;

/// What reached the app action server: the caller and what it asked.
#[derive(Default)]
struct Recording(std::sync::Mutex<Vec<(Caller, RemoteActionMessage)>>);

impl crate::app_serving::AppActionServer for Recording {
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        self.0
            .lock()
            .unwrap()
            .push((caller.clone(), message.clone()));
        tilo_actions::remote::refuse(
            &message,
            ActionCallError::Refused {
                reason: "recorded".into(),
            },
        )
        .map(Pending::ready)
    }
}

fn request() -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: "so.tilo.notes".into(),
            address: tilo_actions::ActionAddress::new("note:1", "rename"),
        },
        argument: InvokeArgument::Text("Plan".into()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn frame(message: RemoteActionMessage) -> Vec<u8> {
    RemoteActionFrame::new(message).encode().unwrap()
}

fn signed_by(
    workspace: WorkspaceId,
    request: &InvokeRequest,
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> InvokeRequest {
    let mut request = request.clone();
    request.signed = Some(Box::new(
        sign_invocation_now(&workspace.to_string(), &request, cert, key, unix_now_ns()).unwrap(),
    ));
    request
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one session's whole setup, its calls and their answers"
)]
async fn an_invocation_runs_only_as_the_key_that_signed_it() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    let (a, a_key) = certified_device(&root, user.public_key, "a");
    let (b, b_key) = certified_device(&root, user.public_key, "b");
    let (outsider, outsider_key) = certified_device(&root, user.public_key, "outsider");
    let id = WorkspaceId::from_u128(21);
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(tmp.path(), id).unwrap();
    wr.list_environment(a.device_public_key, true);
    wr.list_environment(b.device_public_key, true);
    let (atx, _arx) = mpsc::channel(8);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    for device in [&a, &outsider] {
        wr.add_subscriber(Subscriber {
            device_public_key: device.device_public_key,
            tx: atx.clone(),
            disconnect: disconnect.clone(),
            content_waiting: Arc::default(),
            peer: device.clone(),
        });
    }
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let holds = Holds::open();
    let recording = Arc::new(Recording::default());
    let server: Arc<dyn crate::app_serving::AppActionServer> = recording.clone();
    let mut session = holds.ctx(&registry, &atx, &disconnect, (&user.public_key, &host.cert));
    session.app_serving = Some(&server);
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: host.device_public_key,
    };
    let counted = || recording.0.lock().unwrap().len();
    let refused = |answer: Option<Envelope>| match answer {
        Some(Envelope::RemoteActionUndeliverable { refusal, .. }) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let invoke = |request: InvokeRequest| frame(RemoteActionMessage::Invoke(request));

    // A signed invocation runs as its signer.
    let asked = signed_by(id, &request(), &a.cert, &a_key);
    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), invoke(asked.clone())).await,
        None
    );
    {
        let seen = recording.0.lock().unwrap();
        let [(caller, RemoteActionMessage::Invoke(ran))] = seen.as_slice() else {
            panic!("one invocation reached the app action server: {seen:?}");
        };
        assert_eq!(
            *caller,
            Caller::authenticated(user.public_key, a.device_public_key)
        );
        // The signed invocation travels with the request to where it runs.
        assert_eq!(ran.signed, asked.signed);
    }

    // An unsigned one is refused.
    let refusal =
        refused(handle_remote_action(&a, &session, &[id], route(), invoke(request())).await);
    assert!(
        matches!(&refusal, Refusal::NotAllowed(reason) if reason.starts_with("invocation_unsigned")),
        "{refusal:?}"
    );

    // Changed after signing: its target, its argument, or the workspace it is
    // sent to.
    let mut retargeted = signed_by(id, &request(), &a.cert, &a_key);
    retargeted.target.address.action = "delete".into();
    let mut reargued = signed_by(id, &request(), &a.cert, &a_key);
    reargued.argument = InvokeArgument::Text("Other".into());
    let elsewhere = signed_by(WorkspaceId::from_u128(22), &request(), &a.cert, &a_key);
    for changed in [retargeted, reargued, elsewhere] {
        let refusal =
            refused(handle_remote_action(&a, &session, &[id], route(), invoke(changed)).await);
        assert!(
            matches!(&refusal, Refusal::NotAllowed(reason) if reason.starts_with("invocation_signature_invalid")),
            "{refusal:?}"
        );
    }

    // Signed by another member's key and sent on this connection: its answer
    // would reach a device that never asked.
    let others = signed_by(id, &request(), &b.cert, &b_key);
    let refusal = refused(handle_remote_action(&a, &session, &[id], route(), invoke(others)).await);
    assert!(
        matches!(&refusal, Refusal::NotAllowed(reason) if reason.contains("another key")),
        "{refusal:?}"
    );

    // Signed under a certificate of the same key that has expired.
    let mut expired = a.cert.clone();
    expired.expires_at_ns = Some(2);
    sign_device_cert(&mut expired, &root).unwrap();
    let late = signed_by(id, &request(), &expired, &a_key);
    let refusal = refused(handle_remote_action(&a, &session, &[id], route(), invoke(late)).await);
    assert!(
        matches!(&refusal, Refusal::NotAllowed(reason) if reason.starts_with("invocation_signature_invalid")),
        "{refusal:?}"
    );

    // A key the workspace does not admit is denied, and nothing runs.
    let foreign = signed_by(id, &request(), &outsider.cert, &outsider_key);
    let refusal =
        refused(handle_remote_action(&outsider, &session, &[id], route(), invoke(foreign)).await);
    assert!(matches!(refusal, Refusal::NotAMember(_)), "{refusal:?}");

    // One invocation id names one request: signed again for the same request
    // it is a retry, for another action it is a conflict, as the invocation log
    // answers one.
    let retry = signed_by(id, &asked, &a.cert, &a_key);
    assert_eq!(
        handle_remote_action(&a, &session, &[id], route(), invoke(retry)).await,
        None
    );
    let mut again = asked.clone();
    again.target.address.action = "delete".into();
    let again = signed_by(id, &again, &a.cert, &a_key);
    let Some(Envelope::RemoteAction { frame: answer, .. }) =
        handle_remote_action(&a, &session, &[id], route(), invoke(again)).await
    else {
        panic!("a reused invocation id is answered");
    };
    assert_eq!(
        RemoteActionFrame::decode(&answer).unwrap().message,
        RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: asked.invocation,
            status: ReceiptStatus::Rejected {
                error: ActionCallError::InvocationConflict,
            },
        })
    );
    assert_eq!(counted(), 2, "only the signed invocation and its retry ran");

    // Cancelling names an invocation the signer started: it still reaches the
    // provider, which holds it to its caller.
    assert_eq!(
        handle_remote_action(
            &a,
            &session,
            &[id],
            route(),
            frame(RemoteActionMessage::Cancel {
                invocation: asked.invocation,
            }),
        )
        .await,
        None
    );
    assert!(matches!(
        recording.0.lock().unwrap().last(),
        Some((_, RemoteActionMessage::Cancel { .. }))
    ));

    // A key its person revoked is told so, and nothing runs.
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: user.public_key,
        revoked_device_public_key: a.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    holds.revocations.write().unwrap().insert(entry).unwrap();
    let before = counted();
    let revoked = signed_by(id, &request(), &a.cert, &a_key);
    let refusal =
        refused(handle_remote_action(&a, &session, &[id], route(), invoke(revoked)).await);
    assert!(matches!(refusal, Refusal::NotAMember(_)), "{refusal:?}");
    assert_eq!(counted(), before);
}
