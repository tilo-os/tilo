//! Bounded actor work and cancellation for canonical native enrollment.
use super::VerifiedNativeEnrollment;
use crate::{
    network::actor::HostIdentity,
    shell::{native_workspace_owner::NativeWorkspaceOwner, state::WorkspaceId},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tilo_lock::MutexRecover as _;
use tokio::sync::watch;

const MAX_PENDING: usize = 8;
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Shared {
    result: Mutex<Option<Result<VerifiedNativeEnrollment, String>>>,
    cancelled: AtomicBool,
    cancel: watch::Sender<bool>,
}
#[derive(Clone)]
pub(crate) struct Attempt {
    id: u64,
    local: WorkspaceId,
    identity_topic: u64,
    owner: NativeWorkspaceOwner,
    shared: Arc<Shared>,
}
impl Attempt {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }
    pub(crate) fn local(&self) -> WorkspaceId {
        self.local
    }
    pub(crate) fn identity_topic(&self) -> u64 {
        self.identity_topic
    }
    pub(crate) fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
    }
    pub(crate) fn cancel(&self) -> Result<(), String> {
        // Serialize cancellation with the final synchronous installation barrier.
        let _turn = self.shared.result.lock_recover();
        self.shared.cancelled.store(true, Ordering::SeqCst);
        self.shared.cancel.send_replace(true);
        // Also retire a completed actor attempt: there may no longer be a task
        // watching cancellation. A still retains the exact uncertain proof.
        cancel_pending(&self.owner, self.local)
    }
    pub(crate) fn finish<T>(
        &self,
        install: impl FnOnce(VerifiedNativeEnrollment) -> Result<T, String>,
    ) -> Option<Result<T, String>> {
        let Ok(mut result) = self.shared.result.lock() else {
            return Some(Err("native enrollment attempt unavailable".into()));
        };
        result.as_ref()?;
        if self.shared.cancelled.load(Ordering::SeqCst) {
            let cleanup = cancel_pending(&self.owner, self.local);
            *result = None;
            return Some(cleanup.and(Err("native enrollment cancelled; proof retained".into())));
        }
        Some(result.take()?.and_then(install))
    }
}
fn cancel_pending(owner: &NativeWorkspaceOwner, local: WorkspaceId) -> Result<(), String> {
    if let Some(pending) = owner.pending_enrollment_for_slot(local)? {
        owner.cancel_enrollment(pending.id().room(), pending.id())?;
    }
    Ok(())
}
pub struct Command {
    attempt: Attempt,
    invitation: String,
    via: Via,
    receiver: watch::Receiver<bool>,
}

/// How an enrollment reaches the host that issued its invitation.
#[derive(Clone, Default)]
pub(crate) enum Via {
    /// The host's iroh endpoint, which the invitation names.
    #[default]
    Iroh,
    /// The host's HTTPS routes, for a host that publishes no iroh address,
    /// with what its admission door gave when the invitation came from one.
    Https {
        http: Arc<dyn tilo_relay_client::HttpClient>,
        door: Option<tilo_relay_session::workspace_join::DoorAdmission>,
    },
}
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeEnrollmentCommand")
    }
}
pub(crate) fn command(
    owner: NativeWorkspaceOwner,
    local: WorkspaceId,
    identity_topic: u64,
    invitation: String,
    via: Via,
) -> Result<(Command, Attempt), String> {
    if invitation.len() > 384 * 1024 {
        return Err("native invitation exceeds bound".into());
    }
    let id = NEXT
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "native enrollment attempt IDs exhausted")?;
    let (cancel, receiver) = watch::channel(false);
    let attempt = Attempt {
        id,
        local,
        identity_topic,
        owner,
        shared: Arc::new(Shared {
            result: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            cancel,
        }),
    };
    Ok((
        Command {
            attempt: attempt.clone(),
            invitation,
            via,
            receiver,
        },
        attempt,
    ))
}
/// Renews this device's custody, answering the custody that then presents
/// the renewal.
type Renewal = fn(HostIdentity) -> std::io::Result<HostIdentity>;

/// The session's renewal ([`crate::firstboot::renew_due_device_certificates`]):
/// the session's custody reads its certificates from disk, so it presents
/// what was renewed there.
fn renew_session_custody(identity: HostIdentity) -> std::io::Result<HostIdentity> {
    crate::firstboot::renew_due_device_certificates()?;
    Ok(identity)
}

