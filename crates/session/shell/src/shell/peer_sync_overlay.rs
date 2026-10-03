//! Peer invite/connect overlay.
//!
//! This component owns the complete local interaction: prompt state, editing,
//! clipboard actions, painted geometry and input routing. The shell host only
//! performs the network request emitted by [`PeerSyncOverlay::handle_event`]
//! and returns its result through [`PeerSyncOverlay::finish_request`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;

use crate::shell::i18n::{t, t_args};
use ui::theme::Theme;

/// A transport request emitted by the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncRequest {
    IssueInvite,
    ExportIdentity,
    IssueInviteFor {
        name: String,
        role: crate::network::invite::InviteRole,
        recipient_user_public_key: [u8; 32],
    },
    AcceptInvite(String),
    PollEnrollment(u64),
    CancelEnrollment(u64),
    ListPendingInvitations,
    ResendInvitation([u8; 32]),
    CancelInvitation([u8; 32]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInvitationSummary {
    pub id: [u8; 32],
    pub name: String,
    pub role: crate::network::invite::InviteRole,
    pub expiry_unix_seconds: u64,
}

/// A person whose identity was previously verified by the shell or a trusted
/// Contacts integration. The key, rather than the display name, is what makes
/// an invitation recipient-bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RememberedInvitee {
    pub name: String,
    pub user_public_key: [u8; 32],
    /// A short, human-checkable description of why this choice is safe to
    /// reuse (for example, "Verified on 2 Sep").
    pub verification: String,
}

