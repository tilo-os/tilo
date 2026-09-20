//! Shared History model adapter and native standalone presentation.

use super::App;
use crate::history_source::HistoryComparison;
use crate::standalone_history::{HistoryError, HistoryReadContext, HistoryTarget};
use crate::standalone_history_host::{
    HISTORY_OPERATION, HistoryCommand, SEARCH_OPERATION, StandaloneHistoryHost,
};
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
            return self.history_chrome_enabled && self.document_history.has_search();
        }
        if encoded.type_id == HISTORY_OPERATION {
            return self.history_chrome_enabled;
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
        ) -> R,
    ) -> Result<R, String> {
        if let Some(dialog) = self.close_admission.dialog.as_deref() {
            return Ok(visit(
                dialog,
                &self.layout.node_map,
                self.focused_node,
                &self.layout.taffy,
            ));
        }
        let Some(layout) = self.history_layout else {
            return Ok(visit(
                self.root_widget.as_ref(),
                &self.layout.node_map,
                self.focused_node,
                &self.layout.taffy,
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
                    &self.layout.node_map,
                    self.focused_node,
                    &self.layout.taffy,
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
            && self.history_layout.is_none_or(|layout| layout.app_visible)
            && let Some(root) = self
                .history_layout
                .map(|layout| layout.app)
                .or(self.layout.root_node)
        {
            self.root_widget.handle_event(
                event,
                scale,
                &self.input.modifiers,
                &mut self.state.doc,
                &self.state.root,
                handled,
                focus,
                root,
                &self.layout.taffy,
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
        self.document_history
            .toolbar_mut()
            .refresh_control_action_availability(&allowed);
        if let Some(search) = self.document_history.search_mut() {
            search.refresh_control_action_availability(&allowed);
        }
        if let Some(pane) = self.document_history.pane_mut() {
            pane.refresh_control_action_availability(&allowed);
        }
    }

    pub(super) fn history_local_resources(&mut self) -> Vec<tilo_actions::Resource> {
        if !self.history_chrome_enabled {
            return self.close_admission.resources();
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
        let resources = self.history_local_resources();
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
            if !self.history_chrome_enabled || !self.document_history.has_search() {
                return Err("Search host is unavailable".into());
            }
            serde_json::from_slice::<()>(&encoded.payload)
                .map_err(|_| "Invalid Search operation")?;
            self.open_action_palette();
            return Ok(());
        }
        if encoded.type_id == HISTORY_OPERATION {
            return self.invoke_history_local(encoded);
        }
        let doc = &self.state.doc;
        let path = &self.state.root;
        let result = self
            .document_history
            .pane_mut()
            .ok_or("History is not open")?
            .start_resource_operation(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                doc,
                path,
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
        let result = self.with_history_host(|host, context| host.process_pane_intents(context));
        if let Err(error) = result {
            tracing::debug!(?error, "history input refused");
        }
        self.history_changed(revision, open);
    }

    pub(super) fn history_needs_redraw(&mut self) -> bool {
        if !self.history_chrome_enabled {
            return false;
        }
        let revision = self.document_history.revision();
        let open = self.document_history.is_open();
        let dirty = self.with_history_host(|host, context| {
            host.presentation_roots(context)
                .map(|roots| roots.into_iter().any(Widget::needs_redraw))
                .unwrap_or(true)
        });
        self.history_changed(revision, open);
        dirty
    }

    pub(super) fn dispatch_history_controls(&mut self) -> usize {
        if !self.history_chrome_enabled {
            return 0;
        }
        let mut pending = Vec::new();
        while let Some(intent) = self
            .document_history
            .toolbar_mut()
            .take_control_invocation()
        {
            pending.push(intent);
        }
        if let Some(search) = self.document_history.search_mut() {
            while let Some(intent) = search.take_control_invocation() {
                pending.push(intent);
            }
        }
        if let Some(pane) = self.document_history.pane_mut() {
            while let Some(intent) = pane.take_control_invocation() {
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
            self.document_history
                .toolbar_mut()
                .on_control_invocation_result(&intent.address, &result);
            if let Some(search) = self.document_history.search_mut() {
                search.on_control_invocation_result(&intent.address, &result);
            }
            if let Some(pane) = self.document_history.pane_mut() {
                pane.on_control_invocation_result(&intent.address, &result);
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

        let app = self.root_widget.build_layout(
            &mut self.layout.taffy,
            &self.theme,
            &mut self.layout.node_map,
        );
        if !self.history_chrome_enabled {
            self.history_layout = None;
            self.layout.root_node = Some(app);
            // SAFETY: these freshly built maps describe the exclusively owned app.
            unsafe {
                self.bindings.rebuild(
                    self.root_widget.as_ref(),
                    &self.layout.taffy,
                    app,
                    &self.layout.node_map,
                );
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
                std::iter::once(layout.toolbar)
                    .chain(layout.search)
                    .chain(layout.pane),
            ),
        );
        // SAFETY: every root and node belongs to this same freshly built arena;
        // chrome is admitted separately and does not inherit app authority.
        unsafe {
            self.bindings
                .rebuild_roots(&roots, &self.layout.taffy, &self.layout.node_map);
        }
    }

    pub(super) fn apply_history_presentation(&mut self, root: taffy::NodeId) {
        if let Some(layout) = self.history_layout {
            let origin = self.document_history.apply_layout(
                layout,
                &self.layout.taffy,
                vello::kurbo::Point::ORIGIN,
            );
            if layout.app_visible {
                self.root_widget
                    .apply_layout(&self.layout.taffy, layout.app, origin);
            }
        } else {
            self.root_widget
                .apply_layout(&self.layout.taffy, root, vello::kurbo::Point::ORIGIN);
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
                            scale,
                            &self.input.modifiers,
                            doc,
                            path,
                            handled,
                            focus,
                            node,
                            &self.layout.taffy,
                        )
                    },
                )
            };
            return;
        }
        if !keyboard || self.focused_node == Some(layout.toolbar) {
            self.document_history.toolbar_mut().handle_event(
                event,
                scale,
                &self.input.modifiers,
                &mut self.state.doc,
                &self.state.root,
                handled,
                focus,
                layout.toolbar,
                &self.layout.taffy,
            );
        }
        if !*handled
            && !keyboard
            && let (Some(search), Some(node)) = (self.document_history.search_mut(), layout.search)
        {
            search.handle_event(
                event,
                scale,
                &self.input.modifiers,
                &mut self.state.doc,
                &self.state.root,
                handled,
                focus,
                node,
                &self.layout.taffy,
            );
        }
        let pane_focused = self
            .focused_node
            .is_some_and(|node| node != layout.toolbar && self.is_history_node(node));
        if !*handled
            && (!keyboard || pane_focused)
            && let Some(node) = layout.pane
            && let Some(pane) = self.document_history.pane_mut()
        {
            pane.handle_event(
                event,
                scale,
                &self.input.modifiers,
                &mut self.state.doc,
                &self.state.root,
                handled,
                focus,
                node,
                &self.layout.taffy,
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
        let mut pending = vec![layout.toolbar];
        pending.extend(layout.search);
        pending.extend(layout.pane);
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

    /// Enable shared standalone services presentation without wrapping the app
    /// root. Native windows enable this automatically; embedding remains opt-in.
    pub fn enable_document_history_chrome(&mut self) {
        self.history_chrome_enabled = true;
        self.document_history.enable_search(self.input.platform);
        self.remember_history_content();
        self.layout.structure_dirty = true;
        self.layout.layout_dirty = true;
    }

    pub(super) fn history_changed(&mut self, revision: u64, open: bool) {
        if revision != self.document_history.revision() {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.layout.structure_dirty |= open != self.document_history.is_open();
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
        self.history_changed(revision, open);
        result.map_err(|error| format!("History is unavailable: {error:?}"))
    }

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
        } else if let Some(focus) =
            self.with_history_host(|host, context| host.take_restore_focus(context))
        {
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
            .with_history_host(|host, context| host.local_resources(context))
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
        let Ok(resources) = self.with_history_host(|host, context| host.local_resources(context))
        else {
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
    fn legacy_history_observes_committed_state_without_closing_pending_edits() {
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

    #[test]
    fn replacing_legacy_document_invalidates_even_with_no_record_set() {
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
    fn layout(app: &mut App, width: u32) {
        let root = app.layout.root_node.expect("initialized layout");
        assert!(super::super::run_layout_pass(app, root, width, 700, 1.0, false, None).is_some());
    }

    #[test]
    fn native_history_local_door_preserves_root_and_excludes_broker_resources() {
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
        assert!(
            app.collect_resources()
                .iter()
                .all(|resource| !resource.id.as_str().starts_with("tilo.host:"))
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

    #[test]
    fn native_history_narrow_presentation_excludes_app_focus_and_accessibility() {
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
    #[test]
    fn native_history_redraw_probe_rebuilds_after_document_revocation() {
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

    #[test]
    fn native_history_click_finalization_preserves_pending_app_edits() {
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
    #[test]
    fn native_history_tab_finalization_preserves_pending_app_edits() {
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
        let toolbar = app.history_layout.unwrap().toolbar;
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
        let history_handled = app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus);
        assert!(!handled, "toolbar leaves Tab to host navigation");
        assert!(app.focus_tab(false));
        app.finalize_handled_native_event(&event, false, history_handled);
        assert_ne!(app.focused_node, Some(toolbar));
        assert_eq!(app.state.doc.pending_ops(), pending);
    }
    #[test]
    fn native_history_retained_widgets_survive_moving_initialized_app() {
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Text::new("Document")),
        );
        app.state.doc.put(ROOT, "title", "saved").unwrap();
        app.state.commit_action(None, None);
        app.enable_document_history_chrome();
        app.init_layout();
        let toolbar = app.history_layout.unwrap().toolbar;
        let admitted = app.layout.node_map[&toolbar];
        let mut moved = Box::new(app);
        let retained = std::ptr::from_mut(moved.document_history.toolbar_mut() as &mut dyn Widget);
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
        let retained_pane =
            std::ptr::from_mut(moved_again.document_history.pane_mut().unwrap() as &mut dyn Widget);
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
