//! An agent's turns against a Word Processor shaped like the one the guest
//! serves: the open-with-a-file call, the directory its reply carries,
//! session handles and multi-word needles.
use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionHostError, CatalogueProvider, CatalogueResource, InvocationReceipt, InvocationState,
    Pending,
};
use tilo_actions::{ActionParam, InvocationUpdate, ParamType};

/// Ids shaped like the bound UI resources a live Word Processor publishes.
const RICH: &str = "tilo-ui-bound:v1:2a4120d7-2f63-4578-aed8-11cb91ed3628:c0fe4ad7-b7d0-46c3-ab7e-78d12d5d86f4:71646f632e726963682d74657874";
const DOC: &str = "tilo-ui-bound:v1:2a4120d7-2f63-4578-aed8-11cb91ed3628:c0fe4ad7-b7d0-46c3-ab7e-78d12d5d86f4:776f72642d70726f636573736f722e646f63756d656e74";
const STYLE: &str = "tilo-ui-bound:v1:2a4120d7-2f63-4578-aed8-11cb91ed3628:c0fe4ad7-b7d0-46c3-ab7e-78d12d5d86f4:776f72642d70726f636573736f722e7374796c653a626f6479";
const APP: &str = "so.tilo.word-processor";
const WORKSPACE: &str = "workspace:room";
const PERMISSIONS: &str = "workspace.permissions:90142c0f-1092-4d22-84a2-f9af0e57dab9";

/// A workspace offering one app, which publishes its directory as soon as
/// it is opened.
#[derive(Clone)]
struct Writer {
    /// The app's action that opens a path, if it has one.
    opener: Option<&'static str>,
    running: Arc<Mutex<bool>>,
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
    /// For each of the app's next top-level listings, whether it is empty,
    /// as while the app publishes again during its start.
    blank_listings: Arc<Mutex<std::collections::VecDeque<bool>>>,
    /// The text `read-text` answers; set, `read-text` declares itself the
    /// document's read.
    document: Option<String>,
    /// Rows per page of a resource's listing, as when the host's byte
    /// budget cuts it short.
    listing_page: Option<usize>,
    /// Every question and invocation the host was sent, in order.
    log: Arc<Mutex<Vec<Asked>>>,
}

/// One request a [`Writer`] was sent.
#[derive(Clone, Debug, PartialEq)]
enum Asked {
    Query(Box<CatalogueQuery>),
    Invoke { revision: Option<String> },
}

impl Writer {
    fn new(opener: Option<&'static str>) -> Self {
        Self {
            opener,
            running: Arc::default(),
            invoked: Arc::default(),
            blank_listings: Arc::default(),
            document: None,
            listing_page: None,
            log: Arc::default(),
        }
    }

    /// What the host was sent since the last call, and forget it.
    fn asked(&self) -> Vec<Asked> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    fn reading(mut self, document: impl Into<String>) -> Self {
        self.document = Some(document.into());
        self
    }

    fn invoked(&self) -> Vec<(String, String, String, Value)> {
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
                    request.target.provider.clone(),
                    request.target.address.resource.clone(),
                    request.target.address.action.clone(),
                    arguments,
                )
            })
            .collect()
    }

    fn rows(&self, provider: &str) -> Vec<CatalogueRow> {
        if provider == WORKSPACE {
            let mut open = row(
                "",
                "app.so.tilo.word-processor",
                "applications.app",
                "open",
                Vec::new(),
            );
            open.app_id = "so.tilo.desktop".into();
            let mut assign = row(
                "",
                PERMISSIONS,
                "workspace.permissions",
                "profile.assign",
                Vec::new(),
            );
            assign.app_id = "so.tilo.desktop".into();
            return vec![open, assign];
        }
        let path = || ActionParam::optional("path", ParamType::Text, "Direct filesystem path");
        let mut rows =
            vec![
                row(
                    "",
                    RICH,
                    "qdoc.rich-text",
                    "apply",
                    vec![
                        ActionParam::required("action", ParamType::Object, "An edit")
                            .with_schema(json!({"oneOf": []})),
                    ],
                ),
                CatalogueRow {
                    action: None,
                    has_children: true,
                    ..row("", DOC, "word_processor.document", "", Vec::new())
                },
                row(
                    DOC,
                    DOC,
                    "word_processor.document",
                    "read-text",
                    vec![
                        ActionParam::required(
                            "pageSize",
                            ParamType::Integer,
                            "Maximum items in this page",
                        )
                        .with_schema(json!({"type": "integer", "minimum": 1, "maximum": 64})),
                        ActionParam::optional("cursor", ParamType::Text, "Opaque nextCursor"),
                    ],
                ),
                row(DOC, DOC, "word_processor.document", "save-as", vec![path()]),
                row(
                    DOC,
                    DOC,
                    "word_processor.document",
                    "select-block",
                    vec![ActionParam::required("block", ParamType::Text, "Paragraph")],
                ),
                apply_style_row(),
                row(
                    DOC,
                    DOC,
                    "word_processor.document",
                    "set-paragraph-style",
                    vec![
                        ActionParam::required("kind", ParamType::Text, "Block kind")
                            .with_choices(["paragraph", "heading", "list_item"]),
                        ActionParam::optional("level", ParamType::Integer, "Heading level"),
                    ],
                ),
                row(
                    DOC,
                    STYLE,
                    "word_processor.style",
                    "toggle-bold",
                    Vec::new(),
                ),
            ];
        if let Some(opener) = self.opener {
            rows.push(row(
                DOC,
                DOC,
                "word_processor.document",
                opener,
                vec![path()],
            ));
        }
        for action in rows.iter_mut().filter_map(|row| row.action.as_mut()) {
            if action.address.action == "set-paragraph-style" {
                action.description = Some(
                    "Make your selected paragraphs (or the caret paragraph) a heading, list or body paragraph"
                        .into(),
                );
            }
        }
        rows
    }
}

