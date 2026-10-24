//! The agent surface against a Notes shaped like the live one: a notebook
//! whose pages are resources, each page's body published beside it under a
//! bound id that only the app's own key reads (`notes.notebook/page/<id>/body`).
use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueResource,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{ActionParam, InvocationUpdate, ParamType, ResourceSemanticIdentity};

const APP: &str = "so.tilo.notes";
const WORKSPACE: &str = "workspace:room";
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
struct Notes {
    /// Whether the app runs; a launch from the workspace sets it.
    running: Arc<Mutex<bool>>,
    /// Which publication the bound ids belong to: a restart changes it.
    publication: &'static str,
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
    /// How many times the app's directory moves on while a level is being
    /// read: while it is above zero the app's top level comes in two pages,
    /// and asking for the second finds the revision changed.
    moves: Arc<Mutex<u32>>,
    /// Rows a search's page holds, when set: more come in a second page,
    /// which finds the revision changed, as a busy app's directory does.
    search_page: Arc<Mutex<Option<usize>>>,
    /// What the directory was asked, in order: `apps` for which apps run,
    /// else the provider and the parent a listing is under.
    asked: Arc<Mutex<Vec<String>>>,
    /// Whether Sheets runs too, its directory never answering: the answers
    /// it was asked for are held here, unanswered.
    silent: Arc<Mutex<Option<Vec<Unanswered>>>>,
    /// Lines in each of the two pages the Todo note reads in, when it is a
    /// long one: the notebook's read then takes a cursor.
    long: Arc<Mutex<Option<usize>>>,
}

/// A directory answer that is never given.
type Unanswered = tilo_actions::remote::Completer<Result<CataloguePage, ActionHostError>>;

impl Notes {
    fn new(running: bool) -> Self {
        Self {
            running: Arc::new(Mutex::new(running)),
            publication: "2a4120d7-2f63-4578-aed8-11cb91ed3628",
            invoked: Arc::default(),
            moves: Arc::default(),
            search_page: Arc::default(),
            asked: Arc::default(),
            silent: Arc::default(),
            long: Arc::default(),
        }
    }

    fn restarted(&self) -> Self {
        Self {
            publication: "99999999-2f63-4578-aed8-11cb91ed3628",
            ..self.clone()
        }
    }

    fn id(&self, key: &str) -> String {
        if key == NOTEBOOK {
            key.to_owned()
        } else {
            bound(self.publication, key)
        }
    }

