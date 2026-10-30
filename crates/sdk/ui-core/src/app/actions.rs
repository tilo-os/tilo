// Action/resource discovery, publication, dispatch, and palette integration.
//
// Kept as an `App` implementation so custom widgets and built-in controls
// share one authoritative action/resource route.

/// A broker's resolution of a resource still being read, answered once its
/// read lands.
pub(super) struct AwaitedResolution {
    revision: String,
    resource: String,
    action: String,
    reply: tokio::sync::oneshot::Sender<runtime_collab::ActionDirectoryResolution>,
}

impl App {
    /// Lets the hosted app register its action types, e.g.
    /// `app.action_registry_mut().register::<TubeAction>("tube.action")`.
    pub fn action_registry_mut(&mut self) -> &mut tilo_actions::ActionRegistry {
        self.actions.action_registry_generation =
            self.actions.action_registry_generation.saturating_add(1);
        self.invalidate_widget_resource_cache();
        &mut self.actions.action_registry
    }

    pub fn set_action_transport(&mut self, transport: Box<dyn tilo_actions::ActionTransport>) {
        let _entry = self.enter_wake_scope();
        self.actions.action_transport = transport;
    }

    fn resource_cache_key(&self) -> ResourceCacheKey {
        ResourceCacheKey {
            mutation_generation: self.actions.resource_generation.get(),
        }
    }

    /// Invalidate declarations derived from document or widget-local state.
    fn invalidate_widget_resource_cache(&mut self) {
        // What the widgets declare changed, and so may their semantics.
        self.mark_semantic_tree_stale();
        // At `u64::MAX` there is no fresh key to issue. Eviction still makes
        // each later materialisation a new snapshot, so it cannot alias a
        // retained tree that happened to have the same terminal generation.
        if let Some(next) = self.actions.resource_generation.get().checked_add(1) {
            self.actions.resource_generation.set(next);
        }
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_stamp = None;
        self.actions.action_directory_cursors.clear();
        self.actions.resource_cache.get_mut().take();
        self.actions.local_resource_cache.get_mut().take();
        self.actions.open_resource_search_cache.get_mut().take();
        self.actions.last_control_availability = None;
        // Also at an exhausted generation, which no longer moves on.
        self.layout.walks = runtime::TreeWalks::default();
    }

    /// The retained tree the frame's whole-tree walks read, as far as a
    /// walk can tell it apart from the last one
    /// ([`runtime::TreeGeneration`]).
    fn tree_generation(&self) -> runtime::TreeGeneration {
        runtime::TreeGeneration {
            resources: self.actions.resource_generation.get(),
            rebuilds: self.layout.rebuilds,
            remounts: self.layout.remounts,
            permissions: self.actions.permission_generation.get(),
            shared_permissions: crate::permissions::shared_revision(),
        }
    }

