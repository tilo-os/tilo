// Action/resource discovery, publication, dispatch, and palette integration.
//
// Kept as an `App` implementation so custom widgets and built-in controls
// share one authoritative action/resource route.

#[derive(Clone, Copy)]
struct ResourceInvocation<'a> {
    address: &'a tilo_actions::ActionAddress,
    actor: &'a str,
    argument: Option<&'a str>,
    confirmation_satisfied: bool,
    broker_admitted: bool,
    confirmation_evidence: Option<&'a serde_json::Value>,
    invocation: Option<tilo_actions::InvocationId>,
    ambient_context: Option<&'a tilo_actions::ActionContext>,
    scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
}

impl<'a> ResourceInvocation<'a> {
    fn new(address: &'a tilo_actions::ActionAddress, actor: &'a str) -> Self {
        Self {
            address,
            actor,
            argument: None,
            confirmation_satisfied: false,
            broker_admitted: false,
            confirmation_evidence: None,
            invocation: None,
            ambient_context: None,
            scope: None,
        }
    }

    fn argument(mut self, argument: Option<&'a str>) -> Self {
        self.argument = argument;
        self
    }

    fn confirmed(mut self, confirmed: bool) -> Self {
        self.confirmation_satisfied = confirmed;
        self
    }

    fn broker_admitted(mut self, admitted: bool) -> Self {
        self.broker_admitted = admitted;
        self
    }

    fn evidence(mut self, evidence: Option<&'a serde_json::Value>) -> Self {
        self.confirmation_evidence = evidence;
        self
    }

    fn invocation(mut self, invocation: Option<tilo_actions::InvocationId>) -> Self {
        self.invocation = invocation;
        self
    }

    fn ambient_context(mut self, context: Option<&'a tilo_actions::ActionContext>) -> Self {
        self.ambient_context = context;
        self
    }