    /// What the directory was asked since the last time this was taken.
    fn take_asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.lock().unwrap())
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
                    semantic_identity: (key != NOTEBOOK)
                        .then(|| ResourceSemanticIdentity::new(kind, key))
                        .flatten(),
                    address: tilo_actions::ActionAddress::new(id.clone(), name),
                    resource_kind: kind.into(),
                    resource_label: label.into(),
                    action_label: title.into(),
                    description: Some(format!("{title}. Longer explanation that is not listed.")),
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: operation,
                    contract: tilo_actions::ActionContract::default(),
                    input: tilo_actions::InputSpec::None,
                    params,
                    view: None,
                },
            ),
            has_children,
            visibility: None,
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
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new(format!("app.{app}"), "open"),
                    resource_kind: "applications.app".into(),
                    resource_label: label.into(),
                    action_label: "Open".into(),
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
            };
            return vec![open(APP, "Notes"), open("so.tilo.sheets", "Sheets")];
        }
        let text = |name: &'static str, required: bool| {
            if required {
                ActionParam::required(name, ParamType::Text, "Text")
            } else {
                ActionParam::optional(name, ParamType::Text, "Text")
            }
        };
        let long = self.long.lock().unwrap().is_some();
        let mut rows = vec![
            self.row("", NOTEBOOK, "notes.notebook", "Notebook", None, true),
            self.row("", TODO_BODY, "tilo.rich-text", "Todo", None, false),
            self.row("", SHOP_BODY, "tilo.rich-text", "Groceries", None, false),
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
                Some((
                    "read",
                    "Read",
                    if long {
                        vec![text("item", false), text("cursor", false)]
                    } else {
                        vec![text("item", false)]
                    },
                    View,
                )),
                false,
            ),
            self.row(NOTEBOOK, TODO, "notes.page", "Todo", None, true),
            self.row(NOTEBOOK, SHOP, "notes.page", "Groceries", None, true),
        ];
        for (page, label) in [(TODO, "Todo"), (SHOP, "Groceries")] {
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
        }
        for (body, label) in [(TODO_BODY, "Todo"), (SHOP_BODY, "Groceries")] {
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

impl ActionHost for Notes {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let running = *self.running.lock().unwrap();
        if query.target.is_none() && query.needle.is_empty() {
            self.asked
                .lock()
                .unwrap()
                .push(query.provider.as_ref().map_or_else(
                    || "apps".to_owned(),
                    |provider| {
                        format!(
                            "{provider} under {}",
                            query.parent_resource.as_deref().unwrap_or_default()
                        )
                    },
                ));
        }
        let silent = self.silent.lock().unwrap().is_some();
        let Some(provider) = query.provider.clone() else {
            let instance = |provider: &str, app_id: &str| CatalogueProvider {
                provider: provider.into(),
                app_id: app_id.into(),
                room: "room".into(),
                record_id: String::new(),
                browsable: true,
            };
            return Pending::ready(Ok(CataloguePage {
                revision: "v1".into(),
                providers: running
                    .then(|| instance("1", APP))
                    .into_iter()
                    .chain(silent.then(|| instance("2", "so.tilo.sheets")))
                    .collect(),
                ..CataloguePage::default()
            }));
        };
        if provider == "2" {
            let (unanswered, answer) = tilo_actions::remote::pending();
            if let Some(held) = self.silent.lock().unwrap().as_mut() {
                held.push(unanswered);
            }
            return answer;
        }
        if provider != WORKSPACE && !running {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let mut rows = self.rows(&provider);
        for row in &mut rows {
            row.provider.clone_from(&provider);
        }
        if let Some(target) = &query.target {
            rows.retain(|row| row.action.as_ref().is_some_and(|a| a.address == *target));
        } else if query.needle.is_empty() {
            let parent = query.parent_resource.clone().unwrap_or_default();
            rows.retain(|row| row.parent_resource == parent);
            let moves = *self.moves.lock().unwrap();
            if provider != WORKSPACE && parent.is_empty() && moves > 0 {
                if query.cursor.is_empty() {
                    rows.truncate(1);
                    return Pending::ready(Ok(CataloguePage {
                        revision: format!("v{moves}"),
                        rows,
                        next_cursor: b"second".to_vec(),
                        ..CataloguePage::default()
                    }));
                }
                let after = moves.saturating_sub(1);
                *self.moves.lock().unwrap() = after;
                return Pending::ready(Ok(CataloguePage {
                    revision: format!("v{after}"),
                    stale_cursor: true,
                    ..CataloguePage::default()
                }));
            }
        } else {
            rows.retain(|row| host_matches(row, &query.needle));
            let search_page = *self.search_page.lock().unwrap();
            if let Some(page) = search_page {
                if !query.cursor.is_empty() {
                    return Pending::ready(Ok(CataloguePage {
                        revision: "v2".into(),
                        stale_cursor: true,
                        ..CataloguePage::default()
                    }));
                }
                if rows.len() > page {
                    rows.truncate(page);
                    return Pending::ready(Ok(CataloguePage {
                        revision: "v1".into(),
                        rows,
                        next_cursor: b"more".to_vec(),
                        ..CataloguePage::default()
                    }));
                }
            }
        }
        Pending::ready(Ok(CataloguePage {
            revision: "v1".into(),
            rows,
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        let known = self
            .rows(&request.target.provider)
            .iter()
            .any(|row| row.resource.id == request.target.address.resource);
        if !known || (request.target.provider != WORKSPACE && !*self.running.lock().unwrap()) {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        if request.target.provider == WORKSPACE {
            *self.running.lock().unwrap() = true;
        }
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationState::new(
            invocation,
            InvocationUpdate::Accepted { invocation },
        )
        .receipt()))
    }

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
        let long = *self.long.lock().unwrap();
        let update = match action.as_str() {
            "slow" => InvocationUpdate::Accepted { invocation: *id },
            "rename" if !arguments.contains_key("title") => {
                failed("invalid_arguments", "argument `title` is required")
            }
            "read" => match arguments.get("item").and_then(Value::as_str) {
                None => succeeded(Some(json!({
                    "kind": "notes.notebook",
                    "pages": [
                        {"item": TODO, "title": "Todo", "parent": null},
                        {"item": SHOP, "title": "Groceries", "parent": null},
                    ],
                    "tags": [],
                }))),
                Some(TODO) => match long {
                    Some(lines) => {
                        let cursor = arguments.get("cursor").and_then(Value::as_str);
                        succeeded(Some(long_page(lines, cursor)))
                    }
                    None => succeeded(Some(todo_page())),
                },
                Some(other) => failed("not_found", &format!("no note `{other}`")),
            },
            "append-paragraph" => succeeded(Some(json!({"block": "b9", "note": null}))),
            _ => succeeded(None),
        };
        Some(InvocationState::new(*id, update))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
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

/// A page of a long Todo note, as Notes answers it: `lines` lines of the
/// first half, with the cursor to the second, or of the second half when
/// `cursor` asks for it.
fn long_page(lines: usize, cursor: Option<&str>) -> Value {
    let half = usize::from(cursor == Some("second"));
    let markdown = (0..lines)
        .map(|line| format!("- [ ] item {half}-{line:03} ^h{half}l{line:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut page =
        json!({"kind": "notes.page", "item": TODO, "title": "Todo", "markdown": markdown});
    if half == 0 {
        page["nextCursor"] = json!("second");
    }
    page
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// What a tool call answered, as the agent reads it: the text, and whether
/// the call is an error.
async fn tool(server: &McpServer<Notes>, name: &str, arguments: Value) -> (String, bool) {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .await
        .unwrap();
    let result = &reply["result"];
    assert!(result.get("structuredContent").is_none(), "{reply}");
    (
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {reply}"))
            .to_owned(),
        result["isError"] == true,
    )
}

fn agent(host: &Notes) -> McpServer<Notes> {
    McpServer::for_agent(host.clone(), "room".into(), "claude-code")
}

#[test]
fn a_path_is_app_names_and_an_action() {
    assert_eq!(
        parse_path("notes/Todo/body#append-paragraph").unwrap(),
        ParsedPath {
            app: "notes".into(),
            segments: vec!["Todo".into(), "body".into()],
            action: Some("append-paragraph".into()),
        }
    );
    assert_eq!(
        parse_path("  notes ").unwrap(),
        ParsedPath {
            app: "notes".into(),
            segments: Vec::new(),
            action: None,
        }
    );
    // A title with a slash or a hash writes it escaped.
    let parsed = parse_path(r"notes/Q1\/Q2 plan\#1#rename").unwrap();
    assert_eq!(parsed.segments, vec!["Q1/Q2 plan#1".to_owned()]);
    assert_eq!(parsed.action.as_deref(), Some("rename"));
    assert_eq!(path_name("Q1/Q2 plan#1"), r"Q1\/Q2 plan\#1");
    assert!(parse_path("").is_err());
    assert!(parse_path("#rename").is_err());
}

#[test]
fn an_agent_is_offered_five_small_tools_and_no_classic_ones() {
    let host = Notes::new(true);
    run(async {
        let reply = agent(&host)
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(
            names,
            [
                "tilo_ls",
                "tilo_read",
                "tilo_do",
                "tilo_find",
                "tilo_status"
            ]
        );
        let size = serde_json::to_string(tools).unwrap().len();
        assert!(size < 3_500, "the tool list is {size} bytes");
        let classic = McpServer::new(host.clone(), "room".into(), "owner".into())
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        let classic = serde_json::to_string(&classic["result"]["tools"])
            .unwrap()
            .len();
        assert!(size < classic, "{size} against {classic}");
        // An agent's session answers no classic tool; a classic session no agent tool.
        let refused = agent(&host)
            .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_query","arguments":{}}}))
            .await
            .unwrap();
        assert!(refused["error"].is_object(), "{refused}");
    });
}

#[test]
fn the_apps_are_listed_by_name_running_first() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(&agent(&host), "tilo_ls", json!({})).await;
        assert!(!failed, "{text}");
        assert_eq!(
            text,
            "apps\nnotes  Notes  running\nsheets  Sheets  installed"
        );
    });
}

#[test]
fn an_app_lists_its_pages_and_actions_by_the_names_a_person_uses() {
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

#[test]
fn a_page_lists_its_actions_and_the_body_published_beside_it() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": "notes/todo"})).await;
        assert!(!failed, "{text}");
        assert!(text.starts_with("notes/Todo  (notes.page)\n"), "{text}");
        assert!(text.contains("#rename(title)  Rename note"), "{text}");
        assert!(
            text.contains("#delete  Delete note [destructive]"),
            "{text}"
        );
        // The body shares the page's title; it is named by what tells it apart.
        assert!(text.contains("body  tilo.rich-text"), "{text}");
        let (body, failed) =
            tool(&agent(&host), "tilo_ls", json!({"path": "notes/Todo/body"})).await;
        assert!(!failed, "{body}");
        assert!(body.contains("#append-paragraph(text)"), "{body}");
        assert!(!body.contains("Groceries"), "{body}");
    });
}

#[test]
fn an_action_is_described_on_demand() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(
            &agent(&host),
            "tilo_ls",
            json!({"path": "notes/Todo/body#insert-block"}),
        )
        .await;
        assert!(!failed, "{text}");
        assert!(
            text.starts_with("notes/Todo/body#insert-block  Insert paragraph\nInsert paragraph\n"),
            "{text}"
        );
        assert!(text.contains("\n  after — Text"), "{text}");
        assert!(text.contains("\n  text?"), "{text}");
        assert!(!text.contains("Longer explanation"), "{text}");
    });
}

