//! What a session service publishes.
//!
//! The desktop services the session starts (the portal backend, the
//! notification daemon, the polkit agent, the network secret agent) have no
//! UI of their own. Each publishes resources and the actions on them
//! ([`Service`]) through the session, as any app publishes its own, so a
//! person, an agent and a headless test reach them the same way. What
//! needs a person's answer goes out as a question ([`Questions`]): a
//! resource the session's sheet presents and answers through its actions.
//!
//! A service's actions are effects ([`effect`]) whose arguments travel as
//! one JSON object; the session resolves an address against
//! [`Service::resources`], binds the arguments against the action's
//! declared parameters and hands the result to [`Service::run`]
//! ([`invoke`]).

use std::sync::Arc;

use serde::de::DeserializeOwned;
use tilo_actions::{
    ActionAddress, ActionOutput, ActionParam, ActionSpec, DoorError, EncodedAction, OperationKind,
    ResourceSpec,
};

mod questions;

pub use questions::{
    Answer, Asked, QUESTION_KIND, QUESTIONS_KIND, QUESTIONS_RESOURCE, Question, Questions,
    question_resource,
};

/// A service the session starts: what it publishes now and how it carries
/// out an action on it.
pub trait Service: Send + Sync {
    /// Everything the service publishes now, with its actions.
    fn resources(&self) -> Vec<ResourceSpec>;

    /// Carry out `action`, published on resource `resource` and built by
    /// its spec from arguments the session already bound against it.
    ///
    /// # Errors
    /// The action's refusal or failure.
    fn run(&self, resource: &str, action: &EncodedAction) -> Result<ActionOutput, DoorError>;
}

/// An effect a service publishes: `params` bound strictly, and carried to
/// [`Service::run`] as one JSON object under `type_id`.
#[must_use]
pub fn effect(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &str,
    params: Vec<ActionParam>,
) -> ActionSpec {
    let encoded_as = type_id.to_owned();
    ActionSpec::typed_operation(
        id,
        label,
        "",
        OperationKind::Effect,
        type_id,
        params,
        move |args| EncodedAction {
            type_id: encoded_as.clone(),
            payload: serde_json::to_vec(&args.to_json()).unwrap_or_default(),
        },
    )
}

/// An effect only the person at this device runs, as they act on it here
/// (their own sheet, their own notifications).
#[must_use]
pub fn own_device(mut spec: ActionSpec) -> ActionSpec {
    spec.type_mut()
        .contract
        .tags
        .push(tilo_actions::OWN_DEVICE_SCOPE.to_owned());
    spec
}

/// An effect that needs its person attending: whoever acts for them may
/// decline it but is never let to give it ([`tilo_actions::ATTENDED_TAG`]).
#[must_use]
pub fn attended(mut spec: ActionSpec) -> ActionSpec {
    spec.type_mut()
        .contract
        .tags
        .push(tilo_actions::ATTENDED_TAG.to_owned());
    spec
}

/// The arguments of an effect built by [`effect`].
///
/// # Errors
/// [`DoorError::InvalidArguments`] when they do not read as `T`.
pub fn arguments<T: DeserializeOwned>(action: &EncodedAction) -> Result<T, DoorError> {
    serde_json::from_slice(&action.payload)
        .map_err(|error| DoorError::InvalidArguments(error.to_string()))
}

/// Invoke action `action` of resource `resource` on whichever of
/// `services` publishes it; `None` when none does.
#[must_use]
pub fn invoke(
    services: &[Arc<dyn Service>],
    resource: &str,
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Option<Result<ActionOutput, DoorError>> {
    let address = ActionAddress::new(resource, action);
    services.iter().find_map(|service| {
        let published = service.resources();
        let spec = tilo_actions::door::resolve(&published, &address)?;
        Some(
            spec.build_json(arguments)
                .map_err(DoorError::from)
                .and_then(|built| service.run(resource, &built)),
        )
    })
}

#[cfg(test)]
mod tests;
