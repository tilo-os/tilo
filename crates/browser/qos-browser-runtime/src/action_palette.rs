//! Inert broker descriptors rendered with the shared ActionPalette schema.
use serde::Deserialize;
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionLocus, OperationKind, Resource,
};

#[derive(Deserialize)]
struct Descriptor {
    key: String,
    action: String,
    action_label: String,
    resource_label: String,
    resource_kind: String,
    operation_kind: OperationKind,
    params: Vec<ActionParam>,
    contract: ActionContract,
    description: Option<String>,
}

pub(crate) fn resources(json: &str) -> Result<Vec<Resource>, String> {
    if json.len() > 4 * 1024 * 1024 {
        return Err("action projection capacity exceeded".into());
    }
    let descriptors: Vec<Descriptor> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    descriptors
        .into_iter()
        .map(|row| {
            let mut spec = ActionSpec::typed(
                row.action,
                row.action_label,
                "",
                ExecutionLocus::ClientLocal,
                row.params,
                |_| EncodedAction {
                    type_id: "qos.caller.intent".into(),
                    payload: vec![],
                },
            );
            spec.kind = row.operation_kind;
            spec.contract = row.contract;
            spec.description = row.description;
            Ok(Resource::new(row.key, row.resource_kind, row.resource_label).with_action(spec))
        })
        .collect()
}

pub(crate) fn request(
    pending: ui::action_palette::PendingOperation,
) -> Result<serde_json::Value, String> {
    let wire = pending.wire_argument();
    let argument = match wire {
        Some(value) => {
            if let Some(json) = value.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX) {
                serde_json::from_str(json).map_err(|e| e.to_string())?
            } else {
                serde_json::Value::String(value)
            }
        }
        None => serde_json::Value::Null,
    };
    let evidence = pending.confirmation_evidence();
    let confirmation = if evidence.get("confirm").and_then(|v| v.as_bool()) == Some(true) {
        serde_json::json!({"kind":"confirm"})
    } else if let Some(phrase) = evidence.get("confirmation_phrase").and_then(|v| v.as_str()) {
        serde_json::json!({"kind":"phrase","phrase":phrase})
    } else {
        serde_json::Value::Null
    };
    Ok(
        serde_json::json!({"control":pending.address.resource,"argument":argument,"confirmation":confirmation}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broker_descriptor_projects_real_contract_and_schema_without_executing() {
        let descriptor = serde_json::json!([{
            "key":"exact-certified-route", "resource":"writer:document", "action":"insert-table",
            "resource_label":"Document", "resource_kind":"document", "action_label":"Insert table",
            "operation_kind":OperationKind::Document, "params":[], "contract":ActionContract::default(),
            "description":null,"input":{"kind":"none"}
        }]);
        let projected = resources(&descriptor.to_string()).unwrap();
        assert_eq!(projected[0].id.as_str(), "exact-certified-route");
        assert_eq!(projected[0].actions[0].id, "insert-table");
        assert_eq!(projected[0].actions[0].kind, OperationKind::Document);
        assert_eq!(projected[0].actions[0].contract, ActionContract::default());
        let mut palette = ui::action_palette::ActionPalette::new();
        palette.load_open_resources(&projected);
        palette.open();
        assert!(palette.activate_address("exact-certified-route", "insert-table"));
        let request = request(palette.take_operation().unwrap()).unwrap();
        assert_eq!(
            request,
            serde_json::json!({"control":"exact-certified-route","argument":null,"confirmation":null})
        );
    }
}
