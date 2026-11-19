//! What an effect, or one of the resource's own actions, looks like to
//! callers right now: unavailable while the engine is offline, labelled
//! differently while recording, saying what it does on one row.

use tilo_actions::{ActionSpec, Availability};
use tilo_resource::ActionRef;

/// What an effect, or one of the resource's own actions, looks like to
/// callers right now, when it differs from its declaration: unavailable
/// while the engine is offline, labelled "Stop recording" while recording.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectStatus {
    id: &'static str,
    availability: Option<Availability>,
    label: Option<String>,
    /// What the effect says of itself on this entity right now, when the
    /// entity's own state says it (what erasing this drive loses).
    description: Option<String>,
    /// What its caller types to confirm this invocation on this entity (the
    /// drive's name), when the entity's state says it.
    phrase: Option<String>,
    /// The row or item resource the status is about, when it is about one
    /// ([`Self::on`]) rather than the resource its view publishes.
    item: Option<String>,
}

impl EffectStatus {
    /// `effect` cannot be invoked now: `code` names why for programs,
    /// `reason` for people.
    pub fn unavailable<E, A>(
        effect: ActionRef<E, A>,
        code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: effect.id,
            availability: Some(Availability::Unavailable {
                code: code.into(),
                reason: reason.into(),
            }),
            label: None,
            description: None,
            phrase: None,
            item: None,
        }
    }

    /// `effect` says of itself what `description` says on this entity, and is
    /// offered as declared otherwise.
    pub fn described<E, A>(effect: ActionRef<E, A>, description: impl Into<String>) -> Self {
        Self {
            id: effect.id,
            availability: None,
            label: None,
            description: Some(description.into()),
            phrase: None,
            item: None,
        }
    }

    /// `effect` is offered as `label` now.
    pub fn labelled<E, A>(effect: ActionRef<E, A>, label: impl Into<String>) -> Self {
        Self {
            id: effect.id,
            availability: None,
            label: Some(label.into()),
            description: None,
            phrase: None,
            item: None,
        }
    }

    /// This status is about the item or row published as `item` rather than
    /// the resource its view publishes: the effect is offered as the status
    /// says on that entity's published actions alone (a process that is a
    /// system service cannot be ended), served by
    /// [`Publishes::row_status`](super::Publishes::row_status).
    #[must_use]
    pub fn on(mut self, item: impl Into<String>) -> Self {
        self.item = Some(item.into());
        self
    }

    /// Offer the effect as `label` too.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Say what the effect does on this entity right now, in the entity's
    /// own words: what erasing this drive loses.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Have the caller type `phrase` to confirm this invocation on this
    /// entity: the name of what it erases.
    #[must_use]
    pub fn with_phrase(mut self, phrase: impl Into<String>) -> Self {
        self.phrase = Some(phrase.into());
        self
    }

    /// How the effect stands now, when the status narrows it.
    #[must_use]
    pub fn availability(&self) -> Option<&Availability> {
        self.availability.as_ref()
    }

    /// What the effect says of itself on this entity, when the status says.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// What its caller types to confirm it on this entity, when the status
    /// asks for one.
    #[must_use]
    pub fn phrase(&self) -> Option<&str> {
        self.phrase.as_deref()
    }

    /// The id of the action or effect this status is about.
    #[must_use]
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// The item or row this status is about, when it is about one rather
    /// than the published resource.
    pub(crate) fn item(&self) -> Option<&str> {
        self.item.as_deref()
    }

    /// Show `spec` as this status says it is right now.
    pub(crate) fn overlay(&self, spec: &mut ActionSpec) {
        if let Some(availability) = &self.availability {
            spec.type_mut().contract.availability = availability.clone();
        }
        if let Some(label) = &self.label {
            spec.type_mut().label.clone_from(label);
        }
        if let Some(description) = &self.description {
            spec.type_mut().description = Some(description.clone());
        }
        if let Some(phrase) = &self.phrase {
            spec.type_mut().contract.confirmation = tilo_actions::ConfirmationPolicy::Phrase {
                phrase: phrase.clone(),
            };
        }
    }
}