/// An action taking another resource: a handle there stands for it.
fn apply_style_row() -> CatalogueRow {
    row(
        DOC,
        DOC,
        "word_processor.document",
        "apply-style",
        vec![ActionParam::required("style", ParamType::Resource, "Style")],
    )
}

fn row(
    parent: &str,
    resource: &str,
    kind: &str,
    action: &str,
    params: Vec<ActionParam>,
) -> CatalogueRow {
    let label = if kind == "word_processor.style" {
        "body"
    } else {
        "Document"
    };
    CatalogueRow {
        provider: String::new(),
        app_id: APP.into(),
        parent_resource: parent.into(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: kind.into(),
            label: label.into(),
            presentation: None,
        },
        action: (!action.is_empty()).then(|| tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(resource, action),
            resource_kind: kind.into(),
            resource_label: label.into(),
            action_label: action.replace('-', " "),
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            input: tilo_actions::InputSpec::None,
            params,
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

/// The host's own needle: a substring of an id, label or parameter name,
/// anywhere below the provider's root.
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

impl ActionHost for Writer {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.log
            .lock()
            .unwrap()
            .push(Asked::Query(Box::new(query.clone())));
        let running = *self.running.lock().unwrap();
        let Some(provider) = query.provider.clone() else {
            return Pending::ready(Ok(CataloguePage {
                revision: "v5".into(),
                providers: running
                    .then(|| CatalogueProvider {
                        provider: "1".into(),
                        app_id: APP.into(),
                        room: "room".into(),
                        record_id: String::new(),
                        browsable: true,
                    })
                    .into_iter()
                    .collect(),
                ..CataloguePage::default()
            }));
        };
        if provider != WORKSPACE && !running {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let listing =
            query.target.is_none() && query.needle.is_empty() && query.parent_resource.is_none();
        if provider != WORKSPACE
            && listing
            && self.blank_listings.lock().unwrap().pop_front() == Some(true)
        {
            return Pending::ready(Ok(CataloguePage {
                revision: "v5".into(),
                exhausted: true,
                ..CataloguePage::default()
            }));
        }
        let mut rows = self.rows(&provider);
        for row in &mut rows {
            row.provider.clone_from(&provider);
            if let Some(action) = row
                .action
                .as_mut()
                .filter(|action| action.address.action == "read-text" && self.document.is_some())
            {
                action
                    .contract
                    .tags
                    .push(tilo_actions::DOCUMENT_READ_TAG.into());
            }
        }
        if let Some(target) = &query.target {
            rows.retain(|row| row.action.as_ref().is_some_and(|a| a.address == *target));
        } else if query.needle.is_empty() {
            let parent = query.parent_resource.clone().unwrap_or_default();
            rows.retain(|row| row.parent_resource == parent);
        } else {
            rows.retain(|row| host_matches(row, &query.needle));
        }
        if let Some(size) = self
            .listing_page
            .filter(|_| query.parent_resource.is_some())
        {
            let offset: usize = String::from_utf8_lossy(&query.cursor).parse().unwrap_or(0);
            let next = offset.saturating_add(size);
            let exhausted = next >= rows.len();
            return Pending::ready(Ok(CataloguePage {
                revision: "v5".into(),
                rows: rows.into_iter().skip(offset).take(size).collect(),
                next_cursor: if exhausted {
                    Vec::new()
                } else {
                    next.to_string().into_bytes()
                },
                exhausted,
                budget_exhausted: !exhausted,
                ..CataloguePage::default()
            }));
        }
        rows.truncate(query.page_size());
        Pending::ready(Ok(CataloguePage {
            revision: "v5".into(),
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
        self.log.lock().unwrap().push(Asked::Invoke {
            revision: request.revision.clone(),
        });
        let known = self
            .rows(&request.target.provider)
            .iter()
            .any(|row| row.resource.id == request.target.address.resource);
        // As the broker refuses an instance that is no longer live.
        let gone = request.target.provider != WORKSPACE && !*self.running.lock().unwrap();
        if !known || gone {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        if request.target.provider == WORKSPACE {
            if !matches!(&request.argument, InvokeArgument::Json(map) if map.is_empty()) {
                return Pending::ready(Err(ActionHostError::Refused {
                    reason: "launch content requires an application the launcher authenticated"
                        .into(),
                }));
            }
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
        let page_size = arguments.get("pageSize").and_then(Value::as_u64);
        // As the broker validates arguments against the published schema.
        let refusal = if page_size.is_some_and(|size| size > 64) {
            Some("argument `pageSize` must be at most 64")
        } else if arguments
            .get("block")
            .and_then(Value::as_str)
            .is_some_and(|block| block.starts_with('r'))
        {
            Some("no block has that id")
        } else {
            None
        };
        let update = if let Some(message) = refusal {
            InvocationUpdate::Failed {
                invocation: *id,
                error: tilo_actions::ActionFailure {
                    code: "invalid_arguments".into(),
                    message: message.into(),
                    details: None,
                    retryable: false,
                },
            }
        } else {
            let value = self.document.as_ref().filter(|_| action == "read-text").map(
                |markdown| json!({"version": 1, "content": {"markdown": markdown}, "nextCursor": "c2"}),
            );
            InvocationUpdate::Succeeded {
                invocation: *id,
                output: tilo_actions::ActionOutput {
                    value,
                    ..tilo_actions::ActionOutput::default()
                },
            }
        };
        Some(InvocationState::new(*id, update))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        Ok(())
    }
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// One turn's tool call: its structured result, or its error text.
async fn call(server: &McpServer<Writer>, name: &str, arguments: Value) -> Result<Value, String> {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .await
        .unwrap();
    let result = &reply["result"];
    if result["isError"] == true {
        return Err(result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned());
    }
    Ok(result["structuredContent"].clone())
}

/// The resource a compact page names `id` by, and that resource's actions.
fn resource<'a>(page: &'a Value, kind: &str) -> &'a Value {
    page["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|resource| resource["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} in {page}"))
}

#[test]
fn opening_an_app_with_a_file_opens_it_through_the_apps_own_action() {
    let host = Writer::new(Some("open"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        let reply = call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/paper.docx"}}),
        )
        .await
        .unwrap();
        assert_eq!(reply["update"]["state"], "succeeded", "{reply}");
        assert_eq!(reply["peer_id"], 1, "{reply}");
        assert!(reply["opened"].get("error").is_none(), "{reply}");
        assert_eq!(reply["opened"]["update"]["state"], "succeeded", "{reply}");
        assert_eq!(reply["opened"]["action"], "open", "{reply}");

        let document = resource(&reply["directory"], "word_processor.document");
        assert_eq!(reply["opened"]["resource"], document["resource"]);
        let actions = &document["actions"];
        let style = actions["set-paragraph-style"].as_str().unwrap();
        assert!(
            !style.contains('—'),
            "an open directory leaves descriptions to tilo_query: {style}"
        );
        assert_eq!(
            actions["read-text"], "read text (pageSize:integer 1..64, cursor?)",
            "a bound the app enforces is shown before a call trips over it"
        );
        assert!(
            resource(&reply["directory"], "word_processor.style")["actions"]["toggle-bold"]
                .is_string()
        );
        assert!(resource(&reply["directory"], "qdoc.rich-text")["actions"]["apply"].is_string());
        assert!(reply["directory"].get("truncated").is_none(), "{reply}");
        assert!(reply["directory"].to_string().len() < DIRECTORY_BYTES);
    });
    // The workspace launched the app empty; the app itself read the file,
    // inside its sandbox, under the grants it holds.
    assert_eq!(
        host.invoked(),
        [
            (
                WORKSPACE.into(),
                "app.so.tilo.word-processor".into(),
                "open".into(),
                json!({})
            ),
            (
                "1".into(),
                DOC.into(),
                "open".into(),
                json!({"path": "/data/in/paper.docx"})
            ),
        ]
    );
}

/// computer (run 20260930-105710): every task that opened a file spent its
/// next turn reading it.
#[test]
fn opening_a_file_answers_its_first_page_through_the_apps_declared_read() {
    let text = "# Quarterly Report {#b0}\nOur comittee met. {#b1}\n";
    let host = Writer::new(Some("open")).reading(text);
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/report.docx"}}),
    ))
    .unwrap();
    let content = &reply["content"];
    let document = resource(&reply["directory"], "word_processor.document");
    assert_eq!(content["resource"], document["resource"], "{reply}");
    assert_eq!(content["action"], "read-text", "{reply}");
    assert_eq!(content["value"]["content"]["markdown"], text, "{reply}");
    assert_eq!(
        content["value"]["nextCursor"], "c2",
        "a whole page keeps its cursor"
    );
    assert!(content.get("truncated").is_none(), "{reply}");
    let calls = host.invoked();
    assert_eq!(
        calls.last().unwrap(),
        &("1".into(), DOC.into(), "read-text".into(), json!({})),
        "the read runs with no arguments: {calls:?}"
    );

    let empty = Writer::new(Some("open")).reading(text);
    let server = McpServer::new(empty.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
    ))
    .unwrap();
    assert!(reply.get("content").is_none(), "no file, no read: {reply}");
    assert!(empty.invoked().iter().all(|call| call.2 != "read-text"));
}

#[test]
fn a_long_first_page_is_cut_and_drops_its_cursor() {
    let text = (0..500)
        .map(|n| format!("Paragraph {n} of a long report. {{#b{n}}}\n"))
        .collect::<Vec<_>>()
        .concat();
    let host = Writer::new(Some("open")).reading(text);
    let server = McpServer::new(host, "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/long.docx"}}),
    ))
    .unwrap();
    let content = &reply["content"];
    assert_eq!(content["truncated"], true, "{content}");
    assert!(content["value"].to_string().len() <= content::CONTENT_BYTES);
    let markdown = content["value"]["content"]["markdown"].as_str().unwrap();
    assert!(markdown.starts_with("Paragraph 0 of a long report. {#b0}\n"));
    assert!(content["value"].get("nextCursor").is_none(), "{content}");
}

