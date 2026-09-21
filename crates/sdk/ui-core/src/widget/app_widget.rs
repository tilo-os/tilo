//! The `AppWidget` trait — the single contract between the tilo shell and
//! any in-process app hosted inside a tile. Extends the base `Widget` trait
//! with app-level metadata (kind, title, icon, permission hint, commands).
//!
//! `AppKind` is the tile-host enum re-exported from `tile_host`: one enum,
//! `Terminal, Files, Editor, Browser, Settings`.

use crate::widget::{AppKind, Widget};

/// A typed, device-local request from app content to shell chrome. Keeping
/// this on the common app contract lets both in-process tiles and future IPC
/// adapters use one destination without an app reaching into compositor
/// state or pretending a status toast completed the navigation.
#[derive(Debug, Clone, PartialEq)]
pub enum AppHostIntent {
    ShowWorkspaceActivity {
        resource_id: String,
        version_id: Option<[u8; 16]>,
        /// A factual app-native event that may not yet be part of the shell's
        /// CRDT record index. This makes the handoff exact instead of opening
        /// an empty resource lens while the durable indexes converge.
        entry: Option<crate::widget::HistoryEntry>,
    },
}

/// A lightweight icon descriptor usable before the full asset pipeline lands.
/// `Glyph` is a Nerd Font / unicode codepoint (existing approach in both
/// the iced file-explorer and quick-terminal). `Named` is a symbolic name for
/// future theme-driven icon resolution.
#[derive(Debug, Clone)]
pub enum IconDescriptor {
    Glyph(&'static str),
    Named(String),
}

/// An [`AppCommand::invoke`] handler.
pub type AppCommandInvoke =
    Box<dyn Fn(&mut automerge::AutoCommit, &automerge::ObjId) + Send + Sync>;

/// A command palette entry contributed by an app. Invoked with a mutable
/// CRDT handle and the app's root `ObjId` so the command can mutate state
/// directly (matching `quick-ui`'s direct-mutation event model).
pub struct AppCommand {
    pub id: String,
    pub label: String,
    pub shortcut: Option<String>,
    pub invoke: AppCommandInvoke,
}

impl std::fmt::Debug for AppCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCommand")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("shortcut", &self.shortcut)
            .field("invoke", &"<fn>")
            .finish()
    }
}

/// Trait every in-process app implements so the shell can host it inside a
/// `TileContent::App`. `AppWidget: Widget` means the shell always falls
/// through to the widget contract for layout, draw, and events.
pub trait AppWidget: Widget {
    /// Capture app-owned local presentation alongside the ordinary View.
    /// # Errors
    /// Rejects an invalid application contribution before either contribution is saved.
    fn capture_application_view(
        &self,
    ) -> Result<Option<crate::application_view::ApplicationLocalView>, crate::view::ThawError> {
        Ok(None)
    }
    /// Prepare current topology from an immutable latest-record loan.
    /// # Errors
    /// A payload requires explicit application support and validation.
    fn restore_application_view(
        &mut self,
        _local: &crate::application_view::ApplicationLocalView,
        _doc: &automerge::AutoCommit,
    ) -> Result<crate::application_view::ApplicationFocusRestore, crate::view::ThawError> {
        Err(crate::view::ThawError::State(
            "application-local restoration unsupported".into(),
        ))
    }
    /// Finish after ordinary View restoration and before any mount or capture.
    /// # Errors
    /// Applications may reject a prepared local plan without publishing a partial view.
    fn finish_application_view_restore(&mut self) -> Result<(), crate::view::ThawError> {
        Ok(())
    }

    fn app_kind(&self) -> AppKind;
    fn app_title(&self) -> &str;
    fn app_icon(&self) -> IconDescriptor;
    /// Which role permission governs this app. `None` means "ungated".
    fn permission_hint(&self) -> Option<AppKind>;
    /// Command-palette contributions. Default is none.
    fn commands(&self) -> Vec<AppCommand> {
        vec![]
    }

