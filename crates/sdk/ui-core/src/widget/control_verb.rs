//! A control's own verb: what pressing the control itself reports — the text
//! a field's person typed, the choice a picker's person made. Published as
//! control `tilo.control:<id>`'s view action and answered by the control's
//! own handler, so a person's press here, a screen reader, a script and an
//! agent reach the same handler through the same action. What a verb changes
//! is what this window shows, never the document: it is absent from history.
//!
//! The argument a verb carries is its one declared parameter `value`, of the
//! type the control reports ([`VerbArgument`]).

use super::{ActionEmitter, ControlBindingMetadata};
use crate::tilo_actions::{
    ActionBinding, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, ResourceSpec,
};

/// The type of the value a verb's action declares.
#[derive(Clone, Copy)]
pub(crate) enum VerbArgument {
    Text,
    Integer,
    Flag,
}

impl VerbArgument {
    fn param_type(self) -> ParamType {
        match self {
            Self::Text => ParamType::Text,
            Self::Integer => ParamType::Integer,
            Self::Flag => ParamType::Boolean,
        }
    }

    fn json(self, args: &crate::tilo_actions::ActionArgs) -> serde_json::Value {
        match self {
            Self::Text => serde_json::json!(args.text("value").unwrap_or_default()),
            Self::Integer => serde_json::json!(args.integer("value").unwrap_or_default()),
            Self::Flag => serde_json::json!(args.boolean("value").unwrap_or_default()),
        }
    }
}

/// What a verb's operation carries: the control and the value it reports.
#[derive(serde::Deserialize)]
struct Payload<A> {
    control: String,
    value: A,
}

/// A control's published verb and the emitter it reports through.
pub(crate) struct ControlVerb<A> {
    control: String,
    type_id: &'static str,
    resource: ResourceSpec,
    action: ActionEmitter<A>,
}

impl<A: serde::Serialize + 'static> ControlVerb<A> {
    /// `verb` of control `id`, called `label`, its one argument `argument`.
    #[expect(
        clippy::expect_used,
        reason = "the generated payload is serializable and the action is the resource's own"
    )]
    pub(crate) fn new(
        id: &str,
        label: &'static str,
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

    /// Report one value. A report the host has not collected yet is replaced
    /// by the next: the control's latest state is what it says.
    pub(crate) fn emit(&mut self, value: A) {
        if let Err(error) = self.action.emit(value) {
            tracing::warn!(%error, control = %self.control, "a control refused its own verb");
        }
    }

    pub(crate) fn resource(&self) -> tilo_actions::ResourceSpec {
        self.resource.clone()
    }

    pub(crate) fn install_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let _ = self.action.install_custody(domain, attachment);
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

    /// The control's published verb, for the inventory an agent reads.
    pub(crate) fn metadata(&self, control_id: &str) -> ControlBindingMetadata {
        self.action.metadata(control_id)
    }
}

impl<A: serde::de::DeserializeOwned> ControlVerb<A> {
    /// The value a `view` operation addressed to this control reports, when
    /// the operation is this control's verb.
    pub(crate) fn start(
        &self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<A>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind != OperationKind::View || request.type_id != self.type_id {
            return Ok(None);
        }
        let payload: Payload<A> =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        Ok((payload.control == self.control).then_some(payload.value))
    }
}
