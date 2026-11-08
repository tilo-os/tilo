//! The workspace directory in a page: the session's own sheet
//! ([`tilo_sheets::directory`]), drawn over the desktop, with what is this
//! page's own — the site whose directory it lists, the person a join is made
//! as, and the workspaces the page holds. Reads, joins and pasted links are
//! [`tilo_workspace_directory`]'s, over the page's `fetch`; what the person
//! picks is handed to the page as a [`HostRequest`], as the session's shell
//! hands the same effects to the desk.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use serde::Deserialize;
use tilo_sheets::Presentation;
use tilo_sheets::directory::{self, Ask, Directory, DirectoryEffect, Status};
use tilo_workspace_directory::{Held, Person, Store};

use ui::LocalState;

use crate::over_page::OverSheet;
use crate::shell::{HostRequest, HostRequests};

/// What the page tells its desktop about the workspace directory
/// (`mountDesktop({directory})`): a page has no `community.toml`, no identity
/// on disk and no desk of its own to read them from.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct PageDirectoryConfig {
    /// The site whose directory the sheet lists: the page's own entry.
    #[serde(default)]
    pub site: String,
    /// The person a join is made as, as the page keeps them; absent where the
    /// page has no person of its own yet.
    #[serde(default)]
    pub person: Option<PagePerson>,
    /// The workspaces this page holds, each named by the host action id that
    /// opens it.
    #[serde(default)]
    pub held: Vec<Held>,
}

/// The person a page joins a public workspace as.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PagePerson {
    /// Their root key, hex: the key a door admits.
    #[serde(rename = "userPublicKey")]
    pub user: String,
    /// The name a workspace learns of them.
    #[serde(default)]
    pub name: String,
}

/// The sheet over the page, while it is up.
pub(crate) struct PageDirectory {
    requests: HostRequests,
    store: Store,
    presentation: Presentation,
    query: LocalState<String>,
    asked: Rc<RefCell<Vec<DirectoryEffect>>>,
    /// The site whose directory the sheet lists.
    community: Option<url::Url>,
    person: Option<Person>,
    held: Vec<Held>,
    /// The origin of the door last asked: where a page redeems the invitation
    /// of a door that names no origin.
    door_origin: Option<String>,
    /// What the built sheet shows and the search it was built with, so it is
    /// built again only when either moved.
    built: (Directory, u64),
}

impl PageDirectory {
    pub(crate) fn new(
        requests: HostRequests,
        config: Option<&PageDirectoryConfig>,
        wake: &Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let community = config
            .map(|config| config.site.trim())
            .filter(|site| !site.is_empty())
            .and_then(|site| tilo_workspace_directory::site_entry(site).ok());
        let person = config
            .and_then(|config| config.person.as_ref())
            .and_then(|person| {
                let user: [u8; 32] = hex::decode(&person.user).ok()?.try_into().ok()?;
                Some(Person {
                    user,
                    name: person.name.clone(),
                })
            });
        let store = Store::default();
        store.waking(Arc::clone(wake));
        Self {
            requests,
            store,
            presentation: Presentation::default(),
            query: LocalState::new(String::new()),
            asked: Rc::default(),
            community,
            person,
            held: config.map(|config| config.held.clone()).unwrap_or_default(),
            door_origin: None,
            built: (Directory::default(), u64::MAX),
        }
    }

    /// Put the sheet up, reading the page's directory as the session's shell
    /// reads its community's.
    pub(crate) fn open(&self) {
        self.query.set(String::new());
        self.store.set_note(None);
        self.store.read(self.community.clone(), None);
        self.presentation.open(false);
    }

    pub(crate) fn is_open(&self) -> bool {
        self.presentation.is_open()
    }

