//! The active workspace's shortcuts, as the palette and `tilo-control` see
//! them.
//!
//! Every row comes from the action door itself: the broker's catalogue for
//! the local person in the active workspace, narrowed to the shortcuts app.
//! Invoking a row invokes that same published action through the broker, so
//! the palette reaches exactly what an agent, a peer or the MCP bridge
//! reaches, with the same authority checks — and a run shows up as a broker
//! job like any other background action.

use std::rc::Rc;

use tilo_actions::remote::{
    ActionHost as _, Caller, CatalogueQuery, CatalogueRow, InvokeArgument, InvokeRequest,
    MAX_CATALOGUE_PAGE, PendingPoll, RemoteActionTarget, workspace_provider,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_automation::SHORTCUTS_APP_ID;

use crate::shell::collab_broker::AppActionBroker;
use crate::shell::remote_actions::BrokerActionHost;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{
    ActionReceipt, ResourceKind, SearchContext, SearchProvider, SearchResult,
};

pub const PROVIDER_ID: &str = "shortcuts";

pub struct ShortcutsProvider {
    broker: AppActionBroker,
}

impl ShortcutsProvider {
    #[must_use]
    pub fn new(broker: AppActionBroker) -> Self {
        Self { broker }
    }

    fn result(&self, caller: &Caller, row: CatalogueRow, score: i32) -> Option<SearchResult> {
        let descriptor = row.action?;
        let broker = self.broker.clone();
        let caller = caller.clone();
        let target = RemoteActionTarget {
            provider: row.provider,
            address: ActionAddress::new(
                descriptor.address.resource.clone(),
                descriptor.address.action.clone(),
            ),
        };
        let invoke = Rc::new(
            move |_doc: &mut automerge::AutoCommit,
                  _root: &automerge::ObjId,
                  _chrome: &crate::shell::chrome_state::Chrome,
                  args: &tilo_actions::ActionArgs|
                  -> Result<ActionReceipt, ActionError> {
                let invocation = InvocationId::new();
                let request = InvokeRequest {
                    invocation,
                    target: target.clone(),
                    argument: if args.is_empty() {
                        InvokeArgument::None
                    } else {
                        InvokeArgument::Json(args.to_json())
                    },
                    context: None,
                    confirmation: None,
                    revision: None,
                };
                let mut admitted =
                    BrokerActionHost::new(broker.clone()).invoke_job(&caller, request);
                match admitted.poll_ready() {
                    PendingPoll::Ready(Ok((job, _))) => Ok(ActionReceipt {
                        broker_invocation_id: Some(job),
                        update: broker
                            .status(job)
                            .unwrap_or(InvocationUpdate::Accepted { invocation }),
                    }),
                    PendingPoll::Ready(Err(error)) => Err(ActionError::Refused(error.to_string())),
                    PendingPoll::Waiting | PendingPoll::Abandoned => Err(ActionError::Refused(
                        "the action broker did not admit the shortcut".into(),
                    )),
                }
            },
        );
        let open = Rc::clone(&invoke);
        Some(SearchResult {
            kind: ResourceKind::Action,
            operation_kind: descriptor.kind,
            provider_id: PROVIDER_ID.into(),
            id: format!(
                "{}#{}",
                descriptor.address.resource, descriptor.address.action
            ),
            title: descriptor.action_label.clone(),
            subtitle: descriptor.description.clone(),
            badge: Some("Shortcut".into()),
            score,
            params: descriptor.params.clone(),
            contract: Some(descriptor.contract),
            on_open: Rc::new(move |doc, root, chrome, args| {
                open(doc, root, chrome, args).map(drop)
            }),
            on_invoke: Some(invoke),
            on_begin_invoke: None,
        })
    }
}

impl SearchProvider for ShortcutsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Shortcuts"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let Some(room) = ctx
            .chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room)
        else {
            return Vec::new();
        };
        let provider = workspace_provider(&room.to_string());
        let caller =
            Caller::local_person(ctx.chrome.local_user_for_room(&room)).in_room(room.to_string());
        let mut page = BrokerActionHost::new(self.broker.clone()).query(
            &caller,
            CatalogueQuery {
                provider: Some(provider),
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        let PendingPoll::Ready(Ok(page)) = page.poll_ready() else {
            return Vec::new();
        };
        let needle = ctx.needle.trim().to_lowercase();
        page.rows
            .into_iter()
            .filter(|row| row.app_id == SHORTCUTS_APP_ID)
            .filter_map(|row| {
                let descriptor = row.action.as_ref()?;
                let haystack = format!(
                    "{} {} {}#{}",
                    descriptor.action_label.to_lowercase(),
                    descriptor.resource_label.to_lowercase(),
                    descriptor.address.resource,
                    descriptor.address.action
                );
                let score = if needle.is_empty() {
                    0
                } else if haystack.contains(&needle) {
                    80
                } else {
                    return None;
                };
                self.result(&caller, row, score)
            })
            .collect()
    }
}
