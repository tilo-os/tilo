//! "Invite people": the link into the workspace in front.
//!
//! The person first picks the part a link brings someone in at and, for a
//! person the workspace knows, whose device alone may redeem it; under those
//! choices stands the link made at them, as a code to scan and as the site's
//! short link with the button that puts it on the clipboard. Below it stand
//! the invitations that still work, each with the button that takes it back.
//! The shell makes, lists and cancels them; it keeps the sheet current through
//! [`InviteSeat`].

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{
    Banner, Button, ButtonRole, Column, ControlSize, Element, ListRow, ModalDialog, Row, Section,
    Segment, TextInput, ToggleGroup,
};

use crate::presentation::{Presentation, Seat, presented};
use crate::{caption, code, word, words};

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.invite";

/// The sheet's resource: the door its actions are published on. It holds
/// nothing; the invitations are the workspace's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct InviteLinks {}

#[resource(kind = "invite.links")]
impl InviteLinks {}

/// What the sheet asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.invite.link")]
pub enum InviteEffect {
    /// Make an invitation
    #[effect(own_device)]
    Create {
        /// The part it brings someone in at, as the sheet lists it
        #[param("The part it brings someone in at, as the sheet lists it")]
        role: String,
        /// The person whose device alone may redeem it; anyone holding the
        /// link when empty
        #[param(
            "The person whose device alone may redeem it, by their key; anyone holding it when empty"
        )]
        recipient: String,
    },
    /// Take an invitation back: its link admits nobody from now on
    #[effect(own_device)]
    Revoke {
        #[param("The invitation, by the id the sheet lists it under")]
        invite: String,
    },
    /// Put the link on the clipboard
    #[effect(own_device, kind = "read")]
    Copy {
        #[param("The link, as the sheet shows it")]
        link: String,
    },
    /// Put the note saying what became of the last ask away
    #[effect(own_device, kind = "read")]
    ClearNote,
    /// Close the sheet
    #[effect(own_device, kind = "read")]
    Done,
}

/// One part an invitation may bring someone in at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Role {
    /// What the shell is asked for it by.
    pub key: String,
    /// What it is called, and what it lets them do.
    pub label: String,
    pub brings: String,
}

/// A person the workspace knows, whose device alone may redeem a link made
/// for them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// Their key, as the invitation names them.
    pub key: String,
    pub name: String,
}

/// The link the sheet last made, as it stands shown: until the sheet closes,
/// a link shown is one the person means to hand over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Created {
    /// The invitation's id, as taking it back names it.
    pub invite: String,
    /// The site's short link to it (`https://<site>/i/<handle>#<key>`), which
    /// the code to scan carries too.
    pub link: String,
    /// The part it brings someone in at, and the key of the person whose
    /// device alone redeems it (empty: anyone holding it). The sheet shows the
    /// link while its choices stand at these, so the choices describe it.
    pub role: String,
    pub recipient: String,
    /// When it stops admitting anyone (nanoseconds since the epoch).
    pub expires_at_ns: u64,
    /// What the link is called, how long it works, and the button that
    /// copies it.
    pub link_label: String,
    pub lasts: String,
    pub copy: String,
    /// Taking the link back, beside the one that copies it.
    pub revoke: String,
    /// The same link as a code to scan, for the person reading it.
    pub alt: String,
}

/// An invitation that still works.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// The invitation's id, as taking it back names it.
    pub key: String,
    /// The part it brings someone in at, and whom it is for.
    pub title: String,
    pub whose: String,
    pub lasts: String,
    pub revoke: String,
}

/// What became of the last ask, when there is something to say about it:
/// what went wrong, or what was done.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    /// It went wrong, rather than went as asked.
    pub bad: bool,
}

/// What the sheet shows: plain data the shell keeps current, in the person's
/// own words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Invite {
    pub title: String,
    pub description: String,
    pub close: String,
    /// Whether this person may make a link here, and why they may not.
    pub may_invite: bool,
    pub refusal: String,
    /// The parts on offer, least first, and where the chooser starts.
    pub role_label: String,
    pub roles: Vec<Role>,
    /// Who the link is for: anyone holding it, or a person here.
    pub for_label: String,
    pub people: Vec<Person>,
    pub anyone: String,
    /// The button that makes the link, and the one that makes another once
    /// a link is shown.
    pub create: String,
    pub create_another: String,
    pub created: Option<Created>,
    pub pending_title: String,
    pub pending: Vec<Pending>,
    pub note: Option<Note>,
}

