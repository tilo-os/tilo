//! Peer invite, join and pairing sheet.
//!
//! This component owns the local interaction: what the sheet is asking,
//! what was typed and chosen, the clipboard and the requests it makes. The
//! sheet itself is the shared modal and the shared controls
//! (`sheet::Sheet`); this file says what they mean. The shell host mounts
//! the overlay in its widget tree like any retained widget, performs the
//! network request it takes from [`PeerSyncOverlay::take_request`] and
//! returns the result through [`PeerSyncOverlay::finish_typed_request`].

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use winit::keyboard::NamedKey;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::shell::i18n::{t, t_args};

mod sheet;

/// How long a copied invite token stays on the clipboard: time to paste it
/// into a message to the person it is for.
const INVITE_ON_CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(300);

/// How long an invite admits: an hour, as a person's invite does.
pub const INVITE_EXPIRES_IN_SECONDS: u64 = 3600;

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
    /// Try again to join a workspace whose join failed, or that needs
    /// joining again: from the workspace itself, which is the room.
    RetryJoin(crate::shell::state::RoomId),
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
    /// No one in particular: whoever opens the link joins. The sheet says
    /// so, since the link itself is the credential.
    Link,
}

impl InviteRecipientPath {
    /// The invite names a person (a remembered or a pasted identity).
    pub fn is_person(self) -> bool {
        !matches!(self, Self::Link)
    }
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
    /// The person's device that found it, when another of their devices
    /// asks this one to approve it.
    pub via: Option<String>,
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
    pub invite_context: InviteContext,
    pub status: Option<String>,
    pub pairing_open: bool,
    /// Its link, once made: what the new device opens.
    pub pairing_link: Option<String>,
    /// The link went to the clipboard.
    pub pairing_copied: bool,
}

/// What the person asked the sheet for, whichever control they used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    CreateInvite,
    /// Who the invite is for: a person (`false`) or anyone with the link.
    SelectWho(bool),
    SelectRole(crate::network::invite::InviteRole),
    /// A person the Who field suggested, by their place among the people
    /// this computer knows.
    SelectKnown(usize),
    /// Let go of a pasted identity.
    ForgetIdentity,
    Join,
    Cancel,
    CopyToken,
    PasteToken,
    Resend([u8; 32]),
    CancelPending([u8; 32]),
    CopyIdentity,
    CopyPairLink,
}

/// Result of one intent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EventOutcome {
    request: Option<PeerSyncRequest>,
    /// A named invite asks to copy only after minting succeeds.
    copy_result: bool,
}

impl EventOutcome {
    fn consumed() -> Self {
        Self {
            request: None,
            copy_result: false,
        }
    }

    fn request(request: PeerSyncRequest, copy_result: bool) -> Self {
        Self {
            request: Some(request),
            copy_result,
        }
    }
}

