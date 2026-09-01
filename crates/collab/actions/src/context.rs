//! Human context carried by an action into durable history.
//!
//! The action payload says how to reproduce a mutation. [`ActionContext`]
//! says how a person should find and understand it later. It deliberately
//! contains no generated prose: apps provide the stable action/resource
//! labels they already expose through the action door, while a person or
//! agent may add an explicit purpose and work-group label.

use serde::{Deserialize, Serialize};

/// Prefix used when context is embedded in an Automerge change message.
/// Ordinary application-authored commit messages remain untouched.
pub const CHANGE_MESSAGE_PREFIX: &str = "qos.action/1 ";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionContext {
    /// Human action label, e.g. "Archive" or "Rename".
    pub action_label: String,
    /// Publishing app, when the host knows it.
    pub app: Option<ActionApp>,
    /// The resource the action addressed.
    pub resource: Option<ActionResource>,
    /// What the actor says they are trying to accomplish. Never inferred.
    pub purpose: Option<String>,
    /// Stable cross-action work unit: issue, agent run, task, or session.
    pub group: Option<ActionGroup>,
    /// Browseable dimensions that do not fit the resource hierarchy.
    pub facets: Vec<ActionFacet>,
}

impl ActionContext {
    pub fn for_resource(
        action_label: impl Into<String>,
        id: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            action_label: action_label.into(),
            resource: Some(ActionResource {
                id: id.into(),
                kind: kind.into(),
                label: label.into(),
            }),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_app(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.app = Some(ActionApp {
            id: id.into(),
            label: label.into(),
        });
        self
    }

    #[must_use]
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        let purpose = purpose.into();
        self.purpose = nonempty(&purpose);
        self
    }

    #[must_use]
    pub fn with_group(
        mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        self.group = Some(ActionGroup {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
        });
        self
    }

    #[must_use]
    pub fn with_facet(mut self, facet: ActionFacet) -> Self {
        self.facets.push(facet);
        self
    }

    /// Encode into the Automerge change message. Returning `None` keeps an
    /// empty/default context from displacing an app's ordinary history path.
    pub fn change_message(&self) -> Option<String> {
        if self == &Self::default() {
            return None;
        }
        serde_json::to_string(self)
            .ok()
            .map(|json| format!("{CHANGE_MESSAGE_PREFIX}{json}"))
    }

    /// Read context from a change message. Unknown and malformed messages are
    /// ordinary legacy/app commits, never an error that hides their history.
    pub fn from_change_message(message: &str) -> Option<Self> {
        let json = message.strip_prefix(CHANGE_MESSAGE_PREFIX)?;
        serde_json::from_str(json).ok()
    }

    /// Best available truthful line for a collapsed activity group.
    pub fn summary(&self) -> Option<String> {
        if let Some(purpose) = self.purpose.as_deref().and_then(nonempty_ref) {
            return Some(purpose.to_string());
        }
        match (
            nonempty_ref(&self.action_label),
            self.resource
                .as_ref()
                .and_then(|resource| nonempty_ref(&resource.label)),
        ) {
            (Some(action), Some(resource)) => Some(format!("{action} · {resource}")),
            (Some(action), None) => Some(action.to_string()),
            (None, Some(resource)) => Some(resource.to_string()),
            (None, None) => None,
        }
    }
}

fn nonempty(value: &str) -> Option<String> {
    nonempty_ref(value).map(str::to_string)
}

fn nonempty_ref(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionApp {
    /// Stable package/application id.
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResource {
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionGroup {
    /// Stable key shared by every action in the work unit.
    pub id: String,
    pub label: String,
    /// Open vocabulary: `issue`, `task`, `agent-run`, `session`, …
    pub kind: String,
}

/// A filterable activity dimension. Open key/value vocabulary lets future
/// apps add facets without changing the wire enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionFacet {
    /// e.g. `path`, `directory`, `file-type`, `mime`, `repository`.
    pub key: String,
    pub value: String,
}

impl ActionFacet {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_message_round_trips_context() {
        let context = ActionContext::for_resource(
            "Rename",
            "files.entry:brief.md",
            "files.entry",
            "brief.md",
        )
        .with_app("org.qos.Files", "Files")
        .with_purpose("Prepare the launch brief")
        .with_group("issue:42", "Launch brief", "issue")
        .with_facet(ActionFacet::new("file-type", "Markdown"));
        let message = context.change_message().expect("non-empty context");
        assert_eq!(ActionContext::from_change_message(&message), Some(context));
    }

    #[test]
    fn unrelated_or_malformed_messages_are_legacy_history() {
        assert!(ActionContext::from_change_message("saved draft").is_none());
        assert!(ActionContext::from_change_message("qos.action/1 {").is_none());
        assert!(ActionContext::default().change_message().is_none());
    }

    #[test]
    fn summary_prefers_self_described_purpose() {
        let contextual = ActionContext::for_resource("Rename", "id", "kind", "brief.md")
            .with_purpose("Prepare the launch brief");
        assert_eq!(
            contextual.summary().as_deref(),
            Some("Prepare the launch brief")
        );

        let structural = ActionContext::for_resource("Rename", "id", "kind", "brief.md");
        assert_eq!(structural.summary().as_deref(), Some("Rename · brief.md"));
    }
}
