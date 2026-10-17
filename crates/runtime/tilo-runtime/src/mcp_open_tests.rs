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
    /// The app's action declared as opening a path, if it has one: an
    /// `import-<extension>` is declared for that extension, any other for
    /// any file.
    opener: Option<&'static str>,
    /// An action that takes a `path` without being declared as opening it.
    takes_path: Option<&'static str>,
    running: Arc<Mutex<bool>>,
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
    /// For each of the app's next top-level listings, whether it is empty,
    /// as while the app publishes again during its start.
    blank_listings: Arc<Mutex<std::collections::VecDeque<bool>>>,
    /// One failed directory query, selected by its parent resource.
    query_failure: Arc<Mutex<Option<DirectoryFailure>>>,
    /// The text `read-text` answers; set, `read-text` declares itself the
    /// document's read.
    document: Option<String>,
    /// Rows per page of a resource's listing, as when the host's byte
    /// budget cuts it short.
    listing_page: Option<usize>,
    /// Whether, after a write and until it publishes again, the app answers
    /// its top-level listing by walking its widgets, whose work budget ends
    /// the first page after one resource; `Some(true)` when it publishes
    /// between that page and the next, which leaves the cursor stale.
    walks_after_write: Option<bool>,
    walking: Arc<Mutex<bool>>,
    /// Every question and invocation the host was sent, in order.
    log: Arc<Mutex<Vec<Asked>>>,
    /// The instance an `open` starts, which its answer names.
    started: u64,
    /// Another instance of the app that comes up with it, which nobody
    /// asked for (another session's open).
    bystander: Option<u64>,
}