#[derive(Default)]
pub struct PeerSyncOverlay {
    state: PeerSyncSnapshot,
    remembered_invitees: Vec<RememberedInvitee>,
    /// The sheet: the shared modal and the controls in it.
    sheet: sheet::Sheet,
    /// Whether the status says something went wrong, shown as an error.
    status_is_error: bool,
    /// The request the person's last input made, until the host takes it,
    /// and whether its result is copied once it is back.
    requested: Option<(PeerSyncRequest, bool)>,
    enrollment: Option<u64>,
    enrollment_poll_at: Option<std::time::Instant>,
    retired_enrollments: std::collections::VecDeque<u64>,
    /// Whether the sheet offers an invite link for anyone besides an invite
    /// for a person. Off: such a link is a credential whoever holds it can
    /// use, and the product has not decided to offer it here.
    link_invites: bool,
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
        sheet::Mode::of(&self.state).is_some()
    }

    /// Say `status`, as information.
    fn say(&mut self, status: String) {
        self.state.status = Some(status);
        self.status_is_error = false;
    }

    /// Say what went wrong and what the person can do, as an error.
    fn fail(&mut self, status: String) {
        self.state.status = Some(status);
        self.status_is_error = true;
    }

    /// Say nothing.
    fn quiet(&mut self) {
        self.state.status = None;
        self.status_is_error = false;
    }

    /// Open "Pair a new device": the link is being made.
    pub fn prompt_pair(&mut self) {
        self.retire_enrollment();
        self.state.invite_name = None;
        self.state.connect_input = None;
        self.state.invite_token = None;
        self.state.pairing_open = true;
        self.state.pairing_link = None;
        self.state.pairing_copied = false;
        self.say(t("pairing-making"));
        self.refresh();
    }

    /// Show the pairing link the network made, while the sheet is open.
    pub fn show_pairing(&mut self, link: String) {
        if !self.state.pairing_open {
            return;
        }
        self.state.pairing_link = Some(link);
        self.state.pairing_copied = false;
        self.say(t("pairing-waiting"));
        self.refresh();
    }

    /// Whether the sheet animates: it arrives, and its waiting indicator
    /// turns while a pairing link is out.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        self.sheet
            .dialog()
            .is_some_and(ui::widget::Widget::needs_animation_frame)
    }

    /// Say why no pairing link could be made.
    pub fn pairing_failed(&mut self, error: &str) {
        if self.state.pairing_open {
            tracing::warn!(%error, "no pairing link");
            self.fail(t("pairing-failed"));
            self.refresh();
        }
    }

    /// Say that the device the link invited became one of the person's,
    /// and close the sheet's waiting.
    pub fn pairing_done(&mut self, label: &str) {
        if self.state.pairing_open {
            self.state.pairing_link = None;
            self.say(t_args(
                "pairing-done",
                &[("device", label.to_owned().into())],
            ));
            self.refresh();
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
        self.quiet();
        self.refresh();
    }

    /// Offer, or stop offering, an invite link for anyone. Off by default.
    pub fn set_link_invites_enabled(&mut self, enabled: bool) {
        self.link_invites = enabled;
        if !enabled && self.state.invite_recipient_path == InviteRecipientPath::Link {
            self.state.invite_recipient_path = InviteRecipientPath::New;
        }
        // Who stands above the field only while there is a choice.
        self.sheet.rebuild();
        self.refresh();
    }

    /// The invite is for a person: always, unless links for anyone are
    /// offered and chosen.
    fn names_person(&self) -> bool {
        self.state.invite_recipient_path.is_person() || !self.link_invites
    }

    pub fn composer_open(&self) -> bool {
        self.state.invite_name.is_some()
    }

    /// Say, in place of anything the network would have answered, why the
    /// sheet cannot invite right now and what the person can do.
    pub fn show_unavailable(&mut self, message: String) {
        self.fail(message);
        self.refresh();
    }

    /// The network came up: an explanation of its absence no longer holds.
    pub fn clear_unavailable(&mut self, message: &str) {
        if self.state.status.as_deref() == Some(message) {
            self.quiet();
            self.refresh();
        }
    }

    pub fn prompt_invite_for(&mut self, name: String) {
        self.prompt_invite();
        self.state.invite_name = Some(name);
        self.refresh();
    }

    pub fn prompt_connect(&mut self) {
        self.retire_enrollment();
        self.state.invite_name = None;
        self.state.connect_input = Some(String::new());
        self.quiet();
        self.refresh();
    }

    /// Open the join field holding `text`, a link or a code the person
    /// typed elsewhere (the directory's search); empty, as
    /// [`Self::prompt_connect`].
    pub fn prompt_connect_with(&mut self, text: &str) {
        self.prompt_connect();
        let text: String = text.split_whitespace().collect();
        if !text.is_empty() {
            self.state.connect_input = Some(text);
            self.quiet();
        }
        self.refresh();
    }

    /// Say the card's `request` is being carried out: minting, redeeming,
    /// resending or cancelling waits on the network off the frame loop, and
    /// [`Self::finish_typed_request`] replaces this once it is back.
    pub fn begin_request(&mut self, request: &PeerSyncRequest) {
        if matches!(
            request,
            PeerSyncRequest::IssueInviteFor { .. }
                | PeerSyncRequest::IssueInvitation(_)
                | PeerSyncRequest::AcceptInvite(_)
                | PeerSyncRequest::ResendInvitation(_)
                | PeerSyncRequest::CancelInvitation(_)
        ) {
            self.say(t("peer-sync-working"));
            self.refresh();
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

    /// Apply one request's outcome to the sheet. A failure is said in the
    /// person's terms, with what they can do; what the network answered is
    /// logged, never shown.
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
        if let Err(error) = &outcome {
            tracing::warn!(%error, ?request, "peer-sync request failed");
        }
        match (request, outcome) {
            (PeerSyncRequest::AcceptInvite(_), Ok(PeerSyncResponse::EnrollmentPending { id })) => {
                self.enrollment = Some(id);
                self.enrollment_poll_at = None;
                self.say(t("peer-sync-joining"));
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
                self.say(format!("Joined room {room}"));
            }
            (PeerSyncRequest::PollEnrollment(id), Err(error)) if self.enrollment == Some(*id) => {
                self.enrollment = None;
                self.enrollment_poll_at = None;
                self.fail(failure(&error, "peer-sync-join-refused"));
            }
            // An invitation action answers its caller, not this card; leaving
            // a workspace and pairing a device answer the surfaces that asked.
            (
                PeerSyncRequest::PollEnrollment(_)
                | PeerSyncRequest::CancelEnrollment(_)
                | PeerSyncRequest::PrepareInvitation
                | PeerSyncRequest::LeaveSite(_)
                | PeerSyncRequest::RetryJoin(_)
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
                self.say(t("peer-sync-identity-copied"));
            }
            (
                PeerSyncRequest::IssueInviteFor { name, .. },
                Ok(PeerSyncResponse::Message(token)),
            ) => {
                self.finish_invite(token, Some(name.clone()), copy_result);
            }
            // A link for anyone: the card's own, with no one named.
            (PeerSyncRequest::IssueInvitation(_), Ok(PeerSyncResponse::Message(token))) => {
                self.finish_invite(token, None, copy_result);
            }
            (
                PeerSyncRequest::IssueInviteFor { .. } | PeerSyncRequest::IssueInvitation(_),
                Err(error),
            ) => {
                self.state.invite_token = None;
                self.state.invite_for = None;
                self.fail(failure(&error, "peer-sync-invite-failed"));
            }
            (PeerSyncRequest::ExportIdentity, Err(error)) => {
                self.fail(failure(&error, "peer-sync-identity-unavailable"));
            }
            (PeerSyncRequest::AcceptInvite(_), Ok(PeerSyncResponse::Message(room))) => {
                self.state.connect_input = None;
                self.say(t_args("peer-sync-joined-room", &[("room", room.into())]));
            }
            (PeerSyncRequest::AcceptInvite(_), Err(error)) => {
                self.fail(failure(&error, "peer-sync-join-refused"));
            }
            (
                PeerSyncRequest::ListPendingInvitations,
                Ok(PeerSyncResponse::PendingInvitations(rows)),
            ) => self.state.pending_invitations = rows,
            // Without the list there is nothing pending to show; the person
            // can still invite, and the network's own words stay in the log.
            (PeerSyncRequest::ListPendingInvitations, Err(_)) => {
                self.state.pending_invitations.clear();
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
                self.fail(failure(&error, "peer-sync-resend-failed"));
            }
            (PeerSyncRequest::CancelInvitation(_), Ok(PeerSyncResponse::Message(_))) => {
                self.say(t("peer-sync-invite-canceled"));
            }
            (PeerSyncRequest::CancelInvitation(_), Err(error)) => {
                self.fail(failure(&error, "peer-sync-cancel-failed"));
            }
            (_, Ok(_)) => {
                self.fail(t("peer-sync-unexpected-response"));
            }
        }
        self.refresh();
    }

    fn finish_invite(&mut self, token: String, invite_for: Option<String>, copy_result: bool) {
        self.quiet();
        self.state.invite_token = Some(token);
        self.state.invite_for = invite_for;
        self.state.invite_name = None;
        if copy_result {
            self.copy_invite_token();
        }
    }

    /// The request the person's last input made, and whether its result is
    /// copied once it is back. The host carries it out.
    pub fn take_request(&mut self) -> Option<(PeerSyncRequest, bool)> {
        self.requested.take()
    }

    /// Route one event to the sheet. While it is up the sheet is modal:
    /// every event is its own, so a token's characters never reach a
    /// shell chord. Only the unmodified Tab, and the Enter or Space it did
    /// not use, go on to the window, which moves focus or presses the
    /// focused control with them.
    fn route(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.visible() {
            return;
        }
        let modifiers = ctx.modifiers.state();
        let chord = modifiers.control_key() || modifiers.super_key();
        // Showing an invite, Copy is the sheet's whole point: the copy
        // chord does it too.
        if chord
            && sheet::Mode::of(&self.state) == Some(sheet::Mode::Created)
            && typed(event).is_some_and(|key| key.eq_ignore_ascii_case("c"))
        {
            self.activate(Intent::CopyToken);
            *ctx.handled = true;
            self.refresh();
            return;
        }
        self.sheet.before_event();
        self.sheet.handle_event(event, ctx);
        let unmodified = !modifiers.control_key() && !modifiers.alt_key() && !modifiers.super_key();
        let window_key = unmodified
            && [NamedKey::Tab, NamedKey::Enter, NamedKey::Space]
                .iter()
                .any(|key| pressed(event, *key));
        if !window_key {
            *ctx.handled = true;
        }
        self.after_input();
    }

    /// Read what the person did in the sheet and act on it.
    fn after_input(&mut self) {
        match sheet::Mode::of(&self.state) {
            Some(sheet::Mode::Invite) => {
                let typed = self.sheet.who_text();
                if self.state.invite_name.as_deref() != Some(typed.as_str()) {
                    self.name_typed(typed);
                }
                if let Some(key) = self.sheet.take_accepted()
                    && let Some(index) = self
                        .remembered_invitees
                        .iter()
                        .position(|person| person.user_public_key == key)
                {
                    self.activate(Intent::SelectKnown(index));
                }
                let role = sheet::ROLES
                    .get(self.sheet.role.get())
                    .copied()
                    .unwrap_or(self.state.invite_role);
                if role != self.state.invite_role {
                    self.activate(Intent::SelectRole(role));
                }
                if self.link_invites {
                    let link = self.sheet.audience.get() == 1;
                    if link == self.names_person() {
                        self.activate(Intent::SelectWho(link));
                    }
                }
            }
            Some(sheet::Mode::Join) => {
                // No site, link or invite holds a space: what a link picked
                // up on its way here (a chat's line breaks) is let go.
                let typed: String = self.sheet.link_text().split_whitespace().collect();
                if self.state.connect_input.as_deref() != Some(typed.as_str()) {
                    self.state.connect_input = Some(typed);
                }
            }
            Some(sheet::Mode::Created | sheet::Mode::Pairing) | None => {}
        }
        let presses = self.sheet.presses.clone();
        let mut intents: Vec<Intent> = [
            (&presses.create, Intent::CreateInvite),
            (&presses.join, Intent::Join),
            (&presses.paste_link, Intent::PasteToken),
            (&presses.copy_identity, Intent::CopyIdentity),
            (&presses.copy_invite, Intent::CopyToken),
            (&presses.copy_pair_link, Intent::CopyPairLink),
            (&presses.forget_identity, Intent::ForgetIdentity),
            (&presses.cancel, Intent::Cancel),
        ]
        .into_iter()
        .filter(|(flag, _)| sheet::take(flag))
        .map(|(_, intent)| intent)
        .collect();
        intents.extend(sheet::take_id(&presses.resend).map(Intent::Resend));
        intents.extend(sheet::take_id(&presses.cancel_pending).map(Intent::CancelPending));
        for intent in intents {
            let outcome = self.activate(intent);
            if let Some(request) = outcome.request {
                self.requested = Some((request, outcome.copy_result));
            }
        }
        self.refresh();
    }

    /// The Who field changed. An identity pasted into it binds the invite
    /// to that identity and leaves the rest of what was typed as the name;
    /// anything else is the name, which narrows the people offered.
    fn name_typed(&mut self, typed: String) {
        if let Some((key, rest)) = find_recipient_identity(&typed) {
            // Never carry a remembered person's display name over to a
            // freshly exchanged key; the token would remain safe, but the
            // recipient name recorded alongside it would be misleading.
            let name = if self.state.invite_recipient_path == InviteRecipientPath::Known {
                String::new()
            } else {
                rest
            };
            self.state.invite_name = Some(name);
            self.state.invite_recipient_key = Some(key);
            self.state.invite_recipient_path = InviteRecipientPath::New;
            self.quiet();
            self.sheet.close_suggestions();
            return;
        }
        self.state.invite_name = Some(typed);
        self.name_edited();
        let offered: Vec<(String, [u8; 32], String)> = self
            .suggestions()
            .into_iter()
            .filter_map(|index| self.remembered_invitees.get(index))
            .map(|person| {
                (
                    person.name.clone(),
                    person.user_public_key,
                    person.verification.clone(),
                )
            })
            .collect();
        self.sheet.offer(&offered);
    }

    /// The name was typed into: a remembered person stays chosen only while
    /// the field still holds their name, so a name never carries another
    /// person's identity.
    fn name_edited(&mut self) {
        if self.state.invite_recipient_path != InviteRecipientPath::Known {
            return;
        }
        let typed = self.state.invite_name.as_deref().unwrap_or_default();
        let still = self.remembered_invitees.iter().any(|person| {
            Some(person.user_public_key) == self.state.invite_recipient_key && person.name == typed
        });
        if !still {
            self.state.invite_recipient_path = InviteRecipientPath::New;
            self.state.invite_recipient_key = None;
            self.quiet();
        }
    }

    /// The remembered people whose name holds what was typed, by their index
    /// in the remembered list: the field's suggestions. Nobody is offered
    /// once the field names the person already chosen.
    fn suggestions(&self) -> Vec<usize> {
        let typed = self
            .state
            .invite_name
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if typed.is_empty() || self.state.invite_recipient_path == InviteRecipientPath::Known {
            return Vec::new();
        }
        self.remembered_invitees
            .iter()
            .enumerate()
            .filter(|(_, person)| person.name.to_lowercase().contains(&typed))
            .map(|(index, _)| index)
            .collect()
    }

    fn activate(&mut self, intent: Intent) -> EventOutcome {
        match intent {
            Intent::CreateInvite => self.submit_invite(),
            Intent::SelectWho(link) => {
                if link && !self.link_invites {
                    return EventOutcome::consumed();
                }
                if link {
                    self.state.invite_recipient_path = InviteRecipientPath::Link;
                } else if !self.state.invite_recipient_path.is_person() {
                    self.state.invite_recipient_path = if self.state.invite_recipient_key.is_some()
                        && self.remembered_invitees.iter().any(|person| {
                            Some(person.user_public_key) == self.state.invite_recipient_key
                        }) {
                        InviteRecipientPath::Known
                    } else {
                        InviteRecipientPath::New
                    };
                }
                self.quiet();
                EventOutcome::consumed()
            }
            Intent::SelectRole(role) => {
                self.state.invite_role = role;
                EventOutcome::consumed()
            }
            Intent::SelectKnown(index) => {
                if let Some(person) = self.remembered_invitees.get(index) {
                    self.state.invite_name = Some(person.name.clone());
                    self.state.invite_recipient_key = Some(person.user_public_key);
                    self.state.invite_recipient_path = InviteRecipientPath::Known;
                    self.quiet();
                }
                self.sheet.close_suggestions();
                EventOutcome::consumed()
            }
            Intent::ForgetIdentity => {
                self.state.invite_recipient_key = None;
                self.state.invite_recipient_path = InviteRecipientPath::New;
                self.quiet();
                EventOutcome::consumed()
            }
            Intent::Join => self.submit_connect(),
            Intent::Cancel => {
                self.dismiss();
                EventOutcome::consumed()
            }
            Intent::CopyPairLink => {
                if let Some(link) = self.state.pairing_link.as_deref() {
                    // The link makes whoever opens it a device of the person
                    // once approved: it stays out of clipboard history and
                    // sync, and leaves the clipboard after a while.
                    if ui::try_clipboard_copy_secret(link, INVITE_ON_CLIPBOARD_FOR).outcome()
                        == Some(true)
                    {
                        self.state.pairing_copied = true;
                    } else {
                        self.fail(t("peer-sync-copy-failed"));
                    }
                }
                EventOutcome::consumed()
            }
            Intent::CopyToken => {
                self.copy_invite_token();
                EventOutcome::consumed()
            }
            Intent::PasteToken => {
                self.paste_invite_token();
                EventOutcome::consumed()
            }
            Intent::Resend(id) => {
                EventOutcome::request(PeerSyncRequest::ResendInvitation(id), false)
            }
            Intent::CancelPending(id) => {
                EventOutcome::request(PeerSyncRequest::CancelInvitation(id), false)
            }
            Intent::CopyIdentity => EventOutcome::request(PeerSyncRequest::ExportIdentity, false),
        }
    }

    /// The one primary action: mint the invite as the sheet shows it. A
    /// person's invite is bound to their identity, so only they can redeem
    /// it; a link admits whoever opens it before it expires.
    fn submit_invite(&mut self) -> EventOutcome {
        if self.state.invite_name.is_none() {
            return EventOutcome::consumed();
        }
        if self.state.invite_recipient_path == InviteRecipientPath::Link && self.link_invites {
            return EventOutcome::request(
                PeerSyncRequest::IssueInvitation(InvitationTerms {
                    role: self.state.invite_role,
                    expires_in_seconds: INVITE_EXPIRES_IN_SECONDS,
                    invitee_name: None,
                    recipient_user_public_key: None,
                }),
                true,
            );
        }
        let name = self
            .state
            .invite_name
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        if name.is_empty() {
            self.fail(t("peer-sync-name-required"));
            return EventOutcome::consumed();
        }
        let Some(recipient_user_public_key) = self.state.invite_recipient_key else {
            self.fail(t("peer-sync-identity-required"));
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

    fn submit_connect(&self) -> EventOutcome {
        if self.enrollment.is_some() {
            return EventOutcome::consumed();
        }
        let Some(token) = self.state.connect_input.as_deref() else {
            return EventOutcome::consumed();
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
        if ui::try_clipboard_copy_secret(token, INVITE_ON_CLIPBOARD_FOR).outcome() == Some(true) {
            self.say(t("peer-sync-copied"));
        } else {
            self.fail(t("peer-sync-copy-failed"));
        }
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
                self.quiet();
            }
            None => self.fail(t("peer-sync-clipboard-empty")),
        }
    }

    fn dismiss(&mut self) {
        self.retire_enrollment();
        let context = std::mem::take(&mut self.state.invite_context);
        self.state = PeerSyncSnapshot {
            invite_context: context,
            ..PeerSyncSnapshot::default()
        };
        self.status_is_error = false;
        self.refresh();
    }

    /// Bring the sheet up to date with what the overlay holds.
    fn refresh(&mut self) {
        self.sheet.refresh(
            &self.state,
            &self.remembered_invitees,
            self.link_invites,
            self.status_is_error,
        );
    }

    /// The sheet's card as last laid out.
    #[must_use]
    pub fn card_rect(&self) -> Rect {
        self.sheet.card()
    }

    /// The sheet's named controls and where they were last laid out, for
    /// tests and `tilo-control`'s named targets.
    pub fn targets(&self) -> Vec<(String, Rect)> {
        self.controls()
            .into_iter()
            .map(|control| (control.id, control.rect))
            .collect()
    }

    /// The sheet's named controls in reading order: their names, what a
    /// screen reader calls them and where they were last laid out.
    pub fn controls(&self) -> Vec<SheetControl> {
        fn walk(widget: &dyn Widget, out: &mut Vec<SheetControl>) {
            if let Some(id) = widget.test_id()
                && id.starts_with(sheet::SHEET)
            {
                let node = widget.accessibility();
                out.push(SheetControl {
                    id: id.to_owned(),
                    label: node
                        .as_ref()
                        .and_then(|node| node.name.clone())
                        .unwrap_or_default(),
                    button: node.is_some_and(|node| node.role == ui::a11y::Role::Button),
                    rect: widget.get_rect(),
                });
            }
            for child in widget.children() {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        if let Some(dialog) = self.sheet.dialog() {
            walk(dialog, &mut out);
        }
        out
    }

    /// Focus the sheet's field, as the window does when it opens.
    #[cfg(test)]
    pub fn focus_field_for_tests(&mut self) {
        self.sheet.focus_field();
    }

    #[cfg(test)]
    pub fn set_snapshot_for_tests(&mut self, snapshot: PeerSyncSnapshot) {
        self.state = snapshot;
        self.refresh();
    }

    /// Show `snapshot`, for a screenshot scene.
    pub fn set_snapshot_for_shots(&mut self, snapshot: PeerSyncSnapshot) {
        self.state = snapshot;
        self.refresh();
    }

    /// Show `snapshot` with its status as an error, for a screenshot scene.
    pub fn set_failed_snapshot_for_shots(&mut self, snapshot: PeerSyncSnapshot) {
        self.state = snapshot;
        self.status_is_error = true;
        self.refresh();
    }
}

/// One of the sheet's named controls, as [`PeerSyncOverlay::controls`]
/// reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetControl {
    /// Its address (`peer-sync.primary`, `peer-sync-who`, …).
    pub id: String,
    /// What a screen reader calls it.
    pub label: String,
    pub button: bool,
    pub rect: Rect,
}

/// What a failed request says: the network being off in the person's
/// terms, else `key`'s message with what they can do.
fn failure(error: &str, key: &str) -> String {
    if error == NOT_RUNNING {
        t("peer-sync-offline")
    } else {
        t(key)
    }
}

/// The characters a key press types, if it types any.
fn typed(event: &WindowEvent) -> Option<&str> {
    match event {
        WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Character(text),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } => Some(text.as_str()),
        _ => None,
    }
}

fn pressed(event: &WindowEvent, key: NamedKey) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput {
            event: winit::event::KeyEvent {
                logical_key: winit::keyboard::Key::Named(named),
                state: winit::event::ElementState::Pressed,
                ..
            },
            ..
        } if *named == key
    )
}

