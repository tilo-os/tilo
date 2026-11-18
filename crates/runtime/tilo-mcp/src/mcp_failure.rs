//! Why an agent tool's call did not run, kept typed until its MCP answer is made.

use super::BridgeError;
use super::agent::instead::{Call, said};

/// Why an agent tool's call did not run, as its MCP answer carries it: what
/// the caller reads, and the host's typed refusal when one decided it, which
/// a typed answer names by its code. `instead` holds the calls that work
/// instead, which what the caller reads ends with.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) message: String,
    pub(crate) host: Option<tilo_actions::remote::ActionCallError>,
    pub(crate) instead: Vec<Call>,
}

impl Failure {
    /// `self`, ending with `calls`: the calls that work instead.
    pub(crate) fn ending_with(mut self, calls: Vec<Call>) -> Self {
        self.instead = calls;
        self
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            host: None,
            instead: Vec::new(),
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
            instead: Vec::new(),
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
            instead: Vec::new(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        f.write_str(&said(&self.instead))
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        let mut text = failure.message;
        text.push_str(&said(&failure.instead));
        text
    }
}
