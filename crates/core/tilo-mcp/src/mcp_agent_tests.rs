//! The agent surface against a Notes shaped like the live one: a notebook
//! whose pages are resources, each page's body published beside it under a
//! bound id that only the app's own key reads (`notes.notebook/page/<id>/body`).
use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, CatalogueResource,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{
    ActionParam, Availability, InvocationUpdate, ParamType, ResourceSemanticIdentity,
};

const APP: &str = "so.tilo.notes";
const WORKSPACE: &str = "workspace:workspace";
/// The workspace itself, as the provider above names it.
const WORKSPACE_ID: &str = "workspace";
const NOTEBOOK: &str = "notes.notebook";
const TODO: &str = "notes.page:3fa2c1d0-1111-4222-8333-444455556666";
const SHOP: &str = "notes.page:9b8e7d6c-1111-4222-8333-777788889999";
const TODO_BODY: &str = "notes.notebook/page/3fa2c1d0-1111-4222-8333-444455556666/body";
const SHOP_BODY: &str = "notes.notebook/page/9b8e7d6c-1111-4222-8333-777788889999/body";

/// The id a broker mints for a bound resource: per publication, so it
/// differs after every restart; only the hex tail is the app's own name.
fn bound(publication: &str, key: &str) -> String {
    format!(
        "tilo-ui-bound:v1:{publication}:c0fe4ad7-b7d0-46c3-ab7e-78d12d5d86f4:{}",
        hex::encode(key)
    )
}

#[derive(Clone)]
pub(super) struct Notes {
    /// Whether the app runs; a launch from the workspace sets it.
    running: Arc<Mutex<bool>>,
    /// Which publication the bound ids belong to: a restart changes it.
    publication: &'static str,
    pub(super) invoked: Arc<Mutex<Vec<InvokeRequest>>>,
    /// The surface the installed Notes publishes when set: the actions its package declares,
    /// as the workspace's listing carries them.
    surface: Arc<Mutex<Option<String>>>,
    /// The steps each note's `history` verb answers, newest last.
    history: Arc<Mutex<Vec<Value>>>,
    /// Who is in the workspace, as this host's roster names them; `None` for
    /// a host that reads no roster.
    here: Arc<Mutex<Option<Vec<tilo_actions::remote::Here>>>>,
    /// Whether this host puts a note's deletion to the person before it runs,
    /// as a workspace whose grants ask does.
    asks: Arc<Mutex<bool>>,
    /// The action refused to the agent, and how the workspace tells it so.
    refuses: Arc<Mutex<Option<(&'static str, Availability)>>>,
    /// An action its person is asked to allow, by its name, and when they answer: until
    /// then the app says that its person is asked, and nothing of it has happened.
    person_asked: Arc<Mutex<Option<(&'static str, tokio::time::Instant)>>>,
    /// Whether this host keeps a search of its own; one that holds no records to search
    /// keeps none.
    searches: bool,
    /// The invocations stopped through their record.
    cancelled: Arc<Mutex<std::collections::HashSet<tilo_actions::InvocationId>>>,
}

/// The asked record the test app says a call waits in, as a workspace's dispatch names it.
pub(super) const ASKED_RECORD: &str = "workspace/invocations/7";


impl Notes {
    pub(super) fn new(running: bool) -> Self {
        Self {
            running: Arc::new(Mutex::new(running)),
            publication: "2a4120d7-2f63-4578-aed8-11cb91ed3628",
            invoked: Arc::default(),
            surface: Arc::default(),
            history: Arc::default(),
            here: Arc::new(Mutex::new(Some(vec![tilo_actions::remote::Here {
                who: "ada".into(),
                name: "Ada".into(),
                kind: "person".into(),
                works_in: None,
                devices: Vec::new(),
            }]))),
            asks: Arc::default(),
            refuses: Arc::default(),
            person_asked: Arc::default(),
            searches: true,
            cancelled: Arc::default(),
        }
    }

    /// The same Notes, keeping no search of its own.
    fn without_search(mut self) -> Self {
        self.searches = false;
        self
    }

    /// The same Notes, its action `action` refused to the agent as `availability` says.
    fn refusing(self, action: &'static str, availability: Availability) -> Self {
        *self.refuses.lock().unwrap() = Some((action, availability));
        self
    }

    /// The same host, putting a note's deletion to the person first.
    fn asking(self) -> Self {
        *self.asks.lock().unwrap() = true;
        self
    }

    /// One new step in every note's history: `entry` as the history verb
    /// answers it.
    pub(super) fn changed(&self, entry: Value) {
        self.history.lock().unwrap().push(entry);
    }

    /// Who is in the workspace now.
    pub(super) fn people(&self, here: Option<Vec<tilo_actions::remote::Here>>) {
        *self.here.lock().unwrap() = here;
    }

    /// The same Notes installed, its package declaring `actions`.
    fn declaring(self, actions: &[&str]) -> Self {
        *self.surface.lock().unwrap() = Some(
            json!({"resources": [{
                "kind": "notes.notebook",
                "label": "Notes",
                "version": 1,
                "shows": actions,
            }]})
            .to_string(),
        );
        self
    }

    /// Every note: its key, its body's key and its title.
    fn notes() -> Vec<(String, String, String)> {
        vec![
            (TODO.to_owned(), TODO_BODY.to_owned(), "Todo".to_owned()),
            (
                SHOP.to_owned(),
                SHOP_BODY.to_owned(),
                "Groceries".to_owned(),
            ),
        ]
    }

    fn id(&self, key: &str) -> String {
        if key == NOTEBOOK {
            key.to_owned()
        } else {
            bound(self.publication, key)
        }
    }

    fn invoked(&self) -> Vec<(String, String, Value)> {
        self.invoked
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                let arguments = match &request.argument {
                    InvokeArgument::Json(map) => Value::Object(map.clone()),
                    _ => Value::Null,
                };
                (
                    request.target.address.resource.clone(),
                    request.target.address.action.clone(),
                    arguments,
                )
            })
            .collect()
    }

