//! What an effect, or one of the resource's own actions, looks like to
//! callers right now: unavailable while the engine is offline, labelled
//! differently while recording, saying what it does on one row.

use crate::ActionRef;
use tilo_actions::{ActionKind, ActionParam, ActionSpec, Availability};

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
    /// One of the effect's parameters, narrowed to what this entity offers
    /// for it now.
    params: Vec<ParamStatus>,
    /// The row or item resource the status is about, when it is about one
    /// ([`Self::on`]) rather than the resource its view publishes.
    item: Option<String>,
    /// What the effect is on this entity alone: a page's reads take the
    /// authority to operate a page that holds the person's sign-ins, and only
    /// what any visitor has when it does not.
    kind: Option<ActionKind>,
}

/// One of an effect's parameters as the entity offers it now: the values it
/// takes here (a device's name in the nearby targets this window can see),
/// what it means here, the bounds this entity's state sets.
#[derive(Clone, Debug, PartialEq)]
struct ParamStatus {
    name: String,
    /// Each accepted value with the words it reads by, when they differ from
    /// the value itself.
    choices: Option<Vec<(String, Option<String>)>>,
    description: Option<String>,
    schema: Option<serde_json::Value>,
}

impl ParamStatus {
    fn apply(&self, param: &mut ActionParam) {
        if let Some(description) = &self.description {
            param.description = description.clone().into();
        }
        if let Some(schema) = &self.schema {
            param.schema = Some(std::sync::Arc::new(schema.clone()));
        }
        // Choices last: each one's label is a schema annotation, kept beside
        // the bounds the status sets.
        if let Some(choices) = &self.choices {
            if choices.iter().all(|(_, label)| label.is_none()) {
                param.choices = choices
                    .iter()
                    .map(|(value, _)| value.clone().into())
                    .collect();
            } else {
                *param =
                    param
                        .clone()
                        .with_labelled_choices(choices.iter().map(|(value, label)| {
                            (
                                value.clone(),
                                label.clone().unwrap_or_else(|| value.clone()),
                            )
                        }));
            }
        }
    }
}

impl EffectStatus {
    /// `effect` cannot be invoked now: `code` names why for programs,
    /// `reason` for people.
    pub fn unavailable<E, A>(
        effect: ActionRef<E, A>,
        code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::unavailable_id(effect.id, code.into(), reason.into())
    }

    /// [`Self::unavailable`] once the effect's id and its words are read, so
    /// every vocabulary's status is built by one function.
    fn unavailable_id(id: &'static str, code: String, reason: String) -> Self {
        Self {
            id,
            availability: Some(Availability::Unavailable { code, reason }),
            label: None,
            description: None,
            phrase: None,
            params: Vec::new(),
            item: None,
            kind: None,
        }
    }

    /// `effect` says of itself what `description` says on this entity, and is
    /// offered as declared otherwise.
    pub fn described<E, A>(effect: ActionRef<E, A>, description: impl Into<String>) -> Self {
        Self::described_id(effect.id, description.into())
    }

    /// [`Self::described`] once the effect's id and its words are read.
    fn described_id(id: &'static str, description: String) -> Self {
        Self {
            id,
            availability: None,
            label: None,
            description: Some(description),
            phrase: None,
            params: Vec::new(),
            item: None,
            kind: None,
        }
    }

    /// `effect` is `kind` on this entity, offered as declared otherwise.
    pub fn of_kind<E, A>(effect: ActionRef<E, A>, kind: ActionKind) -> Self {
        Self::of_kind_id(effect.id, kind)
    }

    /// [`Self::of_kind`] once the effect's id is read.
    fn of_kind_id(id: &'static str, kind: ActionKind) -> Self {
        Self {
            id,
            availability: None,
            label: None,
            description: None,
            phrase: None,
            params: Vec::new(),
            item: None,
            kind: Some(kind),
        }
    }

    /// `effect` is offered as `label` now.
    pub fn labelled<E, A>(effect: ActionRef<E, A>, label: impl Into<String>) -> Self {
        Self::labelled_id(effect.id, label.into())
    }

    /// [`Self::labelled`] once the effect's id and its words are read.
    fn labelled_id(id: &'static str, label: String) -> Self {
        Self {
            id,
            availability: None,
            label: Some(label),
            description: None,
            phrase: None,
            params: Vec::new(),
            item: None,
            kind: None,
        }
    }

    /// The effect's parameter `param` accepts `choices` on this entity: the
    /// apps that open this file, the devices nearby now.
    #[must_use]
    pub fn with_param_choices<P, V>(self, param: P, choices: impl IntoIterator<Item = V>) -> Self
    where
        P: Into<String>,
        V: Into<String>,
    {
        self.with_labelled_param_choices(
            param,
            choices.into_iter().map(|choice| {
                let choice = choice.into();
                (choice.clone(), choice)
            }),
        )
    }

    /// [`Self::with_param_choices`], each choice with the words it reads by:
    /// a nearby device's handle reads as the device's name.
    #[must_use]
    pub fn with_labelled_param_choices<P, V, L>(
        mut self,
        param: P,
        choices: impl IntoIterator<Item = (V, L)>,
    ) -> Self
    where
        P: Into<String>,
        V: Into<String>,
        L: Into<String>,
    {
        let choices: Vec<(String, Option<String>)> = choices
            .into_iter()
            .map(|(value, label)| {
                let value = value.into();
                let label = label.into();
                (value.clone(), (label != value).then_some(label))
            })
            .collect();
        self.param(param).choices = Some(choices);
        self
    }

