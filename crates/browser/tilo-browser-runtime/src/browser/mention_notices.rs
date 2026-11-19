//! Telling this page's person when someone mentions them, wherever in the workspace.
//!
//! The records are what this page's replication took in from the workspace and wrote
//! ([`crate::replicated::changes`], [`crate::replicated::resource_document`]), so a record open in
//! a window or not is looked at as the change reaches it, and the notices are the session's own
//! role ([`tilo_session::mentions`]) shown through this host's notifications
//! ([`tilo_session::platform::notice::Toolkit`]). Choosing one opens the place the mention is in,
//! as any open goes: through the desktop's session, as this person.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop_search::InvokeTarget;
use tilo_session::mentions::Mentions;
use tilo_session::platform::notice::Place;
use tilo_session::platform::records::{Changed, Changes, Records, Since};
use ui::automerge::AutoCommit;

use super::Desktop;

/// The records this page holds from the workspace it replicated, as the mention role reads them.
struct Taken {
    /// The workspace's peer on this page, as replication names it.
    peer: u32,
    /// The stream of record changes the page's replication feeds.
    changes: Changes,
}

impl Records for Taken {
    fn take_changed(&self) -> Vec<Changed> {
        self.changes.take()
    }

    fn document(&self, app: &str, _workspace: &str, record: &str) -> Option<AutoCommit> {
        crate::replicated::resource_document(self.peer, app, record)
    }

    fn since(
        &self,
        app: &str,
        _workspace: &str,
        record: &str,
        heads: &[ui::automerge::ChangeHash],
    ) -> Since {
        crate::replicated::record_since(self.peer, app, record, heads)
    }

    fn heads(
        &self,
        app: &str,
        _workspace: &str,
        record: &str,
    ) -> Option<Vec<ui::automerge::ChangeHash>> {
        crate::replicated::record_heads(self.peer, app, record)
    }
}

/// This page's mentions of `person` in the workspace `peer` replicated into `store`, drawn by the
/// frame `wake` asks for.
pub(super) fn role(
    store: tilo_session::platform::Store,
    peer: u32,
    person: &str,
    (name, shown): (
        tilo_session::mentions::Names,
        tilo_session::mentions::OnScreen,
    ),
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Mentions {
    Mentions::over(tilo_session::mentions::Given {
        person: vec![person.to_owned()],
        store: Some(store),
        records: Some(Rc::new(Taken {
            peer,
            changes: crate::replicated::changes(),
        })),
        notices: Some(crate::host_services::notices()),
        name,
        shown,
        wake: Some(wake),
    })
}

impl Desktop {
    /// Look for new mentions of this person in what reached the page, and open what they chose
    /// from a notice. Nothing is looked at before the page's first frame is up: the code that
    /// looks comes after it ([`tilo_session::mentions::STEP`]), and what reached the page waits.
    pub(super) fn tell_mentions(&self) {
        if super::first_frame_presented() {
            self.mentions.borrow().step();
        }
        self.open_chosen_mentions();
        self.show_asked_in_apps();
    }

    /// Open, as this person, each place they chose from a mention's notice: the app on the record,
    /// showing the item the mention is in.
    fn open_chosen_mentions(&self) {
        for place in self.mentions.borrow().take_chosen() {
            self.open(&place);
        }
    }

    /// Show, as this person, what the desktop asked to see in an app's window
    /// ([`crate::shell::HostRequest::ShowInApp`]): through the session, which opens the window and
    /// shows it there, as a notice's place; and each link it asked an app to show
    /// ([`crate::shell::HostRequest::OpenLink`]): the app's `open-link`, the same way.
    fn show_asked_in_apps(&self) {
        let mut asked = Vec::new();
        let mut links = Vec::new();
        self.requests.borrow_mut().retain(|request| match request {
            crate::shell::HostRequest::ShowInApp(place) => {
                asked.push(place.clone());
                false
            }
            crate::shell::HostRequest::OpenLink {
                app,
                resource,
                link,
            } => {
                links.push(
                    InvokeTarget::action(
                        app.clone(),
                        self.workspace.as_str(),
                        resource.clone(),
                        tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION,
                    )
                    .with_argument(tilo_actions::remote::OPEN_LINK_PARAM, link.clone()),
                );
                false
            }
            _ => true,
        });
        for place in asked {
            self.open(&place);
        }
        for link in links {
            self.invoke_as_person(&link);
        }
    }

    /// Ask the session to open `place` as this person, as a search hit opens what it found.
    pub(super) fn open(&self, place: &Place) {
        self.invoke_as_person(&InvokeTarget::place(self.workspace.as_str(), place));
    }
}

/// What the person's mentions are told by, kept where their settings are
/// ([`crate::browser`]'s mount hands them in): the session's role over what this page replicated,
/// told through the host's notifications, drawing the frame `woken` asks for.
pub(super) fn over(
    store: Rc<dyn durable_store::DurableStore>,
    asks: &crate::browser::session_asks::Asks,
    person: &str,
    people: &Rc<RefCell<HashMap<String, crate::desktop::PersonIdentity>>>,
    here: &tilo_desktop::here::Here,
    (shell, woken): (
        &tilo_desktop_surface::DesktopHandle,
        &Arc<std::sync::atomic::AtomicBool>,
    ),
) -> Mentions {
    let shell = shell.clone();
    role(
        store,
        asks.peer().unwrap_or_default(),
        person,
        (
            name_of(here, people, &shell),
            Rc::new(move |app: &str, item: &str| shell.shows_item(app, item)),
        ),
        waking(woken),
    )
}

/// The name the person's page knows `user` by: a person who is here, else one it has met, else
/// the one the workspace's governance keeps for them, so a page that cannot reach presence still
/// names who mentioned the person; else what a member nobody named is called.
fn name_of(
    here: &tilo_desktop::here::Here,
    people: &Rc<RefCell<HashMap<String, crate::desktop::PersonIdentity>>>,
    shell: &tilo_desktop_surface::DesktopHandle,
) -> tilo_session::mentions::Names {
    let seen = Rc::clone(here);
    let known = Rc::clone(people);
    let shell = shell.clone();
    Rc::new(move |user: &ui::MemberId| {
        let Some(person) = user.person_id() else {
            return ui::presence::unnamed_label(user);
        };
        seen.borrow()
            .person(person)
            .or_else(|| known.borrow().get(person).cloned())
            .map(|(name, _)| name)
            .or_else(|| {
                shell.read(|tree| {
                    tree.workspaces
                        .iter()
                        .find_map(|workspace| workspace.person_name(person))
                        .map(str::to_owned)
                })
            })
            .unwrap_or_else(|| ui::presence::unnamed_label(user))
    })
}

/// What tells the page it has something to read.
fn waking(
    woken: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::sync::Arc<dyn Fn() + Send + Sync> {
    let woken = std::sync::Arc::clone(woken);
    std::sync::Arc::new(move || {
        woken.store(true, std::sync::atomic::Ordering::Release);
    })
}