#[test]
fn a_path_means_the_same_resource_after_the_desktop_restarts() {
    let before = Notes::new(true);
    let after = before.restarted();
    assert_ne!(
        before.id(TODO_BODY),
        after.id(TODO_BODY),
        "the broker's ids moved"
    );
    run(async {
        for host in [&before, &after] {
            let (text, failed) = tool(
                &agent(host),
                "tilo_do",
                json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "x"}}),
            )
            .await;
            assert!(!failed, "{text}");
            let invoked = host.invoked();
            let (resource, action, _) = invoked.last().unwrap();
            assert_eq!(*resource, host.id(TODO_BODY));
            assert_eq!(action, "append-paragraph");
        }
    });
}

#[test]
fn a_page_reads_as_markdown_with_the_ids_edits_take() {
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

#[test]
fn an_app_reads_as_the_list_of_what_it_holds() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(&agent(&host), "tilo_read", json!({"path": "notes"})).await;
        assert!(!failed, "{text}");
        assert!(
            text.starts_with("pages:\n  Todo  (notes.page:3fa2c1d0"),
            "{text}"
        );
        assert!(!text.contains("tags"), "empty things are left out: {text}");
    });
}

#[test]
fn a_do_answers_with_its_result_and_never_a_receipt() {
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

#[test]
fn a_refused_action_says_why_and_what_it_takes() {
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
        assert!(
            text.contains("has no action explode; its actions: rename, delete, slow"),
            "{text}"
        );
        let (text, failed) = tool(&agent(&host), "tilo_do", json!({"path": "notes/Todo"})).await;
        assert!(failed && text.contains("name the action"), "{text}");
    });
}

