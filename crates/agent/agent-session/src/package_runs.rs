//! How a device that holds a workspace runs the workspace's tilolang packages for its pages:
//! one process per workspace and app ([`PageLaunch::start_page`]), which serves every page open
//! on the app.
//! The process lays a page's view out itself and answers show and act on its own connection
//! ([`tilo_server_protocol::page_window`]); this asks it and answers the page's person.
//!
//! Each page has its own connection, made by the device's launch services
//! ([`PageLaunch`]), which the device's broker believes as that page of the app. A page whose
//! person signed their call hands the call with each ask: the process writes the change as them,
//! under the invocation they signed, and every holder checks it.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{Caller, SignedCall};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::page::{PageAction, PagePatch, PageRefusal};
use tilo_server_protocol::page_window::{self, Answer, Ask};

/// What a device that runs packages tells its broker and its services about a page it makes.
pub trait PageLaunch: Send + Sync {
    /// The apps of the workspace this device runs pages of, each its id and the name people call
    /// it.
    fn apps(&self, workspace: &str) -> Vec<(String, String)>;

    /// Whether the workspace admits `caller` to read its apps: only a member is shown a page.
    fn admits(&self, workspace: &str, caller: &Caller) -> bool;

    /// The collaboration connection of the page `tab` of `app` in `workspace`, which the device's
    /// broker believes as that page alone, acting as `caller`.
    fn collaboration(
        &self,
        app: &str,
        workspace: &str,
        tab: u64,
        caller: &Caller,
    ) -> Option<OwnedFd>;

    /// Start, or reuse, the workspace's process of `app` and hand it the page `page.patch` is
    /// one end of: the process lays the view out and answers show and act there.
    ///
    /// # Errors
    /// Why the process does not start here.
    fn start_page(&self, page: StartPage<'_>) -> Result<(), String>;
}

/// One page a device starts in an app's process: the process's end of the page's connection and
/// the connections it is made with.
pub struct StartPage<'a> {
    pub app: &'a str,
    pub workspace: &'a str,
    /// The tab the page takes, as the process names it.
    pub tab: u64,
    /// The member the page is shown to, whose steps the process's writes are theirs.
    pub person: &'a str,
    pub patch: OwnedFd,
    pub collaboration: OwnedFd,
    pub identity: OwnedFd,
    pub content: Option<OwnedFd>,
}

/// One page a package's process laid out: what it shows, and the handle its person's acts go
/// through.
pub struct Opened {
    pub title: String,
    pub css: String,
    pub body: String,
    pub page: Arc<dyn PackagePage>,
}

/// One page open in a package's process, asked on its connection.
pub trait PackagePage: Send + Sync {
    /// Act on it as its person did, or with no act, answer what changed on it since: the elements
    /// that changed, outermost first.
    fn act(
        &self,
        act: Option<(u32, Option<String>, PageAction)>,
        signed: Option<Box<SignedCall>>,
        invoker: Option<String>,
    ) -> Result<Vec<PagePatch>, PageRefusal>;
}

/// The runs of one workspace's packages on this device.
pub struct PackageRuns {
    workspace: String,
    /// Where the device's broker and services are reached, and the apps it runs pages of.
    launch: Arc<dyn PageLaunch>,
    /// The person at this device, whose the process of a page is.
    person: String,
    tabs: Arc<AtomicU64>,
}

