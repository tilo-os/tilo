// The app's side of the core its frames run.

impl App {
    pub(crate) fn tree(&self) -> crate::app_core::AppTree<'_> {
        crate::app_core::AppTree {
            state: &self.state,
            root: self.root_widget.as_ref(),
            #[cfg(feature = "model")]
            history: self.history.as_deref(),
            bindings: &self.bindings,
            node_map: &self.layout.node_map,
            taffy: &self.layout.taffy,
            focused: self.focused_node,
            root_node: self.layout.root_node,
            pointer: &self.input.pointer,
        }
    }

    pub(crate) fn document(&mut self) -> crate::app_core::HostDocument<'_> {
        crate::app_core::HostDocument {
            state: &mut self.state,
            bindings: &self.bindings,
            node_map: &self.layout.node_map,
        }
    }

    #[cfg(feature = "model")]
    pub(crate) fn history_commands_arrived(&mut self) {
        if self.history_focus.is_none() {
            self.history_focus = self.capture_focus_identity();
        }
        self.layout.node_map.clear();
        self.bindings.clear();
        self.focused_node = None;
        self.layout.request_rebuild();
        self.layout.layout_dirty = true;
    }

    /// The window's History, to change. Tilo's model keeps what changed and who changed it:
    /// without it a window has no History.
    pub(crate) fn history_widget_mut(&mut self) -> Option<&mut dyn Widget> {
        #[cfg(feature = "model")]
        let history = self
            .history
            .as_deref_mut()
            .map(|history| history as &mut dyn Widget);
        #[cfg(not(feature = "model"))]
        let history = None;
        history
    }

    /// Whether `node` is one of the window's History's own.
    pub(crate) fn history_owns_node(&self, node: NodeId) -> bool {
        #[cfg(feature = "model")]
        let owns = self.tree().history_owns(node);
        #[cfg(not(feature = "model"))]
        let owns = {
            let _ = node;
            false
        };
        owns
    }

    /// Whether the window's History is open.
    #[cfg(all(feature = "native", not(target_os = "android")))]
    pub(crate) fn history_open(&self) -> bool {
        #[cfg(feature = "model")]
        let open = self
            .history
            .as_ref()
            .is_some_and(|history| history.is_open());
        #[cfg(not(feature = "model"))]
        let open = false;
        open
    }

    /// The action palette, open or arriving. Tilo's model is the action layer it searches and
    /// runs: without it a window has none.
    pub(crate) fn palette_widget(&self) -> Option<&dyn Widget> {
        #[cfg(feature = "model")]
        let palette = self
            .actions
            .action_palette
            .as_deref()
            .map(|palette| palette as &dyn Widget);
        #[cfg(not(feature = "model"))]
        let palette = None;
        palette
    }

    /// [`Self::palette_widget`], to change.
    pub(crate) fn palette_widget_mut(&mut self) -> Option<&mut dyn Widget> {
        #[cfg(feature = "model")]
        let palette = self
            .actions
            .action_palette
            .as_deref_mut()
            .map(|palette| palette as &mut dyn Widget);
        #[cfg(not(feature = "model"))]
        let palette = None;
        palette
    }

    /// The action palette while it takes the window.
    pub(crate) fn presented_palette(&self) -> Option<&dyn Widget> {
        #[cfg(feature = "model")]
        let palette = self
            .actions
            .action_palette
            .as_deref()
            .filter(|palette| palette.is_presented())
            .map(|palette| palette as &dyn Widget);
        #[cfg(not(feature = "model"))]
        let palette = None;
        palette
    }

    /// Whether the action palette is open.
    pub(crate) fn palette_open(&self) -> bool {
        self.palette_widget().is_some()
    }

    /// Whether the action palette takes the window.
    pub(crate) fn palette_presented(&self) -> bool {
        self.presented_palette().is_some()
    }

    /// Whether the action palette is still arriving or leaving.
    pub(crate) fn palette_moving(&self) -> bool {
        #[cfg(feature = "model")]
        let moving = self
            .actions
            .action_palette
            .as_deref()
            .is_some_and(crate::action_palette::ActionPalette::is_moving);
        #[cfg(not(feature = "model"))]
        let moving = false;
        moving
    }

    pub(crate) fn visit_layout_roots(&self, visit: &mut dyn FnMut(&dyn Widget)) {
        visit(self.root_widget.as_ref());
        if let Some(history) = self.history_widget() {
            visit(history);
        }
        if let Some(dialog) = self.close_admission.dialog.as_deref() {
            visit(dialog);
        }
        if let Some(palette) = self.palette_widget() {
            visit(palette);
        }
    }

    /// What the window presents: History, the palette while it takes the window, the app's own
    /// surface while History leaves it operable, and a close dialog standing in for the app.
    ///
    /// A pressed change pins its past in place of the live app: the app's controls and its
    /// widgets are then reachable by nothing, and a frame owes them nothing.
    pub(crate) fn presentation_roots(&self) -> Vec<&dyn Widget> {
        let mut roots: Vec<&dyn Widget> = Vec::new();
        if let Some(history) = self.history_widget() {
            roots.push(history);
        }
        if let Some(palette) = self.presented_palette() {
            roots.push(palette);
            return roots;
        }
        if self.app_live() {
            roots.push(self.root_widget.as_ref());
        }
        if let Some(dialog) = self.close_admission.dialog.as_deref() {
            roots.push(dialog);
        }
        roots
    }

    pub(crate) fn pointer_hovers(&self) -> bool {
        self.input.interaction.supports_hover()
    }

    pub(crate) fn adopted_layout_mut(&mut self) -> &mut crate::retained_widgets::Adopted {
        &mut self.layout.adopted
    }

    pub(crate) fn layout_mut(&mut self) -> crate::app_core::HostLayout<'_> {
        crate::app_core::HostLayout {
            taffy: &mut self.layout.taffy,
            node_map: &mut self.layout.node_map,
            bindings: &mut self.bindings,
            focused: &mut self.focused_node,
        }
    }

    pub(crate) fn discard_layout_maps(&mut self) {
        self.bindings.clear();
        self.layout.node_map.clear();
        self.layout.adopted.clear();
        self.layout.request_rebuild();
    }

    pub(crate) fn layout_admitted(&mut self) {
        self.layout.admissions = self.layout.admissions.wrapping_add(1);
    }

    pub(crate) fn composition_replaced(&mut self) -> bool {
        self.forget_replaced_widgets()
    }

    pub(crate) fn remount_parts(&mut self) -> crate::app_core::HostRemount<'_> {
        crate::app_core::HostRemount {
            root: self.root_widget.as_mut(),
            state: &self.state,
            theme: &self.theme,
            taffy: &mut self.layout.taffy,
            node_map: &mut self.layout.node_map,
            bindings: &mut self.bindings,
            #[cfg(feature = "model")]
            close_reports: &mut self.component_close_reports,
            interaction: self.input.interaction,
        }
    }

    pub(crate) fn begin_remount(&mut self) -> bool {
        self.layout.remount = false;
        !self.layout.structure_dirty
            && self.layout.root_node.is_some()
            && self.close_admission.dialog.is_none()
    }

    pub(crate) fn remount_requested(&self) -> bool {
        self.layout.remount
    }

    pub(crate) fn remounted(&mut self, built: usize) {
        self.layout.remounts = self.layout.remounts.wrapping_add(1);
        let built = u64::try_from(built).unwrap_or(u64::MAX);
        self.layout.frames.nodes_built = self.layout.frames.nodes_built.saturating_add(built);
        self.layout.layout_dirty = true;
        // Fresh controls have not seen where the pointer is.
        self.input.pointer_replay.mark_unsettled();
        self.needs_sync = false;
        self.invalidate_widget_resource_cache();
    }

    pub(crate) fn composition_dirty(&mut self) -> bool {
        self.take_composition_dirty()
    }

    pub(crate) fn widget_reached(&mut self) {
        self.mark_semantic_tree_stale();
    }

    pub(crate) fn host_root_mut(&mut self) -> &mut dyn Widget {
        self.root_widget.as_mut()
    }

    pub(crate) fn focus_parts(&mut self) -> crate::app_core::HostFocus<'_> {
        crate::app_core::HostFocus {
            root: self.root_widget.as_mut(),
            #[cfg(feature = "model")]
            history: self.history.as_mut(),
            node_map: &self.layout.node_map,
            bindings: &self.bindings,
            taffy: &self.layout.taffy,
            focused: &mut self.focused_node,
        }
    }

    #[cfg(feature = "model")]
    pub(crate) fn history_focus_resumed(&mut self) {
        self.history_focus = None;
    }

    #[cfg(feature = "model")]
    pub(crate) fn registered_history_mut(
        &mut self,
    ) -> Option<&mut crate::history_mode::host::HistoryChrome> {
        self.history.as_mut()
    }

    #[cfg(feature = "model")]
    pub(crate) fn service_history_own_controls(&mut self) -> bool {
        let Some(history) = self.history.as_mut() else {
            return false;
        };
        let path = &self.state.root;
        let answered =
            history.answer_own_controls(&mut crate::widget::Edits::lend(&mut self.state.doc), path);
        answered > 0
    }

    #[cfg(feature = "model")]
    pub(crate) fn acting_for_mut(
        &mut self,
    ) -> &mut std::collections::HashMap<
        tilo_actions::InvocationId,
        crate::invocation_scope::Continuation,
    > {
        &mut self.actions.acting_for
    }

    #[cfg(feature = "model")]
    pub(crate) fn dispatch_current_history_control(
        &mut self,
        prepared: crate::history_mode::pending_controls::PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        use crate::bound_dispatch::local_target::LocalResourceTarget;
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(prepared.invocation.arguments.clone())
        );
        let actor = tilo_actions::Actor::Member(self.local_author.clone());
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

    #[cfg(feature = "model")]
    pub(crate) fn ask_through_device(
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

impl App {
    /// The modifier keys held now.
    pub(crate) fn modifiers(&self) -> &winit::event::Modifiers {
        &self.input.modifiers
    }

    pub(crate) fn platform(&self) -> crate::platform::PlatformConventions {
        self.input.platform
    }

    /// The focus order, kept inside the keyboard scope (the focused shell
    /// window, [`crate::widget::Widget::scopes_keyboard_focus`])
    /// unless a trap owns the keyboard.
    pub(crate) fn keyboard_travel_order(&self) -> Vec<taffy::NodeId> {
        if self.accessibility.focus_trap_active {
            return self.focusable_order.clone();
        }
        self.tree()
            .within_keyboard_scope(self.focusable_order.clone())
    }

    pub(crate) fn keyboard_entry_order(&self) -> Vec<taffy::NodeId> {
        self.focusable_order.clone()
    }

    pub(crate) fn keyboard_entry_owed(&mut self) -> &mut bool {
        &mut self.input.keyboard_entry_owed
    }

    pub(crate) fn move_focus(&mut self, node: Option<taffy::NodeId>) {
        self.set_focused_node(node);
    }

    pub(crate) fn key_fallback_acted(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }

    pub(crate) fn root_event(&mut self) -> Option<crate::app_core::RootEvent<'_>> {
        Some(crate::app_core::RootEvent {
            node: self.layout.root_node?,
            root: self.root_widget.as_mut(),
            state: &mut self.state,
            modifiers: &self.input.modifiers,
            taffy: &self.layout.taffy,
            scale_factor: 1.0,
        })
    }

    pub(crate) fn tab_order_continues_in_host(&self) -> bool {
        self.input.tab_order_continues_in_host
    }
}
