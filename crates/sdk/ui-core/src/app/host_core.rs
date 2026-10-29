// The native host's side of the core both app hosts share.

impl crate::host_core::HostCore for App {
    fn tree(&self) -> crate::host_core::HostTree<'_> {
        crate::host_core::HostTree {
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

    fn document(&mut self) -> crate::host_core::HostDocument<'_> {
        crate::host_core::HostDocument {
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

    fn adopted_layout_mut(&mut self) -> &mut crate::retained_widgets::Adopted {
        &mut self.layout.adopted
    }

    fn layout_mut(&mut self) -> crate::host_core::HostLayout<'_> {
        crate::host_core::HostLayout {
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

    fn remount_parts(&mut self) -> crate::host_core::HostRemount<'_> {
        crate::host_core::HostRemount {
            root: self.root_widget.as_mut(),
            state: &self.state,
            theme: &self.theme,
            taffy: &mut self.layout.taffy,
            node_map: &mut self.layout.node_map,
            bindings: &mut self.bindings,
            close_reports: &mut self.component_close_reports,
        }
    }

    fn begin_remount(&mut self) -> bool {
        self.layout.remount = false;
        !self.layout.structure_dirty
            && self.layout.root_node.is_some()
            && self.close_admission.dialog.is_none()
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

    fn registered_history_mut(&mut self) -> Option<&mut crate::history_mode::host::HistoryChrome> {
        self.history.as_mut()
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
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        use crate::bound_dispatch::local_target::LocalResourceTarget;
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
            LocalResourceTarget::Outer => self.invoke_unbound_resource_action(
                &ResourceInvocation::new(&prepared.invocation.address, &actor)
                    .argument(Some(&argument))
                    .confirmed(prepared.confirmed),
            ),
        }
    }
}

impl crate::host_core::KeyHost for App {
    fn modifiers(&self) -> &winit::event::Modifiers {
        &self.input.modifiers
    }

    fn platform(&self) -> crate::platform::PlatformConventions {
        self.input.platform
    }

    /// The focus order, kept inside the keyboard scope (the focused desktop
    /// window's content, [`crate::widget::Widget::scopes_keyboard_focus`])
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

    fn move_focus(&mut self, node: Option<taffy::NodeId>) {
        self.set_focused_node(node);
    }

    fn key_fallback_acted(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }

    fn root_event(&mut self) -> Option<crate::host_core::RootEvent<'_>> {
        Some(crate::host_core::RootEvent {
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
