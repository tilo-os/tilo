//! Sealed original native task invocation, never a deserializable authority claim.
use crate::collab_broker_capnp::CapnpBrokerState;
use std::sync::Arc;

/// Host-only request retained from the actual published invocation and admission.
/// Applications cannot construct this context or replace its caller, task or policy guard.
#[derive(Clone)]
pub struct ManagedTaskLaunchContext {
    pub(crate) room: String,
    pub(crate) task: String,
    pub(crate) invocation: uuid::Uuid,
    pub(crate) certificate: tilo_user_identity::DeviceCert,
    pub(crate) recheck: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub(crate) broker: std::sync::Weak<CapnpBrokerState>,
}
impl ManagedTaskLaunchContext {
    #[must_use]
    pub fn room(&self) -> &str {
        &self.room
    }
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }
    #[must_use]
    pub fn invocation(&self) -> uuid::Uuid {
        self.invocation
    }
    #[must_use]
    pub fn certificate(&self) -> &tilo_user_identity::DeviceCert {
        &self.certificate
    }
    /// Revalidate the original person, exact task/action and durable invocation session.
    /// # Errors
    /// Refuses changed, canceled or retired original custody and permission.
    pub fn recheck(&self) -> Result<(), String> {
        (self.recheck)()
    }
    /// Retrieve this exact invocation's live broker, never an app-provided endpoint.
    /// # Errors
    /// Refuses a stopped or destroyed broker.
    pub fn broker(&self) -> Result<Arc<CapnpBrokerState>, String> {
        let broker = self
            .broker
            .upgrade()
            .ok_or("native launch broker retired")?;
        if !broker.is_accepting() {
            return Err("native launch broker stopped".into());
        }
        Ok(broker)
    }
    /// Whether the original host broker still admits work; used for owned-process cleanup.
    #[must_use]
    pub fn host_live(&self) -> bool {
        self.broker
            .upgrade()
            .is_some_and(|broker| broker.is_accepting())
    }
}
pub type ManagedTaskLaunchFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;
/// Trusted native launcher installed by the host before any collaborator action admission.
pub trait ManagedTaskLaunches: Send + Sync {
    /// The trusted installation is available for this canonical native room.
    fn available(&self, room: &str) -> bool;
    /// Launch the selected queued task with its original authenticated person custody.
    fn launch(&self, context: ManagedTaskLaunchContext) -> ManagedTaskLaunchFuture;
}
