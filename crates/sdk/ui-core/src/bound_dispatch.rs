//! Lifetime and canonical-record routing shared by retained and raw widget dispatch.
pub(crate) mod legacy_document;

mod deferred_intent;
use deferred_intent::validate_deferred_arguments;
pub(crate) use deferred_intent::{DeferredIntent, PreparedActionIntent};

use crate::permissions::{PermissionContext, UserId};
use crate::record::{RecordCheckout, RecordOpenError, RecordStore};
use crate::wake::{WakeHandle, WakeScopeGuard};
use crate::widget::Widget;
use automerge::{AutoCommit, ChangeHash, ObjId, ROOT, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{NodeId, TaffyTree};

thread_local! {
    static PHYSICAL_TEXT: Cell<bool> = const { Cell::new(false) };
    static DRAINED_PHYSICAL_TEXT: Cell<bool> = const { Cell::new(false) };
    static NATIVE_TEXT_EXECUTION: Cell<bool> = const { Cell::new(false) };
}

struct PhysicalTextScope(bool);
impl Drop for PhysicalTextScope {
    fn drop(&mut self) {
        PHYSICAL_TEXT.with(|flag| flag.set(self.0));
    }
}

/// Private producer provenance, copied into the exact owned queue entry.
/// It confers no execution authority and is never decoded from a request.
pub(crate) fn with_physical_text<T>(physical: bool, run: impl FnOnce() -> T) -> T {
    let _scope = PhysicalTextScope(PHYSICAL_TEXT.with(|flag| flag.replace(physical)));
    run()
}

pub(crate) fn physical_text() -> bool {
    PHYSICAL_TEXT.with(Cell::get)
}

pub(crate) fn note_drained_physical_text(physical: bool) {
    DRAINED_PHYSICAL_TEXT.with(|flag| flag.set(physical));
}

struct DrainTextScope(bool);
impl Drop for DrainTextScope {
    fn drop(&mut self) {
        DRAINED_PHYSICAL_TEXT.with(|flag| flag.set(self.0));
    }
}

fn capture_physical_drain<T>(run: impl FnOnce() -> T) -> (T, bool) {
    let _scope = DrainTextScope(DRAINED_PHYSICAL_TEXT.with(|flag| flag.replace(false)));
    let result = run();
    (result, DRAINED_PHYSICAL_TEXT.with(Cell::get))
}

struct NativeTextScope(bool);
impl Drop for NativeTextScope {
    fn drop(&mut self) {
        NATIVE_TEXT_EXECUTION.with(|flag| flag.set(self.0));
    }
}

fn with_native_text<T>(physical: bool, run: impl FnOnce() -> T) -> T {
    let _scope = NativeTextScope(NATIVE_TEXT_EXECUTION.with(|flag| flag.replace(physical)));
    run()
}

/// Runtime destination; the domain address remains independent of UI instances.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundActionAddress {
    pub instance_id: crate::application::InstanceId,
    pub address: tilo_actions::ActionAddress,
}

#[derive(Debug)]
pub(crate) enum BoundActionError {
    Door(tilo_actions::DoorError),
    Dispatch(BoundDispatchError),
    AmbiguousTarget {
        address: tilo_actions::ActionAddress,
    },
}
impl BoundActionError {
    pub(crate) fn into_door(self) -> tilo_actions::DoorError {
        match self {
            Self::Door(error) => error,
            error => tilo_actions::DoorError::Unavailable {
                code: error.code().to_owned(),
                reason: error.to_string(),
            },
        }
    }
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Door(error) => error.code(),
            Self::Dispatch(BoundDispatchError::CrossRecordReentry) => "cross_record_reentry",
            Self::Dispatch(BoundDispatchError::ClosedLifetime) => "closed_lifetime",
            Self::Dispatch(_) => "record_unavailable",
            Self::AmbiguousTarget { .. } => "ambiguous_action_target",
        }
    }
}
impl std::fmt::Display for BoundActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Door(error) => error.fmt(f),
            Self::Dispatch(error) => write!(f, "component dispatch refused: {error:?}"),
            Self::AmbiguousTarget { address } => {
                write!(
                    f,
                    "more than one authorized component accepts {}",
                    address.encode()
                )
            }
        }
    }
}
impl std::error::Error for BoundActionError {}
/// The unchanged domain enum groups Unavailable failures. UI routing keeps its
/// additional stable codes when serializing an external failure envelope.
pub(crate) fn door_error_code(error: &tilo_actions::DoorError) -> &str {
    if let tilo_actions::DoorError::Unavailable { code, .. } = error
        && matches!(
            code.as_str(),
            "cross_record_reentry"
                | "ambiguous_action_target"
                | "bound_broker_route_unavailable"
                | "undeclared_document_mutation"
        )
    {
        return code;
    }
    error.code()
}
impl From<tilo_actions::DoorError> for BoundActionError {
    fn from(error: tilo_actions::DoorError) -> Self {
        Self::Door(error)
    }
}
impl From<BoundDispatchError> for BoundActionError {
    fn from(error: BoundDispatchError) -> Self {
        Self::Dispatch(error)
    }
}

/// A UI selection retains the captured lifetime and registry owner, never just
/// the reusable numeric instance or a domain resource ID.
#[derive(Clone)]
pub(crate) struct BoundActionTarget {
    pub(crate) physical_text: bool,
    pub(crate) binding: CallbackBinding,
    pub(crate) address: BoundActionAddress,
}
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

pub(crate) fn has_matching_broker_component(
    root: &dyn Widget,
    route: Option<&crate::collaboration::PrimaryRouteWitness>,
) -> bool {
    if root
        .callback_binding()
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

/// Refresh every component control's availability from its own resources,
/// plus the doors its host answers for it (`host_doors`): the window's
/// History door wherever History serves it. Opening search is always one.
pub(crate) fn refresh_bound_controls(
    root: &mut dyn Widget,
    snapshots: &[BoundResourceSnapshot],
    host_doors: &std::collections::HashSet<tilo_actions::ActionAddress>,
) -> Result<(), BoundDispatchError> {
    // A component hosted in a tile with History runs that tile's History door.
    let mut tile_doors = Vec::new();
    crate::history_mode::capability::visit_framework_history(&*root, &mut |history, _| {
        if let Some(owner) = history.enclosing_owner() {
            tile_doors.push((owner.clone(), history.door_addresses()));
        }
    });
    let mut visit = |binding: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        let owner = crate::history_mode::HistoryOwner::component(binding.clone());
        let tile_doors = tile_doors
            .iter()
            .filter(|(enclosing, _)| enclosing.same_owner(&owner))
            .flat_map(|(_, doors)| doors.iter().cloned());
        let resources = snapshots
            .iter()
            .filter(|snapshot| snapshot.binding.same_component(binding))
            .flat_map(|snapshot| snapshot.resources.iter().cloned())
            .filter_map(|resource| filter_resource(binding, resource))
            .collect::<Vec<_>>();
        let mut allowed: std::collections::HashSet<_> = tilo_actions::door::survey(&resources)
            .into_iter()
            .filter(|descriptor| descriptor.contract.availability.is_available())
            .map(|descriptor| descriptor.address)
            .collect();
        // A component's host — its window or the shell — opens search.
        allowed.insert(crate::search::search_address());
        allowed.extend(host_doors.iter().cloned());
        allowed.extend(tile_doors);
        crate::widget::controls::refresh_control_action_availability(widget, &allowed);
        None
    };
    root.visit_components(&mut ComponentRequest {
        target: None,
        found: false,
        visit: &mut visit,
    })
}

/// A component's control intent, with the effect invocation it continues
/// when the component's effect handler or completion queued it.
pub(crate) type PendingComponentControl = (
    BoundActionTarget,
    tilo_actions::ControlInvocation,
    Option<tilo_actions::InvocationId>,
);

pub(crate) struct DrainedControlInvocations {
    pub(crate) pending: Vec<PendingComponentControl>,
    pub(crate) error: Option<BoundDispatchError>,
}

pub(crate) fn drain_control_invocations(root: &mut dyn Widget) -> DrainedControlInvocations {
    let batch = capture_control_invocations(root);
    DrainedControlInvocations {
        pending: batch
            .controls
            .into_iter()
            .zip(batch.continuing)
            .map(|((target, intent), on_behalf_of)| (target, intent, on_behalf_of))
            .collect(),
        error: batch.failures.into_iter().next(),
    }
}

#[derive(Default)]
pub(crate) struct BoundControlBatch {
    pub(crate) visited: Vec<CallbackBinding>,
    pub(crate) controls: Vec<(BoundActionTarget, tilo_actions::ControlInvocation)>,
    /// For each of `controls`, in order, the effect invocation it continues.
    pub(crate) continuing: Vec<Option<tilo_actions::InvocationId>>,
    pub(crate) failures: Vec<BoundDispatchError>,
}

/// Move every reachable component control while preserving controls captured
/// before a later owner traversal failure.
pub(crate) fn capture_control_invocations(root: &mut dyn Widget) -> BoundControlBatch {
    visit_control_queues(root, true)
}

/// Retry custody traversal without opening any live queue after History entry.
pub(crate) fn inspect_control_owners(root: &mut dyn Widget) -> BoundControlBatch {
    visit_control_queues(root, false)
}

/// Snapshot the finite binding queues through immutable owner metadata. These
/// descriptors are sealed before the fallible mutable traversal starts. They
/// are never matched to later emissions by value or reinserted in a live queue.
pub(crate) struct ControlQueueSnapshot {
    /// Retain even empty queues so suspended output can be discarded without
    /// successfully traversing their mutable owners again.
    pub(crate) owners: Vec<CallbackBinding>,
    pub(crate) controls: Vec<(BoundActionTarget, tilo_actions::ControlInvocation)>,
}

