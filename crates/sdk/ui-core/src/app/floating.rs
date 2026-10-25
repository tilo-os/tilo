use std::collections::{HashMap, HashSet};

use crate::floating_surface::{
    FloatingSurfaceHostMessage, FloatingSurfacePresentation, FloatingSurfaceServiceClient,
};
use crate::vello::kurbo::Vec2;

/// What the shell was last told on this connection, where telling it
/// nothing yet differs from telling it "none".
pub enum Told<T> {
    Nothing,
    Was(T),
}

// Only the native event-loop runner tells the shell anything.
#[cfg(feature = "native")]
impl<T: PartialEq> Told<T> {
    fn was(&self, value: &T) -> bool {
        matches!(self, Self::Was(told) if told == value)
    }
}

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
    expect(
        dead_code,
        reason = "the surface service client, presentations and capture handshake are read only by the native event-loop runner"
    )
)]
pub(super) struct FloatingRuntime {
    pub client: Option<FloatingSurfaceServiceClient>,
    /// The authenticated shell's palette survives private settings reloads.
    pub host_theme: Option<crate::Theme>,
    network_status_active: bool,
    host_shell_allow: Option<std::rc::Weak<std::cell::RefCell<crate::widget::HostShellAllow>>>,
    display_move: Option<(
        u64,
        std::rc::Weak<std::cell::RefCell<crate::widget::DeviceDisplayMoveReply>>,
    )>,
    next_display_move: u64,
    pub presentations: HashMap<String, FloatingSurfacePresentation>,
    pub active_ids: HashSet<String>,
    pub prepared_ids: HashSet<String>,
    /// Exact app-owned Present requests accepted on the current connection,
    /// whose retained editors await the host's PrepareSurface response.
    pub pending_ime_ids: HashSet<String>,
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
    pub drawn_action_markers: Vec<crate::app::PaintedActionMarker>,
    pub submitted_action_markers:
        std::collections::VecDeque<(u64, Vec<crate::app::PaintedActionMarker>)>,
    pub auxiliary_capture_frames: HashMap<String, AuxiliaryCaptureFrames>,
    /// The shell was told this app can stream its page.
    pub paint_capable_sent: bool,
    /// The shell was told this app answers selection requests.
    pub selection_capable_sent: bool,
    /// The window resource last told the shell on this connection.
    pub window_resource_sent: Told<Option<tilo_actions::ActionResource>>,
    /// Outside a Tilo session, "close this tab" closes this window.
    pub close_window_requested: bool,
    /// The [`crate::window_toolbar::revision`] of the slots last sent to
    /// the shell on this connection.
    pub toolbar_sent: Option<u64>,
    /// The window bar's search and History control for this app
    /// ([`window_bar`]).
    pub window_bar: window_bar::WindowBarRuntime,
    /// The record this window shows, as its route names it, and the kind of
    /// resource it keeps: what the desktop lists it under for sharing.
    pub window_record: Option<(String, String)>,
    /// What of [`crate::window_share`] the shell was told on this
    /// connection.
    pub window_share_sent: WindowShareSent,
}

/// What of a window's live sharing the shell was told on one connection.
#[derive(Default)]
pub struct WindowShareSent {
    /// The revision of the embeds last sent (0: none declared).
    pub embeds: u64,
    /// The revision of the shares last sent (0: none).
    pub shared: u64,
    /// The window's record was sent.
    pub record: bool,
}

impl FloatingRuntime {
    #[cfg(any(feature = "native", test))]
    pub(super) fn complete_action_marker_frame(
        &mut self,
        completed: u64,
        presented: bool,
    ) -> Option<Vec<crate::app::PaintedActionMarker>> {
        let mut result = None;
        while self
            .submitted_action_markers
            .front()
            .is_some_and(|(frame, _)| *frame <= completed)
        {
            if let Some((frame, markers)) = self.submitted_action_markers.pop_front()
                && frame == completed
                && presented
            {
                result = Some(markers);
            }
        }
        result
    }

