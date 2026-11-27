//! Lifetime and canonical-record routing shared by retained and raw widget dispatch.
#[cfg(feature = "model")]
mod bound_widget;
#[cfg(all(feature = "model", not(any(test, feature = "test-support"))))]
pub(crate) use bound_widget::history_owner;
#[cfg(all(feature = "model", any(test, feature = "test-support")))]
#[doc(hidden)]
pub use bound_widget::history_owner;
#[cfg(feature = "model")]
#[cfg(test)]
pub(crate) use bound_widget::keyboard_input_scope;
#[cfg(feature = "model")]
pub(crate) use bound_widget::{BoundTraversal, visit_components, visit_resource_owners};
#[cfg(feature = "model")]
pub use bound_widget::{BoundWidget, callback_binding, resource_owners};
#[cfg(feature = "model")]
pub(crate) mod local_target;

#[cfg(feature = "model")]
mod published_target;
#[cfg(feature = "model")]
use published_target::{resolve_published_action, shared_target};

#[cfg(feature = "model")]
mod executing;
#[cfg(feature = "model")]
use executing::{ExecutingHostDevice, ExecutingHostPerson, ExecutingScope};
#[cfg(feature = "model")]
pub use executing::{
    executing_device, executing_person, executing_scope, local_person_invocation_permitted,
};
use tilo_actions::ResourceSpec;

mod node_map;
mod resource_walk;
#[cfg(feature = "model")]
pub use node_map::document_edit_stamp;
pub use node_map::invoke_bound_node;
#[cfg(feature = "model")]
pub(crate) use node_map::{bound_target, callback_binding_of, component_control};
#[cfg(feature = "model")]
pub(crate) use resource_walk::action_directory_page_at;
pub(crate) use resource_walk::collect_widget_resources;
#[cfg(feature = "model")]
pub use resource_walk::published_search_providers;
#[cfg(test)]
#[cfg(feature = "model")]
pub(crate) use resource_walk::resolve_widget_resource;
pub use resource_walk::{
    find_resource, past_page_labels, past_page_len, past_page_range, published_resources,
    resources_past_page,
};

#[cfg(feature = "model")]
mod hosted;
#[cfg(feature = "model")]
pub(crate) use hosted::{HostedInvocation, published_here};

#[cfg(feature = "model")]
mod deferred_intent;
#[cfg(feature = "model")]
mod filter;
#[cfg(feature = "model")]
use deferred_intent::validate_deferred_arguments;
#[cfg(feature = "model")]
pub(crate) use deferred_intent::{DeferredIntent, PreparedActionIntent};
#[cfg(feature = "model")]
pub(crate) use filter::{filter_resource, filter_shared};
#[cfg(feature = "model")]
use filter::{inherit_filtered, insert_available_permitted};

#[cfg(feature = "model")]
mod control;
#[cfg(feature = "model")]
pub(crate) use control::{
    ControlContinuationArguments, ControlDeclaration, ControlIdentity, ControlObservation,
    control_declaration, control_draft_identity, control_observation,
    control_observation_with_arguments,
};

#[cfg(feature = "model")]
use crate::published::SharedResource;
#[cfg(feature = "model")]
use crate::resource::{DocumentCheckout, ResourceOpenError, ResourceSaveStatus, ResourceStore};
#[cfg(feature = "model")]
use crate::wake::{WakeHandle, WakeScopeGuard};
#[cfg(feature = "model")]
use automerge::{AutoCommit, ChangeHash, ObjId, ROOT, transaction::Transactable};
#[cfg(feature = "model")]
use std::cell::{Cell, RefCell};
#[cfg(feature = "model")]
use std::collections::HashMap;
#[cfg(feature = "model")]
use std::rc::Rc;
#[cfg(all(feature = "model", feature = "native"))]
use taffy::NodeId;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::widget::bound_nodes::BoundNodeMap;
#[cfg(feature = "model")]
use tilo_ui_base::widget::component_walk::has_separate_document_owner;
#[cfg(feature = "model")]
use tilo_ui_base::{
    permissions::MemberId, permissions::PermissionContext, permissions::SharedPermissions,
};

#[cfg(feature = "model")]
mod action_part;
#[cfg(feature = "model")]
pub(crate) use action_part::ToolkitPart;

#[cfg(feature = "model")]
use tilo_ui_base::widget::input_provenance::Input;
#[cfg(feature = "model")]
use tilo_ui_base::{
    widget::input_provenance::capture_physical_drain, widget::input_provenance::native_text_input,
    widget::input_provenance::with_native_text,
};
#[cfg(test)]
use tilo_ui_base::{
    widget::input_provenance::physical_text, widget::input_provenance::with_physical_text,
};

#[cfg(feature = "model")]
use tilo_ui_base::widget::controls::Origin;

/// Runtime destination; the domain address remains independent of UI instances.
#[cfg(feature = "model")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundActionAddress {
    pub instance_id: crate::application::InstanceId,
    pub address: tilo_actions::ActionAddress,
}

#[cfg(feature = "model")]
impl BoundActionAddress {
    /// The address of `action` on `resource` as `instance` publishes it, or `None` when either half is
    /// empty or contains the address separator.
    #[must_use]
    pub fn new(
        instance_id: crate::application::InstanceId,
        resource: impl AsRef<str>,
        action: impl AsRef<str>,
    ) -> Option<Self> {
        Some(Self {
            instance_id,
            address: tilo_actions::ActionAddress::checked(resource, action)?,
        })
    }
}

#[cfg(feature = "model")]
#[derive(Debug)]
pub(crate) enum BoundActionError {
    Api(tilo_actions::ApiError),
    Dispatch(BoundDispatchError),
    AmbiguousTarget {
        address: tilo_actions::ActionAddress,
    },
    /// The resolved action runs through the workspace broker, so the API handed it back before
    /// anything ran ([`ResourceActionRequest::route`]): the arguments it bound to the resource,
    /// so the host routes the same call instead of resolving it a second time from a tree that
    /// need not list what its own resolution found.
    Rerouted {
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    },
    /// The resolved action is another provider's ([`tilo_actions::ActionSpec::run_by`]): nothing ran
    /// here, and the host invokes it at that provider.
    Hosted(HostedInvocation),
}

#[cfg(feature = "model")]
impl BoundActionError {
    pub(crate) fn into_api(self) -> tilo_actions::ApiError {
        match self {
            Self::Api(error) => error,
            error => tilo_actions::ApiError::Unavailable {
                code: error.code().to_owned(),
                reason: error.to_string(),
            },
        }
    }
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Api(error) => error.code(),
            Self::Dispatch(BoundDispatchError::CrossResourceReentry) => "cross_resource_reentry",
            Self::Dispatch(BoundDispatchError::ClosedLifetime) => "closed_lifetime",
            Self::Dispatch(_) => "resource_unavailable",
            Self::AmbiguousTarget { .. } => "ambiguous_action_target",
            Self::Rerouted { .. } => "bound_broker_route_required",
            Self::Hosted(_) => "provider_unavailable",
        }
    }
}
#[cfg(feature = "model")]
impl std::fmt::Display for BoundActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(error) => error.fmt(f),
            Self::Dispatch(error) => write!(f, "component dispatch refused: {error:?}"),
            Self::AmbiguousTarget { address } => {
                write!(
                    f,
                    "more than one authorized component accepts {}",
                    address.encode()
                )
            }
            Self::Rerouted { .. } => write!(f, "the action runs through the workspace broker"),
            Self::Hosted(hosted) => write!(f, "the action runs at `{}`", hosted.provider),
        }
    }
}
#[cfg(feature = "model")]
impl std::error::Error for BoundActionError {}

/// Why a resource's operation failed, as the API answers it: a resource that
/// says what an action changes has not arrived yet
/// ([`tilo_actions::api::NOT_READY_REASON`]) answers a refusal whose caller
/// asks again, rather than a failure that says nothing was made.
#[cfg(feature = "model")]
fn operation_failure(reason: String) -> tilo_actions::ApiError {
    if reason == tilo_actions::api::NOT_READY_REASON {
        return tilo_actions::ApiError::Unavailable {
            code: tilo_actions::api::NOT_READY.to_owned(),
            reason,
        };
    }
    tilo_actions::ApiError::Failed(reason)
}

/// [`operation_failure`] for a refusal the handler named: a code a caller
/// decides on reaches it as that code (a message the mailbox holds already),
/// and one this API answers itself — a write waiting for what it changes —
/// keeps its own answer.
#[cfg(feature = "model")]
fn operation_refusal(refusal: tilo_resource::Refusal) -> tilo_actions::ApiError {
    match refusal.code() {
        Some(code) if code != tilo_actions::api::NOT_READY => {
            tilo_actions::ApiError::refused(tilo_actions::ActionError::from(refusal))
        }
        _ => operation_failure(refusal.into_reason()),
    }
}

/// The unchanged domain enum groups Unavailable failures.
#[cfg(feature = "model")]
pub(crate) fn api_error_code(error: &tilo_actions::ApiError) -> &str {
    if let tilo_actions::ApiError::Unavailable { code, .. } = error
        && matches!(
            code.as_str(),
            "cross_resource_reentry"
                | "ambiguous_action_target"
                | "bound_broker_route_unavailable"
                | "undeclared_document_mutation"
        )
    {
        return code;
    }
    error.code()
}
#[cfg(feature = "model")]
impl From<tilo_actions::ApiError> for BoundActionError {
    fn from(error: tilo_actions::ApiError) -> Self {
        Self::Api(error)
    }
}
#[cfg(feature = "model")]
impl From<BoundDispatchError> for BoundActionError {
    fn from(error: BoundDispatchError) -> Self {
        Self::Dispatch(error)
    }
}

/// A UI selection retains the captured lifetime and registry owner, never just
/// the reusable numeric instance or a domain resource ID.
#[cfg(feature = "model")]
#[derive(Clone)]
pub(crate) struct BoundActionTarget {
    pub(crate) input: Input,
    pub(crate) binding: CallbackBinding,
    pub(crate) address: BoundActionAddress,
}
#[cfg(feature = "model")]
impl BoundActionTarget {
    pub(crate) fn key(&self) -> String {
        format!(
            "i:{}:{}:{}",
            self.address.instance_id,
            self.binding.lifetime_id,
            self.address.address.encode()
        )
    }
}

#[cfg(feature = "model")]
pub(crate) fn has_matching_broker_component(
    root: &dyn Widget,
    route: Option<&tilo_ui_base::collaboration::AcceptedRoute>,
) -> bool {
    if callback_binding(root)
        .is_some_and(|binding| binding.enter().is_ok() && binding.matches_broker_route(route))
    {
        return true;
    }
    let mut found = false;
    root.for_each_child(&mut |child| {
        if !found {
            found = has_matching_broker_component(child, route);
        }
    });
    found
}

/// Refresh every component control's availability from its resources, plus the APIs its host answers
/// for it (`host_apis`): the window's History API wherever History serves it.
#[cfg(feature = "model")]
pub(crate) fn refresh_bound_controls(
    root: &mut dyn Widget,
    snapshots: &[BoundResourceSnapshot],
    host_apis: &std::collections::HashSet<tilo_actions::ActionAddress>,
) -> Result<(), BoundDispatchError> {
    let mut tile_apis = Vec::new();
    crate::history_mode::capability::visit_framework_history(&*root, &mut |history, _| {
        if let Some(owner) = history.enclosing_owner() {
            tile_apis.push((owner.clone(), history.api_addresses()));
        }
    });
    let mut visit = |binding: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        let owner = crate::history_mode::HistoryOwner::component(binding.clone());
        let mut addresses = host_apis.clone();
        addresses.insert(crate::widget::search_request::search_address());
        addresses.extend(
            tile_apis
                .iter()
                .filter(|(enclosing, _)| enclosing.same_owner(&owner))
                .flat_map(|(_, addresses)| addresses.iter().cloned()),
        );
        let allowed = component_controls_allowed(binding, snapshots, addresses);
        tilo_ui_base::widget::controls::refresh_control_action_availability(widget, &allowed);
        None
    };
    visit_components(root, &mut ComponentRequest::new(None, &mut visit))
}

/// The addresses a component's controls may run: every available action of
/// its published resources that its permissions admit, and `APIs`, what
/// its host answers. Derived from the tree once, then kept: the same forest
/// (a cached snapshot is handed to every frame until the component's
/// declarations change) reuses it as it is, and the next forest,
/// which shares every subtree an edit did not touch, updates the addresses of
/// the nodes that changed only. Either needs the same permissions
/// and APIs; anything else derives afresh.
#[cfg(feature = "model")]
pub(crate) fn component_controls_allowed(
    binding: &CallbackBinding,
    snapshots: &[BoundResourceSnapshot],
    addresses: std::collections::HashSet<tilo_actions::ActionAddress>,
) -> Rc<std::collections::HashSet<tilo_actions::ActionAddress>> {
    let mut owned = snapshots
        .iter()
        .filter(|snapshot| snapshot.binding.same_component(binding));
    let only = match (owned.next(), owned.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    };
    let permissions = binding.permissions.borrow().directory_observation().ok();
    if let (Some(snapshot), Some(permissions)) = (only, permissions)
        && let Some(forest) = snapshot.shared_forest()
    {
        let mut memo = snapshot.controls.borrow_mut();
        if let Some(memo) = memo
            .as_mut()
            .filter(|memo| memo.permissions == permissions && memo.addresses == addresses)
        {
            if !Rc::ptr_eq(&memo.forest, forest) {
                memo.update(binding, forest);
            }
            return Rc::clone(&memo.allowed);
        }
        let derived = ControlAvailability::derive(binding, forest, permissions, addresses);
        let allowed = Rc::clone(&derived.allowed);
        *memo = Some(derived);
        return allowed;
    }
    let mut allowed = addresses.clone();
    for snapshot in snapshots
        .iter()
        .filter(|snapshot| snapshot.binding.same_component(binding))
    {
        insert_available_permitted(binding, snapshot.resources(), &mut allowed);
    }
    Rc::new(allowed)
}

#[cfg(feature = "model")]
impl ControlAvailability {
    fn derive(
        binding: &CallbackBinding,
        forest: &Rc<[SharedResource]>,
        permissions: tilo_ui_base::permissions::DirectoryPermissionObservation,
        addresses: std::collections::HashSet<tilo_actions::ActionAddress>,
    ) -> Self {
        let mut derived = Self {
            forest: Rc::clone(forest),
            permissions,
            allowed: Rc::new(addresses.clone()),
            addresses,
            counts: HashMap::new(),
        };
        let allowed = Rc::make_mut(&mut derived.allowed);
        for node in forest.iter() {
            visit_subtree(node, &mut |node| {
                count_addresses(
                    binding,
                    node,
                    true,
                    &mut derived.counts,
                    allowed,
                    &derived.addresses,
                );
            });
        }
        derived
    }

    fn update(&mut self, binding: &CallbackBinding, next: &Rc<[SharedResource]>) {
        let last = std::mem::replace(&mut self.forest, Rc::clone(next));
        let allowed = Rc::make_mut(&mut self.allowed);
        let (counts, addresses) = (&mut self.counts, &self.addresses);
        crate::published::diff(&last, next, &mut |change| match change {
            crate::published::Change::Removed(subtree) => visit_subtree(subtree, &mut |node| {
                count_addresses(binding, node, false, counts, allowed, addresses);
            }),
            crate::published::Change::Added(subtree) => visit_subtree(subtree, &mut |node| {
                count_addresses(binding, node, true, counts, allowed, addresses);
            }),
            crate::published::Change::Head { last, next } if same_offer(last, next) => {}
            crate::published::Change::Head { last, next } => {
                count_addresses(binding, last, false, counts, allowed, addresses);
                count_addresses(binding, next, true, counts, allowed, addresses);
            }
            crate::published::Change::Roots => {}
        });
    }
}

#[cfg(feature = "model")]
fn same_offer(last: &SharedResource, next: &SharedResource) -> bool {
    let (last, next) = (last.head(), next.head());
    last.id == next.id
        && last.kind == next.kind
        && last.actions.len() == next.actions.len()
        && last
            .actions
            .iter()
            .zip(&next.actions)
            .all(|(last, next)| last.id == next.id && last.contract == next.contract)
}

#[cfg(feature = "model")]
fn visit_subtree(node: &SharedResource, visit: &mut dyn FnMut(&SharedResource)) {
    visit(node);
    for child in node.children() {
        visit_subtree(child, visit);
    }
}

#[cfg(feature = "model")]
fn count_addresses(
    binding: &CallbackBinding,
    node: &SharedResource,
    add: bool,
    counts: &mut HashMap<tilo_actions::ActionAddress, usize>,
    allowed: &mut std::collections::HashSet<tilo_actions::ActionAddress>,
    addresses: &std::collections::HashSet<tilo_actions::ActionAddress>,
) {
    let head = node.head();
    let permissions = binding.permissions.borrow();
    let decider = permissions.action_decider(&binding.app_id);
    for action in &head.actions {
        if !(action.contract.availability.is_available()
            && decider.permits(&head.kind, head.id.as_str(), action))
        {
            continue;
        }
        let address = tilo_actions::ActionAddress::new(head.id.as_str(), &action.id);
        if add {
            let count = counts.entry(address.clone()).or_insert(0);
            *count = count.saturating_add(1);
            if *count == 1 {
                allowed.insert(address);
            }
        } else if let Some(count) = counts.get_mut(&address) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&address);
                if !addresses.contains(&address) {
                    allowed.remove(&address);
                }
            }
        }
    }
}

/// A component's control intent, with the effect invocation it continues when the component's effect
/// handler or completion queued it, and who asked for it.
#[cfg(feature = "model")]
pub(crate) type PendingComponentControl = (
    BoundActionTarget,
    tilo_actions::ControlInvocation,
    Option<tilo_actions::InvocationId>,
    Origin,
);

#[cfg(feature = "model")]
pub(crate) struct DrainedControlInvocations {
    pub(crate) pending: Vec<PendingComponentControl>,
    pub(crate) error: Option<BoundDispatchError>,
}

#[cfg(feature = "model")]
pub(crate) fn drain_control_invocations(root: &mut dyn Widget) -> DrainedControlInvocations {
    let batch = capture_control_invocations(root);
    DrainedControlInvocations {
        pending: batch
            .controls
            .into_iter()
            .zip(batch.continuing)
            .zip(batch.origins)
            .map(|(((target, intent), on_behalf_of), origin)| {
                (target, intent, on_behalf_of, origin)
            })
            .collect(),
        error: batch.failures.into_iter().next(),
    }
}

#[cfg(feature = "model")]
#[derive(Default)]
pub(crate) struct BoundControlBatch {
    pub(crate) visited: Vec<CallbackBinding>,
    pub(crate) controls: Vec<(BoundActionTarget, tilo_actions::ControlInvocation)>,
    /// For each of `controls`, in order, the effect invocation it continues.
    pub(crate) continuing: Vec<Option<tilo_actions::InvocationId>>,
    /// For each of `controls`, in order, who asked for it.
    pub(crate) origins: Vec<Origin>,
    pub(crate) failures: Vec<BoundDispatchError>,
}

/// Move every reachable component control while preserving controls captured before a later owner
/// traversal failure.
#[cfg(feature = "model")]
pub(crate) fn capture_control_invocations(root: &mut dyn Widget) -> BoundControlBatch {
    visit_control_queues(root, true)
}

/// Retry ownership traversal without opening any live queue after History entry.
#[cfg(feature = "model")]
pub(crate) fn inspect_control_owners(root: &mut dyn Widget) -> BoundControlBatch {
    visit_control_queues(root, false)
}

/// Snapshot the finite binding queues through immutable owner metadata. These
/// descriptors are sealed before the fallible mutable traversal starts. They
/// are never matched to later emissions by value or reinserted in a live queue.
#[cfg(feature = "model")]
pub(crate) struct ControlQueueSnapshot {
    /// Retain even empty queues so suspended output can be discarded without successfully traversing
    /// their mutable owners again.
    pub(crate) owners: Vec<CallbackBinding>,
    pub(crate) controls: Vec<(BoundActionTarget, tilo_actions::ControlInvocation)>,
}

#[cfg(feature = "model")]
pub(crate) fn snapshot_control_queues(root: &dyn Widget) -> ControlQueueSnapshot {
    fn visit(
        widget: &dyn Widget,
        seen: &mut Vec<CallbackBinding>,
        output: &mut Vec<(BoundActionTarget, tilo_actions::ControlInvocation)>,
    ) {
        if let Some(binding) = callback_binding(widget)
            && !seen.iter().any(|prior| prior.same_component(&binding))
        {
            seen.push(binding.clone());
            for invocation in binding.queued_controls.borrow().iter() {
                output.push((
                    BoundActionTarget {
                        input: Input::Discrete,
                        address: BoundActionAddress {
                            instance_id: binding.instance_id,
                            address: invocation.address.clone(),
                        },
                        binding: binding.clone(),
                    },
                    invocation.clone(),
                ));
            }
        }
        widget.for_each_child(&mut |child| visit(child, seen, output));
    }
    let mut output = Vec::new();
    let mut owners = Vec::new();
    visit(root, &mut owners, &mut output);
    ControlQueueSnapshot {
        owners,
        controls: output,
    }
}

#[cfg(feature = "model")]
fn visit_control_queues(root: &mut dyn Widget, drain: bool) -> BoundControlBatch {
    let mut invocations = Vec::new();
    let mut continuing = Vec::new();
    let mut origins = Vec::new();
    let mut visited = Vec::new();
    let mut visit = |binding: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        visited.push(binding.clone());
        if !drain {
            return None;
        }
        let queued = binding
            .queued_controls
            .borrow_mut()
            .drain(..)
            .collect::<Vec<_>>();
        for intent in queued {
            continuing.push(None);
            origins.push(Origin::Person);
            invocations.push((
                BoundActionTarget {
                    input: Input::Discrete,
                    binding: binding.clone(),
                    address: BoundActionAddress {
                        instance_id: binding.instance_id,
                        address: intent.address.clone(),
                    },
                },
                intent,
            ));
        }
        while let (Some((intent, on_behalf_of)), input, origin) = capture_physical_drain(|| {
            tilo_ui_base::widget::controls::take_attributed_control_invocation(widget)
        }) {
            continuing.push(on_behalf_of);
            origins.push(origin);
            invocations.push((
                BoundActionTarget {
                    input,
                    binding: binding.clone(),
                    address: BoundActionAddress {
                        instance_id: binding.instance_id,
                        address: intent.address.clone(),
                    },
                },
                intent,
            ));
        }
        None
    };
    let failure = visit_components(root, &mut ComponentRequest::new(None, &mut visit)).err();
    BoundControlBatch {
        visited,
        controls: invocations,
        continuing,
        origins,
        failures: failure.into_iter().collect(),
    }
}