/// The custody an enrollment presents for `topic`: when the certificate it
/// would present is due for renewal, renewed first through `renew`, so the
/// host sees the renewal and not the certificate it replaces.
async fn custody_to_present(identity: HostIdentity, topic: u64, renew: Renewal) -> HostIdentity {
    let due = identity
        .current_native_material(topic)
        .ok()
        .and_then(|(certificate, _)| tilo_identity_host::provisioning::renewal_due_at(&certificate))
        .is_some_and(|due| due <= tilo_clock::unix_now_ns());
    if !due {
        return identity;
    }
    let held = identity.clone();
    match tokio::task::spawn_blocking(move || renew(identity)).await {
        Ok(Ok(renewed)) => renewed,
        Ok(Err(error)) => {
            tracing::warn!(%error, "could not renew this device's certificate before enrolling");
            held
        }
        Err(error) => {
            tracing::warn!(%error, "could not renew this device's certificate before enrolling");
            held
        }
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Registry {
    /// Whether an enrollment is still talking to its host.
    pub(crate) fn is_busy(&self) -> bool {
        self.tasks.iter().any(|task| !task.is_finished())
    }

    pub(crate) fn handle(
        &mut self,
        command: Command,
        endpoint: iroh::Endpoint,
        identity: Option<&HostIdentity>,
    ) {
        self.tasks.retain(|task| !task.is_finished());
        let failure = if self.tasks.len() >= MAX_PENDING {
            Some("native enrollment task quota reached")
        } else if identity.is_none() {
            Some("native device custody unavailable")
        } else {
            None
        };
        if let Some(error) = failure {
            if let Ok(mut result) = command.attempt.shared.result.lock() {
                *result = Some(Err(error.into()));
            }
            return;
        }
        // `failure` above already answered a missing custody.
        let Some(identity) = identity.cloned() else {
            return;
        };
        self.tasks.push(tokio::spawn(async move {
            let Command { attempt, invitation, via, mut receiver } = command;
            let client = match via {
                Via::Iroh => tilo_relay_session::workspace_join::WorkspaceJoinClient::new(endpoint),
                Via::Https { http, door } => tilo_relay_session::workspace_join::WorkspaceJoinClient::https(http, door),
            };
            let identity = custody_to_present(identity, attempt.identity_topic, renew_session_custody).await;
            let result = if *receiver.borrow() { Err("native enrollment cancelled".into()) } else {
                tokio::select! {
                    biased;
                    _ = receiver.changed() => Err("native enrollment cancelled".into()),
                    value = tokio::time::timeout(Duration::from_secs(60), tilo_joined_room::enroll::prepare_and_complete(
                        client, identity, attempt.identity_topic,
                        Some(attempt.local), invitation, attempt.owner.clone())) => value.unwrap_or_else(|_| Err("native enrollment deadline elapsed; proof retained for exact retry".into())),
                }
            };
            // The exchange future has been dropped before cancellation touches the
            // store, so it cannot persist a new proof after this cleanup barrier.
            let result = if attempt.shared.cancelled.load(Ordering::SeqCst) {
                cancel_pending(&attempt.owner, attempt.local).and_then(|()| Err("native enrollment cancelled; proof retained".into()))
            } else { result };
            if let Ok(mut stored) = attempt.shared.result.lock() { *stored = Some(result); }
        }));
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    const SECOND: u64 = 1_000_000_000;

    fn custody(issued_at_ns: u64, expires_at_ns: u64) -> HostIdentity {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "device".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns,
            expires_at_ns: Some(expires_at_ns),
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        }
    }

    fn renewed(_: HostIdentity) -> std::io::Result<HostIdentity> {
        let now = tilo_clock::unix_now_ns();
        Ok(custody(now, now.saturating_add(600 * SECOND)))
    }

    fn never(_: HostIdentity) -> std::io::Result<HostIdentity> {
        panic!("a certificate not yet due is presented as it is")
    }

    #[tokio::test]
    async fn an_enrollment_renews_a_certificate_that_is_due_and_presents_the_renewal() {
        let now = tilo_clock::unix_now_ns();
        // Two thirds through its lifetime.
        let due = custody(
            now.saturating_sub(400 * SECOND),
            now.saturating_add(200 * SECOND),
        );
        let presented = custody_to_present(due.clone(), 0, renewed).await;
        let (certificate, _) = presented.current_native_material(0).unwrap();
        assert_ne!(certificate, due.device_cert);
        assert!(certificate.issued_at_ns >= now, "presents the renewal");
    }

    #[tokio::test]
    async fn an_enrollment_presents_a_certificate_not_yet_due_as_it_is() {
        let now = tilo_clock::unix_now_ns();
        let young = custody(
            now.saturating_sub(100 * SECOND),
            now.saturating_add(500 * SECOND),
        );
        let presented = custody_to_present(young.clone(), 0, never).await;
        assert_eq!(presented.device_cert, young.device_cert);
    }
}
