//! The page's palette is the session's search: the one role a native desktop runs stands here too,
//! over what this page publishes.
//!
//! [`tilo_session::search::SessionSearch`] is the desktop's search beyond its own windows and
//! commands — what stands over the shell as published actions, Settings' pages, the apps this page
//! has, the records its workspaces hold, the question the person types, and the sources they
//! switched off — and the rows a host alone can carry out. The shell draws the palette and the row
//! the person chose ([`tilo_desktop_surface::SearchHost`]); this page hands over what only it
//! knows and takes the choices back to the shell it arranges.

use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::ApiError;
use tilo_actions::remote::{RemoteActionTarget, application_provider};
use tilo_desktop_search::SearchPreferences;
use tilo_desktop_search::Target;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_desktop_surface::session::Reveal;
use tilo_session::platform::apps::Apps;
use tilo_session::search::{AppActions, SearchOptions, SessionSearch};

/// The page's action host: a row that reaches an app's own window, which the page opens the app
/// for.
///
/// A native desktop's action host starts an app and delivers its actions into the process it
/// started. The page draws that app's window itself, so it asks the shell to bring the app's window
/// forward — opening it when the app is closed — and asks for the action to run in it once it is up,
/// as a window asking another app to show something does ([`super::session_asks`]).
struct PageActions {
    handle: DesktopHandle,
    reveals: crate::browser::Reveals,
    /// The content the page holds, which a workspace's files are by hash.
    content: tilo_content::ContentStore,
}

impl AppActions for PageActions {
    /// The page arranges its own windows: every other row is the shell's, which launches, focuses
    /// and opens addresses where this page does. A workspace's file by its content is the page's:
    /// only it holds the bytes that say which app opens the file.
    fn performs(&self, target: &Target) -> bool {
        matches!(target, Target::Invoke(_) | Target::Content { .. })
    }

