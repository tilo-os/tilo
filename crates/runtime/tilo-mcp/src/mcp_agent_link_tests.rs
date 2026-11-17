//! A call that made or changed something answers the link that opens it:
//! the item its answer names after `->`, when the item offers a `share-link`
//! of its own, else the link to the record it is kept in.
use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, InvocationReceipt, InvocationState,
    InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{ActionContract, ActionKind, ExecutionLocus, InputSpec, InvocationUpdate};

const APP: &str = "so.tilo.test";
const PROVIDER: &str = "record:so.tilo.test:";
const ROOT: &str = "test.note";
const ITEM: &str = "test.note/items/42";
/// The chain of parts beside the record that keeps the items, when they are
/// kept so: what the app lists at its top level, the items beneath it.
const CHAIN: &str = "test.notes";
const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
const ENTRY: &str = "https://tilo.so/";

/// An app whose `create` makes the item [`ITEM`], served as a record.
#[derive(Clone)]
struct Tracker {
    /// Whether the item it makes offers a `share-link` of its own.
    linked_item: bool,
    /// Whether the item is kept in a part of [`CHAIN`] rather than in the
    /// record itself, so the app lists it a level down.
    in_chain: bool,
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
}

fn descriptor(resource: &str, action: &str, kind: OperationKind) -> ActionDescriptor {
    let mut descriptor = ActionDescriptor {
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
    };
    if kind == OperationKind::View {
        descriptor.contract = ActionContract::view();
    }
    descriptor
}

fn row(resource: &str, parent: &str, action: Option<ActionDescriptor>) -> CatalogueRow {
    CatalogueRow {
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

/// The URL the app answers a `share-link` on `resource` with.
fn url(resource: &str) -> String {
    format!("{ENTRY}app/#/w/{WORKSPACE}/{resource}")
}

impl Tracker {
    fn new(linked_item: bool) -> Self {
        Self {
            linked_item,
            in_chain: false,
            invoked: Arc::default(),
        }
    }

    /// An app keeping its items in parts beside its record.
    fn in_chain(mut self) -> Self {
        self.in_chain = true;
        self
    }

    /// The rows the directory holds under `parent`: the record's own, and the
    /// item's own (the share-link when the item offers one).
    fn rows(&self, parent: &str) -> Vec<CatalogueRow> {
        if parent.is_empty() {
            let mut rows = vec![row(
                "",
                "",
                Some(descriptor("", "create", OperationKind::Document)),
            )];
            if self.in_chain {
                // Listed as a host lists a chain at the top level: not as a
                // container, its items read a level further down.
                rows.push(row(CHAIN, "", None));
            } else {
                rows.push(row(ITEM, "", None));
            }
            return rows;
        }
        if self.in_chain && parent == CHAIN {
            return vec![row(ITEM, CHAIN, None)];
        }
        let link = self.linked_item.then(|| {
            descriptor(
                ITEM,
                tilo_actions::remote::SHARE_LINK_ACTION,
                OperationKind::Effect,
            )
        });
        vec![row(ITEM, parent, link)]
    }
}

impl ActionProvider for Tracker {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let rows = match (query.provider.as_deref(), &query.target) {
            (None, _) => Vec::new(),
            // An exact address is answered at whatever level it stands.
            (Some(_), Some(target)) => ["", CHAIN, ITEM]
                .into_iter()
                .flat_map(|parent| self.rows(parent))
                .filter(|row| {
                    row.action
                        .as_ref()
                        .is_some_and(|action| action.address == *target)
                })
                .collect(),
            (Some(_), None) => self.rows(query.parent_resource.as_deref().unwrap_or("")),
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
        let (resource, action) = (
            request.target.address.resource.clone(),
            request.target.address.action.clone(),
        );
        drop(invoked);
        let output = match action.as_str() {
            tilo_actions::remote::SHARE_LINK_ACTION => tilo_actions::ActionOutput {
                value: Some(json!({ "url": url(&resource) })),
                resources: Vec::new(),
            },
            "create" => tilo_actions::ActionOutput {
                value: None,
                resources: vec![ITEM.into()],
            },
            _ => tilo_actions::ActionOutput::default(),
        };
        Some(InvocationState::new(
            *id,
            InvocationUpdate::Succeeded {
                invocation: *id,
                output,
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

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn agent(host: &Tracker) -> McpServer<Tracker> {
    McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada")
        .with_agent_session("mcp:test", "MCP agent")
}

/// The text a tool call answered.
async fn tool(server: &McpServer<Tracker>, name: &str, arguments: Value) -> (String, bool) {
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

tilo_test_env::scenario! {
    fn a_create_answers_the_link_that_opens_what_it_made;

    phase a_create_answers_its_items_own_link;
    // The item the answer names after `->` offers a `share-link` (its app
    // opens items), so the link names the item, not the app's record.
    {
        let host = Tracker::new(true);
        run(async {
            let server = agent(&host);
            let (text, failed) = tool(
                &server,
                "tilo_do",
                json!({"path": "test#create", "args": {"title": "First note"}}),
            )
            .await;
            assert!(!failed, "{text}");
            assert!(text.contains(&format!("-> {ITEM}")), "{text}");
            assert!(
                text.ends_with(&format!("link: {}", url(ITEM))),
                "the link names the item it made: {text}"
            );
            let asked: Vec<String> = host
                .invoked
                .lock()
                .unwrap()
                .iter()
                .map(|request| {
                    format!(
                        "{}#{}",
                        request.target.address.resource, request.target.address.action
                    )
                })
                .collect();
            assert!(
                asked.contains(&format!("{ITEM}#{}", tilo_actions::remote::SHARE_LINK_ACTION)),
                "the item's own link was asked for, not the record's: {asked:?}"
            );
        });
    }

    phase a_create_answers_the_own_link_of_an_item_kept_in_a_part;
    // An item kept in a part beside the record is listed under the chain of
    // parts, which the record's top level does not list as a container; the
    // link is asked of the item by its address and still names it.
    {
        let host = Tracker::new(true).in_chain();
        run(async {
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "test#create", "args": {"title": "First note"}}),
            )
            .await;
            assert!(!failed, "{text}");
            assert!(text.contains(&format!("-> {ITEM}")), "{text}");
            assert!(
                text.ends_with(&format!("link: {}", url(ITEM))),
                "the link names the item kept in a part: {text}"
            );
        });
    }

    phase a_create_answers_the_records_link_when_the_item_offers_none;
    {
        let host = Tracker::new(false);
        run(async {
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "test#create", "args": {"title": "First note"}}),
            )
            .await;
            assert!(!failed, "{text}");
            assert!(text.contains(&format!("-> {ITEM}")), "{text}");
            assert!(
                text.ends_with(&format!(
                    "link: {}",
                    url("")
                )),
                "the record's link is the fallback: {text}"
            );
        });
    }
}