impl Invite {
    /// The part the chooser stands on when the person has picked none: the
    /// highest a new person here usually joins at while it is on offer, and
    /// else the least this person may hand out.
    #[must_use]
    pub fn default_role(&self) -> usize {
        self.roles
            .iter()
            .rposition(|role| role.key == "editor")
            .unwrap_or_else(|| self.roles.len().saturating_sub(1))
    }
}

/// What a host keeps of the sheet between passes: the link it last made, the
/// invitation still on its way to a site's shelf, and what became of the last
/// ask. The session's desk and a page's host replica keep this the same way,
/// so a link is handed over the same way wherever the sheet is shown: as the
/// site's short link, once the site keeps the invitation, and never as the
/// invitation's own text.
#[derive(Clone, Debug, Default)]
pub struct Making {
    created: Option<Created>,
    shelving: Option<Created>,
    note: Option<Note>,
}

impl Making {
    /// The link this opening shows, if any.
    #[must_use]
    pub fn made(&self) -> Option<Created> {
        self.created.clone()
    }

    /// Whether the link shown is still on its way to its site's shelf.
    #[must_use]
    pub fn making(&self) -> bool {
        self.shelving.is_some()
    }

    /// What became of the last ask.
    #[must_use]
    pub fn note(&self) -> Option<Note> {
        self.note.clone()
    }

    /// `created`'s invitation waits for its site's shelf: [`Self::shelved`]
    /// shows it once the site answers with its short link.
    pub fn shelving(&mut self, created: Created) {
        self.shelving = Some(created);
        self.note = None;
    }

    /// The site answered for `invite`: its short link when it kept the
    /// invitation, else why not, which the sheet says in place of a link.
    pub fn shelved(&mut self, invite: &str, link: Result<String, String>) {
        let Some(mut created) = self.shelving.take() else {
            return;
        };
        if created.invite != invite {
            self.shelving = Some(created);
            return;
        }
        match link {
            Ok(link) => {
                created.link = link;
                self.created = Some(created);
                self.note = None;
            }
            Err(reason) => self.say(words("invite-failed", &[("reason", &reason)]), true),
        }
    }

    /// An invitation was taken back: no link of this opening shows it.
    pub fn revoked(&mut self, invite: &str) {
        self.created = self
            .created
            .take()
            .filter(|created| created.invite != invite);
        self.shelving = self
            .shelving
            .take()
            .filter(|created| created.invite != invite);
    }

    /// `link` is on the clipboard: its copy button says so while it shows.
    pub fn copied(&mut self, link: &str) {
        if let Some(created) = self.created.as_mut().filter(|created| created.link == link) {
            created.copy = word("invite-copied");
        }
    }

    /// Say in the sheet what became of the last ask.
    pub fn say(&mut self, text: String, bad: bool) {
        self.note = Some(Note { text, bad });
    }

    /// The person put the note away.
    pub fn clear(&mut self) {
        self.note = None;
    }

    /// The sheet opened afresh: no link of an earlier opening shows.
    pub fn opening(&mut self) {
        *self = Self::default();
    }
}

/// What the sheet shows of the link it just made at `role` for `recipient`
/// (empty: anyone holding it), once its site answers with the link: the words
/// around it, and how long it works.
#[must_use]
pub fn made(invite: &str, role: &str, recipient: &str, expires_at_ns: u64) -> Created {
    Created {
        invite: invite.to_owned(),
        link: String::new(),
        role: role.to_owned(),
        recipient: recipient.to_owned(),
        expires_at_ns,
        link_label: word("invite-link-label"),
        lasts: lasting(expires_at_ns),
        copy: word("invite-copy"),
        revoke: word("invite-revoke"),
        alt: word("invite-code-alt"),
    }
}

/// How the sheet names a person who chose no name: never by a piece of their key, which tells
/// nobody who they are.
#[must_use]
pub fn unnamed_person() -> String {
    word("invite-person-unnamed")
}

