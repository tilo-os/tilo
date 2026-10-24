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
/// Told an attempt's id once its result is stored, outside the result's lock.
pub(crate) type Landed = Arc<dyn Fn(u64) + Send + Sync>;
struct Shared {
    result: Mutex<Option<Result<VerifiedNativeEnrollment, String>>>,
    cancelled: AtomicBool,
    cancel: watch::Sender<bool>,
    landed: Landed,
}
impl Shared {
    /// Store how the attempt `id` ended, then say it landed.
    fn land(&self, id: u64, result: Result<VerifiedNativeEnrollment, String>) {
        if let Ok(mut stored) = self.result.lock() {
            *stored = Some(result);
        }
        (self.landed)(id);
    }
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
    /// Whether the attempt's result is stored and not yet finished.
    pub(crate) fn has_landed(&self) -> bool {
        self.shared.result.lock_recover().is_some()
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
    landed: Landed,
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
            landed,
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
#[derive(Default)]
pub(crate) struct Registry {
    /// Each enrollment's task, each with a host on another machine: one
    /// with this device's own host runs in place
    /// (`ShellNetwork::enroll_in_process`).
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Registry {
    /// Whether an enrollment is still talking to a host on another machine.
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
            command
                .attempt
                .shared
                .land(command.attempt.id, Err(error.into()));
            return;
        }
        // `failure` above already answered a missing custody.
        let Some(identity) = identity.cloned() else {
            return;
        };
        let task = tokio::spawn(async move {
            let Command {
                attempt,
                invitation,
                via,
                mut receiver,
            } = command;
            let client = match via {
                Via::Iroh => tilo_relay_session::workspace_join::WorkspaceJoinClient::new(endpoint),
                Via::Https { http, door } => {
                    tilo_relay_session::workspace_join::WorkspaceJoinClient::https(http, door)
                }
            };
            // A certificate due for renewal is renewed before the host sees it.
            if let Ok(Err(error)) =
                tokio::task::spawn_blocking(crate::firstboot::renew_due_device_certificates).await
            {
                tracing::warn!(%error, "could not renew this device's certificate before enrolling");
            }
            let result = if *receiver.borrow() {
                Err("native enrollment cancelled".into())
            } else {
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
                cancel_pending(&attempt.owner, attempt.local)
                    .and_then(|()| Err("native enrollment cancelled; proof retained".into()))
            } else {
                result
            };
            attempt.shared.land(attempt.id, result);
        });
        self.tasks.push(task);
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
