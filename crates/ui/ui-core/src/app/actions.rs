// Action/resource discovery, publication, dispatch, and palette integration.
//
// Kept as an `App` implementation so custom widgets and built-in controls
// share one authoritative action/resource route.

#[cfg(feature = "model")]
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
            locale: crate::i18n::I18nManager::revision(),
        }
    }

    /// Invalidate declarations derived from document or widget-local state.
    pub(super) fn invalidate_widget_resource_cache(&mut self) {
        // What the widgets declare changed, and so may their semantics.
        self.mark_semantic_tree_stale();
        // At `u64::MAX` there is no fresh key to issue. Eviction still makes
        // each later materialisation a new snapshot, so it cannot alias a
        // retained tree that happened to have the same terminal generation.
        if let Some(next) = self.actions.resource_generation.get().checked_add(1) {
            self.actions.resource_generation.set(next);
        }
        #[cfg(feature = "model")]
        self.retire_action_directory();
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
            shared_permissions: shared_permissions_revision(),
        }
    }

    #[cfg(feature = "model")]
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
        #[cfg(feature = "model")]
        let controls = {
            let mut controls = std::collections::HashMap::new();
            for (node, resource) in semantics.action_resource_nodes() {
                if let Some(node) = node {
                    controls.insert(resource.id.as_str().to_owned(), node);
                }
                complete.push(resource);
            }
            controls
        };
        #[cfg(not(feature = "model"))]
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
            #[cfg(feature = "model")]
            {
                cache.controls = controls;
            }
        }
        // Completing this generation must retain its already-filtered allowed set.
        key
    }

    #[cfg(feature = "model")]
    /// `resource` as the surface publishes it: a control a component shows (`controls`, by the
    /// layout node it stands in) keeps the verbs that component's person may run there now, so a
    /// grant revoked takes the component's controls off the surface with its resources. `None`
    /// when it keeps none.
    fn permitted_control(
        &self,
        controls: &std::collections::HashMap<String, u64>,
        mut resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        let Some(binding) = controls.get(resource.id.as_str()).and_then(|node| {
            self.bindings
                .binding_ref(taffy::NodeId::from(*node))
                .map(|binding| crate::bound_dispatch::callback_binding_of(&**binding))
        }) else {
            return Some(resource);
        };
        let permissions = binding.permissions.borrow();
        let decider = permissions.action_decider(&binding.app_id);
        let (kind, id) = (resource.kind.clone(), resource.id.clone());
        resource
            .actions
            .retain(|action| decider.permits(&kind, id.as_str(), action));
        (!resource.actions.is_empty()).then_some(resource)
    }

    fn collect_declared_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        // Tilo's model is the action layer: without it a window publishes no resources.
        #[cfg(feature = "model")]
        let resources = self.declared_resources_cached();
        #[cfg(not(feature = "model"))]
        let resources = Vec::new();
        resources
    }

    #[cfg(feature = "model")]
    fn declared_resources_cached(&self) -> Vec<tilo_actions::ResourceSpec> {
        let key = self.ensure_resource_cache();
        self.actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map_or_else(Vec::new, |cache| cache.declared.clone())
    }

    #[cfg(feature = "model")]
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
        // Tilo's model is the action layer: without it a window publishes no resources.
        #[cfg(feature = "model")]
        let resources = self.local_resources_cached();
        #[cfg(not(feature = "model"))]
        let resources = Vec::new();
        resources
    }

    #[cfg(feature = "model")]
    fn local_resources_cached(&self) -> Vec<tilo_actions::ResourceSpec> {
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
        #[cfg(feature = "model")]
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

    /// The resource this window shows: the first it publishes. What
    /// a question typed into a context menu is about when nothing more
    /// particular is meant.
    #[cfg(feature = "native")]
    pub(super) fn window_resource(&self) -> Option<tilo_actions::ActionResource> {
        // A window publishes no resources without Tilo's model, so it shows none.
        #[cfg(feature = "model")]
        let resource = self.first_published_resource();
        #[cfg(not(feature = "model"))]
        let resource = None;
        resource
    }

    #[cfg(all(feature = "native", feature = "model"))]
    fn first_published_resource(&self) -> Option<tilo_actions::ActionResource> {
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

    /// The complete machine-driveable surface: domain resources declared by
    /// the app plus ordinary controls projected from its retained semantic
    /// tree. Ctrl+K, scripts, and agents therefore consume one catalogue.
    pub fn collect_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        // Tilo's model is the action layer: without it a window publishes no resources.
        #[cfg(feature = "model")]
        let resources = self.published_resources_cached();
        #[cfg(not(feature = "model"))]
        let resources = Vec::new();
        resources
    }

    #[cfg(feature = "model")]
    fn published_resources_cached(&self) -> Vec<tilo_actions::ResourceSpec> {
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
        #[cfg(feature = "model")]
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
        #[cfg(feature = "model")]
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
        // Shared chrome invokes APIs served by this host, rather than by
        // the app's resource provider. Audit those exact declarations too.
        let mut host_resources = vec![crate::widget::search_request::search_resource()];
        #[cfg(feature = "model")]
        if let Some(history) = &self.history {
            host_resources.push(history.api_resource());
        }
        let mut seen = std::collections::HashSet::new();
        bindings
            .into_iter()
            .map(|binding| {
                let status = match &binding.kind {
                    crate::widget::ControlBindingKind::Action(address) => {
                        if tilo_actions::api::resolve(resources, address).is_none()
                            && tilo_actions::api::resolve(&host_resources, address).is_none()
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

    #[cfg(feature = "model")]
    fn bound_broker_route(&self) -> Option<&crate::collaboration::AcceptedRoute> {
        #[cfg(test)]
        if let Some(route) = &self.actions.bound_broker_route_for_test {
            return Some(route);
        }
        self.collaboration.manager.primary_route()
    }
    #[cfg(feature = "model")]
    fn publication_app_id(&self) -> String {
        if crate::bound_dispatch::has_matching_broker_component(
            self.root_widget.as_ref(),
            self.bound_broker_route(),
        ) && let Some(observation) = self.bound_broker_route()
        {
            return observation.route().app_id.clone();
        }
        crate::collaboration::local_app_id()
    }
    #[cfg(feature = "model")]
    fn require_bound_broker_route(
        &mut self,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Result<(), tilo_actions::ApiError> {
        // A surface's components are published to the shell that placed
        // it, and what that shell asks comes back over the same socket:
        // there is no document route for the binding to name.
        if self.collaboration.manager.hosts_surface() {
            return Ok(());
        }
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
        Err(tilo_actions::ApiError::Unavailable {
            code: "bound_broker_route_unavailable".into(),
            reason:
                "This component has no matching admitted app, workspace and record action route"
                    .into(),
        })
    }

    #[cfg(feature = "model")]
    /// Transport-only projection. Each component is validated with its own
    /// registry before its domain resource is assigned an opaque publication alias.
    pub(super) fn publication_resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
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
        let (complete, controls) = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map(|cache| (cache.complete.clone(), cache.controls.clone()))
            .unwrap_or_default();
        let mut resources = complete
            .unwrap_or_default()
            .into_iter()
            .filter_map(remove_reserved)
            .filter_map(|resource| self.permitted_control(&controls, resource))
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

    #[cfg(feature = "model")]
    /// Publish the live resource/action tree through the session broker.
    /// Level-triggered at the wire, diffed here so an unchanged frame emits
    /// nothing; publishing an empty list explicitly withdraws a closed tree.
    fn publish_action_surface(&mut self) {
        let _phase = tilo_actions::profiling::Phase::start("ui.publish_action_surface");
        self.actions.action_surface_owed = false;
        if self.collaboration.manager.is_connected() {
            let _routes = tilo_actions::profiling::Phase::start("ui.publish.routes");
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
        let unchanged = {
            let _observation = tilo_actions::profiling::Phase::start("ui.publish.observation");
            let stamp = self.publication_stamp();
            stamp.as_ref().is_some_and(|stamp| {
                Some(stamp) == self.actions.published_stamp.as_ref()
                    // The directory's own observation (the bound owners' generations
                    // and permissions), taken now: a grant revoked in place is a
                    // change though no generation moved.
                    && self
                        .try_action_directory_revision()
                        .is_ok_and(|now| now == stamp.revision())
            })
        };
        if unchanged {
            self.settle_held_own_invocations(true);
            return;
        }
        self.publish_action_surface_after_refresh();
    }

    #[cfg(feature = "model")]
    /// Publish the surface a handled input left for the frame after its
    /// sync ([`super::runtime::ActionRuntime::action_surface_owed`]), if no
    /// publication has paid for it since.
    pub(super) fn publish_owed_action_surface(&mut self) {
        if self.actions.action_surface_owed {
            self.publish_action_surface();
        }
    }

    #[cfg(feature = "model")]
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

    /// How long an invocation of this app's own action waits for the route
    /// that offers it while the connection stands, before the person is told
    /// it was not carried out. The hold stands in every build of the toolkit,
    /// with its feature native and without, so the type is named by its full
    /// path: this file's parent brings the short name in for native and test
    /// builds alone.
    const HELD_OWN_INVOCATION_WAIT: crate::time::Duration = crate::time::Duration::from_secs(20);

    #[cfg(feature = "model")]
    /// Send an invocation of this app's own action to the session action
    /// host, on the route, after the surface that offers it. It waits here,
    /// behind any invocation made before it, while the route cannot carry
    /// that surface yet.
    fn send_own_invocation(&mut self, request: tilo_actions::remote::InvokeRequest) {
        self.actions
            .held_own_invocations
            .push_back(HeldOwnInvocation {
                request,
                since: crate::time::now(),
                press: crate::press::current(),
            });
        self.publish_action_surface();
        // Still waiting, and the first to wait: a route that never opens brings
        // no event of its own, so the pass that ends the wait is asked for now.
        if self.actions.held_own_invocations.len() == 1
            && let Some(scope) = &self.wake_scope
        {
            crate::background::Background::current()
                .wake_after(Self::HELD_OWN_INVOCATION_WAIT, scope.handle());
        }
    }

    #[cfg(feature = "model")]
    /// Release the held own invocations, in order, once the route carries
    /// the current surface; refuse them with the connection's own reason
    /// once it has gone. Otherwise they keep waiting: the publication the
    /// route's readiness brings settles them.
    ///
    /// The wait has an end while the connection stands too: once the route's
    /// join has failed, or the oldest of them has waited
    /// [`Self::HELD_OWN_INVOCATION_WAIT`], they are refused with why, and the
    /// person is told ([`Self::settle_own_refusal`]).
    fn settle_held_own_invocations(&mut self, carried: bool) {
        use tilo_actions::remote::{ActionCallError, RemoteActionMessage};
        if self.actions.held_own_invocations.is_empty() {
            return;
        }
        let connected = self.collaboration.manager.is_connected();
        let ended = if connected && !carried {
            let Some(why) = self.held_own_invocations_ended() else {
                return;
            };
            Some(why)
        } else {
            None
        };
        while let Some(held) = self.actions.held_own_invocations.pop_front() {
            let request = held.request;
            let refusal = RemoteActionMessage::Invoke(request.clone());
            if ended.is_none()
                && connected
                && self
                    .collaboration
                    .manager
                    .send(CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Invoke(request),
                        press: held.press,
                    })
            {
                continue;
            }
            let reason = ended
                .clone()
                .unwrap_or_else(|| self.collaboration.manager.state().reason().to_owned());
            if let Some(answer) =
                tilo_actions::remote::refuse(&refusal, ActionCallError::Unavailable { reason })
            {
                self.apply_remote_action_answer(answer);
            }
        }
    }

    #[cfg(feature = "model")]
    /// Why the held own invocations wait no longer though the connection
    /// stands, once they do not: the join of the route that offers them
    /// failed, or the oldest of them waited
    /// [`Self::HELD_OWN_INVOCATION_WAIT`]. Logged as it is found.
    fn held_own_invocations_ended(&self) -> Option<String> {
        let failed = self.bound_broker_route().and_then(|accepted| {
            self.collaboration
                .manager
                .route_join_failure(accepted.route())
        });
        let waited = self
            .actions
            .held_own_invocations
            .front()
            .map(|held| crate::time::now().saturating_duration_since(held.since));
        let why = match (failed, waited) {
            (Some(failure), _) => failure,
            (None, Some(waited)) if waited >= Self::HELD_OWN_INVOCATION_WAIT => {
                crate::i18n::I18nManager::lookup_plain("action-own-route-not-open")
            }
            _ => return None,
        };
        tracing::warn!(
            held = self.actions.held_own_invocations.len(),
            waited_ms = waited.map_or(0, |waited| u64::try_from(waited.as_millis())
                .unwrap_or(u64::MAX)),
            %why,
            "invocations of this window's own actions waited for a route that does not open"
        );
        Some(why)
    }

    #[cfg(feature = "model")]
    fn invoke_published_action(
        &mut self,
        id: u64,
        invocation: tilo_actions::InvocationId,
        resource: String,
        action: String,
        argument: Option<&str>,
        actor: &tilo_actions::Actor,
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
        // A surface's actions belong to the shell that placed it, not to a
        // document: there is no history to ownership them, and no route to
        // observation.
        if !self.collaboration.manager.hosts_surface() && !self.published_action_history_ready() {
            // An already accepted delivery lost its original owner-history
            // ownership. Run no handler; disconnect retains admitted uncertainty.
            self.action_publication_failure = Some(
                "Original document history is unavailable; action outcome unknown. Reconnect and reconcile before retrying.".into(),
            );
            self.collaboration.manager.stop_after_publication_failure();
            return;
        }
        let address = tilo_actions::ActionAddress::new(resource, action);
        // The workspace admitted it, the person's yes included where the call waited for it.
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
        // The invoker is the delivered actor plus the presence and the
        // person it acts for that the host set for it in the delivery context
        // (never trusted from anywhere else). Resolution below rebuilds that presence's own tree, and
        // durable attribution carries the same invoker.
        let invoker = tilo_actions::ActionInvoker::delivered(actor, ambient_context);
        let presence = invoker.presence.clone();
        let attributed =
            ambient_context.map(|context| context.clone().with_invoker(invoker.clone()));
        let ambient_context = attributed.as_ref();
        if let Some(presence) = &presence {
            self.join_presence(presence);
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
        let invoker_scope = crate::invocation_scope::InvokerScope::delivered(
            invoker,
            invocation,
            ambient_context.and_then(|context| context.approval.clone()),
        );
        let result = self.invoke_resource_action(
            ResourceInvocation::new(&address, actor)
                .argument(argument)
                .broker_admitted(true)
                .invocation(Some(invocation))
                .ambient_context(ambient_context),
        );
        drop(invoker_scope);
        let continuation = if matches!(result, Ok(crate::widget::ResourceOperationStart::Accepted))
        {
            None
        } else {
            // What the effect submitted is applied for its invoker before
            // its caller learns it finished.
            self.dispatch_control_invocations();
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
                    code: crate::bound_dispatch::api_error_code(&error).to_string(),
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

    #[cfg(feature = "model")]
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
        if tilo_actions::api::resolve(&resources, address).is_none() {
            resources.extend(crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                &address.resource,
            ));
        }
        if tilo_actions::api::resolve(&resources, address).is_none()
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
                    tilo_actions::api::resolve(snapshot.resources(), &target.address.address)
                })
                .map_or(tilo_actions::ActionKind::Read, |spec| {
                    spec.need_for(None, argument).kind
                });
        }
        tilo_actions::api::resolve(&resources, address)
            .map_or(tilo_actions::ActionKind::Read, |spec| {
                spec.need_for(None, argument).kind
            })
    }

    #[cfg(feature = "model")]
    fn resolve_resource_invocation<'a>(
        resources: &'a [tilo_actions::ResourceSpec],
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<ResolvedResourceInvocation<'a>, tilo_actions::ApiError> {
        crate::bound_dispatch::resolve_resource_invocation(resources, address, argument, None)
    }

    #[cfg(feature = "model")]
    /// The canonical route the app's primary document currently syncs on.
    fn primary_document_route(&self) -> Option<crate::collaboration::RouteSpec> {
        self.collaboration
            .manager
            .primary_route()
            .map(|observation| observation.route().clone())
    }

    #[cfg(feature = "model")]
    fn encode_resource_action(
        resources: &[tilo_actions::ResourceSpec],
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Result<tilo_actions::EncodedAction, tilo_actions::ApiError> {
        crate::bound_dispatch::encode_resource_action(resources, address, typed_arguments, argument)
    }

    #[cfg(feature = "model")]
    /// Route keyboard history through the bound resource's declared action
    /// so compensation receives fresh admission under current permissions.
    pub(super) fn invoke_bound_history(
        &mut self,
        binding: &crate::bound_dispatch::CallbackBinding,
        request: crate::undo::UndoRequest,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.require_bound_broker_route(binding)?;
        let snapshots = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .map_err(tilo_actions::ApiError::Failed)?;
        let (resource, action) =
            crate::bound_dispatch::history_action_resource(&snapshots, binding, request)?;
        let alias = self
            .actions
            .bound_publication
            .alias(binding, resource.id.as_str())
            .map_err(crate::bound_dispatch::BoundActionError::into_api)?;
        let address = tilo_actions::ActionAddress::new(alias, action);
        self.invoke_via_broker(&address, Some(&serde_json::Map::new()), None)
    }

    #[cfg(feature = "model")]
    /// Admit a locally initiated operation at the session broker before the
    /// application performs it. The broker records the invocation first,
    /// re-resolves the live published surface and current permissions, then
    /// directs the exact operation back to this connection. That ordering is
    /// load-bearing for Effects: if the app crashes while doing external work,
    /// history retains an admitted/incomplete operation instead of losing the
    /// action entirely.
    pub(super) fn invoke_via_broker(
        &mut self,
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let mut made = None;
        let started = Self::dispatch_broker_request(
            &mut self.actions.next_broker_request_id,
            self.collaboration.manager.is_connected(),
            address,
            typed_arguments,
            argument,
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

    #[cfg(feature = "model")]
    fn dispatch_broker_request(
        next_request_id: &mut Option<u64>,
        connected: bool,
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        send: impl FnOnce(u64, tilo_actions::remote::InvokeRequest),
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        if !connected {
            return Err(tilo_actions::ApiError::Unavailable {
                code: "authoritative_host_unavailable".into(),
                reason: "This operation requires the workspace action broker, but collaboration is offline".into(),
            });
        }
        let request_id = Self::allocate_broker_request_id(next_request_id)?;
        // The argument travels as the caller gave it: typed arguments as
        // such, a text argument as text, which the owning app binds to the
        // action's input as its own API does (a single parameter, a prompt).
        let argument = match (typed_arguments, argument) {
            (Some(arguments), _) => tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
            (None, Some(text)) => match text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX) {
                Some(json) => {
                    tilo_actions::remote::InvokeArgument::Json(serde_json::from_str(json).map_err(
                        |error| tilo_actions::ApiError::InvalidArguments(error.to_string()),
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
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        );
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }

    #[cfg(feature = "model")]
    fn take_broker_request_id(&mut self) -> Result<u64, tilo_actions::ApiError> {
        Self::allocate_broker_request_id(&mut self.actions.next_broker_request_id)
    }
    #[cfg(feature = "model")]
    fn allocate_broker_request_id(
        next_request_id: &mut Option<u64>,
    ) -> Result<u64, tilo_actions::ApiError> {
        let request_id = next_request_id.take().ok_or_else(|| {
            tilo_actions::ApiError::Unavailable {
                code: "broker_request_identity_exhausted".into(),
                reason: "This app has exhausted its broker request identities; restart it before invoking another hosted operation".into(),
            }
        })?;
        *next_request_id = request_id.checked_add(1);
        Ok(request_id)
    }

    #[cfg(feature = "model")]
    /// This window's session API: its workspace broker connection.
    pub(super) fn broker_api(&mut self) -> BrokerApi<'_> {
        BrokerApi {
            manager: &mut self.collaboration.manager,
            broker_requests: &mut self.actions.broker_requests,
            next_broker_request_id: &mut self.actions.next_broker_request_id,
            pending_application_launches: &mut self.actions.pending_application_launches,
        }
    }

    #[cfg(feature = "model")]
    fn invoke_document_resource_action(
        &mut self,
        invocation: &ResourceInvocation<'_>,
        resources: &[tilo_actions::ResourceSpec],
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        resource: &tilo_actions::ResourceSpec,
        spec: &tilo_actions::ActionSpec,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let ResourceInvocation {
            address,
            actor,
            argument,
            broker_admitted,
            invocation,
            ambient_context,
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
            return self.invoke_via_broker(address, broker_arguments, broker_argument);
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
            .map_err(tilo_actions::ApiError::decoding)?;
        if spec.deferred_invocation_type().is_some() && action.locus() != spec.locus {
            return Err(tilo_actions::ApiError::Failed(
                "Deferred action locus differs from its declaration".into(),
            ));
        }
        if action.locus() == tilo_actions::ExecutionLocus::ServerPreferred && !broker_admitted {
            return self.invoke_via_broker(address, broker_arguments, broker_argument);
        }
        // Only admitted local document execution owns a new change boundary.
        // Encoding failures and broker routing must leave the caller's pending
        // edit intact; the executor still receives the same committed baseline.
        let before = self.state.doc.get_heads();
        let route = self.primary_document_route();
        let mut api = BrokerApi {
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
                        local_author: &self.local_author,
                        broker_admitted,
                        invocation: operation_invocation,
                        source_invocation,
                        ambient: ambient_context,
                        api: Some(&mut api),
                    },
                )
            },
        )?;
        if before != self.state.doc.get_heads() {
            self.state.record_committed_action_authored(
                before,
                None,
                None,
                actor.member_id().cloned(),
            );
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.invalidate_widget_resource_cache();
        }
        Ok(started)
    }

    #[cfg(feature = "model")]
    fn invoke_bound_target(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        invocation: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_bound_target_with_broker(target, invocation, None)
    }

    #[cfg(feature = "model")]
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

    #[cfg(feature = "model")]
    /// A record another workspace holds (a person's profile record in a window of another
    /// workspace) is admitted where it is held: the broker decides it there, never this window,
    /// which only shows what that workspace said of it.
    fn is_decided_by_the_records_workspace(
        resource: &tilo_actions::ResourceSpec,
        invocation: Option<tilo_actions::InvocationId>,
        has_delivery: bool,
        connected: bool,
    ) -> bool {
        resource.workspace.is_some() && invocation.is_none() && (has_delivery || connected)
    }

    #[cfg(feature = "model")]
    /// Ask the workspace's broker for the bound call `target` names with these `arguments`,
    /// under the component's alias. The component's published surface is checked as it is
    /// published, where the live host withdraws one whose actions break their contract; the
    /// one resource the API resolved is not checked again, since an item's views may share the
    /// record's opener and a check of that one resource refuses what the component publishes as
    /// one surface.
    fn route_bound_invocation_to_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        broker_delivery: Option<&mut dyn FnMut(CollabRequest)>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.require_bound_broker_route(&target.binding)?;
        let alias = self
            .actions
            .bound_publication
            .alias(&target.binding, &target.address.address.resource)
            .map_err(crate::bound_dispatch::BoundActionError::into_api)?;
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
                |request_id, request| {
                    broker_requests.insert(BrokerReply::Receipt(request.invocation), request_id);
                    send(CollabRequest::RemoteAction {
                        message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                        press: crate::press::current(),
                    });
                },
            );
        }
        self.invoke_via_broker(&address, arguments, argument)
    }

    #[cfg(feature = "model")]
    /// Invoke the action `address` names, at the component instance `address.instance_id`
    /// names, with `arguments`: the app's own call, run where that component's owner says,
    /// never another instance that publishes the same address.
    ///
    /// # Errors
    /// Refuses an address no mounted component publishes at that instance, a denied or
    /// unavailable action, and a failure the action reports.
    pub(super) fn invoke_bound_action_at(
        &mut self,
        address: &crate::bound_dispatch::BoundActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let translated = self
            .actions
            .bound_publication
            .target(&address.address)
            .map_err(crate::bound_dispatch::BoundActionError::into_api)?;
        if translated
            .as_ref()
            .is_some_and(|target| target.address.instance_id != address.instance_id)
        {
            return Err(tilo_actions::ApiError::Unknown(address.address.clone()));
        }
        let address = translated
            .as_ref()
            .map_or(address, |target| &target.address);
        let mut snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            Some(&address.address),
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_api())?;
        if let Some(target) = translated.as_ref() {
            snapshots.retain(|snapshot| snapshot.binding.same_component(&target.binding));
        }
        let target = crate::bound_dispatch::select_bound_target(
            &snapshots,
            &address.address,
            Some(address.instance_id),
        )
        .map_err(crate::bound_dispatch::BoundActionError::into_api)?
        .ok_or_else(|| {
            crate::bound_dispatch::unoffered(
                self.root_widget.as_ref(),
                &self.state.doc,
                &address.address,
                address.instance_id,
            )
        })?;
        let invocation = tilo_actions::InvocationId::new();
        let actor = tilo_actions::Actor::Member(self.local_author.clone());
        // Run where the component's owner says, as a host's own call: the app's local path,
        // never rerouted through the broker on the action's account.
        let started = crate::bound_dispatch::invoke_resource_action(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            &target,
            self.actions.action_transport.as_mut(),
            None,
            crate::bound_dispatch::ResourceActionRequest {
                argument: None,
                arguments: Some(arguments),
                actor: &actor,
                approval: None,
                broker_admitted: false,
                invocation,
                source_invocation: None,
                ambient: None,
                route: None,
            },
        );
        let started = match started {
            Err(crate::bound_dispatch::BoundActionError::Hosted(hosted)) => {
                self.invoke_hosted(hosted)
            }
            Err(error) => Err(error.into_api()),
            Ok(started) => Ok(started),
        }?;
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            self.actions.bound_invocations.insert(invocation, target);
        }
        Ok(started)
    }

    #[cfg(feature = "model")]
    /// The bound `target` names the action; `request.address` is not consulted.
    fn invoke_bound_target_with_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        request: &ResourceInvocation<'_>,
        broker_delivery: Option<&mut dyn FnMut(CollabRequest)>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let ResourceInvocation {
            actor,
            argument,
            approval,
            broker_admitted,
            invocation,
            ambient_context: ambient,
            ..
        } = *request;
        let _entry = target
            .binding
            .enter()
            .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_api())?;
        if broker_admitted {
            self.require_bound_broker_route(&target.binding)?;
        }
        // The API resolves the action once and asks here, before anything
        // runs, whether it goes through the broker instead. Resolving it a
        // second time for that question built the whole published tree twice
        // per keystroke into a bound field.
        let has_delivery = broker_delivery.is_some();
        let connected = self.collaboration.manager.is_connected();
        let resolved = std::cell::Cell::new(None);
        let route = |resource: &tilo_actions::ResourceSpec, spec: &tilo_actions::ActionSpec| {
            resolved.set(Some(spec.kind));
            !broker_admitted
                && (Self::bound_invocation_needs_broker(
                    spec.kind,
                    spec,
                    invocation,
                    has_delivery,
                    connected,
                ) || Self::is_decided_by_the_records_workspace(
                    resource,
                    invocation,
                    has_delivery,
                    connected,
                ))
        };
        let id = invocation.unwrap_or_default();
        let mut api = BrokerApi {
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
            Some(&mut api),
            crate::bound_dispatch::ResourceActionRequest {
                argument,
                arguments: None,
                actor,
                approval,
                broker_admitted,
                invocation: id,
                source_invocation: broker_admitted.then_some(invocation).flatten(),
                ambient,
                route: Some(&route),
            },
        );
        let started = match started {
            Err(crate::bound_dispatch::BoundActionError::Rerouted { arguments }) => {
                return self.route_resolved_invocation_to_broker(
                    target,
                    request,
                    broker_delivery,
                    arguments,
                );
            }
            // The broker delivers only what this app publishes, never
            // another provider's action.
            Err(crate::bound_dispatch::BoundActionError::Hosted(hosted)) if broker_admitted => {
                return Err(tilo_actions::ApiError::Unknown(hosted.address));
            }
            Err(crate::bound_dispatch::BoundActionError::Hosted(hosted)) => {
                return self.invoke_hosted(hosted);
            }
            started => started,
        };
        let started = started.map_err(crate::bound_dispatch::BoundActionError::into_api);
        // Refused before the API resolved the action: nothing ran, as when
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

    #[cfg(feature = "model")]
    /// Invoke a registered action as this application's current local person.
    /// This uses the same current record, policy and broker checks as the palette.
    /// # Errors
    /// Refuses unavailable, denied, stale or ambiguous actions and invalid arguments.
    pub fn invoke_local_resource_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let actor = tilo_actions::Actor::Member(self.local_author.clone());
        self.invoke_local_resource_action_as(&actor, address, arguments)
    }

    #[cfg(feature = "model")]
    /// [`Self::invoke_local_resource_action`] as `actor` (an agent or a script): what the API
    /// admits for whoever asked, so what acts on this device's person alone is refused to
    /// anyone else.
    pub fn invoke_local_resource_action_as(
        &mut self,
        actor: &tilo_actions::Actor,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let capture = self.begin_local_input_capture();
        let result = self.invoke_local_resource_action_captured(actor, address, arguments);
        drop(capture);
        self.drain_local_input_capture();
        result
    }

    #[cfg(feature = "model")]
    fn invoke_local_resource_action_captured(
        &mut self,
        actor: &tilo_actions::Actor,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let _entry = self.enter_wake_scope();
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(arguments.clone())
        );
        // The window's own Retry for unsaved changes is the host's, not the
        // app's: the same API its header button, the palette and assistive
        // technology reach.
        if address.resource == local_input_capture::UNSAVED_RESOURCE {
            return self
                .invoke_history_address_local(address, Some(&argument))
                .unwrap_or_else(|| Err(tilo_actions::ApiError::Unknown(address.clone())));
        }
        self.invoke_resource_action(
            ResourceInvocation::new(address, actor).argument(Some(&argument)),
        )
    }

    #[cfg(feature = "model")]
    /// Route through the broker an invocation the API resolved as one that
    /// runs there ([`crate::bound_dispatch::BoundActionError::Rerouted`]):
    /// the broker is asked under the component's alias with the arguments the
    /// API bound, exactly as before it decided the route. Nothing ran locally,
    /// and nothing is resolved twice: the component's published tree need not
    /// list what its own resolution found (an item its view builds only when
    /// asked for).
    fn route_resolved_invocation_to_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        request: &ResourceInvocation<'_>,
        broker_delivery: Option<&mut dyn FnMut(CollabRequest)>,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.route_bound_invocation_to_broker(
            target,
            arguments.as_ref(),
            request.argument,
            broker_delivery,
        )
    }

    #[cfg(feature = "model")]
    pub(super) fn invoke_resource_action(
        &mut self,
        request: ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let _ = crate::widget::take_presses();
        let started = (|| {
            use crate::bound_dispatch::local_target::{LocalResourceTarget, select_local_target};
            let address = request.address;
            if let Some(target) = self
                .actions
                .bound_publication
                .target(address)
                .map_err(crate::bound_dispatch::BoundActionError::into_api)?
            {
                self.require_bound_broker_route(&target.binding)?;
                return self.invoke_bound_target_with_broker(&target, &request, None);
            }
            match select_local_target(self.root_widget.as_ref(), &self.state.doc, address) {
                Ok(LocalResourceTarget::Component(target)) => {
                    self.invoke_bound_target_with_broker(&target, &request, None)
                }
                Ok(LocalResourceTarget::Outer) | Err(tilo_actions::ApiError::Unknown(_)) => {
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
        Ok(started)
    }

    #[cfg(feature = "model")]
    fn invoke_unbound_resource_action(
        &mut self,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let ResourceInvocation {
            address,
            actor,
            argument,
            approval,
            broker_admitted,
            invocation,
            ambient_context,
        } = *request;
        // Ordinary local requests use the permission-projected, cached
        // resource surface (including retained bound rows).
        let mut resources = if let Some(presence) = crate::invocation_scope::current_presence() {
            // A presence's tree is rebuilt for it now, never read from the
            // cache of this window's own view.
            let mut resources = Vec::new();
            crate::bound_dispatch::collect_presence_resources(
                self.root_widget.as_ref(),
                &self.state.doc,
                &presence,
                &mut resources,
            );
            resources
        } else if actor.member_id() == Some(&self.local_author) {
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
            if actor.member_id() == Some(&self.local_author) {
                resources
                    .into_iter()
                    .filter_map(|resource| self.filter_resource_for_local_actor(resource))
                    .collect()
            } else {
                resources
            }
        };
        if tilo_actions::api::resolve(&resources, address).is_none() {
            let fallback = crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                &address.resource,
            );
            let fallback = if actor.member_id() == Some(&self.local_author) {
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
        let (typed_arguments, resource, resolved) =
            Self::resolve_resource_invocation(&resources, address, argument)?;
        // What the workspace delivers it admitted, the person's yes included where it waited for
        // one; the person's own call of an action that asks first goes there to be asked.
        if !broker_admitted {
            crate::bound_dispatch::press_asks(
                resolved,
                typed_arguments.as_ref(),
                argument,
                approval,
            )?;
        }
        crate::invocation_scope::check_continuation(
            resolved,
            resolved.need_for(typed_arguments.as_ref(), argument).kind,
        )?;
        // The local presence's tree offers an action under the least it
        // needs; this invocation needs what its value asks.
        if actor.member_id() == Some(&self.local_author) {
            match crate::bound_dispatch::admit_local_action(
                &self.permission_ctx,
                (&crate::collaboration::local_app_id(), resource, resolved),
                typed_arguments.as_ref(),
                argument,
            ) {
                Err(tilo_actions::ApiError::ConfirmationRequired)
                    if broker_admitted || approval.is_some() => {}
                admitted => admitted?,
            }
        }
        let kind = resolved.kind;
        if resolved.contract.runs_on_invokers_device()
            && !crate::bound_dispatch::runs_own_device(
                actor,
                &self.local_author,
                broker_admitted,
                ambient_context,
            )
        {
            return Err(crate::bound_dispatch::own_device_refusal());
        }
        crate::invocation_scope::check_shown_here(actor, &self.local_author, resource, resolved)?;
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
            return self.invoke_via_broker(address, typed_arguments.as_ref(), argument);
        }
        if kind == tilo_actions::OperationKind::Document {
            let request = ResourceInvocation::new(address, actor)
                .argument(argument)
                .approval(approval)
                .broker_admitted(broker_admitted)
                .invocation(invocation)
                .ambient_context(ambient_context);
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
            && kind.reports_a_change(resolved.contract.need().kind)
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
            .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()))?
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
            // argument-binding, confirmation, and broker-admission API above.
            // This branch runs only when the broker directs that admitted action
            // back to its source app, preserving the normal receipt lifecycle.
            if encoded.type_id == tilo_actions::remote::ApplicationLaunch::TYPE {
                let Some(invocation) = invocation else {
                    return Err(tilo_actions::ApiError::Failed(
                        "application launch was not admitted by the broker".into(),
                    ));
                };
                return crate::session_api::start_application_launch(
                    Some(&mut self.broker_api()),
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
                    .map_err(tilo_actions::ApiError::Failed)?;
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
                                .send(CollabRequest::ResourceLocalEffect {
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
                        return Err(tilo_actions::ApiError::Failed(message));
                    }
                };
                if !handled {
                    if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                        self.collaboration
                            .manager
                            .send(CollabRequest::ResourceLocalEffect {
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
                    return Err(tilo_actions::ApiError::Unknown(address.clone()));
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
                        .send(CollabRequest::ResourceLocalEffect {
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
                let mut api = BrokerApi {
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
                                local_author: &self.local_author,
                                broker_admitted,
                                invocation: operation_invocation,
                                source_invocation: broker_admitted.then_some(operation_invocation),
                                ambient: ambient_context,
                                api: Some(&mut api),
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
                            .send(CollabRequest::ResourceLocalEffect {
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
                    .send(CollabRequest::ResourceLocalEffect {
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
                .map_err(|error| tilo_actions::ApiError::Failed(error.to_string()))?;
        }
        result
    }

    #[cfg(feature = "model")]
    fn service_local_control_history(&mut self) -> bool {
        #[cfg(test)]
        let probe = crate::profiling::frame_probe::span("idle.controls");
        // Input framework chrome took (a press on the window's own History) runs no control a
        // widget queued, here or in the model pass: the entry it asks for seals what was queued
        // before it instead, and the frame after it finds the queue empty. Neither does a window
        // still opening, whose queue waits for that entry.
        let controls = if self.input.opening || self.input.passive_input {
            0
        } else {
            self.dispatch_control_invocations()
        };
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

    #[cfg(feature = "model")]
    /// Review pre-entry controls retained by the native managed History surface.
    pub fn pending_history_controls(
        &self,
    ) -> Vec<crate::history_mode::pending_controls::PendingControlReview> {
        self.history
            .as_ref()
            .map_or_else(Vec::new, |history| history.pending_control_reviews())
    }

    #[cfg(feature = "model")]
    /// Drives the production local idle body without creating a window or input.
    #[cfg(any(test, feature = "test-support"))]
    pub fn controlled_local_idle_for_test(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        self.service_local_control_history()
    }

    #[cfg(feature = "model")]
    /// Resolve queued local control intents against the current resource tree
    /// and enter through the same API/lifecycle as every other caller.
    pub(super) fn dispatch_control_invocations(&mut self) -> usize {
        let _phase = tilo_actions::profiling::Phase::start("ui.dispatch_controls");
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
        // check. Keep control/causal intents with their existing producers until
        // the owner applies that capture and its worker rechecks the route.
        // Nothing has crossed the action API yet; accepted deliveries retain
        // the separate fail-closed guard in invoke_published_action.
        if self.collaboration.manager.has_native_committed_primary()
            && !self.published_action_history_ready()
        {
            return 0;
        }
        #[cfg(test)]
        crate::history_mode::embedded::discard_active_output(self.root_widget.as_mut());
        let registered = self.control_domain.drain_partitioned();
        self.dispatch_drained_controls(registered)
    }

    #[cfg(feature = "model")]
    /// Invoke, as the person here, another provider's action a control of
    /// this window is bound to ([`crate::bound_dispatch::BoundActionError::Hosted`]):
    /// the provider admits and runs it as any caller's, through the broker.
    fn invoke_hosted(
        &mut self,
        hosted: crate::bound_dispatch::HostedInvocation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        if !self.collaboration.manager.is_connected() {
            return Err(tilo_actions::ApiError::Unavailable {
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
            // Sent again with the person's yes, the call carries it, as the device's ask sends it.
            confirmation: hosted
                .approval
                .map(tilo_actions::remote::Confirmation::Asked),
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        self.actions
            .broker_requests
            .insert(BrokerReply::Receipt(request.invocation), request_id);
        self.actions.hosted_requests.insert(request_id);
        self.collaboration
            .manager
            .send(CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                press: crate::press::current(),
            });
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }

    #[cfg(feature = "model")]
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
            self.refuse_in_window(&tilo_actions::ApiError::Failed(reason));
        }
        true
    }

    #[cfg(feature = "model")]
    /// The broker refused an invocation of this window's own action outright
    /// (`request_id` is none of the kinds settled before this): nothing was
    /// carried out and no handler of the app answers for it, so the person
    /// is told here, in the window's bar, and a line is logged.
    pub(super) fn settle_own_refusal(
        &mut self,
        request_id: u64,
        result: &Result<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate), String>,
    ) {
        let Err(reason) = result else {
            return;
        };
        tracing::warn!(
            request_id,
            %reason,
            "an invocation of this window's own action was refused"
        );
        self.refuse_in_window(&tilo_actions::ApiError::Failed(reason.clone()));
    }

    #[cfg(feature = "model")]
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

    #[cfg(feature = "model")]
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

    #[cfg(feature = "model")]
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
            let polled = if let Some(target) = self.actions.bound_invocations.get(&invocation) {
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
            if update.is_terminal()
                && self.queue_documents_before_terminal()
                    == crate::app::document_routes::Handoff::Stopped
            {
                // Keep admitted lifecycle state; do not poll the completed
                // operation again or fabricate a no-effect refusal.
                changed = true;
                break;
            }
            if update.is_terminal() {
                self.actions.bound_invocations.remove(&invocation);
                self.actions.directed_invocations.remove(&invocation);
                self.actions.acting_for.remove(&invocation);
                // As a direct answer, it waits for what it caused to land.
                self.settle_reply(pending.wire_id, update);
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
                    .send(CollabRequest::ResourceLocalEffect { address, update });
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

    #[cfg(feature = "model")]
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

    #[cfg(feature = "model")]
    /// Open search the way `request` asks: this app, or one part of it, with
    /// the query already typed. A standalone window shows its own palette;
    /// inside the Tilo shell the component hands the request to the shell.
    pub fn open_search(&mut self, request: crate::widget::search_request::SearchRequest) {
        // A dialog the app holds open keeps the window to itself: search waits until the person
        // finishes it.
        if self.root_widget.modal_capture().is_some() {
            return;
        }
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
            self.rebuild_action_palette_admission();
        }
    }

    #[cfg(feature = "model")]
    /// Inside a Tilo session there is one search, the shell's: hand it
    /// `request` and open nothing here. A window the shell hosts in its own
    /// process asks through its component's host intent; one running as a
    /// process of its own asks over the session's surface service. Whether
    /// the request went to the session.
    pub(super) fn hand_search_to_session(
        &mut self,
        request: &crate::widget::search_request::SearchRequest,
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

    #[cfg(feature = "model")]
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
            || self.palette_open()
            || self.ime_window_state().is_some()
        {
            return false;
        }
        self.open_search(crate::widget::search_request::SearchRequest::this_app());
        true
    }

    #[cfg(feature = "model")]
    /// A printable key no widget took, with a selection in focus and nothing
    /// focused that takes text: the person is talking about the selection.
    /// A send-to box opens on it with the key in its message, where their
    /// typing goes on. Shortcuts, a text field, a terminal and an input
    /// method all take their keys first, and without a shell that sends
    /// nothing happens.
    #[cfg(feature = "native")]
    pub(crate) fn handle_unconsumed_typed_key(
        &mut self,
        event: &winit::event::WindowEvent,
    ) -> bool {
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
            || !crate::shared_selection::send_host_available()
            || self.palette_open()
            || self.ime_window_state().is_some()
        {
            return false;
        }
        let Some(selection) = crate::shared_selection::subtree_selection(self.root_widget.as_ref())
        else {
            return false;
        };
        // A key nothing took over a selection begins a message about it.
        self.open_send_box(Some(selection), None, Some(text), &[], &[]);
        true
    }

    #[cfg(feature = "model")]
    /// Whether this window presents search itself. A shell hosting the app
    /// presents it instead, and receives the request as a host intent.
    fn presents_search(&self) -> bool {
        self.search_presented
    }

    #[cfg(feature = "model")]
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
    fn refuse_in_window(&mut self, error: &tilo_actions::ApiError) {
        self.refuse_at(error, None);
    }

    fn refuse_at(&mut self, error: &tilo_actions::ApiError, anchor: Option<vello::kurbo::Rect>) {
        self.actions.refused.push(error.to_string());
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
    pub(super) fn dismiss_control_notice(&mut self) {
        if self.actions.control_notice.take().is_some() {
            self.request_redraw_for_notice();
        }
    }

    fn request_redraw_for_notice(&mut self) {
        self.layout.layout_dirty = true;
    }

    #[cfg(feature = "model")]
    /// Open search on the app's action `address`, asking for its
    /// arguments: the session's search inside a session, the window's own
    /// outside one. Whether it opened.
    pub(super) fn open_search_at_action(&mut self, address: &tilo_actions::ActionAddress) -> bool {
        let asked = crate::widget::search_request::SearchRequest::asking(
            crate::widget::search_request::ScopeDecl::new(address.resource.clone(), String::new()),
            address.action.clone(),
        );
        if self.hand_search_to_session(&asked) {
            return true;
        }
        self.open_action_palette_at(address)
    }

    #[cfg(feature = "model")]
    fn open_action_palette_at(&mut self, address: &tilo_actions::ActionAddress) -> bool {
        self.open_action_palette();
        let activated = self
            .actions
            .action_palette
            .as_deref_mut()
            .is_some_and(|palette| palette.activate_address(&address.resource, &address.action));
        if !activated {
            self.close_action_palette();
        }
        activated
    }

    #[cfg(feature = "model")]
    fn filter_resource_for_local_actor(
        &self,
        resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        self.permission_ctx
            .permitted_resource(&crate::collaboration::local_app_id(), resource)
    }

    #[cfg(feature = "model")]
    pub fn close_action_palette(&mut self) {
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        self.layout.request_rebuild();
        self.actions.action_palette = None;
    }

    #[cfg(feature = "model")]
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

    #[cfg(feature = "model")]
    pub fn action_palette(&self) -> Option<&crate::action_palette::ActionPalette> {
        self.actions.action_palette.as_deref()
    }

    #[cfg(feature = "model")]
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
                && tilo_actions::api::resolve(snapshot.resources(), &target.address.address)
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

    #[cfg(feature = "model")]
    fn action_palette_handle_event(&mut self, event: &WindowEvent, scale: f64) -> bool {
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            return self.action_palette_handle_key(event);
        }
        if !self.palette_presented() {
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

    #[cfg(feature = "model")]
    fn action_palette_handle_key(&mut self, event: &WindowEvent) -> bool {
        if !self.palette_open() {
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

    #[cfg(feature = "model")]
    fn finish_action_palette_input(&mut self, consumed: bool) -> bool {
        // The transition may have replaced the field or closed the overlay.
        // Re-admission below retires the old tree and preserves the real
        // redraw's structural-change condition for the next layout solve.
        self.layout.request_rebuild();
        let root = self.state.root.clone();
        let exact_capture = self
            .actions
            .action_palette
            .as_deref()
            .and_then(crate::action_palette::ActionPalette::history_exact_capture_request);
        if let Some((target, invocation, original_arguments)) = exact_capture {
            let observation = crate::bound_dispatch::control_observation_with_arguments(
                self.root_widget.as_ref(),
                &self.state.doc,
                &target,
                &invocation,
                &original_arguments,
            )
            .map_err(|error| error.to_string());
            if let Some(palette) = self.actions.action_palette.as_deref_mut() {
                palette.retain_history_exact_observation(observation);
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
            let result = self.run_palette_operation(&pending);
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

/// The wire bytes for a directory page's own fields (revision, cursor, flags and the
/// row list) beyond its rows, on the wire.
#[cfg(feature = "model")]
const DIRECTORY_PAGE_WIRE_BYTES: usize = ::collaboration::action_directory::PAGE_WIRE_BYTES;

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
/// A native window's session API ([`crate::session_api`]): the workspace
/// broker its collaboration connection reaches.
pub(super) struct BrokerApi<'a> {
    manager: &'a mut crate::collaboration::CollaborationManager,
    broker_requests: &'a mut std::collections::HashMap<BrokerReply, u64>,
    next_broker_request_id: &'a mut Option<u64>,
    pending_application_launches:
        &'a mut std::collections::HashMap<u64, tilo_actions::InvocationId>,
}

#[cfg(feature = "model")]
impl BrokerApi<'_> {
    /// Retain exactly one broker request per admitted launcher invocation.
    /// The fixed cap is a failure boundary: overflowing it must refuse the
    /// invocation before it can be left Accepted without a terminal receipt.
    pub(super) fn admit(
        &mut self,
        request_id: u64,
        operation: tilo_actions::InvocationId,
    ) -> Result<(), tilo_actions::ApiError> {
        if self.pending_application_launches.len() >= MAX_PENDING_APPLICATION_LAUNCHES {
            return Err(tilo_actions::ApiError::Unavailable {
                code: "application_launch_queue_full".into(),
                reason: "Too many application launches are awaiting broker receipts".into(),
            });
        }
        self.pending_application_launches
            .insert(request_id, operation);
        Ok(())
    }
}

#[cfg(feature = "model")]
impl crate::session_api::SessionApi for BrokerApi<'_> {
    fn workspace(&self) -> Option<String> {
        self.manager
            .primary_route()
            .map(|observation| observation.route().cluster_id.to_string())
    }

    fn invoke(
        &mut self,
        operation: tilo_actions::InvocationId,
        admitted: bool,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        if !self.manager.is_connected() {
            return Err(tilo_actions::ApiError::Unavailable {
                code: "authoritative_host_unavailable".into(),
                reason: "Opening an application requires the workspace action broker".into(),
            });
        }
        // A launch runs only as the broker delivered it here.
        if !admitted {
            return Err(tilo_actions::ApiError::Failed(
                "application launch was not admitted by the broker".into(),
            ));
        }
        let request_id = App::allocate_broker_request_id(self.next_broker_request_id)?;
        self.admit(request_id, operation)?;
        self.broker_requests
            .insert(BrokerReply::Receipt(request.invocation), request_id);
        self.manager.send(CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
            press: crate::press::current(),
        });
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }
}

impl App {
    pub(crate) fn refused_unsaid(&mut self, error: &tilo_actions::ApiError) {
        self.actions.refused.push(error.to_string());
    }

    #[cfg(feature = "model")]
    pub(crate) fn waiting_controls(&mut self) -> &mut Vec<crate::app_core::WaitingControl> {
        &mut self.actions.waiting_controls
    }

    #[cfg(feature = "model")]
    pub(crate) fn component_awaits(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
    ) -> bool {
        let address = &target.address.address;
        crate::bound_dispatch::component_awaits_resource(
            self.root_widget.as_mut(),
            &self.state.doc,
            &target.binding,
            &address.resource,
            &address.action,
        )
    }

    #[cfg(feature = "model")]
    pub(crate) fn bound_publication(&self) -> &crate::bound_dispatch::BoundPublication {
        &self.actions.bound_publication
    }

    #[cfg(feature = "model")]
    pub(crate) fn broker_connected(&self) -> bool {
        self.collaboration.manager.is_connected()
    }

    #[cfg(feature = "model")]
    /// Whether the window publishes the action `intent` calls at all, whoever may run it: one
    /// the person's grants refuse is still theirs to press, and the workspace says why it won't.
    pub(crate) fn declares(&self, intent: &tilo_actions::ControlInvocation) -> bool {
        let key = self.ensure_declared_resource_cache();
        self.actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .is_some_and(|cache| {
                tilo_actions::api::resolve(&cache.declared, &intent.address).is_some()
            })
    }

    #[cfg(feature = "model")]
    pub(crate) fn locally_offered(&self, intent: &tilo_actions::ControlInvocation) -> bool {
        let (key, generation) = self.ensure_local_resource_cache();
        let cache = self.actions.local_resource_cache.borrow();
        let resources = cache
            .as_ref()
            .filter(|cache| cache.key == key && cache.permission_generation == generation)
            .and_then(|cache| cache.resources.as_deref())
            .unwrap_or_default();
        tilo_actions::api::resolve(resources, &intent.address).is_some()
    }

    #[cfg(feature = "model")]
    pub(crate) fn invoke_bound_control(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_bound_target_with_broker(target, request, None)
    }

    #[cfg(feature = "model")]
    pub(crate) fn invoke_outer_control(
        &mut self,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_unbound_resource_action(request)
    }

    #[cfg(feature = "model")]
    pub(crate) fn history_address(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError>> {
        self.invoke_history_address_local(address, argument)
    }

    #[cfg(feature = "model")]
    pub(crate) fn invoke_chosen_address(
        &mut self,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_resource_action(*request)
    }

    #[cfg(feature = "model")]
    pub(crate) fn is_host_action_resource(&self, resource: &str) -> bool {
        resource == local_input_capture::UNSAVED_RESOURCE
    }

    #[cfg(feature = "model")]
    pub(crate) fn embedded_history_acted(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }

    #[cfg(feature = "model")]
    pub(crate) fn take_search_invocation(
        &mut self,
        intent: &tilo_actions::ControlInvocation,
        owner: Option<&crate::bound_dispatch::CallbackBinding>,
    ) -> bool {
        let Some(request) = crate::widget::search_request::SearchRequest::from_invocation(intent)
        else {
            return false;
        };
        match owner {
            Some(binding) if !self.presents_search() => binding.request_search(request),
            _ => self.open_search(request),
        }
        true
    }

    /// Say that an invocation the person made did not run, and why, at the
    /// control they used: a click is never dropped with only a log line, and
    /// never opens search.
    pub(crate) fn refuse_visibly(&mut self, error: &tilo_actions::ApiError) {
        let anchor = self.tree().acting_control_rect();
        self.refuse_at(error, anchor);
    }
}

/// The revision of the permissions every window shares: an outside authority's change moves it. A
/// window without Tilo's model follows no shared permissions, so it never moves.
fn shared_permissions_revision() -> u64 {
    #[cfg(feature = "model")]
    let revision = crate::permissions::shared_revision();
    #[cfg(not(feature = "model"))]
    let revision = 0;
    revision
}