/// An invitation that still works, in the sheet's words. `invitee` is how the
/// invitation names the person it is for, and `bound` whether it names one at
/// all: an invitation naming nobody is for whoever holds it.
#[must_use]
pub fn pending(
    key: &str,
    role: &str,
    invitee: Option<&str>,
    bound: bool,
    expires_at_ns: u64,
) -> Pending {
    Pending {
        key: key.to_owned(),
        title: role.to_owned(),
        whose: match (invitee, bound) {
            (Some(name), _) => words("invite-for-person", &[("name", name)]),
            (None, true) => word("invite-for-someone"),
            (None, false) => word("invite-for-anyone"),
        },
        lasts: lasting(expires_at_ns),
        revoke: word("invite-revoke"),
    }
}

/// How long a link still works, as a sentence: the minutes left within the
/// hour, else the hours, each counted up so a link never reads as spent early.
#[must_use]
pub fn lasting(expires_at_ns: u64) -> String {
    let secs = expires_at_ns.saturating_sub(ui::time::unix_now_ns()) / 1_000_000_000;
    let minutes = secs.div_ceil(60).max(1);
    let (key, name, count) = if minutes < 60 {
        ("invite-lasting-minutes", "minutes", minutes)
    } else {
        ("invite-lasting-hours", "hours", minutes.div_ceil(60))
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a count of minutes or hours is far inside an f64's integers"
    )]
    let count = ui::i18n::ArgValue::Number(count as f64);
    crate::t_args(key, &[(name, count)])
}

/// What the sheet asks of whoever makes its invitations: the session's desk,
/// which keeps them in the workspace's governance, and a page's host
/// replica, which asks its host. Both answer the same facts and carry out the
/// same presses, so the sheet is the same wherever it stands.
pub trait Invitations {
    /// Whether this person may make a link here.
    fn may_invite(&self) -> bool;
    /// The workspace the links go into, when the sheet is opened for one: the
    /// title says what is being shared, so nobody has to guess the scope.
    fn workspace(&self) -> Option<String> {
        None
    }
    /// The parts they may hand out, least first.
    fn roles(&self) -> Vec<Role> {
        Vec::new()
    }
    /// The people a link can be made for; empty when every link is for
    /// whoever holds it.
    fn people(&self) -> Vec<Person> {
        Vec::new()
    }
    /// The links that still work.
    fn pending(&self) -> Vec<Pending> {
        Vec::new()
    }
    /// The link this opening made.
    fn made(&self) -> Option<Created> {
        None
    }
    /// Whether the link made is still on its way to its site's shelf.
    fn making(&self) -> bool {
        false
    }
    /// What became of the last ask.
    fn note(&self) -> Option<Note> {
        None
    }
    /// Make a link: the part it brings someone in at, and the person whose
    /// device alone redeems it (empty: anyone holding it).
    fn create(&self, role: &str, recipient: &str);
    /// Take a link back: it admits nobody from now on.
    fn revoke(&self, invite: &str);
    /// Put a link on the clipboard.
    fn copy(&self, link: &str);
    /// The person put the note away.
    fn clear_note(&self);
}

/// The sheet's data, as whoever makes its invitations stands: the words are
/// this crate's catalog, so the session's window and a browser page show the
/// same sheet in the same person's language.
#[must_use]
pub fn view(invitations: &dyn Invitations) -> Invite {
    let may_invite = invitations.may_invite();
    // How long the link shown still works, as of this pass: the sheet counts it down.
    let created = invitations.made().map(|created| Created {
        lasts: lasting(created.expires_at_ns),
        ..created
    });
    let title = match invitations.workspace() {
        Some(workspace) => words("invite-title-workspace", &[("workspace", &workspace)]),
        None => word("invite-title"),
    };
    Invite {
        title,
        description: word("invite-description"),
        close: word("invite-close"),
        may_invite,
        refusal: word("invite-refusal"),
        role_label: word("invite-role-label"),
        roles: if may_invite {
            invitations.roles()
        } else {
            Vec::new()
        },
        for_label: word("invite-for-label"),
        people: if may_invite {
            invitations.people()
        } else {
            Vec::new()
        },
        anyone: word("invite-anyone"),
        create: word("invite-create"),
        create_another: word("invite-create-another"),
        pending_title: word("invite-pending-title"),
        pending: if may_invite {
            invitations.pending()
        } else {
            Vec::new()
        },
        created,
        note: invitations.note().or_else(|| {
            invitations.making().then(|| Note {
                text: word("invite-making"),
                bad: false,
            })
        }),
    }
}

