//! Enumerate and admit actions against an app's published resource tree.
//!
//! This is the external door for a console app, and it adds no rules of its
//! own: resolution, availability and strict binding are
//! [`tilo_actions::door`]'s, and confirmation follows the projection the
//! compositor's live-app provider already publishes — an action whose
//! contract asks for `Confirm` takes a required boolean `confirm`, one that
//! asks for a `Phrase` takes a required text `confirmation_phrase`. Those two
//! arguments are consumed here and never reach the action's own binding.

use serde_json::{Map, Value, json};
use tilo_actions::door::{self, ActionAddress};
use tilo_actions::{
    ActionParam, Availability, ConfirmationPolicy, EncodedAction, OperationKind, ParamType,
    Resource,
};

/// Boolean argument that carries an explicit confirmation.
pub const CONFIRM_ARG: &str = "confirm";
/// Text argument that carries a typed confirmation phrase.
pub const PHRASE_ARG: &str = "confirmation_phrase";

/// The single-string id for one action: `<action>@<resource>`. Readable on a
/// command line (`tilo-control actions invoke installer next@installer`) and
/// unambiguous, because action ids never contain `@`.
pub fn id_for(address: &ActionAddress) -> String {
    format!("{}@{}", address.action, address.resource)
}

/// Inverse of [`id_for`]; `None` for anything that is not an id.
pub fn parse_id(id: &str) -> Option<ActionAddress> {
    let (action, resource) = id.split_once('@')?;
    (!action.is_empty() && !resource.is_empty()).then(|| ActionAddress::new(resource, action))
}

/// The parameters a caller must send: the action's own, plus the
/// confirmation argument its contract requires.
pub fn projected_params(params: &[ActionParam], policy: &ConfirmationPolicy) -> Vec<ActionParam> {
    let mut params = params.to_vec();
    match policy {
        ConfirmationPolicy::None => {}
        ConfirmationPolicy::Confirm => params.push(ActionParam::required(
            CONFIRM_ARG,
            ParamType::Boolean,
            "Explicitly confirm this consequential action",
        )),
        ConfirmationPolicy::Phrase { .. } => params.push(ActionParam::required(
            PHRASE_ARG,
            ParamType::Text,
            "Type the action's confirmation phrase exactly",
        )),
    }
    params
}

/// Every published action as inert data, filtered by `query` (every
/// whitespace-separated word must appear in the action's label, resource
/// label, id or kind; an empty query lists everything). Nothing in the reply
/// can perform an action: it carries addresses, never handles.
pub fn enumerate(provider: &str, resources: &[Resource], query: &str) -> Value {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut actions = Vec::new();
    for resource in resources {
        describe_into(provider, resource, &words, &mut actions);
    }
    json!({ "query": query, "count": actions.len(), "actions": actions })
}

fn describe_into(provider: &str, resource: &Resource, words: &[String], out: &mut Vec<Value>) {
    for spec in &resource.actions {
        let address = ActionAddress::new(resource.id.as_str(), &spec.id);
        let id = id_for(&address);
        let haystack =
            format!("{} {} {} {}", spec.label, resource.label, id, resource.kind).to_lowercase();
        if !words.iter().all(|word| haystack.contains(word.as_str())) {
            continue;
        }
        let (available, reason) = match &spec.contract.availability {
            Availability::Available => (true, Value::Null),
            Availability::Unavailable { code, reason } => {
                (false, json!({ "code": code, "reason": reason }))
            }
        };
        let mut projected = projected_params(spec.params(), &spec.contract.confirmation);
        if spec.contract.confirmation == ConfirmationPolicy::None
            && spec.contract.confirmation_depends_on_arguments()
        {
            // Whether this call needs confirming depends on its value (a
            // generic edit that may delete); the refusal says when it does.
            projected.push(ActionParam::optional(
                CONFIRM_ARG,
                ParamType::Boolean,
                "Confirm a consequential value, such as a deletion",
            ));
        }
        let params: Vec<Value> = projected
            .iter()
            .map(|param| serde_json::to_value(param).unwrap_or(Value::Null))
            .collect();
        out.push(json!({
            "provider": provider,
            "id": id,
            "kind": "Actions",
            "operation_kind": spec.kind.as_str(),
            "title": spec.label,
            "subtitle": resource.label,
            "description": spec.description,
            "score": 0,
            "params": params,
            "contract": spec.contract,
            "available": available,
            "unavailable": reason,
            "resource": resource.id.as_str(),
            "resource_kind": resource.kind,
            "resource_detail": resource.presentation.as_ref().and_then(|p| p.detail.clone()),
            "operation": spec.id,
        }));
    }
    for child in &resource.children {
        describe_into(provider, child, words, out);
    }
}

