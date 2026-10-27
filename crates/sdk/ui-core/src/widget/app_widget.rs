//! The `AppWidget` trait — the single contract between the tilo shell and
//! any in-process app hosted inside a tile. Extends the base `Widget` trait
//! with app-level metadata (kind, title, icon, permission hint, commands).
//!
//! `AppKind` is the tile-host enum re-exported from `tile_host`: one enum,
//! `Terminal, Files, Editor, Browser, Settings`.

// Optional floating capabilities remain available through `dyn AppWidget`.
// Re-export the extension at the app contract for hosts that import this module.
use crate::widget::{AppKind, Widget};
use std::cell::RefCell;
use std::fmt;
use std::path::PathBuf;
use std::rc::Weak;

/// An unguessable capability for one Files document-open route. A capability
/// minted anywhere else is insufficient: the shell compares it against the
/// exact capability installed in its current hosted Files inputs.
#[derive(Clone, PartialEq, Eq)]
pub struct DocumentOpenCapability(uuid::Uuid);

impl DocumentOpenCapability {
    #[must_use]
    pub fn issue() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl fmt::Debug for DocumentOpenCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DocumentOpenCapability(..)")
    }
}

/// One Files-to-shell document-open request. The Files widget owns the
/// strong cell; the shell receives only a weak witness, so a detached widget
/// cannot leave an actionable request behind.
#[derive(Debug, PartialEq, Eq)]
pub struct DocumentOpenReply {
    token: u64,
    result: Option<DocumentOpenOutcome>,
}

/// Shell-owned result for one exact Files document-open request.
#[derive(Debug, PartialEq, Eq)]
pub enum DocumentOpenOutcome {
    Opened,
    Refused(String),
    Cancelled,
}

impl DocumentOpenReply {
    #[must_use]
    pub fn pending(token: u64) -> Self {
        Self {
            token,
            result: None,
        }
    }

    #[must_use]
    pub fn token(&self) -> u64 {
        self.token
    }

    #[must_use]
    pub fn is_pending(&self, token: u64) -> bool {
        self.token == token && self.result.is_none()
    }

    pub fn complete(&mut self, token: u64, result: Result<(), String>) {
        if self.is_pending(token) {
            self.result = Some(match result {
                Ok(()) => DocumentOpenOutcome::Opened,
                Err(reason) => DocumentOpenOutcome::Refused(reason),
            });
        }
    }

    pub fn cancel(&mut self, token: u64) {
        if self.is_pending(token) {
            self.result = Some(DocumentOpenOutcome::Cancelled);
        }
    }

    pub fn take_result(&mut self, token: u64) -> Option<DocumentOpenOutcome> {
        (self.token == token).then(|| self.result.take()).flatten()
    }
}

/// The person's answer to [`AppHostIntent::AllowHostShell`]: `None` until
/// they choose, then whether the device's own shell was allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostShellAllow {
    pub answer: Option<bool>,
}

/// Admission result for a topology request, not acknowledgement of peer application.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeviceDisplayMoveReply {
    pub result: Option<Result<(), String>>,
}

/// A typed, device-local request from app content to shell chrome. Keeping
/// this on the common app contract lets both in-process tiles and future IPC
/// adapters use one destination without an app reaching into compositor
/// state or pretending a status toast completed the navigation.
#[derive(Debug, Clone)]
pub enum AppHostIntent {
    MoveDeviceDisplay {
        request: settings_store::device_displays::DeviceDisplayMove,
        reply: Weak<RefCell<DeviceDisplayMoveReply>>,
    },
    ShowWorkspaceActivity {
        resource_id: String,
        version_id: Option<[u8; 16]>,
        /// A factual app-native event that may not yet be part of the shell's
        /// CRDT record index. This makes the handoff exact instead of opening
        /// an empty resource lens while the durable indexes converge.
        entry: Option<crate::widget::HistoryEntry>,
    },
    /// Open the platform search scoped to this app (or one part of it), with
    /// the query already typed. The shell focuses the source tab first.
    OpenSearch(crate::search::SearchRequest),
    /// A Files-selected local document. The shell validates and admits it only
    /// after the originating tile has released its record loan.
    OpenDocument {
        path: PathBuf,
        capability: DocumentOpenCapability,
        token: u64,
        reply: Weak<RefCell<DocumentOpenReply>>,
    },
    /// Terminal asks for the device's own shell, which the person has not
    /// allowed on this device yet. The shell raises its permission prompt
    /// and records the answer where the shell runtime reads it.
    AllowHostShell {
        reply: Weak<RefCell<HostShellAllow>>,
    },
}

