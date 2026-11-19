//! An action the caller's directory does not list, called by its name: the workspace decides
//! it with exactly what the caller sent, and it is never done with arguments nobody gave.
use super::*;

/// Notes with one more action on each of its resources that the directory never lists to the agent: the
/// workspace takes it when `permitted`, else refuses it as not the caller's. What every call
/// of it carried is kept.
#[derive(Clone)]
struct Unlisted {
    notes: Notes,
    permitted: bool,
    sent: Arc<Mutex<Vec<Sent>>>,
}

/// One call of the unlisted action: its invocation, and the arguments it carried.
type Sent = (tilo_actions::InvocationId, serde_json::Map<String, Value>);

/// The action the directory never lists.
const UNLISTED: &str = "pin";

impl ActionProvider for Unlisted {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.notes.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let address = &request.target.address;
        if address.action != UNLISTED {
            return self.notes.invoke(caller, request);
        }
        let known = self
            .notes
            .rows(&request.target.provider)
            .iter()
            .any(|row| row.resource.id == address.resource);
        if !known {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        if !self.permitted {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(map) => map.clone(),
            _ => serde_json::Map::new(),
        };
        let invocation = request.invocation;
        self.sent.lock().unwrap().push((invocation, arguments));
        Pending::ready(Ok(pinned(invocation).receipt()))
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        if self.sent.lock().unwrap().iter().any(|(sent, _)| sent == id) {
            return Some(pinned(*id));
        }
        self.notes.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.notes.cancel(caller, id)
    }
}

/// How a call of the unlisted action ends: done.
fn pinned(invocation: tilo_actions::InvocationId) -> InvocationState {
    InvocationState::new(
        invocation,
        tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput {
                value: Some(json!({"pinned": true})),
                ..tilo_actions::ActionOutput::default()
            },
        },
    )
}

/// An action the directory does not list, called by name, is done once with what the caller
/// sent, or is refused as not theirs; never with arguments nobody gave, and a name the
/// workspace knows nowhere is told where actions are listed.
#[tokio::test]
async fn an_unlisted_action_takes_the_callers_arguments_or_is_refused() {
    let allowed = Unlisted {
        notes: Notes::new(true),
        permitted: true,
        sent: Arc::default(),
    };
    let (text, _) = tool(&agent(&allowed), "tilo_ls", json!({"path": "notes/Todo"})).await;
    assert!(!text.contains(UNLISTED), "never listed: {text}");
    let call = json!({"path": "notes/Todo#pin", "args": {"until": "Friday"}});
    let (text, failed) = tool(&agent(&allowed), "tilo_do", call).await;
    assert!(!failed && text.contains("pinned"), "{text}");
    let sent = allowed.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "done once: {sent:?}");
    assert_eq!(sent[0].1.get("until"), Some(&json!("Friday")), "{sent:?}");
    assert_eq!(
        sent[0].1.len(),
        1,
        "with what the caller sent alone: {sent:?}"
    );

    // Named on the app, it is done on the first of the app's resources that takes it,
    // never on each of them.
    let call = json!({"path": "notes#pin", "args": {"until": "Monday"}});
    let (text, failed) = tool(&agent(&allowed), "tilo_do", call).await;
    assert!(!failed, "{text}");
    let sent = allowed.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "done once more: {sent:?}");
    assert_eq!(sent[1].1.get("until"), Some(&json!("Monday")), "{sent:?}");

    let refused = Unlisted {
        notes: Notes::new(true),
        permitted: false,
        sent: Arc::default(),
    };
    let call = json!({"path": "notes/Todo#pin", "args": {"until": "Friday"}});
    let (text, failed) = tool(&agent(&refused), "tilo_do", call).await;
    assert!(failed && text.starts_with("not_permitted: "), "{text}");
    assert!(refused.sent.lock().unwrap().is_empty(), "nothing was done");

    // A name the workspace has nowhere is the caller's to name better, and nothing is done.
    let call = json!({"path": "notes/Todo#explode", "args": {"now": true}});
    let (text, failed) = tool(&agent(&allowed), "tilo_do", call).await;
    assert!(
        failed
            && text.contains("has no action explode")
            && text.contains("tilo_ls notes/Todo lists them"),
        "{text}"
    );
    assert!(
        allowed.notes.invoked().is_empty(),
        "nothing was done: {text}"
    );
}