pub(crate) fn snapshot_control_queues(root: &dyn Widget) -> ControlQueueSnapshot {
    fn visit(
        widget: &dyn Widget,
        seen: &mut Vec<CallbackBinding>,
        output: &mut Vec<(BoundActionTarget, tilo_actions::ControlInvocation)>,
    ) {
        if let Some(binding) = widget.callback_binding()
            && !seen.iter().any(|prior| prior.same_component(&binding))
        {
            seen.push(binding.clone());
            for invocation in binding.queued_controls.borrow().iter() {
                output.push((
                    BoundActionTarget {
                        physical_text: false,
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

fn visit_control_queues(root: &mut dyn Widget, drain: bool) -> BoundControlBatch {
    let mut invocations = Vec::new();
    let mut continuing = Vec::new();
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
            invocations.push((
                BoundActionTarget {
                    physical_text: false,
                    binding: binding.clone(),
                    address: BoundActionAddress {
                        instance_id: binding.instance_id,
                        address: intent.address.clone(),
                    },
                },
                intent,
            ));
        }
        while let (Some((intent, on_behalf_of)), physical_text) = capture_physical_drain(|| {
            crate::widget::controls::take_attributed_control_invocation(widget)
        }) {
            continuing.push(on_behalf_of);
            invocations.push((
                BoundActionTarget {
                    physical_text,
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
    let failure = root
        .visit_components(&mut ComponentRequest {
            target: None,
            found: false,
            visit: &mut visit,
        })
        .err();
    BoundControlBatch {
        visited,
        controls: invocations,
        continuing,
        failures: failure.into_iter().collect(),
    }
}

/// Deliver feedback only to the component that emitted the local intent.
/// Domain resource addresses can be shared by multiple live instances.
pub(crate) fn notify_control_invocation_result(
    root: &mut dyn Widget,
    target: &BoundActionTarget,
    result: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
) -> Result<(), BoundDispatchError> {
    let mut visit = |_: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        crate::widget::controls::on_control_invocation_result(
            widget,
            &target.address.address,
            result,
        );
        None
    };
    root.visit_components(&mut ComponentRequest {
        target: Some(&target.binding),
        found: false,
        visit: &mut visit,
    })
}

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
            widget.poll_resource_operation(invocation, cancel, doc, path)
        }));
        result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|update| update.as_ref())
            .filter(|update| update.is_terminal())
            .map(|_| (invocation, false))
    };
    root.visit_components(&mut ComponentRequest {
        target: Some(&target.binding),
        found: false,
        visit: &mut delivery,
    })?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

#[derive(Clone)]
pub(crate) struct BoundResourceSnapshot {
    pub(crate) binding: CallbackBinding,
    pub(crate) resources: Vec<tilo_actions::Resource>,
}

/// Reserved transport namespace. Domain resources never contain this rewrite.
pub(crate) const PUBLICATION_PREFIX: &str = "tilo-ui-bound:v1:";
const MAX_PUBLICATION_ALIAS: usize = 4096;
pub(crate) struct BoundPublication {
    host: uuid::Uuid,
    owners: RefCell<HashMap<uuid::Uuid, CallbackBinding>>,
}
impl Default for BoundPublication {
    fn default() -> Self {
        Self {
            host: uuid::Uuid::new_v4(),
            owners: RefCell::new(HashMap::new()),
        }
    }
}
impl BoundPublication {
    pub(crate) fn clear(&self) {
        self.owners.borrow_mut().clear();
    }
    pub(crate) fn alias(
        &self,
        binding: &CallbackBinding,
        resource: &str,
    ) -> Result<String, BoundActionError> {
        let _entry = binding.enter()?;
        let mut owners = self.owners.borrow_mut();
        owners.retain(|_, binding| !binding.closing.get() && binding.lifetime.is_active());
        if owners
            .get(&binding.lifetime_id)
            .is_some_and(|old| !old.same_component(binding))
        {
            return Err(tilo_actions::DoorError::Unavailable {
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
            return Err(tilo_actions::DoorError::InvalidArguments(
                "resource transport alias exceeds its size bound".into(),
            )
            .into());
        }
        let mut alias = prefix;
        alias.push_str(&hex::encode(resource));
        owners.insert(binding.lifetime_id, binding.clone());
        Ok(alias)
    }
    pub(crate) fn target(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Result<Option<BoundActionTarget>, BoundActionError> {
        if !address.resource.starts_with(PUBLICATION_PREFIX) {
            return Ok(None);
        }
        let unknown = || BoundActionError::Door(tilo_actions::DoorError::Unknown(address.clone()));
        if address.resource.len() > MAX_PUBLICATION_ALIAS {
            return Err(unknown());
        }
        let mut pieces = address
            .resource
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
        let _entry = binding.enter()?;
        Ok(Some(BoundActionTarget {
            physical_text: false,
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
        mut resource: tilo_actions::Resource,
    ) -> Result<tilo_actions::Resource, BoundActionError> {
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

pub(crate) fn validate_bound_resources(
    root: &mut dyn Widget,
    binding: &CallbackBinding,
    resources: &[tilo_actions::Resource],
) -> Result<bool, BoundDispatchError> {
    Ok(bound_contract_violations(root, binding, resources)?.is_some_and(|found| found.is_empty()))
}

/// Every contract violation of one component's surface, checked against the
/// component's own registry and operation handlers. `None` when the binding
/// names no mounted component.
pub(crate) fn bound_contract_violations(
    root: &mut dyn Widget,
    binding: &CallbackBinding,
    resources: &[tilo_actions::Resource],
) -> Result<Option<Vec<tilo_actions::ContractViolation>>, BoundDispatchError> {
    let _entry = binding.enter()?;
    let mut violations = None;
    let mut visit = |_: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     registry: &tilo_actions::ActionRegistry| {
        violations = Some(tilo_actions::validate_surface(
            resources,
            registry,
            &widget.resource_operation_handlers(),
        ));
        None
    };
    root.visit_components(&mut ComponentRequest {
        target: Some(binding),
        found: false,
        visit: &mut visit,
    })?;
    Ok(violations)
}

/// Collect each retained component once even when an ordinary container does
/// not forward `search_providers`. Legacy providers keep their root-owned route.
pub(crate) fn search_providers(root: &dyn Widget) -> Vec<Box<dyn crate::search::SearchProvider>> {
    fn visit(
        widget: &dyn Widget,
        seen: &mut std::collections::HashSet<uuid::Uuid>,
        providers: &mut Vec<Box<dyn crate::search::SearchProvider>>,
    ) {
        let binding = widget.callback_binding();
        let Ok(_entry) = binding.as_ref().map(CallbackBinding::enter).transpose() else {
            return;
        };
        if let Some(binding) = &binding
            && seen.insert(binding.lifetime_id)
        {
            providers.extend(crate::search::capability::search_providers(widget));
        }
        widget.for_each_child(&mut |child| visit(child, seen, providers));
    }
    let mut providers = crate::search::capability::search_providers(root)
        .into_iter()
        .filter(|provider| provider.callback_binding().is_none())
        .collect::<Vec<_>>();
    visit(root, &mut std::collections::HashSet::new(), &mut providers);
    providers
}

fn provider_resource(
    widget: &dyn Widget,
    binding: &CallbackBinding,
    address: &tilo_actions::ActionAddress,
) -> Option<tilo_actions::Resource> {
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

pub(crate) fn resolve_component_resource(
    root: &mut dyn Widget,
    doc: &AutoCommit,
    binding: &CallbackBinding,
    resource: &str,
    action: &str,
) -> Result<Option<tilo_actions::Resource>, BoundDispatchError> {
    let _entry = binding.enter()?;
    let mut result = None;
    let mut visit = |current: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        result = Some(current.read_document(doc, |doc, _| {
            resolve_unbound_resource(widget, doc, resource)
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
    root.visit_components(&mut ComponentRequest {
        target: Some(binding),
        found: false,
        visit: &mut visit,
    })?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

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
        let boundary = widget.callback_binding();
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
                        crate::resources::action_directory_visibility(widget, doc, id)
                    })
                    .ok()
                    .flatten()
            } else {
                crate::resources::action_directory_visibility(widget, doc, id)
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

/// Opaque legacy navigation remains scoped to the provider that produced it.
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
        .check(&crate::permissions::PermissionScope::app(
            &binding.app_id,
            collab_types::caps::Verb::Read,
        ))
    {
        return Ok(());
    }
    let mut result = None;
    let mut visit = |current: &CallbackBinding,
                     widget: &mut dyn crate::widget::AppWidget,
                     _: &tilo_actions::ActionRegistry| {
        result = Some(
            current
                .with_document(doc, |doc, path| {
                    current.finish_action_boundary(doc)?;
                    crate::search::capability::activate_search_result(
                        widget,
                        id,
                        &mut crate::widget::Edits::lend(doc),
                        path,
                    );
                    current.drain_owned_tree(widget, doc)?;
                    current.finish_action_boundary(doc)
                })
                .and_then(std::convert::identity),
        );
        None
    };
    root.visit_components(&mut ComponentRequest {
        target: Some(binding),
        found: false,
        visit: &mut visit,
    })?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BoundDirectoryWitness {
    owner: uuid::Uuid,
    pub(crate) generation: Option<u64>,
    permissions: crate::permissions::DirectoryPermissionWitness,
    revisions: Vec<Option<String>>,
    domains: Vec<crate::widget::ActionDirectoryInvocationDomain>,
}
impl BoundDirectoryWitness {
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
pub(crate) fn directory_witness(
    root: &dyn Widget,
    doc: &AutoCommit,
) -> Result<Vec<BoundDirectoryWitness>, String> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<CallbackBinding>,
        witnesses: &mut Vec<BoundDirectoryWitness>,
    ) -> Result<(), String> {
        let boundary = widget.callback_binding();
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
            let permissions = binding.permissions.borrow().directory_witness()?;
            let index = witnesses
                .iter()
                .position(|witness| witness.owner == binding.lifetime_id)
                .unwrap_or_else(|| {
                    witnesses.push(BoundDirectoryWitness {
                        owner: binding.lifetime_id,
                        generation: None,
                        permissions: permissions.clone(),
                        revisions: Vec::new(),
                        domains: Vec::new(),
                    });
                    witnesses.len().saturating_sub(1)
                });
            let witness = witnesses
                .get_mut(index)
                .ok_or_else(|| "directory owner disappeared".to_string())?;
            if witness.permissions != permissions {
                return Err("directory owner policy changed during traversal".into());
            }
            witness.generation = binding.generation().map_err(|error| format!("{error:?}"))?;
            if boundary.is_some() {
                witness.domains.push(
                    binding
                        .read_document(doc, |doc, _| {
                            crate::resources::action_directory_invocation_domain(widget, doc)
                        })
                        .map_err(|error| format!("{error:?}"))?,
                );
            }
            if boundary.is_none() {
                witness.revisions.push(
                    binding
                        .read_document(doc, |doc, _| {
                            crate::resources::action_directory_revision(widget, doc)
                        })
                        .map_err(|error| format!("{error:?}"))?,
                );
            }
        }
        let mut result = Ok(());
        widget.for_each_child(&mut |child| {
            if result.is_ok() {
                result = visit(child, doc, binding.clone(), witnesses);
            }
        });
        result
    }
    let mut witnesses = Vec::new();
    visit(root, doc, None, &mut witnesses)?;
    Ok(witnesses)
}

#[derive(Default)]
pub(crate) struct BoundResourceCache(RefCell<Vec<CachedBoundResources>>);

/// One bound owner's admission generation and record frontier, read on each
/// side of an input dispatch. Only a native surface's input path reads it.
#[cfg(feature = "native")]
pub(crate) struct BoundOwnerMark {
    binding: CallbackBinding,
    generation: Option<u64>,
    document: Option<(Vec<automerge::ChangeHash>, usize)>,
}

/// Every live bound owner under `root`, once each, in tree order.
#[cfg(feature = "native")]
pub(crate) fn bound_owner_marks(root: &dyn Widget, outer: &AutoCommit) -> Vec<BoundOwnerMark> {
    fn visit(widget: &dyn Widget, outer: &AutoCommit, marks: &mut Vec<BoundOwnerMark>) {
        if let Some(binding) = widget.callback_binding()
            && !binding.closing.get()
            && binding.lifetime.is_active()
            && !marks
                .iter()
                .any(|mark| mark.binding.same_component(&binding))
        {
            let entry = binding.enter();
            let generation = binding.generation().ok().flatten();
            let document = binding
                .read_document(outer, |doc, _| crate::widget::settle_mark(doc))
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

#[cfg(feature = "native")]
impl BoundResourceCache {
    /// An input dispatch lends each owner its record mutably, which advances
    /// the owner's admission generation whether or not anything was written.
    /// When the owners' records stand exactly where they did `before` the
    /// dispatch, carry each snapshot that was current then to the generation
    /// now. Returns `false`, carrying nothing, when any owner's record moved
    /// or the owners changed: the caller must republish.
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
}
struct CachedBoundResources {
    snapshot: BoundResourceSnapshot,
    generation: u64,
    local_generation: u64,
    permissions: crate::permissions::DirectoryPermissionWitness,
    revisions: Vec<Option<String>>,
}

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
        resources: &mut Vec<tilo_actions::Resource>,
        revisions: &mut Vec<Option<String>>,
        declarations: bool,
    ) {
        let boundary = widget.callback_binding();
        if boundary
            .as_ref()
            .is_some_and(|binding| !binding.same_component(owner))
        {
            return;
        }
        if boundary.is_none() {
            revisions.push(crate::resources::action_directory_revision(widget, doc));
            if declarations {
                resources.extend(crate::resources::resources(widget, doc));
            }
        }
        widget.for_each_child(&mut |child| {
            owned(child, doc, owner, resources, revisions, declarations);
        });
    }
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        cache: &BoundResourceCache,
        local_generation: u64,
        snapshots: &mut Vec<BoundResourceSnapshot>,
    ) -> Result<(), String> {
        let binding = widget.callback_binding();
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
            let permissions = match binding.permissions.borrow().directory_witness() {
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
                    owned(widget, doc, binding, &mut Vec::new(), &mut revisions, false);
                    revisions
                })
                .map_err(|error| format!("{error:?}"))?;
            let cached = cache
                .0
                .borrow()
                .iter()
                .find(|entry| {
                    entry.snapshot.binding.same_component(binding)
                        && generation == Some(entry.generation)
                        && entry.local_generation == local_generation
                        && entry.permissions == permissions
                        && entry.revisions == revisions
                })
                .map(|entry| entry.snapshot.clone());
            let snapshot = if let Some(cached) = cached {
                cached
            } else {
                let resources = binding
                    .read_document(doc, |doc, _| {
                        let mut resources = Vec::new();
                        owned(widget, doc, binding, &mut resources, &mut Vec::new(), true);
                        resources
                            .into_iter()
                            .filter_map(|resource| filter_resource(binding, resource))
                            .collect()
                    })
                    .map_err(|error| format!("{error:?}"))?;
                // Each row was filtered using current permission checks. Do
                // not cache a mixed projection if time/policy changed while
                // those checks were running.
                if binding.permissions.borrow().directory_witness()? != permissions {
                    return Err("owner policy changed while filtering resources".into());
                }
                let snapshot = BoundResourceSnapshot {
                    binding: binding.clone(),
                    resources,
                };
                let mut entries = cache.0.borrow_mut();
                entries.retain(|entry| {
                    !entry.snapshot.binding.closing.get()
                        && !entry.snapshot.binding.same_component(binding)
                });
                if let Some(generation) = generation {
                    entries.push(CachedBoundResources {
                        snapshot: snapshot.clone(),
                        generation,
                        local_generation,
                        permissions,
                        revisions,
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

/// Action-scoped authority shared by native and headless legacy hosts: the
/// action `action` on the resource `resource_id`.
pub(crate) fn permits_local_action(
    permissions: &PermissionContext,
    app_id: &str,
    resource_id: &str,
    action: &tilo_actions::ActionSpec,
) -> bool {
    permits_local_invocation(
        permissions,
        app_id,
        resource_id,
        action,
        action.contract.permission,
    )
}

/// [`permits_local_action`] for one invocation: what its arguments ask.
pub(crate) fn permits_local_value(
    permissions: &PermissionContext,
    app_id: &str,
    resource_id: &str,
    action: &tilo_actions::ActionSpec,
    typed: Option<&serde_json::Map<String, serde_json::Value>>,
    argument: Option<&str>,
) -> bool {
    let requirement = match typed {
        Some(arguments) => action.permission_for_json(arguments),
        None => action.permission_for_argument(argument),
    };
    permits_local_invocation(permissions, app_id, resource_id, action, requirement)
}

fn permits_local_invocation(
    permissions: &PermissionContext,
    app_id: &str,
    resource_id: &str,
    action: &tilo_actions::ActionSpec,
    requirement: tilo_actions::PermissionRequirement,
) -> bool {
    use crate::permissions::Verb;
    use tilo_actions::PermissionRequirement as Requirement;
    let verb = match requirement {
        Requirement::None => return true,
        Requirement::Read => Verb::Read,
        Requirement::Comment => Verb::Comment,
        Requirement::Write => Verb::Write,
        Requirement::Invite => Verb::Invite,
        Requirement::Admin => Verb::Admin,
    };
    permissions.check(&crate::permissions::PermissionScope::action(
        app_id,
        resource_id,
        &action.id,
        verb,
    ))
}

/// May the component's participant take this one invocation of `action`:
/// what its arguments ask, under the same resource scope the published tree is
/// filtered by.
fn permits_invocation(
    binding: &CallbackBinding,
    resource_id: &str,
    action: &tilo_actions::ActionSpec,
    typed: Option<&serde_json::Map<String, serde_json::Value>>,
    argument: Option<&str>,
) -> bool {
    use crate::permissions::Verb;
    use tilo_actions::PermissionRequirement as Requirement;
    let requirement = match typed {
        Some(arguments) => action.permission_for_json(arguments),
        None => action.permission_for_argument(argument),
    };
    let verb = match requirement {
        Requirement::None => return true,
        Requirement::Read => Verb::Read,
        Requirement::Comment => Verb::Comment,
        Requirement::Write => Verb::Write,
        Requirement::Invite => Verb::Invite,
        Requirement::Admin => Verb::Admin,
    };
    binding
        .permissions
        .borrow()
        .check(&crate::permissions::PermissionScope::action(
            &binding.app_id,
            resource_id,
            &action.id,
            verb,
        ))
}

/// The refusal of an invocation the participant may not take.
pub(crate) fn denied(action: &tilo_actions::ActionSpec) -> tilo_actions::DoorError {
    tilo_actions::DoorError::Unavailable {
        code: "permission_denied".into(),
        reason: format!("you may not {} here", action.label.to_lowercase()),
    }
}

pub(crate) fn filter_resource(
    binding: &CallbackBinding,
    mut resource: tilo_actions::Resource,
) -> Option<tilo_actions::Resource> {
    let resource_id = resource.id.as_str().to_owned();
    resource.actions.retain(|action| {
        use crate::permissions::Verb;
        use tilo_actions::PermissionRequirement as Requirement;
        let verb = match action.contract.permission {
            Requirement::None => return true,
            Requirement::Read => Verb::Read,
            Requirement::Comment => Verb::Comment,
            Requirement::Write => Verb::Write,
            Requirement::Invite => Verb::Invite,
            Requirement::Admin => Verb::Admin,
        };
        binding
            .permissions
            .borrow()
            .check(&crate::permissions::PermissionScope::action(
                &binding.app_id,
                &resource_id,
                &action.id,
                verb,
            ))
    });
    resource.children = resource
        .children
        .into_iter()
        .filter_map(|child| filter_resource(binding, child))
        .collect();
    (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
}

/// Read-only traversal releases each checked document borrow before descending
/// to another component. It never establishes an undo baseline.
pub(crate) fn discover_bound_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    lazy_address: Option<&tilo_actions::ActionAddress>,
) -> Result<Vec<BoundResourceSnapshot>, BoundDispatchError> {
    discover_bound_resources_with_filter(widget, doc, lazy_address, true)
}

fn discover_bound_resources_with_filter(
    widget: &dyn Widget,
    doc: &AutoCommit,
    lazy_address: Option<&tilo_actions::ActionAddress>,
    filtered: bool,
) -> Result<Vec<BoundResourceSnapshot>, BoundDispatchError> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<CallbackBinding>,
        address: Option<&tilo_actions::ActionAddress>,
        snapshots: &mut Vec<BoundResourceSnapshot>,
        filtered: bool,
    ) -> Result<(), BoundDispatchError> {
        let boundary = widget.callback_binding();
        let binding = boundary.clone().or(inherited);
        if let Some(binding) = &binding {
            if binding.closing.get() || !binding.lifetime.is_active() {
                return Ok(());
            }
            if boundary.is_none() {
                let resources = binding.read_document(doc, |doc, _| {
                    // A delivered invocation resolves in its invoker's own tree.
                    let participant = crate::invocation_scope::current_participant();
                    let mut resources =
                        crate::resources::resources_for(widget, participant.as_ref(), doc);
                    if let Some(address) = address
                        && tilo_actions::door::resolve(&resources, address).is_none()
                        && let Some(resource) = crate::resources::resource_for(
                            widget,
                            participant.as_ref(),
                            doc,
                            &address.resource,
                        )
                        .or_else(|| provider_resource(widget, binding, address))
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
                })?;
                if let Some(snapshot) = snapshots
                    .iter_mut()
                    .find(|snapshot| snapshot.binding.same_component(binding))
                {
                    snapshot.resources.extend(resources);
                } else {
                    snapshots.push(BoundResourceSnapshot {
                        binding: binding.clone(),
                        resources,
                    });
                }
            }
        }
        let mut result = Ok(());
        widget.for_each_child(&mut |child| {
            if result.is_ok() {
                result = visit(child, doc, binding.clone(), address, snapshots, filtered);
            }
        });
        result
    }
    let mut snapshots = Vec::new();
    visit(widget, doc, None, lazy_address, &mut snapshots, filtered)?;
    Ok(snapshots)
}

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
            .resources
            .iter()
            .cloned()
            .filter_map(|resource| filter_resource(&snapshot.binding, resource))
            .collect::<Vec<_>>();
        if tilo_actions::door::resolve(&authorized, address).is_none() {
            continue;
        }
        if selected.is_some() {
            return Err(BoundActionError::AmbiguousTarget {
                address: address.clone(),
            });
        }
        selected = Some(BoundActionTarget {
            physical_text: false,
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
    &'a tilo_actions::Resource,
    &'a tilo_actions::ActionSpec,
);

pub(crate) fn find_action_resource<'a>(
    resources: &'a [tilo_actions::Resource],
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource)
        } else {
            find_action_resource(&resource.children, id)
        }
    })
}

pub(crate) fn resolve_resource_invocation<'a>(
    resources: &'a [tilo_actions::Resource],
    address: &tilo_actions::ActionAddress,
    argument: Option<&str>,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    confirmed: bool,
) -> Result<ResolvedResourceInvocation<'a>, tilo_actions::DoorError> {
    if arguments.is_some() && argument.is_some() {
        return Err(tilo_actions::DoorError::InvalidArguments(
            "supply structured arguments or legacy text, not both".into(),
        ));
    }
    let typed = argument
        .and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
        .map(|json| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))
        })
        .transpose()?
        .or_else(|| arguments.cloned());
    let resource = find_action_resource(resources, &address.resource)
        .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == address.action)
        .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
    // Confirmation is decided for these exact arguments: a generic edit
    // needs it only when its value is consequential.
    let confirmation = match typed.as_ref() {
        Some(arguments) => spec.confirmation_for_json(arguments),
        None => spec.confirmation_for_argument(argument),
    };
    if !matches!(confirmation, tilo_actions::ConfirmationPolicy::None) && !confirmed {
        return Err(tilo_actions::DoorError::ConfirmationRequired);
    }
    Ok((typed, resource, spec))
}

pub(crate) fn encode_resource_action(
    resources: &[tilo_actions::Resource],
    address: &tilo_actions::ActionAddress,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    argument: Option<&str>,
) -> Result<tilo_actions::EncodedAction, tilo_actions::DoorError> {
    if let Some(arguments) = arguments {
        tilo_actions::door::encode_json(resources, address, arguments)
    } else {
        tilo_actions::door::encode(resources, address, argument)
    }
}

/// Ordinary confirmation guarantees declared meaning and arguments in the same
/// owner. Hidden referent continuity is additional evidence, only when the
/// provider supplied it at emission; this is deliberately not History custody.
#[derive(Clone, PartialEq)]
struct OrdinaryConfirmationSnapshot {
    resource_id: String,
    resource_kind: String,
    action_id: String,
    label: String,
    description: Option<String>,
    locus: tilo_actions::ExecutionLocus,
    kind: tilo_actions::OperationKind,
    operation_type: Option<String>,
    contract: tilo_actions::ActionContract,
    params: Vec<tilo_actions::ActionParam>,
    input: ControlInputSurface,
    intent: PreparedActionIntent,
    continuation: Option<tilo_actions::ContinuationEvidence>,
}

#[derive(Clone)]
pub(crate) struct OrdinaryConfirmation {
    pub(crate) target: legacy_document::LocalResourceTarget,
    pub(crate) address: tilo_actions::ActionAddress,
    pub(crate) arguments: ControlContinuationArguments,
    domain: crate::control_custody::WeakControlDomain,
    snapshot: OrdinaryConfirmationSnapshot,
    pub(crate) context: tilo_actions::ActionContext,
}

impl OrdinaryConfirmation {
    pub(crate) fn prepare(
        root: &dyn Widget,
        doc: &AutoCommit,
        target: legacy_document::LocalResourceTarget,
        domain: &crate::ControlDomain,
        address: tilo_actions::ActionAddress,
        arguments: ControlContinuationArguments,
    ) -> Result<Self, tilo_actions::DoorError> {
        if domain.closed() {
            return Err(Self::stale());
        }
        let resources = resources_for_control_target(root, doc, &target, &address)?;
        let (resource, spec) = unique_control_spec(&resources, &address)?;
        let context = tilo_actions::ActionContext::for_resource(
            &spec.label,
            resource.id.as_str(),
            &resource.kind,
            &resource.label,
        );
        let snapshot = Self::snapshot(&resources, &address, &arguments)?;
        Ok(Self {
            target,
            address,
            arguments,
            domain: domain.downgrade(),
            snapshot,
            context,
        })
    }
    fn snapshot(
        resources: &[tilo_actions::Resource],
        address: &tilo_actions::ActionAddress,
        arguments: &ControlContinuationArguments,
    ) -> Result<OrdinaryConfirmationSnapshot, tilo_actions::DoorError> {
        let (resource, spec) = unique_control_spec(resources, address)?;
        let (argument, json) = arguments.as_door_arguments();
        let intent = if matches!(spec.input, tilo_actions::ActionInput::Deferred { .. }) {
            validate_deferred_arguments(spec, json, argument)?;
            PreparedActionIntent::Deferred(DeferredIntent::capture(spec)?)
        } else {
            PreparedActionIntent::Encoded(encode_resource_action(
                resources, address, json, argument,
            )?)
        };
        Ok(OrdinaryConfirmationSnapshot {
            resource_id: resource.id.as_str().to_owned(),
            resource_kind: resource.kind.clone(),
            action_id: spec.id.clone(),
            label: spec.label.clone(),
            description: spec.description.clone(),
            locus: spec.locus,
            kind: spec.kind,
            operation_type: spec.operation_type.clone(),
            contract: spec.contract.clone(),
            params: spec.params().to_vec(),
            input: control_input_surface(spec),
            continuation: match &intent {
                PreparedActionIntent::Deferred(intent) => Some(intent.draft().clone()),
                PreparedActionIntent::Encoded(_) => spec.continuation_identity(
                    tilo_actions::ContinuationQuery::Exact(arguments.query()),
                ),
            },
            intent,
        })
    }
    pub(crate) fn validate_owner(
        &self,
        domain: &crate::ControlDomain,
    ) -> Result<(), tilo_actions::DoorError> {
        let original = self.domain.upgrade().ok_or_else(Self::stale)?;
        if original.closed() || domain.closed() || !original.same_owner_incarnation(domain) {
            return Err(Self::stale());
        }
        Ok(())
    }
    pub(crate) fn validate(
        &self,
        root: &dyn Widget,
        doc: &AutoCommit,
        domain: &crate::ControlDomain,
    ) -> Result<(), tilo_actions::DoorError> {
        self.validate_owner(domain)?;
        let resources = resources_for_control_target(root, doc, &self.target, &self.address)?;
        let mut current = Self::snapshot(&resources, &self.address, &self.arguments)?;
        // Absence at emission is not retroactively replaced with provider proof.
        if self.snapshot.continuation.is_none() {
            current.continuation = None;
        }
        if current != self.snapshot {
            return Err(Self::stale());
        }
        Ok(())
    }
    fn stale() -> tilo_actions::DoorError {
        tilo_actions::DoorError::Unavailable {
            code: "stale_ordinary_confirmation".into(),
            reason: "The original confirmation request is no longer valid".into(),
        }
    }
    pub(crate) fn intent(&self) -> &PreparedActionIntent {
        &self.snapshot.intent
    }
    pub(crate) fn kind(&self) -> tilo_actions::OperationKind {
        self.snapshot.kind
    }
    pub(crate) fn confirmation(&self) -> &tilo_actions::ConfirmationPolicy {
        &self.snapshot.contract.confirmation
    }
}

fn control_input_surface(spec: &tilo_actions::ActionSpec) -> ControlInputSurface {
    match &spec.input {
        tilo_actions::ActionInput::Immediate(_) => ControlInputSurface::Immediate,
        tilo_actions::ActionInput::Deferred { type_id, .. } => ControlInputSurface::Deferred {
            type_id: type_id.clone(),
        },
        tilo_actions::ActionInput::Prompted {
            prompt,
            placeholder,
            collects,
            ..
        } => ControlInputSurface::Prompted {
            prompt: prompt.clone(),
            placeholder: placeholder.clone(),
            collects: *collects,
        },
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct ControlWitness {
    identity: ControlIdentity,
    intent: PreparedActionIntent,
    confirmation: tilo_actions::ConfirmationPolicy,
}

#[derive(Clone, PartialEq, Eq)]
enum ControlInputSurface {
    Immediate,
    Deferred {
        type_id: String,
    },
    Prompted {
        prompt: String,
        placeholder: String,
        collects: tilo_actions::Collects,
    },
}

#[derive(Clone, PartialEq)]
pub(crate) struct ControlIdentity {
    continuation: tilo_actions::ContinuationEvidence,
    locus: tilo_actions::ExecutionLocus,
    kind: tilo_actions::OperationKind,
    operation_type: Option<String>,
    risk: tilo_actions::RiskLevel,
    retry: tilo_actions::RetryPolicy,
    reversible: bool,
    execution: tilo_actions::ExecutionMode,
    discovery: tilo_actions::DiscoveryScope,
    result: tilo_actions::ResultSpec,
    tags: Vec<String>,
    params: Vec<tilo_actions::ActionParam>,
    input_surface: ControlInputSurface,
}

impl ControlWitness {
    pub(crate) fn confirmation(&self) -> &tilo_actions::ConfirmationPolicy {
        &self.confirmation
    }
    pub(crate) fn identity(&self) -> &ControlIdentity {
        &self.identity
    }
}

#[derive(Clone)]
pub(crate) enum ControlContinuationArguments {
    Immediate,
    Text(String),
    Json(serde_json::Map<String, serde_json::Value>),
}

impl ControlContinuationArguments {
    pub(crate) fn as_door_arguments(
        &self,
    ) -> (
        Option<&str>,
        Option<&serde_json::Map<String, serde_json::Value>>,
    ) {
        match self {
            Self::Immediate => (None, None),
            Self::Text(text) => (Some(text), None),
            Self::Json(json) => (None, Some(json)),
        }
    }
    fn query(&self) -> tilo_actions::ContinuationArguments<'_> {
        match self {
            Self::Immediate => tilo_actions::ContinuationArguments::Immediate,
            Self::Text(value) => tilo_actions::ContinuationArguments::Text(value),
            Self::Json(arguments) => tilo_actions::ContinuationArguments::Json(arguments),
        }
    }
}

/// Re-resolve one exact custody target and derive inert comparison data.
/// The returned encoding is never executed.
pub(crate) fn control_witness(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &legacy_document::LocalResourceTarget,
    invocation: &tilo_actions::ControlInvocation,
) -> Result<ControlWitness, tilo_actions::DoorError> {
    control_witness_with_arguments(
        root,
        doc,
        target,
        invocation,
        &ControlContinuationArguments::Json(invocation.arguments.clone()),
    )
}

pub(crate) fn control_witness_with_arguments(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &legacy_document::LocalResourceTarget,
    invocation: &tilo_actions::ControlInvocation,
    original_arguments: &ControlContinuationArguments,
) -> Result<ControlWitness, tilo_actions::DoorError> {
    let resources = resources_for_control_target(root, doc, target, &invocation.address)?;
    let (resource, spec) = unique_control_spec(&resources, &invocation.address)?;
    let (argument, arguments) = match original_arguments {
        ControlContinuationArguments::Immediate => (None, None),
        ControlContinuationArguments::Text(value) => (Some(value.as_str()), None),
        ControlContinuationArguments::Json(arguments) => (None, Some(arguments)),
    };
    let (intent, continuation) = if matches!(spec.input, tilo_actions::ActionInput::Deferred { .. })
    {
        validate_deferred_arguments(spec, arguments, argument)?;
        let intent = DeferredIntent::capture(spec)?;
        let continuation = intent.draft().clone();
        (PreparedActionIntent::Deferred(intent), continuation)
    } else {
        let encoded = encode_resource_action(&resources, &invocation.address, arguments, argument)?;
        let continuation = spec
            .continuation_identity(tilo_actions::ContinuationQuery::Exact(
                original_arguments.query(),
            ))
            .ok_or_else(|| tilo_actions::DoorError::Unavailable {
                code: "unsupported_continuation_identity".into(),
                reason: "The action provider cannot prove the original target and handler identity"
                    .into(),
            })?;
        (PreparedActionIntent::Encoded(encoded), continuation)
    };
    Ok(ControlWitness {
        identity: control_identity_from_spec(resource, spec, continuation),
        intent,
        confirmation: spec.contract.confirmation.clone(),
    })
}

fn resources_for_control_target(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &legacy_document::LocalResourceTarget,
    address: &tilo_actions::ActionAddress,
) -> Result<Vec<tilo_actions::Resource>, tilo_actions::DoorError> {
    match target {
        legacy_document::LocalResourceTarget::Component(target) => {
            if target.address.address != *address {
                return Err(tilo_actions::DoorError::Unknown(address.clone()));
            }
            let snapshots = discover_bound_resources_with_filter(root, doc, Some(address), false)
                .map_err(|error| BoundActionError::Dispatch(error).into_door())?;
            let snapshot = snapshots
                .into_iter()
                .find(|snapshot| snapshot.binding.same_component(&target.binding))
                .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
            // Empty and unauthorized declarations still claim their identity.
            // Check the complete owner forest before presentation filtering.
            unique_control_spec(&snapshot.resources, address)?;
            Ok(snapshot
                .resources
                .into_iter()
                .filter_map(|resource| filter_resource(&snapshot.binding, resource))
                .collect())
        }
        legacy_document::LocalResourceTarget::Legacy(target) => {
            if target.address != *address {
                return Err(tilo_actions::DoorError::Unknown(address.clone()));
            }
            Ok(legacy_document::resolve_legacy_invocation(root, target)?)
        }
        legacy_document::LocalResourceTarget::Outer => {
            let mut resources = Vec::new();
            collect_unbound_resources(root, doc, &mut resources);
            if tilo_actions::door::resolve(&resources, address).is_none()
                && let Some(resource) = resolve_unbound_resource(root, doc, &address.resource)
            {
                resources.push(resource);
            }
            Ok(resources)
        }
    }
}

fn unique_control_spec<'a>(
    resources: &'a [tilo_actions::Resource],
    address: &tilo_actions::ActionAddress,
) -> Result<(&'a tilo_actions::Resource, &'a tilo_actions::ActionSpec), tilo_actions::DoorError> {
    fn matches<'a>(
        resources: &'a [tilo_actions::Resource],
        address: &tilo_actions::ActionAddress,
        found: &mut Vec<&'a tilo_actions::Resource>,
    ) {
        for resource in resources {
            if resource.id.as_str() == address.resource {
                found.push(resource);
            }
            matches(&resource.children, address, found);
        }
    }
    let mut found = Vec::new();
    matches(resources, address, &mut found);
    let [resource] = found.as_slice() else {
        return if found.is_empty() {
            Err(tilo_actions::DoorError::Unknown(address.clone()))
        } else {
            Err(tilo_actions::DoorError::Unavailable {
                code: "ambiguous_action_identity".into(),
                reason: "More than one declaration in the retained owner claims this address"
                    .into(),
            })
        };
    };
    let actions = resource
        .actions
        .iter()
        .filter(|spec| spec.id == address.action)
        .collect::<Vec<_>>();
    let [spec] = actions.as_slice() else {
        return if actions.is_empty() {
            Err(tilo_actions::DoorError::Unknown(address.clone()))
        } else {
            Err(tilo_actions::DoorError::Unavailable {
                code: "ambiguous_action_identity".into(),
                reason: "More than one action declaration in the retained resource claims this id"
                    .into(),
            })
        };
    };
    Ok((*resource, *spec))
}

fn control_identity_from_spec(
    _resource: &tilo_actions::Resource,
    spec: &tilo_actions::ActionSpec,
    continuation: tilo_actions::ContinuationEvidence,
) -> ControlIdentity {
    let input_surface = control_input_surface(spec);
    ControlIdentity {
        continuation,
        locus: spec.locus,
        kind: spec.kind,
        operation_type: spec.operation_type.clone(),
        risk: spec.contract.risk,
        retry: spec.contract.retry,
        reversible: spec.contract.reversible,
        execution: spec.contract.execution.clone(),
        discovery: spec.contract.discovery,
        result: spec.contract.result.clone(),
        tags: spec.contract.tags.clone(),
        params: spec.params().to_vec(),
        input_surface,
    }
}

pub(crate) fn control_draft_identity(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &legacy_document::LocalResourceTarget,
    address: &tilo_actions::ActionAddress,
) -> Result<ControlIdentity, tilo_actions::DoorError> {
    let resources = resources_for_control_target(root, doc, target, address)?;
    let (resource, spec) = unique_control_spec(&resources, address)?;
    let continuation = spec
        .continuation_identity(tilo_actions::ContinuationQuery::Draft)
        .ok_or_else(|| tilo_actions::DoorError::Unavailable {
            code: "unsupported_continuation_identity".into(),
            reason: "The action provider cannot prove a stable draft resolution domain".into(),
        })?;
    Ok(control_identity_from_spec(resource, spec, continuation))
}

/// Canonical Document executor, retaining its actual envelope for owned bookkeeping.
fn execute_document(
    doc: &mut AutoCommit,
    action: &dyn tilo_actions::Action,
    encoded: tilo_actions::EncodedAction,
    transport: &mut dyn tilo_actions::ActionTransport,
    actor: &str,
    broker_admitted: bool,
    context: tilo_actions::ActionContext,
) -> Result<tilo_actions::Dispatched, tilo_actions::DoorError> {
    let before = typed_action_before(doc);
    let step = action.change_kind().is_step() && context.change.is_step();
    // An agent principal names one writer; a person may write from several
    // devices at once.
    let writer = if crate::history_source::authors::agent_principal_name(actor).is_some() {
        actor.as_bytes().to_vec()
    } else {
        crate::history_source::authors::device_actor(actor, doc.get_actor())
    };
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
                .map_err(|e| tilo_actions::DoorError::Failed(e.to_string()))?,
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
            .map_err(|e| tilo_actions::DoorError::Failed(e.to_string()))?
        };
    if let Some(before) = before {
        let after = doc.get_heads();
        report_typed_action(doc, before, after, step);
    }
    Ok(dispatched)
}

/// A resolved, encoded resource operation and the caller it runs for.
pub(crate) struct ResourceOperation<'a> {
    pub(crate) target_route: Option<crate::collaboration::RouteSpec>,
    pub(crate) address: &'a tilo_actions::ActionAddress,
    pub(crate) resource: &'a tilo_actions::Resource,
    pub(crate) spec: &'a tilo_actions::ActionSpec,
    pub(crate) encoded: tilo_actions::EncodedAction,
    pub(crate) actor: &'a str,
    pub(crate) local_author: &'a str,
    pub(crate) broker_admitted: bool,
    pub(crate) invocation: tilo_actions::InvocationId,
    /// Exact invocation retained by an admitted external dispatch. Local
    /// operation ids are intentionally not projected into action payloads.
    pub(crate) source_invocation: Option<tilo_actions::InvocationId>,
    pub(crate) ambient: Option<&'a tilo_actions::ActionContext>,
    pub(crate) scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
}

/// One execution body for bound components and legacy shell roots. Broker
/// admission and semantic delivery are host concerns surrounding this body.
pub(crate) fn execute_resource_operation(
    widget: &mut dyn Widget,
    doc: &mut AutoCommit,
    path: &ObjId,
    registry: &tilo_actions::ActionRegistry,
    transport: &mut dyn tilo_actions::ActionTransport,
    operation: ResourceOperation<'_>,
) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
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
        scope,
    } = operation;
    debug_assert!(
        !broker_admitted || source_invocation.is_some(),
        "admitted operation {address:?} must retain its source invocation"
    );
    if spec
        .contract
        .tags
        .iter()
        .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY)
        && actor != local_author
    {
        return Err(tilo_actions::DoorError::Unavailable {
            code: "local_device_only".into(),
            reason: "This action uses private files on the host participant's device".into(),
        });
    }
    if spec.kind == tilo_actions::OperationKind::View
        && !crate::invocation_scope::view_operation_permitted(actor, local_author, &spec.contract)
    {
        return Err(tilo_actions::DoorError::Unavailable { code: "private_view".into(), reason: "A collaborator cannot move this participant's private focus or viewport; use Follow or Reveal explicitly".into() });
    }
    let started = if spec.kind == tilo_actions::OperationKind::Document {
        let action = registry
            .decode(&encoded)
            .map_err(tilo_actions::DoorError::decoding)?;
        if spec.deferred_invocation_type().is_some() && action.locus() != spec.locus {
            return Err(tilo_actions::DoorError::Failed(
                "Deferred action locus differs from its declaration".into(),
            ));
        }
        // Named after the edit it carries, when the spec carries several.
        let mut context = tilo_actions::ActionContext::for_resource(
            spec.history_label(&encoded),
            resource.id.as_str(),
            &resource.kind,
            &resource.label,
        );
        if let Some(ambient) = ambient {
            context = context.enriched_by(ambient);
        }
        // The admitted caller is who the action is by, local or delivered:
        // pinned with it, so its `apply` reads the same author everywhere.
        if context.invoker.is_none() && !actor.is_empty() {
            context = context.with_invoker(tilo_actions::ActionInvoker::actor(actor));
        }
        // Ambient context is descriptive and may arrive from the caller. The
        // retained dispatch boundary is the sole source for invocation
        // identity, so replace any ambient value, including with `None`.
        let context = context
            .with_source_invocation(source_invocation)
            .with_args_from_payload(&encoded.payload);
        // Before apply, so a delete can still read the row it removes. Undo
        // of this span applies the compensating action instead of patches.
        note_compensating_action(doc, action.inverse_encoded(doc), encoded.clone());
        note_native_text_policy(widget, doc, &encoded);
        let applied = match execute_document(
            doc,
            action.as_ref(),
            encoded,
            transport,
            actor,
            broker_admitted,
            context,
        )? {
            tilo_actions::Dispatched::AppliedLocal(envelope) => Some(envelope),
            tilo_actions::Dispatched::SubmittedToHost(_) => None,
        };
        let before = applied
            .as_ref()
            .map(|envelope| envelope.base_change_hashes());
        if let Some(envelope) = applied.as_ref() {
            // Reuse the executor's actual admission frontier: this adds no
            // document loan, commit, replay behavior, or cross-owner traversal.
            crate::history_mode::capability::document_action_applied(
                widget,
                &envelope.action,
                &envelope.base_change_hashes(),
                doc,
                path,
            );
        }
        let value =
            matches!(spec.contract.result, tilo_actions::ResultSpec::Json { .. }).then(|| {
                // Submitted to another host, nothing here shows its effect.
                before
                    .as_ref()
                    .and_then(|before| {
                        let after = doc.get_heads();
                        let changes = doc.diff(before, &after);
                        crate::resources::document_action_result(&*widget, doc, address, &changes)
                    })
                    .unwrap_or(serde_json::Value::Null)
            });
        let output = tilo_actions::ActionOutput {
            value,
            ..tilo_actions::ActionOutput::default()
        };
        if !matches!(
            spec.contract.execution,
            tilo_actions::ExecutionMode::Immediate
        ) && let Some(envelope) = applied.as_ref()
            && let Some(effects) = crate::widget::capability::capability_mut::<
                dyn crate::widget::document_effects::DocumentEffects,
            >(widget)
        {
            let operation = match scope {
                Some(scope) => crate::widget::ResourceOperation::scoped(
                    scope,
                    tilo_actions::OperationKind::Document,
                    &envelope.action.type_id,
                    &envelope.action.payload,
                ),
                None => crate::widget::ResourceOperation::local(
                    source_invocation.unwrap_or(invocation),
                    tilo_actions::OperationKind::Document,
                    &envelope.action.type_id,
                    &envelope.action.payload,
                ),
            };
            let before = envelope.base_change_hashes();
            let applied = crate::widget::document_effects::AppliedDocument {
                invocation,
                before: &before,
                document: doc,
                output: output.clone(),
            };
            effects
                .start_document_effects(&operation, &applied)
                .map_err(tilo_actions::DoorError::Failed)?
                .unwrap_or(crate::widget::ResourceOperationStart::Completed(output))
        } else {
            crate::widget::ResourceOperationStart::Completed(output)
        }
    } else if let Some(readers) =
        crate::semantic_document_read::registered_in_tree(&*widget, &encoded.type_id)
    {
        let refused = |error: crate::semantic_document_read::SemanticReadError| {
            let (code, reason) = error.describe();
            tilo_actions::DoorError::Unavailable {
                code: code.into(),
                reason: reason.into(),
            }
        };
        let selected = crate::semantic_document_read::select(readers, resource, spec, &encoded)
            .map_err(refused)?
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
        crate::widget::ResourceOperationStart::Completed(
            selected.read(scope, doc).map_err(refused)?,
        )
    } else {
        let target = target_route.map(|route| {
            crate::widget::resource_operation::OperationTarget::new(route, address.clone())
        });
        let op = match scope {
            Some(scope) => crate::widget::ResourceOperation::scoped(
                scope,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
            ),
            None => crate::widget::ResourceOperation::local(
                invocation,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
            ),
        };
        let op = op.with_target(target.as_ref());
        widget
            .start_operation(&op, &mut crate::widget::Edits::lend(doc), path)
            .map_err(tilo_actions::DoorError::Failed)?
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?
    };
    match &started {
        crate::widget::ResourceOperationStart::Accepted
            if matches!(
                spec.contract.execution,
                tilo_actions::ExecutionMode::Immediate
            ) =>
        {
            return Err(tilo_actions::DoorError::Failed(
                "an immediate operation cannot remain pending".into(),
            ));
        }
        crate::widget::ResourceOperationStart::Completed(output) => {
            spec.contract
                .result
                .validate(output)
                .map_err(tilo_actions::DoorError::Failed)?;
        }
        crate::widget::ResourceOperationStart::Accepted => {}
    }
    Ok(started)
}

