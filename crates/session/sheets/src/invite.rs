//! "Invite people": the link into the workspace in front.
//!
//! The sheet makes one invitation at a time — the part it brings someone in
//! at, and, for a person the workspace knows, a link only their device can
//! redeem — and shows it as the text to hand over and as a code to scan, with
//! the button that puts it on the clipboard. Below it stand the invitations
//! that still work, each with the button that takes it back. The shell makes,
//! lists and cancels them; it keeps the sheet current through [`InviteSeat`].

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{
    Banner, Button, ButtonRole, Column, ControlSize, Element, ListRow, ModalDialog, QrWidget, Row,
    Section, Segment, Spacer, Text, TextInput, ToggleGroup,
};

use crate::presentation::{Presentation, Seat, presented};
use crate::{word, words};

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.invite";

/// The widest the code is drawn, quiet zone and all: room enough for squares
/// a phone reads across a desk, and no wider than the sheet gives it.
pub const CODE_MAX_PX: usize = 245;

/// The narrowest each module is drawn: an invitation's own bytes make a code
/// so dense that its squares are counted rather than guessed at, and below
/// this they turn to speckle. Two pixels a module make that whole code
/// shorter than the sheet, so a scanner can take it in at once.
pub const CODE_MODULE_PX: u32 = 2;

/// The widest each module is drawn: a link is a few dozen modules, and squares
/// this wide make it conspicuous rather than a stamp.
pub const CODE_MODULE_MAX_PX: u32 = 5;

/// The sheet's resource: the door its actions are published on. It holds
/// nothing; the invitations are the workspace's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct InviteLinks {}

#[resource(kind = "invite.links", label = "Invitations")]
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
    /// The invitation's text, as another device joins with it.
    pub link: String,
    /// The signed invitation itself, which the text spells out in base64:
    /// what the code carries, since the text alone is too long to scan.
    pub code: Vec<u8>,
    /// What the link is called, who it brings in and how long it works, and
    /// the button that copies it.
    pub link_label: String,
    pub brings: String,
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
    /// The button that makes the link.
    pub create: String,
    pub created: Option<Created>,
    pub pending_title: String,
    pub pending: Vec<Pending>,
    pub pending_empty: String,
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
/// link whose invitation is still on its way to a site's shelf, and what
/// became of the last ask. The session's desk and a page's host replica keep
/// this the same way, so a link is handed over the same way wherever the
/// sheet is shown: in the invitation's own bytes until its site answers, and
/// as the short link once it does.
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

    /// Show `created` now; when `shelving`, its invitation waits for a site's
    /// shelf, and [`Self::shelved`] replaces the link with the short one.
    pub fn show(&mut self, created: Created, shelving: bool) {
        self.created = (!shelving).then(|| created.clone());
        self.shelving = shelving.then_some(created);
        self.note = None;
    }

    /// The site answered for `invite`: its short link when it kept the
    /// invitation, the invitation's own text when it did not.
    pub fn shelved(&mut self, invite: &str, link: Option<String>) {
        let Some(mut created) = self.shelving.take() else {
            return;
        };
        if created.invite != invite {
            self.shelving = Some(created);
            return;
        }
        if let Some(link) = link {
            created.code = link.clone().into_bytes();
            created.link = link;
        }
        self.created = Some(created);
        self.note = None;
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

/// What the sheet shows of the link it just made: the invitation's own text
/// and bytes, the words around it, and how long it works.
#[must_use]
pub fn made(
    invite: &str,
    link: &str,
    code: Vec<u8>,
    brings: String,
    expires_at_ns: u64,
) -> Created {
    Created {
        invite: invite.to_owned(),
        link: link.to_owned(),
        code,
        link_label: word("invite-link-label"),
        brings,
        lasts: words(
            "invite-created-lasts",
            &[("minutes", &minutes_left(expires_at_ns).to_string())],
        ),
        copy: word("invite-copy"),
        revoke: word("invite-revoke"),
        alt: word("invite-code-alt"),
    }
}

/// Who a link brings in, in the sheet's words: the part it brings them in at,
/// and the person it names when it names one.
#[must_use]
pub fn brings(role: &str, name: Option<&str>) -> String {
    match name {
        Some(name) => words("invite-created-for", &[("name", name), ("role", role)]),
        None => words("invite-created-anyone", &[("role", role)]),
    }
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
        lasts: words(
            "invite-lasts",
            &[("minutes", &minutes_left(expires_at_ns).to_string())],
        ),
        revoke: word("invite-revoke"),
    }
}