#[test]
fn a_name_that_is_not_there_lists_what_is() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": "notes/Nope"})).await;
        assert!(failed, "{text}");
        assert!(text.starts_with("no Nope in notes; it has: "), "{text}");
        assert!(
            text.contains("Todo") && text.contains("Groceries"),
            "{text}"
        );
        let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": "mail"})).await;
        assert!(failed, "{text}");
        assert!(
            text.contains("no app mail") && text.contains("notes") && text.contains("sheets"),
            "{text}"
        );
    });
}

#[test]
fn steps_run_in_order_and_stop_at_the_first_failure() {
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

#[test]
fn an_action_still_running_says_so_and_status_follows_it() {
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

#[test]
fn an_installed_app_is_started_by_its_path_and_then_listed() {
    let host = Notes::new(false);
    run(async {
        let server = agent(&host);
        let (text, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
        assert!(!failed, "{text}");
        assert!(
            text.contains("installed but not running") && text.contains("notes#open"),
            "{text}"
        );
        let (text, failed) = tool(&server, "tilo_do", json!({"path": "notes#open"})).await;
        assert!(!failed, "{text}");
        assert!(
            text.starts_with("ok: notes is running\nnotes  (so.tilo.notes)\n"),
            "{text}"
        );
        assert!(text.contains("Todo/  notes.page"), "{text}");
        // Not the app's own action on an app that is not running.
        let host = Notes::new(false);
        let (text, failed) = tool(
            &agent(&host),
            "tilo_do",
            json!({"path": "notes/Todo#rename"}),
        )
        .await;
        assert!(
            failed && text.contains("installed but not running"),
            "{text}"
        );
    });
}

#[test]
fn a_search_ranks_apps_then_resources_then_actions_and_lists_ready_paths() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(&agent(&host), "tilo_find", json!({"query": "notes"})).await;
        assert!(!failed, "{text}");
        assert_eq!(text.lines().next(), Some("notes  Notes  running"), "{text}");

        let (text, failed) = tool(&agent(&host), "tilo_find", json!({"query": "groceries"})).await;
        assert!(!failed, "{text}");
        let lines: Vec<&str> = text.lines().collect();
        // The resources come before the actions on them.
        let first_action = lines.iter().position(|line| line.contains('#')).unwrap();
        assert!(
            lines[..first_action].iter().all(|line| !line.contains('#')),
            "{text}"
        );
        assert!(
            lines[..first_action]
                .iter()
                .any(|line| line.contains("notes.page")),
            "{text}"
        );

        let (text, failed) = tool(
            &agent(&host),
            "tilo_find",
            json!({"query": "append paragraph"}),
        )
        .await;
        assert!(!failed, "{text}");
        assert!(
            text.lines()
                .any(|line| line.starts_with("notes/Todo/body#append-paragraph(text)")),
            "a body is found under the note that owns it: {text}"
        );
        let path = text
            .lines()
            .next()
            .unwrap()
            .split('(')
            .next()
            .unwrap()
            .to_owned();
        let (done, failed) = tool(
            &agent(&host),
            "tilo_do",
            json!({"path": path, "args": {"text": "y"}}),
        )
        .await;
        assert!(!failed, "{path}: {done}");

        let (text, failed) = tool(&agent(&host), "tilo_find", json!({"query": "zzzz"})).await;
        assert!(
            !failed && text.starts_with("nothing matches zzzz"),
            "{text}"
        );
        let (text, _) = tool(
            &agent(&host),
            "tilo_find",
            json!({"query": "paragraph", "limit": 1}),
        )
        .await;
        assert!(text.contains("more; narrow the query"), "{text}");
        let (text, _) = tool(
            &agent(&host),
            "tilo_find",
            json!({"query": "paragraph", "app": "sheets"}),
        )
        .await;
        assert!(text.starts_with("nothing matches"), "{text}");
    });
}