/// What a request answers in a session with no network to run it.
pub(crate) const NOT_RUNNING: &str = "peer-sync is not running in this session";

/// The overlay is a retained widget the host mounts while the sheet is up:
/// its one child is the shared modal.
impl Widget for PeerSyncOverlay {
    /// A field typed into or a button pressed through the window's focus
    /// reaches its control directly, not through [`Widget::handle_event`]:
    /// each pass reads what the controls hold before the sheet shows the
    /// overlay's state again.
    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.after_input();
        if let Some(dialog) = self.sheet.dialog_mut() {
            dialog.sync(state, path);
        }
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let before = self.state.clone();
        self.after_input();
        let changed = self.state != before || self.requested.is_some();
        self.sheet
            .dialog_mut()
            .is_some_and(|dialog| dialog.update(state, path))
            || changed
    }

    /// The overlay's own box, the window the sheet stands in, holding the
    /// sheet's node.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let sheet = self.sheet.build_layout(taffy, theme, node_map);
        let style = if sheet.is_some() {
            taffy::Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..taffy::Style::default()
            }
        } else {
            // Closed: nothing to lay out.
            taffy::Style {
                display: taffy::Display::None,
                ..taffy::Style::default()
            }
        };
        let node = ui::widget::TaffyTreeExt::parent_node(taffy, style, sheet.as_slice());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let placed = ui::widget::layout_rect(taffy, node, origin);
        let Some(sheet) = taffy
            .children(node)
            .ok()
            .and_then(|nodes| nodes.first().copied())
        else {
            return;
        };
        self.sheet.apply_layout(taffy, sheet, placed.origin());
    }

    /// The sheet is drawn in the top layer, over everything.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.sheet
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.route(event, ctx);
    }

    fn needs_animation_frame(&self) -> bool {
        PeerSyncOverlay::needs_animation_frame(self)
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.sheet.take_structure_dirty()
    }

    fn take_focus_reset(&mut self) -> bool {
        self.sheet
            .dialog_mut()
            .is_some_and(Widget::take_focus_reset)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.sheet
            .dialog()
            .map(|dialog| vec![dialog as &dyn Widget])
            .unwrap_or_default()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.sheet
            .dialog_mut()
            .map(|dialog| vec![dialog as &mut dyn Widget])
            .unwrap_or_default()
    }

    fn get_rect(&self) -> Rect {
        self.sheet.card()
    }

    fn name(&self) -> &'static str {
        "PeerSyncOverlay"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// The pairing link as its chip shows it: the site, then an ellipsis for
