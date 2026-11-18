//! An action the caller's directory does not list, called by its name: the workspace decides
//! it with exactly what the caller sent, and it is never done with arguments nobody gave, nor
//! on a resource the caller did not name.
use super::*;

/// Notes whose notes named in `on` publish one more action that no listing shows the agent:
/// the workspace takes it when `permitted`, else refuses it as not the caller's, and answers
/// which notes publish it when the action is named by the app alone. What every call of it
/// carried is kept.
#[derive(Clone)]
struct Unlisted {
    notes: Notes,
    on: &'static [&'static str],
    permitted: bool,
    sent: Arc<Mutex<Vec<Sent>>>,
}

/// One call of the unlisted action: its invocation, the note it was on, and the arguments it
/// carried.
type Sent = (
    tilo_actions::InvocationId,
    String,
    serde_json::Map<String, Value>,
);

/// The action no listing shows.
const UNLISTED: &str = "pin";

impl Unlisted {
    fn new(on: &'static [&'static str], permitted: bool) -> Self {
        Self {
            notes: Notes::new(true),
            on,
            permitted,
            sent: Arc::default(),
        }
    }

    /// The rows of the notes that publish the action, one each, as a lookup by name answers.
    fn publishing(&self, provider: &str) -> Vec<CatalogueRow> {
        let mut rows: Vec<CatalogueRow> = Vec::new();
        for mut row in self.notes.rows(provider) {
            let publishes = row.resource.kind == "notes.page"
                && self.on.contains(&row.resource.label.as_str())
                && !rows.iter().any(|seen| seen.resource.id == row.resource.id);
            let Some(action) = row.action.as_mut().filter(|_| publishes) else {
                continue;
            };
            action.address.action = UNLISTED.into();
            if !self.permitted {
                action.contract.availability = Availability::not_allowed(None);
            }
            rows.push(row);
        }
        rows
    }

    /// The labels of the notes the action was done on, in order.
    fn done_on(&self) -> Vec<String> {
        let sent = self.sent.lock().unwrap();
        sent.iter().map(|(_, label, _)| label.clone()).collect()
    }
}

impl ActionProvider for Unlisted {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if query.action.as_deref() == Some(UNLISTED) {
            let provider = query.provider.unwrap_or_default();
            return Pending::ready(Ok(CataloguePage {
                rows: self.publishing(&provider),
                exhausted: true,
                ..CataloguePage::default()
            }));
        }
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
        let Some(note) = self
            .publishing(&request.target.provider)
            .into_iter()
            .find(|row| row.resource.id == address.resource)
        else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        if !self.permitted {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(map) => map.clone(),
            _ => serde_json::Map::new(),
        };
        let invocation = request.invocation;
        self.sent
            .lock()
            .unwrap()
            .push((invocation, note.resource.label, arguments));
        Pending::ready(Ok(pinned(invocation).receipt()))
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        if self
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|(sent, ..)| sent == id)
        {
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

/// An action no listing shows, called by name, is done once with what the caller sent, or is
/// refused as not theirs; never with arguments nobody gave. Named by the app alone, it is done
/// only where one note publishes it: with several, nothing is done and the caller is told how
/// to name each. A name the workspace knows nowhere is told where actions are listed.
#[tokio::test]
async fn an_unlisted_action_takes_the_callers_arguments_or_is_refused() {
    let one = Unlisted::new(&["Todo"], true);
    let (text, _) = tool(&agent(&one), "tilo_ls", json!({"path": "notes/Todo"})).await;
    assert!(!text.contains(UNLISTED), "never listed: {text}");
    let call = json!({"path": "notes/Todo#pin", "args": {"until": "Friday"}});
    let (text, failed) = tool(&agent(&one), "tilo_do", call).await;
    assert!(!failed && text.contains("pinned"), "{text}");
    let sent = one.sent.lock().unwrap().clone();
    let only_friday = serde_json::Map::from_iter([("until".to_owned(), json!("Friday"))]);
    assert_eq!(sent.len(), 1, "done once: {sent:?}");
    assert_eq!(sent[0].2, only_friday, "with what the caller sent alone");
    // Named by the app alone, it is done on the one note that publishes it.
    let call = json!({"path": "notes#pin", "args": {"until": "Monday"}});
    let (text, failed) = tool(&agent(&one), "tilo_do", call).await;
    assert!(!failed, "{text}");
    assert_eq!(one.done_on(), ["Todo", "Todo"]);

    // Two notes publish it: the app alone names neither, and nothing is done.
    let two = Unlisted::new(&["Todo", "Groceries"], true);
    let call = json!({"path": "notes#pin", "args": {"until": "Monday"}});
    let (text, failed) = tool(&agent(&two), "tilo_do", call).await;
    assert!(
        failed
            && text.contains("name one: ")
            && text.contains("notes/Todo#pin")
            && text.contains("notes/Groceries#pin"),
        "{text}"
    );
    assert!(two.done_on().is_empty(), "nothing was done: {text}");
    let call = json!({"path": "notes/Groceries#pin", "args": {"until": "Monday"}});
    let (text, failed) = tool(&agent(&two), "tilo_do", call).await;
    assert!(!failed, "{text}");
    assert_eq!(two.done_on(), ["Groceries"]);

    let refused = Unlisted::new(&["Todo"], false);
    for path in ["notes/Todo#pin", "notes#pin"] {
        let call = json!({"path": path, "args": {"until": "Friday"}});
        let (text, failed) = tool(&agent(&refused), "tilo_do", call).await;
        assert!(
            failed && text.starts_with("not_permitted: "),
            "{path}: {text}"
        );
    }
    assert!(refused.done_on().is_empty(), "nothing was done");

    // A name the workspace has nowhere is the caller's to name better, and nothing is done.
    let call = json!({"path": "notes/Todo#explode", "args": {"now": true}});
    let (text, failed) = tool(&agent(&one), "tilo_do", call).await;
    assert!(
        failed
            && text.contains("has no action explode")
            && text.contains("tilo_ls notes/Todo lists them"),
        "{text}"
    );
    assert!(one.notes.invoked().is_empty(), "nothing was done: {text}");
}