    fn ensure_resource_cache(&self) -> ResourceCacheKey {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let key = self.resource_cache_key();
        let trace = self.trace_resource_stage(Stage::ResourceCache);
        if self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.key == key && cache.complete.is_some())
        {
            trace.finish(CacheResult::Hit);
            return key;
        }
        trace.finish(CacheResult::Miss);
        let key = self.ensure_declared_resource_cache();
        let trace = self.trace_resource_stage(Stage::SemanticConstruction);
        let mut complete = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map_or_else(Vec::new, |cache| cache.declared.clone());
        let mut semantics = crate::a11y_tree::SemanticTree::new();
        semantics.refresh_action_projection(
            self.root_widget.as_ref(),
            &self.layout.node_map,
            &self.bindings.semantic_node_names(&self.layout.node_map),
            self.focused_node,
        );
        complete.extend(semantics.action_resources());
        trace.finish(CacheResult::NotApplicable);
        let trace = self.trace_resource_stage(Stage::SurfaceValidation);
        let mut operation_handlers = crate::widget::resource_operation_handlers(&*self.root_widget);
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        let violations = tilo_actions::validate_surface(
            &complete,
            &self.actions.action_registry,
            &operation_handlers,
        );
        if !violations.is_empty() {
            for violation in violations {
                tracing::error!(
                    address = %violation.address,
                    reason = %violation.reason,
                    "refusing an invalid resource/action surface"
                );
            }
            complete.clear();
        }
        trace.finish(CacheResult::NotApplicable);
        if let Some(cache) = self
            .actions
            .resource_cache
            .borrow_mut()
            .as_mut()
            .filter(|cache| cache.key == key)
        {
            cache.complete = Some(complete);
        }
        // Completing this generation must retain its already-filtered allowed set.
        key
    }

    fn collect_declared_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let key = self.ensure_resource_cache();
        self.actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map_or_else(Vec::new, |cache| cache.declared.clone())
    }

    fn ensure_local_resource_cache(&self) -> (ResourceCacheKey, u64) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        // Every full consumer still constructs and validates the combined surface.
        self.ensure_resource_cache();
        let (key, permission_generation) = self.ensure_local_control_cache();
        let trace = self.trace_resource_stage(Stage::LocalCache);
        if self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| {
                cache.key == key
                    && cache.permission_generation == permission_generation
                    && cache.resources.is_some()
            })
        {
            trace.finish(CacheResult::Hit);
            return (key, permission_generation);
        }
        trace.finish(CacheResult::Miss);
        let trace = self.trace_resource_stage(Stage::LocalPermissionProjection);
        let complete = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .and_then(|cache| cache.complete.clone())
            .unwrap_or_default();
        let resources = complete
            .into_iter()
            .filter_map(|resource| self.filter_resource_for_local_actor(resource))
            .collect();
        if let Some(cache) = self
            .actions
            .local_resource_cache
            .borrow_mut()
            .as_mut()
            .filter(|cache| {
                cache.key == key && cache.permission_generation == permission_generation
            })
        {
            cache.resources = Some(resources);
        }
        trace.finish(CacheResult::NotApplicable);
        (key, permission_generation)
    }

    fn collect_local_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let (key, permission_generation) = self.ensure_local_resource_cache();
        let mut resources = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.key == key && cache.permission_generation == permission_generation
            })
            .and_then(|cache| cache.resources.clone())
            .unwrap_or_default();
        if let Ok(snapshots) = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        ) {
            resources.extend(
                snapshots
                    .into_iter()
                    .flat_map(|snapshot| snapshot.into_resources()),
            );
        }
        resources
    }

    /// The complete machine-driveable surface: domain resources declared by
    /// the app plus ordinary controls projected from its retained semantic
    /// tree. Ctrl+K, scripts, and agents therefore consume one catalogue.
    /// The resource this window shows: the first it publishes. What
    /// a question typed into a context menu is about when nothing more
    /// particular is meant.
    #[cfg(feature = "native")]
    pub(super) fn window_resource(&self) -> Option<tilo_actions::ActionResource> {
        let key = self.ensure_resource_cache();
        let cache = self.actions.resource_cache.borrow();
        let first = cache
            .as_ref()
            .filter(|cache| cache.key == key)?
            .complete
            .as_ref()?
            .first()?;
        Some(tilo_actions::ActionResource {
            id: first.id.0.clone(),
            kind: first.kind.clone(),
            label: first.label.clone(),
        })
    }

    pub fn collect_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let _entry = self.enter_wake_scope();
        let key = self.ensure_resource_cache();
        let mut resources = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .and_then(|cache| cache.complete.clone())
            .unwrap_or_default();
        if let Ok(snapshots) = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        ) {
            resources.extend(
                snapshots
                    .into_iter()
                    .flat_map(|snapshot| snapshot.into_resources()),
            );
        }
        crate::history_source::sentence::learn_labels(&resources);
        resources
    }

    /// Audit every retained control binding against the fresh resource tree.
    /// This is public so development tooling can turn remaining explicit
    /// legacy controls and stale/duplicate action addresses into a coverage
    /// report without inspecting widget implementation details.
    pub fn control_action_coverage(&self) -> Vec<ControlBindingCoverage> {
        let _entry = self.enter_wake_scope();
        let resources = self.collect_resources();
        self.control_action_coverage_for(&resources)
    }

    /// Locate every on-screen affordance for one canonical action and rank
    /// usable, visible, short paths first. Agents still invoke `address`
    /// directly; the chosen route makes the resulting interaction project
    /// into the same UI a person sees without fragile coordinate clicking.
    pub fn action_ui_routes(
        &mut self,
        address: &tilo_actions::ActionAddress,
    ) -> Vec<ActionUiRoute> {
        self.refresh_semantic_tree();
        let bindings = self
            .with_native_presentation(|root, _, _, _, _| {
                crate::widget::controls::control_action_bindings(root)
            })
            .unwrap_or_default();
        crate::action_locus::action_ui_routes(&self.accessibility.semantic_tree, &bindings, address)
    }

    pub fn preferred_action_ui_route(
        &mut self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<ActionUiRoute> {
        self.action_ui_routes(address).into_iter().next()
    }

    fn control_action_coverage_for(
        &self,
        resources: &[tilo_actions::ResourceSpec],
    ) -> Vec<ControlBindingCoverage> {
        self.control_binding_coverage_for(
            resources,
            crate::widget::controls::control_action_bindings(self.root_widget.as_ref()),
        )
    }

    fn control_binding_coverage_for(
        &self,
        resources: &[tilo_actions::ResourceSpec],
        bindings: Vec<crate::widget::ControlBindingMetadata>,
    ) -> Vec<ControlBindingCoverage> {
        // Shared chrome invokes doors served by this host, rather than by
        // the app's resource provider. Audit those exact declarations too.
        let mut host_resources = vec![crate::search::search_resource()];
        if let Some(history) = &self.history {
            host_resources.push(history.door_resource());
        }
        let mut seen = std::collections::HashSet::new();
        bindings
            .into_iter()
            .map(|binding| {
                let status = match &binding.kind {
                    crate::widget::ControlBindingKind::Action(address) => {
                        if tilo_actions::door::resolve(resources, address).is_none()
                            && tilo_actions::door::resolve(&host_resources, address).is_none()
                        {
                            ControlBindingStatus::StaleAddress
                        } else if !seen.insert(address.clone()) {
                            ControlBindingStatus::SharedAddress
                        } else {
                            ControlBindingStatus::Resolved
                        }
                    }
                    crate::widget::ControlBindingKind::CollaborativeDocument(_) => {
                        ControlBindingStatus::CollaborativeDocument
                    }
                    crate::widget::ControlBindingKind::SemanticAction(_) => {
                        ControlBindingStatus::SemanticAction
                    }
                };
                ControlBindingCoverage { binding, status }
            })
            .collect()
    }

    fn invoke_semantic_operation_encoded(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<bool, String> {
        if encoded.type_id != crate::a11y_tree::SEMANTIC_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: crate::a11y_tree::SemanticOperation =
            serde_json::from_slice(&encoded.payload).map_err(|error| error.to_string())?;
        self.refresh_semantic_tree();
        self.invoke_semantic_action(
            &crate::a11y_tree::SemanticId(operation.node),
            &operation.action,
            operation.value.as_deref(),
        )
    }

    fn bound_broker_route(&self) -> Option<&crate::collaboration::PrimaryRouteWitness> {
        #[cfg(test)]
        if let Some(route) = &self.actions.bound_broker_route_for_test {
            return Some(route);
        }
        self.collaboration.manager.primary_route()
    }
    fn publication_app_id(&self) -> String {
        if crate::bound_dispatch::has_matching_broker_component(
            self.root_widget.as_ref(),
            self.bound_broker_route(),
        ) && let Some(witness) = self.bound_broker_route()
        {
            return witness.route().app_id.clone();
        }
        crate::collaboration::local_app_id()
    }
    fn require_bound_broker_route(
        &self,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Result<(), tilo_actions::DoorError> {
        if binding.matches_broker_route(self.bound_broker_route()) {
            return Ok(());
        }
        // Publication must not wait on the transport ack when the Hello already
        // named this component's broker route: Ctrl+K / collect_resources already
        // expose the surface, and owner-commit tests (and reconnecting workers)
        // publish before the acknowledgement frame is observed again.
        if self
            .collaboration
            .manager
            .requested_primary_route()
            .is_some_and(|route| binding.matches_route(route))
        {
            return Ok(());
        }
        Err(tilo_actions::DoorError::Unavailable {
            code: "bound_broker_route_unavailable".into(),
            reason:
                "This component has no matching admitted app, workspace and record action route"
                    .into(),
        })
    }

    /// Transport-only projection. Each component is validated with its own
    /// registry before its domain resource is assigned an opaque publication alias.
    fn publication_resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        fn remove_reserved(
            mut resource: tilo_actions::ResourceSpec,
        ) -> Option<tilo_actions::ResourceSpec> {
            if resource
                .id
                .as_str()
                .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
            {
                return None;
            }
            resource.children = resource
                .children
                .into_iter()
                .filter_map(remove_reserved)
                .collect();
            Some(resource)
        }
        self.refresh_local_undo_discovery();
        let key = self.ensure_resource_cache();
        let mut resources = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .and_then(|cache| cache.complete.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(remove_reserved)
            .collect::<Vec<_>>();
        let components = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        for component in components {
            if self.require_bound_broker_route(&component.binding).is_err() {
                continue;
            }
            let violations = crate::bound_dispatch::bound_contract_violations(
                self.root_widget.as_mut(),
                &component.binding,
                component.resources(),
            );
            if !matches!(&violations, Ok(Some(found)) if found.is_empty()) {
                // Never silent: a contract violation withdraws the whole
                // component's published surface (agents and Ctrl+K lose it),
                // so name the actions that broke it and why.
                let violations = match violations {
                    Ok(Some(found)) => found
                        .iter()
                        .map(|violation| format!("{}: {}", violation.address, violation.reason))
                        .collect::<Vec<_>>()
                        .join("; "),
                    Ok(None) => "the component is no longer mounted".to_owned(),
                    Err(error) => format!("{error:?}"),
                };
                if self
                    .actions
                    .refusals_logged
                    .first(&("component", &violations))
                {
                    tracing::error!(
                        resources = component.resources().len(),
                        %violations,
                        "refusing to publish a component surface that fails action contract validation"
                    );
                }
                continue;
            }
            let binding = component.binding.clone();
            for resource in component
                .into_resources()
                .into_iter()
                .filter_map(crate::bound_dispatch::published_here)
            {
                if let Ok(resource) = self.actions.bound_publication.project(&binding, resource) {
                    resources.push(resource);
                }
            }
        }
        if crate::undo::local_undo_status(self.root_widget.as_ref()).is_some() {
            let address = tilo_actions::ActionAddress::new("tilo.host:edit", "undo");
            if let Some(edit) = self.edit_resource_for(&address) {
                resources.push(edit);
            }
        }
        // Every window zooms: an agent or script asked to make it larger
        // reaches the zoom the keys do.
        #[cfg(feature = "native")]
        resources.extend(self.zoom_resource());
        resources
    }

    /// Publish the live resource/action tree through the session broker.
    /// Level-triggered at the wire, diffed here so an unchanged frame emits
    /// nothing; publishing an empty list explicitly withdraws a closed tree.
    fn publish_action_surface(&mut self) {
        let _phase = tilo_resource::profiling::Phase::start("ui.publish_action_surface");
        self.actions.action_surface_owed = false;
        if self.collaboration.manager.is_connected() {
            let _routes = tilo_resource::profiling::Phase::start("ui.publish.routes");
            self.refresh_document_routes();
        }
        if self.collaboration.manager.take_action_surface_failed() {
            // The broker did not install the last surface: nothing counts as
            // published until it acknowledges one.
            self.actions.last_published_actions = None;
        }
        // Nothing the surface is built from changed since the route was
        // given it (`PublicationStamp`): building it again would
        // publish the same surface, so the route carries it already. Each
        // keystroke begins by publishing (an input's edits are attributed
        // to the surface on the route); this keeps that to a comparison.
        let witness = tilo_resource::profiling::Phase::start("ui.publish.witness");
        let stamp = self.publication_stamp();
        let unchanged = stamp.as_ref().is_some_and(|stamp| {
            Some(stamp) == self.actions.published_stamp.as_ref()
                // The directory's own witness (the bound owners' generations
                // and permissions), taken now: a grant revoked in place is a
                // change though no generation moved.
                && self
                    .try_action_directory_revision()
                    .is_ok_and(|now| now == stamp.revision())
        });
        drop(witness);
        if unchanged {
            self.settle_held_own_invocations(true);
            return;
        }
        self.publish_action_surface_after_refresh();
    }

    /// Publish the surface a handled input left for the frame after its
    /// sync ([`super::runtime::ActionRuntime::action_surface_owed`]), if no
    /// publication has paid for it since.
    #[cfg(feature = "native")]
    pub(super) fn publish_owed_action_surface(&mut self) {
        if self.actions.action_surface_owed {
            self.publish_action_surface();
        }
    }

    fn publish_action_surface_after_refresh(&mut self) {
        let carried = self.publish_action_surface_on_route();
        self.settle_held_own_invocations(carried);
        // What the surface on the route was built from, for the next
        // publication to compare with.
        self.actions.published_stamp = if carried {
            self.publication_stamp()
        } else {
            None
        };
    }

    /// Send an invocation of this app's own action to the session action
    /// host, on the route, after the surface that offers it. It waits here,
    /// behind any invocation made before it, while the route cannot carry
    /// that surface yet.
    fn send_own_invocation(&mut self, request: tilo_actions::remote::InvokeRequest) {
        self.actions.held_own_invocations.push_back(request);
        self.publish_action_surface();
    }

    /// Release the held own invocations, in order, once the route carries
    /// the current surface; refuse them with the connection's own reason
    /// once it has gone. Otherwise they keep waiting: the publication the
    /// route's readiness brings settles them.
    fn settle_held_own_invocations(&mut self, carried: bool) {
        use tilo_actions::remote::{ActionHostError, RemoteActionMessage};
        if self.actions.held_own_invocations.is_empty() {
            return;
        }
        let connected = self.collaboration.manager.is_connected();
        if connected && !carried {
            return;
        }
        while let Some(request) = self.actions.held_own_invocations.pop_front() {
            let refusal = RemoteActionMessage::Invoke(request.clone());
            if connected
                && self
                    .collaboration
                    .manager
                    .send(CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Invoke(request),
                    })
            {
                continue;
            }
            let reason = self.collaboration.manager.state().reason().to_owned();
            if let Some(answer) =
                tilo_actions::remote::refuse(&refusal, ActionHostError::Unavailable { reason })
            {
                self.apply_remote_action_answer(answer);
            }
        }
    }

    /// Withdraw everything this app published: a reset delta with nothing in it.
    fn withdraw_action_surface(&mut self, app_id: String) -> bool {
        let withdrawn = self
            .collaboration
            .manager
            .send(CollabRequest::PublishActionSurface {
                app_id,
                delta: crate::collaboration::SurfaceDelta {
                    reset: true,
                    ..crate::collaboration::SurfaceDelta::default()
                },
            });
        if withdrawn {
            self.actions.last_published_actions = Some(Vec::new());
            self.actions.published_surface =
                Some(crate::collaboration::SurfacePublication::default());
        }
        withdrawn
    }

    fn invoke_published_action(
        &mut self,
        id: u64,
        invocation: tilo_actions::InvocationId,
        resource: String,
        action: String,
        argument: Option<&str>,
        actor: &str,
        ambient_context: Option<&tilo_actions::ActionContext>,
    ) {
        if self.action_publication_failure.is_some() {
            return;
        }
        if !self.collaboration.manager.invocation_owner_current(id) {
            // Accepted for a document this window has since reopened or
            // replaced: nothing ran, and it must not run in the new one.
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation {
                    id,
                    update: tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "action_document_replaced".into(),
                            message: crate::i18n::I18nManager::lookup_plain(
                                "action-document-replaced",
                            ),
                            details: None,
                            retryable: false,
                        },
                    },
                });
            return;
        }
        if !self.published_action_history_ready() {
            // An already accepted delivery lost its original owner-history
            // custody. Run no handler; disconnect retains admitted uncertainty.
            self.action_publication_failure = Some(
                "Original document history is unavailable; action outcome unknown. Reconnect and reconcile before retrying.".into(),
            );
            self.collaboration.manager.stop_after_publication_failure();
            return;
        }
        let address = tilo_actions::ActionAddress::new(resource, action);
        // The broker performed admission and one-use confirmation checks.
        // Required host workers separately recheck the original caller at
        // execution, using the exact host-local retained association.
        // The broker permanently reserves UUIDs across caller identities.
        // Also fail closed here before a malformed/repeated delivery can invoke
        // a second handler or replace an active completion/cancellation route.
        if let Some(pending) = self.actions.directed_invocations.get(&invocation) {
            if pending.wire_id == id {
                return;
            }
        }
        if self.actions.directed_invocations.contains_key(&invocation)
            || self.actions.bound_invocations.contains_key(&invocation)
            || self.actions.local_invocations.contains_key(&invocation)
        {
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation {
                    id,
                    update: tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "action_invocation_identity_conflict".into(),
                            message: "Invocation UUID already has an active operation".into(),
                            details: None,
                            retryable: false,
                        },
                    },
                });
            return;
        }
        // The invoker is the delivered actor plus the participant and the
        // person it acts for that the host set for it in the delivery context
        // (never trusted from anywhere else). Resolution below rebuilds that participant's own tree, and
        // durable attribution carries the same invoker.
        let invoker = tilo_actions::ActionInvoker::delivered(actor, ambient_context);
        let participant = invoker.participant.clone();
        let attributed =
            ambient_context.map(|context| context.clone().with_invoker(invoker.clone()));
        let ambient_context = attributed.as_ref();
        if let Some(participant) = &participant {
            self.join_participant(participant);
        }
        let admitted = self.admitted_kind(&address, argument);
        self.actions.acting_for.insert(
            invocation,
            crate::invocation_scope::Continuation {
                invoker: invoker.clone(),
                context: attributed.clone(),
                admitted,
                refused: None,
                outstanding: std::collections::HashSet::new(),
                deferred: None,
            },
        );
        let invoker_scope = crate::invocation_scope::InvokerScope::delivered(invoker, invocation);
        let authorization = self.authorize_published_invocation(
            &crate::invocation_scope::PublishedInvocationRequest {
                wire_id: id,
                invocation,
                address: address.clone(),
                argument: argument.map(str::to_owned),
                actor: actor.to_owned(),
                participant,
            },
        );
        let authorization = authorization.and_then(|lease| {
            if let Some(lease) = &lease {
                self.check_published_invocation_revision(lease)?;
            }
            self.published_invocation_scope(&address, lease)
        });
        let (result, authorization) = match authorization {
            Ok(authorization) => (
                self.invoke_resource_action(
                    ResourceInvocation::new(&address, actor)
                        .argument(argument)
                        .confirmed(true)
                        .broker_admitted(true)
                        .invocation(Some(invocation))
                        .ambient_context(ambient_context)
                        .scope(authorization.as_ref()),
                ),
                authorization,
            ),
            Err(error) => (Err(error), None),
        };
        drop(invoker_scope);
        let continuation = if matches!(result, Ok(crate::widget::ResourceOperationStart::Accepted))
        {
            None
        } else {
            // What the effect submitted is applied for its invoker before
            // its caller learns it finished.
            self.dispatch_control_invocations();
            if let Some(scope) = &authorization {
                // Submitted continuations already carry their original broker
                // custody. A retained handler clone must not submit more work
                // once this handler has answered, even while those edits settle.
                scope.cancel();
            }
            self.actions.acting_for.remove(&invocation)
        };
        let update = match result {
            Ok(crate::widget::ResourceOperationStart::Completed(output)) => {
                tilo_actions::InvocationUpdate::Succeeded { invocation, output }
            }
            Ok(crate::widget::ResourceOperationStart::Accepted) => {
                self.actions.directed_invocations.insert(
                    invocation,
                    DirectedInvocation {
                        wire_id: id,
                        cancellation_requested: false,
                        authorization,
                    },
                );
                // This delivery was already admitted as Accepted by the
                // broker. Retain the job here; only its later updates belong
                // to the worker. A second Accepted would violate one-use
                // broker admission and is deliberately rejected there.
                return;
            }
            Err(error) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: crate::bound_dispatch::door_error_code(&error).to_string(),
                    message: error.to_string(),
                    details: error.failure_details(),
                    retryable: false,
                },
            },
        };
        let update = match continuation {
            // Its edits are still being admitted by the host: it answers
            // once they all are, or one is refused.
            Some(mut continuation) if !continuation.outstanding.is_empty() => {
                continuation.deferred = Some((id, update));
                self.actions.acting_for.insert(invocation, continuation);
                return;
            }
            Some(continuation) => continuation.settle(update),
            None => update,
        };
        if update.is_terminal() {
            // What it caused in the app is part of it: the frame its change
            // asks for runs now, and the answer waits for any reply that
            // frame owes to land ([`Self::answer_held_invocations`]).
            self.answer_after_its_sync(id, update);
            return;
        }
        self.collaboration
            .manager
            .send(CollabRequest::UpdateInvocation { id, update });
    }

    /// What the invoker of `address` was admitted for: the action's own
    /// kind, which the admitting host checked they may perform. Only reading
    /// when it no longer resolves.
    fn admitted_kind(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> tilo_actions::ActionKind {
        let key = self.ensure_resource_cache();
        let mut resources = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .and_then(|cache| cache.complete.clone())
            .unwrap_or_default();
        if tilo_actions::door::resolve(&resources, address).is_none() {
            resources.extend(crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                &address.resource,
            ));
        }
        if tilo_actions::door::resolve(&resources, address).is_none()
            && let Ok(Some(target)) = self.actions.bound_publication.target(address)
            && let Ok(snapshots) = crate::bound_dispatch::discover_bound_resources(
                self.root_widget.as_ref(),
                &self.state.doc,
                Some(&target.address.address),
            )
        {
            return snapshots
                .iter()
                .filter(|snapshot| snapshot.binding.same_component(&target.binding))
                .find_map(|snapshot| {
                    tilo_actions::door::resolve(snapshot.resources(), &target.address.address)
                })
                .map_or(tilo_actions::ActionKind::Read, |spec| {
                    spec.need_for(None, argument).kind
                });
        }
        tilo_actions::door::resolve(&resources, address)
            .map_or(tilo_actions::ActionKind::Read, |spec| {
                spec.need_for(None, argument).kind
            })
    }

    fn resolve_resource_invocation<'a>(
        resources: &'a [tilo_actions::ResourceSpec],
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
        confirmed_by_broker: bool,
    ) -> Result<ResolvedResourceInvocation<'a>, tilo_actions::DoorError> {
        crate::bound_dispatch::resolve_resource_invocation(
            resources,
            address,
            argument,
            None,
            confirmed_by_broker,
        )
    }

    /// The canonical route the app's primary document currently syncs on.
    fn primary_document_route(&self) -> Option<crate::collaboration::RouteSpec> {
        self.collaboration
            .manager
            .primary_route()
            .map(|witness| witness.route().clone())
    }

    fn encode_resource_action(
        resources: &[tilo_actions::ResourceSpec],
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Result<tilo_actions::EncodedAction, tilo_actions::DoorError> {
        crate::bound_dispatch::encode_resource_action(resources, address, typed_arguments, argument)
    }

    /// Route keyboard history through the bound resource's declared action
    /// so compensation receives fresh admission under current permissions.
    pub(super) fn invoke_bound_history(
        &mut self,
        binding: &crate::bound_dispatch::CallbackBinding,
        request: crate::undo::UndoRequest,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.require_bound_broker_route(binding)?;
        let snapshots = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .map_err(tilo_actions::DoorError::Failed)?;
        let action = match request {
            crate::undo::UndoRequest::Undo => "undo",
            crate::undo::UndoRequest::Redo => "redo",
        };
        let mut candidates = snapshots
            .iter()
            .filter(|snapshot| snapshot.binding.same_component(binding))
            .flat_map(|snapshot| snapshot.resources().iter())
            .filter(|resource| {
                resource.actions.iter().any(|spec| {
                    spec.id == action && spec.kind == tilo_actions::OperationKind::Document
                })
            });
        let resource = candidates
            .next()
            .ok_or_else(|| tilo_actions::DoorError::Unavailable {
                code: "history_action_unavailable".into(),
                reason: "the current document does not publish its history action".into(),
            })?;
        if candidates.next().is_some() {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "history_action_ambiguous".into(),
                reason: "the current document publishes more than one history target".into(),
            });
        }
        let alias = self
            .actions
            .bound_publication
            .alias(binding, resource.id.as_str())
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
        let address = tilo_actions::ActionAddress::new(alias, action);
        self.invoke_via_broker(&address, Some(&serde_json::Map::new()), None, None)
    }

    /// Admit a locally initiated operation at the session broker before the
    /// application performs it. The broker records the invocation first,
    /// re-resolves the live published surface and current permissions, then
    /// directs the exact operation back to this connection. That ordering is
    /// load-bearing for Effects: if the app crashes while doing external work,
    /// history retains an admitted/incomplete operation instead of losing the
    /// action entirely.
    fn invoke_via_broker(
        &mut self,
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        confirmation_evidence: Option<&serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let mut made = None;
        let started = Self::dispatch_broker_request(
            &mut self.actions.next_broker_request_id,
            self.collaboration.manager.is_connected(),
            address,
            typed_arguments,
            argument,
            confirmation_evidence,
            |request_id, request| made = Some((request_id, request)),
        )?;
        if let Some((request_id, request)) = made {
            self.actions
                .broker_requests
                .insert(BrokerReply::Receipt(request.invocation), request_id);
            self.send_own_invocation(request);
        }
        Ok(started)
    }

    /// The confirmation an app's legacy evidence object expresses.
    fn broker_confirmation(
        evidence: Option<&serde_json::Value>,
    ) -> Option<tilo_actions::remote::Confirmation> {
        let evidence = evidence?;
        if let Some(phrase) = evidence
            .get("confirmation_phrase")
            .and_then(serde_json::Value::as_str)
        {
            return Some(tilo_actions::remote::Confirmation::Phrase(
                phrase.to_string(),
            ));
        }
        (evidence.get("confirm").and_then(serde_json::Value::as_bool) == Some(true))
            .then_some(tilo_actions::remote::Confirmation::Confirmed)
    }

    fn dispatch_broker_request(
        next_request_id: &mut Option<u64>,
        connected: bool,
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        confirmation_evidence: Option<&serde_json::Value>,
        send: impl FnOnce(u64, tilo_actions::remote::InvokeRequest),
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        if !connected {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "authoritative_host_unavailable".into(),
                reason: "This operation requires the workspace action broker, but collaboration is offline".into(),
            });
        }
        let request_id = Self::allocate_broker_request_id(next_request_id)?;
        // The argument travels as the caller gave it: typed arguments as
        // such, a text argument as text, which the owning app binds to the
        // action's input as its own door does (a single parameter, a prompt).
        let argument = match (typed_arguments, argument) {
            (Some(arguments), _) => tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
            (None, Some(text)) => match text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX) {
                Some(json) => {
                    tilo_actions::remote::InvokeArgument::Json(serde_json::from_str(json).map_err(
                        |error| tilo_actions::DoorError::InvalidArguments(error.to_string()),
                    )?)
                }
                None => tilo_actions::remote::InvokeArgument::Text(text.to_owned()),
            },
            (None, None) => tilo_actions::remote::InvokeArgument::Json(serde_json::Map::new()),
        };
        send(
            request_id,
            tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::OWN_PROVIDER.into(),
                    address: address.clone(),
                },
                argument,
                context: None,
                confirmation: Self::broker_confirmation(confirmation_evidence),
                revision: None,
                wait_ms: None,
                appearance: None,
            },
        );
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }

    fn take_broker_request_id(&mut self) -> Result<u64, tilo_actions::DoorError> {
        Self::allocate_broker_request_id(&mut self.actions.next_broker_request_id)
    }
    fn allocate_broker_request_id(
        next_request_id: &mut Option<u64>,
    ) -> Result<u64, tilo_actions::DoorError> {
        let request_id = next_request_id.take().ok_or_else(|| {
            tilo_actions::DoorError::Unavailable {
                code: "broker_request_identity_exhausted".into(),
                reason: "This app has exhausted its broker request identities; restart it before invoking another hosted operation".into(),
            }
        })?;
        *next_request_id = request_id.checked_add(1);
        Ok(request_id)
    }

    /// This window's session door: its workspace broker connection.
    pub(super) fn broker_door(&mut self) -> BrokerDoor<'_> {
        BrokerDoor {
            manager: &mut self.collaboration.manager,
            broker_requests: &mut self.actions.broker_requests,
            next_broker_request_id: &mut self.actions.next_broker_request_id,
            pending_application_launches: &mut self.actions.pending_application_launches,
        }
    }

    fn invoke_document_resource_action(
        &mut self,
        invocation: &ResourceInvocation<'_>,
        resources: &[tilo_actions::ResourceSpec],
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        resource: &tilo_actions::ResourceSpec,
        spec: &tilo_actions::ActionSpec,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let ResourceInvocation {
            address,
            actor,
            argument,
            broker_admitted,
            confirmation_evidence,
            invocation,
            ambient_context,
            scope,
            ..
        } = *invocation;
        // The typed text of a prompt travels to the broker as the argument the
        // action itself names for it.
        let prompted_arguments = typed_arguments
            .is_none()
            .then(|| argument.and_then(|text| spec.prompt_arguments(text)))
            .flatten();
        let (broker_arguments, broker_argument) = if prompted_arguments.is_some() {
            (prompted_arguments.as_ref(), None)
        } else {
            (typed_arguments, argument)
        };
        if spec.deferred_invocation_type().is_some()
            && spec.locus == tilo_actions::ExecutionLocus::ServerPreferred
            && !broker_admitted
        {
            return self.invoke_via_broker(
                address,
                broker_arguments,
                broker_argument,
                confirmation_evidence,
            );
        }
        let source_invocation = broker_admitted.then_some(invocation).flatten();
        debug_assert!(
            !broker_admitted || source_invocation.is_some(),
            "admitted document request lost its invocation before dispatch: {invocation:?}"
        );
        let operation_invocation = invocation.unwrap_or_else(tilo_actions::InvocationId::new);
        let encoded = Self::encode_resource_action(resources, address, typed_arguments, argument)?;
        let action = self
            .actions
            .action_registry
            .decode(&encoded)
            .map_err(tilo_actions::DoorError::decoding)?;
        if spec.deferred_invocation_type().is_some() && action.locus() != spec.locus {
            return Err(tilo_actions::DoorError::Failed(
                "Deferred action locus differs from its declaration".into(),
            ));
        }
        if action.locus() == tilo_actions::ExecutionLocus::ServerPreferred && !broker_admitted {
            return self.invoke_via_broker(
                address,
                broker_arguments,
                broker_argument,
                confirmation_evidence,
            );
        }
        // Only admitted local document execution owns a new change boundary.
        // Encoding failures and broker routing must leave the caller's pending
        // edit intact; the executor still receives the same committed baseline.
        let before = self.state.doc.get_heads();
        let route = self.primary_document_route();
        let mut door = BrokerDoor {
            manager: &mut self.collaboration.manager,
            broker_requests: &mut self.actions.broker_requests,
            next_broker_request_id: &mut self.actions.next_broker_request_id,
            pending_application_launches: &mut self.actions.pending_application_launches,
        };
        let started = crate::invocation_scope::with_mounted_document(
            route.clone(),
            &mut self.state.doc,
            |doc| {
                crate::bound_dispatch::execute_resource_operation(
                    self.root_widget.as_mut(),
                    doc,
                    &self.state.root,
                    &self.actions.action_registry,
                    self.actions.action_transport.as_mut(),
                    crate::bound_dispatch::ResourceOperation {
                        target_route: route.clone(),
                        address,
                        resource,
                        spec,
                        encoded,
                        actor,
                        local_author: &self.collaboration.local_author.0,
                        broker_admitted,
                        invocation: operation_invocation,
                        source_invocation,
                        ambient: ambient_context,
                        scope,
                        door: Some(&mut door),
                    },
                )
            },
        )?;
        if before != self.state.doc.get_heads() {
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                Some(crate::permissions::UserId(actor.to_string())),
            );
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.invalidate_widget_resource_cache();
        }
        Ok(started)
    }

    fn invoke_bound_target(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        invocation: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_bound_target_with_broker(target, invocation, None)
    }

    fn bound_invocation_needs_broker(
        kind: tilo_actions::OperationKind,
        spec: &tilo_actions::ActionSpec,
        invocation: Option<tilo_actions::InvocationId>,
        has_delivery: bool,
        connected: bool,
    ) -> bool {
        ((kind == tilo_actions::OperationKind::Effect
            || (kind == tilo_actions::OperationKind::Document
                && spec.contract.execution != tilo_actions::ExecutionMode::Immediate))
            && invocation.is_none()
            && (has_delivery || connected))
            || (kind == tilo_actions::OperationKind::Document
                && spec.locus == tilo_actions::ExecutionLocus::ServerPreferred)
    }

    fn route_bound_invocation_to_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        resources: &[tilo_actions::ResourceSpec],
        arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        evidence: Option<&serde_json::Value>,
        broker_delivery: Option<&mut dyn FnMut(CollabRequest)>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.require_bound_broker_route(&target.binding)?;
        if !crate::bound_dispatch::validate_bound_resources(
            self.root_widget.as_mut(),
            &target.binding,
            resources,
        )
        .unwrap_or(false)
        {
            return Err(tilo_actions::DoorError::Unknown(
                target.address.address.clone(),
            ));
        }
        let alias = self
            .actions
            .bound_publication
            .alias(&target.binding, &target.address.address.resource)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
        let address =
            tilo_actions::ActionAddress::new(alias, target.address.address.action.clone());
        if let Some(send) = broker_delivery {
            let broker_requests = &mut self.actions.broker_requests;
            return Self::dispatch_broker_request(
                &mut self.actions.next_broker_request_id,
                true,
                &address,
                arguments,
                argument,
                evidence,
                |request_id, request| {
                    broker_requests.insert(BrokerReply::Receipt(request.invocation), request_id);
                    send(CollabRequest::RemoteAction {
                        message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                    });
                },
            );
        }
        self.invoke_via_broker(&address, arguments, argument, evidence)
    }

    /// The bound `target` names the action; `request.address` is not consulted.
    fn invoke_bound_target_with_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        request: &ResourceInvocation<'_>,
        broker_delivery: Option<&mut dyn FnMut(CollabRequest)>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let ResourceInvocation {
            actor,
            argument,
            confirmation_satisfied: confirmed,
            broker_admitted,
            invocation,
            ambient_context: ambient,
            scope,
            ..
        } = *request;
        let _entry = target.binding.enter().map_err(|error| {
            crate::bound_dispatch::BoundActionError::Dispatch(error).into_door()
        })?;
        if broker_admitted {
            self.require_bound_broker_route(&target.binding)?;
        }
        // The door resolves the action once and asks here, before anything
        // runs, whether it goes through the broker instead. Resolving it a
        // second time for that question built the whole published tree twice
        // per keystroke into a bound field.
        let has_delivery = broker_delivery.is_some();
        let connected = self.collaboration.manager.is_connected();
        let resolved = std::cell::Cell::new(None);
        let route = |spec: &tilo_actions::ActionSpec| {
            resolved.set(Some(spec.kind));
            !broker_admitted
                && Self::bound_invocation_needs_broker(
                    spec.kind,
                    spec,
                    invocation,
                    has_delivery,
                    connected,
                )
        };
        let id = invocation.unwrap_or_default();
        let mut door = BrokerDoor {
            manager: &mut self.collaboration.manager,
            broker_requests: &mut self.actions.broker_requests,
            next_broker_request_id: &mut self.actions.next_broker_request_id,
            pending_application_launches: &mut self.actions.pending_application_launches,
        };
        let started = crate::bound_dispatch::invoke_resource_action(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            target,
            self.actions.action_transport.as_mut(),
            Some(&mut door),
            crate::bound_dispatch::ResourceActionRequest {
                argument,
                arguments: None,
                actor,
                confirmed,
                broker_admitted,
                invocation: id,
                source_invocation: broker_admitted.then_some(invocation).flatten(),
                ambient,
                scope,
                route: Some(&route),
            },
        );
        let started = match started {
            Err(crate::bound_dispatch::BoundActionError::Rerouted) => {
                return self.route_resolved_invocation_to_broker(target, request, broker_delivery);
            }
            // The broker delivers only what this app publishes, never
            // another provider's action.
            Err(crate::bound_dispatch::BoundActionError::Hosted(hosted)) if broker_admitted => {
                return Err(tilo_actions::DoorError::Unknown(hosted.address));
            }
            Err(crate::bound_dispatch::BoundActionError::Hosted(hosted)) => {
                return self.invoke_hosted(hosted);
            }
            started => started,
        };
        let started = started.map_err(crate::bound_dispatch::BoundActionError::into_door);
        // Refused before the door resolved the action: nothing ran, as when
        // an unknown or unconfirmed action is refused before delivery.
        let Some(kind) = resolved.get() else {
            return started;
        };
        // A post-delivery fault retains canonical writes and must still refresh
        // the host projection. Generation is advanced by the checked frame.
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        self.invalidate_widget_resource_cache();
        let started = started?;
        if actor == self.collaboration.local_author.0 {
            self.take_over_owner_view();
        }
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            self.actions.bound_invocations.insert(id, target.clone());
            if invocation.is_none() {
                self.actions
                    .local_invocations
                    .insert(id, (target.address.address.clone(), kind));
            }
        }
        Ok(started)
    }

    /// Invoke a registered action as this application's current local person.
    /// This uses the same current record, policy and broker checks as the palette.
    /// # Errors
    /// Refuses unavailable, denied, stale or ambiguous actions and invalid arguments.
    pub fn invoke_local_resource_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let capture = self.begin_local_input_capture();
        let result = self.invoke_local_resource_action_captured(address, arguments);
        drop(capture);
        self.drain_local_input_capture();
        result
    }

    fn invoke_local_resource_action_captured(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let _entry = self.enter_wake_scope();
        let actor = self.collaboration.local_author.0.clone();
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(arguments.clone())
        );
        // The window's own Retry for unsaved changes is the host's, not the
        // app's: the same door its header button, the palette and assistive
        // technology reach.
        if address.resource == local_input_capture::UNSAVED_RESOURCE {
            return self
                .invoke_history_address_local(address, Some(&argument))
                .unwrap_or_else(|| Err(tilo_actions::DoorError::Unknown(address.clone())));
        }
        self.invoke_resource_action(
            ResourceInvocation::new(address, &actor).argument(Some(&argument)),
        )
    }

    /// Route through the broker an invocation the door resolved as one that
    /// runs there ([`crate::bound_dispatch::BoundActionError::Rerouted`]):
    /// the component's published resources are discovered and the broker
    /// is asked under the component's alias, exactly as before the door
    /// decided the route. Nothing ran locally.
    fn route_resolved_invocation_to_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        request: &ResourceInvocation<'_>,
        broker_delivery: Option<&mut dyn FnMut(CollabRequest)>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            Some(&target.address.address),
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_door())?;
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.binding.same_component(&target.binding))
            .ok_or_else(|| tilo_actions::DoorError::Unknown(target.address.address.clone()))?;
        let (arguments, _, _) = crate::bound_dispatch::resolve_resource_invocation(
            snapshot.resources(),
            &target.address.address,
            request.argument,
            None,
            request.confirmation_satisfied,
        )?;
        self.route_bound_invocation_to_broker(
            target,
            snapshot.resources(),
            arguments.as_ref(),
            request.argument,
            request.confirmation_evidence,
            broker_delivery,
        )
    }

    fn invoke_resource_action(
        &mut self,
        request: ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let local = request.actor == self.collaboration.local_author.0;
        let _ = crate::widget::take_presses();
        let started = (|| {
            use crate::bound_dispatch::local_target::{LocalResourceTarget, select_local_target};
            let address = request.address;
            if let Some(target) = self
                .actions
                .bound_publication
                .target(address)
                .map_err(crate::bound_dispatch::BoundActionError::into_door)?
            {
                self.require_bound_broker_route(&target.binding)?;
                return self.invoke_bound_target_with_broker(&target, &request, None);
            }
            match select_local_target(self.root_widget.as_ref(), &self.state.doc, address) {
                Ok(LocalResourceTarget::Component(target)) => {
                    self.invoke_bound_target_with_broker(&target, &request, None)
                }
                Ok(LocalResourceTarget::Outer) | Err(tilo_actions::DoorError::Unknown(_)) => {
                    self.invoke_unbound_resource_action(&request)
                }
                Err(error) => Err(error),
            }
        })()?;
        // A control chosen the way a person chooses it (a menu row) asked
        // to be pressed where it is drawn: pressed for this invoker, before
        // the caller learns it finished.
        for at in crate::widget::take_presses() {
            self.dispatch_semantic_click(at.x, at.y);
        }
        if local {
            self.take_over_owner_view();
        }
        Ok(started)
    }

    fn invoke_unbound_resource_action(
        &mut self,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let ResourceInvocation {
            address,
            actor,
            argument,
            confirmation_satisfied,
            broker_admitted,
            confirmation_evidence,
            invocation,
            ambient_context,
            scope,
        } = *request;
        if scope.is_some() {
            return Err(tilo_actions::DoorError::Failed(
                "required invocation has no actual canonical owner".into(),
            ));
        }
        // Ordinary local requests use the permission-projected, cached
        // resource surface (including retained bound rows).
        let mut resources =
            if let Some(participant) = crate::invocation_scope::current_participant() {
                // A participant's tree is rebuilt for it now, never read from the
                // cache of this window's own view.
                let mut resources = Vec::new();
                crate::bound_dispatch::collect_participant_resources(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                    &participant,
                    &mut resources,
                );
                resources
            } else if actor == self.collaboration.local_author.0 {
                self.collect_local_resources()
            } else {
                let key = self.ensure_resource_cache();
                let resources = self
                    .actions
                    .resource_cache
                    .borrow()
                    .as_ref()
                    .filter(|cache| cache.key == key)
                    .and_then(|cache| cache.complete.clone())
                    .unwrap_or_default();
                if actor == self.collaboration.local_author.0 {
                    resources
                        .into_iter()
                        .filter_map(|resource| self.filter_resource_for_local_actor(resource))
                        .collect()
                } else {
                    resources
                }
            };
        if tilo_actions::door::resolve(&resources, address).is_none() {
            let fallback = crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                &address.resource,
            );
            let fallback = if actor == self.collaboration.local_author.0 {
                fallback.and_then(|resource| self.filter_resource_for_local_actor(resource))
            } else {
                fallback
            };
            if let Some(resource) = fallback {
                resources.push(resource);
            }
        }
        if crate::undo::local_undo_status(self.root_widget.as_ref()).is_some() {
            resources.extend(self.edit_resource_for(address));
        }
        #[cfg(feature = "native")]
        if address.resource == crate::host_chrome::zoom::ZOOM_RESOURCE {
            resources.extend(self.zoom_resource());
        }
        let (typed_arguments, resource, resolved) = Self::resolve_resource_invocation(
            &resources,
            address,
            argument,
            confirmation_satisfied,
        )?;
        if let Some(scope) = scope {
            scope.check_dispatch_spec(resolved)?;
        }
        crate::invocation_scope::check_continuation(
            resolved,
            resolved.need_for(typed_arguments.as_ref(), argument).kind,
        )?;
        // The local participant's tree offers an action under the least it
        // needs; this invocation needs what its value asks.
        if actor == self.collaboration.local_author.0 {
            crate::bound_dispatch::admit_local_action(
                &self.permission_ctx,
                (&crate::collaboration::local_app_id(), resource, resolved),
                typed_arguments.as_ref(),
                argument,
                confirmation_satisfied || broker_admitted,
            )?;
        }
        let kind = resolved.kind;
        if resolved.contract.runs_on_invokers_device()
            && !crate::bound_dispatch::runs_own_device(
                actor,
                &self.collaboration.local_author.0,
                broker_admitted,
                ambient_context,
            )
        {
            return Err(crate::bound_dispatch::own_device_refusal());
        }
        if kind == tilo_actions::OperationKind::View
            && !crate::invocation_scope::view_operation_permitted(
                actor,
                &self.collaboration.local_author.0,
                &resolved.contract,
            )
        {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "private_view".into(),
                reason: "A collaborator cannot move this participant's private focus or viewport; use Follow or Reveal explicitly".into(),
            });
        }
        // Every locally initiated external effect or asynchronous document job
        // crosses the same broker
        // admission as an agent invocation before the handler touches the
        // outside world. Directed invocations carry a real id and
        // `broker_admitted=true`, so they execute exactly once when routed
        // back here. Standalone development runs retain the existing local
        // fallback below; installed Tilo apps always have the session broker.
        if (kind == tilo_actions::OperationKind::Effect
            || (kind == tilo_actions::OperationKind::Document
                && resolved.contract.execution != tilo_actions::ExecutionMode::Immediate))
            && invocation.is_none()
            && !broker_admitted
            && self.collaboration.manager.is_connected()
        {
            return self.invoke_via_broker(
                address,
                typed_arguments.as_ref(),
                argument,
                confirmation_evidence,
            );
        }
        if kind == tilo_actions::OperationKind::Document {
            let request = ResourceInvocation::new(address, actor)
                .argument(argument)
                .confirmed(confirmation_satisfied)
                .broker_admitted(broker_admitted)
                .evidence(confirmation_evidence)
                .invocation(invocation)
                .ambient_context(ambient_context)
                .scope(scope);
            return self.invoke_document_resource_action(
                &request,
                &resources,
                typed_arguments.as_ref(),
                resource,
                resolved,
            );
        }
        let _capture = if !broker_admitted && invocation.is_none() {
            let arguments = typed_arguments
                .clone()
                .or_else(|| argument.is_none().then(serde_json::Map::new));
            arguments.map(|arguments| {
                crate::local_document_action::with_invocation(
                    crate::local_document_action::PublishedInvocation {
                        resource: address.resource.clone(),
                        action: address.action.clone(),
                        arguments,
                    },
                )
            })
        } else {
            None
        };
        let encoded =
            Self::encode_resource_action(&resources, address, typed_arguments.as_ref(), argument)?;
        let mut context =
            resolved.history_context(resource.id.as_str(), &resource.kind, &resource.label);
        if let Some(ambient) = ambient_context {
            context = context.enriched_by(ambient);
        }
        let context =
            context.with_source_invocation(broker_admitted.then_some(invocation).flatten());
        let capture_local = !broker_admitted
            && invocation.is_none()
            && crate::local_document_action::input_is_active()
            && context.source_invocation.is_none()
            && !matches!(context.change, tilo_actions::ChangeKind::External);
        let before = if capture_local {
            self.state.doc.get_heads()
        } else {
            Vec::new()
        };
        let observation = if capture_local {
            crate::local_document_action::reserve(
                &encoded,
                Some(&context),
                &self.state.doc,
                &before,
            )
            .map_err(|error| tilo_actions::DoorError::Failed(error.to_string()))?
        } else {
            None
        };
        let captured_encoded = observation.as_ref().map(|_| encoded.clone());
        // This checked entry also owns built-in history and semantic handlers.
        // Nested typed dispatch belongs to this span, not a second admission.
        let suspension = observation
            .as_ref()
            .map(|_| crate::local_document_action::suspend());
        let result = (|| {
            // A launcher action has already passed the ordinary declaration,
            // argument-binding, confirmation, and broker-admission door above.
            // This branch runs only when the broker directs that admitted action
            // back to its source app, preserving the normal receipt lifecycle.
            if encoded.type_id == tilo_actions::remote::ApplicationLaunch::TYPE {
                let Some(invocation) = invocation else {
                    return Err(tilo_actions::DoorError::Failed(
                        "application launch was not admitted by the broker".into(),
                    ));
                };
                return crate::session_door::start_application_launch(
                    Some(&mut self.broker_door()),
                    None,
                    &encoded.payload,
                    invocation,
                    broker_admitted,
                );
            }
            let operation_invocation = invocation.unwrap_or_default();
            #[cfg(feature = "native")]
            if encoded.type_id == crate::host_chrome::zoom::ZOOM_OPERATION {
                self.execute_zoom_operation(&encoded)
                    .map_err(tilo_actions::DoorError::Failed)?;
                return Ok(crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ));
            }
            if encoded.type_id == crate::a11y_tree::SEMANTIC_OPERATION_TYPE {
                let semantic_result = self.invoke_semantic_operation_encoded(&encoded);
                let handled = match semantic_result {
                    Ok(handled) => handled,
                    Err(message) => {
                        if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                            self.collaboration
                                .manager
                                .send(CollabRequest::RecordLocalEffect {
                                    address: address.clone(),
                                    update: tilo_actions::InvocationUpdate::Failed {
                                        invocation: operation_invocation,
                                        error: tilo_actions::ActionFailure {
                                            code: "semantic_action_failed".into(),
                                            message: message.clone(),
                                            details: None,
                                            retryable: false,
                                        },
                                    },
                                });
                        }
                        return Err(tilo_actions::DoorError::Failed(message));
                    }
                };
                if !handled {
                    if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                        self.collaboration
                            .manager
                            .send(CollabRequest::RecordLocalEffect {
                                address: address.clone(),
                                update: tilo_actions::InvocationUpdate::Failed {
                                    invocation: operation_invocation,
                                    error: tilo_actions::ActionFailure {
                                        code: "unknown_action".into(),
                                        message:
                                            "the semantic target no longer accepts this action"
                                                .into(),
                                        details: None,
                                        retryable: false,
                                    },
                                },
                            });
                    }
                    return Err(tilo_actions::DoorError::Unknown(address.clone()));
                }
                // Focus/selection/navigation belong to private view state. A view
                // command must not finalize an unrelated in-progress document edit.
                if kind != tilo_actions::OperationKind::View {
                    self.commit_action();
                }
                self.needs_sync = true;
                self.layout.layout_dirty = true;
                if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                    self.collaboration
                        .manager
                        .send(CollabRequest::RecordLocalEffect {
                            address: address.clone(),
                            update: tilo_actions::InvocationUpdate::Succeeded {
                                invocation: operation_invocation,
                                output: tilo_actions::ActionOutput::default(),
                            },
                        });
                }
                return Ok(crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ));
            }
            let operation_result = {
                let route = self.primary_document_route();
                let mut door = BrokerDoor {
                    manager: &mut self.collaboration.manager,
                    broker_requests: &mut self.actions.broker_requests,
                    next_broker_request_id: &mut self.actions.next_broker_request_id,
                    pending_application_launches: &mut self.actions.pending_application_launches,
                };
                crate::invocation_scope::with_mounted_document(
                    route.clone(),
                    &mut self.state.doc,
                    |doc| {
                        crate::bound_dispatch::execute_resource_operation(
                            self.root_widget.as_mut(),
                            doc,
                            &self.state.root,
                            &self.actions.action_registry,
                            self.actions.action_transport.as_mut(),
                            crate::bound_dispatch::ResourceOperation {
                                target_route: route.clone(),
                                address,
                                resource,
                                spec: resolved,
                                encoded,
                                actor,
                                local_author: &self.collaboration.local_author.0,
                                broker_admitted,
                                invocation: operation_invocation,
                                source_invocation: broker_admitted.then_some(operation_invocation),
                                ambient: ambient_context,
                                scope: None,
                                door: Some(&mut door),
                            },
                        )
                    },
                )
            };
            let started = match operation_result {
                Ok(started) => started,
                Err(message) => {
                    if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                        let update = tilo_actions::InvocationUpdate::Failed {
                            invocation: operation_invocation,
                            error: tilo_actions::ActionFailure {
                                code: "action_failed".into(),
                                message: message.to_string(),
                                details: None,
                                retryable: false,
                            },
                        };
                        self.collaboration
                            .manager
                            .send(CollabRequest::RecordLocalEffect {
                                address: address.clone(),
                                update,
                            });
                    }
                    return Err(message);
                }
            };
            if invocation.is_none()
                && matches!(started, crate::widget::ResourceOperationStart::Accepted)
            {
                self.actions
                    .local_invocations
                    .insert(operation_invocation, (address.clone(), kind));
            }
            if matches!(started, crate::widget::ResourceOperationStart::Accepted)
                || kind == tilo_actions::OperationKind::View
            {
                // Queued work and completed navigation apply their private
                // presentation state in `sync`, even when the invocation
                // changed no document and no input event follows it.
                self.needs_sync = true;
            }
            if let crate::widget::ResourceOperationStart::Completed(output) = &started
                && kind == tilo_actions::OperationKind::Effect
                && invocation.is_none()
            {
                self.collaboration
                    .manager
                    .send(CollabRequest::RecordLocalEffect {
                        address: address.clone(),
                        update: tilo_actions::InvocationUpdate::Succeeded {
                            invocation: operation_invocation,
                            output: output.clone(),
                        },
                    });
            }
            // Effect/view operations are allowed to switch presentation-only
            // resources without touching Automerge.
            self.invalidate_widget_resource_cache();
            self.layout.layout_dirty = true;
            Ok(started)
        })();
        drop(suspension);
        if let (Some(observation), Some(encoded)) = (observation, captured_encoded) {
            observation
                .finish(&mut self.state.doc, &before, encoded, Some(context))
                .map_err(|error| tilo_actions::DoorError::Failed(error.to_string()))?;
        }
        result
    }

    fn service_local_control_history(&mut self) -> bool {
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("idle.controls");
        let controls = self.dispatch_control_invocations();
        #[cfg(test)]
        drop(probe);
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("idle.history");
        let history = self.service_history();
        #[cfg(test)]
        drop(probe);
        let refused = !self.actions.refusals.is_empty();
        self.say_refusals();
        let changed = controls != 0 || history || refused;
        if changed {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        changed
    }

    /// Review pre-entry controls retained by the native managed History surface.
    pub fn pending_history_controls(
        &self,
    ) -> Vec<crate::history_mode::pending_controls::PendingControlReview> {
        self.history
            .as_ref()
            .map_or_else(Vec::new, |history| history.pending_control_reviews())
    }

    /// Drives the production local idle body without creating a window or input.
    #[cfg(any(test, feature = "test-support"))]
    pub fn controlled_local_idle_for_test(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        self.service_local_control_history()
    }

    /// Resolve queued local control intents against the current resource tree
    /// and enter through the same door/lifecycle as every other caller.
    pub(super) fn dispatch_control_invocations(&mut self) -> usize {
        let _phase = tilo_resource::profiling::Phase::start("ui.dispatch_controls");
        // What an earlier pass refused and has not said yet is said first.
        self.say_refusals();
        self.service_close_admission();
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input())
        {
            self.discard_history_local_intents();
            return 0;
        }
        // Embedded History refresh discards only its own hidden live queue.
        // Defer sibling work until this input frame ends without consuming it.
        if self.framework_input.get() {
            return 0;
        }
        // A committed refresh temporarily closes the original native history
        // gate. Keep control/causal intents with their existing producers until
        // the owner applies that capture and its worker rechecks the route.
        // Nothing has crossed the action door yet; accepted deliveries retain
        // the separate fail-closed guard in invoke_published_action.
        if self.collaboration.manager.has_native_committed_primary()
            && !self.published_action_history_ready()
        {
            return 0;
        }
        crate::history_mode::embedded::discard_active_output(self.root_widget.as_mut());
        let registered = self.control_domain.drain_partitioned();
        self.dispatch_drained_controls(registered)
    }

    /// Invoke, as the person here, another provider's action a control of
    /// this window is bound to ([`crate::bound_dispatch::BoundActionError::Hosted`]):
    /// the provider admits and runs it as any caller's, through the broker.
    fn invoke_hosted(
        &mut self,
        hosted: crate::bound_dispatch::HostedInvocation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        if !self.collaboration.manager.is_connected() {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "authoritative_host_unavailable".into(),
                reason: format!(
                    "`{}` runs this, and the workspace action broker is offline",
                    hosted.provider
                ),
            });
        }
        let request_id = self.take_broker_request_id()?;
        let request = tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: hosted.provider,
                address: hosted.address,
            },
            argument: hosted.argument,
            context: None,
            confirmation: hosted
                .confirmed
                .then_some(tilo_actions::remote::Confirmation::Confirmed),
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        self.actions
            .broker_requests
            .insert(BrokerReply::Receipt(request.invocation), request_id);
        self.actions.hosted_requests.insert(request_id);
        self.collaboration
            .manager
            .send(CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
            });
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }

    /// The provider answered the hosted invocation `request_id`
    /// ([`Self::invoke_hosted`]): what it refused is said over the window,
    /// as a refusal here is. Whether it was one.
    pub(super) fn settle_hosted(
        &mut self,
        request_id: u64,
        result: &Result<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate), String>,
    ) -> bool {
        if !self.actions.hosted_requests.remove(&request_id) {
            return false;
        }
        let refused = match result {
            Err(reason) => Some(reason.clone()),
            Ok((_, tilo_actions::InvocationUpdate::Failed { error, .. })) => {
                Some(error.message.clone())
            }
            Ok(_) => None,
        };
        if let Some(reason) = refused {
            self.refuse_in_window(&tilo_actions::DoorError::Failed(reason));
        }
        true
    }

    fn request_invocation_cancellation(&mut self, wire_id: u64) {
        if let Some(pending) = self
            .actions
            .directed_invocations
            .values_mut()
            .find(|pending| pending.wire_id == wire_id)
        {
            pending.cancellation_requested = true;
        }
    }

    fn fail_pending_application_launches_on_disconnect(&mut self) -> bool {
        if self.collaboration.manager.is_connected() {
            return false;
        }

        let launches = std::mem::take(&mut self.actions.pending_application_launches);
        let mut changed = false;
        for (_request_id, invocation) in launches {
            let Some(directed) = self.actions.directed_invocations.remove(&invocation) else {
                continue;
            };
            self.collaboration.manager.send(CollabRequest::UpdateInvocation {
                id: directed.wire_id,
                update: tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "application_launch_broker_disconnected".into(),
                        message: "The launcher broker disconnected before confirming the application launch".into(),
                        details: None,
                        retryable: true,
                    },
                },
            });
            changed = true;
        }
        changed
    }

    /// Give retained widgets a non-blocking lifecycle tick and forward every
    /// progress/item/terminal update to the broker. A terminal update removes
    /// the job before it is sent, making terminal delivery at-most-once even
    /// if a widget accidentally returns the same update again.
    #[allow(
        clippy::too_many_lines,
        reason = "directed and local terminal delivery share the existing status sweep"
    )]
    fn poll_directed_invocations(&mut self) -> bool {
        // A broker disconnect cannot leave an admitted launcher action
        // Accepted forever. Draining the bounded table also makes late or
        // duplicate receipts inert because their request ids are gone.
        let mut changed = self.fail_pending_application_launches_on_disconnect();
        let invocations = self
            .actions
            .directed_invocations
            .iter()
            .map(|(invocation, pending)| (*invocation, pending.clone()))
            .collect::<Vec<_>>();
        for (invocation, pending) in invocations {
            if self.action_publication_failure.is_some() {
                break;
            }
            let authority = self.check_published_continuation(pending.authorization.as_ref());
            let polled = if let Err(reason) = authority {
                // The shared per-job scope is now permanently retired. Pending
                // model work retains that scope and cannot resurrect after this
                // terminal receipt; unrelated jobs remain runnable.
                Ok(Some(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "published_invocation_unauthorized".into(),
                        message: reason,
                        details: None,
                        retryable: false,
                    },
                }))
            } else if let Some(target) = self.actions.bound_invocations.get(&invocation) {
                crate::bound_dispatch::poll_resource_operation(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    target,
                    invocation,
                    pending.cancellation_requested,
                )
            } else {
                Ok(crate::widget::poll_resource_operation(
                    &mut *self.root_widget,
                    invocation,
                    pending.cancellation_requested,
                    &self.state.doc,
                    &self.state.root,
                ))
            };
            let mut update = match polled {
                Ok(Some(update)) => update,
                Ok(None) => continue,
                Err(_) => {
                    if let Some(scope) = &pending.authorization {
                        scope.cancel();
                    }
                    self.actions.bound_invocations.remove(&invocation);
                    self.actions.directed_invocations.remove(&invocation);
                    continue;
                }
            };
            if update.invocation() != invocation
                || matches!(update, tilo_actions::InvocationUpdate::Accepted { .. })
            {
                update = tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "tilo.invalid_lifecycle_update".into(),
                        message: "operation handler returned an invalid lifecycle update".into(),
                        details: None,
                        retryable: false,
                    },
                };
            }
            if update.is_terminal() {
                // What the effect's completion submitted is applied for its
                // invoker before its caller learns it finished.
                self.dispatch_control_invocations();
                if let Some(continuation) = self.actions.acting_for.get_mut(&invocation) {
                    if !continuation.outstanding.is_empty() {
                        if let Some(scope) = &pending.authorization {
                            scope.cancel();
                        }
                        // It answers once the host admitted its edits.
                        continuation.deferred = Some((pending.wire_id, update));
                        self.actions.bound_invocations.remove(&invocation);
                        self.actions.directed_invocations.remove(&invocation);
                        changed = true;
                        continue;
                    }
                    update = continuation.settle(update);
                }
            }
            if update.is_terminal() && !self.queue_documents_before_terminal() {
                // Keep admitted lifecycle state; do not poll the completed
                // operation again or fabricate a no-effect refusal.
                changed = true;
                break;
            }
            if update.is_terminal() {
                if let Some(scope) = &pending.authorization {
                    scope.cancel();
                }
                self.actions.bound_invocations.remove(&invocation);
                self.actions.directed_invocations.remove(&invocation);
                self.actions.acting_for.remove(&invocation);
                // As a direct answer, it waits for what it caused to land.
                self.hold_answer(pending.wire_id, update);
            } else {
                self.collaboration
                    .manager
                    .send(CollabRequest::UpdateInvocation {
                        id: pending.wire_id,
                        update,
                    });
            }
            changed = true;
        }
        // An invocation that ended however it ended runs for nobody, once
        // the edits it submitted are settled.
        let directed = &self.actions.directed_invocations;
        self.actions.acting_for.retain(|invocation, continuation| {
            directed.contains_key(invocation)
                || !continuation.outstanding.is_empty()
                || continuation.deferred.is_some()
        });
        let local_invocations = self
            .actions
            .local_invocations
            .iter()
            .map(|(invocation, (address, kind))| (*invocation, address.clone(), *kind))
            .collect::<Vec<_>>();
        for (invocation, address, kind) in local_invocations {
            let polled = if let Some(target) = self.actions.bound_invocations.get(&invocation) {
                crate::bound_dispatch::poll_resource_operation(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    target,
                    invocation,
                    false,
                )
            } else {
                Ok(crate::widget::poll_resource_operation(
                    &mut *self.root_widget,
                    invocation,
                    false,
                    &self.state.doc,
                    &self.state.root,
                ))
            };
            let update = match polled {
                Ok(Some(update)) => update,
                Ok(None) => continue,
                Err(_) => {
                    self.actions.bound_invocations.remove(&invocation);
                    self.actions.local_invocations.remove(&invocation);
                    if self.close_admission.save_local == Some(invocation) {
                        self.close_admission.save_result =
                            Some(Err("save target is no longer available".into()));
                    }
                    continue;
                }
            };
            let invalid_identity = update.invocation() != invocation;
            if !invalid_identity && self.close_admission.save_local == Some(invocation) {
                self.observe_close_save(&update);
            } else if invalid_identity && self.close_admission.save_local == Some(invocation) {
                self.close_admission.save_result =
                    Some(Err("save returned an invalid invocation identity".into()));
            }
            if update.is_terminal() || invalid_identity {
                self.actions.bound_invocations.remove(&invocation);
                self.actions.local_invocations.remove(&invocation);
            }
            if (update.is_terminal() || invalid_identity)
                && kind == tilo_actions::OperationKind::Effect
            {
                let update = if invalid_identity {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "tilo.invalid_lifecycle_update".into(),
                            message: "operation handler returned an update for another invocation"
                                .into(),
                            details: None,
                            retryable: false,
                        },
                    }
                } else {
                    update
                };
                self.collaboration
                    .manager
                    .send(CollabRequest::RecordLocalEffect { address, update });
            }
            changed = true;
        }
        if changed {
            self.invalidate_widget_resource_cache();
            // Accepted operations can finish from a later model pass without
            // another broker or native input event. Their progress/terminal
            // transition may change document-dependent operations, so refresh
            // the broker surface from this lifecycle boundary as well.
            self.publish_action_surface();
        }
        changed
    }

    pub fn open_action_palette(&mut self) {
        let _entry = self.enter_wake_scope();
        let (key, permission_generation) = self.ensure_local_resource_cache();
        let mut resources = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.key == key && cache.permission_generation == permission_generation
            })
            .and_then(|cache| cache.resources.clone())
            .unwrap_or_default();
        resources.extend(self.history_local_resources());
        let cached_open_resources = {
            self.actions
                .open_resource_search_cache
                .borrow()
                .as_ref()
                .filter(|cache| {
                    cache.key == key && cache.permission_generation == permission_generation
                })
                .map(|cache| cache.provider.clone())
        };
        let open_resources = cached_open_resources.unwrap_or_else(|| {
            let provider = crate::search::OpenResourceSearch::new(&resources);
            *self.actions.open_resource_search_cache.borrow_mut() = Some(OpenResourceSearchCache {
                key: key.clone(),
                permission_generation,
                provider: provider.clone(),
            });
            provider
        });
        let bound = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        let mut providers = crate::bound_dispatch::search_providers(self.root_widget.as_ref());
        providers.push(Box::new(open_resources));
        let mut palette = match self.palette_source {
            PaletteSource::Owned { renderer, field } => {
                crate::action_palette::ActionPalette::with_inputs(renderer(), field)
            }
            PaletteSource::Host => crate::action_palette::ActionPalette::with_host_text(
                self.text_renderer.font_system.locale().to_owned(),
            ),
        };
        palette.load_open_resources(&resources);
        palette.load_bound_resources(bound);
        palette.set_providers(providers);
        palette.open();
        self.actions.palette_window = self.tree().invoking_window_rect();
        // Discovery reads the still-admitted current tree; clear its pointers
        // immediately before replacing the retained modal instance.
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        self.actions.action_palette = Some(Box::new(palette));
        self.rebuild_action_palette_admission();
    }

    /// Open search the way `request` asks: this app, or one part of it, with
    /// the query already typed. A standalone window shows its own palette;
    /// inside the Tilo shell the component hands the request to the shell.
    pub fn open_search(&mut self, request: crate::search::SearchRequest) {
        if self.hand_search_to_session(&request) {
            return;
        }
        self.open_action_palette();
        if self
            .actions
            .action_palette
            .as_deref_mut()
            .is_some_and(|palette| palette.open_on(&request))
        {
            self.capture_ordinary_palette_request();
        }
    }

    /// Inside a Tilo session there is one search, the shell's: hand it
    /// `request` and open nothing here. A window the shell hosts in its own
    /// process asks through its component's host intent; one running as a
    /// process of its own asks over the session's surface service. Whether
    /// the request went to the session.
    pub(super) fn hand_search_to_session(
        &mut self,
        request: &crate::search::SearchRequest,
    ) -> bool {
        if self.floating.client.is_some() {
            self.floating.search_requests.push(request.clone());
            return true;
        }
        if !self.actions.search_in_host || self.presents_search() {
            return false;
        }
        let Some(bound) = self
            .root_widget
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::widget::BoundAppWidget>())
        else {
            return false;
        };
        bound
            .component
            .callback_binding()
            .request_search(request.clone());
        true
    }

    /// `/` pressed with nothing focused that types it: open search of this
    /// app. A text field consumes `/` before this runs, and a focused widget
    /// that takes text (a terminal, a canvas with an IME) keeps it, so
    /// typing a path or a fraction never opens search.
    #[cfg(feature = "native")]
    pub(crate) fn handle_unconsumed_search_key(
        &mut self,
        event: &winit::event::WindowEvent,
    ) -> bool {
        let winit::event::WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Character(character),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let modifiers = self.input.modifiers.state();
        if character.as_str() != "/"
            || modifiers.control_key()
            || modifiers.alt_key()
            || modifiers.super_key()
            || !self.presents_search()
            || self.actions.action_palette.is_some()
            || self.ime_window_state().is_some()
        {
            return false;
        }
        self.open_search(crate::search::SearchRequest::this_app());
        true
    }

    /// A printable key no widget took, with a selection in focus and nothing
    /// focused that takes text: the person is talking about the selection.
    /// The desktop's agent composer opens on it with the key in its field,
    /// where their typing goes on. Shortcuts, a text field, a terminal and
    /// an input method all take their keys first, and without a desktop
    /// agent nothing happens.
    #[cfg(feature = "native")]
    pub(crate) fn handle_unconsumed_typed_key(&self, event: &winit::event::WindowEvent) -> bool {
        let winit::event::WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    text: Some(text),
                    state: winit::event::ElementState::Pressed,
                    repeat: false,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let modifiers = self.input.modifiers.state();
        if text.is_empty()
            || text.chars().any(|c| c.is_control() || c.is_whitespace())
            || modifiers.control_key()
            || modifiers.alt_key()
            || modifiers.super_key()
            || !crate::shared_selection::agent_host_available()
            || self.actions.action_palette.is_some()
            || self.ime_window_state().is_some()
        {
            return false;
        }
        let Some(selection) = crate::shared_selection::subtree_selection(self.root_widget.as_ref())
        else {
            return false;
        };
        crate::shared_selection::ask_agent_typing(selection, text);
        true
    }

    /// Whether this window presents search itself. A shell hosting the app
    /// presents it instead, and receives the request as a host intent.
    fn presents_search(&self) -> bool {
        self.search_presented
    }

    fn rebuild_action_palette_admission(&mut self) {
        // A synchronous action may need the current canonical widget map
        // before the next redraw. Retire the old tree through the normal
        // rebuild lifecycle; never append another host tree with init_layout.
        let pending_sync = self.needs_sync;
        let generation = self.actions.resource_generation.get();
        let open_index = self.actions.open_resource_search_cache.get_mut().take();
        self.layout.request_rebuild();
        self.rebuild_structure();
        // Admitting a modal declares no resource of the tree the open-resource
        // index was just built from. Keep that index across the rebuild's own
        // invalidation so reopening reuses it until state actually mutates.
        if let Some(mut index) = open_index
            && index.key.mutation_generation == generation
            && self.actions.resource_generation.get() == generation.saturating_add(1)
            && index.permission_generation == self.actions.permission_generation.get()
        {
            index.key = self.resource_cache_key();
            *self.actions.open_resource_search_cache.get_mut() = Some(index);
        }
        // Synchronous admission must not consume the dispatch's obligation to
        // sync during normal redraw, even if rebuilding also syncs new widgets.
        self.needs_sync |= pending_sync;
        // Admission is not a layout solve. Keep the native redraw's actual
        // structural-change decision true so fresh nodes are measured before
        // apply, even when the retained background and viewport are unchanged.
        self.layout.request_rebuild();
    }

    /// [`Self::refuse_visibly`] for a refusal that arrives with no control
    /// on screen (a host's or a remote's answer, later): said in the
    /// window's bar.
    fn refuse_in_window(&mut self, error: &tilo_actions::DoorError) {
        self.refuse_at(error, None);
    }

    fn refuse_at(&mut self, error: &tilo_actions::DoorError, anchor: Option<vello::kurbo::Rect>) {
        // Said once the pass ends ([`Self::say_refusals`]): the window's own
        // nodes stay mapped until then, so what the pass still records of
        // them (an edit's history) is kept.
        self.actions
            .refusals
            .push((crate::bound_dispatch::refusal_reason(error), anchor));
        self.layout.layout_dirty = true;
    }

    /// Say why this pass's refused invocations did not run: the last one,
    /// at its control or in the window's bar.
    fn say_refusals(&mut self) {
        let Some((reason, anchor)) = self.actions.refusals.pop() else {
            return;
        };
        self.actions.refusals.clear();
        match anchor {
            Some(anchor) => {
                if let Some(reader) = self.accessibility.screen_reader.as_mut() {
                    reader.announce(reason.clone(), tts::Priority::Notification, true);
                }
                self.actions.control_notice = Some(crate::control_notice::ControlNotice {
                    text: reason,
                    anchor,
                });
                self.request_redraw_for_notice();
            }
            None => crate::control_notice::say_in_window(&reason),
        }
    }

    /// What stands at the control the person last used: why what they did
    /// did not run, until their next press or key.
    #[must_use]
    pub fn control_notice(&self) -> Option<&crate::control_notice::ControlNotice> {
        self.actions.control_notice.as_ref()
    }

    /// The person pressed or typed again: what stood at their last control
    /// is done with.
    #[cfg(feature = "native")]
    pub(super) fn dismiss_control_notice(&mut self) {
        if self.actions.control_notice.take().is_some() {
            self.request_redraw_for_notice();
        }
    }

    fn request_redraw_for_notice(&mut self) {
        self.layout.layout_dirty = true;
    }

    fn capture_ordinary_palette_request(&mut self) {
        let Some(pending) = self
            .actions
            .action_palette
            .as_deref()
            .and_then(|p| p.ordinary_capture_request())
        else {
            return;
        };
        let result = (|| {
            let target = match pending.bound.clone() {
                Some(target) => {
                    crate::bound_dispatch::local_target::LocalResourceTarget::Component(target)
                }
                None => crate::bound_dispatch::local_target::select_local_target(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                    &pending.address,
                )?,
            };
            let arguments = pending.continuation_arguments().ok_or_else(|| {
                tilo_actions::DoorError::InvalidArguments("Invalid confirmation arguments".into())
            })?;
            crate::bound_dispatch::OrdinaryConfirmation::prepare(
                self.root_widget.as_ref(),
                &self.state.doc,
                target,
                &self.control_domain,
                pending.address,
                arguments,
            )
        })();
        if let Some(palette) = self.actions.action_palette.as_deref_mut() {
            match result {
                Ok(request) => palette.retain_ordinary(request),
                Err(_) => palette.close(),
            }
        }
    }

    /// Open search on the app's action `address`, asking for its
    /// arguments: the session's search inside a session, the window's own
    /// outside one. Whether it opened.
    #[cfg(feature = "native")]
    pub(super) fn open_search_at_action(&mut self, address: &tilo_actions::ActionAddress) -> bool {
        let asked = crate::search::SearchRequest::asking(
            crate::search::ScopeDecl::new(address.resource.clone(), String::new()),
            address.action.clone(),
        );
        if self.hand_search_to_session(&asked) {
            return true;
        }
        self.open_action_palette_at(address)
    }

    fn open_action_palette_at(&mut self, address: &tilo_actions::ActionAddress) -> bool {
        self.open_action_palette();
        let activated = self
            .actions
            .action_palette
            .as_deref_mut()
            .is_some_and(|palette| palette.activate_address(&address.resource, &address.action));
        if activated {
            self.capture_ordinary_palette_request();
        } else {
            self.close_action_palette();
        }
        activated
    }

    fn filter_resource_for_local_actor(
        &self,
        resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        self.permission_ctx
            .permitted_resource(&crate::collaboration::local_app_id(), resource)
    }

    pub fn close_action_palette(&mut self) {
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        self.layout.request_rebuild();
        self.actions.action_palette = None;
        if self.actions.ordinary_palette_return_focus.is_some() {
            self.rebuild_action_palette_admission();
            // Admission still requires a measured structural rebuild. Keep the
            // stable opener identity until that final tree has settled, rather
            // than restoring a NodeId that the next rebuild invalidates.
        }
    }

    #[cfg(feature = "native")]
    pub(super) fn restore_ordinary_palette_focus(&mut self) {
        if self.actions.action_palette.is_some() {
            return;
        }
        let Some(focus) = self.actions.ordinary_palette_return_focus.take() else {
            return;
        };
        if self.accessibility.focus_trap_active {
            return;
        }
        // SAFETY: the layout solve has settled the exclusively owned tree and
        // its current custody map. Removed or ambiguous openers cannot match.
        self.focused_node = unsafe {
            focus.restore(
                &self.layout.node_map,
                &self.bindings,
                Some(&self.focusable_order),
            )
        };
    }

    /// Run `f` on the root of the app component this window shows, inside
    /// that component's own frame: for a test, or a host, reaching into the
    /// mounted app between frames. `None` without a mounted component.
    pub fn with_component_root<U>(&mut self, f: impl FnOnce(&mut dyn Widget) -> U) -> Option<U> {
        // Whatever `f` does to it is no change the tracking sees.
        self.mark_semantic_tree_stale();
        let bound = self
            .root_widget
            .as_any_mut()?
            .downcast_mut::<crate::widget::BoundAppWidget>()?;
        let binding = bound.component.callback_binding();
        let _entry = binding.enter().ok()?;
        Some(f(bound.component.root.as_deref_mut()?))
    }

    pub fn action_palette(&self) -> Option<&crate::action_palette::ActionPalette> {
        self.actions.action_palette.as_deref()
    }

    /// The action the host's confirmation is asking about, and the
    /// question it asks, while it asks.
    #[must_use]
    pub fn asking(&self) -> Option<(tilo_actions::ActionAddress, String)> {
        self.actions
            .action_palette
            .as_deref()
            .and_then(crate::action_palette::ActionPalette::asking)
            .map(|(address, expected)| (address.clone(), expected.to_owned()))
    }

    /// Confirm what the host asks through its own dialog, as the person
    /// does: activate its button named for the verb (a destructive question
    /// starts on Cancel, so Enter would not go ahead); what it confirmed is
    /// applied. Answers the action confirmed; `None`, changing nothing, when
    /// nothing is asked or the dialog refused the activation.
    #[cfg(feature = "native")]
    pub fn confirm_asked(&mut self) -> Option<tilo_actions::ActionAddress> {
        let (address, _question) = self.asking()?;
        self.refresh_action_palette_resources();
        let palette = self.actions.action_palette.as_deref_mut()?;
        let target = palette.confirming_target()?;
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        let handled = palette.perform_semantic_action(
            &target,
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        self.finish_action_palette_input(handled);
        self.dispatch_control_invocations();
        handled.then_some(address)
    }

    #[cfg(feature = "native")]
    fn refresh_action_palette_resources(&mut self) {
        // A palette can stay open while document state or the local actor's
        // permissions change. Refresh its authority snapshot immediately
        // before activation; rows retain addresses, never the callbacks or
        // encoded actions from the frame in which search found them.
        // The unbound cache includes projected retained semantic controls as
        // well as app resources. Keep both when refreshing a pending row.
        let (key, generation) = self.ensure_local_resource_cache();
        let mut resources = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key && cache.permission_generation == generation)
            .and_then(|cache| cache.resources.clone())
            .unwrap_or_default();
        resources.extend(self.history_local_resources());
        let lazy_resource_ids = self
            .actions
            .action_palette
            .as_deref()
            .map(crate::action_palette::ActionPalette::referenced_action_resource_ids)
            .unwrap_or_default();
        for id in lazy_resource_ids {
            if let Some(resource) = crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                &id,
            )
            .and_then(|resource| self.filter_resource_for_local_actor(resource))
            {
                resources.push(resource);
            }
        }
        let mut bound = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        let targets = self
            .actions
            .action_palette
            .as_deref()
            .map(crate::action_palette::ActionPalette::referenced_bound_targets)
            .unwrap_or_default();
        for target in targets {
            if let Some(snapshot) = bound
                .iter_mut()
                .find(|snapshot| snapshot.binding.same_component(&target.binding))
                && tilo_actions::door::resolve(snapshot.resources(), &target.address.address)
                    .is_none()
                && let Ok(Some(resource)) = crate::bound_dispatch::resolve_component_resource(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    &target.binding,
                    &target.address.address.resource,
                    &target.address.address.action,
                )
            {
                snapshot.extend([resource]);
            }
        }
        if let Some(palette) = self.actions.action_palette.as_deref_mut() {
            palette.refresh_open_resources(&resources);
            palette.load_bound_resources(bound);
        }
    }

    #[cfg(feature = "native")]
    fn action_palette_handle_event(&mut self, event: &WindowEvent, scale: f64) -> bool {
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            return self.action_palette_handle_key(event);
        }
        if !self
            .actions
            .action_palette
            .as_deref()
            .is_some_and(|palette| palette.is_presented())
        {
            return false;
        }
        self.refresh_action_palette_resources();
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        let consumed = self
            .actions
            .action_palette
            .as_deref_mut()
            .is_some_and(|palette| {
                palette.route_event(
                    event,
                    scale,
                    self.input.modifiers,
                    &mut crate::widget::Edits::lend(&mut self.state.doc),
                    &self.state.root,
                )
            });
        self.finish_action_palette_input(consumed)
    }

    #[cfg(feature = "native")]
    fn action_palette_handle_key(&mut self, event: &WindowEvent) -> bool {
        if self.actions.action_palette.is_none() {
            return false;
        }
        self.refresh_action_palette_resources();
        let Some(palette) = self.actions.action_palette.as_deref_mut() else {
            return false;
        };
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        let root = self.state.root.clone();
        let consumed = palette.handle_key(
            event,
            &self.input.modifiers,
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &root,
        );
        self.finish_action_palette_input(consumed)
    }

    fn finish_action_palette_input(&mut self, consumed: bool) -> bool {
        // The transition may have replaced the field or closed the overlay.
        // Re-admission below retires the old tree and preserves the real
        // redraw's structural-change condition for the next layout solve.
        self.layout.request_rebuild();
        let root = self.state.root.clone();
        self.capture_ordinary_palette_request();
        let exact_capture = self
            .actions
            .action_palette
            .as_deref()
            .and_then(crate::action_palette::ActionPalette::history_exact_capture_request);
        if let Some((target, invocation, original_arguments)) = exact_capture {
            let witness = crate::bound_dispatch::control_witness_with_arguments(
                self.root_widget.as_ref(),
                &self.state.doc,
                &target,
                &invocation,
                &original_arguments,
            )
            .map_err(|error| error.to_string());
            if let Some(palette) = self.actions.action_palette.as_deref_mut() {
                palette.retain_history_exact_witness(witness);
            }
        }
        // An operation row may have fired. Document mutations go through the
        // deterministic executor; effects/views route to the owning widget.
        let pending = self
            .actions
            .action_palette
            .as_deref_mut()
            .and_then(crate::action_palette::ActionPalette::take_operation);
        if let Some(pending) = pending {
            // Projected View actions resolve their retained widget through the
            // current checked map, including after the palette has closed.
            self.rebuild_action_palette_admission();
            let argument = pending.wire_argument();
            let continuation_arguments = pending.continuation_arguments();
            let actor = self.collaboration.local_author.0.clone();
            let palette_request = ResourceInvocation::new(&pending.address, &actor)
                .argument(argument.as_deref())
                .confirmed(true)
                .evidence(Some(pending.confirmation_evidence()))
                .ambient_context(Some(&pending.context));
            // Confirmation was collected by the palette, but the request has
            // not crossed the session broker yet. Host-required documents are
            // therefore submitted to that broker rather than applied here.
            let original_valid = pending.ordinary.as_ref().map_or(Ok(()), |request| {
                request.validate(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                    &self.control_domain,
                )
            });
            let result = if let Err(error) = original_valid {
                Err(error)
            } else if let Some(target) = pending.history_target.clone() {
                let draft = match pending.history_draft_identity.as_ref() {
                    Some(Ok(captured)) => crate::bound_dispatch::control_draft_identity(
                        self.root_widget.as_ref(),
                        &self.state.doc,
                        &target,
                        &pending.address,
                    )
                    .and_then(|current| {
                        (current == *captured).then_some(()).ok_or_else(|| {
                            tilo_actions::DoorError::Unavailable {
                                code: "stale_pending_palette_action".into(),
                                reason: "The palette action resolution domain changed while History was open".into(),
                            }
                        })
                    }),
                    Some(Err(reason)) => Err(tilo_actions::DoorError::Unavailable {
                        code: "stale_pending_palette_action".into(),
                        reason: reason.clone(),
                    }),
                    None => Ok(()),
                };
                let captured = pending
                    .history_identity
                    .as_ref()
                    .ok_or_else(|| tilo_actions::DoorError::Unavailable {
                        code: "stale_pending_palette_action".into(),
                        reason: "The original palette action identity was not captured".into(),
                    })
                    .and_then(|identity| {
                        identity
                            .clone()
                            .map_err(|reason| tilo_actions::DoorError::Unavailable {
                                code: "stale_pending_palette_action".into(),
                                reason,
                            })
                    });
                let current = if pending.history_witness.is_some() {
                    pending
                        .history_identity
                        .as_ref()
                        .ok_or_else(|| tilo_actions::DoorError::Unavailable {
                            code: "stale_pending_palette_action".into(),
                            reason: "The original exact palette identity was not captured".into(),
                        })
                        .and_then(|identity| {
                            identity.clone().map_err(|reason| {
                                tilo_actions::DoorError::Unavailable {
                                    code: "stale_pending_palette_action".into(),
                                    reason,
                                }
                            })
                        })
                } else {
                    crate::bound_dispatch::control_draft_identity(
                        self.root_widget.as_ref(),
                        &self.state.doc,
                        &target,
                        &pending.address,
                    )
                };
                let witness = match pending.history_witness.as_ref() {
                    Some(Ok(captured)) => pending.control_invocation().ok_or_else(|| {
                        tilo_actions::DoorError::InvalidArguments(
                            "The retained palette arguments are incomplete".into(),
                        )
                    }).and_then(|invocation| {
                        let original_arguments = continuation_arguments.as_ref().ok_or_else(|| {
                            tilo_actions::DoorError::InvalidArguments(
                                "The retained palette argument representation is invalid".into(),
                            )
                        })?;
                        crate::bound_dispatch::control_witness_with_arguments(
                            self.root_widget.as_ref(), &self.state.doc, &target, &invocation,
                            original_arguments,
                        ).and_then(|current| {
                            (current == *captured).then_some(()).ok_or_else(|| {
                                tilo_actions::DoorError::Unavailable {
                                    code: "stale_pending_palette_action".into(),
                                    reason: "The exact action encoding changed while History was open".into(),
                                }
                            })
                        })
                    }),
                    Some(Err(reason)) => Err(tilo_actions::DoorError::Unavailable {
                        code: "stale_pending_palette_action".into(),
                        reason: reason.clone(),
                    }),
                    None => pending.control_invocation().ok_or_else(|| {
                        tilo_actions::DoorError::InvalidArguments(
                            "The retained palette arguments are incomplete".into(),
                        )
                    }).and_then(|invocation| {
                        let original_arguments = continuation_arguments.as_ref().ok_or_else(|| {
                            tilo_actions::DoorError::InvalidArguments(
                                "The retained palette argument representation is invalid".into(),
                            )
                        })?;
                        crate::bound_dispatch::control_witness_with_arguments(
                            self.root_widget.as_ref(), &self.state.doc, &target, &invocation,
                            original_arguments,
                        ).map(|_| ())
                    }),
                };
                match (draft, captured, current, witness) {
                    (Ok(()), Ok(captured), Ok(current), Ok(())) if captured == current => match target {
                        crate::bound_dispatch::local_target::LocalResourceTarget::Component(target) => self.invoke_bound_target_with_broker(&target, &palette_request, None),
                        crate::bound_dispatch::local_target::LocalResourceTarget::Outer => self.invoke_unbound_resource_action(&palette_request),
                    },
                    (Ok(()), Ok(_), Ok(_), Ok(())) => Err(tilo_actions::DoorError::Unavailable {
                        code: "stale_pending_palette_action".into(),
                        reason: "The action meaning or confirmation policy changed while History was open".into(),
                    }),
                    (Err(error), _, _, _)
                    | (_, Err(error), _, _)
                    | (_, _, Err(error), _)
                    | (_, _, _, Err(error)) => Err(error),
                }
            } else if let Some(request) = pending.ordinary.as_ref() {
                match &request.target {
                    crate::bound_dispatch::local_target::LocalResourceTarget::Component(target) => {
                        self.invoke_bound_target_with_broker(target, &palette_request, None)
                    }
                    crate::bound_dispatch::local_target::LocalResourceTarget::Outer => {
                        self.invoke_unbound_resource_action(&palette_request)
                    }
                }
            } else if let Some(result) =
                self.invoke_history_address_local(&pending.address, argument.as_deref())
            {
                result
            } else if let Some(target) = pending.bound.as_ref() {
                self.invoke_bound_target_with_broker(target, &palette_request, None)
            } else {
                self.invoke_resource_action(palette_request)
            };
            if let Err(error) = result {
                tracing::warn!(%error, "resource operation failed");
                if pending.history_target.is_some() {
                    if let Some(palette) = self.actions.action_palette.as_deref_mut() {
                        palette.retain_history_refusal(pending, error.to_string());
                    }
                } else {
                    self.refuse_visibly(&error);
                }
            } else {
                self.layout.layout_dirty = true;
            }
        }
        // A content row may have fired: hand its id to the app to navigate,
        // recording an undo point only if it mutated the shared doc.
        let navigation = self
            .actions
            .action_palette
            .as_deref_mut()
            .and_then(super::action_palette::ActionPalette::take_scoped_navigation);
        if let Some((action_id, binding)) = navigation {
            if let Some(binding) = binding {
                let _ = crate::bound_dispatch::activate_bound_search(
                    self.root_widget.as_mut(),
                    &mut self.state.doc,
                    &binding,
                    &action_id,
                );
            } else {
                let nav_before = self.state.doc.get_heads();
                crate::search::capability::activate_search_result(
                    self.root_widget.as_mut(),
                    &action_id,
                    &mut crate::widget::Edits::lend(&mut self.state.doc),
                    &root,
                );
                if self.state.doc.get_heads() != nav_before {
                    self.state.record_committed_action(nav_before, None, None);
                }
            }
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        // If the palette closed (Escape, or a non-prompted selection), drop it.
        if self
            .actions
            .action_palette
            .as_deref()
            .is_some_and(|p| !p.is_open())
        {
            self.close_action_palette();
        }
        if consumed {
            self.invalidate_widget_resource_cache();
            self.layout.layout_dirty = true;
        }
        // Semantic History input can arrive before the next redraw. Publish
        // a current admitted map now, while still requiring the redraw solve.
        // This replaces the prior tree instead of appending another host tree.
        self.rebuild_action_palette_admission();
        consumed
    }
}

