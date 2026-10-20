//! Native runner integration for the same retained `HistoryHost` used headlessly.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::history_mode::frame::FrameworkInputFrame;
use tilo_sort::CompactSort as _;

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
        if let Some(palette) = self
            .actions
            .action_palette
            .as_ref()
            .filter(|p| p.is_presented())
        {
            roots.push(palette as &dyn Widget);
            return roots;
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
        if let Some(history) = &mut self.history {
            history.discard_pending_output(self.root_widget.as_mut());
        } else {
            let _ = crate::bound_dispatch::drain_control_invocations(self.root_widget.as_mut());
            crate::bound_dispatch::legacy_document::discard_legacy_controls(
                self.root_widget.as_mut(),
            );
            while crate::widget::controls::take_control_invocation(self.root_widget.as_mut())
                .is_some()
            {}
        }
    }
    fn capture_history_entry(&mut self) {
        let captured = self.history.as_mut().is_some_and(|history| {
            history.captures_input()
                && history
                    .capture_embedded_pending_controls(self.root_widget.as_mut(), &self.state.doc)
        });
        if captured {
            let address = self
                .actions
                .action_palette
                .as_ref()
                .filter(|palette| !palette.history_has_custody())
                .and_then(crate::action_palette::ActionPalette::history_pending_address);
            let invocation = self
                .actions
                .action_palette
                .as_ref()
                .and_then(crate::action_palette::ActionPalette::history_pending_invocation);
            let original_arguments = self
                .actions
                .action_palette
                .as_ref()
                .and_then(|palette| palette.history_pending_continuation_arguments());
            let custody = address.as_ref().map(|address| {
                let original = self
                    .actions
                    .action_palette
                    .as_ref()
                    .and_then(|p| p.ordinary_request());
                if let Some(original) = original {
                    original.validate_owner(&self.control_domain)?;
                }
                let target =
                    if let Some(original) = original {
                        original.target.clone()
                    } else {
                        match self.actions.action_palette
                        .as_ref()
                        .and_then(|palette| palette.history_pending_bound_target())
                    {
                        Some(bound) => {
                            crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(
                                bound,
                            )
                        }
                        None => crate::bound_dispatch::legacy_document::select_local_target(
                            self.root_widget.as_ref(),
                            &self.state.doc,
                            address,
                        )?,
                    }
                    };
                let witness = invocation.as_ref().zip(original_arguments.as_ref()).map(
                    |(invocation, original_arguments)| {
                        crate::bound_dispatch::control_witness_with_arguments(
                            self.root_widget.as_ref(),
                            &self.state.doc,
                            &target,
                            invocation,
                            original_arguments,
                        )
                        .map_err(|error| error.to_string())
                    },
                );
                let identity = match &witness {
                    Some(Ok(witness)) => Some(Ok(witness.identity().clone())),
                    Some(Err(reason)) => Some(Err(reason.clone())),
                    None => Some(
                        crate::bound_dispatch::control_draft_identity(
                            self.root_widget.as_ref(),
                            &self.state.doc,
                            &target,
                            address,
                        )
                        .map_err(|error| error.to_string()),
                    ),
                };
                Ok::<_, tilo_actions::DoorError>((target, identity, witness))
            });
            if let Some(palette) = self.actions.action_palette.as_mut() {
                palette.suspend_for_history();
                if let Some(custody) = custody {
                    match custody {
                        Ok((target, identity, witness)) => {
                            palette.retain_history_custody(target, identity, witness);
                        }
                        Err(error) => {
                            palette.retain_history_resolution_error(error.to_string());
                        }
                    }
                }
            }
        }
        crate::history_mode::embedded::capture_entries(self.root_widget.as_mut(), &self.state.doc);
    }
    pub(super) fn refresh_framework_history(&mut self, width: f64) {
        self.refresh_document_routes();
        let status = self
            .document_sync_status()
            .map(std::borrow::Cow::into_owned);
        // A status that only changes the text where it already shows repaints
        // that text; it is no reason to rebuild History or the app.
        crate::history_mode::embedded::set_sync_status_in_place(
            self.root_widget.as_mut(),
            status.as_deref(),
        );
        if let Some(history) = &mut self.history
            && !history.sync_status_matches(status.as_deref())
        {
            history.set_sync_status_in_place(status.as_deref(), self.root_widget.as_mut());
        }
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
            self.history_focus = self.capture_focus_identity();
        }
        self.layout.node_map.clear();
        self.bindings.clear();
        self.focused_node = None;
        self.capture_history_entry();
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
        let returning_live = self
            .history
            .as_ref()
            .is_some_and(|history| history.returning_live());
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input())
            || returning_live
        {
            self.discard_history_local_intents();
        }
        if returning_live {
            if let Some(history) = &mut self.history {
                history.finish_return_to_live();
                history.flush_pending_toolbar();
            }
            if let Some(palette) = self.actions.action_palette.as_mut() {
                palette.resume_after_history();
            }
        }
        self.invalidate_widget_resource_cache();
        self.layout.request_rebuild();
        self.layout.layout_dirty = true;
    }
    pub(super) fn update_history_focus_order(
        &mut self,
        restrictions: &crate::history_mode::embedded::FocusRestrictions,
    ) {
        self.focusable_order.clear();
        let root = self
            .history
            .as_ref()
            .and_then(|history| history.review_layout_node())
            .or(self.layout.root_node);
        // History's owned nodes, walked once for this recomputation: both the
        // capture filter and the History-last sort ask it of every node.
        let owned = self
            .history
            .as_ref()
            .map(|history| history.owned_nodes(&self.layout.taffy))
            .unwrap_or_default();
        let captures = self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input());
        let (order, app_trap) = root.map_or_else(
            || (Vec::new(), false),
            |root| {
                crate::focus_navigation::mapped_order(root, &self.layout.taffy, |node| {
                    let allowed = restrictions.allows(node) && (!captures || owned.contains(&node));
                    self.inspect_node(node, |widget| {
                        (
                            allowed && widget.is_focusable() && widget.is_enabled(),
                            allowed && widget.traps_focus(),
                            widget
                                .accessibility_root()
                                .is_some_and(|node| node.state.hidden),
                        )
                    })
                    .unwrap_or((false, false, false))
                })
            },
        );
        self.focusable_order = order;
        if !captures {
            self.focusable_order
                .compact_sort_by_key(|id| owned.contains(id));
        }
        let trap_active = app_trap
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.review_is_open());
        if let Some(next) = crate::focus_navigation::reconcile_trap(
            &mut self.accessibility.focus_trap_active,
            &mut self.accessibility.focus_before_trap,
            self.focused_node,
            &self.focusable_order,
            trap_active,
        ) {
            self.set_focused_node(next);
        }
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
                let next = match (current, self.input.modifiers.state().shift_key()) {
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
        let frame = FrameworkInputFrame::new(&self.framework_input, true);
        let mut handled = false;
        let mut focus = None;
        if matches!(event, WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed
            && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab))
        {
            handled = self.focus_tab(self.input.modifiers.state().shift_key());
        } else if let Some(history) = &mut self.history
            && let Some(root) = self.layout.root_node
        {
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
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: root,
                    taffy: &self.layout.taffy,
                },
            );
        } else if let Some(root) = self.layout.root_node {
            self.root_widget.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: root,
                    taffy: &self.layout.taffy,
                },
            );
        }
        if let Some(focus) = focus {
            self.set_focused_node(Some(focus));
        }
        self.handle_unconsumed_activation_key(event, &mut handled);
        // A genuine open request must seal the live queue before any History
        // output disposal. The refresh also invalidates admitted raw maps
        // before replacing framework chrome.
        self.refresh_framework_history(
            self.layout.last_layout_logical.map_or(900.0, |size| size.0),
        );
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.captures_input())
        {
            self.discard_history_local_intents();
        }
        self.poll_directed_invocations();
        drop(frame);
        self.service_pending_history_commands();
        self.layout.layout_dirty = true;
        true
    }
    #[cfg(feature = "native")]
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
                &self.collaboration.local_author,
            );
            if !restored.is_empty() {
                self.needs_sync = true;
                self.invalidate_widget_resource_cache();
                self.publish_owned_documents();
            }
            self.layout.request_rebuild();
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
            if changed {
                self.layout.request_rebuild();
            }
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
                &self.collaboration.local_author,
            );
            let restored = history.take_restored_source();
            if changed {
                self.publish_history_write(restored.as_ref());
            }
            self.layout.request_rebuild();
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

    /// Write the descriptions the shell gave into the document, when no
    /// edit is open, so they reach everyone in it; otherwise they wait for
    /// the next pump.
    #[cfg(feature = "native")]
    pub(super) fn write_history_descriptions(&mut self) {
        let Some(history) = self.history.as_mut() else {
            return;
        };
        if history.has_unwritten_descriptions()
            && history.service_descriptions(
                &mut self.state,
                &self.permission_ctx,
                &self.collaboration.local_author,
            )
        {
            let restored = history.take_restored_source();
            self.publish_history_write(restored.as_ref());
        }
    }

    /// Sync and publish what a History write changed in `source`.
    #[cfg(feature = "native")]
    fn publish_history_write(
        &mut self,
        source: Option<&crate::history_mode::HistorySourceRegistration>,
    ) {
        self.needs_sync = true;
        self.invalidate_widget_resource_cache();
        match source {
            Some(crate::history_mode::HistorySourceRegistration::HostDocument { .. }) => {
                self.broadcast_doc_delta();
            }
            Some(crate::history_mode::HistorySourceRegistration::OwnedDocument { .. }) => {
                self.publish_owned_documents();
            }
            _ => {}
        }
    }

    /// Called only after native exit layout, focus, drawing and publication.
    /// A deferred or failed frame retains its leases until a later completion.
    #[cfg(feature = "native")]
    pub(super) fn finish_history_return_frame(&mut self) {
        // Keep an outer covering lease through nested discard callbacks too.
        crate::history_mode::embedded::finish_returns(self.root_widget.as_mut());
        if let Some(history) = &mut self.history {
            if history.has_settled_control_return() {
                history.discard_pending_output(self.root_widget.as_mut());
                history.finish_control_return();
            }
        }
    }
}
