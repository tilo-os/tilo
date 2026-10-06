use std::collections::{HashMap, HashSet};

use crate::floating_surface::{
    FloatingSurfaceHostMessage, FloatingSurfacePresentation, FloatingSurfaceServiceClient,
};
use crate::vello::kurbo::Vec2;

#[derive(Default)]
pub(super) struct AuxiliaryCaptureFrames {
    painting: bool,
    presented: bool,
    latched: bool,
    queued: std::collections::VecDeque<bool>,
}

impl AuxiliaryCaptureFrames {
    fn protected(&self) -> bool {
        self.latched || self.painting || self.presented || self.queued.iter().any(|secret| *secret)
    }
}

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
    host_shell_allow: Option<std::rc::Weak<std::cell::RefCell<crate::widget::HostShellAllow>>>,
    pub presentations: HashMap<String, FloatingSurfacePresentation>,
    pub active_ids: HashSet<String>,
    pub prepared_ids: HashSet<String>,
    pub coordinate_offsets: HashMap<String, Vec2>,
    pub suspended_ids: HashSet<String>,
    pub host_messages: Vec<FloatingSurfaceHostMessage>,
    /// Whether the app wants its windows withheld from captures.
    pub capture_protected: bool,
    /// Whether the frame currently being prepared paints a secret.
    pub secret_on_screen: bool,
    /// What the shell was last told (it starts unprotected), so the
    /// request goes out once per change.
    pub capture_protection_sent: bool,
    pub capture_generation: u64,
    pub capture_acknowledged: Option<u64>,
    pub capture_host_required: bool,
    /// Monotonic native lifetime latch: worker completion cannot confirm
    /// that the compositor replaced its last secret buffer.
    pub presented_secret: bool,
    pub submitted_secrets: std::collections::VecDeque<(u64, bool)>,
    pub auxiliary_capture_frames: HashMap<String, AuxiliaryCaptureFrames>,
    /// The shell was told this app can stream its page.
    pub paint_capable_sent: bool,
    /// Outside a Tilo session, "close this tab" closes this window.
    pub close_window_requested: bool,
}

impl FloatingRuntime {
    pub(super) fn new(client: Option<FloatingSurfaceServiceClient>) -> Self {
        Self {
            capture_host_required: client.is_some(),
            client,
            host_shell_allow: None,
            presentations: HashMap::new(),
            active_ids: HashSet::new(),
            prepared_ids: HashSet::new(),
            coordinate_offsets: HashMap::new(),
            suspended_ids: HashSet::new(),
            host_messages: Vec::new(),
            capture_protected: false,
            secret_on_screen: false,
            capture_protection_sent: false,
            capture_generation: 0,
            capture_acknowledged: None,
            presented_secret: false,
            submitted_secrets: std::collections::VecDeque::new(),
            auxiliary_capture_frames: HashMap::new(),
            paint_capable_sent: false,
            close_window_requested: false,
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

    /// Record whether the frame just drawn shows a secret field with a
    /// value, masked or shown. Such a frame is withheld from captures like a
    /// protected app's, so a typed password never reaches a screenshot or a
    /// screen share.
    #[cfg_attr(
        not(feature = "native"),
        allow(dead_code, reason = "only the native runner draws frames")
    )]
    pub(crate) fn set_secret_on_screen(&mut self, shown: bool) {
        self.floating.secret_on_screen = shown;
    }

    /// What the shell should withhold: everything the app asked for, and
    /// any window while it shows a secret.
    #[cfg_attr(
        not(feature = "native"),
        allow(dead_code, reason = "only the native runner draws frames")
    )]
    #[must_use]
    pub(crate) fn wants_capture_protection(&self) -> bool {
        self.floating.capture_protected
            || self.floating.secret_on_screen
            || self.floating.presented_secret
            || self
                .floating
                .submitted_secrets
                .iter()
                .any(|(_, secret)| *secret)
            || self
                .floating
                .auxiliary_capture_frames
                .values()
                .any(AuxiliaryCaptureFrames::protected)
    }
}

impl App {
    /// A queued request is insufficient: the compositor must have applied it
    /// before the render worker can present any secret pixels.
    #[cfg(feature = "native")]
    fn capture_frame_ready(&mut self) -> bool {
        if !self.floating.capture_host_required || !self.wants_capture_protection() {
            return true;
        }
        self.pump_floating_surface_service();
        self.floating.client.is_some()
            && self.floating.capture_protection_sent
            && self.floating.capture_acknowledged == Some(self.floating.capture_generation)
    }

    #[cfg(feature = "native")]
    fn capture_frame_submitted(&mut self, sequence: u64) {
        let secret = self.floating.secret_on_screen || self.floating.capture_protected;
        // A worker present() is not a compositor buffer acknowledgement.
        // Keep this app-wide latch until native windows/process teardown.
        self.floating.presented_secret |= secret;
        self.floating
            .submitted_secrets
            .push_back((sequence, secret));
    }