/// A search narrows to an app by the name `tilo_ls` lists or by its whole
/// id, and an app that is not there is an error naming the ones that are,
/// never an empty search.
#[test]
fn a_search_narrows_to_an_app_by_name_or_id_and_names_the_apps_when_none_is() {
    let host = Notes::new(true);
    run(async {
        let query = |app: &str| json!({"query": "append paragraph", "app": app});
        let (by_name, failed) = tool(&agent(&host), "tilo_find", query("notes")).await;
        assert!(!failed, "{by_name}");
        assert!(
            by_name
                .lines()
                .any(|line| line.starts_with("notes/Todo/body#append-paragraph(text)")),
            "{by_name}"
        );
        for app in [APP, "SO.TILO.NOTES", " Notes "] {
            let (text, failed) = tool(&agent(&host), "tilo_find", query(app)).await;
            assert_eq!((text.as_str(), failed), (by_name.as_str(), false), "{app}");
        }
        let (text, failed) = tool(&agent(&host), "tilo_find", query("so.tilo.nope")).await;
        assert!(failed, "{text}");
        assert_eq!(text, "no app so.tilo.nope; the apps: notes, sheets");
    });
}

/// A name searched whole is found even where its longest word fills more
/// pages than a search reads: "paragraph" names both bodies' append and
/// insert, and the second page is gone by the time it is asked for.
#[test]
fn a_search_finds_a_name_whose_longest_word_many_rows_share() {
    let host = Notes::new(true);
    *host.search_page.lock().unwrap() = Some(2);
    run(async {
        let (text, failed) = tool(
            &agent(&host),
            "tilo_find",
            json!({"query": "insert paragraph"}),
        )
        .await;
        assert!(!failed, "{text}");
        for page in ["Todo", "Groceries"] {
            assert!(
                text.lines()
                    .any(|line| line.starts_with(&format!("notes/{page}/body#insert-block"))),
                "{page}'s insert is missing: {text}"
            );
        }
    });
}

