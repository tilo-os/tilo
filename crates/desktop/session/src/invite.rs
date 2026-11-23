//! Inviting people into a workspace: the sheet a link is made from, the links that still work, and
//! taking one back, on every host.
//! The sheet shows and asks; the workspace's host makes, lists and cancels the invitations
//! ([`Workspace`]): on a desktop the shell's own published actions, so a link handed over from here
//! is the same invitation an app or an agent makes; on the page the workspace's host it reaches.
//! What is the role's here is what the sheet says, and putting the link on the clipboard long enough
//! to hand it over.

#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page opens the sheet itself")
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use tilo_sheets::SheetState as _;
use tilo_sheets::directory::Store;
use tilo_sheets::invite as sheet;
use tilo_sheets::{word, words};

use crate::Shared;
use crate::platform::clipboard::Clipboard;

/// How long a link the sheet copied stays on the clipboard: long enough to paste it into a message
/// to whoever it is for, and no longer, because the link is what admits them.
const ON_CLIPBOARD_FOR: Duration = Duration::from_secs(300);

/// An invitation the workspace made, with its id, signed text, native link if offered,
/// and when it stops admitting anyone (nanoseconds since the epoch).
pub struct Issued {
    pub invite: String,
    pub text: String,
    pub link: Option<String>,
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
    /// Whether [`Workspace::people`] answers yet, or is still being asked for.
    fn people_known(&self) -> bool {
        true
    }
    /// The links that still work.
    fn pending(&self) -> Vec<sheet::Pending>;
    /// Make a link at `role`, which only `recipient`'s device redeems when one is named.
    fn issue(&self, role: &str, recipient: &str, answer: Answer<Issued>);
    /// Take back the link `invite`: whoever holds it admits nobody from now on.
    fn cancel(&self, invite: &str, answer: Answer<()>);
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
    /// Invitations into `workspace`, handed over through `store` and copied to `clipboard`.
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

    fn people_known(&self) -> bool {
        self.workspace.people_known()
    }

    fn pending(&self) -> Vec<sheet::Pending> {
        self.workspace.pending()
    }

    fn made(&self) -> Option<sheet::Created> {
        self.made.borrow().made()
    }

    fn making(&self) -> bool {
        self.issuing.get()
    }

    fn note(&self) -> Option<sheet::Note> {
        self.made.borrow().note()
    }

    fn create(&self, role: &str, recipient: &str) {
        let asked = (role.to_owned(), recipient.to_owned());
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
                // The link names the invitation at the workspace's host, or carries it.
                let (role, recipient) = &asked;
                let mut created =
                    sheet::made(&issued.invite, role, recipient, issued.expires_at_ns);
                if let Some(link) = issued.link {
                    created.link = link;
                }
                // The sheet's state is let go before a refusal is said in it, which borrows it
                // again.
                let handed = store.hand_over(&mut made.borrow_mut(), created, &issued.text);
                if let Err(reason) = handed {
                    tracing::warn!(%reason, "the invitation was not handed over");
                    say_refused(&made, "invite-failed", Refused::Because(reason));
                }
            }),
        );
    }

    fn revoke(&self, invite: &str) {
        let (made, cancelled) = (Rc::clone(&self.made), invite.to_owned());
        self.workspace.cancel(
            invite,
            Box::new(move |answer| match answer {
                Ok(()) => {
                    // The workspace no longer hands it to whoever holds its link, and the link
                    // this opening showed goes with it.
                    made.borrow_mut().revoked(&cancelled);
                    made.borrow_mut().say(word("invite-revoked"), false);
                }
                Err(refused) => say_refused(&made, "invite-revoke-failed", refused),
            }),
        );
    }

    fn copy(&self, link: &str) {
        self.clipboard.copy(link.to_owned(), ON_CLIPBOARD_FOR);
        self.made.borrow_mut().copied(link);
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
        let Some(state) = self.invite.as_ref() else {
            return;
        };
        self.inviting.opening();
        self.show_invite();
        state.presentation().open(false);
    }

    /// Open the invitation sheet for a resource a Nearby Send action asked to share: sharing a
    /// resource is inviting people into the workspace that holds it.
    #[cfg(target_os = "linux")]
    pub(crate) fn follow_nearby_shares(&self) {
        if self.nearby_shares.borrow().is_empty() {
            return;
        }
        self.nearby_shares.borrow_mut().clear();
        crate::quick_settings::close(self);
        self.open_invite();
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
        let Some(state) = self.invite.as_ref() else {
            return;
        };
        sheet::answer(&state.take_asked(), &self.inviting);
        if state.presentation().is_open() {
            self.show_invite();
        }
    }

    /// Show the sheet what the workspace stands as, for the person asking.
    fn show_invite(&self) {
        if let Some(state) = self.invite.as_ref() {
            state.show(sheet::view(&self.inviting));
        }
    }
}