#[cfg(feature = "model")]
thread_local! {
    static ANSWERING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(feature = "model")]
fn answering() -> bool {
    ANSWERING.with(std::cell::Cell::get)
}

/// While held, a document action the API applies answers what it changed
/// even when its declaration answers nothing: it continues an operation
/// that waits on it, and that operation replies to its caller from it.
#[cfg(feature = "model")]
pub(crate) struct Answering(bool);

#[cfg(feature = "model")]
impl Answering {
    pub(crate) fn enter(on: bool) -> Self {
        Self(ANSWERING.with(|cell| cell.replace(on)))
    }
}

#[cfg(feature = "model")]
impl Drop for Answering {
    fn drop(&mut self) {
        ANSWERING.with(|cell| cell.set(self.0));
    }
}

/// Deliver feedback only to the component that emitted the local intent: domain resource addresses can
/// be shared by several live instances.
#[cfg(feature = "model")]
pub(crate) fn notify_control_invocation_result(
    root: &mut dyn Widget,
    target: &BoundActionTarget,
    result: &Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError>,
    on_behalf_of: Option<tilo_actions::InvocationId>,
) -> Result<(), BoundDispatchError> {
    let mut visit = |_: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        tilo_ui_base::widget::controls::on_control_invocation_result_for(
            widget,
            &target.address.address,
            result,
            on_behalf_of,
        );
        None
    };
    visit_components(
        root,
        &mut ComponentRequest::new(Some(&target.binding), &mut visit),
    )
}

#[cfg(feature = "model")]
pub(crate) fn poll_resource_operation(
    root: &mut dyn Widget,
    document: &AutoCommit,
    target: &BoundActionTarget,
    invocation: tilo_actions::InvocationId,
    cancel: bool,
) -> Result<Option<tilo_actions::InvocationUpdate>, BoundDispatchError> {
    let _entry = target.binding.enter()?;
    let mut result = None;
    let mut delivery = |binding: &CallbackBinding,
                        widget: &mut dyn crate::widget::AppWidget,
                        _: &tilo_actions::ActionRegistry| {
        result = Some(binding.read_document(document, |doc, path| {
            tilo_ui_base::widget::poll_resource_operation(
                &mut *widget,
                invocation,
                cancel,
                doc,
                path,
            )
        }));
        result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|update| update.as_ref())
            .filter(|update| update.is_terminal())
            .map(|_| (invocation, false))
    };
    visit_components(
        root,
        &mut ComponentRequest::new(Some(&target.binding), &mut delivery),
    )?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

/// One bound component's published resources.
#[cfg(feature = "model")]
#[derive(Clone)]
pub(crate) struct BoundResourceSnapshot {
    pub(crate) binding: CallbackBinding,
    forest: Rc<Forest>,
    /// The component controls' allowed addresses last derived, carried from
    /// one snapshot of the component to the one that replaces it.
    controls: Rc<RefCell<Option<ControlAvailability>>>,
}

#[cfg(feature = "model")]
struct Forest {
    shared: std::cell::OnceCell<Rc<[SharedResource]>>,
    plain: std::cell::OnceCell<Rc<Vec<ResourceSpec>>>,
}

#[cfg(feature = "model")]
impl BoundResourceSnapshot {
    pub(crate) fn new(binding: CallbackBinding, resources: Vec<ResourceSpec>) -> Self {
        Self {
            binding,
            forest: Rc::new(Forest {
                shared: std::cell::OnceCell::new(),
                plain: std::cell::OnceCell::from(Rc::new(resources)),
            }),
            controls: Rc::default(),
        }
    }

    pub(crate) fn shared(
        binding: CallbackBinding,
        forest: Rc<[SharedResource]>,
        controls: Rc<RefCell<Option<ControlAvailability>>>,
    ) -> Self {
        Self {
            binding,
            forest: Rc::new(Forest {
                shared: std::cell::OnceCell::from(forest),
                plain: std::cell::OnceCell::new(),
            }),
            controls,
        }
    }

    /// The component's resources as ordinary trees, materialised once per snapshot from its shared
    /// forest.
    pub(crate) fn resources(&self) -> &Rc<Vec<ResourceSpec>> {
        self.forest.plain.get_or_init(|| {
            Rc::new(crate::published::to_resources(
                self.forest.shared.get().map_or(&[][..], |forest| forest),
            ))
        })
    }

    /// [`Self::resources`] by value.
    pub(crate) fn into_resources(self) -> Vec<ResourceSpec> {
        let resources = Rc::clone(self.resources());
        drop(self);
        Rc::unwrap_or_clone(resources)
    }

    /// The shared forest, when the snapshot was published as one.
    pub(crate) fn shared_forest(&self) -> Option<&Rc<[SharedResource]>> {
        self.forest.shared.get()
    }

    /// Add `more` to the component's resources: a new tree, sharing nothing derived from this one.
    pub(crate) fn extend(&mut self, more: impl IntoIterator<Item = ResourceSpec>) {
        let binding = self.binding.clone();
        let mut resources =
            std::mem::replace(self, Self::new(binding.clone(), Vec::new())).into_resources();
        resources.extend(more);
        *self = Self::new(binding, resources);
    }
}

/// What [`refresh_bound_controls`] derived from one component's forest, and everything that derivation
/// read: the forest itself (kept, so the next one is diffed against it), the component's permissions
/// and the APIs its host offers.
#[cfg(feature = "model")]
pub(crate) struct ControlAvailability {
    forest: Rc<[SharedResource]>,
    permissions: tilo_ui_base::permissions::DirectoryPermissionObservation,
    addresses: std::collections::HashSet<tilo_actions::ActionAddress>,
    /// How many available, permitted actions of `forest` have each address.
    counts: HashMap<tilo_actions::ActionAddress, usize>,
    /// `APIs` and every address in `counts`.
    allowed: Rc<std::collections::HashSet<tilo_actions::ActionAddress>>,
}

/// Reserved transport namespace. Domain resources never contain this rewrite.
#[cfg(feature = "model")]
pub(crate) const PUBLICATION_PREFIX: &str = tilo_actions::BOUND_RESOURCE_PREFIX;
#[cfg(feature = "model")]
const MAX_PUBLICATION_ALIAS: usize = 4096;
#[cfg(feature = "model")]
pub(crate) struct BoundPublication {
    host: uuid::Uuid,
    owners: RefCell<HashMap<uuid::Uuid, CallbackBinding>>,
    /// The alias each live component's resource is published under now, by
    /// the resource's own id and the component publishing it: a caller that
    /// names the app's id — its directory, its palette, what the broker
    /// delivers — resolves it here ([`Self::target`]) while one component
    /// publishes it.
    aliases: RefCell<HashMap<String, Vec<(uuid::Uuid, String)>>>,
    /// Bumped every time a component published a resource here: a host that could not reach a
    /// destination yet watches it and asks again when it moves, instead of asking again frame after
    /// frame.
    published: Cell<u64>,
}
#[cfg(feature = "model")]
impl Default for BoundPublication {
    fn default() -> Self {
        Self {
            host: uuid::Uuid::new_v4(),
            owners: RefCell::new(HashMap::new()),
            aliases: RefCell::new(HashMap::new()),
            published: Cell::new(0),
        }
    }
}
#[cfg(feature = "model")]
impl BoundPublication {
    pub(crate) fn alias(
        &self,
        binding: &CallbackBinding,
        resource: &str,
    ) -> Result<String, BoundActionError> {
        let _entry = binding.enter()?;
        let mut owners = self.owners.borrow_mut();
        owners.retain(|_, binding| !binding.closing.get() && binding.lifetime.is_active());
        let mut aliases = self.aliases.borrow_mut();
        aliases.retain(|_, carriers| {
            carriers.retain(|(owner, _)| owners.contains_key(owner));
            !carriers.is_empty()
        });
        if owners
            .get(&binding.lifetime_id)
            .is_some_and(|old| !old.same_component(binding))
        {
            return Err(tilo_actions::ApiError::Unavailable {
                code: "publication_identity_collision".into(),
                reason: "component publication identity collision".into(),
            }
            .into());
        }
        let prefix = format!("{PUBLICATION_PREFIX}{}:{}:", self.host, binding.lifetime_id);
        if resource
            .len()
            .checked_mul(2)
            .and_then(|length| length.checked_add(prefix.len()))
            .is_none_or(|length| length > MAX_PUBLICATION_ALIAS)
        {
            return Err(tilo_actions::ApiError::InvalidArguments(
                "resource transport alias exceeds its size bound".into(),
            )
            .into());
        }
        let mut alias = prefix;
        alias.push_str(&hex::encode(resource));
        let carriers = aliases.entry(resource.to_owned()).or_default();
        carriers.retain(|(owner, _)| *owner != binding.lifetime_id);
        carriers.push((binding.lifetime_id, alias.clone()));
        owners.insert(binding.lifetime_id, binding.clone());
        self.published.set(self.published.get().wrapping_add(1));
        Ok(alias)
    }
    pub(crate) fn target(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Result<Option<BoundActionTarget>, BoundActionError> {
        self.target_within(address, &mut HeldScope::default())
    }

    /// The live aliases resource `id` (the app's own id) is published under, one per component
    /// publishing it.
    fn live_aliases(&self, id: &str) -> Vec<String> {
        let owners = self.owners.borrow();
        self.aliases
            .borrow()
            .get(id)
            .into_iter()
            .flatten()
            .filter(|(owner, _)| {
                owners
                    .get(owner)
                    .is_some_and(|binding| !binding.closing.get() && binding.lifetime.is_active())
            })
            .map(|(_, alias)| alias.clone())
            .collect()
    }

    /// Whether more than one live component publishes resource `id` (the app's own id): the
    /// id then names none of them, and each is addressed by its alias.
    pub(crate) fn shared(&self, id: &str) -> bool {
        self.live_aliases(id).len() > 1
    }

    /// [`Self::target`] within a walk that resolves many addresses of one surface: the scope
    /// `held` carries from one lookup to the next.
    pub(crate) fn target_within(
        &self,
        address: &tilo_actions::ActionAddress,
        held: &mut HeldScope,
    ) -> Result<Option<BoundActionTarget>, BoundActionError> {
        // A caller may name the app's own id for a resource its window
        // published under a transport alias: the directory offers it that
        // way, and so does what the broker delivers. Resolve it through the
        // identity the publication recorded to the alias carrying it now.
        // An id more than one component publishes names none of them.
        let recorded = if address.resource.starts_with(PUBLICATION_PREFIX) {
            None
        } else {
            let mut aliases = self.live_aliases(&address.resource);
            if aliases.len() > 1 {
                return Err(BoundActionError::AmbiguousTarget {
                    address: address.clone(),
                });
            }
            aliases.pop()
        };
        let resource = recorded.as_deref().unwrap_or(address.resource.as_str());
        if !resource.starts_with(PUBLICATION_PREFIX) {
            return Ok(None);
        }
        let unknown = || BoundActionError::Api(tilo_actions::ApiError::Unknown(address.clone()));
        if resource.len() > MAX_PUBLICATION_ALIAS {
            return Err(unknown());
        }
        let mut pieces = resource
            .strip_prefix(PUBLICATION_PREFIX)
            .ok_or_else(unknown)?
            .split(':');
        let host = pieces
            .next()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or_else(unknown)?;
        let owner = pieces
            .next()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or_else(unknown)?;
        let encoded = pieces.next().ok_or_else(unknown)?;
        if host != self.host || pieces.next().is_some() || encoded.len() % 2 != 0 {
            return Err(unknown());
        }
        let bytes = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let high = char::from(*pair.first()?).to_digit(16)?;
                let low = char::from(*pair.get(1)?).to_digit(16)?;
                u8::try_from(high.checked_mul(16)?.checked_add(low)?).ok()
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(unknown)?;
        let resource = String::from_utf8(bytes).map_err(|_| unknown())?;
        let binding = self
            .owners
            .borrow()
            .get(&owner)
            .cloned()
            .ok_or_else(unknown)?;
        held.enter(Some(&binding))?;
        Ok(Some(BoundActionTarget {
            input: Input::Discrete,
            address: BoundActionAddress {
                instance_id: binding.instance_id,
                address: tilo_actions::ActionAddress::new(resource, address.action.clone()),
            },
            binding,
        }))
    }
    pub(crate) fn project(
        &self,
        binding: &CallbackBinding,
        mut resource: ResourceSpec,
    ) -> Result<ResourceSpec, BoundActionError> {
        // Every resource of the surface is aliased through the component that publishes it, one per
        // row of a long collection: the entry below holds it for the whole recursion.
        let _entry = binding.enter()?;
        resource.semantic_identity = tilo_actions::ResourceSemanticIdentity::new(
            resource.kind.clone(),
            resource.id.as_str(),
        );
        resource.id = tilo_actions::ResourceId::new(self.alias(binding, resource.id.as_str())?);
        resource.children = resource
            .children
            .into_iter()
            .map(|child| self.project(binding, child))
            .collect::<Result<_, _>>()?;
        Ok(resource)
    }
}

#[cfg(feature = "model")]
pub(crate) fn validate_bound_resources(
    root: &mut dyn Widget,
    binding: &CallbackBinding,
    resources: &[ResourceSpec],
) -> Result<bool, BoundDispatchError> {
    Ok(bound_contract_violations(root, binding, resources)?.is_some_and(|found| found.is_empty()))
}

/// Every contract violation of one component's surface, checked against the component registry and
/// operation handlers.
#[cfg(feature = "model")]
pub(crate) fn bound_contract_violations(
    root: &mut dyn Widget,
    binding: &CallbackBinding,
    resources: &[ResourceSpec],
) -> Result<Option<Vec<tilo_actions::ContractViolation>>, BoundDispatchError> {
    let _entry = binding.enter()?;
    let mut violations = None;
    let mut visit = |_: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     registry: &tilo_actions::ActionRegistry| {
        violations = Some(tilo_actions::validate_surface(
            resources,
            registry,
            &tilo_ui_base::widget::resource_operation_handlers(&*widget),
        ));
        None
    };
    visit_components(root, &mut ComponentRequest::new(Some(binding), &mut visit))?;
    Ok(violations)
}

/// Collect each retained component once even when an ordinary container does
/// not forward `search_providers`. Legacy providers keep their root-owned route.
#[cfg(feature = "model")]
pub(crate) fn search_providers(root: &dyn Widget) -> Vec<Box<dyn crate::search::SearchProvider>> {
    collect_search_sources(
        root,
        &|widget| crate::search::capability::search_providers(widget),
        &|provider| provider.callback_binding().is_none(),
    )
}

/// [`search_providers`] as the action directory's observation reads them: the
/// same providers in the same order, described without being made, so a
/// source that indexes to answer builds nothing until someone searches.
#[cfg(feature = "model")]
pub(crate) fn search_provider_descriptors(
    root: &dyn Widget,
) -> Vec<crate::search::capability::SearchProviderDescriptor> {
    collect_search_sources(
        root,
        &|widget| crate::search::capability::search_provider_descriptors(widget),
        &|descriptor| descriptor.owner.is_none(),
    )
}

#[cfg(feature = "model")]
fn collect_search_sources<T>(
    root: &dyn Widget,
    read: &dyn Fn(&dyn Widget) -> Vec<T>,
    unowned: &dyn Fn(&T) -> bool,
) -> Vec<T> {
    fn visit<T>(
        widget: &dyn Widget,
        read: &dyn Fn(&dyn Widget) -> Vec<T>,
        seen: &mut std::collections::HashSet<uuid::Uuid>,
        found: &mut Vec<T>,
    ) {
        let binding = callback_binding(widget);
        let Ok(_entry) = binding.as_ref().map(CallbackBinding::enter).transpose() else {
            return;
        };
        if let Some(binding) = &binding
            && seen.insert(binding.lifetime_id)
        {
            found.extend(read(widget));
        }
        widget.for_each_child(&mut |child| visit(child, read, seen, found));
    }
    let mut found = read(root)
        .into_iter()
        .filter(|source| unowned(source))
        .collect::<Vec<_>>();
    visit(
        root,
        read,
        &mut std::collections::HashSet::new(),
        &mut found,
    );
    found
}

#[cfg(feature = "model")]
fn provider_resource(
    widget: &dyn Widget,
    binding: &CallbackBinding,
    address: &tilo_actions::ActionAddress,
) -> Option<ResourceSpec> {
    crate::search::capability::search_providers(widget)
        .into_iter()
        .filter(|provider| {
            provider
                .callback_binding()
                .as_ref()
                .is_none_or(|owner| owner.same_component(binding))
        })
        .find_map(|provider| provider.resolve_action_resource(address))
}

#[cfg(feature = "model")]
pub(crate) fn resolve_component_resource(
    root: &mut dyn Widget,
    doc: &AutoCommit,
    binding: &CallbackBinding,
    resource: &str,
    action: &str,
) -> Result<Option<ResourceSpec>, BoundDispatchError> {
    let _entry = binding.enter()?;
    let mut result = None;
    let mut visit = |current: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        result = Some(current.read_document(doc, |doc, _| {
            resolve_published(widget, doc, Walk::Component, resource)
                .or_else(|| {
                    provider_resource(
                        widget,
                        current,
                        &tilo_actions::ActionAddress::new(resource, action),
                    )
                })
                .and_then(|resource| filter_resource(current, resource))
        }));
        None
    };
    visit_components(root, &mut ComponentRequest::new(Some(binding), &mut visit))?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

/// Whether the component `binding` hosts is up and shown to its member but does not publish
/// resource `id` yet: it publishes resources this member may see, and `id` is none of them. What a
/// component publishes nothing of, or nothing this member may see, is not on its way.
#[cfg(feature = "model")]
pub(crate) fn component_awaits_resource(
    root: &mut dyn Widget,
    doc: &AutoCommit,
    binding: &CallbackBinding,
    id: &str,
    action: &str,
) -> bool {
    let Ok(_entry) = binding.enter() else {
        return false;
    };
    let mut awaits = false;
    let mut visit = |current: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        awaits = current
            .read_document(doc, |doc, _| {
                // A resource it publishes, whatever this member may do on it, is no resource
                // still on its way: what is missing there is refused at once.
                if resolve_published(widget, doc, Walk::Component, id)
                    .or_else(|| {
                        provider_resource(
                            widget,
                            current,
                            &tilo_actions::ActionAddress::new(id, action),
                        )
                    })
                    .is_some()
                {
                    return false;
                }
                let presence = crate::invocation_scope::current_presence();
                let mut published = Vec::new();
                collect_published(
                    widget,
                    doc,
                    Walk::Component,
                    presence.as_ref(),
                    &mut published,
                );
                let permitted: Vec<_> = published
                    .into_iter()
                    .filter_map(|resource| filter_resource(current, resource))
                    .collect();
                !permitted.is_empty()
                    && !permitted
                        .iter()
                        .any(|resource| holds_resource(resource, id))
            })
            .unwrap_or(false);
        None
    };
    visit_components(root, &mut ComponentRequest::new(Some(binding), &mut visit)).is_ok() && awaits
}

/// Whether `resource` or one of its children is resource `id`.
#[cfg(feature = "model")]
fn holds_resource(resource: &ResourceSpec, id: &str) -> bool {
    resource.id.as_str() == id
        || resource
            .children
            .iter()
            .any(|child| holds_resource(child, id))
}

/// [`unbound_resource_pending`] in the component `binding` hosts.
#[cfg(feature = "model")]
pub(crate) fn component_resource_pending(
    root: &mut dyn Widget,
    binding: &CallbackBinding,
    resource: &str,
) -> bool {
    let Ok(_entry) = binding.enter() else {
        return false;
    };
    let mut pending = false;
    let mut visit = |_: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        pending = unbound_resource_pending(widget, resource);
        None
    };
    visit_components(root, &mut ComponentRequest::new(Some(binding), &mut visit)).is_ok() && pending
}

#[cfg(feature = "model")]
pub(crate) fn publication_visibility(
    root: &dyn Widget,
    doc: &AutoCommit,
    owner: Option<&CallbackBinding>,
    id: &str,
) -> Option<tilo_actions::ActionAddress> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        owner: Option<&CallbackBinding>,
        inherited: Option<CallbackBinding>,
        id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        let boundary = callback_binding(widget);
        let binding = boundary.clone().or(inherited);
        let _entry = binding
            .as_ref()
            .map(CallbackBinding::enter)
            .transpose()
            .ok()?;
        if boundary.is_none()
            && match (owner, binding.as_ref()) {
                (Some(owner), Some(binding)) => owner.same_component(binding),
                (None, None) => true,
                _ => false,
            }
        {
            let found = if let Some(binding) = &binding {
                binding
                    .read_document(doc, |doc, _| {
                        crate::widget::resources::action_directory_visibility(widget, doc, id)
                    })
                    .ok()
                    .flatten()
            } else {
                crate::widget::resources::action_directory_visibility(widget, doc, id)
            };
            if found.is_some() {
                return found;
            }
        }
        let mut found = None;
        widget.for_each_child(&mut |child| {
            if found.is_none() {
                found = visit(child, doc, owner, binding.clone(), id);
            }
        });
        found
    }
    visit(root, doc, owner, None, id)
}

