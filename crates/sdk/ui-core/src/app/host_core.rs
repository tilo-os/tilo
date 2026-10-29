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
            records: self.persistence.records.as_ref(),
            permissions: &self.permission_ctx,
            focused: self.focused_node,
            history_layout: self.history_layout,
            root_node: self.layout.root_node,
            pointer: &self.input.pointer,
        }
    }

    fn document_history_with_tree(
        &mut self,
    ) -> (
        &mut crate::standalone_history_host::StandaloneHistoryHost,
        crate::host_core::HostTree<'_>,
    ) {
        (
            &mut self.document_history,
            crate::host_core::HostTree {
                state: &self.state,
                root: self.root_widget.as_ref(),
                history: self.history.as_deref(),
                bindings: &self.bindings,
                node_map: &self.layout.node_map,
                taffy: &self.layout.taffy,
                records: self.persistence.records.as_ref(),
                permissions: &self.permission_ctx,
                focused: self.focused_node,
                history_layout: self.history_layout,
                root_node: self.layout.root_node,
                pointer: &self.input.pointer,
            },
        )
    }

    fn document(&mut self) -> crate::host_core::HostDocument<'_> {
        crate::host_core::HostDocument {
            state: &mut self.state,
            bindings: &self.bindings,
            node_map: &self.layout.node_map,
            author: &self.collaboration.local_author,
        }
    }

    fn widget_reached(&mut self) {
        self.mark_semantic_tree_stale();
    }

    fn history_changed_document(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        match self.persistence.records.as_mut() {
            Some(set) => set.tick(&mut self.state.doc),
            None => {
                if let Some(saver) = self.persistence.autosave.as_mut() {
                    saver.request(&mut self.state.doc);
                }
            }
        }
    }

    /// The preview root runs inside this app's wake scope, like its live
    /// root.
    fn build_history_preview(
        &mut self,
        factory: &crate::standalone_history_host::PreviewRootFactory,
    ) -> Result<Box<dyn Widget>, String> {
        let _scope = self.enter_wake_scope();
        factory()
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