struct DirectoryFailure {
    parent: Option<String>,
    error: ActionHostError,
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
            takes_path: None,
            running: Arc::default(),
            invoked: Arc::default(),
            blank_listings: Arc::default(),
            query_failure: Arc::default(),
            document: None,
            listing_page: None,
            walks_after_write: None,
            walking: Arc::default(),
            log: Arc::default(),
            started: 1,
            bystander: None,
        }
    }

    /// What the host was sent since the last call, and forget it.
    fn asked(&self) -> Vec<Asked> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    fn taking_path(mut self, action: &'static str) -> Self {
        self.takes_path = Some(action);
        self
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

    /// `provider`'s rows as its directory lists them.
    fn listed_rows(&self, provider: &str) -> Vec<CatalogueRow> {
        let mut rows = self.rows(provider);
        for row in &mut rows {
            row.provider = provider.to_owned();
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
        rows
    }

    /// The top-level page an app walking its widgets after a write answers:
    /// its first stops at its work budget after one resource.
    fn walked_listing(
        &self,
        query: &CatalogueQuery,
        rows: &[CatalogueRow],
    ) -> Option<CataloguePage> {
        let mut walking = self.walking.lock().unwrap();
        if !*walking {
            return None;
        }
        if query.cursor.is_empty() {
            return Some(CataloguePage {
                revision: "v5".into(),
                rows: rows.iter().take(1).cloned().collect(),
                next_cursor: b"1".to_vec(),
                budget_exhausted: true,
                ..CataloguePage::default()
            });
        }
        if self.walks_after_write == Some(true) {
            *walking = false;
            drop(walking);
            return Some(CataloguePage {
                revision: "v6".into(),
                stale_cursor: true,
                ..CataloguePage::default()
            });
        }
        Some(CataloguePage {
            revision: "v5".into(),
            rows: rows.iter().skip(1).cloned().collect(),
            exhausted: true,
            ..CataloguePage::default()
        })
    }

    fn rows(&self, provider: &str) -> Vec<CatalogueRow> {
        if provider == WORKSPACE {
            return workspace_rows();
        }
        let path = || ActionParam::optional("path", ParamType::Text, "Direct filesystem path");
        let mut rows =
            vec![
                row(
                    "",
                    RICH,
                    "tilo.rich-text",
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
        // Scripted text edits are offered by the fixture holding that text.
        if self.document.is_some() {
            rows.extend(text_edit_rows());
        }
        if let Some(opener) = self.opener {
            let mut row = row(DOC, DOC, "word_processor.document", opener, vec![path()]);
            if let Some(action) = row.action.as_mut() {
                let tags = &mut action.contract.tags;
                tags.push(tilo_actions::OPENS_PATH_TAG.into());
                if let Some(extension) = opener.strip_prefix("import-") {
                    tags.push(format!(
                        "{}{extension}",
                        tilo_actions::OPENS_PATH_EXTENSION_PREFIX
                    ));
                }
            }
            rows.push(row);
        }
        if let Some(action) = self.takes_path {
            rows.push(row(
                DOC,
                DOC,
                "word_processor.document",
                action,
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

/// The fixture workspace offers opening its app and assigning permissions.
fn workspace_rows() -> Vec<CatalogueRow> {
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
    vec![open, assign]
}

/// Actions of the scripted fixture when it holds editable text.
fn text_edit_rows() -> [CatalogueRow; 3] {
    [
        row(
            DOC,
            DOC,
            "word_processor.document",
            "paste",
            vec![ActionParam::required(
                "markdown",
                ParamType::Text,
                "Content",
            )],
        ),
        row(
            DOC,
            DOC,
            "word_processor.document",
            "select-all",
            Vec::new(),
        ),
        row(
            DOC,
            DOC,
            "word_processor.document",
            "apply-spelling-suggestion",
            vec![
                ActionParam::required("block", ParamType::Text, "Paragraph"),
                ActionParam::required("start", ParamType::Integer, "Start offset"),
                ActionParam::required("end", ParamType::Integer, "End offset"),
                ActionParam::required("replacement", ParamType::Text, "Replacement"),
            ],
        ),
    ]
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
            let mut peers = self.bystander.into_iter().collect::<Vec<_>>();
            peers.push(self.started);
            peers.sort_unstable();
            return Pending::ready(Ok(CataloguePage {
                revision: "v5".into(),
                providers: peers
                    .into_iter()
                    .filter(|_| running)
                    .map(|peer| CatalogueProvider {
                        provider: peer.to_string(),
                        app_id: APP.into(),
                        room: "room".into(),
                        record_id: String::new(),
                        browsable: true,
                    })
                    .collect(),
                ..CataloguePage::default()
            }));
        };
        if provider != WORKSPACE && !running {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let mut failure = self.query_failure.lock().unwrap();
        if provider != WORKSPACE
            && query.target.is_none()
            && failure
                .as_ref()
                .is_some_and(|failure| failure.parent == query.parent_resource)
        {
            return Pending::ready(Err(failure.take().unwrap().error));
        }
        drop(failure);
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
        let mut rows = self.listed_rows(&provider);
        if let Some(target) = &query.target {
            rows.retain(|row| row.action.as_ref().is_some_and(|a| a.address == *target));
        } else if query.needle.is_empty() {
            let parent = query.parent_resource.clone().unwrap_or_default();
            rows.retain(|row| row.parent_resource == parent);
        } else {
            rows.retain(|row| host_matches(row, &query.needle));
        }
        if listing && let Some(page) = self.walked_listing(&query, &rows) {
            return Pending::ready(Ok(page));
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
        let known = self.rows(&request.target.provider).iter().any(|row| {
            row.action
                .as_ref()
                .is_some_and(|action| action.address == request.target.address)
        });
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
        } else if self.walks_after_write.is_some() {
            *self.walking.lock().unwrap() = true;
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
        let (target, arguments) = self
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
                (request.target.clone(), arguments)
            })?;
        let action = target.address.action.clone();
        // As the app's door binds a read's arguments against the schema it
        // publishes (the document's read pages from its start when given
        // none), and refuses what it cannot find.
        let params = self
            .rows(&target.provider)
            .into_iter()
            .filter_map(|row| row.action)
            .find(|offered| offered.address == target.address)
            .map(|offered| offered.params)
            .unwrap_or_default()
            .into_iter()
            .filter(|param| arguments.contains_key(param.name.as_ref()))
            .collect::<Vec<_>>();
        let refusal = if action == "read-text"
            && let Err(error) = tilo_actions::ActionArgs::bind_json(&params, &arguments)
        {
            Some(tilo_actions::DoorError::from(error))
        } else if arguments
            .get("block")
            .and_then(Value::as_str)
            .is_some_and(|block| block.starts_with('r'))
        {
            Some(tilo_actions::DoorError::InvalidArguments(
                "no block has that id".into(),
            ))
        } else {
            None
        };
        let update = if let Some(error) = refusal {
            InvocationUpdate::Failed {
                invocation: *id,
                error: tilo_actions::ActionFailure {
                    code: error.code().into(),
                    message: error.to_string(),
                    details: error.failure_details(),
                    retryable: false,
                },
            }
        } else {
            let value = if target.provider == WORKSPACE {
                // As the broker answers a workspace open: with the
                // instance the launch started.
                Some(json!({"peer_id": self.started}))
            } else {
                self.document.as_ref().filter(|_| action == "read-text").map(
                    |markdown| json!({"version": 1, "content": {"markdown": markdown}, "nextCursor": "c2"}),
                )
            };
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
async fn call<H: ActionHost + Send + Sync + 'static>(
    server: &McpServer<H>,
    name: &str,
    arguments: Value,
) -> Result<Value, String> {
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
        assert!(resource(&reply["directory"], "tilo.rich-text")["actions"]["apply"].is_string());
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

/// Two sessions opening one app at once each get the instance their own
/// launch started: the host names it, and the bridge neither lists the
/// app's instances before nor after to guess which one is new.
#[test]
fn an_open_answers_the_instance_the_host_started() {
    let host = Writer {
        started: 7,
        bystander: Some(5),
        ..Writer::new(Some("open"))
    };
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": "/data/in/paper.docx"}}),
    ))
    .unwrap();
    assert_eq!(reply["update"]["state"], "succeeded", "{reply}");
    assert_eq!(reply["peer_id"], 7, "{reply}");
    assert_eq!(reply["opened"]["update"]["state"], "succeeded", "{reply}");
    let asked = host.asked();
    assert!(
        !asked.iter().any(lists_instances),
        "no instance listing was asked: {asked:?}"
    );
    let opened = host.invoked();
    assert_eq!(opened.len(), 2, "{opened:?}");
    assert_eq!(opened[1].0, "7", "the file opened in the started instance");
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

/// A file is opened only through an action its app declares as opening
/// paths, and one declared for other extensions does not open it. An
/// action that merely takes a `path` (importing an installer image, which
/// needs an administrator) is never chosen by its name.
#[test]
fn only_a_declared_opener_opens_a_path() {
    let open = |path: &str| json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open", "arguments": {"path": path}});
    let host = Writer::new(None).taking_path("import-iso");
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(
        &server,
        "tilo_invoke",
        open("/home/you/Downloads/Win11.iso"),
    ))
    .unwrap();
    assert_eq!(reply["opened"]["error"], OPENED_NO_ACTION, "{reply}");
    assert!(
        host.invoked().iter().all(|call| call.2 != "import-iso"),
        "{:?}",
        host.invoked()
    );

    let host = Writer::new(Some("import-csv"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let reply = run(call(&server, "tilo_invoke", open("/data/in/paper.docx"))).unwrap();
    assert_eq!(reply["opened"]["error"], OPENED_NO_ACTION, "{reply}");
    assert_eq!(host.invoked().len(), 1, "nothing but the launch ran");
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
    // The host answers once the app has published, but the app may publish
    // again while it binds its document: the first listings read empty.
    host.blank_listings.lock().unwrap().extend([true, true]);
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
fn a_handle_in_a_resource_argument_is_expanded_and_text_is_not_replayed() {
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
        let style = resource(&page, "tilo.rich-text")["resource"]
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
        assert_eq!(selected["update"]["state"], "failed", "{selected}");
        assert_eq!(
            selected["update"]["error"]["message"],
            "no block has that id"
        );
        assert!(selected.get("adjusted").is_none(), "{selected}");
        assert_eq!(
            selected["hint"],
            format!(
                "block `{style}` is a session handle for {RICH}; pass that id if you meant the resource"
            ),
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
    assert_eq!(selects.len(), 1, "{selects:?}");
    assert!(
        selects[0]["block"]
            .as_str()
            .is_some_and(|handle| handle.starts_with('r')),
        "a text argument stays as given: {selects:?}"
    );
}

/// A host that mutates before reporting execution failure. The real ledger
/// deduplicates the caller's id; it cannot protect against a bridge inventing
/// a fresh id. Admission refusals, in contrast, record no mutation.
#[derive(Clone)]
struct FailingMutationHost {
    directory: Writer,
    ledger: Arc<Mutex<tilo_actions::remote::InvocationLedger<InvocationState>>>,
    mutations: Arc<Mutex<Vec<InvokeRequest>>>,
    refuse: bool,
}

impl FailingMutationHost {
    fn new(refuse: bool) -> Self {
        let directory = Writer::new(None);
        *directory.running.lock().unwrap() = true;
        Self {
            directory,
            ledger: Arc::default(),
            mutations: Arc::default(),
            refuse,
        }
    }
}

impl ActionHost for FailingMutationHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.directory.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if self.refuse {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        let result = self
            .ledger
            .lock()
            .unwrap()
            .admit(caller, &request, || {
                self.mutations.lock().unwrap().push(request.clone());
                Ok(InvocationState::new(
                    request.invocation,
                    InvocationUpdate::Failed {
                        invocation: request.invocation,
                        error: tilo_actions::ActionFailure {
                            code: "write_failed".into(),
                            message: "mutation applied; completion failed".into(),
                            details: Some(json!({"applied": true})),
                            retryable: false,
                        },
                    },
                ))
            })
            .map(|(state, _)| state.receipt());
        Pending::ready(result)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.ledger.lock().unwrap().get(caller, id).cloned()
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        Ok(())
    }
}

fn mutation_call(handle: &str, id: tilo_actions::InvocationId) -> Value {
    json!({"peer_id": 1, "revision": "v5", "resource": DOC, "action": "select-block",
        "arguments": {"block": handle}, "invocation_id": id.0.to_string()})
}

fn assert_failed_mutation(reply: &Value, id: tilo_actions::InvocationId) {
    assert_eq!(reply["invocation_id"], id.0.to_string(), "{reply}");
    assert_eq!(reply["update"]["state"], "failed", "{reply}");
    assert_eq!(reply["update"]["error"]["code"], "write_failed", "{reply}");
    assert_eq!(
        reply["update"]["error"]["message"],
        "mutation applied; completion failed"
    );
    assert_eq!(
        reply["update"]["error"]["details"],
        json!({"applied": true})
    );
    assert!(reply.get("adjusted").is_none(), "{reply}");
}

#[test]
fn a_failed_call_with_an_unexpanded_handle_is_not_rerun() {
    let host = FailingMutationHost::new(false);
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        let page = call(&server, "tilo_query", json!({"peer_id": 1}))
            .await
            .unwrap();
        let handle = resource(&page, "tilo.rich-text")["resource"]
            .as_str()
            .unwrap();
        assert_ne!(handle, RICH);
        let id = tilo_actions::InvocationId::new();
        let arguments = mutation_call(handle, id);
        let reply = call(&server, "tilo_invoke", arguments.clone())
            .await
            .unwrap();
        assert_eq!(
            host.mutations.lock().unwrap().len(),
            1,
            "one admission must apply one mutation"
        );
        assert_failed_mutation(&reply, id);
        let retried = call(&server, "tilo_invoke", arguments).await.unwrap();
        assert_failed_mutation(&retried, id);
        let mutations = host.mutations.lock().unwrap();
        assert_eq!(
            mutations.len(),
            1,
            "same-id retry must retain the original admission"
        );
        assert_eq!(mutations[0].invocation, id);
        assert!(
            matches!(&mutations[0].argument, InvokeArgument::Json(map) if map["block"] == handle)
        );
        drop(mutations);
    });
}

#[test]
fn an_admission_refusal_with_a_text_handle_is_not_an_execution_failure() {
    let host = FailingMutationHost::new(true);
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        let page = call(&server, "tilo_query", json!({"peer_id": 1}))
            .await
            .unwrap();
        let handle = resource(&page, "tilo.rich-text")["resource"]
            .as_str()
            .unwrap();
        let refusal = call(
            &server,
            "tilo_invoke",
            mutation_call(handle, tilo_actions::InvocationId::new()),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal, ActionHostError::NotPermitted.to_string());
        assert!(host.mutations.lock().unwrap().is_empty());
    });
}

#[test]
fn the_http_bridge_does_not_readmit_a_failed_text_handle_mutation() {
    use http_body_util::BodyExt;
    use hyper::{HeaderMap, Method, StatusCode, header::HeaderValue};

    let host = FailingMutationHost::new(false);
    let endpoint = crate::mcp_http::Endpoint::new(
        host.clone(),
        "room".into(),
        "owner".into(),
        Some("secret".into()),
        None,
    );
    run(async {
        let initialize = json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params": {
            "protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"handle-test", "version":"1"}}});
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        let opened = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers,
                initialize.to_string().as_bytes(),
            )
            .await;
        assert_eq!(opened.status(), StatusCode::OK);
        headers.insert(
            "mcp-session-id",
            HeaderValue::from_str(opened.headers()["mcp-session-id"].to_str().unwrap()).unwrap(),
        );
        let query = json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"tilo_query", "arguments":{"peer_id":1}}});
        let listed = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers,
                query.to_string().as_bytes(),
            )
            .await;
        assert_eq!(listed.status(), StatusCode::OK);
        let bytes = listed.into_body().collect().await.unwrap().to_bytes();
        let page: Value = serde_json::from_slice(&bytes).unwrap();
        let handle = resource(&page["result"]["structuredContent"], "tilo.rich-text")["resource"]
            .as_str()
            .unwrap();
        assert_ne!(handle, RICH);
        let id = tilo_actions::InvocationId::new();
        let invoke = json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"tilo_invoke", "arguments":mutation_call(handle, id)}});
        for _ in 0..2 {
            let response = endpoint
                .dispatch(
                    &Method::POST,
                    "/mcp",
                    &headers,
                    invoke.to_string().as_bytes(),
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let reply: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                host.mutations.lock().unwrap().len(),
                1,
                "HTTP must not readmit a failed mutation"
            );
            assert_eq!(
                reply["result"]["isError"], false,
                "execution failure is a structured update: {reply}"
            );
            assert_failed_mutation(&reply["result"]["structuredContent"], id);
        }
        let mutations = host.mutations.lock().unwrap();
        assert_eq!(mutations[0].invocation, id);
        assert!(
            matches!(&mutations[0].argument, InvokeArgument::Json(map) if map["block"] == handle)
        );
        drop(mutations);
    });
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
        let rich = resource(&page, "tilo.rich-text")["resource"]
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

