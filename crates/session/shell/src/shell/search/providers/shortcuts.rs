//! The active workspace's shortcuts, as the palette and `tilo-control` see
//! them.
//!
//! Every row comes from the action door itself: the broker's catalogue for
//! the local person in the active workspace, narrowed to the shortcuts app
//! before the broker pages it. Invoking a row invokes that same published
//! action through the broker, so the palette reaches exactly what an agent, a
//! peer or the MCP bridge reaches, with the same authority checks — and a run
//! shows up as a broker job like any other background action.
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
use crate::shell::i18n::t_args;
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
                invoke_through_broker(&broker, &caller, &target, arguments)
            },
        );
        let open = Rc::clone(&invoke);
        let (subtitle, params) = subtitle_and_params(&descriptor, quick);
        Some(SearchResult {
            facts: Vec::new(),
            shortcut: None,
            kind: ResourceKind::Action,
            operation_kind: descriptor.kind,
            provider_id: PROVIDER_ID.into(),
            id: format!(
                "{}#{}",
                descriptor.address.resource, descriptor.address.action
            ),
            title: descriptor.action_label.clone(),
            subtitle,
            badge: None,
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

/// A row's subtitle and the inputs it still asks for: a quick action names
/// the resource it runs on and does not ask for its bound input again.
fn subtitle_and_params(
    descriptor: &tilo_actions::ActionDescriptor,
    quick: Option<&QuickAction>,
) -> (Option<String>, Vec<tilo_actions::ActionParam>) {
    match quick {
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
    }
}

/// A resource bound to a shortcut's first input.
struct QuickAction {
    /// The resource's label, for the row.
    label: String,
    arguments: Map<String, Value>,
}

/// Ask the action broker to run `target` with `arguments` for `caller`, and
/// answer with the job it admitted.
fn invoke_through_broker(
    broker: &AppActionBroker,
    caller: &Caller,
    target: &RemoteActionTarget,
    arguments: Map<String, Value>,
) -> Result<ActionReceipt, ActionError> {
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
        appearance: None,
    };
    let mut admitted = BrokerActionHost::new(broker.clone()).invoke_job(caller, request);
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
            .map(crate::shell::state::Workspace::room)
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
                // The host builds and pages only the shortcuts app's rows, so
                // the governance, keyboard and other applications' descriptors
                // a keypress would otherwise pay for are never built.
                app_id: Some(SHORTCUTS_APP_ID.into()),
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        let PendingPoll::Ready(Ok(page)) = page.poll_ready() else {
            return Vec::new();
        };
        let within = ctx.chrome.search_resource_scope.clone();
        let (rows, fields): (Vec<SearchResult>, Vec<_>) = page
            .rows
            .into_iter()
            // Defensive: the host already narrowed to this app, and a row of
            // another application is never offered as a shortcut.
            .filter(|row| row.app_id == SHORTCUTS_APP_ID)
            .filter_map(|row| {
                let descriptor = row.action.as_ref()?;
                let fields = vec![
                    (
                        descriptor.action_label.clone(),
                        ui::search::FieldWeight::TITLE,
                    ),
                    (
                        descriptor.resource_label.clone(),
                        ui::search::FieldWeight::SUBTITLE,
                    ),
                    (
                        format!(
                            "{}#{}",
                            descriptor.address.resource, descriptor.address.action
                        ),
                        ui::search::FieldWeight::IDENTITY,
                    ),
                ];
                let result = match &within {
                    Some(resource) => self.quick_action(&caller, row, resource, 0),
                    None => self.result(&caller, row, 0),
                }?;
                Some((result, fields))
            })
            .unzip();
        crate::shell::search::rank_rows(
            &ui::search::ListIndex::default(),
            ctx.needle.trim(),
            rows,
            &fields,
        )
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
        let owner = crate::shell::state::local_user_id_for_room(&workspace.room());
        let on_file = Shortcut {
            id: "archive-file".into(),
            name: "Archive a file".into(),
            inputs: vec![
                ActionParam::required("file", ParamType::Resource, "The file")
                    .of_resource_kind("files.entry"),
                ActionParam::optional("note", ParamType::Text, "Why"),
            ],
            steps: Vec::new(),
            step_timeout_secs: None,
            output: Some(Binding::Input("file".into())),
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: Some(owner.clone()),
            saved_proof: None,
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
        assert_eq!(
            row.badge, None,
            "the trailing slot is for live state, not the kind"
        );
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

    /// The page the broker's action door answers `caller` with.
    fn broker_page(
        broker: &AppActionBroker,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> tilo_actions::remote::CataloguePage {
        let mut pending = BrokerActionHost::new(broker.clone()).query(caller, query);
        let PendingPoll::Ready(Ok(page)) = pending.poll_ready() else {
            panic!("an eager catalogue answers at once");
        };
        page
    }

    /// The room and local person of `chrome`'s active workspace.
    fn room_and_caller(chrome: &Chrome) -> (String, Caller) {
        let state = chrome.borrow();
        let room = state
            .tree()
            .desktop
            .active_workspace()
            .map(Workspace::room)
            .expect("a workspace");
        let caller =
            Caller::local_person(state.local_user_for_room(&room)).in_room(room.to_string());
        (room.to_string(), caller)
    }

    #[test]
    fn an_app_scoped_query_answers_the_same_shortcut_rows_as_a_filtered_catalogue() {
        let chrome = chrome_with_file_shortcut();
        let (room, caller) = room_and_caller(&chrome);
        let broker = crate::shell::broker_services::test_broker_with_membership(
            chrome.borrow().tree_handle(),
        );
        let provider = workspace_provider(&room);
        let scoped = broker_page(
            &broker,
            &caller,
            CatalogueQuery {
                provider: Some(provider.clone()),
                app_id: Some(SHORTCUTS_APP_ID.into()),
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        let unfiltered = broker_page(
            &broker,
            &caller,
            CatalogueQuery {
                provider: Some(provider.clone()),
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        assert_eq!(
            scoped.rows,
            unfiltered
                .rows
                .iter()
                .filter(|row| row.app_id == SHORTCUTS_APP_ID)
                .cloned()
                .collect::<Vec<_>>(),
            "the host narrows before paging, so the same shortcut rows come back"
        );
        let mut run_resources: Vec<&str> = scoped
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .filter(|action| action.address.action == "run")
            .map(|action| action.address.resource.as_str())
            .collect();
        run_resources.sort_unstable();
        assert_eq!(
            run_resources,
            ["shortcuts.shortcut:archive-file", "shortcuts.shortcut:tidy"],
            "both of the workspace's shortcuts are offered"
        );
        assert!(
            unfiltered.rows.len() > scoped.rows.len(),
            "the unscoped catalogue also offers other applications' rows"
        );

        // Naming no provider still answers the aggregate catalogue, narrowed
        // to the app: the workspace provider stays discoverable that way.
        let aggregate = broker_page(
            &broker,
            &caller,
            CatalogueQuery {
                app_id: Some(SHORTCUTS_APP_ID.into()),
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        drop(broker);
        assert!(
            aggregate
                .rows
                .iter()
                .all(|row| row.app_id == SHORTCUTS_APP_ID),
            "no other application's rows are offered"
        );
        assert_eq!(
            aggregate
                .rows
                .iter()
                .filter(|row| row.provider == provider)
                .cloned()
                .collect::<Vec<_>>(),
            scoped.rows,
            "the workspace provider's rows are the same with no provider named"
        );
    }

    #[test]
    fn an_app_scoped_query_still_denies_a_stranger() {
        let chrome = chrome_with_file_shortcut();
        let (room, _) = room_and_caller(&chrome);
        let broker = crate::shell::broker_services::test_broker_with_membership(
            chrome.borrow().tree_handle(),
        );
        let stranger = Caller::authenticated([9; 32], [8; 32]).in_room(room.clone());
        let page = broker_page(
            &broker,
            &stranger,
            CatalogueQuery {
                provider: Some(workspace_provider(&room)),
                app_id: Some(SHORTCUTS_APP_ID.into()),
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        drop(broker);
        assert!(
            page.rows.is_empty(),
            "narrowing by app never offers a caller without a role anything"
        );
    }
}