    /// A resource row (no action) or an action row of `key`.
    fn row(
        &self,
        parent: &str,
        key: &str,
        kind: &str,
        label: &str,
        action: Option<(&str, &str, Vec<ActionParam>, tilo_actions::OperationKind)>,
        has_children: bool,
    ) -> CatalogueRow {
        let id = self.id(key);
        CatalogueRow {
            offered: None,
            provider: "1".into(),
            app_id: APP.into(),
            parent_resource: if parent.is_empty() {
                String::new()
            } else {
                self.id(parent)
            },
            resource: CatalogueResource {
                id: id.clone(),
                kind: kind.into(),
                label: label.into(),
                presentation: None,
            },
            action: action.map(
                |(name, title, params, operation)| tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: (key != NOTEBOOK)
                        .then(|| ResourceSemanticIdentity::new(kind, key))
                        .flatten(),
                    address: tilo_actions::ActionAddress::new(id.clone(), name),
                    resource_kind: kind.into(),
                    resource_label: label.into(),
                    action_label: title.into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: Some(format!("{title}. Longer explanation that is not listed.")),
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: operation,
                    contract: tilo_actions::ActionContract {
                        availability: match &*self.refuses.lock().unwrap() {
                            Some((refused, availability)) if *refused == name => {
                                availability.clone()
                            }
                            _ => tilo_actions::Availability::Available,
                        },
                        ..tilo_actions::ActionContract::default()
                    },
                    input: tilo_actions::InputSpec::None,
                    params,
                    view: None,
                },
            ),
            has_children,
            visibility: None,
            surface: None,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture's whole catalogue, one provider per arm, reads as data"
    )]
    fn rows(&self, provider: &str) -> Vec<CatalogueRow> {
        use tilo_actions::OperationKind::{Document, Effect, View};
        if provider == WORKSPACE {
            let open = |app: &str, label: &str| CatalogueRow {
                offered: None,
                provider: WORKSPACE.into(),
                app_id: "so.tilo.desktop".into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: format!("app.{app}"),
                    kind: "applications.app".into(),
                    label: label.into(),
                    presentation: None,
                },
                action: Some(tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new(format!("app.{app}"), "open"),
                    resource_kind: "applications.app".into(),
                    resource_label: label.into(),
                    action_label: "Open".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: Effect,
                    contract: tilo_actions::ActionContract::default(),
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                }),
                has_children: false,
                visibility: None,
                surface: None,
            };
            let mut rows = vec![open(APP, "Notes"), open("so.tilo.sheets", "Sheets")];
            // An installed app's row carries the surface its package publishes.
            let declaring = self.surface.lock().unwrap().clone();
            if let Some(surface) = declaring {
                for row in &mut rows {
                    if row.resource.id == format!("app.{APP}") {
                        row.surface = Some(surface.clone());
                    }
                }
            }
            return rows;
        }
        let text = |name: &'static str, required: bool| {
            if required {
                ActionParam::required(name, ParamType::Text, "Text")
            } else {
                ActionParam::optional(name, ParamType::Text, "Text")
            }
        };
        let notes = Self::notes();
        let mut rows = vec![self.row("", NOTEBOOK, "notes.notebook", "Notebook", None, true)];
        rows.extend([
            self.row(
                NOTEBOOK,
                NOTEBOOK,
                "notes.notebook",
                "Notebook",
                Some(("new-page", "New note", vec![text("title", false)], Effect)),
                false,
            ),
            self.row(
                NOTEBOOK,
                NOTEBOOK,
                "notes.notebook",
                "Notebook",
                Some(("read", "Read", vec![text("item", false)], View)),
                false,
            ),
        ]);
        for (page, _, label) in &notes {
            rows.push(self.row(NOTEBOOK, page, "notes.page", label, None, true));
        }
        // A note's body is published under its note, as the live Notes
        // publishes it: a field's row names its note as its parent, and the
        // search names a row about the body by the note's title.
        for (page, body, label) in &notes {
            rows.push(self.row(page, body, "tilo.rich-text", label, None, false));
        }
        for (page, _, label) in &notes {
            rows.push(self.row(
                page,
                page,
                "notes.page",
                label,
                Some((
                    "rename",
                    "Rename note",
                    vec![ActionParam::required(
                        "title",
                        ParamType::Text,
                        "The note's new title",
                    )],
                    Document,
                )),
                false,
            ));
            let mut delete = self.row(
                page,
                page,
                "notes.page",
                label,
                Some(("delete", "Delete note", Vec::new(), Document)),
                false,
            );
            if let Some(action) = delete.action.as_mut() {
                action.contract.risk = tilo_actions::RiskLevel::Destructive;
            }
            rows.push(delete);
            rows.push(self.row(
                page,
                page,
                "notes.page",
                label,
                Some(("slow", "Slow thing", Vec::new(), Effect)),
                false,
            ));
            rows.push(self.row(
                page,
                page,
                "notes.page",
                label,
                Some(("history", "History", Vec::new(), View)),
                false,
            ));
        }
        for (_, body, label) in &notes {
            rows.push(self.row(
                body,
                body,
                "tilo.rich-text",
                label,
                Some((
                    "append-paragraph",
                    "Append paragraph",
                    vec![text("text", true)],
                    Document,
                )),
                false,
            ));
            rows.push(self.row(
                body,
                body,
                "tilo.rich-text",
                label,
                Some((
                    "set-block-done",
                    "Tick or untick a checklist item",
                    vec![text("block", true), text("done", false)],
                    Document,
                )),
                false,
            ));
            rows.push(self.row(
                body,
                body,
                "tilo.rich-text",
                label,
                Some((
                    "insert-block",
                    "Insert paragraph",
                    vec![text("after", true), text("text", false)],
                    Document,
                )),
                false,
            ));
        }
        rows
    }
}

