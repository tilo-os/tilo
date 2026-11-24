//! The workspace's action registry, read and called by code: `tilo.actions`'s two verbs,
//! answered here at the dispatch they project.
//!
//! `catalogue` answers the same pages a `CatalogueRequest` answers, narrowed by a word and an
//! app. `call` turns an address (`<app id>/<resource id>#<action>`) into the target the
//! catalogue lists and invokes it as the running invocation's child, with the caller unchanged:
//! the person, or the attested app acting for them. Nothing is elevated; the target's own
//! policy, confirmation and refusal apply.

use std::sync::Arc;

use super::AppBrokerState;
use super::invocations::by;
use tilo_actions::remote::workspace_invocations::InvocationBy;

use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE,
    Pending, ReceiptStatus, RemoteActionTarget, pending,
};
use tilo_actions::tree::{Address, app_of};
use tilo_actions::{
    ActionKind, ActionOutput, ConfirmationPolicy, InvocationId, ParamType,
    remote::ACTIONS_PROVIDER, remote::workspace_invocations::Invocation as InvocationRecord,
    remote::workspace_invocations::InvocationStatus,
};

/// The provider a call of `tilo.actions`'s verbs names.
pub(super) const PROVIDER: &str = ACTIONS_PROVIDER;

impl AppBrokerState {
    /// Answer one call of `tilo.actions`: the catalogue it names, or the action at the address
    /// it names.
    pub(super) fn invoke_actions(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match request.target.address.action.as_str() {
            "catalogue" => self.actions_catalogue(caller, request),
            "call" => self.actions_call(caller, request),
            _ => Ok(Pending::ready(Err(ActionCallError::UnknownAction))),
        }
        .unwrap_or_else(|reason| Pending::ready(Err(ActionCallError::InvalidRequest { reason })))
    }

