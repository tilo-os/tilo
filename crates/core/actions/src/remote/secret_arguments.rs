//! The arguments of a call that carry a secret: those its action's published descriptor declares
//! [`ParamType::Secret`](crate::ParamType::Secret). Whatever keeps a call (its asked record, the
//! device's record of its invocations) keeps none of their values; they reach the provider in the
//! call alone.

use std::sync::Arc;

use super::{InvokeArgument, InvokeRequest, RemoteActionTarget};

/// The parameters of the action a target names that take a secret, by name, as the descriptor
/// it is published with says: what the device's dispatch answers from its one registry of
/// actions, for whatever keeps a call of it.
pub type SecretParameters = Arc<dyn Fn(&RemoteActionTarget) -> Vec<String> + Send + Sync>;

/// The parameters of `params` that take a secret, by name.
#[must_use]
pub fn secret_parameters(params: &[crate::ActionParam]) -> Vec<String> {
    params
        .iter()
        .filter(|param| param.ty == crate::ParamType::Secret)
        .map(|param| param.name.to_string())
        .collect()
}

/// `request` with the arguments `names` taken out, beside those it carried, by name: what a
/// record of the call keeps, and what reaches the provider in the call alone.
#[must_use]
pub fn without_secrets(
    request: &InvokeRequest,
    names: &[String],
) -> (InvokeRequest, serde_json::Map<String, serde_json::Value>) {
    let mut kept = request.clone();
    let mut taken = serde_json::Map::new();
    if let InvokeArgument::Json(arguments) = &mut kept.argument {
        for name in names {
            if let Some(value) = arguments.remove(name) {
                taken.insert(name.clone(), value);
            }
        }
    }
    (kept, taken)
}