/// How long a link has left, in the whole minutes the sheet says.
#[must_use]
pub fn minutes_left(expires_at_ns: u64) -> usize {
    let now_ns = ui::time::unix_now_ns();
    let left = expires_at_ns.saturating_sub(now_ns);
    usize::try_from(left / 60_000_000_000).unwrap_or(usize::MAX)
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
        created: invitations.made(),
        pending_title: word("invite-pending-title"),
        pending: if may_invite {
            invitations.pending()
        } else {
            Vec::new()
        },
        pending_empty: word("invite-pending-empty"),
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

/// Carry out what the person pressed in the sheet since it was last shown.
pub fn drive(seat: &InviteSeat, invitations: &dyn Invitations) {
    answer(&seat.take_asked(), invitations);
}

/// What the person picked in the sheet: the part to bring someone in at, and
/// whose device may redeem the link. `None` is nobody's choice yet, and shows
/// the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choosing {
    pub role: Option<usize>,
    pub whose: Option<usize>,
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

/// The sheet as its hosts draw it, from the [`Invite`] they keep current:
/// the part and the person the link is for, the link last made with its code
/// and its copy button, and the links that still work. What the person picks
/// is kept in `choosing` and the link shown in `link`; every press is asked
/// through `ask`. The session's window ([`InviteApp`]) and a browser page
/// build this same sheet from the same data.
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
    if let Some(note) = &invite.note {
        body = body.push(notice(note, ask));
    }
    // The link last made stands first: it is what the person hands over, and
    // the form that makes another one falls under it. The code is taller than
    // a field, so anywhere but the top of the sheet it would open clipped.
    if let Some(created) = &invite.created {
        body = body.push(handover(created, link, ask));
    }
    body = body.push(composing(invite, choosing, ask));
    if invite.may_invite {
        body = body.push(pending_rows(invite, ask));
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
/// notice over the sheet.
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

/// Making a link: the part it brings someone in at, who it is for, and the
/// button that makes it. Without the grant, why there is nothing to make.
fn composing(invite: &Invite, choosing: &LocalState<Choosing>, ask: &Ask) -> Element {
    let mut items: Vec<Element> = Vec::new();
    if !invite.may_invite {
        items.push(caption(&invite.refusal));
        return Box::new(Section::new(items));
    }
    let picked = choosing.get();
    if !invite.roles.is_empty() {
        items.push(caption(&invite.role_label));
        items.push(roles(invite, choosing, &picked));
        // What the part on offer means for whoever joins with the link, in
        // one line under the choice: the same words its hint carries.
        let at = picked
            .role
            .filter(|at| *at < invite.roles.len())
            .unwrap_or_else(|| invite.default_role());
        if let Some(role) = invite.roles.get(at) {
            items.push(caption(&role.brings));
        }
    }
    if !invite.people.is_empty() {
        items.push(caption(&invite.for_label));
        items.push(whose(invite, choosing, &picked));
    }
    // What the link is made at, as the person stands having picked: the
    // button reads it when pressed, so a choice made after the button was
    // drawn is the one it asks with.
    let picked_role = choosing.clone();
    let picked_whose = choosing.clone();
    let making = ask.clone();
    let default_role = invite.default_role();
    let role_keys: Vec<String> = invite.roles.iter().map(|role| role.key.clone()).collect();
    let people_keys: Vec<String> = invite
        .people
        .iter()
        .map(|person| person.key.clone())
        .collect();
    items.push(Box::new(
        Row::new().with_rhythm(Rhythm::Related).push(Box::new(
            Button::new("invite-create", &invite.create)
                .role(ButtonRole::Primary)
                .on_view(move || {
                    let at = picked_role.get().role.unwrap_or(default_role);
                    let role = role_keys.get(at).cloned().unwrap_or_default();
                    let recipient = picked_whose
                        .get()
                        .whose
                        .and_then(|at| people_keys.get(at).cloned())
                        .unwrap_or_default();
                    making(InviteEffect::Create { role, recipient });
                }),
        )),
    ));
    Box::new(Section::new(items))
}

/// The link last made, standing on its own: the thing the person hands over.
fn handover(created: &Created, link: &LocalState<String>, ask: &Ask) -> Element {
    Box::new(Section::new(handover_parts(created, link, ask)))
}

/// The parts on offer, the one the person may bring someone in at chosen.
fn roles(invite: &Invite, choosing: &LocalState<Choosing>, picked: &Choosing) -> Element {
    let at = picked
        .role
        .filter(|at| *at < invite.roles.len())
        .unwrap_or_else(|| invite.default_role());
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
fn whose(invite: &Invite, choosing: &LocalState<Choosing>, picked: &Choosing) -> Element {
    // The first segment is anyone holding the link, the others the people.
    let at = picked
        .whose
        .filter(|at| *at < invite.people.len())
        .map_or(0, |at| at.saturating_add(1));
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
                choosing.update(|choosing| choosing.whose = index.checked_sub(1));
            }),
    )
}