#[test]
fn bound_ids_decode_to_the_apps_own_name_and_nothing_else_does() {
    assert_eq!(
        bound_key(&bound("a", TODO_BODY)).as_deref(),
        Some(TODO_BODY)
    );
    assert_eq!(bound_key("notes.notebook"), None);
    assert_eq!(bound_key("tilo-ui-bound:v1:a:b:zz"), None);
    assert_eq!(bound_key("tilo-ui-bound:v1:a:b:abc"), None);
}

#[test]
fn a_read_is_cut_at_a_line_and_says_how_much_is_left() {
    let mut text = (0..100)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let total = text.chars().count();
    let left = cut_at_line(&mut text, 200).unwrap();
    assert_eq!(text.chars().count() + left, total);
    assert!(text.ends_with(|c: char| c.is_ascii_digit()) && !text.ends_with('\n'));
    assert!(text.lines().all(|line| line.starts_with("line ")));
    let mut short = "a\nb".to_owned();
    assert_eq!(cut_at_line(&mut short, 200), None);
}

/// A read cut at `max_chars` ends in a cursor, and following the cursors
/// answers the rest of the note once and in order, across the page the app
/// itself paged to.
#[test]
fn a_cut_read_continues_from_its_cursor_through_the_apps_own_pages() {
    let host = Notes::new(true);
    *host.long.lock().unwrap() = Some(40);
    run(async {
        let mut cursor: Option<String> = None;
        let mut read: Vec<String> = Vec::new();
        let mut cursors: Vec<String> = Vec::new();
        for _ in 0..40 {
            let mut args = json!({"path": "notes/Todo", "max_chars": 200});
            if let Some(cursor) = &cursor {
                args["cursor"] = json!(cursor);
            }
            let (text, failed) = tool(&agent(&host), "tilo_read", args).await;
            assert!(!failed, "{text}");
            let (body, last) = text.rsplit_once('\n').unwrap_or((text.as_str(), ""));
            cursor = last
                .split_once("cursor \"")
                .and_then(|(_, rest)| rest.strip_suffix('"'))
                .map(str::to_owned);
            let body = if cursor.is_some() {
                body
            } else {
                text.as_str()
            };
            assert!(body.chars().count() <= 200, "{text}");
            read.extend(
                body.lines()
                    .filter(|line| line.starts_with("- [ ]"))
                    .map(str::to_owned),
            );
            match &cursor {
                Some(next) => cursors.push(next.clone()),
                None => break,
            }
        }
        assert!(
            cursor.is_none(),
            "still a cursor after 40 reads: {cursors:?}"
        );
        let expected: Vec<String> = (0..2)
            .flat_map(|half| {
                (0..40).map(move |line| format!("- [ ] item {half}-{line:03} ^h{half}l{line:03}"))
            })
            .collect();
        assert_eq!(read, expected, "cursors: {cursors:?}");
        assert!(
            cursors.iter().any(|cursor| cursor == "second"),
            "{cursors:?}"
        );
        assert!(
            cursors
                .iter()
                .any(|cursor| cursor.starts_with("read+") && cursor.ends_with("+second")),
            "a cut of the app's second page keeps its cursor: {cursors:?}"
        );
        // The app is asked for its second page by its own cursor, never by one
        // of the door's.
        for (_, action, arguments) in host.invoked() {
            if action == "read" {
                let asked = arguments.get("cursor").and_then(Value::as_str);
                assert!(asked.is_none_or(|asked| asked == "second"), "{arguments}");
            }
        }
    });
}

