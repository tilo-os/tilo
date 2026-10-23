//! Standalone History presentation for the existing headless host.
//! The operational app root and its lifecycle remain owned by `FrameHarness`.

use super::{
    ElementState, FrameHarness, HashMap, InspectionSnapshot, Key, NamedKey, NodeId, Point, Rect,
    Widget, WindowEvent, collect_inspection,
};
use crate::standalone_history::{HistoryError, HistoryReadContext, HistoryTarget};
use crate::standalone_history_host::HISTORY_OPERATION;
use crate::widget::layout::CompositionFocus;

impl FrameHarness {
    /// Enable the same standalone chrome used by a native App window. Widget
    /// and embedded-component harnesses retain their original content viewport.
    ///
    /// As in the App, a root whose framework owner already supplies its own
    /// shared History surface (a hosted component, a registered framework
    /// app) keeps that one: wrapping it in the standalone adapter as well
    /// would stack two History toolbars and squeeze the app's layout.
    pub fn enable_document_history_chrome(&mut self) {
        self.invalidate_local();
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            self.invalidate_layout_tree();
            self.relayout();
        }
        if self.history.is_some()
            || crate::history_mode::embedded::has_surfaces(self.root.as_ref())
            || crate::history_mode::capability::provides_document_history(self.root.as_ref())
        {
            return;
        }
        if !self.history_chrome_enabled {
            self.history_chrome_enabled = true;
            self.remember_history_content();
            self.invalidate_layout_tree();
            self.relayout();
        }
    }

    fn history_context(&self) -> HistoryReadContext<'_> {
        HistoryReadContext {
            document: &self.state.doc,
            active_record: self.records.as_ref().and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permissions,
        }
    }

    fn with_history<R>(
        &mut self,
        visit: impl FnOnce(
            &mut crate::standalone_history_host::StandaloneHistoryHost,
            &HistoryReadContext<'_>,
        ) -> R,
    ) -> R {
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self.records.as_ref().and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permissions,
        };
        visit(&mut self.document_history, &context)
    }

    fn capture_history_target(&self) -> Result<HistoryTarget, HistoryError> {
        crate::standalone_history_host::capture_history_target(
            self.focused
                .and_then(|node| self.bindings.binding_for(node))
                .or_else(|| self.root.callback_binding()),
            &self.bindings,
            self.node_map.keys().copied(),
            self.records.as_ref(),
            self.state.document_identity(),
        )
    }

    pub(super) fn remember_history_content(&mut self) {
        if !self.history_chrome_enabled
            || self.focused.is_some_and(|node| self.is_history_node(node))
        {
            return;
        }
        let target = self.capture_history_target();
        let focus = CompositionFocus::capture_live(self.root.as_ref());
        self.document_history.remember_content_target(target, focus);
    }

    pub(super) fn is_history_node(&self, node: NodeId) -> bool {
        self.history_layout
            .is_some_and(|layout| layout.contains(&self.taffy, node))
    }

    pub(super) fn try_presentation<R>(
        &self,
        visit: impl FnOnce(&dyn Widget, NodeId) -> R,
    ) -> Result<R, String> {
        let Some(layout) = self.history_layout else {
            return Ok(visit(self.root.as_ref(), self.child_node));
        };
        self.document_history
            .with_validated_presentation(
                &self.history_context(),
                self.root.as_ref(),
                layout,
                Rect::new(
                    0.0,
                    0.0,
                    f64::from(self.viewport.0),
                    f64::from(self.viewport.1),
                ),
                |root| visit(root, layout.root),
            )
            .map_err(|error| format!("History frame is unavailable: {error:?}"))
    }

    #[expect(
        clippy::expect_used,
        reason = "infallible current-frame inspection must fail explicitly after authority revocation; try_presentation_inspection exposes the fallible route"
    )]
    pub(super) fn presentation<R>(&self, visit: impl FnOnce(&dyn Widget, NodeId) -> R) -> R {
        self.try_presentation(visit)
            .expect("headless frame authority changed; relayout before observing")
    }

    /// Observe a frame with an explicit error if its document authority changed.
    /// No cached private History rows escape an invalid frame.
    pub fn try_presentation_inspection(&self) -> Result<InspectionSnapshot, String> {
        self.drain_component_wakes();
        self.try_presentation(|root, _| InspectionSnapshot {
            viewport: [self.viewport.0, self.viewport.1],
            keyboard_focus_owner: self.focused.and_then(|focused| {
                self.named_target_nodes()
                    .into_iter()
                    .filter_map(|(path, node)| (node == focused).then_some(path))
                    .min()
            }),
            root: collect_inspection(root, "", &mut HashMap::new()),
            presentation: None,
        })
    }

    /// Inspect the visible app-plus-host tree; operational root inspection is
    /// deliberately retained separately by `inspection()`.
    ///
    /// # Panics
    /// Panics if authority changed since layout. Use `try_presentation_inspection`
    /// when the caller can recover from a revoked frame without rebuilding it.
    pub fn presentation_inspection(&self) -> InspectionSnapshot {
        self.presentation(|root, _| InspectionSnapshot {
            viewport: [self.viewport.0, self.viewport.1],
            keyboard_focus_owner: self.focused.and_then(|focused| {
                self.named_target_nodes()
                    .into_iter()
                    .filter_map(|(path, node)| (node == focused).then_some(path))
                    .min()
            }),
            root: collect_inspection(root, "", &mut HashMap::new()),
            presentation: None,
        })
    }

    /// The doors the window answers for the app, whatever its tree
    /// publishes: opening search, and History. History serves its door where
    /// the app registers one, else the document History chrome does; a
    /// window with neither still publishes the door, unavailable, as a
    /// native window does.
    pub(super) fn host_door_resources(&self) -> Vec<tilo_actions::Resource> {
        let mut doors = vec![crate::search::search_resource()];
        if let Some(history) = &self.history {
            doors.push(history.door_resource());
        } else if !self.history_chrome_enabled {
            doors.push(crate::standalone_history_host::history_resource(
                false, false,
            ));
        } else {
            let door = self
                .history_resources_snapshot()
                .unwrap_or_default()
                .into_iter()
                .find(|resource| {
                    resource.id.as_str() == crate::standalone_history_host::HISTORY_RESOURCE
                })
                .unwrap_or_else(|| {
                    crate::standalone_history_host::history_resource(
                        true,
                        self.document_history.is_open(),
                    )
                });
            doors.push(door);
        }
        doors
    }

    pub(super) fn history_resources_snapshot(&self) -> Result<Vec<tilo_actions::Resource>, String> {
        let Some(layout) = self.history_layout else {
            return Ok(Vec::new());
        };
        self.document_history
            .validated_local_resources(&self.history_context(), layout)
            .map_err(|error| format!("History is unavailable: {error:?}"))
    }

    fn refresh_history_resources(&mut self) -> Result<Vec<tilo_actions::Resource>, String> {
        self.with_history(|host, context| {
            let mut resources = host.local_resources(context)?;
            for root in host.presentation_roots(context)? {
                crate::widget::collect_widget_resources(root, context.document, &mut resources);
            }
            Ok::<_, crate::standalone_history_host::HistoryHostError>(resources)
        })
        .map_err(|error| format!("History is unavailable: {error:?}"))
    }

    /// Local host actions are intentionally separate from `resources()`, the
    /// application's publishable catalogue. No remote actor is granted this door.
    pub fn local_history_resources(&mut self) -> Result<Vec<tilo_actions::Resource>, String> {
        self.invalidate_local();
        if !self.history_chrome_enabled {
            return Ok(Vec::new());
        }
        self.remember_history_content();
        let result = self
            .with_history(
                super::super::standalone_history_host::StandaloneHistoryHost::local_resources,
            )
            .map_err(|error| format!("History is unavailable: {error:?}"));
        self.relayout();
        result?;
        self.history_resources_snapshot()
    }

    /// Invoke only this host's local History action surface. This is not a
    /// collaborator transport entry point and never searches app destinations.
    ///
    /// # Errors
    /// Refuses disabled chrome, stale sessions, denied reads and invalid arguments.
    pub fn invoke_local_history_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<(), String> {
        self.invalidate_local();
        self.run_local_history_action(address, argument)?;
        self.idle_control_tick();
        self.relayout();
        Ok(())
    }

    /// Open History as the History shortcut does: the app's registered
    /// History where it has one, else the document History chrome.
    pub fn open_history(&mut self) -> Result<(), String> {
        self.invalidate_local();
        if let Some(history) = &self.history {
            if !history.request_open() {
                return Err("History is unavailable".into());
            }
            self.relayout();
            return Ok(());
        }
        self.invoke_local_history_action(
            &tilo_actions::ActionAddress::new(
                crate::standalone_history_host::HISTORY_RESOURCE,
                "open",
            ),
            None,
        )
    }

    /// Resolve and run one History door action; the next frame presents it.
    fn run_local_history_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<(), String> {
        // The app's registered History owns the door wherever it has one,
        // as in the App: the standalone chrome never stands in for it.
        if let Some(history) = &mut self.history {
            let resources = [history.door_resource()];
            let (arguments, _, _) = crate::bound_dispatch::resolve_resource_invocation(
                &resources, address, argument, None, false,
            )
            .map_err(|error| error.to_string())?;
            let encoded = crate::bound_dispatch::encode_resource_action(
                &resources,
                address,
                arguments.as_ref(),
                None,
            )
            .map_err(|error| error.to_string())?;
            return history.invoke_door(&encoded);
        }
        if !self.history_chrome_enabled {
            return Err("History chrome is not enabled".into());
        }
        self.remember_history_content();
        let resources = self
            .with_history(|host, context| {
                let mut resources = host.local_resources(context)?;
                for root in host.presentation_roots(context)? {
                    crate::widget::collect_widget_resources(root, context.document, &mut resources);
                }
                Ok::<_, crate::standalone_history_host::HistoryHostError>(resources)
            })
            .map_err(|error| format!("History is unavailable: {error:?}"))?;
        let (arguments, _, _) = crate::bound_dispatch::resolve_resource_invocation(
            &resources, address, argument, None, false,
        )
        .map_err(|error| error.to_string())?;
        let encoded = crate::bound_dispatch::encode_resource_action(
            &resources,
            address,
            arguments.as_ref(),
            None,
        )
        .map_err(|error| error.to_string())?;
        self.execute_history_operation(&encoded)
    }

    /// A control in the app's tree bound to the window's History door (the
    /// History control in the app's header row) runs there, as the History
    /// shortcut does, never as an app action. An app `owner` hosted in a tile
    /// opens that tile's History. Returns whether it was one.
    pub(super) fn take_history_invocation(
        &mut self,
        intent: &tilo_actions::ControlInvocation,
        owner: Option<crate::history_mode::HistoryOwner>,
    ) -> bool {
        if intent.address.resource != crate::standalone_history_host::HISTORY_RESOURCE {
            return false;
        }
        let embedded = owner.and_then(|owner| {
            crate::history_mode::embedded::invoke_door(self.root.as_mut(), &owner, intent)
        });
        let result = embedded.unwrap_or_else(|| {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(intent.arguments.clone())
            );
            self.run_local_history_action(&intent.address, Some(&argument))
                .map_err(tilo_actions::DoorError::Failed)
        });
        if let Err(error) = &result {
            tracing::debug!(?error, "headless: History control refused");
        }
        let result = result.map(|()| {
            crate::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
        });
        crate::widget::controls::on_control_invocation_result(
            self.root.as_mut(),
            &intent.address,
            &result,
        );
        true
    }

    fn execute_history_operation(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        let was_open = self.document_history.is_open();
        let result = if encoded.type_id == HISTORY_OPERATION {
            self.with_history(|host, context| host.invoke_local(encoded, context))
                .map_err(|error| format!("History is unavailable: {error:?}"))
        } else {
            let pane = self
                .document_history
                .pane_mut()
                .ok_or("History is not open")?;
            crate::widget::start_without_document_writes(
                pane,
                &crate::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut crate::widget::Edits::lend(&mut self.state.doc),
                &self.state.root,
            )?
            .ok_or("Unknown local History operation")?;
            self.with_history(
                super::super::standalone_history_host::StandaloneHistoryHost::process_pane_intents,
            )
            .map(|_| ())
            .map_err(|error| format!("History is unavailable: {error:?}"))
        };
        self.apply_confirmed_history_restore();
        self.fulfil_history_preview();
        self.history_focus_pending |= !was_open && self.document_history.is_open();
        result
    }

    /// Let History preview a selected version with a fresh read-only instance
    /// of the app's root, as the native App does.
    pub fn set_history_preview_root(
        &mut self,
        factory: impl Fn() -> Result<Box<dyn Widget>, String> + 'static,
    ) {
        self.invalidate_local();
        self.document_history
            .set_preview_root(std::rc::Rc::new(factory));
    }

    fn fulfil_history_preview(&mut self) {
        if !self.document_history.has_preview_request() {
            return;
        }
        let root = self
            .document_history
            .preview_root()
            .and_then(|factory| factory().ok());
        self.with_history(|host, context| host.install_preview(root, context));
        self.invalidate_layout_tree();
    }

    /// The headless counterpart of the App's restore door: the same checked
    /// write through the same undo log, attributed to this harness's author.
    fn apply_confirmed_history_restore(&mut self) {
        let Some(confirmed) = self.document_history.take_confirmed_restore() else {
            return;
        };
        let result = crate::standalone_history::apply_restore(
            &mut self.state,
            &confirmed,
            Some(self.author.clone()),
        );
        if matches!(result, Ok(true)) {
            self.invalidate_layout_tree();
        }
        let _ = self
            .with_history(super::super::standalone_history_host::StandaloneHistoryHost::refresh);
        self.document_history.report_restore(&result);
    }

    pub(super) fn history_tick(&mut self) -> Result<(), String> {
        for intent in self.document_history.take_control_invocations() {
            let result = (|| {
                let resources = self
                    .refresh_history_resources()
                    .map_err(tilo_actions::DoorError::Failed)?;
                let encoded = crate::bound_dispatch::encode_resource_action(
                    &resources,
                    &intent.address,
                    Some(&intent.arguments),
                    None,
                )?;
                self.execute_history_operation(&encoded)
                    .map_err(tilo_actions::DoorError::Failed)?;
                Ok(crate::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ))
            })();
            self.document_history
                .report_control_result(&intent.address, &result);
            result.map_err(|error| error.to_string())?;
        }
        let result = self
            .with_history(
                super::super::standalone_history_host::StandaloneHistoryHost::process_pane_intents,
            )
            .map(|_| ())
            .map_err(|error| format!("History is unavailable: {error:?}"));
        self.apply_confirmed_history_restore();
        self.fulfil_history_preview();
        // Keep Accepted app jobs alive while browsing chrome. This existing idle
        // boundary records completed actions but does not commit unrelated typing.
        self.idle_control_tick();
        result
    }

    pub(super) fn prepare_history_frame(&mut self) -> (bool, bool) {
        if !self.history_chrome_enabled {
            return (false, false);
        }
        let revision = self.document_history.revision();
        let presented_open = self.history_layout.and_then(|layout| layout.pane).is_some();
        self.remember_history_content();
        // A revoked session is purged by the shared controller before rebuilding.
        let _ = self
            .with_history(super::super::standalone_history_host::StandaloneHistoryHost::refresh);
        let resources = self.refresh_history_resources().unwrap_or_default();
        let allowed = resources
            .into_iter()
            .flat_map(|resource| {
                resource.actions.into_iter().map(move |action| {
                    tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id)
                })
            })
            .collect();
        self.document_history.refresh_control_availability(&allowed);
        self.document_history
            .sync(&self.state.doc, &self.state.root);
        let reshaped = self.document_history.take_structure_dirty();
        (
            revision != self.document_history.revision(),
            presented_open != self.document_history.is_open() || reshaped,
        )
    }

    pub(super) fn build_history_frame(&mut self, palette_node: Option<NodeId>) -> NodeId {
        if !self.history_chrome_enabled {
            self.history_layout = None;
            // SAFETY: the original retained root and freshly built arena are exclusive.
            unsafe {
                self.bindings.rebuild(
                    self.root.as_ref(),
                    &self.taffy,
                    self.child_node,
                    &self.node_map,
                );
            }
            return self.child_node;
        }
        let layout = self.document_history.build_layout(
            self.child_node,
            &mut self.taffy,
            &self.theme,
            &mut self.node_map,
            f64::from(self.viewport.0),
        );
        self.admit_layout_maps();
        let palette_node = palette_node.filter(|&node| {
            self.taffy
                .add_child(layout.root, node)
                .inspect_err(|error| {
                    tracing::error!(%error, "headless: palette could not join the history frame");
                })
                .is_ok()
        });
        self.history_layout = Some(layout);
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self.records.as_ref().and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permissions,
        };
        let chrome = self
            .document_history
            .presentation_roots(&context)
            .unwrap_or_default();
        let mut roots: Vec<(&dyn Widget, NodeId)> = vec![(
            self.history.as_ref().map_or_else(
                || self.root.as_ref(),
                |history| history.as_ref() as &dyn Widget,
            ),
            self.child_node,
        )];
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
        if let (Some(node), Some(palette)) = (palette_node, self.action_palette.as_deref()) {
            roots.push((palette, node));
        }
        // SAFETY: all retained roots use the same fresh arena, admitted independently
        // so app callback authority never propagates into host chrome.
        unsafe {
            self.bindings.rebuild_roots_with_retained(
                &roots,
                self.history.as_ref().map(|_| self.root.as_ref()),
                &self.taffy,
                &self.node_map,
            );
        }
        layout.root
    }

    pub(super) fn apply_history_frame(&mut self) {
        if let Some(layout) = self.history_layout {
            let origin = self
                .document_history
                .apply_layout(layout, &self.taffy, Point::ORIGIN);
            if layout.app_visible {
                self.root.apply_layout(&self.taffy, self.child_node, origin);
            }
        } else {
            self.root
                .apply_layout(&self.taffy, self.child_node, Point::ORIGIN);
        }
    }

    pub(super) fn restore_history_focus(&mut self) {
        if !self.history_chrome_enabled {
            return;
        }
        let order = self.focus_order();
        if self.history_focus_pending {
            self.history_focus_pending = false;
            let target = order
                .iter()
                .copied()
                .find(|node| {
                    self.is_history_node(*node)
                        && self
                            .inspect_node(*node, |widget| widget.name() == "ActivityTimeline")
                            .unwrap_or(false)
                })
                .or_else(|| {
                    order
                        .iter()
                        .copied()
                        .find(|node| self.is_history_node(*node))
                });
            self.set_focused(target);
        } else if let Some(focus) = self.with_history(
            super::super::standalone_history_host::StandaloneHistoryHost::take_restore_focus,
        ) {
            // SAFETY: the restored tree is admitted and visible targets bound eligibility.
            let target = unsafe { focus.restore(&self.node_map, &self.bindings, Some(&order)) };
            self.set_focused(target);
        }
        if self.focused.is_some_and(|node| !order.contains(&node)) {
            self.set_focused(order.first().copied());
        }
    }

    fn close_history_session(&mut self) {
        let resources = self
            .with_history(
                super::super::standalone_history_host::StandaloneHistoryHost::local_resources,
            )
            .unwrap_or_default();
        if let Some(resource) = resources
            .iter()
            .find(|resource| resource.kind == "tilo.host.history.session")
        {
            let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "close");
            if let Ok(encoded) =
                crate::bound_dispatch::encode_resource_action(&resources, &address, None, None)
            {
                let _ = self.execute_history_operation(&encoded);
            }
        }
    }

    pub(super) fn history_event(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
    ) -> bool {
        let Some(layout) = self.history_layout else {
            return false;
        };
        self.remember_history_content();
        // Input must not operate a frame whose captured document is no longer readable.
        if self
            .document_history
            .validate_presentation(&self.history_context(), layout)
            .is_err()
        {
            self.prepare_history_frame();
            *handled = true;
            return true;
        }
        if matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.logical_key == Key::Named(NamedKey::Escape) && event.state == ElementState::Pressed)
            && self.document_history.is_open()
        {
            self.close_history_session();
            *handled = true;
            return true;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let point = Point::new(position.x / self.input_scale, position.y / self.input_scale);
            self.history_pointer_owned = self
                .document_history
                .controls_mut()
                .any(|control| control.get_rect().contains(point));
        }
        let keyboard = matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        );
        if keyboard {
            let Some(node) = self.focused.filter(|node| self.is_history_node(*node)) else {
                // A managed History surface owns its modal keyboard routing
                // even when standalone chrome has hidden the live app. Let
                // the existing HistoryHost route below receive Enter/Escape;
                // only a genuinely hidden ordinary app has no recipient.
                return self.history.is_none() && !layout.app_visible;
            };
            // SAFETY: focused retained chrome is unbound and the admitted tree
            // remains exclusive for the duration of this event.
            let _ = unsafe {
                crate::bound_dispatch::invoke_bound_node(
                    &self.bindings,
                    &self.node_map,
                    node,
                    &mut self.state.doc,
                    &self.state.root,
                    |widget, doc, path| {
                        widget.handle_event(
                            event,
                            &mut crate::widget::EventCtx {
                                scale_factor: self.input_scale,
                                modifiers: &self.modifiers,
                                state: crate::widget::Edits::lend(doc),
                                path,
                                handled,
                                focus_target: focus,
                                node_id: node,
                                taffy: &self.taffy,
                            },
                        );
                    },
                )
            };
            return true;
        }
        if let (Some(toolbar), Some(node)) = (self.document_history.toolbar_mut(), layout.toolbar) {
            toolbar.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: self.input_scale,
                    modifiers: &self.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: node,
                    taffy: &self.taffy,
                },
            );
        }
        if !*handled
            && let Some(node) = layout.pane
            && let Some(pane) = self.document_history.pane_mut()
        {
            pane.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: self.input_scale,
                    modifiers: &self.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: node,
                    taffy: &self.taffy,
                },
            );
        }
        if !*handled
            && let Some(node) = layout.preview
            && let Some(preview) = self.document_history.preview_mut()
        {
            preview.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: self.input_scale,
                    modifiers: &self.modifiers,
                    state: crate::widget::Edits::lend(&mut self.state.doc),
                    path: &self.state.root,
                    handled,
                    focus_target: focus,
                    node_id: node,
                    taffy: &self.taffy,
                },
            );
        }
        if !layout.app_visible {
            *handled = true;
        }
        *handled
            || (self.history_pointer_owned
                && matches!(
                    event,
                    WindowEvent::CursorMoved { .. }
                        | WindowEvent::MouseInput { .. }
                        | WindowEvent::MouseWheel { .. }
                ))
    }

    pub(super) fn history_named_action(
        &mut self,
        node: NodeId,
        semantic_target: Option<&str>,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        self.try_presentation(|_, _| ())?;
        let handled = self
            .dispatch_node(node, |widget, doc, path| {
                if let Some(target) = semantic_target {
                    widget.perform_semantic_action(
                        target,
                        action,
                        &mut crate::widget::Edits::lend(doc),
                        path,
                    )
                } else {
                    widget.perform_action(action, &[], &mut crate::widget::Edits::lend(doc), path)
                }
            })
            .map_err(|error| format!("History action refused: {error:?}"))?;
        if matches!(action, crate::a11y::Action::Focus) {
            self.set_focused(Some(node));
        }
        if !handled
            && matches!(
                action,
                crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck
            )
        {
            let point = self
                .inspect_node(node, |widget| widget.get_rect().center())
                .map_err(|error| format!("History target refused: {error:?}"))?;
            self.click_at(point.x, point.y);
        } else if !handled && !matches!(action, crate::a11y::Action::Focus) {
            return Err("History action is unsupported".into());
        }
        self.history_tick()?;
        self.relayout();
        Ok(())
    }

    pub(super) fn history_a11y_resource(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Option<Result<(), String>> {
        let resources = match self.history_resources_snapshot() {
            Ok(resources) => resources,
            Err(error) => return Some(Err(error)),
        };
        for resource in resources {
            for spec in resource.actions {
                if crate::accesskit_bridge::resource_action_node_id(resource.id.as_str(), &spec.id)
                    == target
                {
                    let argument = match action {
                        crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck => None,
                        crate::a11y::Action::SetValue(value) => Some(value.as_str()),
                        _ => {
                            return Some(Err(
                                "History resource action does not support this verb".into()
                            ));
                        }
                    };
                    return Some(self.invoke_local_history_action(
                        &tilo_actions::ActionAddress::new(resource.id.as_str(), &spec.id),
                        argument,
                    ));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{Spec, write_spec_outputs};
    use crate::widget::{Button, HistoryEntryId};
    use crate::{PermissionContext, State, Theme};
    use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
    use std::cell::Cell;
    use std::rc::Rc;
    use taffy::TaffyTree;

    fn fixture() -> (FrameHarness, Vec<HistoryEntryId>, Rc<Cell<u32>>) {
        let mut state = State::new();
        state.doc.put(ROOT, "value", "before").unwrap();
        state.doc.commit();
        let ids = ["first", "middle", "last"]
            .into_iter()
            .map(|value| {
                state.doc.put(ROOT, "value", value).unwrap();
                crate::history_source::entry_id_from_hash(&state.doc.commit().unwrap().to_string())
            })
            .collect();
        let calls = Rc::new(Cell::new(0));
        let called = calls.clone();
        let root = Box::new(
            Button::new("app-operation", "App operation")
                .on_view(move || called.set(called.get() + 1)),
        );
        let mut driver = FrameHarness::new_with_controlled_inputs(
            state,
            root,
            (1200, 700),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("history-fixture".into()),
        );
        driver.enable_document_history_chrome();
        (driver, ids, calls)
    }

    /// A modal is presented in the window's top layer: its scrim dims the
    /// top of app content too, not only the pane the dialog is mounted in.
    #[test]
    fn a_modal_scrim_covers_the_top_of_app_content() {
        fn bar_pixel(open: bool) -> [u8; 4] {
            let body = crate::widget::Column::new()
                .push(Box::new(crate::widget::Text::new("Draft")))
                .push(Box::new(
                    crate::widget::ModalDialog::new(
                        "Discard draft?",
                        crate::widget::Text::new("The draft is not saved."),
                    )
                    .open(open)
                    .secondary_action("Keep", || {})
                    .primary_action("Discard", || {}),
                ));
            let mut driver = FrameHarness::new_with_theme(
                State::new(),
                Box::new(body),
                (900, 600),
                Theme::light(),
            );
            driver.enable_document_history_chrome();
            driver.relayout();
            let rgba = driver.render_rgba();
            let (x, y) = (880_usize, 8_usize);
            let at = (y * 900 + x) * 4;
            [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
        }
        let plain = bar_pixel(false);
        let dimmed = bar_pixel(true);
        assert!(
            u16::from(dimmed[0]) + 20 < u16::from(plain[0]),
            "the top of app content is under the scrim: {plain:?} -> {dimmed:?}"
        );
    }

    /// Where the app draws the window's header row, History is one glyph at
    /// its trailing edge: no bar above the app, and the glyph reads as on
    /// while History shows and closes it again.
    #[test]
    fn the_history_control_stands_in_the_app_header_row() {
        fn history_toggled(driver: &FrameHarness) -> Option<accesskit::Toggled> {
            driver
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| {
                    node.label() == Some("History") && node.role() == accesskit::Role::Button
                })
                .and_then(|(_, node)| node.toggled())
        }
        let mut state = State::new();
        for value in ["before", "after"] {
            state.doc.put(ROOT, "value", value).unwrap();
            state.doc.commit();
        }
        let shell = crate::widget::NavShell::new()
            .content_header("Notes", vec![])
            .content(Box::new(crate::widget::Text::new("Body")));
        let mut driver = FrameHarness::new_with_controlled_inputs(
            state,
            Box::new(shell),
            (1200, 700),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("history-header".into()),
        );
        driver.enable_document_history_chrome();
        driver.relayout();

        let targets = driver.targets();
        assert_eq!(
            driver
                .action_ui_routes(&tilo_actions::ActionAddress::new(
                    crate::standalone_history_host::HISTORY_RESOURCE,
                    "open",
                ))
                .len(),
            1,
            "one visible button bound to the canonical History action"
        );
        assert!(
            !targets.contains_key(crate::standalone_history_host::BAR_BUTTON_ID),
            "the window draws no History bar above a header row"
        );
        let control = targets["nav-shell-history"];
        let header = f64::from(Theme::dark().tokens.metrics.header_height);
        assert!(
            control.x1 > 1200.0 - 24.0 && control.y1 <= header + 8.0,
            "History stands in the window's top-trailing corner: {control:?}"
        );
        assert_eq!(history_toggled(&driver), Some(accesskit::Toggled::False));

        driver.click_named("nav-shell-history").unwrap();
        assert!(
            driver.targets().contains_key("standalone-history"),
            "the glyph opens History"
        );
        assert_eq!(
            history_toggled(&driver),
            Some(accesskit::Toggled::True),
            "and reads as on while it shows"
        );

        driver.click_named("nav-shell-history").unwrap();
        assert!(
            !driver.targets().contains_key("standalone-history"),
            "pressed again, it closes History"
        );
        assert_eq!(history_toggled(&driver), Some(accesskit::Toggled::False));
    }

    /// The host door remains available without adding a second app header.
    #[test]
    fn an_app_without_a_history_slot_starts_at_zero_and_opens_the_published_door() {
        let (mut driver, _, _) = fixture();
        driver.relayout();
        assert!(
            !driver
                .targets()
                .contains_key(crate::standalone_history_host::BAR_BUTTON_ID)
        );
        assert_eq!(
            driver.targets()["app-operation"].y0.total_cmp(&0.0),
            std::cmp::Ordering::Equal
        );
        assert!(driver.history_layout.unwrap().toolbar.is_none());
        let open = open_address();
        let action = crate::accesskit_bridge::resource_action_node_id(&open.resource, &open.action);
        driver
            .perform_a11y_action(action, &crate::a11y::Action::Activate)
            .unwrap();
        assert!(driver.document_history.is_open());
        assert!(driver.targets().contains_key("standalone-history"));
    }

    fn open_address() -> tilo_actions::ActionAddress {
        tilo_actions::ActionAddress::new(crate::standalone_history_host::HISTORY_RESOURCE, "open")
    }

    fn session_address(driver: &mut FrameHarness, action: &str) -> tilo_actions::ActionAddress {
        let resources = driver.local_history_resources().unwrap();
        let session = resources
            .iter()
            .find(|resource| resource.kind == "tilo.host.history.session")
            .unwrap();
        tilo_actions::ActionAddress::new(session.id.as_str(), action)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn headless_history_keeps_the_canonical_door_nodes_and_focus_contract() {
        // phase: headless_history_cli_inspect_preserves_operational_root_contract
        {
            let (mut driver, _, _) = fixture();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            driver.resize((640, 700)).unwrap();
            let expected = serde_json::to_value(driver.inspection()).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join("inspect.json");
            let spec = Spec {
                inspect: Some(output.to_str().unwrap().to_owned()),
                ..Spec::default()
            };
            write_spec_outputs(&spec, &mut driver).unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
            assert_eq!(
                json["root"], expected["root"],
                "CLI consumers retain the complete operational root shape"
            );
            assert_eq!(json["viewport"], expected["viewport"]);
            assert_eq!(json["root"]["widget"], "Button");
            assert_eq!(json["root"]["test_id"], "app-operation");
            assert!(json["root"]["children"].as_array().unwrap().is_empty());
            let visible = serde_json::to_string(&json["presentation"]).unwrap();
            assert!(!visible.contains("standalone-history-open"));
            assert!(visible.contains("standalone-history-close"));
            assert!(
                !visible.contains("app-operation"),
                "narrow presentation excludes app content without changing root inspection"
            );
            assert!(
                !serde_json::to_string(&json["root"])
                    .unwrap()
                    .contains("standalone-history")
            );
        }
        // phase: headless_history_uses_visible_canonical_door_and_keeps_app_publication_private
        {
            let (mut driver, _, calls) = fixture();
            let root = std::ptr::from_ref(driver.root.as_ref());
            let published_before: Vec<_> = driver
                .resources()
                .into_iter()
                .map(|resource| resource.id)
                .collect();
            assert!(!driver.targets().contains_key("standalone-history-open"));
            assert_eq!(
                driver.targets()["app-operation"].y0.total_cmp(&0.0),
                std::cmp::Ordering::Equal
            );
            assert!(
                driver
                    .local_history_resources()
                    .unwrap()
                    .iter()
                    .any(|resource| {
                        resource.id.as_str() == open_address().resource
                            && resource.actions.iter().any(|action| action.id == "open")
                    })
            );
            let open = open_address();
            let accesskit_open =
                crate::accesskit_bridge::resource_action_node_id(&open.resource, &open.action);
            driver
                .perform_a11y_action(accesskit_open, &crate::a11y::Action::Activate)
                .unwrap();
            assert!(driver.targets().contains_key("standalone-history"));
            assert!(driver.a11y_node_named("Close history").is_some());
            let published_after: Vec<_> = driver
                .resources()
                .into_iter()
                .map(|resource| resource.id)
                .collect();
            assert_eq!(
                published_before, published_after,
                "private host controls never join app publication"
            );
            let host_resources: Vec<_> = driver
                .resources()
                .into_iter()
                .filter(|resource| resource.id.as_str().starts_with("tilo.host:"))
                .collect();
            assert_eq!(
                host_resources.len(),
                1,
                "private history controls stay local"
            );
            assert_eq!(host_resources[0].id.as_str(), "tilo.host:search");
            assert!(host_resources[0].children.is_empty());
            assert_eq!(
                serde_json::to_value(tilo_actions::door::survey(&host_resources)).unwrap(),
                serde_json::to_value(tilo_actions::door::survey(&[
                    crate::search::search_resource()
                ]))
                .unwrap(),
                "only the canonical public search contract is exposed"
            );
            let local = driver.local_history_resources().unwrap();
            let session = local
                .iter()
                .find(|resource| resource.kind == "tilo.host.history.session")
                .unwrap();
            for action in ["select", "compare", "close"] {
                assert!(session.actions.iter().any(|spec| spec.id == action));
            }
            let close = session_address(&mut driver, "close");
            driver.invoke_local_history_action(&close, None).unwrap();
            assert!(std::ptr::eq(root, std::ptr::from_ref(driver.root.as_ref())));
            driver
                .perform_named("app-operation", &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(
                calls.get(),
                1,
                "operational app root still receives its own action"
            );
        }
        // phase: headless_history_retained_nodes_survive_host_move
        {
            let (mut driver, _, _) = fixture();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            let original = driver.node_map[&driver.history_layout.unwrap().pane.unwrap()];
            let mut moved = Box::new(driver);
            let actual =
                std::ptr::from_mut(moved.document_history.pane_mut().unwrap() as &mut dyn Widget);
            assert!(
                std::ptr::addr_eq(original, actual),
                "admission still names the actually retained widget"
            );
            assert!(moved.document_history.is_open());
            moved
                .perform_named("standalone-history-close", &crate::a11y::Action::Activate)
                .unwrap();
            assert!(!moved.document_history.is_open());
        }
        // phase: headless_history_narrow_frame_pairs_nodes_and_restores_original_focus
        {
            fn paired(widget: &dyn Widget, node: NodeId, taffy: &TaffyTree<()>) {
                let nodes = taffy.children(node).unwrap();
                let children = widget.children();
                assert_eq!(
                    children.len(),
                    nodes.len(),
                    "{} presentation topology",
                    widget.name()
                );
                for (child, node) in children.into_iter().zip(nodes) {
                    paired(child, node, taffy);
                }
            }
            let (mut driver, _, _) = fixture();
            driver
                .perform_named("app-operation", &crate::a11y::Action::Focus)
                .unwrap();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            assert!(driver.targets().contains_key("app-operation"));
            driver.resize((640, 700)).unwrap();
            driver.presentation(|root, node| paired(root, node, &driver.taffy));
            assert!(!driver.targets().contains_key("app-operation"));
            assert!(driver.a11y_node_named("App operation").is_none());
            assert!(
                driver
                    .focus_order()
                    .iter()
                    .all(|node| driver.is_history_node(*node))
            );
            assert!(
                driver
                    .focused()
                    .is_some_and(|node| driver.is_history_node(node))
            );
            assert!(
                !serde_json::to_string(&driver.presentation_inspection())
                    .unwrap()
                    .contains("App operation")
            );
            assert_eq!(
                driver.inspection().root.test_id.as_deref(),
                Some("app-operation")
            );
            driver
                .perform_named("standalone-history-close", &crate::a11y::Action::Activate)
                .unwrap();
            assert!(driver.targets().contains_key("app-operation"));
            assert_eq!(
                driver.focused_access_node().unwrap().name.as_deref(),
                Some("App operation")
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn headless_history_input_never_commits_pending_typing() {
        // phase: headless_history_browsing_polls_accepted_jobs_without_committing_typing
        {
            use crate::widget::control_result_probe::Probe;
            for succeeds in [true, false] {
                let (mut probe, observed) = Probe::new(true);
                probe.terminal_after = Some((3, succeeds));
                probe.dirty_typing = true;
                let mut driver = FrameHarness::new_with_controlled_inputs(
                    State::new(),
                    Box::new(probe),
                    (1200, 700),
                    Theme::dark(),
                    crate::font_catalog::fixture_renderer(),
                    crate::permissions::UserId("reader".into()),
                );
                driver.enable_document_history_chrome();
                observed.enqueue("run");
                driver
                    .invoke_local_history_action(&open_address(), None)
                    .unwrap();
                assert!(driver.document_history.is_open());
                assert!(driver.wait_until(tilo_test_env::hang_bound(), |driver| {
                    driver.pending_control_invocations.is_empty()
                }));
                assert_eq!(observed.calls.get(), 1);
                assert_eq!(observed.polls.get(), 3);
                assert_eq!(observed.commits.get(), 0);
                assert_eq!(driver.control_failures.is_empty(), succeeds);
            }
        }
        // phase: headless_history_pointer_semantic_and_keyboard_do_not_commit_pending_typing
        {
            let (mut driver, _, _) = fixture();
            driver
                .state_mut()
                .doc
                .put(ROOT, "value", "pending typing")
                .unwrap();
            let pending = driver.state.doc.pending_ops();
            assert!(pending > 0);
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            assert!(driver.document_history.is_open());
            assert_eq!(driver.state.doc.pending_ops(), pending);
            driver.click_named("standalone-history-close").unwrap();
            assert!(!driver.document_history.is_open());
            assert_eq!(driver.state.doc.pending_ops(), pending);
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            driver.key("Tab");
            assert_eq!(driver.state.doc.pending_ops(), pending);
            driver
                .perform_named("standalone-history-close", &crate::a11y::Action::Activate)
                .unwrap();
            assert!(!driver.document_history.is_open());
            assert_eq!(driver.state.doc.pending_ops(), pending);
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            assert!(driver.document_history.is_open());
            assert_eq!(driver.state.doc.pending_ops(), pending);
            let close = driver.a11y_node_named("Close history").unwrap();
            driver
                .perform_a11y_action(close, &crate::a11y::Action::Activate)
                .unwrap();
            assert!(!driver.document_history.is_open());
            assert_eq!(driver.state.doc.pending_ops(), pending);
        }
        // phase: headless_history_exact_three_change_comparison_and_old_session_refusal
        {
            let (mut driver, ids, _) = fixture();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            let select = session_address(&mut driver, "select");
            let args = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({"entries": ids.iter().map(crate::widget::history_group::hex16).collect::<Vec<_>>()})
            );
            driver
                .state_mut()
                .doc
                .put(ROOT, "value", "pending")
                .unwrap();
            let pending = driver.state.doc.pending_ops();
            driver
                .invoke_local_history_action(&select, Some(&args))
                .unwrap();
            let compare = session_address(&mut driver, "compare");
            driver.invoke_local_history_action(&compare, None).unwrap();
            let comparison = driver.with_history(|host, context| {
                host.session(context)
                    .unwrap()
                    .unwrap()
                    .comparison()
                    .unwrap()
                    .clone()
            });
            assert_eq!(comparison.entries.len(), 3);
            assert!(ids.iter().all(|id| comparison.entries.contains(id)));
            let before = AutoCommit::load(&comparison.before_document).unwrap();
            let after = AutoCommit::load(&comparison.after_document).unwrap();
            assert_eq!(
                before.get(ROOT, "value").unwrap().unwrap().0.as_str(),
                Some("before")
            );
            assert_eq!(
                after.get(ROOT, "value").unwrap().unwrap().0.as_str(),
                Some("last")
            );
            assert_eq!(driver.state.doc.pending_ops(), pending);
            let close = session_address(&mut driver, "close");
            driver.invoke_local_history_action(&close, None).unwrap();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            assert!(
                driver
                    .invoke_local_history_action(&select, Some(&args))
                    .is_err()
            );
        }
    }

    #[test]
    fn headless_history_invalidates_closed_revoked_or_replaced_frames() {
        // phase: headless_history_bound_lifetime_closure_invalidates_private_frame
        {
            use crate::application_test_support::{ProbeHost, runner_inputs};
            let owner = ProbeHost::new();
            let (component, _) = crate::widget::control_result_probe::component(&owner, 11, false);
            let mut driver =
                FrameHarness::from_component(component, (1200, 700), runner_inputs()).unwrap();
            driver.enable_document_history_chrome();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            let close = session_address(&mut driver, "close");
            driver.close_components();
            assert!(driver.try_presentation_inspection().is_err());
            assert!(driver.invoke_local_history_action(&close, None).is_err());
            assert!(!driver.document_history.is_open());
        }
        // phase: headless_history_revocation_refuses_cached_observation_and_old_actions
        {
            let (mut driver, _, _) = fixture();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            driver.resize((640, 700)).unwrap();
            let close = session_address(&mut driver, "close");
            driver.set_permission_ctx(PermissionContext::stranger("denied"));
            assert!(
                driver.try_presentation_inspection().is_err(),
                "immutable inspection rechecks current authority"
            );
            assert!(driver.invoke_local_history_action(&close, None).is_err());
            driver.relayout();
            assert!(!driver.document_history.is_open());
            assert!(driver.history_layout.unwrap().app_visible);
            assert!(!driver.targets().contains_key("standalone-history"));
        }
        // phase: headless_history_document_replacement_refuses_old_frame_and_recovers_layout
        {
            let (mut driver, _, _) = fixture();
            driver
                .invoke_local_history_action(&open_address(), None)
                .unwrap();
            driver.resize((640, 700)).unwrap();
            assert!(driver.state_mut().adopt_doc(AutoCommit::new()));
            assert!(driver.try_presentation_inspection().is_err());
            assert!(
                driver
                    .perform_named("standalone-history-close", &crate::a11y::Action::Activate)
                    .is_err()
            );
            driver.relayout();
            assert!(!driver.document_history.is_open());
            assert!(driver.targets().contains_key("app-operation"));
        }
    }
}
