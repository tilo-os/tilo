//! Paths reach every publisher of the workspace, not only apps' windows: the
//! host's own services by the app id they answer for (`desktop#list-windows`
//! is the desktop's layout), and one window of an app by the provider an
//! `open` answered (`browser@42/page#read-page` reaches that window, never
//! the other Browser window beside it).
use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueResource,
    InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_actions::{InvocationUpdate, OperationKind};

const DESKTOP: &str = "tilo.desktop";
const BROWSER: &str = "so.tilo.browser";
/// The two Browser windows: the older, and the newer one a path without a
/// window would reach.
const OLDER: &str = "41";
const NEWER: &str = "42";

/// A workspace with the desktop's layout service and two Browser windows, each
/// answering who it is.
#[derive(Clone, Default)]
struct Workspace {
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
}

fn row(provider: &str, app: &str, resource: &str, action: &str) -> CatalogueRow {
    CatalogueRow {
        provider: provider.into(),
        app_id: app.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: resource.split(':').next().unwrap_or(resource).into(),
            label: resource.into(),
            presentation: None,
        },
        action: Some(tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(resource, action),
            resource_kind: resource.into(),
            resource_label: resource.into(),
            action_label: action.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            state: tilo_actions::ActionState::default(),
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

impl Workspace {
    fn rows(provider: &str) -> Vec<CatalogueRow> {
        match provider {
            DESKTOP => vec![
                row(
                    DESKTOP,
                    "so.tilo.desktop",
                    "desktop:session",
                    "list-windows",
                ),
                row(DESKTOP, "so.tilo.desktop", "desktop:session", "open"),
            ],
            OLDER | NEWER => vec![
                row(provider, BROWSER, "page", "read-page"),
                row(provider, BROWSER, "page", "activate-window"),
            ],
            _ => Vec::new(),
        }
    }
}

impl ActionHost for Workspace {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let Some(provider) = query.provider.clone() else {
            let listed = |provider: &str, app_id: &str| CatalogueProvider {
                provider: provider.into(),
                app_id: app_id.into(),
                workspace: "workspace".into(),
                record_id: String::new(),
                browsable: true,
            };
            return Pending::ready(Ok(CataloguePage {
                revision: "v1".into(),
                providers: vec![
                    listed(OLDER, BROWSER),
                    listed(DESKTOP, "so.tilo.desktop"),
                    listed(NEWER, BROWSER),
                ],
                ..CataloguePage::default()
            }));
        };
        let mut rows = Self::rows(&provider);
        if let Some(target) = &query.target {
            rows.retain(|row| row.action.as_ref().is_some_and(|a| a.address == *target));
        }
        if let Some(name) = &query.resource_name {
            rows.retain(|row| row.resource.id == *name || row.resource.label == *name);
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
        let known = Self::rows(&request.target.provider).iter().any(|row| {
            row.action
                .as_ref()
                .is_some_and(|a| a.address == request.target.address)
        });
        if !known {
            return Pending::ready(Err(ActionHostError::UnknownAction));
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
        let provider = self
            .invoked
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.invocation == *id)
            .map(|request| request.target.provider.clone())?;
        Some(InvocationState::new(
            *id,
            InvocationUpdate::Succeeded {
                invocation: *id,
                output: tilo_actions::ActionOutput {
                    value: Some(json!({ "answered_by": provider })),
                    ..tilo_actions::ActionOutput::default()
                },
            },
        ))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Ok(()))
    }
}

async fn tool(server: &McpServer<Workspace>, name: &str, arguments: Value) -> (String, bool) {
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

fn agent(workspace: &Workspace) -> McpServer<Workspace> {
    McpServer::for_agent(workspace.clone(), "workspace".into(), "planner", "ada")
}

fn answered_by(workspace: &Workspace) -> Vec<String> {
    workspace
        .invoked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.target.provider.clone())
        .collect()
}

#[tokio::test]
async fn the_desktops_layout_service_answers_to_its_name() {
    let workspace = Workspace::default();
    let server = agent(&workspace);
    let (text, failed) = tool(&server, "tilo_do", json!({"path": "desktop#list-windows"})).await;
    assert!(!failed, "{text}");
    assert_eq!(answered_by(&workspace), vec![DESKTOP.to_owned()]);

    let (listed, failed) = tool(&server, "tilo_ls", json!({})).await;
    assert!(!failed, "{listed}");
    assert!(
        listed
            .lines()
            .any(|line| line.starts_with("desktop") && line.ends_with("service")),
        "the desktop is listed as a service: {listed}"
    );
}

#[tokio::test]
async fn a_window_named_by_its_provider_is_the_one_that_answers() {
    let workspace = Workspace::default();
    let server = agent(&workspace);
    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": format!("browser@{OLDER}/page#read-page")}),
    )
    .await;
    assert!(!failed, "{text}");
    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "browser/page#read-page"}),
    )
    .await;
    assert!(!failed, "{text}");
    assert_eq!(
        answered_by(&workspace),
        vec![OLDER.to_owned(), NEWER.to_owned()],
        "the named window, then the newest when none is named"
    );

    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "browser@99/page#read-page"}),
    )
    .await;
    assert!(failed && text.contains("no running window 99"), "{text}");
    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": format!("notes@{OLDER}/page#read-page")}),
    )
    .await;
    assert!(failed && text.contains("not notes"), "{text}");
    assert_eq!(answered_by(&workspace).len(), 2, "nothing else ran");
}

/// The person sees what their agent opens: `open` of an app that runs already, and "Show this
/// window" on one of its windows, each ask for the app's window on the person's desk (the host
/// leaves one already there where it is, and the page brings it into view). A read shows nothing.
#[tokio::test]
async fn opening_or_showing_a_running_app_asks_for_its_window_on_the_desk() {
    let workspace = Workspace::default();
    let server = agent(&workspace);
    let browser = || {
        vec![crate::mcp::ActedRecord {
            app: BROWSER.into(),
            record: String::new(),
        }]
    };
    let (text, failed) = tool(&server, "tilo_do", json!({"path": "browser#open"})).await;
    assert!(
        !failed && text.starts_with("ok: browser is running"),
        "{text}"
    );
    assert_eq!(server.take_changed(), browser(), "open shows the app");
    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "browser/page#activate-window"}),
    )
    .await;
    assert!(!failed, "{text}");
    assert_eq!(
        server.take_changed(),
        browser(),
        "showing a window shows it"
    );
    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "browser/page#read-page"}),
    )
    .await;
    assert!(!failed, "{text}");
    assert!(server.take_changed().is_empty(), "a read shows nothing");
}