    pub(super) fn new(client: Option<FloatingSurfaceServiceClient>) -> Self {
        Self {
            capture_host_required: client.is_some(),
            client,
            host_theme: None,
            network_status_active: false,
            host_shell_allow: None,
            display_move: None,
            next_display_move: 0,
            presentations: HashMap::new(),
            active_ids: HashSet::new(),
            prepared_ids: HashSet::new(),
            pending_ime_ids: HashSet::new(),
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
            drawn_action_markers: Vec::new(),
            submitted_action_markers: std::collections::VecDeque::new(),
            auxiliary_capture_frames: HashMap::new(),
            paint_capable_sent: false,
            selection_capable_sent: false,
            window_resource_sent: Told::Nothing,
            close_window_requested: false,
            toolbar_sent: None,
            window_bar: window_bar::WindowBarRuntime::default(),
            window_record: None,
            window_share_sent: WindowShareSent::default(),
        }
    }
}

#[cfg(test)]
mod marker_frame_tests {
    use super::FloatingRuntime;

    #[test]
    fn only_an_exact_successfully_presented_frame_exports_its_inventory() {
        let mut runtime = FloatingRuntime::new(None);
        runtime.submitted_action_markers.push_back((1, Vec::new()));
        runtime.submitted_action_markers.push_back((2, Vec::new()));
        assert!(runtime.complete_action_marker_frame(1, false).is_none());
        assert_eq!(runtime.submitted_action_markers.len(), 1);
        assert_eq!(
            runtime.complete_action_marker_frame(2, true),
            Some(Vec::new()),
            "an actually presented empty inventory withdraws prior markers"
        );
        assert!(runtime.complete_action_marker_frame(2, true).is_none());
        assert!(runtime.submitted_action_markers.is_empty());
    }
}

impl Drop for FloatingRuntime {
    fn drop(&mut self) {
        if self.network_status_active {
            tilo_host::network_status::clear_host_status();
        }
    }
}

impl App {
    /// Pump the real native host channel in compositor-free acceptance fixtures.
    /// Returns whether this launch's inherited connection is still present.
    /// This exposes no connection injection or permission bypass.
    #[cfg(all(feature = "native", feature = "test-support"))]
    pub fn pump_native_host_for_test(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        self.pump_floating_surface_service();
        self.floating.client.is_some()
    }

    #[cfg(feature = "native")]
    fn pending_floating_ime_source(&self) -> Option<&str> {
        self.floating.client.as_ref()?;
        let active_ids = self.root_widget.active_floating_surface_ids();
        let mut sources = self.floating.pending_ime_ids.iter().filter(|id| {
            active_ids.contains(id) && self.root_widget.floating_surface_ime(id).is_some()
        });
        let source = sources.next()?;
        sources.next().is_none().then_some(source.as_str())
    }

    /// Publish queued Present requests before comparing the composing source's
    /// IME projection. This deliberately does not read host lifecycle messages:
    /// Prepare must be applied by the native runner before pending custody ends.
    #[cfg(feature = "native")]
    fn flush_floating_present_requests_before_ime(&mut self) {
        let Some(mut client) = self.floating.client.take() else {
            return;
        };
        if let Err(error) = self.send_floating_present_requests(&mut client) {
            tracing::warn!(%error, "floating-surface request failed; keeping app UI local");
            self.reattach_after_floating_service_loss();
            return;
        }
        self.floating.client = Some(client);
    }

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
    #[cfg(feature = "native")]
    pub(crate) fn set_secret_on_screen(&mut self, shown: bool) {
        self.floating.secret_on_screen = shown;
    }

    /// What the shell should withhold: everything the app asked for, and
    /// any window while it shows a secret.
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
    /// Attach an explicit host-service connection to the actual native pump in a fixture.
    /// Existing production dispatch, connection retention and outcome serialization are unchanged.
    #[cfg(all(feature = "native", feature = "test-support"))]
    pub fn test_floating_service_connection(&mut self, client: FloatingSurfaceServiceClient) {
        self.floating.client = Some(client);
    }

