//! Native runner integration for the same retained `HistoryHost` used headlessly.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::history_mode::frame::FrameworkInputFrame;

impl App {
    /// # Errors
    /// Refuses missing, stale, foreign or unsupported installed owner registrations.
    pub fn validate_history_registration(
        &self,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        let history = self
            .history
            .as_ref()
            .ok_or(crate::history_mode::HistoryRegistrationError::Missing)?;
        history.validate()?;
        history.validate_root(self.root_widget.as_ref())
    }
    pub(super) fn history_presentation_roots(&self) -> Vec<&dyn Widget> {
        let mut roots = Vec::new();
        if let Some(history) = &self.history {
            roots.push(history.as_ref() as &dyn Widget);
        }
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
        {
            roots.push(self.root_widget.as_ref());
        }
        roots
    }
    pub(super) fn discard_history_local_intents(&mut self) {
        let _ = crate::bound_dispatch::drain_control_invocations(self.root_widget.as_mut());
        while self.root_widget.take_control_invocation().is_some() {}
        self.actions.action_palette = None;
    }
    pub(super) fn refresh_framework_history(&mut self, width: f64) {
        self.refresh_document_routes();
        let status = self.document_sync_status().map(str::to_owned);
        let nested = crate::history_mode::embedded::needs_refresh(
            self.root_widget.as_ref(),
            &self.state.doc,
            width,
            status.as_deref(),
        );
        let invalid = self
            .history
            .as_ref()
            .is_some_and(|history| history.validate_root(self.root_widget.as_ref()).is_err());
        let status_changed = self
            .history
            .as_ref()
            .is_some_and(|history| !history.sync_status_matches(status.as_deref()));
        if !nested
            && !invalid
            && !status_changed
            && self
                .history
                .as_ref()
                .is_none_or(|history| !history.needs_refresh(&self.state.doc, width))
        {
            return;
        }
        // Refresh may replace retained chrome. No raw map survives it.
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
        if let Some(history) = &mut self.history {
            if status_changed {
                history.set_sync_status(status.clone());
            }
            if invalid {
                history.invalidate_owner();
            } else {
                history.set_width(width);
                history.refresh(&self.state.doc);
            }
        }
        crate::history_mode::embedded::refresh(
            self.root_widget.as_mut(),
            &self.state.doc,
            width,
            status.as_deref(),
        );
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input() || history.returning_live())
        {
            self.discard_history_local_intents();
        }
        self.invalidate_widget_resource_cache();
        self.layout.structure_dirty = true;
        self.layout.layout_dirty = true;
    }
    pub(super) fn update_history_focus_order(&mut self) {
        self.focusable_order.clear();
        let root = self
            .history
            .as_ref()
            .and_then(|history| history.review_layout_node())
            .or(self.layout.root_node);
        let mut nodes: Vec<_> = root.into_iter().collect();
        while let Some(node) = nodes.pop() {
            let allowed = crate::history_mode::embedded::allows_focus(
                self.root_widget.as_ref(),
                node,
                &self.layout.taffy,
            ) && self.history.as_ref().is_none_or(|history| {
                !history.captures_input() || history.owns_node(node, &self.layout.taffy)
            });
            if allowed
                && self
                    .inspect_node(node, |widget| widget.is_focusable() && widget.is_enabled())
                    .unwrap_or(false)
            {
                self.focusable_order.push(node);
            }
            nodes.extend(
                self.layout
                    .taffy
                    .children(node)
                    .unwrap_or_default()
                    .into_iter()
                    .rev(),
            );
        }
        self.focus_trap_active = self
            .history
            .as_ref()
            .is_some_and(|history| history.review_is_open());
        self.focus_before_trap = None;
    }
    pub(super) fn suspend_history_focus(&mut self, target: Option<NodeId>) {
        if unsafe {
            crate::history_mode::embedded::move_focus(
                self.root_widget.as_mut(),
                target,
                self.focused_node,
                &self.layout.node_map,
                &self.bindings,
                &self.layout.taffy,
            )
        } {
            self.focused_node = None;
        }
        let resumed = self.history.as_ref().is_some_and(|history| {
            !history.captures_input()
                && history.has_saved_focus()
                && target.is_some_and(|node| !history.owns_node(node, &self.layout.taffy))
        });
        if resumed {
            if let Some(history) = &mut self.history {
                history.discard_saved_focus();
            }
            self.history_focus = None;
            let old: Vec<_> = self
                .layout
                .node_map
                .keys()
                .copied()
                .filter(|node| {
                    Some(*node) != target
                        && self
                            .inspect_node(*node, |widget| widget.is_focused())
                            .unwrap_or(false)
                })
                .collect();
            for node in old {
                let _ = self.mutate_node(node, |widget| widget.set_focus(false));
            }
        }
        if let Some(history) = &mut self.history
            && target.is_some_and(|node| history.owns_node(node, &self.layout.taffy))
            && self
                .focused_node
                .is_some_and(|node| !history.owns_node(node, &self.layout.taffy))
        {
            unsafe {
                history.preserve_focus(
                    self.focused_node,
                    &self.layout.node_map,
                    &self.bindings,
                    &self.layout.taffy,
                );
            }
            self.focused_node = None;
        }
    }
    pub(super) fn history_controls_event(&mut self, event: &WindowEvent, scale: f64) -> bool {
        let nested = crate::history_mode::embedded::observe_event(
            self.root_widget.as_mut(),
            event,
            scale,
            self.focused_node,
            &self.layout.taffy,
        );
        if let Some(history) = &mut self.history {
            history.observe_pointer(event, scale);
        }
        if matches!(event, WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Released)
            && std::mem::take(&mut self.history_key_release)
        {
            return true;
        }
        let mut controls = nested
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.controls_event(event, scale));
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab)
        {
            let len = self.focusable_order.len();
            if len != 0 {
                let current = self.focused_node.and_then(|node| {
                    self.focusable_order
                        .iter()
                        .position(|candidate| *candidate == node)
                });
                let next = match (current, self.modifiers.state().shift_key()) {
                    (Some(0), true) => len.saturating_sub(1),
                    (Some(index), true) => index.saturating_sub(1),
                    (Some(index), false) => index.saturating_add(1).checked_rem(len).unwrap_or(0),
                    (None, _) => 0,
                };
                controls |= self.focusable_order.get(next).is_some_and(|node| {
                    crate::history_mode::embedded::owns_node(
                        self.root_widget.as_ref(),
                        *node,
                        &self.layout.taffy,
                    ) || self
                        .history
                        .as_ref()
                        .is_some_and(|history| history.owns_node(*node, &self.layout.taffy))
                });
            }
        }
        if controls
            && matches!(event, WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed)
        {
            self.history_key_release = true;
        }
        controls
    }
    /// Called before ordinary shortcuts, palettes, live pointer capture or
    /// event finalization. Admitted external lifecycles keep their own polling.
    pub(super) fn dispatch_framework_history_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
    ) -> bool {
        let _frame = FrameworkInputFrame::new(&self.framework_input, true);
        let mut handled = false;
        let mut focus = None;
        if matches!(event, WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed
            && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab))
        {
            handled = self.focus_tab(self.modifiers.state().shift_key());
        } else if let Some(history) = &mut self.history {
            if history.captures_input()
                && self
                    .focused_node
                    .is_some_and(|node| !history.owns_node(node, &self.layout.taffy))
            {
                unsafe {
                    history.preserve_focus(
                        self.focused_node,
                        &self.layout.node_map,
                        &self.bindings,
                        &self.layout.taffy,
                    );
                }
                self.focused_node = None;
            }
            history.route_event(
                self.root_widget.as_mut(),
                event,
                scale,
                &self.modifiers,
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus,
                &self.layout.taffy,
            );
        } else if let Some(root) = self.layout.root_node {
            self.root_widget.handle_event(
                event,
                scale,
                &self.modifiers,
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus,
                root,
                &self.layout.taffy,
            );
        }
        if let Some(focus) = focus {
            self.set_focused_node(Some(focus));
        }
        self.handle_unconsumed_activation_key(event, &mut handled);
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input())
        {
            self.discard_history_local_intents();
        }
        self.poll_directed_invocations();
        self.refresh_framework_history(
            self.layout.last_layout_logical.map_or(900.0, |size| size.0),
        );
        self.layout.layout_dirty = true;
        true
    }
    pub(super) fn settle_history_presentation(&mut self) -> bool {
        self.refresh_document_routes();
        if crate::history_mode::embedded::has_review(self.root_widget.as_ref()) {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            let restored = crate::history_mode::embedded::service_reviews(
                self.root_widget.as_mut(),
                &mut self.state,
                &self.permission_ctx,
                &self.local_author,
            );
            if !restored.is_empty() {
                self.needs_sync = true;
                self.invalidate_widget_resource_cache();
                self.publish_owned_documents();
            }
            self.layout.structure_dirty = true;
            return true;
        }
        let nested_focus = unsafe {
            crate::history_mode::embedded::restore_focus(
                self.root_widget.as_mut(),
                &self.layout.node_map,
                &self.bindings,
            )
        };
        let Some(history) = &mut self.history else {
            if let Some(focus) = nested_focus {
                self.focused_node = Some(focus);
                self.history_focus = None;
            } else if let Some(focus) = self.history_focus.take() {
                self.focused_node = unsafe {
                    focus.restore(
                        &self.layout.node_map,
                        &self.bindings,
                        Some(&self.focusable_order),
                    )
                };
            }
            return false;
        };

        if history.validate_root(self.root_widget.as_ref()).is_err() {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            let changed = history.invalidate_owner();
            self.layout.structure_dirty |= changed;
            return changed;
        }
        if history.has_review_request() {
            self.layout.node_map.clear();
            self.bindings.clear();
            self.focused_node = None;
            let changed = history.service_restore(
                &mut self.state,
                self.root_widget.as_ref(),
                &self.permission_ctx,
                &self.local_author,
            );
            let restored = history.take_restored_source();
            if changed {
                self.needs_sync = true;
                self.invalidate_widget_resource_cache();
                if matches!(
                    restored,
                    Some(crate::history_mode::HistorySourceRegistration::HostDocument { .. })
                ) {
                    self.broadcast_doc_delta();
                }
                if matches!(
                    restored,
                    Some(crate::history_mode::HistorySourceRegistration::OwnedDocument { .. })
                ) {
                    self.publish_owned_documents();
                }
            }
            self.layout.structure_dirty = true;
            return true;
        }
        if history.returning_live() {
            self.focused_node =
                unsafe { history.restore_focus(&self.layout.node_map, &self.bindings) };
            self.history_focus = None;
        } else if let Some(focus) = self.history_focus.take() {
            self.focused_node = unsafe {
                focus.restore(
                    &self.layout.node_map,
                    &self.bindings,
                    Some(&self.focusable_order),
                )
            };
        }
        false
    }
}