/// [`publication_visibility`] for each of `wanted` (an owner and a resource id), in one walk of the
/// widget tree: what each resource's first publisher in tree order says discloses it.
#[cfg(feature = "model")]
pub(crate) fn publication_visibilities(
    root: &dyn Widget,
    doc: &AutoCommit,
    wanted: &[(Option<CallbackBinding>, String)],
) -> Vec<Option<tilo_actions::ActionAddress>> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<CallbackBinding>,
        wanted: &[(Option<CallbackBinding>, String)],
        found: &mut [Option<tilo_actions::ActionAddress>],
    ) {
        let boundary = callback_binding(widget);
        let binding = boundary.clone().or(inherited);
        let Ok(_entry) = binding.as_ref().map(CallbackBinding::enter).transpose() else {
            return;
        };
        if boundary.is_none()
            && tilo_ui_base::widget::capability::capability::<
                dyn crate::widget::resources::PublishesResources,
            >(widget)
            .is_some()
        {
            let mut answer = |doc: &AutoCommit| {
                for ((owner, id), slot) in wanted.iter().zip(found.iter_mut()) {
                    if slot.is_some() {
                        continue;
                    }
                    let mine = match (owner, binding.as_ref()) {
                        (Some(owner), Some(binding)) => owner.same_component(binding),
                        (None, None) => true,
                        _ => false,
                    };
                    if mine {
                        *slot =
                            crate::widget::resources::action_directory_visibility(widget, doc, id);
                    }
                }
            };
            if let Some(binding) = &binding {
                let _ = binding.read_document(doc, |doc, _| answer(doc));
            } else {
                answer(doc);
            }
        }
        if found.iter().all(Option::is_some) {
            return;
        }
        widget.for_each_child(&mut |child| {
            visit(child, doc, binding.clone(), wanted, found);
        });
    }
    let mut found = vec![None; wanted.len()];
    visit(root, doc, None, wanted, &mut found);
    found
}

/// Opaque legacy navigation remains scoped to the provider that produced it.
#[cfg(feature = "model")]
pub(crate) fn activate_bound_search(
    root: &mut dyn Widget,
    doc: &mut AutoCommit,
    binding: &CallbackBinding,
    id: &str,
) -> Result<(), BoundDispatchError> {
    let _entry = binding.enter()?;
    if !binding
        .permissions
        .borrow()
        .check(&tilo_ui_base::permissions::PermissionScope::app(
            &binding.app_id,
            tilo_ui_base::permissions::ActionKind::Read,
        ))
    {
        return Ok(());
    }
    let mut result = None;
    let mut visit = |current: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        current.note_input();
        result = Some(
            current
                .with_document(doc, |doc, path| {
                    current.finish_action_boundary(doc)?;
                    crate::search::capability::activate_search_result(
                        widget,
                        id,
                        &mut tilo_ui_base::widget::Edits::lend(doc),
                        path,
                    );
                    current.drain_owned_tree(widget, doc)?;
                    current.finish_action_boundary(doc)
                })
                .and_then(std::convert::identity),
        );
        None
    };
    visit_components(root, &mut ComponentRequest::new(Some(binding), &mut visit))?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

#[cfg(feature = "model")]
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BoundDirectoryObservation {
    owner: uuid::Uuid,
    pub(crate) generation: Option<u64>,
    permissions: tilo_ui_base::permissions::DirectoryPermissionObservation,
    revisions: Vec<Option<String>>,
    domains: Vec<tilo_ui_base::widget::ActionDirectoryInvocationDomain>,
}
#[cfg(feature = "model")]
impl BoundDirectoryObservation {
    pub(crate) fn supports_model_refresh(&self) -> bool {
        self.generation.is_some_and(|value| value != u64::MAX)
            && !self.domains.is_empty()
            && self.domains.iter().all(|domain| domain.is_supported())
    }
    /// Only used after fresh full declaration/effective-domain equality in one
    /// hosted model pass. Generation is bookkeeping, never document authority.
    pub(crate) fn same_model_owner_after(&self, before: &Self) -> bool {
        self.supports_model_refresh()
            && before.supports_model_refresh()
            && self.owner == before.owner
            && self.permissions == before.permissions
            && self.revisions == before.revisions
            && self.domains == before.domains
            && matches!((before.generation, self.generation), (Some(old), Some(new)) if new >= old)
    }
}
#[cfg(feature = "model")]
pub(crate) fn directory_observation(
    root: &dyn Widget,
    doc: &AutoCommit,
) -> Result<Vec<BoundDirectoryObservation>, String> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<CallbackBinding>,
        observations: &mut Vec<BoundDirectoryObservation>,
    ) -> Result<(), String> {
        let boundary = callback_binding(widget);
        let binding = boundary.clone().or(inherited);
        if binding
            .as_ref()
            .is_some_and(|binding| binding.closing.get() || !binding.lifetime.is_active())
        {
            return Ok(());
        }
        let _entry = binding
            .as_ref()
            .map(CallbackBinding::enter)
            .transpose()
            .map_err(|error| format!("{error:?}"))?;
        if let Some(binding) = &binding {
            let permissions = binding.permissions.borrow().directory_observation()?;
            let index = observations
                .iter()
                .position(|observation| observation.owner == binding.lifetime_id)
                .unwrap_or_else(|| {
                    observations.push(BoundDirectoryObservation {
                        owner: binding.lifetime_id,
                        generation: None,
                        permissions: permissions.clone(),
                        revisions: Vec::new(),
                        domains: Vec::new(),
                    });
                    observations.len().saturating_sub(1)
                });
            let observation = observations
                .get_mut(index)
                .ok_or_else(|| "directory owner disappeared".to_string())?;
            if observation.permissions != permissions {
                return Err("directory owner policy changed during traversal".into());
            }
            observation.generation = binding.generation().map_err(|error| format!("{error:?}"))?;
            if boundary.is_some() {
                observation.domains.push(
                    binding
                        .read_document(doc, |doc, _| {
                            crate::widget::resources::action_directory_invocation_domain(
                                widget, doc,
                            )
                        })
                        .map_err(|error| format!("{error:?}"))?,
                );
            }
            if boundary.is_none() {
                observation.revisions.push(
                    binding
                        .read_document(doc, |doc, _| {
                            crate::widget::resources::action_directory_revision(widget, doc)
                        })
                        .map_err(|error| format!("{error:?}"))?,
                );
            }
        }
        let mut result = Ok(());
        widget.for_each_child(&mut |child| {
            if result.is_ok() {
                result = visit(child, doc, binding.clone(), observations);
            }
        });
        result
    }
    let mut observations = Vec::new();
    visit(root, doc, None, &mut observations)?;
    Ok(observations)
}

#[cfg(feature = "model")]
#[derive(Default)]
pub(crate) struct BoundResourceCache(RefCell<Vec<CachedBoundResources>>);

/// One bound owner's admission generation and record frontier, read on each side of an input
/// dispatch (the native App).
#[cfg(feature = "model")]
pub(crate) struct BoundOwnerMark {
    binding: CallbackBinding,
    /// Only the native input path has snapshots across generations.
    generation: Option<u64>,
    document: Option<(Vec<automerge::ChangeHash>, usize)>,
}

/// What an owner's published controls and synced root read besides its record and its widgets: what its
/// permissions mean now (a grant expires with time) and the durability it shows (its record's and its
/// chain parts', [`CallbackBinding::shown_save_status`]), which its root shows once synced.
#[cfg(feature = "model")]
#[derive(PartialEq)]
pub(crate) struct OwnerInputs {
    pub(crate) permissions:
        Result<tilo_ui_base::permissions::DirectoryPermissionObservation, String>,
    pub(crate) save: Result<crate::resource::ResourceSaveStatus, String>,
}

#[cfg(feature = "model")]
impl BoundOwnerMark {
    pub(crate) fn is_committed(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(|(_, pending)| *pending == 0)
    }
}

/// Every live bound owner under `root`, once each, in tree order.
#[cfg(feature = "model")]
pub(crate) fn bound_owner_marks(root: &dyn Widget, outer: &AutoCommit) -> Vec<BoundOwnerMark> {
    owner_marks(root, outer)
}

#[cfg(feature = "model")]
fn owner_marks(root: &dyn Widget, outer: &AutoCommit) -> Vec<BoundOwnerMark> {
    fn visit(widget: &dyn Widget, outer: &AutoCommit, marks: &mut Vec<BoundOwnerMark>) {
        if let Some(binding) = callback_binding(widget)
            && !binding.closing.get()
            && binding.lifetime.is_active()
            && !marks
                .iter()
                .any(|mark| mark.binding.same_component(&binding))
        {
            let entry = binding.enter();
            let generation = binding.generation().ok().flatten();
            let document = binding
                .read_document(outer, |doc, _| tilo_ui_base::doc::settle_mark(doc))
                .ok();
            drop(entry);
            marks.push(BoundOwnerMark {
                binding,
                generation,
                document,
            });
        }
        widget.for_each_child(&mut |child| visit(child, outer, marks));
    }
    let mut marks = Vec::new();
    visit(root, outer, &mut marks);
    marks
}

#[cfg(feature = "model")]
impl BoundResourceCache {
    /// An input dispatch lends each owner its record mutably, which advances the owner's admission
    /// generation whether or not anything was written. When the records stand exactly where they
    /// did `before`, carry each snapshot current then to the generation now. Returns `false`,
    /// carrying nothing, when any owner's record moved or the owners changed: the caller must
    /// republish.
    pub(crate) fn carry_across_input(
        &self,
        before: &[BoundOwnerMark],
        after: &[BoundOwnerMark],
    ) -> bool {
        let unmoved = before.len() == after.len()
            && before.iter().zip(after).all(|(before, after)| {
                before.binding.same_component(&after.binding)
                    && before.document.is_some()
                    && before.document == after.document
            });
        if !unmoved {
            return false;
        }
        let mut entries = self.0.borrow_mut();
        for (before, after) in before.iter().zip(after) {
            let (Some(was), Some(now)) = (before.generation, after.generation) else {
                continue;
            };
            for entry in entries.iter_mut().filter(|entry| {
                entry.generation == was && entry.snapshot.binding.same_component(&before.binding)
            }) {
                entry.generation = now;
            }
        }
        true
    }

    /// Whether what a component's published resources were read from outside it moved since they
    /// were gathered (a shared record a peer's change merged into): the next gathering reads them
    /// again, and what is shown of them is stale until it does.
    pub(crate) fn outside_reads_moved(&self) -> bool {
        self.0.borrow().iter().any(
            |entry| matches!(&entry.local, LocalInputs::Owner(_, reads) if reads.stand().is_err()),
        )
    }
}
#[cfg(feature = "model")]
struct CachedBoundResources {
    snapshot: BoundResourceSnapshot,
    generation: u64,
    /// What local input the snapshot was published after: the owner's input revision and what its
    /// declarations read outside it, which move only with what reached that owner
    /// ([`tilo_ui_base::component_inputs`]); the host's frame count for an owner without one.
    local: LocalInputs,
    locale: u64,
    permissions: tilo_ui_base::permissions::DirectoryPermissionObservation,
    revisions: Vec<Option<String>>,
    filter_token: u64,
}

#[cfg(feature = "model")]
enum LocalInputs {
    Owner(u64, tilo_ui_base::component_inputs::Reads),
    Frames(u64),
}

#[cfg(feature = "model")]
impl LocalInputs {
    /// Whether a snapshot published after `self` still has for an owner at `revision` in host frame
    /// `frames`.
    fn hold(&self, revision: Option<u64>, frames: u64) -> bool {
        match (self, revision) {
            (Self::Owner(was, reads), Some(now)) => *was == now && reads.stand().is_ok(),
            (Self::Frames(was), None) => *was == frames,
            _ => false,
        }
    }
}

#[cfg(feature = "model")]
#[allow(
    clippy::too_many_lines,
    reason = "private nested traversal and per-owner cache admission stay together"
)]
pub(crate) fn discover_cached_bound_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    cache: &BoundResourceCache,
    local_generation: u64,
) -> Result<Vec<BoundResourceSnapshot>, String> {
    fn owned(
        widget: &dyn Widget,
        doc: &AutoCommit,
        owner: &CallbackBinding,
        resources: &mut Vec<SharedResource>,
        revisions: &mut Vec<Option<String>>,
        declarations: bool,
        taken: &mut crate::widget::resources::TakenResources,
        root: bool,
    ) {
        let boundary = callback_binding(widget);
        if boundary
            .as_ref()
            .is_some_and(|binding| !binding.same_component(owner))
        {
            return;
        }
        if boundary.is_none() {
            revisions.push(crate::widget::resources::action_directory_revision(
                widget, doc,
            ));
            if declarations {
                let found = crate::widget::resources::shared_resources(widget, doc);
                let fresh: Vec<_> = found
                    .into_iter()
                    .map(|resource| {
                        // Only the component's root speaks for its resource: what a
                        // widget below it publishes is views and effects, never a
                        // change of the document.
                        if root {
                            return resource;
                        }
                        SharedResource::from_resource(crate::widget::resources::outside_document(
                            resource.to_resource(),
                        ))
                    })
                    .filter(|resource| !taken.holds(resource.id()))
                    .collect();
                for resource in &fresh {
                    taken.take(resource.id());
                }
                resources.extend(fresh);
            }
        }
        // A widget hosting a component is where that component's tree starts: what it
        // publishes speaks for the resource, and only below it do views and effects stand.
        let below = boundary.is_some();
        widget.for_each_child(&mut |child| {
            owned(
                child,
                doc,
                owner,
                resources,
                revisions,
                declarations,
                taken,
                below,
            );
        });
    }
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        cache: &BoundResourceCache,
        local_generation: u64,
        snapshots: &mut Vec<BoundResourceSnapshot>,
    ) -> Result<(), String> {
        let binding = callback_binding(widget);
        if binding
            .as_ref()
            .is_some_and(|binding| binding.closing.get() || !binding.lifetime.is_active())
        {
            return Ok(());
        }
        let _entry = binding
            .as_ref()
            .map(CallbackBinding::enter)
            .transpose()
            .map_err(|error| format!("{error:?}"))?;
        if let Some(binding) = &binding
            && !snapshots
                .iter()
                .any(|snapshot| snapshot.binding.same_component(binding))
        {
            let generation = binding.generation().map_err(|error| format!("{error:?}"))?;
            let permissions = match binding.permissions.borrow().directory_observation() {
                Ok(permissions) => permissions,
                Err(error) => {
                    cache
                        .0
                        .borrow_mut()
                        .retain(|entry| !entry.snapshot.binding.same_component(binding));
                    return Err(error);
                }
            };
            let revisions = binding
                .read_document(doc, |doc, _| {
                    let mut revisions = Vec::new();
                    owned(
                        widget,
                        doc,
                        binding,
                        &mut Vec::new(),
                        &mut revisions,
                        false,
                        &mut crate::widget::resources::TakenResources::default(),
                        true,
                    );
                    revisions
                })
                .map_err(|error| format!("{error:?}"))?;
            let locale = tilo_ui_base::i18n::I18nManager::revision();
            let cached = cache
                .0
                .borrow()
                .iter()
                .find(|entry| {
                    entry.snapshot.binding.same_component(binding)
                        && generation == Some(entry.generation)
                        && entry
                            .local
                            .hold(binding.lifetime.revision(), local_generation)
                        && entry.locale == locale
                        && entry.permissions == permissions
                        && entry.revisions == revisions
                })
                .map(|entry| entry.snapshot.clone());
            let snapshot = if let Some(cached) = cached {
                cached
            } else {
                let _phase = tilo_actions::profiling::Phase::start("ui.bound_discovery_miss");
                // The snapshot this one replaces: its controls' derivation
                // and its filter token carry over, so what the two forests
                // share is neither filtered nor surveyed again.
                let (filter_token, controls, last) = cache
                    .0
                    .borrow()
                    .iter()
                    .find(|entry| entry.snapshot.binding.same_component(binding))
                    .map_or_else(
                        || (crate::published::fresh_token(), Rc::default(), None),
                        |entry| {
                            let (token, last) = if entry.permissions == permissions {
                                (entry.filter_token, entry.snapshot.shared_forest().cloned())
                            } else {
                                (crate::published::fresh_token(), None)
                            };
                            (token, Rc::clone(&entry.snapshot.controls), last)
                        },
                    );
                let (resources, reads) = crate::resource::reads::reading(&binding.lifetime, || {
                    binding.read_document(doc, |doc, _| {
                        let mut resources = Vec::new();
                        owned(
                            widget,
                            doc,
                            binding,
                            &mut resources,
                            &mut Vec::new(),
                            true,
                            &mut crate::widget::resources::TakenResources::default(),
                            true,
                        );
                        resources
                            .iter()
                            .filter_map(|resource| {
                                if let Some(last) = last.as_ref().and_then(|last| {
                                    last.iter().find(|last| last.id() == resource.id())
                                }) {
                                    inherit_filtered(binding, filter_token, last, resource);
                                }
                                filter_shared(binding, filter_token, resource)
                            })
                            .collect::<Rc<[SharedResource]>>()
                    })
                });
                let resources = resources.map_err(|error| format!("{error:?}"))?;
                let revision = binding.lifetime.revision();
                if binding.permissions.borrow().directory_observation()? != permissions {
                    return Err("owner policy changed while filtering resources".into());
                }
                let snapshot = BoundResourceSnapshot::shared(binding.clone(), resources, controls);
                let mut entries = cache.0.borrow_mut();
                entries.retain(|entry| {
                    !entry.snapshot.binding.closing.get()
                        && !entry.snapshot.binding.same_component(binding)
                });
                if let Some(generation) = generation {
                    entries.push(CachedBoundResources {
                        snapshot: snapshot.clone(),
                        generation,
                        local: revision.map_or(LocalInputs::Frames(local_generation), |revision| {
                            LocalInputs::Owner(revision, reads)
                        }),
                        locale,
                        permissions,
                        revisions,
                        filter_token,
                    });
                }
                snapshot
            };
            snapshots.push(snapshot);
        }
        let mut result = Ok(());
        widget.for_each_child(&mut |child| {
            if result.is_ok() {
                result = visit(child, doc, cache, local_generation, snapshots);
            }
        });
        result
    }
    let mut snapshots = Vec::new();
    visit(widget, doc, cache, local_generation, &mut snapshots)?;
    Ok(snapshots)
}

/// Action-scoped authority the bound dispatch shares: the action `action` on
/// `resource` runs when allowed, and is
/// [`tilo_actions::ApiError::ConfirmationRequired`] where the workspace's grants
/// leave it to the person: the window then sends it through the device's
/// dispatch, which asks them. A plain quick control (a button's effect) gets
/// no further than an action declaring its confirmation.
#[cfg(feature = "model")]
pub(crate) fn admit_local_action(
    permissions: &PermissionContext,
    (app_id, resource, action): (&str, &ResourceSpec, &tilo_actions::ActionSpec),
    typed: Option<&serde_json::Map<String, serde_json::Value>>,
    argument: Option<&str>,
) -> Result<(), tilo_actions::ApiError> {
    let answer = permissions.invocation_admission(app_id, resource, action, typed, argument);
    tilo_ui_base::permissions::admit_in_window(answer, action, permissions.asker()).map_err(
        |refused| match permissions.shown_refusal(resource, action) {
            Some(held) if answer.answer == tilo_ui_base::permissions::Answer::Deny => held,
            _ => refused,
        },
    )
}

/// Whether the person's own call of `spec` with these arguments waits for their yes:
/// [`tilo_actions::ApiError::ConfirmationRequired`] when it does and `approval`, their approve of
/// the asked record the call was kept as, is not there. The window then sends the call through the
/// device's dispatch, where it is kept as that record and comes back approved once they allow it;
/// what the phrase they typed must say is checked where it is admitted.
///
/// It waits where the action's contract asks ([`tilo_actions::ConfirmationPolicy`]), which Shift
/// skips for what Undo takes back ([`crate::widget::shift_skips_confirmation`]), and, when their
/// press made it (`pressed`), where a press of it confirms
/// ([`tilo_actions::ActionSpec::confirms_for`]): a delete Undo takes back unless Shift is held,
/// what Undo cannot take back and what leaves the system always.
#[cfg(feature = "model")]
pub(crate) fn press_asks(
    spec: &tilo_actions::ActionSpec,
    typed: Option<&serde_json::Map<String, serde_json::Value>>,
    argument: Option<&str>,
    approval: Option<&tilo_actions::remote::asked::Approval>,
    pressed: bool,
) -> Result<(), tilo_actions::ApiError> {
    let confirmation = match typed {
        Some(arguments) => spec.confirmation_for_json(arguments),
        None => spec.confirmation_for_argument(argument),
    };
    let contract_asks = !matches!(confirmation, tilo_actions::ConfirmationPolicy::None)
        && !crate::widget::shift_skips_confirmation(spec.contract.can_be_undone());
    let press_asks = pressed
        && spec
            .confirms_for(typed, argument)
            .on_press(crate::widget::shift_held());
    if approval.is_none() && (contract_asks || press_asks) {
        Err(tilo_actions::ApiError::ConfirmationRequired)
    } else {
        Ok(())
    }
}

/// Why an invocation the person made did not run, in words for them: what
/// the window says instead of dropping the click unseen.
pub(crate) fn refusal_reason(error: &tilo_actions::ApiError) -> String {
    use tilo_actions::ApiError;
    let reason = match error {
        ApiError::Unknown(_) | ApiError::UnregisteredType(_) => {
            "It isn't available to you here.".to_owned()
        }
        ApiError::ConfirmationRequired => "It needs your go-ahead first.".to_owned(),
        ApiError::Unavailable { code, reason } => {
            internal_refusal(code).map_or_else(|| reason.clone(), str::to_owned)
        }
        ApiError::Refused { reason, .. } | ApiError::Failed(reason) => reason.clone(),
        ApiError::MissingArgument(_)
        | ApiError::UnexpectedArgument(_)
        | ApiError::InvalidArguments(_)
        | ApiError::ArgumentOutOfRange { .. } => "It couldn't run. Try again.".to_owned(),
    };
    let mut reason = reason.trim().to_owned();
    if let Some(first) = reason.chars().next()
        && first.is_lowercase()
    {
        reason = first.to_uppercase().chain(reason.chars().skip(1)).collect();
    }
    if !reason.ends_with(['.', '!', '?']) {
        reason.push('.');
    }
    reason
}