#[test]
fn a_read_cursor_names_what_was_answered_and_the_apps_cursor() {
    assert_eq!(
        parse_read_cursor(&read_cursor(120, None)),
        Some((120, None))
    );
    assert_eq!(
        parse_read_cursor(&read_cursor(7, Some("a+b"))),
        Some((7, Some("a+b".to_owned())))
    );
    assert_eq!(parse_read_cursor("second"), None);
    assert_eq!(parse_read_cursor("read+x"), None);
    let mut text = "one\ntwo\nthree".to_owned();
    assert_eq!(skip_answered(&mut text, 3), 4, "the line break goes too");
    assert_eq!(text, "two\nthree");
    let mut short = "ab".to_owned();
    assert_eq!(skip_answered(&mut short, 10), 2);
    assert_eq!(short, "");
}

#[test]
fn a_block_action_takes_the_id_a_read_showed_and_an_abbreviation_of_its_name() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(
            &agent(&host),
            "tilo_do",
            // The action by the start of its name, the block by the id the read showed.
            json!({"path": "notes/Todo/body#set-block", "args": {"block": "b1"}}),
        )
        .await;
        assert_eq!(
            (text.as_str(), failed),
            ("ok", false),
            "one action starts so"
        );
        let (text, failed) = tool(
            &agent(&host),
            "tilo_do",
            json!({"path": "notes/Todo/body#set-block-done", "args": {"block": "b1"}}),
        )
        .await;
        assert_eq!((text.as_str(), failed), ("ok", false));
        let invoked = host.invoked();
        let (resource, action, arguments) = invoked.last().unwrap();
        assert_eq!(*resource, host.id(TODO_BODY));
        assert_eq!(
            (action.as_str(), arguments),
            ("set-block-done", &json!({"block": "b1"}))
        );
    });
}

/// A level is read as one snapshot: when the app's directory moves on
/// between its pages, the read starts again at the new revision and the agent
/// sees the whole level, not an error or a mix of two revisions.
#[test]
fn a_level_read_across_a_moving_directory_starts_again_at_the_new_revision() {
    let steady = Notes::new(true);
    let moving = Notes::new(true);
    *moving.moves.lock().unwrap() = 2;
    let (expected, failed) = run(tool(&agent(&steady), "tilo_ls", json!({"path": "notes"})));
    assert!(!failed, "{expected}");
    let (listed, failed) = run(tool(&agent(&moving), "tilo_ls", json!({"path": "notes"})));
    assert!(!failed, "{listed}");
    assert_eq!(listed, expected);
    assert_eq!(
        *moving.moves.lock().unwrap(),
        0,
        "both moves were read past"
    );
}

