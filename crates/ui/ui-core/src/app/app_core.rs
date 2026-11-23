// The native host's side of the core both app hosts share.

impl crate::app_core::AppCore for App {
    fn tree(&self) -> crate::app_core::AppTree<'_> {
        crate::app_core::AppTree {
            state: &self.state,
            root: self.root_widget.as_ref(),
            history: self.history.as_deref(),
            bindings: &self.bindings,
            node_map: &self.layout.node_map,
            taffy: &self.layout.taffy,
            focused: self.focused_node,
            root_node: self.layout.root_node,
            pointer: &self.input.pointer,
        }
    }

    fn document(&mut self) -> crate::app_core::HostDocument<'_> {
        crate::app_core::HostDocument {
            state: &mut self.state,
            bindings: &self.bindings,
            node_map: &self.layout.node_map,
        }
    }

    fn history_commands_arrived(&mut self) {
        if self.history_focus.is_none() {
            self.history_focus = self.capture_focus_identity();
        }
        self.layout.node_map.clear();
        self.bindings.clear();
        self.focused_node = None;
        self.layout.request_rebuild();
        self.layout.layout_dirty = true;
    }

    fn visit_layout_roots(&self, visit: &mut dyn FnMut(&dyn Widget)) {
        visit(self.root_widget.as_ref());
        if let Some(history) = self.history.as_deref() {
            visit(history);
        }
        if let Some(dialog) = self.close_admission.dialog.as_deref() {
            visit(dialog);
        }
        if let Some(palette) = self.actions.action_palette.as_deref() {
            visit(palette);
        }
    }

    fn presentation_roots(&self) -> Vec<&dyn Widget> {
        let mut roots: Vec<&dyn Widget> = vec![self.root_widget.as_ref()];
        if let Some(history) = self.history.as_deref() {
            roots.push(history);
        }
        if let Some(dialog) = self.close_admission.dialog.as_deref() {
            roots.push(dialog);
        }
        if let Some(palette) = self.actions.action_palette.as_deref() {
            roots.push(palette);
        }
        roots
    }

    fn pointer_hovers(&self) -> bool {
        self.input.interaction.supports_hover()
    }

    fn adopted_layout_mut(&mut self) -> &mut crate::retained_widgets::Adopted {
        &mut self.layout.adopted
    }

    fn layout_mut(&mut self) -> crate::app_core::HostLayout<'_> {
        crate::app_core::HostLayout {
            taffy: &mut self.layout.taffy,
            node_map: &mut self.layout.node_map,
            bindings: &mut self.bindings,
            focused: &mut self.focused_node,
        }
    }

    fn discard_layout_maps(&mut self) {
        self.bindings.clear();
        self.layout.node_map.clear();
        self.layout.adopted.clear();
        self.layout.request_rebuild();
    }

    fn layout_admitted(&mut self) {
        self.layout.admissions = self.layout.admissions.wrapping_add(1);
    }

    fn composition_replaced(&mut self) -> bool {
        self.forget_replaced_widgets()
    }

    fn remount_parts(&mut self) -> crate::app_core::HostRemount<'_> {
        crate::app_core::HostRemount {
            root: self.root_widget.as_mut(),
            state: &self.state,
            theme: &self.theme,
            taffy: &mut self.layout.taffy,
            node_map: &mut self.layout.node_map,
            bindings: &mut self.bindings,
            close_reports: &mut self.component_close_reports,
            interaction: self.input.interaction,
        }
    }

    fn begin_remount(&mut self) -> bool {
        self.layout.remount = false;
        !self.layout.structure_dirty
            && self.layout.root_node.is_some()
            && self.close_admission.dialog.is_none()
    }

    fn remount_requested(&self) -> bool {
        self.layout.remount
    }

    fn remounted(&mut self, _built: usize) {
        self.layout.remounts = self.layout.remounts.wrapping_add(1);
        self.layout.layout_dirty = true;
        // Fresh controls have not seen where the pointer is.
        self.input.pointer_replay.mark_unsettled();
        self.needs_sync = false;
        self.invalidate_widget_resource_cache();
    }

    fn composition_dirty(&mut self) -> bool {
        self.take_composition_dirty()
    }

    fn widget_reached(&mut self) {
        self.mark_semantic_tree_stale();
    }

    fn host_root_mut(&mut self) -> &mut dyn Widget {
        self.root_widget.as_mut()
    }

    fn focus_parts(&mut self) -> crate::app_core::HostFocus<'_> {
        crate::app_core::HostFocus {
            root: self.root_widget.as_mut(),
            history: self.history.as_mut(),
            node_map: &self.layout.node_map,
            bindings: &self.bindings,
            taffy: &self.layout.taffy,
            focused: &mut self.focused_node,
        }
    }

    fn history_focus_resumed(&mut self) {
        self.history_focus = None;
    }

    fn registered_history_mut(&mut self) -> Option<&mut crate::history_mode::host::HistoryChrome> {
        self.history.as_mut()
    }

    fn service_history_own_controls(&mut self) -> bool {
        let Some(history) = self.history.as_mut() else {
            return false;
        };
        let path = &self.state.root;
        let answered =
            history.answer_own_controls(&mut crate::widget::Edits::lend(&mut self.state.doc), path);
        answered > 0
    }

    fn acting_for_mut(
        &mut self,
    ) -> &mut std::collections::HashMap<
        tilo_actions::InvocationId,
        crate::invocation_scope::Continuation,
    > {
        &mut self.actions.acting_for
    }

    fn dispatch_current_history_control(
        &mut self,
        prepared: crate::history_mode::pending_controls::PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        use crate::bound_dispatch::local_target::LocalResourceTarget;
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(prepared.invocation.arguments.clone())
        );
        let actor = tilo_actions::Actor::Member(self.collaboration.local_author.clone());
        match prepared.target {
            LocalResourceTarget::Component(target) => self.invoke_bound_target_with_broker(
                &target,
                &ResourceInvocation::new(&target.address.address, &actor).argument(Some(&argument)),
                None,
            ),
            LocalResourceTarget::Outer => self.invoke_unbound_resource_action(
                &ResourceInvocation::new(&prepared.invocation.address, &actor)
                    .argument(Some(&argument)),
            ),
        }
    }

    fn ask_through_device(
        &mut self,
        target: &crate::bound_dispatch::local_target::LocalResourceTarget,
        address: &tilo_actions::ActionAddress,
        arguments: &crate::bound_dispatch::ControlContinuationArguments,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        use crate::bound_dispatch::local_target::LocalResourceTarget;
        // The device knows the call by the address this window publishes it under: a component's
        // own resource by its alias.
        let published = match target {
            LocalResourceTarget::Component(target) => tilo_actions::ActionAddress::new(
                self.actions
                    .bound_publication
                    .alias(&target.binding, &address.resource)
                    .map_err(crate::bound_dispatch::BoundActionError::into_api)?,
                address.action.clone(),
            ),
            LocalResourceTarget::Outer => address.clone(),
        };
        let (argument, typed) = arguments.as_api_arguments();
        self.invoke_via_broker(&published, typed, argument)
    }
}