/// What a person reads for a refusal the toolkit routing made, by
/// its code: those refusals say, for an agent or a log, which step of the
/// dispatch stopped, never what the person can do. `None` for a refusal
/// worded for the person (an app, a host's, a permission's).
fn internal_refusal(code: &str) -> Option<&'static str> {
    const UNREACHABLE: &str = "It can't be reached right now. Try again.";
    const CHANGED: &str = "It changed before it could run. Try again.";
    const COULD_NOT_RUN: &str = "It couldn't run. Try again.";
    Some(match code {
        "authoritative_host_unavailable"
        | "broker_unavailable"
        | "bound_broker_route_required"
        | "bound_broker_route_unavailable"
        | "provider_unavailable"
        | "remote_actions_unavailable"
        | "application_launch_broker_disconnected" => UNREACHABLE,
        "stale_pending_palette_action"
        | "stale_pending_control"
        | "stale_deferred_intent"
        | "stale_revision"
        | "published_invocation_revision_changed"
        | "published_invocation_contract_changed"
        | "action_document_replaced"
        | "closed_lifetime"
        | "resource_unavailable" => CHANGED,
        "cross_resource_reentry"
        | "ambiguous_action_target"
        | "ambiguous_action_identity"
        | "undeclared_document_mutation"
        | "unsupported_continuation_identity"
        | "unsupported_deferred_intent"
        | "published_invocation_unauthorized"
        | "publication_identity_collision"
        | "private_view"
        | "native_history_unobserved"
        | "native_history_unconfirmed"
        | "history_action_unavailable"
        | "history_action_ambiguous"
        | "broker_request_identity_exhausted"
        | "application_launch_queue_full"
        | "application_launch_invalid_receipt"
        | "action_invocation_identity_conflict"
        | "unknown_action"
        | "unknown_invocation"
        | "invocation_conflict"
        | "invalid_arguments" => COULD_NOT_RUN,
        _ => return None,
    })
}

#[cfg(feature = "model")]
pub(crate) use ::collaboration::permissions::denied;

/// The one resource `binding` publishes for the document's `request`ed history action, and that
/// action's name: a keyboard undo or redo reaches a document that declares its history through the
/// action it publishes, and exactly one of them.
#[cfg(feature = "model")]
pub(crate) fn history_action_resource<'a>(
    snapshots: &'a [BoundResourceSnapshot],
    binding: &CallbackBinding,
    request: tilo_ui_base::widget::undo_access::UndoRequest,
) -> Result<(&'a ResourceSpec, &'static str), tilo_actions::ApiError> {
    let action = match request {
        tilo_ui_base::widget::undo_access::UndoRequest::Undo => "undo",
        tilo_ui_base::widget::undo_access::UndoRequest::Redo => "redo",
    };
    let mut candidates = snapshots
        .iter()
        .filter(|snapshot| snapshot.binding.same_component(binding))
        .flat_map(|snapshot| snapshot.resources().iter())
        .filter(|resource| {
            resource
                .actions
                .iter()
                .any(|spec| spec.id == action && spec.kind == tilo_actions::OperationKind::Document)
        });
    let resource = candidates
        .next()
        .ok_or_else(|| tilo_actions::ApiError::Unavailable {
            code: "history_action_unavailable".into(),
            reason: "the current document does not publish its history action".into(),
        })?;
    if candidates.next().is_some() {
        return Err(tilo_actions::ApiError::Unavailable {
            code: "history_action_ambiguous".into(),
            reason: "the current document publishes more than one history target".into(),
        });
    }
    Ok((resource, action))
}

/// Read-only traversal releases each checked document borrow before descending
/// to another component. It never establishes an undo baseline.
#[cfg(feature = "model")]
pub(crate) fn discover_bound_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    lazy_address: Option<&tilo_actions::ActionAddress>,
) -> Result<Vec<BoundResourceSnapshot>, BoundDispatchError> {
    discover_bound_resources_with_filter(widget, doc, lazy_address, true)
}

#[cfg(feature = "model")]
fn discover_bound_resources_with_filter(
    widget: &dyn Widget,
    doc: &AutoCommit,
    lazy_address: Option<&tilo_actions::ActionAddress>,
    filtered: bool,
) -> Result<Vec<BoundResourceSnapshot>, BoundDispatchError> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<&CallbackBinding>,
        root: bool,
        address: Option<&tilo_actions::ActionAddress>,
        snapshots: &mut Vec<BoundResourceSnapshot>,
        taken: &mut std::collections::HashMap<usize, crate::widget::resources::TakenResources>,
        filtered: bool,
    ) -> Result<(), BoundDispatchError> {
        let boundary = callback_binding(widget);
        let binding = boundary.as_ref().or(inherited);
        if let Some(binding) = binding {
            if binding.closing.get() || !binding.lifetime.is_active() {
                return Ok(());
            }
            let snapshot = snapshots
                .iter()
                .position(|snapshot| snapshot.binding.same_component(binding));
            let publishes = || {
                tilo_ui_base::widget::capability::capability::<
                    dyn crate::widget::resources::PublishesResources,
                >(widget)
                .is_some()
                    || (address.is_some()
                        && tilo_ui_base::widget::capability::capability::<
                            dyn crate::search::capability::SearchSource,
                        >(widget)
                        .is_some())
            };
            if boundary.is_none() && (snapshot.is_none() || publishes()) {
                let resources = match binding.read_document(doc, |doc, _| {
                    let presence = crate::invocation_scope::current_presence();
                    // Asked for one address, the component's own tree answers with the node it
                    // names, found in its shared tree, not with every resource it publishes.
                    if let Some(address) = address
                        && root
                        && presence.is_none()
                        && let Some(forest) = tilo_ui_base::widget::capability::capability::<
                            dyn crate::widget::resources::PublishesResources,
                        >(widget)
                        .and_then(|publisher| publisher.shared_resources_for(None, doc))
                    {
                        if let Some(found) =
                            shared_target(&forest, filtered.then_some(binding), address)
                        {
                            return vec![found];
                        }
                        // One the shared tree leaves out (an item past a
                        // published page) is resolved alone by id, as the
                        // tree would carry it: never by publishing every
                        // resource to look for it.
                        if let Some(resource) = crate::widget::resources::resource_for(
                            widget,
                            None,
                            doc,
                            &address.resource,
                        ) && tilo_actions::api::resolve(std::slice::from_ref(&resource), address)
                            .is_some()
                        {
                            return if filtered {
                                filter_resource(binding, resource).into_iter().collect()
                            } else {
                                vec![resource]
                            };
                        }
                    }
                    let mut resources =
                        crate::widget::resources::resources_for(widget, presence.as_ref(), doc);
                    if !root {
                        resources = resources
                            .into_iter()
                            .map(crate::widget::resources::outside_document)
                            .collect();
                    }
                    if let Some(address) = address
                        && tilo_actions::api::resolve(&resources, address).is_none()
                        && let Some(resource) = crate::widget::resources::resource_for(
                            widget,
                            presence.as_ref(),
                            doc,
                            &address.resource,
                        )
                        .or_else(|| provider_resource(widget, binding, address))
                        .map(|resource| below_root(root, resource))
                    {
                        resources.push(resource);
                    }
                    if filtered {
                        resources
                            .into_iter()
                            .filter_map(|resource| filter_resource(binding, resource))
                            .collect()
                    } else {
                        resources
                    }
                }) {
                    // A component whose record is still on its way has nothing to act on yet: it
                    // publishes its surface once the record arrives and wakes it.
                    Err(BoundDispatchError::Record(ResourceOpenError::Arriving)) => return Ok(()),
                    read => read?,
                };
                let index = snapshot.unwrap_or(snapshots.len());
                let read = taken.entry(index).or_default();
                let resources: Vec<_> = resources
                    .into_iter()
                    .filter(|resource| !read.holds(resource.id.as_str()))
                    .collect();
                for resource in &resources {
                    read.take(resource.id.as_str());
                }
                if index == snapshots.len() {
                    snapshots.push(BoundResourceSnapshot::new(binding.clone(), resources));
                } else if let Some(snapshot) = snapshots.get_mut(index)
                    && !resources.is_empty()
                {
                    snapshot.extend(resources);
                }
            }
        }
        let mut result = Ok(());
        let below = boundary.is_some();
        widget.for_each_child(&mut |child| {
            if result.is_ok() {
                result = visit(
                    child, doc, binding, below, address, snapshots, taken, filtered,
                );
            }
        });
        result
    }
    let mut snapshots = Vec::new();
    visit(
        widget,
        doc,
        None,
        true,
        lazy_address,
        &mut snapshots,
        &mut std::collections::HashMap::new(),
        filtered,
    )?;
    Ok(snapshots)
}

/// What a call of `address` that no component offers its caller is told: `permission_denied`,
/// saying why, where a component of `instance` publishes the action and the permissions there keep
/// it from them, as the workspace's own API says it; [`tilo_actions::ApiError::Unknown`] where
/// nothing publishes it.
#[cfg(feature = "model")]
pub(crate) fn unoffered(
    widget: &dyn Widget,
    doc: &AutoCommit,
    address: &tilo_actions::ActionAddress,
    instance: crate::application::InstanceId,
) -> tilo_actions::ApiError {
    let published = discover_bound_resources_with_filter(widget, doc, Some(address), false)
        .ok()
        .and_then(|snapshots| {
            snapshots
                .iter()
                .filter(|snapshot| snapshot.binding.instance_id == instance)
                .find_map(|snapshot| {
                    tilo_actions::api::resolve(snapshot.resources(), address)
                        .map(|spec| spec.label.to_lowercase())
                })
        });
    match published {
        Some(label) => tilo_actions::ApiError::Unavailable {
            code: "permission_denied".into(),
            reason: format!("your permissions in this workspace don't let you {label} here"),
        },
        None => tilo_actions::ApiError::Unknown(address.clone()),
    }
}

#[cfg(feature = "model")]
pub(crate) fn select_bound_target(
    snapshots: &[BoundResourceSnapshot],
    address: &tilo_actions::ActionAddress,
    instance: Option<crate::application::InstanceId>,
) -> Result<Option<BoundActionTarget>, BoundActionError> {
    let mut selected = None;
    for snapshot in snapshots {
        if instance.is_some_and(|instance| instance != snapshot.binding.instance_id) {
            continue;
        }
        snapshot.binding.enter()?;
        let authorized = snapshot
            .resources()
            .iter()
            .cloned()
            .filter_map(|resource| filter_resource(&snapshot.binding, resource))
            .collect::<Vec<_>>();
        if tilo_actions::api::resolve(&authorized, address).is_none() {
            continue;
        }
        if selected.is_some() {
            return Err(BoundActionError::AmbiguousTarget {
                address: address.clone(),
            });
        }
        selected = Some(BoundActionTarget {
            input: Input::Discrete,
            binding: snapshot.binding.clone(),
            address: BoundActionAddress {
                instance_id: snapshot.binding.instance_id,
                address: address.clone(),
            },
        });
    }
    Ok(selected)
}

pub(crate) type ResolvedResourceInvocation<'a> = (
    Option<serde_json::Map<String, serde_json::Value>>,
    &'a ResourceSpec,
    &'a tilo_actions::ActionSpec,
);

pub(crate) fn find_action_resource<'a>(
    resources: &'a [ResourceSpec],
    id: &str,
) -> Option<&'a ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource)
        } else {
            find_action_resource(&resource.children, id)
        }
    })
}

pub(crate) fn resolve_resource_invocation<'a>(
    resources: &'a [ResourceSpec],
    address: &tilo_actions::ActionAddress,
    argument: Option<&str>,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<ResolvedResourceInvocation<'a>, tilo_actions::ApiError> {
    if arguments.is_some() && argument.is_some() {
        return Err(tilo_actions::ApiError::InvalidArguments(
            "supply structured arguments or legacy text, not both".into(),
        ));
    }
    let typed = argument
        .and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
        .map(|json| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                .map_err(|error| tilo_actions::ApiError::InvalidArguments(error.to_string()))
        })
        .transpose()?
        .or_else(|| arguments.cloned());
    let resource = find_action_resource(resources, &address.resource)
        .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == address.action)
        .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
    Ok((typed, resource, spec))
}

pub(crate) fn encode_resource_action(
    resources: &[ResourceSpec],
    address: &tilo_actions::ActionAddress,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    argument: Option<&str>,
) -> Result<tilo_actions::EncodedAction, tilo_actions::ApiError> {
    if let Some(arguments) = arguments {
        tilo_actions::api::encode_json(resources, address, arguments)
    } else {
        tilo_actions::api::encode(resources, address, argument)
    }
}

/// Resolve an invocation's arguments against `resources` and encode it.
pub(crate) fn encode_resource_invocation(
    resources: &[ResourceSpec],
    address: &tilo_actions::ActionAddress,
    argument: Option<&str>,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<tilo_actions::EncodedAction, tilo_actions::ApiError> {
    let (arguments, _, _) = resolve_resource_invocation(resources, address, argument, arguments)?;
    encode_resource_action(resources, address, arguments.as_ref(), None)
}

/// The item resources the action in `envelope` created, each with its id and
/// the address it is published under, in minting order: what a write answers
/// its caller, so nothing reads an id out of a rendered address. `minted`
/// are the ids the action logged as it applied — every member of a group's
/// together ([`tilo_resource::take_minted`]) — or, when it logged none, those
/// its seed mints; each is found in the collection the action made its entry
/// in ([`tilo_resource::minted_entry`]), never by reading the change it
/// wrote, so answering costs what it made, not what it wrote.
#[cfg(feature = "model")]
pub fn minted_items(
    doc: &AutoCommit,
    kind: &str,
    minted: &[String],
    envelope: &tilo_actions::ActionEnvelope,
) -> Vec<(String, String)> {
    let mut stored = std::collections::HashMap::new();
    let ids: Vec<String> = if minted.is_empty() {
        let mut found = Vec::new();
        for keys in tilo_resource::minted_keys(envelope) {
            let Some((id, obj)) = keys
                .into_iter()
                .find_map(|id| tilo_resource::minted_entry(&id).map(|obj| (id, obj)))
            else {
                break;
            };
            stored.insert(id.clone(), obj);
            found.push(id);
        }
        found
    } else {
        for id in minted {
            if let Some(obj) = tilo_resource::minted_entry(id) {
                stored.insert(id.clone(), obj);
            }
        }
        minted.to_vec()
    };
    if stored.is_empty() {
        return Vec::new();
    }
    // Each item it stored, under the path of map keys down to it from the document's root: `issues`, or
    // `issues/<issue>/comments` for one inside an item.
    // The document's own collections publish items. What a write that made one stored inside it
    // (an issue's body blocks) is part of that item; one that made none made the item it added
    // to a collection of an item (a comment in an issue's `comments`), named under it. Deeper
    // keys (a text's blocks) are the item's content, not items. Whether a collection is the
    // document's own is one step up from it, so the paths of the others are walked only when it
    // made no item of the document's own.
    let mut own: std::collections::HashMap<&automerge::ObjId, Option<String>> =
        std::collections::HashMap::new();
    let own_key = |obj: &automerge::ObjId| -> Option<String> {
        let parent = automerge::ReadDoc::parents(&*doc, obj).ok()?.next()?;
        match parent.prop {
            automerge::Prop::Map(key)
                if parent.obj == automerge::ROOT && key != tilo_resource::collection::VALUE =>
            {
                Some(key)
            }
            _ => None,
        }
    };
    let made = |id: &str| -> bool {
        stored.get(id).is_some_and(|obj| {
            automerge::ReadDoc::get(&*doc, obj, id)
                .ok()
                .flatten()
                .is_some()
        })
    };
    let mut top = Vec::new();
    for id in &ids {
        let Some(obj) = stored.get(id.as_str()) else {
            continue;
        };
        let key = own.entry(obj).or_insert_with(|| own_key(obj)).clone();
        if let Some(key) = key
            && made(id)
        {
            top.push((id.clone(), format!("{kind}/{key}/{id}")));
        }
    }
    if !top.is_empty() {
        return top;
    }
    // Items made side by side share their collection, whose path is read once.
    let depth = |path: &str| path.split('/').count();
    let mut paths: std::collections::HashMap<&automerge::ObjId, Option<String>> =
        std::collections::HashMap::new();
    ids.into_iter()
        .filter_map(|id| {
            let obj = stored.get(id.as_str())?;
            if !made(&id) {
                return None;
            }
            let path = paths
                .entry(obj)
                .or_insert_with(|| {
                    tilo_resource::collection::path_to(&*doc, obj).map(|path| path.join("/"))
                })
                .clone()?;
            (depth(&path) == 3).then(|| {
                let address = format!("{kind}/{path}/{id}");
                (id, address)
            })
        })
        .collect()
}

/// `value` beside what the write made: the ids it minted, and per item its
/// id and the address it is published under (`made`). A create thus answers
/// what it made on every surface, and a bulk of them answers each.
#[cfg(feature = "model")]
pub fn with_made(
    value: Option<serde_json::Value>,
    minted: &[String],
    items: &[(String, String)],
) -> Option<serde_json::Value> {
    if minted.is_empty() && items.is_empty() {
        return value;
    }
    let made: Vec<serde_json::Value> = items
        .iter()
        .map(|(id, address)| serde_json::json!({"id": id, "address": address}))
        .collect();
    let mut fields = match value {
        Some(serde_json::Value::Object(fields)) => fields,
        None | Some(serde_json::Value::Null) => serde_json::Map::new(),
        // An answer that is not an object (an editor's summary naming the
        // blocks it made) already says what it made; it is kept as it is.
        Some(other) => return Some(other),
    };
    if !minted.is_empty() {
        fields
            .entry("minted")
            .or_insert_with(|| serde_json::json!(minted));
    }
    if !made.is_empty() {
        fields.insert("made".into(), serde_json::Value::Array(made));
    }
    Some(serde_json::Value::Object(fields))
}

/// Canonical Document executor, retaining its actual envelope for owned bookkeeping.
/// Who writes a document change `actor` makes here: `actor`'s handle, which
/// history reads back, joined to the writer of this copy of the record.
///
/// Automerge numbers each writer's changes, so a writer must belong to one
/// copy: a person writes from several devices at once, an agent from a
/// replica per turn, and a copy opened from a workspace's record that lacks the
/// writer latest change (one refused, one not yet published, a state
/// restored from a backup) would number its next change again. Two changes
/// under one writer and sequence cannot both be held, and a record keeping
/// both no longer reads whole.
#[cfg(feature = "model")]
pub fn document_writer(actor: &tilo_actions::Actor, doc: &AutoCommit) -> Vec<u8> {
    crate::history_source::authors::device_actor(&actor.actor_bytes(), doc.get_actor())
}

#[cfg(feature = "model")]
fn execute_document(
    doc: &mut AutoCommit,
    action: &dyn tilo_actions::Action,
    encoded: tilo_actions::EncodedAction,
    transport: &mut dyn tilo_actions::ActionTransport,
    actor: &tilo_actions::Actor,
    broker_admitted: bool,
    context: tilo_actions::ActionContext,
) -> Result<tilo_actions::Dispatched, tilo_actions::ApiError> {
    let before = typed_action_before(doc);
    let capture_heads = if broker_admitted {
        Vec::new()
    } else {
        doc.get_heads()
    };
    let observation = if broker_admitted {
        None
    } else {
        crate::local_document_action::reserve(&encoded, Some(&context), doc, &capture_heads)
            .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()))?
    };
    let capture_before = observation.as_ref().map(|_| capture_heads);
    let capture_encoded = observation.as_ref().map(|_| encoded.clone());
    let capture_context = observation.as_ref().map(|_| context.clone());
    let step = action.change_kind().is_step() && context.change.is_step();
    let writer = document_writer(actor, doc);
    note_active_writer(&writer);
    let dispatched =
        if broker_admitted && action.locus() == tilo_actions::ExecutionLocus::ServerPreferred {
            tilo_actions::Dispatched::AppliedLocal(
                tilo_actions::apply_local_with_context(
                    doc,
                    action,
                    encoded,
                    Some(writer.as_slice()),
                    context,
                )
                .map_err(|e| tilo_actions::ApiError::Failed(e.to_string()))?,
            )
        } else {
            tilo_actions::dispatch_by_with_context(
                doc,
                action,
                encoded,
                transport,
                Some(writer.as_slice()),
                context,
            )
            .map_err(|e| tilo_actions::ApiError::Failed(e.to_string()))?
        };
    if let (Some(observation), Some(capture_before), Some(encoded)) =
        (observation, capture_before, capture_encoded)
        && matches!(&dispatched, tilo_actions::Dispatched::AppliedLocal(_))
    {
        observation
            .finish(doc, &capture_before, encoded, capture_context)
            .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()))?;
    }
    if let Some(before) = before {
        let after = doc.get_heads();
        report_typed_action(doc, before, after, step);
    }
    Ok(dispatched)
}

#[cfg(feature = "model")]
fn note_active_writer(writer: &[u8]) {
    let writers = ACTIVE.with_borrow(|active| {
        let handle = active.as_ref().and_then(|entry| entry.3.clone())?;
        let frame = handle.try_borrow().ok()?;
        frame.as_ref().map(|frame| Rc::clone(&frame.state.writers))
    });
    if let Some(writers) = writers {
        writers.borrow_mut().note(automerge::ActorId::from(writer));
    }
}

/// The refusal of an own-device effect for someone not at this device.
#[cfg(feature = "model")]
pub(crate) fn own_device_refusal() -> tilo_actions::ApiError {
    tilo_actions::ApiError::Unavailable {
        code: "own_device".into(),
        reason: "This action acts on the device of whoever invokes it: here, the host presence's"
            .into(),
    }
}

/// Whether `actor` may run an own-device effect here ([`tilo_actions::OWN_DEVICE_SCOPE`]): it acts on
/// the device of whoever invokes it, so only the person at this device, or an agent the broker admitted
/// acting for them, has it as their own.
#[cfg(feature = "model")]
pub(crate) fn runs_own_device(
    actor: &tilo_actions::Actor,
    local_author: &tilo_actions::MemberId,
    broker_admitted: bool,
    ambient: Option<&tilo_actions::ActionContext>,
) -> bool {
    actor.member_id() == Some(local_author)
        || (broker_admitted
            && ambient
                .and_then(|context| context.invoker.as_ref())
                .is_some_and(|invoker| {
                    invoker.acting.by == *actor
                        && invoker.acting.is_agent()
                        && Some(invoker.acting.acted_for_person()) == Some(local_author.person_id())
                }))
}