    /// Poll the actual native host-service pump without running a window event loop.
    #[cfg(all(feature = "native", feature = "test-support"))]
    pub fn test_pump_floating_service(&mut self) -> bool {
        self.pump_floating_surface_service()
    }

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
        // Operations reply through this exact authenticated connection. Restore it
        // before dispatch, rather than leaving their outcome sink temporarily empty.
        self.floating.client = Some(client);
        changed |= self.apply_floating_host_messages(messages);
        self.floating.active_ids = self
            .root_widget
            .active_floating_surface_ids()
            .into_iter()
            .collect();
        self.floating
            .pending_ime_ids
            .retain(|id| self.floating.active_ids.contains(id));
        changed || geometry_changed
    }

    #[cfg(feature = "native")]
    fn take_native_host_intent(&mut self) -> Option<crate::widget::AppHostIntent> {
        crate::widget::capability::capability_mut::<dyn crate::AppWidget>(&mut *self.root_widget)?
            .take_host_intent()
    }

    #[cfg(feature = "native")]
    fn drain_unhosted_floating_requests(&mut self) -> bool {
        self.floating.pending_ime_ids.clear();
        let mut changed = if let Some(intent) = self.take_native_host_intent() {
            match intent {
                crate::widget::AppHostIntent::AllowHostShell { reply } => {
                    if let Some(reply) = reply.upgrade() {
                        reply.borrow_mut().answer = Some(false);
                    }
                }
                crate::widget::AppHostIntent::MoveDeviceDisplay { reply, .. } => {
                    if let Some(reply) = reply.upgrade() {
                        reply.borrow_mut().result =
                            Some(Err(crate::i18n::I18nManager::lookup_plain(
                                "device-display-session-unavailable",
                            )));
                    }
                }
                _ => {}
            }
            true
        } else {
            false
        };
        // Outside a Tilo session there is no trusted shell chooser to open.
        // Drain the edge so reconnecting later cannot replay a stale gesture.
        while self.root_widget.take_capture_studio_request().is_some() {}
        for request in crate::shell_tabs::take_requests() {
            self.apply_unhosted_tab_request(request);
            changed = true;
        }
        // No desktop bar: the runner's own row is the window's bar, hosting
        // the app's slots and serving its search and location requests
        // (`native_runner::slot_bar`).
        // No desktop agent to ask: offer no entry, and drop what was asked
        // while the connection went away.
        crate::shared_selection::set_agent_host_available(false);
        let _ = crate::shared_selection::take_agent_requests();
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
        if let Some(intent) = self.take_native_host_intent() {
            match intent {
                crate::widget::AppHostIntent::AllowHostShell { reply } => {
                    self.floating.host_shell_allow = Some(reply);
                    client.send(
                        &crate::floating_surface::FloatingSurfaceClientMessage::AllowHostShell,
                    )?;
                }
                crate::widget::AppHostIntent::MoveDeviceDisplay {
                    request: placement,
                    reply,
                } => {
                    if self.floating.display_move.is_some() {
                        if let Some(reply) = reply.upgrade() {
                            reply.borrow_mut().result =
                                Some(Err(crate::i18n::I18nManager::lookup_plain(
                                    "device-display-request-pending",
                                )));
                        }
                    } else if reply.upgrade().is_some() {
                        self.floating.next_display_move =
                            self.floating.next_display_move.wrapping_add(1);
                        let request = self.floating.next_display_move;
                        self.floating.display_move = Some((request, reply));
                        client.send(&crate::floating_surface::FloatingSurfaceClientMessage::MoveDeviceDisplay { request, placement })?;
                    }
                }
                _ => {}
            }
            *changed = true;
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
        // The window's bar shows the app's slots while the shell hears it.
        if crate::window_toolbar::set_hosted(true) {
            *changed = true;
        }
        let toolbar = crate::window_toolbar::revision();
        if self.floating.toolbar_sent != Some(toolbar) {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::Toolbar {
                    toolbar: crate::window_toolbar::shown(),
                    instance,
                },
            )?;
            self.floating.toolbar_sent = Some(toolbar);
        }
        if crate::window_toolbar::take_edit_request() {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::EditLocation { instance },
            )?;
            *changed = true;
        }
        // The window's bar presents this app's search and History: its
        // header drops its own, and the bar hears what the window offers.
        if !self.floating.window_bar.presents() {
            self.set_window_presents_history(true);
            *changed = true;
        }
        if let Some(offer) = self.take_window_offer_change() {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::WindowOffer {
                    offer,
                    instance,
                },
            )?;
        }
        if let Some(opening) = crate::window_toolbar::take_search_request() {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::OpenSearch {
                    instance,
                    opening,
                },
            )?;
            *changed = true;
        }
        // History's changes are described by the shell's model, never the
        // app's; what it answered is written once no edit is open.
        self.write_history_descriptions();
        let changes = self
            .history
            .as_mut()
            .map(|history| history.take_describe_request())
            .unwrap_or_default();
        if !changes.is_empty() {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::DescribeHistory { changes },
            )?;
        }
        if !self.floating.selection_capable_sent {
            client
                .send(&crate::floating_surface::FloatingSurfaceClientMessage::SelectionCapable)?;
            self.floating.selection_capable_sent = true;
            crate::shared_selection::set_agent_host_available(true);
        }
        // What this window shows, so the desktop's own menus for it (its
        // window menu) attach it as the apps' menus do.
        let window = self.window_resource();
        if !self.floating.window_resource_sent.was(&window) {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::WindowResource {
                    resource: window.clone(),
                },
            )?;
            self.floating.window_resource_sent = Told::Was(window);
        }
        self.send_window_share(client, instance)?;
        for request in crate::shared_selection::take_agent_requests() {
            let (selection, bounds) = self.agent_reference(request);
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::AskAgent {
                    selection,
                    bounds,
                },
            )?;
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
        self.send_floating_present_requests(client)?;
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

    /// Tell the shell what changed of this window's live sharing
    /// ([`crate::window_share`]): its record once, its embeds and what it
    /// shares whenever they change.
    #[cfg(feature = "native")]
    fn send_window_share(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
        instance: Option<u64>,
    ) -> Result<(), String> {
        let sent = &mut self.floating.window_share_sent;
        if !sent.record
            && let Some((record, kind)) = self.floating.window_record.clone()
        {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::WindowRecord {
                    record,
                    kind,
                    instance,
                },
            )?;
            sent.record = true;
        }
        if let Some((revision, embeds)) = crate::window_share::own_embeds_since(sent.embeds) {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::Embeds {
                    embeds: embeds
                        .iter()
                        .map(crate::window_share::DeclaredEmbed::from)
                        .collect(),
                    instance,
                },
            )?;
            sent.embeds = revision;
        }
        if let Some((revision, shared)) = crate::window_share::shared_since(sent.shared) {
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::SharedWindows {
                    shared: shared
                        .into_iter()
                        .take(crate::window_share::MAX_SHARED)
                        .collect(),
                },
            )?;
            sent.shared = revision;
        }
        Ok(())
    }

    /// What the desktop said of windows shared live changed what the app
    /// shows: it syncs and lays out again.
    fn window_share_changed(&mut self) {
        self.needs_sync = true;
        self.layout.layout_dirty = true;
    }

    #[cfg(feature = "native")]
    fn send_floating_present_requests(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
    ) -> Result<(), String> {
        while let Some(request) = self.root_widget.take_floating_surface_request() {
            self.floating.suspended_ids.remove(&request.id);
            client.send(
                &crate::floating_surface::FloatingSurfaceClientMessage::Present {
                    request: (&request).into(),
                },
            )?;
            if request.role.uses_app_owned_host()
                && matches!(
                    request.source,
                    crate::floating_surface::FloatingSource::AppElement { .. }
                )
                && self.root_widget.floating_surface_ime(&request.id).is_some()
            {
                self.floating.pending_ime_ids.insert(request.id);
            }
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
                        input_rect: presentation
                            .input_rect
                            .map(crate::floating_surface::protocol_rect_array),
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
                FloatingSurfaceHostMessage::DeviceDisplayMoveResult { request, result } => {
                    if self
                        .floating
                        .display_move
                        .as_ref()
                        .is_some_and(|(pending, _)| pending == request)
                        && let Some((_, reply)) = self.floating.display_move.take()
                        && let Some(reply) = reply.upgrade()
                    {
                        reply.borrow_mut().result = Some(result.clone());
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
                    self.floating.pending_ime_ids.remove(id);
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
                    self.floating.pending_ime_ids.remove(id);
                    self.floating.prepared_ids.remove(id);
                    self.floating.coordinate_offsets.remove(id);
                    self.floating.suspended_ids.remove(id);
                }
                FloatingSurfaceHostMessage::Suspended { id } => {
                    self.floating.pending_ime_ids.remove(id);
                    self.floating.prepared_ids.remove(id);
                    self.floating.coordinate_offsets.remove(id);
                    self.floating.suspended_ids.insert(id.clone());
                }
                FloatingSurfaceHostMessage::Appearance {
                    network_status,
                    room_sync,
                    ..
                } => {
                    // A room that has not synced shows so in every empty
                    // state, so what it shows changes with it.
                    if crate::room_sync::set(room_sync.clone()) {
                        self.layout.layout_dirty = true;
                        self.layout.request_rebuild();
                    }
                    if let Some(status) = network_status {
                        self.floating.network_status_active = true;
                        tilo_host::network_status::set_host_status(
                            status.available,
                            status.metered,
                        );
                    } else {
                        if self.floating.network_status_active {
                            tilo_host::network_status::clear_host_status();
                        }
                        self.floating.network_status_active = false;
                    }
                    if let Some(theme) = message.configured_theme() {
                        self.floating.host_theme = Some(theme.clone());
                        if self.accessibility.base_theme.document != theme.document {
                            self.set_theme(theme);
                        }
                    }
                    if let Some(appearance) = message.window_appearance() {
                        self.set_window_appearance(appearance);
                    }
                    // The window's own concern, not a floating surface's.
                    continue;
                }
                // Never delivered: the client's `pump` ends on it.
                FloatingSurfaceHostMessage::VersionMismatch { .. } => {}
                FloatingSurfaceHostMessage::PaintKeyframe => {
                    crate::webview::paint::set_share_watched(true);
                }
                FloatingSurfaceHostMessage::PaintStop => {
                    crate::webview::paint::set_share_watched(false);
                }
                FloatingSurfaceHostMessage::ToolbarEvent { event } => {
                    // The bar's search and History act here; the app's own
                    // slots are the app's to read, on its sync pass
                    // (`window_toolbar::take_events`). A host message asks
                    // for a new frame, but a frame syncs only when something
                    // asked for it: without this, what the person typed or
                    // committed in the bar waited, unread, for whatever made
                    // the app sync next (Enter in the address opened nothing).
                    if self.apply_window_bar_event(event).is_none() {
                        self.deliver_app_slot_event(event.clone());
                        self.needs_sync = true;
                    }
                }
                FloatingSurfaceHostMessage::HistoryDescriptions { described } => {
                    if let Some(history) = self.history.as_mut() {
                        history.receive_descriptions(described.clone());
                    }
                    self.write_history_descriptions();
                }
                FloatingSurfaceHostMessage::PointAt { request, point } => {
                    self.refresh_semantic_tree();
                    let control = self
                        .semantic_tree()
                        .pointed_at(vello::kurbo::Point::new(point[0], point[1]));
                    if let Some(client) = &mut self.floating.client {
                        let _ = client.send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::Pointed {
                                request: *request,
                                control,
                            },
                        );
                    }
                }
                FloatingSurfaceHostMessage::Pick { request, query } => {
                    let answer = crate::pick_wire::answer(self.root_widget.as_ref(), query);
                    if let Some(client) = &mut self.floating.client {
                        let _ = client.send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::PickAnswered {
                                request: *request,
                                answer,
                            },
                        );
                    }
                }
                FloatingSurfaceHostMessage::ShareSelection => {
                    let (selection, bounds) =
                        self.agent_reference(crate::shared_selection::AgentRequest::default());
                    if let Some(client) = &mut self.floating.client {
                        let _ = client.send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::AskAgent {
                                selection,
                                bounds,
                            },
                        );
                    }
                }
                // What the desktop says of windows shared live reaches the
                // app's next sync, where it reads them
                // (`crate::window_share`).
                FloatingSurfaceHostMessage::DrawsEmbeds => {
                    crate::window_share::set_drawn(true);
                    self.window_share_changed();
                }
                FloatingSurfaceHostMessage::EmbedStates { states } => {
                    if crate::window_share::set_own_states(states.clone()) {
                        self.window_share_changed();
                    }
                }
                FloatingSurfaceHostMessage::OpenWindows { windows } => {
                    let windows = windows
                        .iter()
                        .take(crate::window_share::MAX_WINDOWS)
                        .map(crate::window_share::OpenWindow::bounded)
                        .collect();
                    if crate::window_share::publish_windows(windows) {
                        self.window_share_changed();
                    }
                }
                FloatingSurfaceHostMessage::WindowShareAsk { ask } => {
                    crate::window_share::deliver_ask(ask.bounded());
                    self.window_share_changed();
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
        // A window still waiting for the shell's first word never gets
        // it: what its room holds says it cannot arrive.
        if crate::room_sync::shell_lost() {
            self.layout.layout_dirty = true;
            self.layout.request_rebuild();
        }
        // The bar that showed the app's slots is gone with the shell; a new
        // connection is told them afresh.
        crate::window_toolbar::set_hosted(false);
        self.floating.toolbar_sent = None;
        self.floating.window_resource_sent = Told::Nothing;
        // Nothing draws the embeds or lists the windows any more; a new
        // shell is told the embeds, the shares and the record afresh.
        crate::window_share::host_lost();
        self.floating.window_share_sent = WindowShareSent::default();
        self.window_share_changed();
        // Nothing presents the app's search and History any more: it shows
        // its own again.
        self.floating.window_bar.disconnected();
        if let Some(history) = self.history.as_mut() {
            history.describing_disconnected();
        }
        if self.floating.window_bar.presents() {
            self.set_window_presents_history(false);
        }
        if self.floating.network_status_active {
            tilo_host::network_status::clear_host_status();
            self.floating.network_status_active = false;
        }
        if let Some((_, reply)) = self.floating.display_move.take()
            && let Some(reply) = reply.upgrade()
        {
            reply.borrow_mut().result = Some(Err(crate::i18n::I18nManager::lookup_plain(
                "device-display-session-disconnected",
            )));
        }
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
            .chain(self.floating.pending_ime_ids.iter())
            .chain(self.floating.suspended_ids.iter())
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.floating.presentations.clear();
        self.floating.active_ids.clear();
        self.floating.prepared_ids.clear();
        self.floating.pending_ime_ids.clear();
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
                position: winit::dpi::PhysicalPosition::new(position.x + dx, position.y + dy),
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
    request.message(instance)
}

