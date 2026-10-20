//! Peer invite/connect overlay.
//!
//! This component owns the complete local interaction: prompt state, editing,
//! clipboard actions, painted geometry and input routing. The shell host only
//! performs the network request emitted by [`PeerSyncOverlay::handle_event`]
//! and returns its result through [`PeerSyncOverlay::finish_request`].

use ui::num::f32_of_f64;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;

use crate::shell::i18n::{t, t_args};
use ui::TextRole;
use ui::theme::Theme;
use ui::widget::ButtonRole;

mod accessibility;
pub use accessibility::is_sheet_target as is_accessibility_target;

/// How long a copied invite token stays on the clipboard: time to paste it
/// into a message to the person it is for.
const INVITE_ON_CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(300);

/// A transport request emitted by the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncRequest {
    /// The invite composer opened: an invite is likely, so the network
    /// starts this device's relays now and the invite can name one.
    PrepareInvitation,
    ExportIdentity,
    IssueInviteFor {
        name: String,
        role: crate::network::invite::InviteRole,
        recipient_user_public_key: [u8; 32],
    },
    AcceptInvite(String),
    /// Leave a site's workspace: it goes from this computer, and is not
    /// joined again unasked.
    LeaveSite(crate::shell::state::RoomId),
    /// Make a pairing link for a new device of the person.
    InviteDevice,
    /// The new devices asking under this computer's open pairing links.
    DevicesAsking,
    /// Approve or decline a new device asking to be the person.
    AnswerDevice {
        digest: [u8; 32],
        approve: bool,
    },
    PollEnrollment(u64),
    CancelEnrollment(u64),
    ListPendingInvitations,
    ResendInvitation([u8; 32]),
    CancelInvitation([u8; 32]),
    /// Mint an invitation on the terms an `invitation.issue` action stated.
    IssueInvitation(InvitationTerms),
    /// The public workspaces the sites this computer knows list, and those
    /// of the site the person typed, when they typed one.
    Directory {
        site: Option<String>,
    },
    /// Join a public workspace the directory listed, with the role its
    /// owner set.
    JoinListed(Box<crate::shell::network::DirectoryListing>),
    /// Report a public workspace this computer joined from a directory to
    /// the people who run it.
    ReportWorkspace {
        room: crate::shell::state::RoomId,
        reason: String,
    },
    /// Whether the workspace in `room` is public, and with what.
    Publication(crate::shell::state::RoomId),
    /// Make the workspace in `room` public with `listing`, or private again
    /// with `None`.
    Publish {
        room: crate::shell::state::RoomId,
        listing: Option<tilo_relay_protocol::directory::Listing>,
    },
    /// Ask the workspace in `room` for a bot, or take the request back.
    AskForBot {
        room: crate::shell::state::RoomId,
        ask: crate::shell::network::BotAsk,
    },
}

/// What an `invitation.issue` action asks to be minted for the active
/// workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationTerms {
    pub role: crate::network::invite::InviteRole,
    pub expires_in_seconds: u64,
    /// The authority-side name recorded for the invitee once they join.
    pub invitee_name: Option<String>,
    /// When set, only a device certified for this person identity can
    /// redeem the invitation; possessing the token alone is not enough.
    pub recipient_user_public_key: Option<[u8; 32]>,
}

/// The JSON output an invitation action returns for `response`. The token
/// is returned to the authorized caller and is never logged.
///
/// # Errors
///
/// When `request` is not an invitation action's request, or the bridge
/// answered with a response that request cannot produce.
pub fn network_action_output(
    request: &PeerSyncRequest,
    response: PeerSyncResponse,
) -> Result<serde_json::Value, String> {
    match (request, response) {
        (
            PeerSyncRequest::IssueInvitation(_) | PeerSyncRequest::ResendInvitation(_),
            PeerSyncResponse::Message(token),
        ) => Ok(serde_json::json!({ "invite": token })),
        (PeerSyncRequest::CancelInvitation(_), PeerSyncResponse::Message(_)) => {
            Ok(serde_json::json!({ "canceled": true }))
        }
        (PeerSyncRequest::ListPendingInvitations, PeerSyncResponse::PendingInvitations(rows)) => {
            let invitations: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|invite| {
                    serde_json::json!({
                        "invite_id": hex::encode(invite.id),
                        "name": invite.name,
                        "role": invite.role,
                        "expires_at": invite.expiry_unix_seconds,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "invitations": invitations }))
        }
        _ => Err("peer-sync answered an unexpected response".into()),
    }
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
    EnrollmentPending {
        id: u64,
    },
    EnrollmentJoined {
        id: u64,
        room: String,
    },
    Message(String),
    PendingInvitations(Vec<PendingInvitationSummary>),
    /// A pairing link for a new device, and when it stops admitting one
    /// (Unix-epoch milliseconds).
    PairingInvite {
        link: String,
        expires_at_ms: u64,
    },
    /// The new devices asking to be the person.
    DevicesAsking(Vec<DeviceAskingSummary>),
    /// The public workspaces, in the order the directory sheet shows them.
    Directory(Vec<crate::shell::network::DirectoryListing>),
    /// A public workspace was joined: its room, and the workspace slot it
    /// fills on this desktop when one was made for it.
    JoinedWorkspace {
        room: crate::shell::state::RoomId,
        workspace: Option<crate::shell::state::WorkspaceId>,
    },
    /// Whether a workspace is public, and with what.
    Publication(crate::shell::network::Publication),
}

/// A new device asking to be the person, as the approval card shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAskingSummary {
    /// The request, as the answer names it.
    pub digest: [u8; 32],
    /// What it calls itself (such as "Safari on iPhone").
    pub label: String,
    /// The code it shows.
    pub code: String,
    /// When the host received it, Unix-epoch milliseconds.
    pub requested_at_ms: u64,
    /// Where it came from, when the host can say (ISO 3166-1 alpha-2).
    pub country: Option<String>,
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
    /// "Pair a new device" is open.
    pub pairing_open: bool,
    /// Its link, once made: what the new device opens.
    pub pairing_link: Option<String>,
    /// The link went to the clipboard.
    pub pairing_copied: bool,
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
    CopyPairLink,
}