/// A resolved, encoded resource operation and the caller it runs for.
#[cfg(feature = "model")]
pub(crate) struct ResourceOperation<'a> {
    pub(crate) target_route: Option<tilo_ui_base::collaboration::RouteSpec>,
    pub(crate) address: &'a tilo_actions::ActionAddress,
    pub(crate) resource: &'a ResourceSpec,
    pub(crate) spec: &'a tilo_actions::ActionSpec,
    pub(crate) encoded: tilo_actions::EncodedAction,
    pub(crate) actor: &'a tilo_actions::Actor,
    pub(crate) local_author: &'a tilo_actions::MemberId,
    pub(crate) broker_admitted: bool,
    pub(crate) invocation: tilo_actions::InvocationId,
    /// Exact invocation retained by an admitted external dispatch.
    pub(crate) source_invocation: Option<tilo_actions::InvocationId>,
    pub(crate) ambient: Option<&'a tilo_actions::ActionContext>,
    /// The session's action API of the host running it: where opening another app goes
    /// ([`crate::session_api`]).
    pub(crate) api: Option<&'a mut dyn crate::session_api::SessionApi>,
}

/// One execution body for bound components, under the broker's admission.
#[cfg(feature = "model")]
pub(crate) fn execute_resource_operation(
    widget: &mut dyn Widget,
    doc: &mut AutoCommit,
    path: &ObjId,
    registry: &tilo_actions::ActionRegistry,
    transport: &mut dyn tilo_actions::ActionTransport,
    operation: ResourceOperation<'_>,
) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
    perform_resource_operation(widget, doc, path, registry, transport, operation)
}

#[cfg(feature = "model")]
fn perform_resource_operation(
    widget: &mut dyn Widget,
    doc: &mut AutoCommit,
    path: &ObjId,
    registry: &tilo_actions::ActionRegistry,
    transport: &mut dyn tilo_actions::ActionTransport,
    operation: ResourceOperation<'_>,
) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
    let ResourceOperation {
        target_route,
        address,
        resource,
        spec,
        encoded,
        actor,
        local_author,
        broker_admitted,
        invocation,
        source_invocation,
        ambient,
        api,
    } = operation;
    debug_assert!(
        !broker_admitted || source_invocation.is_some(),
        "admitted operation {address:?} must retain its source invocation"
    );
    if spec.contract.runs_on_invokers_device()
        && !runs_own_device(actor, local_author, broker_admitted, ambient)
    {
        return Err(own_device_refusal());
    }
    crate::invocation_scope::check_shown_here(actor, local_author, resource, spec)?;
    let mut context = spec.history_context(resource.id.as_str(), &resource.kind, &resource.label);
    if let Some(ambient) = ambient {
        context = context.enriched_by(ambient);
    }
    if context.app.is_none()
        && let Some(route) = target_route.as_ref()
    {
        context = context.with_app(&route.app_id, &route.app_id);
    }
    // The admitted caller is who the action is by, local or delivered:
    // pinned with it, so its `apply` reads the same author everywhere.
    if context.invoker.is_none() {
        context =
            context.with_invoker(tilo_actions::ActionInvoker::admitted(&actor.clone().into()));
    }
    // Ambient context is descriptive and may arrive from the caller. The
    // retained dispatch boundary is the sole source for invocation
    // identity, so replace any ambient value, including with `None`.
    let mut context = context.with_source_invocation(source_invocation);
    if !spec.carries_actions()
        && let Some(described) = tilo_actions::Described::of_published(
            &encoded,
            tilo_actions::ActionKey::new("", &resource.kind, &spec.id),
        )
    {
        context = context.with_described(described);
    }

    // Reserve at the checked operation entry, not just in typed Document apply.
    // The callback remains synchronous; admission is drained asynchronously.
    // Broker deliveries already own an admission and must never be reported twice. Only what the
    // broker admits a change report for is captured: a document's change is reserved in typed
    // apply, and an effect changes nothing a history records.
    let capture_local = !broker_admitted
        && spec.kind != tilo_actions::OperationKind::Document
        && spec.kind.reports_a_change(spec.contract.need().kind)
        && crate::local_document_action::input_is_active()
        && context.source_invocation.is_none()
        && !matches!(context.change, tilo_actions::ChangeKind::External);
    let capture_heads = if capture_local {
        doc.get_heads()
    } else {
        Vec::new()
    };
    let observation = if capture_local {
        crate::local_document_action::reserve(&encoded, Some(&context), doc, &capture_heads)
            .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()))?
    } else {
        None
    };
    let capture_encoded = observation.as_ref().map(|_| encoded.clone());
    let capture_context = observation.as_ref().map(|_| context.clone());
    let result = (|| {
        let asked_of_session = encoded.type_id == collab_types::app_actions::OPEN_APP_OPERATION;
        let started = if spec.kind == tilo_actions::OperationKind::Document {
            let action = registry
                .decode(&encoded)
                .map_err(tilo_actions::ApiError::decoding)?;
            if spec.deferred_invocation_type().is_some() && action.locus() != spec.locus {
                return Err(tilo_actions::ApiError::Failed(
                    "Deferred action locus differs from its declaration".into(),
                ));
            }
            note_native_text_policy(widget, doc, &encoded);
            // Input that runs without an authenticated publication leaves its
            // history unsigned only where it changes the document.
            let unsigned = if !broker_admitted && source_invocation.is_none() {
                crate::local_document_action::unsigned_input(Some(&context))
                    .map(|edits| (edits, doc.get_heads()))
            } else {
                None
            };
            let declared = matches!(spec.contract.result, tilo_actions::ResultSpec::Json { .. });
            // An action that answers what it changed reads it off the change
            // alone: automerge's diff index, armed for this action where no
            // one else keeps one and no transaction is open, so the answer
            // costs what the action changed, not a pass over every operation
            // the document holds.
            let indexed = (declared || answering())
                && doc.diff_cursor().is_empty()
                && automerge::transaction::Transactable::pending_ops(doc) == 0;
            if indexed {
                doc.update_diff_cursor();
            }
            // Where the document stood before the action: one that moves several steps writes
            // a change for each, and what it changed is all of them.
            let start = doc.get_heads();
            let dispatched = execute_document(
                doc,
                action.as_ref(),
                encoded,
                transport,
                actor,
                broker_admitted,
                context,
            );
            if indexed && dispatched.is_err() {
                doc.reset_diff_cursor();
            }
            if let Some((edits, heads)) = unsigned
                && doc.get_heads() != heads
            {
                crate::local_document_action::note_unsigned_edit(&edits);
            }
            let applied = match dispatched? {
                tilo_actions::Dispatched::AppliedLocal(envelope) => Some(envelope),
                tilo_actions::Dispatched::SubmittedToHost(_) => None,
            };
            let before = applied.as_ref().map(|_| start);
            if let Some(envelope) = applied.as_ref() {
                crate::history_mode::capability::document_action_applied(
                    widget,
                    &envelope.action,
                    &envelope.base_change_hashes(),
                    doc,
                    path,
                );
            }
            // An edit an effect waits on answers it what it changed, whatever its
            // declaration: the effect replies to its caller from that.
            let value = (declared || answering())
                .then(|| {
                    before.as_ref().and_then(|before| {
                        let after = doc.get_heads();
                        let changes = doc.diff(before, &after);
                        let value = crate::widget::resources::document_action_result(
                            &*widget, doc, address, &changes,
                        );
                        // An answer names the text it wrote too: whoever's view shows that text
                        // draws those characters as the writer's, with the writer's caret at
                        // their end.
                        match tilo_ui_base::presence::written_by(doc, &changes) {
                            Some(wrote) if declared => {
                                tilo_actions::ActionOutput::noting_wrote(value, &wrote)
                            }
                            _ => value,
                        }
                    })
                })
                .and_then(|value| match value {
                    None if declared => Some(serde_json::Value::Null),
                    value => value,
                });
            if indexed {
                doc.reset_diff_cursor();
            }
            // What the action minted, taken before it answers: a group's
            // members' ids are here together.
            let minted = tilo_resource::take_minted();
            let made = match (applied.as_ref(), before.as_ref()) {
                (Some(envelope), Some(_)) => minted_items(doc, &resource.kind, &minted, envelope),
                _ => Vec::new(),
            };
            let value = with_made(value, &minted, &made);
            let resources = made.into_iter().map(|(_, address)| address).collect();
            let output = tilo_actions::ActionOutput { value, resources };
            if !matches!(
                spec.contract.execution,
                tilo_actions::ExecutionMode::Immediate
            ) && let Some(envelope) = applied.as_ref()
                && let Some(effects) = tilo_ui_base::widget::capability::capability_mut::<
                    dyn tilo_ui_base::widget::document_effects::DocumentEffects,
                >(widget)
            {
                let operation = tilo_ui_base::widget::ResourceOperation::local(
                    source_invocation.unwrap_or(invocation),
                    tilo_actions::OperationKind::Document,
                    &envelope.action.type_id,
                    &envelope.action.payload,
                );
                let before = envelope.base_change_hashes();
                let applied = tilo_ui_base::widget::document_effects::AppliedDocument {
                    invocation,
                    before: &before,
                    document: doc,
                    output: output.clone(),
                };
                effects
                    .start_document_effects(&operation, &applied)
                    .map_err(tilo_actions::ApiError::Failed)?
                    .unwrap_or(tilo_ui_base::widget::ResourceOperationStart::Completed(
                        output,
                    ))
            } else {
                tilo_ui_base::widget::ResourceOperationStart::Completed(output)
            }
        } else if let Some(readers) =
            tilo_ui_base::semantic_readers::registered_in_tree(&*widget, &encoded.type_id)
        {
            let refused = |error: tilo_ui_base::semantic_readers::SemanticReadError| {
                let (code, reason) = error.describe();
                tilo_actions::ApiError::Unavailable {
                    code: code.into(),
                    reason: reason.into(),
                }
            };
            let selected = crate::semantic_document_read::select(readers, resource, spec, &encoded)
                .map_err(refused)?
                .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
            tilo_ui_base::widget::ResourceOperationStart::Completed(
                selected.read(doc).map_err(refused)?,
            )
        } else if encoded.type_id == collab_types::app_actions::OPEN_APP_OPERATION {
            crate::session_api::start_application_launch(
                api,
                target_route.as_ref(),
                &encoded.payload,
                source_invocation.unwrap_or(invocation),
                broker_admitted,
            )?
        } else {
            let target = target_route.map(|route| {
                tilo_ui_base::widget::resource_operation::OperationTarget::new(
                    route,
                    address.clone(),
                )
            });
            let op = tilo_ui_base::widget::ResourceOperation::local(
                invocation,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
            );
            let op = op
                .with_target(target.as_ref())
                .with_resource(resource.id.as_str());
            tilo_actions::with_invoking_context(ambient.cloned(), || {
                tilo_ui_base::widget::start_operation_refused(
                    &mut *widget,
                    &op,
                    &mut tilo_ui_base::widget::Edits::lend(doc),
                    path,
                )
            })
            .map_err(operation_refusal)?
            .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?
        };
        match &started {
            tilo_ui_base::widget::ResourceOperationStart::Accepted
                if !asked_of_session
                    && matches!(
                        spec.contract.execution,
                        tilo_actions::ExecutionMode::Immediate
                    ) =>
            {
                return Err(tilo_actions::ApiError::Failed(
                    "an immediate operation cannot remain pending".into(),
                ));
            }
            tilo_ui_base::widget::ResourceOperationStart::Completed(output) => {
                // What a document action answers an effect waiting on it is
                // beside its declaration, which says what its callers get.
                let declared = match &spec.contract.result {
                    tilo_actions::ResultSpec::None | tilo_actions::ResultSpec::Job
                        if answering() =>
                    {
                        tilo_actions::ActionOutput {
                            value: None,
                            ..output.clone()
                        }
                    }
                    _ => output.clone(),
                };
                spec.contract
                    .result
                    .validate(&declared)
                    .map_err(tilo_actions::ApiError::Failed)?;
            }
            tilo_ui_base::widget::ResourceOperationStart::Accepted => {}
        }
        Ok(started)
    })();
    if let (Some(observation), Some(encoded)) = (observation, capture_encoded) {
        // A view or a read the input made is reported while it changed nothing, as the broker
        // admits it: a record's own bookkeeping — a page's actions published as it opens — is no
        // change of the person's to keep, and a report of it is refused.
        if doc.get_heads() == capture_heads {
            observation
                .finish(doc, &capture_heads, encoded, capture_context)
                .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()))?;
        }
    }
    result
}

/// One caller's request to a bound resource action, before resolution.
#[cfg(feature = "model")]
#[derive(Clone, Copy)]
pub(crate) struct ResourceActionRequest<'a> {
    pub(crate) argument: Option<&'a str>,
    pub(crate) arguments: Option<&'a serde_json::Map<String, serde_json::Value>>,
    pub(crate) actor: &'a tilo_actions::Actor,
    /// The person's approve of the asked record this call was kept as, when it is that call sent
    /// again ([`press_asks`]).
    pub(crate) approval: Option<&'a tilo_actions::remote::asked::Approval>,
    pub(crate) broker_admitted: bool,
    /// The person's own press made it ([`press_asks`]).
    pub(crate) pressed: bool,
    pub(crate) invocation: tilo_actions::InvocationId,
    pub(crate) source_invocation: Option<tilo_actions::InvocationId>,
    pub(crate) ambient: Option<&'a tilo_actions::ActionContext>,
    /// Asked with the action as the API resolved it, before any check after resolution and before
    /// anything runs: `true` hands the invocation back unexecuted as [`BoundActionError::Rerouted`],
    /// for the host to route through the workspace broker. Asked with the resource it is invoked
    /// on, as one another workspace holds is admitted there.
    pub(crate) route: Option<&'a dyn Fn(&ResourceSpec, &tilo_actions::ActionSpec) -> bool>,
}

/// Private checked front API.
#[cfg(feature = "model")]
pub(crate) fn invoke_resource_action(
    root: &mut dyn Widget,
    document: &mut AutoCommit,
    target: &BoundActionTarget,
    transport: &mut dyn tilo_actions::ActionTransport,
    api: Option<&mut dyn crate::session_api::SessionApi>,
    request: ResourceActionRequest<'_>,
) -> Result<tilo_ui_base::widget::ResourceOperationStart, BoundActionError> {
    perform_resource_action(root, document, target, transport, api, request)
}

#[cfg(feature = "model")]
fn perform_resource_action(
    root: &mut dyn Widget,
    document: &mut AutoCommit,
    target: &BoundActionTarget,
    transport: &mut dyn tilo_actions::ActionTransport,
    mut api: Option<&mut dyn crate::session_api::SessionApi>,
    request: ResourceActionRequest<'_>,
) -> Result<tilo_ui_base::widget::ResourceOperationStart, BoundActionError> {
    let ResourceActionRequest {
        argument,
        arguments,
        actor,
        approval,
        broker_admitted,
        pressed,
        invocation,
        source_invocation,
        ambient,
        route: reroute,
    } = request;
    let _entry = target.binding.enter()?;
    target.binding.note_input();
    let mut result = None;
    let mut accepted = false;
    let mut delivery = |binding: &CallbackBinding,
                        widget: &mut dyn crate::widget::AppWidget,
                        registry: &tilo_actions::ActionRegistry| {
        let route = binding
            .records
            .readable()
            .then(|| binding.broker_route().or_else(|| binding.admission_route()))
            .flatten();
        result = Some(
            binding
                .with_resource_frame(document, false, |doc, path| {
                    let presence = crate::invocation_scope::current_presence();
                    let resolved = presence
                        .is_none()
                        .then(|| resolve_published_action(widget, doc, binding, &target.address.address))
                        .flatten();
                    let resources = if let Some(resource) = resolved {
                        vec![resource]
                    } else {
                        let mut resources = Vec::new();
                        // The loan's root belongs to this component. Nested boundaries are
                        // independent destinations and cannot donate declarations here.
                        collect_published(
                            widget,
                            doc,
                            Walk::Component,
                            presence.as_ref(),
                            &mut resources,
                        );
                        if tilo_actions::api::resolve(&resources, &target.address.address).is_none()
                            && let Some(resource) = resolve_published(
                                widget,
                                doc,
                                Walk::Component,
                                &target.address.address.resource,
                            )
                                    .or_else(|| {
                                        provider_resource(widget, binding, &target.address.address)
                                    })
                        {
                            resources.push(resource);
                        }
                        resources
                            .into_iter()
                            .filter_map(|resource| filter_resource(binding, resource))
                            .collect::<Vec<_>>()
                    };
                    let (arguments, resource, spec) = resolve_resource_invocation(
                        &resources,
                        &target.address.address,
                        argument,
                        arguments,
                    )?;
                    if let Some(hosted) = HostedInvocation::of(
                        spec,
                        &target.address.address,
                        arguments.as_ref(),
                        argument,
                        approval,
                    ) {
                        return Err(BoundActionError::Hosted(hosted));
                    }
                    if reroute.is_some_and(|reroute| reroute(resource, spec)) {
                        return Err(BoundActionError::Rerouted { arguments });
                    }
                    // An invocation the workspace delivers was admitted there, the person's yes
                    // included where it waited for one; what this window was told of its actions
                    // only shows its controls.
                    if !broker_admitted {
                        press_asks(spec, arguments.as_ref(), argument, approval, pressed)?;
                    }
                    // A record another workspace holds is decided there: its grants and the
                    // person's answers about it are the workspace's, and this window's view of
                    // them is its controls, never the call. A settings section is the person's
                    // own, decided by its model and kept by the settings store.
                    if !broker_admitted
                        && resource.workspace.is_none()
                        && !resource.kind.starts_with("settings.")
                    {
                        let (answer, spent) = binding.permissions.borrow().invocation_verdict(
                            &binding.app_id,
                            resource,
                            spec,
                            arguments.as_ref(),
                            argument,
                        );
                        let asker = binding.permissions.borrow().asker();
                        match tilo_ui_base::permissions::admit_in_window(answer, spec, asker) {
                            Err(tilo_actions::ApiError::ConfirmationRequired)
                                if approval.is_some() => {}
                            admitted => admitted.map_err(|refused| {
                                match binding.permissions.borrow().shown_refusal(resource, spec) {
                                    Some(held)
                                        if answer.answer == tilo_ui_base::permissions::Answer::Deny =>
                                    {
                                        held
                                    }
                                    _ => refused,
                                }
                            })?,
                        }
                        // A grant given once is spent by admitting this call,
                        // before it runs: the host revokes it, and this API never
                        // relies on it again.
                        binding
                            .permissions
                            .update(|permissions| permissions.spend_once(&spent))
                            .map_err(|_| denied(spec))?;
                    }
                    crate::invocation_scope::check_continuation(
                        spec,
                        spec.need_for(arguments.as_ref(), argument).kind,
                    )?;
                    // A refused private View must not invoke a Deferred encoder.
                    // The execution guard below remains an independent boundary.
                    crate::invocation_scope::check_shown_here(actor, &binding.author, resource, spec)?;
                    let _capture = if !broker_admitted && source_invocation.is_none() {
                        let supplied = arguments.clone().or_else(|| match argument {
                            None => Some(serde_json::Map::new()),
                            Some(value) if spec.params().len() == 1 => Some(
                                serde_json::Map::from_iter([(
                                    spec.params()[0].name.to_string(),
                                    serde_json::Value::String(value.to_owned()),
                                )]),
                            ),
                            Some(_) => None,
                        });
                        supplied.map(|arguments| {
                            crate::local_document_action::with_invocation(
                                crate::local_document_action::PublishedInvocation {
                                    resource: target.address.address.resource.clone(),
                                    action: target.address.address.action.clone(),
                                    arguments,
                                },
                            )
                        })
                    } else {
                        None
                    };
                    let owner_app = route
                        .as_ref()
                        .map_or(binding.app_id.as_str(), |route| route.app_id.as_str());
                    let ambient = ambient
                        .unwrap_or(&*binding.operation_context)
                        .clone()
                        .with_app(owner_app, owner_app);
                    let encoded = encode_resource_action(
                        &resources,
                        &target.address.address,
                        arguments.as_ref(),
                        argument,
                    )?;
                    let execute = |widget: &mut dyn Widget, doc: &mut AutoCommit, path: &ObjId| {
                        with_native_text(if actor.member_id().is_some_and(|member| *member == binding.author && !member.is_agent()) { target.input } else { Input::Discrete }, || perform_resource_operation(
                            widget,
                            doc,
                            path,
                            registry,
                            transport,
                            ResourceOperation {
                                target_route: route.clone(),
                                address: &target.address.address,
                                resource,
                                spec,
                                encoded,
                                actor,
                                local_author: &binding.author,
                                broker_admitted,
                                invocation,
                                source_invocation,
                                ambient: Some(&ambient),
                                api: match api.as_mut() {
                                    Some(api) => Some(&mut **api as &mut dyn crate::session_api::SessionApi),
                                    None => None,
                                },
                            },
                        ))
                    };
                    let run = |doc: &mut AutoCommit| {
                    let mut operation = BoundOperation::enter(binding, doc, spec.kind)?;
                    let started = crate::invocation_scope::with_loaned_resource(
                        &binding.records,
                        &binding.checkout,
                        &binding.app_id,
                        route.clone(),
                        operation.doc,
                        |doc| execute(widget, doc, path),
                    );
                    accepted = matches!(started, Ok(tilo_ui_base::widget::ResourceOperationStart::Accepted));
                    // Consume the legacy rising edge without widening an exact typed
                    // span to arbitrary writes elsewhere in the opaque callback.
                    binding.drain_owned_tree(widget, operation.doc)?;
                    if operation.finish() {
                        return Err(BoundActionError::Api(tilo_actions::ApiError::Unavailable {
                            code: "undeclared_document_mutation".into(),
                            reason: "callback mutated the document without a declared Document follow-up; bytes were retained".into(),
                        }));
                    }
                    started.map_err(BoundActionError::Api)
                    };
                    run(doc)
                })
                .map_err(BoundActionError::Dispatch)
                .and_then(std::convert::identity),
        );
        // Even a post-delivery contract fault must retain Accepted lifecycle
        // ownership for the component's existing CLOSED cancellation/cleanup path.
        accepted.then_some((invocation, true))
    };
    visit_components(
        root,
        &mut ComponentRequest::new(Some(&target.binding), &mut delivery),
    )?;
    result.unwrap_or(Err(BoundActionError::Dispatch(
        BoundDispatchError::ClosedLifetime,
    )))
}