/// Room for a directory page's own fields (revision, cursor, flags and the
/// row list) beyond its rows, on the wire.
const DIRECTORY_PAGE_WIRE_BYTES: usize = ::collaboration::action_directory::PAGE_WIRE_BYTES;

/// Every `(resource, action)` in `resource`'s tree, to `max_depth` levels
/// below it, whose action id or label, description, parameter names and
/// descriptions or resource label contains `needle` (already lowercase). Each matched resource is kept without its
/// children.
fn collect_named_actions(
    resource: &tilo_actions::ResourceSpec,
    needle: &str,
    depth: u16,
    max_depth: u16,
    out: &mut Vec<(tilo_actions::ResourceSpec, String)>,
) {
    let contains = |text: &str| text.to_lowercase().contains(needle);
    let mut shallow = None;
    for action in &resource.actions {
        if contains(&action.id)
            || contains(&action.label)
            || action.description.as_deref().is_some_and(contains)
            || contains(&resource.label)
            || action
                .params()
                .iter()
                .any(|param| contains(&param.name) || contains(&param.description))
        {
            let shallow = shallow.get_or_insert_with(|| tilo_actions::ResourceSpec {
                children: Vec::new(),
                ..resource.clone()
            });
            out.push((shallow.clone(), action.id.clone()));
        }
    }
    if depth < max_depth {
        for child in &resource.children {
            collect_named_actions(child, needle, depth.saturating_add(1), max_depth, out);
        }
    }
}

