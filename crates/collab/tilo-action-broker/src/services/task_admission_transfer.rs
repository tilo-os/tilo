//! One-use handoff while a blocking owner worker retains its admission turn.
use std::{future::Future, pin::Pin, sync::mpsc};
use tokio::sync::oneshot;

/// Original authenticated task caller, captured only by the broker from its
/// admitted connection. This is evidence to revalidate, never a new grant.
#[derive(Clone)]
pub struct NativeTaskAdmissionCaller {
    pub(crate) certificate: tilo_user_identity::DeviceCert,
    pub(crate) agent_id: String,
    pub(crate) room: String,
    pub(crate) capped: bool,
}

impl NativeTaskAdmissionCaller {
    /// Revalidate the original caller against the exact guarded owner view.
    /// No owner read or broker callback is allowed from this check.
    ///
    /// # Errors
    /// The certificate, membership, delegation or exact agent binding changed.
    pub fn check(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        requirement: &super::NativeTaskActionRequirement,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        let binding = requirement
            .binding
            .as_ref()
            .ok_or("native task caller binding absent")?;
        if self.room != view.room().to_string() || self.agent_id != binding.agent_id {
            return Err("native task original caller binding changed".into());
        }
        let ns = u64::try_from(
            now.duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "native task caller clock invalid")?
                .as_nanos(),
        )
        .map_err(|_| "native task caller clock overflow")?;
        tilo_user_identity::verify_device_cert_at(&self.certificate, ns)
            .map_err(|error| error.to_string())?;
        let peer = view.peer();
        if !peer.certificates().contains(&self.certificate)
            || self.certificate.capabilities.can_act_as_host
        {
            return Err("native task original caller certificate no longer admitted".into());
        }
        let mut member = peer.permissions(self.certificate.device_public_key, now)?;
        // Match the broker's room-specific device ceiling: a foreign
        // certificate is uncapped on the device governing this room. Keep
        // the captured ceiling too, so a previously capped connection cannot
        // gain authority through this handoff.
        let currently_capped = self.certificate.capabilities.foreign
            && self.certificate.device_public_key != view.governor().device_public_key;
        if self.capped || currently_capped {
            return Err("native task original caller delegation is capped".into());
        }
        let person =
            crate::collab_broker_capnp::principal_id_for_root(self.certificate.user_public_key);
        if member.person != person {
            return Err("native task original caller person changed".into());
        }
        // Evaluate the actual delegation bound, rather than rejecting its
        // presence. This is the same administrative authority required at
        // channel admission, checked on the caller rather than the starter.
        // Channel delegation uses the room-specific foreign ceiling, rather
        // than treating a locally governing foreign certificate as capped.
        member.foreign = currently_capped;
        // The agent's device delegation is not an administrative operation.
        // Like channel admission, validate its person's current authority to
        // launch the agent; exact task/action scope is checked separately.
        member.delegate = None;
        if !member.allows(
            &tilo_action_policy::roles::governance_action(
                &self.room,
                tilo_action_policy::roles::governance::GRANT,
            ),
            tilo_action_policy::roles::GOVERNANCE_KIND,
            now,
        ) {
            return Err("native task caller no longer has administration authority".into());
        }
        peer.workspace()
            .check_task_execution_actor(&self.agent_id, None, now)
    }
}

pub type NativeTaskAdmissionFuture =
    Pin<Box<dyn Future<Output = Result<NativeTaskAdmissionReady, String>> + Send>>;

/// Permission to execute the original local enqueue closure exactly once.
///
/// The owner worker still holds its serialized admission guard. Dropping this
/// value cancels the callback; it never makes consumed custody replayable.
#[must_use = "dropping admission readiness cancels the owner callback"]
pub struct NativeTaskAdmissionReady {
    completion: mpsc::SyncSender<Result<(), String>>,
    finished: oneshot::Receiver<Result<(), String>>,
}

impl NativeTaskAdmissionReady {
    /// Complete the local enqueue, then await the owner's final result. Keep the
    /// actual job on the LocalSet; only its success or error crosses this door.
    ///
    /// # Errors
    /// The enqueue failed or the owner worker ended without confirming completion.
    pub async fn complete(self, result: Result<(), String>) -> Result<(), String> {
        self.completion
            .try_send(result)
            .map_err(|_| "native task admission owner ended; outcome unknown".to_owned())?;
        self.finished
            .await
            .map_err(|_| "native task admission owner interrupted; outcome unknown".to_owned())?
    }
}

/// Worker half of the transfer. Only `ready_and_wait` blocks, and must run on
/// the blocking worker *inside* the owner's guarded admission callback.
pub struct NativeTaskAdmissionTransfer {
    ready: Option<oneshot::Sender<Result<NativeTaskAdmissionReady, String>>>,
    finished: Option<oneshot::Sender<Result<(), String>>>,
    result: Option<oneshot::Receiver<Result<(), String>>>,
}

impl NativeTaskAdmissionTransfer {
    #[must_use]
    pub fn channel() -> (Self, NativeTaskAdmissionFuture) {
        let (ready, receive) = oneshot::channel();
        let (finished, result) = oneshot::channel();
        (
            Self {
                ready: Some(ready),
                finished: Some(finished),
                result: Some(result),
            },
            Box::pin(async move {
                receive.await.map_err(|_| {
                    "native task admission owner interrupted; outcome unknown".to_owned()
                })?
            }),
        )
    }

    /// Hand off readiness exactly once and retain the owner's guard until the
    /// local closure completes or drops its completion sender.
    ///
    /// # Errors
    /// The local caller cancelled, enqueue failed, or readiness was already used.
    pub fn ready_and_wait(&mut self) -> Result<(), String> {
        let sender = self
            .ready
            .take()
            .ok_or("native task readiness already used")?;
        let finished = self
            .result
            .take()
            .ok_or("native task readiness already used")?;
        let (completion, receive) = mpsc::sync_channel(1);
        sender
            .send(Ok(NativeTaskAdmissionReady {
                completion,
                finished,
            }))
            .map_err(|_| "native task admission cancelled".to_owned())?;
        receive
            .recv()
            .map_err(|_| "native task admission cancelled; outcome unknown".to_owned())?
    }

    /// Publish the result of the existing owner admission method, including
    /// failures before readiness. This cannot mint another Ready value.
    pub fn finish(mut self, result: Result<(), String>) {
        if let Some(sender) = self.ready.take() {
            let error = result
                .err()
                .unwrap_or_else(|| "native task admission ended without readiness".to_owned());
            let _ = sender.send(Err(error));
        } else if let Some(sender) = self.finished.take() {
            let _ = sender.send(result);
        }
    }
}

#[cfg(test)]
#[path = "task_admission_transfer_tests.rs"]
mod tests;
