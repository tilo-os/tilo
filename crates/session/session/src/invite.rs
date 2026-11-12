//! Inviting people into a workspace: the sheet a link is made from, the links that still work, and
//! taking one back, on every host.
//! The sheet shows and asks; the workspace's host makes, lists and cancels the invitations
//! ([`Workspace`]): on a desktop the desk's own published actions, so a link handed over from here
//! is the same invitation an app or an agent makes; on the page the workspace's host it reaches.
//! What is the role's here is what the sheet says, leaving the link on its site's shelf, and
//! putting it on the clipboard long enough to hand it over.

#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page opens the sheet itself")
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use tilo_desktop::workspace_settings as domain;
use tilo_desktop_model::role_from_id;
use tilo_sheets::Seat as _;
use tilo_sheets::invite as sheet;
use tilo_sheets::{word, words};
use tilo_workspace_directory::Store;

use crate::Shared;
use crate::platform::clipboard::Clipboard;

/// How long a link the sheet copied stays on the clipboard: long enough to paste it into a message
/// to whoever it is for, and no longer, because the link is what admits them.
const ON_CLIPBOARD_FOR: Duration = Duration::from_secs(300);

/// An invitation the workspace made: its id (hex), its text (the link when no site keeps it), and
/// when it stops admitting anyone (nanoseconds since the epoch).
pub struct Issued {
    pub invite: String,
    pub text: String,
    pub expires_at_ns: u64,
}

/// Why the workspace made or took back nothing: it did not answer, or it said no and why.
pub enum Refused {
    Unanswered,
    Because(String),
}

/// What the host hears of an ask, once the workspace answered.
pub type Answer<T> = Box<dyn FnOnce(Result<T, Refused>)>;

/// The workspace invitations are made into, as this host reaches it.
pub trait Workspace {
    /// Whether the person may invite anyone into it from here.
    fn may_invite(&self) -> bool;
    /// The sheet went up: what stands is asked again, and `listed` hears if that failed.
    fn opened(&self, listed: Answer<()>) {
        drop(listed);
    }
    /// What it is called, when the sheet names it.
    fn name(&self) -> Option<String> {
        None
    }
    /// The parts this person may hand out.
    fn roles(&self) -> Vec<sheet::Role>;
    /// The people a link may be made for.
    fn people(&self) -> Vec<sheet::Person>;
    /// The links that still work.
    fn pending(&self) -> Vec<sheet::Pending>;
    /// Make a link at `role`, which only `recipient`'s device redeems when one is named.
    fn issue(&self, role: &str, recipient: &str, answer: Answer<Issued>);
    /// Take back the link `invite`: whoever holds it admits nobody from now on.
    fn cancel(&self, invite: &str, answer: Answer<()>);
    /// The site whose shelf keeps a link, and the workspace's own host a link names.
    fn shelf(&self) -> (Option<url::Url>, Option<String>);
}

/// The invite sheet's role: what it shows and what its presses do, over the workspace's host.
pub struct Invite {
    workspace: Rc<dyn Workspace>,
    clipboard: Rc<dyn Clipboard>,
    store: Rc<Store>,
    made: Rc<RefCell<sheet::Making>>,
    /// A link was asked for and the workspace has not answered yet.
    issuing: Rc<Cell<bool>>,
}

impl Invite {
    /// Invitations into `workspace`, left on the shelves `store` reaches and copied to `clipboard`.
    #[must_use]
    pub fn new(
        workspace: Rc<dyn Workspace>,
        clipboard: Rc<dyn Clipboard>,
        store: Rc<Store>,
    ) -> Self {
        Self {
            workspace,
            clipboard,
            store,
            made: Rc::default(),
            issuing: Rc::default(),
        }
    }

    /// The sheet goes up: nothing it made before shows.
    pub fn opening(&self) {
        self.issuing.set(false);
        self.made.borrow_mut().opening();
        let made = Rc::clone(&self.made);
        self.workspace.opened(Box::new(move |listed| {
            if let Err(refused) = listed {
                say_refused(&made, "invite-failed", refused);
            }
        }));
    }

    /// Take what a shelf answered: a link the sheet shows may have become the site's short one.
    /// Whether one answered.
    pub fn follow_shelf(&self) -> bool {
        self.store.follow_shelf(&mut self.made.borrow_mut())
    }
}

impl sheet::Invitations for Invite {
    fn may_invite(&self) -> bool {
        self.workspace.may_invite()
    }

    fn workspace(&self) -> Option<String> {
        self.workspace.name()
    }

    fn roles(&self) -> Vec<sheet::Role> {
        self.workspace.roles()
    }