/// What one painted target says, for assistive technology.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DrawnLabel {
    label: String,
    /// Whose row the target acts on, when the label alone is ambiguous.
    description: Option<String>,
    /// For a choice, whether it is the chosen one.
    selected: Option<bool>,
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
    /// What each painted target says, in `button_rects` order: the label,
    /// whose row it acts on, and for a choice whether it is chosen.
    button_labels: Vec<DrawnLabel>,
    pending_offset: usize,
    enrollment: Option<u64>,
    enrollment_poll_at: Option<std::time::Instant>,
    retired_enrollments: std::collections::VecDeque<u64>,
    /// Turns beside "Waiting for your new device…" while a pairing link is
    /// out.
    waiting: ui::widget::Spinner,
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
            || self.state.pairing_open
    }

    /// Open "Pair a new device": the link is being made.
    pub fn prompt_pair(&mut self) {
        self.retire_enrollment();
        self.state.invite_name = None;
        self.state.connect_input = None;
        self.state.invite_token = None;
        self.state.invite_review_open = false;
        self.state.pairing_open = true;
        self.state.pairing_link = None;
        self.state.pairing_copied = false;
        self.state.status = Some(t("pairing-making"));
    }

    /// Show the pairing link the network made, while the sheet is open.
    pub fn show_pairing(&mut self, link: String) {
        if !self.state.pairing_open {
            return;
        }
        self.state.pairing_link = Some(link);
        self.state.pairing_copied = false;
        self.state.status = Some(t("pairing-waiting"));
    }

    /// Whether the sheet animates: its waiting indicator turns while a
    /// pairing link is out.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        self.state.pairing_open
            && self.state.pairing_link.is_some()
            && ui::widget::Widget::needs_animation_frame(&self.waiting)
    }

    /// Say why no pairing link could be made.
    pub fn pairing_failed(&mut self, error: &str) {
        if self.state.pairing_open {
            self.state.status = Some(error.to_owned());
        }
    }

    /// Say that the device the link invited became one of the person's,
    /// and close the sheet's waiting.
    pub fn pairing_done(&mut self, label: &str) {
        if self.state.pairing_open {
            self.state.pairing_link = None;
            self.state.status = Some(t_args(
                "pairing-done",
                &[("device", label.to_owned().into())],
            ));
        }
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

    /// Open the join field holding `text`, a link or a code the person
    /// typed elsewhere (the directory's search); empty, as
    /// [`Self::prompt_connect`].
    pub fn prompt_connect_with(&mut self, text: &str) {
        self.prompt_connect();
        let text: String = text.split_whitespace().collect();
        if !text.is_empty() {
            self.state.connect_input = Some(text);
            self.state.status = None;
        }
    }

    /// Say the card's `request` is being carried out: minting, redeeming,
    /// resending or cancelling waits on the network off the frame loop, and
    /// [`Self::finish_typed_request`] replaces this once it is back.
    pub fn begin_request(&mut self, request: &PeerSyncRequest) {
        if matches!(
            request,
            PeerSyncRequest::IssueInviteFor { .. }
                | PeerSyncRequest::AcceptInvite(_)
                | PeerSyncRequest::ResendInvitation(_)
                | PeerSyncRequest::CancelInvitation(_)
        ) {
            self.state.status = Some(t("peer-sync-working"));
        }
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

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per request and response pair keeps every outcome of the card in one table"
    )]
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
                self.state.status = Some(t("peer-sync-joining"));
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
            // An invitation action answers its caller, not this card; leaving
            // a workspace and pairing a device answer the surfaces that asked.
            (
                PeerSyncRequest::PollEnrollment(_)
                | PeerSyncRequest::CancelEnrollment(_)
                | PeerSyncRequest::IssueInvitation(_)
                | PeerSyncRequest::PrepareInvitation
                | PeerSyncRequest::LeaveSite(_)
                | PeerSyncRequest::InviteDevice
                | PeerSyncRequest::DevicesAsking
                | PeerSyncRequest::AnswerDevice { .. }
                | PeerSyncRequest::Directory { .. }
                | PeerSyncRequest::JoinListed(_)
                | PeerSyncRequest::ReportWorkspace { .. }
                | PeerSyncRequest::Publication(_)
                | PeerSyncRequest::Publish { .. }
                | PeerSyncRequest::AskForBot { .. },
                _,
            ) => {}

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
            (PeerSyncRequest::IssueInviteFor { .. }, Err(error)) => {
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
                self.active_input_mut(connect_open)
                    .map(ui::delete_last_grapheme);
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
                self.state.pairing_open = false;
                self.state.pairing_link = None;
                self.state.pairing_copied = false;
                self.state.status = None;
                EventOutcome::consumed()
            }
            Button::CopyPairLink => {
                if let Some(link) = self.state.pairing_link.as_deref() {
                    // The link makes whoever opens it a device of the person
                    // once approved: it stays out of clipboard history and
                    // sync, and leaves the clipboard after a while.
                    if ui::try_clipboard_copy_secret(link, INVITE_ON_CLIPBOARD_FOR).outcome()
                        == Some(true)
                    {
                        self.state.pairing_copied = true;
                    } else {
                        self.state.status = Some(t("peer-sync-copy-failed"));
                    }
                }
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
        // The token admits whoever pastes it: it stays out of clipboard
        // history and device sync, and leaves the clipboard after a while.
        self.state.status = Some(t(
            if ui::try_clipboard_copy_secret(token, INVITE_ON_CLIPBOARD_FOR).outcome() == Some(true)
            {
                "peer-sync-copied"
            } else {
                "peer-sync-copy-failed"
            },
        ));
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

    /// The sheet's title at `at`, elided to `avail`; returns how far the
    /// body starts below it.
    fn draw_title(&self, ctx: &mut PaintCtx<'_>, at: Point, avail: f64) -> f64 {
        let theme = ctx.theme;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let title = t(if self.state.pairing_open {
            "pairing-title"
        } else if self.state.invite_review_open {
            "peer-sync-title-review"
        } else if self.state.invite_name.is_some() {
            "peer-sync-title-invite"
        } else if self.state.connect_input.is_some() {
            "peer-sync-title-join"
        } else {
            "peer-sync-title-workspace-invite"
        });
        // Pairing is a guided sheet of its own, under a heading.
        let title_role = if self.state.pairing_open {
            TextRole::Heading
        } else {
            TextRole::Body
        };
        let title =
            ctx.text
                .elide_to_width_style(&title, f32_of_f64(avail), &theme.text(title_role));
        ctx.text.draw_style(
            ctx.scene,
            &title,
            colors.text_primary,
            ctx.transform.pre_translate(at.to_vec2()),
            None,
            &theme.text(title_role),
        );
        if self.state.pairing_open {
            f64::from(theme.text(TextRole::Heading).line_height_px()) + f64::from(tokens.spacing.s4)
        } else {
            f64::from(theme.tokens.typography.size(TextRole::Body).max(14.0))
                + f64::from(tokens.spacing.s3)
        }
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
        self.button_labels.clear();
        if !self.visible() {
            self.rect = Rect::ZERO;
            return;
        }
        self.rect = card_rect(host_rect, &self.state, theme);
        let rect = self.rect;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        // Panel role (HIG §9.1/§9.5): the Panel radius, ground and derived
        // edge, painted by the one Panel painter.
        let _ = ui::chrome::shell::paint_panel(scene, transform, rect, theme, 1.0);

        // Panel padding (HIG §9.6).
        let pad = f64::from(tokens.spacing.s4);
        let mut y = rect.y0 + pad;
        let body_size = tokens.typography.size(TextRole::Caption).max(10.0);
        let m = SheetMetrics {
            rect,
            pad,
            avail: rect.width() - pad * 2.0,
            body: body_size,
            name: tokens.typography.size(TextRole::Caption).max(12.0),
        };
        y += self.draw_title(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            Point::new(rect.x0 + pad, y),
            m.avail,
        );
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        // The footer first: its buttons stay anchored to the sheet's bottom
        // edge, wrapping onto more rows on a narrow sheet, and the body
        // above them clips at the footer so a short window never paints the
        // form through its buttons.
        let footer = self.layout_buttons(&mut ctx, &m);
        let footer_top = footer
            .iter()
            .map(|(_, _, rect, _)| rect.y0)
            .fold(rect.y1 - pad, f64::min);
        let body_clip = Rect::new(
            rect.x0,
            y,
            rect.x1,
            (footer_top - f64::from(tokens.spacing.s2)).max(y),
        );
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body_clip,
        );
        y = self.draw_body(&mut ctx, &m, y);
        if self.state.invite_name.is_some() && !self.state.pending_invitations.is_empty() {
            y = self.draw_pending_invitations(&mut ctx, &m, y);
        }
        if let Some(status) = self
            .state
            .status
            .as_deref()
            .filter(|_| !self.state.pairing_open)
        {
            let status = ctx.text.elide_to_width_style(
                status,
                f32_of_f64(m.avail),
                &theme.text(TextRole::Caption),
            );
            ctx.text.draw_style(
                ctx.scene,
                &status,
                colors.text_primary,
                transform.pre_translate((rect.x0 + pad, y).into()),
                None,
                &theme.text(TextRole::Caption),
            );
        }
        ctx.scene.pop_layer();
        // A control the clip hid is not a target either: keep only the body
        // controls wholly inside the visible body (labels are index-aligned).
        let shown = |at: &Rect| at.y0 >= body_clip.y0 - 0.5 && at.y1 <= body_clip.y1 + 0.5;
        let (rects, labels): (Vec<_>, Vec<_>) = std::mem::take(&mut self.button_rects)
            .into_iter()
            .zip(std::mem::take(&mut self.button_labels))
            .filter(|((_, at), _)| shown(at))
            .unzip();
        self.button_rects = rects;
        self.button_labels = labels;
        self.paint_buttons(&mut ctx, footer);
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
        let heading = pending_heading(self.pending_offset, end, total);
        let heading = text.elide_to_width_style(
            &heading,
            f32_of_f64(m.avail),
            &theme.text(TextRole::Caption),
        );
        text.draw_style(
            scene,
            &heading,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
            &theme.text(TextRole::Caption),
        );
        let spacing = &theme.tokens.spacing;
        let (s1, s2) = (f64::from(spacing.s1), f64::from(spacing.s2));
        y += f64::from(body) + s2;
        for invite in self
            .state
            .pending_invitations
            .iter()
            .skip(self.pending_offset)
            .take(3)
        {
            // Two caption lines inside `s1` above, between and below, never
            // under the pointer target its buttons need.
            let row_h = (2.0 * f64::from(body) + 3.0 * s1)
                .max(f64::from(theme.tokens.metrics.min_pointer_target) + 2.0 * s1);
            let row = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + row_h);
            // Control role (HIG §9.1/§9.5): a row card at the Control radius
            // (`radii.detail` is for details inside a control) with an edge
            // derived from its lift floor, so it registers in light as well.
            ui::surface::card(
                scene,
                transform,
                row,
                ui::chrome::ChromeRole::Control.radius(theme, row.height()),
                rest_floor(theme),
            );
            let detail = role_and_expiry(
                "peer-sync-role-expires",
                invite.role,
                invite.expiry_unix_seconds,
            );
            // The row's buttons are as wide as their labels (`s3` either
            // side), `s1` apart, inset `s1` from the row; the name and the
            // detail take the room left of them.
            let copy_label = t("peer-sync-copy");
            let cancel_label = t("peer-sync-cancel");
            let label_w = |text: &mut TextRenderer, label: &str| {
                f64::from(
                    text.measure_style(label, None, &theme.text(TextRole::Caption))
                        .0,
                ) + 2.0 * f64::from(spacing.s3)
            };
            let cancel_w = label_w(text, &cancel_label);
            let copy_w = label_w(text, &copy_label);
            let target = f64::from(theme.tokens.metrics.min_pointer_target);
            let by0 = row.center().y - target / 2.0;
            let cancel = Rect::new(row.x1 - s1 - cancel_w, by0, row.x1 - s1, by0 + target);
            let resend = Rect::new(cancel.x0 - s1 - copy_w, by0, cancel.x0 - s1, by0 + target);
            let text_w = f32_of_f64((resend.x0 - row.x0 - s2 - s2).max(0.0));
            let shown_name =
                text.elide_to_width_style(&invite.name, text_w, &theme.text(TextRole::Caption));
            let shown_detail =
                text.elide_to_width_style(&detail, text_w, &theme.text(TextRole::Caption));
            text.draw_style(
                scene,
                &shown_name,
                colors.text_primary,
                transform.pre_translate((row.x0 + s2, row.y0 + s1).into()),
                None,
                &theme.text(TextRole::Caption),
            );
            text.draw_style(
                scene,
                &shown_detail,
                colors.text_dim,
                transform.pre_translate((row.x0 + s2, row.y1 - s1 - f64::from(body)).into()),
                None,
                &theme.text(TextRole::Caption),
            );
            for (label, button, button_rect) in [
                (copy_label, Button::Resend(invite.id), resend),
                (cancel_label, Button::CancelPending(invite.id), cancel),
            ] {
                Self::draw_row_button(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    button_rect,
                    label.as_str(),
                );
                self.button_rects.push((button, button_rect));
                self.button_labels.push(DrawnLabel {
                    label,
                    description: Some(invite.name.clone()),
                    selected: None,
                });
            }
            y = row.y1 + s2;
        }
        y
    }

    /// A pill button inside a pending-invitation row: its lift and its
    /// centred label.
    fn draw_row_button(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        ui::surface::card(
            scene,
            transform,
            rect,
            rect.height() / 2.0,
            rest_floor(theme),
        );
        let (width, height) = text.measure_style(label, None, &theme.text(TextRole::Caption));
        text.draw_style(
            scene,
            label,
            colors.text_primary,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(width) / 2.0,
                    rect.center().y - f64::from(height) / 2.0,
                )
                    .into(),
            ),
            None,
            &theme.text(TextRole::Caption),
        );
    }

    fn draw_body(&mut self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, y: f64) -> f64 {
        if self.state.pairing_open {
            return self.draw_pairing(ctx, m, y);
        }
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

    /// "Pair a new device": the link as a large code a phone scans, the
    /// link in a chip that copies it, the three ways a new device uses it,
    /// why it is safe, and the sheet waiting for the device.
    fn draw_pairing(&mut self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, mut y: f64) -> f64 {
        let Some(link) = self.state.pairing_link.clone() else {
            // Making the link, a failure or the device paired: one line.
            return self.draw_pairing_status(ctx, m, y);
        };
        let SheetMetrics { rect, .. } = *m;
        let spacing = &ctx.theme.tokens.spacing;
        let (s3, s4) = (f64::from(spacing.s3), f64::from(spacing.s4));

        // The code, large and centred.
        let side = PAIRING_QR_SIDE.min(m.avail);
        let qr_x = rect.center().x - side / 2.0;
        let mut code = ui::widget::qr::QrWidget::new(link.as_bytes())
            .with_secret()
            .with_alt(t("pairing-qr-alt"));
        code.set_bounds(Rect::new(qr_x, y, qr_x + side, y + side));
        ui::widget::Widget::draw(
            &mut code,
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ui::presence::PresenceCtx::empty(),
        );
        y += side + s3;
        y = self.draw_pair_link_chip(ctx, m, y, &link) + s4;
        y = draw_pairing_ways(ctx, m, y) + s3;
        y = draw_pairing_safe(ctx, m, y) + s4;
        self.draw_pairing_waiting(ctx, m, y)
    }

    /// The sheet's one line while no link is out.
    fn draw_pairing_status(&self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, y: f64) -> f64 {
        let Some(status) = self.state.status.as_deref() else {
            return y;
        };
        let body = ctx.theme.text(TextRole::Body);
        let (_, height) = ctx
            .text
            .measure_style(status, Some(f32_of_f64(m.avail)), &body);
        ctx.text.draw_style(
            ctx.scene,
            status,
            ctx.theme.tokens.colors.text_primary,
            ctx.transform.pre_translate((m.rect.x0 + m.pad, y).into()),
            Some(f32_of_f64(m.avail)),
            &body,
        );
        y + f64::from(height)
    }

    /// The link, shortened to its site, in a centred chip that copies it.
    fn draw_pair_link_chip(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        m: &SheetMetrics,
        y: f64,
        link: &str,
    ) -> f64 {
        let theme = ctx.theme;
        let tokens = &theme.tokens;
        let caption = theme.text(TextRole::Caption);
        let caption_h = f64::from(caption.line_height_px());
        let (s2, s3) = (f64::from(tokens.spacing.s2), f64::from(tokens.spacing.s3));
        let copied = self.state.pairing_copied;
        let chip_text = if copied {
            t("pairing-copied")
        } else {
            format!("{} · {}", short_pairing_link(link), t("pairing-copy-link"))
        };
        let icon = 14.0;
        let (text_w, _) = ctx.text.measure_style(&chip_text, None, &caption);
        let chip_h = f64::from(tokens.metrics.min_pointer_target).max(caption_h + s2 * 2.0);
        let chip_w = (icon + s2 + f64::from(text_w) + s3 * 2.0).min(m.avail);
        let cx = m.rect.center().x;
        let chip = Rect::new(cx - chip_w / 2.0, y, cx + chip_w / 2.0, y + chip_h);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            rest_floor(theme),
            None,
            &RoundedRect::from_rect(chip, chip_h / 2.0),
        );
        let mid = chip.center().y;
        ui::icons::draw_icon(
            ctx.scene,
            ctx.transform,
            Rect::new(
                chip.x0 + s3,
                mid - icon / 2.0,
                chip.x0 + s3 + icon,
                mid + icon / 2.0,
            ),
            if copied {
                ui::icons::Icon::Check
            } else {
                ui::icons::Icon::Copy
            },
            tokens.colors.text_primary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let shown = ctx.text.elide_to_width_style(
            &chip_text,
            f32_of_f64((chip_w - icon - s2 - s3 * 2.0).max(0.0)),
            &caption,
        );
        ctx.text.draw_style(
            ctx.scene,
            &shown,
            tokens.colors.text_primary,
            ctx.transform
                .pre_translate((chip.x0 + s3 + icon + s2, mid - caption_h / 2.0).into()),
            None,
            &caption,
        );
        self.button_rects.push((Button::CopyPairLink, chip));
        self.button_labels.push(DrawnLabel {
            label: t("pairing-copy-link"),
            description: None,
            selected: None,
        });
        y + chip_h
    }

    /// Waiting, gently: a small turning indicator beside the line, centred.
    fn draw_pairing_waiting(&mut self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, y: f64) -> f64 {
        let theme = ctx.theme;
        let caption = theme.text(TextRole::Caption);
        let caption_h = f64::from(caption.line_height_px());
        let s2 = f64::from(theme.tokens.spacing.s2);
        let status = self
            .state
            .status
            .clone()
            .unwrap_or_else(|| t("pairing-waiting"));
        let (status_w, _) = ctx.text.measure_style(&status, None, &caption);
        let line_w = caption_h + s2 + f64::from(status_w);
        let x = m.rect.center().x - line_w / 2.0;
        self.waiting
            .set_rect(Rect::new(x, y, x + caption_h, y + caption_h));
        ui::widget::Widget::draw(
            &mut self.waiting,
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ui::presence::PresenceCtx::empty(),
        );
        ctx.text.draw_style(
            ctx.scene,
            &status,
            theme.tokens.colors.text_dim,
            ctx.transform.pre_translate((x + caption_h + s2, y).into()),
            None,
            &caption,
        );
        y + caption_h
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
        let heading = ctx.text.elide_to_width_style(
            &t("peer-sync-recipient-heading"),
            f32_of_f64(m.avail),
            &ctx.theme.text(TextRole::Caption),
        );
        ctx.text.draw_style(
            ctx.scene,
            &heading,
            colors.text_dim,
            ctx.transform.pre_translate((rect.x0 + pad, y).into()),
            None,
            &ctx.theme.text(TextRole::Caption),
        );
        let spacing = &ctx.theme.tokens.spacing;
        let (s1, s2, s3) = (
            f64::from(spacing.s1),
            f64::from(spacing.s2),
            f64::from(spacing.s3),
        );
        let choice_h = f64::from(ctx.theme.tokens.metrics.min_pointer_target);
        y += f64::from(body) + s2;
        // Snapshot the short rendered list before registering hit targets;
        // drawing a target mutates `self.button_rects`, so borrowing the
        // source collection through that call would be invalid.
        let choices: Vec<RememberedInvitee> =
            self.remembered_invitees.iter().take(2).cloned().collect();
        for (index, person) in choices.into_iter().enumerate() {
            let choice = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + choice_h);
            let selected = self.state.invite_recipient_path == InviteRecipientPath::Known
                && self.state.invite_recipient_key == Some(person.user_public_key);
            // The name leads and how they were verified trails in secondary
            // ink: two facts, not one string joined by a dot.
            self.draw_choice(
                ctx,
                choice,
                (person.name.as_str(), Some(person.verification.as_str())),
                selected,
                Button::SelectKnown(index),
            );
            y = choice.y1 + s1;
        }
        let choice = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + choice_h);
        self.draw_choice(
            ctx,
            choice,
            (t("peer-sync-recipient-new").as_str(), None),
            self.state.invite_recipient_path == InviteRecipientPath::New,
            Button::SelectNewRecipient,
        );
        choice.y1 + s3
    }

    fn draw_role_choices(&mut self, ctx: &mut PaintCtx<'_>, m: &SheetMetrics, mut y: f64) -> f64 {
        let SheetMetrics {
            rect, pad, body, ..
        } = *m;
        let colors = &ctx.theme.tokens.colors;
        let heading = ctx.text.elide_to_width_style(
            &t("peer-sync-role-heading"),
            f32_of_f64(m.avail),
            &ctx.theme.text(TextRole::Caption),
        );
        ctx.text.draw_style(
            ctx.scene,
            &heading,
            colors.text_dim,
            ctx.transform.pre_translate((rect.x0 + pad, y).into()),
            None,
            &ctx.theme.text(TextRole::Caption),
        );
        let spacing = &ctx.theme.tokens.spacing;
        let choice_h = f64::from(ctx.theme.tokens.metrics.min_pointer_target);
        y += f64::from(body) + f64::from(spacing.s2);
        let options = [
            (crate::network::invite::InviteRole::Viewer, 0.0),
            (crate::network::invite::InviteRole::Commenter, 1.0),
            (crate::network::invite::InviteRole::Editor, 2.0),
        ];
        let gap = f64::from(spacing.s2);
        let width = (rect.width() - pad * 2.0 - gap * 2.0) / 3.0;
        for (role, column) in options {
            let label = role_label(role);
            let x0 = rect.x0 + pad + (width + gap) * column;
            let choice = Rect::new(x0, y, x0 + width, y + choice_h);
            self.draw_choice(
                ctx,
                choice,
                (label.as_str(), None),
                self.state.invite_role == role,
                Button::SelectRole(role),
            );
        }
        y + choice_h
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
                colors.intent.accent
            } else {
                colors.text_primary
            };
            let shown =
                text.elide_to_width_style(line, f32_of_f64(avail), &theme.text(TextRole::Caption));
            text.draw_style(
                scene,
                &shown,
                color,
                transform.pre_translate((rect.x0 + pad, y).into()),
                None,
                &theme.text(TextRole::Caption),
            );
            y += f64::from(body) + f64::from(theme.tokens.spacing.s2);
        }
        let note = text.elide_to_width_style(
            &t("peer-sync-review-note"),
            f32_of_f64(avail),
            &theme.text(TextRole::Caption),
        );
        text.draw_style(
            scene,
            &note,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y + f64::from(theme.tokens.spacing.s1)).into()),
            None,
            &theme.text(TextRole::Caption),
        );
        y + f64::from(body) + f64::from(theme.tokens.spacing.s2)
    }

    fn draw_choice(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        (label, detail): (&str, Option<&str>),
        selected: bool,
        button: Button,
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
            rest_floor(theme)
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
        let inset = f64::from(theme.tokens.spacing.s3);
        let room = (rect.width() - 2.0 * inset).max(0.0);
        if let Some(detail) = detail {
            // A row: the label on the leading inset, the detail ending on
            // the trailing one, each elided against the room the other left.
            let (detail_w, _) = text.measure_style(detail, None, &theme.text(TextRole::Caption));
            let detail_w = f64::from(detail_w).min(room / 2.0);
            let shown = text.elide_to_width_style(
                label,
                f32_of_f64((room - detail_w - inset).max(0.0)),
                &theme.text(TextRole::Caption),
            );
            let detail = text.elide_to_width_style(
                detail,
                f32_of_f64(detail_w),
                &theme.text(TextRole::Caption),
            );
            let (_, height) = text.measure_style(&shown, None, &theme.text(TextRole::Caption));
            let ty = rect.center().y - f64::from(height) / 2.0;
            text.draw_style(
                scene,
                &shown,
                colors.text_primary,
                transform.pre_translate((rect.x0 + inset, ty).into()),
                None,
                &theme.text(TextRole::Caption),
            );
            let (dw, _) = text.measure_style(&detail, None, &theme.text(TextRole::Caption));
            text.draw_style(
                scene,
                &detail,
                colors.text_dim,
                transform.pre_translate((rect.x1 - inset - f64::from(dw), ty).into()),
                None,
                &theme.text(TextRole::Caption),
            );
        } else {
            let shown =
                text.elide_to_width_style(label, f32_of_f64(room), &theme.text(TextRole::Caption));
            let (width, height) = text.measure_style(&shown, None, &theme.text(TextRole::Caption));
            text.draw_style(
                scene,
                &shown,
                colors.text_primary,
                transform.pre_translate(
                    (
                        rect.center().x - f64::from(width) / 2.0,
                        rect.center().y - f64::from(height) / 2.0,
                    )
                        .into(),
                ),
                None,
                &theme.text(TextRole::Caption),
            );
        }
        self.button_rects.push((button, rect));
        self.button_labels.push(DrawnLabel {
            label: label.to_owned(),
            description: detail.map(str::to_owned),
            selected: Some(selected),
        });
    }

    /// The footer's buttons and where they go: right-aligned on the sheet's
    /// bottom padding line, each as wide as its face, wrapping onto a row
    /// above when the next one would run past the sheet's leading padding.
    fn layout_buttons(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &SheetMetrics,
    ) -> Vec<(String, Button, Rect, bool)> {
        let SheetMetrics { rect, pad, .. } = *m;
        let (text, theme) = (&mut *ctx.text, ctx.theme);
        let buttons: Vec<(String, Button)> = if self.state.pairing_open {
            // The link copies from its chip; the footer only closes.
            vec![(t("pairing-done-button"), Button::Cancel)]
        } else if self.state.invite_review_open {
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
        // (§9.6) with the Control radius (§9.1).
        let bh = f64::from(theme.tokens.metrics.min_pointer_target);
        let gap = f64::from(theme.tokens.spacing.s2);
        let mut by = rect.y1 - pad - bh;
        let mut bx = rect.x1 - pad;
        let mut out = Vec::with_capacity(buttons.len());
        for (label, button) in buttons {
            let primary = matches!(
                button,
                Button::Join | Button::CopyToken | Button::CreateInvite | Button::CopyPairLink
            );
            // Sized by the face itself (its label style, role and padding),
            // not by the label measured at the sheet's caption size: the
            // Large face sets its label in its own, larger style, so a
            // caption-measured width elided "Cancel" to "Ca...".
            let bw = button_face(Rect::ZERO, &label, primary)
                .intrinsic_width(text, theme)
                .min(m.avail);
            if bx - bw < rect.x0 + pad - 0.5 && bx < rect.x1 - pad {
                by -= bh + gap;
                bx = rect.x1 - pad;
            }
            let brect = Rect::new(bx - bw, by, bx, by + bh);
            out.push((label, button, brect, primary));
            bx = brect.x0 - gap;
        }
        out
    }

    fn paint_buttons(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        buttons: Vec<(String, Button, Rect, bool)>,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        for (label, button, brect, primary) in buttons {
            let face = button_face(brect, &label, primary);
            ui::widget::Button::paint(scene, transform, text, theme, &face);
            self.button_rects.push((button, brect));
            self.button_labels.push(DrawnLabel {
                label,
                description: None,
                selected: None,
            });
        }
    }

    /// The sheet's rect and its drawn buttons' labels, as painted last frame.
    #[cfg(test)]
    pub fn drawn_labels_for_tests(&self) -> (Rect, Vec<String>) {
        (
            self.rect,
            self.button_labels
                .iter()
                .map(|drawn| drawn.label.clone())
                .collect(),
        )
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
                        Button::CopyPairLink => "copy-pair-link",
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

    /// Show `snapshot`, for a screenshot scene.
    pub fn set_snapshot_for_shots(&mut self, snapshot: PeerSyncSnapshot) {
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

/// The pending list's heading, with the page it shows when it pages.
fn pending_heading(offset: usize, end: usize, total: usize) -> String {
    if total > 3 {
        t_args(
            "peer-sync-pending-heading-paged",
            &[
                ("first", offset.saturating_add(1).into()),
                ("last", end.into()),
                ("total", total.into()),
            ],
        )
    } else {
        t("peer-sync-pending-heading")
    }
}

/// A footer button's face: a Large shared `Button`, the primary role for
/// the step's main action.
fn button_face(at: Rect, label: &str, primary: bool) -> ui::widget::ButtonFace<'_> {
    let face = ui::widget::ButtonFace::new(at, label).control_size(ui::widget::ControlSize::Large);
    if primary {
        face.role(ButtonRole::Primary)
    } else {
        face
    }
}

/// The resting floor of a row, field or choice on the sheet: the first
/// step of the floor ramp from the panel ground (HIG §9.3), so its hover
/// and chosen states continue the same ramp.
fn rest_floor(theme: &Theme) -> vello::peniko::Color {
    ui::chrome::selection_floor_ramp(
        theme,
        ui::chrome::SelectionState::active(false),
        ui::chrome::shell::panel_ground(theme),
    )
}

fn card_rect(host: Rect, state: &PeerSyncSnapshot, theme: &Theme) -> Rect {
    // A sheet with a form in it takes the sheet width (HIG §9.6), shrinking
    // inside the panel padding on a narrow host rather than a bespoke
    // 400–520 band. Its height is what the form wants, never more than the
    // host less the same margin: on a short window the body clips above
    // footer buttons that stay anchored (see `draw_at_theme_scale`).
    let margin = f64::from(theme.tokens.spacing.s4);
    let w = f64::from(theme.tokens.metrics.panel_width_sheet)
        .min(host.width() - 2.0 * margin)
        .max(0.0);
    let wanted: f64 = if state.pairing_open {
        if state.pairing_link.is_some() {
            PAIRING_SHEET_HEIGHT
        } else {
            200.0
        }
    } else if state.invite_review_open {
        330.0
    } else if state.invite_name.is_some() {
        if state.pending_invitations.is_empty() {
            430.0
        } else {
            610.0
        }
    } else if state.connect_input.is_some() {
        270.0
    } else {
        200.0
    };
    let h = wanted.min(host.height() - 2.0 * margin).max(0.0);
    let cx = f64::midpoint(host.x0, host.x1);
    let cy = f64::midpoint(host.y0, host.y1);
    Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
}

/// The height of "Pair a new device" with its link out: the heading, the
/// code, the link chip, the three ways, the reassurance, the waiting line
/// and the footer, with nothing left over below.
const PAIRING_SHEET_HEIGHT: f64 = ui::tokens::Layout::BASE.pairing_sheet_height as f64;

/// The side of the pairing code: large enough for a phone camera from
/// arm's length.
const PAIRING_QR_SIDE: f64 = ui::tokens::Layout::BASE.pairing_qr_side as f64;

/// The pairing link as its chip shows it: the site, then an ellipsis for
/// the secret rest.
fn short_pairing_link(link: &str) -> String {
    url::Url::parse(link)
        .ok()
        .and_then(|url| url.host_str().map(|host| format!("{host}/…")))
        .unwrap_or_else(|| "…".to_owned())
}

/// The three ways a new device uses the pairing link: its icon, the kind of
/// device, and what to do on it.
const PAIRING_WAYS: [(ui::icons::Icon, &str, &str); 3] = [
    (
        ui::icons::Icon::Phone,
        "pairing-way-phone",
        "pairing-way-phone-how",
    ),
    (
        ui::icons::Icon::Monitor,
        "pairing-way-computer",
        "pairing-way-computer-how",
    ),
    (
        ui::icons::Icon::Globe,
        "pairing-way-browser",
        "pairing-way-browser-how",
    ),
];

/// The side of a way's icon.
const PAIRING_WAY_ICON: f64 = ui::tokens::Layout::BASE.avatar as f64;

/// The column the ways and the reassurance share, centred under the code:
/// its left edge, its width, and where the "how" text starts in it.
fn pairing_column(ctx: &mut PaintCtx<'_>, m: &SheetMetrics) -> (f64, f64, f64) {
    let body = ctx.theme.text(TextRole::Body);
    let spacing = &ctx.theme.tokens.spacing;
    let (s3, s4) = (f64::from(spacing.s3), f64::from(spacing.s4));
    let mut label_w: f64 = 0.0;
    let mut how_w: f64 = 0.0;
    for (_, label, how) in PAIRING_WAYS {
        label_w = label_w.max(f64::from(ctx.text.measure_style(&t(label), None, &body).0));
        how_w = how_w.max(f64::from(ctx.text.measure_style(&t(how), None, &body).0));
    }
    let how_at = PAIRING_WAY_ICON + s3 + label_w + s4;
    let width = (how_at + how_w).min(m.avail);
    (m.rect.center().x - width / 2.0, width, how_at)
}

/// The three ways, one line each, in the shared column.
fn draw_pairing_ways(ctx: &mut PaintCtx<'_>, m: &SheetMetrics, mut y: f64) -> f64 {
    let (x0, width, how_at) = pairing_column(ctx, m);
    let theme = ctx.theme;
    let tokens = &theme.tokens;
    let body = theme.text(TextRole::Body);
    let body_h = f64::from(body.line_height_px());
    let (s2, s3) = (f64::from(tokens.spacing.s2), f64::from(tokens.spacing.s3));
    let icon = PAIRING_WAY_ICON;
    let row_h = body_h.max(icon) + s2;
    for (glyph, label, how) in PAIRING_WAYS {
        let mid = y + row_h / 2.0;
        ui::icons::draw_icon(
            ctx.scene,
            ctx.transform,
            Rect::new(x0, mid - icon / 2.0, x0 + icon, mid + icon / 2.0),
            glyph,
            tokens.colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        ctx.text.draw_style(
            ctx.scene,
            &t(label),
            tokens.colors.text_primary,
            ctx.transform
                .pre_translate((x0 + icon + s3, mid - body_h / 2.0).into()),
            None,
            &body,
        );
        let how_x = x0 + how_at;
        let how = ctx.text.elide_to_width_style(
            &t(how),
            f32_of_f64((x0 + width - how_x).max(0.0)),
            &body,
        );
        ctx.text.draw_style(
            ctx.scene,
            &how,
            tokens.colors.text_dim,
            ctx.transform
                .pre_translate((how_x, mid - body_h / 2.0).into()),
            None,
            &body,
        );
        y += row_h;
    }
    y
}

/// Why pairing is safe, said once, beside a shield, in the shared column.
fn draw_pairing_safe(ctx: &mut PaintCtx<'_>, m: &SheetMetrics, y: f64) -> f64 {
    let (x0, column, _) = pairing_column(ctx, m);
    let theme = ctx.theme;
    let tokens = &theme.tokens;
    let caption = theme.text(TextRole::Caption);
    let caption_h = f64::from(caption.line_height_px());
    let s2 = f64::from(tokens.spacing.s2);
    let shield = 14.0;
    let width = (column - shield - s2).max(0.0);
    let safe = t("pairing-safe");
    let (_, height) = ctx
        .text
        .measure_style(&safe, Some(f32_of_f64(width)), &caption);
    ui::icons::draw_icon(
        ctx.scene,
        ctx.transform,
        Rect::new(
            x0,
            y + (caption_h - shield) / 2.0,
            x0 + shield,
            y + f64::midpoint(caption_h, shield),
        ),
        ui::icons::Icon::Shield,
        tokens.colors.text_dim,
        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    ctx.text.draw_style(
        ctx.scene,
        &safe,
        tokens.colors.text_dim,
        ctx.transform.pre_translate((x0 + shield + s2, y).into()),
        Some(f32_of_f64(width)),
        &caption,
    );
    y + f64::from(height)
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

/// Whose identity the invite binds, or how to bind one.
fn naming_identity_note(recipient: Option<&[u8; 32]>, path: InviteRecipientPath) -> String {
    recipient.map_or_else(
        || t("peer-sync-recipient-hint"),
        |key| {
            let fingerprint: ui::i18n::ArgValue = identity_fingerprint(key).into();
            match path {
                InviteRecipientPath::Known => t_args(
                    "peer-sync-identity-remembered",
                    &[("fingerprint", fingerprint)],
                ),
                InviteRecipientPath::New => {
                    t_args("peer-sync-identity-new", &[("fingerprint", fingerprint)])
                }
            }
        },
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
        ..
    } = *m;
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let colors = &theme.tokens.colors;
    let spacing = &theme.tokens.spacing;
    let (s2, s3) = (f64::from(spacing.s2), f64::from(spacing.s3));
    let label = text.elide_to_width_style(
        &t(if recipient_path == InviteRecipientPath::Known {
            "peer-sync-selected-person"
        } else {
            "peer-sync-name-new-person"
        }),
        f32_of_f64(avail),
        &theme.text(TextRole::Caption),
    );
    text.draw_style(
        scene,
        &label,
        colors.text_dim,
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
        &theme.text(TextRole::Caption),
    );
    y += f64::from(body) + s2;
    let field_h = f64::from(theme.tokens.metrics.min_pointer_target) + f64::from(spacing.s1);
    let field = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + field_h);
    let rr = RoundedRect::from_rect(field, f64::from(theme.tokens.radii.detail));
    scene.fill(Fill::NonZero, transform, rest_floor(theme), None, &rr);
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        colors.intent.accent,
        None,
        &rr,
    );
    let shown = text.elide_to_width_style(
        typed,
        f32_of_f64((avail - 2.0 * s3 - s2).max(0.0)),
        &theme.text(TextRole::Caption),
    );
    let (tw, th) = text.measure_style(&shown, None, &theme.text(TextRole::Caption));
    let ty = f64::midpoint(field.y0, field.y1) - f64::from(th) / 2.0;
    let caret_w = 1.5;
    // The caret sits on the text origin; an empty field shows its placeholder
    // one `s1` to the right of the caret so the two never overlap (HIG §7).
    let text_x = field.x0
        + s3
        + if typed.is_empty() {
            caret_w + f64::from(theme.tokens.spacing.s1)
        } else {
            0.0
        };
    text.draw_style(
        scene,
        &if typed.is_empty() {
            t("peer-sync-name-placeholder")
        } else {
            shown.clone()
        },
        if typed.is_empty() {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate((text_x, ty).into()),
        None,
        &theme.text(TextRole::Caption),
    );
    let caret_x = field.x0
        + s3
        + if typed.is_empty() {
            0.0
        } else {
            f64::from(tw) + 2.0
        };
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.accent,
        None,
        &Rect::new(caret_x, field.y0 + s2, caret_x + caret_w, field.y1 - s2),
    );
    y = field.y1 + s3;
    let identity = naming_identity_note(recipient, recipient_path);
    let note =
        text.elide_to_width_style(&identity, f32_of_f64(avail), &theme.text(TextRole::Caption));
    text.draw_style(
        scene,
        &note,
        colors.text_dim,
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
        &theme.text(TextRole::Caption),
    );
    y + f64::from(body) + s2
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
    let row_h = person_h(ctx.theme);
    y = draw_person(
        ctx,
        Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + row_h),
        who,
        &sub,
        invite_for.is_some(),
    ) + f64::from(ctx.theme.tokens.spacing.s3);
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
        let row_h = person_h(ctx.theme);
        y = draw_person(
            ctx,
            Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + row_h),
            &t("peer-sync-invite-received"),
            &role_and_expiry(
                "peer-sync-received-role-expires",
                invite.role,
                invite.expiry_unix_seconds,
            ),
            false,
        ) + f64::from(ctx.theme.tokens.spacing.s3);
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
    let (line, understood) = if typed.is_empty() {
        (t("peer-sync-paste-invite-hint"), false)
    } else {
        match crate::shell::network::join_field::describe(typed) {
            Some(line) => (line, true),
            None => (
                t_args(
                    "peer-sync-token-characters",
                    &[
                        ("token", elide_token(typed).into()),
                        ("count", typed.chars().count().into()),
                    ],
                ),
                false,
            ),
        }
    };
    if understood {
        // What was typed, in a field, so the person sees what joins.
        y = draw_typed_field(
            &mut PaintCtx::new(scene, transform, text, theme),
            Rect::new(rect.x0 + pad, y, rect.x1 - pad, y),
            avail,
            typed,
        );
    }
    let line = text.elide_to_width_style(&line, f32_of_f64(avail), &theme.text(TextRole::Caption));
    text.draw_style(
        scene,
        &line,
        if typed.is_empty() || understood {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
        &theme.text(TextRole::Caption),
    );
    y += f64::from(name) + 8.0;
    if !typed.is_empty() && !understood {
        let incomplete = text.elide_to_width_style(
            &t("peer-sync-token-incomplete"),
            f32_of_f64(avail),
            &theme.text(TextRole::Caption),
        );
        text.draw_style(
            scene,
            &incomplete,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
            &theme.text(TextRole::Caption),
        );
        y += f64::from(body) + 8.0;
    }
    y
}

/// What was typed, in a field along `row`'s top edge, elided to `avail`;
/// returns where the next line starts.
fn draw_typed_field(ctx: &mut PaintCtx<'_>, row: Rect, avail: f64, typed: &str) -> f64 {
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let colors = &theme.tokens.colors;
    let field_h = f64::from(theme.tokens.metrics.min_pointer_target);
    let field = Rect::new(row.x0, row.y0, row.x1, row.y0 + field_h);
    scene.fill(
        Fill::NonZero,
        transform,
        rest_floor(theme),
        None,
        &RoundedRect::from_rect(field, f64::from(theme.tokens.radii.control)),
    );
    let style = theme.text(TextRole::Body);
    let shown = text.elide_to_width_style(typed, f32_of_f64(avail - 24.0), &style);
    let (_, shown_h) = text.measure_style(&shown, None, &style);
    text.draw_style(
        scene,
        &shown,
        colors.text_primary,
        transform
            .pre_translate((field.x0 + 12.0, field.center().y - f64::from(shown_h) / 2.0).into()),
        None,
        &style,
    );
    field.y1 + 8.0
}

/// A person row: the panel row height (HIG §9.6).
fn person_h(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.panel_row_height)
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
        ui::widget::AvatarFace::new(&initial, who).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            Rect::new(
                center.x - 16.0,
                center.y - 16.0,
                center.x + 16.0,
                center.y + 16.0,
            ),
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
    let ns = theme.tokens.typography.size(TextRole::Caption).max(12.0);
    let ss = theme.tokens.typography.size(TextRole::Caption).max(10.0);
    let who =
        text.elide_to_width_style(who, f32_of_f64(rect.x1 - x), &theme.text(TextRole::Caption));
    text.draw_style(
        scene,
        &who,
        colors.text_primary,
        transform.pre_translate((x, rect.y0 + 2.0).into()),
        None,
        &theme.text(TextRole::Caption),
    );
    let sub =
        text.elide_to_width_style(sub, f32_of_f64(rect.x1 - x), &theme.text(TextRole::Caption));
    text.draw_style(
        scene,
        &sub,
        colors.text_dim,
        transform.pre_translate((x, rect.y0 + 2.0 + f64::from(ns) + 6.0).into()),
        None,
        &theme.text(TextRole::Caption),
    );
    rect.y0 + 2.0 + f64::from(ns) + 6.0 + f64::from(ss)
}

fn draw_code(ctx: &mut PaintCtx<'_>, at: Point, code: &str, avail: f64) -> f64 {
    let (scene, transform, text, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let colors = &theme.tokens.colors;
    let (_, fh) = text.measure_style(code, None, &theme.text(TextRole::Heading));
    let (inset_x, inset_y) = (
        f64::from(theme.tokens.spacing.s3),
        f64::from(theme.tokens.spacing.s2),
    );
    let field = Rect::new(
        at.x,
        at.y,
        at.x + avail,
        at.y + f64::from(fh) + 2.0 * inset_y,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        rest_floor(theme),
        None,
        &RoundedRect::from_rect(field, f64::from(theme.tokens.radii.control)),
    );
    text.draw_style(
        scene,
        code,
        colors.text_primary,
        transform.pre_translate((field.x0 + inset_x, field.y0 + inset_y).into()),
        None,
        &theme.text(TextRole::Heading),
    );
    field.y1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pairing sheet shows its link as the site only, animates only
    /// while it waits for a device, and stops once closed.
    #[test]
    fn the_pairing_sheet_shows_the_site_and_waits_while_its_link_is_out() {
        let shown = short_pairing_link("https://tilo.coffee/#/@eu1.tilo.coffee/w/x?pair=a1a1");
        assert_eq!(shown, "tilo.coffee/…");
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_pair();
        assert!(!overlay.needs_animation_frame(), "nothing to wait for yet");
        overlay.show_pairing("https://tilo.coffee/#/w/x?pair=a1".into());
        assert!(
            overlay.needs_animation_frame(),
            "waiting for the new device"
        );
        overlay.pairing_done("Safari on iPhone");
        assert!(!overlay.needs_animation_frame(), "the device paired");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn named_invites_bind_role_and_verified_identity_before_review() {
        /// HIG §9.5: the invite card's rows and mini pills rest on the lift
        /// floor with an edge derived from it; that edge must register on every
        /// shipped palette, and the card's Panel border on its Panel ground:
        /// as a hairline in high contrast, where it alone separates the panel,
        /// and as the quieter floating edge elsewhere.
        fn invite_rows_and_panel_edge_register_on_every_palette() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let ground = ui::chrome::shell::panel_ground(&theme);
                let floor = rest_floor(&theme);
                let panel_min = if theme.tokens.colors.palette.high_contrast {
                    ui::tokens::colors::HAIRLINE_MIN_DELTA
                } else {
                    ui::tokens::colors::FLOATING_EDGE_MIN_DELTA
                };
                for (what, edge, fill, min) in [
                    (
                        "panel border",
                        ui::chrome::shell::panel_border(&theme),
                        ground,
                        panel_min,
                    ),
                    (
                        "row card",
                        ui::surface::edge_on(floor),
                        floor,
                        ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    ),
                ] {
                    let delta = ui::surface::edge_delta(edge, fill);
                    assert!(
                        delta >= min,
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
        let event = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
        ui::synthetic_input::keyboard_input(event)
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