    /// Take what the directory task answered, show it, and carry out what the
    /// person pressed since the last pass. Whether anything changed.
    fn follow(&mut self, show: &mut dyn FnMut(Option<ui::widget::Element>), missing: bool) -> bool {
        let mut changed = self.store.answered();
        for joined in self.store.take_joined() {
            changed = true;
            // A page joins no community of its own as it starts: a join it was
            // let into is the person's to open, not the page's.
            if joined.community.is_some() {
                continue;
            }
            let origin = joined
                .granted
                .origin
                .clone()
                .or_else(|| self.door_origin.clone())
                .unwrap_or_default();
            self.close();
            self.requests.borrow_mut().push(HostRequest::Joined {
                origin,
                invitation: joined.granted.invitation.clone(),
                admission_token: joined.granted.admission_token.clone(),
                workspace: joined.granted.workspace.clone(),
                name: joined.name.clone(),
            });
        }
        if self.presentation.is_open() {
            let shown = self.store.view(&self.held);
            let revision = self.query.revision();
            if missing || shown != self.built.0 || revision != self.built.1 {
                self.built = (shown.clone(), revision);
                // A page keeps no log of its own: a site that could not be
                // read says why here, where its people's consoles keep it.
                if let Status::Failed(error) = &shown.status {
                    web_sys::console::warn_1(&format!("workspace directory: {error}").into());
                }
                let asked = Rc::clone(&self.asked);
                let ask: Ask = Rc::new(move |effect: DirectoryEffect| {
                    asked.borrow_mut().push(effect);
                });
                show(Some(directory::sheet(&shown, &self.query, &ask, None)));
                changed = true;
            }
        }
        let effects: Vec<DirectoryEffect> = std::mem::take(&mut *self.asked.borrow_mut());
        for effect in effects {
            changed = true;
            self.answer(effect);
        }
        if !self.presentation.is_open() {
            show(None);
        }
        changed
    }

    /// Carry out one thing the person pressed, as the session's shell carries
    /// out the same effect for the desk.
    fn answer(&mut self, effect: DirectoryEffect) {
        match effect {
            DirectoryEffect::Join { key } => self.join_listed(&key),
            DirectoryEffect::Open { key } => {
                self.close();
                self.requests.borrow_mut().push(HostRequest::Action(key));
            }
            DirectoryEffect::Look { text } => self.store.read(None, Some(text)),
            DirectoryEffect::LinkOrCode { text } => {
                let taken = self
                    .person
                    .clone()
                    .ok_or_else(no_person)
                    .and_then(|person| self.store.take(&text, &person));
                if let Err(reason) = taken {
                    self.store.set_note(Some(reason));
                }
            }
            DirectoryEffect::Refresh => self.store.read(self.community.clone(), None),
            DirectoryEffect::ClearNote => self.store.set_note(None),
            DirectoryEffect::Done => self.close(),
        }
    }

    /// Join the listing the sheet knows as `key`, as this page's person: one
    /// join at a time, and the sheet says why one did not start.
    fn join_listed(&mut self, key: &str) {
        let Some(listing) = self.store.listing(key) else {
            self.store
                .set_note(Some(tilo_sheets::word("directory-listing-gone")));
            return;
        };
        match self.store.joining() {
            Some(joining) if joining == key => self.store.set_note(Some(tilo_sheets::words(
                "directory-already-joining",
                &[("name", &listing.entry.name)],
            ))),
            // One join at a time: the first is still on its way.
            Some(_) => {}
            None => {
                self.door_origin = listing
                    .door
                    .as_ref()
                    .map(|door| door.origin().ascii_serialization());
                let joined = self
                    .person
                    .clone()
                    .ok_or_else(no_person)
                    .and_then(|person| self.store.join(key, &listing, &person));
                if let Err(reason) = joined {
                    self.store.set_note(Some(reason));
                }
            }
        }
    }

    fn close(&self) {
        self.presentation.close();
    }
}

/// Why nothing was asked: a page that named no person joins nowhere as
/// anybody.
fn no_person() -> String {
    "this page has no identity of its own yet, so it cannot join a workspace".to_owned()
}

impl OverSheet for PageDirectory {
    fn follow_sheet(
        &mut self,
        show: &mut dyn FnMut(Option<ui::widget::Element>),
        missing: bool,
    ) -> bool {
        self.follow(show, missing)
    }

    fn drawn(&self) -> bool {
        self.presentation.is_drawn()
    }

    fn sheet_name(&self) -> &'static str {
        "PageDirectory"
    }
}
