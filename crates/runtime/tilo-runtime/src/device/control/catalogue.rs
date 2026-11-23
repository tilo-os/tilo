//! What the device publishes on `devices/<device>/vm`: one action per step its step program
//! declares, each with the parameters it declared.

use tilo_actions::{
    ActionKind, ActionParam, ActionSpec, ConfirmationPolicy, EncodedAction, OperationKind,
    ResourceSpec,
};

/// One step as `tilo-device-step catalogue` declares it.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Declared {
    /// The action id, which the step's unit is named by.
    pub id: String,
    pub label: String,
    pub params: Vec<ActionParam>,
    /// Whether a person confirms it before it runs.
    #[serde(default)]
    pub confirm: bool,
}

/// The resource `resource` with an action for each `declared` step, published by `provider`.
pub(super) fn resources(
    provider: &str,
    resource: &str,
    declared: &[Declared],
) -> Vec<ResourceSpec> {
    let mut spec = ResourceSpec::new(resource, "devices.vm", "Device");
    for step in declared {
        let type_id = format!("{provider}.vm/{}", step.id);
        let encoded = type_id.clone();
        let mut action = ActionSpec::typed_operation(
            step.id.clone(),
            step.label.clone(),
            "",
            OperationKind::Effect,
            type_id,
            step.params.clone(),
            move |_| EncodedAction {
                type_id: encoded.clone(),
                payload: Vec::new(),
            },
        );
        let contract = &mut action.type_mut().contract;
        contract.kind = ActionKind::Outside;
        if step.confirm {
            contract.confirmation = ConfirmationPolicy::Confirm;
        }
        spec.actions.push(action);
    }
    vec![spec]
}
