//! A call that made or changed something answers the link that opens it, as
//! the server that invoked it minted it in the same answer: the call asks for
//! nothing more, and a path it invoked before is not resolved again.
use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, InvocationReceipt, InvocationState,
    InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{
    ActionContract, ActionKind, ExecutionLocus, InputSpec, InvocationUpdate, OperationKind,
};

const APP: &str = "so.tilo.test";
const PROVIDER: &str = "record:so.tilo.test:";
const ROOT: &str = "test.note";
const ITEM: &str = "test.note/items/42";
const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
const ENTRY: &str = "https://tilo.so/";

/// An app whose `create` makes the item [`ITEM`], served as a record.
#[derive(Clone, Default)]
struct Tracker {
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
    /// How many directory queries were asked.
    queried: Arc<Mutex<usize>>,
}

fn descriptor(resource: &str, action: &str, kind: OperationKind) -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(resource, action),
        resource_kind: ROOT.into(),
        resource_label: "Note".into(),
        action_label: action.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::ClientLocal,
        kind,
        contract: ActionContract {
            kind: ActionKind::Undoable,
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
        effects: Vec::new(),
    }
}

fn row(resource: &str, parent: &str, action: Option<ActionDescriptor>) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: PROVIDER.into(),
        app_id: APP.into(),
        parent_resource: parent.into(),
        resource: tilo_actions::remote::CatalogueResource {
            id: resource.into(),
            kind: ROOT.into(),
            label: resource.into(),
            presentation: None,
        },
        action,
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// The URL the server minted for the item a `create` made.
fn url(resource: &str) -> String {
    format!("{ENTRY}app/#/w/{WORKSPACE}/{resource}")
}

impl Tracker {
    /// The rows the directory holds under `parent`: the document's own, and the item.
    fn rows(parent: &str) -> Vec<CatalogueRow> {
        if parent.is_empty() {
            return vec![
                row(
                    "",
                    "",
                    Some(descriptor("", "create", OperationKind::Document)),
                ),
                row(ITEM, "", None),
            ];
        }
        vec![row(ITEM, parent, None)]
    }

    /// What the app was asked to invoke, as `resource#action`.
    fn asked(&self) -> Vec<String> {
        self.invoked
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                format!(
                    "{}#{}",
                    request.target.address.resource, request.target.address.action
                )
            })
            .collect()
    }
}

impl ActionProvider for Tracker {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let mut queried = self.queried.lock().unwrap();
        *queried = queried.saturating_add(1);
        drop(queried);
        let rows = match (query.provider.as_deref(), &query.target) {
            (None, _) => Vec::new(),
            // An exact address is answered at whatever level it stands.
            (Some(_), Some(target)) => Self::rows("")
                .into_iter()
                .filter(|row| {
                    row.action
                        .as_ref()
                        .is_some_and(|action| action.address == *target)
                })
                .collect(),
            (Some(_), None) => Self::rows(query.parent_resource.as_deref().unwrap_or("")),
        };
        let mut page = CataloguePage::from_rows("1".into(), rows, &query);
        page.providers = vec![CatalogueProvider {
            provider: PROVIDER.into(),
            app_id: APP.into(),
            workspace: "workspace".into(),
            document_id: String::new(),
            browsable: true,
        }];
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let invoked = self.invoked.lock().unwrap();
        let request = invoked.iter().find(|request| request.invocation == *id)?;
        let made = request.target.address.action == "create";
        drop(invoked);
        let output = tilo_actions::ActionOutput {
            value: None,
            resources: if made { vec![ITEM.into()] } else { Vec::new() },
        };
        Some(
            InvocationState::new(
                *id,
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output,
                },
            )
            .linked(made.then(|| url(ITEM))),
        )
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn agent(host: &Tracker) -> McpServer<Tracker> {
    McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada")
        .with_agent_session("mcp:test")
}

/// An app served as a record, a session works through: two entries of a journal,
/// each offering `archive`, and the journal's own `new-entry`.
#[derive(Clone, Default)]
struct Journal {
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
    /// How many directory queries were asked.
    queried: Arc<Mutex<usize>>,
}

const JOURNAL_APP: &str = "so.tilo.journal";
const JOURNAL_PROVIDER: &str = "record:so.tilo.journal:";
const FIRST: &str = "journal.entries/a";
const SECOND: &str = "journal.entries/b";

impl Journal {
    /// The journal's own row and its two entries, as one page of the app's level.
    fn rows(parent: &str) -> Vec<CatalogueRow> {
        if !parent.is_empty() {
            return Vec::new();
        }
        vec![
            journal_row(
                "",
                "",
                Some(descriptor("", "new-entry", OperationKind::Document)),
            ),
            journal_row(
                FIRST,
                "First",
                Some(descriptor(FIRST, "archive", OperationKind::Document)),
            ),
            journal_row(
                SECOND,
                "Second",
                Some(descriptor(SECOND, "archive", OperationKind::Document)),
            ),
        ]
    }

    fn asked(&self) -> Vec<String> {
        self.invoked
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                format!(
                    "{}#{}",
                    request.target.address.resource, request.target.address.action
                )
            })
            .collect()
    }

    fn queried(&self) -> usize {
        *self.queried.lock().unwrap()
    }
}

