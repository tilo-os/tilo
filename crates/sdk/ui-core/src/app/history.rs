//! The native host's side of the window's History: the registered History
//! beside the live root, its door, the presentation it lays out, and the
//! native runner integration of the same retained `HistoryHost` used
//! headlessly.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::history_mode::door::HISTORY_OPERATION;
use crate::history_mode::frame::FrameworkInputFrame;
use crate::search::request::SEARCH_OPERATION;
use crate::widget::Widget;

impl App {
    /// # Errors
    /// Refuses missing, stale, foreign or unsupported installed owner registrations.
    pub fn validate_history_registration(
        &self,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        self.tree().validate_history_registration()
    }
    pub(super) fn history_presentation_roots(&self) -> Vec<&dyn Widget> {
        let mut roots = Vec::new();
        if let Some(history) = &self.history {
            roots.push(history.as_ref() as &dyn Widget);
        }
        if let Some(palette) = self
            .actions
            .action_palette
            .as_deref()
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
                .as_deref()
                .filter(|palette| !palette.history_has_custody())
                .and_then(crate::action_palette::ActionPalette::history_pending_address);
            let invocation = self
                .actions
                .action_palette
                .as_deref()
                .and_then(crate::action_palette::ActionPalette::history_pending_invocation);
            let original_arguments = self
                .actions
                .action_palette
                .as_deref()
                .and_then(|palette| palette.history_pending_continuation_arguments());
            let custody = address.as_ref().map(|address| {
                let original = self
                    .actions
                    .action_palette
                    .as_deref()
                    .and_then(|p| p.ordinary_request());
                if let Some(original) = original {
                    original.validate_owner(&self.control_domain)?;
                }
                let target = if let Some(original) = original {
                    original.target.clone()
                } else {
                    match self
                        .actions
                        .action_palette
                        .as_deref()
                        .and_then(|palette| palette.history_pending_bound_target())
                    {
                        Some(bound) => {
                            crate::bound_dispatch::local_target::LocalResourceTarget::Component(
                                bound,
                            )
                        }
                        None => crate::bound_dispatch::local_target::select_local_target(
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
            if let Some(palette) = self.actions.action_palette.as_deref_mut() {
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
    /// Reconcile the document routes with the tree, and refresh History
    /// where it needs it: the window's own, and the surfaces in the tree.
    ///
    /// Each walk of the whole tree here runs only when what it reads moved
    /// since it last ran ([`super::runtime::TreeWalks`]): a frame only an
    /// animation asked for walks nothing. What other threads deliver to a
    /// History surface is news ([`crate::history_mode::news`]), as is a
    /// request its controls queued.
    pub(super) fn refresh_framework_history(&mut self, width: f64) {
        self.restore_failed_document_publications();
        let tree = self.tree_generation();
        let routes = super::runtime::RouteInputs {
            tree,
            witness: self.bound_broker_route().cloned(),
            autosave_generation: self.autosave_generation,
            document: crate::doc::settle_mark(&self.state.doc),
        };
        if self.layout.walks.routes.as_ref() != Some(&routes) {
            #[cfg(test)]
            let _walk = crate::profiling::frame_probe::span("walk.routes");
            self.reconcile_document_routes();
            self.layout.walks.routes = Some(routes);
        }
        let status = self.window_status();
        let surfaces = super::runtime::HistorySurfaceInputs {
            news: crate::history_mode::news(),
            document: crate::doc::settle_mark(&self.state.doc),
            narrow: width < 600.0,
        };
        let mut walked = self
            .layout
            .walks
            .history
            .take()
            .filter(|walk| walk.tree == tree && walk.status == status);
        let embedded = walked
            .as_ref()
            .is_none_or(|walk| walk.surfaces_owed(&surfaces));
        // A status that only changes the text where it already shows repaints
        // that text; it is no reason to rebuild History or the app.
        #[cfg(test)]
        let walk = embedded.then(|| crate::profiling::frame_probe::span("walk.history"));
        if embedded {
            crate::history_mode::embedded::set_sync_status_in_place(
                self.root_widget.as_mut(),
                status.as_ref(),
            );
        }
        if let Some(history) = &mut self.history
            && !history.sync_status_matches(status.as_ref())
        {
            history.set_sync_status_in_place(status.as_ref(), self.root_widget.as_mut());
        }
        let found = if embedded {
            crate::history_mode::embedded::surfaces_need_refresh(
                self.root_widget.as_ref(),
                &self.state.doc,
                width,
                status.as_ref(),
            )
        } else {
            // Nothing they read moved since the last walk found them at rest.
            crate::history_mode::embedded::SurfacesWalk {
                found: walked.as_ref().is_some_and(|walk| walk.embedded),
                due: walked.as_mut().and_then(|walk| walk.due.take()),
                ..Default::default()
            }
        };
        let nested = found.needed;
        #[cfg(test)]
        drop(walk);
        // The window's own History found its root in this same tree.
        let root_valid = self.history.as_ref().is_some_and(|history| {
            walked.as_ref().is_some_and(|walk| walk.root_valid)
                || history.validate_root(self.root_widget.as_ref()).is_ok()
        });
        let invalid = self.history.is_some() && !root_valid;
        let status_changed = self
            .history
            .as_ref()
            .is_some_and(|history| !history.sync_status_matches(status.as_ref()));
        self.layout.walks.history = Some(super::runtime::HistoryWalk {
            tree,
            status: status.clone(),
            embedded: found.found,
            root_valid,
            surfaces,
            needed: found.needed,
            due: found.due,
        });
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
                history.present_entries_of(self.root_widget.as_ref());
                history.refresh(&self.state.doc);
            }
        }
        crate::history_mode::embedded::refresh(
            self.root_widget.as_mut(),
            &self.state.doc,
            width,
            status.as_ref(),
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
                if history.deliver_opened_entry(self.root_widget.as_mut()) {
                    self.needs_sync = true;
                }
            }
            if let Some(palette) = self.actions.action_palette.as_deref_mut() {
                palette.resume_after_history();
            }
        }
        self.invalidate_widget_resource_cache();
        self.layout.request_rebuild();
        self.layout.layout_dirty = true;
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

impl App {
    pub(super) fn is_history_encoded_local(&self, encoded: &tilo_actions::EncodedAction) -> bool {
        if encoded.type_id == crate::close_admission::OPERATION {
            return self.close_admission.dialog.is_some();
        }
        if encoded.type_id == SEARCH_OPERATION {
            return self.search_presented;
        }
        if encoded.type_id == HISTORY_OPERATION {
            return self.history.is_some();
        }
        if encoded.type_id == crate::host_chrome::edit::EDIT_OPERATION {
            return true;
        }
        #[cfg(feature = "native")]
        if encoded.type_id == crate::host_chrome::zoom::ZOOM_OPERATION {
            return self.accessibility.zoom.in_window();
        }
        self.history_local_resources().iter().flat_map(|resource| &resource.actions).any(|action| matches!(action.input(), tilo_actions::ActionInput::Immediate(Some(value)) if value.as_ref() == encoded))
    }
    pub(super) fn with_native_presentation<R>(
        &self,
        visit: impl FnOnce(
            &dyn Widget,
            &std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
            Option<taffy::NodeId>,
            &taffy::TaffyTree<()>,
            &crate::a11y_tree::AdmittedNodeNames,
        ) -> R,
    ) -> Result<R, String> {
        if let Some(dialog) = self.close_admission.dialog.as_deref() {
            return Ok(visit(
                dialog,
                &self.layout.node_map,
                self.focused_node,
                &self.layout.taffy,
                &self.bindings.semantic_node_names(&self.layout.node_map),
            ));
        }
        let exported = self.bindings.exported_nodes(&self.layout.node_map);
        if self.history.is_some()
            || self
                .actions
                .action_palette
                .as_deref()
                .is_some_and(|palette| palette.is_presented())
        {
            let root = FrameworkPresentation {
                roots: self.history_presentation_roots(),
                layout_root: self
                    .layout
                    .root_node
                    .ok_or("History presentation requires layout")?,
            };
            return Ok(visit(
                &root,
                &exported.nodes,
                exported.focus(self.focused_node),
                &self.layout.taffy,
                &exported.names,
            ));
        }
        Ok(visit(
            self.root_widget.as_ref(),
            &exported.nodes,
            exported.focus(self.focused_node),
            &self.layout.taffy,
            &exported.names,
        ))
    }

    pub(super) fn presentation_handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
    ) -> bool {
        if self.close_handle_event(event, scale) {
            *handled = true;
            return true;
        }
        if self.history_controls_event(event, scale) {
            *handled |= self.dispatch_framework_history_event(event, scale);
            return *handled;
        }
        let history_handled = *handled;
        if !history_handled
            && self
                .history
                .as_ref()
                .is_none_or(|history| history.live_interactive())
            && let Some(root) = self
                .history
                .as_ref()
                .and_then(|history| history.live_layout_node())
                .or(self.layout.root_node)
        {
            self.root_widget.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: root,
                    taffy: &self.layout.taffy,
                },
            );
        }
        history_handled
    }

    /// The History door's addresses: a History control in the app's tree
    /// (its header row) is bound to the same door.
    pub(super) fn refresh_history_controls(
        &self,
    ) -> std::collections::HashSet<tilo_actions::ActionAddress> {
        self.history_local_resources()
            .into_iter()
            .filter(|resource| resource.id.as_str() == crate::history_mode::door::HISTORY_RESOURCE)
            .flat_map(|resource| {
                resource.actions.into_iter().map(move |action| {
                    tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id)
                })
            })
            .collect()
    }

    pub(super) fn history_local_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let mut resources = self.close_admission.resources();
        resources.extend(self.history.as_ref().map(|history| history.door_resource()));
        if self.search_presented {
            resources.push(crate::search::search_resource());
        }
        resources.extend(self.unsaved_changes_resources());
        #[cfg(feature = "native")]
        resources.extend(self.zoom_resource());
        resources
    }

    pub(super) fn invoke_history_address_local(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>> {
        let mut resources = self.history_local_resources();
        resources.extend(self.edit_resource_for(address));
        tilo_actions::door::resolve(&resources, address)?;
        Some((|| {
            let encoded = crate::bound_dispatch::encode_resource_invocation(
                &resources, address, argument, None, false,
            )?;
            self.execute_history_encoded_local(&encoded)
                .map_err(tilo_actions::DoorError::Failed)?;
            Ok(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ))
        })())
    }

    pub(super) fn execute_history_encoded_local(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        if encoded.type_id == crate::close_admission::OPERATION {
            return self.invoke_close_encoded(encoded);
        }
        if encoded.type_id == super::local_input_capture::UNSAVED_OPERATION {
            return self.retry_unsaved_changes();
        }
        if encoded.type_id == SEARCH_OPERATION {
            if !self.search_presented {
                return Err("Search host is unavailable".into());
            }
            let request = crate::search::SearchRequest::decode(&encoded.payload)
                .map_err(|_| "Invalid Search operation")?;
            self.open_search(request);
            return Ok(());
        }
        if encoded.type_id == HISTORY_OPERATION {
            self.history
                .as_mut()
                .ok_or("History is unavailable")?
                .invoke_door(encoded)?;
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            return Ok(());
        }
        if encoded.type_id == crate::host_chrome::edit::EDIT_OPERATION {
            return self.execute_edit_operation(encoded);
        }
        #[cfg(feature = "native")]
        if encoded.type_id == crate::host_chrome::zoom::ZOOM_OPERATION {
            return self.execute_zoom_operation(encoded);
        }
        Err("Unknown History operation".into())
    }

    /// Build the layout of whatever the window presents, and check the map
    /// it registered against the live tree at once.
    pub(super) fn build_history_presentation(&mut self) {
        self.build_presentation_layout();
        self.admit_layout_maps();
    }

    fn build_presentation_layout(&mut self) {
        if let Some(dialog) = self.close_admission.dialog.as_mut() {
            self.layout.taffy = taffy::TaffyTree::new();
            dialog.sync(&self.state.doc, &self.state.root);
            let root = dialog.build_layout(
                &mut self.layout.taffy,
                &self.theme,
                &mut self.layout.node_map,
            );
            self.layout.root_node = Some(root);
            // SAFETY: this modal is the sole presented root in this fresh
            // arena. The app tree remains alive, but cannot receive input.
            unsafe {
                self.bindings.rebuild(
                    dialog.as_ref(),
                    &self.layout.taffy,
                    root,
                    &self.layout.node_map,
                );
            }
            return;
        }

        let width = self.layout.last_layout_logical.map_or(900.0, |size| size.0);
        self.refresh_framework_history(width);
        let status = self.window_status();
        crate::history_mode::embedded::refresh(
            self.root_widget.as_mut(),
            &self.state.doc,
            width,
            status.as_ref(),
        );
        // The palette owns its measured overlay geometry. Admit its absolute
        // leaf as a separate presentation root so app/legacy authority and
        // inactive boundaries cannot leak into the host confirmation surface.
        let palette_node = self
            .actions
            .action_palette
            .as_deref_mut()
            .filter(|palette| palette.is_presented())
            .map(|palette| {
                palette.build_layout(
                    &mut self.layout.taffy,
                    &self.theme,
                    &mut self.layout.node_map,
                )
            });
        if let Some(history) = &mut self.history {
            let root = history.build_composition(
                self.root_widget.as_mut(),
                &mut self.layout.taffy,
                &self.theme,
                &mut self.layout.node_map,
            );
            self.layout.root_node = Some(root);
            let mut roots: Vec<(&dyn Widget, taffy::NodeId)> = vec![(history.as_ref(), root)];
            if let (Some(node), Some(palette)) =
                (palette_node, self.actions.action_palette.as_deref())
            {
                roots.push((palette, node));
            }
            // SAFETY: App owns this settled forest and its retained live root.
            unsafe {
                self.bindings.rebuild_roots_with_retained(
                    &roots,
                    Some(self.root_widget.as_ref()),
                    &self.layout.taffy,
                    &self.layout.node_map,
                );
            }
            return;
        }
        let app = self.root_widget.build_layout(
            &mut self.layout.taffy,
            &self.theme,
            &mut self.layout.node_map,
        );
        self.layout.root_node = Some(app);
        let mut roots: Vec<(&dyn Widget, taffy::NodeId)> = vec![(self.root_widget.as_ref(), app)];
        if let (Some(node), Some(palette)) = (palette_node, self.actions.action_palette.as_deref())
        {
            roots.push((palette, node));
        }
        // SAFETY: every disjoint presentation root belongs to this App.
        unsafe {
            self.bindings
                .rebuild_roots(&roots, &self.layout.taffy, &self.layout.node_map);
        }
    }

    #[cfg(feature = "native")]
    pub(super) fn apply_history_presentation(&mut self, root: taffy::NodeId) {
        if let Some(history) = &mut self.history {
            history.apply_composition(
                self.root_widget.as_mut(),
                &self.layout.taffy,
                root,
                self.layout.content_origin,
            );
        } else {
            self.root_widget
                .apply_layout(&self.layout.taffy, root, self.layout.content_origin);
        }
    }

    /// Omit the automatically added History and Search controls at native
    /// startup. System surfaces such as a login screen have no document to
    /// browse. Their own semantic actions and accessibility remain available.
    /// Ordinary applications retain native document chrome by default.
    #[must_use]
    pub fn without_native_document_chrome(mut self) -> Self {
        self.native_document_chrome = false;
        if let Some(history) = &mut self.history {
            history.remove_open_button();
        }
        self
    }

    /// Apply the native window's default presentation before the event loop
    /// starts. Where the host's own chrome (a menu bar, the Tilo shell)
    /// presents History and Search, the window draws neither, and search is
    /// the window's palette.
    #[cfg(any(feature = "native", test))]
    pub(super) fn prepare_native_presentation(&mut self, chrome: crate::host_chrome::HostChrome) {
        if chrome.draws_in_window() {
            // Native apps use their integrated header or the platform action
            // door.
            if let Some(history) = &mut self.history {
                history.remove_fallback_bar();
                self.layout.request_rebuild();
            } else if self.native_document_chrome {
                self.search_presented = true;
            }
            return;
        }
        if let Some(history) = &mut self.history {
            history.remove_open_button();
            self.layout.request_rebuild();
        }
        if self.native_document_chrome {
            self.search_presented = true;
        }
    }

    /// This app is shown in a desktop window, whose bar presents its
    /// History and search while `tab_presents`: the app draws no History
    /// band of its own, offers its header row the control only while the
    /// bar is hidden, and its header drops its own search button.
    #[cfg(feature = "native")]
    pub(crate) fn set_window_presents_history(&mut self, tab_presents: bool) {
        self.set_window_presents_bar(tab_presents);
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            history.set_open_button(!tab_presents);
            history.set_window_presents(tab_presents);
            self.layout.request_rebuild();
        }
    }