/// One caller's request to a bound resource action, before resolution.
#[derive(Clone, Copy)]
pub(crate) struct ResourceActionRequest<'a> {
    pub(crate) argument: Option<&'a str>,
    pub(crate) arguments: Option<&'a serde_json::Map<String, serde_json::Value>>,
    pub(crate) actor: &'a str,
    pub(crate) confirmed: bool,
    pub(crate) broker_admitted: bool,
    pub(crate) invocation: tilo_actions::InvocationId,
    pub(crate) source_invocation: Option<tilo_actions::InvocationId>,
    pub(crate) ambient: Option<&'a tilo_actions::ActionContext>,
    pub(crate) scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
}

/// Private checked front door. `document` carries the explicit active Doc when
/// called reentrantly; the outer admission moves/restores record-owned history.
pub(crate) fn invoke_resource_action(
    root: &mut dyn Widget,
    document: &mut AutoCommit,
    target: &BoundActionTarget,
    transport: &mut dyn tilo_actions::ActionTransport,
    request: ResourceActionRequest<'_>,
) -> Result<crate::widget::ResourceOperationStart, BoundActionError> {
    let ResourceActionRequest {
        argument,
        arguments,
        actor,
        confirmed,
        broker_admitted,
        invocation,
        source_invocation,
        ambient,
        scope,
    } = request;
    let _entry = target.binding.enter()?;
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
                .with_record_frame(document, false, |doc, path| {
                    let mut resources = Vec::new();
                    // The loan's root belongs to this component. Nested boundaries are
                    // independent destinations and cannot donate declarations here.
                    // A delivered invocation resolves in its invoker's own tree.
                    match crate::invocation_scope::current_participant() {
                        Some(participant) => collect_participant_resources(
                            widget,
                            doc,
                            &participant,
                            &mut resources,
                        ),
                        None => collect_component_resources(widget, doc, &mut resources),
                    }
                    if tilo_actions::door::resolve(&resources, &target.address.address).is_none()
                        && let Some(resource) =
                            resolve_unbound_resource(widget, doc, &target.address.address.resource)
                                .or_else(|| {
                                    provider_resource(widget, binding, &target.address.address)
                                })
                    {
                        resources.push(resource);
                    }
                    let resources = resources
                        .into_iter()
                        .filter_map(|resource| filter_resource(binding, resource))
                        .collect::<Vec<_>>();
                    let (arguments, resource, spec) = resolve_resource_invocation(
                        &resources,
                        &target.address.address,
                        argument,
                        arguments,
                        confirmed,
                    )?;
                    if let Some(scope) = scope {
                        scope.check_dispatch_spec(spec)?;
                    }
                    // The tree offers an action under the least it needs;
                    // this invocation needs what its value asks.
                    if !permits_invocation(binding, &target.address.address.resource, spec, arguments.as_ref(), argument) {
                        return Err(denied(spec).into());
                    }
                    crate::invocation_scope::check_continuation(
                        spec,
                        match arguments.as_ref() {
                            Some(arguments) => spec.permission_for_json(arguments),
                            None => spec.permission_for_argument(argument),
                        },
                    )?;
                    // A refused private View must not invoke a Deferred encoder.
                    // The execution guard below remains an independent boundary.
                    if spec.kind == tilo_actions::OperationKind::View
                        && !crate::invocation_scope::view_operation_permitted(
                            actor,
                            binding.author.0.as_str(),
                            &spec.contract,
                        )
                    {
                        return Err(tilo_actions::DoorError::Unavailable {
                            code: "private_view".into(),
                            reason: "A collaborator cannot move this participant's private focus or viewport; use Follow or Reveal explicitly".into(),
                        }.into());
                    }
                    if let Some(scope) = scope {
                        scope.capture_locus_basis(target, invocation, actor, doc);
                    }
                    let encoded = encode_resource_action(
                        &resources,
                        &target.address.address,
                        arguments.as_ref(),
                        argument,
                    )?;
                    let execute = |widget: &mut dyn Widget, doc: &mut AutoCommit, path: &ObjId| {
                        with_native_text(target.physical_text && actor == binding.author.0 && crate::history_source::authors::agent_principal_name(actor).is_none(), || execute_resource_operation(
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
                                local_author: &binding.author.0,
                                broker_admitted,
                                invocation,
                                source_invocation,
                                ambient: ambient.or(Some(&binding.operation_context)),
                                scope,
                            },
                        ))
                    };
                    let run = |doc: &mut AutoCommit| {
                    let mut operation = BoundOperation::enter(binding, doc, spec.kind)?;
                    let started = crate::invocation_scope::with_loaned_record(
                        &binding.records,
                        &binding.checkout,
                        &binding.app_id,
                        route.clone(),
                        operation.doc,
                        |doc| execute(widget, doc, path),
                    );
                    accepted = matches!(started, Ok(crate::widget::ResourceOperationStart::Accepted));
                    // Consume the legacy rising edge without widening an exact typed
                    // span to arbitrary writes elsewhere in the opaque callback.
                    binding.drain_owned_tree(widget, operation.doc)?;
                    if operation.finish() {
                        return Err(BoundActionError::Door(tilo_actions::DoorError::Unavailable {
                            code: "undeclared_document_mutation".into(),
                            reason: "callback mutated the document without a declared Document follow-up; bytes were retained".into(),
                        }));
                    }
                    started.map_err(BoundActionError::Door)
                    };
                    match scope {
                        Some(scope) => scope.with_document_mutation(doc, |doc| Ok(run(doc)))
                            .map_err(|error| BoundActionError::Door(tilo_actions::DoorError::Failed(error)))?,
                        None => run(doc),
                    }
                })
                .map_err(BoundActionError::Dispatch)
                .and_then(std::convert::identity),
        );
        // Even a post-delivery contract fault must retain Accepted lifecycle
        // custody for the component's existing CLOSED cancellation/cleanup path.
        accepted.then_some((invocation, true))
    };
    root.visit_components(&mut ComponentRequest {
        target: Some(&target.binding),
        found: false,
        visit: &mut delivery,
    })?;
    result.unwrap_or(Err(BoundActionError::Dispatch(
        BoundDispatchError::ClosedLifetime,
    )))
}

