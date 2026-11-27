impl App {
    /// Starts a new autosave generation so writes queued for the previous
    /// document are recognised as stale. A `u64` counter never wraps in
    /// practice; wrapping keeps this total instead of panicking.
    #[cfg(test)]
    #[cfg(feature = "model")]
    pub(super) fn advance_autosave_generation(&mut self) {
        self.autosave_generation = self.autosave_generation.wrapping_add(1);
    }

    /// Show the record the window's component asked to open in its place
    /// ([`AppComponent::take_replacement`](crate::application::AppComponent::take_replacement)):
    /// close the component, keeping its recovery, mount the one it built,
    /// and move the window's collaboration to the new record's route.
    /// Whether the window now shows another record: never without Tilo's model, which alone hosts
    /// a component.
    pub fn swap_requested_component(&mut self) -> bool {
        #[cfg(feature = "model")]
        let swapped = self.swap_in_replacement();
        #[cfg(not(feature = "model"))]
        let swapped = false;
        swapped
    }

    #[cfg(feature = "model")]
    fn swap_in_replacement(&mut self) -> bool {
        let _phase = tilo_actions::profiling::Phase::start("ui.swap_component");
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
        self.close_components();
        self.retirement = retirement;
        let control_domain = tilo_ui_base::control_domain::ControlDomain::outer();
        tilo_ui_base::widget::controls::attach_controls(
            root.as_mut(),
            &control_domain,
            tilo_ui_base::control_domain::DomainAttachment::Initial,
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
            let manager = std::mem::replace(
                &mut self.collaboration.manager,
                CollaborationManager::disabled(),
            );
            self.collaboration.manager = manager.for_reopened(&checkout);
        }
        if let Some(scope) = self.presence.broadcast_scope.clone() {
            self.broadcast_presence(scope, ::collaboration::PresencePayload::Arrived);
        }
        true
    }

    #[cfg(feature = "model")]
    /// The window's collaboration, for a test of what it joined.
    #[cfg(any(test, feature = "test-support"))]
    pub fn collaboration_manager(&self) -> &CollaborationManager {
        &self.collaboration.manager
    }

    #[cfg(feature = "model")]
    /// Send what the window's records changed to their workspace, where the store writes them
    /// itself.
    #[cfg(feature = "native")]
    fn autosave_tick(&mut self) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
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
        let traversal = crate::bound_dispatch::visit_components(
            &mut *self.root_widget,
            &mut crate::bound_dispatch::ComponentRequest::new(None, &mut visit),
        );
        // Release component callback loans before requesting writer work. Owners
        // already collected still progress if another live component refuses entry.
        for owner in owners {
            owner.tick();
        }
        traversal
    }

    /// Save what the window holds as it goes, suspends or gives its place: a `kill` right after
    /// the last keystroke still finds it on disk.
    #[cfg(feature = "native")]
    fn autosave_flush(&mut self) {
        // Only Tilo's model keeps a component's record to save.
        #[cfg(feature = "model")]
        for error in self.checkpoint_before_close() {
            tracing::error!(%error, "could not save the window's state");
        }
    }
}
