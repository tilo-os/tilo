//! What only the loop can answer, as the walk's resources and actions: the desktop's state, each
//! window's semantics tree, and a press on a control.
//!
//! The provider a session serves its own actions through is not this one: this sits beside it and
//! answers the resources and actions the loop's own eye makes — the layout, the windows with where
//! they stand and how many frames they drew, the questions waiting for the person, the search rows
//! the shell shows, the devices keeping a workspace, and the state dump a tool reads to see the
//! desktop as it is. A press on a control is that node's accessibility action, addressed
//! `ui:<SemanticId>#<action>`, the one path a person's assistive tool and the page use.
//!
//! The loop fills the [`Snapshot`] it answers from on its pass ([`SessionProvider::keep`]); a read
//! is therefore never the desktop as it was before the pass that read it. The provider is plain so
//! another host can mount it: [`SessionProvider::new`] takes the provider a session already serves
//! and the snapshot to serve beside it, and [`SessionProvider::with_press`] where a press runs.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    ContentGets, ContentPuts, Here, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    Readings, ReceiptStatus, ResourceChanges, ResourceReads, ResourceSearch, SessionState,
    WatchScope,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionOutput, ActionState, ExecutionLocus,
    InputSpec, InvocationId, OperationKind, ResultSpec,
};
use tilo_lock::MutexRecover as _;

/// The provider the session's own resources and actions are published under.
pub const PROVIDER: &str = "so.tilo.session";

/// The resource the whole desktop state is read as: what `tilo-control state-dump` prints.
pub const STATE: &str = "state";
/// The windows with the surface each fills, where its frame draws it, and how many frames it drew.
pub const WINDOWS: &str = "windows";
/// The person's layout, every workspace's rows, strips, groups and places.
pub const LAYOUT: &str = "layout";
/// The questions waiting for the person, as their sheets show them.
pub const QUESTIONS: &str = "questions";
/// What the shell's open search shows: its query, its rows, and whether it still waits for sources.
pub const SEARCH: &str = "search";
/// The devices keeping each workspace this computer holds, and whether its host took it in.
pub const DEVICES: &str = "devices";

/// The action a resource is read through.
pub const READ: &str = "read";

/// The prefix a control's node is addressed by: `ui:<SemanticId>`.
pub const NODE_PREFIX: &str = "ui:";

/// What the loop keeps of the desktop for the resources above, as the tools that drive a desktop
/// read them: the same values the control socket's own reads answered.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub state: Value,
    pub windows: Value,
    pub layout: Value,
    pub questions: Value,
    pub search: Value,
    pub devices: Value,
}

impl Snapshot {
    /// The value the resource `resource` is read as, when the loop has filled it.
    #[must_use]
    pub fn read(&self, resource: &str) -> Option<&Value> {
        Some(match resource {
            STATE => &self.state,
            WINDOWS => &self.windows,
            LAYOUT => &self.layout,
            QUESTIONS => &self.questions,
            SEARCH => &self.search,
            DEVICES => &self.devices,
            _ => return None,
        })
    }
}

/// Where a press on a control runs: the node `node` (`<SemanticId>`) is acted on as `action`
/// says, through the same accessibility action a person's assistive tool and the page use.
pub trait Press: Send + Sync {
    /// Press the control `node` as `action`.
    ///
    /// # Errors
    /// No node is shown by that id, or it answers nothing of `action`.
    fn press(&self, node: &str, action: &str) -> Result<Value, String>;
}

/// The session's own resources and actions, served beside the provider it already serves.
pub struct SessionProvider<H: ActionProvider> {
    /// The provider a session serves its actions through.
    inner: Arc<H>,
    /// What the loop last read of the desktop.
    snapshot: Arc<Mutex<Snapshot>>,
    /// Where a press runs, once a host that shows windows hands it here.
    press: Option<Arc<dyn Press>>,
}

impl<H: ActionProvider> SessionProvider<H> {
    /// `inner` served beside the loop's `snapshot`.
    #[must_use]
    pub fn new(inner: Arc<H>, snapshot: Arc<Mutex<Snapshot>>) -> Self {
        Self {
            inner,
            snapshot,
            press: None,
        }
    }

    /// The same provider, pressing through `press`.
    #[must_use]
    pub fn with_press(mut self, press: Arc<dyn Press>) -> Self {
        self.press = Some(press);
        self
    }

    /// The snapshot the loop fills.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Mutex<Snapshot>> {
        Arc::clone(&self.snapshot)
    }

    /// The loop read the desktop: what the resources answer from now.
    pub fn keep(&self, snapshot: Snapshot) {
        *self.snapshot.lock_recover() = snapshot;
    }

    /// The rows the resources are listed as.
    fn rows() -> Vec<tilo_actions::remote::CatalogueRow> {
        [
            (STATE, "State"),
            (WINDOWS, "Windows"),
            (LAYOUT, "Layout"),
            (QUESTIONS, "Questions"),
            (SEARCH, "Search"),
            (DEVICES, "Devices"),
        ]
        .into_iter()
        .map(|(resource, label)| {
            tilo_actions::remote::CatalogueRow::for_action(
                PROVIDER,
                PROVIDER,
                read_descriptor(resource, label),
            )
        })
        .collect()
    }
}