impl App {
    /// What `request` is about, as the shell is told: a typed reference
    /// and where in this window it is drawn.
    #[cfg(feature = "native")]
    fn agent_reference(
        &self,
        request: crate::shared_selection::AgentRequest,
    ) -> (
        Option<crate::shared_selection::SharedSelection>,
        Option<[f64; 4]>,
    ) {
        request.resolve(
            crate::shared_selection::subtree_selection(self.root_widget.as_ref()),
            &self.collect_resources(),
        )
    }

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
            // window manager, and there is no closed-tab history to reopen
            // nor a group of tabs to step through.
            TabRequest::ReopenClosed
            | TabRequest::CycleTab { .. }
            | TabRequest::PlaceNextWindowBeside { .. } => {}
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

#[cfg(test)]
#[cfg(feature = "native")]
mod host_shell_allow_tests {
    use super::*;

    /// The shell asks what is at a point of the window; the app names the
    /// control there by its durable id, role and name, and nothing where
    /// nothing is.
    #[cfg(unix)]
    #[test]
    fn a_point_of_the_window_is_answered_with_the_control_drawn_there() {
        use crate::floating_surface::{FloatingSurfaceClientMessage, PointedControl};
        use crate::widget::{Button, Column};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let (app_side, mut shell_side) = UnixStream::pair().unwrap();
        shell_side.set_nonblocking(true).unwrap();
        let root = Column::new()
            .with_no_padding()
            .with_gap(20.0)
            .push(Box::new(Button::new("save", "Save")))
            .push(Box::new(Button::new("cancel", "Cancel")));
        let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(root));
        app.init_layout();
        app.layout.layout_dirty = true;
        let root_node = app.layout.root_node.unwrap();
        crate::app::run_layout_pass(&mut app, root_node, 800, 600, 1.0, false, None).unwrap();
        app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
        app.refresh_semantic_tree();
        let cancel = app
            .semantic_tree()
            .find_by_name("Cancel")
            .and_then(|node| node.bounds)
            .expect("the Cancel button is laid out");