/// Only a file opened is read: an app that could not open it answers no
/// page of a document it does not hold.
#[test]
fn a_file_the_app_could_not_open_is_not_read() {
    let host = Writer::new(None).reading("stale");
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/paper.docx"}}),
    ))
    .unwrap();
    assert!(reply["opened"]["error"].is_string(), "{reply}");
    assert!(reply.get("content").is_none(), "{reply}");
    assert!(host.invoked().iter().all(|call| call.2 != "read-text"));
}

#[test]
fn an_app_without_open_imports_the_file_by_its_extension() {
    let host = Writer::new(Some("import-csv"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/Sales.CSV"}}),
    ))
    .unwrap();
    assert_eq!(reply["opened"]["action"], "import-csv", "{reply}");
    assert_eq!(host.invoked()[1].3, json!({"path": "/data/in/Sales.CSV"}));
}

/// workflows (guest run 20260929-200306): opening a workbook Sheets already
/// held started a second window that imported it again.
#[test]
fn opening_a_file_an_instance_holds_returns_that_instance() {
    let host = Writer::new(Some("import-csv"));
    let open_files = Arc::new(OpenFiles::default());
    let session = || {
        McpServer::new(host.clone(), "room".into(), "owner".into())
            .with_open_files(Arc::clone(&open_files))
    };
    let (first, second) = (session(), session());
    let open = |path: &str| json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": path}});
    run(async {
        // Case `a_workspace_open_is_answered_by_the_instance_it_opened_in`.
        {
            call(&first, "tilo_invoke", open("/data/in/Sales.csv"))
                .await
                .unwrap();
            let again = call(&second, "tilo_invoke", open("/data/in/Sales.csv"))
                .await
                .unwrap();
            assert_eq!(again["peer_id"], 1, "{again}");
            assert_eq!(again["update"]["state"], "succeeded", "{again}");
            assert_eq!(again["opened"]["already_open"], true, "{again}");
            assert!(again["directory"]["resources"].is_array(), "{again}");
        }

        // Case `a_file_imported_directly_is_found_too`.
        {
            call(&first, "tilo_invoke", json!({"peer_id": 1, "resource": DOC, "action": "import-csv", "arguments": {"path": "/data/in/Costs.csv"}}))
                .await
                .unwrap();
            let again = call(&second, "tilo_invoke", open("/data/in/Costs.csv"))
                .await
                .unwrap();
            assert_eq!(again["opened"]["already_open"], true, "{again}");
        }
    });
    let calls = host.invoked();
    let launches = calls.iter().filter(|call| call.0 == WORKSPACE).count();
    assert_eq!(launches, 1, "nothing started a second instance: {calls:?}");
    let imports = calls
        .iter()
        .filter(|call| call.2 == "import-csv")
        .map(|call| call.3["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        imports,
        ["/data/in/Sales.csv", "/data/in/Costs.csv"],
        "each file was read once"
    );
}

/// computer (run 20260930-071012, doc-create-memo): opened without a file,
/// the Word Processor's `open` answered `"resources": []`, and the agent
/// spent two query turns on its directory.
#[test]
fn an_app_opened_empty_answers_once_its_directory_lists_a_resource() {
    let host = Writer::new(None);
    host.blank_listings
        .lock()
        .unwrap()
        .extend([false, true, true]);
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
    ))
    .unwrap();
    assert!(
        resource(&reply["directory"], "word_processor.document")["children"] == true,
        "{reply}"
    );
    assert!(host.blank_listings.lock().unwrap().is_empty());
}