/// Whether `row` is the one a listing asked for: its resource named by `resource_name`, and
/// `needle` met by its id, its label or its action's label.
fn names(
    row: &tilo_actions::remote::CatalogueRow,
    needle: &str,
    resource_name: Option<&str>,
) -> bool {
    if let Some(name) = resource_name
        && !row.resource.id.eq_ignore_ascii_case(name)
        && !row.resource.kind.eq_ignore_ascii_case(name)
        && !row.resource.label.eq_ignore_ascii_case(name)
    {
        return false;
    }
    if needle.is_empty() {
        return true;
    }
    let needle = needle.to_lowercase();
    let action_label = row
        .action
        .as_ref()
        .map(|action| action.action_label.to_lowercase())
        .unwrap_or_default();
    row.resource.id.to_lowercase().contains(&needle)
        || row.resource.label.to_lowercase().contains(&needle)
        || action_label.contains(&needle)
}

/// The `read` action of `resource`, as the catalogue lists it.
fn read_descriptor(resource: &str, label: &str) -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        state: ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(resource, READ),
        resource_kind: String::new(),
        resource_label: label.to_owned(),
        action_label: "Read".to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: Some(label.to_owned()),
        locus: ExecutionLocus::default(),
        kind: OperationKind::View,
        contract: ActionContract {
            result: ResultSpec::Json {
                schema: serde_json::json!({ "type": "object" }),
            },
            ..ActionContract::view()
        },
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// An answer that reads `value`.
fn completed(invocation: InvocationId, value: Value) -> InvocationReceipt {
    InvocationReceipt {
        invocation,
        status: ReceiptStatus::Completed {
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
    }
}

impl<H: ActionProvider> ActionProvider for SessionProvider<H> {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // The session's own rows stand beside whatever the provider it serves lists: one
        // catalogue, whoever reads it. A query for another provider, or for one resource's
        // children, is that provider's alone.
        let ours = if query.canonical_route.is_none()
            && query.view_resource.is_none()
            && query.target.is_none()
            && query.parent_resource.as_deref().is_none_or(str::is_empty)
            && query
                .provider
                .as_deref()
                .is_none_or(|provider| provider == PROVIDER)
            && query.app_id.as_deref().is_none_or(|app| app == PROVIDER)
        {
            Self::rows()
                .into_iter()
                .filter(|row| names(row, &query.needle, query.resource_name.as_deref()))
                .collect()
        } else {
            Vec::new()
        };
        self.inner.query(caller, query).map(move |page| {
            page.map(|mut page| {
                page.rows.extend(ours);
                page
            })
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.target.provider == PROVIDER {
            let resource = request.target.address.resource.as_str();
            let action = request.target.address.action.as_str();
            if let Some(node) = resource.strip_prefix(NODE_PREFIX) {
                let Some(press) = &self.press else {
                    return Pending::ready(Err(ActionCallError::UnknownAction));
                };
                return Pending::ready(
                    press
                        .press(node, action)
                        .map(|value| completed(request.invocation, value))
                        .map_err(|reason| ActionCallError::Refused { reason }),
                );
            }
            if action == READ {
                // The desktop state is the person's at this device: an agent acts through the
                // resources its grants publish, never by reading the person's screen state.
                if !caller.acting().is_person() {
                    return Pending::ready(Err(ActionCallError::NotPermitted));
                }
                let read = self
                    .snapshot
                    .lock_recover()
                    .read(resource)
                    .cloned()
                    .ok_or_else(|| ActionCallError::Unavailable {
                        reason: "the session has not read its desktop yet".to_owned(),
                    });
                return Pending::ready(read.map(|value| completed(request.invocation, value)));
            }
        }
        self.inner.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.inner.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.inner.next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<SessionState> {
        self.inner.session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        self.inner.session_state_changed(caller)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.inner.changed(caller, scope)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.inner.cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        self.inner.confirm(caller, request)
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        self.inner.content_puts()
    }

    fn content_gets(&self) -> Option<Arc<dyn ContentGets>> {
        self.inner.content_gets()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<Here>> {
        self.inner.here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        self.inner.resource_reads()
    }

    fn readings(&self) -> Option<Arc<dyn Readings>> {
        self.inner.readings()
    }

    fn resource_changes(&self) -> Option<Arc<dyn ResourceChanges>> {
        self.inner.resource_changes()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.inner.change_feed()
    }

    fn resource_search(&self) -> Option<Arc<dyn ResourceSearch>> {
        self.inner.resource_search()
    }

    fn retire_execution(&self, caller: &Caller) {
        self.inner.retire_execution(caller);
    }
}