fn collect_component_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    resources: &mut Vec<tilo_actions::Resource>,
) {
    if widget.has_separate_document_owner() {
        return;
    }
    resources.extend(crate::resources::resources(widget, doc));
    widget.for_each_child(&mut |child| collect_component_resources(child, doc, resources));
}

pub(crate) fn collect_unbound_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    resources: &mut Vec<tilo_actions::Resource>,
) {
    collect_component_resources(widget, doc, resources);
}

/// [`collect_unbound_resources`] rebuilt for one participant's own view.
/// Never cached: it is the invoker's tree at invoke time.
pub(crate) fn collect_participant_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    participant: &tilo_actions::ParticipantId,
    resources: &mut Vec<tilo_actions::Resource>,
) {
    if widget.has_separate_document_owner() {
        return;
    }
    resources.extend(crate::resources::resources_for(
        widget,
        Some(participant),
        doc,
    ));
    widget.for_each_child(&mut |child| {
        collect_participant_resources(child, doc, participant, resources);
    });
}

pub(crate) fn resolve_unbound_resource(
    widget: &dyn Widget,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::Resource> {
    if widget.has_separate_document_owner() {
        return None;
    }
    let participant = crate::invocation_scope::current_participant();
    let mut found = crate::resources::resource_for(widget, participant.as_ref(), doc, id);
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = resolve_unbound_resource(child, doc, id);
        }
    });
    found
}

