impl App {
    /// Write the live document back through `autosave`, the writer of the
    /// store it was loaded from.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "storage identity must fail closed rather than wrap and admit an old owner"
    )]
    pub fn with_autosave(mut self, autosave: app_state::DocWriter) -> Self {
        self.autosave_generation = self
            .autosave_generation
            .checked_add(1)
            .expect("autosave generation exhausted");
        if let Some(history) = &mut self.history {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            history.invalidate_owner();
            self.layout.request_rebuild();
        }
        self.persistence.autosave = Some(autosave);
        self
    }

    #[must_use]
    pub fn with_resources(
        mut self,
        mut records: crate::resource::ResourceSet,
        workspace_index: automerge::AutoCommit,
    ) -> Self {
        let _entry = self.enter_wake_scope();
        self.advance_autosave_generation();
        if let Some(history) = &mut self.history {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            history.invalidate_owner();
            self.layout.request_rebuild();
        }
        // Learn the current workspace grant before the widget selects a record.
        // Discovery is metadata-only: it must not create an empty record
        // replica or join a collaboration route for every indexed document.
        records.discover_workspace(&workspace_index);
        self.persistence.records = Some(records);
        self.persistence.workspace_index =
            Some(crate::state::DocumentReplica::new(workspace_index));
        self
    }

    /// Starts a new autosave generation so writes queued for the previous
    /// document are recognised as stale. A `u64` counter never wraps in
    /// practice; wrapping keeps this total instead of panicking.
    pub(super) fn advance_autosave_generation(&mut self) {
        self.autosave_generation = self.autosave_generation.wrapping_add(1);
    }

    pub fn records(&self) -> Option<&crate::resource::ResourceSet> {
        self.persistence.records.as_ref()
    }

    pub fn workspace_index(&self) -> Option<&automerge::AutoCommit> {
        self.persistence
            .workspace_index
            .as_ref()
            .map(crate::state::DocumentReplica::document)
    }

    /// Show the record the window's component asked to open in its place
    /// ([`AppComponent::take_replacement`](crate::application::AppComponent::take_replacement)):
    /// close the component, keeping its recovery, mount the one it built,
    /// and move the window's collaboration to the new record's route.
    /// Whether the window now shows another record.
    pub fn swap_requested_component(&mut self) -> bool {
        let _phase = tilo_resource::profiling::Phase::start("ui.swap_component");
        let _entry = self.enter_wake_scope();
        let Some(replacement) =
            crate::component_lifecycle::take_replacement(self.root_widget.as_mut())
        else {
            return false;
        };
        let (mut component, outcome) = replacement.into_parts();
        let Some(handle) = self.wake_scope.as_ref().map(crate::wake::WakeScope::handle) else {
            let _ = component.close();
            outcome.answer(Err("this window cannot show another record".into()));
            return false;
        };
        let retirement = component.take_root_retirement();
        if let Err(error) =
            component.mount(move |event| crate::app::forward_component_wake(&handle, &event))
        {
            let _ = component.close();
            outcome.answer(Err(error.to_string()));
            return false;
        }
        let binding = component.callback_binding();
        let history = component.history.take();
        let mut root: Box<dyn Widget> = Box::new(crate::widget::BoundAppWidget::new(component));
        if let Some(registration) = &history
            && let Err(error) = registration.validate_root(root.as_ref())
        {
            self.component_close_reports
                .extend(crate::component_lifecycle::close_components(root.as_mut()));
            outcome.answer(Err(error.to_string()));
            return false;
        }
        // The asking component answers the operation that asked before it
        // closes.
        outcome.answer(Ok(()));
        self.poll_directed_invocations();
        // The outgoing component is closed whole: its controls, its
        // semantic mount and its writer. Its writes commit on the store's
        // thread: a window showing the next document never waits on the disk.
        {
            let _started_only = durable_store::waiting::forbid_on_this_thread();
            self.close_components();
        }
        self.retirement = retirement;
        let control_domain = crate::control_domain::ControlDomain::outer();
        crate::widget::controls::attach_controls(
            root.as_mut(),
            &control_domain,
            crate::control_domain::DomainAttachment::Initial,
        );
        control_domain.finish_initial_installation();
        self.control_domain = control_domain;
        self.root_widget = root;
        self.history = None;
        self.layout.node_map.clear();
        self.bindings.clear();
        self.focused_node = None;
        self.layout.request_rebuild();
        self.layout.layout_dirty = true;
        self.needs_sync = true;
        if let Some(registration) = history
            && let Err(error) = self.install_history(registration)
        {
            tracing::warn!(%error, "the reopened record shows without its history");
        }
        let checkout = binding.checkout;
        if self.collaboration.manager.has_native_committed_primary() {
            if let Err(error) = self.collaboration.manager.switch_native_resource(&checkout) {
                tracing::warn!(%error, "the reopened record collaborates once its route opens");
            }
        } else {
            self.collaboration.manager = CollaborationManager::for_resource(&checkout);
        }
        if let Some(scope) = self.presence.broadcast_scope.clone() {
            self.broadcast_presence(scope, crate::collaboration::PresencePayload::Arrived);
        }
        true
    }

    /// The window's collaboration, for a test of what it joined.
    #[cfg(test)]
    pub(crate) fn collaboration_manager(&self) -> &CollaborationManager {
        &self.collaboration.manager
    }

    pub fn record_tick(&mut self) -> bool {
        let _phase = tilo_resource::profiling::Phase::start("ui.record_tick");
        let _entry = self.enter_wake_scope();
        let native = self.collaboration.manager.has_native_committed_primary();
        let initial = if native {
            let Some(want) = crate::resource::capability::record_intent(self.root_widget.as_ref())
            else {
                return false;
            };
            if self
                .persistence
                .records
                .as_ref()
                .is_some_and(|records| records.active() == Some(&want.id))
            {
                return false;
            }
            let Some(read) = self.collaboration.manager.prepare_native_resource(&want) else {
                return false;
            };
            Some((want, read))
        } else {
            None
        };
        let Some(set) = self.persistence.records.as_mut() else {
            return false;
        };
        let switched = if let Some((want, read)) = &initial {
            let accepted = read
                .with_mutation(|mutation| {
                    set.checkout_with_initial(
                        &mut self.state,
                        self.persistence
                            .workspace_index
                            .as_mut()
                            .map(crate::state::DocumentReplica::document_mut),
                        want,
                        mutation,
                        crate::history_mode::routes::merge_into_state(mutation),
                    )
                })
                .unwrap_or(false);
            if accepted {
                read.applied();
                Some(want.clone())
            } else {
                None
            }
        } else {
            crate::resource::tick(
                set,
                &mut self.state,
                self.persistence
                    .workspace_index
                    .as_mut()
                    .map(crate::state::DocumentReplica::document_mut),
                &*self.root_widget,
            )
        };
        match switched {
            Some(checkout) => {
                self.advance_autosave_generation();
                if let Some(history) = &mut self.history {
                    self.layout.node_map.clear();
                    self.bindings.clear();
                    self.focused_node = None;
                    history.invalidate_owner();
                    self.layout.request_rebuild();
                }
                self.state.rebase_outbound();
                // Preserve the launcher's unqualified route for the workspace
                // index before moving the editable document to its record
                // route. The two Automerge graphs never share a channel or
                // an apply target.
                if native {
                    if self.native_workspace_route.is_none() {
                        self.native_workspace_route = self
                            .collaboration
                            .manager
                            .primary_route()
                            .map(|observation| observation.route().clone());
                    }
                    if let Err(error) = self.collaboration.manager.switch_native_resource(&checkout)
                    {
                        tracing::warn!(%error,"native document route switch remains unavailable");
                        return false;
                    }
                } else if self.collaboration.workspace_manager.is_none() {
                    let workspace = std::mem::replace(
                        &mut self.collaboration.manager,
                        CollaborationManager::for_resource(&checkout),
                    );
                    self.collaboration.workspace_manager = Some(workspace);
                } else {
                    self.collaboration.manager = CollaborationManager::for_resource(&checkout);
                }
                tracing::debug!(
                    target: "tilo.state",
                    event = "resource_checkout",
                    record = checkout.id.as_str(),
                    connected = self.collaboration.manager.is_connected(),
                    "switched document collaboration route"
                );
                self.needs_sync = true;
                self.layout.layout_dirty = true;
                // Presence announces the location once the new route is
                // acknowledged, and the route reads its record as it opens;
                // the manager connects in the background.
                if let Some(scope) = self.presence.broadcast_scope.clone() {
                    self.broadcast_presence(scope, crate::collaboration::PresencePayload::Arrived);
                }
                true
            }
            None => false,
        }
    }

    /// With records in play, the App's autosave writes the workspace index
    /// instead of the live document — sending the live doc to both would put
    /// a record's content in the workspace index.
    #[cfg(feature = "native")]
    fn autosave_tick(&mut self) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        match self.persistence.records.as_mut() {
            Some(set) => {
                set.tick(&mut self.state.doc);
                if let (Some(saver), Some(workspace)) = (
                    self.persistence.autosave.as_mut(),
                    self.persistence.workspace_index.as_mut(),
                ) {
                    let index = workspace.document_mut();
                    index.commit();
                    saver.request(index);
                }
            }
            None => {
                if let Some(saver) = self.persistence.autosave.as_mut() {
                    self.state.doc.commit();
                    saver.request(&self.state.doc);
                }
            }
        }
        // Collect through the same live component boundary used by dispatch.
        // Distinct embedded owners participate; repeated views share one tick.
        let mut owners: Vec<crate::resource::ResourceStore> = Vec::new();
        let mut visit = |binding: &crate::bound_dispatch::CallbackBinding,
                         _: &mut dyn crate::widget::AppWidget,
                         _: &tilo_actions::ActionRegistry| {
            if !owners
                .iter()
                .any(|owner| owner.same_owner(&binding.records))
            {
                owners.push(binding.records.clone());
            }
            None
        };
        let traversal =
            self.root_widget
                .visit_components(&mut crate::bound_dispatch::ComponentRequest {
                    target: None,
                    found: false,
                    visit: &mut visit,
                });
        // Release component callback loans before requesting writer work. Owners
        // already collected still progress if another live component refuses entry.
        for owner in owners {
            if let Err(error) = owner.tick() {
                tracing::error!(%error, "a record could not be offered for saving");
            }
        }
        traversal
    }

    /// Save what the window holds as it goes, suspends or gives its place: a `kill` right after
    /// the last keystroke still finds it on disk.
    #[cfg(feature = "native")]
    fn autosave_flush(&mut self) {
        for error in self.checkpoint_before_close() {
            tracing::error!(%error, "could not save the window's state");
        }
    }

    /// Lets a caller check [`app_state::DocWriter::last_error`] rather
    /// than assuming the disk accepted the writes.
    pub fn autosave(&self) -> Option<&app_state::DocWriter> {
        self.persistence.autosave.as_ref()
    }
}
