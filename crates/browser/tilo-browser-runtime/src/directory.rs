//! The workspace directory in a page: the session's own role
//! ([`tilo_session::directory`]) and sheet, drawn over the desktop, with what
//! is this page's own — the site whose directory it lists, the person a join is
//! made as, and the workspaces the page holds. What the person picks is handed
//! to the page as a [`HostRequest`].

use std::sync::Arc;

use serde::Deserialize;
use tilo_sheets::Seat as _;
use tilo_sheets::directory::{Directory, DirectorySeat, Status};
use tilo_workspace_directory::{Held, Joined, Person, Store};

use crate::over_page::{OverSheet, PageSheet};
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

/// What the page is to the directory: its site, its person, the workspaces it
/// holds, and the page a join or an open is handed to.
struct PageHost {
    requests: HostRequests,
    community: Option<url::Url>,
    person: Option<Person>,
    held: Vec<Held>,
}

impl tilo_session::directory::Host for PageHost {
    fn site(&self) -> Option<url::Url> {
        self.community.clone()
    }

    fn person(&self) -> Result<Person, String> {
        // A page that named no person joins nowhere as anybody.
        self.person.clone().ok_or_else(|| {
            "this page has no identity of its own yet, so it cannot join a workspace".to_owned()
        })
    }

    fn held(&self) -> Vec<Held> {
        self.held.clone()
    }

    fn open(&self, key: &str) -> Result<(), String> {
        self.requests
            .borrow_mut()
            .push(HostRequest::Action(key.to_owned()));
        Ok(())
    }

    fn add(&self, joined: &Joined) -> Result<(), String> {
        let door = joined.granted.door();
        self.requests.borrow_mut().push(HostRequest::Joined {
            // A door's host is reached at the door's own origin.
            origin: door
                .map(|door| door.origin().ascii_serialization())
                .unwrap_or_default(),
            door: door.map(ToString::to_string),
            invitation: joined.granted.invitation().map(str::to_owned),
            workspace: joined.granted.workspace.clone(),
            name: joined.name.clone(),
        });
        Ok(())
    }
}

/// The sheet over the page, while it is up.
type Wake = Arc<dyn Fn() + Send + Sync>;

/// The page's directory sheet, as the sheets' code makes it
/// (`crate::over_page::SHEETS`).
pub(crate) fn make(
    requests: HostRequests,
    config: Option<&PageDirectoryConfig>,
    wake: &Wake,
) -> Box<dyn PageSheet> {
    Box::new(PageDirectory::new(requests, config, wake))
}

struct PageDirectory {
    host: PageHost,
    store: Store,
    seat: DirectorySeat,
    /// What the built sheet shows and the search it was built with, so it is
    /// built again only when either moved.
    built: (Directory, u64),
}

impl PageDirectory {
    fn new(requests: HostRequests, config: Option<&PageDirectoryConfig>, wake: &Wake) -> Self {
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
            host: PageHost {
                requests,
                community,
                person,
                held: config.map(|config| config.held.clone()).unwrap_or_default(),
            },
            store,
            seat: DirectorySeat::default(),
            built: (Directory::default(), u64::MAX),
        }
    }
}

impl PageSheet for PageDirectory {
    /// Put the sheet up, as the session's shell does.
    fn open(&self) {
        tilo_session::directory::open(Some(&self.seat), &self.store, &self.host);
    }

    fn is_open(&self) -> bool {
        self.seat.presentation().is_open()
    }
}

impl OverSheet for PageDirectory {
    fn follow_sheet(
        &mut self,
        show: &mut dyn FnMut(Option<ui::widget::Element>),
        missing: bool,
    ) -> bool {
        let before = (self.seat.directory(), self.is_open());
        tilo_session::directory::follow(
            Some(&self.seat),
            &self.store,
            &*crate::host_services::notices(),
            &self.host,
        );
        let mut changed = before != (self.seat.directory(), self.is_open());
        if self.is_open() {
            let shown = self.seat.directory();
            let revision = self.seat.query_revision();
            if missing || shown != self.built.0 || revision != self.built.1 {
                // A page keeps no log of its own: a site that could not be
                // read says why here, where its people's consoles keep it.
                if let Status::Failed(error) = &shown.status {
                    web_sys::console::warn_1(&format!("workspace directory: {error}").into());
                }
                self.built = (shown, revision);
                show(Some(self.seat.sheet()));
                changed = true;
            }
        } else {
            show(None);
        }
        changed
    }

    fn drawn(&self) -> bool {
        self.seat.presentation().is_drawn()
    }

    fn sheet_name(&self) -> &'static str {
        "PageDirectory"
    }
}