/// What `read-text` was asked for, in order.
fn read_sizes(host: &Writer) -> Vec<Value> {
    host.invoked()
        .iter()
        .filter(|call| call.2 == "read-text")
        .map(|call| call.3["pageSize"].clone())
        .collect()
}

/// cross-standup asked `read-text` for 200 blocks and was refused. One call
/// is one invocation: the refusal carries the bound as data and the answer
/// names it, and nothing runs again with another value. Replaces the bridge
/// that clamped the number and started a second invocation under a fresh
/// id, which a retry of the caller's id then repeated.
#[test]
fn an_out_of_range_number_is_refused_with_its_bound_and_never_rerun() {
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
    assert_eq!(reply["update"]["state"], "failed", "{reply}");
    assert_eq!(
        reply["update"]["error"]["details"],
        json!({"param": "pageSize", "violation": {"kind": "maximum", "bound": 64}}),
        "{reply}"
    );
    assert_eq!(
        reply["update"]["error"]["message"],
        "argument `pageSize` must be at most 64"
    );
    assert_eq!(
        reply["hint"],
        "pageSize must be at most 64; call again with a value in range, such as 64"
    );
    assert!(reply.get("adjusted").is_none(), "{reply}");
    assert_eq!(read_sizes(&host), [json!(200)], "exactly one invocation");
}

