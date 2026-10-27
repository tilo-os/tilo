//! The invite, join and pairing sheet, made of the shared widgets.
//!
//! One modal ([`ModalDialog`]: the floating sheet over a scrim, with its
//! title, its footer buttons, Escape and Enter) holds a column of shared
//! controls: the Who field ([`Autocomplete`], a real [`TextInput`] with
//! type-ahead over the people this computer knows), Access as a segmented
//! [`ToggleGroup`], pending invites as [`ListRow`]s in a [`Section`], and
//! errors as a [`Banner`]. Text input, focus, input methods, selection,
//! keyboard, touch and accessibility are theirs. The sheet only says what
//! each control shows and reports what the person did ([`Presses`]); the
//! overlay decides what that means.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::LocalState;
use ui::a11y::Politeness;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    Autocomplete, AutocompleteOutcome, Banner, BannerNotice, Button, ButtonRole, Column,
    ControlSize, Element, ListRow, ModalDialog, QrWidget, Row, Section, Segment, Slot, SlotHandle,
    Spinner, Suggestion, Text, TextInput, ToggleGroup, When, Widget, labeled_field,
};

use super::{
    InviteRecipientPath, PeerSyncSnapshot, PendingInvitationSummary, RememberedInvitee,
    elide_token, identity_fingerprint, parse_invite, peer_fingerprint, role_and_expiry, role_label,
    short_pairing_link,
};
use crate::network::invite::InviteRole;
use crate::shell::i18n::{t, t_args};

/// The Who field's address, for focus, tests and `tilo-control`.
pub(super) const WHO_FIELD: &str = "peer-sync-who";
/// The join field's address.
pub(super) const LINK_FIELD: &str = "peer-sync-link";
/// The modal's address; its footer buttons are `<this>.primary` and
/// `<this>.secondary`.
pub(super) const SHEET: &str = "peer-sync";

/// The role Access offers until the person picks another: Editor, its
/// last segment.
const DEFAULT_ROLE: usize = 2;

/// The roles Access offers, in the order its segments stand.
pub(super) const ROLES: [InviteRole; 3] = [
    InviteRole::Viewer,
    InviteRole::Commenter,
    InviteRole::Editor,
];

/// Which of its four faces the sheet shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// Who, their access, and Create invite.
    Invite,
    /// The invite just made, ready to copy.
    Created,
    /// A link or site to join.
    Join,
    /// "Pair a new device".
    Pairing,
}

impl Mode {
    /// The face `state` asks for, or `None` while the sheet is closed.
    pub(super) fn of(state: &PeerSyncSnapshot) -> Option<Self> {
        if state.pairing_open {
            Some(Self::Pairing)
        } else if state.invite_name.is_some() {
            Some(Self::Invite)
        } else if state.invite_token.is_some() {
            Some(Self::Created)
        } else if state.connect_input.is_some() {
            Some(Self::Join)
        } else {
            None
        }
    }
}

/// What the person pressed since the overlay last looked. Each control sets
/// its flag; the overlay takes it once.
#[derive(Clone)]
pub(super) struct Presses {
    pub(super) create: LocalState<bool>,
    pub(super) cancel: LocalState<bool>,
    pub(super) join: LocalState<bool>,
    pub(super) paste_link: LocalState<bool>,
    pub(super) copy_identity: LocalState<bool>,
    pub(super) copy_invite: LocalState<bool>,
    pub(super) copy_pair_link: LocalState<bool>,
    pub(super) forget_identity: LocalState<bool>,
    pub(super) resend: LocalState<Option<[u8; 32]>>,
    pub(super) cancel_pending: LocalState<Option<[u8; 32]>>,
}

impl Default for Presses {
    fn default() -> Self {
        Self {
            create: LocalState::new(false),
            cancel: LocalState::new(false),
            join: LocalState::new(false),
            paste_link: LocalState::new(false),
            copy_identity: LocalState::new(false),
            copy_invite: LocalState::new(false),
            copy_pair_link: LocalState::new(false),
            forget_identity: LocalState::new(false),
            resend: LocalState::new(None),
            cancel_pending: LocalState::new(None),
        }
    }
}

