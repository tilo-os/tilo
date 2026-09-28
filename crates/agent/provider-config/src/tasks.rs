//! The registry of model tasks: every feature that asks the model gateway for
//! inference names one of these, never a provider profile or a model.
//!
//! A task is a typed capability consumer. It states who may ask for it
//! ([`Consumer`]), what a model must be able to do to serve it
//! ([`Requirements`]), and how private its input is ([`PrivacyClass`]). The
//! gateway resolves `(consumer, task, agent)` to a concrete profile and model
//! through the person's assignments ([`crate::assign`]) and enforces the task's
//! constraints whatever a caller asks for.
//!
//! The registry is closed and compiled into the trusted gateway on purpose: a
//! sandboxed app cannot declare its own privacy class. A feature registers a
//! task by adding a [`TaskSpec`] constant here and listing it in [`REGISTRY`].

use serde::{Deserialize, Serialize};

use crate::{EndpointLocation, ModelCapability, ModelDescriptor, ProviderProfile};

/// Who is asking the gateway. Apps are identified by the launcher-attested
/// package id; the session is the compositor itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "principal", rename_all = "snake_case")]
pub enum Consumer {
    App { app_id: String },
    Session,
}

impl Consumer {
    #[must_use]
    pub fn app(app_id: &str) -> Self {
        Self::App {
            app_id: app_id.to_string(),
        }
    }
}

/// A consumer as a task declares it, in `const` form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllowedConsumer {
    App(&'static str),
    Session,
}

impl AllowedConsumer {
    fn admits(self, consumer: &Consumer) -> bool {
        match (self, consumer) {
            (Self::App(allowed), Consumer::App { app_id }) => allowed == app_id,
            (Self::Session, Consumer::Session) => true,
            _ => false,
        }
    }
}

/// How private a task's input is, and therefore where it may be served.
///
/// Ordered from least to most private, so the stricter of two classes is
/// their `max`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    /// Any configured provider may serve it.
    #[default]
    Open,
    /// Served on this device unless the person explicitly allows a hosted or
    /// private-network model for this task (or agent). Presets never grant
    /// that override.
    Sensitive,
    /// Only a supervised model on this device, whatever anyone assigns.
    DeviceOnly,
}

impl PrivacyClass {
    /// Whether a model at `location` may serve this class, given whether the
    /// person explicitly allowed off-device models for the assignment.
    #[must_use]
    pub const fn permits(self, location: EndpointLocation, allow_off_device: bool) -> bool {
        match (self, location) {
            (_, EndpointLocation::ThisDevice) | (Self::Open, _) => true,
            (Self::Sensitive, _) => allow_off_device,
            (Self::DeviceOnly, _) => false,
        }
    }
}

/// What a model must declare to serve a task. Declarations come from the
/// signed catalogue for local models and from the provider profile otherwise;
/// nothing is inferred from a model's name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Requirements {
    pub tools: bool,
    /// Fixed protocols the model must declare.
    pub capabilities: &'static [ModelCapability],
    /// Smallest context window, in tokens. `0` means no requirement; a model
    /// with an unknown window satisfies only a zero requirement.
    pub min_context: u32,
}

/// The first requirement a model does not meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unmet {
    Tools,
    Capability { capability: ModelCapability },
    Context,
}

impl Requirements {
    /// Check `model` against these requirements.
    ///
    /// # Errors
    ///
    /// Returns the first capability the model does not declare.
    pub fn check(&self, model: &ModelDescriptor) -> Result<(), Unmet> {
        if self.tools && !model.tool_calling {
            return Err(Unmet::Tools);
        }
        if let Some(capability) = self
            .capabilities
            .iter()
            .copied()
            .find(|capability| !model.declares(*capability))
        {
            return Err(Unmet::Capability { capability });
        }
        if self.min_context > 0
            && model
                .context_window
                .is_none_or(|window| window < self.min_context)
        {
            return Err(Unmet::Context);
        }
        Ok(())
    }
}