/// the secret rest.
fn short_pairing_link(link: &str) -> String {
    url::Url::parse(link)
        .ok()
        .and_then(|url| url.host_str().map(|host| format!("{host}/…")))
        .unwrap_or_else(|| "…".to_owned())
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
/// An identity pasted into the Who field, anywhere among what was typed,
/// and the rest of what was typed, which is the person's name.
fn find_recipient_identity(typed: &str) -> Option<([u8; 32], String)> {
    let words: Vec<&str> = typed.split_whitespace().collect();
    let (at, key) = words
        .iter()
        .enumerate()
        .find_map(|(at, word)| parse_recipient_identity(word).map(|key| (at, key)))?;
    let rest = words
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != at)
        .map(|(_, word)| *word)
        .collect::<Vec<_>>()
        .join(" ");
    Some((key, rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::headless::FrameHarness;

    /// The overlay mounted in a window, as the shell mounts it.
    fn mounted(overlay: PeerSyncOverlay, theme: Theme) -> FrameHarness {
        let mut frame = FrameHarness::for_root(ui::State::new(), overlay, (1280, 800), theme);
        frame.recompose().expect("the sheet lays out");
        frame
    }

    fn overlay(frame: &FrameHarness) -> &PeerSyncOverlay {
        frame.root::<PeerSyncOverlay>()
    }

    /// Change the overlay as its host does between frames.
    fn with(frame: &mut FrameHarness, change: impl FnOnce(&mut PeerSyncOverlay)) {
        change(frame.root_mut::<PeerSyncOverlay>());
        frame.recompose().expect("the sheet lays out");
    }

    /// The request the person's input made, as the host takes it.
    fn request(frame: &mut FrameHarness) -> Option<(PeerSyncRequest, bool)> {
        let mut taken = None;
        with(frame, |overlay| taken = overlay.take_request());
        taken
    }

    fn find<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if widget.test_id() == Some(id) {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find(child, id))
    }

    fn named(frame: &FrameHarness, id: &str) -> Rect {
        overlay(frame)
            .targets()
            .into_iter()
            .find_map(|(name, rect)| (name == id).then_some(rect))
            .unwrap_or_else(|| panic!("no {id} in {:?}", overlay(frame).targets()))
    }

    fn type_into_who(frame: &mut FrameHarness, text: &str) {
        frame
            .click_named(sheet::WHO_FIELD)
            .expect("the Who field is there");
        frame.type_text(text);
    }

    /// The pairing sheet shows its link as the site only, animates only
    /// while it waits for a device, and stops once closed.
    #[test]
    fn the_pairing_sheet_shows_the_site_and_waits_while_its_link_is_out() {
        let shown = short_pairing_link("https://tilo.coffee/#/@eu1.tilo.coffee/w/x?pair=a1a1");
        assert_eq!(shown, "tilo.coffee/…");
        let mut pairing = PeerSyncOverlay::default();
        pairing.prompt_pair();
        let mut frame = mounted(pairing, Theme::dark());
        assert!(
            frame.settle_for_capture(ui::time::Duration::from_secs(5)),
            "nothing to wait for yet"
        );
        with(&mut frame, |overlay| {
            overlay.show_pairing("https://tilo.coffee/#/w/x?pair=a1".into());
        });
        assert!(
            overlay(&frame).needs_animation_frame(),
            "waiting for the new device"
        );
        with(&mut frame, |overlay| {
            overlay.pairing_done("Safari on iPhone");
        });
        assert!(
            frame.settle_for_capture(ui::time::Duration::from_secs(5)),
            "the device paired"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn named_invites_bind_role_and_verified_identity() {
        fn named_invite_refuses_blank_and_preserves_name_after_transport_failure() {
            let mut invite = PeerSyncOverlay::default();
            invite.prompt_invite();
            let mut frame = mounted(invite, Theme::dark());
            frame.click_named(sheet::WHO_FIELD).unwrap();
            frame.key("Enter");
            assert!(request(&mut frame).is_none());
            assert_eq!(
                overlay(&frame).state.status.as_deref(),
                Some("Say who this invite is for")
            );
            frame.type_text("Ada");
            frame.key("Enter");
            assert!(
                request(&mut frame).is_none(),
                "an invite for a person is bound to their identity"
            );
            with(&mut frame, |overlay| {
                overlay.state.invite_recipient_key = Some([0x11; 32]);
            });
            frame.click_named(sheet::WHO_FIELD).unwrap();
            frame.key("Enter");
            let (submit, copy) = request(&mut frame).expect("Enter creates the invite");
            assert_eq!(
                submit,
                PeerSyncRequest::IssueInviteFor {
                    name: "Ada".into(),
                    role: crate::network::invite::InviteRole::Editor,
                    recipient_user_public_key: [0x11; 32],
                }
            );
            with(&mut frame, |overlay| {
                overlay.finish_request(&submit, Err("offline".into()), copy);
            });
            assert_eq!(overlay(&frame).state.invite_name.as_deref(), Some("Ada"));
        }

        fn explicit_role_choice_reaches_the_recipient_bound_request() {
            let mut invite = PeerSyncOverlay::default();
            invite.prompt_invite();
            let mut frame = mounted(invite, Theme::dark());
            type_into_who(&mut frame, "Lin");
            with(&mut frame, |overlay| {
                overlay.state.invite_recipient_key = Some([0x22; 32]);
            });
            frame.click_named("peer-sync-access").unwrap();
            // The control's own keys move the choice: Viewer, its first.
            frame.key("Home");
            assert!(request(&mut frame).is_none());
            assert_eq!(
                overlay(&frame).state.invite_role,
                crate::network::invite::InviteRole::Viewer
            );
            frame
                .click_named(&format!("{}.primary", sheet::SHEET))
                .unwrap();
            assert_eq!(
                request(&mut frame).map(|(request, _)| request),
                Some(PeerSyncRequest::IssueInviteFor {
                    name: "Lin".into(),
                    role: crate::network::invite::InviteRole::Viewer,
                    recipient_user_public_key: [0x22; 32],
                })
            );
        }

        fn remembered_person_is_suggested_and_selects_their_verified_identity() {
            let mut invite = PeerSyncOverlay::default();
            invite.set_remembered_invitees(vec![
                RememberedInvitee {
                    name: "Ada Lovelace".into(),
                    user_public_key: [0x41; 32],
                    verification: "Verified yesterday".into(),
                },
                RememberedInvitee {
                    name: "Bo".into(),
                    user_public_key: [0x42; 32],
                    verification: "Verified in person".into(),
                },
            ]);
            invite.prompt_invite();
            let mut frame = mounted(invite, Theme::dark());
            type_into_who(&mut frame, "ad");
            assert_eq!(
                overlay(&frame).suggestions(),
                [0],
                "typing narrows the people"
            );
            // The field offers them; the arrow and Enter take one.
            frame.key("ArrowDown");
            frame.key("Enter");
            assert_eq!(
                overlay(&frame).snapshot().invite_name.as_deref(),
                Some("Ada Lovelace")
            );
            assert_eq!(
                overlay(&frame).snapshot().invite_recipient_path,
                InviteRecipientPath::Known
            );
            assert!(
                request(&mut frame).is_none(),
                "taking a person is not inviting"
            );
            frame
                .click_named(&format!("{}.primary", sheet::SHEET))
                .unwrap();
            assert_eq!(
                request(&mut frame).map(|(request, _)| request),
                Some(PeerSyncRequest::IssueInviteFor {
                    name: "Ada Lovelace".into(),
                    role: crate::network::invite::InviteRole::Editor,
                    recipient_user_public_key: [0x41; 32],
                })
            );
            // Editing the name lets go of the person it no longer names.
            frame.click_named(sheet::WHO_FIELD).unwrap();
            frame.key("End");
            frame.key("Backspace");
            assert_eq!(overlay(&frame).snapshot().invite_recipient_key, None);
            assert_eq!(
                overlay(&frame).snapshot().invite_recipient_path,
                InviteRecipientPath::New
            );
        }

        fn an_identity_pasted_into_who_binds_the_invite_and_leaves_the_name() {
            let mut invite = PeerSyncOverlay::default();
            invite.prompt_invite();
            let mut frame = mounted(invite, Theme::dark());
            let key = [0x5a; 32];
            frame.click_named(sheet::WHO_FIELD).unwrap();
            frame.commit_text(&format!("Lin tilo-identity:{}", hex::encode(key)));
            let snapshot = overlay(&frame).snapshot();
            assert_eq!(snapshot.invite_recipient_key, Some(key));
            assert_eq!(snapshot.invite_recipient_path, InviteRecipientPath::New);
            assert_eq!(snapshot.invite_name.as_deref(), Some("Lin"));
            assert_eq!(
                overlay(&frame).sheet.name.get(),
                "Lin",
                "the field keeps the name"
            );
            frame.click_named("peer-sync-forget-identity").unwrap();
            assert_eq!(overlay(&frame).snapshot().invite_recipient_key, None);
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
            overlay.activate(Intent::SelectKnown(0));
            assert_eq!(overlay.snapshot().invite_name.as_deref(), Some("Mina"));
            assert_eq!(overlay.snapshot().invite_recipient_key, Some([0x52; 32]));
        }

        fn pending_invites_are_named_and_emit_exact_resend_and_cancel_requests() {
            let mut invite = PeerSyncOverlay::default();
            invite.prompt_invite();
            let row = PendingInvitationSummary {
                id: [7; 32],
                name: "Ada".into(),
                role: crate::network::invite::InviteRole::Commenter,
                expiry_unix_seconds: u64::MAX,
            };
            invite.finish_typed_request(
                &PeerSyncRequest::ListPendingInvitations,
                Ok(PeerSyncResponse::PendingInvitations(vec![row.clone()])),
                false,
            );
            assert_eq!(invite.snapshot().pending_invitations, vec![row]);
            let mut frame = mounted(invite, Theme::dark());
            let id = hex::encode([7; 32]);
            // The row's action says what it does, unlike the sheet's own
            // footer, which only closes the sheet.
            let label = |frame: &FrameHarness, id: &str| {
                overlay(frame)
                    .controls()
                    .into_iter()
                    .find_map(|control| (control.id == id).then_some(control.label))
                    .unwrap_or_default()
            };
            assert_eq!(
                label(&frame, &format!("peer-sync-cancel-pending-{id}")),
                "Revoke"
            );
            assert_eq!(label(&frame, "peer-sync.primary"), "Create invite");
            assert_eq!(label(&frame, "peer-sync.secondary"), "Done");
            frame
                .click_named(&format!("peer-sync-resend-{id}"))
                .unwrap();
            assert_eq!(
                request(&mut frame).map(|(request, _)| request),
                Some(PeerSyncRequest::ResendInvitation([7; 32]))
            );
            frame
                .click_named(&format!("peer-sync-cancel-pending-{id}"))
                .unwrap();
            assert_eq!(
                request(&mut frame).map(|(request, _)| request),
                Some(PeerSyncRequest::CancelInvitation([7; 32]))
            );
        }

        named_invite_refuses_blank_and_preserves_name_after_transport_failure();
        explicit_role_choice_reaches_the_recipient_bound_request();
        remembered_person_is_suggested_and_selects_their_verified_identity();
        an_identity_pasted_into_who_binds_the_invite_and_leaves_the_name();
        closing_the_sheet_keeps_host_supplied_verified_people_for_next_invite();
        pending_invites_are_named_and_emit_exact_resend_and_cancel_requests();
    }

    /// A failure says what happened in the person's terms and what to do,
    /// shown as an error; the network's own words never reach the sheet.
    #[test]
    fn failures_are_said_in_peoples_terms() {
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_invite();
        let mint = PeerSyncRequest::IssueInviteFor {
            name: "Ada".into(),
            role: crate::network::invite::InviteRole::Editor,
            recipient_user_public_key: [1; 32],
        };
        overlay.finish_request(&mint, Err(NOT_RUNNING.into()), true);
        let said = overlay.snapshot().status.unwrap_or_default();
        assert!(
            said.starts_with("Invites need the network to be on"),
            "{said}"
        );
        assert!(overlay.status_is_error);
        overlay.finish_request(&mint, Err("relay quota exceeded (code 7)".into()), true);
        let said = overlay.snapshot().status.unwrap_or_default();
        assert!(
            !said.contains("quota") && !said.contains("peer-sync"),
            "{said}"
        );
        overlay.finish_typed_request(
            &PeerSyncRequest::ListPendingInvitations,
            Err("table locked".into()),
            false,
        );
        assert!(
            !overlay
                .snapshot()
                .status
                .unwrap_or_default()
                .contains("locked"),
            "a pending list that could not be read is not the person's problem"
        );
    }

    #[test]
    #[serial_test::serial(clipboard)]
    fn connect_and_identity_cards_are_trimmed_exact_and_non_secret() {
        fn connect_is_trimmed_and_only_closes_after_success() {
            let mut join = PeerSyncOverlay::default();
            join.prompt_connect();
            let mut frame = mounted(join, Theme::dark());
            frame.click_named(sheet::LINK_FIELD).unwrap();
            frame.type_text(" token ");
            frame.key("Enter");
            let (submit, _) = request(&mut frame).expect("Enter joins");
            assert_eq!(submit, PeerSyncRequest::AcceptInvite("token".into()));
            with(&mut frame, |overlay| {
                overlay.finish_request(&submit, Err("bad token".into()), false);
            });
            assert!(overlay(&frame).state.connect_input.is_some());
            with(&mut frame, |overlay| {
                overlay.finish_request(&submit, Ok("room-7".into()), false);
            });
            assert!(overlay(&frame).state.connect_input.is_none());
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
            assert_eq!(find_recipient_identity("Ada"), None, "a name is a name");
        }

        fn exporting_identity_copies_a_non_secret_card_without_closing_join() {
            let mut join = PeerSyncOverlay::default();
            join.prompt_connect();
            let mut frame = mounted(join, Theme::dark());
            frame.click_named("peer-sync-copy-identity").unwrap();
            assert_eq!(
                request(&mut frame).map(|(request, _)| request),
                Some(PeerSyncRequest::ExportIdentity)
            );
            let card = format!("tilo-identity:{}", hex::encode([0x2a; 32]));
            with(&mut frame, |overlay| {
                overlay.finish_request(&PeerSyncRequest::ExportIdentity, Ok(card.clone()), false);
            });
            assert_eq!(ui::text_selection::clipboard_paste(), Some(card));
            assert!(overlay(&frame).state.connect_input.is_some());
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

    #[test]
    fn the_invite_sheet_is_built_from_the_shared_sheet_field_segmented_control_and_buttons() {
        let mut invite = PeerSyncOverlay::default();
        invite.prompt_invite();
        let frame = mounted(invite, Theme::dark());
        let dialog = overlay(&frame).sheet.dialog().expect("the sheet is up");
        assert!(
            dialog.as_any().is_some_and(
                <dyn std::any::Any>::is::<ui::widget::ModalDialog<ui::widget::Column>>
            ),
            "the shared modal sheet"
        );
        let who = find(dialog, sheet::WHO_FIELD).expect("a Who field");
        assert!(
            who.as_any()
                .is_some_and(<dyn std::any::Any>::is::<ui::widget::Autocomplete>),
            "Who is the shared field with type-ahead"
        );
        let access = find(dialog, "peer-sync-access").expect("an Access control");
        assert_eq!(
            access.name(),
            "ToggleGroup",
            "Access is the shared segmented control"
        );
        for footer in ["primary", "secondary"] {
            let id = format!("{}.{footer}", sheet::SHEET);
            assert!(
                named(&frame, &id).area() > 0.0,
                "the sheet's own {footer} button"
            );
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

    #[test]
    fn the_invite_sheet_is_who_then_access_then_one_primary_invite() {
        let mut invite = PeerSyncOverlay::default();
        invite.prompt_invite();
        let mut frame = mounted(invite, Theme::dark());
        let names: Vec<String> = overlay(&frame)
            .targets()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let first = |name: &str| names.iter().position(|n| n == name).unwrap_or(usize::MAX);
        let primary = format!("{}.primary", sheet::SHEET);
        assert!(
            first(sheet::WHO_FIELD) < first("peer-sync-access")
                && first("peer-sync-access") < first(&primary),
            "who, then access, then Invite: {names:?}"
        );
        let who = named(&frame, sheet::WHO_FIELD);
        let access = named(&frame, "peer-sync-access");
        let create = named(&frame, &primary);
        assert!(
            who.y1 <= access.y0 && access.y1 <= create.y0,
            "who stands above access, above the button"
        );
        assert_eq!(
            names.iter().filter(|n| **n == primary).count(),
            1,
            "one Invite"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.contains("review") || n.contains("back")),
            "no second step: {names:?}"
        );
        // Access is one segmented control: three segments of one row,
        // touching, all the same height.
        let dialog = overlay(&frame).sheet.dialog().expect("the sheet is up");
        let control = find(dialog, "peer-sync-access").expect("Access");
        let roles: Vec<Rect> = control
            .accessibility()
            .map(|node| node.children)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|segment| segment.bounds.map(ui::a11y::AccessBounds::to_rect))
            .collect();
        assert_eq!(roles.len(), 3, "{roles:?}");
        for pair in roles.windows(2) {
            assert!(
                (pair[0].x1 - pair[1].x0).abs() < 0.5,
                "segments touch: {roles:?}"
            );
            assert!(
                (pair[0].y0 - pair[1].y0).abs() < 0.5
                    && (pair[0].height() - pair[1].height()).abs() < 0.5
            );
        }
        // The Invite button is the primary one, and the only one the sheet
        // marks so: Enter presses it.
        let focused = frame.tab_through();
        assert!(
            focused
                .iter()
                .any(|(_, name)| name.as_deref() == Some("Create invite")),
            "{focused:?}"
        );

        // Links for anyone are not offered until the product decides to:
        // no Who choice, and choosing a link does nothing.
        assert!(
            !names.iter().any(|n| n == "peer-sync-audience"),
            "{names:?}"
        );
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_invite();
        overlay.activate(Intent::SelectWho(true));
        assert!(overlay.snapshot().invite_recipient_path.is_person());

        // Offered, a link for anyone asks no name or identity, and one
        // Invite mints it.
        overlay.set_link_invites_enabled(true);
        overlay.activate(Intent::SelectWho(true));
        let mint = overlay.activate(Intent::CreateInvite);
        assert_eq!(
            mint.request,
            Some(PeerSyncRequest::IssueInvitation(InvitationTerms {
                role: crate::network::invite::InviteRole::Editor,
                expires_in_seconds: INVITE_EXPIRES_IN_SECONDS,
                invitee_name: None,
                recipient_user_public_key: None,
            }))
        );
        overlay.finish_typed_request(
            mint.request.as_ref().unwrap(),
            Ok(PeerSyncResponse::Message("tilo-invite".into())),
            false,
        );
        assert_eq!(
            overlay.snapshot().invite_token.as_deref(),
            Some("tilo-invite")
        );
    }

    #[test]
    fn the_name_placeholder_and_caret_share_one_line_centred_in_the_field() {
        let theme = Theme::dark();
        let mut invite = PeerSyncOverlay::default();
        invite.prompt_invite();
        let frame = mounted(invite, theme.clone());
        let dialog = overlay(&frame).sheet.dialog().expect("the sheet is up");
        let who = find(dialog, sheet::WHO_FIELD)
            .and_then(|who| who.as_any())
            .and_then(|any| any.downcast_ref::<ui::widget::Autocomplete>())
            .expect("the Who field");
        let field = who.input().get_rect();
        let caret = who.input().caret_rect(&theme, 0);
        assert!(caret.height() > 0.0, "{caret:?}");
        assert!(
            (caret.center().y - field.center().y).abs() < 1.0,
            "the text's line is centred in the field, so the caret drawn over it and the placeholder stand level: {caret:?} in {field:?}"
        );
        assert!(caret.y0 > field.y0 && caret.y1 < field.y1);
    }
}