/// Read a press once.
pub(super) fn take(flag: &LocalState<bool>) -> bool {
    let pressed = flag.get();
    if pressed {
        flag.set(false);
    }
    pressed
}

/// Read a row's press once.
pub(super) fn take_id(flag: &LocalState<Option<[u8; 32]>>) -> Option<[u8; 32]> {
    let pressed = flag.get();
    if pressed.is_some() {
        flag.set(None);
    }
    pressed
}

/// Whose identity the invite is bound to, as the row under the Who field
/// shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Recipient {
    /// Nobody yet: how to name someone.
    Unbound,
    /// Someone this computer knows, verified before.
    Known {
        name: String,
        key: [u8; 32],
        verification: String,
    },
    /// An identity the person pasted.
    Pasted { key: [u8; 32] },
}

impl Recipient {
    fn of(state: &PeerSyncSnapshot, people: &[RememberedInvitee]) -> Self {
        match (state.invite_recipient_path, state.invite_recipient_key) {
            (InviteRecipientPath::Known, Some(key)) => people
                .iter()
                .find(|person| person.user_public_key == key)
                .map_or(Self::Pasted { key }, |person| Self::Known {
                    name: person.name.clone(),
                    key,
                    verification: person.verification.clone(),
                }),
            (_, Some(key)) => Self::Pasted { key },
            (_, None) => Self::Unbound,
        }
    }
}

/// What the join field's text is, as the line under it says.
#[derive(Clone, Debug, PartialEq, Eq)]
enum JoinDetail {
    /// Nothing typed: what to type.
    Hint,
    /// An invitation: what it gives and the code it carries.
    Invite { access: String, code: String },
    /// A site or a link the sheet understands.
    Understood(String),
    /// Something that is not a site or a link yet: how it begins and ends,
    /// and how long it is.
    Incomplete(String),
}

impl JoinDetail {
    fn of(typed: &str) -> Self {
        if typed.trim().is_empty() {
            return Self::Hint;
        }
        if let Some(invite) = parse_invite(typed) {
            return Self::Invite {
                access: role_and_expiry(
                    "peer-sync-received-role-expires",
                    invite.role,
                    invite.expiry_unix_seconds,
                ),
                code: peer_fingerprint(&invite.owner_peer_id),
            };
        }
        crate::shell::network::join_field::describe(typed).map_or_else(
            || {
                Self::Incomplete(t_args(
                    "peer-sync-token-characters",
                    &[
                        ("token", elide_token(typed).into()),
                        ("count", typed.chars().count().into()),
                    ],
                ))
            },
            Self::Understood,
        )
    }
}

/// What a face without a field shows, so it is rebuilt only when that
/// changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StaticFace {
    title: String,
    token: Option<String>,
    invite_for: Option<String>,
    pairing_link: Option<String>,
    pairing_copied: bool,
    /// What "Pair a new device" says (making the link, waiting, done or
    /// why it failed); the other faces say it in their status line.
    pairing_status: Option<String>,
}

/// The open sheet's controls and the state they are bound to.
pub(super) struct Sheet {
    pub(super) presses: Presses,
    pub(super) name: LocalState<String>,
    /// The join field's text, bound both ways.
    pub(super) link: LocalState<String>,
    /// The chosen Access segment, an index into [`ROLES`].
    pub(super) role: LocalState<usize>,
    /// Who the invite is for, when links for anyone are offered: a person
    /// (0) or anyone with the link (1).
    pub(super) audience: LocalState<usize>,
    status: LocalState<String>,
    has_status: LocalState<bool>,
    error: LocalState<Option<BannerNotice>>,
    names_person: LocalState<bool>,
    recipient: Option<(SlotHandle, Recipient)>,
    pending: Option<(SlotHandle, Vec<PendingInvitationSummary>)>,
    join_detail: Option<(SlotHandle, JoinDetail)>,
    /// The suggestions the Who field offers, as the person keys they name.
    offered: Vec<[u8; 32]>,
    /// The suggestion highlighted when the event being routed arrived: a
    /// click or Enter accepts it.
    highlighted: Option<[u8; 32]>,
    dialog: Option<ModalDialog<Column>>,
    mode: Option<Mode>,
    face: Option<StaticFace>,
    structure_dirty: bool,
}

