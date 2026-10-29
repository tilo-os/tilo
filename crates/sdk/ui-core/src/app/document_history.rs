//! The native host's side of the window's History: the registered History
//! beside the live root, its door, and the presentation it lays out.

use super::App;
use crate::history_mode::door::HISTORY_OPERATION;
use crate::host_core::HostCore as _;
use crate::search::request::SEARCH_OPERATION;
use crate::widget::Widget;

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
    pub fn enable_document_history_chrome(&mut self) {
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