/// Carry out what the person pressed since it was last shown: whoever makes
/// the invitations takes the presses the same way wherever the sheet stands.
/// Closing is the host's to do: the sheet put itself away as it asked, and
/// its presentation says so.
pub fn answer(asked: &[InviteEffect], invitations: &dyn Invitations) {
    for effect in asked {
        match effect {
            InviteEffect::Create { role, recipient } => invitations.create(role, recipient),
            InviteEffect::Revoke { invite } => invitations.revoke(invite),
            InviteEffect::Copy { link } => invitations.copy(link),
            InviteEffect::ClearNote => invitations.clear_note(),
            InviteEffect::Done => {}
        }
    }
}

/// What the person picked in the sheet: the part to bring someone in at, and
/// whose device may redeem the link, as the chooser's segment (0: anyone
/// holding it, then the people). `None` is nobody's choice yet, and shows what
/// the link shown was made at, else the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choosing {
    pub role: Option<usize>,
    pub whose: Option<usize>,
}

impl Choosing {
    /// The segments the choosers stand on in `invite`: the part, and whose.
    fn standing(&self, invite: &Invite) -> (usize, usize) {
        let made = invite.created.as_ref();
        let role = self
            .role
            .filter(|at| *at < invite.roles.len())
            .or_else(|| {
                made.and_then(|made| invite.roles.iter().position(|role| role.key == made.role))
            })
            .unwrap_or_else(|| invite.default_role());
        let whose = self
            .whose
            .filter(|at| *at <= invite.people.len())
            .or_else(|| {
                made.and_then(|made| {
                    if made.recipient.is_empty() {
                        return Some(0);
                    }
                    let at = invite
                        .people
                        .iter()
                        .position(|person| person.key == made.recipient);
                    at.map(|at| at.saturating_add(1))
                })
            })
            .unwrap_or(0);
        (role, whose)
    }

    /// The link the choosers stand at in `invite`: the part's key, and the
    /// person's (empty: anyone holding it).
    fn asking(&self, invite: &Invite) -> (String, String) {
        let (role, whose) = self.standing(invite);
        let role = invite.roles.get(role).map(|role| role.key.clone());
        let recipient = whose
            .checked_sub(1)
            .and_then(|at| invite.people.get(at))
            .map(|person| person.key.clone());
        (role.unwrap_or_default(), recipient.unwrap_or_default())
    }

    /// The link made, while the choosers stand at what it was made at: a link
    /// shown under the choices is always the one they describe.
    #[must_use]
    pub fn shown<'a>(&self, invite: &'a Invite) -> Option<&'a Created> {
        let (role, recipient) = self.asking(invite);
        invite
            .created
            .as_ref()
            .filter(|made| made.role == role && made.recipient == recipient)
    }
}

/// What the shell lends the sheet: whether it is up, what it shows, and where
/// its requests go.
#[derive(Clone, Default)]
pub struct InviteSeat {
    presentation: Presentation,
    invite: Rc<RefCell<Invite>>,
    shown: Rc<RefCell<Option<LocalState<Invite>>>>,
    asked: Rc<RefCell<Vec<InviteEffect>>>,
}

impl std::fmt::Debug for InviteSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InviteSeat")
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl crate::Seat for InviteSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl InviteSeat {
    /// Show `invite`, or the same invitation as it stands now.
    pub fn show(&self, invite: Invite) {
        if *self.invite.borrow() == invite {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.set(invite.clone());
        }
        *self.invite.borrow_mut() = invite;
    }

    /// What the sheet shows now.
    pub fn invite(&self) -> Invite {
        self.invite.borrow().clone()
    }

    /// The requests made since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<InviteEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }
}

/// How the sheet asks its host for something: an effect the person pressed,
/// carried out by whoever shows the sheet. The session's window pushes it
/// into the seat its shell reads; a page's host hands it to the page.
pub type Ask = Rc<dyn Fn(InviteEffect)>;