/// Shell-owned context shown before an invitation is minted. It is display
/// context only: the signed token remains the source of authority.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InviteContext {
    pub workspace_name: String,
    pub inviter_name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InviteRecipientPath {
    /// A key was selected from a previously verified identity.
    Known,
    /// The recipient supplied a fresh identity card out of band.
    #[default]
    New,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncResponse {
    EnrollmentPending { id: u64 },
    EnrollmentJoined { id: u64, room: String },
    Message(String),
    PendingInvitations(Vec<PendingInvitationSummary>),
}

impl From<String> for PeerSyncResponse {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<&str> for PeerSyncResponse {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

/// State exposed to the compositor's structured test dump.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerSyncSnapshot {
    pub invite_token: Option<String>,
    pub connect_input: Option<String>,
    pub invite_name: Option<String>,
    pub invite_for: Option<String>,
    pub invite_role: crate::network::invite::InviteRole,
    pub pending_invitations: Vec<PendingInvitationSummary>,
    pub invite_recipient_key: Option<[u8; 32]>,
    pub invite_recipient_path: InviteRecipientPath,
    pub invite_review_open: bool,
    pub invite_context: InviteContext,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    CreateInvite,
    ReviewInvite,
    BackToComposer,
    SelectRole(crate::network::invite::InviteRole),
    SelectKnown(usize),
    SelectNewRecipient,
    Join,
    Cancel,
    CopyToken,
    PasteToken,
    Resend([u8; 32]),
    CancelPending([u8; 32]),
    PasteIdentity,
    CopyIdentity,
}

/// Result of routing one input event through the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventOutcome {
    pub consumed: bool,
    pub request: Option<PeerSyncRequest>,
    /// A named invite asks to copy only after minting succeeds.
    pub copy_result: bool,
}

impl EventOutcome {
    fn consumed() -> Self {
        Self {
            consumed: true,
            request: None,
            copy_result: false,
        }
    }

    fn ignored() -> Self {
        Self {
            consumed: false,
            request: None,
            copy_result: false,
        }
    }

    fn request(request: PeerSyncRequest, copy_result: bool) -> Self {
        Self {
            consumed: true,
            request: Some(request),
            copy_result,
        }
    }
}

#[derive(Default)]
pub struct PeerSyncOverlay {
    state: PeerSyncSnapshot,
    remembered_invitees: Vec<RememberedInvitee>,
    rect: Rect,
    button_rects: Vec<(Button, Rect)>,
    pending_offset: usize,
    enrollment: Option<u64>,
    enrollment_poll_at: Option<std::time::Instant>,
    retired_enrollments: std::collections::VecDeque<u64>,
}

impl PeerSyncOverlay {
    pub fn pending_enrollment_request(&mut self) -> Option<PeerSyncRequest> {
        if let Some(id) = self.retired_enrollments.pop_front() {
            return Some(PeerSyncRequest::CancelEnrollment(id));
        }
        let id = self.enrollment?;
        let now = std::time::Instant::now();
        if self.enrollment_poll_at.is_some_and(|previous| {
            now.duration_since(previous) < std::time::Duration::from_millis(200)
        }) {
            return None;
        }
        self.enrollment_poll_at = Some(now);
        Some(PeerSyncRequest::PollEnrollment(id))
    }
    fn retire_enrollment(&mut self) {
        if let Some(id) = self.enrollment.take() {
            self.retired_enrollments.push_back(id);
        }
        self.enrollment_poll_at = None;
    }

    pub fn snapshot(&self) -> PeerSyncSnapshot {
        self.state.clone()
    }

    pub fn visible(&self) -> bool {
        self.state.connect_input.is_some()
            || self.state.invite_token.is_some()
            || self.state.invite_name.is_some()
    }

    /// Set the display-only room context used by the invite review. Keeping it
    /// on the overlay makes callers provide it deliberately instead of
    /// accidentally minting a token from an ambiguous shell surface.
    pub fn set_invite_context(&mut self, context: InviteContext) {
        self.state.invite_context = context;
    }

    /// Replace the trusted, remembered identities offered by the “someone I
    /// know” path. Callers must have verified these identities before passing
    /// them here; this component never turns a typed name into a trusted key.
    pub fn set_remembered_invitees(&mut self, invitees: Vec<RememberedInvitee>) {
        self.remembered_invitees = invitees;
    }

    /// Add identities derived from the active room's governance projection
    /// without discarding identities supplied by Contacts or another trusted
    /// source. The root key is the deduplication key; labels are never used as
    /// identity.
    pub fn merge_remembered_invitees(&mut self, invitees: Vec<RememberedInvitee>) {
        for invitee in invitees {
            if !self
                .remembered_invitees
                .iter()
                .any(|known| known.user_public_key == invitee.user_public_key)
            {
                self.remembered_invitees.push(invitee);
            }
        }
    }

    pub fn prompt_invite(&mut self) {
        self.retire_enrollment();
        self.state.invite_name = Some(String::new());
        self.state.connect_input = None;
        self.state.invite_token = None;
        self.state.invite_for = None;
        self.state.invite_recipient_key = None;
        self.state.invite_recipient_path = InviteRecipientPath::New;
        self.state.invite_review_open = false;
        self.state.status = None;
        self.pending_offset = 0;
    }

    pub fn prompt_invite_for(&mut self, name: String) {
        self.prompt_invite();
        self.state.invite_name = Some(name);
    }

    pub fn prompt_connect(&mut self) {
        self.retire_enrollment();
        self.state.invite_name = None;
        self.state.connect_input = Some(String::new());
        self.state.status = Some(t("peer-sync-paste-token"));
    }

    pub fn issue_invite_request(&self) -> PeerSyncRequest {
        PeerSyncRequest::IssueInvite
    }

    /// Apply the host's transport result. Failed named invites retain the
    /// typed name so they can be retried; successful connects close the input.
    pub fn finish_request(
        &mut self,
        request: &PeerSyncRequest,
        outcome: Result<String, String>,
        copy_result: bool,
    ) {
        let outcome = outcome.map(PeerSyncResponse::Message);
        self.finish_typed_request(request, outcome, copy_result);
    }

    pub fn finish_typed_request(
        &mut self,
        request: &PeerSyncRequest,
        outcome: Result<PeerSyncResponse, String>,
        copy_result: bool,
    ) {
        match (request, outcome) {
            (PeerSyncRequest::AcceptInvite(_), Ok(PeerSyncResponse::EnrollmentPending { id })) => {
                self.enrollment = Some(id);
                self.enrollment_poll_at = None;
                self.state.status = Some("Joining…".into());
            }
            (
                PeerSyncRequest::PollEnrollment(requested),
                Ok(PeerSyncResponse::EnrollmentPending { id }),
            ) if *requested == id && self.enrollment == Some(id) => {}
            (
                PeerSyncRequest::PollEnrollment(requested),
                Ok(PeerSyncResponse::EnrollmentJoined { id, room }),
            ) if *requested == id && self.enrollment == Some(id) => {
                self.enrollment = None;
                self.enrollment_poll_at = None;
                self.state.connect_input = None;
                self.state.status = Some(format!("Joined room {room}"));
            }
            (PeerSyncRequest::PollEnrollment(id), Err(error)) if self.enrollment == Some(*id) => {
                self.enrollment = None;
                self.enrollment_poll_at = None;
                self.state.status = Some(format!("Join incomplete: {error}"));
            }
            (PeerSyncRequest::PollEnrollment(_) | PeerSyncRequest::CancelEnrollment(_), _) => {}

            (PeerSyncRequest::IssueInvite, Ok(PeerSyncResponse::Message(token))) => {
                self.finish_invite(token, None, copy_result);
            }
            (PeerSyncRequest::ExportIdentity, Ok(PeerSyncResponse::Message(card))) => {
                ui::text_selection::clipboard_copy(&card);
                self.state.status = Some(t("peer-sync-identity-copied"));
            }
            (
                PeerSyncRequest::IssueInviteFor { name, .. },
                Ok(PeerSyncResponse::Message(token)),
            ) => {
                self.finish_invite(token, Some(name.clone()), copy_result);
            }
            (PeerSyncRequest::IssueInvite | PeerSyncRequest::IssueInviteFor { .. }, Err(error)) => {
                self.state.invite_token = None;
                self.state.invite_for = None;
                self.state.status = Some(t_args(
                    "peer-sync-invite-failed",
                    &[("error", error.into())],
                ));
            }
            (PeerSyncRequest::ExportIdentity, Err(error)) => {
                self.state.status = Some(t_args(
                    "peer-sync-identity-unavailable",
                    &[("error", error.into())],
                ));
            }
            (PeerSyncRequest::AcceptInvite(_), Ok(PeerSyncResponse::Message(room))) => {
                self.state.connect_input = None;
                self.state.status = Some(t_args("peer-sync-joined-room", &[("room", room.into())]));
            }
            (PeerSyncRequest::AcceptInvite(_), Err(error)) => {
                self.state.status =
                    Some(t_args("peer-sync-join-refused", &[("error", error.into())]));
            }
            (
                PeerSyncRequest::ListPendingInvitations,
                Ok(PeerSyncResponse::PendingInvitations(rows)),
            ) => self.state.pending_invitations = rows,
            (PeerSyncRequest::ListPendingInvitations, Err(error)) => {
                self.state.status = Some(t_args(
                    "peer-sync-pending-unavailable",
                    &[("error", error.into())],
                ));
            }
            (PeerSyncRequest::ResendInvitation(id), Ok(PeerSyncResponse::Message(token))) => {
                let name = self
                    .state
                    .pending_invitations
                    .iter()
                    .find(|invite| &invite.id == id)
                    .map(|invite| invite.name.clone());
                self.finish_invite(token, name, true);
            }
            (PeerSyncRequest::ResendInvitation(_), Err(error)) => {
                self.state.status = Some(t_args(
                    "peer-sync-resend-failed",
                    &[("error", error.into())],
                ));
            }
            (PeerSyncRequest::CancelInvitation(_), Ok(PeerSyncResponse::Message(_))) => {
                self.state.status = Some(t("peer-sync-invite-canceled"));
            }
            (PeerSyncRequest::CancelInvitation(_), Err(error)) => {
                self.state.status = Some(t_args(
                    "peer-sync-cancel-failed",
                    &[("error", error.into())],
                ));
            }
            (_, Ok(_)) => {
                self.state.status = Some(t("peer-sync-unexpected-response"));
            }
        }
    }

    fn finish_invite(&mut self, token: String, invite_for: Option<String>, copy_result: bool) {
        self.state.status = None;
        self.state.invite_token = Some(token);
        self.state.invite_for = invite_for;
        self.state.invite_name = None;
        self.state.invite_review_open = false;
        if copy_result {
            self.copy_invite_token();
        }
    }

    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        cursor: Option<Point>,
    ) -> EventOutcome {
        let connect_open = self.state.connect_input.is_some();
        let composer_open = self.state.invite_name.is_some();
        let mods = modifiers.state();
        let chord = mods.control_key() || mods.super_key();
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key(logical_key, chord, connect_open, composer_open),
            WindowEvent::KeyboardInput { .. } => EventOutcome {
                consumed: connect_open || composer_open,
                request: None,
                copy_result: false,
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(point) = cursor else {
                    return EventOutcome::ignored();
                };
                if let Some(button) = self.button_rects.iter().find_map(|(button, rect)| {
                    (rect.area() > 0.0 && rect.contains(point)).then_some(*button)
                }) {
                    return self.activate(button);
                }
                EventOutcome {
                    consumed: self.visible() && self.rect.contains(point),
                    request: None,
                    copy_result: false,
                }
            }
            WindowEvent::MouseWheel { delta, .. }
                if composer_open && !self.state.invite_review_open =>
            {
                let down = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y < 0.0,
                    winit::event::MouseScrollDelta::PixelDelta(point) => point.y < 0.0,
                };
                let max = self.state.pending_invitations.len().saturating_sub(3);
                self.pending_offset = if down {
                    self.pending_offset.saturating_add(1).min(max)
                } else {
                    self.pending_offset.saturating_sub(1)
                };
                EventOutcome::consumed()
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::MouseInput { .. } => EventOutcome {
                consumed: self.visible() && cursor.is_some_and(|p| self.rect.contains(p)),
                request: None,
                copy_result: false,
            },
            _ => EventOutcome::ignored(),
        }
    }

    fn handle_key(
        &mut self,
        key: &Key,
        chord: bool,
        connect_open: bool,
        composer_open: bool,
    ) -> EventOutcome {
        match key {
            Key::Character(s) if chord && s.eq_ignore_ascii_case("c") => {
                self.copy_invite_token();
                EventOutcome::consumed()
            }
            Key::Character(s) if chord && s.eq_ignore_ascii_case("v") => {
                if composer_open {
                    self.paste_recipient_identity();
                } else {
                    self.paste_invite_token();
                }
                EventOutcome::consumed()
            }
            Key::Named(NamedKey::Escape) if self.visible() => {
                if self.state.invite_review_open {
                    self.state.invite_review_open = false;
                    self.state.status = None;
                    return EventOutcome::consumed();
                }
                self.dismiss();
                EventOutcome::consumed()
            }
            Key::Named(NamedKey::Enter) if composer_open && self.state.invite_review_open => {
                self.submit_invite()
            }
            Key::Named(NamedKey::Enter) if composer_open => self.begin_review(),
            Key::Named(NamedKey::Enter) if connect_open => self.submit_connect(),
            Key::Named(NamedKey::Backspace)
                if connect_open || (composer_open && !self.state.invite_review_open) =>
            {
                self.active_input_mut(connect_open).map(String::pop);
                EventOutcome::consumed()
            }
            Key::Character(s)
                if (connect_open || (composer_open && !self.state.invite_review_open))
                    && !chord =>
            {
                if let Some(input) = self.active_input_mut(connect_open) {
                    input.push_str(s.as_str());
                }
                EventOutcome::consumed()
            }
            Key::Named(NamedKey::Space)
                if composer_open && !self.state.invite_review_open && !chord =>
            {
                if let Some(input) = self.state.invite_name.as_mut() {
                    input.push(' ');
                }
                EventOutcome::consumed()
            }
            _ if connect_open || composer_open => EventOutcome::consumed(),
            _ => EventOutcome::ignored(),
        }
    }

    fn active_input_mut(&mut self, connect_open: bool) -> Option<&mut String> {
        if connect_open {
            self.state.connect_input.as_mut()
        } else if self.state.invite_recipient_path == InviteRecipientPath::New {
            self.state.invite_name.as_mut()
        } else {
            None
        }
    }

    fn activate(&mut self, button: Button) -> EventOutcome {
        match button {
            Button::CreateInvite => self.submit_invite(),
            Button::ReviewInvite => self.begin_review(),
            Button::BackToComposer => {
                self.state.invite_review_open = false;
                EventOutcome::consumed()
            }
            Button::SelectRole(role) => {
                self.state.invite_role = role;
                EventOutcome::consumed()
            }
            Button::SelectKnown(index) => {
                if let Some(person) = self.remembered_invitees.get(index) {
                    self.state.invite_name = Some(person.name.clone());
                    self.state.invite_recipient_key = Some(person.user_public_key);
                    self.state.invite_recipient_path = InviteRecipientPath::Known;
                    self.state.status = Some(t_args(
                        "peer-sync-using-verified-identity",
                        &[("verification", person.verification.as_str().into())],
                    ));
                }
                EventOutcome::consumed()
            }
            Button::SelectNewRecipient => {
                self.state.invite_recipient_path = InviteRecipientPath::New;
                self.state.invite_name = Some(String::new());
                self.state.invite_recipient_key = None;
                self.state.status = Some(t("peer-sync-ask-for-identity"));
                EventOutcome::consumed()
            }
            Button::Join => self.submit_connect(),
            Button::Cancel => {
                self.retire_enrollment();
                self.state.connect_input = None;
                self.state.invite_name = None;
                self.state.status = None;
                EventOutcome::consumed()
            }
            Button::CopyToken => {
                self.copy_invite_token();
                EventOutcome::consumed()
            }
            Button::PasteToken => {
                self.paste_invite_token();
                EventOutcome::consumed()
            }
            Button::Resend(id) => {
                EventOutcome::request(PeerSyncRequest::ResendInvitation(id), false)
            }
            Button::CancelPending(id) => {
                EventOutcome::request(PeerSyncRequest::CancelInvitation(id), false)
            }
            Button::PasteIdentity => {
                // Never carry a remembered person's display name over to a
                // freshly exchanged key; the token would remain safe, but the
                // recipient name recorded alongside it would be misleading.
                if self.state.invite_recipient_path == InviteRecipientPath::Known {
                    self.state.invite_name = Some(String::new());
                }
                self.state.invite_recipient_key = None;
                self.state.invite_recipient_path = InviteRecipientPath::New;
                self.paste_recipient_identity();
                EventOutcome::consumed()
            }
            Button::CopyIdentity => EventOutcome::request(PeerSyncRequest::ExportIdentity, false),
        }
    }

    fn submit_invite(&mut self) -> EventOutcome {
        if !self.state.invite_review_open {
            return self.begin_review();
        }
        let Some(name) = self.state.invite_name.as_deref() else {
            return EventOutcome::ignored();
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            self.state.status = Some(t("peer-sync-name-required"));
            return EventOutcome::consumed();
        }
        let Some(recipient_user_public_key) = self.state.invite_recipient_key else {
            self.state.status = Some(t("peer-sync-identity-required"));
            return EventOutcome::consumed();
        };
        EventOutcome::request(
            PeerSyncRequest::IssueInviteFor {
                name,
                role: self.state.invite_role,
                recipient_user_public_key,
            },
            true,
        )
    }

    /// Move into the explicit final review; no network operation happens here.
    fn begin_review(&mut self) -> EventOutcome {
        let Some(name) = self.state.invite_name.as_deref() else {
            return EventOutcome::ignored();
        };
        if name.trim().is_empty() {
            self.state.status = Some(t("peer-sync-name-required"));
            return EventOutcome::consumed();
        }
        if self.state.invite_recipient_key.is_none() {
            self.state.status = Some(t("peer-sync-recipient-required"));
            return EventOutcome::consumed();
        }
        self.state.invite_review_open = true;
        self.state.status = None;
        EventOutcome::consumed()
    }

    fn submit_connect(&self) -> EventOutcome {
        if self.enrollment.is_some() {
            return EventOutcome::consumed();
        }
        let Some(token) = self.state.connect_input.as_deref() else {
            return EventOutcome::ignored();
        };
        EventOutcome::request(
            PeerSyncRequest::AcceptInvite(token.trim().to_string()),
            false,
        )
    }

    fn copy_invite_token(&mut self) {
        let Some(token) = self.state.invite_token.as_deref() else {
            return;
        };
        ui::text_selection::clipboard_copy(token);
        self.state.status = Some(t("peer-sync-copied"));
    }

    fn paste_invite_token(&mut self) {
        if self.state.connect_input.is_none() {
            return;
        }
        let pasted = ui::text_selection::clipboard_paste()
            .map(|s| s.split_whitespace().collect::<String>())
            .filter(|s| !s.is_empty());
        match pasted {
            Some(token) => {
                self.state.connect_input = Some(token);
                self.state.status = Some(t("peer-sync-pasted"));
            }
            None => self.state.status = Some(t("peer-sync-clipboard-empty")),
        }
    }

    fn paste_recipient_identity(&mut self) {
        self.state.invite_recipient_path = InviteRecipientPath::New;
        let Some(raw) = ui::text_selection::clipboard_paste() else {
            self.state.status = Some(t("peer-sync-clipboard-empty"));
            return;
        };
        match parse_recipient_identity(&raw) {
            Some(key) => {
                self.state.invite_recipient_key = Some(key);
                self.state.status = Some(t_args(
                    "peer-sync-identity-bound",
                    &[("fingerprint", identity_fingerprint(&key).into())],
                ));
            }
            None => {
                self.state.status = Some(t("peer-sync-identity-invalid"));
            }
        }
    }

    fn dismiss(&mut self) {
        self.retire_enrollment();
        self.state = PeerSyncSnapshot::default();
    }

    pub fn draw(
        &mut self,
        host_rect: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // Theme type tokens already carry the user's text scale, and this
        // sheet lays out and paints with those resolved sizes; the renderer
        // must not scale them a second time (HIG §7a), so it is reset for
        // the pass exactly as the lens pass resets it, and restored after.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_at_theme_scale(host_rect, scene, transform, text_renderer, theme);
        text_renderer.set_text_scale(renderer_scale);
    }

    fn draw_at_theme_scale(
        &mut self,
        host_rect: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.button_rects.clear();
        if !self.visible() {
            self.rect = Rect::ZERO;
            return;
        }
        self.rect = card_rect(host_rect, &self.state);
        let rect = self.rect;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        // Panel role (HIG §9.1/§9.5): the sheet's corner is the Panel radius
        // and its edge is derived against the ground, not declared.
        let card = RoundedRect::from_rect(
            rect,
            ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );

        let pad = 20.0;
        let mut y = rect.y0 + pad;
        let title_size = tokens.typography.scale.base.max(14.0);
        let body_size = tokens.typography.scale.xs.max(10.0);
        let m = SheetMetrics {
            rect,
            pad,
            avail: rect.width() - pad * 2.0,
            body: body_size,
            name: tokens.typography.scale.sm.max(12.0),
        };
        let title = t(if self.state.invite_review_open {
            "peer-sync-title-review"
        } else if self.state.invite_name.is_some() {
            "peer-sync-title-invite"
        } else if self.state.connect_input.is_some() {
            "peer-sync-title-join"
        } else {
            "peer-sync-title-workspace-invite"
        });
        text_renderer.draw_text(
            scene,
            &title,
            title_size,
            colors.text_primary,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(title_size) + 14.0;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        y = self.draw_body(&mut ctx, &m, y);
        if self.state.invite_name.is_some() && !self.state.pending_invitations.is_empty() {
            y = self.draw_pending_invitations(&mut ctx, &m, y);
        }
        if let Some(status) = self.state.status.as_deref() {
            let status = ctx
                .text
                .elide_to_width(status, body_size, f64_to_f32(m.avail));
            ctx.text.draw_text(
                ctx.scene,
                &status,
                body_size,
                colors.text_primary,
                transform.pre_translate((rect.x0 + pad, y).into()),
                None,
            );
        }
        self.draw_buttons(&mut ctx, &m);
    }

    fn draw_pending_invitations(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        m: &SheetMetrics,
        mut y: f64,
    ) -> f64 {
        let (rect, pad, body) = (m.rect, m.pad, m.body);
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        let total = self.state.pending_invitations.len();
        let end = self.pending_offset.saturating_add(3).min(total);
        let heading = if total > 3 {
            t_args(
                "peer-sync-pending-heading-paged",
                &[
                    ("first", self.pending_offset.saturating_add(1).into()),
                    ("last", end.into()),
                    ("total", total.into()),
                ],
            )
        } else {
            t("peer-sync-pending-heading")
        };
        text.draw_text(
            scene,
            &heading,
            body,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(body) + 8.0;
        for invite in self
            .state
            .pending_invitations
            .iter()
            .skip(self.pending_offset)
            .take(3)
        {
            let row = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 42.0);
            // Control role (HIG §9.1/§9.5): a row card at the Control radius
            // (`radii.sm` is for details inside a control) with an edge
            // derived from its lift floor, so it registers in light as well.
            ui::surface::card(
                scene,
                transform,
                row,
                ui::chrome::ChromeRole::Control.radius(theme, row.height()),
                colors.lift(0.35),
            );
            let detail = role_and_expiry(
                "peer-sync-role-expires",
                invite.role,
                invite.expiry_unix_seconds,
            );
            let shown_name = text.elide_to_width(&invite.name, body, 150.0);
            let shown_detail = text.elide_to_width(&detail, body, 190.0);
            text.draw_text(
                scene,
                &shown_name,
                body,
                colors.text_primary,
                transform.pre_translate((row.x0 + 10.0, row.y0 + 6.0).into()),
                None,
            );
            text.draw_text(
                scene,
                &shown_detail,
                body,
                colors.text_dim,
                transform.pre_translate((row.x0 + 10.0, row.y0 + 23.0).into()),
                None,
            );
            let cancel = Rect::new(row.x1 - 62.0, row.y0 + 8.0, row.x1 - 8.0, row.y1 - 8.0);
            let resend = Rect::new(cancel.x0 - 68.0, cancel.y0, cancel.x0 - 6.0, cancel.y1);
            for (label, button, button_rect) in [
                (t("peer-sync-copy"), Button::Resend(invite.id), resend),
                (
                    t("peer-sync-cancel"),
                    Button::CancelPending(invite.id),
                    cancel,
                ),
            ] {
                Self::draw_row_button(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    button_rect,
                    label.as_str(),
                    body,
                );
                self.button_rects.push((button, button_rect));
            }
            y = row.y1 + 6.0;
        }
        y
    }

    /// A pill button inside a pending-invitation row: its lift and its
    /// centred label.
    fn draw_row_button(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str, size: f32) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        ui::surface::card(
            scene,
            transform,
            rect,
            rect.height() / 2.0,
            colors.lift(0.35),
        );
        let (width, height) = text.measure(label, size, None);
        text.draw_text(
            scene,
            label,
            size,
            colors.text_primary,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(width) / 2.0,
                    rect.center().y - f64::from(height) / 2.0,
                )
                    .into(),
            ),
            None,
        );
    }

    fn draw_body(&mut self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, y: f64) -> f64 {
        let invite_name = self.state.invite_name.clone();
        if let Some(typed) = invite_name.as_deref() {
            if self.state.invite_review_open {
                self.draw_invite_review(ctx, m, y)
            } else {
                let y = draw_naming(
                    ctx,
                    m,
                    y,
                    typed,
                    self.state.invite_recipient_path,
                    self.state.invite_recipient_key.as_ref(),
                );
                let y = self.draw_recipient_choices(ctx, m, y);
                self.draw_role_choices(ctx, m, y)
            }
        } else if let Some(token) = self.state.invite_token.as_deref() {
            draw_invite(ctx, m, y, token, self.state.invite_for.as_deref())
        } else {
            draw_receiving(ctx, m, y, self.state.connect_input.as_deref().unwrap_or(""))
        }
    }

    fn draw_recipient_choices(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        m: &SheetMetrics,
        mut y: f64,
    ) -> f64 {
        let SheetMetrics {
            rect, pad, body, ..
        } = *m;
        let colors = &ctx.theme.tokens.colors;
        ctx.text.draw_text(
            ctx.scene,
            &t("peer-sync-recipient-heading"),
            body,
            colors.text_dim,
            ctx.transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(body) + 7.0;
        // Snapshot the short rendered list before registering hit targets;
        // drawing a target mutates `self.button_rects`, so borrowing the
        // source collection through that call would be invalid.
        let choices: Vec<RememberedInvitee> =
            self.remembered_invitees.iter().take(2).cloned().collect();
        for (index, person) in choices.into_iter().enumerate() {
            let choice = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 30.0);
            let selected = self.state.invite_recipient_path == InviteRecipientPath::Known
                && self.state.invite_recipient_key == Some(person.user_public_key);
            self.draw_choice(
                ctx,
                choice,
                &format!("{} · {}", person.name, person.verification),
                selected,
                Button::SelectKnown(index),
                body,
            );
            y = choice.y1 + 5.0;
        }
        let choice = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 30.0);
        self.draw_choice(
            ctx,
            choice,
            &t("peer-sync-recipient-new"),
            self.state.invite_recipient_path == InviteRecipientPath::New,
            Button::SelectNewRecipient,
            body,
        );
        choice.y1 + 12.0
    }

    fn draw_role_choices(&mut self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, mut y: f64) -> f64 {
        let SheetMetrics {
            rect, pad, body, ..
        } = *m;
        let colors = &ctx.theme.tokens.colors;
        ctx.text.draw_text(
            ctx.scene,
            &t("peer-sync-role-heading"),
            body,
            colors.text_dim,
            ctx.transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(body) + 7.0;
        let options = [
            (crate::network::invite::InviteRole::Viewer, 0.0),
            (crate::network::invite::InviteRole::Commenter, 1.0),
            (crate::network::invite::InviteRole::Editor, 2.0),
        ];
        let gap = 6.0;
        let width = (rect.width() - pad * 2.0 - gap * 2.0) / 3.0;
        for (role, column) in options {
            let label = role_label(role);
            let x0 = rect.x0 + pad + (width + gap) * column;
            let choice = Rect::new(x0, y, x0 + width, y + 32.0);
            self.draw_choice(
                ctx,
                choice,
                &label,
                self.state.invite_role == role,
                Button::SelectRole(role),
                body,
            );
        }
        y + 32.0
    }

    fn draw_invite_review(&self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, mut y: f64) -> f64 {
        let SheetMetrics {
            rect,
            pad,
            body,
            avail,
            ..
        } = *m;
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        let key = self.state.invite_recipient_key.as_ref();
        let workspace = self.state.invite_context.workspace_name.as_str();
        let inviter = self.state.invite_context.inviter_name.as_str();
        let lines = [
            if workspace.trim().is_empty() {
                t("peer-sync-review-workspace-this")
            } else {
                t_args(
                    "peer-sync-review-workspace",
                    &[("workspace", workspace.into())],
                )
            },
            if inviter.trim().is_empty() {
                t("peer-sync-review-inviter-you")
            } else {
                t_args("peer-sync-review-inviter", &[("inviter", inviter.into())])
            },
            self.state.invite_name.as_deref().map_or_else(
                || t("peer-sync-review-recipient-unnamed"),
                |name| t_args("peer-sync-review-recipient", &[("name", name.into())]),
            ),
            t_args(
                "peer-sync-review-role",
                &[("role", role_label(self.state.invite_role).into())],
            ),
            key.map_or_else(
                || t("peer-sync-review-identity-missing"),
                |key| {
                    t_args(
                        "peer-sync-review-identity",
                        &[("fingerprint", identity_fingerprint(key).into())],
                    )
                },
            ),
        ];
        for (index, line) in lines.iter().enumerate() {
            let color = if index == 4 {
                colors.accent[5]
            } else {
                colors.text_primary
            };
            let shown = text.elide_to_width(line, body, f64_to_f32(avail));
            text.draw_text(
                scene,
                &shown,
                body,
                color,
                transform.pre_translate((rect.x0 + pad, y).into()),
                None,
            );
            y += f64::from(body) + 9.0;
        }
        text.draw_text(
            scene,
            &t("peer-sync-review-note"),
            body,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y + 5.0).into()),
            None,
        );
        y + f64::from(body) + 10.0
    }

    fn draw_choice(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        label: &str,
        selected: bool,
        button: Button,
        body: f32,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        // Selector role (HIG §9.3): the chosen recipient or role is a lift
        // with the tier's hairline, never an accent fill (the accent means
        // live state, §9.4); idle choices rest on the lift floor.
        let tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(selected));
        let fill = if selected {
            tier.fill
        } else {
            colors.lift(0.35)
        };
        let radius = rect.height() / 2.0;
        let shape = RoundedRect::from_rect(rect, radius);
        scene.fill(Fill::NonZero, transform, fill, None, &shape);
        if let Some(border) = tier.border {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                border,
                None,
                &ui::surface::edge_outline(rect, radius),
            );
        }
        let shown = text.elide_to_width(label, body, f64_to_f32(rect.width() - 16.0));
        let (width, height) = text.measure(&shown, body, None);
        text.draw_text(
            scene,
            &shown,
            body,
            colors.text_primary,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(width) / 2.0,
                    rect.center().y - f64::from(height) / 2.0,
                )
                    .into(),
            ),
            None,
        );
        self.button_rects.push((button, rect));
    }

    fn draw_buttons(&mut self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics) {
        let SheetMetrics {
            rect, pad, body, ..
        } = *m;
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let buttons: Vec<(String, Button)> = if self.state.invite_review_open {
            vec![
                (t("peer-sync-create-invite"), Button::CreateInvite),
                (t("peer-sync-back"), Button::BackToComposer),
                (t("peer-sync-cancel"), Button::Cancel),
            ]
        } else if self.state.invite_name.is_some() {
            vec![
                (t("peer-sync-review-invite"), Button::ReviewInvite),
                (t("peer-sync-paste-identity"), Button::PasteIdentity),
                (t("peer-sync-cancel"), Button::Cancel),
            ]
        } else if self.state.connect_input.is_some() {
            vec![
                (t("peer-sync-join"), Button::Join),
                (t("peer-sync-cancel"), Button::Cancel),
                (t("peer-sync-paste"), Button::PasteToken),
                (t("peer-sync-copy-identity"), Button::CopyIdentity),
            ]
        } else {
            vec![(t("peer-sync-copy"), Button::CopyToken)]
        };
        // Shared `Button` faces (HIG §9.9) at the minimum pointer target
        // (§9.6) with the Control radius (§9.1) — the former 30 px pills
        // were under the target and hand-painted an accent wash.
        let bh = f64::from(theme.tokens.metrics.min_pointer_target);
        let pad_x = f64::from(theme.tokens.spacing.s3);
        let gap = f64::from(theme.tokens.spacing.s2);
        let by = rect.y1 - pad - bh;
        let mut bx = rect.x1 - pad;
        for (label, button) in buttons {
            let (tw, _) = text.measure(&label, body, None);
            let bw = f64::from(tw) + 2.0 * pad_x;
            let brect = Rect::new(bx - bw, by, bx, by + bh);
            let primary = matches!(
                button,
                Button::Join | Button::CopyToken | Button::CreateInvite
            );
            // The pass paints with the renderer reset to 1× (see `draw`), so
            // the face is handed the theme-scaled label size explicitly.
            let mut face = ui::widget::ButtonFace::new(brect, &label)
                .radius(ui::chrome::ChromeRole::Control.radius(theme, bh))
                .text_size(body);
            if primary {
                face = face.primary();
            }
            ui::widget::Button::paint(scene, transform, text, theme, &face);
            self.button_rects.push((button, brect));
            bx = brect.x0 - gap;
        }
    }

    pub fn button_rects_for_tests(&self) -> Vec<(&'static str, Rect)> {
        self.button_rects
            .iter()
            .map(|(button, rect)| {
                (
                    match button {
                        Button::CreateInvite => "create",
                        Button::ReviewInvite => "review",
                        Button::BackToComposer => "back",
                        Button::SelectRole(_) => "role",
                        Button::SelectKnown(_) => "known",
                        Button::SelectNewRecipient => "new-recipient",
                        Button::Join => "join",
                        Button::Cancel => "cancel",
                        Button::CopyToken => "copy",
                        Button::PasteToken => "paste",
                        Button::Resend(_) => "resend",
                        Button::CancelPending(_) => "cancel-pending",
                        Button::PasteIdentity => "paste-identity",
                        Button::CopyIdentity => "copy-identity",
                    },
                    *rect,
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub fn set_snapshot_for_tests(&mut self, snapshot: PeerSyncSnapshot) {
        self.state = snapshot;
    }
}

/// The sheet's card, its inner padding and content width, and the body and
/// name type sizes every section of one paint pass lays out against.
#[derive(Clone, Copy)]
struct SheetMetrics {
    rect: Rect,
    pad: f64,
    avail: f64,
    body: f32,
    name: f32,
}

fn card_rect(host: Rect, state: &PeerSyncSnapshot) -> Rect {
    // A sheet with a form in it takes the sheet width (HIG §9.6), shrinking
    // inside the panel padding on a narrow host rather than a bespoke
    // 400–520 band. Unscaled `BASE` like the rest of this sheet's geometry.
    let w = f64::from(ui::tokens::Metrics::BASE.panel_width_sheet)
        .min(host.width() - 2.0 * f64::from(ui::tokens::Spacing::BASE.s4))
        .max(0.0);
    let h = if state.invite_review_open {
        330.0
    } else if state.invite_name.is_some() {
        if state.pending_invitations.is_empty() {
            430.0
        } else {
            610.0
        }
    } else if state.connect_input.is_some() {
        220.0
    } else {
        200.0
    };
    let cx = f64::midpoint(host.x0, host.x1);
    let cy = f64::midpoint(host.y0, host.y1);
    Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
}

fn parse_invite(token: &str) -> Option<crate::network::invite::PeerInviteToken> {
    crate::network::invite::validate_invite_token(token.trim()).ok()
}

fn peer_fingerprint(peer_id: &str) -> String {
    let chars: Vec<char> = peer_id.chars().collect();
    let tail = chars.get(chars.len().saturating_sub(8)..).unwrap_or(&[]);
    if tail.len() <= 4 {
        return tail.iter().collect();
    }
    let (a, b) = tail.split_at(4);
    format!(
        "{} {}",
        a.iter().collect::<String>(),
        b.iter().collect::<String>()
    )
}

fn parse_recipient_identity(value: &str) -> Option<[u8; 32]> {
    let compact = value.trim();
    let encoded = compact
        .strip_prefix("tilo-identity:")
        .unwrap_or(compact)
        .trim();
    let bytes = hex::decode(encoded).ok()?;
    bytes.try_into().ok()
}

fn identity_fingerprint(key: &[u8; 32]) -> String {
    let encoded = hex::encode(key);
    peer_fingerprint(&encoded)
}

/// How long until `expiry`, as the unit and count the expiry messages
/// select on.
fn time_until(expiry: u64) -> (&'static str, u64) {
    let now = tilo_clock::unix_now();
    match expiry.saturating_sub(now) {
        0 => ("now", 0),
        s if s < 120 => ("seconds", s),
        s if s < 5400 => ("minutes", s / 60),
        s if s < 172_800 => ("hours", s / 3600),
        s => ("days", s / 86_400),
    }
}

/// An invite role as a label of its own (a choice, or the value after a
/// "·" separator).
fn role_label(role: crate::network::invite::InviteRole) -> String {
    use crate::network::invite::InviteRole;
    t(match role {
        InviteRole::Viewer => "peer-sync-role-viewer",
        InviteRole::Commenter => "peer-sync-role-commenter",
        InviteRole::Editor => "peer-sync-role-editor",
    })
}

/// One whole "role · expires in …" message, so each language words the
/// expiry for its own unit and plural.
fn role_and_expiry(key: &str, role: crate::network::invite::InviteRole, expiry: u64) -> String {
    let (unit, count) = time_until(expiry);
    t_args(
        key,
        &[
            ("role", role_label(role).into()),
            ("unit", unit.into()),
            ("count", count.into()),
        ],
    )
}

fn elide_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 20 {
        return token.to_string();
    }
    format!(
        "{}…{}",
        chars.get(..10).unwrap_or(&[]).iter().collect::<String>(),
        chars
            .get(chars.len().saturating_sub(6)..)
            .unwrap_or(&[])
            .iter()
            .collect::<String>()
    )
}

fn draw_naming(
    ctx: &mut PaintCtx<'_>,
    m: &SheetMetrics,
    mut y: f64,
    typed: &str,
    recipient_path: InviteRecipientPath,
    recipient: Option<&[u8; 32]>,
) -> f64 {
    let SheetMetrics {
        rect,
        pad,
        avail,
        body,
        name,
    } = *m;
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let colors = &theme.tokens.colors;
    text.draw_text(
        scene,
        &t(if recipient_path == InviteRecipientPath::Known {
            "peer-sync-selected-person"
        } else {
            "peer-sync-name-new-person"
        }),
        body,
        colors.text_dim,
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
    );
    y += f64::from(body) + 8.0;
    let field = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 38.0);
    let rr = RoundedRect::from_rect(field, f64::from(theme.tokens.radii.sm));
    scene.fill(Fill::NonZero, transform, colors.lift(0.35), None, &rr);
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        colors.accent[5],
        None,
        &rr,
    );
    let shown = text.elide_to_width(typed, name, f64_to_f32(avail - 40.0));
    let (tw, th) = text.measure(&shown, name, None);
    let ty = f64::midpoint(field.y0, field.y1) - f64::from(th) / 2.0;
    let caret_w = 1.5;
    // The caret sits on the text origin; an empty field shows its placeholder
    // one `s1` to the right of the caret so the two never overlap (HIG §7).
    let text_x = field.x0
        + 12.0
        + if typed.is_empty() {
            caret_w + f64::from(theme.tokens.spacing.s1)
        } else {
            0.0
        };
    text.draw_text(
        scene,
        &if typed.is_empty() {
            t("peer-sync-name-placeholder")
        } else {
            shown.clone()
        },
        name,
        if typed.is_empty() {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate((text_x, ty).into()),
        None,
    );
    let caret_x = field.x0
        + 12.0
        + if typed.is_empty() {
            0.0
        } else {
            f64::from(tw) + 2.0
        };
    scene.fill(
        Fill::NonZero,
        transform,
        colors.accent[5],
        None,
        &Rect::new(caret_x, field.y0 + 9.0, caret_x + caret_w, field.y1 - 9.0),
    );
    y = field.y1 + 12.0;
    let identity = recipient.map_or_else(
        || t("peer-sync-recipient-hint"),
        |key| {
            let fingerprint: ui::i18n::ArgValue = identity_fingerprint(key).into();
            match recipient_path {
                InviteRecipientPath::Known => t_args(
                    "peer-sync-identity-remembered",
                    &[("fingerprint", fingerprint)],
                ),
                InviteRecipientPath::New => {
                    t_args("peer-sync-identity-new", &[("fingerprint", fingerprint)])
                }
            }
        },
    );
    let note = text.elide_to_width(&identity, body, f64_to_f32(avail));
    text.draw_text(
        scene,
        &note,
        body,
        colors.text_dim,
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
    );
    y + f64::from(body) + 10.0
}

fn draw_invite(
    ctx: &mut PaintCtx<'_>,
    m: &SheetMetrics,
    mut y: f64,
    token: &str,
    invite_for: Option<&str>,
) -> f64 {
    let SheetMetrics {
        rect, pad, avail, ..
    } = *m;
    let parsed = parse_invite(token);
    let anyone = t("peer-sync-invite-anyone");
    let who = invite_for.unwrap_or(&anyone);
    let sub = parsed.as_ref().map_or_else(String::new, |invite| {
        role_and_expiry(
            "peer-sync-role-expires",
            invite.role,
            invite.expiry_unix_seconds,
        )
    });
    y = draw_person(
        ctx,
        Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 44.0),
        who,
        &sub,
        invite_for.is_some(),
    ) + 12.0;
    if let Some(invite) = parsed {
        y = draw_code(
            ctx,
            Point::new(rect.x0 + pad, y),
            &peer_fingerprint(&invite.owner_peer_id),
            avail,
        );
    }
    y
}