/// Runtime metadata; does not own the component's widget or mount lifetime.
#[derive(Clone)]
pub struct CallbackBinding {
    pub(crate) lifetime: WakeHandle,
    pub(crate) closing: Rc<Cell<bool>>,
    pub(crate) registry_owner: std::rc::Weak<()>,
    pub(crate) lifetime_id: uuid::Uuid,
    pub(crate) control_domain: crate::control_custody::WeakControlDomain,
    pub(crate) queued_controls:
        Rc<RefCell<std::collections::VecDeque<tilo_actions::ControlInvocation>>>,
    /// Searches the component asked the host to open, drained as host
    /// intents by the shell that presents search.
    pub(crate) search_requests:
        Rc<RefCell<std::collections::VecDeque<crate::search::SearchRequest>>>,
    #[allow(
        dead_code,
        reason = "binding retains original authority for the following typed-action provenance task"
    )]
    pub(crate) instance_id: crate::application::InstanceId,
    pub(crate) app_id: String,
    pub(crate) records: RecordStore,
    pub(crate) checkout: RecordCheckout,
    pub(crate) permissions: Rc<RefCell<PermissionContext>>,
    #[allow(
        dead_code,
        reason = "binding retains original authority for the following typed-action provenance task"
    )]
    pub(crate) author: UserId,
    #[allow(
        dead_code,
        reason = "binding retains original authority for the following typed-action provenance task"
    )]
    pub(crate) operation_context: tilo_actions::ActionContext,
    /// Edits commit as the author's own writer on the record
    /// ([`crate::application::HostContext::write_as_author`]).
    pub(crate) writes_as_author: bool,
}

/// Private retained traversal request. The public Widget trait cannot expose a
/// root/registry closure directly; only toolkit code can construct this request.
type ComponentVisitor<'a> = dyn FnMut(
        &CallbackBinding,
        &mut dyn crate::widget::AppWidget,
        &tilo_actions::ActionRegistry,
    ) -> Option<(tilo_actions::InvocationId, bool)>
    + 'a;
pub(crate) struct ComponentRequest<'a> {
    pub(crate) target: Option<&'a CallbackBinding>,
    pub(crate) found: bool,
    pub(crate) visit: &'a mut ComponentVisitor<'a>,
}

/// One original ordinary model pass for the current retained owner. No callback
/// is omitted from a wake reason, document frontier or presentation state.
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
                .with_settlement(outer, |document, record_root| {
                    owner.update(document, record_root);
                    owner.settle(&mut crate::widget::Edits::lend(document), record_root);
                    current.drain_owned_tree(owner, document).map(|_| ())
                })
                .and_then(|result| result);
            // Preserve the original separate update and sync admissions/actor
            // restoration, rather than holding a canonical loan across both.
            let synced = current
                .with_document(outer, |document, record_root| {
                    owner.sync(document, record_root);
                    current.drain_owned_tree(owner, document).map(|_| ())
                })
                .and_then(|result| result);
            // The original host attempts sync after a nonpanic update/drain
            // refusal. Retain its first error without omitting the fresh door.
            let admitted = updated.and(synced);
            // Original status projection follows the canonical loan, including
            // refusal. No application callback runs under a record-store loan.
            let projected = current.enter().and_then(|_entry| {
                current
                    .records
                    .save_status(&current.checkout.id)
                    .map_err(BoundDispatchError::Record)
                    .map(|status| owner.record_save_status(status))
            });
            // Offer only this admitted record, after both canonical loans.
            // Pending host acknowledgement is accepted, not a model retry.
            let offered = current.enter().and_then(|_entry| {
                current
                    .records
                    .offer_record(&current.checkout.id)
                    .map_err(BoundDispatchError::Persistence)
            });
            result = Some(admitted.and(projected).and(offered));
        }
        None
    };
    let mut request = ComponentRequest {
        target: Some(captured),
        found: false,
        visit: &mut visit,
    };
    root.visit_retained_components(&mut RetainedComponentRequest {
        inner: &mut request,
    })?;
    result.unwrap_or(Err(BoundDispatchError::ClosedLifetime))
}

/// SDK-owned retained component traversal, separate from public callback reach.
pub struct RetainedComponentRequest<'a, 'b> {
    inner: &'a mut ComponentRequest<'b>,
}
impl<'b> RetainedComponentRequest<'_, 'b> {
    pub(crate) fn inner(&mut self) -> &mut ComponentRequest<'b> {
        self.inner
    }
    /// Forward only through actual retained storage. Unknown roots preserve their
    /// original `visit_components` callback, including refusals and side effects.
    pub fn visit(&mut self, child: &mut dyn Widget) -> Result<(), BoundDispatchError> {
        if self.inner.found {
            return Ok(());
        }
        child.visit_retained_components(self)
    }
}

#[derive(Debug)]
pub enum BoundDispatchError {
    MissingNode,
    ClosedLifetime,
    CrossRecordReentry,
    Record(RecordOpenError),
    Persistence(String),
}

/// Logical typing ownership survives physical commits made when an opaque
/// callback from another instance is admitted. Read-only entry never touches it.
pub(crate) struct PendingRecordAction {
    instance_id: crate::application::InstanceId,
    owner: Rc<Cell<bool>>,
    before: Vec<ChangeHash>,
    author: UserId,
    metadata: Option<String>,
    context: tilo_actions::ActionContext,
    native_text: Option<NativeTextSpan>,
    inverse: crate::state::CommittedSpanInverse,
}

struct NativeTextSpan {
    action: tilo_actions::EncodedAction,
    frontier: Vec<ChangeHash>,
}

struct NativeTextPolicy {
    action: tilo_actions::EncodedAction,
    continues: bool,
    continuable: bool,
}

pub(crate) struct RecordActionState {
    pub(crate) invocation_provenance: Rc<RefCell<crate::invocation_scope::InvocationProvenance>>,
    pending: Option<PendingRecordAction>,
    pub(crate) generation: Option<u64>,
    statuses: HashMap<(UserId, crate::application::InstanceId), crate::undo::UndoStatus>,
}
impl Default for RecordActionState {
    fn default() -> Self {
        Self {
            invocation_provenance: Rc::default(),
            pending: None,
            generation: Some(0),
            statuses: HashMap::new(),
        }
    }
}
impl RecordActionState {
    #[cfg(test)]
    pub(crate) fn statuses_for_test(
        &self,
    ) -> Vec<(
        String,
        crate::application::InstanceId,
        crate::undo::UndoStatus,
    )> {
        let mut statuses = self
            .statuses
            .iter()
            .map(|((author, instance), status)| (author.0.clone(), *instance, status.clone()))
            .collect::<Vec<_>>();
        statuses.sort_unstable_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        statuses
    }

    /// Persistence may close a storage transaction while retaining the logical
    /// undo burst; preserve its original provenance before serialization does so.
    pub(crate) fn commit_for_save(&self, doc: &mut AutoCommit) {
        if let Some(pending) = &self.pending {
            tilo_resource::history::commit_stamped_with_message(
                doc,
                pending.context.change_message(),
            );
        } else if doc.pending_ops() != 0 {
            // Legacy widget input is not wrapped in a bound action frame.
            // Persistence must still close that transaction or DocWriter sees
            // unchanged heads and silently writes only the earlier snapshot.
            doc.commit();
        }
    }
    /// Explicit external mutable loans (merge/import/revision/legacy history)
    /// are boundaries before foreign changes can join a local logical burst.
    pub(crate) fn finish_external(
        &mut self,
        doc: &mut AutoCommit,
        history: &mut crate::state::UndoLog,
    ) {
        if let Some(pending) = self.pending.take() {
            tilo_resource::history::commit_stamped_with_message(
                doc,
                pending.context.change_message(),
            );
            history.record_committed_span_with_inverse(
                pending.before,
                doc.get_heads(),
                None,
                pending.metadata,
                Some(pending.author),
                Some(pending.instance_id),
                pending.inverse,
            );
        }
        self.statuses.clear();
    }
}

struct ActiveMutation {
    binding: CallbackBinding,
    /// A foreign segment is compared against this boundary after delivery.
    /// Same-owner continuation does not close or refresh the open transaction.
    before: Option<Vec<ChangeHash>>,
    /// Settlement names its owner without closing a draft until an action writes.
    deferred: bool,
}
struct ActionFrame {
    history: crate::state::UndoLog,
    state: RecordActionState,
    mutation: Option<ActiveMutation>,
    operations: Vec<OperationSpan>,
    /// The record's own writer, which the document writes as outside a
    /// per-author binding's segment and again when the loan ends.
    writer: automerge::ActorId,
}
type FrameHandle = Rc<RefCell<Option<ActionFrame>>>;

/// Owned bookkeeping only; the explicit document loan remains on the stack.
/// Effect cursors police opaque mutations. View has no cursor until a real
/// typed producer arrives, so pure navigation never closes pending typing.
struct OperationSpan {
    binding: CallbackBinding,
    cursor: Option<Vec<ChangeHash>>,
    effect: bool,
    violation: bool,
    /// Compensating document action captured before `apply`, for undo.
    undo_via: Option<tilo_actions::EncodedAction>,
    /// The forward action, so redo runs through `apply` as well.
    redo_via: Option<tilo_actions::EncodedAction>,
    native_text: Option<NativeTextPolicy>,
}

fn operation_frame(doc: &AutoCommit) -> Option<FrameHandle> {
    ACTIVE.with_borrow(|active| {
        let (_, _, witness, handle) = active.as_ref()?;
        if !std::ptr::eq(*witness, doc) {
            return None;
        }
        let handle = handle.as_ref()?;
        let owned = handle.borrow();
        let operation = owned.as_ref()?.operations.last()?;
        // The checked component loan established registry custody. Recheck the
        // captured lifetime before accepting evidence from an explicit Doc.
        if operation.binding.closing.get()
            || !operation.binding.lifetime.is_active()
            || operation.binding.registry_owner.upgrade().is_none()
        {
            return None;
        }
        Some(handle.clone())
    })
}