#[test]
fn an_app_that_cannot_open_a_path_says_so_and_still_opens() {
    let host = Writer::new(None);
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/paper.docx"}}),
    ))
    .unwrap();
    assert_eq!(reply["peer_id"], 1, "{reply}");
    assert!(reply["opened"]["error"].is_string(), "{reply}");
    assert!(reply["directory"]["resources"].is_array(), "{reply}");
    assert_eq!(host.invoked().len(), 1, "nothing but the launch ran");
    assert!(
        run(call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": 7}}),
        ))
        .is_err()
    );
}

#[test]
fn the_workspace_page_offers_a_path_on_opening_an_app() {
    let server = McpServer::new(Writer::new(Some("open")), "room".into(), "owner".into());
    let page = run(call(&server, "tilo_query", json!({"workspace": true}))).unwrap();
    assert_eq!(
        resource(&page, "applications.app")["actions"]["open"],
        "open (path?)",
        "{page}"
    );
    let full = run(call(
        &server,
        "tilo_query",
        json!({"workspace": true, "detail": "full"}),
    ))
    .unwrap();
    assert_eq!(
        full["rows"][0]["descriptor"]["params"],
        json!([]),
        "version 1 rows are as published"
    );
}

/// The reference partner proxy provisions file grants from a compact
/// workspace page: the permission roles' resource, and each app named by the
/// `app.` prefix of its resource. A handle there left every app ungranted.
#[test]
fn workspace_pages_name_their_resources_in_full() {
    let server = McpServer::new(Writer::new(Some("open")), "room".into(), "owner".into());
    let page = run(call(&server, "tilo_query", json!({"workspace": true}))).unwrap();
    let names = page["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["resource"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["app.so.tilo.word-processor", PERMISSIONS], "{page}");
    let found = run(call(
        &server,
        "tilo_query",
        json!({"workspace": true, "needle": "permission assign"}),
    ))
    .unwrap();
    assert_eq!(found["resources"][0]["resource"], PERMISSIONS, "{found}");
}

/// computer (run 20260930-071012, deck-launch): `set-slide-text` was refused
/// `slide: "r1"`, the directory's handle for the slide.
#[test]
fn a_handle_in_a_resource_argument_is_expanded_and_elsewhere_named_in_full() {
    let host = Writer::new(Some("open"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
        )
        .await
        .unwrap();
        let page = call(&server, "tilo_query", json!({"peer_id": 1}))
            .await
            .unwrap();
        let doc = resource(&page, "word_processor.document")["resource"]
            .as_str()
            .unwrap()
            .to_owned();
        let style = resource(&page, "qdoc.rich-text")["resource"]
            .as_str()
            .unwrap()
            .to_owned();
        let styled = call(&server, "tilo_invoke", json!({"peer_id": 1, "resource": doc, "action": "apply-style", "arguments": {"style": style}}))
            .await
            .unwrap();
        assert_eq!(styled["update"]["state"], "succeeded", "{styled}");
        assert!(styled.get("hint").is_none(), "{styled}");

        let selected = call(&server, "tilo_invoke", json!({"peer_id": 1, "resource": doc, "action": "select-block", "arguments": {"block": style}}))
            .await
            .unwrap();
        assert_eq!(selected["update"]["state"], "succeeded", "{selected}");
        let adjusted = selected["adjusted"].as_str().unwrap_or_default();
        assert!(
            adjusted.contains(&format!("block {style} was refused; used its id {RICH}")),
            "{selected}"
        );
    });
    let calls = host.invoked();
    let arguments = |action: &str| {
        calls
            .iter()
            .filter(|call| call.2 == action)
            .map(|call| call.3.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(arguments("apply-style"), [json!({"style": RICH})]);
    let selects = arguments("select-block");
    assert_eq!(selects.len(), 2, "{selects:?}");
    assert!(
        selects[0]["block"]
            .as_str()
            .is_some_and(|handle| handle.starts_with('r')),
        "a text argument goes as given first: {selects:?}"
    );
    assert_eq!(selects[1], json!({"block": RICH}));
}

#[test]
fn handles_stand_for_long_ids_and_full_ids_keep_working() {
    let host = Writer::new(Some("open"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
        )
        .await
        .unwrap();
        let page = call(&server, "tilo_query", json!({"peer_id": 1}))
            .await
            .unwrap();
        let rich = resource(&page, "qdoc.rich-text")["resource"]
            .as_str()
            .unwrap()
            .to_owned();
        let doc = resource(&page, "word_processor.document")["resource"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(rich.starts_with('r') && rich.len() < 6, "{page}");
        assert!(
            !page.to_string().contains(DOC),
            "no long id is repeated: {page}"
        );
        let again = call(&server, "tilo_query", json!({"peer_id": 1}))
            .await
            .unwrap();
        assert_eq!(page, again, "a handle names the same resource all session");

        let children = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "parent_resource": doc}),
        )
        .await
        .unwrap();
        assert!(
            resource(&children, "word_processor.document")["actions"]["save-as"].is_string(),
            "{children}"
        );
        let resolved = call(
            &server,
            "tilo_resolve",
            json!({"peer_id": 1, "resource": doc, "action": "save-as"}),
        )
        .await
        .unwrap();
        assert_eq!(
            resolved["row"]["resource"], doc,
            "a resolved contract names the resource by its handle"
        );
        assert!(!resolved.to_string().contains(DOC), "{resolved}");
        for name in [doc.as_str(), DOC] {
            call(&server, "tilo_invoke", json!({"peer_id": 1, "resource": name, "action": "save-as", "arguments": {"path": "/data/out/a.docx"}}))
                .await
                .unwrap();
        }
        let full = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "detail": "full"}),
        )
        .await
        .unwrap();
        assert_eq!(
            full["rows"][0]["resource"], RICH,
            "version 1 rows carry full ids"
        );
    });
    let saves = host
        .invoked()
        .into_iter()
        .filter(|call| call.2 == "save-as")
        .collect::<Vec<_>>();
    assert_eq!(saves.len(), 2);
    assert!(saves.iter().all(|call| call.1 == DOC), "{saves:?}");
    assert_eq!(
        page::Handles::default().short("sheets.workbook"),
        "sheets.workbook"
    );
}

