//! Which owner a local action address resolves to: the outer document or
//! exactly one mounted component. Requests are toolkit-private and finite;
//! no widget pointer or mutable document loan escapes a traversal.
use super::{AutoCommit, BoundActionAddress, BoundActionTarget, CallbackBinding, Widget};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};

#[allow(
    clippy::needless_pass_by_value,
    reason = "normalize owned and borrowed error types into a refusal"
)]
fn refused(code: &str, reason: impl ToString) -> DoorError {
    refusal(code, reason.to_string())
}

/// [`refused`] once its reason is text. The traversals that refuse are
/// generic over the widget they visit, and apps compile them for the
/// toolkit's own widgets, so building the error stays here.
fn refusal(code: &str, reason: String) -> DoorError {
    DoorError::Unavailable {
        code: code.into(),
        reason,
    }
}
#[derive(Clone)]
pub(crate) enum LocalResourceTarget {
    Outer,
    Component(BoundActionTarget),
}
struct OwnerResources {
    owner: CallbackBinding,
    resources: Vec<ResourceSpec>,
    interactive: bool,
}
pub(crate) struct ResourceReadRequest<'a> {
    address: Option<&'a ActionAddress>,
    owners: Vec<OwnerResources>,
    hidden: bool,
}
/// Collect just this document domain. Nested owners are never ambient
/// children.
fn owned_resources<W: Widget + ?Sized>(
    root: &W,
    doc: &AutoCommit,
    address: Option<&ActionAddress>,
    resources: &mut Vec<ResourceSpec>,
) {
    owned_resources_at(root, None, doc, address, resources);
}
/// [`owned_resources`] of a component's root: only the root speaks for the
/// component's document, what the rest of its tree publishes carries no
/// document action.
fn component_resources<W: Widget + ?Sized>(
    root: &W,
    doc: &AutoCommit,
    address: Option<&ActionAddress>,
    resources: &mut Vec<ResourceSpec>,
) {
    owned_resources_at(root, Some(true), doc, address, resources);
}
/// `owner` says whether `widget` is a component's root (`Some(true)`), below
/// it (`Some(false)`), or in a tree no component owns (`None`).
fn owned_resources_at<W: Widget + ?Sized>(
    widget: &W,
    owner: Option<bool>,
    doc: &AutoCommit,
    address: Option<&ActionAddress>,
    resources: &mut Vec<ResourceSpec>,
) {
    // A delivered invocation resolves against its invoker's own view,
    // rebuilt now; outside one this is the widget's own view.
    let participant = crate::invocation_scope::current_participant();
    let mut local = crate::resources::resources_for(widget, participant.as_ref(), doc);
    if owner == Some(false) {
        local = local
            .into_iter()
            .map(crate::resources::outside_document)
            .collect();
    }
    let eager_claim = address.is_some_and(|address| claims(&local, address) != 0);
    resources.extend(local);
    if let Some(address) = address
        && !eager_claim
        && let Some(resource) =
            crate::resources::resource_for(widget, participant.as_ref(), doc, &address.resource)
    {
        resources.push(if owner == Some(false) {
            crate::resources::resolved_outside_document(resource)
        } else {
            resource
        });
    }
    widget.for_each_child(&mut |child| {
        if !child.has_separate_document_owner() {
            owned_resources_at(child, owner.map(|_| false), doc, address, resources);
        }
    });
}
fn provider<W: Widget + ?Sized>(
    root: &W,
    binding: &CallbackBinding,
    address: &ActionAddress,
) -> Option<ResourceSpec> {
    provider_at(root, true, binding, address)
}
fn provider_at<W: Widget + ?Sized>(
    widget: &W,
    owner: bool,
    binding: &CallbackBinding,
    address: &ActionAddress,
) -> Option<ResourceSpec> {
    let mut found = crate::search::capability::search_providers(widget)
        .into_iter()
        .filter(|p| {
            p.callback_binding()
                .as_ref()
                .is_none_or(|b| b.same_component(binding))
        })
        .find_map(|p| p.resolve_action_resource(address))
        .map(|resource| {
            if owner {
                resource
            } else {
                crate::resources::resolved_outside_document(resource)
            }
        });
    widget.for_each_child(&mut |child| {
        if found.is_none() && !child.has_separate_document_owner() {
            found = provider_at(child, false, binding, address);
        }
    });
    found
}
impl ResourceReadRequest<'_> {
    pub(crate) fn visit<W: Widget + ?Sized>(&mut self, root: &W) -> Result<(), DoorError> {
        if let Some(binding) = root.callback_binding() {
            binding
                .enter()
                .map_err(|e| refused("record_unavailable", format!("{e:?}")))?;
            if !self
                .owners
                .iter()
                .any(|snapshot| snapshot.owner.same_component(&binding))
            {
                let resources = binding
                    .read_document(&AutoCommit::new(), |doc, _| {
                        let mut resources = Vec::new();
                        // The boundary adapter forwards its root resources and also
                        // exposes that root as a child. Collect actual roots once.
                        root.for_each_child(&mut |child| {
                            if !child.has_separate_document_owner() {
                                component_resources(child, doc, self.address, &mut resources);
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
                    })
                    .map_err(|e| refused("record_unavailable", format!("{e:?}")))?;
                self.owners.push(OwnerResources {
                    owner: binding,
                    resources,
                    interactive: !self.hidden,
                });
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
    /// A tile's live content, hidden from dispatch while `interactive` is false.
    pub(crate) fn tile(&mut self, interactive: bool, live: &dyn Widget) -> Result<(), DoorError> {
        let old_hidden = self.hidden;
        self.hidden |= !interactive;
        let result = live.visit_resource_owners(self);
        self.hidden = old_hidden;
        result
    }
}
fn inventory(
    root: &dyn Widget,
    address: Option<&ActionAddress>,
) -> Result<Vec<OwnerResources>, DoorError> {
    let mut request = ResourceReadRequest {
        address,
        owners: vec![],
        hidden: false,
    };
    root.visit_resource_owners(&mut request)?;
    Ok(request.owners)
}
fn claims(resources: &[ResourceSpec], address: &ActionAddress) -> usize {
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
    for snapshot in inventory(root, Some(address))? {
        let n = claims(&snapshot.resources, address);
        count = count.saturating_add(n);
        if n != 0 {
            interactive = snapshot.interactive;
            // Hidden claims still count: they can never donate fallback.
            let binding = snapshot.owner;
            target = LocalResourceTarget::Component(BoundActionTarget {
                input: crate::bound_dispatch::Input::Discrete,
                address: BoundActionAddress {
                    instance_id: binding.instance_id,
                    address: address.clone(),
                },
                binding,
            });
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