/// A native window's session door ([`crate::session_door`]): the workspace
/// broker its collaboration connection reaches.
pub(super) struct BrokerDoor<'a> {
    manager: &'a mut crate::collaboration::CollaborationManager,
    broker_requests: &'a mut std::collections::HashMap<BrokerReply, u64>,
    next_broker_request_id: &'a mut Option<u64>,
    pending_application_launches:
        &'a mut std::collections::HashMap<u64, tilo_actions::InvocationId>,
}

impl BrokerDoor<'_> {
    /// Retain exactly one broker request per admitted launcher invocation.
    /// The fixed cap is a failure boundary: overflowing it must refuse the
    /// invocation before it can be left Accepted without a terminal receipt.
    pub(super) fn admit(
        &mut self,
        request_id: u64,
        operation: tilo_actions::InvocationId,
    ) -> Result<(), tilo_actions::DoorError> {
        if self.pending_application_launches.len() >= MAX_PENDING_APPLICATION_LAUNCHES {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "application_launch_queue_full".into(),
                reason: "Too many application launches are awaiting broker receipts".into(),
            });
        }
        self.pending_application_launches
            .insert(request_id, operation);
        Ok(())
    }
}

impl crate::session_door::SessionDoor for BrokerDoor<'_> {
    fn workspace(&self) -> Option<String> {
        self.manager
            .primary_route()
            .map(|witness| witness.route().cluster_id.to_string())
    }

    fn invoke(
        &mut self,
        operation: tilo_actions::InvocationId,
        admitted: bool,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        if !self.manager.is_connected() {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "authoritative_host_unavailable".into(),
                reason: "Opening an application requires the workspace action broker".into(),
            });
        }
        // A launch runs only as the broker delivered it here.
        if !admitted {
            return Err(tilo_actions::DoorError::Failed(
                "application launch was not admitted by the broker".into(),
            ));
        }
        let request_id = App::allocate_broker_request_id(self.next_broker_request_id)?;
        self.admit(request_id, operation)?;
        self.broker_requests
            .insert(BrokerReply::Receipt(request.invocation), request_id);
        self.manager.send(CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
        });
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }
}

