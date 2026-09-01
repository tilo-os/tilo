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
    /// Fill missing provenance from a task/session context while preserving
    /// every value supplied by the concrete action/resource.
    #[must_use]
    pub fn enriched_by(mut self, ambient: &Self) -> Self {
        if self.action_label.trim().is_empty() {
            self.action_label.clone_from(&ambient.action_label);
        }
        if self.app.is_none() {
            self.app.clone_from(&ambient.app);
        }
        if self.resource.is_none() {
            self.resource.clone_from(&ambient.resource);
        }
        if self.purpose.is_none() {
            self.purpose.clone_from(&ambient.purpose);
        }
        if self.group.is_none() {
            self.group.clone_from(&ambient.group);
        }
        for facet in &ambient.facets {
            if !self.facets.contains(facet) {
                self.facets.push(facet.clone());
            }
        }
        self
    }

    /// Build truthful baseline context from a serializable action enum.
    ///
    /// Serde's externally tagged representation exposes the stable variant
    /// name without requiring every app to duplicate a `match` merely to get a
    /// readable fallback. Apps should still replace the label and attach a
    /// resource for important domain actions.
    pub fn for_action<T: Serialize>(
        app_id: impl Into<String>,
        app_label: impl Into<String>,
        action: &T,
    ) -> Self {
        let action_label = serde_json::to_value(action)
            .ok()
            .as_ref()
            .and_then(action_variant)
            .map_or_else(|| "Update".to_string(), humanize_identifier);
        Self {
            action_label,
            ..Self::default()
        }
        .with_app(app_id, app_label)
    }

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

    /// Fill app ownership from the registered action type when a generic
    /// palette/door caller has only resource metadata. Explicit app context
    /// always wins. Action types conventionally start with the package stem
    /// (`word-processor.action`, `api_tester.action`).
    #[must_use]
    pub fn with_inferred_app(mut self, type_id: &str) -> Self {
        if self.app.is_some() {
            return self;
        }
        let stem = type_id.split('.').next().unwrap_or("").trim();
        let words: Vec<&str> = stem
            .split(['-', '_'])
            .filter(|word| !word.is_empty())
            .collect();
        if words.is_empty() {
            return self;
        }
        let title_word = |word: &str| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        };
        let component = words
            .iter()
            .map(|word| title_word(word))
            .collect::<String>();
        let label = words
            .iter()
            .map(|word| title_word(word))
            .collect::<Vec<_>>()
            .join(" ");
        self.app = Some(ActionApp {
            id: format!("org.qos.{component}"),
            label,
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

fn action_variant(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(variant) => Some(variant),
        serde_json::Value::Object(fields) if fields.len() == 1 => {
            fields.keys().next().map(String::as_str)
        }
        _ => None,
    }
}

fn humanize_identifier(value: &str) -> String {
    let mut result = String::with_capacity(value.len().saturating_add(4));
    let mut previous_lowercase_or_digit = false;
    for character in value.chars() {
        if character == '_' || character == '-' {
            if !result.ends_with(' ') {
                result.push(' ');
            }
            previous_lowercase_or_digit = false;
            continue;
        }
        if character.is_uppercase() && previous_lowercase_or_digit {
            result.push(' ');
        }
        previous_lowercase_or_digit = character.is_lowercase() || character.is_ascii_digit();
        result.push(character);
    }
    result
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

    #[test]
    fn serializable_action_provides_a_readable_truthful_fallback() {
        #[derive(Serialize)]
        enum ExampleAction {
            RenameFile { id: u64 },
        }
        let context =
            ActionContext::for_action("org.qos.Code", "Code", &ExampleAction::RenameFile { id: 7 });
        assert_eq!(context.action_label, "Rename File");
        assert_eq!(
            context.app.as_ref().map(|app| app.label.as_str()),
            Some("Code")
        );
    }

    #[test]
    fn generic_palette_context_infers_stable_app_ownership() {
        let context = ActionContext::for_resource("Export", "doc:1", "document", "Brief")
            .with_inferred_app("word-processor.action");
        assert_eq!(
            context.app,
            Some(ActionApp {
                id: "org.qos.WordProcessor".into(),
                label: "Word Processor".into(),
            })
        );
    }

    #[test]
    fn ambient_context_fills_intent_without_overwriting_the_action() {
        let action = ActionContext::for_resource("Rename", "file:1", "file", "brief.md")
            .with_app("org.qos.Code", "Code");
        let ambient = ActionContext::default()
            .with_purpose("Prepare launch")
            .with_group("issue:42", "Launch", "issue")
            .with_app("wrong", "Wrong");
        let merged = action.enriched_by(&ambient);
        assert_eq!(
            merged.app.as_ref().map(|app| app.id.as_str()),
            Some("org.qos.Code")
        );
        assert_eq!(merged.purpose.as_deref(), Some("Prepare launch"));
        assert_eq!(
            merged.group.as_ref().map(|group| group.id.as_str()),
            Some("issue:42")
        );
    }
}