#[cfg(feature = "model")]
fn below_root(root: bool, resource: ResourceSpec) -> ResourceSpec {
    if root {
        resource
    } else {
        crate::widget::resources::resolved_outside_document(resource)
    }
}

#[cfg(feature = "model")]
#[derive(Clone, Copy)]
enum Walk {
    Component,
    Unbound,
}

#[cfg(feature = "model")]
fn for_each_published(widget: &dyn Widget, walk: Walk, visit: &mut dyn FnMut(&dyn Widget, bool)) {
    fn descend(
        widget: &dyn Widget,
        root: bool,
        walk: Walk,
        visit: &mut dyn FnMut(&dyn Widget, bool),
    ) {
        if has_separate_document_owner(widget) {
            return;
        }
        visit(widget, root);
        let whole = matches!(walk, Walk::Unbound);
        widget.for_each_child(&mut |child| descend(child, whole, walk, visit));
    }
    descend(widget, true, walk, visit);
}

#[cfg(feature = "model")]
fn collect_published(
    widget: &dyn Widget,
    doc: &AutoCommit,
    walk: Walk,
    presence: Option<&tilo_actions::PresenceId>,
    resources: &mut Vec<ResourceSpec>,
) {
    let mut taken = crate::widget::resources::TakenResources::default();
    for_each_published(widget, walk, &mut |widget, root| {
        let found = match presence {
            Some(presence) => crate::widget::resources::resources_for(widget, Some(presence), doc),
            None => crate::widget::resources::resources(widget, doc),
        };
        let found: Vec<_> = found
            .into_iter()
            .filter(|resource| !taken.holds(resource.id.as_str()))
            .collect();
        for resource in &found {
            taken.take(resource.id.as_str());
        }
        if root {
            resources.extend(found);
        } else {
            resources.extend(
                found
                    .into_iter()
                    .map(crate::widget::resources::outside_document),
            );
        }
    });
}

#[cfg(feature = "model")]
fn resolve_published(
    widget: &dyn Widget,
    doc: &AutoCommit,
    walk: Walk,
    id: &str,
) -> Option<ResourceSpec> {
    let presence = crate::invocation_scope::current_presence();
    let mut found = None;
    for_each_published(widget, walk, &mut |widget, root| {
        if found.is_none() {
            found = crate::widget::resources::resource_for(widget, presence.as_ref(), doc, id)
                .map(|resource| below_root(root, resource));
        }
    });
    found
}

/// What the outer tree rooted at `widget` publishes.
#[cfg(feature = "model")]
pub(crate) fn collect_unbound_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    resources: &mut Vec<ResourceSpec>,
) {
    collect_published(widget, doc, Walk::Unbound, None, resources);
}

/// [`collect_unbound_resources`] rebuilt for one presence view.
/// Never cached: it is the invoker's tree at invoke time.
#[cfg(feature = "model")]
pub(crate) fn collect_presence_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    presence: &tilo_actions::PresenceId,
    resources: &mut Vec<ResourceSpec>,
) {
    collect_published(widget, doc, Walk::Unbound, Some(presence), resources);
}

/// Resource `id` resolved in the outer tree rooted at `widget`.
#[cfg(feature = "model")]
pub(crate) fn resolve_unbound_resource(
    widget: &dyn Widget,
    doc: &AutoCommit,
    id: &str,
) -> Option<ResourceSpec> {
    resolve_published(widget, doc, Walk::Unbound, id)
}

/// Whether resource `id`, which `widget`'s subtree resolves to nothing now, may resolve once what it
/// reads arrives ([`crate::widget::resources::PublishesResources::resource_pending`]).
#[cfg(feature = "model")]
pub(crate) fn unbound_resource_pending(widget: &dyn Widget, id: &str) -> bool {
    if has_separate_document_owner(widget) {
        return false;
    }
    let mut pending = crate::widget::resources::resource_pending(widget, id);
    widget.for_each_child(&mut |child| {
        pending = pending || unbound_resource_pending(child, id);
    });
    pending
}

/// Runtime metadata; does not own the component's widget or mount lifetime.
#[cfg(feature = "model")]
#[derive(Clone)]
pub struct CallbackBinding {
    pub(crate) lifetime: WakeHandle,
    pub(crate) closing: Rc<Cell<bool>>,
    pub(crate) registry_owner: std::rc::Weak<()>,
    pub(crate) lifetime_id: uuid::Uuid,
    pub(crate) control_domain: tilo_ui_base::control_domain::WeakControlDomain,
    pub(crate) queued_controls:
        Rc<RefCell<std::collections::VecDeque<tilo_actions::ControlInvocation>>>,
    /// Searches the component asked the process to open, drained as process intents by the shell that
    /// presents search.
    pub(crate) search_requests:
        Rc<RefCell<std::collections::VecDeque<crate::widget::search_request::SearchRequest>>>,
    pub(crate) instance_id: crate::application::InstanceId,
    pub(crate) app_id: String,
    /// Executing device supplied by the component's ProcessContext, never its caller.
    pub(crate) device_id: String,
    /// Immutable local person supplied at host preparation, not the action author.
    pub(crate) local_person: MemberId,
    pub(crate) records: ResourceStore,
    pub(crate) checkout: DocumentCheckout,
    pub(crate) permissions: SharedPermissions,
    pub(crate) author: MemberId,
    pub(crate) operation_context: Rc<tilo_actions::ActionContext>,
    /// Edits commit as the author writer on the record
    /// ([`crate::application::ProcessContext::write_as_author`]).
    pub(crate) writes_as_author: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl CallbackBinding {
    /// The records the component publishes into, read by a test through its binding.
    #[doc(hidden)]
    pub fn records_for_test(&self) -> &ResourceStore {
        &self.records
    }

    /// The record the component's edits are checked out against.
    #[doc(hidden)]
    pub fn checkout_for_test(&self) -> &DocumentCheckout {
        &self.checkout
    }

    /// The person the component writes as.
    #[doc(hidden)]
    pub fn author_for_test(&self) -> &MemberId {
        &self.author
    }
}

#[cfg(feature = "model")]
impl tilo_ui_base::collaboration::native_initial::ComponentBinding for CallbackBinding {
    type Entry = CallbackScopeGuard;
    type EnterError = BoundDispatchError;
    fn admission_route(&self) -> Option<tilo_ui_base::collaboration::RouteSpec> {
        CallbackBinding::admission_route(self)
    }
    fn app_id(&self) -> &str {
        &self.app_id
    }
    fn checkout(&self) -> &DocumentCheckout {
        &self.checkout
    }
    fn enter(&self) -> Result<CallbackScopeGuard, BoundDispatchError> {
        CallbackBinding::enter(self)
    }
    fn merge_committed(&self, mutation: collab_types::DocumentMutation) -> Result<(), String> {
        self.records
            .merge_replica(&self.checkout, mutation)
            .map(|_| ())
    }
}

/// Private retained traversal request. The public Widget trait cannot expose a
/// root/registry closure directly; only toolkit code can construct this request.
#[cfg(feature = "model")]
type ComponentVisitor<'a> = dyn FnMut(
        &CallbackBinding,
        &mut dyn crate::widget::AppWidget,
        &tilo_actions::ActionRegistry,
    ) -> Option<(tilo_actions::InvocationId, bool)>
    + 'a;
#[cfg(feature = "model")]
pub(crate) struct ComponentRequest<'a> {
    pub(crate) target: Option<&'a CallbackBinding>,
    pub(crate) found: bool,
    pub(crate) visit: &'a mut ComponentVisitor<'a>,
    /// The component shows its record's save status in place of the visit
    /// ([`service_owner_save_progress`]); `shown` says whether that changed what it shows.
    pub(crate) save_progress: Option<bool>,
}
#[cfg(feature = "model")]
impl<'a> ComponentRequest<'a> {
    /// A visit of the component `target` names, or of every component.
    pub(crate) fn new(
        target: Option<&'a CallbackBinding>,
        visit: &'a mut ComponentVisitor<'a>,
    ) -> Self {
        Self {
            target,
            found: false,
            visit,
            save_progress: None,
        }
    }
}

/// One original ordinary model pass for the current retained owner.
#[cfg(feature = "model")]
pub(crate) fn service_retained_owner_model(
    root: &mut dyn Widget,
    outer: &mut AutoCommit,
    work: &crate::owner_wake::OwnerWakeRequest,
) -> Result<(), BoundDispatchError> {
    let captured = work.binding();
    let _entry = captured.enter()?;
    let mut result = None;
    let mut visit = |current: &CallbackBinding,
                     owner: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        if current.same_component(captured) {
            let updated = current
                .with_settlement(outer, |document, resource_root| {
                    owner.update(document, resource_root);
                    owner.settle(
                        &mut tilo_ui_base::widget::Edits::lend(document),
                        resource_root,
                    );
                    current.drain_owned_tree(owner, document).map(|_| ())
                })
                .and_then(|result| result);
            let synced = current
                .with_document(outer, |document, resource_root| {
                    owner.sync(document, resource_root);
                    current.drain_owned_tree(owner, document).map(|_| ())
                })
                .and_then(|result| result);
            // A refused update is still synced, and the first error stands.
            let admitted = updated.and(synced);
            // Status is projected after the loan, even refused; no callback runs in it.
            let projected = current.enter().and_then(|_entry| {
                let handle = current.lifetime.clone();
                let wake: crate::resource::ReadyWake =
                    std::sync::Arc::new(move || handle.wake_save_progress());
                let parts = current.chain_parts();
                current
                    .sampled_save_status(&parts, &wake)
                    .map_err(BoundDispatchError::Record)
                    .map(|status| owner.resource_save_status(status))
            });
            let offered = current.enter().and_then(|_entry| {
                current
                    .records
                    .offer_resource(&current.checkout.id)
                    .map_err(BoundDispatchError::Persistence)
            });
            result = Some(admitted.and(projected).and(offered));
        }
        None
    };
    let mut request = ComponentRequest::new(Some(captured), &mut visit);
    visit_components(root, &mut request)?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

/// A record's save moved on and nothing else woke the owner
/// ([`crate::owner_wake::OwnerWakeRequest::save_progress_only`]): its owner is told the save status
/// alone, by the component that tells it in every frame, against what it last told it. A save
/// landing after every keystroke repeats the status the key's own frame already showed, and
/// telling it again would draw a second frame for it. Returns whether what the owner shows changed.
#[cfg(feature = "model")]
pub(crate) fn service_owner_save_progress(
    root: &mut dyn Widget,
    work: &crate::owner_wake::OwnerWakeRequest,
) -> Result<bool, BoundDispatchError> {
    let captured = work.binding();
    let _entry = captured.enter()?;
    let mut visit = |_: &CallbackBinding,
                     _: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| None;
    let mut request = ComponentRequest::new(Some(captured), &mut visit);
    request.save_progress = Some(false);
    visit_components(root, &mut request)?;
    if !request.found {
        return Err(BoundDispatchError::ClosedLifetime);
    }
    Ok(request.save_progress == Some(true))
}

#[derive(Debug)]
pub enum BoundDispatchError {
    MissingNode,
    ClosedLifetime,
    CrossResourceReentry,
    #[cfg(feature = "model")]
    Record(ResourceOpenError),
    Persistence(String),
}

/// Logical typing ownership survives physical commits made when an opaque
/// callback from another instance is admitted. Read-only entry never touches it.
#[cfg(feature = "model")]
pub(crate) struct PendingResourceAction {
    instance_id: crate::application::InstanceId,
    owner: Rc<Cell<bool>>,
    before: Vec<ChangeHash>,
    author: MemberId,
    metadata: Option<String>,
    context: tilo_actions::ActionContext,
    native_text: Option<NativeTextSpan>,
}

#[cfg(feature = "model")]
struct NativeTextSpan {
    action: tilo_actions::EncodedAction,
    frontier: Vec<ChangeHash>,
}

#[cfg(feature = "model")]
struct NativeTextPolicy {
    action: tilo_actions::EncodedAction,
    continues: bool,
    continuable: bool,
}

#[cfg(feature = "model")]
struct ActiveMutation {
    binding: CallbackBinding,
    before: Option<Vec<ChangeHash>>,
    /// Settlement names its owner without closing a draft until an action writes.
    deferred: bool,
}
#[cfg(feature = "model")]
struct ActionFrame {
    history: collab_types::history::UndoLog,
    state: crate::resource::ResourceActions,
    mutation: Option<ActiveMutation>,
    operations: Vec<OperationSpan>,
    writer: automerge::ActorId,
}
#[cfg(feature = "model")]
type FrameHandle = Rc<RefCell<Option<ActionFrame>>>;

/// Owned bookkeeping only; the explicit document loan remains on the stack.
/// Effect cursors police opaque mutations. View has no cursor until a real
/// typed producer arrives, so pure navigation never closes pending typing.
#[cfg(feature = "model")]
struct OperationSpan {
    binding: CallbackBinding,
    cursor: Option<Vec<ChangeHash>>,
    effect: bool,
    violation: bool,
    native_text: Option<NativeTextPolicy>,
}

#[cfg(feature = "model")]
fn operation_frame(doc: &AutoCommit) -> Option<FrameHandle> {
    ACTIVE.with_borrow(|active| {
        let (_, _, observation, handle) = active.as_ref()?;
        if !std::ptr::eq(*observation, doc) {
            return None;
        }
        let handle = handle.as_ref()?;
        let owned = handle.borrow();
        let operation = owned.as_ref()?.operations.last()?;
        if operation.binding.closing.get()
            || !operation.binding.lifetime.is_active()
            || operation.binding.registry_owner.upgrade().is_none()
        {
            return None;
        }
        Some(handle.clone())
    })
}

/// The frame of `doc` while a callback of a bound component mutates it (a sync, an event) or one of its
/// operations runs.
#[cfg(feature = "model")]
fn mutation_frame(doc: &AutoCommit) -> Option<FrameHandle> {
    if let Some(handle) = operation_frame(doc) {
        return Some(handle);
    }
    ACTIVE.with_borrow(|active| {
        let (_, _, observation, handle) = active.as_ref()?;
        if !std::ptr::eq(*observation, doc) {
            return None;
        }
        let handle = handle.as_ref()?;
        let owned = handle.borrow();
        let binding = &owned.as_ref()?.mutation.as_ref()?.binding;
        if binding.closing.get()
            || !binding.lifetime.is_active()
            || binding.registry_owner.upgrade().is_none()
        {
            return None;
        }
        Some(handle.clone())
    })
}

#[cfg(feature = "model")]
fn note_native_text_policy(
    widget: &dyn Widget,
    doc: &AutoCommit,
    encoded: &tilo_actions::EncodedAction,
) {
    let input = native_text_input();
    if input == Input::Discrete {
        return;
    }
    let Some(history) = tilo_ui_base::widget::capability::capability::<
        dyn crate::history_mode::capability::DocumentHistory,
    >(widget) else {
        return;
    };
    let Some(handle) = operation_frame(doc) else {
        return;
    };
    let mut owned = handle.borrow_mut();
    let Some(frame) = owned.as_mut() else {
        return;
    };
    let Some(operation) = frame.operations.last() else {
        return;
    };
    let previous = frame
        .state
        .part()
        .pending
        .as_ref()
        .filter(|pending| {
            Rc::ptr_eq(&pending.owner, &operation.binding.closing)
                && pending.author == operation.binding.author
                && pending.instance_id == operation.binding.instance_id
        })
        .and_then(|pending| pending.native_text.as_ref());
    let (continues, continuable) =
        history.physical_text_policy(previous.map(|span| &span.action), encoded);
    let continues = continues && input != Input::Starting;
    if let Some(operation) = frame.operations.last_mut() {
        operation.native_text = Some(NativeTextPolicy {
            action: encoded.clone(),
            continues,
            continuable,
        });
    }
}

/// Narrow shared producer hook. Called only when a typed action is about to
/// execute; returning None preserves the legacy `LocalDispatch` path unchanged.
#[cfg(feature = "model")]
pub(crate) fn typed_action_before(doc: &mut AutoCommit) -> Option<Vec<ChangeHash>> {
    let handle = mutation_frame(doc)?;
    let mut owned = handle.borrow_mut();
    let frame = owned.as_mut()?;
    if let Some(active) = frame.mutation.as_ref().filter(|active| active.deferred) {
        let binding = active.binding.clone();
        frame.activate(&binding, doc);
    }
    let before = frame.commit_physical(doc);
    for operation in &mut frame.operations {
        if operation.cursor.is_none() {
            operation.cursor = Some(before.clone());
        }
    }
    Some(before)
}

/// A successful executor reports owned exact heads, never a borrowed root or
/// registry. No-op and failed/submitted-without-change dispatches add nothing.
///
/// `step` is whether the change is an undo step of its author's
/// ([`tilo_actions::ChangeKind::is_step`]). One that is not (fetched rows, a
/// process's output) closes the step open before it and is left out of every
/// step, so no undo takes it back with someone's edit.
#[cfg(feature = "model")]
pub(crate) fn report_typed_action(
    doc: &AutoCommit,
    before: Vec<ChangeHash>,
    after: Vec<ChangeHash>,
    step: bool,
) {
    if before == after {
        return;
    }
    let Some(handle) = mutation_frame(doc) else {
        return;
    };
    let mut owned = handle.borrow_mut();
    let Some(frame) = owned.as_mut() else {
        return;
    };
    if !step {
        frame.pass_over(before, after, doc);
        return;
    }
    let Some(operation) = frame.operations.last() else {
        return;
    };
    let binding = operation.binding.clone();
    let boundary = frame
        .operations
        .iter()
        .find_map(|scope| scope.cursor.as_ref())
        .cloned()
        .unwrap_or_else(|| before.clone());
    let native = frame
        .operations
        .last_mut()
        .and_then(|operation| operation.native_text.take());
    let continues = native.as_ref().is_some_and(|policy| policy.continues)
        && frame.state.part().pending.as_ref().is_some_and(|pending| {
            Rc::ptr_eq(&pending.owner, &binding.closing)
                && pending.author == binding.author
                && pending.instance_id == binding.instance_id
                && pending
                    .native_text
                    .as_ref()
                    .is_some_and(|span| span.frontier == before)
        });
    if !continues {
        frame.finish_pending_at(boundary, doc);
    }
    for operation in &mut frame.operations {
        operation.violation |= operation.effect
            && operation
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor != &before);
        operation.cursor = Some(after.clone());
    }
    if let Some(native) = native.filter(|policy| policy.continuable) {
        if !continues {
            frame.state.part().pending = Some(PendingResourceAction::new(&binding, before));
        }
        if let Some(pending) = frame.state.part().pending.as_mut() {
            pending.native_text = Some(NativeTextSpan {
                action: native.action,
                frontier: after.clone(),
            });
        }
        if let Some(active) = frame.mutation.as_mut() {
            active.before = (!active.binding.same_component(&binding)).then_some(after);
        }
        frame.state.part().statuses.clear();
        frame.state.generation = frame
            .state
            .generation
            .and_then(|value| value.checked_add(1));
        return;
    }
    if continues {
        frame.finish_pending_at(before.clone(), doc);
    }
    frame.history.record_committed_span(
        doc,
        before,
        after.clone(),
        None,
        None,
        Some(binding.author.clone()),
        Some(binding.instance_id),
    );
    frame.reset_operation_baseline(&binding, after);
    frame.state.generation = frame
        .state
        .generation
        .and_then(|value| value.checked_add(1));
}

#[cfg(feature = "model")]
struct BoundOperation<'a> {
    doc: &'a mut AutoCommit,
    frame: FrameHandle,
    depth: usize,
    finished: bool,
}
#[cfg(feature = "model")]
impl<'a> BoundOperation<'a> {
    fn enter(
        binding: &CallbackBinding,
        doc: &'a mut AutoCommit,
        kind: tilo_actions::OperationKind,
    ) -> Result<Self, BoundDispatchError> {
        if !binding
            .check_resource()?
            .is_some_and(|pointer| std::ptr::eq(pointer, doc))
        {
            return Err(BoundDispatchError::Record(ResourceOpenError::Busy));
        }
        let handle = ACTIVE
            .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
            .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
        let depth = {
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            let effect = kind == tilo_actions::OperationKind::Effect;
            let cursor = effect.then(|| frame.commit_physical(doc));
            let depth = frame.operations.len();
            frame.operations.push(OperationSpan {
                binding: binding.clone(),
                cursor,
                effect,
                violation: false,
                native_text: None,
            });
            depth
        };
        Ok(Self {
            doc,
            frame: handle,
            depth,
            finished: false,
        })
    }
    fn finish(&mut self) -> bool {
        if self.finished {
            return false;
        }
        self.finished = true;
        let mut owned = self.frame.borrow_mut();
        let Some(frame) = owned.as_mut() else {
            return false;
        };
        debug_assert_eq!(frame.operations.len().checked_sub(1), Some(self.depth));
        let after = frame
            .operations
            .last()
            .filter(|operation| operation.effect)
            .map(|_| frame.commit_physical(self.doc));
        let Some(operation) = frame.operations.pop() else {
            return false;
        };
        let mut violation = operation.violation;
        if let Some(after) = after
            && let Some(cursor) = operation.cursor
            && cursor != after
        {
            violation = true;
            frame.finish_pending_at(cursor, self.doc);
            frame.reset_operation_baseline(&operation.binding, after.clone());
            for ancestor in &mut frame.operations {
                ancestor.violation |= ancestor.effect;
                ancestor.cursor = Some(after.clone());
            }
            frame.state.generation = frame
                .state
                .generation
                .and_then(|value| value.checked_add(1));
        }
        violation
    }
}
#[cfg(feature = "model")]
impl Drop for BoundOperation<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(feature = "model")]
struct RestoreFrame<'a> {
    history: &'a mut collab_types::history::UndoLog,
    state: &'a mut crate::resource::ResourceActions,
    frame: FrameHandle,
}
#[cfg(feature = "model")]
impl Drop for RestoreFrame<'_> {
    fn drop(&mut self) {
        if let Some(owned) = self.frame.borrow_mut().take() {
            *self.history = owned.history;
            *self.state = owned.state;
        }
    }
}

