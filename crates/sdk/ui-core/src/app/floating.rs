use std::collections::{HashMap, HashSet};

use crate::floating_surface::{
    FloatingSurfaceHostMessage, FloatingSurfacePresentation, FloatingSurfaceServiceClient,
};
use crate::vello::kurbo::Vec2;

/// App-runtime state for detachable and picture-in-picture surfaces.
///
/// Keeping this lifecycle together prevents the main [`App`] state
/// from becoming the schema for every optional host integration.
pub(super) struct FloatingRuntime {
    pub client: Option<FloatingSurfaceServiceClient>,
    pub presentations: HashMap<String, FloatingSurfacePresentation>,
    pub active_ids: HashSet<String>,
    pub prepared_ids: HashSet<String>,
    pub coordinate_offsets: HashMap<String, Vec2>,
    pub suspended_ids: HashSet<String>,
    pub host_messages: Vec<FloatingSurfaceHostMessage>,
}

impl FloatingRuntime {
    fn new(client: Option<FloatingSurfaceServiceClient>) -> Self {
        Self {
            client,
            presentations: HashMap::new(),
            active_ids: HashSet::new(),
            prepared_ids: HashSet::new(),
            coordinate_offsets: HashMap::new(),
            suspended_ids: HashSet::new(),
            host_messages: Vec::new(),
        }
    }
}

impl App {
    #[allow(
        clippy::too_many_lines,
        reason = "the service pump keeps one ordered lifecycle transaction in a single function"
    )]
    fn pump_floating_surface_service(&mut self) -> bool {
        let active_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let Some(client) = self.floating.client.as_mut() else {
            let mut changed = false;
            // Outside a Tilo session there is no trusted shell chooser to
            // open. Drain the edge so reconnecting later cannot replay a
            // stale user gesture.
            while self.root_widget.take_capture_studio_request().is_some() {}
            while let Some(request) = self.root_widget.take_floating_surface_request() {
                let message = crate::floating_surface::FloatingSurfaceHostMessage::Reattach {
                    id: request.id,
                };
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
            return changed;
        };

        let mut changed = false;
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
            if let Err(error) = client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::OpenCaptureStudio { mode },
            ) {
                tracing::warn!(%error, "shell capture-studio request failed");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
            changed = true;
        }
        while let Some(request) = self.root_widget.take_floating_surface_request() {
            self.floating.suspended_ids.remove(&request.id);
            let message = crate::floating_surface::FloatingSurfaceClientMessage::Present {
                request: (&request).into(),
            };
            if let Err(error) = client.send(&message) {
                tracing::warn!(%error, "floating-surface service send failed; keeping app UI local");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }
        for (id, region) in self.root_widget.take_floating_surface_source_updates() {
            if let Err(error) = client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::UpdateRegion {
                    id,
                    region: crate::floating_surface::protocol_rect_array(region),
                },
            ) {
                tracing::warn!(%error, "floating-surface source update failed");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }

        // Generic live mirrors have no `FloatingSurfacePresentation`: their
        // pixels live in the shell capture host. The stable activity-ID edge
        // is therefore their authoritative close/toggle signal.
        for id in generic_floating_ids_to_dismiss(
            &self.floating.active_ids,
            &active_ids,
            &self.floating.presentations,
            &self.floating.suspended_ids,
        ) {
            if let Err(error) =
                client.send(&crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id })
            {
                tracing::warn!(%error, "floating-surface dismiss failed");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }

        let presentations = self
            .root_widget
            .floating_surface_presentations()
            .into_iter()
            .map(|presentation| (presentation.id.clone(), presentation))
            .collect::<std::collections::HashMap<_, _>>();
        for (id, presentation) in &presentations {
            if !self.floating.prepared_ids.contains(id) {
                continue;
            }
            let changed = self
                .floating
                .presentations
                .get(id)
                .is_none_or(|previous| previous != presentation);
            if changed
                && let Err(error) = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::SurfaceGeometry {
                        id: id.clone(),
                        rect: crate::floating_surface::protocol_rect_array(
                            presentation.window_rect,
                        ),
                        content_rect: crate::floating_surface::protocol_rect_array(
                            presentation.content_rect,
                        ),
                    },
                )
            {
                tracing::warn!(%error, "floating-surface geometry send failed");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
            if presentation.returning_to_app && !self.floating.suspended_ids.contains(id) {
                self.floating.suspended_ids.insert(id.clone());
                if let Err(error) = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::Suspend {
                        id: id.clone(),
                        rect: crate::floating_surface::protocol_rect_array(
                            presentation.window_rect,
                        ),
                        content_rect: crate::floating_surface::protocol_rect_array(
                            presentation.content_rect,
                        ),
                    },
                ) {
                    tracing::warn!(%error, "floating-surface return handoff failed");
                    self.floating.client = None;
                    self.reattach_after_floating_service_loss();
                    return changed;
                }
            }
        }
        for id in self.floating.presentations.keys() {
            if presentations.contains_key(id) {
                continue;
            }
            if self.floating.suspended_ids.contains(id) {
                continue;
            }
            let Some(previous) = self.floating.presentations.get(id) else {
                continue;
            };
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
            if let Err(error) = client.send(&message) {
                tracing::warn!(%error, "floating-surface lifecycle send failed");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        }
        // The activity can end while its native surface is already suspended,
        // so no presentation edge remains to trigger permanent cleanup.
        let ended_suspended = self
            .floating
            .suspended_ids
            .iter()
            .filter(|id| !active_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in ended_suspended {
            if let Err(error) = client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss { id: id.clone() },
            ) {
                tracing::warn!(%error, "floating-surface dismiss send failed");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
            self.floating.suspended_ids.remove(&id);
        }
        let geometry_changed = presentations != self.floating.presentations;
        self.floating.presentations = presentations;

        let messages = match client.pump() {
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(%error, "floating-surface service disconnected; keeping app UI local");
                self.floating.client = None;
                self.reattach_after_floating_service_loss();
                return changed;
            }
        };
        for message in messages {
            match &message {
                crate::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id,
                    coordinate_offset,
                    ..
                } => {
                    self.floating.coordinate_offsets.insert(
                        id.clone(),
                        vello::kurbo::Vec2::new(coordinate_offset[0], coordinate_offset[1]),
                    );
                    if self.floating.prepared_ids.insert(id.clone()) {
                        // Force one geometry publication on the next pump,
                        // even when the pixels did not move while preparing.
                        self.floating.presentations.remove(id);
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, .. } => {
                    if self.floating.prepared_ids.insert(id.clone()) {
                        self.floating.presentations.remove(id);
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                | crate::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                    self.floating.prepared_ids.remove(id);
                    self.floating.coordinate_offsets.remove(id);
                    self.floating.suspended_ids.remove(id);
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Suspended { id } => {
                    self.floating.prepared_ids.remove(id);
                    self.floating.coordinate_offsets.remove(id);
                    self.floating.suspended_ids.insert(id.clone());
                }
                crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { .. } => {}
            }
            if let crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { bounds } =
                &message
            {
                match crate::floating_surface::protocol_rect(*bounds) {
                    Ok(bounds) => {
                        self.root_widget.set_floating_surface_bounds(bounds);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "shell sent invalid floating-surface bounds");
                    }
                }
            }
            self.root_widget.on_floating_surface_host_message(&message);
            self.floating.host_messages.push(message);
            changed = true;
        }
        self.floating.active_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect();
        changed || geometry_changed
    }

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