impl Default for Sheet {
    fn default() -> Self {
        Self {
            presses: Presses::default(),
            name: LocalState::new(String::new()),
            link: LocalState::new(String::new()),
            role: LocalState::new(DEFAULT_ROLE),
            audience: LocalState::new(0),
            status: LocalState::new(String::new()),
            has_status: LocalState::new(false),
            error: LocalState::new(None),
            names_person: LocalState::new(true),
            recipient: None,
            pending: None,
            join_detail: None,
            offered: Vec::new(),
            highlighted: None,
            dialog: None,
            mode: None,
            face: None,
            structure_dirty: false,
        }
    }
}

impl Sheet {
    /// Build the sheet afresh on the next refresh: what it offers changed.
    pub(super) fn rebuild(&mut self) {
        self.mode = None;
        self.face = None;
    }

    /// Show what `state` holds: a new face when the mode changed, else the
    /// changed parts in place, so the field keeps its caret, selection and
    /// any text an input method is composing.
    pub(super) fn refresh(
        &mut self,
        state: &PeerSyncSnapshot,
        people: &[RememberedInvitee],
        link_invites: bool,
        error: bool,
    ) {
        let mode = Mode::of(state);
        let face = matches!(mode, Some(Mode::Created | Mode::Pairing)).then(|| StaticFace {
            title: title(state, mode),
            token: state.invite_token.clone(),
            invite_for: state.invite_for.clone(),
            pairing_link: state.pairing_link.clone(),
            pairing_copied: state.pairing_copied,
            pairing_status: state.status.clone().filter(|_| state.pairing_open),
        });
        if mode != self.mode || face != self.face {
            let first = self.dialog.is_none();
            self.mode = mode;
            self.face = face;
            self.recipient = None;
            self.pending = None;
            self.join_detail = None;
            self.structure_dirty = true;
            self.name.set(state.invite_name.clone().unwrap_or_default());
            self.link
                .set(state.connect_input.clone().unwrap_or_default());
            self.dialog = mode.map(|mode| {
                let dialog = self.build(mode, state, link_invites);
                if first {
                    // A sheet that opens arrives (`motion::Appear`).
                    let mut dialog = dialog;
                    dialog.set_open(true);
                    dialog
                } else {
                    // A sheet that changes face is already there.
                    dialog.open(true)
                }
            });
        }
        self.show_status(state, error);
        match mode {
            Some(Mode::Invite) => self.show_invite(state, people, link_invites),
            Some(Mode::Join) => self.show_join(state),
            Some(Mode::Created | Mode::Pairing) | None => {}
        }
    }

    fn show_status(&self, state: &PeerSyncSnapshot, error: bool) {
        let status = state.status.clone().filter(|_| !state.pairing_open);
        let notice = status
            .as_ref()
            .filter(|_| error)
            .map(|status| BannerNotice::error(status.clone()));
        if self.error.get() != notice {
            self.error.set(notice);
        }
        let line = status.filter(|_| !error).unwrap_or_default();
        if self.status.get() != line {
            self.has_status.set(!line.is_empty());
            self.status.set(line);
        }
    }