fn host_matches(row: &CatalogueRow, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    let mut fields = vec![row.resource.id.clone(), row.resource.label.clone()];
    if let Some(action) = &row.action {
        fields.push(action.address.action.clone());
        fields.push(action.action_label.clone());
        fields.extend(action.params.iter().map(|param| param.name.to_string()));
    }
    fields
        .iter()
        .any(|field| field.to_lowercase().contains(&needle))
}

impl Notes {
    /// What the session answers `query`: the instances that run, or one provider's rows the
    /// query names, by action, target, name, parent or needle.
    fn answer(&self, query: &CatalogueQuery) -> Pending<Result<CataloguePage, ActionCallError>> {
        let running = *self.running.lock().unwrap();
        let Some(provider) = query.provider.clone() else {
            return Pending::ready(Ok(CataloguePage {
                revision: "v1".into(),
                providers: running
                    .then(|| CatalogueProvider {
                        provider: "1".into(),
                        app_id: APP.into(),
                        workspace: "workspace".into(),
                        document_id: String::new(),
                        browsable: true,
                    })
                    .into_iter()
                    .collect(),
                ..CataloguePage::default()
            }));
        };
        if provider != WORKSPACE && !running {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let mut rows = self.rows(&provider);
        for row in &mut rows {
            row.provider.clone_from(&provider);
        }
        // One only the person at the device takes is told of when named, never listed.
        let shown = |row: &CatalogueRow| {
            row.action.as_ref().is_some_and(|a| {
                matches!(a.contract.availability.refusal(), Some(("private_view", _)))
            })
        };
        rows.retain(|row| query.target.is_some() || query.action.is_some() || !shown(row));
        if let Some(id) = &query.action {
            rows.retain(|row| row.action.as_ref().is_some_and(|a| a.address.action == *id));
        } else if let Some(target) = &query.target {
            rows.retain(|row| row.action.as_ref().is_some_and(|a| a.address == *target));
        } else if let Some(name) = &query.resource_name {
            // A name is answered whole, among the parent's children: an app answers with the
            // resources so named, the workspace's catalogue with their rows, actions too.
            let parent = query.parent_resource.clone().unwrap_or_default();
            rows.retain(|row| {
                row.parent_resource == parent
                    && (provider == WORKSPACE || row.action.is_none())
                    && (row.resource.kind == *name || row.resource.label.eq_ignore_ascii_case(name))
            });
        } else if query.needle.is_empty() {
            let parent = query.parent_resource.clone().unwrap_or_default();
            rows.retain(|row| row.parent_resource == parent);
        } else {
            rows.retain(|row| host_matches(row, &query.needle));
        }
        Pending::ready(Ok(CataloguePage {
            revision: "v1".into(),
            rows,
            exhausted: true,
            ..CataloguePage::default()
        }))
    }
}

impl ActionProvider for Notes {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // A listing taken a resource at a time is answered as the workspace answers it: a
        // resource's actions as one row, each action described once a page.
        if query.by_resource {
            return self.answer(&query).map(|page| {
                page.map(|mut page| {
                    page.rows = tilo_actions::remote::compact(page.rows);
                    page
                })
            });
        }
        self.answer(&query)
    }

    fn here(&self, _caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.here.lock().unwrap().clone()
    }

