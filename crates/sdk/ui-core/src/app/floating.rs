use std::collections::{HashMap, HashSet};

use crate::floating_surface::{
    FloatingSurfaceHostMessage, FloatingSurfacePresentation, FloatingSurfaceServiceClient,
};
use crate::vello::kurbo::Vec2;

/// App-runtime state for detachable and picture-in-picture surfaces.
///
/// Keeping this lifecycle together prevents the main [`App`] state
/// from becoming the schema for every optional host integration.
#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
pub(super) struct FloatingRuntime {
    pub client: Option<FloatingSurfaceServiceClient>,
    pub presentations: HashMap<String, FloatingSurfacePresentation>,
    pub active_ids: HashSet<String>,
    pub prepared_ids: HashSet<String>,
    pub coordinate_offsets: HashMap<String, Vec2>,
    pub suspended_ids: HashSet<String>,
    pub host_messages: Vec<FloatingSurfaceHostMessage>,
    /// Whether the app wants its windows withheld from captures.
    pub capture_protected: bool,
    /// What the shell was last told (it starts unprotected), so the
    /// request goes out once per change.
    pub capture_protection_sent: bool,
}

impl FloatingRuntime {
    pub(super) fn new(client: Option<FloatingSurfaceServiceClient>) -> Self {
        Self {
            client,
            presentations: HashMap::new(),
            active_ids: HashSet::new(),
            prepared_ids: HashSet::new(),
            coordinate_offsets: HashMap::new(),
            suspended_ids: HashSet::new(),
            host_messages: Vec::new(),
            capture_protected: false,
            capture_protection_sent: false,
        }
    }
}

impl App {
    /// Withhold this app's windows from screenshots, recordings, screen
    /// sharing and remote viewers — for windows that show secrets, such as a
    /// password manager's. The local display is unaffected; captures show a
    /// themed placeholder instead. Outside a Tilo session this is a no-op.
    pub fn set_capture_protected(&mut self, protected: bool) {
        self.floating.capture_protected = protected;
    }

    /// Whether [`Self::set_capture_protected`] asked for protection.
    #[must_use]
    pub fn capture_protected(&self) -> bool {
        self.floating.capture_protected
    }
}