    /// The History control the window shows for this app, if it has a
    /// History to open.
    #[cfg(feature = "native")]
    pub(crate) fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
        self.history
            .as_ref()
            .filter(|history| history.offers_control())
            .map(|history| crate::widget::HistoryControl::of(history.is_open()))
    }

    /// The window's History control was pressed: run it on this app's
    /// History door, as the History shortcut does.
    #[cfg(feature = "native")]
    pub(crate) fn invoke_window_history_control(
        &mut self,
        control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
        let history = self.history.as_mut().ok_or("History is unavailable")?;
        let intent = crate::history_mode::door::history_control_binding(control)
            .invoke(())
            .map_err(|error| error.to_string())?;
        crate::history_mode::embedded::run_door_control(history.as_mut(), &intent)
            .map_err(|error| error.to_string())?;
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        Ok(())
    }

    /// Present the window's History in the app's own header rather than a
    /// fallback bar of its own, and search as the window's palette.
    pub fn present_history_in_header(&mut self) {
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            self.layout.request_rebuild();
        } else {
            self.search_presented = true;
            self.invalidate_widget_resource_cache();
        }
    }
}

/// Immutable projection used only while traversing an owned framework composition.
struct FrameworkPresentation<'a> {
    roots: Vec<&'a dyn Widget>,
    layout_root: taffy::NodeId,
}
impl Widget for FrameworkPresentation<'_> {
    fn build_layout(
        &mut self,
        _: &mut taffy::TaffyTree<()>,
        _: &crate::Theme,
        _: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.layout_root
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.roots.clone()
    }
    fn name(&self) -> &'static str {
        "FrameworkHistoryPresentation"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.roots
            .iter()
            .map(|root| root.get_rect())
            .reduce(|left, right| left.union(right))
            .unwrap_or_default()
    }
}