/// One registered task.
#[derive(Debug, PartialEq, Eq)]
pub struct TaskSpec {
    /// Stable id, `<feature>.<task>`. It is persisted in assignments.
    pub id: &'static str,
    /// Fluent key of the task's name in Settings.
    pub label_key: &'static str,
    /// Fluent key of the one-line explanation shown beside it.
    pub description_key: &'static str,
    pub consumers: &'static [AllowedConsumer],
    pub requires: Requirements,
    pub privacy: PrivacyClass,
    /// Whether an agent-specific assignment can override the task's own.
    pub per_agent: bool,
    pub request: TaskRequest,
}

/// Who writes the request a task sends to its model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskRequest {
    /// The caller writes a provider-dialect body; the gateway sets the model
    /// and options (the `Plan` verb).
    CallerDialect,
    /// The gateway writes the provider request through a fixed verb; no
    /// caller-written body may reach the model.
    Fixed,
}

impl TaskSpec {
    #[must_use]
    pub fn admits(&self, consumer: &Consumer) -> bool {
        self.consumers
            .iter()
            .any(|allowed| allowed.admits(consumer))
    }

    /// Whether `profile`'s `model` meets this task's capability and privacy
    /// constraints, with or without an explicit off-device allowance.
    #[must_use]
    pub fn permits(
        &self,
        profile: &ProviderProfile,
        model: &ModelDescriptor,
        allow_off_device: bool,
    ) -> bool {
        self.requires.check(model).is_ok()
            && self.privacy.permits(profile.location, allow_off_device)
    }
}

pub const AGENT_APP: &str = "so.tilo.agent";
pub const PHOTOS_APP: &str = "so.tilo.photos";

/// An agent choosing and sequencing actions from the live catalogue.
pub const AGENT_PLANNING: TaskSpec = TaskSpec {
    id: "agent.planning",
    label_key: "ai-task-agent-planning",
    description_key: "ai-task-agent-planning-description",
    consumers: &[AllowedConsumer::App(AGENT_APP)],
    requires: Requirements {
        tools: true,
        capabilities: &[],
        min_context: 16_384,
    },
    privacy: PrivacyClass::Open,
    per_agent: true,
    request: TaskRequest::CallerDialect,
};

/// A single-shot transformation of text an agent was handed: summarising,
/// extracting to a schema, classifying or a freeform instruction. The
/// gateway writes the provider request itself, with no tools, so the caller
/// supplies only instructions and input.
pub const AGENT_TRANSFORM: TaskSpec = TaskSpec {
    id: "agent.transform",
    label_key: "ai-task-agent-transform",
    description_key: "ai-task-agent-transform-description",
    consumers: &[AllowedConsumer::App(AGENT_APP)],
    requires: Requirements {
        tools: false,
        capabilities: &[],
        min_context: 0,
    },
    privacy: PrivacyClass::Open,
    per_agent: true,
    request: TaskRequest::Fixed,
};

/// Describing, labelling and reading text in a photo for local search.
pub const PHOTOS_VISION: TaskSpec = TaskSpec {
    id: "photos.vision",
    label_key: "ai-task-photos-vision",
    description_key: "ai-task-photos-vision-description",
    consumers: &[AllowedConsumer::App(PHOTOS_APP)],
    requires: Requirements {
        tools: false,
        capabilities: &[ModelCapability::Vision],
        min_context: 0,
    },
    privacy: PrivacyClass::DeviceOnly,
    per_agent: false,
    request: TaskRequest::Fixed,
};

/// Grouping photos by the faces in them. Biometric: never leaves the device.
pub const PHOTOS_FACES: TaskSpec = TaskSpec {
    id: "photos.faces",
    label_key: "ai-task-photos-faces",
    description_key: "ai-task-photos-faces-description",
    consumers: &[AllowedConsumer::App(PHOTOS_APP)],
    requires: Requirements {
        tools: false,
        capabilities: &[ModelCapability::FaceRecognition],
        min_context: 0,
    },
    privacy: PrivacyClass::DeviceOnly,
    per_agent: false,
    request: TaskRequest::Fixed,
};