    fn resource_search(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ResourceSearch>> {
        let search: Arc<dyn tilo_actions::remote::ResourceSearch> = Arc::new(self.clone());
        self.searches.then_some(search)
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        // The invocations record takes back what one of them changed.
        if request.target.provider == tilo_actions::remote::workspace_invocations::PROVIDER {
            self.invoked.lock().unwrap().push(request);
            return Pending::ready(Ok(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }));
        }
        // The workspace answers an application's own action: an app's window is opened on its
        // `application:<workspace>:<app>` token as on the workspace itself, which is where a
        // view that is not up asks for it.
        let workspace = request.target.provider == WORKSPACE
            || tilo_actions::remote::application_provider(&request.target.provider)
                .is_some_and(|(workspace, _)| workspace == WORKSPACE_ID);
        let (address, rows) = (
            &request.target.address,
            self.rows(if workspace {
                WORKSPACE
            } else {
                &request.target.provider
            }),
        );
        // An app takes its steps back on its own package resource. As a workspace answers, an
        // action the resource does not publish is no such action.
        let offered = address.resource == format!("tl:{APP}")
            || rows.iter().any(|row| {
                row.resource.id == address.resource
                    && (workspace
                        || row.action.as_ref().map(|it| &it.address.action)
                            == Some(&address.action))
            });
        if !offered || (!workspace && !*self.running.lock().unwrap()) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        // What the workspace's grants put to the person first: nothing runs
        // until they allow it.
        if *self.asks.lock().unwrap() && request.target.address.action == "delete" {
            return Pending::ready(Err(ActionCallError::confirmation_required()));
        }
        if workspace {
            *self.running.lock().unwrap() = true;
        }
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationState::new(
            invocation,
            InvocationUpdate::Accepted { invocation },
        )
        .receipt()))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "what each of the test app's actions answers, in one match"
    )]
    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let (action, arguments) = self
            .invoked
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.invocation == *id)
            .map(|request| {
                let arguments = match &request.argument {
                    InvokeArgument::Json(map) => map.clone(),
                    _ => serde_json::Map::new(),
                };
                (request.target.address.action.clone(), arguments)
            })?;
        let failed = |code: &str, message: &str| InvocationUpdate::Failed {
            invocation: *id,
            error: tilo_actions::ActionFailure {
                code: code.into(),
                message: message.into(),
                details: None,
                retryable: false,
            },
        };
        let succeeded = |value: Option<Value>| InvocationUpdate::Succeeded {
            invocation: *id,
            output: tilo_actions::ActionOutput {
                value,
                ..tilo_actions::ActionOutput::default()
            },
        };
        let person_asked = self
            .person_asked
            .lock()
            .unwrap()
            .is_some_and(|(asked, answers)| {
                asked == action.as_str() && tokio::time::Instant::now() < answers
            });
        let update = match action.as_str() {
            _ if self.cancelled.lock().unwrap().contains(id) => InvocationUpdate::Cancelled {
                invocation: *id,
                message: None,
            },
            _ if person_asked => InvocationUpdate::Progress {
                invocation: *id,
                sequence: 1,
                phase: tilo_actions::remote::asked::ASKED_PHASE.to_owned(),
                completed: None,
                total: None,
                message: Some(ASKED_RECORD.to_owned()),
            },
            "slow" => InvocationUpdate::Accepted { invocation: *id },
            // The workspace's start of Notes names the instance it started.
            "start" => succeeded(Some(json!({"peer_id": 1}))),
            "rename" if !arguments.contains_key("title") => {
                failed("invalid_arguments", "argument `title` is required")
            }
            "read" => match arguments.get("item").and_then(Value::as_str) {
                None => succeeded(Some(json!({
                    "kind": "notes.notebook",
                    "pages": Self::notes()
                        .iter()
                        .map(|(page, _, title)| json!({"item": page, "title": title, "parent": null}))
                        .collect::<Vec<_>>(),
                    "tags": [],
                }))),
                Some(TODO) => succeeded(Some(todo_page())),
                Some(other) => failed("not_found", &format!("no note `{other}`")),
            },
            "append-paragraph" => succeeded(Some(json!({"block": "b9", "note": null}))),
            "history" => succeeded(Some(json!({
                "history": self.history.lock().unwrap().clone(),
            }))),
            _ => succeeded(None),
        };
        Some(InvocationState::new(*id, update))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        self.cancelled.lock().unwrap().insert(*id);
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// What Notes answers reading the Todo note: its Markdown with a `^id` on
/// each block, and the blocks.
fn todo_page() -> Value {
    json!({
        "kind": "notes.page",
        "item": TODO,
        "title": "Todo",
        "parent": null,
        "body": TODO_BODY,
        "markdown": "- [ ] milk ^b1\n- [x] bread ^b2\ncall mom ^b3\n## Later ^b4",
        "blocks": [
            {"id": "b1", "kind": {"checklist": 0, "done": false}, "text": "milk"},
            {"id": "b2", "kind": {"checklist": 0, "done": true}, "text": "bread"},
            {"id": "b3", "kind": "paragraph", "text": "call mom"},
            {"id": "b4", "kind": {"heading": 2}, "text": "Later"},
        ],
    })
}

pub(super) fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// What a tool call answered, as the agent reads it: the text, and whether
/// the call is an error.
async fn tool<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    name: &str,
    arguments: Value,
) -> (String, bool) {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .await
        .unwrap();
    let result = &reply["result"];
    // Typed only when a host's refusal decided it: its code beside the words.
    let typed = result.get("structuredContent");
    assert!(
        typed.is_none_or(|typed| typed.pointer("/error/code").is_some()),
        "{reply}"
    );
    (
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {reply}"))
            .to_owned(),
        result["isError"] == true,
    )
}

pub(super) fn agent<H>(host: &H) -> McpServer<H>
where
    H: ActionProvider + Clone + Send + Sync + 'static,
{
    McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada")
        .with_agent_session("mcp:test")
}


impl tilo_actions::remote::ResourceSearch for Notes {
    /// A stand-in for the desktop's one search: the rows the app publishes
    /// whose names hold the query's words, best first, each as the resource
    /// it is.
    fn find(
        &self,
        _caller: &Caller,
        needle: &str,
    ) -> Pending<Result<Vec<tilo_actions::remote::Found>, ActionCallError>> {
        let words = needle_words(needle);
        if words.is_empty() {
            return Pending::ready(Ok(Vec::new()));
        }
        let mut found = Vec::new();
        let rows = self.rows("1");
        // A row about what another resource holds is about that resource:
        // the live index lists the notebook's rows, and never both a note
        // and the body inside it.
        // A row about what another resource holds (a body published beside
        // its note) is about that resource: the live index lists the
        // notebook's rows, and never both a note and the body inside it. An
        // action is about the resource it acts on, which is the note itself.
        let owned = |row: &CatalogueRow| {
            row.action.is_none()
                && row.resource.kind == "tilo.rich-text"
                && rows.iter().any(|other| {
                    other.action.is_some()
                        && other.resource.id == row.resource.id
                        && other.resource.kind == row.resource.kind
                })
                && rows.iter().any(|owner| {
                    owner.has_children
                        && owner.resource.id != row.resource.id
                        && row
                            .resource
                            .id
                            .contains(owner.resource.id.rsplit(':').next().unwrap_or_default())
                })
        };
        for row in rows.iter().filter(|row| !owned(row)) {
            // Every word, as the one search reads a query: not the rows matching the most of it.
            let score = word_score(row, &words);
            if score < words.len() {
                continue;
            }
            found.push((
                score,
                tilo_actions::remote::Found {
                    resource: row.resource.id.clone(),
                    kind: row.resource.kind.clone(),
                    label: row.resource.label.clone(),
                    open: Some(
                        row.action.as_ref().map_or_else(
                            || "open".to_owned(),
                            |action| action.address.action.clone(),
                        ),
                    ),
                    arguments: serde_json::Map::new(),
                    app: Some(APP.to_owned()),
                    provider: None,
                    when: None,
                    ..tilo_actions::remote::Found::default()
                },
            ));
        }
        found.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        Pending::ready(Ok(found.into_iter().map(|(_, found)| found).collect()))
    }
}

