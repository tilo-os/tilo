//! Trusted native inference boundary. The application receives an identity, not
//! a credential, endpoint capability, permit, or deferred permission boolean.
use std::sync::Arc;
use std::time::SystemTime;
use tilo_joined_room::{NativeCommittedView, NativeWorkspaceOwner};
use tilo_model_gateway::{FrozenNativeAttempt, NativeModelDispatch};

pub use crate::services::NativeTaskBinding;
pub use tilo_joined_room::task_admission::model_host::NativeModelConnection;

/// Called once after the lease is fenced. Must not synchronously reenter an
/// owner mutation that triggered revocation; enqueue that work instead.
pub type NativeModelCloseCallback = Box<dyn FnOnce() + Send + 'static>;
pub use tilo_joined_room::task_admission::model::{
    NativeModelContext, NativeModelContract, NativeModelPlanning,
};

/// Implemented by the broker from a certificate-authenticated, delivered lease.
/// Never constructed from inference JSON or a sender's display name/profile.
pub trait NativeModelLease: Send + Sync {
    /// Hold the actual delivered lease and protected connection gates through
    /// synchronous enqueue. Unsupported adapters fail closed, never cache `live()`.
    ///
    /// # Errors
    /// Refuses an ended lease or unsupported pinned dispatch.
    fn with_current(
        &self,
        _view: &NativeCommittedView,
        _contract: NativeModelContract,
        _now: SystemTime,
        _enqueue: &mut dyn FnMut(NativeModelContext) -> Result<(), String>,
    ) -> Result<(), String> {
        Err("native MODEL pinned lease unavailable".into())
    }

    fn room(&self) -> &str;
    fn binding(&self) -> &NativeTaskBinding;
    fn live(&self) -> bool;
    /// Host-random process-session and delivered-lease identities. Unsupported
    /// implementations cannot enroll native MODEL authority.
    fn connection(&self) -> Option<NativeModelConnection> {
        None
    }
    /// Register before enrollment. A closed lease invokes the callback before
    /// returning; callbacks never run under the broker's registry locks.
    ///
    /// # Errors
    /// Refuses unsupported close notifications or an unavailable listener gate.
    fn on_close(&self, callback: NativeModelCloseCallback) -> Result<(), String> {
        let _ = callback;
        Err("native MODEL close notifications unavailable".into())
    }
    /// Called inside the owner's mutation turn; must not reenter the owner.
    ///
    /// # Errors
    /// Refuses an ended lease, changed membership or invalid task custody.
    fn current(
        &self,
        view: &NativeCommittedView,
        contract: NativeModelContract,
        now: SystemTime,
    ) -> Result<NativeModelContext, String>;
}

/// Installed by the native host. This is not an application RPC interface.
pub trait NativeModelHost: Send + Sync {
    /// # Errors
    /// Refuses unavailable host custody or a stale delivered task lease.
    fn claim(
        &self,
        lease: Arc<dyn NativeModelLease>,
        invocation: uuid::Uuid,
    ) -> Result<Arc<dyn NativeModelDispatch>, String>;
}

/// Build the context from the actual owner view, after connection/lease checks.
/// Kept here so no caller can replace Model planning with a wire boolean.
///
/// # Errors
/// Refuses missing task custody, a changed run/frontier or unavailable private text.
pub fn current_context(
    view: &NativeCommittedView,
    binding: &NativeTaskBinding,
    contract: NativeModelContract,
) -> Result<NativeModelContext, String> {
    let snapshot = tilo_peer_state::workspace_task::snapshot(
        view.peer()
            .admitted_room()
            .ok_or("native MODEL room unavailable")?,
    )?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == binding.task_id)
        .ok_or("native MODEL task absent")?;
    let control = snapshot
        .controls
        .get(&binding.task_id)
        .ok_or("native MODEL task control absent")?;
    let run = control
        .active_run
        .as_ref()
        .ok_or("native MODEL run ended")?;
    let revision = control
        .directions
        .last()
        .map_or(0, |direction| direction.revision);
    // MODEL prepares the latest desired plan before DirectionsApplied. Never
    // upgrade an older delivered lease to that frontier implicitly.
    let epoch = binding.epoch;
    if run.epoch != epoch
        || control.run_epoch != epoch
        || run.agent_id != binding.agent_id
        || binding.revision != revision
        || run
            .approval
            .as_ref()
            .is_none_or(|approval| binding.starter.as_deref() != Some(approval.person.as_str()))
    {
        return Err("native MODEL delivered binding changed".into());
    }
    let planning = match task.route {
        tilo_desktop_model::WorkspaceTaskRoute::Manual => NativeModelPlanning::Manual,
        tilo_desktop_model::WorkspaceTaskRoute::RulesOnly => NativeModelPlanning::RulesOnly,
        tilo_desktop_model::WorkspaceTaskRoute::Model => NativeModelPlanning::Model,
    };
    Ok(NativeModelContext {
        actor: run.agent_id.clone(),
        task: task.id.clone(),
        epoch: run.epoch,
        revision,
        planning,
        // Remote replicas do not carry owner-private text. The physical host
        // adapter supplies the digest fetched through its authenticated private
        // read before entering this synchronous owner turn. Zero alone never
        // satisfies a typed requirement or host enrollment.
        private_run_digest: if view.device() == view.governor() {
            NativeWorkspaceOwner::current_private_run_digest(view, binding)?
        } else {
            [0; 32]
        },
        contract,
    })
}

/// The gateway already shaped these bytes and opened its transport. Both calls
/// compare the same complete, nonsecret selection, never just a profile id.
///
/// # Errors
/// Refuses a disabled or changed provider, account, model or request option.
pub fn attempt_matches(
    attempt: &FrozenNativeAttempt,
    profile: &agent_provider_config::ProviderProfile,
    model: &str,
) -> Result<(), String> {
    if &attempt.profile != profile || attempt.model != model || !profile.enabled {
        return Err("native MODEL selected provider/account/options changed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_profile_and_model_not_just_provider_id_must_match() {
        let mut profile = agent_provider_config::ProviderProfile {
            enabled: true,
            ..Default::default()
        };
        let mut attempt = FrozenNativeAttempt {
            profile: profile.clone(),
            model: "selected".into(),
            endpoint: "http://localhost:8000/v1/chat/completions".into(),
            body: br#"{"model":"selected"}"#.to_vec(),
        };
        assert!(attempt_matches(&attempt, &profile, "selected").is_ok());
        attempt.profile.endpoint = "http://localhost:9000/v1".into();
        assert!(attempt_matches(&attempt, &profile, "selected").is_err());
        attempt.profile = profile.clone();
        attempt.profile.omit_params.push("temperature".into());
        assert!(attempt_matches(&attempt, &profile, "selected").is_err());
        attempt.profile = profile.clone();
        assert!(attempt_matches(&attempt, &profile, "different-model").is_err());
        profile.enabled = false;
        attempt.profile = profile.clone();
        assert!(attempt_matches(&attempt, &profile, "selected").is_err());
    }
}