    fn show_invite(
        &mut self,
        state: &PeerSyncSnapshot,
        people: &[RememberedInvitee],
        link_invites: bool,
    ) {
        let typed = state.invite_name.clone().unwrap_or_default();
        if self.name.get() != typed {
            // Into the field itself, which publishes it to `name`: a
            // binding is only read on the next document pass.
            if let Some(field) = self.who_field() {
                field.set_text(&typed);
            }
            self.name.set(typed);
        }
        let role = ROLES
            .iter()
            .position(|role| *role == state.invite_role)
            .unwrap_or(DEFAULT_ROLE);
        if self.role.get() != role {
            self.role.set(role);
        }
        let names_person = state.invite_recipient_path.is_person() || !link_invites;
        if self.names_person.get() != names_person {
            self.names_person.set(names_person);
        }
        let audience = usize::from(!names_person);
        if self.audience.get() != audience {
            self.audience.set(audience);
        }
        let recipient = Recipient::of(state, people);
        if let Some((slot, shown)) = self.recipient.as_mut()
            && *shown != recipient
        {
            slot.set(recipient_row(&recipient, &self.presses));
            *shown = recipient;
        }
        let pending = state.pending_invitations.clone();
        if let Some((slot, shown)) = self.pending.as_mut()
            && *shown != pending
        {
            slot.set(pending_section(&pending, &self.presses));
            *shown = pending;
        }
    }

    fn show_join(&mut self, state: &PeerSyncSnapshot) {
        let typed = state.connect_input.clone().unwrap_or_default();
        if self.link.get() != typed {
            if let Some(dialog) = self.dialog.as_mut()
                && let Some(field) = find_mut::<TextInput>(dialog.content_mut(), LINK_FIELD)
            {
                field.set_text(&typed);
            }
            self.link.set(typed.clone());
        }
        let detail = JoinDetail::of(&typed);
        if let Some((slot, shown)) = self.join_detail.as_mut()
            && *shown != detail
        {
            slot.set(join_detail(&detail));
            *shown = detail;
        }
    }

    /// Offer `people` under the Who field, as `typed` narrows them.
    pub(super) fn offer(&mut self, people: &[(String, [u8; 32], String)]) {
        self.offered = people.iter().map(|(_, key, _)| *key).collect();
        let suggestions = people
            .iter()
            .map(|(name, _, verification)| {
                Suggestion::labelled(name.clone(), name.clone())
                    .with_detail(verification.clone())
                    .with_icon(ui::icons::Icon::User)
            })
            .collect();
        if let Some(field) = self.who_field() {
            if people.is_empty() {
                field.close();
            } else {
                field.set_suggestions_open(suggestions);
            }
        }
    }

    pub(super) fn close_suggestions(&mut self) {
        if let Some(field) = self.who_field() {
            field.close();
        }
    }

    /// What the Who field holds now, as the person left it.
    pub(super) fn who_text(&mut self) -> String {
        match self.who_field() {
            Some(field) => field.input().value().to_owned(),
            None => self.name.get(),
        }
    }

    /// What the join field holds now, as the person left it.
    pub(super) fn link_text(&mut self) -> String {
        let field = self
            .dialog
            .as_mut()
            .and_then(|dialog| find_mut::<TextInput>(dialog.content_mut(), LINK_FIELD));
        match field {
            Some(field) => field.value().to_owned(),
            None => self.link.get(),
        }
    }

    /// The person a suggestion the field just accepted names.
    pub(super) fn take_accepted(&mut self) -> Option<[u8; 32]> {
        let highlighted = self.highlighted.take();
        let field = self.who_field()?;
        match field.take_outcome() {
            AutocompleteOutcome::Accepted(_) => highlighted,
            AutocompleteOutcome::None | AutocompleteOutcome::Cancelled => None,
        }
    }

    fn who_field(&mut self) -> Option<&mut Autocomplete> {
        let dialog = self.dialog.as_mut()?;
        find_mut::<Autocomplete>(dialog.content_mut(), WHO_FIELD)
    }

