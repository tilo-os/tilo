//! What a call's request names, read as its action's value, and what its caller is answered,
//! with no secret it carried ever quoted in a refusal.

use serde_json::Value;
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCert;

/// Turn one action's answer into what the caller reads, or the reason it was
/// refused.
pub fn answer<T: serde::Serialize>(outcome: &Result<T, String>) -> Result<Value, ActionCallError> {
    match outcome {
        Ok(value) => serde_json::to_value(value).map_err(|error| ActionCallError::Refused {
            reason: error.to_string(),
        }),
        Err(reason) => Err(ActionCallError::Refused {
            reason: reason.clone(),
        }),
    }
}

/// The arguments of a request, as the value its action names.
pub fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(ActionCallError::invalid_request(format!(
            "a request names {what}"
        )));
    };
    serde_json::from_value(Value::Object(argument.clone())).map_err(|error| {
        ActionCallError::invalid_request(format!(
            "{what} is not readable: {}",
            without_values(&error)
        ))
    })
}

/// `error` said without the values it quotes, which may be a password: serde
/// names a field or a variant between backticks, and quotes a value it could
/// not read (`invalid type: string "…", expected u16`).
#[must_use]
pub fn without_values(error: &serde_json::Error) -> String {
    let said = error.to_string();
    let unquoted: Vec<&str> = said.split('"').step_by(2).collect();
    unquoted.join("").replace(" ,", ",")
}

/// The person `caller` asks for, on the device `certificate` certifies,
/// from a workspace `workspace_id` names: what a request that keeps a
/// person's own secrets needs before it runs. Unknown and forbidden are
/// the same answer.
pub fn asking_person(
    caller: &Caller,
    certificate: Option<&DeviceCert>,
    workspace_id: Option<collab_types::WorkspaceId>,
) -> Result<[u8; 32], ActionCallError> {
    workspace_id.ok_or(ActionCallError::UnknownAction)?;
    if !caller.acting().is_person() || caller.acting().app().is_some() {
        return Err(ActionCallError::UnknownAction);
    }
    let certificate = certificate
        .filter(|certificate| {
            caller.device_public_key() == Some(&certificate.device_public_key)
                && caller.user_public_key() == Some(&certificate.user_public_key)
        })
        .ok_or(ActionCallError::UnknownAction)?;
    Ok(certificate.user_public_key)
}