impl crate::app_core::KeyCore for App {
    fn modifiers(&self) -> &winit::event::Modifiers {
        &self.input.modifiers
    }

    fn platform(&self) -> crate::platform::PlatformConventions {
        self.input.platform
    }

    /// The focus order, kept inside the keyboard scope (the focused shell
    /// window, [`crate::widget::Widget::scopes_keyboard_focus`])
    /// unless a trap owns the keyboard.
    fn keyboard_travel_order(&self) -> Vec<taffy::NodeId> {
        if self.accessibility.focus_trap_active {
            return self.focusable_order.clone();
        }
        self.tree()
            .within_keyboard_scope(self.focusable_order.clone())
    }

    fn keyboard_entry_order(&self) -> Vec<taffy::NodeId> {
        self.focusable_order.clone()
    }

    fn keyboard_entry_owed(&mut self) -> &mut bool {
        &mut self.input.keyboard_entry_owed
    }

    fn move_focus(&mut self, node: Option<taffy::NodeId>) {
        self.set_focused_node(node);
    }

    fn key_fallback_acted(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }

    fn root_event(&mut self) -> Option<crate::app_core::RootEvent<'_>> {
        Some(crate::app_core::RootEvent {
            node: self.layout.root_node?,
            root: self.root_widget.as_mut(),
            state: &mut self.state,
            modifiers: &self.input.modifiers,
            taffy: &self.layout.taffy,
            scale_factor: 1.0,
        })
    }

    fn tab_order_continues_in_host(&self) -> bool {
        self.input.tab_order_continues_in_host
    }

    #[cfg(feature = "native")]
    fn handle_unconsumed_host_key(&mut self, event: &winit::event::WindowEvent) -> bool {
        self.handle_unconsumed_search_key(event) || self.handle_unconsumed_typed_key(event)
    }
}