/// The sheet as its hosts draw it, from the [`Invite`] they keep current: the
/// part and the person the link is for, the link made at those choices with
/// its code and its copy button, and the links that still work. What the
/// person picks is kept in `choosing` and the link shown in `link`; every
/// press is asked through `ask`. The session's window ([`InviteApp`]) and a
/// browser page build this same sheet from the same data.
pub fn sheet(
    invite: &Invite,
    choosing: &LocalState<Choosing>,
    link: &LocalState<String>,
    ask: &Ask,
    presentation: Option<&Presentation>,
) -> Element {
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Section);
    // In the order the person goes: who the link brings in and as what, then the link made at those
    // choices with the buttons that copy it, take it back and make another, or the button that
    // makes it. What a press did is said under the press.
    let picked = choosing.get();
    let shown = picked.shown(invite);
    body = body.push(composing(invite, choosing, &picked));
    if invite.may_invite {
        let mut handing: Vec<Element> = if let Some(created) = shown {
            handover(invite, created, choosing, link, ask)
        } else {
            let create = create_button(invite, choosing, ask, false);
            vec![Box::new(Row::new().push(Box::new(create)))]
        };
        if let Some(note) = &invite.note {
            handing.push(notice(note, ask));
        }
        body = body.push(Box::new(Section::new(handing)));
        if let Some(rows) = pending_rows(invite, shown, ask) {
            body = body.push(rows);
        }
    }
    let dismissed = ask.clone();
    let escaped = ask.clone();
    Box::new(presented(
        presentation,
        ModalDialog::new(invite.title.clone(), body)
            .with_test_id("invite")
            .with_icon(Icon::UserPlus)
            .with_description(invite.description.clone())
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            .secondary_action(invite.close.clone(), move || dismissed(InviteEffect::Done))
            .on_dismiss(move || escaped(InviteEffect::Done)),
    ))
}

/// What the person asked for last went wrong, or went as asked: said in a
/// notice under the press.
fn notice(note: &Note, ask: &Ask) -> Element {
    let dismiss = ask.clone();
    let banner = if note.bad {
        Banner::error(note.text.clone())
    } else {
        Banner::success(note.text.clone())
    };
    Box::new(
        banner
            .with_id("invite-note")
            .dismissible()
            .on_dismiss(move || dismiss(InviteEffect::ClearNote)),
    )
}

/// The choices a link is made at: the part it brings someone in at, with
/// what that part may do, and who it is for. Without the grant, why there is
/// nothing to make.
fn composing(invite: &Invite, choosing: &LocalState<Choosing>, picked: &Choosing) -> Element {
    let mut items: Vec<Element> = Vec::new();
    if !invite.may_invite {
        items.push(caption(&invite.refusal));
        return Box::new(Section::new(items));
    }
    let (role_at, whose_at) = picked.standing(invite);
    if !invite.roles.is_empty() {
        items.push(caption(&invite.role_label));
        items.push(roles(invite, choosing, role_at));
        if let Some(role) = invite.roles.get(role_at) {
            items.push(caption(&role.brings));
        }
    }
    if !invite.people.is_empty() {
        items.push(caption(&invite.for_label));
        items.push(whose(invite, choosing, whose_at));
    }
    Box::new(Section::new(items))
}

/// The button that makes a link at the choices standing when it is pressed,
/// or another one beside the link shown.
fn create_button(
    invite: &Invite,
    choosing: &LocalState<Choosing>,
    ask: &Ask,
    again: bool,
) -> Button {
    let (label, role) = if again {
        (&invite.create_another, ButtonRole::Secondary)
    } else {
        (&invite.create, ButtonRole::Primary)
    };
    let making = ask.clone();
    let choosing = choosing.clone();
    let invite = invite.clone();
    Button::new("invite-create", label)
        .role(role)
        .on_view(move || {
            let (role, recipient) = choosing.get().asking(&invite);
            making(InviteEffect::Create { role, recipient });
        })
}

/// The parts on offer, the one the choosers stand on chosen.
fn roles(invite: &Invite, choosing: &LocalState<Choosing>, at: usize) -> Element {
    let choosing = choosing.clone();
    Box::new(
        ToggleGroup::new(
            invite
                .roles
                .iter()
                .map(|role| Segment::new(role.label.clone()).with_hint(role.brings.clone()))
                .collect(),
            at,
        )
        .with_id("invite-role")
        .with_accessible_name(invite.role_label.clone())
        .on_view("invite-role", move |index| {
            choosing.update(|choosing| choosing.role = Some(index));
        }),
    )
}