fn draw_receiving(ctx: &mut PaintCtx<'_>, m: &SheetMetrics, mut y: f64, typed: &str) -> f64 {
    let SheetMetrics {
        rect,
        pad,
        avail,
        body,
        name,
    } = *m;
    if let Some(invite) = parse_invite(typed) {
        y = draw_person(
            ctx,
            Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 44.0),
            &t("peer-sync-invite-received"),
            &role_and_expiry(
                "peer-sync-received-role-expires",
                invite.role,
                invite.expiry_unix_seconds,
            ),
            false,
        ) + 12.0;
        return draw_code(
            ctx,
            Point::new(rect.x0 + pad, y),
            &peer_fingerprint(&invite.owner_peer_id),
            avail,
        );
    }
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let colors = &theme.tokens.colors;
    let line = if typed.is_empty() {
        t("peer-sync-paste-invite-hint")
    } else {
        t_args(
            "peer-sync-token-characters",
            &[
                ("token", elide_token(typed).into()),
                ("count", typed.chars().count().into()),
            ],
        )
    };
    let line = text.elide_to_width(&line, name, f64_to_f32(avail));
    text.draw_text(
        scene,
        &line,
        name,
        if typed.is_empty() {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
    );
    y += f64::from(name) + 8.0;
    if !typed.is_empty() {
        text.draw_text(
            scene,
            &t("peer-sync-token-incomplete"),
            body,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(body) + 8.0;
    }
    y
}

fn draw_person(ctx: &mut PaintCtx<'_>, rect: Rect, who: &str, sub: &str, named: bool) -> f64 {
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let colors = &theme.tokens.colors;
    let center = Point::new(rect.x0 + 16.0, rect.y0 + 18.0);
    if named {
        let initial = who
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        ui::widget::Avatar::draw_at(
            scene,
            transform,
            Rect::new(
                center.x - 16.0,
                center.y - 16.0,
                center.x + 16.0,
                center.y + 16.0,
            ),
            &initial,
            colors.accent[5],
            theme,
            text,
        );
    } else {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
            transform,
            ui::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26),
            None,
            &vello::kurbo::Circle::new(center, 16.0),
        );
    }
    let x = center.x + 30.0;
    let ns = theme.tokens.typography.scale.sm.max(12.0);
    let ss = theme.tokens.typography.scale.xs.max(10.0);
    let who = text.elide_to_width(who, ns, f64_to_f32(rect.x1 - x));
    text.draw_text(
        scene,
        &who,
        ns,
        colors.text_primary,
        transform.pre_translate((x, rect.y0 + 2.0).into()),
        None,
    );
    let sub = text.elide_to_width(sub, ss, f64_to_f32(rect.x1 - x));
    text.draw_text(
        scene,
        &sub,
        ss,
        colors.text_dim,
        transform.pre_translate((x, rect.y0 + 2.0 + f64::from(ns) + 6.0).into()),
        None,
    );
    rect.y0 + 2.0 + f64::from(ns) + 6.0 + f64::from(ss)
}