    /// Note which suggestion is highlighted before an event is routed: the
    /// event may accept it, and the field then reports only its text.
    fn note_highlight(&mut self) {
        let offered = self.offered.clone();
        self.highlighted = self.who_field().and_then(|field| {
            let label = field.highlighted()?.label.clone();
            let index = field.suggestions().iter().position(|s| s.label == label)?;
            offered.get(index).copied()
        });
    }

    fn build(
        &mut self,
        mode: Mode,
        state: &PeerSyncSnapshot,
        link_invites: bool,
    ) -> ModalDialog<Column> {
        let title = title(state, Some(mode));
        let presses = self.presses.clone();
        let (content, primary, secondary) = match mode {
            Mode::Invite => (
                self.invite_body(link_invites),
                Some((t("peer-sync-create-invite"), presses.create.clone())),
                // Nothing is lost by leaving: the sheet closes, it does not
                // undo anything.
                Some((t("peer-sync-done"), presses.cancel.clone())),
            ),
            Mode::Created => (
                self.created_body(state),
                Some((t("peer-sync-copy-invite"), presses.copy_invite.clone())),
                Some((t("peer-sync-done"), presses.cancel.clone())),
            ),
            Mode::Join => (
                self.join_body(),
                Some((t("peer-sync-join"), presses.join.clone())),
                Some((t("peer-sync-cancel"), presses.cancel.clone())),
            ),
            Mode::Pairing => (
                self.pairing_body(state),
                Some((t("pairing-done-button"), presses.cancel.clone())),
                None,
            ),
        };
        let dismissed = presses.cancel;
        let mut dialog = ModalDialog::new(title, content)
            .with_test_id(SHEET)
            .on_dismiss(move || dismissed.set(true));
        if let Some((label, flag)) = primary {
            dialog = dialog.primary_action(label, move || flag.set(true));
        }
        if let Some((label, flag)) = secondary {
            dialog = dialog.secondary_action(label, move || flag.set(true));
        }
        dialog
    }