    #[cfg(feature = "native")]
    fn capture_frame_presented(&mut self, sequence: u64) {
        while self
            .floating
            .submitted_secrets
            .front()
            .is_some_and(|(frame, _)| *frame <= sequence)
        {
            self.floating.submitted_secrets.pop_front();
        }
    }

    #[cfg(feature = "native")]
    fn capture_auxiliary_drawn(&mut self, id: &str, secret: bool) {
        self.floating
            .auxiliary_capture_frames
            .entry(id.to_owned())
            .or_default()
            .painting = secret;
    }

    #[cfg(feature = "native")]
    fn capture_auxiliary_submitted(&mut self, id: &str) {
        let frames = self
            .floating
            .auxiliary_capture_frames
            .entry(id.to_owned())
            .or_default();
        let secret = frames.painting || self.floating.capture_protected;
        frames.latched |= secret;
        frames.queued.push_back(secret);
    }

    #[cfg(feature = "native")]
    fn capture_auxiliary_completed(&mut self, id: &str, presented: bool) {
        if let Some(frames) = self.floating.auxiliary_capture_frames.get_mut(id) {
            if let Some(secret) = frames.queued.pop_front() {
                if presented {
                    frames.presented = secret;
                }
            }
        }
    }

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
    fn take_native_host_shell_allow(
        &mut self,
    ) -> Option<std::rc::Weak<std::cell::RefCell<crate::widget::HostShellAllow>>> {
        let widget = crate::widget::capability::capability_mut::<dyn crate::AppWidget>(
            &mut *self.root_widget,
        )?;
        match widget.take_host_intent()? {
            crate::widget::AppHostIntent::AllowHostShell { reply } => Some(reply),
            _ => None,
        }
    }

