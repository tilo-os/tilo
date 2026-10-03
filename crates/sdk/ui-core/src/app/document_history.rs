//! Shared History model adapter and native standalone presentation.

use super::App;
use crate::history_source::HistoryComparison;
use crate::search::request::SEARCH_OPERATION;
use crate::standalone_history::{HistoryError, HistoryReadContext, HistoryTarget};
use crate::standalone_history_host::{HISTORY_OPERATION, HistoryCommand, StandaloneHistoryHost};
use crate::widget::{HistoryEntry, HistoryEntryId, Widget};

#[derive(Clone, Debug)]
pub struct DocumentHistorySnapshot {
    pub entries: Vec<HistoryEntry>,
    pub selection: Vec<HistoryEntryId>,
    pub comparison: Option<HistoryComparison>,
}

impl App {
    pub(super) fn is_history_encoded_local(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> bool {
        if encoded.type_id == crate::close_admission::OPERATION {
            return self.close_admission.dialog.is_some();
        }
        if encoded.type_id == SEARCH_OPERATION {
            return self.document_history.has_search();
        }
        if encoded.type_id == HISTORY_OPERATION {
            return self.history_chrome_enabled || self.history.is_some();
        }
        if encoded.type_id == crate::host_chrome::edit::EDIT_OPERATION {
            return true;
        }
        self.history_local_resources().iter().flat_map(|resource| &resource.actions).any(|action| matches!(&action.input, tilo_actions::ActionInput::Immediate(value) if value == encoded))
    }
    pub(super) fn with_native_presentation<R>(
        &mut self,
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
                .as_ref()
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
        let Some(layout) = self.history_layout else {
            return Ok(visit(
                self.root_widget.as_ref(),
                &exported.nodes,
                exported.focus(self.focused_node),
                &self.layout.taffy,
                &exported.names,
            ));
        };
        let _ = self.history_local_resources();
        if layout.pane.is_some() != self.document_history.is_open() {
            self.layout.structure_dirty = true;
            self.layout.layout_dirty = true;
            return Err("History presentation requires layout".into());
        }
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self
                .persistence
                .records
                .as_ref()
                .and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        let rect = self
            .layout
            .taffy
            .layout(layout.root)
            .map(|layout| {
                vello::kurbo::Rect::new(
                    0.0,
                    0.0,
                    f64::from(layout.size.width),
                    f64::from(layout.size.height),
                )
            })
            .unwrap_or_default();
        self.document_history
            .with_presentation(&context, self.root_widget.as_ref(), layout, rect, |root| {
                visit(
                    root,
                    &exported.nodes,
                    exported.focus(self.focused_node),
                    &self.layout.taffy,
                    &exported.names,
                )
            })
            .map_err(|error| format!("History is unavailable: {error:?}"))
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
        self.history_handle_event(event, scale, handled, focus);
        let history_handled = *handled
            || (matches!(
                event,
                winit::event::WindowEvent::KeyboardInput { .. } | winit::event::WindowEvent::Ime(_)
            ) && self
                .focused_node
                .is_some_and(|node| self.is_history_node(node)));
        if !*handled
            && !history_handled
            && self
                .history
                .as_ref()
                .is_none_or(|history| history.live_interactive())
            && self.history_layout.is_none_or(|layout| layout.app_visible)
            && let Some(root) = self
                .history_layout
                .map(|layout| layout.app)
                .or_else(|| {
                    self.history
                        .as_ref()
                        .and_then(|history| history.live_layout_node())
                })
                .or(self.layout.root_node)
        {
            self.root_widget.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: &mut self.state.doc,
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

    pub(super) fn refresh_history_controls(&mut self) {
        let allowed = self
            .history_local_resources()
            .into_iter()
            .flat_map(|resource| {
                resource.actions.into_iter().map(move |action| {
                    tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id)
                })
            })
            .collect();
        for button in self.document_history.in_window_controls_mut() {
            crate::widget::controls::refresh_control_action_availability(button, &allowed);
        }
        if let Some(pane) = self.document_history.pane_mut() {
            crate::widget::controls::refresh_control_action_availability(pane, &allowed);
        }
    }

    pub(super) fn history_local_resources(&mut self) -> Vec<tilo_actions::Resource> {
        if !self.history_chrome_enabled {
            let mut resources = self.close_admission.resources();
            resources.extend(self.framework_history_resource());
            if self.document_history.has_search() {
                resources.push(crate::search::search_resource());
            }
            return resources;
        }
        let revision = self.document_history.revision();
        let open = self.document_history.is_open();
        let resources = self
            .with_history_host(|host, context| {
                let mut resources = host.local_resources(context)?;
                for widget in host.presentation_roots(context)? {
                    crate::widget::collect_widget_resources(
                        widget,
                        context.document,
                        &mut resources,
                    );
                }
                Ok::<_, crate::standalone_history_host::HistoryHostError>(resources)
            })
            .unwrap_or_default();
        self.history_changed(revision, open);
        let mut resources = resources;
        resources.extend(self.close_admission.resources());
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
            let (arguments, _, _) = crate::bound_dispatch::resolve_resource_invocation(
                &resources, address, argument, None, false,
            )?;
            let encoded = crate::bound_dispatch::encode_resource_action(
                &resources,
                address,
                arguments.as_ref(),
                None,
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
        if encoded.type_id == SEARCH_OPERATION {
            if !self.document_history.has_search() {
                return Err("Search host is unavailable".into());
            }
            let request = crate::search::SearchRequest::decode(&encoded.payload)
                .map_err(|_| "Invalid Search operation")?;
            self.open_search(request);
            return Ok(());
        }
        if encoded.type_id == HISTORY_OPERATION && !self.history_chrome_enabled {
            return self.invoke_framework_history(encoded);
        }
        if encoded.type_id == HISTORY_OPERATION {
            return self.invoke_history_local(encoded);
        }
        if encoded.type_id == crate::host_chrome::edit::EDIT_OPERATION {
            return self.execute_edit_operation(encoded);
        }
        let pane = self
            .document_history
            .pane_mut()
            .ok_or("History is not open")?;
        let result = crate::widget::start_without_document_writes(
            pane,
            &crate::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut self.state.doc,
            &self.state.root,
        )?;
        if result.is_none() {
            return Err("Unknown History operation".into());
        }
        self.process_history_intents();
        Ok(())
    }

    pub(super) fn process_history_intents(&mut self) {
        if !self.history_chrome_enabled {
            return;
        }
        let revision = self.document_history.revision();
        let open = self.document_history.is_open();
        let result = self.with_history_host(
            super::super::standalone_history_host::StandaloneHistoryHost::process_pane_intents,
        );
        if let Err(error) = result {
            tracing::debug!(?error, "history input refused");
        }
        self.apply_confirmed_history_restore();
        self.fulfil_history_preview();
        self.history_changed(revision, open);
    }

    /// Let History preview a selected version with the app's own root: a
    /// fresh read-only instance built by `factory`, never the live one.
    #[must_use]
    pub fn with_history_preview_root(
        mut self,
        factory: impl Fn() -> Result<Box<dyn Widget>, String> + 'static,
    ) -> Self {
        self.document_history
            .set_preview_root(std::rc::Rc::new(factory));
        self
    }

    /// Build the preview root a selection asked for, inside this app's wake
    /// scope like its live root, and hand it to the History host.
    fn fulfil_history_preview(&mut self) {
        if !self.document_history.has_preview_request() {
            return;
        }
        let root = self.document_history.preview_root().and_then(|factory| {
            let _scope = self.enter_wake_scope();
            factory()
                .inspect_err(|error| tracing::debug!(%error, "history preview root refused"))
                .ok()
        });
        self.with_history_host(|host, context| host.install_preview(root, context));
    }

    /// Write a restore the History pane confirmed into the live document,
    /// through the same undo log as every other action, then persist it.
    fn apply_confirmed_history_restore(&mut self) {
        let Some(confirmed) = self.document_history.take_confirmed_restore() else {
            return;
        };
        let author = self.local_author().clone();
        let result = self.apply_history_restore(&confirmed, author);
        self.document_history.report_restore(&result);
    }

    /// The one write behind every restore, from the History pane or an
    /// action: one attributed, undoable change, persisted like any other.
    pub(super) fn apply_history_restore(
        &mut self,
        confirmed: &crate::standalone_history::ConfirmedHistoryRestore,
        author: crate::permissions::UserId,
    ) -> Result<bool, HistoryError> {
        let result =
            crate::standalone_history::apply_restore(&mut self.state, confirmed, Some(author));
        if matches!(result, Ok(true)) {
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
        let _ = self.with_history_host(StandaloneHistoryHost::refresh);
        result
    }

    #[cfg(feature = "native")]
    pub(super) fn history_needs_redraw(&mut self) -> bool {
        if !self.history_chrome_enabled {
            return false;
        }
        let revision = self.document_history.revision();
        let open = self.document_history.is_open();
        let dirty = self.with_history_host(|host, context| {
            host.presentation_roots(context)
                .map_or(true, |roots| roots.into_iter().any(Widget::needs_redraw))
        });
        self.history_changed(revision, open);
        dirty
    }

    pub(super) fn dispatch_history_controls(&mut self) -> usize {
        if !self.history_chrome_enabled {
            return 0;
        }
        let mut pending = Vec::new();
        for button in self.document_history.in_window_controls_mut() {
            while let Some(intent) = crate::widget::controls::take_control_invocation(button) {
                pending.push(intent);
            }
        }
        if let Some(pane) = self.document_history.pane_mut() {
            while let Some(intent) = crate::widget::controls::take_control_invocation(pane) {
                pending.push(intent);
            }
        }
        let mut count = 0usize;
        for intent in pending {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments)
            );
            let result = self
                .invoke_history_address_local(&intent.address, Some(&argument))
                .unwrap_or_else(|| Err(tilo_actions::DoorError::Unknown(intent.address.clone())));
            for button in self.document_history.in_window_controls_mut() {
                crate::widget::controls::on_control_invocation_result(
                    button,
                    &intent.address,
                    &result,
                );
            }
            if let Some(pane) = self.document_history.pane_mut() {
                crate::widget::controls::on_control_invocation_result(
                    pane,
                    &intent.address,
                    &result,
                );
            }
            if result.is_ok() {
                count = count.saturating_add(1);
            }
        }
        self.process_history_intents();
        count
    }

    pub(super) fn build_history_presentation(&mut self) {
        if let Some(dialog) = self.close_admission.dialog.as_mut() {
            self.layout.taffy = taffy::TaffyTree::new();
            dialog.sync(&mut self.state.doc, &self.state.root);
            let root = dialog.build_layout(
                &mut self.layout.taffy,
                &self.theme,
                &mut self.layout.node_map,
            );
            self.layout.root_node = Some(root);
            self.history_layout = None;
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
        let status = self.document_sync_status().map(str::to_owned);
        crate::history_mode::embedded::refresh(
            self.root_widget.as_mut(),
            &self.state.doc,
            width,
            status.as_deref(),
        );
        // The palette owns its measured overlay geometry. Admit its absolute
        // leaf as a separate presentation root so app/legacy authority and
        // inactive boundaries cannot leak into the host confirmation surface.
        let palette_node = self
            .actions
            .action_palette
            .as_mut()
            .filter(|palette| palette.is_presented())
            .map(|palette| {
                palette.build_layout(
                    &mut self.layout.taffy,
                    &self.theme,
                    &mut self.layout.node_map,
                )
            });
        if let Some(history) = &mut self.history {
            self.history_layout = None;
            let root = history.build_composition(
                self.root_widget.as_mut(),
                &mut self.layout.taffy,
                &self.theme,
                &mut self.layout.node_map,
            );
            self.layout.root_node = Some(root);
            let mut roots: Vec<(&dyn Widget, taffy::NodeId)> = vec![(history.as_ref(), root)];
            if let (Some(node), Some(palette)) =
                (palette_node, self.actions.action_palette.as_ref())
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
        if !self.history_chrome_enabled {
            self.history_layout = None;
            self.layout.root_node = Some(app);
            let mut roots: Vec<(&dyn Widget, taffy::NodeId)> =
                vec![(self.root_widget.as_ref(), app)];
            if let (Some(node), Some(palette)) =
                (palette_node, self.actions.action_palette.as_ref())
            {
                roots.push((palette, node));
            }
            // SAFETY: every disjoint presentation root belongs to this App.
            unsafe {
                self.bindings
                    .rebuild_roots(&roots, &self.layout.taffy, &self.layout.node_map);
            }
            return;
        }

        let _ = self.history_local_resources();
        self.document_history
            .sync(&mut self.state.doc, &self.state.root);
        let layout = self.document_history.build_layout(
            app,
            &mut self.layout.taffy,
            &self.theme,
            &mut self.layout.node_map,
            self.history_viewport_width,
        );
        self.layout.root_node = Some(layout.root);
        self.history_layout = Some(layout);
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self
                .persistence
                .records
                .as_ref()
                .and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        let chrome = self
            .document_history
            .presentation_roots(&context)
            .unwrap_or_default();
        let mut roots: Vec<(&dyn Widget, taffy::NodeId)> = vec![(self.root_widget.as_ref(), app)];
        roots.extend(
            chrome.into_iter().zip(
                layout
                    .toolbar
                    .into_iter()
                    .chain(layout.search)
                    .chain(layout.pane)
                    .chain(layout.preview),
            ),
        );
        if let (Some(node), Some(palette)) = (palette_node, self.actions.action_palette.as_ref()) {
            roots.push((palette, node));
        }
        // SAFETY: every root and node belongs to this same freshly built arena;
        // chrome is admitted separately and does not inherit app authority.
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
            return;
        }
        if let Some(layout) = self.history_layout {
            let origin = self.document_history.apply_layout(
                layout,
                &self.layout.taffy,
                self.layout.content_origin,
            );
            if layout.app_visible {
                self.root_widget
                    .apply_layout(&self.layout.taffy, layout.app, origin);
            }
        } else {
            self.root_widget
                .apply_layout(&self.layout.taffy, root, self.layout.content_origin);
        }
    }

    pub(super) fn history_handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
    ) {
        let Some(layout) = self.history_layout else {
            return;
        };
        self.remember_history_content();
        if matches!(event, winit::event::WindowEvent::KeyboardInput { event, .. } if event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) && event.state == winit::event::ElementState::Pressed)
            && self.document_history.is_open()
        {
            self.close_document_history();
            *handled = true;
            return;
        }
        let keyboard = matches!(
            event,
            winit::event::WindowEvent::KeyboardInput { .. } | winit::event::WindowEvent::Ime(_)
        );
        if keyboard && let Some(node) = self.focused_node.filter(|node| self.is_history_node(*node))
        {
            // SAFETY: this admitted chrome node is retained by App and unbound;
            // no structural mutation occurs while forwarding its input.
            let _ = unsafe {
                crate::bound_dispatch::invoke_bound_node(
                    &self.bindings,
                    &self.layout.node_map,
                    node,
                    &mut self.state.doc,
                    &self.state.root,
                    |widget, doc, path| {
                        widget.handle_event(
                            event,
                            &mut crate::widget::EventCtx {
                                scale_factor: scale,
                                modifiers: &self.input.modifiers,
                                state: doc,
                                path,
                                handled,
                                focus_target: focus,
                                node_id: node,
                                taffy: &self.layout.taffy,
                            },
                        );
                    },
                )
            };
            return;
        }
        if let (Some(toolbar), Some(node)) = (self.document_history.toolbar_mut(), layout.toolbar)
            && (!keyboard || self.focused_node == Some(node))
        {
            toolbar.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: node,
                    taffy: &self.layout.taffy,
                },
            );
        }
        if !*handled
            && !keyboard
            && let (Some(search), Some(node)) = (self.document_history.search_mut(), layout.search)
        {
            search.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: node,
                    taffy: &self.layout.taffy,
                },
            );
        }
        let pane_focused = self
            .focused_node
            .is_some_and(|node| Some(node) != layout.toolbar && self.is_history_node(node));
        if !*handled
            && (!keyboard || pane_focused)
            && let Some(node) = layout.pane
            && let Some(pane) = self.document_history.pane_mut()
        {
            pane.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: node,
                    taffy: &self.layout.taffy,
                },
            );
        }
        if !*handled
            && !keyboard
            && let Some(node) = layout.preview
            && let Some(preview) = self.document_history.preview_mut()
        {
            preview.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &self.input.modifiers,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: node,
                    taffy: &self.layout.taffy,
                },
            );
        }
        if !layout.app_visible && !keyboard {
            *handled = true;
        }
    }
    pub(super) fn with_history_host<R>(
        &mut self,
        visit: impl FnOnce(&mut StandaloneHistoryHost, &HistoryReadContext<'_>) -> R,
    ) -> R {
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self
                .persistence
                .records
                .as_ref()
                .and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        visit(&mut self.document_history, &context)
    }

    pub(super) fn capture_history_target(&self) -> Result<HistoryTarget, HistoryError> {
        let mut binding = self
            .focused_node
            .and_then(|node| self.bindings.binding_for(node))
            .or_else(|| self.root_widget.callback_binding());
        if binding.is_none() {
            for node in self.layout.node_map.keys() {
                let Some(candidate) = self.bindings.binding_for(*node) else {
                    continue;
                };
                if binding
                    .as_ref()
                    .is_some_and(|current| current.lifetime_id != candidate.lifetime_id)
                {
                    return Err(HistoryError::Unavailable);
                }
                binding = Some(candidate);
            }
        }
        if binding.is_none()
            && self
                .persistence
                .records
                .as_ref()
                .is_some_and(|records| records.active().is_none())
        {
            return Err(HistoryError::Unavailable);
        }
        Ok(binding.map_or_else(
            || HistoryTarget::Legacy {
                app_id: crate::collaboration::local_app_id(),
                record: self
                    .persistence
                    .records
                    .as_ref()
                    .and_then(|records| records.active())
                    .cloned(),
                document_identity: self.state.document_identity(),
            },
            |binding| HistoryTarget::Bound(Box::new(binding)),
        ))
    }

    pub(super) fn is_history_node(&self, node: taffy::NodeId) -> bool {
        let Some(layout) = self.history_layout else {
            return false;
        };
        let mut pending: Vec<_> = layout.toolbar.into_iter().collect();
        pending.extend(layout.search);
        pending.extend(layout.pane);
        pending.extend(layout.preview);
        while let Some(next) = pending.pop() {
            if next == node {
                return true;
            }
            if let Ok(children) = self.layout.taffy.children(next) {
                pending.extend(children);
            }
        }
        false
    }

    pub(super) fn remember_history_content(&mut self) {
        if self
            .focused_node
            .is_some_and(|node| self.is_history_node(node))
        {
            return;
        }
        let target = self.capture_history_target();
        let focus =
            crate::widget::layout::CompositionFocus::capture_live(self.root_widget.as_ref());
        self.document_history.remember_content_target(target, focus);
    }

    /// Omit the automatically added History and Search controls at native
    /// startup. System surfaces such as a login screen have no document to
    /// browse. Their own semantic actions and accessibility remain available.
    /// Ordinary applications retain native document chrome by default.
    #[must_use]
    pub fn without_native_document_chrome(mut self) -> Self {
        self.native_document_chrome = false;
        self
    }

    /// Apply the native window's default presentation before the event loop
    /// starts: standalone roots get the shared History/Search chrome unless
    /// they opted out or a framework owner already supplies it. Its buttons
    /// are drawn in the window only where the host's own chrome (a menu bar,
    /// the Tilo shell) does not present them.
    #[cfg(any(feature = "native", test))]
    pub(super) fn prepare_native_presentation(&mut self, chrome: crate::host_chrome::HostChrome) {
        if chrome.draws_in_window() {
            if self.native_document_chrome && self.history.is_none() {
                self.enable_document_history_chrome();
            }
            return;
        }
        if let Some(history) = &mut self.history {
            history.remove_open_button();
            self.layout.structure_dirty = true;
        } else if self.native_document_chrome {
            self.enable_document_history_chrome();
        }
        if self.native_document_chrome {
            // Search is the palette, whichever History the document has; the
            // host's chrome presents it, so the window draws no copy.
            self.document_history.remove_in_window_controls();
            self.document_history.enable_search(self.input.platform);
        }
    }

    /// A registered framework History serves the same History door as the
    /// standalone one, without selecting a change: it has no session rows.
    fn framework_history_resource(&self) -> Option<tilo_actions::Resource> {
        let history = self.history.as_ref()?;
        Some(crate::standalone_history_host::history_resource(
            true,
            history.is_open(),
        ))
    }

    fn invoke_framework_history(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        let history = self.history.as_mut().ok_or("History is unavailable")?;
        match serde_json::from_slice(&encoded.payload) {
            Ok(HistoryCommand::Open) if history.request_open() => {}
            Ok(HistoryCommand::OpenAt { entry })
                if crate::standalone_history_host::decode_entry(&entry)
                    .is_ok_and(|entry| history.request_open_at(entry)) => {}
            Ok(HistoryCommand::Hide) => history.request_close(),
            _ => return Err("Unsupported History operation".into()),
        }
        self.needs_sync = true;
        self.layout.layout_dirty = true;
        Ok(())
    }

    /// Enable shared standalone services presentation without wrapping the app
    /// root. Native windows enable this automatically; embedding remains opt-in.
    pub fn enable_document_history_chrome(&mut self) {
        // A registered framework owner already supplies its own shared History
        // surface and custody. The standalone adapter serves unregistered roots.
        if self.history.is_some()
            || crate::history_mode::embedded::has_surfaces(self.root_widget.as_ref())
            || crate::history_mode::capability::provides_document_history(self.root_widget.as_ref())
        {
            return;
        }
        self.history_chrome_enabled = true;
        // The published History resource exists only where History does.
        self.invalidate_widget_resource_cache();
        self.document_history.enable_search(self.input.platform);
        self.remember_history_content();
        self.layout.structure_dirty = true;
        self.layout.layout_dirty = true;
    }

    pub(super) fn history_changed(&mut self, revision: u64, open: bool) {
        if revision != self.document_history.revision() {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.layout.structure_dirty |= open != self.document_history.is_open()
                || self.document_history.take_structure_dirty();
            if !open && self.document_history.is_open() {
                self.history_focus_pending = true;
            }
        }
    }

    pub(super) fn invoke_history_local(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        if !self.document_history.is_open() {
            self.remember_history_content();
        }
        let revision = self.document_history.revision();
        let open = self.document_history.is_open();
        let result = self.with_history_host(|host, context| host.invoke_local(encoded, context));
        self.apply_confirmed_history_restore();
        self.fulfil_history_preview();
        self.history_changed(revision, open);
        result.map_err(|error| format!("History is unavailable: {error:?}"))
    }

    #[cfg(feature = "native")]
    pub(super) fn restore_history_focus(&mut self) {
        if self.history_focus_pending {
            self.history_focus_pending = false;
            let target = self
                .focusable_order
                .iter()
                .copied()
                .find(|node| {
                    self.is_history_node(*node)
                        && self
                            .inspect_node(*node, |widget| widget.name() == "ActivityTimeline")
                            .unwrap_or(false)
                })
                .or_else(|| {
                    self.focusable_order
                        .iter()
                        .copied()
                        .find(|node| self.is_history_node(*node))
                });
            self.set_focused_node(target);
        } else if let Some(focus) = self.with_history_host(
            super::super::standalone_history_host::StandaloneHistoryHost::take_restore_focus,
        ) {
            // SAFETY: restoration runs only after building/admitting this tree.
            self.focused_node = unsafe {
                focus.restore(
                    &self.layout.node_map,
                    &self.bindings,
                    Some(&self.focusable_order),
                )
            };
        }
    }

    /// Capture the focused document before moving focus into shared chrome.
    pub fn open_document_history(&mut self) -> Result<(), String> {
        self.remember_history_content();
        let command =
            tilo_actions::EncodedAction::encode_operation(HISTORY_OPERATION, &HistoryCommand::Open)
                .map_err(|error| error.to_string())?;
        self.invoke_history_local(&command)
    }

    pub fn document_history_snapshot(&mut self) -> Result<Option<DocumentHistorySnapshot>, String> {
        let revision = self.document_history.revision();
        let open = self.document_history.is_open();
        let result = self.with_history_host(|host, context| {
            host.session(context).map(|session| {
                session.map(|session| DocumentHistorySnapshot {
                    entries: session.rows().to_vec(),
                    selection: session.selection().to_vec(),
                    comparison: session.comparison().cloned(),
                })
            })
        });
        self.history_changed(revision, open);
        result.map_err(|error| format!("History is unavailable: {error:?}"))
    }

    pub fn select_document_history(&mut self, entries: &[HistoryEntryId]) -> Result<(), String> {
        let resources = self
            .with_history_host(
                super::super::standalone_history_host::StandaloneHistoryHost::local_resources,
            )
            .map_err(|error| format!("History is unavailable: {error:?}"))?;
        let resource = resources
            .iter()
            .find(|resource| resource.kind == "tilo.host.history.session")
            .ok_or("History is not open")?;
        let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "select");
        let arguments = serde_json::json!({"entries": entries.iter().map(crate::widget::history_group::hex16).collect::<Vec<_>>()});
        let (arguments, _, _) = crate::bound_dispatch::resolve_resource_invocation(
            &resources,
            &address,
            None,
            arguments.as_object(),
            false,
        )
        .map_err(|error| error.to_string())?;
        let encoded = crate::bound_dispatch::encode_resource_action(
            &resources,
            &address,
            arguments.as_ref(),
            None,
        )
        .map_err(|error| error.to_string())?;
        self.invoke_history_local(&encoded)
    }

    pub fn close_document_history(&mut self) {
        let Ok(resources) = self.with_history_host(
            super::super::standalone_history_host::StandaloneHistoryHost::local_resources,
        ) else {
            return;
        };
        if let Some(resource) = resources
            .iter()
            .find(|resource| resource.kind == "tilo.host.history.session")
        {
            let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "close");
            if let Ok(encoded) =
                crate::bound_dispatch::encode_resource_action(&resources, &address, None, None)
            {
                let _ = self.invoke_history_local(&encoded);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ROOT, transaction::Transactable};

    #[test]
    fn legacy_history_observes_commits_and_invalidates_on_replacement() {
        // Case `legacy_history_observes_committed_state_without_closing_pending_edits`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Text::new("Document")),
            );
            app.state.doc.put(ROOT, "title", "before").unwrap();
            app.state.commit_action(None, None);
            app.state.doc.put(ROOT, "title", "typing").unwrap();
            let pending = app.state.doc.pending_ops();
            app.open_document_history().unwrap();
            let snapshot = app.document_history_snapshot().unwrap().unwrap();
            assert_eq!(snapshot.entries.len(), 1);
            app.select_document_history(&[snapshot.entries[0].id])
                .unwrap();
            assert!(
                app.document_history_snapshot()
                    .unwrap()
                    .unwrap()
                    .comparison
                    .is_some()
            );
            assert_eq!(app.state.doc.pending_ops(), pending);
            app.close_document_history();
            assert!(app.document_history_snapshot().unwrap().is_none());
        }

        // Case `replacing_legacy_document_invalidates_even_with_no_record_set`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Text::new("Document")),
            );
            app.state
                .doc
                .put(ROOT, "private", "first document")
                .unwrap();
            app.state.commit_action(None, None);
            app.open_document_history().unwrap();
            let old = app.document_history_snapshot().unwrap().unwrap().entries[0].id;
            assert!(app.state.adopt_doc(automerge::AutoCommit::new()));
            assert!(app.document_history_snapshot().is_err());
            assert!(app.select_document_history(&[old]).is_err());
            assert!(!app.document_history.is_open());
        }
    }

    fn layout(app: &mut App, width: u32) {
        let root = app.layout.root_node.expect("initialized layout");
        assert!(super::super::run_layout_pass(app, root, width, 700, 1.0, false, None).is_some());
    }

    #[test]
    fn native_history_surface_chrome_door_and_presentation_follow_the_surface_kind() {
        // Case `native_system_surface_omits_document_chrome_but_keeps_its_controls`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Button::new("login", "Log in", None)),
            )
            .without_native_document_chrome();
            // Exercise the same preparation used before the live event loop.
            app.prepare_native_presentation(crate::host_chrome::HostChrome::InWindow);
            app.init_layout();
            layout(&mut app, 1024);
            app.refresh_semantic_tree();
            assert!(app.history_layout.is_none());
            assert!(!app.history_chrome_enabled);
            let names: Vec<_> = app
                .semantic_tree()
                .iter()
                .filter_map(|node| node.name.as_deref())
                .collect();
            assert!(names.contains(&"Log in"));
            assert!(!names.contains(&"History"));
            assert!(!names.contains(&"Search"));
            let open = tilo_actions::ActionAddress::new(
                crate::standalone_history_host::HISTORY_RESOURCE,
                "open",
            );
            assert!(app.action_ui_routes(&open).is_empty());
        }

        // Case `native_document_surface_keeps_shared_chrome_by_default`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Button::new("document", "Document", None)),
            );
            app.prepare_native_presentation(crate::host_chrome::HostChrome::InWindow);
            app.init_layout();
            layout(&mut app, 1024);
            assert!(app.history_layout.is_some());
            assert!(app.history_chrome_enabled);
            let open = tilo_actions::ActionAddress::new(
                crate::standalone_history_host::HISTORY_RESOURCE,
                "open",
            );
            assert!(!app.action_ui_routes(&open).is_empty());
        }

        // Case `host_owned_chrome_draws_no_history_button_but_keeps_history`:
        // the Tilo shell and the macOS menu bar present History themselves.
        for chrome in [
            crate::host_chrome::HostChrome::TiloShell,
            crate::host_chrome::HostChrome::Native,
        ] {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Button::new("document", "Document", None)),
            );
            app.prepare_native_presentation(chrome);
            app.init_layout();
            layout(&mut app, 1024);
            let layout = app.history_layout.unwrap();
            assert!(
                layout.toolbar.is_none() && layout.search.is_none(),
                "{chrome:?}"
            );
            let open = tilo_actions::ActionAddress::new(
                crate::standalone_history_host::HISTORY_RESOURCE,
                "open",
            );
            assert!(
                app.action_ui_routes(&open).is_empty(),
                "no button in the window"
            );
            assert!(
                app.history_local_resources()
                    .iter()
                    .any(|resource| resource.id.as_str() == open.resource),
                "the host chrome's History action stays"
            );
            assert!(
                app.history_action_resources()
                    .iter()
                    .any(|resource| resource.id.as_str() == super::super::HISTORY_ACTION_RESOURCE),
                "the published History door stays"
            );
            app.open_document_history().unwrap();
            assert!(app.document_history.is_open());
        }

        // Case `native_history_chrome_stays_local_while_history_is_published_gated`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Button::new(
                    "document-control",
                    "Document control",
                    None,
                )),
            );
            app.state.doc.put(ROOT, "title", "before").unwrap();
            app.state.commit_action(None, None);
            app.state.doc.put(ROOT, "title", "pending").unwrap();
            let pending = app.state.doc.pending_ops();
            app.enable_document_history_chrome();
            app.init_layout();
            layout(&mut app, 1200);
            let original = std::ptr::from_ref(app.root_widget.as_ref());
            let open = tilo_actions::ActionAddress::new(
                crate::standalone_history_host::HISTORY_RESOURCE,
                "open",
            );
            assert!(
                !app.action_ui_routes(&open).is_empty(),
                "launcher is a visible canonical affordance"
            );
            app.invoke_history_address_local(&open, None)
                .expect("local History address")
                .unwrap();
            layout(&mut app, 1200);
            assert!(app.document_history.is_open());
            assert!(std::ptr::eq(
                original,
                std::ptr::from_ref(app.root_widget.as_ref())
            ));
            let published = app.publication_resources();
            assert!(
                published
                    .iter()
                    .chain(app.collect_resources().iter())
                    .all(|resource| !resource.id.as_str().starts_with("tilo.host:")),
                "the pane's session, selection and review controls stay local"
            );
            let history = tilo_actions::door::survey(&published)
                .into_iter()
                .filter(|descriptor| {
                    descriptor.address.resource == super::super::history_actions::RESOURCE
                })
                .map(|descriptor| (descriptor.address.action, descriptor.contract.permission))
                .collect::<std::collections::BTreeMap<_, _>>();
            use tilo_actions::PermissionRequirement::{Read, Write};
            assert_eq!(
                history,
                [
                    ("history.close", Write),
                    ("history.compare", Write),
                    ("history.list", Read),
                    ("history.open", Read),
                    ("history.restore", Write),
                    ("history.view_at", Write),
                ]
                .into_iter()
                .map(|(action, permission)| (action.to_owned(), permission))
                .collect(),
                "the broker gates each History action on the permission it declares"
            );
            let snapshot = app.document_history_snapshot().unwrap().unwrap();
            app.select_document_history(&[snapshot.entries[0].id])
                .unwrap();
            assert!(
                app.document_history_snapshot()
                    .unwrap()
                    .unwrap()
                    .comparison
                    .is_some()
            );
            assert_eq!(app.state.doc.pending_ops(), pending);
            app.close_document_history();
            layout(&mut app, 1200);
            assert!(!app.document_history.is_open());
        }

        // Case `native_history_narrow_presentation_excludes_app_focus_and_accessibility`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Button::new(
                    "private-app-control",
                    "Private app control",
                    None,
                )),
            );
            app.state.doc.put(ROOT, "title", "history").unwrap();
            app.state.commit_action(None, None);
            app.enable_document_history_chrome();
            app.init_layout();
            layout(&mut app, 1200);
            let content = app.history_layout.unwrap().app;
            app.set_focused_node(Some(content));
            app.open_document_history().unwrap();
            layout(&mut app, 640);
            assert!(!app.history_layout.unwrap().app_visible);
            assert!(
                app.focused_node
                    .is_some_and(|node| app.is_history_node(node))
            );
            assert!(
                app.focusable_order
                    .iter()
                    .all(|node| app.is_history_node(*node))
            );
            app.refresh_semantic_tree();
            assert!(
                app.semantic_tree()
                    .find_by_name("Private app control")
                    .is_none()
            );
            assert!(
                app.semantic_tree()
                    .iter()
                    .any(|node| node.name.as_deref() == Some("History"))
            );
            app.close_document_history();
            layout(&mut app, 640);
            assert!(app.history_layout.unwrap().app_visible);
            assert_eq!(app.focused_node, Some(app.history_layout.unwrap().app));
            app.refresh_semantic_tree();
            assert!(
                app.semantic_tree()
                    .find_by_name("Private app control")
                    .is_some()
            );
        }

        // Case `native_history_redraw_probe_rebuilds_after_document_revocation`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Button::new("content", "Content", None)),
            );
            app.state.doc.put(ROOT, "title", "private").unwrap();
            app.state.commit_action(None, None);
            app.enable_document_history_chrome();
            app.init_layout();
            layout(&mut app, 640);
            app.open_document_history().unwrap();
            layout(&mut app, 640);
            assert!(!app.history_layout.unwrap().app_visible);
            assert!(app.state.adopt_doc(automerge::AutoCommit::new()));
            assert!(app.history_needs_redraw());
            assert!(app.layout.structure_dirty);
            layout(&mut app, 640);
            assert!(!app.document_history.is_open());
            assert!(app.history_layout.unwrap().app_visible);
        }
    }

    fn native_click(app: &mut App, id: &str) {
        let node = *app
            .layout
            .node_map
            .keys()
            .find(|node| {
                app.inspect_node(**node, |widget| widget.test_id() == Some(id))
                    .unwrap_or(false)
            })
            .expect("retained control");
        let point = app
            .inspect_node(node, |widget| widget.get_rect().center())
            .unwrap();
        native_click_at(app, point);
    }
    fn native_click_at(app: &mut App, point: vello::kurbo::Point) {
        let events = [
            winit::event::WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(point.x, point.y),
            },
            winit::event::WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            winit::event::WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            },
        ];
        for event in events {
            let mut handled = false;
            let mut focus = None;
            let history_handled =
                app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus);
            app.dispatch_control_invocations();
            if let Some(node) = focus {
                app.set_focused_node(Some(node));
            }
            if handled {
                app.finalize_handled_native_event(&event, false, history_handled);
            }
        }
    }

    fn title(app: &App) -> Option<String> {
        use automerge::ReadDoc;
        app.state
            .doc
            .get(ROOT, "title")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
    }

    fn reopen(dir: &std::path::Path) -> App {
        let store: std::rc::Rc<dyn durable_store::DurableStore> =
            std::rc::Rc::new(app_state::FsStore::new(dir));
        let (state, writer) = crate::persist::open_in(store, "so.tilo.history-restore").unwrap();
        let mut app = App::new_with_controlled_inputs(
            state,
            Box::new(crate::widget::Button::new("document", "Document", None)),
        )
        .with_autosave(writer);
        app.prepare_native_presentation(crate::host_chrome::HostChrome::InWindow);
        app.init_layout();
        layout(&mut app, 1200);
        app
    }

    fn confirm_restore(app: &mut App) {
        let confirm = app
            .document_history
            .pane_mut()
            .and_then(|pane| pane.review_dialog())
            .expect("restore review is open")
            .primary_rect()
            .center();
        native_click_at(app, confirm);
        layout(app, 1200);
    }

    #[test]
    fn native_history_restores_a_persisted_version_as_one_undoable_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut versions = Vec::new();
        {
            let store: std::rc::Rc<dyn durable_store::DurableStore> =
                std::rc::Rc::new(app_state::FsStore::new(dir.path()));
            let (mut state, mut writer) =
                crate::persist::open_in(store, "so.tilo.history-restore").unwrap();
            for value in ["v1", "v2", "v3"] {
                state.doc.put(ROOT, "title", value).unwrap();
                state.commit_action(None, None);
                versions.push(crate::history_source::entry_id_from_hash(
                    &state.doc.get_heads()[0].to_string(),
                ));
                writer.request(&mut state.doc);
            }
            writer.flush();
        }

        let mut app = reopen(dir.path());
        assert_eq!(title(&app).as_deref(), Some("v3"));
        native_click(&mut app, "standalone-history-open");
        layout(&mut app, 1200);
        app.select_document_history(&[versions[0]]).unwrap();
        layout(&mut app, 1200);

        // Restoring v1 appends one new change stamped with its action context.
        let changes = app.state.doc.get_changes(&[]).len();
        native_click(&mut app, "standalone-history-restore");
        layout(&mut app, 1200);
        confirm_restore(&mut app);
        assert_eq!(title(&app).as_deref(), Some("v1"));
        let all = app.state.doc.get_changes(&[]);
        assert_eq!(all.len(), changes + 1, "restore appends exactly one change");
        let context = all
            .last()
            .and_then(|change| change.message())
            .and_then(tilo_actions::ActionContext::from_change_message)
            .expect("restore carries its action context");
        assert_eq!(
            context.action_label,
            crate::i18n::I18nManager::lookup_plain("standalone-history-restore-action")
        );

        // The autosaved document reopens at the restored version.
        app.autosave().unwrap().flush();
        assert_eq!(title(&reopen(dir.path())).as_deref(), Some("v1"));

        // Ctrl+Z on an app-owned document is this call in the native runner.
        let author = app.local_author().clone();
        assert!(app.state.undo_as(Some(&author), None, None).is_some());
        assert_eq!(title(&app).as_deref(), Some("v3"));

        // A review that the live document outgrows is refused, unwritten.
        layout(&mut app, 1200);
        app.select_document_history(&[versions[0]]).unwrap();
        layout(&mut app, 1200);
        native_click(&mut app, "standalone-history-restore");
        layout(&mut app, 1200);
        app.state.doc.put(ROOT, "title", "concurrent").unwrap();
        app.state.commit_action(None, None);
        let changes = app.state.doc.get_changes(&[]).len();
        confirm_restore(&mut app);
        assert_eq!(title(&app).as_deref(), Some("concurrent"));
        assert_eq!(app.state.doc.get_changes(&[]).len(), changes);
        app.refresh_semantic_tree();
        assert!(
            app.semantic_tree()
                .find_by_name(&crate::i18n::I18nManager::lookup_plain(
                    "standalone-history-restore-stale"
                ))
                .is_some(),
            "the refusal is announced"
        );
    }

    #[test]
    fn native_history_restore_requires_write() {
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Button::new("document", "Document", None)),
        );
        app.state.doc.put(ROOT, "title", "v1").unwrap();
        app.state.commit_action(None, None);
        app.state.doc.put(ROOT, "title", "v2").unwrap();
        app.state.commit_action(None, None);
        let app_id = crate::collaboration::local_app_id();
        app.permission_ctx = crate::permissions::PermissionContext::from_capabilities(
            crate::permissions::Actor::agent("reader"),
            vec![crate::permissions::CapOverride::grant(
                crate::permissions::Subject::agent("reader"),
                crate::permissions::Scope::App(app_id),
                crate::permissions::Verb::Read,
            )],
        );
        app.prepare_native_presentation(crate::host_chrome::HostChrome::InWindow);
        app.init_layout();
        layout(&mut app, 1200);
        app.open_document_history().unwrap();
        layout(&mut app, 1200);
        let first = app.document_history_snapshot().unwrap().unwrap().entries[0].id;
        app.select_document_history(&[first]).unwrap();
        layout(&mut app, 1200);
        let changes = app.state.doc.get_changes(&[]).len();
        native_click(&mut app, "standalone-history-restore");
        layout(&mut app, 1200);
        assert!(
            app.document_history
                .pane_mut()
                .is_some_and(|pane| !pane.review_is_open()),
            "a reader cannot open a restore review"
        );
        assert_eq!(title(&app).as_deref(), Some("v2"));
        assert_eq!(app.state.doc.get_changes(&[]).len(), changes);
    }

    #[test]
    fn native_history_finalization_and_moves_preserve_pending_app_state() {
        // Case `native_history_click_finalization_preserves_pending_app_edits`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Button::new(
                    "app-write",
                    "Write",
                    Some("pressed"),
                )),
            );
            app.state.doc.put(ROOT, "title", "committed").unwrap();
            app.state.commit_action(None, None);
            app.enable_document_history_chrome();
            app.init_layout();
            layout(&mut app, 1200);
            app.state.doc.put(ROOT, "title", "pending typing").unwrap();
            let pending = app.state.doc.pending_ops();
            assert!(pending > 0);
            native_click(&mut app, "standalone-history-open");
            assert!(app.document_history.is_open());
            assert_eq!(app.state.doc.pending_ops(), pending);
            layout(&mut app, 1200);
            native_click(&mut app, "standalone-history-close");
            assert!(!app.document_history.is_open());
            assert_eq!(app.state.doc.pending_ops(), pending);
            layout(&mut app, 1200);
            native_click(&mut app, "app-write");
            assert_eq!(
                app.state.doc.pending_ops(),
                0,
                "ordinary app input retains the native commit boundary"
            );
        }

        // Case `native_history_tab_finalization_preserves_pending_app_edits`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Text::new("Document")),
            );
            app.state.doc.put(ROOT, "title", "committed").unwrap();
            app.state.commit_action(None, None);
            app.enable_document_history_chrome();
            app.init_layout();
            layout(&mut app, 1200);
            app.open_document_history().unwrap();
            layout(&mut app, 1200);
            let toolbar = app.history_layout.unwrap().toolbar.unwrap();
            app.set_focused_node(Some(toolbar));
            app.state.doc.put(ROOT, "title", "pending typing").unwrap();
            let pending = app.state.doc.pending_ops();
            let key = winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab);
            let event = winit::event::WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event: winit::event::KeyEvent::synthetic(
                    winit::event::RawKeyEvent {
                        physical_key: winit::keyboard::PhysicalKey::Unidentified(
                            winit::keyboard::NativeKeyCode::Unidentified,
                        ),
                        state: winit::event::ElementState::Pressed,
                    },
                    key.clone(),
                    None,
                    winit::keyboard::KeyLocation::Standard,
                    false,
                    None,
                    key,
                ),
                is_synthetic: false,
            };
            let mut handled = false;
            let mut focus = None;
            let history_handled =
                app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus);
            assert!(!handled, "toolbar leaves Tab to host navigation");
            assert!(app.focus_tab(false));
            app.finalize_handled_native_event(&event, false, history_handled);
            assert_ne!(app.focused_node, Some(toolbar));
            assert_eq!(app.state.doc.pending_ops(), pending);
        }

        // Case `native_history_retained_widgets_survive_moving_initialized_app`.
        {
            let mut app = App::new_with_controlled_inputs(
                crate::State::new(),
                Box::new(crate::widget::Text::new("Document")),
            );
            app.state.doc.put(ROOT, "title", "saved").unwrap();
            app.state.commit_action(None, None);
            app.enable_document_history_chrome();
            app.init_layout();
            let toolbar = app.history_layout.unwrap().toolbar.unwrap();
            let admitted = app.layout.node_map[&toolbar];
            let mut moved = Box::new(app);
            let retained = std::ptr::from_mut(
                moved.document_history.toolbar_mut().unwrap() as &mut dyn Widget
            );
            assert!(
                std::ptr::addr_eq(admitted, retained),
                "admitted pointer must still name the actual retained toolbar after App moves"
            );
            let handled = moved
                .dispatch_node(toolbar, |widget, document, path| {
                    widget.perform_action(&crate::a11y::Action::Activate, document, path)
                })
                .unwrap();
            assert!(handled);
            assert_eq!(moved.dispatch_history_controls(), 1);
            assert!(moved.document_history.is_open());
            // The open pane and its inline children must remain stable too.
            moved.rebuild_structure();
            let pane = moved.history_layout.unwrap().pane.unwrap();
            let admitted_pane = moved.layout.node_map[&pane];
            let mut moved_again = Box::new(*moved);
            let retained_pane = std::ptr::from_mut(
                moved_again.document_history.pane_mut().unwrap() as &mut dyn Widget,
            );
            assert!(std::ptr::addr_eq(admitted_pane, retained_pane));
            assert_eq!(
                moved_again
                    .inspect_node(pane, |widget| widget.test_id().map(str::to_owned))
                    .unwrap()
                    .as_deref(),
                Some("standalone-history")
            );
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
    fn apply_layout(&mut self, _: &taffy::TaffyTree<()>, _: taffy::NodeId, _: vello::kurbo::Point) {
    }
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut crate::text_render::TextRenderer,
        _: &crate::Theme,
        _: &crate::presence::PresenceCtx,
    ) {
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