/// A directory that never holds still for one read is reported, once the
/// time an action is waited for has passed, and is read again with a pause
/// that grows rather than as fast as it answers.
#[test]
fn a_directory_that_never_holds_still_is_reported() {
    let moving = Notes::new(true);
    *moving.moves.lock().unwrap() = u32::MAX;
    let (text, failed, waited) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let started = tokio::time::Instant::now();
            let (text, failed) = tool(&agent(&moving), "tilo_ls", json!({"path": "notes"})).await;
            (text, failed, started.elapsed())
        });
    assert!(failed, "{text}");
    assert!(text.contains("kept changing"), "{text}");
    assert!(
        waited >= std::time::Duration::from_millis(DEFAULT_WAIT_MS),
        "gave up after {waited:?}"
    );
    let reads = u32::MAX.saturating_sub(*moving.moves.lock().unwrap());
    assert!(
        (20..=50).contains(&reads),
        "read {reads} times in {waited:?}"
    );
}

#[test]
fn a_changing_directory_is_read_again_at_once_then_after_a_growing_pause() {
    let ms = std::time::Duration::from_millis;
    let pauses: Vec<_> = (0..10).map(restart_pause).collect();
    assert_eq!(
        pauses,
        [
            ms(0),
            ms(0),
            ms(5),
            ms(10),
            ms(20),
            ms(40),
            ms(80),
            ms(160),
            ms(200),
            ms(200)
        ]
    );
    assert_eq!(restart_pause(u32::MAX), RESTART_PAUSE_MAX);
}

/// One call asks the app's directory for each level once and asks which apps
/// run once: reading a note finds the action that reads it among the levels
/// its path was resolved through, and running an action resolves its path in
/// the app it looked up, without asking either again.
#[test]
fn a_call_asks_for_each_directory_level_and_the_running_apps_once() {
    let host = Notes::new(true);
    run(async {
        let (text, failed) = tool(&agent(&host), "tilo_read", json!({"path": "notes/Todo"})).await;
        assert!(!failed, "{text}");
        let asked = host.take_asked();
        let mut once = asked.clone();
        once.sort();
        once.dedup();
        assert_eq!(asked.len(), once.len(), "asked twice: {asked:#?}");

        let (text, failed) = tool(
            &agent(&host),
            "tilo_do",
            json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "x"}}),
        )
        .await;
        assert!(!failed, "{text}");
        let asked = host.take_asked();
        assert_eq!(
            asked.iter().filter(|asked| *asked == "apps").count(),
            1,
            "{asked:#?}"
        );
    });
}

/// An app whose directory does not answer leaves a search with what the
/// others hold, and says it was not searched: one silent app does not fail
/// every search.
#[test]
fn a_search_answers_from_the_apps_that_answer_and_names_the_one_that_did_not() {
    let host = Notes::new(true);
    *host.silent.lock().unwrap() = Some(Vec::new());
    let answered = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(tool(
            &agent(&host),
            "tilo_find",
            json!({"query": "append paragraph"}),
        ));
    let (text, failed) = answered;
    assert!(!failed, "{text}");
    assert!(
        text.lines()
            .any(|line| line.starts_with("notes/Todo/body#append-paragraph(text)")),
        "{text}"
    );
    assert!(
        text.ends_with("\nnot searched, the app did not answer: sheets"),
        "{text}"
    );
}

/// A search no app answered found nothing because it looked nowhere: it
/// fails and says so, rather than answering that nothing matches.
#[test]
fn a_search_no_app_answered_fails_rather_than_finding_nothing() {
    let host = Notes::new(true);
    *host.silent.lock().unwrap() = Some(Vec::new());
    let (text, failed) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(tool(
            &agent(&host),
            "tilo_find",
            json!({"query": "append paragraph", "app": "sheets"}),
        ));
    assert!(failed, "{text}");
    assert!(!text.contains("nothing matches"), "{text}");
    assert_eq!(
        text, "not searched, the app did not answer: sheets; try again",
        "{text}"
    );
}