fn draw_code(ctx: &mut PaintCtx<'_>, at: Point, code: &str, avail: f64) -> f64 {
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let colors = &theme.tokens.colors;
    let size = theme.tokens.typography.scale.lg.max(15.0);
    let (_, fh) = text.measure(code, size, None);
    let field = Rect::new(at.x, at.y, at.x + avail, at.y + f64::from(fh) + 20.0);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.lift(0.35),
        None,
        &RoundedRect::from_rect(field, f64::from(theme.tokens.radii.md)),
    );
    text.draw_text(
        scene,
        code,
        size,
        colors.text_primary,
        transform.pre_translate((field.x0 + 14.0, field.y0 + 10.0).into()),
        None,
    );
    field.y1
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "Text elision accepts f32 widths; narrowing f64 overlay geometry may round subpixel precision without changing record or identity data."
)]
fn f64_to_f32(v: f64) -> f32 {
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn named_invites_bind_role_and_verified_identity_before_review() {
        /// HIG §9.5: the invite card's rows and mini pills rest on the lift
        /// floor with an edge derived from it; that edge must register on every
        /// shipped palette, and the card's Panel border on its Panel ground.
        fn invite_rows_and_panel_edge_register_on_every_palette() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let ground = ui::chrome::shell::panel_ground(&theme);
                let floor = theme.tokens.colors.lift(0.35);
                for (what, edge, fill) in [
                    (
                        "panel border",
                        ui::chrome::shell::panel_border(&theme),
                        ground,
                    ),
                    ("row card", ui::surface::edge_on(floor), floor),
                ] {
                    let delta = ui::surface::edge_delta(edge, fill);
                    assert!(
                        delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{}: the {what} must separate from its fill (Δ luminance {delta:.4})",
                        theme.name
                    );
                }
            }
        }

        fn named_invite_refuses_blank_and_preserves_name_after_transport_failure() {
            let mut overlay = PeerSyncOverlay::default();
            overlay.prompt_invite();
            let blank = overlay.handle_event(
                &key(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                None,
            );
            assert!(blank.request.is_none());
            assert_eq!(
                overlay.state.status.as_deref(),
                Some("Say who this invite is for")
            );
            overlay.handle_event(
                &key(Key::Character("Ada".into())),
                Modifiers::default(),
                None,
            );
            let unbound = overlay.handle_event(
                &key(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                None,
            );
            assert!(
                unbound.request.is_none(),
                "ordinary UI must not mint a bearer invite"
            );
            overlay.state.invite_recipient_key = Some([0x11; 32]);
            let review = overlay.handle_event(
                &key(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                None,
            );
            assert!(review.request.is_none(), "review must not mint a token");
            assert!(overlay.state.invite_review_open);
            let submit = overlay.handle_event(
                &key(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                None,
            );
            assert_eq!(
                submit.request,
                Some(PeerSyncRequest::IssueInviteFor {
                    name: "Ada".into(),
                    role: crate::network::invite::InviteRole::Editor,
                    recipient_user_public_key: [0x11; 32],
                })
            );
            overlay.finish_request(
                submit.request.as_ref().unwrap(),
                Err("offline".into()),
                submit.copy_result,
            );
            assert_eq!(overlay.state.invite_name.as_deref(), Some("Ada"));
        }

        fn explicit_role_choice_and_review_reach_the_recipient_bound_request() {
            let mut overlay = PeerSyncOverlay::default();
            overlay.prompt_invite();
            overlay.handle_event(
                &key(Key::Character("Lin".into())),
                Modifiers::default(),
                None,
            );
            overlay.state.invite_recipient_key = Some([0x22; 32]);
            assert!(
                overlay
                    .activate(Button::SelectRole(
                        crate::network::invite::InviteRole::Viewer
                    ))
                    .request
                    .is_none()
            );
            let review = overlay.handle_event(
                &key(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                None,
            );
            assert!(review.request.is_none());
            assert!(overlay.snapshot().invite_review_open);
            let submit = overlay.handle_event(
                &key(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                None,
            );
            assert_eq!(
                submit.request,
                Some(PeerSyncRequest::IssueInviteFor {
                    name: "Lin".into(),
                    role: crate::network::invite::InviteRole::Viewer,
                    recipient_user_public_key: [0x22; 32],
                })
            );
        }

        fn remembered_person_selects_their_verified_identity_before_review() {
            let mut overlay = PeerSyncOverlay::default();
            overlay.set_remembered_invitees(vec![RememberedInvitee {
                name: "Ada Lovelace".into(),
                user_public_key: [0x41; 32],
                verification: "Verified yesterday".into(),
            }]);
            overlay.set_invite_context(InviteContext {
                workspace_name: "Compiler club".into(),
                inviter_name: "Quinn".into(),
            });
            overlay.prompt_invite();
            overlay.activate(Button::SelectKnown(0));
            assert_eq!(
                overlay.snapshot().invite_name.as_deref(),
                Some("Ada Lovelace")
            );
            assert_eq!(
                overlay.snapshot().invite_recipient_path,
                InviteRecipientPath::Known
            );
            let review = overlay.activate(Button::ReviewInvite);
            assert!(review.request.is_none());
            let snapshot = overlay.snapshot();
            assert!(snapshot.invite_review_open);
            assert_eq!(snapshot.invite_context.workspace_name, "Compiler club");
            let mint = overlay.activate(Button::CreateInvite);
            assert_eq!(
                mint.request,
                Some(PeerSyncRequest::IssueInviteFor {
                    name: "Ada Lovelace".into(),
                    role: crate::network::invite::InviteRole::Editor,
                    recipient_user_public_key: [0x41; 32],
                })
            );
        }

        fn closing_the_sheet_keeps_host_supplied_verified_people_for_next_invite() {
            let mut overlay = PeerSyncOverlay::default();
            overlay.set_remembered_invitees(vec![RememberedInvitee {
                name: "Mina".into(),
                user_public_key: [0x52; 32],
                verification: "Verified in person".into(),
            }]);
            overlay.prompt_invite();
            overlay.dismiss();
            overlay.prompt_invite();
            overlay.activate(Button::SelectKnown(0));
            assert_eq!(overlay.snapshot().invite_name.as_deref(), Some("Mina"));
            assert_eq!(overlay.snapshot().invite_recipient_key, Some([0x52; 32]));
        }

        fn pending_invites_are_named_and_emit_exact_resend_and_cancel_requests() {
            let mut overlay = PeerSyncOverlay::default();
            overlay.prompt_invite();
            let row = PendingInvitationSummary {
                id: [7; 32],
                name: "Ada".into(),
                role: crate::network::invite::InviteRole::Commenter,
                expiry_unix_seconds: u64::MAX,
            };
            overlay.finish_typed_request(
                &PeerSyncRequest::ListPendingInvitations,
                Ok(PeerSyncResponse::PendingInvitations(vec![row.clone()])),
                false,
            );
            assert_eq!(overlay.snapshot().pending_invitations, vec![row]);
            assert_eq!(
                overlay.activate(Button::Resend([7; 32])).request,
                Some(PeerSyncRequest::ResendInvitation([7; 32]))
            );
            assert_eq!(
                overlay.activate(Button::CancelPending([7; 32])).request,
                Some(PeerSyncRequest::CancelInvitation([7; 32]))
            );
        }

        invite_rows_and_panel_edge_register_on_every_palette();
        named_invite_refuses_blank_and_preserves_name_after_transport_failure();
        explicit_role_choice_and_review_reach_the_recipient_bound_request();
        remembered_person_selects_their_verified_identity_before_review();
        closing_the_sheet_keeps_host_supplied_verified_people_for_next_invite();
        pending_invites_are_named_and_emit_exact_resend_and_cancel_requests();
    }

    fn key(key: Key) -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.logical_key = key;
        event.text = None;
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    #[test]
    #[serial_test::serial(clipboard)]
    fn connect_and_identity_cards_are_trimmed_exact_and_non_secret() {
        fn connect_is_trimmed_and_only_closes_after_success() {
            let mut overlay = PeerSyncOverlay::default();
            overlay.prompt_connect();
            overlay.handle_event(
                &key(Key::Character(" token ".into())),
                Modifiers::default(),
                None,
            );
            let submit = overlay.handle_event(
                &key(Key::Named(NamedKey::Enter)),
                Modifiers::default(),
                None,
            );
            assert_eq!(
                submit.request,
                Some(PeerSyncRequest::AcceptInvite("token".into()))
            );
            overlay.finish_request(
                submit.request.as_ref().unwrap(),
                Err("bad token".into()),
                false,
            );
            assert!(overlay.state.connect_input.is_some());
            overlay.finish_request(submit.request.as_ref().unwrap(), Ok("room-7".into()), false);
            assert!(overlay.state.connect_input.is_none());
        }

        fn fingerprint_is_stable_grouped_and_safe_for_short_ids() {
            let a = "12D3KooWQr4Zc8gN3xkLpWv7bTqA9sYd2EfGh1JkLmNpQrStUvWx";
            assert_eq!(peer_fingerprint(a), "QrSt UvWx");
            assert_ne!(
                peer_fingerprint(a),
                peer_fingerprint("12D3KooWQr4Zc8gN3xkLpWv7bTqA9sYd2EfGh1JkLmNpQrStUvAb")
            );
            assert_eq!(peer_fingerprint("abc"), "abc");
            assert_eq!(peer_fingerprint(""), "");
        }

        fn recipient_identity_parser_accepts_only_an_exact_person_key() {
            let key = [0x5a; 32];
            assert_eq!(parse_recipient_identity(&hex::encode(key)), Some(key));
            assert_eq!(
                parse_recipient_identity(&format!("tilo-identity:{}", hex::encode(key))),
                Some(key)
            );
            assert_eq!(parse_recipient_identity("5a"), None);
            assert_eq!(parse_recipient_identity(&"g".repeat(64)), None);
        }

        fn exporting_identity_copies_a_non_secret_card_without_closing_join() {
            let mut overlay = PeerSyncOverlay::default();
            overlay.prompt_connect();
            let card = format!("tilo-identity:{}", hex::encode([0x2a; 32]));
            overlay.finish_request(&PeerSyncRequest::ExportIdentity, Ok(card.clone()), false);
            assert_eq!(ui::text_selection::clipboard_paste(), Some(card));
            assert!(overlay.state.connect_input.is_some());
        }

        fn long_tokens_are_elided_without_changing_short_ones() {
            let shown = elide_token(&"e".repeat(900));
            assert!(shown.chars().count() < 20);
            assert!(shown.contains('…'));
            assert_eq!(elide_token("short-one"), "short-one");
        }

        connect_is_trimmed_and_only_closes_after_success();
        fingerprint_is_stable_grouped_and_safe_for_short_ids();
        recipient_identity_parser_accepts_only_an_exact_person_key();
        exporting_identity_copies_a_non_secret_card_without_closing_join();
        long_tokens_are_elided_without_changing_short_ones();
    }
}

#[cfg(test)]
mod native_enrollment_ui_tests {
    use super::*;
    #[test]
    fn native_pending_join_is_polled_and_never_reported_joined_early() {
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_connect();
        overlay.finish_typed_request(
            &PeerSyncRequest::AcceptInvite("signed".into()),
            Ok(PeerSyncResponse::EnrollmentPending { id: 7 }),
            false,
        );
        assert_eq!(overlay.enrollment, Some(7));
        assert!(!overlay.snapshot().status.unwrap().contains("Joined"));
        assert_eq!(
            overlay.pending_enrollment_request(),
            Some(PeerSyncRequest::PollEnrollment(7))
        );
        assert_eq!(overlay.pending_enrollment_request(), None);
        overlay.finish_typed_request(
            &PeerSyncRequest::PollEnrollment(7),
            Ok(PeerSyncResponse::EnrollmentJoined {
                id: 7,
                room: "actual".into(),
            }),
            false,
        );
        assert_eq!(
            overlay.snapshot().status.as_deref(),
            Some("Joined room actual")
        );
        assert_eq!(overlay.enrollment, None);
    }
    #[test]
    fn native_overlay_close_cancels_once_and_ignores_late_join() {
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_connect();
        overlay.finish_typed_request(
            &PeerSyncRequest::AcceptInvite("signed".into()),
            Ok(PeerSyncResponse::EnrollmentPending { id: 7 }),
            false,
        );
        overlay.dismiss();
        assert_eq!(
            overlay.pending_enrollment_request(),
            Some(PeerSyncRequest::CancelEnrollment(7))
        );
        assert_eq!(overlay.pending_enrollment_request(), None);
        overlay.finish_typed_request(
            &PeerSyncRequest::PollEnrollment(7),
            Ok(PeerSyncResponse::EnrollmentJoined {
                id: 7,
                room: "late".into(),
            }),
            false,
        );
        assert!(overlay.snapshot().status.is_none());
    }
}
