//! A view written in the app language, shown where an app places it.
//!
//! A person or an agent writes a `view` as an app declares one (`view left() { ... }`). It names
//! the workspace's apps by their names and reads what they publish, as whoever looks at it may
//! read it. An app that shows one (a chat message that carries a view) hands it over here and
//! gets the widget that draws it: built from the toolkit's own components, live, and drawn again
//! when the records it reads change.
//!
//! What the view does with an action is decided by the workspace's policy with its writer as the
//! author of its code. A writer the person has not trusted only suggests: what the view asked
//! waits ([`ViewState::suggestions`]) until the person allows that one thing or dismisses it, and
//! nothing changes before. The app that placed the view shows what waits, with the shared
//! controls.
//!
//! The code that runs a view (the language's compiler and interpreter, and the half of the UI
//! host that builds widgets from what a view describes) is no part of a program's start. A
//! program that can run one says how the code is fetched ([`WRITTEN_VIEWS`]), and the first view
//! shown fetches it. A program that cannot leaves the boundary as it is, and a view placed there is
//! [`Placed::Unavailable`].

use std::rc::Rc;

use tilo_actions::{Actor, PersonId};
use ui_core::deferred::Deferred;
use ui_core::remote_actions::RemoteActions;
use ui_core::widget::Element;

/// A written view, as the app that shows it names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrittenView {
    /// The view as its writer wrote it: one `view` declaration and the functions it calls.
    pub source: String,
    /// Who wrote it, as the workspace recorded them (the writer of the message that carries it),
    /// never as the view or whoever sent it claims: the author of its code.
    pub author: Actor,
    /// Who looks at it. It reads what they may read.
    pub viewer: PersonId,
    /// The apps it reads, by the names it calls them (`tasks`), as its writer listed them where
    /// the view was posted. It is given these and no other: an app its definition names that is
    /// not listed is unknown to it.
    pub reads: Vec<String>,
}

/// Something a view asked to do that waits for the person: its writer is not trusted to do it
/// alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// What [`ViewState::allow`] and [`ViewState::dismiss`] name it by.
    pub id: u64,
    /// The app it would change, by the name people call it.
    pub app: String,
    /// What it would do, in the app's words: the action's own name, never the label of the
    /// control that asked for it.
    pub action: String,
    /// What the action is given, exactly as it would be sent: each argument by the name the
    /// app's action gives it, with its value. Nothing is sent that is not here, so whoever shows
    /// a suggestion shows these whole, or says how much it left out.
    pub arguments: Vec<(String, String)>,
    /// Whether the action destroys something.
    pub destructive: bool,
    /// Who wrote the view that asked.
    pub by: Actor,
}

/// What a shown view waits on and was refused, for the app that placed it to say.
pub trait ViewState {
    /// What the view asked to do that waits for the person, oldest first.
    fn suggestions(&self) -> Vec<Suggestion>;
    /// The person allows this one thing: it is done as theirs. Nothing else the view asks is
    /// allowed by it.
    fn allow(&self, id: u64);
    /// The person dismisses it: nothing is done.
    fn dismiss(&self, id: u64);
    /// The apps whose records the person may not read here, by the names people call them. The
    /// view shows nothing of them.
    fn refused(&self) -> Vec<String>;
    /// The apps whose records could not be read here for another reason (this program reads no
    /// records outside their apps), by the names people call them.
    fn unread(&self) -> Vec<String>;
    /// Moves when any of the above does.
    fn revision(&self) -> u64;
}

/// A written view, shown: the widget that draws it, and what it waits on.
pub struct Shown {
    pub widget: Element,
    pub state: Rc<dyn ViewState>,
}

/// Builds a written view's widget. `api` is the action API of the window that shows it: the
/// view's readings and what it is allowed to do go through it, as the person at that window.
#[derive(Clone, Copy)]
pub struct MakeWrittenView(pub fn(&WrittenView, &RemoteActions) -> Shown);

/// The code that runs written views. No program links it: one that can run a view says how the
/// code is fetched (`defer`) and installs it once it is in, and every view placed before then is
/// [`Placed::Arriving`].
pub static WRITTEN_VIEWS: Deferred<MakeWrittenView> = Deferred::new();

/// A written view where an app placed it.
pub enum Placed {
    Shown(Shown),
    /// The code that runs views is on its way: place the view again once it is in
    /// ([`code_generation`] moves then).
    Arriving,
    /// This program cannot run a view, or could not fetch the code that does. Placing the view
    /// again asks for the code again.
    Unavailable,
}

/// Place `view` in a window whose action API is `api`: the widget that draws it, once the code
/// that runs views is in. The first view placed fetches that code.
#[must_use]
pub fn place(view: &WrittenView, api: &RemoteActions) -> Placed {
    match WRITTEN_VIEWS.get() {
        Some(make) => Placed::Shown((make.0)(view, api)),
        None if WRITTEN_VIEWS.fetching() => Placed::Arriving,
        None => Placed::Unavailable,
    }
}

/// Moves when the code that runs views arrives: a view that was [`Placed::Arriving`] is placed
/// again then.
#[must_use]
pub fn code_generation() -> u64 {
    WRITTEN_VIEWS.generation()
}