/// The frame of `doc` while a callback of a bound component mutates it (a
/// sync, an event) or one of its operations runs.
fn mutation_frame(doc: &AutoCommit) -> Option<FrameHandle> {
    if let Some(handle) = operation_frame(doc) {
        return Some(handle);
    }
    ACTIVE.with_borrow(|active| {
        let (_, _, witness, handle) = active.as_ref()?;
        if !std::ptr::eq(*witness, doc) {
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

/// Check a direct affordance against the same live permission-filtered spec
/// as named actions. Only a genuinely unbound call keeps legacy compatibility;
/// an active record with missing/mismatched/revoked custody always refuses.
pub(crate) fn authorize_current_resource_action(
    doc: &AutoCommit,
    resource: &tilo_actions::Resource,
    action_id: &str,
) -> Result<(), String> {
    let active =
        ACTIVE.with_borrow(|active| active.as_ref().map(|entry| (entry.2, entry.3.clone())));
    let Some((witness, frame)) = active else {
        return Ok(());
    };
    if !std::ptr::eq(witness, doc) {
        return Err("resource action document does not match its owned frame".into());
    }
    let frame = frame.ok_or("resource action has no owned frame")?;
    let binding = {
        let owned = frame.borrow();
        let frame = owned.as_ref().ok_or("resource action frame has ended")?;
        frame
            .mutation
            .as_ref()
            .map(|mutation| mutation.binding.clone())
            .or_else(|| {
                frame
                    .operations
                    .last()
                    .map(|operation| operation.binding.clone())
            })
            .ok_or("resource action has no current owner")?
    };
    if binding.registry_owner.upgrade().is_none() {
        return Err("resource action registry owner has ended".into());
    }
    let _entry = binding
        .enter()
        .map_err(|error| format!("resource action owner refused: {error:?}"))?;
    if !binding
        .check_record()
        .map_err(|error| format!("resource action record refused: {error:?}"))?
        .is_some_and(|current| std::ptr::eq(current, doc))
    {
        return Err("resource action record does not match its owner".into());
    }
    if filter_resource(&binding, resource.clone())
        .is_some_and(|resource| resource.actions.iter().any(|action| action.id == action_id))
    {
        Ok(())
    } else {
        Err("resource action is unavailable under current permissions".into())
    }
}

/// Stash the compensating action on the open span. `report_typed_action`
/// records it with the undo entry. No span (a standalone widget) leaves the
/// `ActionHost` path to record its own pair.
fn note_compensating_action(
    doc: &AutoCommit,
    undo_via: Option<tilo_actions::EncodedAction>,
    redo_via: tilo_actions::EncodedAction,
) {
    if undo_via.is_none() {
        return;
    }
    let Some(handle) = operation_frame(doc) else {
        return;
    };
    let mut owned = handle.borrow_mut();
    let Some(frame) = owned.as_mut() else {
        return;
    };
    let Some(operation) = frame.operations.last_mut() else {
        return;
    };
    if operation.undo_via.is_none() {
        operation.undo_via = undo_via;
        operation.redo_via = Some(redo_via);
    }
}

fn note_native_text_policy(
    widget: &dyn Widget,
    doc: &AutoCommit,
    encoded: &tilo_actions::EncodedAction,
) {
    if !NATIVE_TEXT_EXECUTION.with(Cell::get) {
        return;
    }
    let Some(history) = crate::widget::capability::capability::<
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
    if let Some(operation) = frame.operations.last_mut() {
        operation.native_text = Some(NativeTextPolicy {
            action: encoded.clone(),
            continues,
            continuable,
        });
    }
}

/// Narrow shared producer hook. Called only when a typed action is about to
/// execute; returning None preserves the legacy `ActionHost` path unchanged.
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
pub(crate) fn report_typed_action(
    doc: &mut AutoCommit,
    before: Vec<ChangeHash>,
    after: Vec<ChangeHash>,
    step: bool,
) {
    if before == after {
        return;
    }
    // Content from outside applied while a callback mutates the document (a
    // surface recording a process's output as it syncs) stays out of the
    // callback's step as it stays out of an operation's.
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
        && frame.state.pending.as_ref().is_some_and(|pending| {
            Rc::ptr_eq(&pending.owner, &binding.closing)
                && pending.author == binding.author
                && pending.instance_id == binding.instance_id
                && pending
                    .native_text
                    .as_ref()
                    .is_some_and(|span| span.frontier == before)
                && frame.operations.last().is_some_and(|operation| {
                    pending.inverse.undo_via.is_some() == operation.undo_via.is_some()
                })
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
    let (undo_via, redo_via) = frame
        .operations
        .last_mut()
        .map(|operation| (operation.undo_via.take(), operation.redo_via.take()))
        .unwrap_or((None, None));
    if let Some(native) = native.filter(|policy| policy.continuable) {
        if !continues {
            let mut pending = PendingRecordAction::new(&binding, before);
            pending.inverse = crate::state::CommittedSpanInverse { undo_via, redo_via };
            frame.state.pending = Some(pending);
        } else if let Some(pending) = frame.state.pending.as_mut() {
            // The original compensation reverses the whole burst; redo uses
            // the latest full-value action. Never substitute a later inverse.
            pending.inverse.redo_via = redo_via;
        }
        if let Some(pending) = frame.state.pending.as_mut() {
            pending.native_text = Some(NativeTextSpan {
                action: native.action,
                frontier: after.clone(),
            });
        }
        if let Some(active) = frame.mutation.as_mut() {
            active.before = (!active.binding.same_component(&binding)).then_some(after);
        }
        frame.state.statuses.clear();
        frame.state.generation = frame
            .state
            .generation
            .and_then(|value| value.checked_add(1));
        return;
    }
    // A noncontinuable text edit still closes a preceding burst, then gets
    // its own immediate entry, like every programmatic document action.
    if continues {
        frame.finish_pending_at(before.clone(), doc);
    }
    frame.history.record_committed_span_with_inverse(
        before,
        after.clone(),
        None,
        None,
        Some(binding.author.clone()),
        Some(binding.instance_id),
        crate::state::CommittedSpanInverse { undo_via, redo_via },
    );
    frame.reset_operation_baseline(&binding, after);
    frame.state.generation = frame
        .state
        .generation
        .and_then(|value| value.checked_add(1));
}

struct BoundOperation<'a> {
    doc: &'a mut AutoCommit,
    frame: FrameHandle,
    depth: usize,
    finished: bool,
}
impl<'a> BoundOperation<'a> {
    fn enter(
        binding: &CallbackBinding,
        doc: &'a mut AutoCommit,
        kind: tilo_actions::OperationKind,
    ) -> Result<Self, BoundDispatchError> {
        if !binding
            .check_record()?
            .is_some_and(|pointer| std::ptr::eq(pointer, doc))
        {
            return Err(BoundDispatchError::Record(RecordOpenError::Busy));
        }
        let handle = ACTIVE
            .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
            .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
        let depth = {
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            let effect = kind == tilo_actions::OperationKind::Effect;
            let cursor = effect.then(|| frame.commit_physical(doc));
            let depth = frame.operations.len();
            frame.operations.push(OperationSpan {
                binding: binding.clone(),
                cursor,
                effect,
                violation: false,
                undo_via: None,
                redo_via: None,
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
        // Stamp retained opaque writes while their owning operation is still
        // active, including when returning to another instance after unwind.
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
impl Drop for BoundOperation<'_> {
    fn drop(&mut self) {
        // Preserve canonical bytes and restore bookkeeping on unwind as well.
        self.finish();
    }
}

/// Owns every history/pending value while the store exclusively lends its Doc.
/// The handle becomes empty before the record admission ends, including unwind.
struct RestoreFrame<'a> {
    history: &'a mut crate::state::UndoLog,
    state: &'a mut RecordActionState,
    frame: FrameHandle,
}
impl Drop for RestoreFrame<'_> {
    fn drop(&mut self) {
        if let Some(owned) = self.frame.borrow_mut().take() {
            *self.history = owned.history;
            *self.state = owned.state;
        }
    }
}

impl PendingRecordAction {
    fn new(binding: &CallbackBinding, before: Vec<ChangeHash>) -> Self {
        Self {
            instance_id: binding.instance_id,
            owner: binding.closing.clone(),
            before,
            author: binding.author.clone(),
            metadata: None,
            context: binding.operation_context.clone(),
            native_text: None,
            inverse: crate::state::CommittedSpanInverse::default(),
        }
    }
}

impl ActionFrame {
    /// Leave the change from `before` to `after` out of every undo step: the
    /// step open before it is finished at `before`, and the next one starts
    /// at `after`.
    fn pass_over(&mut self, before: Vec<ChangeHash>, after: Vec<ChangeHash>, doc: &mut AutoCommit) {
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
            None => self.state.pending = None,
        }
        self.state.generation = self.state.generation.and_then(|value| value.checked_add(1));
    }

    fn reset_operation_baseline(&mut self, binding: &CallbackBinding, after: Vec<ChangeHash>) {
        self.state.pending = Some(PendingRecordAction::new(binding, after.clone()));
        if let Some(active) = self.mutation.as_mut() {
            active.before = (!active.binding.same_component(binding)).then_some(after);
        }
        self.state.statuses.clear();
    }
    fn status(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: crate::application::InstanceId,
    ) -> crate::undo::UndoStatus {
        let undo = self
            .history
            .next_undo_in(author, instance)
            .map(|entry| (entry.before.clone(), entry.after.clone()));
        let redo = self
            .history
            .next_redo_in(author, instance)
            .map(|entry| (entry.after.clone(), entry.before.clone()));
        let key = (author.clone(), instance);
        let mut status = self.state.statuses.get(&key).cloned().unwrap_or_default();
        let pending_undo = self.state.pending.as_ref().is_some_and(|pending| {
            pending.author == *author
                && pending.instance_id == instance
                && (pending.before != doc.base_heads()
                    || (doc.pending_ops() > 0
                        && self.mutation.as_ref().is_none_or(|active| {
                            Rc::ptr_eq(&active.binding.closing, &pending.owner)
                        })))
        });
        status.can_undo = undo.is_some() || pending_undo;
        status.can_redo = redo.is_some() && !pending_undo;
        // diff closes an AutoCommit transaction. Summaries are refreshed only
        // at committed boundaries; inspecting status preserves pending typing.
        if doc.pending_ops() == 0 {
            self.state.statuses.retain(|(author, instance), _| {
                self.history.next_undo_in(author, *instance).is_some()
                    || self.history.next_redo_in(author, *instance).is_some()
            });
            status.undo_summary = undo
                .map(|(before, after)| crate::history_source::change_summary(doc, &before, &after));
            status.redo_summary = redo
                .map(|(before, after)| crate::history_source::change_summary(doc, &before, &after));
            if status.can_undo || status.can_redo {
                self.state.statuses.insert(key, status.clone());
            }
        }
        if pending_undo {
            // The live transaction has no stable diff yet. Do not describe
            // an older entry as the pending action that Undo will select.
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
        let context = if let Some(operation) = self
            .operations
            .last()
            .filter(|scope| scope.cursor.is_some())
        {
            Some(&operation.binding.operation_context)
        } else {
            match (&self.mutation, &self.state.pending) {
                (Some(active), Some(pending))
                    if Rc::ptr_eq(&active.binding.closing, &pending.owner) =>
                {
                    Some(&pending.context)
                }
                (Some(active), _) => Some(&active.binding.operation_context),
                (None, pending) => pending.as_ref().map(|pending| &pending.context),
            }
        };
        tilo_resource::history::commit_stamped_with_message(
            doc,
            context.and_then(tilo_actions::ActionContext::change_message),
        );
        doc.get_heads()
    }

    /// Write as `actor` from here on. What is pending was written as the
    /// actor before, so it is committed first: `set_actor` would otherwise
    /// commit it without a time.
    fn switch_writer(&self, doc: &mut AutoCommit, actor: automerge::ActorId) {
        if doc.get_actor() != &actor {
            self.commit_physical(doc);
            doc.set_actor(actor);
        }
    }

    /// The writer `binding`'s edits commit as.
    fn writer_of(&self, binding: &CallbackBinding) -> automerge::ActorId {
        if binding.writes_as_author {
            crate::record::author_writer(&self.writer, &binding.author.0)
        } else {
            self.writer.clone()
        }
    }

    fn finish_pending_at(&mut self, after: Vec<ChangeHash>, doc: &mut AutoCommit) {
        if let Some(pending) = self.state.pending.take() {
            let author = pending.author.clone();
            let instance = pending.instance_id;
            self.history.record_committed_span_with_inverse(
                pending.before,
                after,
                None,
                pending.metadata,
                Some(pending.author),
                Some(pending.instance_id),
                pending.inverse,
            );
            self.status(doc, &author, instance);
        }
    }

    /// Observe the completed segment before crossing into another owner, and
    /// again on return. No-op foreign callbacks preserve the original burst.
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
                .pending
                .as_ref()
                .is_some_and(|p| !Rc::ptr_eq(&p.owner, &binding.closing))
            {
                self.finish_pending_at(before.clone(), doc);
            }
            if self.state.pending.is_none() {
                self.state.pending = Some(PendingRecordAction::new(&binding, before));
            }
        }
    }

    fn activate(&mut self, binding: &CallbackBinding, doc: &mut AutoCommit) {
        self.settle_segment(doc);
        let same_owner = self
            .state
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
        if self.state.pending.is_none() {
            self.state.pending = Some(PendingRecordAction::new(
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

struct SettlementReturn<'a> {
    document: &'a mut AutoCommit,
    frame: FrameHandle,
    previous: Option<ActiveMutation>,
}
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
struct MutationReturn<'a> {
    doc: &'a mut AutoCommit,
    frame: FrameHandle,
    previous: Option<CallbackBinding>,
}
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

type ActiveRecord = (
    RecordStore,
    RecordCheckout,
    *const AutoCommit,
    Option<FrameHandle>,
);
thread_local! {
    static ACTIVE: RefCell<Option<ActiveRecord>> = const { RefCell::new(None) };
}
struct ActiveEntry;
impl Drop for ActiveEntry {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|active| *active = None);
    }
}

pub(crate) fn with_owned_record_action<R>(
    records: &RecordStore,
    checkout: &RecordCheckout,
    instance_id: crate::application::InstanceId,
    app_id: &str,
    author: UserId,
    context: tilo_actions::ActionContext,
    callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
) -> Result<R, BoundDispatchError> {
    let scope = crate::wake::WakeScope::new();
    let owner = Rc::new(());
    let binding = CallbackBinding {
        lifetime: scope.handle(),
        closing: Rc::new(Cell::new(false)),
        registry_owner: Rc::downgrade(&owner),
        lifetime_id: uuid::Uuid::new_v4(),
        control_domain: crate::control_custody::WeakControlDomain::default(),
        queued_controls: Rc::new(RefCell::new(std::collections::VecDeque::new())),
        search_requests: Rc::default(),
        instance_id,
        app_id: app_id.to_string(),
        records: records.clone(),
        checkout: checkout.clone(),
        permissions: Rc::new(RefCell::new(PermissionContext::permissive())),
        author,
        operation_context: context,
        writes_as_author: false,
    };
    let mut outer_witness = AutoCommit::new();
    binding.with_document(&mut outer_witness, |document, root| {
        let value = callback(document, root);
        binding.finish_action_boundary(document).map(|()| value)
    })?
}

impl CallbackBinding {
    /// Queue a search for the host presenting this component. Only the
    /// latest request matters: an older one the host has not shown yet is
    /// replaced.
    pub(crate) fn request_search(&self, request: crate::search::SearchRequest) {
        let mut requests = self.search_requests.borrow_mut();
        requests.clear();
        requests.push_back(request);
    }

    /// The search this component asked for, once.
    pub(crate) fn take_search_request(&self) -> Option<crate::search::SearchRequest> {
        self.search_requests.borrow_mut().pop_front()
    }

    pub(crate) fn broker_route(&self) -> Option<crate::collaboration::RouteSpec> {
        let (_, app, _) = self.records.canonical_namespace()?;
        if app != self.app_id {
            return None;
        }
        self.records.route(&self.checkout).ok()
    }
    /// [`Self::broker_route`], or a live New reservation's route before its
    /// factory writes the record. Only native component admission uses it.
    pub(crate) fn admission_route(&self) -> Option<crate::collaboration::RouteSpec> {
        let (_, app, _) = self.records.canonical_namespace()?;
        if app != self.app_id {
            return None;
        }
        self.records.admission_route(&self.checkout).ok()
    }
    pub(crate) fn matches_broker_route(
        &self,
        witness: Option<&crate::collaboration::PrimaryRouteWitness>,
    ) -> bool {
        witness.is_some_and(|witness| self.matches_route(&witness.route))
    }
    /// Whether `route` is the broker route this binding names.
    pub(crate) fn matches_route(&self, route: &crate::collaboration::RouteSpec) -> bool {
        self.broker_route().as_ref() == Some(route)
            || self.admission_route().as_ref() == Some(route)
    }
    pub(crate) fn drain_owned_tree(
        &self,
        widget: &mut dyn Widget,
        doc: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        if widget.callback_binding().is_some() {
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
    ) -> Result<crate::undo::UndoStatus, BoundDispatchError> {
        self.with_record_frame(outer, false, |doc, _| {
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            Ok(frame.status(doc, &self.author, self.instance_id))
        })?
    }
    fn generation(&self) -> Result<Option<u64>, BoundDispatchError> {
        if self.check_record()?.is_some() {
            return ACTIVE.with_borrow(|active| {
                active
                    .as_ref()
                    .and_then(|entry| entry.3.as_ref())
                    .and_then(|handle| handle.borrow().as_ref().map(|frame| frame.state.generation))
                    .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))
            });
        }
        self.records
            .action_generation(&self.checkout.id)
            .map_err(BoundDispatchError::Record)
    }
    pub(crate) fn drain_widget_actions(
        &self,
        widget: &mut dyn Widget,
        doc: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        let Some(before) = crate::undo::take_committed_action(widget) else {
            return Ok(false);
        };
        let keep_open = crate::undo::keeps_undo_step_open(widget);
        let metadata = crate::undo::get_state(widget);
        let handle = ACTIVE
            .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
            .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
        {
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            if !frame.operations.is_empty() {
                return Ok(true);
            }
            frame.settle_segment(doc);
            if frame
                .state
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
            // reported by an ancestor ActionHost. Only advance the pending
            // boundary; never move it backwards across a recorded action.
            frame.commit_physical(doc);
            let advances = frame.state.pending.as_ref().is_none_or(|pending| {
                before == pending.before
                    || doc.get_changes(&before).len() < doc.get_changes(&pending.before).len()
            });
            if advances {
                frame.finish_pending_at(before.clone(), doc);
                frame.state.pending = Some(PendingRecordAction::new(self, before));
            }
            if let Some(pending) = frame.state.pending.as_mut() {
                pending.metadata = metadata;
            }
        }
        // A step the widget keeps open closes at its next boundary, or
        // before an undo, a save or a merge.
        if !keep_open {
            self.finish_action_boundary(doc)?;
        }
        Ok(true)
    }

    /// Close a step the widget keeps open (a word being typed) through the
    /// ordinary mutation entry, as an undo, a save or a merge does, so a
    /// restore reviewed afterwards starts at a finished action boundary.
    pub(crate) fn close_open_step(&self, outer: &mut AutoCommit) -> Result<(), BoundDispatchError> {
        self.with_document(outer, |doc, _| self.finish_action_boundary(doc))?
    }

    pub(crate) fn history_request(
        &self,
        outer: &mut AutoCommit,
        request: crate::undo::UndoRequest,
    ) -> Result<Option<crate::state::UndoEntry>, BoundDispatchError> {
        self.history_request_with_registry(outer, request, None)
    }

    /// Outside changes need a new admitted history invocation: a local
    /// stack inversion has no current broker caller for its consequence.
    pub(crate) fn history_uses_compensation(
        &self,
        outer: &mut AutoCommit,
    ) -> Result<bool, BoundDispatchError> {
        self.with_record_frame(outer, false, |doc, _| {
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

    pub(crate) fn history_request_with_registry(
        &self,
        outer: &mut AutoCommit,
        request: crate::undo::UndoRequest,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Result<Option<crate::state::UndoEntry>, BoundDispatchError> {
        self.with_document(outer, |doc, _| {
            self.finish_action_boundary(doc)?;
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            let mut owned = handle.borrow_mut();
            let frame = owned
                .as_mut()
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            let entry = match request {
                crate::undo::UndoRequest::Undo => frame.history.undo_in_with_registry(
                    doc,
                    &self.author,
                    self.instance_id,
                    None,
                    None,
                    registry,
                ),
                crate::undo::UndoRequest::Redo => frame.history.redo_in_with_registry(
                    doc,
                    &self.author,
                    self.instance_id,
                    None,
                    None,
                    registry,
                ),
            };
            frame.state.pending = Some(PendingRecordAction::new(self, doc.get_heads()));
            Ok(entry)
        })?
    }
    /// Immediate action boundary inside the already admitted owned frame. The
    /// explicit Doc witness is checked before any frame access; no references
    /// are recovered from ACTIVE.
    fn finish_action_boundary(&self, doc: &mut AutoCommit) -> Result<(), BoundDispatchError> {
        let active = self.check_record()?;
        if !active.is_some_and(|pointer| std::ptr::eq(pointer, doc)) {
            return Err(BoundDispatchError::Record(RecordOpenError::Busy));
        }
        let handle = ACTIVE
            .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
            .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
        let mut owned = handle.borrow_mut();
        let frame = owned
            .as_mut()
            .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
        frame.settle_segment(doc);
        let heads = frame.commit_physical(doc);
        frame.finish_pending_at(heads.clone(), doc);
        // Continuation after an immediate action starts at its after-boundary.
        frame.state.pending = Some(PendingRecordAction::new(self, heads));
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
    fn check_record(&self) -> Result<Option<*const AutoCommit>, BoundDispatchError> {
        if self.closing.get() || !self.lifetime.is_active() {
            return Err(BoundDispatchError::ClosedLifetime);
        }
        ACTIVE.with_borrow(|active| match active {
            Some((records, checkout, document, _)) => {
                if self.records.same_owner(records) && self.checkout == *checkout {
                    Ok(Some(*document))
                } else {
                    Err(BoundDispatchError::CrossRecordReentry)
                }
            }
            None => Ok(None),
        })
    }
    pub(crate) fn enter(&self) -> Result<WakeScopeGuard, BoundDispatchError> {
        self.check_record()?;
        self.lifetime
            .enter_if_open()
            .map_err(|_| BoundDispatchError::ClosedLifetime)
    }
    pub(crate) fn with_document<R>(
        &self,
        outer: &mut AutoCommit,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        #[cfg(test)]
        document_admission_probe::record(self);
        self.with_record_frame(outer, true, callback)
    }
    /// Lend typed edits without opening a mutation merely because a frame polls.
    /// The executor activates this owner when a typed action actually writes.
    pub(crate) fn with_settlement<R>(
        &self,
        outer: &mut AutoCommit,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        #[cfg(test)]
        document_admission_probe::record(self);
        self.with_record_frame(outer, false, |document, root| {
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            let previous = {
                let mut owned = handle.borrow_mut();
                let frame = owned
                    .as_mut()
                    .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
                // An exclusive loan still advances admission bookkeeping;
                // only its document transaction waits for an actual action.
                frame.state.generation = frame
                    .state
                    .generation
                    .and_then(|value| value.checked_add(1));
                frame.mutation.replace(ActiveMutation {
                    binding: self.clone(),
                    before: None,
                    deferred: true,
                })
            };
            let restore = SettlementReturn {
                document,
                frame: handle,
                previous,
            };
            Ok(callback(restore.document, root))
        })?
    }
    /// Restore only through the already owned record frame. Unlike ordinary
    /// mutation entry this never activates or flushes an unfinished action.
    #[cfg(test)]
    pub(crate) fn restore_prepared(
        &self,
        outer: &mut AutoCommit,
        admitted: &crate::state::owned_restore::ConfirmedRestore<'_>,
        prepared: &crate::history_mode::restore::PreparedContentRestore,
    ) -> Result<
        crate::state::owned_restore::OwnedActionReceipt,
        crate::state::owned_restore::OwnedRestoreError,
    > {
        self.restore_write(
            outer,
            admitted,
            crate::state::owned_restore::RestoreWrite::Content(prepared),
        )
    }
    pub(crate) fn restore_write(
        &self,
        outer: &mut AutoCommit,
        admitted: &crate::state::owned_restore::ConfirmedRestore<'_>,
        write: crate::state::owned_restore::RestoreWrite<'_>,
    ) -> Result<
        crate::state::owned_restore::OwnedActionReceipt,
        crate::state::owned_restore::OwnedRestoreError,
    > {
        use crate::state::owned_restore::{OwnedContentAction, OwnedRestoreError, same_frontier};
        self.with_record_frame(outer, false, |doc, _| {
            // with_record_frame checked the explicit Doc witness, owner and
            // lifetime, and retains their synchronous exclusive admission.
            let handle = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(OwnedRestoreError::ActiveAction)?;
            let mut owned = handle.borrow_mut();
            let frame = owned.as_mut().ok_or(OwnedRestoreError::ActiveAction)?;
            if frame.mutation.is_some() || !frame.operations.is_empty() {
                return Err(OwnedRestoreError::ActiveAction);
            }
            if doc.pending_ops() != 0 {
                return Err(OwnedRestoreError::PendingEdits);
            }
            if frame
                .state
                .pending
                .as_ref()
                .is_some_and(|pending| !same_frontier(&pending.before, &doc.base_heads()))
            {
                return Err(OwnedRestoreError::UnfinishedActionBoundary);
            }
            let next_generation = frame
                .state
                .generation
                .and_then(|generation| generation.checked_add(1))
                .ok_or(OwnedRestoreError::GenerationExhausted)?;
            frame.switch_writer(doc, frame.writer_of(self));
            let receipt = OwnedContentAction::begin_clean(
                doc,
                &mut frame.history,
                write.current_heads(),
                admitted,
                Some(self.instance_id),
            )?
            .apply(write)?;
            if receipt.changed {
                frame.state.generation = Some(next_generation);
                frame.reset_operation_baseline(self, receipt.after.clone());
            }
            Ok(receipt)
        })
        .map_err(OwnedRestoreError::Bound)?
    }
    pub(crate) fn with_record_frame<R>(
        &self,
        outer: &mut AutoCommit,
        mutation: bool,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        let active = self.check_record()?;
        let _scope = self.enter()?;
        if let Some(document) = active {
            if !std::ptr::eq(document, outer) {
                // Reentry must carry the currently borrowed document, never reconstruct
                // a mutable reference from a saved pointer.
                return Err(BoundDispatchError::Record(RecordOpenError::Busy));
            }
            let frame = ACTIVE
                .with_borrow(|active| active.as_ref().and_then(|entry| entry.3.clone()))
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            return if mutation {
                self.mutate_in_frame(outer, &frame, callback)
            } else {
                Ok(callback(outer, &ROOT))
            };
        }
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
                // Outside a loan the record writes as its own writer, which
                // is what its store persists and other mounts note.
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
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
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
        let active = self.check_record()?;
        let _scope = self.enter()?;
        if let Some(document) = active {
            if !std::ptr::eq(document, outer) {
                return Err(BoundDispatchError::Record(RecordOpenError::Busy));
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

struct BoundNode {
    pointer: *mut dyn Widget,
    semantic_name: String,
    binding: Option<CallbackBinding>,
    legacy: Option<crate::history_mode::HistoryOwner>,
    inactive: bool,
}

/// Legacy tiles retain their real record custody; they do not impersonate a
/// canonical component. The existing owned guard still supplies the only write.
pub(crate) fn restore_legacy_history(
    store: &RecordStore,
    checkout: &RecordCheckout,
    admitted: &crate::state::owned_restore::ConfirmedRestore<'_>,
    write: crate::state::owned_restore::RestoreWrite<'_>,
) -> Result<
    crate::state::owned_restore::OwnedActionReceipt,
    crate::state::owned_restore::OwnedRestoreError,
> {
    use crate::state::owned_restore::{OwnedContentAction, OwnedRestoreError, same_frontier};
    store
        .open_existing_record_checked(checkout)
        .map_err(|error| OwnedRestoreError::Bound(BoundDispatchError::Record(error)))?;
    store
        .with_action_state(&checkout.id, |doc, history, state| {
            if state
                .pending
                .as_ref()
                .is_some_and(|pending| !same_frontier(&pending.before, &doc.base_heads()))
            {
                return Err(OwnedRestoreError::UnfinishedActionBoundary);
            }
            let next = state
                .generation
                .and_then(|value| value.checked_add(1))
                .ok_or(OwnedRestoreError::GenerationExhausted)?;
            let receipt = OwnedContentAction::begin_clean(
                doc,
                history,
                write.current_heads(),
                admitted,
                None,
            )?
            .apply(write)?;
            if receipt.changed {
                state.generation = Some(next);
                state.pending = None;
                state.statuses.clear();
            }
            Ok(receipt)
        })
        .map_err(|error| OwnedRestoreError::Bound(BoundDispatchError::Record(error)))?
}
/// A read-only invalidation stamp. It contains no document text and confers no
/// write authority; an unavailable stamp must invalidate deferred insertion.
#[derive(Clone, PartialEq, Eq)]
pub struct DocumentEditStamp {
    owner: Option<(uuid::Uuid, u64)>,
    frontier: Vec<ChangeHash>,
    pending: usize,
}
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
        if let Some((owner, generation)) = self.owner {
            hash.update(owner.as_bytes());
            hash.update(&generation.to_le_bytes());
        }
        hash.update(&self.pending.to_le_bytes());
        for head in &self.frontier {
            hash.update(&head.0);
        }
        hash.update(&field);
        *hash.finalize().as_bytes()
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

/// Admission metadata for precisely one live widget/Taffy layout.
#[derive(Default)]
pub struct BoundNodeMap {
    nodes: HashMap<NodeId, BoundNode>,
    parents: HashMap<NodeId, NodeId>,
    // Persistent across layout clears; aliases are allocated only for a new
    // owner/lifetime + local node, never once per redraw or tree rebuild.
    a11y_aliases: a11y_owner::OwnerAliases,
}
impl BoundNodeMap {
    /// Read the nearest canonical record owner's monotonic edit generation.
    /// Local writes, remote merges and undo/redo invalidate it, including an
    /// edit followed by its inverse. Reading does not open an action or commit.
    /// # Errors
    /// Rejects missing/inactive nodes, retired owners, borrowed records and
    /// exhausted generations; callers must cancel deferred text insertion.
    pub fn document_edit_stamp(
        &self,
        node: NodeId,
        document: &AutoCommit,
    ) -> Result<DocumentEditStamp, BoundDispatchError> {
        let entry = self
            .nodes
            .get(&node)
            .filter(|entry| !entry.inactive)
            .ok_or(BoundDispatchError::MissingNode)?;
        let owner = if let Some(binding) = &entry.binding {
            let generation = binding
                .generation()?
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            Some((binding.lifetime_id, generation))
        } else if let Some(legacy) = &entry.legacy {
            legacy
                .validate()
                .map_err(|_| BoundDispatchError::ClosedLifetime)?;
            let (store, checkout) = legacy.store_checkout();
            let generation = store
                .action_generation(&checkout.id)
                .map_err(BoundDispatchError::Record)?
                .ok_or(BoundDispatchError::Record(RecordOpenError::Busy))?;
            Some((
                legacy
                    .lifetime_id()
                    .ok_or(BoundDispatchError::ClosedLifetime)?,
                generation,
            ))
        } else {
            None
        };
        Ok(if owner.is_some() {
            DocumentEditStamp {
                owner,
                frontier: Vec::new(),
                pending: 0,
            }
        } else {
            DocumentEditStamp::for_document(document)
        })
    }
    /// Current admitted layout ancestry, including structural non-widget nodes.
    pub(crate) fn is_descendant(&self, mut node: NodeId, ancestor: NodeId) -> bool {
        while let Some(parent) = self.parents.get(&node) {
            if *parent == ancestor {
                return true;
            }
            node = *parent;
        }
        false
    }
    pub(crate) fn binding_for(&self, node: NodeId) -> Option<CallbackBinding> {
        self.nodes
            .get(&node)
            .filter(|node| !node.inactive)
            .and_then(|node| node.binding.clone())
    }
    pub(crate) fn legacy_for(&self, node: NodeId) -> Option<crate::history_mode::HistoryOwner> {
        self.nodes.get(&node).and_then(|entry| entry.legacy.clone())
    }
    pub(crate) fn legacy_status(
        &self,
        node: NodeId,
        author: &UserId,
    ) -> Option<crate::undo::UndoStatus> {
        let owner = self.legacy_for(node)?;
        owner.validate().ok()?;
        let (store, checkout) = owner.store_checkout();
        // A status read is not a mutable loan: it must not advance the
        // record's action generation.
        store.read_history(&checkout.id, |doc, history| {
            crate::undo::status_snapshot(doc, history, Some(author))
        })
    }
    pub(crate) fn owns_document(&self, node: NodeId) -> bool {
        self.nodes
            .get(&node)
            .is_some_and(|entry| entry.binding.is_some() || entry.legacy.is_some())
    }
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.parents.clear();
    }
    /// Admit raw nodes using their nearest boundary in the matching layout topology.
    ///
    /// # Safety
    /// Root, tree, layout root and pointers must belong to the same live build under
    /// one exclusive tree owner. No replacement, rebuild or destruction may occur.
    pub unsafe fn rebuild(
        &mut self,
        root: &dyn Widget,
        taffy: &TaffyTree<()>,
        layout_root: NodeId,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        // SAFETY: the public contract covers the single ownership/presentation root.
        unsafe {
            self.rebuild_with_retained_root(root, None, taffy, layout_root, nodes);
        }
    }

    /// Collect custody from a separately retained live root while inheriting
    /// bindings only through the presentation layout. Chrome stays unbound.
    /// # Safety
    /// Both roots and every admitted pointer belong to the same exclusively
    /// owned settled composition; neither root may be replaced during this call.
    pub(crate) unsafe fn rebuild_with_retained_root(
        &mut self,
        root: &dyn Widget,
        retained: Option<&dyn Widget>,
        taffy: &TaffyTree<()>,
        layout_root: NodeId,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        // SAFETY: the caller owns all supplied roots and layout pointers.
        unsafe { self.rebuild_roots_with_retained(&[(root, layout_root)], retained, taffy, nodes) };
    }

    /// Admit disjoint presentation roots without inheriting authority across roots.
    /// # Safety
    /// Every root and node must belong to the same exclusively owned live arena.
    pub unsafe fn rebuild_roots(
        &mut self,
        roots: &[(&dyn Widget, NodeId)],
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        // SAFETY: the caller supplies the shared live arena and disjoint subtrees.
        unsafe { self.rebuild_roots_with_retained(roots, None, taffy, nodes) };
    }

    pub(crate) unsafe fn rebuild_roots_with_retained(
        &mut self,
        roots: &[(&dyn Widget, NodeId)],
        retained: Option<&dyn Widget>,
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        fn collect(
            widget: &dyn Widget,
            boundaries: &mut HashMap<NodeId, crate::history_mode::HistoryOwner>,
            inactive: &mut std::collections::HashSet<NodeId>,
        ) {
            if let Some(node) = widget.inactive_callback_boundary() {
                inactive.insert(node);
            }
            if let Some((node, owner)) = widget.legacy_callback_boundary() {
                boundaries.insert(node, owner);
            }
            for child in widget.children() {
                collect(child, boundaries, inactive);
            }
        }
        self.clear();
        let mut boundaries = HashMap::new();
        let mut inactive_boundaries = std::collections::HashSet::new();
        for (root, _) in roots {
            collect(*root, &mut boundaries, &mut inactive_boundaries);
        }
        if let Some(retained) = retained {
            collect(retained, &mut boundaries, &mut inactive_boundaries);
        }
        let mut pending: Vec<_> = roots
            .iter()
            .map(|(root, layout_root)| (*layout_root, root.callback_binding(), None, false))
            .collect();
        while let Some((node, inherited, inherited_legacy, inherited_inactive)) = pending.pop() {
            let inactive = inherited_inactive || inactive_boundaries.contains(&node);
            let mut legacy = boundaries.get(&node).cloned().or(inherited_legacy);
            let binding = if let Some(&pointer) = nodes.get(&node) {
                // SAFETY: rebuild's owner guarantees all admitted pointers are live.
                let widget = unsafe { &*pointer };
                let binding = if widget.resets_callback_binding() {
                    None
                } else {
                    widget.callback_binding().or(inherited)
                };
                if binding.is_some() || widget.resets_callback_binding() {
                    legacy = None;
                }
                self.nodes.insert(
                    node,
                    BoundNode {
                        pointer,
                        semantic_name: widget.name().to_owned(),
                        binding: binding.clone(),
                        legacy: legacy.clone(),
                        inactive,
                    },
                );
                binding
            } else {
                inherited
            };
            if let Ok(children) = taffy.children(node) {
                for child in children {
                    self.parents.insert(child, node);
                    pending.push((child, binding.clone(), legacy.clone(), inactive));
                }
            }
        }
        self.a11y_aliases.admit(&self.nodes);
    }
}

#[path = "bound_dispatch/a11y_owner.rs"]
mod a11y_owner;

/// Invoke an admitted node under its original component lifetime and record.
///
/// # Errors
/// Refuses missing, replaced, closed or synchronously cross-record targets.
///
/// # Safety
/// For admitted open or explicitly unbound delivery, the caller exclusively owns
/// the matching live tree/topology with no overlapping target or ancestor reference.
/// No replacement, rebuild or destruction may occur during delivery; no widget
/// reference or callback may escape. Closed entries may retain their actual old
/// pointers after destruction: metadata and lifetime are checked before dereference.
#[allow(
    clippy::implicit_hasher,
    reason = "shared producer/consumer contract uses the existing default-hasher node map"
)]
pub unsafe fn invoke_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    outer: &mut AutoCommit,
    outer_root: &ObjId,
    callback: impl FnOnce(&mut dyn Widget, &mut AutoCommit, &ObjId) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings
        .nodes
        .get(&node)
        .ok_or(BoundDispatchError::MissingNode)?;
    let pointer = nodes.get(&node).ok_or(BoundDispatchError::MissingNode)?;
    if !std::ptr::eq(admitted.pointer, *pointer) {
        return Err(BoundDispatchError::MissingNode);
    }
    if admitted.inactive {
        return Err(BoundDispatchError::MissingNode);
    }
    if let Some(binding) = &admitted.binding {
        binding.with_document(outer, |document, root| {
            // SAFETY: admission and lifetime were checked before this dereference;
            // the caller guarantees live topology and exclusive widget access.
            let widget = unsafe { &mut **pointer };
            let result = callback(widget, document, root);
            let _ = binding.drain_owned_tree(widget, document);
            result
        })
    } else if let Some(owner) = &admitted.legacy {
        owner
            .validate()
            .map_err(|_| BoundDispatchError::ClosedLifetime)?;
        let (store, checkout) = owner.store_checkout();
        store
            .with_checked(&checkout.id, |doc| {
                callback(unsafe { &mut **pointer }, doc, &ROOT)
            })
            .map_err(BoundDispatchError::Record)
    } else {
        // Explicitly admitted unbound widgets preserve the outer document.
        Ok(callback(unsafe { &mut **pointer }, outer, outer_root))
    }
}

/// Enter a retained boundary for the complete duration of a tree walk.
pub(crate) fn enter_widget(
    widget: &dyn Widget,
) -> Result<Option<WakeScopeGuard>, BoundDispatchError> {
    widget
        .callback_binding()
        .map(|binding| binding.enter())
        .transpose()
}

impl BoundNodeMap {
    fn admitted(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        node: NodeId,
    ) -> Result<&BoundNode, BoundDispatchError> {
        let admitted = self
            .nodes
            .get(&node)
            .ok_or(BoundDispatchError::MissingNode)?;
        let pointer = nodes.get(&node).ok_or(BoundDispatchError::MissingNode)?;
        if !std::ptr::eq(admitted.pointer, *pointer) {
            return Err(BoundDispatchError::MissingNode);
        }
        if let Some(owner) = &admitted.legacy {
            owner
                .validate()
                .map_err(|_| BoundDispatchError::ClosedLifetime)?;
        }
        Ok(admitted)
    }
}

/// Shared inspections never borrow the widget's canonical document.
/// # Safety
/// Same live-tree and stable-topology requirements as `invoke_bound_node`;
/// only shared target references may overlap and none may escape.
pub(crate) unsafe fn inspect_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&dyn Widget) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings.admitted(nodes, node)?;
    let _entry = admitted
        .binding
        .as_ref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller owns the stable live tree.
    Ok(callback(unsafe { &*admitted.pointer }))
}

/// Measure or settle one node's geometry. History leaves the live app on
/// screen beside its timeline, inert but visible, so layout still reaches
/// it: only input, focus and effects stop at an inactive boundary.
/// # Safety
/// Same exclusive target/tree requirements as `invoke_bound_node`.
pub(crate) unsafe fn layout_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&mut dyn Widget) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings.admitted(nodes, node)?;
    let _entry = admitted
        .binding
        .as_ref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller exclusively owns this target.
    Ok(callback(unsafe { &mut *admitted.pointer }))
}

/// Non-document mutations (focus/measurement) enter the same lifetime.
/// # Safety
/// Same exclusive target/tree requirements as `invoke_bound_node`.
pub(crate) unsafe fn mutate_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&mut dyn Widget) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings.admitted(nodes, node)?;
    if admitted.inactive {
        return Err(BoundDispatchError::MissingNode);
    }
    let _entry = admitted
        .binding
        .as_ref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller exclusively owns this target.
    Ok(callback(unsafe { &mut *admitted.pointer }))
}

/// Explicit compatibility mode for retained helpers; component adapters still
/// enforce their own entry when called by an unbound legacy traversal.
pub(crate) fn enter_traversal<const BOUND: bool>(
    widget: &dyn Widget,
) -> Result<Option<WakeScopeGuard>, BoundDispatchError> {
    if BOUND {
        enter_widget(widget)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
#[path = "bound_dispatch/owned_restore_tests.rs"]
mod owned_restore_tests;

/// Remote delivery is not a local action. A physical or logical edit must be
/// finished by its existing owner; the network queue never commits it.
pub(crate) fn can_merge_remote_owned_document(
    store: &RecordStore,
    checkout: &RecordCheckout,
) -> Result<bool, crate::history_mode::routes::RemoteApplyError> {
    use crate::history_mode::routes::RemoteApplyError;
    store
        .open_existing_record_checked(checkout)
        .map_err(|error| RemoteApplyError::Unavailable(error.to_string()))?;
    store
        .with_action_state(&checkout.id, |doc, _, state| {
            doc.pending_ops() == 0
                && !state.pending.as_ref().is_some_and(|pending| {
                    !crate::state::owned_restore::same_frontier(&pending.before, &doc.base_heads())
                })
                && state
                    .generation
                    .and_then(|value| value.checked_add(1))
                    .is_some()
        })
        .map_err(|error| RemoteApplyError::Unavailable(error.to_string()))
}

pub(crate) fn merge_remote_owned_document(
    store: &RecordStore,
    checkout: &RecordCheckout,
    mutation: &collab_types::DocumentMutation,
) -> Result<Option<bool>, crate::history_mode::routes::RemoteApplyError> {
    use crate::history_mode::routes::RemoteApplyError;
    store
        .open_existing_record_checked(checkout)
        .map_err(|error| RemoteApplyError::Unavailable(error.to_string()))?;
    let identity = store
        .replica_identity_stamp(checkout)
        .map_err(RemoteApplyError::Unavailable)?;
    store
        .with_action_state(&checkout.id, |doc, history, state| {
            if doc.pending_ops() != 0
                || state.pending.as_ref().is_some_and(|pending| {
                    !crate::state::owned_restore::same_frontier(&pending.before, &doc.base_heads())
                })
            {
                return Ok(None);
            }
            let next = state
                .generation
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    RemoteApplyError::Unavailable("document generation exhausted".into())
                })?;
            let mut candidate = doc.clone();
            let observed_before = state
                .invocation_provenance
                .borrow()
                .is_required()
                .then(|| doc.base_heads());
            let changed = crate::history_mode::routes::merge_committed(&mut candidate, mutation)?;
            identity
                .validate(&candidate)
                .map_err(RemoteApplyError::InvalidMutation)?;
            if changed {
                if let Some(before) = observed_before {
                    let applied = candidate
                        .get_changes(&before)
                        .into_iter()
                        .map(|change| change.hash())
                        .collect::<Vec<_>>();
                    // Observe only the actual newly merged range. Existing
                    // unscoped local edits cannot acquire another caller or
                    // become trusted merely because remote data arrived.
                    state
                        .invocation_provenance
                        .borrow_mut()
                        .observe_admitted(&applied);
                }
                *doc = candidate;
                // A clean continuation marker is not unfinished typing. Retire it
                // after the merge so a later local burst starts at the remote heads.
                state.pending = None;
                history.rebase(doc);
                state.statuses.clear();
                state.generation = Some(next);
            }
            Ok(Some(changed))
        })
        .map_err(|error| RemoteApplyError::Unavailable(error.to_string()))?
}

/// Legacy semantic delivery retains the actual record and ordinary `UndoLog`.
/// Presentation-only verbs use a non-committing loan. Mutating verbs authorize
/// at execution and finish exactly one addressed record action, never outer `State`.
/// # Safety
/// Same stable, exclusive tree contract as `invoke_bound_node`.
pub(crate) unsafe fn invoke_bound_semantic_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    action: &crate::a11y::Action,
    outer: &mut crate::state::State,
    caller: SemanticCaller<'_>,
    callback: impl FnOnce(&mut dyn Widget, &mut AutoCommit, &ObjId) -> R,
) -> Result<R, String> {
    use crate::a11y::Action;
    let SemanticCaller { authority, author } = caller;
    let admitted = bindings
        .admitted(nodes, node)
        .map_err(|error| format!("{error:?}"))?;
    if admitted.inactive {
        return Err("This live control is hidden by History.".into());
    }
    let effect = matches!(
        action,
        Action::Activate
            | Action::ToggleCheck
            | Action::Increment
            | Action::Decrement
            | Action::SetValue(_)
    );
    let Some(owner) = admitted.legacy.as_ref().filter(|_| effect) else {
        return unsafe {
            invoke_bound_node(bindings, nodes, node, &mut outer.doc, &outer.root, callback)
        }
        .map_err(|error| format!("{error:?}"));
    };
    let app = authorize_legacy_owner(owner, authority, author)?;
    let (store, checkout) = owner.store_checkout();
    let pointer = admitted.pointer;
    let context = tilo_actions::ActionContext::for_resource(
        "Edit",
        checkout.id.as_str(),
        "document",
        "Document",
    )
    .with_app(&app, &app);
    let (result, changed) = store
        .with_action_state(&checkout.id, |doc, history, state| {
            if state.pending.is_some() {
                return Err("The document has an unfinished owned action.".to_owned());
            }
            let next = state
                .generation
                .and_then(|generation| generation.checked_add(1))
                .ok_or("document generation exhausted")?;
            if history.revision() == u64::MAX {
                return Err("document history exhausted".into());
            }
            let before = (doc.base_heads(), doc.pending_ops());
            let widget = unsafe { &mut *pointer };
            let result = callback(widget, doc, &ROOT);
            let frontier_changed = (doc.base_heads(), doc.pending_ops()) != before;
            // Typed actions close their transaction at dispatch. Forwarding
            // controls can leave the bracket on a descendant, so retain the
            // admitted owner's frontier as a fallback for committed edits.
            let committed = crate::undo::take_committed_action(widget)
                .or_else(|| (doc.pending_ops() == 0).then_some(before.0));
            let changed = if !frontier_changed {
                false
            } else if let Some(committed) = committed {
                history.record_committed_action_authored(
                    doc,
                    committed,
                    None,
                    crate::undo::get_state(widget),
                    Some(author.clone()),
                )
            } else {
                history.commit_action_authored_with_message(
                    doc,
                    None,
                    crate::undo::get_state(widget),
                    Some(author.clone()),
                    context.change_message(),
                )
            };
            if changed {
                state.generation = Some(next);
                state.statuses.clear();
            }
            Ok((result, changed))
        })
        .map_err(|error| error.to_string())??;
    if changed {
        store.schedule_committed_history(&checkout.id);
    }
    Ok(result)
}

/// The local user a semantic action runs as, and the permissions that gate
/// its effect on an owner-bound node.
#[derive(Clone, Copy)]
pub(crate) struct SemanticCaller<'a> {
    pub(crate) authority: &'a crate::permissions::PermissionContext,
    pub(crate) author: &'a UserId,
}

fn authorize_legacy_owner(
    owner: &crate::history_mode::HistoryOwner,
    authority: &crate::permissions::PermissionContext,
    author: &UserId,
) -> Result<String, String> {
    use crate::permissions::{CapDecision, Principal, ResourceRef, Verb};
    owner.validate().map_err(|error| error.to_string())?;
    let (store, checkout) = owner.store_checkout();
    let (_, app, _) = store
        .history_namespace(&checkout.id)
        .map_err(|error| error.to_string())?;
    let current = authority.at_current_time()?;
    let mapped = current.actor().principals.contains(&author.principal())
        || current
            .actor()
            .principals
            .contains(&Principal::Person(crate::permissions::LOCAL_USER.into()));
    if !mapped
        || app.is_empty()
        || current.decide(&ResourceRef::App(app.clone()), Verb::Write) != CapDecision::Allow
    {
        return Err("You do not have permission to edit this document.".into());
    }
    if let Some(path) = checkout.id.file_path()
        && current.decide(
            &ResourceRef::FsPath(path.to_string_lossy().into_owned()),
            Verb::Write,
        ) != CapDecision::Allow
    {
        return Err("You do not have permission to edit this file.".into());
    }
    Ok(app)
}

pub(crate) fn legacy_history_request(
    owner: &crate::history_mode::HistoryOwner,
    request: crate::undo::UndoRequest,
    authority: &crate::permissions::PermissionContext,
    author: &UserId,
) -> Result<Option<crate::state::UndoEntry>, String> {
    authorize_legacy_owner(owner, authority, author)?;
    let (store, checkout) = owner.store_checkout();
    let result = store
        .with_action_state(&checkout.id, |doc, history, state| {
            if state.pending.is_some() {
                return Err("The document has an unfinished owned action.".to_owned());
            }
            let next = state
                .generation
                .and_then(|generation| generation.checked_add(1))
                .ok_or("document generation exhausted")?;
            if matches!(request, crate::undo::UndoRequest::Undo) && doc.pending_ops() != 0 {
                history.commit_action_authored(doc, None, None, Some(author.clone()));
            }
            let result = match request {
                crate::undo::UndoRequest::Undo => history.undo_as(doc, Some(author), None, None),
                crate::undo::UndoRequest::Redo => history.redo_as(doc, Some(author), None, None),
            };
            if result.is_some() {
                state.generation = Some(next);
                state.statuses.clear();
            }
            Ok(result)
        })
        .map_err(|error| error.to_string())??;
    if result.is_some() {
        store.schedule_committed_history(&checkout.id);
    }
    Ok(result)
}

/// # Safety
/// Caller owns the exact stable node map exclusively, as for `invoke_bound_node`.
pub(crate) unsafe fn drain_legacy_node_action(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    authority: &crate::permissions::PermissionContext,
    author: &UserId,
) -> Result<bool, String> {
    let admitted = bindings
        .admitted(nodes, node)
        .map_err(|error| format!("{error:?}"))?;
    if admitted.inactive {
        return Ok(false);
    }
    let Some(owner) = &admitted.legacy else {
        return Ok(false);
    };
    authorize_legacy_owner(owner, authority, author)?;
    let (store, checkout) = owner.store_checkout();
    let pointer = admitted.pointer;
    let changed = store
        .with_action_state(&checkout.id, |doc, history, state| {
            if state.pending.is_some() {
                return Err("The document has an unfinished owned action.".to_owned());
            }
            let next = state
                .generation
                .and_then(|generation| generation.checked_add(1))
                .ok_or("document generation exhausted")?;
            let widget = unsafe { &mut *pointer };
            let Some(before) = crate::undo::take_committed_action(widget) else {
                return Ok(false);
            };
            // A successful pure resource action may have already recorded the
            // frontier while an older no-op notification remains on the widget.
            // Retire that notification without duplicating or rebasing history.
            if doc.pending_ops() == 0 && history.matches_recorded_frontier(doc) {
                return Ok(false);
            }
            let changed = history.record_committed_action_authored(
                doc,
                before,
                None,
                crate::undo::get_state(widget),
                Some(author.clone()),
            );
            if changed {
                state.generation = Some(next);
                state.statuses.clear();
            }
            Ok(changed)
        })
        .map_err(|error| error.to_string())??;
    if changed {
        store.schedule_committed_history(&checkout.id);
    }
    Ok(changed)
}

#[cfg(test)]
#[path = "bound_dispatch/document_action_ack_tests.rs"]
mod document_action_ack_tests;

#[cfg(test)]
#[path = "bound_dispatch/legacy_document_tests.rs"]
pub(crate) mod legacy_document_tests;

#[cfg(test)]
#[path = "bound_dispatch/semantic_authority_tests.rs"]
mod semantic_authority_tests;

#[cfg(test)]
#[path = "bound_dispatch/author_writer_tests.rs"]
mod author_writer_tests;

#[cfg(test)]
#[path = "bound_dispatch/device_actor_tests.rs"]
mod device_actor_tests;

#[cfg(test)]
#[path = "bound_dispatch/native_text_tests.rs"]
mod native_text_tests;

#[cfg(test)]
pub(crate) mod document_admission_probe {
    use super::*;
    struct Observation {
        owner: std::rc::Weak<Cell<bool>>,
        closing: Rc<RefCell<Vec<bool>>>,
    }
    thread_local! { static CURRENT: RefCell<Option<Observation>> = const { RefCell::new(None) }; }
    pub(crate) struct Probe {
        previous: Option<Observation>,
        closing: Rc<RefCell<Vec<bool>>>,
    }
    impl Probe {
        pub(crate) fn new(binding: &CallbackBinding) -> Self {
            let closing = Rc::new(RefCell::new(Vec::new()));
            let previous = CURRENT.with_borrow_mut(|current| {
                current.replace(Observation {
                    owner: Rc::downgrade(&binding.closing),
                    closing: closing.clone(),
                })
            });
            Self { previous, closing }
        }
        pub(crate) fn attempts(&self) -> Vec<bool> {
            self.closing.borrow().clone()
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            CURRENT.with_borrow_mut(|current| *current = self.previous.take());
        }
    }
    pub(super) fn record(binding: &CallbackBinding) {
        CURRENT.with_borrow(|current| {
            if let Some(current) = current
                && std::rc::Weak::ptr_eq(&current.owner, &Rc::downgrade(&binding.closing))
            {
                current.closing.borrow_mut().push(binding.closing.get());
            }
        });
    }
}