    fn invoke(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ApiError> {
        let app = application_provider(&target.provider)
            .map(|(_, app)| app.to_owned())
            .ok_or_else(|| ApiError::Unavailable {
                code: "unknown_application".into(),
                reason: "That row names no app this page opens".into(),
            })?;
        if !self.showing(&app) {
            self.handle
                .dispatch(ChromeEffect::OpenApp {
                    app: app.clone(),
                    beside: None,
                    background: None,
                    float: None,
                })
                .map_err(|reason| ApiError::Unavailable {
                    code: "application_unavailable".into(),
                    reason,
                })?;
        }
        self.reveals.ask(Reveal {
            app,
            // The app's own record: the window it has of this workspace.
            record: String::new(),
            resource: target.address.resource.clone(),
            action: target.address.action,
            arguments,
            // The person chose the row: nothing waits for what it answers.
            answer: None,
        });
        Ok(())
    }

    /// Open the workspace's content whose hash is `hash` in the app its bytes say opens it: read
    /// where the page keeps the workspace's content, and asked of the workspace when it is not
    /// here yet.
    fn open_content(&self, hash: &str, name: &str) -> Result<(), ApiError> {
        let Some(bytes) = self.content.read(hash) else {
            return Err(ApiError::Unavailable {
                code: "content_on_its_way".into(),
                reason: format!("{name} is still coming from the workspace"),
            });
        };
        let head = bytes
            .get(..bytes.len().min(tilo_content_types::HEAD_BYTES))
            .unwrap_or(&bytes);
        let Some(app) = crate::dropped_files::opens_in(head, name) else {
            return Err(ApiError::Unavailable {
                code: "no_app".into(),
                reason: format!("No app here opens {name}"),
            });
        };
        self.handle
            .dispatch(ChromeEffect::OpenApp {
                app: tilo_desktop_surface::session::content_launch(&app, hash),
                beside: None,
                background: None,
                float: None,
            })
            .map_err(|reason| ApiError::Unavailable {
                code: "application_unavailable".into(),
                reason,
            })?;
        Ok(())
    }
}

impl PageActions {
    /// Whether a window of `app` stands on the shell.
    fn showing(&self, app: &str) -> bool {
        self.handle
            .read(|tree| tilo_desktop_surface::navigation::window_of_app(tree, app).is_some())
    }
}

/// The workspace's own search as this page's palette asks it, over the session the page holds
/// with the workspace.
pub(crate) struct WorkspaceSearch {
    /// The page whose session with the workspace's server answers: looked up
    /// for each ask, as a page that certifies its device again keeps its id
    /// while the session under it is replaced.
    pub peer: Option<u32>,
    pub workspace: String,
    /// What asks for the shell's pass that lists the host's answer.
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

/// The workspace server's search as the page reaches it now.
struct FollowingSearch {
    peer: Option<u32>,
    workspace: String,
}

impl FollowingSearch {
    fn search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        let peer = self.peer?;
        let workspace = crate::peer::workspace_id(&self.workspace).ok()?;
        tilo_actions::remote::ActionProvider::resource_search(&crate::peer_sync::actions_of(
            peer, workspace,
        )?)
    }
}

impl tilo_actions::remote::ResourceSearch for FollowingSearch {
    fn find(
        &self,
        caller: &tilo_actions::remote::Caller,
        needle: &str,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_actions::remote::Found>, tilo_actions::remote::ActionCallError>,
    > {
        match self.search() {
            Some(search) => search.find(caller, needle),
            None => tilo_actions::remote::Pending::ready(Err(crate::peer_sync::no_server())),
        }
    }

    fn find_within(
        &self,
        caller: &tilo_actions::remote::Caller,
        needle: &str,
        within: &tilo_actions::remote::Within,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_actions::remote::Found>, tilo_actions::remote::ActionCallError>,
    > {
        match self.search() {
            Some(search) => search.find_within(caller, needle, within),
            None => tilo_actions::remote::Pending::ready(Err(crate::peer_sync::no_server())),
        }
    }
}

/// What the page's search is mounted over: the bundle's apps as the apps installed here, the
/// records the workspace's indexes name as `store` holds them (what the page's replication took
/// in, noted in its one stream of record changes), the workspace's files, the person's own model on
/// their server as the model its questions run on, and the page as what shows the rows
/// that reach an app's window.
pub(crate) struct SearchParts {
    pub handle: DesktopHandle,
    pub person: String,
    /// The page's peer with the workspace's server, whose session asks the person's model.
    pub peer: Option<u32>,
    /// The workspace the page shows.
    pub workspace: String,
    pub store: tilo_session::platform::Store,
    pub content: tilo_content::ContentStore,
    pub files: Option<Rc<crate::page_files::WorkspaceFiles>>,
    pub host: Option<WorkspaceSearch>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub reveals: crate::browser::Reveals,
}

/// The session's search over this page's layout, mounted as a native desktop mounts it.
fn mount(parts: SearchParts) -> Rc<SessionSearch> {
    // The page holds the records of the apps it opened; the workspace's host holds the rest and
    // ranks them, over the session the page already holds with it.
    let served_here = parts.host.is_some();
    let host_search = parts.host.map(|served| {
        tilo_desktop_search::Held::new(tilo_desktop_search::providers::hosted::HostSearch::new(
            Arc::new(FollowingSearch {
                peer: served.peer,
                workspace: served.workspace.clone(),
            }),
            tilo_actions::remote::Caller::local_person(&parts.person),
            served.workspace,
            served.wake,
        ))
    });
    let search = Rc::new(SessionSearch::new(
        SearchOptions {
            preferences: SearchPreferences::default(),
            records: Some(tilo_session::records::Records::over(
                parts.store,
                crate::replicated::changes(),
                // What the documents hold is the workspace's own search to find, over every one
                // it keeps: the page's rows are their names, which its indexes hold, and reading
                // every document here would open the whole workspace each time one changed.
                !served_here,
            )),
            workspace_files: parts.files.map(|files| files.source()),
            host_search,
            ..SearchOptions::default()
        },
        Some(Arc::new(crate::page_apps::PageApps) as Arc<dyn Apps>),
        &parts.person,
        Some(Rc::new(PageActions {
            handle: parts.handle.clone(),
            reveals: parts.reveals,
            content: parts.content,
        }) as Rc<dyn AppActions>),
        Some(parts.wake),
        Some(Arc::new(PageGateway {
            peer: parts.peer,
            workspace: parts.workspace.clone(),
            person: parts.person.clone(),
        }) as Arc<dyn tilo_session::platform::Models>),
    ));
    parts
        .handle
        .set_search_host(Rc::clone(&search) as Rc<dyn tilo_desktop_surface::SearchHost>);
    search
}

/// What the page asks of its search, as the trait object [`PAGE_SEARCH`] makes.
pub(crate) trait PageSearchApi {
    fn set_workspaces(&self, workspaces: Vec<(String, String)>);
    fn observe(&self, resources: &[tilo_actions::ResourceSpec], tabs: Vec<u64>);
    fn perform_choices(
        &self,
        handle: &DesktopHandle,
        own: &mut dyn FnMut(&tilo_desktop_search::InvokeTarget) -> Result<(), ApiError>,
    ) -> Vec<(Target, ApiError)>;
}

impl PageSearchApi for SessionSearch {
    fn set_workspaces(&self, workspaces: Vec<(String, String)>) {
        Self::set_workspaces(self, workspaces);
    }

    fn observe(&self, resources: &[tilo_actions::ResourceSpec], tabs: Vec<u64>) {
        // A page bundles no programs and serves no notification centre of its own: the palette
        // lists neither.
        Self::observe(self, resources, tabs, None, Vec::new());
    }

    fn perform_choices(
        &self,
        handle: &DesktopHandle,
        own: &mut dyn FnMut(&tilo_desktop_search::InvokeTarget) -> Result<(), ApiError>,
    ) -> Vec<(Target, ApiError)> {
        Self::perform_choices(self, handle, own)
    }
}

/// The page's search code, as a program that fetches it on first use hands it over.
#[derive(Clone, Copy)]
pub struct MakePageSearch(fn(SearchParts) -> Rc<dyn PageSearchApi>);

/// The page's search this build links.
#[must_use]
pub fn builtin_page_search() -> MakePageSearch {
    MakePageSearch(|parts| mount(parts) as Rc<dyn PageSearchApi>)
}

ui::lazy_code! {
    /// The page's search: linked, or with `deferred-page-search` fetched as the page first draws.
    pub static PAGE_SEARCH: MakePageSearch = builtin_page_search, feature = "deferred-page-search";
}

/// The page's search, whose code may still be on its way: until it is in, the shell searches its
/// own windows and commands, and a search open when it arrives opens again over what it adds.
pub(crate) struct PageSearch {
    parts: std::cell::RefCell<Option<SearchParts>>,
    made: std::cell::OnceCell<Rc<dyn PageSearchApi>>,
    /// The layout's revision the search last read what it publishes at, and whether the layout's
    /// code was in then.
    pub seen: std::cell::Cell<Option<(u64, bool)>>,
}

impl PageSearch {
    pub(crate) fn new(parts: SearchParts) -> Self {
        Self {
            parts: std::cell::RefCell::new(Some(parts)),
            made: std::cell::OnceCell::new(),
            seen: std::cell::Cell::default(),
        }
    }

    /// The search, mounted once its code is in.
    pub(crate) fn get(&self) -> Option<&dyn PageSearchApi> {
        if self.made.get().is_none() {
            let code = PAGE_SEARCH.get()?;
            let parts = self.parts.borrow_mut().take()?;
            let _ = self.made.set((code.0)(parts));
        }
        self.made.get().map(|search| &**search)
    }
}

/// The person's own model on their server, as the session's ask reaches it: its models
/// hold the page's own handles, so each question builds the capability where it is asked. A page
/// with no server has none.
struct PageGateway {
    peer: Option<u32>,
    workspace: String,
    person: String,
}

impl tilo_session::platform::Models for PageGateway {
    fn models(&self) -> Option<ui::models::Models> {
        let models = crate::host_services::server_models(self.peer, &self.workspace, &self.person);
        models.is_available().then_some(models)
    }
}
