//! Exact retained-owner resource routing. Requests are toolkit-private and finite;
//! no widget pointer or mutable document loan escapes a traversal.
use super::{
    AutoCommit, BoundActionAddress, BoundActionTarget, CallbackBinding, PermissionContext, ROOT,
    Transactable, UserId, Widget, authorize_legacy_owner, encode_resource_action, execute_document,
    resolve_resource_invocation,
};
use crate::history_mode::HistoryOwner;
use tilo_actions::{ActionAddress, ControlInvocation, DoorError, OperationKind, Resource};

#[allow(
    clippy::needless_pass_by_value,
    reason = "normalize owned and borrowed error types into a refusal"
)]
fn refused(code: &str, reason: impl ToString) -> DoorError {
    DoorError::Unavailable {
        code: code.into(),
        reason: reason.to_string(),
    }
}
#[derive(Clone)]
pub(crate) struct LegacyResourceTarget {
    pub(crate) owner: HistoryOwner,
    pub(crate) address: ActionAddress,
}
#[derive(Clone)]
pub(crate) enum LocalResourceTarget {
    Outer,
    Component(BoundActionTarget),
    Legacy(LegacyResourceTarget),
}
#[derive(Clone)]
enum ResourceOwner {
    Legacy(HistoryOwner),
    Component(CallbackBinding),
}
struct OwnerResources {
    owner: ResourceOwner,
    resources: Vec<Resource>,
    interactive: bool,
}
pub(crate) struct ResourceReadRequest<'a> {
    address: Option<&'a ActionAddress>,
    target: Option<&'a HistoryOwner>,
    owners: Vec<OwnerResources>,
    hidden: bool,
    /// Whether component owners' declarations are read too. A legacy-only
    /// inventory leaves them to the cached component discovery.
    components: bool,
}
/// Collect just this document domain. Nested owners are never ambient children.
fn owned_resources<W: Widget + ?Sized>(
    root: &W,
    doc: &AutoCommit,
    address: Option<&ActionAddress>,
    resources: &mut Vec<Resource>,
) {
    // A delivered invocation resolves against its invoker's own view,
    // rebuilt now; outside one this is the widget's own view.
    let participant = crate::invocation_scope::current_participant();
    let local = crate::resources::resources_for(root, participant.as_ref(), doc);
    let eager_claim = address.is_some_and(|address| claims(&local, address) != 0);
    resources.extend(local);
    if let Some(address) = address
        && !eager_claim
        && let Some(resource) =
            crate::resources::resource_for(root, participant.as_ref(), doc, &address.resource)
    {
        resources.push(resource);
    }
    root.for_each_child(&mut |child| {
        if !child.has_separate_document_owner() {
            owned_resources(child, doc, address, resources);
        }
    });
}
fn provider<W: Widget + ?Sized>(
    root: &W,
    binding: &CallbackBinding,
    address: &ActionAddress,
) -> Option<Resource> {
    let mut found = crate::search::capability::search_providers(root)
        .into_iter()
        .filter(|p| {
            p.callback_binding()
                .as_ref()
                .is_none_or(|b| b.same_component(binding))
        })
        .find_map(|p| p.resolve_action_resource(address));
    root.for_each_child(&mut |child| {
        if found.is_none() && !child.has_separate_document_owner() {
            found = provider(child, binding, address);
        }
    });
    found
}
impl ResourceReadRequest<'_> {
    pub(crate) fn visit<W: Widget + ?Sized>(&mut self, root: &W) -> Result<(), DoorError> {
        if let Some(binding) = root.callback_binding() {
            if self.target.is_some() {
                let mut result = Ok(());
                root.for_each_child(&mut |child| {
                    if result.is_ok() {
                        result = child.visit_resource_owners(self);
                    }
                });
                return result;
            }
            binding
                .enter()
                .map_err(|e| refused("record_unavailable", format!("{e:?}")))?;
            if self.components && !self.owners.iter().any(|snapshot| matches!(&snapshot.owner, ResourceOwner::Component(other) if other.same_component(&binding))) {
                let resources = binding.read_document(&AutoCommit::new(), |doc, _| {
                    let mut resources = Vec::new();
                    // The boundary adapter forwards its root resources and also
                    // exposes that root as a child. Collect actual roots once.
                    root.for_each_child(&mut |child| {
                        if !child.has_separate_document_owner() {
                            owned_resources(child, doc, self.address, &mut resources);
                        }
                    });
                    if let Some(address) = self.address
                        && claims(&resources, address) == 0
                    {
                        root.for_each_child(&mut |child| {
                            if !child.has_separate_document_owner()
                                && let Some(resource) = provider(child, &binding, address)
                            {
                                resources.push(resource);
                            }
                        });
                    }
                    resources
                }).map_err(|e| refused("record_unavailable", format!("{e:?}")))?;
                self.owners.push(OwnerResources { owner: ResourceOwner::Component(binding), resources, interactive: !self.hidden });
            }
        } else if root.has_separate_document_owner() {
            return Err(refused(
                "record_unavailable",
                "Separate document custody is unavailable",
            ));
        }
        let mut result = Ok(());
        root.for_each_child(&mut |child| {
            if result.is_ok() {
                result = child.visit_resource_owners(self);
            }
        });
        result
    }
    pub(crate) fn tile(
        &mut self,
        owner: Option<HistoryOwner>,
        separate: bool,
        interactive: bool,
        live: &dyn Widget,
    ) -> Result<(), DoorError> {
        let old_hidden = self.hidden;
        self.hidden |= !interactive;
        let result = (|| {
            if separate && live.callback_binding().is_none() {
                if self.target.is_some_and(|target| {
                    owner.as_ref().is_none_or(|owner| !owner.same_owner(target))
                }) {
                    return live.visit_resource_owners(self);
                }
                let owner = owner.ok_or_else(|| {
                    refused(
                        "record_unavailable",
                        "Legacy document custody is unavailable",
                    )
                })?;
                owner
                    .validate()
                    .map_err(|e| refused("record_unavailable", e))?;
                if !self.owners.iter().any(|snapshot| matches!(&snapshot.owner, ResourceOwner::Legacy(other) if other.same_owner(&owner))) {
                    let (store, checkout) = owner.store_checkout();
                    let resources = store.read_checked(&checkout.id, |doc| {
                        let mut resources = Vec::new();
                        owned_resources(live, doc, self.address, &mut resources);
                        resources
                    }).map_err(|e| refused("record_unavailable", e))?;
                    self.owners.push(OwnerResources { owner: ResourceOwner::Legacy(owner), resources, interactive: !self.hidden });
                }
            }
            live.visit_resource_owners(self)
        })();
        self.hidden = old_hidden;
        result
    }
}
fn inventory(
    root: &dyn Widget,
    address: Option<&ActionAddress>,
    components: bool,
) -> Result<Vec<OwnerResources>, DoorError> {
    let mut request = ResourceReadRequest {
        address,
        target: None,
        owners: vec![],
        hidden: false,
        components,
    };
    root.visit_resource_owners(&mut request)?;
    Ok(request.owners)
}
fn claims(resources: &[Resource], address: &ActionAddress) -> usize {
    resources
        .iter()
        .map(|resource| {
            let own = if resource.id.as_str() == address.resource {
                resource
                    .actions
                    .iter()
                    .filter(|spec| spec.id == address.action)
                    .count()
            } else {
                0
            };
            own.saturating_add(claims(&resource.children, address))
        })
        .sum()
}
pub(crate) fn select_local_target(
    root: &dyn Widget,
    doc: &AutoCommit,
    address: &ActionAddress,
) -> Result<LocalResourceTarget, DoorError> {
    let mut resources = Vec::new();
    if !root.has_separate_document_owner() {
        owned_resources(root, doc, Some(address), &mut resources);
    }
    let mut count = claims(&resources, address);
    let mut target = LocalResourceTarget::Outer;
    let mut interactive = true;
    for snapshot in inventory(root, Some(address), true)? {
        let n = claims(&snapshot.resources, address);
        count = count.saturating_add(n);
        if n != 0 {
            interactive = snapshot.interactive;
            // Hidden claims still count: they can never donate fallback.
            target = match snapshot.owner {
                ResourceOwner::Legacy(owner) => LocalResourceTarget::Legacy(LegacyResourceTarget {
                    owner,
                    address: address.clone(),
                }),
                ResourceOwner::Component(binding) => {
                    LocalResourceTarget::Component(BoundActionTarget {
                        physical_text: false,
                        address: BoundActionAddress {
                            instance_id: binding.instance_id,
                            address: address.clone(),
                        },
                        binding,
                    })
                }
            };
        }
    }
    match count {
        0 => Err(DoorError::Unknown(address.clone())),
        1 if interactive => Ok(target),
        1 => Err(refused("hidden_document", "Live document is hidden")),
        _ => Err(refused(
            "ambiguous_action_target",
            "More than one current declaration claims this address",
        )),
    }
}
/// Fresh owned declarations for local presentation. Broker publication excludes
/// legacy Documents; their local transport is not broker-directed admission.
pub(crate) fn legacy_resources(
    root: &dyn Widget,
    authority: &PermissionContext,
    author: &UserId,
    broker: bool,
) -> Result<Vec<Resource>, DoorError> {
    let mut output = vec![];
    // Component declarations come from the cached component discovery.
    for snapshot in inventory(root, None, false)? {
        if let ResourceOwner::Legacy(owner) = snapshot.owner
            && snapshot.interactive
        {
            let (store, checkout) = owner.store_checkout();
            let (_, app, _) = store
                .history_namespace(&checkout.id)
                .map_err(|e| refused("record_unavailable", e))?;
            output.extend(snapshot.resources.into_iter().filter_map(|resource| {
                filter_legacy_resource(resource, &owner, &app, authority, author, broker)
            }));
        }
    }
    Ok(output)
}
fn current_authority(authority: &PermissionContext) -> Result<PermissionContext, DoorError> {
    authority
        .at_current_time()
        .map_err(|error| refused("permission_denied", error))
}
fn operation_allowed(
    authority: &PermissionContext,
    app: &str,
    spec: &tilo_actions::ActionSpec,
) -> bool {
    use crate::permissions::{CapDecision, ResourceRef, Verb};
    use tilo_actions::PermissionRequirement as P;
    let verb = match spec.contract.permission {
        P::None => return true,
        P::Read => Verb::Read,
        P::Comment => Verb::Comment,
        P::Write => Verb::Write,
        P::Invite => Verb::Invite,
        P::Admin => Verb::Admin,
    };
    authority.decide(&ResourceRef::App(app.into()), verb) == CapDecision::Allow
}
fn filter_legacy_resource(
    mut resource: Resource,
    owner: &HistoryOwner,
    app: &str,
    authority: &PermissionContext,
    author: &UserId,
    broker: bool,
) -> Option<Resource> {
    let current = current_authority(authority).ok()?;
    resource.actions.retain(|spec| {
        operation_allowed(&current, app, spec)
            && (spec.kind != OperationKind::Document
                || (!broker && authorize_legacy_owner(owner, &current, author).is_ok()))
    });
    resource.children = resource
        .children
        .into_iter()
        .filter_map(|r| filter_legacy_resource(r, owner, app, &current, author, broker))
        .collect();
    (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
}

pub(crate) struct LegacyDocumentServices<'a> {
    pub(crate) registry: &'a tilo_actions::ActionRegistry,
    pub(crate) transport: &'a mut dyn tilo_actions::ActionTransport,
    pub(crate) qualified_local_transport: bool,
    pub(crate) authority: &'a PermissionContext,
    pub(crate) author: &'a UserId,
}
pub(crate) struct LegacyInvocation<'a> {
    pub(crate) argument: Option<&'a str>,
    pub(crate) arguments: Option<&'a serde_json::Map<String, serde_json::Value>>,
    pub(crate) actor: &'a str,
    pub(crate) broker_admitted: bool,
    pub(crate) ambient: Option<&'a tilo_actions::ActionContext>,
}
pub(crate) struct LegacyDocumentOutcome {
    pub(crate) changed: bool,
    pub(crate) envelope: tilo_actions::ActionEnvelope,
}
#[derive(Default)]
pub(crate) struct LegacyControlBatch {
    pub(crate) controls: Vec<(LegacyResourceTarget, ControlInvocation)>,
    pub(crate) failures: Vec<DoorError>,
}
#[allow(
    clippy::large_enum_variant,
    reason = "finite synchronous stack request avoids another retained allocation"
)]
pub(crate) enum ResourceRequest<'a> {
    Drain(&'a mut LegacyControlBatch),
    Discard,
    Feedback {
        target: &'a LegacyResourceTarget,
        outcome: &'a Result<crate::widget::ResourceOperationStart, DoorError>,
        delivered: bool,
    },
    Refresh {
        authority: &'a PermissionContext,
        author: &'a UserId,
    },
    Document {
        target: &'a LegacyResourceTarget,
        services: LegacyDocumentServices<'a>,
        input: LegacyInvocation<'a>,
        scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
        result: Option<Result<LegacyDocumentOutcome, DoorError>>,
    },
    Effect {
        target: &'a LegacyResourceTarget,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        encoded: &'a tilo_actions::EncodedAction,
        outer: &'a mut AutoCommit,
        scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
        result: Option<Result<crate::widget::ResourceOperationStart, DoorError>>,
    },
}
impl ResourceRequest<'_> {
    /// Drain failures belong to one custody boundary; healthy siblings still run.
    pub(crate) fn visit<W: Widget + ?Sized>(&mut self, root: &mut W) -> Result<(), DoorError> {
        if matches!(
            self,
            Self::Feedback {
                delivered: true,
                ..
            }
        ) {
            return Ok(());
        }
        match root.visit_resource_owners_mut(self) {
            Err(error) if matches!(self, Self::Drain(_)) => {
                if let Self::Drain(batch) = self {
                    batch.failures.push(error);
                }
                Ok(())
            }
            result => result,
        }
    }
    pub(crate) fn effect_target(&self, owner: Option<&HistoryOwner>) -> bool {
        matches!(self, Self::Effect { target, result: None, .. } if owner.is_some_and(|owner| owner.same_owner(&target.owner)))
    }
    pub(crate) fn deliver_effect(&mut self, tile: &mut dyn Widget, interactive: bool) {
        if let Self::Effect {
            invocation,
            kind,
            encoded,
            outer,
            scope,
            result,
            target,
        } = self
        {
            *result = Some(if interactive {
                let routing = target.owner.broker_route().map(|route| {
                    crate::widget::resource_operation::OperationTarget::new(
                        route,
                        target.address.clone(),
                    )
                });
                let op = match scope {
                    Some(scope) => crate::widget::ResourceOperation::scoped(
                        scope,
                        *kind,
                        &encoded.type_id,
                        &encoded.payload,
                    ),
                    None => crate::widget::ResourceOperation::local(
                        *invocation,
                        *kind,
                        &encoded.type_id,
                        &encoded.payload,
                    ),
                };
                let op = op.with_target(routing.as_ref());
                tile.start_operation(&op, &mut crate::widget::Edits::lend(outer), &ROOT)
                    .map_err(DoorError::Failed)
                    .and_then(|value| {
                        value.ok_or_else(|| {
                            refused("unknown_action", "Selected owner declined the operation")
                        })
                    })
            } else {
                Err(refused("hidden_document", "Live document is hidden"))
            });
        }
    }
    /// `history_doors` are the tile's own History door actions, which its
    /// app's History control runs (see `history_mode::embedded::invoke_door`).
    pub(crate) fn tile(
        &mut self,
        owner: Option<&HistoryOwner>,
        separate: bool,
        interactive: bool,
        history_doors: &std::collections::HashSet<ActionAddress>,
        live: &mut dyn Widget,
    ) -> Result<(), DoorError> {
        if !interactive && !matches!(self, Self::Discard) {
            return Ok(());
        }
        if separate && live.callback_binding().is_none() {
            // Exact-target execution ignores unrelated unavailable siblings;
            // discovery/drain never interprets unavailable custody as outer.
            let selected = match self {
                Self::Document { target, .. }
                | Self::Effect { target, .. }
                | Self::Feedback { target, .. } => owner
                    .as_ref()
                    .is_some_and(|owner| owner.same_owner(&target.owner)),
                _ => true,
            };
            if selected {
                match self {
                    Self::Feedback {
                        target,
                        outcome,
                        delivered,
                    } if !*delivered => {
                        target
                            .owner
                            .validate()
                            .map_err(|error| refused("record_unavailable", error))?;
                        // Exact current owner only. The callback receives no document
                        // loan; default forwarding stops at nested owner boundaries.
                        crate::widget::controls::on_control_invocation_result(
                            live,
                            &target.address,
                            outcome,
                        );
                        *delivered = true;
                        return Ok(());
                    }
                    Self::Discard => {
                        while crate::widget::controls::take_control_invocation(live).is_some() {}
                    }
                    Self::Drain(output) if interactive => {
                        let owner = owner.ok_or_else(|| {
                            refused(
                                "record_unavailable",
                                "Legacy document custody is unavailable",
                            )
                        })?;
                        owner
                            .validate()
                            .map_err(|e| refused("record_unavailable", e))?;
                        while let Some(intent) =
                            crate::widget::controls::take_control_invocation(live)
                        {
                            output.controls.push((
                                LegacyResourceTarget {
                                    owner: owner.clone(),
                                    address: intent.address.clone(),
                                },
                                intent,
                            ));
                        }
                    }
                    Self::Refresh { authority, author }
                        if interactive
                            && !crate::history_mode::frame::FrameworkInputFrame::active() =>
                    {
                        if let Some(owner) = owner {
                            let (store, checkout) = owner.store_checkout();
                            let (_, app, _) = store
                                .history_namespace(&checkout.id)
                                .map_err(|e| refused("record_unavailable", e))?;
                            let resources = store
                                .read_checked(&checkout.id, |doc| {
                                    let mut resources = vec![];
                                    owned_resources(live, doc, None, &mut resources);
                                    resources
                                })
                                .map_err(|e| refused("record_unavailable", e))?;
                            let resources = resources
                                .into_iter()
                                .filter_map(|r| {
                                    filter_legacy_resource(r, owner, &app, authority, author, false)
                                })
                                .collect::<Vec<_>>();
                            let allowed = tilo_actions::door::survey(&resources)
                                .into_iter()
                                .filter(|d| d.contract.availability.is_available())
                                .map(|d| d.address)
                                .chain(history_doors.iter().cloned())
                                .collect();
                            crate::widget::controls::refresh_control_action_availability(
                                live, &allowed,
                            );
                        }
                    }
                    Self::Document {
                        target,
                        services,
                        input,
                        scope,
                        result,
                    } if result.is_none() => {
                        *result = Some(execute_owned(live, target, services, input, *scope));
                    }
                    _ => {}
                }
            }
        }
        live.visit_resource_owners_mut(self)
    }
}
/// Deliver queued-control feedback only to the still-mounted originating owner.
pub(crate) fn notify_legacy_control_invocation_result(
    root: &mut dyn Widget,
    target: &LegacyResourceTarget,
    outcome: &Result<crate::widget::ResourceOperationStart, DoorError>,
) -> Result<(), DoorError> {
    target
        .owner
        .validate()
        .map_err(|error| refused("record_unavailable", error))?;
    let mut request = ResourceRequest::Feedback {
        target,
        outcome,
        delivered: false,
    };
    root.visit_resource_owners_mut(&mut request)?;
    if matches!(
        request,
        ResourceRequest::Feedback {
            delivered: true,
            ..
        }
    ) {
        Ok(())
    } else {
        Err(refused(
            "closed_lifetime",
            "The originating control owner is no longer mounted or interactive",
        ))
    }
}