/// A refused invocation, with the code the wire reports. The codes are the
/// ones [`tilo_actions::DoorError::code`] and the compositor use, so a caller
/// handles "wrong address", "wrong arguments", "not now" and "not
/// confirmed" the same way against either endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    fn unknown(id: &str) -> Self {
        Self {
            code: "unknown_action",
            message: format!("no action `{id}` is published here; `actions list` shows what is"),
        }
    }

    fn from_door(error: &door::DoorError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

/// An invocation that passed the door: resolved, available, confirmed as
/// its contract requires, and bound strictly into the action's own encoded
/// form. The owning app routes it by `kind`.
pub struct Admitted {
    pub address: ActionAddress,
    pub label: String,
    pub kind: OperationKind,
    pub encoded: EncodedAction,
    /// The phrase the caller typed, for a `Phrase` contract. The app may
    /// reflect it into its own confirmation field so every view of the
    /// ceremony agrees.
    pub phrase: Option<String>,
}

/// Admit one `actions_invoke` request.
///
/// # Errors
///
/// Returns a [`Refusal`] when the provider or id names nothing published
/// here (`unknown_action`), the action is currently unavailable
/// (`action_unavailable`), its confirmation is missing or wrong
/// (`action_confirmation_required`), or the arguments do not satisfy its
/// parameters (`invalid_arguments`).
pub fn admit(
    provider: &str,
    resources: &[Resource],
    requested_provider: &str,
    id: &str,
    args: &Map<String, Value>,
) -> Result<Admitted, Refusal> {
    if requested_provider != provider {
        return Err(Refusal::unknown(id));
    }
    let address = parse_id(id).ok_or_else(|| Refusal::unknown(id))?;
    let spec = door::resolve(resources, &address).ok_or_else(|| Refusal::unknown(id))?;
    if let Availability::Unavailable { code, reason } = &spec.contract.availability {
        return Err(Refusal::from_door(&door::DoorError::Unavailable {
            code: code.clone(),
            reason: reason.clone(),
        }));
    }
    let mut own = args.clone();
    // Classify the call's own arguments, without the confirmation evidence.
    let mut bare = args.clone();
    bare.remove(CONFIRM_ARG);
    bare.remove(PHRASE_ARG);
    let policy = spec.confirmation_for_json(&bare);
    if policy == ConfirmationPolicy::None && spec.contract.confirmation_depends_on_arguments() {
        own.remove(CONFIRM_ARG);
    }
    let phrase = match &policy {
        ConfirmationPolicy::None => None,
        ConfirmationPolicy::Confirm => {
            if own.remove(CONFIRM_ARG) != Some(Value::Bool(true)) {
                return Err(Refusal {
                    code: "action_confirmation_required",
                    message: format!(
                        "`{id}` is consequential: pass \"{CONFIRM_ARG}\": true to confirm it"
                    ),
                });
            }
            None
        }
        ConfirmationPolicy::Phrase { phrase } => {
            let typed = own.remove(PHRASE_ARG);
            if typed.as_ref().and_then(Value::as_str) != Some(phrase.as_str()) {
                return Err(Refusal {
                    code: "action_confirmation_required",
                    message: format!(
                        "`{id}` needs its confirmation phrase typed exactly: pass \"{PHRASE_ARG}\": \"{phrase}\""
                    ),
                });
            }
            Some(phrase.clone())
        }
    };
    let encoded =
        door::encode_json(resources, &address, &own).map_err(|error| Refusal::from_door(&error))?;
    Ok(Admitted {
        label: spec.label.clone(),
        kind: spec.kind,
        address,
        encoded,
        phrase,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionSpec;

    const OP: &str = "test.operation";

    fn encode(value: &Value) -> EncodedAction {
        EncodedAction::encode_operation(OP, value).unwrap()
    }

    fn tree() -> Vec<Resource> {
        vec![
            Resource::new("session", "test.session", "Session")
                .with_action(ActionSpec::typed_operation(
                    "rename",
                    "Rename",
                    "name",
                    OperationKind::Effect,
                    OP,
                    vec![ActionParam::required("name", ParamType::Text, "New name")],
                    |args| encode(&json!({"rename": args.text("name")})),
                ))
                .with_action(
                    ActionSpec::operation("reset", "Reset", OperationKind::Effect, OP, &"reset")
                        .unwrap()
                        .unavailable("not_now", "Finish the current step first"),
                )
                .with_child(
                    Resource::new("disk:/dev/vdb", "test.disk", "vdb").with_action(
                        ActionSpec::operation(
                            "erase",
                            "Erase",
                            OperationKind::Effect,
                            OP,
                            &"erase",
                        )
                        .unwrap()
                        .destructive()
                        .with_contract(tilo_actions::ActionContract {
                            risk: tilo_actions::RiskLevel::Destructive,
                            confirmation: ConfirmationPolicy::Phrase {
                                phrase: "/dev/vdb".into(),
                            },
                            ..tilo_actions::ActionContract::default()
                        }),
                    ),
                )
                .with_child(
                    Resource::new("power", "test.power", "Power").with_action(
                        ActionSpec::operation(
                            "off",
                            "Power off",
                            OperationKind::Effect,
                            OP,
                            &"off",
                        )
                        .unwrap()
                        .privileged(),
                    ),
                ),
        ]
    }

    fn args(value: &Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn catalog_projects_actions_and_admits_only_exact_bindings() {
        // Phase `ids_round_trip_and_reject_non_ids`.
        {
            let address = ActionAddress::new("installer.disk:/dev/vdb", "select");
            assert_eq!(id_for(&address), "select@installer.disk:/dev/vdb");
            assert_eq!(parse_id(&id_for(&address)), Some(address));
            for bad in ["select", "@x", "x@", ""] {
                assert_eq!(parse_id(bad), None, "{bad}");
            }
        }
        // Phase `enumeration_projects_confirmation_and_availability_without_handles`.
        {
            let listed = enumerate("test", &tree(), "");
            assert_eq!(listed["count"], 4);
            let actions = listed["actions"].as_array().unwrap();
            let erase = actions.iter().find(|a| a["operation"] == "erase").unwrap();
            assert_eq!(erase["id"], "erase@disk:/dev/vdb");
            assert!(
                erase["params"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == PHRASE_ARG && p["required"] == true)
            );
            assert_eq!(erase["contract"]["confirmation"]["phrase"], "/dev/vdb");
            let off = actions.iter().find(|a| a["operation"] == "off").unwrap();
            assert!(
                off["params"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == CONFIRM_ARG)
            );
            let reset = actions.iter().find(|a| a["operation"] == "reset").unwrap();
            assert_eq!(reset["available"], false);
            assert_eq!(reset["unavailable"]["code"], "not_now");
            assert_eq!(enumerate("test", &tree(), "power OFF")["count"], 1);
            assert_eq!(enumerate("test", &tree(), "nothing-matches")["count"], 0);
        }
        // Phase `admission_binds_strictly_and_refuses_by_code`.
        {
            let tree = tree();
            let admitted = admit(
                "test",
                &tree,
                "test",
                "rename@session",
                &args(&json!({"name": "x"})),
            )
            .unwrap_or_else(|refusal| panic!("{refusal:?}"));
            assert_eq!(admitted.kind, OperationKind::Effect);
            assert_eq!(admitted.encoded.type_id, OP);
            assert!(admitted.phrase.is_none());

            let code = |provider: &str, id: &str, value: Value| {
                admit("test", &tree, provider, id, &args(&value))
                    .err()
                    .map(|r| r.code)
            };
            assert_eq!(
                code("other", "rename@session", json!({"name": "x"})),
                Some("unknown_action")
            );
            assert_eq!(
                code("test", "nope@session", json!({})),
                Some("unknown_action")
            );
            assert_eq!(code("test", "rename", json!({})), Some("unknown_action"));
            assert_eq!(
                code("test", "rename@session", json!({})),
                Some("invalid_arguments")
            );
            assert_eq!(
                code("test", "rename@session", json!({"name": 3})),
                Some("invalid_arguments")
            );
            assert_eq!(
                code("test", "rename@session", json!({"name": "x", "extra": 1})),
                Some("invalid_arguments")
            );
            assert_eq!(
                code("test", "reset@session", json!({})),
                Some("action_unavailable")
            );
        }
        // Phase `destructive_actions_need_their_exact_confirmation`.
        {
            let tree = tree();
            let refused = |value: Value| {
                admit("test", &tree, "test", "erase@disk:/dev/vdb", &args(&value))
                    .err()
                    .map(|r| r.code)
            };
            assert_eq!(refused(json!({})), Some("action_confirmation_required"));
            assert_eq!(
                refused(json!({PHRASE_ARG: "/dev/vda"})),
                Some("action_confirmation_required")
            );
            assert_eq!(
                refused(json!({CONFIRM_ARG: true})),
                Some("action_confirmation_required"),
                "a boolean cannot stand in for a typed phrase"
            );
            let admitted = admit(
                "test",
                &tree,
                "test",
                "erase@disk:/dev/vdb",
                &args(&json!({PHRASE_ARG: "/dev/vdb"})),
            )
            .unwrap_or_else(|refusal| panic!("{refusal:?}"));
            assert_eq!(admitted.phrase.as_deref(), Some("/dev/vdb"));

            let off = |value: Value| admit("test", &tree, "test", "off@power", &args(&value));
            assert_eq!(
                off(json!({})).err().map(|r| r.code),
                Some("action_confirmation_required")
            );
            assert_eq!(
                off(json!({CONFIRM_ARG: false})).err().map(|r| r.code),
                Some("action_confirmation_required")
            );
            assert!(off(json!({CONFIRM_ARG: true})).is_ok());
        }
    }
}