    /// Who, their access, the invites still pending, and what went wrong.
    fn invite_body(&mut self, link_invites: bool) -> Column {
        let who = Autocomplete::with_input(TextInput::from_state(&self.name))
            .with_id(WHO_FIELD)
            .with_placeholder(&t("peer-sync-who-placeholder"))
            .with_max_visible(4);
        let (recipient, recipient_slot) =
            Slot::new(recipient_row(&Recipient::Unbound, &self.presses));
        self.recipient = Some((recipient_slot, Recipient::Unbound));
        let (pending, pending_slot) = Slot::empty();
        self.pending = Some((pending_slot, Vec::new()));
        let person = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(labeled_field(t("peer-sync-who-label"), who)))
            .push(Box::new(recipient));
        let mut body = column();
        if link_invites {
            body = body.push(Box::new(labeled_field(
                t("peer-sync-who-heading"),
                ToggleGroup::new(
                    vec![
                        Segment::new(t("peer-sync-who-person")),
                        Segment::new(t("peer-sync-who-link")),
                    ],
                    0,
                )
                .fill()
                .with_id("peer-sync-audience")
                .bind_selection(&self.audience),
            )));
        }
        body.push(Box::new(When::from_state(
            &self.names_person,
            |names| *names,
            Box::new(person),
        )))
        .push(Box::new(When::from_state(
            &self.names_person,
            |names| !*names,
            caption(t("peer-sync-link-note-anyone")),
        )))
        .push(Box::new(labeled_field(
            t("peer-sync-role-heading"),
            ToggleGroup::new(
                ROLES
                    .iter()
                    .map(|role| Segment::new(role_label(*role)))
                    .collect(),
                DEFAULT_ROLE,
            )
            .fill()
            .with_id("peer-sync-access")
            .bind_selection(&self.role),
        )))
        .push(Box::new(Banner::from_state(&self.error)))
        .push(self.status_line())
        .push(Box::new(pending))
    }

    /// The invite just made: who it is for, what it gives, and the code
    /// they will see.
    fn created_body(&self, state: &PeerSyncSnapshot) -> Column {
        let parsed = state.invite_token.as_deref().and_then(parse_invite);
        let anyone = t("peer-sync-invite-anyone");
        let who = state.invite_for.clone().unwrap_or(anyone);
        let mut row = ListRow::new("peer-sync-created-for", who.clone());
        if let Some(invite) = parsed.as_ref() {
            row = row.secondary(role_and_expiry(
                "peer-sync-role-expires",
                invite.role,
                invite.expiry_unix_seconds,
            ));
        }
        row = if state.invite_for.is_some() {
            row.avatar(who.clone(), who)
        } else {
            row.icon(ui::icons::Icon::Link)
        };
        let mut body = column().push(Box::new(row));
        if let Some(invite) = parsed {
            body = body.push(Box::new(
                ListRow::new(
                    "peer-sync-created-code",
                    peer_fingerprint(&invite.owner_peer_id),
                )
                .icon(ui::icons::Icon::Shield)
                .secondary(t("peer-sync-code-hint")),
            ));
        }
        body.push(Box::new(Banner::from_state(&self.error)))
            .push(self.status_line())
    }

    /// The link or site to join, what it is, and the two ways to fill it.
    fn join_body(&mut self) -> Column {
        let field = TextInput::from_state(&self.link)
            .with_id(LINK_FIELD)
            .with_placeholder(t("peer-sync-paste-invite-hint"));
        let (detail, detail_slot) = Slot::new(join_detail(&JoinDetail::Hint));
        self.join_detail = Some((detail_slot, JoinDetail::Hint));
        let paste = self.presses.paste_link.clone();
        let copy = self.presses.copy_identity.clone();
        column()
            .push(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .with_no_padding()
                    .with_rhythm(Rhythm::Related)
                    .push(Box::new(labeled_field(
                        t("peer-sync-join-field-label"),
                        field,
                    )))
                    .push(Box::new(detail)),
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new("peer-sync-paste-link", &t("peer-sync-paste-link"))
                            .with_icon(ui::icons::Icon::ClipboardPaste)
                            .on_click(move || paste.set(true)),
                    ))
                    .push(Box::new(
                        Button::new("peer-sync-copy-identity", &t("peer-sync-copy-identity"))
                            .with_icon(ui::icons::Icon::Copy)
                            .on_click(move || copy.set(true)),
                    )),
            ))
            .push(Box::new(Banner::from_state(&self.error)))
            .push(self.status_line())
    }

    /// "Pair a new device": the link as a code a phone scans, a button that
    /// copies it, the three ways a new device uses it, why it is safe, and
    /// the sheet waiting for the device.
    fn pairing_body(&self, state: &PeerSyncSnapshot) -> Column {
        let status = state.status.clone().unwrap_or_else(|| t("pairing-waiting"));
        let Some(link) = state.pairing_link.as_deref() else {
            // Making the link, a failure, or the device paired: one line.
            return column().push(Box::new(Text::new(&status).with_live(Politeness::Polite)));
        };
        let copy = self.presses.copy_pair_link.clone();
        let (label, icon) = if state.pairing_copied {
            (t("pairing-copied"), ui::icons::Icon::Check)
        } else {
            (t("pairing-copy-link"), ui::icons::Icon::Copy)
        };
        let ways = PAIRING_WAYS.iter().map(|(icon, kind, how)| {
            Box::new(ListRow::new(*kind, t(kind)).icon(*icon).secondary(t(how))) as Element
        });
        column()
            .push(Box::new(
                Row::new()
                    .with_justify(taffy::JustifyContent::Center)
                    .push(Box::new(
                        QrWidget::new(link.as_bytes().to_vec())
                            .with_secret()
                            .with_id("peer-sync-pair-code")
                            .with_alt(t("pairing-qr-alt")),
                    )),
            ))
            .push(Box::new(
                ListRow::new("peer-sync-pair-link", short_pairing_link(link))
                    .icon(ui::icons::Icon::Link)
                    .trailing(Box::new(
                        Button::new("peer-sync-copy-pair-link", &label)
                            .with_icon(icon)
                            .on_click(move || copy.set(true)),
                    )),
            ))
            .push(Box::new(Section::new(ways.collect())))
            .push(caption(t("pairing-safe")))
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(Spinner::for_text(TextRole::Caption)))
                    .push(Box::new(
                        Text::new(&status)
                            .with_role(TextRole::Caption)
                            .with_live(Politeness::Polite)
                            .dimmed(),
                    )),
            ))
    }

    /// What the sheet says that is not an error ("Copied", "Working…").
    fn status_line(&self) -> Element {
        Box::new(When::from_state(
            &self.has_status,
            |shown| *shown,
            Box::new(
                Text::from_state(&self.status, Clone::clone)
                    .with_id("peer-sync-status")
                    .with_role(TextRole::Caption)
                    .with_live(Politeness::Polite)
                    .dimmed(),
            ),
        ))
    }

    /// Focus the sheet's field, as the window does when the sheet opens.
    #[cfg(test)]
    pub(super) fn focus_field(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        for id in [WHO_FIELD, LINK_FIELD] {
            if let Some(field) = find_mut::<Autocomplete>(dialog.content_mut(), id) {
                field.set_focus(true);
            }
            if let Some(field) = find_mut::<TextInput>(dialog.content_mut(), id) {
                field.set_focus(true);
            }
        }
    }

    /// Call before routing `event` to the sheet.
    pub(super) fn before_event(&mut self) {
        self.note_highlight();
    }

    /// The open modal, for the overlay's widget passes.
    pub(super) fn dialog(&self) -> Option<&ModalDialog<Column>> {
        self.dialog.as_ref()
    }

    pub(super) fn dialog_mut(&mut self) -> Option<&mut ModalDialog<Column>> {
        self.dialog.as_mut()
    }

    pub(super) fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let d = self
            .dialog
            .as_mut()
            .is_some_and(Widget::take_structure_dirty);
        own | d
    }

    pub(super) fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Option<NodeId> {
        self.dialog
            .as_mut()
            .map(|dialog| dialog.build_layout(taffy, theme, node_map))
    }

    pub(super) fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.apply_layout(taffy, node, origin);
        }
    }

    pub(super) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.draw_overlay(scene, transform, text_renderer, theme, presence);
            dialog.mark_drawn();
        }
    }

    pub(super) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.handle_event(event, ctx);
        }
    }

    /// The sheet's card as last laid out.
    pub(super) fn card(&self) -> Rect {
        self.dialog
            .as_ref()
            .map_or(Rect::ZERO, ModalDialog::dialog_rect)
    }
}

