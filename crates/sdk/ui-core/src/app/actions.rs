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

    #[cfg(feature = "native")]
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
}

impl App {
    /// Lets the hosted app register its action types, e.g.
    /// `app.action_registry_mut().register::<TubeAction>("tube.action")`.
    pub fn action_registry_mut(&mut self) -> &mut tilo_actions::ActionRegistry {
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
        semantics.refresh_bound(
            self.root_widget.as_ref(),
            &self.layout.node_map,
            self.focused_node,
            &[],
        );
        complete.extend(semantics.action_resources());
        trace.finish(CacheResult::NotApplicable);
        let trace = self.trace_resource_stage(Stage::SurfaceValidation);
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
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

    fn collect_declared_resources(&self) -> Vec<tilo_actions::Resource> {
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
        resources
    }

    fn refresh_control_action_availability(&mut self) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let trace = self.trace_resource_stage(Stage::BoundDiscovery);
        self.refresh_history_controls();
        let snapshots = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        trace.finish(CacheResult::NotObserved);
        let trace = self.trace_resource_stage(Stage::BoundControlProjection);
        let _ =
            crate::bound_dispatch::refresh_bound_controls(self.root_widget.as_mut(), &snapshots);
        trace.finish(CacheResult::NotApplicable);
        // Only ActionEmitter controls consume this address projection. Semantic,
        // document and legacy controls still use their normal invocation doors.
        // Recheck presence: widget-local changes can add/remove emitters.
        if !self.root_widget.has_action_control_bindings() {
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
        let allowed = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .map_or_else(std::collections::HashSet::new, |cache| {
                cache.allowed.clone()
            });
        self.root_widget
            .refresh_control_action_availability(&allowed);
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
        let mut routes = Vec::new();
        let bindings = self
            .with_native_presentation(|root, _, _, _| root.control_action_bindings())
            .unwrap_or_default();
        for binding in bindings {
            for path in self
                .accessibility
                .semantic_tree
                .control_paths(&binding.control_id)
            {
                let Some(semantic_id) = path.last() else {
                    continue;
                };
                // Match the canonical ResourceId emitted by SemanticTree.
                // Semantic node ids contain punctuation and address delimiters.
                let encoded_node_id = percent_encoding::utf8_percent_encode(
                    semantic_id.as_str(),
                    percent_encoding::NON_ALPHANUMERIC,
                );
                let semantic_address = address.resource == format!("ui:{encoded_node_id}")
                    && self
                        .accessibility
                        .semantic_tree
                        .action(semantic_id, &address.action)
                        .is_some();
                let matches_address = semantic_address
                    || matches!(
                        &binding.kind,
                        crate::widget::ControlBindingKind::Action(candidate) if candidate == address
                    );
                if !matches_address {
                    continue;
                }
                let Some(node) = self.accessibility.semantic_tree.node(semantic_id) else {
                    continue;
                };
                routes.push(ActionUiRoute {
                    address: address.clone(),
                    control_id: binding.control_id.clone(),
                    path,
                    bounds: node.bounds,
                    focused: node.state.focused,
                    disabled: node.state.disabled,
                });
            }
        }
        routes.sort_by(|left, right| {
            left.disabled
                .cmp(&right.disabled)
                .then_with(|| left.bounds.is_none().cmp(&right.bounds.is_none()))
                .then_with(|| left.path.len().cmp(&right.path.len()))
                .then_with(|| right.focused.cmp(&left.focused))
                .then_with(|| left.control_id.cmp(&right.control_id))
        });
        routes.dedup_by(|left, right| left.path.last() == right.path.last());
        routes
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
        let mut seen = std::collections::HashSet::new();
        self.root_widget
            .control_action_bindings()
            .into_iter()
            .map(|binding| {
                let status = match &binding.kind {
                    crate::widget::ControlBindingKind::Action(address) => {
                        if tilo_actions::door::resolve(resources, address).is_none() {
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
            if !crate::bound_dispatch::validate_bound_resources(
                self.root_widget.as_mut(),
                &component.binding,
                &component.resources,
            )
            .unwrap_or(false)
            {
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
        resources
    }

    /// Publish the live resource/action tree through the session broker.
    /// Level-triggered at the wire, diffed here so an unchanged frame emits
    /// nothing; publishing an empty list explicitly withdraws a closed tree.
    fn publish_action_surface(&mut self) {
        #[cfg(test)]
        {
            self.actions.action_surface_publication_attempts = self
                .actions
                .action_surface_publication_attempts
                .saturating_add(1);
        }
        if !self.collaboration.manager.is_connected() {
            return;
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
        let binding_coverage = self.control_action_coverage_for(&self.collect_resources());
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
                tracing::error!(
                    control = %entry.binding.control_id,
                    binding = ?entry.binding.kind,
                    "refusing to publish a surface with a stale control action binding"
                );
            }
            if self
                .actions
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty())
            {
                self.collaboration
                    .manager
                    .send(CollabRequest::PublishActions {
                        app_id: publication_app_id,
                        actions: Vec::new(),
                    });
                self.actions.last_published_actions = Some(Vec::new());
            }
            return;
        }
        let mut operation_handlers = self.root_widget.resource_operation_handlers();
        operation_handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
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
                tracing::error!(
                    address = %violation.address,
                    reason = %violation.reason,
                    "refusing to publish an invalid resource/action surface"
                );
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
            self.collaboration
                .manager
                .send(CollabRequest::PublishActions {
                    app_id: crate::collaboration::local_app_id(),
                    actions: Vec::new(),
                });
            self.actions.last_published_actions = Some(Vec::new());
            return;
        }
        let descriptors = tilo_actions::door::survey(&resources);
        self.capture_action_directory_snapshot(&resources, &descriptors);
        if self.actions.last_published_actions.as_ref() == Some(&descriptors) {
            return;
        }
        let actions = descriptors.iter().map(Self::exported_action).collect();
        self.collaboration
            .manager
            .send(CollabRequest::PublishActions {
                app_id: publication_app_id,
                actions,
            });
        self.actions.last_published_actions = Some(descriptors);
    }

    fn exported_action(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> crate::collaboration::ExportedAction {
        crate::collaboration::ExportedAction {
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            action_label: descriptor.action_label.clone(),
            description: descriptor.description.clone(),
            operation_kind: descriptor.kind.as_str().to_string(),
            params_json: serde_json::to_string(&descriptor.params).unwrap_or_default(),
            contract_json: serde_json::to_string(&descriptor.contract).unwrap_or_default(),
            input: match &descriptor.input {
                tilo_actions::InputSpec::None => crate::collaboration::ExportedInput::None,
                tilo_actions::InputSpec::Text {
                    prompt,
                    placeholder,
                    collects,
                } => crate::collaboration::ExportedInput::Text {
                    prompt: prompt.clone(),
                    placeholder: placeholder.clone(),
                    wants_content: *collects == tilo_actions::Collects::Content,
                },
            },
        }
    }

    fn action_directory_revision(&mut self) -> String {
        let owners =
            crate::bound_dispatch::directory_witness(self.root_widget.as_ref(), &self.state.doc);
        let reusable = owners
            .as_ref()
            .is_ok_and(|owners| owners.iter().all(|owner| owner.generation.is_some()));
        let witness = action_directory_snapshot::DirectoryWitness {
            key: self.resource_cache_key(),
            permissions: self.permission_ctx.clone(),
            permission_generation: self.actions.permission_generation.get(),
            route: self.bound_broker_route().cloned(),
            owners: owners.unwrap_or_default(),
            root_revision: self.root_widget.action_directory_revision(&self.state.doc),
            providers: crate::bound_dispatch::search_providers(self.root_widget.as_ref())
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
        if reusable
            && let Some((old, revision)) = &self.actions.action_directory_stamp
            && old == &witness
        {
            return revision.clone();
        }
        // Generations are updated at mutation boundaries; reading Automerge
        // heads here would commit an open transaction and split an undo unit.
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_cursors.clear();
        let revision = format!("v5:{}", uuid::Uuid::new_v4());
        self.actions.action_directory_stamp = Some((witness, revision.clone()));
        revision
    }

    fn exported_directory_action(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> runtime_collab::ExportedAction {
        runtime_collab::ExportedAction {
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            action_label: descriptor.action_label.clone(),
            description: descriptor.description.clone(),
            operation_kind: descriptor.kind.as_str().to_string(),
            params_json: serde_json::to_string(&descriptor.params).unwrap_or_default(),
            contract_json: serde_json::to_string(&descriptor.contract).unwrap_or_default(),
            input: match &descriptor.input {
                tilo_actions::InputSpec::None => runtime_collab::ExportedInput::None,
                tilo_actions::InputSpec::Text {
                    prompt,
                    placeholder,
                    collects,
                } => runtime_collab::ExportedInput::Text {
                    prompt: prompt.clone(),
                    placeholder: placeholder.clone(),
                    wants_content: *collects == tilo_actions::Collects::Content,
                },
            },
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
            resource_presentation_json: resource
                .presentation
                .as_ref()
                .filter(|presentation| presentation.is_valid())
                .and_then(|presentation| serde_json::to_string(presentation).ok())
                .unwrap_or_default(),
            action: None,
            descriptor: None,
            has_children: may_descend
                && (!resource.children.is_empty() || !resource.actions.is_empty()),
            visibility_action: Some(visibility.address.action),
            visibility_contract_json: serde_json::to_string(&visibility.contract).ok()?,
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
            resource_presentation_json: String::new(),
            action: Some(action_id.to_owned()),
            descriptor: Some(exported.clone()),
            has_children: false,
            visibility_action: Some(action_id.to_owned()),
            visibility_contract_json: exported.contract_json,
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
        let revision = self.action_directory_revision();
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
        let state = previous.unwrap_or_else(|| ActionDirectoryCursor {
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
        let mut estimated_bytes = 0usize;
        let add = |row: runtime_collab::ActionDirectoryRow,
                   rows: &mut Vec<runtime_collab::ActionDirectoryRow>,
                   bytes: &mut usize| {
            let descriptor_bytes = row.descriptor.as_ref().map_or(0, |descriptor| {
                let input_bytes = match &descriptor.input {
                    runtime_collab::ExportedInput::None => 0,
                    runtime_collab::ExportedInput::Text {
                        prompt,
                        placeholder,
                        ..
                    } => prompt.len() + placeholder.len() + 8,
                };
                descriptor.resource.len()
                    + descriptor.action.len()
                    + descriptor.resource_kind.len()
                    + descriptor.resource_label.len()
                    + descriptor.action_label.len()
                    + descriptor.description.as_ref().map_or(0, String::len)
                    + descriptor.params_json.len()
                    + descriptor.contract_json.len()
                    + input_bytes
            });
            *bytes += row.parent_resource.len()
                + row.resource.len()
                + row.resource_kind.len()
                + row.resource_label.len()
                + row.resource_presentation_json.len()
                + row.action.as_ref().map_or(0, String::len)
                + row.visibility_action.as_ref().map_or(0, String::len)
                + row.visibility_contract_json.len()
                + descriptor_bytes
                + 64;
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
                &query.needle,
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
        let exhausted = !byte_exhausted && provider_index >= providers.len();
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
        if revision != self.action_directory_revision() {
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

    fn pump_action_directory_requests(&mut self) {
        for request in self.collaboration.manager.take_action_directory_requests() {
            match request {
                ActionDirectoryRequest::Query { query, reply } => {
                    let _ = reply.send(self.action_directory_page(query));
                }
                ActionDirectoryRequest::Resolve {
                    revision,
                    resource,
                    action,
                    reply,
                } => {
                    let _ =
                        reply.send(self.resolve_action_directory(&revision, &resource, &action));
                }
            }
        }
    }
    fn invoke_published_action(
        &mut self,
        id: u64,
        resource: String,
        action: String,
        argument: Option<&str>,
        actor: &str,
        ambient_context: Option<&tilo_actions::ActionContext>,
    ) {
        let address = tilo_actions::ActionAddress::new(resource, action);
        // Arrival on this directed stream means the broker already performed
        // permission and one-use confirmation-token checks.
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let result = self.invoke_resource_action(
            ResourceInvocation::new(&address, actor)
                .argument(argument)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(invocation))
                .ambient_context(ambient_context),
        );
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
                tilo_actions::InvocationUpdate::Accepted { invocation }
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
        self.collaboration
            .manager
            .send(CollabRequest::UpdateInvocation { id, update });
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

    fn encode_resource_action(
        resources: &[tilo_actions::Resource],
        address: &tilo_actions::ActionAddress,
        typed_arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Result<tilo_actions::EncodedAction, tilo_actions::DoorError> {
        crate::bound_dispatch::encode_resource_action(resources, address, typed_arguments, argument)
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
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::OWN_PROVIDER.into(),
                    address: address.clone(),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(arguments),
                context: None,
                confirmation: Self::broker_confirmation(confirmation_evidence),
                revision: None,
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
            ambient_context,
            ..
        } = *invocation;
        let encoded = Self::encode_resource_action(resources, address, typed_arguments, argument)?;
        let action = self
            .actions
            .action_registry
            .decode(&encoded)
            .map_err(|_| tilo_actions::DoorError::UnregisteredType(encoded.type_id.clone()))?;
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
        let started = crate::bound_dispatch::execute_resource_operation(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            &self.state.root,
            &self.actions.action_registry,
            self.actions.action_transport.as_mut(),
            address,
            resource,
            spec,
            encoded,
            actor,
            &self.collaboration.local_author.0,
            broker_admitted,
            tilo_actions::InvocationId::new(),
            ambient_context,
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
        (kind == tilo_actions::OperationKind::Effect
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

    fn invoke_bound_target_with_broker(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        invocation: &ResourceInvocation<'_>,
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
            ..
        } = *invocation;
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
            argument,
            None,
            actor,
            confirmed,
            broker_admitted,
            id,
            ambient,
        )
        .map_err(crate::bound_dispatch::BoundActionError::into_door);
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

    fn invoke_resource_action(
        &mut self,
        invocation: ResourceInvocation<'_>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let address = invocation.address;
        if let Some(target) = self
            .actions
            .bound_publication
            .target(address)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?
        {
            self.require_bound_broker_route(&target.binding)?;
            return self.invoke_bound_target(&target, &invocation);
        }
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            Some(address),
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_door())?;
        let target = crate::bound_dispatch::select_bound_target(&snapshots, address, None)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?;
        if let Some(target) = target {
            return self.invoke_bound_target(&target, &invocation);
        }
        self.invoke_unbound_resource_action(&invocation)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "legacy broker and semantic orchestration remains an auditable lifecycle"
    )]
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
        } = *request;
        let mut resources = if actor == self.collaboration.local_author.0 {
            self.collect_local_resources()
        } else {
            self.collect_resources()
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
        let (typed_arguments, resource, resolved) = Self::resolve_resource_invocation(
            &resources,
            address,
            argument,
            confirmation_satisfied,
        )?;
        let kind = resolved.kind;
        if kind == tilo_actions::OperationKind::View && actor != self.collaboration.local_author.0 {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "private_view".into(),
                reason: "A collaborator cannot move this participant's private focus or viewport; use Follow or Reveal explicitly".into(),
            });
        }
        // Every locally initiated external effect crosses the same broker
        // admission as an agent invocation before the handler touches the
        // outside world. Directed invocations carry a real id and
        // `broker_admitted=true`, so they execute exactly once when routed
        // back here. Standalone development runs retain the existing local
        // fallback below; installed Tilo apps always have the session broker.
        if kind == tilo_actions::OperationKind::Effect
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
            return self.invoke_document_resource_action(
                request,
                &resources,
                typed_arguments.as_ref(),
                resource,
                resolved,
            );
        }
        let encoded =
            Self::encode_resource_action(&resources, address, typed_arguments.as_ref(), argument)?;
        // A launcher action has already passed the ordinary declaration,
        // argument-binding, confirmation, and broker-admission door above.
        // This branch runs only when the broker directs that admitted action
        // back to its source app, preserving the normal receipt lifecycle.
        if encoded.type_id == "tilo.application.launch" {
            let launch: ApplicationLaunchPayload = serde_json::from_slice(&encoded.payload)
                .map_err(|_| {
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
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::application_open_target(&room, &launch.app_id),
                argument: tilo_actions::remote::InvokeArgument::Text(launch.content_hash),
                context: None,
                confirmation: None,
                revision: None,
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
        let started = match crate::bound_dispatch::execute_resource_operation(
            self.root_widget.as_mut(),
            &mut self.state.doc,
            &self.state.root,
            &self.actions.action_registry,
            self.actions.action_transport.as_mut(),
            address,
            resource,
            resolved,
            encoded,
            actor,
            &self.collaboration.local_author.0,
            broker_admitted,
            operation_invocation,
            ambient_context,
        ) {
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
        if matches!(started, crate::widget::ResourceOperationStart::Accepted) {
            // Queued widget work advances from `sync`; admission must
            // therefore schedule a model pass even if no document
            // mutation or input event accompanied the invocation.
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
    ) -> bool {
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(intent.arguments.clone())
        );
        let actor = self.collaboration.local_author.0.clone();
        let result = self.invoke_bound_target(
            target,
            &ResourceInvocation::new(&target.address.address, &actor).argument(Some(&argument)),
        );
        if let Err(error) = crate::bound_dispatch::notify_control_invocation_result(
            self.root_widget.as_mut(),
            target,
            &result,
        ) {
            tracing::warn!(?error, "bound control feedback unavailable");
        }
        match result {
            Ok(_) => true,
            Err(tilo_actions::DoorError::ConfirmationRequired) => {
                self.open_action_palette();
                if let Some(palette) = self.actions.action_palette.as_mut() {
                    palette.activate_bound_with_arguments(target, &intent.arguments);
                }
                true
            }
            Err(error) => {
                tracing::warn!(%error, "bound control invocation refused");
                false
            }
        }
    }

    /// Resolve queued local control intents against the current resource tree
    /// and enter through the same door/lifecycle as every other caller.
    fn dispatch_control_invocations(&mut self) -> usize {
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
        let mut dispatched = self.dispatch_history_controls();
        let bound = crate::bound_dispatch::drain_control_invocations(self.root_widget.as_mut());
        if let Some(error) = bound.error {
            tracing::warn!(
                ?error,
                "control traversal incomplete; delivering already drained intents"
            );
        }
        for (target, intent) in bound.pending {
            if self.dispatch_bound_control_invocation(&target, &intent) {
                dispatched = dispatched.saturating_add(1);
            }
        }
        while let Some(intent) = self.root_widget.take_control_invocation() {
            let local_resources = self.collect_local_resources();
            let Some(resolved) = tilo_actions::door::resolve(&local_resources, &intent.address)
            else {
                tracing::warn!(
                    resource = %intent.address.resource,
                    action = %intent.address.action,
                    "action-bound control is unavailable to the local actor"
                );
                self.root_widget.on_control_invocation_result(
                    &intent.address,
                    &Err(tilo_actions::DoorError::Unknown(intent.address.clone())),
                );
                continue;
            };
            if !matches!(
                resolved.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            ) {
                // A bound control never manufactures confirmation. Hand the
                // exact address to the standard palette flow, which owns the
                // explicit confirm/phrase UI and only then enters the door.
                self.root_widget.on_control_invocation_result(
                    &intent.address,
                    &Err(tilo_actions::DoorError::ConfirmationRequired),
                );
                self.open_action_palette();
                let activated = self.actions.action_palette.as_mut().is_some_and(|palette| {
                    palette.activate_address_with_arguments(&intent.address, &intent.arguments)
                });
                if !activated {
                    self.close_action_palette();
                }
                dispatched = dispatched.saturating_add(1);
                continue;
            }
            let json = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments.clone())
            );
            let actor = self.collaboration.local_author.0.clone();
            let result = self.invoke_resource_action(
                ResourceInvocation::new(&intent.address, &actor).argument(Some(&json)),
            );
            self.root_widget
                .on_control_invocation_result(&intent.address, &result);
            match result {
                Ok(_) => dispatched = dispatched.saturating_add(1),
                Err(error) => tracing::warn!(
                    resource = %intent.address.resource,
                    action = %intent.address.action,
                    %error,
                    "action-bound control invocation refused"
                ),
            }
        }
        dispatched
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
            .map(|(invocation, pending)| (*invocation, *pending))
            .collect::<Vec<_>>();
        for (invocation, pending) in invocations {
            let polled = if let Some(target) = self.actions.bound_invocations.get(&invocation) {
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
                self.actions.bound_invocations.remove(&invocation);
                self.actions.directed_invocations.remove(&invocation);
            }
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation {
                    id: pending.wire_id,
                    update,
                });
            changed = true;
        }
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
            .map_or_else(Vec::new, |cache| {
                cache.resources.clone().unwrap_or_default()
            });
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
        self.actions.action_palette = Some(palette);
    }

    fn open_action_palette_at(&mut self, address: &tilo_actions::ActionAddress) -> bool {
        self.open_action_palette();
        let activated =
            self.actions.action_palette.as_mut().is_some_and(|palette| {
                palette.activate_address(&address.resource, &address.action)
            });
        if !activated {
            self.close_action_palette();
        }
        activated
    }

    fn filter_resource_for_local_actor(
        &self,
        mut resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        resource.actions.retain(|action| {
            crate::bound_dispatch::permits_local_action(
                &self.permission_ctx,
                &crate::collaboration::local_app_id(),
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
        self.actions.action_palette = None;
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
            .map_or_else(Vec::new, |cache| {
                cache.resources.clone().unwrap_or_default()
            });
        resources.extend(self.history_local_resources());
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
    fn action_palette_handle_key(&mut self, event: &WindowEvent) -> bool {
        if self.actions.action_palette.is_none() {
            return false;
        }
        self.refresh_action_palette_resources();
        let Some(palette) = self.actions.action_palette.as_mut() else {
            return false;
        };
        let root = self.state.root.clone();
        let consumed = palette.handle_key(event, &self.input.modifiers, &mut self.state.doc, &root);
        // An operation row may have fired. Document mutations go through the
        // deterministic executor; effects/views route to the owning widget.
        let pending = self
            .actions
            .action_palette
            .as_mut()
            .and_then(crate::action_palette::ActionPalette::take_operation);
        if let Some(pending) = pending {
            let argument = pending.wire_argument();
            let actor = self.collaboration.local_author.0.clone();
            // Confirmation was collected by the palette, but the request has
            // not crossed the session broker yet. Host-required documents are
            // therefore submitted to that broker rather than applied here.
            let history_result =
                self.invoke_history_address_local(&pending.address, argument.as_deref());
            let result = if let Some(result) = history_result {
                result
            } else if let Some(target) = pending.bound.as_ref() {
                self.invoke_bound_target(
                    target,
                    &ResourceInvocation::new(&pending.address, &actor)
                        .argument(argument.as_deref())
                        .confirmed(true)
                        .evidence(Some(pending.confirmation_evidence()))
                        .ambient_context(Some(&pending.context)),
                )
            } else {
                self.invoke_resource_action(
                    ResourceInvocation::new(&pending.address, &actor)
                        .argument(argument.as_deref())
                        .confirmed(true)
                        .evidence(Some(pending.confirmation_evidence()))
                        .ambient_context(Some(&pending.context)),
                )
            };
            if let Err(error) = result {
                tracing::warn!(%error, "resource operation failed");
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
                self.root_widget
                    .activate_search_result(&action_id, &mut self.state.doc, &root);
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
            self.actions.action_palette = None;
        }
        if consumed {
            self.invalidate_widget_resource_cache();
        }
        consumed
    }
}