/// Proposing redactions and step titles for a feedback report before it is
/// sent. The trace carries personal text, so it stays local unless the person
/// explicitly allows otherwise.
pub const FEEDBACK_REVIEW: TaskSpec = TaskSpec {
    id: "feedback.review",
    label_key: "ai-task-feedback-review",
    description_key: "ai-task-feedback-review-description",
    consumers: &[AllowedConsumer::Session],
    requires: Requirements {
        tools: false,
        capabilities: &[ModelCapability::TraceReview],
        min_context: 8_192,
    },
    privacy: PrivacyClass::Sensitive,
    per_agent: false,
    request: TaskRequest::Fixed,
};

/// Triaging feedback reports: whether a new report repeats an earlier one,
/// and a forge issue's title and summary. Public `#feedback` reports are
/// open content, so the task is [`PrivacyClass::Open`]; a report from a
/// private direct room is asked for as [`PrivacyClass::Sensitive`]
/// (`assign::resolve_for`), which keeps it on this device by default.
pub const FEEDBACK_TRIAGE: TaskSpec = TaskSpec {
    id: "feedback.triage",
    label_key: "ai-task-feedback-triage",
    description_key: "ai-task-feedback-triage-description",
    consumers: &[AllowedConsumer::App(AGENT_APP)],
    requires: Requirements {
        tools: false,
        capabilities: &[],
        min_context: 0,
    },
    privacy: PrivacyClass::Open,
    per_agent: true,
    request: TaskRequest::Fixed,
};

/// Every task the gateway serves.
pub const REGISTRY: &[&TaskSpec] = &[
    &AGENT_PLANNING,
    &AGENT_TRANSFORM,
    &PHOTOS_VISION,
    &PHOTOS_FACES,
    &FEEDBACK_REVIEW,
    &FEEDBACK_TRIAGE,
];

/// Look up a registered task by id.
#[must_use]
pub fn task(id: &str) -> Option<&'static TaskSpec> {
    REGISTRY.iter().copied().find(|task| task.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_are_unique_and_namespaced() {
        let mut seen = std::collections::BTreeSet::new();
        for task in REGISTRY {
            assert!(task.id.contains('.'), "{}", task.id);
            assert!(seen.insert(task.id), "duplicate {}", task.id);
            assert!(!task.consumers.is_empty(), "{} has no consumer", task.id);
        }
    }

    #[test]
    fn only_declared_consumers_are_admitted() {
        assert!(AGENT_PLANNING.admits(&Consumer::app(AGENT_APP)));
        assert!(!AGENT_PLANNING.admits(&Consumer::app(PHOTOS_APP)));
        assert!(!AGENT_PLANNING.admits(&Consumer::Session));
        assert!(AGENT_TRANSFORM.admits(&Consumer::app(AGENT_APP)));
        assert!(!AGENT_TRANSFORM.admits(&Consumer::app(PHOTOS_APP)));
        assert!(FEEDBACK_REVIEW.admits(&Consumer::Session));
        assert!(!FEEDBACK_REVIEW.admits(&Consumer::app(AGENT_APP)));
    }

    #[test]
    fn privacy_classes_order_where_a_model_may_run() {
        use EndpointLocation::{Hosted, PrivateNetwork, ThisDevice};
        for class in [
            PrivacyClass::Open,
            PrivacyClass::Sensitive,
            PrivacyClass::DeviceOnly,
        ] {
            assert!(class.permits(ThisDevice, false));
        }
        assert!(PrivacyClass::Open.permits(Hosted, false));
        assert!(!PrivacyClass::Sensitive.permits(Hosted, false));
        assert!(PrivacyClass::Sensitive.permits(PrivateNetwork, true));
        assert!(!PrivacyClass::DeviceOnly.permits(Hosted, true));
    }

    #[test]
    fn requirements_read_declared_capabilities_only() {
        let mut model = ModelDescriptor::new("m");
        model.context_window = Some(32_768);
        assert_eq!(AGENT_PLANNING.requires.check(&model), Ok(()));
        model.context_window = None;
        assert_eq!(AGENT_PLANNING.requires.check(&model), Err(Unmet::Context));
        model.capabilities.insert(ModelCapability::Vision);
        assert_eq!(
            PHOTOS_FACES.requires.check(&model),
            Err(Unmet::Capability {
                capability: ModelCapability::FaceRecognition
            })
        );
    }
}