    fn people(&self) -> Vec<sheet::Person> {
        self.workspace.people()
    }

    fn pending(&self) -> Vec<sheet::Pending> {
        self.workspace.pending()
    }

    fn made(&self) -> Option<sheet::Created> {
        self.made.borrow().made()
    }

    fn making(&self) -> bool {
        self.issuing.get() || self.made.borrow().making()
    }

    fn note(&self) -> Option<sheet::Note> {
        self.made.borrow().note()
    }

    fn create(&self, role: &str, recipient: &str) {
        let label = role_from_id(role).map_or_else(|| role.to_owned(), domain::role_label);
        // A link for a person names them as the sheet lists them.
        let name = (!recipient.is_empty()).then(|| {
            let person = self.workspace.people().into_iter();
            person
                .filter(|person| person.key == recipient)
                .map(|person| person.name)
                .next()
                .unwrap_or_else(|| recipient.chars().take(8).collect())
        });
        let brings = sheet::brings(&label, name.as_deref());
        let (site, host) = self.workspace.shelf();
        let (store, made, issuing) = (
            Rc::clone(&self.store),
            Rc::clone(&self.made),
            Rc::clone(&self.issuing),
        );
        self.issuing.set(true);
        self.workspace.issue(
            role,
            recipient,
            Box::new(move |issued| {
                issuing.set(false);
                let issued = match issued {
                    Ok(issued) => issued,
                    Err(refused) => return say_refused(&made, "invite-failed", refused),
                };
                // The person hands over a link, not an invitation's kilobytes: the signed bytes
                // are sealed under a fresh key and wait on the site's shelf under a handle, and
                // the link carries the key in its fragment. With no site, or one that keeps
                // nothing, the link is the invitation's own text.
                if let Err(reason) = store.hand_over(
                    &mut made.borrow_mut(),
                    &issued.invite,
                    &issued.text,
                    issued.expires_at_ns,
                    brings,
                    site.as_ref(),
                    host.as_deref(),
                ) {
                    tracing::warn!(%reason, "the invitation's text is not the invitation's bytes");
                    say_refused(&made, "invite-failed", Refused::Because(reason));
                }
            }),
        );
    }

    fn revoke(&self, invite: &str) {
        let (store, made, cancelled) = (
            Rc::clone(&self.store),
            Rc::clone(&self.made),
            invite.to_owned(),
        );
        self.workspace.cancel(
            invite,
            Box::new(move |answer| match answer {
                Ok(()) => {
                    // The site's copy goes too: a link handed out before is not one that waits
                    // there to be fetched, and the one this opening showed goes with it.
                    store.take_back(&mut made.borrow_mut(), &cancelled);
                    made.borrow_mut().say(word("invite-revoked"), false);
                }
                Err(refused) => say_refused(&made, "invite-revoke-failed", refused),
            }),
        );
    }

    fn copy(&self, link: &str) {
        self.clipboard.copy(link.to_owned(), ON_CLIPBOARD_FOR);
        self.made.borrow_mut().say(word("invite-copied"), false);
    }

    fn clear_note(&self) {
        self.made.borrow_mut().clear();
    }
}

/// Say in the sheet what went wrong with the ask `key` names.
fn say_refused(made: &RefCell<sheet::Making>, key: &str, refused: Refused) {
    let text = match refused {
        Refused::Unanswered => word("invite-no-answer"),
        Refused::Because(reason) => words(key, &[("reason", &reason)]),
    };
    made.borrow_mut().say(text, true);
}

impl Shared {
    /// Open the invite sheet on the workspace in front.
    pub(crate) fn open_invite(&self) {
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        self.inviting.opening();
        self.show_invite();
        seat.presentation().open(false);
    }

    /// Answer for the surfaces that offer inviting whether the person may, every pass: the bar
    /// keeps its invite button, and the roster its footer.
    pub(crate) fn follow_invitable(&self) {
        self.invitable
            .set(sheet::Invitations::may_invite(&self.inviting));
    }

    /// Carry out what the person asked for in the sheet, and keep it current while it is up: who
    /// is in the workspace, the parts this person may hand out and the links that still work all
    /// move under it.
    pub(crate) fn follow_invite(&self) {
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        sheet::drive(seat, &self.inviting);
        self.inviting.follow_shelf();
        if seat.presentation().is_open() {
            self.show_invite();
        }
    }

    /// Show the sheet what the workspace stands as, for the person asking.
    fn show_invite(&self) {
        if let Some(seat) = self.invite.as_ref() {
            seat.show(sheet::view(&self.inviting));
        }
    }
}