/// Who the link is for: anyone holding it, or a person the workspace knows.
fn whose(invite: &Invite, choosing: &LocalState<Choosing>, at: usize) -> Element {
    let mut segments = vec![Segment::new(invite.anyone.clone())];
    segments.extend(
        invite
            .people
            .iter()
            .map(|person| Segment::new(person.name.clone())),
    );
    let choosing = choosing.clone();
    Box::new(
        ToggleGroup::new(segments, at)
            .with_id("invite-for")
            .with_accessible_name(invite.for_label.clone())
            .on_view("invite-for", move |index| {
                choosing.update(|choosing| choosing.whose = Some(index));
            }),
    )
}

/// The link made at the choices, as Add a device shows its own: the code to
/// scan, how long it works, and the link as text with the buttons that copy
/// it, take it back and make another.
fn handover(
    invite: &Invite,
    created: &Created,
    choosing: &LocalState<Choosing>,
    link: &LocalState<String>,
    ask: &Ask,
) -> Vec<Element> {
    if link.get() != created.link {
        link.set(created.link.clone());
    }
    let copying = ask.clone();
    let copied = created.link.clone();
    let revoking = ask.clone();
    let invite_id = created.invite.clone();
    let text = TextInput::from_state(link)
        .with_id("invite-link")
        .with_read_only(true)
        .with_accessible_name(&created.link_label);
    vec![
        crate::centred(code(&created.link, "invite-code", &created.alt)),
        caption(&created.lasts),
        Box::new(text),
        Box::new(
            Row::new()
                .with_rhythm(Rhythm::Tight)
                .push(Box::new(
                    Button::new("invite-copy-link", &created.copy)
                        .with_icon(Icon::Copy)
                        .role(ButtonRole::Primary)
                        .on_view(move || {
                            copying(InviteEffect::Copy {
                                link: copied.clone(),
                            });
                        }),
                ))
                .push(Box::new(
                    Button::new("invite-revoke-made", &created.revoke).on_view(move || {
                        revoking(InviteEffect::Revoke {
                            invite: invite_id.clone(),
                        });
                    }),
                ))
                .push(Box::new(create_button(invite, choosing, ask, true))),
        ),
    ]
}

/// The links that still work, each with the button that takes it back, when
/// there are any; the link shown above with its own buttons is not listed again.
fn pending_rows(invite: &Invite, shown: Option<&Created>, ask: &Ask) -> Option<Element> {
    let shown = shown.map(|created| created.invite.as_str());
    let listed: Vec<&Pending> = invite
        .pending
        .iter()
        .filter(|pending| Some(pending.key.as_str()) != shown)
        .collect();
    if listed.is_empty() {
        return None;
    }
    let mut items: Vec<Element> = Vec::new();
    for waiting in listed {
        let revoking = ask.clone();
        let invite_id = waiting.key.clone();
        let revoke_id = format!("invite-revoke-{}", waiting.key);
        items.push(Box::new(
            ListRow::new(
                format!("invite-pending-{}", waiting.key),
                waiting.title.clone(),
            )
            .subtitle(waiting.whose.clone())
            .secondary(&waiting.lasts)
            .trailing(Box::new(
                Button::new(&revoke_id, &waiting.revoke)
                    .control_size(ControlSize::Small)
                    .on_view(move || {
                        revoking(InviteEffect::Revoke {
                            invite: invite_id.clone(),
                        });
                    }),
            )),
        ));
    }
    Some(Box::new(Section::titled(
        invite.pending_title.clone(),
        items,
    )))
}

/// The sheet as an app.
pub struct InviteApp;