impl App {
    #[cfg(feature = "native")]
    fn pump_floating_surface_service(&mut self) -> bool {
        let active_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let Some(mut client) = self.floating.client.take() else {
            return self.drain_unhosted_floating_requests();
        };

        let mut changed = false;
        if let Err(error) = self.send_pending_floating_requests(&mut client, &mut changed) {
            tracing::warn!(%error, "floating-surface request failed; keeping app UI local");
            self.reattach_after_floating_service_loss();
            return changed;
        }
        if let Err(error) = self.dismiss_inactive_generic_surfaces(&mut client, &active_ids) {
            tracing::warn!(%error, "floating-surface dismiss failed");
            self.reattach_after_floating_service_loss();
            return changed;
        }
        let geometry_changed = match self.reconcile_floating_presentations(&mut client, &active_ids)
        {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(%error, "floating-surface lifecycle update failed");
                self.reattach_after_floating_service_loss();
                return changed;
            }
        };
        let messages = match client.pump() {
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(%error, "floating-surface service disconnected; keeping app UI local");
                self.reattach_after_floating_service_loss();
                return changed;
            }
        };
        changed |= self.apply_floating_host_messages(messages);
        self.floating.active_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect();
        self.floating.client = Some(client);
        changed || geometry_changed
    }

    #[cfg(feature = "native")]
    fn drain_unhosted_floating_requests(&mut self) -> bool {
        // Outside a Tilo session there is no trusted shell chooser to open.
        // Drain the edge so reconnecting later cannot replay a stale gesture.
        while self.root_widget.take_capture_studio_request().is_some() {}
        let mut changed = false;
        while let Some(request) = self.root_widget.take_floating_surface_request() {
            let message =
                crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id: request.id };
            self.root_widget.on_floating_surface_host_message(&message);
            self.floating.host_messages.push(message);
            changed = true;
        }
        let _ = self.root_widget.take_floating_surface_source_updates();
        self.floating.active_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect();
        changed
    }

    #[cfg(feature = "native")]
    fn send_pending_floating_requests(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
        changed: &mut bool,
    ) -> Result<(), String> {
        if self.floating.capture_protection_sent != self.floating.capture_protected {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::SetCaptureProtection {
                    protected: self.floating.capture_protected,
                },
            )?;
            self.floating.capture_protection_sent = self.floating.capture_protected;
        }
        while let Some(mode) = self.root_widget.take_capture_studio_request() {
            let mode = match mode {
                crate::widget::CaptureMode::Screenshot => {
                    crate::floating_surface::CaptureStudioProtocolMode::Screenshot
                }
                crate::widget::CaptureMode::Recording => {
                    crate::floating_surface::CaptureStudioProtocolMode::Recording
                }
                crate::widget::CaptureMode::Pip => {
                    crate::floating_surface::CaptureStudioProtocolMode::PictureInPicture
                }
            };
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::OpenCaptureStudio { mode },
            )?;
            *changed = true;
        }
        while let Some(request) = self.root_widget.take_floating_surface_request() {
            self.floating.suspended_ids.remove(&request.id);
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::Present {
                    request: (&request).into(),
                },
            )?;
        }
        for (id, region) in self.root_widget.take_floating_surface_source_updates() {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::UpdateRegion {
                    id,
                    region: crate::floating_surface::protocol_rect_array(region),
                },
            )?;
        }
        Ok(())
    }

    #[cfg(feature = "native")]
    fn dismiss_inactive_generic_surfaces(
        &self,
        client: &mut FloatingSurfaceServiceClient,
        active_ids: &HashSet<String>,
    ) -> Result<(), String> {
        // Generic live mirrors have no native presentation: their pixels live
        // in the shell capture host, so the activity-ID edge closes them.
        for id in generic_floating_ids_to_dismiss(
            &self.floating.active_ids,
            active_ids,
            &self.floating.presentations,
            &self.floating.suspended_ids,
        ) {
            client.send(&crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id })?;
        }
        Ok(())
    }

    #[cfg(feature = "native")]
    fn reconcile_floating_presentations(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
        active_ids: &HashSet<String>,
    ) -> Result<bool, String> {
        let presentations = self
            .root_widget
            .floating_surface_presentations()
            .into_iter()
            .map(|presentation| (presentation.id.clone(), presentation))
            .collect::<HashMap<_, _>>();
        self.publish_prepared_presentations(client, &presentations)?;
        self.retire_missing_presentations(client, active_ids, &presentations)?;
        let geometry_changed = presentations != self.floating.presentations;
        self.floating.presentations = presentations;
        Ok(geometry_changed)
    }

    #[cfg(feature = "native")]
    fn publish_prepared_presentations(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
        presentations: &HashMap<String, FloatingSurfacePresentation>,
    ) -> Result<(), String> {
        for (id, presentation) in presentations {
            if !self.floating.prepared_ids.contains(id) {
                continue;
            }
            if self
                .floating
                .presentations
                .get(id)
                .is_none_or(|previous| previous != presentation)
            {
                client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::SurfaceGeometry {
                        id: id.clone(),
                        rect: crate::floating_surface::protocol_rect_array(
                            presentation.window_rect,
                        ),
                        content_rect: crate::floating_surface::protocol_rect_array(
                            presentation.content_rect,
                        ),
                    },
                )?;
            }
            if presentation.returning_to_app && !self.floating.suspended_ids.contains(id) {
                self.floating.suspended_ids.insert(id.clone());
                client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::Suspend {
                        id: id.clone(),
                        rect: crate::floating_surface::protocol_rect_array(
                            presentation.window_rect,
                        ),
                        content_rect: crate::floating_surface::protocol_rect_array(
                            presentation.content_rect,
                        ),
                    },
                )?;
            }
        }
        Ok(())
    }

    #[cfg(feature = "native")]
    fn retire_missing_presentations(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
        active_ids: &HashSet<String>,
        presentations: &HashMap<String, FloatingSurfacePresentation>,
    ) -> Result<(), String> {
        for (id, previous) in &self.floating.presentations {
            if presentations.contains_key(id) || self.floating.suspended_ids.contains(id) {
                continue;
            }
            let message = if active_ids.contains(id) {
                self.floating.suspended_ids.insert(id.clone());
                crate::floating_surface::FloatingSurfaceClientMessage::Suspend {
                    id: id.clone(),
                    rect: crate::floating_surface::protocol_rect_array(previous.window_rect),
                    content_rect: crate::floating_surface::protocol_rect_array(
                        previous.content_rect,
                    ),
                }
            } else {
                crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id: id.clone() }
            };
            client.send(&message)?;
        }

        // A generic activity may end after its native surface was suspended,
        // when no presentation edge remains to trigger permanent cleanup.
        let ended_suspended = self
            .floating
            .suspended_ids
            .iter()
            .filter(|id| !active_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in ended_suspended {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id: id.clone() },
            )?;
            self.floating.suspended_ids.remove(&id);
        }
        Ok(())
    }

    #[cfg(feature = "native")]
    fn apply_floating_host_messages(&mut self, messages: Vec<FloatingSurfaceHostMessage>) -> bool {
        let changed = !messages.is_empty();
        for message in messages {
            match &message {
                FloatingSurfaceHostMessage::PrepareSurface {
                    id,
                    coordinate_offset,
                    ..
                } => {
                    self.floating.coordinate_offsets.insert(
                        id.clone(),
                        Vec2::new(coordinate_offset[0], coordinate_offset[1]),
                    );
                    if self.floating.prepared_ids.insert(id.clone()) {
                        // Force one geometry publication on the next pump,
                        // even when the pixels did not move while preparing.
                        self.floating.presentations.remove(id);
                    }
                }
                FloatingSurfaceHostMessage::Adopted { id, .. } => {
                    if self.floating.prepared_ids.insert(id.clone()) {
                        self.floating.presentations.remove(id);
                    }
                }
                FloatingSurfaceHostMessage::Reattach { id }
                | FloatingSurfaceHostMessage::Closed { id } => {
                    self.floating.prepared_ids.remove(id);
                    self.floating.coordinate_offsets.remove(id);
                    self.floating.suspended_ids.remove(id);
                }
                FloatingSurfaceHostMessage::Suspended { id } => {
                    self.floating.prepared_ids.remove(id);
                    self.floating.coordinate_offsets.remove(id);
                    self.floating.suspended_ids.insert(id.clone());
                }
                FloatingSurfaceHostMessage::DesktopBounds { bounds } => {
                    match crate::floating_surface::protocol_rect(*bounds) {
                        Ok(bounds) => self.root_widget.set_floating_surface_bounds(bounds),
                        Err(error) => {
                            tracing::warn!(%error, "shell sent invalid floating-surface bounds");
                        }
                    }
                }
            }
            self.root_widget.on_floating_surface_host_message(&message);
            self.floating.host_messages.push(message);
        }
        changed
    }
    #[cfg(feature = "native")]
    fn reattach_after_floating_service_loss(&mut self) {
        let ids = self
            .floating
            .presentations
            .keys()
            .chain(self.floating.suspended_ids.iter())
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.floating.presentations.clear();
        self.floating.active_ids.clear();
        self.floating.prepared_ids.clear();
        self.floating.coordinate_offsets.clear();
        self.floating.suspended_ids.clear();
        for id in ids {
            let message = crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id };
            self.root_widget.on_floating_surface_host_message(&message);
            self.floating.host_messages.push(message);
        }
    }

    #[cfg(feature = "native")]
    fn take_floating_host_messages(
        &mut self,
    ) -> Vec<crate::floating_surface::FloatingSurfaceHostMessage> {
        std::mem::take(&mut self.floating.host_messages)
    }

    /// Re-express a held pointer delivered through the primary app window in
    /// desktop space while a prepared floating presentation is still active.
    /// This is the transfer-proxy interval: pixels already live in the shell,
    /// but the native secondary may not have committed and taken pointer focus
    /// yet. Once the root reattaches, its presentation disappears immediately,
    /// so the very next primary event remains app-local as usual.
    #[cfg(feature = "native")]
    fn primary_floating_pointer_event(
        &self,
        event: &WindowEvent,
        scale_factor: f64,
    ) -> Option<WindowEvent> {
        if !self.root_widget.wants_pointer_capture() {
            return None;
        }
        let offset = self
            .root_widget
            .floating_surface_presentations()
            .iter()
            .find_map(|presentation| self.floating.coordinate_offsets.get(&presentation.id))?;
        let dx = offset.x * scale_factor;
        let dy = offset.y * scale_factor;
        match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => Some(WindowEvent::CursorMoved {
                device_id: *device_id,
                position: PhysicalPosition::new(position.x + dx, position.y + dy),
            }),
            WindowEvent::Touch(touch) => {
                let mut translated = *touch;
                translated.location.x += dx;
                translated.location.y += dy;
                Some(WindowEvent::Touch(translated))
            }
            _ => None,
        }
    }
}