    /// The actions the caller is offered, as the catalogue lists them.
    fn actions_catalogue(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Pending<Result<InvocationReceipt, ActionCallError>>, String> {
        let arguments = arguments(request);
        let query = arguments
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let app = arguments
            .get("app")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let asked = tilo_actions::remote::CatalogueQuery {
            needle: query,
            max_results: MAX_CATALOGUE_PAGE,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        let invocation = request.invocation;
        Ok(self.action_query(caller, &asked).map(move |page| {
            let page = page?;
            let actions: Vec<serde_json::Value> = page
                .rows
                .into_iter()
                .filter(|row| app.as_ref().is_none_or(|app| row.app_id == *app))
                .filter_map(|row| {
                    let action = row.action?;
                    Some(serde_json::json!({
                        "address": format!(
                            "{}/{}#{}",
                            row.app_id, action.address.resource, action.address.action
                        ),
                        "app": row.app_id,
                        "app_label": app_label(&row.app_id),
                        "label": action.action_label,
                        "resource": action.resource_label,
                        "resource_kind": row.resource.kind,
                        "resource_id": row.resource.id,
                        "kind": if action.contract.kind == ActionKind::Read { "read" } else { "effect" },
                        "params": action.params.iter().map(|param| serde_json::json!({
                            "name": param.name,
                            "ty": param_type_name(param.ty),
                            "required": param.required,
                        })).collect::<Vec<_>>(),
                        "confirms": !matches!(action.contract.confirmation, ConfirmationPolicy::None),
                    }))
                })
                .collect();
            Ok(completed(invocation, serde_json::json!(actions)))
        }))
    }

    /// The action at the address the call names, invoked as this invocation's child.
    fn actions_call(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Pending<Result<InvocationReceipt, ActionCallError>>, String> {
        let arguments = arguments(request);
        let address = arguments
            .get("address")
            .and_then(serde_json::Value::as_str)
            .ok_or("`call` needs the action's address")?;
        let parsed = Address::parse(address).map_err(|why| format!("{address}: {why}"))?;
        let (app, resource, verb) = address_parts(&parsed)
            .ok_or_else(|| format!("{address}: write it as <app id>/<resource id>#<action>"))?;
        let given = arguments
            .get("arguments")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        let outer = request.invocation;
        let parent = request.on_behalf_of;
        let context = request.context.clone();
        let started_ms = now_ms();
        // The row whose target the address names: what the catalogue lists by the app, the
        // resource and the verb, and whose provider answers it.
        let asked = tilo_actions::remote::CatalogueQuery {
            needle: verb.clone(),
            max_results: MAX_CATALOGUE_PAGE,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        let state = Arc::clone(self);
        let caller = caller.clone();
        let who = by(&caller);
        let (completer, answer) = pending();
        self.action_query(&caller, &asked).then(move |page| {
            let target = match page {
                Some(Ok(page)) => target_of(&page.rows, &app, &resource, &verb),
                Some(Err(error)) => {
                    completer.complete(Err(error));
                    return;
                }
                None => None,
            };
            let Some(target) = target else {
                completer.complete(Err(ActionCallError::UnknownAction));
                return;
            };
            if target.provider == PROVIDER {
                completer.complete(Err(ActionCallError::InvalidRequest {
                    reason: "the registry's own verbs cannot be called through it".to_owned(),
                }));
                return;
            }
            let inner = InvokeRequest {
                invocation: InvocationId::new(),
                target,
                argument: InvokeArgument::Json(given.clone()),
                context: context.clone(),
                confirmation: None,
                revision: None,
                // A child of the action that ran the code, not of the registry's own verb.
                on_behalf_of: parent.or(Some(outer)),
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            state
                .action_invoke_with_origin(&caller, None, None, None, &inner)
                .then(move |result| {
                    let result = result.unwrap_or(Err(ActionCallError::Unavailable {
                        reason: "the call was not answered".to_owned(),
                    }));
                    completer.complete(Ok(answered(outer, started_ms, verb.clone(), who, result)));
                });
        });
        Ok(answer)
    }
}

/// The arguments of a call of the registry's verbs.
fn arguments(request: &InvokeRequest) -> serde_json::Map<String, serde_json::Value> {
    match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        InvokeArgument::None | InvokeArgument::Text(_) => serde_json::Map::new(),
    }
}

/// The app, the resource and the verb an address names: `<app id>/<resource id>#<action>`.
fn address_parts(parsed: &Address) -> Option<(String, String, String)> {
    let verb = parsed.action.clone()?;
    if parsed.segments.len() < 2 {
        return None;
    }
    let app = app_of(parsed);
    let resource = parsed
        .resource()
        .strip_prefix(&app)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|resource| !resource.is_empty())?
        .to_owned();
    Some((app, resource, verb))
}

/// The target the catalogue's `rows` name for the action `verb` on `resource` of `app`: the
/// provider that answers it and the address it answers at.
fn target_of(
    rows: &[tilo_actions::remote::CatalogueRow],
    app: &str,
    resource: &str,
    verb: &str,
) -> Option<RemoteActionTarget> {
    rows.iter()
        .filter_map(|row| {
            let descriptor = row.action.as_ref()?;
            (row.app_id == app
                && descriptor.address.action == verb
                && descriptor.address.resource == resource)
                .then(|| RemoteActionTarget {
                    provider: row.provider.clone(),
                    address: descriptor.address.clone(),
                })
        })
        .next()
}

/// The receipt answering `invocation` with `value`.
fn completed(invocation: InvocationId, value: serde_json::Value) -> InvocationReceipt {
    InvocationReceipt {
        invocation,
        status: ReceiptStatus::Completed {
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
    }
}

/// What the inner call came to, as the invocation record the caller reads. A call that waits
/// for its person's yes reads as waiting; a denied one is the refusal it is.
fn answered(
    invocation: InvocationId,
    started_ms: i64,
    action: String,
    who: InvocationBy,
    result: Result<InvocationReceipt, ActionCallError>,
) -> InvocationReceipt {
    let who = who;
    let record = |status: InvocationStatus, output: Option<serde_json::Value>| {
        let now = now_ms();
        InvocationRecord {
            id: Some(invocation.0.to_string()),
            calls: vec![invocation.0.to_string()],
            status,
            action: Some(action.clone()),
            output,
            started_ms,
            ended_ms: (!matches!(
                status,
                InvocationStatus::Waiting | InvocationStatus::Running
            ))
            .then_some(now),
            by: who.clone(),
            remembered: true,
            ..InvocationRecord::default()
        }
    };
    match result {
        Ok(receipt) => match receipt.status {
            ReceiptStatus::Completed { output } => completed(
                invocation,
                serde_json::to_value(record(InvocationStatus::Ended, output.value))
                    .unwrap_or_default(),
            ),
            ReceiptStatus::Accepted => completed(
                invocation,
                serde_json::to_value(record(InvocationStatus::Running, None)).unwrap_or_default(),
            ),
            ReceiptStatus::Failed { error } => {
                let mut held = record(InvocationStatus::Failed, None);
                held.failure = Some(error.code.clone());
                completed(invocation, serde_json::to_value(held).unwrap_or_default())
            }
            ReceiptStatus::Cancelled { .. } => completed(
                invocation,
                serde_json::to_value(record(InvocationStatus::Cancelled, None)).unwrap_or_default(),
            ),
            ReceiptStatus::Rejected { error } => {
                if matches!(error, ActionCallError::ConfirmationRequired(_)) {
                    completed(
                        invocation,
                        serde_json::to_value(record(InvocationStatus::Waiting, None))
                            .unwrap_or_default(),
                    )
                } else {
                    InvocationReceipt {
                        invocation,
                        status: ReceiptStatus::Rejected { error },
                    }
                }
            }
        },
        Err(ActionCallError::ConfirmationRequired(_)) => completed(
            invocation,
            serde_json::to_value(record(InvocationStatus::Waiting, None)).unwrap_or_default(),
        ),
        Err(error) if error.awaits_person() => completed(
            invocation,
            serde_json::to_value(record(InvocationStatus::Waiting, None)).unwrap_or_default(),
        ),
        Err(error) => InvocationReceipt {
            invocation,
            status: ReceiptStatus::Rejected { error },
        },
    }
}

/// What an action's parameter type is called in the catalogue.
fn param_type_name(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Text | ParamType::Secret | ParamType::DateTime => "text",
        ParamType::Integer => "int",
        ParamType::Number => "float",
        ParamType::Boolean => "bool",
        ParamType::Resource => "resource",
        ParamType::Object => "object",
        ParamType::Array => "list",
    }
}

/// An app's name as a person reads it: its last id segment, in sentence case.
fn app_label(app: &str) -> String {
    tilo_actions::field_type::label_of(app.rsplit('.').next().unwrap_or(app))
}

/// Now, in milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use tilo_actions::remote::workspace_invocations::InvocationBy;

    /// An address names the app, the resource and the verb, and nothing names no resource.
    #[test]
    fn an_address_is_the_app_the_resource_and_the_verb() {
        let (app, resource, verb) =
            address_parts(&Address::parse("so.tilo.notes/notes.note:1#write").unwrap()).unwrap();
        assert_eq!(app, "so.tilo.notes");
        assert_eq!(resource, "notes.note:1");
        assert_eq!(verb, "write");
        // A resource nested in another is still the resource.
        let (_, resource, _) =
            address_parts(&Address::parse("so.tilo.notes/nb1/pages/p2#write").unwrap()).unwrap();
        assert_eq!(resource, "nb1/pages/p2");
        assert!(
            address_parts(&Address::parse("so.tilo.notes#write").unwrap()).is_none(),
            "a call names the resource it acts on"
        );
        assert!(address_parts(&Address::parse("so.tilo.notes/notes.note:1").unwrap()).is_none());
    }

    /// A call kept as its asked record reads as the invocation that waits; a denied one is the
    /// refusal it is.
    #[test]
    fn a_waiting_call_reads_as_waiting_and_a_denied_one_is_refused() {
        let who = InvocationBy::default();
        let asked = answered(
            InvocationId::new(),
            1,
            "notes.note:1#write".to_owned(),
            who.clone(),
            Ok(InvocationReceipt {
                invocation: InvocationId::new(),
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::ConfirmationRequired(Box::default()),
                },
            }),
        );
        let ReceiptStatus::Completed { output } = asked.status else {
            panic!("a waiting call answers its invocation: {asked:?}");
        };
        assert_eq!(output.value.unwrap()["status"], "waiting");

        let denied = answered(
            InvocationId::new(),
            1,
            "notes.note:1#write".to_owned(),
            who,
            Ok(InvocationReceipt {
                invocation: InvocationId::new(),
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::NotPermitted,
                },
            }),
        );
        assert!(matches!(denied.status, ReceiptStatus::Rejected { .. }));
    }
}