    fn scope(
        mut self,
        scope: Option<&'a crate::invocation_scope::ResourceInvocationScope>,
    ) -> Self {
        self.scope = scope;
        self
    }
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
        self.actions.legacy_local_transport = false;
    }

    fn resource_cache_key(&self) -> ResourceCacheKey {
        ResourceCacheKey {
            mutation_generation: self.actions.resource_generation.get(),
        }
    }

    /// Invalidate declarations derived from document or widget-local state.
    fn invalidate_widget_resource_cache(&mut self) {
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
        complete.extend(self.history_action_resources());
        trace.finish(CacheResult::NotApplicable);
        let trace = self.trace_resource_stage(Stage::SurfaceValidation);
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        operation_handlers.insert(history_actions::OPERATION.to_string());
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

    fn collect_declared_resources(&self) -> Vec<tilo_actions::Resource> {
        let key = self.ensure_resource_cache();
        let mut resources = self
            .actions
            .resource_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.key == key)
            .map_or_else(Vec::new, |cache| cache.declared.clone());
        if let Ok(legacy) = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root_widget.as_ref(),
            &self.permission_ctx,
            &self.collaboration.local_author,
            false,
        ) {
            resources.extend(legacy);
        }
        resources
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

    fn collect_local_resources(&self) -> Vec<tilo_actions::Resource> {
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
                    .flat_map(|snapshot| snapshot.resources),
            );
        }
        if let Ok(legacy) = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root_widget.as_ref(),
            &self.permission_ctx,
            &self.collaboration.local_author,
            false,
        ) {
            resources.extend(legacy);
        }
        resources
    }

    fn refresh_control_action_availability(&mut self) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        if !self.framework_input.get()
            && self.history.as_ref().is_none_or(|h| !h.captures_input())
            && let Err(error) = crate::bound_dispatch::legacy_document::refresh_legacy_controls(
                self.root_widget.as_mut(),
                &self.permission_ctx,
                &self.collaboration.local_author,
            )
        {
            tracing::warn!(%error, "legacy control availability refused");
        }
        let trace = self.trace_resource_stage(Stage::BoundDiscovery);
        let history_door = self.refresh_history_controls();
        let snapshots = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        trace.finish(CacheResult::NotObserved);
        let trace = self.trace_resource_stage(Stage::BoundControlProjection);
        let _ = crate::bound_dispatch::refresh_bound_controls(
            self.root_widget.as_mut(),
            &snapshots,
            &history_door,
        );
        trace.finish(CacheResult::NotApplicable);
        // Only ActionEmitter controls consume this address projection. Semantic,
        // document and legacy controls still use their normal invocation doors.
        // Recheck presence: widget-local changes can add/remove emitters.
        if !crate::widget::controls::has_action_control_bindings(self.root_widget.as_ref()) {
            // A replacement emitter must not inherit a prior refresh stamp.
            self.actions.last_control_availability = None;
            return;
        }
        let cache_key = self.ensure_local_control_cache();
        let trace = self.trace_resource_stage(Stage::ControlCache);
        if self.actions.last_control_availability.as_ref() == Some(&cache_key) {
            trace.finish(CacheResult::Hit);
            return;
        }
        trace.finish(CacheResult::Miss);
        let trace = self.trace_resource_stage(Stage::ControlRefresh);
        // Clone only the compact address set so the cache borrow does not
        // span a mutable call into the retained widget tree.
        let mut allowed = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .map_or_else(std::collections::HashSet::new, |cache| {
                cache.allowed.clone()
            });
        // Opening search is the host's, and this host always answers it.
        allowed.insert(crate::search::search_address());
        // So is the window's History door, wherever History serves it.
        allowed.extend(history_door);
        crate::widget::controls::refresh_control_action_availability(
            self.root_widget.as_mut(),
            &allowed,
        );
        self.actions.last_control_availability = Some(cache_key);
        trace.finish(CacheResult::NotApplicable);
    }

    /// The complete machine-driveable surface: domain resources declared by
    /// the app plus ordinary controls projected from its retained semantic
    /// tree. Ctrl+K, scripts, and agents therefore consume one catalogue.
    pub fn collect_resources(&self) -> Vec<tilo_actions::Resource> {
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
                    .flat_map(|snapshot| snapshot.resources),
            );
        }
        if let Ok(legacy) = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root_widget.as_ref(),
            &self.permission_ctx,
            &self.collaboration.local_author,
            false,
        ) {
            resources.extend(legacy);
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
        resources: &[tilo_actions::Resource],
    ) -> Vec<ControlBindingCoverage> {
        self.control_binding_coverage_for(
            resources,
            crate::widget::controls::control_action_bindings(self.root_widget.as_ref()),
        )
    }

    fn control_binding_coverage_for(
        &self,
        resources: &[tilo_actions::Resource],
        bindings: Vec<crate::widget::ControlBindingMetadata>,
    ) -> Vec<ControlBindingCoverage> {
        // Shared chrome invokes doors served by this host, rather than by
        // the app's resource provider. Audit those exact declarations too.
        let mut host_resources = vec![crate::search::search_resource()];
        if let Some(history) = &self.history {
            host_resources.push(history.door_resource());
        } else if self.history_chrome_enabled {
            host_resources.push(crate::standalone_history_host::history_resource(
                true,
                self.document_history.is_open(),
            ));
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
                    crate::widget::ControlBindingKind::LegacyDocumentPath(_)
                    | crate::widget::ControlBindingKind::LegacyCallback => {
                        ControlBindingStatus::ExplicitLegacy
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
            return witness.route.app_id.clone();
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
    fn publication_resources(&mut self) -> Vec<tilo_actions::Resource> {
        fn remove_reserved(mut resource: tilo_actions::Resource) -> Option<tilo_actions::Resource> {
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
                &component.resources,
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
                        resources = component.resources.len(),
                        %violations,
                        "refusing to publish a component surface that fails action contract validation"
                    );
                }
                continue;
            }
            for resource in component.resources {
                if let Ok(resource) = self
                    .actions
                    .bound_publication
                    .project(&component.binding, resource)
                {
                    resources.push(resource);
                }
            }
        }
        if let Ok(legacy) = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root_widget.as_ref(),
            &self.permission_ctx,
            &self.collaboration.local_author,
            true,
        ) {
            resources.extend(legacy);
        }
        resources
    }

    /// Publish the live resource/action tree through the session broker.
    /// Level-triggered at the wire, diffed here so an unchanged frame emits
    /// nothing; publishing an empty list explicitly withdraws a closed tree.
    fn publish_action_surface(&mut self) {
        if self.collaboration.manager.is_connected() {
            self.refresh_document_routes();
        }
        self.publish_action_surface_after_refresh();
    }

    fn publish_action_surface_after_refresh(&mut self) {
        #[cfg(test)]
        {
            self.actions.action_surface_publication_attempts = self
                .actions
                .action_surface_publication_attempts
                .saturating_add(1);
        }
        if !self.collaboration.manager.is_connected()
            || !self.published_action_history_ready_after_refresh()
        {
            return;
        }
        if self.collaboration.manager.take_action_surface_failed() {
            // The broker did not install the last surface: nothing counts as
            // published until it acknowledges one.
            self.actions.last_published_actions = None;
        }
        let route = self.bound_broker_route().cloned();
        if self.actions.last_published_bound_route != route {
            self.actions.last_published_actions = None;
            self.actions.action_directory_snapshot = None;
            self.actions.last_published_bound_route = route;
        }
        let publication_app_id = self.publication_app_id();
        // This is independent of the legacy eager surface below.  The
        // broker owns the capability and asks the UI thread to materialise a
        // bounded page; large apps therefore do not become a flattened RPC
        // catalogue just because they are visible in shell search.
        self.collaboration
            .manager
            .publish_action_directory(publication_app_id.clone());
        let resources = self.publication_resources();
        // Use this exact snapshot for coverage as well as publication. That
        // prevents recursive/repeated resource collection and makes the audit
        // correspond to the catalogue about to cross the wire.
        let binding_coverage = self.control_binding_coverage_for(
            &self.collect_resources(),
            crate::widget::controls::action_control_bindings(self.root_widget.as_ref()),
        );
        if binding_coverage
            .iter()
            .any(|entry| entry.status == ControlBindingStatus::StaleAddress)
        {
            self.actions.action_directory_snapshot = None;
            self.actions.action_directory_stamp = None;
            self.actions.action_directory_cursors.clear();
            for entry in binding_coverage
                .iter()
                .filter(|entry| entry.status == ControlBindingStatus::StaleAddress)
            {
                let binding = format!("{:?}", entry.binding.kind);
                if self.actions.refusals_logged.first(&(
                    "stale",
                    &entry.binding.control_id,
                    &binding,
                )) {
                    tracing::error!(
                        control = %entry.binding.control_id,
                        %binding,
                        "refusing to publish a surface with a stale control action binding"
                    );
                }
            }
            if self
                .actions
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty())
            {
                if self
                    .collaboration
                    .manager
                    .send(CollabRequest::PublishActions {
                        app_id: publication_app_id,
                        actions: Vec::new(),
                    })
                {
                    self.actions.last_published_actions = Some(Vec::new());
                }
            }
            return;
        }
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        operation_handlers.insert(history_actions::OPERATION.to_string());
        // Bound components have already been validated against their own registries
        // by publication_resources. Only legacy resources use the host registry.
        let unbound_resources = resources
            .iter()
            .filter(|resource| {
                !resource
                    .id
                    .as_str()
                    .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
            })
            .cloned()
            .collect::<Vec<_>>();
        let violations = tilo_actions::validate_surface(
            &unbound_resources,
            &self.actions.action_registry,
            &operation_handlers,
        );
        if !violations.is_empty() {
            for violation in &violations {
                if self.actions.refusals_logged.first(&(
                    "surface",
                    &violation.address,
                    &violation.reason,
                )) {
                    tracing::error!(
                        address = %violation.address,
                        reason = %violation.reason,
                        "refusing to publish an invalid resource/action surface"
                    );
                }
            }
            // Withdraw anything previously published. A stale, apparently
            // valid catalogue is more dangerous than an empty one.
            if self
                .actions
                .last_published_actions
                .as_ref()
                .is_none_or(Vec::is_empty)
            {
                return;
            }
            if self
                .collaboration
                .manager
                .send(CollabRequest::PublishActions {
                    app_id: crate::collaboration::local_app_id(),
                    actions: Vec::new(),
                })
            {
                self.actions.last_published_actions = Some(Vec::new());
            }
            return;
        }
        let descriptors = tilo_actions::door::survey(&resources);
        self.capture_action_directory_snapshot(&resources, &descriptors);
        if self.actions.last_published_actions.as_ref() == Some(&descriptors) {
            return;
        }
        let actions = descriptors
            .iter()
            .map(crate::collaboration::ExportedAction::from)
            .collect();
        // A surface the transport did not take is retried on the next frame.
        if self
            .collaboration
            .manager
            .send(CollabRequest::PublishActions {
                app_id: publication_app_id,
                actions,
            })
        {
            self.actions.last_published_actions = Some(descriptors);
        }
    }

    fn action_directory_revision(&mut self) -> String {
        // Internal cursor/cache callers cannot turn an unavailable policy into
        // a reusable identity. Public query/resolve/delivery use the fallible
        // method and refuse before returning a cached result or invoking code.
        self.try_action_directory_revision()
            .unwrap_or_else(|_| format!("unavailable:{}", uuid::Uuid::new_v4()))
    }

    fn try_action_directory_revision(&mut self) -> Result<String, String> {
        let captured = (|| {
            let owners = crate::bound_dispatch::directory_witness(
                self.root_widget.as_ref(),
                &self.state.doc,
            )?;
            let permissions = self.permission_ctx.directory_witness()?;
            let reusable = owners.iter().all(|owner| owner.generation.is_some());
            let directory_providers =
                crate::bound_dispatch::search_providers(self.root_widget.as_ref());
            let provider_domains = directory_providers
                .iter()
                .map(|provider| provider.action_directory_invocation_domain())
                .collect();
            let witness = action_directory_snapshot::DirectoryWitness {
                registry_generation: self.actions.action_registry_generation,
                root_domain: crate::resources::action_directory_invocation_domain(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                ),
                provider_domains,
                key: self.resource_cache_key(),
                permissions,
                permission_generation: self.actions.permission_generation.get(),
                route: self.bound_broker_route().cloned(),
                owners,
                root_revision: crate::resources::action_directory_revision(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                ),
                providers: directory_providers
                    .into_iter()
                    .map(|provider| {
                        format!(
                            "{}:{:?}:{:?}",
                            provider.name(),
                            provider
                                .callback_binding()
                                .map(|binding| binding.lifetime_id),
                            provider.action_directory_generation()
                        )
                    })
                    .collect(),
            };
            Ok::<_, String>((reusable, witness))
        })();
        let (reusable, witness) = match captured {
            Ok(value) => value,
            Err(error) => {
                self.actions.action_directory_stamp = None;
                self.actions.action_directory_snapshot = None;
                self.actions.action_directory_cursors.clear();
                return Err(error);
            }
        };
        if reusable
            && let Some((old, revision)) = &self.actions.action_directory_stamp
            && old.as_ref() == &witness
        {
            return Ok(revision.clone());
        }
        // Generations are updated at mutation boundaries; reading Automerge
        // heads here would commit an open transaction and split an undo unit.
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_cursors.clear();
        let revision = format!("v5:{}", uuid::Uuid::new_v4());
        self.actions.action_directory_stamp = Some((std::rc::Rc::new(witness), revision.clone()));
        Ok(revision)
    }

    /// The descriptor a directory row carries. Directory pages have never
    /// carried a resource view: the broker proves a view's mount and range
    /// bindings against the actions on the same page, which a bounded page
    /// cannot promise, so views reach the shell through the published
    /// surface instead.
    fn exported_directory_action(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> runtime_collab::ExportedAction {
        runtime_collab::ExportedAction {
            view: None,
            ..runtime_collab::ExportedAction::from(descriptor)
        }
    }

    fn directory_resource_row(
        &self,
        resource: &tilo_actions::Resource,
        parent_resource: &str,
        may_descend: bool,
    ) -> Option<runtime_collab::ActionDirectoryRow> {
        // Preserve the explicit-or-View disclosure rule, finding only the
        // needed descriptor in this bounded provider result, not surveying a
        // complete action catalogue for each displayed row.
        fn visibility(
            resource: &tilo_actions::Resource,
            explicit: Option<&tilo_actions::ActionAddress>,
        ) -> Option<tilo_actions::ActionDescriptor> {
            resource
                .actions
                .iter()
                .find(|action| match explicit {
                    Some(address) => {
                        address.resource == resource.id.as_str() && address.action == action.id
                    }
                    None => action.kind == tilo_actions::OperationKind::View,
                })
                .map(|action| App::directory_descriptor(resource, action))
                .or_else(|| {
                    resource
                        .children
                        .iter()
                        .find_map(|child| visibility(child, explicit))
                })
        }
        let explicit = self.directory_explicit_visibility(resource);
        let visibility = visibility(resource, explicit.as_ref())?;
        Some(runtime_collab::ActionDirectoryRow {
            parent_resource: parent_resource.to_owned(),
            resource: resource.id.as_str().to_owned(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            presentation: resource
                .presentation
                .clone()
                .filter(tilo_actions::ResourcePresentation::is_valid),
            action: None,
            descriptor: None,
            has_children: may_descend
                && (!resource.children.is_empty() || !resource.actions.is_empty()),
            visibility: Some(tilo_actions::remote::RowVisibility {
                action: visibility.address.action,
                contract: visibility.contract,
            }),
        })
    }

    fn directory_action_row(
        resource: &tilo_actions::Resource,
        action_id: &str,
    ) -> Option<runtime_collab::ActionDirectoryRow> {
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == action_id)?;
        let descriptor = Self::directory_descriptor(resource, action);
        let exported = Self::exported_directory_action(&descriptor);
        Some(runtime_collab::ActionDirectoryRow {
            parent_resource: resource.id.as_str().to_owned(),
            resource: resource.id.as_str().to_owned(),
            resource_kind: exported.resource_kind.clone(),
            resource_label: exported.resource_label.clone(),
            presentation: None,
            action: Some(action_id.to_owned()),
            visibility: Some(tilo_actions::remote::RowVisibility {
                action: action_id.to_owned(),
                contract: exported.contract.clone(),
            }),
            descriptor: Some(exported),
            has_children: false,
        })
    }

    fn action_directory_cursor_key(&mut self) -> String {
        let revision = self.action_directory_revision();
        format!("tilo-dir-{}-{revision}", uuid::Uuid::new_v4())
    }

    fn action_directory_next_cursor(&mut self, state: ActionDirectoryCursor) -> Vec<u8> {
        if self.actions.action_directory_cursors.len() >= ACTION_DIRECTORY_MAX_CURSORS {
            // Bound memory even when a remote peer starts many abandoned
            // searches. Old cursors become stale rather than holding a tree.
            self.actions.action_directory_cursors.clear();
        }
        let key = self.action_directory_cursor_key();
        self.actions
            .action_directory_cursors
            .insert(key.clone(), state);
        key.into_bytes()
    }

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "every directory arithmetic/index is guarded by the hard page budgets and loop bounds directly above it"
    )]
    fn action_directory_page(
        &mut self,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> runtime_collab::ActionDirectoryPage {
        let Ok(revision) = self.try_action_directory_revision() else {
            return runtime_collab::ActionDirectoryPage {
                revision: String::new(),
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: !query.revision.is_empty(),
                budget_exhausted: true,
            };
        };
        let requested_revision = query.revision;
        if !requested_revision.is_empty() && requested_revision != revision {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }
        let max_results = usize::try_from(query.max_results)
            .unwrap_or(ACTION_DIRECTORY_MAX_RESULTS)
            .clamp(1, ACTION_DIRECTORY_MAX_RESULTS);
        let max_work = usize::try_from(query.max_work)
            .unwrap_or(ACTION_DIRECTORY_MAX_WORK)
            .clamp(1, ACTION_DIRECTORY_MAX_WORK);
        let max_bytes = usize::try_from(query.max_bytes)
            .unwrap_or(ACTION_DIRECTORY_MAX_BYTES)
            .clamp(1, ACTION_DIRECTORY_MAX_BYTES);
        let max_depth = query.max_depth.min(ACTION_DIRECTORY_MAX_DEPTH);
        let cursor_key = if query.cursor.is_empty() {
            None
        } else {
            match String::from_utf8(query.cursor) {
                Ok(cursor) => Some(cursor),
                Err(_) => {
                    return runtime_collab::ActionDirectoryPage {
                        revision,
                        rows: Vec::new(),
                        next_cursor: Vec::new(),
                        exhausted: false,
                        stale_cursor: true,
                        budget_exhausted: false,
                    };
                }
            }
        };
        let previous = cursor_key
            .as_deref()
            .and_then(|key| self.actions.action_directory_cursors.remove(key));
        if cursor_key.is_some() && previous.is_none() {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }
        let mut state = previous.unwrap_or_else(|| ActionDirectoryCursor {
            revision: revision.clone(),
            needle: query.needle.clone(),
            parent_resource: query.parent_resource.clone(),
            provider_index: 0,
            provider_cursor: None,
            browse_offset: 0,
            root_widget_index: 0,
            root_provider_cursor: None,
        });
        if state.revision != revision
            || state.needle != query.needle
            || state.parent_resource != query.parent_resource
        {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }

        let mut rows = Vec::new();
        let mut estimated_bytes = DIRECTORY_PAGE_WIRE_BYTES.saturating_add(revision.len());
        let add = |row: runtime_collab::ActionDirectoryRow,
                   rows: &mut Vec<runtime_collab::ActionDirectoryRow>,
                   bytes: &mut usize| {
            *bytes = bytes.saturating_add(runtime_collab::action_directory_row_wire_bytes(&row));
            rows.push(row);
        };

        if query.needle.is_empty() {
            if query.parent_resource.is_empty() {
                let mut widget_index = state.root_widget_index;
                let mut provider_cursor = state.root_provider_cursor.clone();
                let mut inspected = 0usize;
                let mut exhausted = false;
                let mut projected_offset = state.browse_offset;
                let mut projected_done = true;
                if let Some(snapshot) = &self.actions.action_directory_snapshot {
                    while let Some(row) = snapshot.roots.get(projected_offset) {
                        if rows.len() >= max_results || inspected >= max_work {
                            break;
                        }
                        let mut row = row.clone();
                        row.has_children &= max_depth > 0;
                        add(row, &mut rows, &mut estimated_bytes);
                        if estimated_bytes > max_bytes {
                            rows.pop();
                            break;
                        }
                        projected_offset = projected_offset.saturating_add(1);
                        inspected = inspected.saturating_add(1);
                    }
                    projected_done = projected_offset >= snapshot.roots.len();
                }
                while projected_done && rows.len() < max_results && inspected < max_work {
                    let Some(page) = crate::widget::action_directory_page_at(
                        self.root_widget.as_ref(),
                        &self.state.doc,
                        widget_index,
                        provider_cursor.as_deref().unwrap_or_default(),
                        1,
                    ) else {
                        exhausted = true;
                        break;
                    };
                    let Some((mut page, binding)) = page else {
                        widget_index = widget_index.saturating_add(1);
                        provider_cursor = None;
                        inspected = inspected.saturating_add(1);
                        continue;
                    };
                    if page.resources.len() > 1 || (!page.exhausted && page.next_cursor.is_empty())
                    {
                        // The declared page contract was violated. A hidden
                        // tail is worse than no remote hierarchy at all.
                        return runtime_collab::ActionDirectoryPage {
                            revision,
                            rows: Vec::new(),
                            next_cursor: Vec::new(),
                            exhausted: false,
                            stale_cursor: false,
                            budget_exhausted: true,
                        };
                    }
                    if let Some(binding) = binding {
                        if self.require_bound_broker_route(&binding).is_ok()
                            && crate::bound_dispatch::validate_bound_resources(
                                self.root_widget.as_mut(),
                                &binding,
                                &page.resources,
                            )
                            .unwrap_or(false)
                        {
                            page.resources = page
                                .resources
                                .into_iter()
                                .filter_map(|resource| {
                                    crate::bound_dispatch::filter_resource(&binding, resource)
                                })
                                .filter_map(|resource| {
                                    self.actions
                                        .bound_publication
                                        .project(&binding, resource)
                                        .ok()
                                })
                                .collect();
                        } else {
                            page.resources.clear();
                        }
                    } else {
                        page.resources.retain(|resource| {
                            !resource
                                .id
                                .as_str()
                                .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
                        });
                    }
                    let before_index = widget_index;
                    let before_cursor = provider_cursor.clone();
                    if let Some(resource) = page.resources.first()
                        && !self.actions.action_directory_snapshot.as_ref().is_some_and(
                            |snapshot| snapshot.nodes.contains_key(resource.id.as_str()),
                        )
                        && let Some(row) = self.directory_resource_row(resource, "", max_depth > 0)
                    {
                        add(row, &mut rows, &mut estimated_bytes);
                        if estimated_bytes > max_bytes {
                            rows.pop();
                            break;
                        }
                    }
                    if page.exhausted {
                        widget_index = widget_index.saturating_add(1);
                        provider_cursor = None;
                    } else {
                        provider_cursor = Some(page.next_cursor);
                    }
                    inspected = inspected.saturating_add(1);
                    if rows.len() >= max_results {
                        break;
                    }
                    // An empty non-exhausted source page may not advance
                    // indefinitely; its opaque cursor must still change.
                    if rows.is_empty()
                        && before_index == widget_index
                        && before_cursor == provider_cursor
                    {
                        return runtime_collab::ActionDirectoryPage {
                            revision,
                            rows: Vec::new(),
                            next_cursor: Vec::new(),
                            exhausted: false,
                            stale_cursor: false,
                            budget_exhausted: true,
                        };
                    }
                }
                let next_cursor = (!exhausted).then(|| {
                    self.action_directory_next_cursor(ActionDirectoryCursor {
                        browse_offset: projected_offset,
                        root_widget_index: widget_index,
                        root_provider_cursor: provider_cursor,
                        ..state
                    })
                });
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows,
                    next_cursor: next_cursor.unwrap_or_default(),
                    exhausted,
                    stale_cursor: false,
                    budget_exhausted: !exhausted,
                };
            }
            let mut offset = state.browse_offset;
            let mut inspected = 0usize;
            let mut projected_total = 0usize;
            if let Some(node) = self
                .actions
                .action_directory_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.nodes.get(&query.parent_resource))
            {
                projected_total = node.rows.len();
                while let Some(row) = node.rows.get(offset) {
                    if rows.len() >= max_results || inspected >= max_work {
                        break;
                    }
                    let mut row = row.clone();
                    row.has_children &= max_depth > 0
                        && node.depth.saturating_add(1) < usize::from(ACTION_DIRECTORY_MAX_DEPTH);
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        break;
                    }
                    offset = offset.saturating_add(1);
                    inspected = inspected.saturating_add(1);
                }
            }
            if offset < projected_total || rows.len() >= max_results || inspected >= max_work {
                let next_cursor = self.action_directory_next_cursor(ActionDirectoryCursor {
                    browse_offset: offset,
                    ..state
                });
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows,
                    next_cursor,
                    exhausted: false,
                    stale_cursor: false,
                    budget_exhausted: true,
                };
            }
            let parent = self.resolve_publication_resource(&query.parent_resource, "");
            let parent_actions = parent.as_ref().map_or_else(Vec::new, |resource| {
                resource
                    .actions
                    .iter()
                    .map(|action| action.id.clone())
                    .collect()
            });
            let direct = parent
                .as_ref()
                .map_or_else(Vec::new, |resource| resource.children.clone());
            let total = projected_total
                .saturating_add(parent_actions.len())
                .saturating_add(direct.len());
            while offset < total && rows.len() < max_results && inspected < max_work {
                let provider_offset = offset.saturating_sub(projected_total);
                let row = if provider_offset < parent_actions.len() {
                    let action = &parent_actions[provider_offset];
                    let projected =
                        self.actions
                            .action_directory_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| {
                                snapshot
                                    .actions
                                    .contains_key(&tilo_actions::ActionAddress::new(
                                        &query.parent_resource,
                                        action,
                                    ))
                            });
                    if projected {
                        None
                    } else {
                        parent
                            .as_ref()
                            .and_then(|resource| Self::directory_action_row(resource, action))
                    }
                } else {
                    let resource = &direct[provider_offset - parent_actions.len()];
                    if self
                        .actions
                        .action_directory_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.nodes.contains_key(resource.id.as_str()))
                    {
                        None
                    } else {
                        self.directory_resource_row(resource, &query.parent_resource, max_depth > 0)
                    }
                };
                if let Some(row) = row {
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        break;
                    }
                }
                offset += 1;
                inspected += 1;
            }
            let exhausted = offset >= total;
            let next_cursor = (!exhausted).then(|| {
                self.action_directory_next_cursor(ActionDirectoryCursor {
                    browse_offset: offset,
                    ..state
                })
            });
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows,
                next_cursor: next_cursor.unwrap_or_default(),
                exhausted,
                stale_cursor: false,
                budget_exhausted: !exhausted,
            };
        }

        let providers = crate::bound_dispatch::search_providers(self.root_widget.as_ref())
            .into_iter()
            .filter(|provider| provider.action_directory_generation().is_some())
            .collect::<Vec<_>>();
        let mut provider_index = state.provider_index;
        let mut provider_cursor = state.provider_cursor;
        let mut work_left = max_work;
        let mut byte_exhausted = false;
        while provider_index < providers.len() && rows.len() < max_results && work_left > 0 {
            if provider_cursor.as_ref().is_some_and(|cursor| {
                !providers[provider_index].action_directory_cursor_is_current(cursor)
            }) {
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows: Vec::new(),
                    next_cursor: Vec::new(),
                    exhausted: false,
                    stale_cursor: true,
                    budget_exhausted: false,
                };
            }
            // Request one row at a time.  The provider's opaque cursor is
            // after its returned page, so asking for a larger page and then
            // stopping at the byte ceiling would silently skip the unfit tail.
            // This preserves every canonical result while still bounding this
            // request to `max_results` provider steps.
            let room = 1;
            let page = providers[provider_index].search_page(
                &crate::search::SearchQuery::new(query.needle.as_str()),
                provider_cursor.as_ref(),
                work_left,
                room,
            );
            work_left = work_left.saturating_sub(page.inspected);
            for hit in page.results {
                let Some(address) = hit.action_address() else {
                    continue;
                };
                let binding = providers[provider_index].callback_binding();
                if binding
                    .as_ref()
                    .is_some_and(|binding| self.require_bound_broker_route(binding).is_err())
                {
                    continue;
                }
                let resource = providers[provider_index]
                    .resolve_action_resource(&address)
                    .or_else(|| {
                        if let Some(binding) = &binding {
                            crate::bound_dispatch::resolve_component_resource(
                                self.root_widget.as_mut(),
                                &self.state.doc,
                                binding,
                                &address.resource,
                                &address.action,
                            )
                            .ok()
                            .flatten()
                        } else {
                            crate::bound_dispatch::resolve_unbound_resource(
                                self.root_widget.as_ref(),
                                &self.state.doc,
                                &address.resource,
                            )
                        }
                    })
                    .and_then(|resource| {
                        binding.as_ref().map_or(Some(resource.clone()), |binding| {
                            crate::bound_dispatch::filter_resource(binding, resource)
                        })
                    });
                let Some(mut resource) = resource else {
                    continue;
                };
                if let Some(binding) = providers[provider_index].callback_binding() {
                    if !crate::bound_dispatch::validate_bound_resources(
                        self.root_widget.as_mut(),
                        &binding,
                        std::slice::from_ref(&resource),
                    )
                    .unwrap_or(false)
                    {
                        continue;
                    }
                    let Ok(projected) = self.actions.bound_publication.project(&binding, resource)
                    else {
                        continue;
                    };
                    resource = projected;
                } else if resource
                    .id
                    .as_str()
                    .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
                {
                    continue;
                }
                let Some(row) = Self::directory_action_row(&resource, &address.action) else {
                    continue;
                };
                let before = estimated_bytes;
                add(row, &mut rows, &mut estimated_bytes);
                if estimated_bytes > max_bytes {
                    rows.pop();
                    estimated_bytes = before;
                    byte_exhausted = true;
                    break;
                }
            }
            if byte_exhausted {
                break;
            }
            if let Some(next) = page.next {
                provider_cursor = Some(next);
                if rows.len() < max_results && estimated_bytes < max_bytes && work_left > 0 {
                    continue;
                }
                break;
            }
            provider_index += 1;
            provider_cursor = None;
        }
        // Actions an app publishes through its resource tree rather than a
        // search provider are found by name too, in tree order, resuming at
        // the offset of the next unreturned match.
        let mut tree_exhausted = true;
        if !byte_exhausted && provider_index >= providers.len() {
            let needle = query.needle.to_lowercase();
            let mut matches = Vec::new();
            for resource in self.publication_resources() {
                collect_named_actions(
                    &resource,
                    &needle,
                    0,
                    ACTION_DIRECTORY_MAX_DEPTH,
                    &mut matches,
                );
            }
            let mut offset = state.browse_offset;
            while let Some((resource, action)) = matches.get(offset) {
                if rows.len() >= max_results {
                    break;
                }
                if let Some(row) = Self::directory_action_row(resource, action) {
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        byte_exhausted = true;
                        break;
                    }
                }
                offset += 1;
            }
            tree_exhausted = offset >= matches.len();
            state.browse_offset = offset;
        }
        let exhausted = !byte_exhausted && provider_index >= providers.len() && tree_exhausted;
        // Keep the source cursor *before* an oversized row when earlier rows
        // fitted. A caller that raises maxBytes resumes exactly at that row;
        // dropping it would restart and duplicate the fitted prefix.
        let next_cursor = (!exhausted && (!byte_exhausted || !rows.is_empty())).then(|| {
            self.action_directory_next_cursor(ActionDirectoryCursor {
                provider_index,
                provider_cursor,
                ..state
            })
        });
        runtime_collab::ActionDirectoryPage {
            revision,
            rows,
            next_cursor: next_cursor.unwrap_or_default(),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }

    fn resolve_publication_resource(
        &mut self,
        resource_id: &str,
        action_id: &str,
    ) -> Option<tilo_actions::Resource> {
        let address = tilo_actions::ActionAddress::new(resource_id, action_id);
        match self.actions.bound_publication.target(&address) {
            Ok(Some(target)) => {
                self.require_bound_broker_route(&target.binding).ok()?;
                let resource = crate::bound_dispatch::resolve_component_resource(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    &target.binding,
                    &target.address.address.resource,
                    action_id,
                )
                .ok()
                .flatten()?;
                if !crate::bound_dispatch::validate_bound_resources(
                    self.root_widget.as_mut(),
                    &target.binding,
                    std::slice::from_ref(&resource),
                )
                .unwrap_or(false)
                {
                    return None;
                }
                self.actions
                    .bound_publication
                    .project(&target.binding, resource)
                    .ok()
            }
            Err(_) => None,
            Ok(None) => crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                resource_id,
            )
            .or_else(|| {
                crate::bound_dispatch::search_providers(self.root_widget.as_ref())
                    .into_iter()
                    .filter(|provider| {
                        provider.callback_binding().is_none()
                            && provider.action_directory_generation().is_some()
                    })
                    .find_map(|provider| provider.resolve_action_resource(&address))
            }),
        }
    }

    fn resolve_action_directory(
        &mut self,
        revision: &str,
        resource_id: &str,
        action_id: &str,
    ) -> runtime_collab::ActionDirectoryResolution {
        let Ok(current) = self.try_action_directory_revision() else {
            return runtime_collab::ActionDirectoryResolution::Unavailable;
        };
        // No revision asks for the directory as it is now: a caller that
        // names an action it already knows (an agent's) chose it from no
        // page, and reading the revision first would cost it a round trip.
        if !revision.is_empty() && revision != current {
            return runtime_collab::ActionDirectoryResolution::StaleRevision;
        }
        if let Some(row) = self
            .actions
            .action_directory_snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .actions
                    .get(&tilo_actions::ActionAddress::new(resource_id, action_id))
            })
        {
            return runtime_collab::ActionDirectoryResolution::Found(Box::new(row.clone()));
        }
        let resource = self.resolve_publication_resource(resource_id, action_id);
        resource
            .as_ref()
            .and_then(|resource| Self::directory_action_row(resource, action_id))
            .map_or(runtime_collab::ActionDirectoryResolution::NotFound, |row| {
                runtime_collab::ActionDirectoryResolution::Found(Box::new(row))
            })
    }

    fn published_action_directory_page(
        &mut self,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> runtime_collab::ActionDirectoryPage {
        if self.published_action_history_ready() {
            self.action_directory_page(query)
        } else {
            runtime_collab::ActionDirectoryPage {
                revision: String::new(),
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: !query.revision.is_empty(),
                budget_exhausted: true,
            }
        }
    }

    fn published_action_directory_resolution(
        &mut self,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> runtime_collab::ActionDirectoryResolution {
        if self.published_action_history_ready() {
            self.resolve_action_directory(revision, resource, action)
        } else {
            runtime_collab::ActionDirectoryResolution::Unavailable
        }
    }

    fn pump_action_directory_requests(&mut self) {
        for request in self.collaboration.manager.take_action_directory_requests() {
            match request {
                ActionDirectoryRequest::Query { query, reply } => {
                    // A cancelled caller never receives this page. Do not consume
                    // its one-use continuation before admitting a live response.
                    if !reply.is_closed() {
                        let _ = reply.send(self.published_action_directory_page(query));
                    }
                }
                ActionDirectoryRequest::Resolve {
                    revision,
                    resource,
                    action,
                    reply,
                } => {
                    let _ = reply.send(
                        self.published_action_directory_resolution(&revision, &resource, &action),
                    );
                }
            }
        }
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
        let admitted = self.admitted_requirement(&address, argument);
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
        let invoker_scope = crate::invocation_scope::InvokerScope::enter(invoker);
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
                    details: None,
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
        // A terminal must follow the document commands it covers: queue every
        // changed owned record first, or retire the transport as uncertain.
        if update.is_terminal() && !self.queue_documents_before_terminal() {
            return;
        }
        self.collaboration
            .manager
            .send(CollabRequest::UpdateInvocation { id, update });
    }

    /// What the invoker of `address` was admitted for: the action's own
    /// requirement for its `argument`, which the admitting host checked
    /// they hold. Nothing when it no longer resolves.
    fn admitted_requirement(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> tilo_actions::PermissionRequirement {
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
                    tilo_actions::door::resolve(&snapshot.resources, &target.address.address)
                })
                .map_or(tilo_actions::PermissionRequirement::None, |spec| {
                    spec.permission_for_argument(argument)
                });
        }
        tilo_actions::door::resolve(&resources, address)
            .map_or(tilo_actions::PermissionRequirement::None, |spec| {
                spec.permission_for_argument(argument)
            })
    }

    fn resolve_resource_invocation<'a>(
        resources: &'a [tilo_actions::Resource],
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
            .map(|witness| witness.route.clone())
    }

    fn encode_resource_action(
        resources: &[tilo_actions::Resource],
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
            .flat_map(|snapshot| &snapshot.resources)
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
        self.publish_action_surface();
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
        let broker_requests = &mut self.actions.broker_requests;
        let manager = &mut self.collaboration.manager;
        Self::dispatch_broker_request(
            &mut self.actions.next_broker_request_id,
            manager.is_connected(),
            address,
            typed_arguments,
            argument,
            confirmation_evidence,
            |request_id, request| {
                broker_requests.insert(BrokerReply::Receipt(request.invocation), request_id);
                manager.send(CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                });
            },
        )
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
        let arguments = typed_arguments.cloned().unwrap_or_else(|| {
            let mut arguments = serde_json::Map::new();
            if let Some(value) = argument {
                arguments.insert(
                    tilo_actions::PROMPTED_PARAM.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
            arguments
        });
        send(
            request_id,
            tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::OWN_PROVIDER.into(),
                    address: address.clone(),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(arguments),
                context: None,
                confirmation: Self::broker_confirmation(confirmation_evidence),
                revision: None,
                wait_ms: None,
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

    /// Retain exactly one broker request per admitted launcher invocation.
    /// The fixed cap is a failure boundary: overflowing it must refuse the
    /// invocation before it can be left Accepted without a terminal receipt.
    fn admit_application_launch(
        &mut self,
        request_id: u64,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(), tilo_actions::DoorError> {
        if self.actions.pending_application_launches.len() >= MAX_PENDING_APPLICATION_LAUNCHES {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "application_launch_queue_full".into(),
                reason: "Too many application launches are awaiting broker receipts".into(),
            });
        }
        self.actions
            .pending_application_launches
            .insert(request_id, invocation);
        Ok(())
    }

    fn invoke_document_resource_action(
        &mut self,
        invocation: &ResourceInvocation<'_>,
        resources: &[tilo_actions::Resource],
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        resource: &tilo_actions::Resource,
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
        if spec.deferred_invocation_type().is_some()
            && spec.locus == tilo_actions::ExecutionLocus::ServerPreferred
            && !broker_admitted
        {
            return self.invoke_via_broker(
                address,
                typed_arguments,
                argument,
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
                typed_arguments,
                argument,
                confirmation_evidence,
            );
        }
        // Only admitted local document execution owns a new change boundary.
        // Encoding failures and broker routing must leave the caller's pending
        // edit intact; the executor still receives the same committed baseline.
        let before = self.state.doc.get_heads();
        let route = self.primary_document_route();
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
        resources: &[tilo_actions::Resource],
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
        self.publish_action_surface();
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
            confirmation_evidence: evidence,
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
        let (arguments, _, spec) = crate::bound_dispatch::resolve_resource_invocation(
            &snapshot.resources,
            &target.address.address,
            argument,
            None,
            confirmed,
        )?;
        let kind = spec.kind;
        if !broker_admitted
            && Self::bound_invocation_needs_broker(
                kind,
                spec,
                invocation,
                broker_delivery.is_some(),
                self.collaboration.manager.is_connected(),
            )
        {
            return self.route_bound_invocation_to_broker(
                target,
                &snapshot.resources,
                arguments.as_ref(),
                argument,
                evidence,
                broker_delivery,
            );
        }
        let id = invocation.unwrap_or_default();
        let started = crate::bound_dispatch::invoke_resource_action(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            target,
            self.actions.action_transport.as_mut(),
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
            },
        )
        .map_err(crate::bound_dispatch::BoundActionError::into_door);
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

    fn invoke_resource_action(
        &mut self,
        request: ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let local = request.actor == self.collaboration.local_author.0;
        let started = (|| {
            use crate::bound_dispatch::legacy_document::{
                LocalResourceTarget, select_local_target,
            };
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
                Ok(LocalResourceTarget::Legacy(target)) => {
                    self.invoke_legacy_resource_action(&target, &request)
                }
                Ok(LocalResourceTarget::Outer) | Err(tilo_actions::DoorError::Unknown(_)) => {
                    self.invoke_unbound_resource_action(&request, None)
                }
                Err(error) => Err(error),
            }
        })()?;
        if local {
            self.take_over_owner_view();
        }
        Ok(started)
    }

    /// The legacy `target` names the action; `request.address` is not consulted.
    fn invoke_legacy_resource_action(
        &mut self,
        target: &crate::bound_dispatch::legacy_document::LegacyResourceTarget,
        request: &ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let ResourceInvocation {
            actor,
            argument,
            confirmation_satisfied,
            broker_admitted,
            invocation,
            ambient_context: ambient,
            scope,
            ..
        } = *request;
        use crate::bound_dispatch::legacy_document::{
            LegacyDocumentServices, LegacyInvocation, invoke_legacy_document_scoped,
            resolve_legacy_invocation,
        };
        let resources = resolve_legacy_invocation(self.root_widget.as_ref(), target)?;
        let (_, _, spec) = Self::resolve_resource_invocation(
            &resources,
            &target.address,
            argument,
            confirmation_satisfied,
        )?;
        if spec.kind != tilo_actions::OperationKind::Document {
            return self.invoke_unbound_resource_action(
                &ResourceInvocation {
                    address: &target.address,
                    ..*request
                },
                Some(target),
            );
        }
        let outcome = invoke_legacy_document_scoped(
            self.root_widget.as_mut(),
            target,
            LegacyDocumentServices {
                registry: &self.actions.action_registry,
                transport: self.actions.action_transport.as_mut(),
                qualified_local_transport: self.actions.legacy_local_transport,
                authority: &self.permission_ctx,
                author: &self.collaboration.local_author,
            },
            LegacyInvocation {
                argument,
                arguments: None,
                actor,
                broker_admitted: broker_admitted || invocation.is_some(),
                ambient,
            },
            scope,
        )?;
        tracing::trace!(action = %outcome.envelope.id.0, changed = outcome.changed, "legacy Document applied on captured owner");
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        self.invalidate_widget_resource_cache();
        Ok(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn invoke_unbound_resource_action(
        &mut self,
        request: &ResourceInvocation<'_>,
        legacy: Option<&crate::bound_dispatch::legacy_document::LegacyResourceTarget>,
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
        if scope.is_some() && legacy.is_none() {
            return Err(tilo_actions::DoorError::Failed(
                "required invocation has no actual canonical owner".into(),
            ));
        }
        // A legacy non-Document request carries its resolved owner from the
        // initial door. Keep that captured target through the final resolve so
        // a later tree change cannot redirect it to another same-address
        // resource. Ordinary local requests use the permission-projected,
        // cached resource surface (including retained bound and legacy rows).
        let mut resources = if let Some(target) = legacy {
            crate::bound_dispatch::legacy_document::resolve_legacy_invocation(
                self.root_widget.as_ref(),
                target,
            )?
        } else if let Some(participant) = crate::invocation_scope::current_participant() {
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
        // The legacy target above is an owner witness. If that owner no
        // longer offers the action, reject it rather than resolving a same
        // address from another live branch.
        if legacy.is_none() && tilo_actions::door::resolve(&resources, address).is_none() {
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
            match typed_arguments.as_ref() {
                Some(arguments) => resolved.permission_for_json(arguments),
                None => resolved.permission_for_argument(argument),
            },
        )?;
        // The local participant's tree offers an action under the least it
        // needs; this invocation needs what its value asks.
        if actor == self.collaboration.local_author.0
            && !crate::bound_dispatch::permits_local_value(
                &self.permission_ctx,
                &crate::collaboration::local_app_id(),
                &address.resource,
                resolved,
                typed_arguments.as_ref(),
                argument,
            )
        {
            return Err(crate::bound_dispatch::denied(resolved));
        }
        let kind = resolved.kind;
        if resolved
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY)
            && actor != self.collaboration.local_author.0
        {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "local_device_only".into(),
                reason: "This action uses private files on the host participant's device".into(),
            });
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
        if resolved.deferred_invocation_type().is_some() {
            if let Some(target) = legacy {
                crate::bound_dispatch::legacy_document::authorize_legacy_operation(
                    target,
                    &self.permission_ctx,
                    resolved,
                )?;
            }
        }
        let encoded =
            Self::encode_resource_action(&resources, address, typed_arguments.as_ref(), argument)?;
        // A launcher action has already passed the ordinary declaration,
        // argument-binding, confirmation, and broker-admission door above.
        // This branch runs only when the broker directs that admitted action
        // back to its source app, preserving the normal receipt lifecycle.
        if encoded.type_id == tilo_actions::remote::ApplicationLaunch::TYPE {
            let launch: tilo_actions::remote::ApplicationLaunch =
                serde_json::from_slice(&encoded.payload).map_err(|_| {
                    tilo_actions::DoorError::Failed("invalid application launch payload".into())
                })?;
            if !launch.valid() {
                return Err(tilo_actions::DoorError::Failed(
                    "application launch payload is out of bounds".into(),
                ));
            }
            if !self.collaboration.manager.is_connected() {
                return Err(tilo_actions::DoorError::Unavailable {
                    code: "authoritative_host_unavailable".into(),
                    reason: "Opening staged media requires the workspace action broker".into(),
                });
            }
            let Some(room) = self
                .collaboration
                .manager
                .primary_route()
                .map(|witness| witness.route.cluster_id.to_string())
            else {
                return Err(tilo_actions::DoorError::Unavailable {
                    code: "authoritative_host_unavailable".into(),
                    reason: "Opening staged media requires an admitted workspace route".into(),
                });
            };
            let request_id = self.take_broker_request_id()?;
            let Some(invocation) = invocation else {
                return Err(tilo_actions::DoorError::Failed(
                    "application launch was not admitted by the broker".into(),
                ));
            };
            self.admit_application_launch(request_id, invocation)?;
            let request = tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::application_open_target(&room, &launch.app_id),
                argument: launch.argument(),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
            };
            self.actions
                .broker_requests
                .insert(BrokerReply::Receipt(request.invocation), request_id);
            self.collaboration
                .manager
                .send(CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                });
            return Ok(crate::widget::ResourceOperationStart::Accepted);
        }
        let operation_invocation = invocation.unwrap_or_default();
        if encoded.type_id == history_actions::OPERATION {
            let result = self
                .invoke_history_action(&encoded, actor)
                .and_then(|output| {
                    resolved
                        .contract
                        .result
                        .validate(&output)
                        .map_err(tilo_actions::DoorError::Failed)?;
                    Ok(output)
                });
            if kind == tilo_actions::OperationKind::Effect && invocation.is_none() {
                let update = match &result {
                    Ok(output) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation: operation_invocation,
                        output: output.clone(),
                    },
                    Err(error) => tilo_actions::InvocationUpdate::Failed {
                        invocation: operation_invocation,
                        error: tilo_actions::ActionFailure {
                            code: crate::bound_dispatch::door_error_code(error).to_string(),
                            message: error.to_string(),
                            details: None,
                            retryable: false,
                        },
                    },
                };
                self.collaboration
                    .manager
                    .send(CollabRequest::RecordLocalEffect {
                        address: address.clone(),
                        update,
                    });
            }
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.invalidate_widget_resource_cache();
            return result.map(crate::widget::ResourceOperationStart::Completed);
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
                                    message: "the semantic target no longer accepts this action"
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
        let operation_result = if let Some(target) = legacy {
            (|| {
                crate::bound_dispatch::legacy_document::authorize_legacy_operation(
                    target,
                    &self.permission_ctx,
                    resolved,
                )?;
                let started = crate::bound_dispatch::legacy_document::invoke_legacy_effect_scoped(
                    self.root_widget.as_mut(),
                    target,
                    operation_invocation,
                    kind,
                    &encoded,
                    &mut self.state.doc,
                    scope,
                )?;
                match &started {
                    crate::widget::ResourceOperationStart::Accepted
                        if resolved.contract.execution
                            == tilo_actions::ExecutionMode::Immediate =>
                    {
                        return Err(tilo_actions::DoorError::Failed(
                            "An immediate operation cannot remain pending".into(),
                        ));
                    }
                    crate::widget::ResourceOperationStart::Completed(output) => resolved
                        .contract
                        .result
                        .validate(output)
                        .map_err(tilo_actions::DoorError::Failed)?,
                    crate::widget::ResourceOperationStart::Accepted => {}
                }
                Ok(started)
            })()
        } else {
            let route = self.primary_document_route();
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
    }

    fn dispatch_bound_control_invocation(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        intent: &tilo_actions::ControlInvocation,
        on_behalf_of: Option<tilo_actions::InvocationId>,
    ) -> bool {
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(intent.arguments.clone())
        );
        // An edit an effect submitted is applied for the effect's invoker,
        // in its context, as far as the invoker was admitted.
        let acting = on_behalf_of.and_then(|id| self.actions.acting_for.get(&id).cloned());
        // With the workspace's broker at hand it admits the edit for the
        // invoker, with their own grants, under the component's published
        // address.
        if let (Some(acting), Some(running)) = (&acting, on_behalf_of)
            && self.collaboration.manager.is_connected()
        {
            let result = self
                .actions
                .bound_publication
                .alias(&target.binding, &target.address.address.resource)
                .map_err(crate::bound_dispatch::BoundActionError::into_door)
                .and_then(|alias| {
                    self.send_follow_up(
                        running,
                        &tilo_actions::ActionAddress::new(
                            alias,
                            target.address.address.action.clone(),
                        ),
                        &intent.arguments,
                        acting.context.clone(),
                    )
                });
            self.note_continuation(on_behalf_of, &result);
            if let Err(error) = crate::bound_dispatch::notify_control_invocation_result(
                self.root_widget.as_mut(),
                target,
                &result,
                on_behalf_of,
            ) {
                tracing::warn!(?error, "bound control feedback unavailable");
            }
            return result.is_ok();
        }
        let actor = acting.as_ref().map_or_else(
            || self.collaboration.local_author.0.clone(),
            |acting| acting.invoker.actor.clone(),
        );
        let scopes = acting
            .as_ref()
            .map(crate::invocation_scope::Continuation::enter);
        let answering = crate::bound_dispatch::Answering::enter(on_behalf_of.is_some());
        let result = self.invoke_bound_target(
            target,
            &ResourceInvocation::new(&target.address.address, &actor)
                .argument(Some(&argument))
                .ambient_context(acting.as_ref().and_then(|acting| acting.context.as_ref())),
        );
        drop(answering);
        drop(scopes);
        // Nobody here asked for it: the person at the device is never
        // prompted to confirm another invoker's edit.
        let result = match result {
            Err(tilo_actions::DoorError::ConfirmationRequired) if acting.is_some() => {
                self.note_continuation(on_behalf_of, &result);
                if let Err(error) = crate::bound_dispatch::notify_control_invocation_result(
                    self.root_widget.as_mut(),
                    target,
                    &result,
                    on_behalf_of,
                ) {
                    tracing::warn!(?error, "bound control feedback unavailable");
                }
                return false;
            }
            result => result,
        };
        self.note_continuation(on_behalf_of, &result);
        if let Err(error) = crate::bound_dispatch::notify_control_invocation_result(
            self.root_widget.as_mut(),
            target,
            &result,
            on_behalf_of,
        ) {
            tracing::warn!(?error, "bound control feedback unavailable");
        }
        match result {
            Ok(_) => true,
            Err(tilo_actions::DoorError::ConfirmationRequired) => {
                self.open_ordinary_confirmation(
                    crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(
                        target.clone(),
                    ),
                    intent.address.clone(),
                    crate::bound_dispatch::ControlContinuationArguments::Json(
                        intent.arguments.clone(),
                    ),
                );
                true
            }
            Err(error) => {
                tracing::warn!(%error, "bound control invocation refused");
                false
            }
        }
    }

    fn service_local_control_history(&mut self) -> bool {
        let controls = self.dispatch_control_invocations();
        let history = self.service_history();
        let changed = controls != 0 || history;
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

    fn service_pending_history_commands(&mut self) {
        let commands = self
            .history
            .as_ref()
            .map_or_else(Vec::new, |history| history.drain_pending_control_commands());
        let embedded =
            crate::history_mode::embedded::drain_pending_commands(self.root_widget.as_mut());
        if !commands.is_empty() || !embedded.is_empty() {
            if self.history_focus.is_none() {
                self.history_focus = unsafe {
                    crate::widget::layout::CompositionFocus::capture(
                        self.focused_node,
                        &self.layout.node_map,
                        &self.bindings,
                    )
                };
            }
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            self.layout.structure_dirty = true;
            self.layout.layout_dirty = true;
        }
        for command in commands {
            let (token, confirmation) = match command {
                crate::history_mode::pending_controls::ContinuationCommand::Review(token) => {
                    (token, None)
                }
                crate::history_mode::pending_controls::ContinuationCommand::Confirm(
                    token,
                    value,
                ) => (token, Some(value)),
                crate::history_mode::pending_controls::ContinuationCommand::Cancel(token) => {
                    if let Some(history) = &mut self.history {
                        history.cancel_pending_control(&token);
                    }
                    continue;
                }
            };
            let Some(prepared) =
                self.history
                    .as_mut()
                    .and_then(|history| match confirmation.as_deref() {
                        Some(value) => history.prepare_pending_control_typed(&token, value),
                        None => history.prepare_pending_control(&token, false),
                    })
            else {
                continue;
            };
            let confirmed = confirmation.is_some();
            let id = prepared.id;
            let result = self.dispatch_prepared_history_control(prepared);
            let Some(history) = self.history.as_mut() else {
                continue;
            };
            match result {
                Err(tilo_actions::DoorError::ConfirmationRequired) if !confirmed => {
                    history.pending_control_needs_confirmation(id);
                }
                Ok(_) => history.pending_control_consumed(id),
                Err(error) => history.pending_control_refused(id, error.to_string()),
            }
        }
        for pending in embedded {
            let (token, confirmation) = match pending.command {
                crate::history_mode::pending_controls::ContinuationCommand::Review(token) => {
                    (token, None)
                }
                crate::history_mode::pending_controls::ContinuationCommand::Confirm(
                    token,
                    value,
                ) => (token, Some(value)),
                crate::history_mode::pending_controls::ContinuationCommand::Cancel(token) => {
                    crate::history_mode::embedded::cancel_pending_command(
                        self.root_widget.as_mut(),
                        pending.surface,
                        &token,
                    );
                    continue;
                }
            };
            let prepared = match confirmation.as_deref() {
                Some(value) => crate::history_mode::embedded::prepare_pending_command_typed(
                    self.root_widget.as_mut(),
                    pending.surface,
                    &token,
                    value,
                ),
                None => crate::history_mode::embedded::prepare_pending_command(
                    self.root_widget.as_mut(),
                    pending.surface,
                    &token,
                    false,
                ),
            };
            let Some(prepared) = prepared else {
                continue;
            };
            let confirmed = confirmation.is_some();
            let id = prepared.id;
            let result = self.dispatch_prepared_history_control(prepared);
            let outcome = match result {
                Err(tilo_actions::DoorError::ConfirmationRequired) if !confirmed => {
                    crate::history_mode::embedded::PendingCommandOutcome::NeedsConfirmation
                }
                Ok(_) => crate::history_mode::embedded::PendingCommandOutcome::Consumed,
                Err(error) => {
                    crate::history_mode::embedded::PendingCommandOutcome::Refused(error.to_string())
                }
            };
            crate::history_mode::embedded::finish_pending_command(
                self.root_widget.as_mut(),
                pending.surface,
                id,
                &outcome,
            );
        }
    }

    fn dispatch_prepared_history_control(
        &mut self,
        prepared: crate::history_mode::pending_controls::PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        use crate::bound_dispatch::legacy_document::LocalResourceTarget;
        let captured =
            prepared
                .witness
                .as_ref()
                .map_err(|reason| tilo_actions::DoorError::Unavailable {
                    code: "stale_pending_control".into(),
                    reason: reason.clone(),
                })?;
        let current = crate::bound_dispatch::control_witness(
            self.root_widget.as_ref(),
            &self.state.doc,
            &prepared.target,
            &prepared.invocation,
        )?;
        if &current != captured {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "stale_pending_control".into(),
                reason: "The action meaning or confirmation policy changed while History was open"
                    .into(),
            });
        }
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(prepared.invocation.arguments.clone())
        );
        let actor = self.collaboration.local_author.0.clone();
        match prepared.target {
            LocalResourceTarget::Component(target) => self.invoke_bound_target_with_broker(
                &target,
                &ResourceInvocation::new(&target.address.address, &actor)
                    .argument(Some(&argument))
                    .confirmed(prepared.confirmed),
                None,
            ),
            LocalResourceTarget::Legacy(target) => self.invoke_legacy_resource_action(
                &target,
                &ResourceInvocation::new(&target.address, &actor)
                    .argument(Some(&argument))
                    .confirmed(prepared.confirmed),
            ),
            LocalResourceTarget::Outer => self.invoke_unbound_resource_action(
                &ResourceInvocation::new(&prepared.invocation.address, &actor)
                    .argument(Some(&argument))
                    .confirmed(prepared.confirmed),
                None,
            ),
        }
    }

    /// Drives the production local idle body without creating a window or input.
    #[cfg(any(test, feature = "test-support"))]
    pub fn controlled_local_idle_for_test(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        self.service_local_control_history()
    }

    fn dispatch_legacy_control_invocations(
        &mut self,
        registered: Vec<(
            crate::bound_dispatch::legacy_document::LegacyResourceTarget,
            tilo_actions::ControlInvocation,
        )>,
    ) -> usize {
        let mut dispatched = 0usize;
        let batch = crate::bound_dispatch::legacy_document::drain_legacy_controls(
            self.root_widget.as_mut(),
        );
        for error in batch.failures {
            tracing::warn!(%error, "legacy control drain refused");
        }
        for (target, intent) in registered.into_iter().chain(batch.controls) {
            if self.take_history_invocation(&intent, Some(target.owner.clone())) {
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments.clone())
            );
            let actor = self.collaboration.local_author.0.clone();
            let result = self.invoke_legacy_resource_action(
                &target,
                &ResourceInvocation::new(&target.address, &actor).argument(Some(&argument)),
            );
            if let Err(error) =
                crate::bound_dispatch::legacy_document::notify_legacy_control_invocation_result(
                    self.root_widget.as_mut(),
                    &target,
                    &result,
                )
            {
                tracing::warn!(%error, "legacy control feedback unavailable");
            }
            match result {
                Ok(_) => dispatched = dispatched.saturating_add(1),
                Err(tilo_actions::DoorError::ConfirmationRequired)
                    if crate::bound_dispatch::legacy_document::resolve_legacy_invocation(
                        self.root_widget.as_ref(),
                        &target,
                    )
                    .ok()
                    .and_then(|resources| {
                        tilo_actions::door::resolve(&resources, &target.address)
                            .map(|spec| spec.kind != tilo_actions::OperationKind::Document)
                    })
                    .unwrap_or(false) =>
                {
                    // Existing Effect/View confirmation remains the palette's job.
                    // Its bare address is rechecked; ambiguity never picks an owner.
                    self.open_ordinary_confirmation(
                        crate::bound_dispatch::legacy_document::LocalResourceTarget::Legacy(target),
                        intent.address,
                        crate::bound_dispatch::ControlContinuationArguments::Json(intent.arguments),
                    );
                    dispatched = dispatched.saturating_add(1);
                }
                Err(error) => tracing::warn!(%error, "legacy control invocation refused"),
            }
        }
        dispatched
    }

    /// Resolve queued local control intents against the current resource tree
    /// and enter through the same door/lifecycle as every other caller.
    pub(super) fn dispatch_control_invocations(&mut self) -> usize {
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
        let mut registered = self.control_domain.drain_partitioned();
        let mut dispatched = self.dispatch_history_controls();
        let bound = crate::bound_dispatch::drain_control_invocations(self.root_widget.as_mut());
        if let Some(error) = bound.error {
            tracing::warn!(
                ?error,
                "control traversal incomplete; delivering already drained intents"
            );
        }
        for (target, intent, on_behalf_of) in registered
            .components
            .into_iter()
            .map(|(target, intent)| (target, intent, None))
            .chain(bound.pending)
        {
            if on_behalf_of.is_none()
                && target.binding.enter().is_ok()
                && self.take_search_invocation(&intent, Some(&target.binding))
            {
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            if self.dispatch_bound_control_invocation(&target, &intent, on_behalf_of) {
                dispatched = dispatched.saturating_add(1);
            }
        }
        dispatched =
            dispatched.saturating_add(self.dispatch_legacy_control_invocations(registered.legacy));
        while let Some((intent, on_behalf_of)) = registered
            .outer
            .pop_front()
            .map(|intent| (intent, None))
            .or_else(|| {
                crate::widget::controls::take_attributed_control_invocation(
                    self.root_widget.as_mut(),
                )
            })
        {
            if on_behalf_of.is_none() && self.take_history_invocation(&intent, None) {
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            if on_behalf_of.is_none() && self.take_search_invocation(&intent, None) {
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            // An edit an effect submitted is applied for the effect's
            // invoker, in its context, as long as the effect runs.
            let acting = on_behalf_of.and_then(|id| self.actions.acting_for.get(&id).cloned());
            let (key, generation) = self.ensure_local_resource_cache();
            let local_resources = self
                .actions
                .local_resource_cache
                .borrow()
                .as_ref()
                .filter(|cache| cache.key == key && cache.permission_generation == generation)
                .and_then(|cache| cache.resources.clone())
                .unwrap_or_default();
            let Some(resolved) = tilo_actions::door::resolve(&local_resources, &intent.address)
            else {
                tracing::warn!(
                    resource = %intent.address.resource,
                    action = %intent.address.action,
                    "action-bound control is unavailable to the local actor"
                );
                let refused = Err(tilo_actions::DoorError::Unknown(intent.address.clone()));
                self.note_continuation(on_behalf_of, &refused);
                crate::widget::controls::on_control_invocation_result(
                    self.root_widget.as_mut(),
                    &intent.address,
                    &refused,
                );
                continue;
            };
            if acting.is_some()
                && !matches!(
                    resolved.confirmation_for_json(&intent.arguments),
                    tilo_actions::ConfirmationPolicy::None
                )
            {
                // Nobody here asked for it: the person at the device is
                // never prompted to confirm another invoker's edit.
                tracing::warn!(resource = %intent.address.resource, action = %intent.address.action, "an effect's edit that needs confirmation is refused");
                let refused = Err(tilo_actions::DoorError::ConfirmationRequired);
                self.note_continuation(on_behalf_of, &refused);
                crate::widget::controls::on_control_invocation_result(
                    self.root_widget.as_mut(),
                    &intent.address,
                    &refused,
                );
                continue;
            }
            if !matches!(
                resolved.confirmation_for_json(&intent.arguments),
                tilo_actions::ConfirmationPolicy::None
            ) {
                // A bound control never manufactures confirmation. Hand the
                // exact address to the standard palette flow, which owns the
                // explicit confirm/phrase UI and only then enters the door.
                crate::widget::controls::on_control_invocation_result(
                    self.root_widget.as_mut(),
                    &intent.address,
                    &Err(tilo_actions::DoorError::ConfirmationRequired),
                );
                self.open_ordinary_confirmation(
                    crate::bound_dispatch::legacy_document::LocalResourceTarget::Outer,
                    intent.address,
                    crate::bound_dispatch::ControlContinuationArguments::Json(intent.arguments),
                );
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            // With the workspace's broker at hand it admits the edit for the
            // invoker, with their own grants.
            if let (Some(acting), Some(running)) = (&acting, on_behalf_of)
                && self.collaboration.manager.is_connected()
            {
                let result = self.send_follow_up(
                    running,
                    &intent.address,
                    &intent.arguments,
                    acting.context.clone(),
                );
                self.note_continuation(on_behalf_of, &result);
                crate::widget::controls::on_control_invocation_result(
                    self.root_widget.as_mut(),
                    &intent.address,
                    &result,
                );
                if result.is_ok() {
                    dispatched = dispatched.saturating_add(1);
                }
                continue;
            }
            let json = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments.clone())
            );
            let actor = acting.as_ref().map_or_else(
                || self.collaboration.local_author.0.clone(),
                |acting| acting.invoker.actor.clone(),
            );
            let scopes = acting
                .as_ref()
                .map(crate::invocation_scope::Continuation::enter);
            // An outer control was produced outside every component and
            // legacy owner: it acts on the outer document it was captured
            // with, even when an owner declares the same address.
            let result = self.invoke_unbound_resource_action(
                &ResourceInvocation::new(&intent.address, &actor)
                    .argument(Some(&json))
                    .ambient_context(acting.as_ref().and_then(|acting| acting.context.as_ref())),
                None,
            );
            drop(scopes);
            self.note_continuation(on_behalf_of, &result);
            crate::widget::controls::on_control_invocation_result(
                self.root_widget.as_mut(),
                &intent.address,
                &result,
            );
            match result {
                Ok(_) => dispatched = dispatched.saturating_add(1),
                Err(error) => {
                    tracing::warn!(resource = %intent.address.resource, action = %intent.address.action, %error, "action-bound control invocation refused");
                }
            }
        }
        dispatched
    }

    /// Ask the broker to admit an edit the effect invocation `running`
    /// submitted for its invoker: it decides with the invoker's grants and,
    /// when they allow it, hands it back here as theirs.
    fn send_follow_up(
        &mut self,
        running: tilo_actions::InvocationId,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        context: Option<tilo_actions::ActionContext>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let request_id = self.take_broker_request_id()?;
        let request = tilo_actions::remote::InvokeRequest {
            on_behalf_of: Some(running),
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::OWN_PROVIDER.into(),
                address: address.clone(),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
            context,
            confirmation: None,
            revision: None,
            wait_ms: None,
        };
        self.actions
            .broker_requests
            .insert(BrokerReply::Receipt(request.invocation), request_id);
        self.actions.follow_ups.insert(request_id, running);
        if let Some(continuation) = self.actions.acting_for.get_mut(&running) {
            continuation.outstanding.insert(request_id);
        }
        self.collaboration
            .manager
            .send(CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
            });
        Ok(crate::widget::ResourceOperationStart::Accepted)
    }

    /// The broker answered the edit request `request_id` an effect
    /// submitted: a refusal fails the effect, and the effect's held answer
    /// goes out once its last edit is settled. Whether it was one.
    pub(super) fn settle_follow_up(
        &mut self,
        request_id: u64,
        result: &Result<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate), String>,
    ) -> bool {
        let Some(running) = self.actions.follow_ups.remove(&request_id) else {
            return false;
        };
        let refusal = match result {
            Err(reason) => Some(tilo_actions::ActionFailure {
                code: "permission_denied".into(),
                message: reason.clone(),
                details: None,
                retryable: false,
            }),
            Ok((_, tilo_actions::InvocationUpdate::Failed { error, .. })) => Some(error.clone()),
            Ok((_, tilo_actions::InvocationUpdate::Cancelled { .. })) => {
                Some(tilo_actions::ActionFailure {
                    code: "follow_up_cancelled".into(),
                    message: "an edit the effect submitted was cancelled".into(),
                    details: None,
                    retryable: false,
                })
            }
            Ok(_) => None,
        };
        let Some(continuation) = self.actions.acting_for.get_mut(&running) else {
            return true;
        };
        continuation.outstanding.remove(&request_id);
        if continuation.refused.is_none() {
            continuation.refused = refusal;
        }
        if !continuation.outstanding.is_empty() || continuation.deferred.is_none() {
            return true;
        }
        let Some(continuation) = self.actions.acting_for.remove(&running) else {
            return true;
        };
        let Some((wire_id, update)) = continuation.deferred.clone() else {
            return true;
        };
        let update = continuation.settle(update);
        if self.queue_documents_before_terminal() {
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation {
                    id: wire_id,
                    update,
                });
        }
        true
    }

    /// Keep why an edit the running effect `on_behalf_of` submitted was
    /// refused: the effect then fails rather than claim what it did not do.
    fn note_continuation(
        &mut self,
        on_behalf_of: Option<tilo_actions::InvocationId>,
        result: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        let Err(error) = result else {
            return;
        };
        if let Some(continuation) = on_behalf_of.and_then(|id| self.actions.acting_for.get_mut(&id))
            && continuation.refused.is_none()
        {
            continuation.refused = Some(crate::invocation_scope::Continuation::failure(error));
        }
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
                Ok(self.root_widget.poll_resource_operation(
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
            }
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation {
                    id: pending.wire_id,
                    update,
                });
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
                Ok(self.root_widget.poll_resource_operation(
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
        history_actions::without_duplicate_launcher(&mut resources);
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
        let cached_open_resources = if self.history_chrome_enabled {
            None
        } else {
            cached_open_resources
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
        let legacy_resources = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root_widget.as_ref(),
            &self.permission_ctx,
            &self.collaboration.local_author,
            false,
        )
        .unwrap_or_default();
        let legacy_open_resources = crate::search::OpenResourceSearch::new(&legacy_resources);
        let mut resources = resources;
        resources.extend(legacy_resources);
        let bound = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        let mut providers = crate::bound_dispatch::search_providers(self.root_widget.as_ref());
        providers.push(Box::new(open_resources));
        providers.push(Box::new(legacy_open_resources));
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
        // Discovery reads the still-admitted current tree; clear its pointers
        // immediately before replacing the retained modal instance.
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        self.actions.action_palette = Some(palette);
        self.rebuild_action_palette_admission();
    }

    /// Open search the way `request` asks: this app, or one part of it, with
    /// the query already typed. A standalone window shows its own palette;
    /// inside the Tilo shell the component hands the request to the shell.
    pub fn open_search(&mut self, request: crate::search::SearchRequest) {
        self.open_action_palette();
        if let Some(palette) = self.actions.action_palette.as_mut() {
            palette.set_scope(request.resource_scope().cloned());
            if let Some(query) = &request.query {
                palette.set_query(query);
                palette.select_query();
            }
        }
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

    /// Whether this window presents search itself. A shell hosting the app
    /// presents it instead, and receives the request as a host intent.
    fn presents_search(&self) -> bool {
        self.document_history.has_search()
    }

    /// A control in the app's tree bound to the window's History door (the
    /// History control in the app's header row) runs there, as the History
    /// shortcut does, never as an app action. An app `owner` hosted in a tile
    /// opens that tile's History. Returns whether it was one.
    fn take_history_invocation(
        &mut self,
        intent: &tilo_actions::ControlInvocation,
        owner: Option<crate::history_mode::HistoryOwner>,
    ) -> bool {
        if intent.address.resource != crate::standalone_history_host::HISTORY_RESOURCE {
            return false;
        }
        let embedded = owner.and_then(|owner| {
            crate::history_mode::embedded::invoke_door(self.root_widget.as_mut(), &owner, intent)
        });
        let result = if let Some(result) = embedded {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            result.map(|()| {
                crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )
            })
        } else {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments.clone())
            );
            self.invoke_history_address_local(&intent.address, Some(&argument))
                .unwrap_or_else(|| Err(tilo_actions::DoorError::Unknown(intent.address.clone())))
        };
        crate::widget::controls::on_control_invocation_result(
            self.root_widget.as_mut(),
            &intent.address,
            &result,
        );
        true
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

    fn rebuild_action_palette_admission(&mut self) {
        // A synchronous action may need the current canonical widget map
        // before the next redraw. Retire the old tree through the normal
        // rebuild lifecycle; never append another host tree with init_layout.
        let pending_sync = self.needs_sync;
        let generation = self.actions.resource_generation.get();
        let open_index = self.actions.open_resource_search_cache.get_mut().take();
        self.layout.structure_dirty = true;
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
        self.layout.structure_dirty = true;
    }

    fn open_ordinary_confirmation(
        &mut self,
        target: crate::bound_dispatch::legacy_document::LocalResourceTarget,
        address: tilo_actions::ActionAddress,
        arguments: crate::bound_dispatch::ControlContinuationArguments,
    ) -> bool {
        let request = crate::bound_dispatch::OrdinaryConfirmation::prepare(
            self.root_widget.as_ref(),
            &self.state.doc,
            target,
            &self.control_domain,
            address,
            arguments,
        );
        let Ok(request) = request else {
            return false;
        };
        if self.actions.action_palette.is_none() {
            // Native semantic focus is authoritative even before the next
            // retained render propagates the widget's focused flag.
            // SAFETY: modal admission has not retired either map; both still
            // describe this exclusively held, live background widget tree.
            self.actions.ordinary_palette_return_focus = unsafe {
                crate::widget::layout::CompositionFocus::capture(
                    self.focused_node,
                    &self.layout.node_map,
                    &self.bindings,
                )
            }
            .or_else(|| {
                crate::widget::layout::CompositionFocus::capture_live(self.root_widget.as_ref())
            });
        }
        self.open_action_palette();
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        if let Some(palette) = self.actions.action_palette.as_mut() {
            palette.activate_ordinary(request);
        }
        self.rebuild_action_palette_admission();
        true
    }

    fn capture_ordinary_palette_request(&mut self) {
        let Some(pending) = self
            .actions
            .action_palette
            .as_ref()
            .and_then(|p| p.ordinary_capture_request())
        else {
            return;
        };
        let result = (|| {
            let target = match pending.bound.clone() {
                Some(target) => {
                    crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(target)
                }
                None => crate::bound_dispatch::legacy_document::select_local_target(
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
        if let Some(palette) = self.actions.action_palette.as_mut() {
            match result {
                Ok(request) => palette.retain_ordinary(request),
                Err(_) => palette.close(),
            }
        }
    }

    /// Open the palette an app asked for ([`crate::host_palette`]): searching
    /// for its query, or on one of its actions. Whether one was opened.
    #[cfg(feature = "native")]
    pub(crate) fn open_requested_palette(&mut self) -> bool {
        match crate::host_palette::take() {
            Some(crate::host_palette::PaletteRequest::Search(query)) => {
                self.open_action_palette();
                if let Some(palette) = self.actions.action_palette.as_mut() {
                    palette.set_query(&query);
                }
                true
            }
            Some(crate::host_palette::PaletteRequest::Action(address)) => {
                self.open_action_palette_at(&address)
            }
            None => false,
        }
    }

    fn open_action_palette_at(&mut self, address: &tilo_actions::ActionAddress) -> bool {
        self.open_action_palette();
        let activated =
            self.actions.action_palette.as_mut().is_some_and(|palette| {
                palette.activate_address(&address.resource, &address.action)
            });
        if activated {
            self.capture_ordinary_palette_request();
        } else {
            self.close_action_palette();
        }
        activated
    }

    fn filter_resource_for_local_actor(
        &self,
        mut resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        let resource_id = resource.id.as_str().to_owned();
        resource.actions.retain(|action| {
            crate::bound_dispatch::permits_local_action(
                &self.permission_ctx,
                &crate::collaboration::local_app_id(),
                &resource_id,
                action,
            )
        });
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.filter_resource_for_local_actor(child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }

    pub fn close_action_palette(&mut self) {
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        self.layout.structure_dirty = true;
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

    pub fn action_palette(&self) -> Option<&crate::action_palette::ActionPalette> {
        self.actions.action_palette.as_ref()
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
        history_actions::without_duplicate_launcher(&mut resources);
        if let Ok(legacy) = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root_widget.as_ref(),
            &self.permission_ctx,
            &self.collaboration.local_author,
            false,
        ) {
            resources.extend(legacy);
        }
        let lazy_resource_ids = self
            .actions
            .action_palette
            .as_ref()
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
            .as_ref()
            .map(crate::action_palette::ActionPalette::referenced_bound_targets)
            .unwrap_or_default();
        for target in targets {
            if let Some(snapshot) = bound
                .iter_mut()
                .find(|snapshot| snapshot.binding.same_component(&target.binding))
                && tilo_actions::door::resolve(&snapshot.resources, &target.address.address)
                    .is_none()
                && let Ok(Some(resource)) = crate::bound_dispatch::resolve_component_resource(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    &target.binding,
                    &target.address.address.resource,
                    &target.address.address.action,
                )
            {
                snapshot.resources.push(resource);
            }
        }
        if let Some(palette) = self.actions.action_palette.as_mut() {
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
            .as_ref()
            .is_some_and(|palette| palette.is_presented())
        {
            return false;
        }
        self.refresh_action_palette_resources();
        self.bindings.clear();
        self.layout.node_map.clear();
        self.focused_node = None;
        let consumed = self.actions.action_palette.as_mut().is_some_and(|palette| {
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
        let Some(palette) = self.actions.action_palette.as_mut() else {
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
        self.layout.structure_dirty = true;
        let root = self.state.root.clone();
        self.capture_ordinary_palette_request();
        let exact_capture = self
            .actions
            .action_palette
            .as_ref()
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
            if let Some(palette) = self.actions.action_palette.as_mut() {
                palette.retain_history_exact_witness(witness);
            }
        }
        // An operation row may have fired. Document mutations go through the
        // deterministic executor; effects/views route to the owning widget.
        let pending = self
            .actions
            .action_palette
            .as_mut()
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
                        crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(target) => self.invoke_bound_target_with_broker(&target, &palette_request, None),
                        crate::bound_dispatch::legacy_document::LocalResourceTarget::Legacy(target) => self.invoke_legacy_resource_action(&target, &palette_request),
                        crate::bound_dispatch::legacy_document::LocalResourceTarget::Outer => self.invoke_unbound_resource_action(&palette_request, None),
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
                    crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(
                        target,
                    ) => self.invoke_bound_target_with_broker(target, &palette_request, None),
                    crate::bound_dispatch::legacy_document::LocalResourceTarget::Legacy(target) => {
                        self.invoke_legacy_resource_action(target, &palette_request)
                    }
                    crate::bound_dispatch::legacy_document::LocalResourceTarget::Outer => {
                        self.invoke_unbound_resource_action(&palette_request, None)
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
                if pending.history_target.is_some()
                    && let Some(palette) = self.actions.action_palette.as_mut()
                {
                    palette.retain_history_refusal(pending, error.to_string());
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
            .as_mut()
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
            .as_ref()
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
const DIRECTORY_PAGE_WIRE_BYTES: usize = 512;

/// Every `(resource, action)` in `resource`'s tree, to `max_depth` levels
/// below it, whose action id or label, description, parameter names and
/// descriptions or resource label contains `needle` (already lowercase). Each matched resource is kept without its
/// children.
fn collect_named_actions(
    resource: &tilo_actions::Resource,
    needle: &str,
    depth: u16,
    max_depth: u16,
    out: &mut Vec<(tilo_actions::Resource, String)>,
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
            let shallow = shallow.get_or_insert_with(|| tilo_actions::Resource {
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