/// The undo bracket a widget reported since the last sweep, with what the boundary keeps of it: whether
/// the widget keeps the step open, and its ephemeral state.
#[cfg(feature = "model")]
pub(crate) struct TakenActionBracket {
    before: Vec<ChangeHash>,
    keep_open: bool,
    metadata: Option<String>,
}

#[cfg(feature = "model")]
impl TakenActionBracket {
    /// `widget`'s bracket, if it reported one.
    pub(crate) fn take(widget: &mut dyn Widget) -> Option<Self> {
        let before = tilo_ui_base::widget::undo_access::take_committed_action(widget)?;
        Some(Self {
            before,
            keep_open: tilo_ui_base::widget::undo_access::keeps_undo_step_open(widget),
            metadata: tilo_ui_base::widget::undo_points::get_state(widget),
        })
    }
}

#[cfg(feature = "model")]
impl PendingResourceAction {
    fn new(binding: &CallbackBinding, before: Vec<ChangeHash>) -> Self {
        Self {
            instance_id: binding.instance_id,
            owner: binding.closing.clone(),
            before,
            author: binding.author.clone(),
            metadata: None,
            context: binding
                .authored_context()
                .in_gesture(Some(tilo_actions::ActionId::new())),
            native_text: None,
        }
    }
}

#[cfg(feature = "model")]
impl ActionFrame {
    fn pass_over(&mut self, before: Vec<ChangeHash>, after: Vec<ChangeHash>, doc: &AutoCommit) {
        let boundary = self
            .operations
            .iter()
            .find_map(|scope| scope.cursor.as_ref())
            .cloned()
            .unwrap_or_else(|| before.clone());
        self.finish_pending_at(boundary, doc);
        for operation in &mut self.operations {
            operation.violation |= operation.effect
                && operation
                    .cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor != &before);
            operation.cursor = Some(after.clone());
        }
        let binding = self
            .operations
            .last()
            .map(|operation| operation.binding.clone())
            .or_else(|| self.mutation.as_ref().map(|active| active.binding.clone()));
        match binding {
            Some(binding) => self.reset_operation_baseline(&binding, after),
            None => self.state.part().pending = None,
        }
        self.state.generation = self.state.generation.and_then(|value| value.checked_add(1));
    }

    fn reset_operation_baseline(&mut self, binding: &CallbackBinding, after: Vec<ChangeHash>) {
        self.state.part().pending = Some(PendingResourceAction::new(binding, after.clone()));
        if let Some(active) = self.mutation.as_mut() {
            active.before = (!active.binding.same_component(binding)).then_some(after);
        }
        self.state.part().statuses.clear();
    }
    fn status(
        &mut self,
        doc: &AutoCommit,
        author: &MemberId,
        instance: crate::application::InstanceId,
    ) -> tilo_ui_base::widget::undo_access::UndoStatus {
        let key = (author.clone(), instance);
        let moment = self.history.moment(doc);
        let pending_undo = self.pending_undo(doc, author, instance);
        let locale = tilo_ui_base::i18n::I18nManager::revision();
        if let Some((at, pending, described_in, status)) = self.state.part().answered.get(&key)
            && *at == moment
            && *pending == pending_undo
            && *described_in == locale
        {
            return status.clone();
        }
        let status = self.status_now(doc, author, instance);
        if self.state.part().answered.len() >= 32 {
            self.state.part().answered.clear();
        }
        self.state
            .part()
            .answered
            .insert(key, (moment, pending_undo, locale, status.clone()));
        status
    }

    fn pending_undo(
        &self,
        doc: &AutoCommit,
        author: &MemberId,
        instance: crate::application::InstanceId,
    ) -> bool {
        self.state.pending().is_some_and(|pending| {
            pending.author == *author
                && pending.instance_id == instance
                && (pending.before != doc.base_heads()
                    || (doc.pending_ops() > 0
                        && self.mutation.as_ref().is_none_or(|active| {
                            Rc::ptr_eq(&active.binding.closing, &pending.owner)
                        })))
        })
    }

    fn status_now(
        &mut self,
        doc: &AutoCommit,
        author: &MemberId,
        instance: crate::application::InstanceId,
    ) -> tilo_ui_base::widget::undo_access::UndoStatus {
        let undo = self.history.next_undo_in(doc, author, instance);
        let redo = self.history.next_redo_in(doc, author, instance);
        let key = (author.clone(), instance);
        let mut status = self
            .state
            .part()
            .statuses
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let pending_undo = self.pending_undo(doc, author, instance);
        status.can_undo = undo.is_some() || pending_undo;
        status.can_redo = redo.is_some() && !pending_undo;
        if doc.pending_ops() == 0 {
            self.state.part().statuses.retain(|(author, instance), _| {
                self.history.next_undo_in(doc, author, *instance).is_some()
                    || self.history.next_redo_in(doc, author, *instance).is_some()
            });
            status.undo_summary = undo.as_ref().and_then(crate::undo::step_summary);
            status.redo_summary = redo.as_ref().and_then(crate::undo::step_summary);
            if status.can_undo || status.can_redo {
                self.state.part().statuses.insert(key, status.clone());
            }
        }
        if pending_undo {
            status.undo_summary = None;
            status.redo_summary = None;
        }
        status
    }
    fn commit_physical(&self, doc: &mut AutoCommit) -> Vec<ChangeHash> {
        // Automerge 0.8 can retain an unused actor in its patch log after an
        // empty commit_with. A read-only preflight needs only the existing heads.
        if doc.pending_ops() == 0 {
            return doc.get_heads();
        }
        tilo_resource::history::commit_stamped_with_message(doc, self.step_message());
        doc.get_heads()
    }

    fn step_message(&self) -> Option<String> {
        let context = if let Some(operation) = self
            .operations
            .last()
            .filter(|scope| scope.cursor.is_some())
        {
            Some(operation.binding.authored_context())
        } else {
            match (&self.mutation, self.state.pending()) {
                (Some(active), Some(pending))
                    if Rc::ptr_eq(&active.binding.closing, &pending.owner) =>
                {
                    Some(pending.context.clone())
                }
                (Some(active), _) => Some(active.binding.authored_context()),
                (None, pending) => pending.map(|pending| pending.context.clone()),
            }
        };
        context.and_then(|context| context.in_step().change_message())
    }

    /// Write as `actor` from here on. What is pending was written as the
    /// actor before, so it is committed first: `set_actor` would otherwise
    /// commit it without a time.
    fn switch_writer(&self, doc: &mut AutoCommit, actor: automerge::ActorId) {
        self.state.writers.borrow_mut().note(actor.clone());
        if doc.get_actor() != &actor {
            self.commit_physical(doc);
            doc.set_actor(actor);
        }
    }

    fn writer_of(&self, binding: &CallbackBinding) -> automerge::ActorId {
        if binding.writes_as_author {
            crate::resource::author_writer(&self.writer, binding.author.actor_bytes())
        } else {
            self.writer.clone()
        }
    }

    fn finish_pending_at(&mut self, after: Vec<ChangeHash>, doc: &AutoCommit) {
        if let Some(pending) = self.state.part().pending.take() {
            let author = pending.author.clone();
            let instance = pending.instance_id;
            self.history.record_committed_span(
                doc,
                pending.before,
                after,
                None,
                pending.metadata,
                Some(pending.author),
                Some(pending.instance_id),
            );
            self.status(doc, &author, instance);
        }
    }

    fn settle_segment(&mut self, doc: &mut AutoCommit) {
        let Some(active) = self.mutation.as_ref() else {
            return;
        };
        let Some(before) = active.before.clone() else {
            return;
        };
        let binding = active.binding.clone();
        let after = self.commit_physical(doc);
        if before != after {
            if self
                .state
                .part()
                .pending
                .as_ref()
                .is_some_and(|p| !Rc::ptr_eq(&p.owner, &binding.closing))
            {
                self.finish_pending_at(before.clone(), doc);
            }
            if self.state.part().pending.is_none() {
                self.state.part().pending = Some(PendingResourceAction::new(&binding, before));
            }
        }
    }

    fn activate(&mut self, binding: &CallbackBinding, doc: &mut AutoCommit) {
        self.settle_segment(doc);
        let same_owner = self
            .state
            .part()
            .pending
            .as_ref()
            .is_some_and(|p| Rc::ptr_eq(&p.owner, &binding.closing));
        let before = if same_owner {
            None
        } else {
            let heads = self.commit_physical(doc);
            Some(heads)
        };
        self.switch_writer(doc, self.writer_of(binding));
        if self.state.part().pending.is_none() {
            self.state.part().pending = Some(PendingResourceAction::new(
                binding,
                before.unwrap_or_default(),
            ));
            self.mutation = Some(ActiveMutation {
                binding: binding.clone(),
                before: None,
                deferred: false,
            });
            return;
        }
        self.mutation = Some(ActiveMutation {
            binding: binding.clone(),
            before,
            deferred: false,
        });
    }
}

#[cfg(feature = "model")]
struct SettlementReturn<'a> {
    document: &'a mut AutoCommit,
    frame: FrameHandle,
    previous: Option<ActiveMutation>,
}
#[cfg(feature = "model")]
impl Drop for SettlementReturn<'_> {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.borrow_mut().as_mut() {
            frame.settle_segment(self.document);
            match self.previous.take() {
                Some(previous) if !previous.deferred => {
                    frame.activate(&previous.binding, self.document);
                }
                previous => frame.mutation = previous,
            }
        }
    }
}

/// Restores the caller's segment before its opaque callback resumes. Bookkeeping
/// borrows end before arbitrary widget code runs, so nested same-record delivery
/// reuses owned state without reconstructing references from thread-local pointers.
#[cfg(feature = "model")]
struct MutationReturn<'a> {
    doc: &'a mut AutoCommit,
    frame: FrameHandle,
    previous: Option<CallbackBinding>,
}
#[cfg(feature = "model")]
impl Drop for MutationReturn<'_> {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.borrow_mut().as_mut() {
            if let Some(previous) = &self.previous {
                frame.activate(previous, self.doc);
            } else {
                frame.settle_segment(self.doc);
                frame.mutation = None;
            }
        }
    }
}

#[cfg(feature = "model")]
type ActiveResource = (
    ResourceStore,
    DocumentCheckout,
    *const AutoCommit,
    Option<FrameHandle>,
);
#[cfg(feature = "model")]
thread_local! {
    static ACTIVE: RefCell<Option<ActiveResource>> = const { RefCell::new(None) };
}
/// What entering a component's callback scope has until it is dropped.
#[cfg(feature = "model")]
pub struct CallbackScopeGuard {
    _wake: Option<WakeScopeGuard>,
    _device: Option<ExecutingHostDevice>,
    _person: Option<ExecutingHostPerson>,
    _scope: Option<ExecutingScope>,
    _actor: Option<tilo_resource::action::ResourceActorScope>,
    /// The depth [`OPEN_SCOPES`] stood at before this scope opened it: dropping the guard closes
    /// every scope that stood above it.
    opened: Option<usize>,
}

#[cfg(feature = "model")]
impl CallbackScopeGuard {
    /// Whether this guard opened a scope, rather than running inside one this thread held.
    fn opens(&self) -> bool {
        self.opened.is_some()
    }

    /// The guard for work that runs inside a scope this thread already holds
    /// ([`CallbackBinding::enter`]).
    fn held() -> Self {
        Self {
            _wake: None,
            _device: None,
            _person: None,
            _scope: None,
            _actor: None,
            opened: None,
        }
    }
}

#[cfg(feature = "model")]
impl Drop for CallbackScopeGuard {
    fn drop(&mut self) {
        if let Some(depth) = self.opened.take() {
            OPEN_SCOPES.with_borrow_mut(|open| open.truncate(depth));
        }
    }
}

#[cfg(feature = "model")]
thread_local! {
    /// The component scopes this thread has entered and not left, innermost last. A hosted
    /// component's descendants share its binding, so the walks over them and the document reads
    /// under them meet the same scope at every widget: entering it again while it is open would
    /// only set what it has already set.
    static OPEN_SCOPES: RefCell<Vec<OpenScope>> = const { RefCell::new(Vec::new()) };
}

/// A component's callback scope open on this thread ([`OPEN_SCOPES`]): what entering it again must
/// meet to run the work inside it.
#[cfg(feature = "model")]
struct OpenScope {
    closing: Rc<Cell<bool>>,
    lifetime_id: uuid::Uuid,
    actor: MemberId,
    device_id: String,
    local_person: MemberId,
}

#[cfg(feature = "model")]
impl OpenScope {
    /// Whether `binding` opens the scope this one is, entering as `actor`: its local identity
    /// (device and person) is what its callbacks run as, so another one opens its own scope.
    fn same(&self, binding: &CallbackBinding, actor: &MemberId) -> bool {
        self.lifetime_id == binding.lifetime_id
            && Rc::ptr_eq(&self.closing, &binding.closing)
            && self.actor == *actor
            && self.device_id == binding.device_id
            && self.local_person == binding.local_person
    }
}

/// One component's scope held across a walk that visits many of its widgets or resources one after
/// another. A walk that recurses keeps its own guard alive for its children; a loop over nodes of
/// the same component holds it here, and enters it again only when the component changes.
#[cfg(feature = "model")]
#[derive(Default)]
pub(crate) struct HeldScope {
    held: Option<(CallbackBinding, CallbackScopeGuard)>,
}

#[cfg(feature = "model")]
impl HeldScope {
    /// Enter `binding`'s scope for the visit about to run, unless it is the one this walk holds.
    ///
    /// # Errors
    /// As [`CallbackBinding::enter`].
    pub(crate) fn enter(
        &mut self,
        binding: Option<&CallbackBinding>,
    ) -> Result<(), BoundDispatchError> {
        let Some(binding) = binding else {
            self.held = None;
            return Ok(());
        };
        if let Some((held, _)) = &self.held
            && held.enters_same_scope(binding)
        {
            binding.check_resource()?;
            return Ok(());
        }
        self.held = None;
        let guard = binding.enter()?;
        self.held = Some((binding.clone(), guard));
        Ok(())
    }
}

#[cfg(feature = "model")]
struct ActiveEntry;
#[cfg(feature = "model")]
impl Drop for ActiveEntry {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|active| *active = None);
    }
}

#[cfg(feature = "model")]
impl CallbackBinding {
    /// The entry of edits this binding makes: its operation context, by its
    /// author unless the context names someone.
    pub(crate) fn authored_context(&self) -> tilo_actions::ActionContext {
        let mut context = (*self.operation_context).clone();
        if context.invoker.is_none() {
            context.invoker = Some(tilo_actions::ActionInvoker::member(self.author.clone()));
        }
        context
    }
}

#[cfg(feature = "model")]
impl CallbackBinding {
    /// Queue a search for the host presenting this component. Only the
    /// latest request matters: an older one the host has not shown yet is
    /// replaced.
    pub(crate) fn request_search(&self, request: crate::widget::search_request::SearchRequest) {
        let mut requests = self.search_requests.borrow_mut();
        requests.clear();
        requests.push_back(request);
    }

    /// The search this component asked for, once.
    pub(crate) fn take_search_request(
        &self,
    ) -> Option<crate::widget::search_request::SearchRequest> {
        self.search_requests.borrow_mut().pop_front()
    }

    /// The chain parts this window keeps beside its own document (a
    /// notebook's notes): saved as the document is.
    pub(crate) fn chain_parts(&self) -> Vec<DocumentCheckout> {
        let held = &self.records;
        held.open_resources()
            .into_iter()
            .filter(|part| part.id.chain_part().is_some())
            .collect()
    }

    /// The durability the window shows: its own document's and `parts`', the
    /// worse of them, since a window whose part is not stored has unsaved
    /// work whoever writes it.
    ///
    /// # Errors
    /// The window's own document cannot be sampled now.
    pub(crate) fn shown_save_status(
        &self,
        parts: &[DocumentCheckout],
    ) -> Result<ResourceSaveStatus, ResourceOpenError> {
        let held = &self.records;
        let mut status = held.save_status(&self.checkout.id)?;
        for part in parts {
            if matches!(status, ResourceSaveStatus::Failed(_)) {
                break;
            }
            match held.save_status(&part.id) {
                Ok(failed @ ResourceSaveStatus::Failed(_)) => status = failed,
                Ok(ResourceSaveStatus::Saving) if status == ResourceSaveStatus::Saved => {
                    status = ResourceSaveStatus::Saving;
                }
                _ => {}
            }
        }
        Ok(status)
    }

    /// Arm `wake` for every write the window shows still being written, its
    /// own and `parts`': some stores move a save on only once asked to watch
    /// it.
    pub(crate) fn arm_save_progress(
        &self,
        parts: &[DocumentCheckout],
        wake: &crate::resource::ReadyWake,
    ) {
        let arm = |id| {
            if self.records.save_status(id).ok() == Some(ResourceSaveStatus::Saving) {
                self.records
                    .wake_on_save_progress(id, std::sync::Arc::clone(wake));
            }
        };
        arm(&self.checkout.id);
        for part in parts {
            arm(&part.id);
        }
    }

    /// [`Self::shown_save_status`], arming `wake` for each write still in
    /// flight first, so a save that landed before the arming is heard of.
    ///
    /// # Errors
    /// The window's own document cannot be sampled now.
    pub(crate) fn sampled_save_status(
        &self,
        parts: &[DocumentCheckout],
        wake: &crate::resource::ReadyWake,
    ) -> Result<ResourceSaveStatus, ResourceOpenError> {
        self.arm_save_progress(parts, wake);
        self.shown_save_status(parts)
    }