/// The caller retries an out-of-range call with its own invocation id, as
/// the tool tells it to after a lost reply: every start the host sees
/// carries that id and the caller's own value, so a host that keeps its
/// ids (as every production host does) runs it at most once.
#[test]
fn a_retried_invocation_id_never_runs_twice() {
    let host = Writer::new(Some("open"));
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    let id = tilo_actions::InvocationId::new();
    let read = json!({
        "peer_id": 1, "resource": DOC, "action": "read-text",
        "arguments": {"pageSize": 200}, "invocation_id": id.0.to_string()
    });
    let (first, again) = run(async {
        call(
            &server,
            "tilo_invoke",
            json!({"workspace": true, "resource": "app.so.tilo.word-processor", "action": "open"}),
        )
        .await
        .unwrap();
        let first = call(&server, "tilo_invoke", read.clone()).await.unwrap();
        let again = call(&server, "tilo_invoke", read).await.unwrap();
        (first, again)
    });
    for reply in [&first, &again] {
        assert_eq!(reply["invocation_id"], id.0.to_string(), "{reply}");
        assert_eq!(reply["update"]["state"], "failed", "{reply}");
    }
    let started: Vec<_> = host
        .invoked
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.target.address.action == "read-text")
        .map(|request| request.invocation)
        .collect();
    assert!(
        started.iter().all(|started| *started == id),
        "no invocation under another id: {started:?}"
    );
    assert!(!started.is_empty());
    assert!(read_sizes(&host).iter().all(|size| *size == json!(200)));
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
fn a_resource_named_by_kind_right_after_a_write_is_found_past_the_first_page() {
    for publishes_between_pages in [false, true] {
        let mut host = Writer::new(Some("open"));
        host.walks_after_write = Some(publishes_between_pages);
        *host.running.lock().unwrap() = true;
        let server = McpServer::new(host.clone(), "room".into(), "owner".into());
        run(async {
            let page = call(
                &server,
                "tilo_query",
                json!({"app_id": APP, "needle": "select block"}),
            )
            .await
            .unwrap();
            let doc = resource(&page, "word_processor.document")["resource"].clone();
            let write = json!({"app_id": APP, "resource": doc, "action": "select-block", "arguments": {"block": "b0"}});
            call(&server, "tilo_invoke", write).await.unwrap();
            for action in ["save-as", "select-block"] {
                let by_kind = json!({"app_id": APP, "resource": "word_processor.document", "action": action, "arguments": {}});
                let ran = call(&server, "tilo_invoke", by_kind).await;
                assert!(
                    ran.is_ok(),
                    "{action} by kind after a write (publishing between pages: {publishes_between_pages}): {ran:?}"
                );
            }
        });
        let invoked = host.invoked();
        assert_eq!(invoked.len(), 3, "{invoked:?}");
        assert!(invoked.iter().all(|call| call.1 == DOC), "{invoked:?}");
    }
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

#[test]
fn an_edit_and_history_named_by_kind_reach_the_same_discovered_record() {
    let host = Writer::new(Some("history"));
    *host.running.lock().unwrap() = true;
    let server = McpServer::new(host.clone(), "room".into(), "owner".into());
    run(async {
        let page = call(&server, "tilo_query", json!({"app_id": APP}))
            .await
            .unwrap();
        let handle = resource(&page, "word_processor.document")["resource"].clone();
        let details = call(
            &server,
            "tilo_query",
            json!({"app_id": APP, "parent_resource": handle}),
        )
        .await
        .unwrap();
        assert!(resource(&details, "word_processor.document")["actions"]["history"].is_string());
        for action in ["save-as", "history"] {
            let reply = call(
                &server,
                "tilo_invoke",
                json!({
                    "app_id": APP, "resource": "word_processor.document", "action": action
                }),
            )
            .await
            .unwrap();
            assert_eq!(reply["update"]["state"], "succeeded", "{reply}");
        }
        let reply = call(
            &server,
            "tilo_invoke",
            json!({
                "app_id": APP, "resource": handle, "action": "history"
            }),
        )
        .await
        .unwrap();
        assert_eq!(reply["update"]["state"], "succeeded", "{reply}");
    });
    let invoked = host.invoked();
    assert_eq!(invoked.len(), 3, "{invoked:?}");
    for (_, record, _, _) in invoked {
        assert_eq!(
            record, DOC,
            "edits and history share the actual discovered record"
        );
    }
}

#[test]
fn directory_failures_are_not_reported_as_missing_resources() {
    for tool in ["tilo_invoke", "tilo_resolve"] {
        for failure in [
            ActionHostError::Unavailable {
                reason: "the original directory is disconnected".into(),
            },
            ActionHostError::NotPermitted,
        ] {
            let expected = failure.to_string();
            let host = Writer::new(Some("history"));
            *host.running.lock().unwrap() = true;
            *host.query_failure.lock().unwrap() = Some(DirectoryFailure {
                parent: None,
                error: failure,
            });
            let server = McpServer::new(host.clone(), "room".into(), "owner".into());
            let error = run(call(
                &server,
                tool,
                json!({
                    "app_id": APP, "resource": "word_processor.document", "action": "history",
                    "revision": "v5"
                }),
            ))
            .unwrap_err();
            assert!(error.contains(&expected), "{tool}: {error}");
            assert!(!error.contains("no resource"), "{tool}: {error}");
            assert!(host.invoked().is_empty(), "a failed query starts no action");
            if tool == "tilo_resolve" {
                assert!(
                    host.asked().iter().any(|asked| matches!(
                        asked,
                        Asked::Query(query) if query.target.is_some()
                    )),
                    "resolve must reach its target before the alias query fails"
                );
            }
        }
    }
}

#[test]
fn directory_failures_are_not_reported_as_missing_actions() {
    for tool in ["tilo_invoke", "tilo_resolve"] {
        let host = Writer::new(None);
        *host.running.lock().unwrap() = true;
        *host.query_failure.lock().unwrap() = Some(DirectoryFailure {
            parent: Some(DOC.into()),
            error: ActionHostError::Unavailable {
                reason: "the original action directory is disconnected".into(),
            },
        });
        let server = McpServer::new(host.clone(), "room".into(), "owner".into());
        let error = run(call(
            &server,
            tool,
            json!({
                "app_id": APP, "resource": DOC, "action": "history", "revision": "v5"
            }),
        ))
        .unwrap_err();
        assert!(
            error.contains("the original action directory is disconnected"),
            "{tool}: {error}"
        );
        assert!(!error.contains("no resource"), "{tool}: {error}");
        assert!(
            host.invoked().is_empty(),
            "a missing action cannot dispatch"
        );
    }
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

/// #3870: an agent that listed or browsed an instance acts on it without
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

/// #3870: a batch asks at most once before its first step, and never
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

/// #3870: an instance the session saw that has gone since is refused as
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