impl PackageRuns {
    #[must_use]
    pub fn new(
        workspace: impl Into<String>,
        launch: Arc<dyn PageLaunch>,
        person: impl Into<String>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            launch,
            person: person.into(),
            tabs: Arc::new(AtomicU64::new(1)),
        }
    }

    /// The apps of `workspace` the device runs pages of, each its id and the name people call it.
    #[must_use]
    pub fn apps(&self, workspace: WorkspaceId) -> Vec<(String, String)> {
        if workspace.to_string() != self.workspace {
            return Vec::new();
        }
        self.launch.apps(&self.workspace)
    }

    /// Open a page of `app` for `caller`, asked for on `connection`'s other end: the app's view
    /// runs in the workspace's process of the app, started when none runs, and lays its window
    /// out there. The page shows what `show` names first.
    ///
    /// # Errors
    /// No process can start here, the package does not verify, or the page's connections cannot
    /// be made.
    pub fn open(
        &self,
        workspace: WorkspaceId,
        app: &str,
        caller: &Caller,
        show: Option<String>,
        signed: Option<Box<SignedCall>>,
        invoker: Option<String>,
    ) -> Result<Opened, PageRefusal> {
        if workspace.to_string() != self.workspace {
            return Err(PageRefusal::Elsewhere);
        }
        if !self.launch.admits(&self.workspace, caller) {
            return Err(PageRefusal::NotMember);
        }
        let tab = self.tabs.fetch_add(1, Ordering::Relaxed);
        let (ours, theirs) = UnixStream::pair().map_err(unavailable)?;
        let collaboration = self
            .launch
            .collaboration(app, &self.workspace, tab, caller)
            .ok_or_else(|| {
                PageRefusal::Unavailable(format!("{app} could not reach its workspace"))
            })?;
        // The identity service and the content store, opened and scoped for the app in its
        // workspace exactly as the launcher opens them for a launched app's.
        let identity = launcher::services::resolve_identity_bind(app, Some(&self.workspace), false)
            .map_err(|error| PageRefusal::Unavailable(format!("the identity service: {error}")))?
            .ok_or_else(|| PageRefusal::Unavailable("the identity service is not running".into()))?
            .host;
        let content = launcher::services::resolve_blob_bind(app, Some(&self.workspace), false)
            .ok()
            .flatten()
            .map(|bind| bind.host);
        self.launch
            .start_page(StartPage {
                app,
                workspace: &self.workspace,
                tab,
                person: &self.person,
                patch: OwnedFd::from(theirs),
                collaboration,
                identity,
                content,
            })
            .map_err(PageRefusal::Unavailable)?;
        let page = Arc::new(Open {
            connection: Mutex::new(ours),
            app: app.to_owned(),
        });
        let shown = page.ask(Ask::Show {
            show,
            signed,
            invoker,
        })?;
        let Answer::Shown { css, body } = shown else {
            return Err(PageRefusal::Unavailable(
                "the app's page was not shown".to_owned(),
            ));
        };
        Ok(Opened {
            // The page's own title is the app's name in the catalogue that lists it; the
            // address is what this process knows of it.
            title: app.to_owned(),
            css,
            body,
            page,
        })
    }
}

/// Why the page cannot be had: the error for logs.
fn unavailable(error: impl std::fmt::Display) -> PageRefusal {
    PageRefusal::Unavailable(error.to_string())
}

/// One page open in the app's process: the service's thread asks its connection, one ask at a
/// time, and the process answers each before the next.
struct Open {
    connection: Mutex<UnixStream>,
    app: String,
}

impl Open {
    /// `ask` asked on the page's connection, and the answer read before another ask is taken.
    fn ask(&self, ask: Ask) -> Result<Answer, PageRefusal> {
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        page_window::write(&mut *connection, &ask)
            .and_then(|()| page_window::read(&mut *connection))
            .map_err(|error| unavailable(format!("{}'s page: {error}", self.app)))
    }
}

impl PackagePage for Open {
    fn act(
        &self,
        act: Option<(u32, Option<String>, PageAction)>,
        signed: Option<Box<SignedCall>>,
        invoker: Option<String>,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let ask = match act {
            Some((id, target, action)) => Ask::Act {
                id,
                target,
                action,
                signed,
                invoker,
            },
            None => Ask::Changes,
        };
        self.ask(ask).and_then(|answer| match answer {
            Answer::Patched(patches) => Ok(patches),
            Answer::Shown { .. } => Ok(Vec::new()),
            Answer::Failed(reason) => Err(unavailable(reason)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "6f1d2c3b-4a59-4e7f-8a1b-2c3d4e5f6a7b";

    /// A device whose workspace admits one key, and whose broker refuses every connection.
    struct Device([u8; 32]);

    impl PageLaunch for Device {
        fn apps(&self, workspace: &str) -> Vec<(String, String)> {
            (workspace == WORKSPACE)
                .then(|| vec![("so.tilo.notes".to_owned(), "Notes".to_owned())])
                .unwrap_or_default()
        }

        fn admits(&self, workspace: &str, caller: &Caller) -> bool {
            workspace == WORKSPACE && caller.user_public_key() == Some(&self.0)
        }

        fn collaboration(&self, _: &str, _: &str, _: u64, _: &Caller) -> Option<OwnedFd> {
            None
        }

        fn start_page(&self, _: StartPage<'_>) -> Result<(), String> {
            Err("no process starts in this test".to_owned())
        }
    }

    #[test]
    fn a_caller_the_workspace_does_not_admit_is_refused_before_a_process_starts() {
        let runs = PackageRuns::new(WORKSPACE, Arc::new(Device([7; 32])), "ada");
        assert_eq!(
            runs.open(
                WorkspaceId::parse(WORKSPACE).unwrap(),
                "so.tilo.notes",
                &Caller::authenticated([9; 32], [0; 32]),
                None,
                None,
                None
            )
            .err(),
            Some(PageRefusal::NotMember)
        );
    }
}