#[test]
fn a_needle_no_row_matches_whole_finds_the_rows_matching_most_of_it() {
    let server = McpServer::new(Writer::new(Some("open")), "room".into(), "owner".into());
    run(async {
        call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
        )
        .await
        .unwrap();
        let page = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "open paragraph style heading"}),
        )
        .await
        .unwrap();
        assert_eq!(page["partial_match"], true, "{page}");
        let document = resource(&page, "word_processor.document");
        let style = document["actions"]["set-paragraph-style"].as_str().unwrap();
        assert!(
            style.contains('—'),
            "a query keeps the description an open directory leaves out: {style}"
        );
        let whole = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "paragraph style"}),
        )
        .await
        .unwrap();
        assert!(whole.get("partial_match").is_none(), "{whole}");
        let full = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "open paragraph style heading", "detail": "full"}),
        )
        .await
        .unwrap();
        assert_eq!(
            full["rows"],
            json!([]),
            "version 1 keeps its every-word needle"
        );
        let nothing = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "zebra quokka"}),
        )
        .await
        .unwrap();
        assert_eq!(nothing["resources"], json!([]), "{nothing}");
    });
}

#[test]
fn a_found_resource_lists_every_action_across_the_hosts_pages() {
    let writer = Writer {
        listing_page: Some(2),
        ..Writer::new(Some("open"))
    };
    let server = McpServer::new(writer, "room".into(), "owner".into());
    run(async {
        call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
        )
        .await
        .unwrap();
        let page = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "save"}),
        )
        .await
        .unwrap();
        let actions = resource(&page, "word_processor.document")["actions"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for action in [
            "read-text",
            "save-as",
            "select-block",
            "apply-style",
            "set-paragraph-style",
            "open",
        ] {
            assert!(actions.contains(action), "{action}: {page}");
        }
        let capped = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "save", "max_results": 1}),
        )
        .await
        .unwrap();
        let actions = resource(&capped, "word_processor.document")["actions"]
            .as_object()
            .unwrap();
        assert_eq!(
            actions.keys().collect::<Vec<_>>(),
            ["save-as"],
            "max_results caps the rows: {capped}"
        );
    });
}

#[test]
fn a_session_pinned_to_full_rows_gets_no_directory() {
    let server = McpServer::new(Writer::new(Some("open")), "room".into(), "owner".into());
    run(async {
        server
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"experimental":{"so.tilo.mcp":{"detail":"full"}}}}}))
            .await
            .unwrap();
        let reply = call(&server, "tilo_invoke", json!({"workspace": true, "revision": "0.0", "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {}}))
            .await
            .unwrap();
        assert_eq!(reply["peer_id"], 1, "{reply}");
        assert!(reply.get("directory").is_none(), "{reply}");
    });
}

