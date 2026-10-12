//! The active workspace's shortcuts, as the palette and `tilo-control` see
//! them.
//!
//! Every row comes from the action door itself: the broker's catalogue for
//! the local person in the active workspace, narrowed to the shortcuts app.
//! Invoking a row invokes that same published action through the broker, so
//! the palette reaches exactly what an agent, a peer or the MCP bridge
//! reaches, with the same authority checks — and a run shows up as a broker
//! job like any other background action.
//!
//! While the palette searches inside one resource
//! ([`SearchResourceScope`]), it offers instead the shortcuts that run on
//! that resource as quick actions: those whose first input takes resources
//! of its kind ([`quick_action_input`]). Such a row runs the shortcut with
//! the resource bound to that input.

use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionHost as _, Caller, CatalogueQuery, CatalogueRow, InvokeArgument, InvokeRequest,
    MAX_CATALOGUE_PAGE, PendingPoll, RemoteActionTarget, workspace_provider,
    workspace_provider_room,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_automation::{SHORTCUTS_APP_ID, quick_action_arguments, quick_action_input};

use crate::shell::automation_host::run_target;
use crate::shell::chrome_state::SearchResourceScope;
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::i18n::{t, t_args};
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
        self.row_result(caller, row, score, None)
    }

    /// The row running shortcut `row` on the resource the palette searches
    /// in, when it runs on resources of that kind.
    fn quick_action(
        &self,
        caller: &Caller,
        row: CatalogueRow,
        within: &SearchResourceScope,
        score: i32,
    ) -> Option<SearchResult> {
        let descriptor = row.action.as_ref()?;
        let input = quick_action_input(&descriptor.params, &within.kind)?;
        let bound = QuickAction {
            label: within.label.clone(),
            arguments: quick_action_arguments(input, &within.resource),
        };
        self.row_result(caller, row, score, Some(&bound))
    }

    fn row_result(
        &self,
        caller: &Caller,
        row: CatalogueRow,
        score: i32,
        quick: Option<&QuickAction>,
    ) -> Option<SearchResult> {
        let descriptor = row.action?;
        let bound = quick
            .map(|quick| quick.arguments.clone())
            .unwrap_or_default();
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
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  _chrome: &crate::shell::chrome_state::Chrome,
                  args: &tilo_actions::ActionArgs|
                  -> Result<ActionReceipt, ActionError> {
                let mut arguments = bound.clone();
                if !args.is_empty() {
                    arguments.extend(args.to_json());
                }
                if let Some(room) = workspace_provider_room(&target.provider)
                    && let Some(shortcut) = run_target(&target.address)
                {
                    return start_interactive_run(&broker, &caller, room, shortcut, arguments);
                }
                let invocation = InvocationId::new();
                let request = InvokeRequest {
                    on_behalf_of: None,
                    invocation,
                    target: target.clone(),
                    argument: if arguments.is_empty() {
                        InvokeArgument::None
                    } else {
                        InvokeArgument::Json(arguments)
                    },
                    context: None,
                    confirmation: None,
                    revision: None,
                    wait_ms: None,
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
        let (subtitle, params) = match quick {
            // The bound input is not asked for again.
            Some(quick) => (
                Some(t_args(
                    "search-shortcut-run-on",
                    &[("resource", quick.label.as_str().into())],
                )),
                descriptor
                    .params
                    .iter()
                    .filter(|param| !quick.arguments.contains_key(param.name.as_ref()))
                    .cloned()
                    .collect(),
            ),
            None => (descriptor.description.clone(), descriptor.params.clone()),
        };
        Some(SearchResult {
            kind: ResourceKind::Action,
            operation_kind: descriptor.kind,
            provider_id: PROVIDER_ID.into(),
            id: format!(
                "{}#{}",
                descriptor.address.resource, descriptor.address.action
            ),
            title: descriptor.action_label.clone(),
            subtitle,
            badge: Some(if quick.is_some() {
                t("search-badge-quick-action")
            } else {
                t("search-badge-shortcut")
            }),
            score,
            params,
            contract: Some(descriptor.contract),
            on_open: Rc::new(move |doc, root, chrome, args| {
                open(doc, root, chrome, args).map(drop)
            }),
            on_invoke: Some(invoke),
            on_begin_invoke: None,
        })
    }
}

/// A resource bound to a shortcut's first input.
struct QuickAction {
    /// The resource's label, for the row.
    label: String,
    arguments: Map<String, Value>,
}

/// Start a shortcut run for the person at the palette: the run asks them for
/// its confirmations and questions in the shell's sheet.
fn start_interactive_run(
    broker: &AppActionBroker,
    caller: &Caller,
    room: &str,
    shortcut: &str,
    arguments: Map<String, Value>,
) -> Result<ActionReceipt, ActionError> {
    match broker.start_interactive_shortcut_run(caller, room, shortcut, arguments) {
        Ok(job) => Ok(ActionReceipt {
            broker_invocation_id: Some(job),
            update: broker
                .status(job)
                .unwrap_or_else(|| InvocationUpdate::Accepted {
                    invocation: InvocationId::new(),
                }),
        }),
        Err(error) => Err(ActionError::Refused(error.to_string())),
    }
}

impl SearchProvider for ShortcutsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Shortcuts"
    }

    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-shortcuts")
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
        let within = ctx.chrome.search_resource_scope.clone();
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
                match &within {
                    Some(resource) => self.quick_action(&caller, row, resource, score),
                    None => self.result(&caller, row, score),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use automerge::AutoCommit;
    use tilo_actions::params::{ActionParam, ParamType};
    use tilo_automation::{Binding, Shortcut};

    use super::*;
    use crate::shell::chrome_state::{Chrome, ChromeState};
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

    /// A workspace the local person owns, holding a shortcut that runs on
    /// one file and hands the file back as its output, and one that takes
    /// nothing.
    fn chrome_with_file_shortcut() -> Chrome {
        let mut workspace = Workspace::new(WorkspaceId(1), "Quick actions");
        let owner = crate::shell::state::local_user_id_for_room(&workspace.room);
        let on_file = Shortcut {
            id: "archive-file".into(),
            name: "Archive a file".into(),
            inputs: vec![
                ActionParam::required("file", ParamType::Resource, "The file")
                    .of_resource_kind("files.entry"),
                ActionParam::optional("note", ParamType::Text, "Why"),
            ],
            steps: Vec::new(),
            output: Some(Binding::Input("file".into())),
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: Some(owner.clone()),
        };
        let mut plain = on_file.clone();
        plain.id = "tidy".into();
        plain.name = "Tidy".into();
        plain.inputs.clear();
        plain.output = None;
        workspace.set_owner(owner);
        workspace.shortcuts = vec![on_file, plain];
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        tree.desktop.active = WorkspaceId(1);
        ChromeState::new_with_handle(tree.shared())
    }

    fn scope(kind: &str) -> SearchResourceScope {
        SearchResourceScope {
            peer_id: 7,
            cluster_id: "room".into(),
            app_id: "so.tilo.files".into(),
            resource: format!("{kind}:0123"),
            label: "receipt.pdf".into(),
            kind: kind.into(),
        }
    }

    fn query(provider: &ShortcutsProvider, chrome: &Chrome) -> Vec<SearchResult> {
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    }

    fn ids(results: &[SearchResult]) -> Vec<String> {
        let mut ids: Vec<String> = results.iter().map(|row| row.id.clone()).collect();
        ids.sort();
        ids
    }

    #[test]
    fn a_resource_offers_the_shortcuts_that_run_on_its_kind_only() {
        let chrome = chrome_with_file_shortcut();
        let broker = crate::shell::broker_services::test_broker_with_membership(
            chrome.borrow().tree_handle(),
        );
        let provider = ShortcutsProvider::new(broker.clone());
        let everything = ids(&query(&provider, &chrome));
        assert!(
            everything.contains(&"shortcuts.shortcut:archive-file#run".to_string())
                && everything.contains(&"shortcuts.shortcut:tidy#run".to_string()),
            "outside a resource every shortcut is listed: {everything:?}"
        );

        chrome.borrow_mut().search_resource_scope = Some(scope("calendar.event"));
        assert!(
            query(&provider, &chrome).is_empty(),
            "an event is offered no file shortcut"
        );

        chrome.borrow_mut().search_resource_scope = Some(scope("files.entry.image"));
        let offered = query(&provider, &chrome);
        assert_eq!(ids(&offered), ["shortcuts.shortcut:archive-file#run"]);
        let row = &offered[0];
        assert_eq!(row.badge.as_deref(), Some("Quick action"));
        assert_eq!(row.subtitle.as_deref(), Some("Run on receipt.pdf"));
        assert_eq!(
            row.params
                .iter()
                .map(|param| param.name.as_ref())
                .collect::<Vec<_>>(),
            ["note"],
            "the file is bound, not asked for"
        );

        let invoke = row.on_invoke.clone().expect("the row runs");
        let mut doc = AutoCommit::new();
        let receipt = invoke(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &tilo_actions::ActionArgs::none(),
        )
        .expect("the quick action starts");
        let job = receipt.broker_invocation_id.expect("a broker job");
        let deadline = tilo_test_env::hang_deadline();
        let output = loop {
            match broker.status(job) {
                Some(InvocationUpdate::Succeeded { output, .. }) => break output,
                Some(InvocationUpdate::Failed { error, .. }) => {
                    panic!("the quick action failed: {}", error.message)
                }
                _ => {}
            }
            assert!(Instant::now() < deadline, "the quick action never finished");
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(provider);
        drop(broker);
        assert_eq!(
            output.value,
            Some(serde_json::json!("files.entry.image:0123")),
            "the run received the resource through its first input"
        );
    }
}