    pub(crate) fn broker_route(&self) -> Option<tilo_ui_base::collaboration::RouteSpec> {
        let (_, app, _) = self.records.canonical_namespace()?;
        if app != self.app_id {
            return None;
        }
        self.records.route(&self.checkout).ok()
    }
    /// [`Self::broker_route`], or a live New reservation's route before its factory writes the record.
    pub(crate) fn admission_route(&self) -> Option<tilo_ui_base::collaboration::RouteSpec> {
        let (_, app, _) = self.records.canonical_namespace()?;
        if app != self.app_id {
            return None;
        }
        self.records.admission_route(&self.checkout).ok()
    }
    pub(crate) fn matches_broker_route(
        &self,
        observation: Option<&tilo_ui_base::collaboration::AcceptedRoute>,
    ) -> bool {
        observation.is_some_and(|observation| self.matches_route(observation.route()))
    }
    /// Whether `route` is the broker route this binding names.
    pub(crate) fn matches_route(&self, route: &tilo_ui_base::collaboration::RouteSpec) -> bool {
        self.broker_route().as_ref() == Some(route)
            || self.admission_route().as_ref() == Some(route)
    }
    pub(crate) fn drain_owned_tree(
        &self,
        widget: &mut dyn Widget,
        doc: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        if callback_binding(widget).is_some() {
            return Ok(false);
        }
        let mut changed = self.drain_widget_actions(widget, doc)?;
        for child in widget.children_mut() {
            changed |= self.drain_owned_tree(child, doc)?;
        }
        Ok(changed)
    }
    pub(crate) fn undo_status(
        &self,
        outer: &mut AutoCommit,
    ) -> Result<tilo_ui_base::widget::undo_access::UndoStatus, BoundDispatchError> {
        self.with_resource_frame(outer, false, |doc, _| {
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            Ok(frame.status(doc, &self.author, self.instance_id))
        })?
    }
    fn generation(&self) -> Result<Option<u64>, BoundDispatchError> {
        if self.check_resource()?.is_some() {
            return ACTIVE.with_borrow(|active| {
                active
                    .as_ref()
                    .and_then(|entry| entry.3.as_ref())
                    .and_then(|handle| handle.borrow().as_ref().map(|frame| frame.state.generation))
                    .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))
            });
        }
        self.records
            .action_generation(&self.checkout.id)
            .map_err(BoundDispatchError::Record)
    }
    fn document_edit_stamp(
        &self,
        outer: &AutoCommit,
    ) -> Result<DocumentEditStamp, BoundDispatchError> {
        let active = self.check_resource()?;
        let _scope = self.enter()?;
        if let Some(document) = active {
            if !std::ptr::eq(document, outer) {
                return Err(BoundDispatchError::Record(ResourceOpenError::Busy));
            }
            return ACTIVE.with_borrow(|active| {
                let handle = active
                    .as_ref()
                    .and_then(|entry| entry.3.as_ref())
                    .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
                let owned = handle
                    .try_borrow()
                    .map_err(|_| BoundDispatchError::Record(ResourceOpenError::Busy))?;
                let frame = owned
                    .as_ref()
                    .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
                if frame.state.generation.is_none_or(|value| value == u64::MAX)
                    || frame.history.revision() == u64::MAX
                {
                    return Err(BoundDispatchError::Record(ResourceOpenError::Busy));
                }
                Ok(DocumentEditStamp::for_owned_document(
                    outer,
                    self.lifetime_id,
                    frame.history.revision(),
                ))
            });
        }
        let lifetime = self.lifetime_id;
        self.records
            .read_edit_state(&self.checkout.id, |document, revision| {
                DocumentEditStamp::for_owned_document(document, lifetime, revision)
            })
            .map_err(BoundDispatchError::Record)
    }
    pub(crate) fn drain_widget_actions(
        &self,
        widget: &mut dyn Widget,
        doc: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        let Some(taken) = TakenActionBracket::take(widget) else {
            return Ok(false);
        };
        self.drain_taken_actions(taken, doc)
    }
    /// Record what [`TakenActionBracket::take`] read from a widget, under
    /// the record frame this binding's caller has open.
    pub(crate) fn drain_taken_actions(
        &self,
        taken: TakenActionBracket,
        doc: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        let TakenActionBracket {
            before,
            keep_open,
            metadata,
        } = taken;
        let handle = ACTIVE
            .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
            .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
        {
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            if !frame.operations.is_empty() {
                return Ok(true);
            }
            frame.settle_segment(doc);
            if frame
                .state
                .part()
                .pending
                .as_ref()
                .is_some_and(|pending| !Rc::ptr_eq(&pending.owner, &self.closing))
            {
                // A foreign no-op notification is not mutation evidence. Its
                // acknowledgement must not consume the pending owner's burst
                // or replace the saved metadata.
                return Ok(true);
            }
            // A nested action can already have consumed a bracket still
            // reported by an ancestor LocalDispatch. Only advance the pending
            // boundary; never move it backwards across a recorded action.
            frame.commit_physical(doc);
            let advances = frame.state.part().pending.as_ref().is_none_or(|pending| {
                before == pending.before
                    || doc.get_changes(&before).len() < doc.get_changes(&pending.before).len()
            });
            if advances {
                frame.finish_pending_at(before.clone(), doc);
                frame.state.part().pending = Some(PendingResourceAction::new(self, before));
            }
            if let Some(pending) = frame.state.part().pending.as_mut() {
                pending.metadata = metadata;
            }
        }
        if !keep_open {
            self.finish_action_boundary(doc)?;
        }
        Ok(true)
    }

    /// Close a step the widget keeps open (a word being typed) through the ordinary mutation entry, as
    /// an undo, a save or a merge does, so a restore reviewed afterwards starts at a finished action
    /// boundary.
    pub(crate) fn close_open_step(&self, outer: &mut AutoCommit) -> Result<(), BoundDispatchError> {
        self.with_document(outer, |doc, _| self.finish_action_boundary(doc))?
    }

    /// Outside changes need a new admitted history invocation: a local
    /// stack inversion has no current broker caller for its consequence.
    pub(crate) fn history_uses_compensation(
        &self,
        outer: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        self.with_resource_frame(outer, false, |doc, _| {
            tilo_resource::history::committed_versions(doc)
                .iter()
                .any(|version| {
                    version
                        .context
                        .as_ref()
                        .is_some_and(|context| context.compensation.is_some())
                })
        })
    }

    /// Undo or redo in place, reverting the step's changes.
    pub(crate) fn history_request(
        &self,
        outer: &mut AutoCommit,
        request: tilo_ui_base::widget::undo_access::UndoRequest,
    ) -> Result<Option<crate::state::UndoEntry>, BoundDispatchError> {
        self.with_document(outer, |doc, _| {
            self.finish_action_boundary(doc)?;
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            let entry = match request {
                tilo_ui_base::widget::undo_access::UndoRequest::Undo => {
                    frame
                        .history
                        .undo_in(doc, &self.author, self.instance_id, None, None)
                }
                tilo_ui_base::widget::undo_access::UndoRequest::Redo => {
                    frame
                        .history
                        .redo_in(doc, &self.author, self.instance_id, None, None)
                }
            };
            frame.state.part().pending = Some(PendingResourceAction::new(self, doc.get_heads()));
            Ok(entry)
        })?
    }
    /// Immediate action boundary inside the already admitted owned frame,
    /// as [`Self::close_open_step`] makes one, but only for a step an open
    /// action names: an unnamed change is one the identity service refuses
    /// to sign, so its step stays open for its boundary. Whether a
    /// named step closed. A publication commits the step's words without
    /// ending it, so the step is open whether or not they are committed.
    pub(crate) fn close_named_step(
        &self,
        outer: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        let pending_action = self
            .records
            .with_action_state(&self.checkout.id, |_, _, state| {
                state.part().pending.is_some()
            })
            .map_err(BoundDispatchError::Record)?;
        if !pending_action {
            return Ok(false);
        }
        self.with_document(outer, |doc, _| {
            let named = self.with_active_frame(doc, |frame| frame.step_message().is_some())?;
            if named {
                self.finish_action_boundary(doc)?;
            }
            Ok(named)
        })?
    }

    fn with_active_frame<R>(
        &self,
        doc: &AutoCommit,
        read: impl FnOnce(&ActionFrame) -> R,
    ) -> Result<R, BoundDispatchError> {
        let active = self.check_resource()?;
        if !active.is_some_and(|pointer| std::ptr::eq(pointer, doc)) {
            return Err(BoundDispatchError::Record(ResourceOpenError::Busy));
        }
        let handle = ACTIVE
            .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
            .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
        let owned = handle.borrow();
        let frame = owned
            .as_ref()
            .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
        Ok(read(frame))
    }

    fn finish_action_boundary(&self, doc: &mut AutoCommit) -> Result<(), BoundDispatchError> {
        let active = self.check_resource()?;
        if !active.is_some_and(|pointer| std::ptr::eq(pointer, doc)) {
            return Err(BoundDispatchError::Record(ResourceOpenError::Busy));
        }
        let handle = ACTIVE
            .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
            .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
        let mut owned = handle.borrow_mut();
        let frame = owned
            .as_mut()
            .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
        frame.settle_segment(doc);
        let heads = frame.commit_physical(doc);
        frame.finish_pending_at(heads.clone(), doc);
        frame.state.part().pending = Some(PendingResourceAction::new(self, heads));
        if let Some(active) = frame.mutation.as_mut() {
            active.before = None;
        }
        Ok(())
    }
    pub(crate) fn same_component(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.closing, &other.closing)
            && std::rc::Weak::ptr_eq(&self.registry_owner, &other.registry_owner)
            && self.records.same_owner(&other.records)
            && self.checkout == other.checkout
            && self.instance_id == other.instance_id
    }
    /// Whether entering `other` sets exactly what entering `self` sets: the same open lifetime and wake
    /// route, record, executing device, person and author.
    pub(crate) fn enters_same_scope(&self, other: &Self) -> bool {
        self.lifetime_id == other.lifetime_id
            && Rc::ptr_eq(&self.closing, &other.closing)
            && self.lifetime.same_open_scope(&other.lifetime)
            && self.records.same_owner(&other.records)
            && self.checkout == other.checkout
            && self.device_id == other.device_id
            && self.local_person == other.local_person
            && self.author == other.author
    }
    fn check_resource(&self) -> Result<Option<*const AutoCommit>, BoundDispatchError> {
        if self.closing.get() || !self.lifetime.is_active() {
            return Err(BoundDispatchError::ClosedLifetime);
        }
        ACTIVE.with_borrow(|active| match active {
            Some((records, checkout, document, _)) => {
                if self.records.same_owner(records) && self.checkout == *checkout {
                    Ok(Some(*document))
                } else {
                    Err(BoundDispatchError::CrossResourceReentry)
                }
            }
            None => Ok(None),
        })
    }
    pub(crate) fn enter(&self) -> Result<CallbackScopeGuard, BoundDispatchError> {
        self.check_resource()?;
        let actor = crate::invocation_scope::current_invoker()
            .and_then(|invoker| invoker.acting.members().first().map(|m| (*m).clone()))
            .unwrap_or_else(|| self.author.clone());
        // A component's widgets inherit its binding, so the walks that visit them and the
        // document reads under them enter the same scope again and again: one already open on this
        // thread runs the work inside it instead of opening it once more ([`OPEN_SCOPES`]).
        if OPEN_SCOPES.with_borrow(|open| open.last().is_some_and(|open| open.same(self, &actor))) {
            return Ok(CallbackScopeGuard::held());
        }
        let _profile = tilo_actions::profiling::Phase::start("ui.callback_scope");
        let wake = self
            .lifetime
            .enter_if_open()
            .map_err(|_| BoundDispatchError::ClosedLifetime)?;
        let opened = OPEN_SCOPES.with_borrow_mut(|open| {
            open.push(OpenScope {
                closing: Rc::clone(&self.closing),
                lifetime_id: self.lifetime_id,
                actor: actor.clone(),
                device_id: self.device_id.clone(),
                local_person: self.local_person.clone(),
            });
            open.len() - 1
        });
        Ok(CallbackScopeGuard {
            _wake: Some(wake),
            _device: Some(ExecutingHostDevice::enter(&self.device_id)),
            _person: Some(ExecutingHostPerson::enter(&self.local_person)),
            _scope: Some(ExecutingScope::enter(self.lifetime_id)),
            _actor: Some(tilo_resource::action::ResourceActorScope::enter(actor)),
            opened: Some(opened),
        })
    }
    pub(crate) fn with_document<R>(
        &self,
        outer: &mut AutoCommit,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        self.with_resource_frame(outer, true, callback)
    }
    /// Input reached the component outside its passes (an event, an action, focus): its input revision
    /// moves, so its next pass runs ([`tilo_ui_base::component_inputs`]).
    pub(crate) fn note_input(&self) {
        self.lifetime.note_change();
    }
    /// Lend typed edits without opening a mutation merely because a frame polls.
    /// The executor activates this owner when a typed action actually writes.
    pub(crate) fn with_settlement<R>(
        &self,
        outer: &mut AutoCommit,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        self.with_resource_frame(outer, false, |document, root| {
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            let previous = {
                let mut owned = handle.borrow_mut();
                let frame = owned
                    .as_mut()
                    .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
                frame.mutation.replace(ActiveMutation {
                    binding: self.clone(),
                    before: None,
                    deferred: true,
                })
            };
            let before = tilo_ui_base::doc::settle_mark(document);
            let restore = SettlementReturn {
                document,
                frame: handle,
                previous,
            };
            let result = callback(restore.document, root);
            if tilo_ui_base::doc::settle_mark(restore.document) != before
                && let Some(frame) = restore.frame.borrow_mut().as_mut()
            {
                frame.state.generation = frame
                    .state
                    .generation
                    .and_then(|value| value.checked_add(1));
            }
            Ok(result)
        })?
    }
    /// Run the history verb `run` names on this component's record, decoded by the component's own
    /// registry, so a restore is checked against what the component reads. It runs only through the
    /// record frame the component already owns: it never activates or flushes an unfinished action.
    /// Whether the record changed.
    pub(crate) fn run_history_edit(
        &self,
        root: &mut dyn Widget,
        outer: &mut AutoCommit,
        run: &crate::history_mode::owner::HistoryRun<'_>,
    ) -> Result<bool, String> {
        use crate::history_mode::owner::{HistoryRun, NOT_RESTORED};
        let encoded = run.encoded();
        let mut result = None;
        let mut visit = |binding: &CallbackBinding,
                         _: &mut dyn crate::widget::AppWidget,
                         registry: &tilo_actions::ActionRegistry| {
            result = Some(match registry.decode(&encoded) {
                Ok(action) => binding.run_history_action(outer, action.as_ref(), run),
                Err(_) => Err(HistoryRun::unsupported()),
            });
            None
        };
        visit_components(root, &mut ComponentRequest::new(Some(self), &mut visit))
            .map_err(|_| NOT_RESTORED.to_owned())?;
        result.unwrap_or_else(|| Err(NOT_RESTORED.to_owned()))
    }

    fn run_history_action(
        &self,
        outer: &mut AutoCommit,
        action: &dyn tilo_actions::Action,
        run: &crate::history_mode::owner::HistoryRun<'_>,
    ) -> Result<bool, String> {
        use crate::history_mode::owner::{FINISH_EDIT, NOT_RESTORED};
        self.with_resource_frame(outer, false, |doc, _| {
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(FINISH_EDIT)?;
            let mut owned = handle.borrow_mut();
            let frame = owned.as_mut().ok_or(FINISH_EDIT)?;
            if frame.mutation.is_some() || !frame.operations.is_empty() {
                return Err(FINISH_EDIT.to_owned());
            }
            if frame.state.part().pending.as_ref().is_some_and(|pending| {
                !collab_types::history::same_frontier(&pending.before, &doc.base_heads())
            }) {
                return Err(FINISH_EDIT.to_owned());
            }
            let next_generation = frame
                .state
                .generation
                .and_then(|generation| generation.checked_add(1))
                .ok_or(NOT_RESTORED)?;
            frame.switch_writer(doc, frame.writer_of(self));
            let changed = run.step(doc, &mut frame.history, action, Some(self.instance_id))?;
            if changed {
                frame.state.generation = Some(next_generation);
                frame.reset_operation_baseline(self, doc.get_heads());
            }
            Ok(changed)
        })
        .map_err(|_| NOT_RESTORED.to_owned())?
    }
    pub(crate) fn with_resource_frame<R>(
        &self,
        outer: &mut AutoCommit,
        mutation: bool,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        let active = self.check_resource()?;
        let _scope = self.enter()?;
        if let Some(document) = active {
            if !std::ptr::eq(document, outer) {
                // Reentry must carry the currently borrowed document, never reconstruct
                // a mutable reference from a saved pointer.
                return Err(BoundDispatchError::Record(ResourceOpenError::Busy));
            }
            let frame = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            return if mutation {
                self.mutate_in_frame(outer, &frame, callback)
            } else {
                Ok(callback(outer, &ROOT))
            };
        }
        let _phase = tilo_actions::profiling::Phase::start("ui.record_frame");
        self.records
            .with_action_state(&self.checkout.id, |document, history, state| {
                let frame = Rc::new(RefCell::new(Some(ActionFrame {
                    history: std::mem::take(history),
                    state: std::mem::take(state),
                    mutation: None,
                    operations: Vec::new(),
                    writer: document.get_actor().clone(),
                })));
                let _restore = RestoreFrame {
                    history,
                    state,
                    frame: frame.clone(),
                };
                ACTIVE.with_borrow_mut(|active| {
                    *active = Some((
                        self.records.clone(),
                        self.checkout.clone(),
                        document,
                        Some(frame.clone()),
                    ));
                });
                let _active = ActiveEntry;
                let result = if mutation {
                    self.mutate_in_frame(document, &frame, callback)
                } else {
                    Ok(callback(document, &ROOT))
                };
                if let Some(frame) = frame.borrow().as_ref() {
                    frame.switch_writer(document, frame.writer.clone());
                }
                result
            })
            .map_err(BoundDispatchError::Record)?
    }
    fn mutate_in_frame<R>(
        &self,
        doc: &mut AutoCommit,
        handle: &FrameHandle,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        let previous = {
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(ResourceOpenError::Busy))?;
            let previous = frame.mutation.as_ref().map(|m| m.binding.clone());
            frame.state.generation = frame
                .state
                .generation
                .and_then(|value| value.checked_add(1));
            if previous
                .as_ref()
                .is_some_and(|binding| Rc::ptr_eq(&binding.closing, &self.closing))
            {
                drop(owned);
                return Ok(callback(doc, &ROOT));
            }
            frame.activate(self, doc);
            previous
        };
        let restore = MutationReturn {
            doc,
            frame: handle.clone(),
            previous,
        };
        Ok(callback(restore.doc, &ROOT))
    }
    pub(crate) fn read_document<R>(
        &self,
        outer: &AutoCommit,
        callback: impl FnOnce(&AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        let active = self.check_resource()?;
        let _scope = self.enter()?;
        if let Some(document) = active {
            if !std::ptr::eq(document, outer) {
                return Err(BoundDispatchError::Record(ResourceOpenError::Busy));
            }
            return Ok(callback(outer, &ROOT));
        }
        self.records
            .read_checked(&self.checkout.id, |document| {
                ACTIVE.with_borrow_mut(|active| {
                    *active = Some((self.records.clone(), self.checkout.clone(), document, None));
                });
                let _active = ActiveEntry;
                callback(document, &ROOT)
            })
            .map_err(BoundDispatchError::Record)
    }
}

/// Record owners outside a component (person records, routed records) keep their real record ownership;
/// they do not impersonate a canonical component. The record is taken as it is stored: no app's
/// types read it.
#[cfg(feature = "model")]
pub(crate) fn run_record_history_edit(
    store: &ResourceStore,
    checkout: &DocumentCheckout,
    run: &crate::history_mode::owner::HistoryRun<'_>,
) -> Result<bool, String> {
    use crate::history_mode::owner::{FINISH_EDIT, NOT_RESTORED};
    store
        .open_existing_resource_checked(checkout)
        .map_err(|_| NOT_RESTORED.to_owned())?;
    let action = crate::history_source::DocumentHistoryEdit::new(run.edit.clone());
    store
        .with_action_state(&checkout.id, |doc, history, state| {
            if state.part().pending.as_ref().is_some_and(|pending| {
                !collab_types::history::same_frontier(&pending.before, &doc.base_heads())
            }) {
                return Err(FINISH_EDIT.to_owned());
            }
            let next = state
                .generation
                .and_then(|value| value.checked_add(1))
                .ok_or(NOT_RESTORED)?;
            let changed = run.step(doc, history, &action, None)?;
            if changed {
                state.generation = Some(next);
                state.part().pending = None;
                state.part().statuses.clear();
            }
            Ok(changed)
        })
        .map_err(|_| NOT_RESTORED.to_owned())?
}
/// A read-only invalidation stamp. It contains no document text and confers no
/// write authority; an unavailable stamp must invalidate deferred insertion.
#[cfg(feature = "model")]
#[derive(Clone, PartialEq, Eq)]
pub struct DocumentEditStamp {
    // Exact owner lifetime and undo/redo revision, never callback admission.
    owner: Option<(uuid::Uuid, u64)>,
    frontier: Vec<ChangeHash>,
    pending: usize,
}
#[cfg(feature = "model")]
impl DocumentEditStamp {
    /// Bind an embedded field's private equality check to its record and node.
    #[cfg(feature = "native")]
    pub(crate) fn bind_fingerprint(
        &self,
        node: NodeId,
        field: [u8; 32],
        key: &[u8; 32],
    ) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_keyed(key);
        hash.update(b"tilo.ime.embedded-edit.v1");
        hash.update(&u64::from(node).to_le_bytes());
        if let Some((owner, history_revision)) = self.owner {
            hash.update(owner.as_bytes());
            hash.update(&history_revision.to_le_bytes());
        }
        hash.update(&self.pending.to_le_bytes());
        for head in &self.frontier {
            hash.update(&head.0);
        }
        hash.update(&field);
        *hash.finalize().as_bytes()
    }

    pub(crate) fn for_owned_document(
        document: &AutoCommit,
        lifetime: uuid::Uuid,
        history_revision: u64,
    ) -> Self {
        Self {
            owner: Some((lifetime, history_revision)),
            frontier: document.base_heads(),
            pending: document.pending_ops(),
        }
    }

    /// Snapshot an unbound host document without committing its open action.
    pub fn for_document(document: &AutoCommit) -> Self {
        Self {
            owner: None,
            frontier: document.base_heads(),
            pending: document.pending_ops(),
        }
    }
}

#[cfg(feature = "model")]
#[cfg(test)]
#[path = "bound_dispatch/bound_action_address_tests.rs"]
mod bound_action_address_tests;

/// Whether a document cannot take a peer's changes yet: an unfinished edit, or a restore over a
/// different frontier.
#[cfg(feature = "model")]
pub(crate) fn defers_remote(store: &ResourceStore, checkout: &DocumentCheckout) -> bool {
    store
        .with_action_state(&checkout.id, |doc, _, state| {
            defers_remote_state(doc, state)
        })
        .unwrap_or(false)
}

#[cfg(feature = "model")]
fn defers_remote_state(doc: &AutoCommit, state: &crate::resource::ResourceActions) -> bool {
    doc.pending_ops() != 0
        || state.pending().is_some_and(|pending| {
            !collab_types::history::same_frontier(&pending.before, &doc.base_heads())
        })
}

#[cfg(feature = "model")]
pub(crate) fn merge_remote_owned_document(
    store: &ResourceStore,
    checkout: &DocumentCheckout,
    mut incoming: tilo_ui_base::collaboration::Incoming<'_>,
) -> Result<crate::history_mode::routes::MergedRemote, crate::history_mode::routes::RemoteApplyError>
{
    use crate::history_mode::routes::{MergedRemote, RemoteApplyError};
    store
        .open_existing_resource_checked(checkout)
        .map_err(|error| RemoteApplyError::Unavailable(error.to_string()))?;
    let merged = store
        .with_action_state(&checkout.id, |doc, history, state| {
            if defers_remote_state(doc, state) {
                return Ok(MergedRemote::deferred());
            }
            let next = state
                .generation
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    RemoteApplyError::Unavailable("document generation exhausted".into())
                })?;
            let mut candidate = doc.clone();
            let before = doc.base_heads();
            let changed =
                crate::history_mode::routes::merge_committed(&mut candidate, incoming.reborrow())?;
            let mut observed = Vec::new();
            if changed {
                observed = candidate
                    .get_changes(&before)
                    .into_iter()
                    .map(|change| change.hash())
                    .collect::<Vec<_>>();
                *doc = candidate;
                state.part().pending = None;
                history.rebase(doc);
                state.part().statuses.clear();
                state.generation = Some(next);
            }
            if let tilo_ui_base::collaboration::Incoming::Changes(bytes) = incoming {
                // A change whose history this replica lacks waits for it, but
                // this route delivered it, so it is never sent back.
                observed.extend(crate::state::changes_beyond(doc, bytes));
            }
            Ok(MergedRemote {
                changed: Some(changed),
                observed,
            })
        })
        .map_err(|error| RemoteApplyError::Unavailable(error.to_string()))??;
    if merged.changed == Some(true) {
        store.peer_change_merged(&checkout.id);
    }
    Ok(merged)
}

/// Deliver a semantic (accessibility) action to one admitted node, refused
/// while History has its live content inert.
/// # Safety
/// Same stable, exclusive tree contract as `invoke_bound_node`.
pub(crate) unsafe fn invoke_bound_semantic_node<R>(
    bindings: &BoundNodeMap,
    nodes: &std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    node: taffy::NodeId,
    outer: &mut crate::state::State,
    callback: impl FnOnce(&mut dyn Widget, &mut automerge::AutoCommit, &automerge::ObjId) -> R,
) -> Result<R, String> {
    let admitted = bindings
        .admitted(nodes, node)
        .map_err(|error| format!("{error:?}"))?;
    if admitted.inactive {
        return Err("This live control is hidden by History.".into());
    }
    unsafe { invoke_bound_node(bindings, nodes, node, &mut outer.doc, &outer.root, callback) }
        .map_err(|error| format!("{error:?}"))
}

#[cfg(feature = "model")]
#[cfg(test)]
#[path = "bound_dispatch/document_action_ack_tests.rs"]
mod document_action_ack_tests;

#[cfg(feature = "model")]
#[cfg(test)]
#[path = "bound_dispatch/author_writer_tests.rs"]
mod author_writer_tests;

#[cfg(feature = "model")]
#[cfg(test)]
#[path = "bound_dispatch/device_actor_tests.rs"]
mod device_actor_tests;

#[cfg(feature = "model")]
#[cfg(test)]
#[path = "bound_dispatch/native_text_tests.rs"]
mod native_text_tests;
