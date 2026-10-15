//! Where an admitted offloadable invocation runs.
//!
//! An action whose contract declares [`Offload`] is a pure transform of
//! content: its inputs are the content its invocation names, its outputs are
//! content, and they depend on the inputs alone. The action door admits such
//! an invocation exactly as any other — who may invoke never changes — and
//! then asks the device's [`ActionOffload`] whether another of the person's
//! devices should run it. When one did, the door answers the invocation with
//! that output ([`Offloaded::Ran`]); otherwise, for any reason at all, the
//! app's own handler runs it here ([`Offloaded::RunHere`]), so a failed or
//! unreachable worker only ever costs time, never a different answer.
//!
//! The inputs a job may read are only the content the invocation's own
//! content parameters name ([`content_inputs`]), as the invoker's room
//! reaches it; never a path, an environment or an argument vector.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::{
    ARGUMENTS_JSON_PREFIX, ActionAddress, ActionOutput, ActionParam, Offload, content_hash,
};

/// One admitted invocation of an offloadable action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffloadRequest {
    /// The room (workspace) it was invoked in.
    pub room: String,
    /// The app that publishes the action.
    pub app_id: String,
    /// The invoked action.
    pub address: ActionAddress,
    /// The contract's declaration: package, entrypoint and bounds.
    pub offload: Offload,
    /// Its inputs: the content hashes its arguments name, in order.
    pub inputs: Vec<String>,
}

/// What became of an offload request.
#[derive(Clone, Debug, PartialEq)]
pub enum Offloaded {
    /// Another device ran it; this is what a local run returns.
    Ran {
        /// The action's output.
        output: ActionOutput,
        /// The device that ran it, as the offloader names devices.
        device: String,
        /// Wall time from the request to the output.
        elapsed: Duration,
    },
    /// Run it here, through the app's handler, and why.
    RunHere {
        /// Why it was not offloaded: no better-placed device, or what
        /// failed remotely.
        reason: String,
    },
}

/// A boxed, sendable future, as [`ActionOffload`] answers.
pub type OffloadFuture<'a> = Pin<Box<dyn Future<Output = Offloaded> + Send + 'a>>;

/// The device's route for offloadable invocations.
pub trait ActionOffload: Send + Sync {
    /// Run `request` on the best-placed other device, or say why it should
    /// run here.
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_>;

    /// The app's handler ran `request` here in `elapsed`: what placement
    /// weighs moving its inputs against next time.
    fn ran_here(&self, request: &OffloadRequest, elapsed: Duration) {
        let _ = (request, elapsed);
    }
}

/// The content an invocation's `arguments` name through the content
/// parameters among `params` ([`ActionParam::is_content`],
/// [`ActionParam::is_content_list`]), in declaration order: a bare hash or a
/// `tilo.content:` address each.
///
/// # Errors
///
/// Names the parameter whose argument is not content.
pub fn content_inputs(
    params: &[ActionParam],
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<String>, String> {
    let hash = |name: &str, value: &serde_json::Value| -> Result<String, String> {
        let text = value
            .as_str()
            .ok_or_else(|| format!("`{name}` is not a content address"))?;
        let hash = content_hash(text).unwrap_or(text);
        let valid = hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        valid
            .then(|| hash.to_owned())
            .ok_or_else(|| format!("`{name}` is not a content address"))
    };
    let mut inputs = Vec::new();
    for param in params {
        let Some(value) = arguments
            .get(param.name.as_ref())
            .filter(|value| !value.is_null())
        else {
            continue;
        };
        if param.is_content() {
            inputs.push(hash(&param.name, value)?);
        } else if param.is_content_list() {
            let items = value
                .as_array()
                .ok_or_else(|| format!("`{}` is not a list of content", param.name))?;
            for item in items {
                inputs.push(hash(&param.name, item)?);
            }
        }
    }
    Ok(inputs)
}

/// [`content_inputs`] of an invocation's wire `argument`: its named
/// arguments ([`ARGUMENTS_JSON_PREFIX`]), or none.
///
/// # Errors
///
/// When the argument is not named arguments, or names something that is not
/// content.
pub fn argument_inputs(
    params: &[ActionParam],
    argument: Option<&str>,
) -> Result<Vec<String>, String> {
    let Some(argument) = argument else {
        return Ok(Vec::new());
    };
    let json = argument
        .strip_prefix(ARGUMENTS_JSON_PREFIX)
        .ok_or("the invocation's arguments are not named")?;
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).map_err(|error| format!("unreadable arguments: {error}"))?;
    content_inputs(params, &arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParamType;

    #[test]
    fn the_inputs_are_the_content_the_content_parameters_name_in_order() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let params = vec![
            ActionParam::required("images", ParamType::Array, "Images").as_content_list(),
            ActionParam::optional("label", ParamType::Text, "A label"),
            ActionParam::optional("cover", ParamType::Text, "Cover").as_content(),
        ];
        let arguments = serde_json::json!({
            "images": [a, format!("tilo.content:{b}")],
            "label": "not content",
            "cover": a,
        });
        let arguments = arguments.as_object().unwrap();
        assert_eq!(
            content_inputs(&params, arguments),
            Ok(vec![a.clone(), b, a])
        );
        assert!(params[0].is_content_list() && !params[0].is_content());
        assert!(params[2].is_content() && !params[2].is_content_list());

        // On the wire, as named arguments.
        let wire = format!(
            "{ARGUMENTS_JSON_PREFIX}{}",
            serde_json::to_string(arguments).unwrap()
        );
        assert_eq!(
            argument_inputs(&params, Some(&wire)),
            content_inputs(&params, arguments)
        );
        assert_eq!(argument_inputs(&params, None), Ok(Vec::new()));
        assert!(argument_inputs(&params, Some("free text")).is_err());

        // A path is never content, nor is a hash spelled otherwise.
        for wrong in ["/home/someone/secret.png", &"B".repeat(64)] {
            let arguments = serde_json::json!({ "images": [wrong] });
            assert!(content_inputs(&params, arguments.as_object().unwrap()).is_err());
        }
    }
}