pub(crate) fn drain_legacy_controls(root: &mut dyn Widget) -> LegacyControlBatch {
    let mut output = LegacyControlBatch::default();
    let _ = ResourceRequest::Drain(&mut output).visit(root);
    output
}
pub(crate) fn discard_legacy_controls(root: &mut dyn Widget) {
    let _ = root.visit_resource_owners_mut(&mut ResourceRequest::Discard);
}
pub(crate) fn refresh_legacy_controls(
    root: &mut dyn Widget,
    authority: &PermissionContext,
    author: &UserId,
) -> Result<(), DoorError> {
    root.visit_resource_owners_mut(&mut ResourceRequest::Refresh { authority, author })
}
pub(crate) fn invoke_legacy_document<'a>(
    root: &mut dyn Widget,
    target: &'a LegacyResourceTarget,
    services: LegacyDocumentServices<'a>,
    input: LegacyInvocation<'a>,
) -> Result<LegacyDocumentOutcome, DoorError> {
    invoke_legacy_document_scoped(root, target, services, input, None)
}

pub(crate) fn invoke_legacy_document_scoped<'a>(
    root: &mut dyn Widget,
    target: &'a LegacyResourceTarget,
    services: LegacyDocumentServices<'a>,
    input: LegacyInvocation<'a>,
    scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
) -> Result<LegacyDocumentOutcome, DoorError> {
    let mut request = ResourceRequest::Document {
        target,
        services,
        input,
        scope,
        result: None,
    };
    root.visit_resource_owners_mut(&mut request)?;
    let ResourceRequest::Document { result, .. } = request else {
        return Err(refused(
            "invalid_owner_request",
            "Owner traversal changed request kind",
        ));
    };
    result.unwrap_or_else(|| {
        Err(refused(
            "closed_lifetime",
            "The captured legacy owner is no longer mounted",
        ))
    })
}
pub(crate) fn resolve_legacy_invocation(
    root: &dyn Widget,
    target: &LegacyResourceTarget,
) -> Result<Vec<Resource>, DoorError> {
    // This exact-target lookup must not depend on unrelated busy siblings.
    let mut request = ResourceReadRequest {
        address: Some(&target.address),
        target: Some(&target.owner),
        owners: vec![],
        hidden: false,
        components: true,
    };
    root.visit_resource_owners(&mut request)?;
    let snapshot = request.owners.into_iter().find(|snapshot| matches!(&snapshot.owner, ResourceOwner::Legacy(owner) if owner.same_owner(&target.owner))).ok_or_else(|| refused("closed_lifetime", "Captured owner is no longer mounted"))?;
    if !snapshot.interactive {
        return Err(refused("hidden_document", "Live document is hidden"));
    }
    if claims(&snapshot.resources, &target.address) != 1 {
        return Err(refused(
            "ambiguous_action_target",
            "The exact owner must advertise one matching action",
        ));
    }
    Ok(snapshot.resources)
}
pub(crate) fn invoke_legacy_effect(
    root: &mut dyn Widget,
    target: &LegacyResourceTarget,
    invocation: tilo_actions::InvocationId,
    kind: OperationKind,
    encoded: &tilo_actions::EncodedAction,
    outer: &mut AutoCommit,
) -> Result<crate::widget::ResourceOperationStart, DoorError> {
    invoke_legacy_effect_scoped(root, target, invocation, kind, encoded, outer, None)
}