        let below = cancel.y1 + 400.0;
        for (request, point) in [
            (1, [cancel.center().x, cancel.center().y]),
            (2, [cancel.center().x, below]),
        ] {
            let mut wire =
                serde_json::to_vec(&FloatingSurfaceHostMessage::PointAt { request, point })
                    .unwrap();
            wire.push(b'\n');
            shell_side.write_all(&wire).unwrap();
        }
        app.pump_floating_surface_service();
        let mut received = Vec::new();
        assert_eq!(
            shell_side.read_to_end(&mut received).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let answers: Vec<(u64, Option<PointedControl>)> = received
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| match serde_json::from_slice(line).unwrap() {
                FloatingSurfaceClientMessage::Pointed { request, control } => {
                    Some((request, control))
                }
                _ => None,
            })
            .collect();
        let [(1, Some(control)), (2, None)] = answers.as_slice() else {
            panic!("one answer per request: {answers:?}");
        };
        assert_eq!(control.role, "Button");
        assert_eq!(control.label.as_deref(), Some("Cancel"));
        assert!(control.id.contains("cancel"), "{}", control.id);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn network_status_follows_the_real_service_pump_and_discards_disconnected_host() {
        use crate::floating_surface::HostNetworkStatus;
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let (app_side, mut shell_side) = UnixStream::pair().unwrap();
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Container::new()),
        );
        app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
        for metered in [None, Some(true), Some(false)] {
            let mut message = FloatingSurfaceHostMessage::appearance(&Default::default());
            if let FloatingSurfaceHostMessage::Appearance { network_status, .. } = &mut message {
                *network_status = Some(HostNetworkStatus {
                    available: Some(true),
                    metered,
                });
            }
            let mut frame = serde_json::to_vec(&message).unwrap();
            frame.push(b'\n');
            shell_side.write_all(&frame).unwrap();
            app.pump_floating_surface_service();
            assert_eq!(tilo_host::network_status::current().metered, metered);
        }
        drop(shell_side);
        app.pump_floating_surface_service();
        assert!(!app.floating.network_status_active);
        assert_ne!(tilo_host::network_status::current().metered, Some(false));
        // An older host has no richer status; the same appearance still parses.
        let old: FloatingSurfaceHostMessage =
            serde_json::from_str(r#"{"type":"appearance","workspace_hue":180}"#).unwrap();
        assert!(matches!(
            old,
            FloatingSurfaceHostMessage::Appearance {
                network_status: None,
                ..
            }
        ));
        // Loss is terminal for this App: pumping again cannot reacquire a
        // consumed inherited descriptor or resurrect the stale snapshot.
        app.pump_floating_surface_service();
        assert!(app.floating.client.is_none());
        assert_ne!(tilo_host::network_status::current().metered, Some(false));
        drop(app);

        // A fresh App gets a fresh launch connection. This controlled fixture
        // covers closure; the launcher boundary fixture qualifies discovery.
        let (app_side, mut shell_side) = UnixStream::pair().unwrap();
        let mut relaunched = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Container::new()),
        );
        relaunched.floating.client =
            Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
        let mut message = FloatingSurfaceHostMessage::appearance(&Default::default());
        if let FloatingSurfaceHostMessage::Appearance { network_status, .. } = &mut message {
            *network_status = Some(HostNetworkStatus {
                available: Some(true),
                metered: Some(false),
            });
        }
        let mut frame = serde_json::to_vec(&message).unwrap();
        frame.push(b'\n');
        shell_side.write_all(&frame).unwrap();
        relaunched.pump_floating_surface_service();
        assert_eq!(tilo_host::network_status::current().metered, Some(false));
        drop(relaunched);
        assert_ne!(tilo_host::network_status::current().metered, Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn device_shell_operation_is_acknowledged_by_the_real_service_pump() {
        use crate::floating_surface::{DeviceShellOperation, FloatingSurfaceClientMessage};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (app_side, mut shell_side) = UnixStream::pair().unwrap();
        shell_side.set_nonblocking(true).unwrap();
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Container::new()),
        );
        app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
        for request in [71, 72] {
            let mut wire = serde_json::to_vec(&FloatingSurfaceHostMessage::DeviceShellOperation {
                request,
                operation: DeviceShellOperation::Inspect,
            })
            .unwrap();
            wire.push(b'\n');
            shell_side.write_all(&wire).unwrap();
            assert!(app.pump_floating_surface_service());
            let mut received = Vec::new();
            let error = shell_side.read_to_end(&mut received).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            let outcomes = received
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<FloatingSurfaceClientMessage>(line).unwrap())
                .filter_map(|message| match message {
                    FloatingSurfaceClientMessage::DeviceShellOutcome { request, result } => {
                        Some((request, result))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                outcomes,
                vec![(request, Err("Terminal is not available".into()))],
                "even refusal must acknowledge the exact operation through the live authenticated connection"
            );
            assert!(
                app.floating.client.is_some(),
                "the same connection remains available for the next request"
            );
        }
    }

    struct DeviceShellProbe {
        calls: std::rc::Rc<std::cell::Cell<usize>>,
        container: crate::widget::Container,
    }

    impl Widget for DeviceShellProbe {
        crate::provides!(dyn crate::AppWidget);
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            theme: &crate::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            self.container.build_layout(taffy, theme, nodes)
        }
        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: vello::kurbo::Point,
        ) {
            self.container.apply_layout(taffy, node, origin);
        }
        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text: &mut crate::TextRenderer,
            theme: &crate::Theme,
            presence: &crate::PresenceCtx,
        ) {
            self.container.draw(scene, transform, text, theme, presence);
        }
        fn name(&self) -> &str {
            "DeviceShellProbe"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.container.get_rect()
        }
    }

    impl crate::AppWidget for DeviceShellProbe {
        fn app_kind(&self) -> crate::AppKind {
            crate::AppKind::Terminal
        }
        fn app_title(&self) -> &str {
            "Device shell probe"
        }
        fn app_icon(&self) -> crate::IconDescriptor {
            crate::IconDescriptor::Glyph("T")
        }
        fn permission_hint(&self) -> Option<crate::AppKind> {
            Some(crate::AppKind::Terminal)
        }
        fn device_shell_operation(
            &mut self,
            operation: &crate::floating_surface::DeviceShellOperation,
        ) -> Result<serde_json::Value, String> {
            self.calls.set(self.calls.get() + 1);
            match operation {
                crate::floating_surface::DeviceShellOperation::Inspect => {
                    Ok(serde_json::json!({"session": "exact-session"}))
                }
                _ => Err("Session no longer matches".into()),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn device_shell_success_and_refusal_are_sent_once_on_the_original_connection() {
        use crate::floating_surface::{DeviceShellOperation, FloatingSurfaceClientMessage};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let (app_side, mut shell_side) = UnixStream::pair().unwrap();
        shell_side.set_nonblocking(true).unwrap();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(DeviceShellProbe {
                calls: calls.clone(),
                container: crate::widget::Container::new(),
            }),
        );
        app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
        for (request, operation, expected) in [
            (
                91,
                DeviceShellOperation::Inspect,
                Ok(serde_json::json!({"session":"exact-session"})),
            ),
            (
                92,
                DeviceShellOperation::Read {
                    session: "stale-session".into(),
                },
                Err("Session no longer matches".into()),
            ),
        ] {
            let mut wire = serde_json::to_vec(&FloatingSurfaceHostMessage::DeviceShellOperation {
                request,
                operation,
            })
            .unwrap();
            wire.push(b'\n');
            shell_side.write_all(&wire).unwrap();
            assert!(app.pump_floating_surface_service());
            let mut received = Vec::new();
            assert_eq!(
                shell_side.read_to_end(&mut received).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            let outcomes = received
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<FloatingSurfaceClientMessage>(line).unwrap())
                .filter_map(|message| match message {
                    FloatingSurfaceClientMessage::DeviceShellOutcome { request, result } => {
                        Some((request, result))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(outcomes, vec![(request, expected)]);
            let executed = calls.get();
            app.pump_floating_surface_service();
            assert_eq!(
                calls.get(),
                executed,
                "idle pumping must not replay a consumed operation"
            );
            let mut extra = [0; 1];
            assert_eq!(
                shell_side.read(&mut extra).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
        assert_eq!(calls.get(), 2);
        drop(shell_side);
        app.pump_floating_surface_service();
        assert!(
            app.floating.client.is_none(),
            "a disconnected connection is never restored"
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn display_move_reply_matches_exact_request_and_disconnect_never_replays_it() {
        let mut app = App::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Container::new()),
        );
        let reply = std::rc::Rc::new(std::cell::RefCell::new(
            crate::widget::DeviceDisplayMoveReply::default(),
        ));
        app.floating.display_move = Some((7, std::rc::Rc::downgrade(&reply)));
        let message = FloatingSurfaceHostMessage::DeviceDisplayMoveResult {
            request: 8,
            result: Ok(()),
        };
        let wire = serde_json::to_vec(&message).unwrap();
        app.apply_floating_host_messages(vec![serde_json::from_slice(&wire).unwrap()]);
        assert!(
            reply.borrow().result.is_none(),
            "another gesture cannot acknowledge this move"
        );
        app.apply_floating_host_messages(vec![
            FloatingSurfaceHostMessage::DeviceDisplayMoveResult {
                request: 7,
                result: Ok(()),
            },
        ]);
        assert_eq!(reply.borrow_mut().result.take(), Some(Ok(())));
        assert!(app.floating.display_move.is_none());
        app.floating.display_move = Some((9, std::rc::Rc::downgrade(&reply)));
        app.reattach_after_floating_service_loss();
        assert!(reply.borrow_mut().result.take().unwrap().is_err());
        assert!(app.floating.display_move.is_none());
        app.apply_floating_host_messages(vec![
            FloatingSurfaceHostMessage::DeviceDisplayMoveResult {
                request: 9,
                result: Ok(()),
            },
        ]);
        assert!(
            reply.borrow().result.is_none(),
            "late success cannot revive a disconnected gesture"
        );
    }

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