/// A tool call that asks for typed answers: its `structuredContent`, and whether it is an error.
pub(crate) async fn answer<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    name: &str,
    arguments: Value,
) -> (Value, bool) {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments,
                "_meta":{"so.tilo.mcp":{"structured":true}}}}))
        .await
        .unwrap();
    let result = &reply["result"];
    let structured = result["structuredContent"].clone();
    assert!(
        structured.is_object(),
        "{name} answered no typed value: {reply}"
    );
    (structured, result["isError"].as_bool().unwrap_or_default())
}

/// What `future` came to, on a clock that moves only when everything waits.
fn waited<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(future)
}

/// What the tool `call` names answered on `server` while the person of the action `asked` of
/// `notes` is asked to allow it and answers only after that long, whether the call failed, and
/// how long it was held.
fn while_asked(
    notes: &Notes,
    server: &McpServer<Notes>,
    asked: (&'static str, std::time::Duration),
    call: (&str, Value),
) -> (String, bool, std::time::Duration) {
    waited(async {
        let started = tokio::time::Instant::now();
        *notes.person_asked.lock().unwrap() =
            Some((asked.0, started.checked_add(asked.1).unwrap()));
        let (text, failed) = tool(server, call.0, call.1).await;
        (text, failed, started.elapsed())
    })
}

/// A rename of the Todo note, with what else the call names.
fn rename(more: &Value) -> Value {
    let mut call = json!({"path": "notes/Todo#rename", "args": {"title": "Later"}});
    if let (Some(call), Some(more)) = (call.as_object_mut(), more.as_object()) {
        call.extend(more.clone());
    }
    call
}

tilo_test_env::scenario! {
    fn an_agent_lists_reads_calls_and_watches_the_workspace;

    phase the_apps_are_listed_by_name_running_first;
    {
        let notes = Notes::new(true);
        run(async {
            let server = agent(&notes);
            let (text, failed) = tool(&server, "tilo_ls", json!({})).await;
            assert!(!failed, "{text}");
            assert_eq!(
                text,
                "apps\nnotes  Notes  running\nsheets  Sheets  installed"
            );
            // A typed answer is its fields: the words an agent reads are no part of it.
            let typed = server
                .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                    "params":{"name":"tilo_ls","arguments":{},
                              "_meta":{"so.tilo.mcp":{"structured":true}}}}))
                .await
                .unwrap();
            assert!(typed["result"]["structuredContent"].get("text").is_none(), "{typed}");
        });
    }

    phase an_app_lists_its_pages_and_actions_by_the_names_a_person_uses;
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": "notes"})).await;
            assert!(!failed, "{text}");
            assert!(text.starts_with("notes  (so.tilo.notes)\n"), "{text}");
            for line in [
                "Todo/  notes.page",
                "Groceries/  notes.page",
                "#new-page(title?)  New note",
                "#read(item?)",
            ] {
                assert!(text.lines().any(|found| found == line), "{line} in\n{text}");
            }
            assert!(
                !text.contains("tilo-ui-bound") && !text.contains("3fa2c1d0"),
                "{text}"
            );
            let (actions, _) = tool(
                &agent(&host),
                "tilo_ls",
                json!({"path":"notes","kind":"actions"}),
            )
            .await;
            assert!(!actions.contains("notes.page"), "{actions}");
            let (resources, _) = tool(
                &agent(&host),
                "tilo_ls",
                json!({"path":"notes","kind":"resources"}),
            )
            .await;
            assert!(!resources.contains('#'), "{resources}");
            let (first, _) = tool(&agent(&host), "tilo_ls", json!({"path":"notes","limit":1})).await;
            assert!(first.contains("… 4 more: cursor \"1\""), "{first}");
            let (rest, _) = tool(
                &agent(&host),
                "tilo_ls",
                json!({"path":"notes","limit":10,"cursor":"1"}),
            )
            .await;
            assert!(
                !rest.contains("Notebook/") && rest.contains("Groceries/"),
                "{rest}"
            );
        });
    }

    phase a_page_reads_as_markdown_with_the_ids_edits_take;
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_read", json!({"path": "notes/Todo"})).await;
            assert!(!failed, "{text}");
            assert_eq!(
                text,
                "# Todo\n- [ ] milk ^b1\n- [x] bread ^b2\ncall mom ^b3\n## Later ^b4"
            );
            // The notebook's read takes the page by its stable key, not the broker's id.
            let invoked = host.invoked();
            let (resource, action, arguments) = invoked.last().unwrap();
            assert_eq!((resource.as_str(), action.as_str()), (NOTEBOOK, "read"));
            assert_eq!(*arguments, json!({"item": TODO}));
            let structured = todo_page().to_string();
            eprintln!(
                "page read: {} bytes as text, {} as JSON",
                text.len(),
                structured.len()
            );
            assert!(
                text.len() * 2 < structured.len(),
                "{} against {}",
                text.len(),
                structured.len()
            );
        });
    }

    phase a_do_answers_with_its_result_and_never_a_receipt;
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "Buy milk"}}),
            )
            .await;
            assert!(!failed, "{text}");
            assert_eq!(text, "ok\n{\"block\":\"b9\"}");
            let invoked = host.invoked();
            let (resource, action, arguments) = invoked.last().unwrap();
            assert_eq!(*resource, host.id(TODO_BODY));
            assert_eq!(action, "append-paragraph");
            assert_eq!(*arguments, json!({"text": "Buy milk"}));
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "notes/Todo#delete"}),
            )
            .await;
            assert_eq!((text.as_str(), failed), ("ok", false));
        });
    }

    phase a_refused_action_says_why_and_what_it_takes;
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "notes/Todo#rename"}),
            )
            .await;
            assert!(failed, "{text}");
            assert!(
                text.starts_with("failed: invalid_arguments: argument `title` is required"),
                "{text}"
            );
            assert!(text.contains("\n  title"), "the parameters follow: {text}");
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "notes/Todo#explode"}),
            )
            .await;
            assert!(failed, "{text}");
            // Not the app's whole list: what may have been meant, then where
            // the actions are.
            assert!(
                text.contains("has no action explode")
                    && text.contains("tilo_ls notes/Todo lists them"),
                "{text}"
            );
            assert!(!text.contains("delete, slow"), "no wall of names: {text}");
            // Named by the app alone, an action that does not exist is no such action either:
            // the caller is told so, and nothing is run.
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "notes#not-a-real-action"}),
            )
            .await;
            assert!(failed, "an unknown action is refused: {text}");
            assert!(
                text.contains("has no action not-a-real-action")
                    && text.contains("tilo_ls notes lists them"),
                "{text}"
            );
            // An app that does not run yet declares its actions in the surface its package
            // publishes: a name none of them has is no action of it, and nothing starts for it.
            let stopped = Notes::new(false).declaring(&["new-note"]);
            let (text, failed) = tool(
                &agent(&stopped),
                "tilo_do",
                json!({"path": "notes#not-a-real-action"}),
            )
            .await;
            assert!(failed, "a stopped app's unknown action is refused: {text}");
            assert!(
                text.starts_with("unknown_action:")
                    && text.contains("notes has no action not-a-real-action"),
                "refused as no such action, at once: {text}"
            );
            assert!(
                stopped.invoked.lock().unwrap().is_empty(),
                "nothing was started for it"
            );
            let (text, failed) = tool(&agent(&host), "tilo_do", json!({"path": "notes/Todo"})).await;
            assert!(failed && text.contains("name the action"), "{text}");
            // An action its permissions deny the agent is refused as not allowed, with where
            // that is changed: never as one the app does not have, and nothing is invoked.
            let notes = Notes::new(true).refusing("rename", Availability::not_allowed(Some(tilo_action_policy::Refuser::Settings)));
            let (text, failed) = tool(
                &agent(&notes),
                "tilo_do",
                json!({"path": "notes/Todo#rename", "args": {"title": "Done"}}),
            )
            .await;
            assert!(failed, "{text}");
            assert!(
                text.starts_with("not_permitted: the person you work for set this to Deny")
                    && text.contains("Workspace settings → Agents")
                    && !text.contains("not allowed: "),
                "{text}"
            );
            assert!(!text.contains("has no action"), "{text}");
            assert!(notes.invoked.lock().unwrap().is_empty(), "nothing was invoked");
            let (text, _) = tool(&agent(&notes), "tilo_ls", json!({"path": "notes/Todo"})).await;
            assert!(
                text.contains("rename") && text.contains("set this to Deny"),
                "the listing shows it, refused: {text}"
            );
            // One only the person at the device takes is never listed, yet refused by name.
            for (action, at) in [("rename", "notes/Todo"), ("new-page", "notes")] {
                let notes = Notes::new(true).refusing(action, Availability::shown_here());
                let path = format!("{at}#{action}");
                let args = json!({"path": path, "args": {"title": "Done"}});
                let (text, failed) = tool(&agent(&notes), "tilo_do", args).await;
                let whose = format!("private_view: #{action} only changes what the person at");
                assert!(failed && text.contains(&whose), "{text}");
                assert!(notes.invoked.lock().unwrap().is_empty(), "nothing was invoked");
                let (text, _) = tool(&agent(&notes), "tilo_ls", json!({"path": at})).await;
                assert!(!text.contains(action), "never listed: {text}");
            }
        });
    }

    phase steps_run_in_order_and_stop_at_the_first_failure;
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"calls": [
                    {"path": "notes/Todo/body#append-paragraph", "args": {"text": "a"}},
                    {"path": "notes/Todo#rename"},
                    {"path": "notes/Todo/body#append-paragraph", "args": {"text": "never"}},
                ]}),
            )
            .await;
            assert!(failed, "{text}");
            assert!(text.starts_with("[1] notes/Todo/body#append-paragraph: ok\n{\"block\":\"b9\"}\n[2] notes/Todo#rename: failed: invalid_arguments"), "{text}");
            assert!(
                text.contains("stopped at 2 of 3; earlier steps stay applied"),
                "{text}"
            );
            assert_eq!(host.invoked().len(), 2, "the third step never ran");
        });
    }

    phase an_action_still_running_says_so_and_status_follows_it;
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "notes/Todo#slow", "wait_ms": 30}),
            )
            .await;
            assert!(failed, "{text}");
            let id = text
                .strip_prefix("pending ")
                .and_then(|rest| rest.split(':').next())
                .unwrap_or_else(|| panic!("{text}"));
            assert!(uuid::Uuid::parse_str(id).is_ok(), "{text}");
            let (status, failed) =
                tool(&agent(&host), "tilo_status", json!({"invocation_id": id})).await;
            assert!(failed && status.starts_with("pending "), "{status}");
        });
    }

    phase a_watch_answers_changes_runs_and_presence;
    // A watch tells what happened since its cursor: a change to a resource and
    // who made it, a run that finished, and who is in the workspace.
    {
        let host = Notes::new(true);
        run(async {
            let server = agent(&host).agent_named("Claude Code");
            let (start, failed) = answer(&server, "tilo_watch", json!({"wait_ms": 50})).await;
            assert!(!failed, "{start}");
            // A watch with no cursor starts now: the presence it says is who is here now, and
            // nothing that happened yet is news.
            assert!(
                start["events"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .all(|event| event["kind"] == "presence"),
                "a watch with no cursor starts now, saying who is here: {start}"
            );
            let cursor = start["cursor"].as_str().unwrap().to_owned();

            host.changed(json!({
                "change": "c1a2",
                "happened": "edited",
                "sentence": "added \"Buy milk\"",
                "by": "Ada",
                "time_ms": 1_730_000_000_000_i64,
            }));
            host.people(Some(vec![
                tilo_actions::remote::Here {
                    who: "ada".into(),
                    name: "Ada".into(),
                    kind: "person".into(),
                    works_in: None,
                    devices: Vec::new(),
                },
                tilo_actions::remote::Here {
                    who: "agent:claude".into(),
                    name: "Claude Code".into(),
                    kind: "agent".into(),
                    works_in: None,
                    devices: Vec::new(),
                },
            ]));
            let (ran, failed) = answer(
                &server,
                "tilo_do",
                json!({"path":"notes/Todo/body#append-paragraph","args":{"text":"x"}}),
            )
            .await;
            assert!(!failed, "{ran}");

            let (watched, failed) = answer(
                &server,
                "tilo_watch",
                json!({"path":"notes/Todo","cursor":cursor,"wait_ms":50}),
            )
            .await;
            assert!(!failed, "{watched}");
            let events = watched["events"].as_array().unwrap();
            let change = events
                .iter()
                .find(|event| event["kind"] == "change")
                .expect("the change is told");
            assert_eq!(change["path"], "notes/Todo");
            assert_eq!(change["change"], "c1a2");
            assert_eq!(change["what"], "edited");
            assert_eq!(change["sentence"], "added \"Buy milk\"");
            assert_eq!(change["by"], "Ada");
            let told = events
                .iter()
                .find(|event| event["kind"] == "invocation")
                .unwrap_or_else(|| panic!("the invocation is told: {watched}"));
            assert_eq!(told["invocation_id"], ran["invocation_id"]);
            assert_eq!(told["path"], "notes/Todo/body#append-paragraph");
            assert_eq!(told["phase"], "succeeded");
            assert_eq!(
                told["title"], "Claude Code",
                "a step asked nothing is named by the agent"
            );
            let here = events
                .iter()
                .find(|event| event["kind"] == "presence")
                .expect("who is here is told");
            assert!(
                here["here"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|member| member["name"] == "Claude Code" && member["kind"] == "agent"),
                "{here}"
            );
            // Everything it told is told once.
            let (again, failed) = answer(
                &server,
                "tilo_watch",
                json!({"path":"notes/Todo","cursor":watched["cursor"].as_str().unwrap(),"wait_ms":50}),
            )
            .await;
            assert!(!failed, "{again}");
            assert_eq!(again["events"], json!([]), "{again}");
        });
    }

    phase an_action_the_person_must_allow_answers_pending_with_the_ask;
    // An action the person must allow: a typed call answers `pending` with the
    // ask rather than holding the caller, and the text a model reads is what it
    // was.
    {
        let host = Notes::new(true).asking();
        run(async {
            let server = agent(&host);
            let (asked, failed) = answer(&server, "tilo_do", json!({"path":"notes/Todo#delete"})).await;
            assert!(!failed, "pending is not an error: {asked}");
            assert_eq!(asked["status"], "pending");
            let id = asked["invocation_id"]
                .as_str()
                .expect("the call names its invocation");
            assert_eq!(asked["ask"]["invocation_id"], id);
            assert_eq!(asked["path"], "notes/Todo#delete");

            // A model still reads the refusal's words.
            let (text, failed) = tool(&server, "tilo_do", json!({"path":"notes/Todo#delete"})).await;
            assert!(failed, "{text}");
            assert!(text.contains("requires confirmation"), "{text}");
        });
    }

    phase find_answers_by_the_desktops_one_search_or_says_there_is_none;
    // `find` answers by the one search the desktop runs, and a host without one
    // says so: it never answers by a ranking of its own, which is what the
    // palette and text typed on the canvas would not agree with.
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_find", json!({"query": "Groceries"})).await;
            assert!(!failed, "{text}");
            // The note leads: a row about the note's body is about the note, too,
            // and every row names the app's path down to what it is about.
            assert_eq!(
                text.lines().next(),
                Some("notes/Notebook/Groceries  notes.page"),
                "{text}"
            );
            // A host with no search of its own is told so, never given a second
            // ranking's answers.
            let no_search = host.clone().without_search();
            let (text, failed) = tool(
                &agent(&no_search),
                "tilo_find",
                json!({"query": "Groceries"}),
            )
            .await;
            assert!(failed, "{text}");
            assert!(text.contains("keeps no search of its workspace"), "{text}");
        });
    }

    phase an_invocation_is_taken_back_and_stopped_through_its_record;
    // Undo and cancel are the invocation record's own actions: `tilo_status` asks the record,
    // and nothing beside it is looked for.
    {
        let host = Notes::new(true);
        run(async {
            let server = agent(&host);
            let (ran, failed) = answer(
                &server,
                "tilo_do",
                json!({"path":"notes/Todo/body#append-paragraph","args":{"text":"x"}}),
            )
            .await;
            assert!(!failed, "{ran}");
            let id = ran["invocation_id"].as_str().unwrap().to_owned();
            let (text, failed) = tool(
                &server,
                "tilo_status",
                json!({"invocation_id": id, "undo": true}),
            )
            .await;
            assert!(!failed, "{text}");
            assert_eq!(text, format!("{id} is taken back: what it changed is gone"));
            let undone = host.invoked.lock().unwrap().last().cloned().unwrap();
            assert_eq!(
                undone.target.provider,
                tilo_actions::remote::workspace_invocations::PROVIDER
            );
            assert_eq!(undone.target.address.resource, id);
            assert_eq!(
                undone.target.address.action,
                tilo_actions::remote::workspace_invocations::UNDO
            );

            let (text, failed) = tool(
                &server,
                "tilo_do",
                json!({"path": "notes/Todo#slow", "wait_ms": 30}),
            )
            .await;
            assert!(failed && text.starts_with("pending "), "{text}");
            let id = text
                .strip_prefix("pending ")
                .and_then(|rest| rest.split(':').next())
                .unwrap()
                .to_owned();
            let (text, failed) = tool(
                &server,
                "tilo_status",
                json!({"invocation_id": id, "cancel": true}),
            )
            .await;
            // A stopped call did not do what it was asked: the answer says so as a failure.
            assert_eq!((text.as_str(), failed), ("cancelled", true));
            let (text, failed) = tool(
                &server,
                "tilo_status",
                json!({"invocation_id": id, "cancel": true, "undo": true}),
            )
            .await;
            assert!(failed && text.contains("ask one of them"), "{text}");
        });
    }
}