pub(crate) fn invoke_legacy_effect_scoped(
    root: &mut dyn Widget,
    target: &LegacyResourceTarget,
    invocation: tilo_actions::InvocationId,
    kind: OperationKind,
    encoded: &tilo_actions::EncodedAction,
    outer: &mut AutoCommit,
    scope: Option<&crate::invocation_scope::ResourceInvocationScope>,
) -> Result<crate::widget::ResourceOperationStart, DoorError> {
    let mut request = ResourceRequest::Effect {
        target,
        invocation,
        kind,
        encoded,
        outer,
        scope,
        result: None,
    };
    root.visit_resource_owners_mut(&mut request)?;
    let ResourceRequest::Effect { result, .. } = request else {
        return Err(refused(
            "invalid_owner_request",
            "Owner traversal changed request kind",
        ));
    };
    result.unwrap_or_else(|| {
        Err(refused(
            "closed_lifetime",
            "Captured effect owner is no longer mounted",
        ))
    })
}
#[allow(
    clippy::too_many_lines,
    reason = "admission, dispatch and bookkeeping remain within one checked owner loan"
)]
fn execute_owned(
    root: &mut dyn Widget,
    target: &LegacyResourceTarget,
    services: &mut LegacyDocumentServices<'_>,
    input: &LegacyInvocation<'_>,
    scope: Option<&crate::invocation_scope::ResourceInvocationScope>,
) -> Result<LegacyDocumentOutcome, DoorError> {
    if scope.is_none()
        && (!services.qualified_local_transport
            || input.broker_admitted
            || input.actor != services.author.0)
    {
        return Err(refused(
            "legacy_document_route_unsupported",
            "Legacy documents require the local author and built-in local transport",
        ));
    }
    let current = current_authority(services.authority)?;
    let app = authorize_legacy_owner(&target.owner, &current, services.author)
        .map_err(|e| refused("permission_denied", e))?;
    let (store, checkout) = target.owner.store_checkout();
    let admission = store
        .admission(checkout)
        .map_err(|_| refused("closed_lifetime", "Record custody changed"))?;
    let outcome = store
        .with_action_state(&checkout.id, |doc, history, state| {
            let mut execute = |doc: &mut AutoCommit| {
                if target.owner.lifetime_id().is_none() || !admission.matches(doc, checkout) {
                    return Err(refused("closed_lifetime", "Record custody changed"));
                }
                if state.pending.is_some()
                    || doc.pending_ops() != 0
                    || !history.matches_recorded_frontier(doc)
                {
                    return Err(refused(
                        "record_action_pending",
                        "The record has an unfinished edit",
                    ));
                }
                let next = state
                    .generation
                    .and_then(|g| g.checked_add(1))
                    .ok_or_else(|| {
                        refused("record_action_pending", "Record generation exhausted")
                    })?;
                if history.revision() == u64::MAX {
                    return Err(refused("record_action_pending", "Record history exhausted"));
                }
                let mut resources = vec![];
                owned_resources(root, doc, Some(&target.address), &mut resources);
                if claims(&resources, &target.address) != 1 {
                    return Err(refused(
                        "ambiguous_action_target",
                        "The exact owner must advertise one matching action",
                    ));
                }
                let (arguments, resource, spec) = resolve_resource_invocation(
                    &resources,
                    &target.address,
                    input.argument,
                    input.arguments,
                    false,
                )?;
                if let Some(scope) = scope {
                    scope.check_dispatch_spec(spec)?;
                }
                if spec.kind != OperationKind::Document
                    || spec.locus != tilo_actions::ExecutionLocus::ClientLocal
                    || spec.contract.execution != tilo_actions::ExecutionMode::Immediate
                    || !matches!(
                        spec.contract.confirmation,
                        tilo_actions::ConfirmationPolicy::None
                    )
                {
                    return Err(refused(
                        "legacy_document_route_unsupported",
                        "Only immediate ClientLocal documents without confirmation are supported",
                    ));
                }
                if !operation_allowed(&current, &app, spec) {
                    return Err(refused(
                        "permission_denied",
                        "The owner does not grant this operation",
                    ));
                }
                spec.contract
                    .result
                    .validate(&tilo_actions::ActionOutput::default())
                    .map_err(DoorError::Failed)?;
                let encoded = encode_resource_action(
                    &resources,
                    &target.address,
                    arguments.as_ref(),
                    input.argument,
                )?;
                let action = services
                    .registry
                    .decode(&encoded)
                    .map_err(DoorError::decoding)?;
                if action.locus() != tilo_actions::ExecutionLocus::ClientLocal {
                    return Err(refused(
                        "legacy_document_route_unsupported",
                        "Decoded action is not ClientLocal",
                    ));
                }
                let mut context = tilo_actions::ActionContext::for_resource(
                    resource.id.as_str(),
                    &resource.kind,
                    &resource.label,
                )
                .with_app(&app, &app);
                if let Some(ambient) = input.ambient {
                    context = context.enriched_by(ambient);
                }
                let dispatched = execute_document(
                    doc,
                    action.as_ref(),
                    encoded,
                    services.transport,
                    input.actor,
                    false,
                    context,
                )?;
                let tilo_actions::Dispatched::AppliedLocal(envelope) = dispatched else {
                    return Err(refused(
                        "legacy_document_route_unsupported",
                        "Local action was unexpectedly submitted",
                    ));
                };
                let changed = history.record_committed_action_authored(
                    doc,
                    envelope.base_change_hashes(),
                    None,
                    crate::undo::get_state(root),
                    Some(services.author.clone()),
                );
                if changed {
                    state.generation = Some(next);
                    state.statuses.clear();
                }
                crate::history_mode::capability::document_action_applied(
                    root,
                    &envelope.action,
                    &envelope.base_change_hashes(),
                    doc,
                    &ROOT,
                );
                Ok(LegacyDocumentOutcome { changed, envelope })
            };
            match scope {
                Some(scope) => scope
                    .with_document_mutation(doc, |doc| Ok(execute(doc)))
                    .map_err(DoorError::Failed)?,
                None => execute(doc),
            }
        })
        .map_err(|e| refused("record_unavailable", e))??;
    if outcome.changed {
        store.schedule_committed_history(&checkout.id);
    }
    Ok(outcome)
}

pub(crate) fn authorize_legacy_operation(
    target: &LegacyResourceTarget,
    authority: &PermissionContext,
    spec: &tilo_actions::ActionSpec,
) -> Result<(), DoorError> {
    target
        .owner
        .validate()
        .map_err(|e| refused("record_unavailable", e))?;
    let (store, checkout) = target.owner.store_checkout();
    let (_, app, _) = store
        .history_namespace(&checkout.id)
        .map_err(|e| refused("record_unavailable", e))?;
    if !operation_allowed(&current_authority(authority)?, &app, spec) {
        return Err(refused(
            "permission_denied",
            "The actual owner does not grant this operation",
        ));
    }
    Ok(())
}