    /// The effect's parameter `param` means `description` on this entity.
    #[must_use]
    pub fn with_param_description<P: Into<String>>(
        mut self,
        param: P,
        description: impl Into<String>,
    ) -> Self {
        self.param(param).description = Some(description.into());
        self
    }

    /// The effect's parameter `param` is bounded by `schema` on this entity.
    #[must_use]
    pub fn with_param_schema<P: Into<String>>(
        mut self,
        param: P,
        schema: serde_json::Value,
    ) -> Self {
        self.param(param).schema = Some(schema);
        self
    }

    /// The status of `param` on this entity, added when the status does not
    /// name it yet.
    #[allow(
        clippy::expect_used,
        reason = "the parameter was just added when the search found none"
    )]
    fn param(&mut self, param: impl Into<String>) -> &mut ParamStatus {
        let name = param.into();
        if !self.params.iter().any(|held| held.name == name) {
            self.params.push(ParamStatus {
                name: name.clone(),
                choices: None,
                description: None,
                schema: None,
            });
        }
        self.params
            .iter_mut()
            .find(|held| held.name == name)
            .expect("the parameter was just added when it was missing")
    }

    /// This status is about the item or row published as `item` rather than
    /// the resource its view publishes: the effect is offered as the status
    /// says on that entity's published actions alone (a process that is a
    /// system service cannot be ended), served by
    /// a publisher's row status.
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
    #[must_use]
    pub fn item(&self) -> Option<&str> {
        self.item.as_deref()
    }

    /// Show `spec` as this status says it is right now.
    pub fn overlay(&self, spec: &mut ActionSpec) {
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
        for param in &self.params {
            spec.narrow_param(&param.name, |declared| param.apply(declared));
        }
        if let Some(kind) = self.kind {
            spec.type_mut().contract.kind = kind;
        }
    }
}

/// Lay `statuses` over `resource`'s own actions, each by its id.
pub fn apply_statuses(resource: &mut tilo_actions::ResourceSpec, statuses: &[EffectStatus]) {
    for status in statuses {
        for spec in resource
            .actions
            .iter_mut()
            .filter(|spec| spec.id == status.id())
        {
            status.overlay(spec);
        }
    }
}

/// Lay what a publisher says of each row's effects over `rows`, the rows
/// nested beneath them included.
pub fn apply_row_statuses(
    rows: &mut [tilo_actions::ResourceSpec],
    statuses: &dyn Fn(&str) -> Vec<EffectStatus>,
) {
    for row in rows {
        let said = statuses(row.id.as_str());
        if !said.is_empty() {
            apply_statuses(row, &said);
        }
        apply_row_statuses(&mut row.children, statuses);
    }
}

#[cfg(test)]
mod tests {
    use super::EffectStatus;
    use crate::Effect as _;

    /// A send to one device: what a row's status narrows per entity.
    #[derive(Clone, Debug, PartialEq, Eq, crate::Effect)]
    #[effect(operation = "test.status.view", catalogue = "tests/locales")]
    enum StatusEffect {
        /// Send
        #[effect(kind = "read")]
        Send {
            #[param("The device")]
            target: String,
            #[param("The port")]
            port: i64,
        },
    }

    #[test]
    fn a_status_narrows_the_parameters_of_the_effect_it_is_about() {
        let mut spec = StatusEffect::Send {
            target: String::new(),
            port: 0,
        }
        .bound_asking(&["target", "port"]);
        EffectStatus::labelled(StatusEffect::SEND, "Send")
            .with_labelled_param_choices("target", [("a", "Device A"), ("b", "Device B")])
            .with_param_description("port", "The receiver's port")
            .with_param_schema("port", serde_json::json!({"minimum": 1}))
            .overlay(&mut spec);
        let target = spec
            .params()
            .iter()
            .find(|param| param.name == "target")
            .expect("the target parameter");
        assert_eq!(
            target
                .choices
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["a", "b"],
            "only the devices this entity can send to are offered"
        );
        assert_eq!(target.choice_label("a"), Some("Device A"));
        let port = spec
            .params()
            .iter()
            .find(|param| param.name == "port")
            .expect("the port parameter");
        assert_eq!(&*port.description, "The receiver's port");
        assert_eq!(
            port.schema.as_deref(),
            Some(&serde_json::json!({"minimum": 1})),
            "the entity's state bounds what it accepts"
        );
    }

    #[test]
    fn a_status_leaves_the_parameters_it_does_not_name_as_declared() {
        let mut spec = StatusEffect::Send {
            target: String::new(),
            port: 0,
        }
        .bound_asking(&["target", "port"]);
        let declared = spec
            .params()
            .iter()
            .find(|param| param.name == "port")
            .expect("the port parameter")
            .clone();
        EffectStatus::labelled(StatusEffect::SEND, "Send")
            .with_param_choices("missing", ["a"])
            .overlay(&mut spec);
        assert_eq!(
            spec.params()
                .iter()
                .find(|param| param.name == "port")
                .expect("the port parameter"),
            &declared
        );
    }
}