fn title(state: &PeerSyncSnapshot, mode: Option<Mode>) -> String {
    match mode {
        Some(Mode::Pairing) => t("pairing-title"),
        Some(Mode::Invite) if !state.invite_context.workspace_name.is_empty() => t_args(
            "peer-sync-title-invite-to",
            &[(
                "workspace",
                state.invite_context.workspace_name.clone().into(),
            )],
        ),
        Some(Mode::Invite) => t("peer-sync-title-invite"),
        Some(Mode::Join) => t("peer-sync-title-join"),
        Some(Mode::Created) | None => t("peer-sync-title-workspace-invite"),
    }
}

/// The sheet's body column: its groups a section apart.
fn column() -> Column {
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Section)
}

/// One line of secondary text that wraps.
fn caption(text: impl AsRef<str>) -> Element {
    Box::new(
        Text::new(text.as_ref())
            .with_role(TextRole::Caption)
            .dimmed(),
    )
}

/// Under the Who field: whom the invite is bound to, or how to name them.
fn recipient_row(recipient: &Recipient, presses: &Presses) -> Element {
    match recipient {
        Recipient::Unbound => caption(t("peer-sync-recipient-hint")),
        Recipient::Known {
            name,
            key,
            verification,
        } => Box::new(
            ListRow::new("peer-sync-recipient", name.clone())
                .avatar(name.clone(), hex::encode(key))
                .secondary(verification.clone()),
        ),
        Recipient::Pasted { key } => {
            let forget = presses.forget_identity.clone();
            Box::new(
                ListRow::new("peer-sync-recipient", t("peer-sync-identity-row"))
                    .icon(ui::icons::Icon::User)
                    .secondary(identity_fingerprint(key))
                    .trailing(Box::new(
                        Button::new("peer-sync-forget-identity", &t("peer-sync-identity-remove"))
                            .role(ButtonRole::Plain)
                            .control_size(ControlSize::Small)
                            .on_click(move || forget.set(true)),
                    )),
            )
        }
    }
}

