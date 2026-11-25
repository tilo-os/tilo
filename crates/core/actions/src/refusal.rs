//! Why an action declined to run, as the verb that refused it says.

use crate::ActionError;

/// Why an action declined to run, in the user's words, and, where a caller
/// acts on the reason rather than reads it, a stable code naming it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    code: Option<String>,
    reason: String,
}

impl Refusal {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            code: None,
            reason: reason.into(),
        }
    }

    /// A refusal a caller tells apart by `code` (a thing the action would add
    /// is there already, say), with `reason` in words beside it for whoever
    /// reads.
    pub fn coded(code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            code: Some(code.into()),
            reason: reason.into(),
        }
    }

    /// The code the app gave, for a caller that decides on it.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Why, in the user's words.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Why, in the user's words, owned.
    #[must_use]
    pub fn into_reason(self) -> String {
        self.reason
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl From<&str> for Refusal {
    fn from(reason: &str) -> Self {
        Self::new(reason)
    }
}

impl From<String> for Refusal {
    fn from(reason: String) -> Self {
        Self::new(reason)
    }
}

/// A verb's refusal as the action's: one a caller decides on keeps its code.
impl From<Refusal> for ActionError {
    fn from(refusal: Refusal) -> Self {
        match refusal.code {
            Some(code) => Self::Unavailable {
                code,
                message: refusal.reason,
            },
            None => Self::Refused(refusal.reason),
        }
    }
}

/// An action's refusal as a verb's, for a reader above the action that hands
/// it on: one a caller decides on keeps its code.
impl From<ActionError> for Refusal {
    fn from(error: ActionError) -> Self {
        match error {
            ActionError::Unavailable { code, message } => Self::coded(code, message),
            other => Self::new(other.to_string()),
        }
    }
}