/// A person who allows it while the call still waits for them is not said to be asked: the
/// call is answered with what the action did.
#[test]
fn a_call_whose_person_allows_it_within_its_wait_is_answered_with_what_it_did() {
    let notes = Notes::new(true);
    let (text, failed, held) = while_asked(
        &notes,
        &agent(&notes),
        ("rename", std::time::Duration::from_secs(2)),
        ("tilo_do", rename(&json!({}))),
    );
    assert!(!failed && text.starts_with("ok"), "{text}");
    assert!(
        held >= std::time::Duration::from_secs(2)
            && held < std::time::Duration::from_millis(DEFAULT_WAIT_MS),
        "answered after {held:?}"
    );
}

/// `tilo_status` naming no invocation says what this session ran and how
/// each stands, the calls that named no id of their own among them, rather
/// than refusing the call for a missing field.
#[test]
fn a_status_naming_no_invocation_lists_the_sessions_invocations() {
    let host = Notes::new(true);
    run(async {
        let server = agent(&host);
        let (text, failed) = tool(&server, "tilo_status", json!({})).await;
        assert!(!failed, "{text}");
        assert!(text.contains("ran nothing yet"), "{text}");
        let (text, failed) = tool(
            &server,
            "tilo_do",
            json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "Buy milk"}}),
        )
        .await;
        assert!(!failed, "{text}");
        let (text, failed) = tool(&server, "tilo_status", json!({})).await;
        assert!(!failed, "{text}");
        let line = text
            .lines()
            .find(|line| line.contains("notes/Todo/body#append-paragraph"))
            .unwrap_or_else(|| panic!("the call is listed: {text}"));
        let id = line.split_whitespace().next().unwrap();
        assert!(line.ends_with("succeeded"), "{line}");
        // The id it lists is one `tilo_status` answers for.
        let (one, failed) = tool(&server, "tilo_status", json!({"invocation_id": id})).await;
        assert!(!failed, "{one}");
        let (refused, failed) = tool(&server, "tilo_status", json!({"cancel": true})).await;
        assert!(failed && refused.contains("invocation_id"), "{refused}");
    });
}