/// doc-real-headings (guest run 20260929-142555) took 12 turns: the open
/// with a file was refused, then three `tilo_query` turns found the
/// document's actions and a read failed on `pageSize`. The same task now
/// needs open, read, edit-and-save.
#[test]
fn doc_real_headings_takes_three_tilo_turns() {
    let host = Writer::new(Some("open"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let turns = run(async {
        let mut turns = 0;
        turns += 1;
        let opened = call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/paper.docx"}}),
        )
        .await
        .unwrap();
        let document = resource(&opened["directory"], "word_processor.document");
        let doc = document["resource"].as_str().unwrap().to_owned();
        let read = &document["actions"]["read-text"];
        assert!(read.as_str().unwrap().contains("1..64"));

        turns += 1;
        call(&server, "tilo_invoke", json!({"peer_id": 1, "resource": doc, "action": "read-text", "arguments": {"pageSize": 64}}))
            .await
            .unwrap();

        turns += 1;
        for block in ["b.0", "b.2", "b.4"] {
            call(&server, "tilo_invoke", json!({"peer_id": 1, "resource": doc, "action": "select-block", "arguments": {"block": block}}))
                .await
                .unwrap();
            call(&server, "tilo_invoke", json!({"peer_id": 1, "resource": doc, "action": "set-paragraph-style", "arguments": {"kind": "heading", "level": 2}}))
                .await
                .unwrap();
        }
        call(&server, "tilo_invoke", json!({"peer_id": 1, "resource": doc, "action": "save-as", "arguments": {"path": "/data/out/paper.docx"}}))
            .await
            .unwrap();
        turns
    });
    assert_eq!(turns, 3);
    let calls = host.invoked();
    assert!(
        calls
            .iter()
            .skip(1)
            .all(|call| call.0 == "1" && call.1 == DOC),
        "{calls:?}"
    );
    assert_eq!(calls.len(), 10);
}

/// cross-standup asked `read-text` for 200 blocks and was refused; the
/// bridge now reads the most the action allows and says so.
#[test]
fn a_number_beyond_its_bound_is_clamped_and_the_answer_says_so() {
    let host = Writer::new(Some("open"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(async {
        call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
        )
        .await
        .unwrap();
        call(
            &server,
            "tilo_invoke",
            json!({"peer_id": 1, "resource": DOC, "action": "read-text", "arguments": {"pageSize": 200}}),
        )
        .await
        .unwrap()
    });
    assert_eq!(reply["update"]["state"], "succeeded", "{reply}");
    assert_eq!(reply["adjusted"], "pageSize 200 is out of range; used 64");
    let calls = host.invoked();
    let sizes: Vec<&Value> = calls
        .iter()
        .filter(|call| call.2 == "read-text")
        .map(|call| &call.3["pageSize"])
        .collect();
    assert_eq!(sizes, [&json!(200), &json!(64)]);
}

/// Edits and the save that follows are one `tilo_invoke`. A failure stops
/// the batch, and the single-call form is unchanged.
#[test]
fn calls_run_in_order_and_stop_at_the_first_failure() {
    let host = Writer::new(Some("open")).reading("# Memo {#b0}\n");
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let open = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/memo.docx"}}),
    ))
    .unwrap();
    assert!(
        open.get("results").is_none(),
        "one action stays one reply: {open}"
    );
    assert_eq!(open["content"]["action"], "read-text");
    let doc = resource(&open["directory"], "word_processor.document")["resource"]
        .as_str()
        .unwrap()
        .to_owned();

    let saved = run(call(
        &server,
        "tilo_invoke",
        json!({"peer_id": 1, "calls": [
            {"resource": doc, "action": "paste", "arguments": {"markdown": "# Done {#b0}\n"}},
            {"resource": doc, "action": "save-as", "arguments": {"path": "/data/out/memo.docx"}}
        ]}),
    ))
    .unwrap();
    assert_eq!(saved["results"].as_array().unwrap().len(), 2, "{saved}");
    assert!(saved.get("stopped").is_none(), "{saved}");
    assert!(
        saved["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step.get("update").is_none() && step.get("error").is_none()),
        "a step that succeeded is its output alone: {saved}"
    );

    let before = host.invoked().len();
    let stopped = run(call(
        &server,
        "tilo_invoke",
        json!({"peer_id": 1, "calls": [
            {"resource": doc, "action": "select-block", "arguments": {"block": "r-no-such"}},
            {"resource": doc, "action": "save-as", "arguments": {"path": "/data/out/should-not.docx"}}
        ]}),
    ))
    .unwrap();
    assert_eq!(stopped["stopped"], 0, "{stopped}");
    assert_eq!(stopped["results"].as_array().unwrap().len(), 1, "{stopped}");
    assert_eq!(stopped["results"][0]["update"]["state"], "failed");
    assert!(
        host.invoked()
            .iter()
            .skip(before)
            .all(|call| call.2 != "save-as"),
        "the save after a failure does not run: {:?}",
        host.invoked()
    );

    let kept = run(call(
        &server,
        "tilo_invoke",
        json!({"peer_id": 1, "calls": [
            {"resource": doc, "action": "paste", "arguments": {"markdown": "# Again {#b0}\n"}},
            {"resource": doc, "action": "select-block", "arguments": {"block": "r-no-such"}}
        ]}),
    ))
    .unwrap();
    assert_eq!(kept["stopped"], 1, "{kept}");
    assert!(
        kept["kept"]
            .as_str()
            .is_some_and(|note| note.contains("0..1")),
        "a batch stopped after a step that ran says that step stayed: {kept}"
    );
    assert!(stopped.get("kept").is_none(), "{stopped}");

    let both = run(call(
        &server,
        "tilo_invoke",
        json!({"peer_id": 1, "resource": doc, "action": "paste", "calls": [{"resource": doc, "action": "paste"}]}),
    ));
    assert!(both.unwrap_err().contains("not both"));
    let neither = run(call(&server, "tilo_invoke", json!({"peer_id": 1})));
    assert!(neither.unwrap_err().contains("or calls"));
}

/// The computer suite's shape, scripted: open, then edits and save-as in
/// one call. `get_computer` and `finish` are the proxy's and the harness's.
#[test]
fn scripted_replays_of_computer_suite_tasks() {
    // doc-create-memo: open empty, paste the memo, save-as.
    replay(
        "doc-create-memo",
        None,
        &json!([
            {"action": "paste", "arguments": {"markdown": "# Memo {#b0}\nShip Friday. {#b1}\n"}},
            {"action": "save-as", "arguments": {"path": "/data/out/memo.docx"}}
        ]),
    );
    // doc-fix-typos: open the file (its text comes back), fix, save-as.
    replay(
        "doc-fix-typos",
        Some("/data/in/notes.docx"),
        &json!([
            {"action": "apply-spelling-suggestion", "arguments": {"block": "b1", "start": 4, "end": 12, "replacement": "committee"}},
            {"action": "save-as", "arguments": {"path": "/data/out/notes.docx"}}
        ]),
    );
    // cross-standup: open the notes, replace them, save-as.
    replay(
        "cross-standup",
        Some("/data/in/notes.md"),
        &json!([
            {"action": "select-all", "arguments": {}},
            {"action": "paste", "arguments": {"markdown": "# Standup {#b0}\n"}},
            {"action": "save-as", "arguments": {"path": "/data/out/standup.docx"}}
        ]),
    );
}

fn replay(task: &str, path: Option<&str>, calls: &Value) {
    let host = Writer::new(Some("open")).reading("# Notes {#b0}\nThe comittee met. {#b1}\n");
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let mut open_args =
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"});
    if let Some(path) = path {
        open_args["arguments"] = json!({"path": path});
    }
    let open = run(call(&server, "tilo_invoke", open_args)).unwrap();
    let doc = resource(&open["directory"], "word_processor.document")["resource"]
        .as_str()
        .unwrap()
        .to_owned();
    if path.is_some() {
        assert!(
            open["content"]["value"]["content"]["markdown"]
                .as_str()
                .unwrap()
                .contains("comittee"),
            "{task}"
        );
    } else {
        assert!(open.get("content").is_none(), "{task}");
    }
    let mut batch = Vec::new();
    for call in calls.as_array().unwrap() {
        batch.push(json!({
            "resource": doc,
            "action": call["action"],
            "arguments": call["arguments"],
        }));
    }
    let saved = run(call(
        &server,
        "tilo_invoke",
        json!({"peer_id": open["peer_id"], "calls": batch}),
    ))
    .unwrap();
    assert!(saved.get("stopped").is_none(), "{task}: {saved}");
    assert_eq!(
        saved["results"].as_array().unwrap().len(),
        calls.as_array().unwrap().len(),
        "{task}"
    );
    let actions = host
        .invoked()
        .into_iter()
        .map(|call| call.2)
        .collect::<Vec<_>>();
    eprintln!(
        "{task}: get_computer; tilo_invoke open -> peer {} content {}; tilo_invoke calls {:?}; finish",
        open["peer_id"],
        open.get("content").is_some(),
        actions
            .iter()
            .skip(if path.is_some() { 3 } else { 1 })
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_batch_invocation_id_is_stable_per_step() {
    let id = "6a1e5c3a-9d2f-4a6b-8c1e-2f3a4b5c6d7e";
    assert_eq!(batch_invocation(Some(id), 0).unwrap().as_deref(), Some(id));
    let second = batch_invocation(Some(id), 1).unwrap().unwrap();
    assert_ne!(second, id);
    assert_eq!(
        batch_invocation(Some(id), 1).unwrap().as_deref(),
        Some(second.as_str())
    );
    assert!(batch_invocation(None, 2).unwrap().is_none());
    assert!(batch_invocation(Some("not-a-uuid"), 0).is_err());
}

#[test]
fn a_parent_resource_is_named_as_resources_are_named_elsewhere() {
    let host = Writer::new(Some("open"));
    *host.running.lock().unwrap() = true;
    // A fresh connection: no query has issued a handle yet.
    let server = McpServer::new(host, "room".into(), "owner".into());
    run(async {
        for parent in ["word_processor.document", DOC] {
            let children = call(
                &server,
                "tilo_query",
                json!({"app_id": APP, "parent_resource": parent}),
            )
            .await
            .unwrap();
            assert!(
                resource(&children, "word_processor.document")["actions"]["save-as"].is_string(),
                "{parent}: {children}"
            );
        }
        let page = call(&server, "tilo_query", json!({"app_id": APP}))
            .await
            .unwrap();
        let handle = resource(&page, "word_processor.document")["resource"].clone();
        let by_handle = call(
            &server,
            "tilo_query",
            json!({"app_id": APP, "parent_resource": handle}),
        )
        .await
        .unwrap();
        assert!(
            resource(&by_handle, "word_processor.style")["actions"]["toggle-bold"].is_string(),
            "{by_handle}"
        );

        let leaf = call(
            &server,
            "tilo_query",
            json!({"app_id": APP, "parent_resource": RICH}),
        )
        .await
        .unwrap();
        assert_eq!(
            leaf["resources"],
            json!([]),
            "a resource with nothing beneath it lists nothing"
        );

        for (unknown, says) in [
            ("r42", "no handle of this connection"),
            ("canvas.artwork", "nothing is listed under canvas.artwork"),
        ] {
            let refused = call(
                &server,
                "tilo_query",
                json!({"app_id": APP, "parent_resource": unknown}),
            )
            .await
            .unwrap_err();
            assert!(
                refused.contains(says) && refused.contains("Document (word_processor.document)"),
                "{refused}"
            );
        }
    });
}

#[test]
fn an_app_and_its_resources_are_named_as_an_agent_sees_them() {
    let host = Writer::new(Some("open"));
    *host.running.lock().unwrap() = true;
    // A fresh connection: no query has issued a handle yet.
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        let by_kind = json!({"app_id": "word-processor", "resource": "word_processor.document", "action": "save-as", "arguments": {"path": "/data/out/a.docx"}});
        call(&server, "tilo_invoke", by_kind).await.unwrap();

        let resolved = call(
            &server,
            "tilo_resolve",
            json!({"app_id": APP, "resource": "word_processor.document", "action": "read-text"}),
        )
        .await
        .unwrap();
        let param = &resolved["row"]["descriptor"]["params"][0];
        assert_eq!(param["schema"]["type"], "integer", "{resolved}");
        assert!(
            resolved["row"].get("visibility_action").is_none(),
            "nothing is said to be null: {resolved}"
        );
        assert!(
            param.get("ty").is_none() && param.get("nullable").is_none(),
            "the schema says the type once: {resolved}"
        );

        let unknown_app = call(&server, "tilo_query", json!({"app_id": "sheets"}))
            .await
            .unwrap_err();
        assert!(
            unknown_app.contains("running: so.tilo.word-processor"),
            "{unknown_app}"
        );

        let ambiguous = call(
            &server,
            "tilo_invoke",
            json!({"app_id": APP, "resource": "document", "action": "save-as"}),
        )
        .await
        .unwrap_err();
        assert!(ambiguous.contains("names 2 resources"), "{ambiguous}");

        let stale = call(
            &server,
            "tilo_invoke",
            json!({"app_id": APP, "resource": "r7", "action": "save-as"}),
        )
        .await
        .unwrap_err();
        assert!(
            stale.contains("no handle of this connection")
                && stale.contains("Document (word_processor.document)"),
            "{stale}"
        );

        let no_action = call(
            &server,
            "tilo_resolve",
            json!({"app_id": APP, "resource": "word_processor.document", "action": "print"}),
        )
        .await
        .unwrap_err();
        assert!(
            no_action.contains("has no action print") && no_action.contains("save-as"),
            "{no_action}"
        );
    });
    let saves = host
        .invoked()
        .into_iter()
        .filter(|call| call.2 == "save-as")
        .collect::<Vec<_>>();
    assert_eq!(saves.len(), 1, "{saves:?}");
    assert_eq!(saves[0].1, DOC, "the kind named the document");
}

/// Whether `asked` is a question about which instances run: providers
/// only, so a host builds no rows for it.
fn lists_instances(asked: &Asked) -> bool {
    matches!(asked, Asked::Query(query) if query.provider.is_none() && query.max_results == 0)
}

/// A started action, as the host saw it: a call naming no revision passes
/// none on.
const STARTED: Asked = Asked::Invoke { revision: None };

fn toggle_bold() -> Value {
    json!({"resource": STYLE, "action": "toggle-bold"})
}

/// TIL-588: an agent that listed or browsed an instance acts on it without
/// asking the host anything first.
#[test]
fn invoke_on_a_seen_peer_asks_no_catalogue_question() {
    let host = Writer::new(None);
    *host.running.lock().unwrap() = true;
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "bold"}),
        )
        .await
        .unwrap();
        host.asked();
        let reply = call(
            &server,
            "tilo_invoke",
            json!({"peer_id": 1, "resource": STYLE, "action": "toggle-bold"}),
        )
        .await
        .unwrap();
        assert_eq!(reply["update"]["state"], "succeeded", "{reply}");
    });
    assert_eq!(host.asked(), [STARTED]);
}