impl ResourceApp for InviteApp {
    type Resource = InviteLinks;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Invite people";
    const ICON: Option<&'static str> = Some("user-plus");
    // Over the desktop: what is not the sheet shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<InviteLinks>) -> Element {
        let seat = cx.take_lent::<InviteSeat>();
        let initial = match &seat {
            Some(seat) => seat.invite(),
            None => cx
                .fixture::<Invite>()
                .map(|invite| (*invite).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(initial);
        let choosing = cx.local(Choosing::default());
        let link = cx.local(String::new());
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
            // The sheet follows what the shell says of its presentation: being
            // opened, taken down, put on another display.
            seat.presentation.wake_with(cx.waker());
        }
        let asking = seat.clone();
        let presentation = seat.as_ref().map(|seat| seat.presentation().clone());
        cx.effects(move |effect: InviteEffect, _cx| match &asking {
            Some(seat) => {
                if effect == InviteEffect::Done {
                    // Its own close: the card leaves the screen, and the shell tells it gone
                    // once nothing of it is drawn.
                    seat.presentation.close();
                }
                seat.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no workspace to invite anyone into here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        let ask: Ask = Rc::new(move |effect: InviteEffect| dispatch.effect(&effect));
        cx.switch(&shown, move |cx, invite| {
            let invite = invite.clone();
            let picked = choosing.clone();
            let choosing = choosing.clone();
            let link = link.clone();
            let ask = ask.clone();
            let presentation = presentation.clone();
            cx.switch(&picked, move |_cx, _picked| {
                sheet(&invite, &choosing, &link, &ask, presentation.as_ref())
            })
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &["ready", "made", "unable"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<InviteLinks>) {
        fixture.provide(sample(fixture.scenario()));
    }
}

/// The sheet over a sample workspace, for captures and tests: three parts on
/// offer, two people, two links that still work, and in `made` an editor's link
/// for Ada just made; in `unable` the person may not invite.
#[must_use]
pub fn sample(scenario: Option<&str>) -> Invite {
    view(&Sample {
        may_invite: scenario != Some("unable"),
        made: scenario == Some("made"),
    })
}

/// The workspace [`sample`] shows.
struct Sample {
    may_invite: bool,
    made: bool,
}

/// What [`Sample`]'s links work for from now, in minutes.
fn sample_expiry(minutes: u64) -> u64 {
    ui::time::unix_now_ns().saturating_add(minutes.saturating_mul(60_000_000_000))
}

impl Invitations for Sample {
    fn may_invite(&self) -> bool {
        self.may_invite
    }
    fn roles(&self) -> Vec<Role> {
        [
            (
                "viewer",
                "Viewer",
                "Can view shared work, but cannot comment or edit it.",
            ),
            (
                "commenter",
                "Commenter",
                "Can view shared work and add comments, but cannot edit it.",
            ),
            (
                "editor",
                "Editor",
                "Can view, comment on, and edit shared work.",
            ),
        ]
        .map(|(key, label, brings)| Role {
            key: key.to_owned(),
            label: label.to_owned(),
            brings: brings.to_owned(),
        })
        .to_vec()
    }
    fn people(&self) -> Vec<Person> {
        [("9a1f", "Ada Lovelace-Byron"), ("4c7e", "Grace Hopper")]
            .map(|(key, name)| Person {
                key: key.to_owned(),
                name: name.to_owned(),
            })
            .to_vec()
    }
    fn pending(&self) -> Vec<Pending> {
        let mut listed = vec![
            pending("3b9d", "Editor", None, false, sample_expiry(59)),
            pending(
                "77ad",
                "Viewer",
                Some("Grace Hopper"),
                true,
                sample_expiry(12),
            ),
        ];
        if self.made {
            let made = pending(
                "8f2c",
                "Editor",
                Some("Ada Lovelace-Byron"),
                true,
                sample_expiry(60),
            );
            listed.push(made);
        }
        listed
    }
    fn made(&self) -> Option<Created> {
        self.made.then(|| Created {
            link: "https://tilo.sh/i/abcdefghijklmnop#AAABBBCCCDDDEEEFFFGGGHHHIIIJJJKKKLLLMMMNNNO"
                .to_owned(),
            ..made("8f2c", "editor", "9a1f", sample_expiry(60))
        })
    }
    fn create(&self, _role: &str, _recipient: &str) {}
    fn revoke(&self, _invite: &str) {}
    fn copy(&self, _link: &str) {}
    fn clear_note(&self) {}
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(InviteApp)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed step in a test is the assertion"
    )]

    use super::*;

    const INVITE: &str = "49a289c86314e68b618e01abca086915354cdb16fd11fdd45746a95161ee71f8";
    const SHORT: &str =
        "https://tilo.sh/i/abcdefghijklmnop#AAABBBCCCDDDEEEFFFGGGHHHIIIJJJKKKLLLMMMNNNO";

    fn made_link() -> Created {
        made(INVITE, "editor", "", 0)
    }

    /// A sheet whose links go into `workspace`, when it names one.
    struct Into(Option<&'static str>);

    impl Invitations for Into {
        fn may_invite(&self) -> bool {
            false
        }
        fn workspace(&self) -> Option<String> {
            self.0.map(str::to_owned)
        }
        fn create(&self, _role: &str, _recipient: &str) {}
        fn revoke(&self, _invite: &str) {}
        fn copy(&self, _link: &str) {}
        fn clear_note(&self) {}
    }

    tilo_test_env::scenario! {
        /// `view`: what the sheet shows of the invitations it is given.
        fn the_view_names_its_workspace;

        phase the_sheet_says_which_workspace_its_links_go_into;
        {
            assert_eq!(
                view(&Into(None)).title,
                word("invite-title"),
                "a sheet opened for nothing in particular says only what it is for"
            );
            assert_eq!(
                view(&Into(Some("Design"))).title,
                words("invite-title-workspace", &[("workspace", "Design")]),
                "a sheet opened for a workspace names it, so the scope is never a guess"
            );
        }
    }

    tilo_test_env::scenario! {
        /// `Making`: the link one opening of the sheet made, from asking the site to taking it back.
        fn a_link_in_the_making_is_shown_once_its_site_keeps_it;

        phase a_link_waits_for_its_site_and_then_shows_the_short_one;
        {
            let mut making = Making::default();
            making.shelving(made_link());
            assert!(making.making(), "the site has not answered yet");
            assert!(
                making.made().is_none(),
                "a link is handed over once it is the one the site keeps"
            );

            // An answer for another opening's link changes nothing.
            making.shelved("another invitation", Ok("https://elsewhere/i/x".to_owned()));
            assert!(making.making(), "still waiting for its own site");

            making.shelved(INVITE, Ok(SHORT.to_owned()));
            assert!(!making.making());
            let shown = making.made().unwrap();
            assert_eq!(shown.link, SHORT);
            assert_eq!(shown.link_label, word("invite-link-label"));
            assert_eq!(shown.alt, word("invite-code-alt"));
        }

        phase a_site_that_keeps_nothing_shows_no_link_and_says_why;
        {
            let mut making = Making::default();
            making.shelving(made_link());
            making.shelved(INVITE, Err("the site is down".to_owned()));
            assert!(!making.making());
            assert!(
                making.made().is_none(),
                "the invitation's own text is never handed over"
            );
            let note = making.note().unwrap();
            assert!(note.bad);
            assert!(note.text.contains("the site is down"), "{}", note.text);
        }

        phase a_link_taken_back_leaves_nothing_of_this_opening_behind;
        {
            let mut making = Making::default();
            making.shelving(made_link());
            making.shelved(INVITE, Ok(SHORT.to_owned()));
            assert!(making.made().is_some());

            making.revoked("another invitation");
            assert!(making.made().is_some(), "another link is not this one");

            making.revoked(INVITE);
            assert!(making.made().is_none());
            assert!(!making.making());

            // A link still on its way to the site is taken back the same way.
            making.shelving(made_link());
            making.revoked(INVITE);
            assert!(!making.making());
            assert!(making.made().is_none());
        }

        phase opening_the_sheet_again_forgets_the_link_of_the_earlier_opening;
        {
            let mut making = Making::default();
            making.shelving(made_link());
            making.shelved(INVITE, Ok(SHORT.to_owned()));
            making.copied(SHORT);
            assert_eq!(making.made().unwrap().copy, word("invite-copied"));
            making.say("Taken back".to_owned(), false);
            making.opening();
            assert!(making.made().is_none());
            assert!(making.note().is_none());
            assert!(!making.making());
        }
    }

    #[test]
    fn how_long_a_link_works_reads_as_a_sentence() {
        let now = ui::time::unix_now_ns();
        // A few seconds pass between making a link and reading it: the count goes up, never down.
        let at = |secs: u64| now.saturating_add(secs.saturating_mul(1_000_000_000));
        assert_eq!(lasting(at(60 * 60 - 5)), "Works for the next hour.");
        assert_eq!(lasting(at(59 * 60 - 5)), "Works for the next 59 minutes.");
        assert_eq!(lasting(at(30)), "Works for the next minute.");
        assert_eq!(lasting(at(3 * 3600 - 5)), "Works for the next 3 hours.");
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(lasting(at(60 * 60 - 5)), "Gilt für die nächste Stunde.");
        assert_eq!(
            lasting(at(12 * 60 - 5)),
            "Gilt für die nächsten 12 Minuten."
        );
        ui::i18n::I18nManager::set_locale("en-US");
    }
}