#[path = "mcp_agent_link_tests.rs"]
mod link;

/// The words of a search, as the desktop's one search reads a query.
fn needle_words(needle: &str) -> Vec<String> {
    needle
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// How many of `words` begin a word of the row's names, or are one of those
/// words in another inflection.
fn word_score(row: &CatalogueRow, words: &[String]) -> usize {
    let mut names = vec![
        row.resource.label.as_str(),
        row.resource.kind.as_str(),
        row.app_id.as_str(),
    ];
    if let Some(action) = &row.action {
        names.extend([action.address.action.as_str(), action.action_label.as_str()]);
        names.extend(action.description.as_deref());
        for param in &action.params {
            names.extend([param.name.as_ref(), param.description.as_ref()]);
        }
    }
    let names = names
        .iter()
        .flat_map(|name| name.split(|c: char| !c.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let stems = names
        .iter()
        .map(|name| tilo_actions::remote::stem_word(name))
        .collect::<Vec<_>>();
    words
        .iter()
        .filter(|word| {
            let stemmed = tilo_actions::remote::stem_word(word);
            // A word begins a name's word, or is that word in another
            // inflection: "drawing" and "draw" are the same word to a
            // caller describing what it wants to do.
            names.iter().any(|name| name.starts_with(word.as_str())) || stems.contains(&stemmed)
        })
        .count()
}
