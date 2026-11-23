//! A control's own verb: what pressing the control itself reports — the text a field's person typed,
//! the choice a picker's person made. Published as control `tilo.control:<id>`'s view action and
//! answered by the control's own handler, so a person's press here, a screen reader and a script
//! reach the same handler through the same action. It stands in for that person's input: no agent
//! invokes it (`tilo_actions::kind_is_a_drawn_control`).

use super::{ActionEmitter, ControlBindingMetadata};
use crate::tilo_actions::{
    ActionBinding, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, ResourceSpec,
};

#[derive(Clone, Copy)]
pub(crate) enum VerbArgument {
    Text,
    Integer,
    Number,
    Flag,
}

impl VerbArgument {
    fn param_type(self) -> ParamType {
        match self {
            Self::Text => ParamType::Text,
            Self::Integer => ParamType::Integer,
            Self::Number => ParamType::Number,
            Self::Flag => ParamType::Boolean,
        }
    }

    fn json(self, args: &crate::tilo_actions::ActionArgs) -> serde_json::Value {
        match self {
            Self::Text => serde_json::json!(args.text("value").unwrap_or_default()),
            Self::Integer => serde_json::json!(args.integer("value").unwrap_or_default()),
            Self::Number => serde_json::json!(args.number("value").unwrap_or_default()),
            Self::Flag => serde_json::json!(args.boolean("value").unwrap_or_default()),
        }
    }
}

#[derive(serde::Deserialize)]
struct Payload<A> {
    control: String,
    value: A,
}

pub(crate) struct ControlVerb<A> {
    control: String,
    type_id: &'static str,
    resource: ResourceSpec,
    action: ActionEmitter<A>,
}

impl<A: serde::Serialize + 'static> ControlVerb<A> {
    #[expect(
        clippy::expect_used,
        reason = "the generated payload is serializable and the action is the resource's own"
    )]
    pub(crate) fn new(
        id: &str,
        label: &str,
        verb: &'static str,
        type_id: &'static str,
        argument: VerbArgument,
    ) -> Self {
        let control = id.to_owned();
        let param = ActionParam::required("value", argument.param_type(), "What it reports");
        let spec = ActionSpec::typed_operation(
            verb,
            label,
            label,
            OperationKind::View,
            type_id,
            vec![param],
            move |args| {
                EncodedAction::encode_operation(
                    type_id,
                    &serde_json::json!({ "control": control, "value": argument.json(args) }),
                )
                .expect("a control verb's generated payload is serializable")
            },
        )
        // A typed operation's kind is named by its declaration, and this is it: a value verb
        // changes only the control. What the app writes from the value is decided as that
        // write is, so a person typing in a field is not asked as if the field reached out.
        .of_kind(crate::tilo_actions::ActionKind::Read)
        .for_audience(crate::tilo_actions::Audience::Agents);
        let resource = ResourceSpec::new(format!("tilo.control:{id}"), "tilo.control", label)
            .with_action(spec.clone());
        let binding = ActionBinding::new(&resource, &spec, |value: A| {
            serde_json::Map::from_iter([(
                "value".into(),
                serde_json::to_value(value).unwrap_or_default(),
            )])
        })
        .expect("a control verb's argument is the value its action declares");
        Self {
            control: id.to_owned(),
            type_id,
            resource,
            action: ActionEmitter::new_replace_one(binding),
        }
    }

    pub(crate) fn emit(&mut self, value: A) {
        if let Err(error) = self.action.emit(value) {
            tracing::warn!(%error, control = %self.control, "a control refused its own verb");
        }
    }

    pub(crate) fn resource(&self) -> tilo_actions::ResourceSpec {
        self.resource.clone()
    }

    pub(crate) fn attach(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        let _ = self.action.attach(domain, attachment);
    }

    pub(crate) fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.take()
    }

    pub(crate) fn refresh_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.action.refresh_availability(allowed);
    }

    pub(crate) fn metadata(&self, control_id: &str) -> ControlBindingMetadata {
        self.action.metadata(control_id)
    }
}

impl<A: serde::de::DeserializeOwned> ControlVerb<A> {
    pub(crate) fn start(
        &self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<A>, String> {
        let request = op.request();
        if request.kind != OperationKind::View || request.type_id != self.type_id {
            return Ok(None);
        }
        let payload: Payload<A> =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        Ok((payload.control == self.control).then_some(payload.value))
    }
}

/// How a control with an optional bound action `self.action` and an optional verb of its own
/// `self.change_verb` joins its window's controls: both attach, report and refresh together.
macro_rules! action_and_verb_controls {
    () => {
        fn attach_controls(
            &mut self,
            domain: &crate::ControlDomain,
            attachment: crate::control_domain::DomainAttachment,
        ) {
            if let Some(action) = &mut self.action {
                let _ = action.attach(domain, attachment);
            }
            if let Some(verb) = &mut self.change_verb {
                verb.attach(domain, attachment);
            }
        }
        fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
            self.action
                .as_mut()
                .and_then(crate::widget::ActionEmitter::take)
                .or_else(|| {
                    self.change_verb
                        .as_mut()
                        .and_then(crate::widget::control_verb::ControlVerb::take)
                })
        }
        fn refresh_control_action_availability(
            &mut self,
            allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
        ) {
            if let Some(action) = &mut self.action {
                action.refresh_availability(allowed);
            }
            if let Some(verb) = &mut self.change_verb {
                verb.refresh_availability(allowed);
            }
        }
    };
}
pub(crate) use action_and_verb_controls;