/// The link last made: what it brings someone in at, how long it works, the
/// code to scan and the button that copies the text.
fn handover_parts(created: &Created, link: &LocalState<String>, ask: &Ask) -> Vec<Element> {
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
    let drawn = QrWidget::new(created.code.clone()).with_id("invite-code");
    let px = module_px(drawn.total_modules());
    let code = drawn.with_module_px(px).with_alt(created.alt.clone());
    // The code stands on a line of its own, centred: the room its modules
    // need is wider than what is left beside a field.
    vec![
        caption(&created.link_label),
        Box::new(
            Row::new()
                .with_rhythm(Rhythm::Tight)
                .push(Box::new(Spacer::new()))
                .push(Box::new(code))
                .push(Box::new(Spacer::new())),
        ),
        Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_rhythm(Rhythm::Related)
                .push(Box::new(text))
                .push(caption(&created.brings))
                .push(caption(&created.lasts))
                .push(Box::new(
                    Row::new()
                        .with_rhythm(Rhythm::Tight)
                        .push(Box::new(
                            Button::new("invite-copy-link", &created.copy).on_view(move || {
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
                        )),
                )),
        ),
    ]
}

/// How wide each module of a code `modules` across is drawn: as wide as
/// [`CODE_MAX_PX`] allows, between the narrowest and the widest a module
/// gets. A link's few dozen modules fill the room and read at a glance; an
/// invitation's own bytes are drawn narrow enough that the whole code still
/// fits the sheet.
#[must_use]
pub fn module_px(modules: usize) -> u32 {
    u32::try_from(CODE_MAX_PX.checked_div(modules).unwrap_or(CODE_MAX_PX))
        .unwrap_or(CODE_MODULE_MAX_PX)
        .clamp(CODE_MODULE_PX, CODE_MODULE_MAX_PX)
}

/// The links that still work, each with the button that takes it back.
fn pending_rows(invite: &Invite, ask: &Ask) -> Element {
    let mut items: Vec<Element> = Vec::new();
    if invite.pending.is_empty() {
        items.push(caption(&invite.pending_empty));
    }
    for waiting in &invite.pending {
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
    Box::new(Section::titled(invite.pending_title.clone(), items))
}

/// What the sheet says under a group: the small print of what a choice
/// means.
fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption))
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
        let invite = match fixture.scenario() {
            Some("unable") => Invite {
                may_invite: false,
                refusal: "Only a member whose part here allows inviting can make a link."
                    .to_owned(),
                roles: Vec::new(),
                people: Vec::new(),
                ..sample_invite()
            },
            Some("made") => {
                let mut invite = sample_invite();
                let link = "https://tilo.sh/i/abcdefghijklmnop#AAABBBCCCDDDEEEFFFGGG\
                            HHHIIIJJJKKKLLLMMMNNNO";
                invite.created = Some(Created {
                    invite: "8f2c".to_owned(),
                    link: link.to_owned(),
                    code: link.as_bytes().to_vec(),
                    link_label: "The link".to_owned(),
                    brings: "Ada Lovelace-Byron joins as an editor.".to_owned(),
                    lasts: "The link works for the next 60 minutes.".to_owned(),
                    copy: "Copy link".to_owned(),
                    revoke: "Take back".to_owned(),
                    alt: "The invitation link as a code to scan".to_owned(),
                });
                invite
            }
            _ => sample_invite(),
        };
        fixture.provide(invite);
    }
}

