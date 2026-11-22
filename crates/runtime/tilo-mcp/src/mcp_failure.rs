//! Why an agent tool's call did not run, kept typed until its MCP answer is made.

use super::BridgeError;

/// Why an agent tool's call did not run, as its MCP answer carries it: what
/// the caller reads, and the host's typed refusal when one decided it, which
/// a typed answer names by its code.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) message: String,
    pub(crate) host: Option<tilo_actions::remote::ActionCallError>,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            host: None,
        }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

impl From<tilo_actions::remote::ActionCallError> for Failure {
    fn from(error: tilo_actions::remote::ActionCallError) -> Self {
        Self {
            message: error.to_string(),
            host: Some(error),
        }
    }
}

impl From<BridgeError> for Failure {
    fn from(error: BridgeError) -> Self {
        let host = match &error {
            BridgeError::Host(host) => Some(host.clone()),
            BridgeError::Bridge(_) => None,
        };
        Self {
            message: error.into(),
            host,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.message
    }
}

impl From<tilo_actions::tree::Unresolved> for Failure {
    fn from(unresolved: tilo_actions::tree::Unresolved) -> Self {
        String::from(unresolved).into()
    }
}