impl PartialEq for AppHostIntent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::MoveDeviceDisplay {
                    request: left,
                    reply: left_reply,
                },
                Self::MoveDeviceDisplay {
                    request: right,
                    reply: right_reply,
                },
            ) => left == right && Weak::ptr_eq(left_reply, right_reply),
            (
                Self::ShowWorkspaceActivity {
                    resource_id: left_resource,
                    version_id: left_version,
                    entry: left_entry,
                },
                Self::ShowWorkspaceActivity {
                    resource_id: right_resource,
                    version_id: right_version,
                    entry: right_entry,
                },
            ) => {
                left_resource == right_resource
                    && left_version == right_version
                    && left_entry == right_entry
            }
            (
                Self::OpenDocument {
                    path: left_path,
                    capability: left_capability,
                    token: left_token,
                    reply: left_reply,
                },
                Self::OpenDocument {
                    path: right_path,
                    capability: right_capability,
                    token: right_token,
                    reply: right_reply,
                },
            ) => {
                left_path == right_path
                    && left_capability == right_capability
                    && left_token == right_token
                    && Weak::ptr_eq(left_reply, right_reply)
            }
            (Self::OpenSearch(left), Self::OpenSearch(right)) => left == right,
            (
                Self::AllowHostShell { reply: left_reply },
                Self::AllowHostShell { reply: right_reply },
            ) => Weak::ptr_eq(left_reply, right_reply),
            _ => false,
        }
    }
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
    /// Whether this app owns a host-managed document record.
    ///
    /// In-process apps default to one scratch record per tab. A widget that
    /// merely presents an external surface can opt out so a missing scratch
    /// record never replaces that surface with a storage-error card.
    fn uses_host_record(&self) -> bool {
        true
    }

    /// Capture app-owned local presentation alongside the ordinary View.
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
    /// Which role permission governs this app. Default `None`: ungated.
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
    /// Command-palette contributions. Default is none.
    fn commands(&self) -> Vec<AppCommand> {
        vec![]
    }

    /// Framework-owned durability, delivered only after the record loan ends.
    fn record_save_status(&mut self, _status: crate::record::RecordSaveStatus) {}

    /// The room refused a change this record made, and the record withdrew
    /// it: the action is refused as one refused on this device is.
    /// Delivered, like [`Self::record_save_status`], after the record loan
    /// ends.
    fn record_refused(&mut self, _refusal: &crate::record::RecordRefusal) {}

    /// What the app said it did since last asked ("Added Buy milk"), for
    /// the record to hold until the room keeps it
    /// ([`crate::record::RecordStore::hold_notice`]).
    fn take_notices(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// The room kept what `text` says the app did: say it now. Delivered,
    /// like [`Self::record_refused`], after the record loan ends.
    fn record_said(&mut self, _text: &str) {}

    /// Answer a device-owner operation from the authenticated hosting shell.
    /// This is not an app action and never grants access to a host shell.
    ///
    /// # Errors
    /// Refuses unsupported or unavailable device-shell operations.
    fn device_shell_operation(
        &mut self,
        _operation: &crate::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        Err("This application has no device shell".into())
    }

    /// Handle the trusted shell owner selecting the device shell, without granting it.
    fn request_device_shell(&mut self) -> bool {
        false
    }

    /// Whether the person asked to try a failed record save again
    /// ([`crate::SaveAction::Retry`] on the [`crate::SaveStatusSurface`]),
    /// once. The host that owns the record submits its changes again before
    /// it next reports [`Self::record_save_status`].
    fn take_save_retry(&mut self) -> bool {
        false
    }

    /// Drain a one-shot request for shell-owned chrome.
    fn take_host_intent(&mut self) -> Option<AppHostIntent> {
        None
    }

    /// The narrowest window this app is still usable in, in logical px: a
    /// Tilo app's compact layout, a client's `xdg_toplevel` min-size hint.
    /// Resizing a window stops its edge here rather than squeezing the app
    /// (`widget::edge_resize`). By default what the app's tree says
    /// ([`crate::widget::minimum_window_width`]: a `NavShell` states its
    /// compact layout's minimum), so an app in the shell's process is
    /// stopped where its own window would be. `None` says nothing, and the
    /// strip's floor applies.
    fn min_window_width(&self) -> Option<f64> {
        crate::widget::minimum_window_width(self)
    }

    /// The widest window this app can fill, in logical px: a client's
    /// `xdg_toplevel` max-size hint. `None`: no maximum.
    fn max_window_width(&self) -> Option<f64> {
        None
    }

    /// A person is dragging one edge of this app's window; `fixed` is the
    /// edge that holds still on screen (`None` once nothing holds). An app
    /// that draws a frame of another size than its window until it catches
    /// up, a Wayland client answering a configure, draws that frame against
    /// the fixed edge, at its own scale, rather than stretched or slid.
    fn set_live_resize_anchor(&mut self, _fixed: Option<crate::widget::WindowEdge>) {}

    /// The edge drag ended. An app holding a frame against the fixed edge
    /// keeps doing so until its content has the window's size again, then
    /// lets go of it.
    fn release_live_resize_anchor(&mut self) {}

    /// What a private window offers its own device: the resources and
    /// actions its door publishes, for the host to list and invoke for this
    /// device's person without publishing them to any room. `None` for a
    /// window that is not private, whose actions reach automation through
    /// its collaboration route instead.
    fn device_local_resources(&self) -> Option<Vec<tilo_actions::Resource>> {
        None
    }

    /// Invoke one of [`Self::device_local_resources`]' actions as the
    /// window's own person, through its door.
    ///
    /// # Errors
    /// Refuses a window that offers no device-local actions, and whatever
    /// the door refuses.
    fn invoke_device_local_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        Err(tilo_actions::DoorError::Unknown(address.clone()))
    }
}

// The sole internal owner adapter; raw descendants inherit its layout boundary.
mod component_adapter {
    use super::{AppHostIntent, AppWidget, IconDescriptor, Widget};
    use crate::application::AppComponent;
    use crate::widget::{
        ActionDirectoryResourcePage, Affine, AppKind, AudiencePresentationProvider,
        AudiencePresentationWidgetExt, AutoCommit, CaptureMode, CompleteContentProvider,
        CompleteContentWidgetExt, Continuable, ControlBindingMetadata, FloatingSurfaceProvider,
        FloatingSurfaceWidgetExt, FullscreenProvider, FullscreenWidgetExt, HashMap, HashSet,
        Instant, NodeId, ObjId, PermissionScope, PresenceCtx, PresenceKey, ResourceOperationStart,
        Scene, TaffyTree, TaffyTreeExt, TextRenderer, Theme, WindowEvent,
    };

    pub(crate) struct BoundAppWidget {
        pub(crate) component: AppComponent,
        /// A complete-content capture laid the root out in a tree of its own:
        /// the host rebuilds its layout before the next frame.
        relayout: bool,
    }

    impl BoundAppWidget {
        pub(crate) fn new(component: AppComponent) -> Self {
            Self {
                component,
                relayout: false,
            }
        }

        /// Run `f` on the component's root inside the component's record
        /// frame, lending it the frame's edits at the record root. With no
        /// root, or when the frame refuses, the answer is `R::default()`.
        fn with_root_edits<R: Default>(
            &mut self,
            edits: &mut crate::widget::Edits<'_>,
            f: impl FnOnce(&mut (dyn Widget + 'static), &mut crate::widget::Edits<'_>, &ObjId) -> R,
        ) -> R {
            let binding = self.component.callback_binding();
            binding
                .with_document(edits.document_mut(), |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return R::default();
                    };
                    f(root, &mut crate::widget::Edits::lend(document), record_root)
                })
                .unwrap_or_default()
        }