/// The invites still waiting to be used, each a row with Copy invite and
/// Cancel invite.
fn pending_section(pending: &[PendingInvitationSummary], presses: &Presses) -> Element {
    if pending.is_empty() {
        return ui::widget::optional(None);
    }
    let rows = pending
        .iter()
        .map(|invite| {
            let id = hex::encode(invite.id);
            let (resend, cancel) = (presses.resend.clone(), presses.cancel_pending.clone());
            let invite_id = invite.id;
            Box::new(
                ListRow::new(format!("peer-sync-pending-{id}"), invite.name.clone())
                    .avatar(invite.name.clone(), id.clone())
                    .secondary(role_and_expiry(
                        "peer-sync-role-expires",
                        invite.role,
                        invite.expiry_unix_seconds,
                    ))
                    .trailing(Box::new(
                        Row::new()
                            .no_wrap()
                            .push(Box::new(
                                Button::new(
                                    &format!("peer-sync-resend-{id}"),
                                    &t("peer-sync-copy"),
                                )
                                .control_size(ControlSize::Small)
                                .on_click(move || resend.set(Some(invite_id))),
                            ))
                            .push(Box::new(
                                Button::new(
                                    &format!("peer-sync-cancel-pending-{id}"),
                                    &t("peer-sync-revoke"),
                                )
                                .control_size(ControlSize::Small)
                                .on_click(move || cancel.set(Some(invite_id))),
                            )),
                    )),
            ) as Element
        })
        .collect();
    Box::new(Section::titled(t("peer-sync-pending-heading"), rows))
}

/// Under the join field: what was typed, in the person's terms.
fn join_detail(detail: &JoinDetail) -> Element {
    match detail {
        JoinDetail::Hint => caption(t("peer-sync-paste-token")),
        JoinDetail::Understood(line) => caption(line.clone()),
        JoinDetail::Incomplete(summary) => Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_rhythm(Rhythm::Related)
                .push(caption(t("peer-sync-token-incomplete")))
                .push(caption(summary.clone())),
        ),
        JoinDetail::Invite { access, code } => Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_rhythm(Rhythm::Related)
                .push(Box::new(
                    ListRow::new("peer-sync-join-invite", t("peer-sync-invite-received"))
                        .icon(ui::icons::Icon::Link)
                        .secondary(access.clone()),
                ))
                .push(Box::new(
                    ListRow::new("peer-sync-join-code", code.clone())
                        .icon(ui::icons::Icon::Shield)
                        .secondary(t("peer-sync-code-check")),
                )),
        ),
    }
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

/// The widget named `id` under `widget`, as a `T`. A wrapper (a labelled
/// field) answers to its control's name too, so the search goes on into
/// it until it reaches the `T`.
fn find_mut<'a, T: 'static>(widget: &'a mut dyn Widget, id: &str) -> Option<&'a mut T> {
    let is =
        widget.test_id() == Some(id) && widget.as_any().is_some_and(<dyn std::any::Any>::is::<T>);
    if is {
        return widget.as_any_mut()?.downcast_mut::<T>();
    }
    widget
        .children_mut()
        .into_iter()
        .find_map(|child| find_mut::<T>(child, id))
}
