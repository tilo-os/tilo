//! The arguments of a call that carry a secret: those its action's published descriptor declares
//! [`ParamType::Secret`](crate::ParamType::Secret), or every one of them where no descriptor of
//! the action is found. Whatever keeps a call (its asked record, the device's record of its
//! invocations) keeps their names alone; their values reach the provider in the call alone.

use std::sync::Arc;

use super::{InvokeArgument, InvokeRequest, RemoteActionTarget};

/// The parameters of the action a target names that take a secret, by name, as the descriptor
/// it is published with says: what the device's dispatch answers from its one registry of
/// actions, for whatever keeps a call of it. `None` where it finds no descriptor of the action
/// (one an app lists only as it is asked, one only another device answers): then every
/// argument is taken for a secret.
pub type SecretParameters = Arc<dyn Fn(&RemoteActionTarget) -> Option<Vec<String>> + Send + Sync>;

/// Whether the call `request` answers a secret, as its action's declaration says
/// ([`crate::SECRET_ANSWER_TAG`]): what whatever keeps the call keeps of its answer, which is
/// only that it was given.
pub type SecretAnswers = Arc<dyn Fn(&InvokeRequest) -> bool + Send + Sync>;

/// The name a call's single text argument ([`InvokeArgument::Text`]) is kept under where it is
/// left out: no parameter's name begins with `$` but what the system adds.
pub const TEXT_ARGUMENT: &str = "$text";

/// The parameters of `params` that take a secret, by name.
#[must_use]
pub fn secret_parameters(params: &[crate::ActionParam]) -> Vec<String> {
    params
        .iter()
        .filter(|param| param.ty == crate::ParamType::Secret)
        .map(|param| param.name.to_string())
        .collect()
}

/// The arguments of `request` no record of it keeps, by name: those `described` names, or,
/// where no descriptor of its action was found, every one it carries.
#[must_use]
pub fn secret_arguments(request: &InvokeRequest, described: Option<Vec<String>>) -> Vec<String> {
    if let Some(names) = described {
        return names;
    }
    match &request.argument {
        InvokeArgument::None => Vec::new(),
        InvokeArgument::Text(_) => vec![TEXT_ARGUMENT.to_owned()],
        // The platform's own fields (`$from`, a call's provenance) are no argument's data: a
        // call without a descriptor of its action keeps them, and only them.
        InvokeArgument::Json(arguments) => arguments
            .keys()
            .filter(|name| !name.starts_with('$'))
            .cloned()
            .collect(),
    }
}

/// `request` with the arguments `names` taken out ([`TEXT_ARGUMENT`] its text), beside those it
/// carried, by name: what a record of the call keeps, and what reaches the provider in the call
/// alone.
#[must_use]
pub fn without_secrets(
    request: &InvokeRequest,
    names: &[String],
) -> (InvokeRequest, serde_json::Map<String, serde_json::Value>) {
    let mut kept = request.clone();
    let mut taken = serde_json::Map::new();
    match &mut kept.argument {
        InvokeArgument::Json(arguments) => {
            for name in names {
                if let Some(value) = arguments.remove(name) {
                    taken.insert(name.clone(), value);
                }
            }
        }
        InvokeArgument::Text(text) if names.iter().any(|name| name == TEXT_ARGUMENT) => {
            taken.insert(TEXT_ARGUMENT.to_owned(), std::mem::take(text).into());
            kept.argument = InvokeArgument::None;
        }
        InvokeArgument::Text(_) | InvokeArgument::None => {}
    }
    (kept, taken)
}

/// Put `taken`, what [`without_secrets`] took out of a call, back into `request`.
pub fn restore(request: &mut InvokeRequest, mut taken: serde_json::Map<String, serde_json::Value>) {
    if let Some(serde_json::Value::String(text)) = taken.remove(TEXT_ARGUMENT) {
        request.argument = InvokeArgument::Text(text);
    }
    if taken.is_empty() {
        return;
    }
    match &mut request.argument {
        InvokeArgument::Json(arguments) => arguments.extend(taken),
        argument => *argument = InvokeArgument::Json(taken),
    }
}