    /// Framework-owned durability, delivered only after the record loan ends.
    fn record_save_status(&mut self, _status: crate::record::RecordSaveStatus) {}

    /// Drain a one-shot request for shell-owned chrome.
    fn take_host_intent(&mut self) -> Option<AppHostIntent> {
        None
    }
}

// The sole internal owner adapter; raw descendants inherit its layout boundary.
mod component_adapter {
    use super::{AppCommand, AppHostIntent, AppWidget, IconDescriptor, Widget};
    use crate::application::AppComponent;
    use crate::widget::{
        ActionDirectoryResourcePage, Affine, AppKind, AutoCommit, CaptureMode, Continuable,
        ControlBindingMetadata, DragPayload, HashMap, HashSet, Instant, NodeId, ObjId,
        PermissionScope, PresenceCtx, PresenceKey, ResourceOperationStart, Scene, TaffyTree,
        TaffyTreeExt, TextRenderer, Theme, WindowEvent,
    };

    pub(crate) struct BoundAppWidget {
        pub(crate) component: AppComponent,
    }

    impl Continuable for BoundAppWidget {
        fn continuation_metadata(
            &self,
            state: &AutoCommit,
            _path: &ObjId,
        ) -> Option<crate::collaboration::ContinuationMetadata> {
            let binding = self.component.callback_binding();
            binding
                .read_document(state, |document, record_root| {
                    let root = self.component.root.as_deref()?;
                    root.continuable()?
                        .continuation_metadata(document, record_root)
                })
                .unwrap_or(None)
        }