        /// Run `f` on the component's root inside its callback binding.
        /// `None` when the binding refuses or there is no root.
        fn in_root<'a, R>(
            &'a self,
            f: impl FnOnce(&'a (dyn AppWidget + 'static)) -> R,
        ) -> Option<R> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            self.component.root.as_deref().map(f)
        }

        /// Whether the record's last sampled save is still unconfirmed.
        fn saving(&self) -> bool {
            let binding = self.component.callback_binding();
            binding.enter().is_ok() && self.component.record_saving()
        }

        /// [`Self::in_root`] with the root lent mutably.
        fn in_root_mut<R>(
            &mut self,
            f: impl FnOnce(&mut (dyn AppWidget + 'static)) -> R,
        ) -> Option<R> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            self.component.root.as_deref_mut().map(f)
        }
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
                    crate::widget::capability::capability::<dyn Continuable>(root)?
                        .continuation_metadata(document, record_root)
                })
                .unwrap_or(None)
        }

        fn restore_continuation_metadata(
            &mut self,
            metadata: &crate::collaboration::ContinuationMetadata,
            state: &mut crate::widget::Edits<'_>,
            _path: &ObjId,
        ) -> bool {
            self.with_root_edits(state, |root, edits, record_root| {
                let Some(continuable) =
                    crate::widget::capability::capability_mut::<dyn Continuable>(&mut *root)
                else {
                    return false;
                };
                continuable.restore_continuation_metadata(metadata, edits, record_root)
            })
        }

        fn restore_live_continuation_metadata(
            &mut self,
            metadata: &crate::collaboration::ContinuationMetadata,
            state: &AutoCommit,
            _path: &ObjId,
        ) -> bool {
            let binding = self.component.callback_binding();
            binding
                .read_document(state, |document, record_root| {
                    self.component.root.as_deref_mut().is_some_and(|root| {
                        crate::widget::capability::capability_mut::<dyn Continuable>(root)
                            .is_some_and(|continuable| {
                                continuable.restore_live_continuation_metadata(
                                    metadata,
                                    document,
                                    record_root,
                                )
                            })
                    })
                })
                .unwrap_or(false)
        }
    }

    impl FloatingSurfaceProvider for BoundAppWidget {
        fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
            self.in_root_mut(|root| root.on_floating_surface_ime_source_changed(id));
        }

        fn draw_floating_surface(
            &mut self,
            id: &str,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text_renderer: &mut crate::text_render::TextRenderer,
            theme: &crate::theme::Theme,
            presence: &crate::presence::PresenceCtx,
        ) -> bool {
            self.in_root_mut(|root| {
                root.draw_floating_surface(id, scene, transform, text_renderer, theme, presence)
            })
            .unwrap_or_default()
        }

        fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
            self.in_root_mut(|root| root.on_floating_surface_focus_changed(id, focused));
        }

        fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
            self.in_root_mut(|root| root.on_floating_surface_event_source(id));
        }

        fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
            self.in_root(|root| root.floating_surface_accessibility(id))
                .flatten()
        }

        fn perform_floating_surface_accessibility_action(
            &mut self,
            id: &str,
            target: &str,
            action: &crate::a11y::Action,
            state: &mut crate::widget::Edits<'_>,
            _path: &ObjId,
        ) -> bool {
            self.with_root_edits(state, |root, edits, record_root| {
                root.perform_floating_surface_accessibility_action(
                    id,
                    target,
                    action,
                    edits,
                    record_root,
                )
            })
        }

        fn floating_surface_ime(
            &self,
            id: &str,
        ) -> Option<(crate::ime::InputContent, vello::kurbo::Rect)> {
            self.in_root(|root| root.floating_surface_ime(id)).flatten()
        }

        fn take_floating_surface_request(
            &mut self,
        ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
            self.in_root_mut(|root| root.take_floating_surface_request())
                .flatten()
        }

        fn take_capture_studio_request(&mut self) -> Option<CaptureMode> {
            self.in_root_mut(|root| root.take_capture_studio_request())
                .flatten()
        }

        fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)> {
            self.in_root_mut(FloatingSurfaceWidgetExt::take_floating_surface_source_updates)
                .unwrap_or_default()
        }

        fn draw_floating_surfaces(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text_renderer: &mut crate::text_render::TextRenderer,
            theme: &crate::theme::Theme,
            presence: &crate::presence::PresenceCtx,
        ) {
            self.in_root_mut(|root| {
                root.draw_floating_surfaces(scene, transform, text_renderer, theme, presence)
            });
        }

        fn floating_surface_presentations(
            &self,
        ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
            self.in_root(FloatingSurfaceWidgetExt::floating_surface_presentations)
                .unwrap_or_default()
        }

        fn active_floating_surface_ids(&self) -> Vec<String> {
            self.in_root(FloatingSurfaceWidgetExt::active_floating_surface_ids)
                .unwrap_or_default()
        }

        fn set_floating_surface_bounds(&mut self, bounds: vello::kurbo::Rect) {
            self.in_root_mut(|root| root.set_floating_surface_bounds(bounds));
        }

        fn on_floating_surface_host_message(
            &mut self,
            message: &crate::floating_surface::FloatingSurfaceHostMessage,
        ) {
            self.in_root_mut(|root| root.on_floating_surface_host_message(message));
        }
    }

    impl AudiencePresentationProvider for BoundAppWidget {
        fn audience_presentation_request(
            &self,
        ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
            self.in_root(|root| root.audience_presentation_request())
                .flatten()
        }

        fn on_audience_presentation_changed(&mut self, active: bool) {
            self.in_root_mut(|root| root.on_audience_presentation_changed(active));
        }

        fn draw_audience_presentation(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            rect: vello::kurbo::Rect,
        ) -> bool {
            self.in_root_mut(|root| {
                root.draw_audience_presentation(scene, transform, text_renderer, theme, rect)
            })
            .unwrap_or_default()
        }

        fn handle_audience_presentation_event(
            &mut self,
            event: &WindowEvent,
            state: &mut crate::widget::Edits<'_>,
            _path: &ObjId,
        ) -> bool {
            self.with_root_edits(state, |root, edits, record_root| {
                root.handle_audience_presentation_event(event, edits, record_root)
            })
        }

        fn on_audience_presentation_closed(&mut self) {
            self.in_root_mut(|root| root.on_audience_presentation_closed());
        }
    }

    impl FullscreenProvider for BoundAppWidget {
        fn fullscreen_intent(&self) -> Option<bool> {
            self.in_root(|root| root.fullscreen_intent()).flatten()
        }

        fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
            self.in_root(FullscreenWidgetExt::fullscreen_params)
                .unwrap_or_else(|| {
                    crate::fullscreen::FullscreenRequest::mirror(
                        crate::fullscreen::FullscreenTarget::Largest,
                    )
                })
        }

        fn on_fullscreen_changed(&mut self, active: bool) {
            self.in_root_mut(|root| root.on_fullscreen_changed(active));
        }
    }

    impl CompleteContentProvider for BoundAppWidget {
        /// The root's own complete content, else a scrolling document it
        /// shows at full height: the window taller by what the document's
        /// primary scroll hides.
        fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
            self.in_root(|root| {
                if let Some(size) = root.complete_content_size() {
                    return Some(size);
                }
                let hidden = unrolled_height(root)?;
                let rect = root.get_rect();
                Some(vello::kurbo::Size::new(
                    rect.width(),
                    rect.height() + hidden,
                ))
            })
            .flatten()
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
            if self.component.behaviour.capture_protected {
                crate::secret_paint::note_secret_painted();
            }
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            if root.complete_content_size().is_some() {
                return root.draw_complete_content(
                    scene,
                    bounds,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                );
            }
            if unrolled_height(root).is_none() || bounds.area() <= 0.0 {
                return false;
            }
            // The capture is another presentation of the same view: lay the
            // root out at the complete size, where its document fits without
            // scrolling, draw it, and lay it back out where it lives, at the
            // place its reader had scrolled to.
            let live = root.get_rect();
            let offset = root
                .primary_vertical_scroll()
                .map(|region| region.metrics.offset);
            if crate::widget::layout::lay_out_detached(root, bounds, theme, text_renderer).is_err()
            {
                return false;
            }
            let permissions = binding.permissions.borrow().clone();
            presence.with_permissions(permissions, || {
                root.draw(scene, transform, text_renderer, theme, presence);
            });
            let _ = crate::widget::layout::lay_out_detached(root, live, theme, text_renderer);
            if let Some(offset) = offset {
                let _ = root.apply_primary_vertical_scroll(
                    crate::widget::PrimaryScrollAction::Absolute(offset),
                );
            }
            self.relayout = true;
            true
        }
    }

    /// How much of the root's document its primary vertical scroll hides.
    fn unrolled_height(root: &dyn Widget) -> Option<f64> {
        let metrics = root.primary_vertical_scroll()?.metrics;
        let hidden = metrics.content - metrics.viewport;
        (hidden.is_finite() && hidden > 1.0).then_some(hidden)
    }

    impl Widget for BoundAppWidget {
        crate::provides!(
            dyn crate::AppWidget,
            dyn crate::widget::FullscreenProvider,
            dyn crate::widget::CompleteContentProvider,
            dyn crate::widget::Continuable,
            dyn crate::widget::FloatingSurfaceProvider,
            dyn crate::widget::AudiencePresentationProvider,
            dyn crate::collaboration::participants::Participants,
            dyn crate::undo::CommittedActions,
            dyn crate::undo::UndoControl,
            dyn crate::undo::LocalUndo,
            dyn crate::history_mode::capability::DocumentHistory,
            dyn crate::widget::controls::ActionControls,
            dyn crate::resources::PublishesResources,
            dyn crate::search::capability::SearchSource,
            dyn crate::permissions::capability::PermissionGated,
            dyn crate::presence::capability::PublishesPresence,
            dyn crate::record::capability::RecordScoped,
            dyn crate::view::capability::ViewState,
            dyn crate::component_lifecycle::ComponentLifecycle,
            dyn crate::component_lifecycle::WindowActivation,
            dyn crate::undo::UndoPoints,
            dyn crate::widget::ResourceOperations
        );
        fn as_any(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }

        fn observe_physical_input(&mut self, input: crate::widget::PhysicalInput) {
            self.in_root_mut(|root| root.observe_physical_input(input));
        }

        // A component's diagnostic probe observes its own record, the
        // document its widgets actually render, not the runner's outer one.
        fn native_input_snapshot(
            &self,
            _: &crate::widget::AppModel,
            _: &crate::widget::ModelPath,
        ) -> Option<serde_json::Value> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            let root = self.component.root.as_deref()?;
            binding
                .records
                .read(&binding.checkout.id, |doc| {
                    root.native_input_snapshot(doc, &automerge::ROOT)
                })
                .flatten()
        }

        fn view_only_scroll(&self, event: &WindowEvent) -> bool {
            self.in_root(|root| root.view_only_scroll(event))
                .unwrap_or_default()
        }

        fn primary_vertical_scroll(&self) -> Option<crate::widget::PrimaryScrollRegion> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            let mut region = self.component.root.as_deref()?.primary_vertical_scroll()?;
            region.owner_generation = Some(binding.lifetime_id);
            Some(region)
        }
        fn apply_primary_vertical_scroll(
            &mut self,
            action: crate::widget::PrimaryScrollAction,
        ) -> crate::widget::PrimaryScrollResult {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return crate::widget::PrimaryScrollResult::Unsupported;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return crate::widget::PrimaryScrollResult::Unsupported;
            };
            root.apply_primary_vertical_scroll(action)
                .stamp(binding.lifetime_id)
        }

        fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
            Some(self.component.callback_binding())
        }
        // Local feedback belongs to the emitting component. The host delivers
        // bound results through with_live_component; generic tree recursion
        // must neither cross into another record nor revive a closed owner.
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
                .is_none_or(|target| target.same_component(&binding))
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
        fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
            self.in_root_mut(|root| root.mount_native_views(host));
        }
        fn unmount_native_views(&mut self) {
            self.in_root_mut(|root| root.unmount_native_views());
        }
        fn native_view_frame_start(&mut self) {
            self.in_root_mut(|root| root.native_view_frame_start());
        }
        fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
            self.in_root_mut(|root| root.set_platform_conventions(conventions));
        }
        fn set_interaction_environment(
            &mut self,
            environment: crate::interaction::InteractionEnvironment,
        ) {
            self.in_root_mut(|root| root.set_interaction_environment(environment));
        }
        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            let (state, _) = crate::doc::document_ref(state, path);
            let binding = self.component.callback_binding();
            let sync_root = |component: &mut AppComponent| {
                binding
                    .read_document(state, |document, record_root| {
                        if let Some(root) = component.root.as_deref_mut() {
                            root.sync(document, record_root);
                        }
                    })
                    .unwrap_or(());
            };
            sync_root(&mut self.component);
            // The record's durability, sampled once the root has synced. A
            // change is shown by syncing the root again with it, so the frame
            // needs no rebuilt tree (which repaints the whole window).
            if self.component.project_record_save_status() {
                sync_root(&mut self.component);
            }
        }
        fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, _path: &ObjId) {
            let binding = self.component.callback_binding();
            binding
                .with_settlement(edits.document_mut(), |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    let before = crate::doc::settle_mark(document);
                    root.settle(&mut crate::widget::Edits::lend(document), record_root);
                    let _ = binding.drain_owned_tree(root, document);
                    // What it wrote shows in this same frame.
                    if crate::doc::settle_mark(document) != before {
                        root.update(document, record_root);
                        root.sync(document, record_root);
                    }
                })
                .unwrap_or(());
        }
        fn update(
            &mut self,
            state: &crate::widget::AppModel,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let (state, _) = crate::doc::document_ref(state, path);
            let binding = self.component.callback_binding();
            // The frame's presence, under the component's own authority, as
            // its draw sees it.
            let permissions = binding.permissions.borrow().clone();
            let update = |component: &mut AppComponent| {
                binding
                    .read_document(state, |document, record_root| {
                        component
                            .root
                            .as_deref_mut()
                            .is_some_and(|root| root.update(document, record_root))
                    })
                    .unwrap_or(false)
            };
            let component = &mut self.component;
            crate::presence::frame::frame(|presence| {
                presence.with_permissions(permissions.clone(), || update(component))
            })
            .unwrap_or_else(|| update(component))
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let Some(child) = self.in_root_mut(|root| root.build_layout(taffy, theme, node_map))
            else {
                return taffy.leaf_node(taffy::Style::default());
            };
            let style = taffy::Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..Default::default()
            };
            taffy.widget_node(style, &[child], node_map, self)
        }
        fn measure(
            &mut self,
            known: taffy::Size<Option<f32>>,
            available: taffy::Size<taffy::AvailableSpace>,
            text_renderer: &mut TextRenderer,
        ) -> taffy::Size<f32> {
            self.in_root_mut(|root| root.measure(known, available, text_renderer))
                .unwrap_or_default()
        }
        fn apply_layout(
            &mut self,
            taffy: &TaffyTree<()>,
            node: NodeId,
            origin: vello::kurbo::Point,
        ) {
            self.in_root_mut(|root| {
                if let Ok(children) = taffy.children(node)
                    && let Some(child) = children.first()
                {
                    let origin = crate::widget::layout_rect(taffy, node, origin).origin();
                    root.apply_layout(taffy, *child, origin);
                }
            });
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
            // Protection belongs to this component even inside a desktop or
            // chrome container; its enclosing presented frame must retain it.
            if self.component.behaviour.capture_protected {
                crate::secret_paint::note_secret_painted();
            }
            // The component's own authority, not whatever its container holds.
            let permissions = binding.permissions.borrow().clone();
            presence.with_permissions(permissions, || {
                root.draw(scene, transform, text_renderer, theme, presence);
            });
        }
        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
            let binding = self.component.callback_binding();
            binding
                .with_document(ctx.state.document_mut(), |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    if let Ok(children) = ctx.taffy.children(ctx.node_id)
                        && let Some(child) = children.first()
                    {
                        root.handle_event(
                            event,
                            &mut crate::widget::EventCtx {
                                scale_factor: ctx.scale_factor,
                                modifiers: ctx.modifiers,
                                state: crate::widget::Edits::lend(document),
                                path: record_root,
                                handled: ctx.handled,
                                focus_target: ctx.focus_target,
                                node_id: *child,
                                taffy: ctx.taffy,
                            },
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
        fn name(&self) -> &'static str {
            "BoundComponent"
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
        fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
            self.in_root(|root| root.test_rects()).unwrap_or_default()
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.in_root(|root| root.get_rect()).unwrap_or_default()
        }
        fn cursor_shape(&self, point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
            self.in_root(|root| root.cursor_shape(point)).flatten()
        }
        fn tooltip(&self) -> Option<&str> {
            self.in_root(|root| root.tooltip()).flatten()
        }
        fn set_focus(&mut self, focused: bool) {
            self.in_root_mut(|root| root.set_focus(focused));
        }
        fn wants_tab_input(&self) -> bool {
            self.in_root(|root| root.wants_tab_input())
                .unwrap_or_default()
        }
        fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
            self.in_root(|root| root.ime_cursor_rect()).flatten()
        }
        fn traps_focus(&self) -> bool {
            self.in_root(|root| root.traps_focus()).unwrap_or_default()
        }
        fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<vello::kurbo::Rect> {
            self.in_root(|root| root.synthetic_accessibility_rect(path))
                .flatten()
        }
        fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
            self.in_root_mut(|root| root.focus_synthetic_accessibility(path))
                .unwrap_or_default()
        }
        fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
            self.in_root(|root| root.accessibility_child_parent(child_index))
                .flatten()
        }
        fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
            self.in_root(|root| root.accessibility_child_path(child_index))
                .flatten()
        }
        fn accessibility_represents_child(&self, child_index: usize) -> bool {
            self.in_root(|root| root.accessibility_represents_child(child_index))
                .unwrap_or_default()
        }
        fn accessibility_child_description(&self, child_index: usize) -> Option<String> {
            self.in_root(|root| root.accessibility_child_description(child_index))
                .flatten()
        }
        fn perform_action(
            &mut self,
            action: &crate::a11y::Action,
            key_path: &[u64],
            state: &mut crate::widget::Edits<'_>,
            _path: &crate::widget::ModelPath,
        ) -> bool {
            self.with_root_edits(state, |root, edits, record_root| {
                root.perform_action(action, key_path, edits, record_root)
            })
        }
        fn perform_semantic_action(
            &mut self,
            target: &str,
            action: &crate::a11y::Action,
            state: &mut crate::widget::Edits<'_>,
            _path: &crate::widget::ModelPath,
        ) -> bool {
            self.with_root_edits(state, |root, edits, record_root| {
                root.perform_semantic_action(target, action, edits, record_root)
            })
        }
        fn is_enabled(&self) -> bool {
            self.in_root(|root| root.is_enabled()).unwrap_or_default()
        }
        fn ime_requires_deferred_ack(&self) -> bool {
            self.in_root(|root| root.ime_requires_deferred_ack())
                .unwrap_or(true)
        }

        fn ime_refuses_ordinary_commits(&self) -> bool {
            self.in_root(|root| root.ime_refuses_ordinary_commits())
                .unwrap_or(true)
        }

        fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
            self.in_root_mut(|root| root.start_deferred_text())
                .flatten()
        }

        fn commit_deferred_text(
            &mut self,
            text: &str,
        ) -> Option<crate::webview::text_input::Receipt> {
            self.in_root_mut(|root| root.commit_deferred_text(text))
                .flatten()
        }

        fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
            self.in_root(|root| root.ime_edit_fingerprint(key))
                .flatten()
        }
        fn ime_content(&self) -> Option<crate::ime::InputContent> {
            self.in_root(|root| root.ime_content()).flatten()
        }
        fn ime_multiline(&self) -> bool {
            self.in_root(Widget::ime_multiline).unwrap_or_default()
        }

        fn presented(&self) -> bool {
            self.in_root(|root| root.presented()).unwrap_or_default()
        }
        fn arrives_from(&self) -> crate::arriving::Source {
            self.in_root(|root| root.arrives_from())
                .unwrap_or(crate::arriving::Source::Local)
        }
        fn arrival_progress(&self) -> Option<crate::arriving::Progress> {
            self.in_root(|root| root.arrival_progress()).flatten()
        }
        fn arrival_shape(&self) -> crate::widget::SkeletonShape {
            self.in_root(|root| root.arrival_shape())
                .unwrap_or(crate::widget::SkeletonShape::Ground)
        }
        fn text_review(&self) -> Option<(usize, String)> {
            self.in_root(|root| root.text_review()).flatten()
        }
        fn nav_availability(&self) -> crate::gesture::NavAvailability {
            self.in_root(Widget::nav_availability).unwrap_or_default()
        }
        fn on_external_event(
            &mut self,
            payload: &str,
            state: &mut crate::widget::Edits<'_>,
            _root: &crate::widget::ModelPath,
        ) {
            self.with_root_edits(state, |root, edits, record_root| {
                root.on_external_event(payload, edits, record_root);
            });
        }
        fn on_nav_gesture(
            &mut self,
            dir: crate::gesture::NavDirection,
            state: &crate::widget::AppModel,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let (state, _) = crate::doc::document_ref(state, path);
            let binding = self.component.callback_binding();
            binding
                .read_document(state, |document, record_root| {
                    self.component
                        .root
                        .as_deref_mut()
                        .is_some_and(|root| root.on_nav_gesture(dir, document, record_root))
                })
                .unwrap_or(false)
        }
        fn take_focus_reset(&mut self) -> bool {
            self.in_root_mut(|root| root.take_focus_reset())
                .unwrap_or_default()
        }
        fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
            self.in_root(|root| root.appearance_intent()).flatten()
        }
        fn set_motion_reduced(&mut self, reduced: bool) {
            self.in_root_mut(|root| root.set_motion_reduced(reduced));
        }
        fn needs_idle_sync(&self) -> bool {
            self.in_root(|root| root.needs_idle_sync())
                .unwrap_or_default()
        }

        fn apply_room_action(
            &mut self,
            doc: &mut crate::widget::Edits<'_>,
            type_id: &str,
            payload: &[u8],
        ) -> bool {
            self.with_root_edits(doc, |root, edits, _| {
                root.apply_room_action(edits, type_id, payload)
            })
        }
        fn wants_pointer_capture(&self) -> bool {
            self.in_root(|root| root.wants_pointer_capture())
                .unwrap_or_default()
        }
        fn needs_animation_frame(&self) -> bool {
            self.in_root(|root| root.needs_animation_frame())
                .unwrap_or_default()
        }
        /// A save the store has not confirmed yet is pending too: "Saving…"
        /// is a placeholder for "Saved", so a still capture waits for it.
        fn has_pending_visual_work(&self) -> bool {
            self.in_root(Widget::has_pending_visual_work)
                .unwrap_or_default()
                || self.saving()
        }
        fn has_transient_feedback(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            self.component.record_saving()
                || self
                    .component
                    .root
                    .as_deref()
                    .is_some_and(Widget::has_transient_feedback)
        }
        // The component's root serves its own deadlines, inside the
        // component's scope.
        fn serve_deadline(&mut self, now: Instant) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return true;
            };
            self.component
                .root
                .as_deref_mut()
                .is_none_or(|root| root.serve_deadline(now))
        }
        fn next_wake_deadline(&self) -> Option<Instant> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            self.component.root.as_deref()?.next_wake_deadline()
        }
        fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
            self.in_root(|root| root.modal_capture()).flatten()
        }
        fn take_structure_dirty(&mut self) -> bool {
            self.take_structure_change().rebuilds()
        }
        /// The root's change as it says it; the component's own reasons (a
        /// relayout it asked for, a save status no `App` presented) rebuild.
        fn take_structure_change(&mut self) -> crate::widget::StructureChange {
            use crate::widget::StructureChange;
            let relayout = StructureChange::of(std::mem::take(&mut self.relayout));
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return relayout;
            };
            let save_status_changed =
                StructureChange::of(self.component.take_save_status_changed());
            let Some(root) = self.component.root.as_deref_mut() else {
                return relayout;
            };
            root.take_structure_change()
                .join(save_status_changed)
                .join(relayout)
        }
        fn needs_redraw(&self) -> bool {
            self.in_root(|root| root.needs_redraw()).unwrap_or_default()
        }
        fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
            self.in_root(|root| root.collect_damage(regions));
        }
        fn mark_drawn(&mut self) {
            self.in_root_mut(|root| root.mark_drawn());
        }
        fn release_scene_cache(&mut self) {
            self.in_root_mut(|root| root.release_scene_cache());
        }
        fn is_layout_dirty(&self) -> bool {
            self.in_root(|root| root.is_layout_dirty())
                .unwrap_or_default()
        }
        fn layout_needs_measure(&self) -> bool {
            self.in_root(|root| root.layout_needs_measure())
                .unwrap_or_default()
        }
        fn draw_overlay(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            if self.component.behaviour.capture_protected {
                crate::secret_paint::note_secret_painted();
            }
            self.in_root_mut(|root| {
                root.draw_overlay(scene, transform, text_renderer, theme, presence)
            });
        }
    }

    impl crate::widget::ResourceOperations for BoundAppWidget {
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            self.in_root(|root| crate::widget::local_resource_operation_handlers(root))
                .unwrap_or(&[])
        }

        fn resource_operation_handlers(&self) -> HashSet<String> {
            self.in_root(|root| crate::widget::resource_operation_handlers(root))
                .unwrap_or_default()
        }

        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            doc: &mut crate::widget::Edits<'_>,
            path: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            // A required operation arrives inside its owner's checked record
            // frame; it is routed without taking this component's document loan.
            if op.is_scoped() {
                return crate::widget::start_in_children(self, op, doc, path);
            }
            let binding = self.component.callback_binding();
            let result = binding
                .with_document(doc.document_mut(), |document, record_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return Err("component callback refused".into());
                    };
                    crate::widget::start_operation(
                        &mut *root,
                        op,
                        &mut crate::widget::Edits::lend(document),
                        record_root,
                    )
                })
                .unwrap_or_else(|_| Err("component callback refused".into()));
            if matches!(result, Ok(Some(ResourceOperationStart::Accepted)))
                && !self.component.invocations.contains(&op.invocation())
            {
                self.component.invocations.push(op.invocation());
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
                    crate::widget::poll_resource_operation(
                        &mut *root,
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
    }

    impl crate::undo::UndoPoints for BoundAppWidget {
        fn get_state(&self) -> Option<String> {
            self.in_root(|root| crate::undo::get_state(root)).flatten()
        }
        fn set_state(&mut self, state: &str) {
            self.in_root_mut(|root| crate::undo::set_state(root, state));
        }
    }

    impl crate::component_lifecycle::WindowActivation for BoundAppWidget {
        fn take_window_activation_intent(&self) -> bool {
            self.in_root(crate::component_lifecycle::take_window_activation_intent)
                .unwrap_or_default()
        }
    }

    impl crate::component_lifecycle::ComponentLifecycle for BoundAppWidget {
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
        fn take_replacement(&mut self) -> Option<crate::application::ComponentReplacement> {
            self.component.take_replacement()
        }
        fn take_save_status_presentation(&mut self) -> bool {
            self.component.take_save_status_changed()
        }
        fn take_component_close_reports(
            &mut self,
        ) -> Vec<crate::application::ComponentCloseReport> {
            self.component.take_close_reports()
        }
    }

    impl crate::view::capability::ViewState for BoundAppWidget {
        fn view_state(&self) -> Option<crate::view::ViewContribution> {
            self.in_root(|root| crate::view::capability::view_state(root))
                .flatten()
        }
        fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
            self.in_root_mut(|root| crate::view::capability::restore_view_state(root, view));
        }
    }

    impl crate::record::capability::RecordScoped for BoundAppWidget {
        fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
            self.in_root(|root| crate::record::capability::record_intent(root))
                .flatten()
        }
        fn set_record_scope(&mut self, record: &crate::record::RecordId) {
            self.in_root_mut(|root| crate::record::capability::set_record_scope(root, record));
        }
    }

    impl crate::presence::capability::PublishesPresence for BoundAppWidget {
        fn presence_key(&self) -> Option<PresenceKey> {
            self.in_root(|root| crate::presence::capability::presence_key(root))
                .flatten()
        }
        fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
            self.in_root(|root| crate::presence::capability::presence_payload(root))
                .flatten()
        }
    }

    impl crate::permissions::capability::PermissionGated for BoundAppWidget {
        fn permission_scope(&self) -> Option<PermissionScope> {
            self.in_root(|root| crate::permissions::capability::permission_scope(root))
                .flatten()
        }
    }

    impl crate::search::capability::SearchSource for BoundAppWidget {
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            self.component.search_providers()
        }
        fn search_provider_descriptors(
            &self,
        ) -> Vec<crate::search::capability::SearchProviderDescriptor> {
            self.component.search_provider_descriptors()
        }
        fn activate_search_result(
            &mut self,
            action_id: &str,
            doc: &mut crate::widget::Edits<'_>,
            _path: &crate::widget::ModelPath,
        ) {
            self.with_root_edits(doc, |root, edits, record_root| {
                crate::search::capability::activate_search_result(
                    root,
                    action_id,
                    edits,
                    record_root,
                );
            });
        }
    }

    impl crate::resources::PublishesResources for BoundAppWidget {
        fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let Some(root) = self.component.root.as_deref() else {
                        return Vec::new();
                    };
                    crate::resources::resources(root, document)
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
                    crate::resources::action_directory_resource_page(
                        root,
                        document,
                        cursor,
                        max_results,
                    )
                })
                .unwrap_or(None)
        }
        fn action_directory_revision(&self, doc: &AutoCommit) -> Option<String> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let root = self.component.root.as_deref()?;
                    crate::resources::action_directory_revision(root, document)
                })
                .unwrap_or(None)
        }
        fn action_directory_invocation_domain(
            &self,
            doc: &AutoCommit,
        ) -> crate::widget::ActionDirectoryInvocationDomain {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    self.component
                        .root
                        .as_deref()
                        .map(|root| {
                            crate::resources::action_directory_invocation_domain(root, document)
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        }
        fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let root = self.component.root.as_deref()?;
                    crate::resources::resource(root, document, id)
                })
                .unwrap_or(None)
        }
        /// A participant's tree is built against the component's own record
        /// document, like [`Self::resources`]; the default would fall back to
        /// this boundary's own view (the published schema) on the outer doc.
        fn resources_for(
            &self,
            participant: Option<&tilo_actions::ParticipantId>,
            doc: &AutoCommit,
        ) -> Vec<tilo_actions::Resource> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let Some(root) = self.component.root.as_deref() else {
                        return Vec::new();
                    };
                    crate::resources::resources_for(root, participant, document)
                })
                .unwrap_or_else(|_| Vec::new())
        }
        fn resource_for(
            &self,
            participant: Option<&tilo_actions::ParticipantId>,
            doc: &AutoCommit,
            id: &str,
        ) -> Option<tilo_actions::Resource> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _record_root| {
                    let root = self.component.root.as_deref()?;
                    crate::resources::resource_for(root, participant, document, id)
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
                    crate::resources::action_directory_visibility(root, document, resource_id)
                })
                .unwrap_or(None)
        }
    }

    impl crate::widget::controls::ActionControls for BoundAppWidget {
        fn install_control_custody(
            &mut self,
            parent: &crate::ControlDomain,
            attachment: crate::control_custody::CustodyAttachment,
        ) {
            let binding = self.component.callback_binding();
            if let Some(domain) = binding.control_domain.upgrade() {
                let _ = domain.attach_to(parent);
                if let Some(root) = self.component.root.as_deref_mut() {
                    crate::widget::controls::install_control_custody(root, &domain, attachment);
                }
            }
        }
        fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref_mut()?;
            if let Some(intent) = crate::widget::controls::take_control_invocation(root) {
                let mut queued = binding.queued_controls.borrow_mut();
                if queued.len() >= 64 {
                    queued.pop_front();
                }
                queued.push_back(intent);
            }
            // The private host sweep consumes the queue with this captured owner.
            None
        }
        fn on_control_invocation_result(
            &mut self,
            _: &tilo_actions::ActionAddress,
            _: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
        ) {
        }
        fn has_pending_control_invocation(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            !binding.queued_controls.borrow().is_empty()
                || self.component.root.as_deref().is_some_and(|root| {
                    crate::widget::controls::has_pending_control_invocation(root)
                })
        }
        fn action_control_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
            self.in_root(|root| crate::widget::controls::action_control_bindings(root))
                .unwrap_or_default()
        }
        fn has_action_control_bindings(&self) -> bool {
            self.in_root(|root| crate::widget::controls::has_action_control_bindings(root))
                .unwrap_or_default()
        }
        fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
            self.in_root(|root| crate::widget::controls::control_action_bindings(root))
                .unwrap_or_default()
        }
        fn refresh_control_action_availability(
            &mut self,
            _allowed: &HashSet<tilo_actions::ActionAddress>,
        ) {
            // The private host sweep supplies this component's own authority.
        }
    }

    impl crate::history_mode::capability::DocumentHistory for BoundAppWidget {
        // The files of the mounted filesystem the component's History reads
        // are named by its root, which witnesses the checked-file source it
        // registered. The component's own registration is the runner's; a
        // root's registration of anything else is not answered here.
        fn history_registration(
            &self,
        ) -> Result<
            Option<crate::history_mode::HistoryRegistration>,
            crate::history_mode::HistoryRegistrationError,
        > {
            self.in_root(|root| {
                Ok(crate::history_mode::capability::history_registration(root)?
                    .filter(crate::history_mode::HistoryRegistration::is_checked_files_only))
            })
            .unwrap_or(Ok(None))
        }

        fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
            let binding = self.component.callback_binding();
            let _entry = binding
                .enter()
                .map_err(|error| format!("Component is unavailable: {error:?}"))?;
            let root = self
                .component
                .root
                .as_deref()
                .ok_or("Component root is unavailable")?;
            crate::history_mode::capability::history_restore_readiness(root, scope)
        }
    }

    impl crate::undo::LocalUndo for BoundAppWidget {
        fn local_undo_status(&self) -> Option<crate::undo::LocalUndoStatus> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            crate::undo::local_undo_status(self.component.root.as_deref()?)
        }

        fn local_undo_is_deferred(&self) -> bool {
            self.in_root(|root| crate::undo::local_undo_is_deferred(root))
                .unwrap_or_default()
        }

        fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
            self.in_root_mut(|root| crate::undo::handle_local_undo_request(root, request))
                .unwrap_or_default()
        }
    }

    impl crate::undo::UndoControl for BoundAppWidget {}

    impl crate::undo::CommittedActions for BoundAppWidget {
        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            // Consumed under the canonical record admission.
            None
        }
    }

    impl crate::collaboration::participants::Participants for BoundAppWidget {
        fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
            crate::collaboration::participants::join_participant_in_children(self, participant)
        }
        fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
            crate::collaboration::participants::leave_participant_in_children(self, participant);
        }
        fn set_participant_label(
            &mut self,
            participant: &tilo_actions::ParticipantId,
            label: &str,
        ) {
            crate::collaboration::participants::set_participant_label_in_children(
                self,
                participant,
                label,
            );
        }
        fn set_participant_host(&mut self, host: bool) {
            crate::collaboration::participants::set_participant_host_in_children(self, host);
        }
        /// Participant views (agent carets) live against the component's own
        /// record document, the one `sync` hands the app, never the outer
        /// App document: resolved against the outer document a Writer caret
        /// finds no block, and no participant presence is produced at all.
        fn participant_presence(
            &self,
            state: &AutoCommit,
        ) -> Vec<crate::widget::ParticipantPresence> {
            let binding = self.component.callback_binding();
            binding
                .read_document(state, |document, _record_root| {
                    self.component
                        .root
                        .as_deref()
                        .map(|root| {
                            crate::collaboration::participants::participant_presence(root, document)
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()
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
        fn device_shell_operation(
            &mut self,
            operation: &crate::floating_surface::DeviceShellOperation,
        ) -> Result<serde_json::Value, String> {
            let binding = self.component.callback_binding();
            let _entry = binding
                .enter()
                .map_err(|_| "Terminal is not available".to_string())?;
            self.component
                .root
                .as_deref_mut()
                .ok_or("Terminal is not available")?
                .device_shell_operation(operation)
        }
        fn request_device_shell(&mut self) -> bool {
            self.in_root_mut(AppWidget::request_device_shell)
                .unwrap_or_default()
        }
        fn take_host_intent(&mut self) -> Option<AppHostIntent> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            if let Some(request) = binding.take_search_request() {
                return Some(AppHostIntent::OpenSearch(request));
            }
            self.component.root.as_deref_mut()?.take_host_intent()
        }
    }
}
pub(crate) use component_adapter::BoundAppWidget;

#[cfg(test)]
mod native_lifecycle_tests {
    use super::*;
    use crate::application::{LaunchTarget, RestoreInput};
    use crate::application_test_support as probe;

    #[test]
    fn native_history_status_crosses_only_the_live_bound_owner() {
        use crate::undo::{LocalUndo, LocalUndoStatus, UndoStatus};
        use crate::widget::Decorator;
        use std::cell::Cell;
        use std::rc::Rc;

        struct History {
            root: Box<dyn AppWidget>,
            reads: Rc<Cell<u32>>,
        }
        impl Decorator for History {
            crate::provides!(dyn LocalUndo; inner = root);
            type Inner = dyn AppWidget;
            fn inner(&self) -> &(dyn AppWidget + 'static) {
                self.root.as_ref()
            }
            fn inner_mut(&mut self) -> &mut (dyn AppWidget + 'static) {
                self.root.as_mut()
            }
        }
        impl AppWidget for History {
            fn app_kind(&self) -> AppKind {
                self.root.app_kind()
            }
            fn app_title(&self) -> &str {
                self.root.app_title()
            }
            fn app_icon(&self) -> IconDescriptor {
                self.root.app_icon()
            }
            fn permission_hint(&self) -> Option<AppKind> {
                self.root.permission_hint()
            }
        }
        impl LocalUndo for History {
            fn local_undo_status(&self) -> Option<LocalUndoStatus> {
                self.reads.set(self.reads.get() + 1);
                Some(LocalUndoStatus::Ready(UndoStatus {
                    can_undo: true,
                    ..Default::default()
                }))
            }
            fn local_undo_is_deferred(&self) -> bool {
                self.reads.set(self.reads.get() + 1);
                true
            }
            fn handle_local_undo_request(&mut self, _: crate::undo::UndoRequest) -> bool {
                true
            }
        }
        let host = probe::ProbeHost::new();
        let mut component =
            probe::instantiate_probe(&host, 42, LaunchTarget::New, RestoreInput::None).unwrap();
        let reads = Rc::new(Cell::new(0));
        component.root = Some(Box::new(History {
            root: component.root.take().unwrap(),
            reads: reads.clone(),
        }));
        let binding = component.callback_binding();
        let mut widget = BoundAppWidget::new(component);
        assert!(matches!(
            crate::undo::local_undo_status(&widget),
            Some(LocalUndoStatus::Ready(UndoStatus { can_undo: true, .. }))
        ));
        assert!(crate::undo::local_undo_is_deferred(&widget));
        let live_reads = reads.get();
        assert!(live_reads >= 2);
        binding.closing.set(true);
        assert!(crate::undo::local_undo_status(&widget).is_none());
        assert!(!crate::undo::local_undo_is_deferred(&widget));
        assert_eq!(
            reads.get(),
            live_reads,
            "closing owner must never be queried"
        );
        widget.component.close();
        assert!(crate::undo::local_undo_status(&widget).is_none());
        assert!(!crate::undo::local_undo_is_deferred(&widget));
        assert_eq!(
            reads.get(),
            live_reads,
            "closed owner must never be queried"
        );
    }

    #[test]
    fn free_native_lifecycle_walk_enters_bound_owner_once_and_refuses_closed_owner() {
        let host = probe::ProbeHost::new();
        let component =
            probe::instantiate_probe(&host, 41, LaunchTarget::New, RestoreInput::None).unwrap();
        let binding = component.callback_binding();
        let mut widget = BoundAppWidget::new(component);
        let native_host = crate::widget::NativeViewHost::headless(
            crate::platform::PlatformConventions::default(),
            1.0,
        );

        crate::widget::mount_native_views(&mut widget, &native_host);
        crate::widget::native_view_frame_start(&mut widget);
        crate::widget::unmount_native_views(&mut widget);
        let observations = probe::take_native_lifecycle_observations();
        assert_eq!(
            observations
                .iter()
                .map(|(event, _)| *event)
                .collect::<Vec<_>>(),
            vec!["mount", "frame", "unmount"]
        );
        assert!(
            observations
                .iter()
                .all(|(_, lifetime)| lifetime.is_active())
        );

        binding.closing.set(true);
        crate::widget::mount_native_views(&mut widget, &native_host);
        crate::widget::native_view_frame_start(&mut widget);
        crate::widget::unmount_native_views(&mut widget);
        assert!(probe::take_native_lifecycle_observations().is_empty());

        widget.component.close();
        assert!(
            observations
                .iter()
                .all(|(_, lifetime)| !lifetime.is_active())
        );
        let teardown = probe::take_native_lifecycle_observations();
        assert_eq!(
            teardown.iter().map(|(event, _)| *event).collect::<Vec<_>>(),
            vec!["unmount"]
        );
        assert!(teardown.iter().all(|(_, lifetime)| !lifetime.is_active()));
    }
}