/// The sheet as a workspace hands it out, for captures.
fn sample_invite() -> Invite {
    Invite {
        title: "Invite people".to_owned(),
        description: "Make a link that brings someone into this workspace. Each link works for \
                      one hour, and you can take it back here."
            .to_owned(),
        close: "Close".to_owned(),
        may_invite: true,
        refusal: String::new(),
        role_label: "They join as".to_owned(),
        roles: vec![
            Role {
                key: "viewer".to_owned(),
                label: "Viewer".to_owned(),
                brings: "Can view shared work, but cannot comment or edit it.".to_owned(),
            },
            Role {
                key: "commenter".to_owned(),
                label: "Commenter".to_owned(),
                brings: "Can view shared work and add comments, but cannot edit it.".to_owned(),
            },
            Role {
                key: "editor".to_owned(),
                label: "Editor".to_owned(),
                brings: "Can view, comment on, and edit shared work.".to_owned(),
            },
        ],
        for_label: "This link is for".to_owned(),
        people: vec![
            Person {
                key: "9a1f".to_owned(),
                name: "Ada Lovelace-Byron".to_owned(),
            },
            Person {
                key: "4c7e".to_owned(),
                name: "Grace Hopper".to_owned(),
            },
        ],
        anyone: "Anyone with the link".to_owned(),
        create: "Create link".to_owned(),
        created: None,
        pending_title: "Links that still work".to_owned(),
        pending: vec![
            Pending {
                key: "3b9d".to_owned(),
                title: "Editor".to_owned(),
                whose: "For anyone with the link".to_owned(),
                lasts: "59 min left".to_owned(),
                revoke: "Take back".to_owned(),
            },
            Pending {
                key: "77ad".to_owned(),
                title: "Viewer".to_owned(),
                whose: "For Grace Hopper".to_owned(),
                lasts: "12 min left".to_owned(),
                revoke: "Take back".to_owned(),
            },
        ],
        pending_empty: "No link is waiting to be used.".to_owned(),
        note: None,
    }
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
    const LONG: &str = "tilo-invitation-v1:the invitation's own text";
    const SHORT: &str = "https://tilo.sh/#/@https:rooms.example/w/1?invite=abcdefghijklmnop";

    fn made_link() -> Created {
        made(
            INVITE,
            LONG,
            LONG.as_bytes().to_vec(),
            brings("Editor", None),
            0,
        )
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

    #[test]
    fn the_sheet_says_which_workspace_its_links_go_into() {
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

    #[test]
    fn a_link_waits_for_its_site_and_then_shows_the_short_one() {
        let mut making = Making::default();
        making.show(made_link(), true);
        assert!(making.making(), "the site has not answered yet");
        assert!(
            making.made().is_none(),
            "a link is handed over once it is the one the site keeps"
        );

        making.shelved(INVITE, Some(SHORT.to_owned()));
        assert!(!making.making());
        let shown = making.made().unwrap();
        assert_eq!(shown.link, SHORT);
        assert_eq!(
            shown.code,
            SHORT.as_bytes(),
            "the code to scan is the link that is handed over"
        );
        assert_eq!(shown.link_label, word("invite-link-label"));
        assert_eq!(shown.alt, word("invite-code-alt"));
    }

    #[test]
    fn a_site_that_keeps_nothing_hands_over_the_invitations_own_text() {
        let mut making = Making::default();
        making.show(made_link(), false);
        let shown = making.made().unwrap();
        assert!(!making.making());
        assert_eq!(shown.link, LONG);
        assert_eq!(shown.code, LONG.as_bytes());

        // An answer that never came for this opening's own link changes nothing.
        let mut waiting = Making::default();
        waiting.show(made_link(), true);
        waiting.shelved("another invitation", Some(SHORT.to_owned()));
        assert!(waiting.making(), "still waiting for its own site");
        assert!(waiting.made().is_none());
    }

    #[test]
    fn a_link_taken_back_leaves_nothing_of_this_opening_behind() {
        let mut making = Making::default();
        making.show(made_link(), false);
        assert!(making.made().is_some());

        making.revoked("another invitation");
        assert!(making.made().is_some(), "another link is not this one");

        making.revoked(INVITE);
        assert!(making.made().is_none());
        assert!(!making.making());

        // A link still on its way to the site is taken back the same way.
        making.show(made_link(), true);
        making.revoked(INVITE);
        assert!(!making.making());
        assert!(making.made().is_none());
    }

    #[test]
    fn opening_the_sheet_again_forgets_the_link_of_the_earlier_opening() {
        let mut making = Making::default();
        making.show(made_link(), false);
        making.say("Copied".to_owned(), false);
        making.opening();
        assert!(making.made().is_none());
        assert!(making.note().is_none());
        assert!(!making.making());
    }
}