        fn restore_continuation_metadata(
            &mut self,
            metadata: &crate::collaboration::ContinuationMetadata,
            state: &mut AutoCommit,
            _path: &ObjId,
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    let Some(continuable) = root.continuable_mut() else {
                        return false;
                    };
                    continuable.restore_continuation_metadata(metadata, document, record_root)
                })
                .unwrap_or(false)
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "existing Widget callback signatures"
    )]
    impl Widget for BoundAppWidget {
        fn continuable(&self) -> Option<&dyn Continuable> {
            Some(self)
        }

        fn continuable_mut(&mut self) -> Option<&mut dyn Continuable> {
            Some(self)
        }

        fn unsaved_documents(
            &self,
            _: &AutoCommit,
        ) -> Vec<crate::close_admission::UnsavedDocument> {
            self.component.unsaved_documents()
        }
        fn checkpoint_components(&mut self) -> Vec<String> {
            self.component.checkpoint_for_close()
        }
        fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
            self.component.finish_admitted_close();
            self.component.take_close_reports()
        }

        fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
            self.component.close();
            self.component.take_close_reports()
        }
        fn take_component_close_reports(
            &mut self,
        ) -> Vec<crate::application::ComponentCloseReport> {
            self.component.take_close_reports()
        }

        fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
            Some(self.component.callback_binding())
        }
        // Local feedback belongs to the emitting component. The host delivers
        // bound results through with_live_component; generic tree recursion
        // must neither cross into another record nor revive a closed owner.
        fn on_control_invocation_result(
            &mut self,
            _: &tilo_actions::ActionAddress,
            _: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
        ) {
        }
        fn visit_components(
            &mut self,
            request: &mut crate::bound_dispatch::ComponentRequest<'_>,
        ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
            let binding = self.component.callback_binding();
            if binding.closing.get() {
                return Ok(());
            }
            let Ok(_entry) = binding.lifetime.enter_if_open() else {
                return Ok(());
            };
            if request
                .target
                .is_none_or(|target| target.instance_id == binding.instance_id)
            {
                let target = request.target.unwrap_or(&binding);
                if let Some((invocation, accepted)) = self
                    .component
                    .with_live_component(target, &mut *request.visit)?
                {
                    if accepted {
                        if !self.component.invocations.contains(&invocation) {
                            self.component.invocations.push(invocation);
                        }
                    } else {
                        self.component.invocations.retain(|id| *id != invocation);
                    }
                }
                request.found = true;
                if request.target.is_some() {
                    return Ok(());
                }
            }
            if let Some(root) = self.component.root.as_deref_mut() {
                root.visit_components(request)?;
            }
            Ok(())
        }
        fn take_collab_requests(&mut self) -> Vec<crate::collaboration::CollabRequest> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return Vec::new();
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return Vec::new();
            };
            root.take_collab_requests()
        }
        fn handle_collab_event(&mut self, event: &crate::collaboration::CollabEvent) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.handle_collab_event(event);
        }
        fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.mount_native_views(host);
        }
        fn unmount_native_views(&mut self) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.unmount_native_views();
        }
        fn native_view_frame_start(&mut self) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.native_view_frame_start();
        }
        fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.set_platform_conventions(conventions);
        }
        fn set_interaction_environment(
            &mut self,
            environment: crate::interaction::InteractionEnvironment,
        ) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.set_interaction_environment(environment);
        }
        fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    root.sync(document, record_root);
                    let _ = binding.drain_owned_tree(root, document);
                })
                .unwrap_or(());
            self.component.project_record_save_status();
        }
        fn update(&mut self, state: &mut AutoCommit, _path: &ObjId) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    let updated = root.update(document, record_root);
                    let _ = binding.drain_owned_tree(root, document);
                    updated
                })
                .unwrap_or(false)
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return taffy.leaf_node(taffy::Style::default());
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return taffy.leaf_node(taffy::Style::default());
            };
            let child = root.build_layout(taffy, theme, node_map);
            let node = taffy.parent_node(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            );
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn measure(
            &mut self,
            known: taffy::Size<Option<f32>>,
            available: taffy::Size<taffy::AvailableSpace>,
            text_renderer: &mut TextRenderer,
        ) -> taffy::Size<f32> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return taffy::Size::ZERO;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return taffy::Size::ZERO;
            };
            root.measure(known, available, text_renderer)
        }
        fn apply_layout(
            &mut self,
            taffy: &TaffyTree<()>,
            node: NodeId,
            origin: vello::kurbo::Point,
        ) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            if let Some(root) = self.component.root.as_deref_mut()
                && let Ok(children) = taffy.children(node)
                && let Some(child) = children.first()
            {
                let location = taffy.layout(node).copied().unwrap_or_default().location;
                let origin = vello::kurbo::Point::new(
                    origin.x + f64::from(location.x),
                    origin.y + f64::from(location.y),
                );
                root.apply_layout(taffy, *child, origin);
            }
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            // TileHost's clipped/animated steady-state paint uses ordinary draw.
            // Canonical roots must still receive their permission-aware hook.
            let permissions = binding.permissions.borrow().clone();
            root.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                &permissions,
            );
        }
        fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.complete_content_size()
        }
        fn draw_complete_content(
            &mut self,
            scene: &mut Scene,
            bounds: vello::kurbo::Rect,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            root.draw_complete_content(scene, bounds, transform, text_renderer, theme, presence)
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            scale_factor: f64,
            modifiers: &winit::event::Modifiers,
            state: &mut AutoCommit,
            _path: &ObjId,
            handled: &mut bool,
            focus_target: &mut Option<NodeId>,
            node_id: NodeId,
            taffy: &TaffyTree<()>,
        ) {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    if let Ok(children) = taffy.children(node_id)
                        && let Some(child) = children.first()
                    {
                        root.handle_event(
                            event,
                            scale_factor,
                            modifiers,
                            document,
                            record_root,
                            handled,
                            focus_target,
                            *child,
                            taffy,
                        );
                    }
                    let _ = binding.drain_owned_tree(root, document);
                })
                .unwrap_or(());
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.component
                .root
                .as_deref()
                .map_or_else(Vec::new, |root| vec![root as &dyn Widget])
        }
        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
            if let Some(root) = self.component.root.as_deref() {
                visitor(root);
            }
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.component
                .root
                .as_deref_mut()
                .map_or_else(Vec::new, |root| vec![root as &mut dyn Widget])
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return &[];
            };
            let Some(root) = self.component.root.as_deref() else {
                return &[];
            };
            root.local_resource_operation_handlers()
        }
        fn resource_operation_handlers(&self) -> HashSet<String> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return HashSet::new();
            };
            let Some(root) = self.component.root.as_deref() else {
                return HashSet::new();
            };
            root.resource_operation_handlers()
        }
        fn name(&self) -> &'static str {
            "BoundComponent"
        }
        fn test_id(&self) -> Option<&str> {
            None
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
        fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return Vec::new();
            };
            let Some(root) = self.component.root.as_deref() else {
                return Vec::new();
            };
            root.test_rects()
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return vello::kurbo::Rect::ZERO;
            };
            let Some(root) = self.component.root.as_deref() else {
                return vello::kurbo::Rect::ZERO;
            };
            root.get_rect()
        }
        fn take_floating_surface_request(
            &mut self,
        ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref_mut()?;
            root.take_floating_surface_request()
        }
        fn take_capture_studio_request(&mut self) -> Option<CaptureMode> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref_mut()?;
            root.take_capture_studio_request()
        }
        fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return Vec::new();
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return Vec::new();
            };
            root.take_floating_surface_source_updates()
        }
        fn draw_floating_surfaces(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text_renderer: &mut crate::text_render::TextRenderer,
            theme: &crate::theme::Theme,
            presence: &crate::presence::PresenceCtx,
        ) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        }
        fn floating_surface_presentations(
            &self,
        ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return Vec::new();
            };
            let Some(root) = self.component.root.as_deref() else {
                return Vec::new();
            };
            root.floating_surface_presentations()
        }
        fn active_floating_surface_ids(&self) -> Vec<String> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return Vec::new();
            };
            let Some(root) = self.component.root.as_deref() else {
                return Vec::new();
            };
            root.active_floating_surface_ids()
        }
        fn set_floating_surface_bounds(&mut self, bounds: vello::kurbo::Rect) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.set_floating_surface_bounds(bounds);
        }
        fn on_floating_surface_host_message(
            &mut self,
            message: &crate::floating_surface::FloatingSurfaceHostMessage,
        ) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.on_floating_surface_host_message(message);
        }
        fn cursor_shape(&self, point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.cursor_shape(point)
        }
        fn tooltip(&self) -> Option<&str> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.tooltip()
        }
        fn is_focusable(&self) -> bool {
            false
        }
        fn set_focus(&mut self, focused: bool) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.set_focus(focused);
        }
        fn is_focused(&self) -> bool {
            false
        }
        fn wants_tab_input(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.wants_tab_input()
        }
        fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.ime_cursor_rect()
        }
        fn traps_focus(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.traps_focus()
        }
        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            None
        }
        fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<vello::kurbo::Rect> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.synthetic_accessibility_rect(path)
        }
        fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            root.focus_synthetic_accessibility(path)
        }
        fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.accessibility_child_parent(child_index)
        }
        fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.accessibility_child_path(child_index)
        }
        fn perform_action(
            &mut self,
            action: &crate::a11y::Action,
            state: &mut AutoCommit,
            _path: &ObjId,
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    root.perform_action(action, document, record_root)
                })
                .unwrap_or(false)
        }
        fn perform_action_at(
            &mut self,
            action: &crate::a11y::Action,
            key_path: &[u64],
            state: &mut AutoCommit,
            _path: &ObjId,
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    root.perform_action_at(action, key_path, document, record_root)
                })
                .unwrap_or(false)
        }
        fn perform_semantic_action(
            &mut self,
            target: &str,
            action: &crate::a11y::Action,
            state: &mut AutoCommit,
            _path: &ObjId,
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    root.perform_semantic_action(target, action, document, record_root)
                })
                .unwrap_or(false)
        }
        fn is_enabled(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.is_enabled()
        }
        fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.ime_purpose()
        }
        fn presented(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.presented()
        }
        fn opening(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.opening()
        }
        fn needs_commit(&self) -> bool {
            // Consumed under the canonical record admission.
            false
        }
        fn take_commit_context(&mut self) -> Option<tilo_actions::ActionContext> {
            // Consumed under the canonical record admission.
            None
        }
        fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref_mut()?;
            if let Some(intent) = root.take_control_invocation() {
                let mut queued = binding.queued_controls.borrow_mut();
                if queued.len() >= 64 {
                    queued.pop_front();
                }
                queued.push_back(intent);
            }
            // The private host sweep consumes the queue with this captured owner.
            None
        }
        fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return Vec::new();
            };
            let Some(root) = self.component.root.as_deref() else {
                return Vec::new();
            };
            root.control_action_bindings()
        }
        fn refresh_control_action_availability(
            &mut self,
            _allowed: &HashSet<tilo_actions::ActionAddress>,
        ) {
            // The private host sweep supplies this component's own authority.
        }
        fn on_commit(&mut self) {
            // Consumed under the canonical record admission.
        }
        fn get_state(&self) -> Option<String> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.get_state()
        }
        fn set_state(&mut self, state: &str) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.set_state(state);
        }
        fn text_review(&self) -> Option<(usize, String)> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.text_review()
        }
        fn on_external_event(&mut self, payload: &str, state: &mut AutoCommit, _root: &ObjId) {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    root.on_external_event(payload, document, record_root);
                })
                .unwrap_or(());
        }
        fn on_nav_gesture(
            &mut self,
            dir: crate::gesture::NavDirection,
            state: &mut AutoCommit,
            _path: &ObjId,
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    root.on_nav_gesture(dir, document, record_root)
                })
                .unwrap_or(false)
        }
        fn take_focus_reset(&mut self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            root.take_focus_reset()
        }
        fn fullscreen_intent(&self) -> Option<bool> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.fullscreen_intent()
        }
        fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return crate::fullscreen::FullscreenRequest::mirror(
                    crate::fullscreen::FullscreenTarget::Largest,
                );
            };
            let Some(root) = self.component.root.as_deref() else {
                return crate::fullscreen::FullscreenRequest::mirror(
                    crate::fullscreen::FullscreenTarget::Largest,
                );
            };
            root.fullscreen_params()
        }
        fn on_fullscreen_changed(&mut self, active: bool) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.on_fullscreen_changed(active);
        }
        fn audience_presentation_request(
            &self,
        ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.audience_presentation_request()
        }
        fn on_audience_presentation_changed(&mut self, active: bool) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.on_audience_presentation_changed(active);
        }
        fn draw_audience_presentation(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            rect: vello::kurbo::Rect,
        ) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            root.draw_audience_presentation(scene, transform, text_renderer, theme, rect)
        }
        fn handle_audience_presentation_event(
            &mut self,
            event: &WindowEvent,
            state: &mut AutoCommit,
            _path: &ObjId,
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    root.handle_audience_presentation_event(event, document, record_root)
                })
                .unwrap_or(false)
        }
        fn on_audience_presentation_closed(&mut self) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.on_audience_presentation_closed();
        }
        fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.record_intent()
        }
        fn view_state(&self) -> Option<crate::view::ViewContribution> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.view_state()
        }
        fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.restore_view_state(view);
        }
        fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.appearance_intent()
        }
        fn set_motion_reduced(&mut self, reduced: bool) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.set_motion_reduced(reduced);
        }
        fn set_record_scope(&mut self, record: &crate::record::RecordId) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.set_record_scope(record);
        }
        fn presence_key(&self) -> Option<PresenceKey> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.presence_key()
        }
        fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.presence_payload()
        }
        fn needs_idle_sync(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.needs_idle_sync()
        }
        fn permission_scope(&self) -> Option<PermissionScope> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.permission_scope()
        }
        fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let Some(root) = self.component.root.as_deref() else {
                        return Vec::new();
                    };
                    root.resources(document)
                })
                .unwrap_or_else(|_| Vec::new())
        }
        fn action_directory_resource_page(
            &self,
            doc: &AutoCommit,
            cursor: &[u8],
            max_results: usize,
        ) -> Option<ActionDirectoryResourcePage> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let root = self.component.root.as_deref()?;
                    root.action_directory_resource_page(document, cursor, max_results)
                })
                .unwrap_or(None)
        }
        fn action_directory_revision(&self, doc: &AutoCommit) -> Option<String> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let root = self.component.root.as_deref()?;
                    root.action_directory_revision(document)
                })
                .unwrap_or(None)
        }
        fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let root = self.component.root.as_deref()?;
                    root.resource(document, id)
                })
                .unwrap_or(None)
        }
        fn action_directory_visibility(
            &self,
            doc: &AutoCommit,
            resource_id: &str,
        ) -> Option<tilo_actions::ActionAddress> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let root = self.component.root.as_deref()?;
                    root.action_directory_visibility(document, resource_id)
                })
                .unwrap_or(None)
        }
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            self.component.search_providers()
        }
        fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _path: &ObjId) {
            let binding = self.component.callback_binding();
            binding
                .with_document(doc, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    root.activate_search_result(action_id, document, record_root);
                })
                .unwrap_or(());
        }
        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            // Consumed under the canonical record admission.
            None
        }
        fn set_undo_status(&mut self, _: &crate::undo::UndoStatus) {
            // Individual bound nodes receive their own record/instance status.
        }
        fn take_undo_request(&mut self) -> Option<crate::undo::UndoRequest> {
            // The host captures the origin from its admitted node map.
            None
        }
        fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            root.handle_local_undo_request(request)
        }
        fn apply_room_action(
            &mut self,
            doc: &mut AutoCommit,
            type_id: &str,
            payload: &[u8],
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .with_document(doc, |document, _record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return false;
                    };
                    root.apply_room_action(document, type_id, payload)
                })
                .unwrap_or(false)
        }
        fn invoke_resource_operation(
            &mut self,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &AutoCommit,
            _path: &ObjId,
        ) -> Result<bool, String> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return Err("component callback refused".into());
                    };
                    root.invoke_resource_operation(kind, type_id, payload, document, record_root)
                })
                .unwrap_or_else(|_| Err("component callback refused".into()))
        }
        fn invoke_resource_operation_v2(
            &mut self,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &AutoCommit,
            _path: &ObjId,
        ) -> Result<Option<tilo_actions::ActionOutput>, String> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return Err("component callback refused".into());
                    };
                    root.invoke_resource_operation_v2(kind, type_id, payload, document, record_root)
                })
                .unwrap_or_else(|_| Err("component callback refused".into()))
        }
        fn start_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &AutoCommit,
            _path: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            let binding = self.component.callback_binding();
            let result = binding
                .read_document(doc, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return Err("component callback refused".into());
                    };
                    root.start_resource_operation(
                        invocation,
                        kind,
                        type_id,
                        payload,
                        document,
                        record_root,
                    )
                })
                .unwrap_or_else(|_| Err("component callback refused".into()));
            if matches!(result, Ok(Some(ResourceOperationStart::Accepted)))
                && !self.component.invocations.contains(&invocation)
            {
                self.component.invocations.push(invocation);
            }
            result
        }
        fn start_resource_operation_with_document(
            &mut self,
            invocation: tilo_actions::InvocationId,
            kind: tilo_actions::OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &mut AutoCommit,
            _path: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            let binding = self.component.callback_binding();
            let result = binding
                .with_document(doc, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return Err("component callback refused".into());
                    };
                    root.start_resource_operation_with_document(
                        invocation,
                        kind,
                        type_id,
                        payload,
                        document,
                        record_root,
                    )
                })
                .unwrap_or_else(|_| Err("component callback refused".into()));
            if matches!(result, Ok(Some(ResourceOperationStart::Accepted)))
                && !self.component.invocations.contains(&invocation)
            {
                self.component.invocations.push(invocation);
            }
            result
        }
        fn poll_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            cancellation_requested: bool,
            doc: &AutoCommit,
            _path: &ObjId,
        ) -> Option<tilo_actions::InvocationUpdate> {
            let binding = self.component.callback_binding();
            let update = binding
                .read_document(doc, |document, record_root| {
                    let root = self.component.root.as_deref_mut()?;
                    root.poll_resource_operation(
                        invocation,
                        cancellation_requested,
                        document,
                        record_root,
                    )
                })
                .unwrap_or(None);
            if update
                .as_ref()
                .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            {
                self.component.invocations.retain(|id| *id != invocation);
            }
            update
        }
        fn accepts_drop(&self, payload: &DragPayload) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.accepts_drop(payload)
        }
        fn on_drop(&mut self, payload: DragPayload, state: &mut AutoCommit, _path: &ObjId) {
            let binding = self.component.callback_binding();
            binding
                .with_document(state, |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    root.on_drop(payload, document, record_root);
                })
                .unwrap_or(());
        }
        fn wants_pointer_capture(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.wants_pointer_capture()
        }
        fn is_floating(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.is_floating()
        }
        fn needs_animation_frame(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.needs_animation_frame()
        }
        fn next_wake_deadline(&self) -> Option<Instant> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.next_wake_deadline()
        }
        fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref()?;
            root.modal_capture()
        }
        fn take_structure_dirty(&mut self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            root.take_structure_dirty()
        }
        fn needs_redraw(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.needs_redraw()
        }
        fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref() else {
                return;
            };
            root.collect_damage(regions);
        }
        fn mark_drawn(&mut self) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.mark_drawn();
        }
        fn release_scene_cache(&mut self) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.release_scene_cache();
        }
        fn is_layout_dirty(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.is_layout_dirty()
        }
        fn layout_needs_measure(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            root.layout_needs_measure()
        }
        fn draw_with_presence(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
            _permissions: &crate::permissions::PermissionContext,
        ) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            let permissions = binding.permissions.borrow().clone();
            root.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                &permissions,
            );
        }
        fn draw_overlay(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            root.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }
    impl AppWidget for BoundAppWidget {
        fn app_kind(&self) -> AppKind {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return AppKind::Editor;
            };
            self.component
                .root
                .as_deref()
                .map_or(AppKind::Editor, AppWidget::app_kind)
        }
        fn app_title(&self) -> &str {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return "Closed";
            };
            self.component
                .root
                .as_deref()
                .map_or("Closed", AppWidget::app_title)
        }
        fn app_icon(&self) -> IconDescriptor {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return IconDescriptor::Glyph("");
            };
            self.component
                .root
                .as_deref()
                .map_or(IconDescriptor::Glyph(""), AppWidget::app_icon)
        }
        fn permission_hint(&self) -> Option<AppKind> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            self.component.root.as_deref()?.permission_hint()
        }
        // Canonical components discover commands through resource operations.
        // Legacy direct callbacks are deliberately not exported by this adapter.
        fn commands(&self) -> Vec<AppCommand> {
            vec![]
        }
        fn take_host_intent(&mut self) -> Option<AppHostIntent> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            self.component.root.as_deref_mut()?.take_host_intent()
        }
    }
}
pub(crate) use component_adapter::BoundAppWidget;