impl crate::host_core::ControlHost for App {
    fn local_author(&self) -> &crate::permissions::UserId {
        &self.collaboration.local_author
    }

    fn bound_publication(&self) -> &crate::bound_dispatch::BoundPublication {
        &self.actions.bound_publication
    }

    fn broker_connected(&self) -> bool {
        self.collaboration.manager.is_connected()
    }

    fn send_follow_up(
        &mut self,
        running: tilo_actions::InvocationId,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        context: Option<tilo_actions::ActionContext>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        App::send_follow_up(self, running, address, arguments, context)
    }

    fn local_confirmation(
        &self,
        intent: &tilo_actions::ControlInvocation,
    ) -> Option<tilo_actions::ConfirmationPolicy> {
        let (key, generation) = self.ensure_local_resource_cache();
        let cache = self.actions.local_resource_cache.borrow();
        let resources = cache
            .as_ref()
            .filter(|cache| cache.key == key && cache.permission_generation == generation)
            .and_then(|cache| cache.resources.as_deref())
            .unwrap_or_default();
        tilo_actions::door::resolve(resources, &intent.address)
            .map(|resolved| resolved.confirmation_for_json(&intent.arguments))
    }

    fn invoke_bound_control(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_bound_target_with_broker(target, request, None)
    }

    fn invoke_outer_control(
        &mut self,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_unbound_resource_action(request)
    }