/// TIL-588: a batch asks at most once before its first step, and never
/// between steps; listing instances asks for providers only.
#[test]
fn a_batch_asks_at_most_once_before_its_first_step() {
    let host = Writer::new(None);
    *host.running.lock().unwrap() = true;
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let steps = (0..10).map(|_| toggle_bold()).collect::<Vec<_>>();
    run(async {
        call(&server, "tilo_list_apps", json!({})).await.unwrap();
        let listed = host.asked();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(lists_instances(&listed[0]), "{listed:?}");

        let reply = call(
            &server,
            "tilo_invoke",
            json!({"peer_id": 1, "calls": steps}),
        )
        .await
        .unwrap();
        assert!(reply.get("stopped").is_none(), "{reply}");
    });
    assert_eq!(host.asked(), vec![STARTED; 10]);

    // A session that never listed them, naming the app by id: one list.
    let fresh = Writer::new(None);
    *fresh.running.lock().unwrap() = true;
    let server = McpServer::new(fresh.clone(), "room".into(), "owner".into());
    let steps = (0..10).map(|_| toggle_bold()).collect::<Vec<_>>();
    run(async {
        let reply = call(
            &server,
            "tilo_invoke",
            json!({"app_id": APP, "calls": steps}),
        )
        .await
        .unwrap();
        assert!(reply.get("stopped").is_none(), "{reply}");
    });
    let asked = fresh.asked();
    assert_eq!(asked.len(), 11, "{asked:?}");
    assert!(lists_instances(&asked[0]), "{asked:?}");
    assert_eq!(asked[1..], vec![STARTED; 10]);
}

