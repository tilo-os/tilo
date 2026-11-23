use super::*;

/// The live session's membership: who joined, who left, who was told.
mod live_session_tests;
/// An agent from another of the person's devices, and the grant that lets it in here.
mod other_device_agents_tests;
/// Remote actions: who may still act, and what acting costs.
mod remote_action_tests;
/// Invocations run as the key that signed them.
mod signed_invocation_tests;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

pub(super) fn certified_device(
    root: &ed25519_dalek::SigningKey,
    user: [u8; 32],
    label: &str,
) -> (AuthenticatedPeer, ed25519_dalek::SigningKey) {
    let (key, device) = DeviceIdentity::generate(label, 1);
    let mut cert = DeviceCert {
        user_public_key: user,
        device_public_key: device.public_key,
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    // Exercise certificate signature and nonce proof, matching the server
    // handshake rather than constructing unchecked AuthenticatedPeer data.
    let hello = crate::auth::Hello::new(cert, [2; 32]);
    let peer = crate::auth::accept_hello(
        &hello,
        &user,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    let proof = crate::auth::build_hello_ack2(&key, &[1; 32]);
    crate::auth::verify_hello_ack2(&proof, &peer.device_public_key, &[1; 32]).unwrap();
    (peer, key)
}

/// The trust one test's connection answers at, kept alive beside the
/// context that borrows them.
struct Holds {
    trust: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
    metrics: Arc<crate::metrics::Metrics>,
}

impl Holds {
    /// Holds that admit this server's own person's devices and revoke nobody.
    fn open() -> Self {
        Self {
            trust: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            metrics: Arc::new(crate::metrics::Metrics::default()),
        }
    }

    /// What `who`'s connection is handled in, with every hold a test does not
    /// vary left closed.
    fn ctx<'a>(
        &'a self,
        workspaces: &'a WorkspaceRegistry,
        outbound: &'a mpsc::Sender<Envelope>,
        disconnect: &'a mpsc::Sender<()>,
        who: (&'a [u8; 32], &'a DeviceCert),
    ) -> SessionCtx<'a> {
        SessionCtx {
            workspaces,
            outbound,
            content: Box::leak(Box::default()),
            own_user_public_key: who.0,
            own_device_public_key: &who.1.device_public_key,
            app_serving: None,
            metrics: &self.metrics,
            trust_policy: &self.trust,
            blocked: Box::leak(Box::default()),
            disconnect,
            stop: None,
            presence: None,
            revocations: &self.revocations,
            admin: None,
            fetch: None,
            agents: Box::leak(Box::default()),
            environments: Box::leak(Box::default()),
            agent: Box::leak(Box::default()),
            execution: Box::leak(Box::new(tilo_actions::remote::ExecutionLease::mint())),
        }
    }
}

/// Counts what reaches the app action server; needs no reply.
#[derive(Default)]
pub(super) struct CountingServer(std::sync::atomic::AtomicUsize);

impl crate::app_serving::AppActionServer for CountingServer {
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
        _caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // A request is always answered: this app action server counts it and refuses it.
        tilo_actions::remote::refuse(
            &message,
            tilo_actions::remote::ActionCallError::Refused {
                reason: "counted".into(),
            },
        )
        .map(tilo_actions::remote::Pending::ready)
    }
}