    fn is_host_action_resource(&self, resource: &str) -> bool {
        resource == local_input_capture::UNSAVED_RESOURCE
    }

    fn run_local_history_address(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: &str,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_history_address_local(address, Some(argument))
            .unwrap_or_else(|| Err(tilo_actions::DoorError::Unknown(address.clone())))
    }

    fn embedded_history_acted(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }

    fn take_search_invocation(
        &mut self,
        intent: &tilo_actions::ControlInvocation,
        owner: Option<&crate::bound_dispatch::CallbackBinding>,
    ) -> bool {
        let Some(request) = crate::search::SearchRequest::from_invocation(intent) else {
            return false;
        };
        match owner {
            Some(binding) if !self.presents_search() => binding.request_search(request),
            _ => self.open_search(request),
        }
        true
    }

    fn control_domain(&self) -> &crate::control_custody::ControlDomain {
        &self.control_domain
    }

    fn ask_in_palette(&mut self, request: crate::bound_dispatch::OrdinaryConfirmation) -> bool {
        if self.actions.action_palette.is_none() {
            // Native semantic focus is authoritative even before the next
            // retained render propagates the widget's focused flag.
            self.actions.ordinary_palette_return_focus =
                self.capture_focus_identity().or_else(|| {
                    crate::widget::layout::CompositionFocus::capture_live(self.root_widget.as_ref())
                });
        }
        // Read before the tree it is found in is discarded.
        let anchor = self.tree().acting_control_rect();
        self.open_action_palette();
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        if let Some(palette) = self.actions.action_palette.as_deref_mut() {
            // The question stands at the control the person used.
            palette.anchor_confirmation(anchor);
            palette.activate_ordinary(request);
        }
        if self
            .actions
            .action_palette
            .as_deref()
            .is_some_and(crate::action_palette::ActionPalette::went_ahead)
        {
            return self.finish_action_palette_input(true);
        }
        self.rebuild_action_palette_admission();
        true
    }

    /// Say that an invocation the person made did not run, and why, at the
    /// control they used: a click is never dropped with only a log line, and
    /// never opens search.
    fn refuse_visibly(&mut self, error: &tilo_actions::DoorError) {
        let anchor = self.tree().acting_control_rect();
        self.refuse_at(error, anchor);
    }
}