/// TIL-588: an instance the session saw that has gone since is refused as
/// before, by asking once the host refused the call; a browse never takes
/// it as live.
#[test]
fn a_seen_peer_that_has_gone_is_refused_as_unavailable() {
    let host = Writer::new(None);
    *host.running.lock().unwrap() = true;
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "bold"}),
        )
        .await
        .unwrap();
        *host.running.lock().unwrap() = false;
        host.asked();
        let refused = call(
            &server,
            "tilo_invoke",
            json!({"peer_id": 1, "resource": STYLE, "action": "toggle-bold"}),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, "Unavailable live application instance");
        let asked = host.asked();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[0], STARTED);
        assert!(lists_instances(&asked[1]), "{asked:?}");

        // Gone, it is asked about before the next call, and not started.
        let again = call(
            &server,
            "tilo_invoke",
            json!({"peer_id": 1, "resource": STYLE, "action": "toggle-bold"}),
        )
        .await
        .unwrap_err();
        assert_eq!(again, "Unavailable live application instance");
        let asked = host.asked();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(lists_instances(&asked[0]), "{asked:?}");

        // Live again under a new list, a browse still asks first.
        *host.running.lock().unwrap() = true;
        call(&server, "tilo_list_apps", json!({})).await.unwrap();
        *host.running.lock().unwrap() = false;
        host.asked();
        let browsed = call(
            &server,
            "tilo_query",
            json!({"peer_id": 1, "needle": "bold"}),
        )
        .await
        .unwrap_err();
        assert_eq!(browsed, "Unavailable live application instance");
    });
    assert!(host.invoked().is_empty());
}