fn journal_row(resource: &str, label: &str, action: Option<ActionDescriptor>) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: JOURNAL_PROVIDER.into(),
        app_id: JOURNAL_APP.into(),
        parent_resource: String::new(),
        resource: tilo_actions::remote::CatalogueResource {
            id: resource.into(),
            kind: ROOT.into(),
            label: label.into(),
            presentation: None,
        },
        action,
        has_children: false,
        visibility: None,
        surface: None,
    }
}

impl ActionProvider for Journal {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let mut queried = self.queried.lock().unwrap();
        *queried = queried.saturating_add(1);
        drop(queried);
        let rows = match (query.provider.as_deref(), &query.target) {
            (None, _) => Vec::new(),
            (Some(_), Some(target)) => Journal::rows("")
                .into_iter()
                .filter(|row| {
                    row.action
                        .as_ref()
                        .is_some_and(|action| action.address == *target)
                })
                .collect(),
            (Some(_), None) => {
                let parent = query.parent_resource.clone().unwrap_or_default();
                let mut rows = Journal::rows(&parent);
                if let Some(name) = query.resource_name.as_deref() {
                    rows.retain(|row| {
                        row.resource.id == name
                            || row.resource.label.trim().eq_ignore_ascii_case(name)
                    });
                }
                rows
            }
        };
        let mut page = CataloguePage::from_rows("1".into(), rows, &query);
        page.providers = vec![CatalogueProvider {
            provider: JOURNAL_PROVIDER.into(),
            app_id: JOURNAL_APP.into(),
            workspace: "workspace".into(),
            document_id: String::new(),
            browsable: true,
        }];
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request.clone());
        // A workspace with no machine to place a window in refuses an open.
        if request.target.provider.starts_with("application:") {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "this workspace has no machine to place a window in".into(),
            }));
        }
        let accepted = matches!(
            request.target.address.action.as_str(),
            "new-entry" | "archive"
        );
        Pending::ready(if accepted {
            Ok(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            })
        } else {
            Err(ActionCallError::UnknownAction)
        })
    }

    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        Some(InvocationState::new(
            *id,
            InvocationUpdate::Succeeded {
                invocation: *id,
                output: tilo_actions::ActionOutput::default(),
            },
        ))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// The text a tool call answered.
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
    (
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {reply}"))
            .to_owned(),
        result["isError"] == true,
    )
}

#[test]
fn a_create_answers_the_link_minted_with_it_and_a_second_asks_only_the_invocation() {
    let host = Tracker::default();
    run(async {
        let server = agent(&host);
        let create = json!({"path": "test#create", "args": {"title": "First note"}});
        let (text, failed) = tool(&server, "tilo_do", create.clone()).await;
        assert!(!failed, "{text}");
        assert!(text.contains(&format!("-> {ITEM}")), "{text}");
        assert!(
            text.ends_with(&format!("link: {}", url(ITEM))),
            "the link names the item it made: {text}"
        );
        let share = tilo_actions::remote::SHARE_LINK_ACTION;
        assert!(
            !host
                .asked()
                .iter()
                .any(|asked| asked.ends_with(&format!("#{share}"))),
            "no link was asked for beside the change: {:?}",
            host.asked()
        );
        // The same path again: the action it resolved to is invoked, and nothing
        // else is asked (no lookup, and the document's window was shown already).
        let (asked, queried) = (host.asked().len(), *host.queried.lock().unwrap());
        let (text, failed) = tool(&server, "tilo_do", create).await;
        assert!(!failed, "{text}");
        assert!(text.ends_with(&format!("link: {}", url(ITEM))), "{text}");
        assert_eq!(
            *host.queried.lock().unwrap(),
            queried,
            "the path was not resolved again"
        );
        assert_eq!(
            host.asked()[asked..],
            ["#create".to_owned()],
            "only the action was invoked"
        );
    });
}

/// One action call is one request to the workspace: an app this session has
/// looked up and read the directory of is not asked about again for a write on
/// another of its resources, and the write opens nothing beside it.
#[test]
fn a_write_on_a_resource_the_apps_directory_names_asks_only_its_invocation() {
    let host = Journal::default();
    run(async {
        let server = McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada")
            .with_agent_session("mcp:test");
        let (text, failed) = tool(&server, "tilo_do", json!({"path": "journal#new-entry"})).await;
        assert!(!failed, "{text}");
        let (asked, queried) = (host.asked().len(), host.queried());
        let (text, failed) = tool(
            &server,
            "tilo_do",
            json!({"path": "journal/Second#archive"}),
        )
        .await;
        assert!(!failed, "{text}");
        assert_eq!(
            host.queried(),
            queried,
            "the app and its directory were not asked for again: {:?}",
            host.asked()
        );
        assert_eq!(
            host.asked()[asked..],
            [format!("{SECOND}#archive")],
            "only the action was invoked"
        );
        assert!(
            host.asked()[asked..]
                .iter()
                .all(|asked| !asked.ends_with("#open")),
            "the write opens nothing beside it: {:?}",
            host.asked()
        );
    });
}
