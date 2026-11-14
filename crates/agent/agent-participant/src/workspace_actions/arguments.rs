//! The arguments an invocation carries, as the door binds them.

use tilo_actions::PROMPTED_PARAM;
use tilo_actions::remote::InvokeArgument;

/// The arguments an invocation carries, as the door binds them.
pub(crate) fn invoke_arguments(
    argument: &InvokeArgument,
) -> serde_json::Map<String, serde_json::Value> {
    match argument {
        InvokeArgument::None => serde_json::Map::new(),
        InvokeArgument::Text(text) => {
            let mut arguments = serde_json::Map::new();
            arguments.insert(PROMPTED_PARAM.into(), text.clone().into());
            arguments
        }
        InvokeArgument::Json(arguments) => arguments.clone(),
    }
}