    #[cfg(feature = "native")]
    fn drain_unhosted_floating_requests(&mut self) -> bool {
        let mut changed = false;
        if let Some(reply) = self
            .take_native_host_shell_allow()
            .and_then(|reply| reply.upgrade())
        {
            reply.borrow_mut().answer = Some(false);
            changed = true;
        }
        // Outside a Tilo session there is no trusted shell chooser to open.
        // Drain the edge so reconnecting later cannot replay a stale gesture.
        while self.root_widget.take_capture_studio_request().is_some() {}
        for request in crate::shell_tabs::take_requests() {
            self.apply_unhosted_tab_request(request);
            changed = true;
        }
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
        if let Some(reply) = self.take_native_host_shell_allow() {
            self.floating.host_shell_allow = Some(reply);
            *changed = true;
            client.send(&crate::floating_surface::FloatingSurfaceClientMessage::AllowHostShell)?;
        }
        let protected = self.wants_capture_protection();
        if !self.floating.paint_capable_sent && crate::webview::paint::share_capable() {
            client.send(&crate::floating_surface::FloatingSurfaceClientMessage::PaintCapable)?;
            self.floating.paint_capable_sent = true;
        }
        // A window withheld from captures shares no page either.
        let shared = crate::webview::paint::take_share_frames();
        if !protected {
            use base64::Engine as _;
            for frame in shared {
                client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::PaintStream {
                        payload: base64::engine::general_purpose::STANDARD.encode(&frame.payload),
                        page: frame.page,
                        scale: frame.scale,
                    },
                )?;
            }
        }
        if self.floating.capture_protection_sent != protected {
            self.floating.capture_generation = self
                .floating
                .capture_generation
                .checked_add(1)
                .ok_or("capture protection generation exhausted")?;
            self.floating.capture_acknowledged = None;
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::SetCaptureProtection {
                    protected,
                    generation: Some(self.floating.capture_generation),
                },
            )?;
            self.floating.capture_protection_sent = protected;
        }
        let instance = crate::shell_tabs::launch_instance();
        for request in crate::shell_tabs::take_requests() {
            client.send(&tab_request_message(request, instance))?;
            *changed = true;
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
                FloatingSurfaceHostMessage::DeviceShellOperation { request, operation } => {
                    let result = crate::widget::capability::capability_mut::<dyn crate::AppWidget>(
                        &mut *self.root_widget,
                    )
                    .ok_or_else(|| "Terminal is not available".to_string())
                    .and_then(|widget| widget.device_shell_operation(operation));
                    if let Some(client) = &mut self.floating.client {
                        let _ = client.send(&crate::floating_surface::FloatingSurfaceClientMessage::DeviceShellOutcome { request: *request, result });
                    }
                }
                FloatingSurfaceHostMessage::CaptureProtectionApplied {
                    generation,
                    protected,
                } => {
                    if *generation == self.floating.capture_generation
                        && *protected == self.floating.capture_protection_sent
                    {
                        self.floating.capture_acknowledged = Some(*generation);
                    }
                }
                FloatingSurfaceHostMessage::HostShellAllowed { allowed } => {
                    if let Some(reply) = self
                        .floating
                        .host_shell_allow
                        .take()
                        .and_then(|reply| reply.upgrade())
                    {
                        reply.borrow_mut().answer = Some(*allowed);
                    }
                }
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
                FloatingSurfaceHostMessage::Appearance { .. } => {
                    if let Some(appearance) = message.window_appearance() {
                        self.set_window_appearance(appearance);
                    }
                    // The window's own concern, not a floating surface's.
                    continue;
                }
                FloatingSurfaceHostMessage::PaintKeyframe => {
                    crate::webview::paint::set_share_watched(true);
                }
                FloatingSurfaceHostMessage::PaintStop => {
                    crate::webview::paint::set_share_watched(false);
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
        if let Some(reply) = self
            .floating
            .host_shell_allow
            .take()
            .and_then(|reply| reply.upgrade())
        {
            reply.borrow_mut().answer = Some(false);
        }
        // Without the shell nobody can be watching the shared page.
        crate::webview::paint::set_share_watched(false);
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

/// The `shell-ui` message that carries `request` for the app the shell
/// launched as record `instance`.
#[cfg(feature = "native")]
fn tab_request_message(
    request: crate::shell_tabs::TabRequest,
    instance: Option<u64>,
) -> crate::floating_surface::FloatingSurfaceClientMessage {
    use crate::floating_surface::FloatingSurfaceClientMessage as Message;
    use crate::shell_tabs::TabRequest;
    match request {
        TabRequest::OpenBeside { target, background } => Message::OpenTabBeside {
            target,
            background,
            instance,
        },
        TabRequest::CloseThis => Message::CloseTab { instance },
        TabRequest::ReopenClosed => Message::ReopenClosedTab,
        TabRequest::PlaceNextWindowBeside { background } => Message::PlaceNextWindowBeside {
            background,
            instance,
        },
    }
}

impl App {
    /// Without a desktop tab model, another instance is another window of
    /// this app, and closing this tab closes this window.
    #[cfg(feature = "native")]
    fn apply_unhosted_tab_request(&mut self, request: crate::shell_tabs::TabRequest) {
        use crate::shell_tabs::TabRequest;
        match request {
            TabRequest::OpenBeside { target, .. } => {
                if let Err(error) = open_standalone_instance(target.as_deref()) {
                    tracing::warn!(%error, "could not open another window of this app");
                }
            }
            TabRequest::CloseThis => self.floating.close_window_requested = true,
            // A window this app starts itself is placed by the host's own
            // window manager, and there is no closed-tab history to reopen.
            TabRequest::ReopenClosed | TabRequest::PlaceNextWindowBeside { .. } => {}
        }
    }

    /// Whether a tab request asked to close this window. Consumed.
    #[cfg(feature = "native")]
    fn take_close_window_request(&mut self) -> bool {
        std::mem::take(&mut self.floating.close_window_requested)
    }
}

/// Start another process of this executable showing `target`.
#[cfg(feature = "native")]
fn open_standalone_instance(target: Option<&str>) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(executable);
    if let Some(target) = target {
        // One opaque positional, never something an argument parser
        // could read as a flag.
        if target.starts_with('-') {
            return Err("refusing an argument-shaped target".into());
        }
        command.arg(target);
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    crate::background::Background::current()
        .spawn("standalone-instance-reaper", move || {
            let _ = child.wait();
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(all(test, feature = "native"))]
mod host_shell_allow_tests {
    use super::*;
    #[test]
    fn desktop_reply_resolves_native_permission_and_service_loss_refuses_it() {
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Container::new()),
        );
        for allowed in [false, true] {
            let reply = std::rc::Rc::new(std::cell::RefCell::new(
                crate::widget::HostShellAllow::default(),
            ));
            app.floating.host_shell_allow = Some(std::rc::Rc::downgrade(&reply));
            let wire =
                serde_json::to_vec(&FloatingSurfaceHostMessage::HostShellAllowed { allowed })
                    .unwrap();
            app.apply_floating_host_messages(vec![serde_json::from_slice(&wire).unwrap()]);
            assert_eq!(reply.borrow().answer, Some(allowed));
            assert!(app.floating.host_shell_allow.is_none());
        }
        let reply = std::rc::Rc::new(std::cell::RefCell::new(
            crate::widget::HostShellAllow::default(),
        ));
        app.floating.host_shell_allow = Some(std::rc::Rc::downgrade(&reply));
        app.reattach_after_floating_service_loss();
        assert_eq!(reply.borrow().answer, Some(false));
    }
}
