//! A workspace's apps as web pages, served by the device that holds it (`/w/<workspace>/...`).
//!
//! An app's page is a view the device opens on the app's own process for whoever opened the
//! page, as any window is shown elsewhere ([`PageViews`]): its window lays the view out and
//! answers the page's markup, and what the person does on the page comes back to it as the
//! view's semantic actions. The device keeps no copy of what the page shows.
//!
//! The workspace's desktop (`/w/<workspace>/desktop`) is a view of the shell (`so.tilo.shell`)
//! opened the same way: the layout the workspace's people share, read and changed through the
//! layout's actions as the page's person. Where the shell leaves a window to the device that
//! shows it, the page shows a link to that window's own page ([`DesktopWells`]).
//!
//! The workspace's front page and its search are the device's own pages, and everything they
//! show is asked of the device's dispatch as the page's person: the apps and actions they may
//! use from its catalogue, what the workspace holds from its search, and who is here from its
//! presence. A row's action runs as any caller's invocation does, admitted where every other
//! one is.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use runtime_collab::native_media::{Request, Snapshot};
use tilo_actions::remote::{
    Caller, CatalogueQuery, FOUND_FILE, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionTarget, Within, pending,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::page::window::{WindowAsk, WindowLink};
use tilo_server_protocol::page::{
    CallAnswer, DESKTOP, OpenedPage, PageAction, PageAddress, PagePatch, PageRefusal, encode_part,
};
use tilo_workspace_service::app_serving::{AppPages, PageAct};
use ui::composition::page::escape;

use super::{SessionHost, Sessions};
use collab_types::WorkspaceId;

/// The app name a page address carries for the workspace's search
/// (`/w/<workspace>/search/<words>`): not an app but the system's own search over what the
/// workspace holds.
const SEARCH_APP: &str = "search";

/// How long a page waits on the dispatch or on its window before it says the page is
/// unavailable.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// The windows a device shows on web pages: a view of an app, opened for the page's person.
pub trait PageViews: Send + Sync {
    /// A window of the app `app` (its id, among the apps the catalogue lists `caller`) in
    /// `workspace`, shown for `caller` and acting as them, started as any window of the app is.
    ///
    /// # Errors
    /// The workspace has no such app, or its window could not be started.
    fn open(&self, workspace: &str, app: &str, caller: &Caller) -> Result<WindowLink, PageRefusal>;
}

/// The pages a device serves, each by its number.
#[derive(Clone, Default)]
pub struct Pages(Arc<Inner>);

#[derive(Default)]
struct Inner {
    sessions: OnceLock<Arc<dyn Sessions>>,
    /// Whose keys this device holds revoked: what a call a page's reader signed is checked
    /// against.
    devices: OnceLock<Arc<crate::personal_devices::PersonalDevices>>,
    views: OnceLock<Arc<dyn PageViews>>,
    open: Mutex<HashMap<u64, Arc<Mutex<Held>>>>,
    /// How a page's view reaches the browser that shows it when it asks for a call: what the
    /// view's device cannot carry itself, because the microphone, camera and the workspace's
    /// live session are in the browser.
    calls: Mutex<CallRoutes>,
    /// Woken when a view queues a call step, so a page's stream tells the browser at once.
    calls_woken: Arc<tokio::sync::Notify>,
    next: AtomicU64,
}

/// Where each page's call steps go: the pages whose views show a call, the steps their views
/// asked for and not yet handed to the browser, and who waits for each answer.
#[derive(Default)]
struct CallRoutes {
    /// By page number, what a call's request names the page by.
    routes: HashMap<u64, CallRoute>,
    /// The answer a step waits for, by the page's number and the step's request id.
    waiting: HashMap<(u64, u64), tilo_actions::remote::Completer<Result<Snapshot, String>>>,
    /// The number the next step of a page is asked by.
    next: u64,
}

struct CallRoute {
    /// The workspace's id, as a call's request names it.
    workspace: WorkspaceId,
    /// The app's full id (`so.tilo.chat`), as a call's request names it.
    app: String,
    /// The person the page was opened for, as a call's request names the view's caller.
    person: String,
    /// The steps the view asked for and the browser has not been given yet.
    steps: Vec<(u64, Request)>,
}

enum Held {
    /// An app's window, shown on the page: the app it shows and the workspace it is in, which a
    /// call its reader signed names.
    App {
        window: WindowLink,
        workspace: String,
        app: String,
        /// What the page shows where the view leaves a window to the device: the desktop's.
        wells: Option<DesktopWells>,
    },
    /// The workspace's front page: it changes only when opened again.
    Home,
    Search(Box<Search>),
}

impl Pages {
    /// Ask the workspaces `sessions` answers for from now on.
    pub(super) fn serve(
        &self,
        sessions: Arc<dyn Sessions>,
        devices: Arc<crate::personal_devices::PersonalDevices>,
    ) {
        let _ = self.0.sessions.set(sessions);
        let _ = self.0.devices.set(devices);
    }

    /// Show apps' views through `views` from now on. A second call is ignored.
    pub fn show_through(&self, views: Arc<dyn PageViews>) {
        let _ = self.0.views.set(views);
    }

    /// Ask the browser that shows the page of `app` in `workspace` for `person` to carry
    /// `request`: what to wait on for its answer, or none when no page shows that view. A view
    /// laid out where the browser is not, whose page shows a call, has the browser carry it.
    pub fn ask_call(
        &self,
        workspace: &str,
        app: &str,
        person: &str,
        request: Request,
    ) -> Option<tilo_actions::remote::Pending<Result<Snapshot, String>>> {
        self.0.ask_call(workspace, app, person, request)
    }
}

impl Inner {
    /// The workspace `workspace_id` names on this device, and its dispatch.
    fn dispatch(&self, workspace_id: WorkspaceId) -> Result<(String, SessionHost), PageRefusal> {
        let sessions = self
            .sessions
            .get()
            .ok_or_else(|| unavailable("this device's actions are not running yet"))?;
        let workspace = sessions
            .workspace(workspace_id)
            .ok_or(PageRefusal::Elsewhere)?;
        let dispatch = sessions
            .dispatch(&workspace)
            .ok_or_else(|| unavailable("this device answers no actions in that workspace"))?;
        Ok((workspace, dispatch))
    }

    fn hold(&self, held: Held) -> u64 {
        let page = self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.open
            .lock_recover()
            .insert(page, Arc::new(Mutex::new(held)));
        page
    }

    fn open_page(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        address: &PageAddress,
    ) -> Result<OpenedPage, PageRefusal> {
        let (workspace, dispatch) = self.dispatch(workspace_id)?;
        if address.is_home() {
            let (title, css, body) = home(workspace_id, &dispatch, caller)?;
            let page = self.hold(Held::Home);
            return Ok(OpenedPage {
                page,
                title,
                css,
                body,
            });
        }
        if address.app == SEARCH_APP {
            let mut search = Search {
                workspace_id,
                dispatch,
                caller: caller.clone(),
                rows: Vec::new(),
                notes: None,
                notice: None,
                asking: None,
                pending: None,
            };
            let needle = address.show.as_deref().unwrap_or_default();
            search.find(needle);
            let body = search_body(needle, &search.results());
            let page = self.hold(Held::Search(Box::new(search)));
            return Ok(OpenedPage {
                page,
                title: "Search".to_owned(),
                css: page_css() + HOME_CSS + SEARCH_CSS,
                body,
            });
        }
        let views = self
            .views
            .get()
            .ok_or_else(|| unavailable("this device shows no app's window on a page"))?;
        let desktop = address.app == DESKTOP;
        let app_id = if desktop {
            tilo_workspace_service::apps::shell::SHELL_APP.to_owned()
        } else {
            app_named(&dispatch, caller, &address.app)?
        };
        let mut window = views.open(&workspace, &app_id, caller)?;
        let mut wells = desktop.then(|| DesktopWells {
            workspace_id,
            dispatch: dispatch.clone(),
            caller: caller.clone(),
            shown: None,
        });
        if let Some(wells) = &mut wells {
            wells.follow(&mut window)?;
        }
        let (title, css, body) = window.open(address.show.clone()).map_err(unavailable)?;
        let page = self.hold(Held::App {
            window,
            workspace,
            app: address.app.clone(),
            wells,
        });
        self.route_call(page, workspace_id, &app_id, caller);
        Ok(OpenedPage {
            page,
            title,
            css,
            body,
        })
    }

    fn act(&self, page: u64, act: Option<PageAct>) -> Result<Vec<PagePatch>, PageRefusal> {
        let held = self
            .open
            .lock_recover()
            .get(&page)
            .cloned()
            .ok_or_else(|| unavailable("the page was closed"))?;
        let mut held = held.lock_recover();
        match (&mut *held, act) {
            (
                Held::App {
                    window,
                    workspace,
                    app,
                    wells,
                },
                Some(act),
            ) => {
                let mut patches = match wells {
                    Some(wells) => wells.follow(window)?,
                    None => Vec::new(),
                };
                let invoker = act
                    .signed
                    .as_deref()
                    .map(|call| {
                        let asked = (act.id, act.target.as_deref(), &act.action);
                        self.signer(call, workspace, app, asked)
                    })
                    .transpose()?;
                patches.extend(
                    window
                        .act(&WindowAsk::Act {
                            id: act.id,
                            target: act.target,
                            action: act.action,
                            signed: act.signed,
                            invoker,
                        })
                        .map_err(unavailable)?,
                );
                Ok(patches)
            }
            (Held::App { window, wells, .. }, None) => {
                let mut patches = match wells {
                    Some(wells) => wells.follow(window)?,
                    None => Vec::new(),
                };
                patches.extend(window.changed().map_err(unavailable)?);
                Ok(patches)
            }
            (Held::Home, _) => Ok(Vec::new()),
            (Held::Search(search), act) => Ok(search.act(act)),
        }
    }

    /// The member who signed `call` for an act on the page of `app` in `workspace`, as a change's
    /// stamp names its invoker, once the call checks: a call that does not is refused, and the
    /// act runs nowhere.
    fn signer(
        &self,
        call: &tilo_actions::remote::SignedCall,
        workspace: &str,
        app: &str,
        asked: (u32, Option<&str>, &PageAction),
    ) -> Result<String, PageRefusal> {
        let revocations = self
            .devices
            .get()
            .map(|devices| devices.revocations())
            .ok_or_else(|| unavailable("this device's actions are not running yet"))?;
        let cert = tilo_server_protocol::invocation::verify_page_call(
            call,
            workspace,
            app,
            asked,
            &revocations,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| unavailable(format!("the act's signed call does not check: {error}")))?;
        let person = tilo_action_policy::MemberId::person(hex::encode(cert.user_public_key));
        Ok(collab_types::Stamp::by(&tilo_action_policy::Actor::member(
            person,
        )))
    }

    /// Point the call steps of `page` at the browser that shows it: a view's call is the
    /// browser's, since its microphone, camera and screen are there.
    fn route_call(&self, page: u64, workspace: WorkspaceId, app: &str, caller: &Caller) {
        let acting = caller.acting();
        let person = acting
            .person_id()
            .or_else(|| acting.acted_for_person())
            .map_or_else(|| acting.by.to_string(), ToOwned::to_owned);
        self.calls.lock_recover().routes.insert(
            page,
            CallRoute {
                workspace,
                app: app.to_owned(),
                person,
                steps: Vec::new(),
            },
        );
    }

    /// The call steps the view of `page` asked for since the browser was last given them.
    fn take_calls(&self, page: u64) -> Vec<(u64, Request)> {
        self.calls
            .lock_recover()
            .routes
            .get_mut(&page)
            .map(|route| std::mem::take(&mut route.steps))
            .unwrap_or_default()
    }

    /// Ask the browser that shows the page of `app` in `workspace` for `person` to carry
    /// `request`: what to wait on for its answer, or `None` when no page shows that view.
    fn ask_call(
        &self,
        workspace: &str,
        app: &str,
        person: &str,
        request: Request,
    ) -> Option<tilo_actions::remote::Pending<Result<Snapshot, String>>> {
        let mut calls = self.calls.lock_recover();
        let asked = WorkspaceId::parse(workspace);
        let page = calls
            .routes
            .iter()
            .filter(|(_, route)| {
                Some(route.workspace) == asked && route.app == app && route.person == person
            })
            .map(|(page, _)| *page)
            .max()?;
        let request_id = calls.next;
        calls.next = calls.next.saturating_add(1);
        if let Some(route) = calls.routes.get_mut(&page) {
            route.steps.push((request_id, request));
        }
        let (done, wait) = tilo_actions::remote::pending();
        calls.waiting.insert((page, request_id), done);
        drop(calls);
        // The streams waiting now, and one still getting there: a step queued between the last
        // drain and the next wait must not go unheard.
        self.calls_woken.notify_waiters();
        self.calls_woken.notify_one();
        Some(wait)
    }

    /// What the browser answered to the step `request_id` of `page`.
    fn answer_call(&self, page: u64, request_id: u64, answer: CallAnswer) {
        let done = self
            .calls
            .lock_recover()
            .waiting
            .remove(&(page, request_id));
        if let Some(done) = done {
            done.complete(match answer {
                CallAnswer::Answered(snapshot) => Ok(snapshot),
                CallAnswer::Refused(why) => Err(why),
            });
        }
    }

    /// Let go of what `page`'s view asked of its browser, answering each as unanswered: its
    /// page is closed.
    fn forget_page(&self, page: u64) {
        let mut calls = self.calls.lock_recover();
        calls.routes.remove(&page);
        let waiting: Vec<(u64, u64)> = calls
            .waiting
            .keys()
            .filter(|(of, _)| *of == page)
            .copied()
            .collect();
        for key in waiting {
            if let Some(done) = calls.waiting.remove(&key) {
                done.complete(Err("the page closed".to_owned()));
            }
        }
    }
}

/// The links a desktop page stands where its shell leaves a window to the device: each window's
/// own page, on the record it shows, for the apps the page's person may open. Read again from
/// the layout's actions as the page's person before each answer, so a window someone else
/// opened anywhere leads to its page here too.
struct DesktopWells {
    workspace_id: WorkspaceId,
    dispatch: SessionHost,
    caller: Caller,
    /// What the page's window was last told.
    shown: Option<std::collections::BTreeMap<String, String>>,
}

impl DesktopWells {
    /// Tell `window` the wells as the layout has them now, when they moved: what that changed.
    fn follow(&mut self, window: &mut WindowLink) -> Result<Vec<PagePatch>, PageRefusal> {
        let wells = self.read()?;
        if self.shown.as_ref() == Some(&wells) {
            return Ok(Vec::new());
        }
        let patches = window.wells(wells.clone()).map_err(unavailable)?;
        self.shown = Some(wells);
        Ok(patches)
    }

    /// Each window on the layout, by its key, as the link to its own page.
    fn read(&self) -> Result<std::collections::BTreeMap<String, String>, PageRefusal> {
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: crate::device::layout::PROVIDER.to_owned(),
                address: ActionAddress::new(crate::device::layout::RESOURCE, "read"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = waited(self.dispatch.invoke(&self.caller, request))?.map_err(unavailable)?;
        let read = match receipt.status {
            ReceiptStatus::Completed { output } => output.value.unwrap_or_default(),
            ReceiptStatus::Rejected { error } => return Err(unavailable(error)),
            ReceiptStatus::Failed { error } => return Err(unavailable(error.message)),
            ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
                return Err(unavailable("the layout was not read"));
            }
        };
        let listed: Vec<String> = catalogue(&self.dispatch, &self.caller)?
            .providers
            .into_iter()
            .map(|provider| provider.app_id)
            .collect();
        let mut wells = std::collections::BTreeMap::new();
        for window in read["windows"].as_array().into_iter().flatten() {
            let field = |name: &str| window[name].as_str().unwrap_or_default();
            let (key, app, title, record) = (
                field("key"),
                field("app"),
                field("app_title"),
                field("record"),
            );
            if key.is_empty() || !listed.iter().any(|listed| listed == app) {
                continue;
            }
            let href = PageAddress {
                workspace: self.workspace_id.to_string(),
                app: page_app_name(app).to_owned(),
                show: (!record.is_empty()).then(|| record.to_owned()),
            }
            .path();
            wells.insert(
                key.to_owned(),
                format!(
                    "<a class=\"tilo-canvas-open\" href=\"{}\">Open {}</a>",
                    escape(&href),
                    escape(title)
                ),
            );
        }
        Ok(wells)
    }
}

/// Run `work` off the page's own thread: a window and the dispatch answer in their own time.
fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Pending<T> {
    let (done, answer) = pending();
    std::thread::spawn(move || done.complete(work()));
    answer
}

impl AppPages for Pages {
    fn open(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        address: PageAddress,
    ) -> Pending<Result<OpenedPage, PageRefusal>> {
        let inner = Arc::clone(&self.0);
        let caller = caller.clone();
        off_thread(move || inner.open_page(workspace_id, &caller, &address))
    }

    fn act(
        &self,
        _workspace_id: WorkspaceId,
        page: u64,
        act: Option<PageAct>,
    ) -> Pending<Result<Vec<PagePatch>, PageRefusal>> {
        let inner = Arc::clone(&self.0);
        off_thread(move || inner.act(page, act))
    }

    fn close(&self, _workspace_id: WorkspaceId, page: u64) {
        // The window's link closes with it, and the window goes.
        self.0.open.lock_recover().remove(&page);
        self.0.forget_page(page);
    }

    fn take_calls(&self, _workspace_id: WorkspaceId, page: u64) -> Vec<(u64, Request)> {
        self.0.take_calls(page)
    }

    fn answer_call(&self, page: u64, request_id: u64, answer: CallAnswer) {
        self.0.answer_call(page, request_id, answer);
    }

    fn calls_waker(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(Arc::clone(&self.0.calls_woken))
    }
}

fn unavailable(error: impl std::fmt::Display) -> PageRefusal {
    PageRefusal::Unavailable(error.to_string())
}

/// What `answer` settles to, or why the page waited on it in vain.
fn waited<T>(answer: Pending<T>) -> Result<T, PageRefusal> {
    match answer.wait_timeout(ANSWER_WITHIN) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(unavailable("the device dropped the page's question")),
        Err(_) => Err(unavailable("the device did not answer in time")),
    }
}

/// The stylesheet every page of the device's own starts from: the theme's and the shared page
/// rules.
fn page_css() -> String {
    ui::composition::page::theme_css(&ui::Theme::default()) + ui::composition::page::PAGE_CSS
}

/// An app's page name: the last part of its id (`notes` for `so.tilo.notes`).
fn page_app_name(app_id: &str) -> &str {
    app_id.rsplit_once('.').map_or(app_id, |(_, last)| last)
}

/// What the workspace's catalogue lists `caller`: the apps they may use, and their records.
fn catalogue(
    dispatch: &SessionHost,
    caller: &Caller,
) -> Result<tilo_actions::remote::CataloguePage, PageRefusal> {
    waited(dispatch.query(
        caller,
        CatalogueQuery {
            max_results: 200,
            ..CatalogueQuery::default()
        },
    ))?
    .map_err(unavailable)
}

/// The apps `caller` may open in the workspace, as `(title, page name)`, by title: each app
/// whose records the catalogue lists them, named as its own top resource is.
fn apps(dispatch: &SessionHost, caller: &Caller) -> Result<Vec<(String, String)>, PageRefusal> {
    let listed = catalogue(dispatch, caller)?;
    let mut apps: Vec<(String, String)> = Vec::new();
    for provider in &listed.providers {
        let name = page_app_name(&provider.app_id);
        if name.is_empty() || apps.iter().any(|(_, known)| known == name) {
            continue;
        }
        let title = listed
            .rows
            .iter()
            .find(|row| row.app_id == provider.app_id && row.parent_resource.is_empty())
            .map_or_else(|| field_title(name), |row| row.resource.label.clone());
        apps.push((title, name.to_owned()));
    }
    apps.sort();
    Ok(apps)
}

/// The id of the app a page names (`notes`, or its whole id), among the apps `caller` may open
/// in the workspace: the catalogue lists an app's records to whoever may use it.
fn app_named(dispatch: &SessionHost, caller: &Caller, name: &str) -> Result<String, PageRefusal> {
    catalogue(dispatch, caller)?
        .providers
        .into_iter()
        .map(|provider| provider.app_id)
        .find(|app_id| app_id == name || page_app_name(app_id) == name)
        .ok_or(PageRefusal::NoSuchApp)
}

/// The workspace's front page: its way into search, who is here and what they work in, and
/// the apps a member opens, each a link to its own page.
fn home(
    workspace_id: WorkspaceId,
    dispatch: &SessionHost,
    caller: &Caller,
) -> Result<(String, String, String), PageRefusal> {
    let apps = apps(dispatch, caller)?;
    let mut here = dispatch.here(caller).unwrap_or_default();
    here.sort_by(|a, b| a.name.cmp(&b.name));
    let mut body = format!(
        "<aside class=\"tilo-view tilo-home\"><h1 class=\"tilo-home-title\">Tilo</h1>\
         <a class=\"tilo-home-search\" href=\"/w/{workspace_id}/{SEARCH_APP}\" \
         aria-label=\"Search the workspace\">Search apps, actions and records</a>\
         <a class=\"tilo-home-desktop\" href=\"/w/{workspace_id}/{DESKTOP}\">Desktop</a>"
    );
    // Who is here is said only when the presence says someone is: a page that claims "0
    // here" of a workspace someone is reading is wrong about the one person it can see.
    let count = if here.is_empty() {
        "People".to_owned()
    } else {
        format!("{} here", here.len())
    };
    let _ = write!(
        body,
        "<section class=\"tilo-people\" aria-label=\"People\"><h2 class=\"tilo-people-count\">\
         {count}</h2><ul class=\"tilo-people-list\">"
    );
    for member in &here {
        let what = member.works_in.as_ref().map_or_else(String::new, |window| {
            format!(
                "<span class=\"tilo-person-what\">{}</span>",
                escape(&field_title(page_app_name(&window.app)))
            )
        });
        let _ = write!(
            body,
            "<li class=\"tilo-person\" data-present><span class=\"tilo-person-name\">{name}\
             </span><span class=\"tilo-person-kind\">{kind}</span>{what}</li>",
            name = escape(&member.name),
            kind = escape(&member.kind),
        );
    }
    body.push_str("</ul></section><ul class=\"tilo-home-apps\">");
    for (title, name) in &apps {
        let _ = write!(
            body,
            "<li><a class=\"tilo-home-app\" href=\"/w/{workspace_id}/{name}\">{icon}\
             <span>{title}</span></a></li>",
            icon = initial_tile(title),
            name = escape(name),
            title = escape(title),
        );
    }
    body.push_str("</ul></aside>");
    Ok(("Tilo".to_owned(), page_css() + HOME_CSS, body))
}

/// An app's first letter, on the tile its icon stands on.
fn initial_tile(title: &str) -> String {
    let initial = title
        .chars()
        .next()
        .map(|first| first.to_uppercase().to_string());
    format!(
        "<span class=\"tilo-home-initial\">{}</span>",
        escape(initial.as_deref().unwrap_or(""))
    )
}

/// The element number the search results stand at: what a field's keystroke patches in place,
/// under the field the person keeps typing in.
const SEARCH_RESULTS: u32 = 2;

/// The most result rows one search page shows: the field is for finding a thing, not for
/// reading the workspace whole.
const MOST_ROWS: usize = 40;

/// The element number the first result row stands at; the field and the results stand before
/// it.
const SEARCH_FIRST_ROW: u32 = 10;

/// The element numbers of the button that runs the action a row asks fields for, and of the
/// one that puts it away.
const SEARCH_FORM_RUN: u32 = 3;
const SEARCH_FORM_CANCEL: u32 = 4;

/// The element number of that action's first field: past every row.
const SEARCH_FIRST_FIELD: u32 = 1 << 20;

/// The workspace's search page, as its person left it.
struct Search {
    workspace_id: WorkspaceId,
    dispatch: SessionHost,
    caller: Caller,
    rows: Vec<SearchRow>,
    /// What could not be searched here, when something could not.
    notes: Option<String>,
    /// What the last action answered.
    notice: Option<String>,
    /// The fields of the action the person chose, while they fill them.
    asking: Option<Asking>,
    /// The action the page ran and waits on.
    pending: Option<PendingRun>,
}

/// One row of a search page: what it says, the page it opens, or the action it runs.
struct SearchRow {
    /// The row's element number (`data-p`): what the page's act names it by.
    id: u32,
    label: String,
    secondary: String,
    href: Option<String>,
    run: Option<RowRun>,
    /// Whether the row stands under the apps.
    app: bool,
}

/// The action a search row runs: the target a caller invokes, as the catalogue named it.
#[derive(Clone)]
struct RowRun {
    target: RemoteActionTarget,
    /// The catalogue revision the row was listed at: what an action whose contract asks for
    /// confirmation is shown and refused at.
    revision: String,
    fields: Vec<RowField>,
}

/// One thing a row's action needs typed: the parameter it fills, as the action describes it.
#[derive(Clone)]
struct RowField {
    name: String,
    hint: String,
    number: bool,
}

/// A row's action that takes what the person types: its fields and what each holds.
struct Asking {
    label: String,
    run: RowRun,
    values: Vec<String>,
}

struct PendingRun {
    invocation: InvocationId,
    label: String,
}

impl RowField {
    /// The fields the action's required parameters ask for, when each is one a line of typed
    /// text fills; `None` when one is not (an object, a list), which the page has no field for.
    fn of(params: &[tilo_actions::ActionParam]) -> Option<Vec<Self>> {
        use tilo_actions::ParamType;
        params
            .iter()
            .filter(|param| param.required)
            .map(|param| {
                let number = match param.ty {
                    ParamType::Text | ParamType::DateTime | ParamType::Secret => false,
                    ParamType::Integer | ParamType::Number => true,
                    _ => return None,
                };
                Some(Self {
                    name: param.name.to_string(),
                    hint: param.description.to_string(),
                    number,
                })
            })
            .collect()
    }

    /// What `typed` gives this parameter: a number field's text as the number it says, `None`
    /// when it says none.
    fn value(&self, typed: &str) -> Option<serde_json::Value> {
        let typed = typed.trim();
        if typed.is_empty() {
            return None;
        }
        if !self.number {
            return Some(typed.into());
        }
        typed
            .parse::<i64>()
            .map(serde_json::Value::from)
            .ok()
            .or_else(|| {
                typed
                    .parse::<f64>()
                    .ok()
                    .and_then(|n| serde_json::Number::from_f64(n).map(Into::into))
            })
    }
}

impl Search {
    fn next_id(&self) -> u32 {
        SEARCH_FIRST_ROW.saturating_add(u32::try_from(self.rows.len()).unwrap_or(u32::MAX))
    }

    /// Search the workspace for `needle` as the page's person: the apps it names, the records
    /// whose text holds it, and the actions their catalogue offers.
    fn find(&mut self, needle: &str) {
        let typed = needle.trim();
        let query = typed.to_lowercase();
        self.rows.clear();
        let mut notes: Vec<&str> = Vec::new();
        // The apps stand first and always: with nothing typed the page is the way in to all
        // of them.
        match apps(&self.dispatch, &self.caller) {
            Ok(apps) => {
                for (title, name) in apps.into_iter().filter(|(title, name)| {
                    query.is_empty()
                        || title.to_lowercase().contains(&query)
                        || name.contains(&query)
                }) {
                    let id = self.next_id();
                    self.rows.push(SearchRow {
                        id,
                        label: title,
                        secondary: String::new(),
                        href: Some(format!("/w/{}/{name}", self.workspace_id)),
                        run: None,
                        app: true,
                    });
                }
            }
            Err(_) => notes.push("The workspace's apps could not be listed here."),
        }
        if !typed.is_empty() {
            if self.found(typed).is_err() {
                notes.push("The workspace's records could not be searched here.");
            }
            if self.actions(typed).is_err() {
                notes.push("The workspace's actions could not be searched here.");
            }
            if self.rows.iter().all(|row| row.app) && notes.is_empty() {
                notes.push("Nothing here matches.");
            }
        }
        self.notes = (!notes.is_empty()).then(|| notes.join(" "));
    }

    /// The records the workspace's search finds for `typed`, each a row that opens its page.
    fn found(&mut self, typed: &str) -> Result<(), PageRefusal> {
        let request = tilo_actions::remote::search::Query::request(
            InvocationId::new(),
            typed,
            &Within::default(),
        );
        let receipt = waited(self.dispatch.invoke(&self.caller, request))?.map_err(unavailable)?;
        let found = tilo_actions::remote::search::found(receipt).map_err(unavailable)?;
        for found in found.into_iter().filter(|found| found.kind != FOUND_FILE) {
            let Some(app) = found.app.as_deref() else {
                continue;
            };
            let name = page_app_name(app);
            let href = format!(
                "/w/{}/{name}/{sub}",
                self.workspace_id,
                sub = encode_part(&found.resource),
            );
            if self
                .rows
                .iter()
                .any(|row| row.href.as_deref() == Some(href.as_str()))
            {
                continue;
            }
            let id = self.next_id();
            self.rows.push(SearchRow {
                id,
                label: found.label,
                secondary: field_title(name),
                href: Some(href),
                run: None,
                app: false,
            });
        }
        Ok(())
    }

    /// The actions the workspace's records offer for `typed`, which a row runs: the same
    /// invocation an agent or a script makes, decided for this person by the same admission.
    fn actions(&mut self, typed: &str) -> Result<(), PageRefusal> {
        let listed = waited(self.dispatch.query(
            &self.caller,
            CatalogueQuery {
                needle: typed.to_owned(),
                max_results: 40,
                ..CatalogueQuery::default()
            },
        ))?
        .map_err(unavailable)?;
        for row in listed.rows {
            let Some(action) = row.action else { continue };
            let Some(fields) = RowField::of(&action.params) else {
                continue;
            };
            let id = self.next_id();
            self.rows.push(SearchRow {
                id,
                label: action.action_label,
                secondary: row.resource.label,
                href: None,
                run: Some(RowRun {
                    target: RemoteActionTarget {
                        provider: row.provider,
                        address: ActionAddress::new(row.resource.id, action.address.action),
                    },
                    revision: listed.revision.clone(),
                    fields,
                }),
                app: false,
            });
        }
        Ok(())
    }

    /// Act on the page: typing narrows it, a row runs its action, and with nothing done, an
    /// action it ran that has settled says so.
    fn act(&mut self, act: Option<PageAct>) -> Vec<PagePatch> {
        let Some(PageAct { id, action, .. }) = act else {
            return if self.settled() {
                self.patched()
            } else {
                Vec::new()
            };
        };
        match action {
            PageAction::Value(text) if id >= SEARCH_FIRST_FIELD => {
                // What the person types into a field of the action they chose is kept until
                // they run it.
                if let Some(value) = self.asking.as_mut().and_then(|asking| {
                    let field = usize::try_from(id.checked_sub(SEARCH_FIRST_FIELD)?).ok()?;
                    asking.values.get_mut(field)
                }) {
                    *value = text;
                }
                return Vec::new();
            }
            PageAction::Activate if id == SEARCH_FORM_CANCEL => self.asking = None,
            PageAction::Activate if id == SEARCH_FORM_RUN => self.run_asked(),
            PageAction::Value(text) => {
                self.notice = None;
                self.find(&text);
            }
            PageAction::Activate => self.run_row(id),
            // The search page lays out at any size and has nothing that steps.
            PageAction::Toggle
            | PageAction::Focus
            | PageAction::Select { .. }
            | PageAction::Increment
            | PageAction::Decrement
            | PageAction::Resize { .. } => {
                return Vec::new();
            }
        }
        self.patched()
    }

    /// Run the row numbered `id`'s action, or ask for its fields first.
    fn run_row(&mut self, id: u32) {
        if self.pending.is_some() {
            return;
        }
        let Some((label, run)) = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.run.clone().map(|run| (row.label.clone(), run)))
        else {
            return;
        };
        if run.fields.is_empty() {
            self.invoke(label, run, InvokeArgument::None);
        } else {
            self.asking = Some(Asking {
                label,
                values: vec![String::new(); run.fields.len()],
                run,
            });
        }
    }

    /// Run the action the page asked the person's fields for, with what they typed. A field
    /// left empty is said, and nothing runs.
    fn run_asked(&mut self) {
        let Some(asking) = self.asking.take() else {
            return;
        };
        let mut arguments = serde_json::Map::new();
        for (field, typed) in asking.run.fields.iter().zip(&asking.values) {
            let Some(value) = field.value(typed) else {
                self.notice = Some(format!(
                    "{}: {}",
                    field_title(&field.name),
                    if field.number && !typed.trim().is_empty() {
                        "write a number."
                    } else {
                        "write something first."
                    }
                ));
                self.asking = Some(asking);
                return;
            };
            arguments.insert(field.name.clone(), value);
        }
        self.invoke(asking.label, asking.run, InvokeArgument::Json(arguments));
    }

    fn invoke(&mut self, label: String, run: RowRun, argument: InvokeArgument) {
        let invocation = InvocationId::new();
        let request = InvokeRequest {
            invocation,
            target: run.target,
            argument,
            context: None,
            confirmation: None,
            revision: Some(run.revision),
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = waited(self.dispatch.invoke(&self.caller, request))
            .and_then(|admitted| admitted.map_err(unavailable));
        self.notice = Some(match receipt {
            Ok(InvocationReceipt {
                status: ReceiptStatus::Accepted,
                ..
            }) => {
                let running = format!("Running {label}…");
                self.pending = Some(PendingRun { invocation, label });
                running
            }
            Ok(InvocationReceipt { status, .. }) => said_of_status(&label, status),
            Err(refusal) => format!("{label}: {}", refused_words(&refusal)),
        });
    }

    /// Whether the action the page waits on has settled: what it answered is then the page's
    /// notice.
    fn settled(&mut self) -> bool {
        let Some(run) = &self.pending else {
            return false;
        };
        let notice = match self
            .dispatch
            .observe(&self.caller, &run.invocation)
            .map(|state| state.update)
        {
            Some(InvocationUpdate::Succeeded { output, .. }) => said_of(&run.label, &output),
            Some(InvocationUpdate::Failed { error, .. }) => {
                format!("{}: {}", run.label, error.message)
            }
            Some(InvocationUpdate::Cancelled { message, .. }) => format!(
                "{}: {}",
                run.label,
                message.unwrap_or_else(|| "stopped.".to_owned())
            ),
            Some(_) => return false,
            None => format!("{}: the device no longer knows of it.", run.label),
        };
        self.notice = Some(notice);
        self.pending = None;
        true
    }

    /// The results as they stand, under the field.
    fn results(&self) -> String {
        let mut html = results_html(&self.rows, self.notice.as_deref(), self.notes.as_deref());
        if let Some(asking) = &self.asking {
            html = asking_html(&asking.label, &asking.run.fields, &asking.values) + &html;
        }
        html
    }

    fn patched(&self) -> Vec<PagePatch> {
        vec![PagePatch {
            id: SEARCH_RESULTS,
            html: results_div(&self.results()),
        }]
    }
}

/// What an admission answered at once, as a person reads it.
fn said_of_status(label: &str, status: ReceiptStatus) -> String {
    match status {
        ReceiptStatus::Completed { output } => said_of(label, &output),
        ReceiptStatus::Failed { error } => format!("{label}: {}", error.message),
        ReceiptStatus::Rejected { error } => format!("{label}: {error}"),
        ReceiptStatus::Cancelled { message } => {
            format!("{label}: {}", message.as_deref().unwrap_or("stopped."))
        }
        ReceiptStatus::Accepted => format!("Running {label}…"),
    }
}

fn refused_words(refusal: &PageRefusal) -> String {
    match refusal {
        PageRefusal::Unavailable(reason) => reason.clone(),
        other => format!("{other:?}"),
    }
}

/// A parameter's or an app's name as a title: `display_name` reads "Display name".
fn field_title(name: &str) -> String {
    let words = name.replace(['_', '-'], " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What the action `label` answered, as a person reads it: the value it answered with, when it
/// names something readable, or that it is done.
fn said_of(label: &str, output: &ActionOutput) -> String {
    let said = match &output.value {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(serde_json::Value::Object(fields)) => ["link", "url", "text", "message"]
            .iter()
            .find_map(|key| fields.get(*key).and_then(serde_json::Value::as_str))
            .map(str::to_owned),
        _ => None,
    };
    let said: String = said
        .unwrap_or_else(|| "Done.".to_owned())
        .chars()
        .take(600)
        .collect();
    format!("{label}: {said}")
}

/// The fields the action `label` asks for, holding what was typed: a form over the rows, its
/// first field taking the caret.
fn asking_html(label: &str, fields: &[RowField], values: &[String]) -> String {
    let mut out = format!(
        "<form class=\"tilo-search-form\" aria-label=\"{label}\"><h2>{label}</h2>",
        label = escape(label)
    );
    for (index, (field, value)) in fields.iter().zip(values).enumerate() {
        let id = SEARCH_FIRST_FIELD.saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
        let _ = write!(
            out,
            "<label class=\"tilo-search-form-field\"><span>{title}</span>\
             <input name=\"{name}\" type=\"{kind}\" data-a=\"value\" data-p=\"{id}\" \
             value=\"{value}\"{focus}><small>{hint}</small></label>",
            title = escape(&field_title(&field.name)),
            name = escape(&field.name),
            kind = if field.number { "number" } else { "text" },
            value = escape(value),
            focus = if index == 0 { " autofocus" } else { "" },
            hint = escape(&field.hint),
        );
    }
    let _ = write!(
        out,
        "<div class=\"tilo-search-form-actions\">\
         <button type=\"button\" data-a=\"activate\" data-p=\"{SEARCH_FORM_CANCEL}\">Cancel</button>\
         <button type=\"submit\" data-a=\"activate\" data-p=\"{SEARCH_FORM_RUN}\">{}</button>\
         </div></form>",
        escape(label)
    );
    out
}

/// The rows of the search page, grouped: the apps, then what the workspace holds and what its
/// actions offer. `notice` says what the last action answered, and `notes` what could not be
/// searched here.
fn results_html(rows: &[SearchRow], notice: Option<&str>, notes: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(notice) = notice {
        let _ = write!(
            out,
            "<p class=\"tilo-search-notice\" role=\"status\">{}</p>",
            escape(notice)
        );
    }
    if let Some(notes) = notes {
        let _ = write!(out, "<p class=\"tilo-search-note\">{}</p>", escape(notes));
    }
    for (app, heading) in [(true, "Apps"), (false, "Actions and records")] {
        let group: Vec<&SearchRow> = rows
            .iter()
            .filter(|row| row.app == app)
            .take(MOST_ROWS)
            .collect();
        if group.is_empty() {
            continue;
        }
        let _ = write!(
            out,
            "<section class=\"tilo-search-group\" aria-label=\"{heading}\"><h2>{heading}</h2><ul>"
        );
        for row in group {
            let secondary = if row.secondary.is_empty() {
                String::new()
            } else {
                format!(
                    "<span class=\"tilo-search-row-kind\">{}</span>",
                    escape(&row.secondary)
                )
            };
            let label = escape(&row.label);
            let _ = match &row.href {
                Some(href) => write!(
                    out,
                    "<li><a class=\"tilo-search-row\" href=\"{}\">\
                     <span class=\"tilo-search-row-label\">{label}</span>{secondary}</a></li>",
                    escape(href),
                ),
                None => write!(
                    out,
                    "<li><button class=\"tilo-search-row\" type=\"button\" data-a=\"activate\" \
                     data-p=\"{}\"><span class=\"tilo-search-row-label\">{label}</span>\
                     {secondary}</button></li>",
                    row.id,
                ),
            };
        }
        out.push_str("</ul></section>");
    }
    out
}

/// The results element, holding `contents`: the one element a search page patches.
fn results_div(contents: &str) -> String {
    format!("<div class=\"tilo-search-results\" data-p=\"{SEARCH_RESULTS}\">{contents}</div>")
}

/// The search page `needle` reached with and the results it answers with: a field where the
/// person keeps typing, over what the workspace's search found.
fn search_body(needle: &str, results: &str) -> String {
    format!(
        "<div class=\"tilo-view tilo-search\" data-p=\"0\" data-focus=\"1\">\
         <div class=\"tilo-search-bar\">\
         <input class=\"tilo-search-field\" type=\"search\" data-a=\"value\" data-t=\"search\" \
         data-p=\"1\" value=\"{needle}\" placeholder=\"Search apps, actions and records\" \
         aria-label=\"Search\"></div>{results}</div>",
        needle = escape(needle),
        results = results_div(results),
    )
}

/// The workspace's front page: a side of its people and its apps as tiles, on the theme's own
/// ground.
const HOME_CSS: &str = "\
.tilo-home{height:100%;box-sizing:border-box;overflow-y:auto;display:flex;flex-direction:column;gap:20px;\
padding:clamp(24px,6vh,56px) clamp(20px,6vw,64px);background:var(--tilo-ground);}\
.tilo-home-title{margin:0;font-size:18px;font-weight:600;}\
.tilo-home-search{display:block;box-sizing:border-box;max-width:880px;padding:12px 16px;\
border-radius:12px;background:var(--tilo-raised);color:var(--tilo-text-dim);\
text-decoration:none;font-size:16px;}\
.tilo-home-search:hover{color:var(--tilo-text);}\
.tilo-home-search:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-home-desktop{align-self:flex-start;padding:8px 14px;border-radius:10px;background:var(--tilo-raised);\
color:var(--tilo-text);text-decoration:none;font-size:14px;font-weight:500;}\
.tilo-home-desktop:hover{background:var(--tilo-hover);}\
.tilo-home-desktop:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-home-apps{display:grid;grid-template-columns:repeat(auto-fill,minmax(118px,1fr));\
gap:8px;margin:0;padding:0;list-style:none;max-width:880px;}\
.tilo-home-app{display:flex;flex-direction:column;align-items:center;gap:10px;padding:18px 10px;\
border-radius:14px;color:var(--tilo-text);text-decoration:none;font-size:13px;font-weight:500;}\
.tilo-home-app:hover{background:var(--tilo-hover);}\
.tilo-home-app:active{background:var(--tilo-press);}\
.tilo-home-app:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-people{display:flex;flex-direction:column;gap:8px;max-width:880px;}\
.tilo-people-count{margin:0;font-size:13px;font-weight:600;color:var(--tilo-text-dim);}\
.tilo-people-list{display:flex;flex-direction:column;gap:2px;margin:0;padding:0;list-style:none;}\
.tilo-person{display:flex;align-items:baseline;gap:8px;padding:6px 8px;border-radius:8px;font-size:14px;opacity:.55;}\
.tilo-person[data-present]{opacity:1;}\
.tilo-person[data-present] .tilo-person-name::before{content:\"\";display:inline-block;width:6px;height:6px;\
margin-right:8px;border-radius:50%;background:var(--tilo-accent);vertical-align:middle;}\
.tilo-person-name{font-weight:500;}\
.tilo-person-kind{font-size:12px;color:var(--tilo-text-tertiary);}\
.tilo-person-what{margin-left:auto;font-size:13px;color:var(--tilo-text-dim);}\
.tilo-home-initial{width:52px;height:52px;border-radius:12px;display:grid;place-items:center;\
background:var(--tilo-raised);color:var(--tilo-text-dim);font-size:22px;font-weight:600;}";

/// The search page's own ground: the field on the front page's ground, over the rows it finds.
const SEARCH_CSS: &str = "\
.tilo-search{height:100%;overflow-y:auto;display:flex;flex-direction:column;gap:20px;\
padding:clamp(24px,6vh,56px) clamp(20px,6vw,64px);background:var(--tilo-ground);}\
.tilo-search-bar{max-width:880px;}\
.tilo-search-field{width:100%;box-sizing:border-box;padding:12px 16px;border:0;outline:none;\
border-radius:12px;background:var(--tilo-raised);color:var(--tilo-text);font:inherit;font-size:16px;}\
.tilo-search-field:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-search-results{display:flex;flex-direction:column;gap:20px;max-width:880px;}\
.tilo-search-group{display:flex;flex-direction:column;gap:6px;}\
.tilo-search-group h2{margin:0;font-size:13px;font-weight:600;color:var(--tilo-text-dim);}\
.tilo-search-group ul{display:flex;flex-direction:column;gap:2px;margin:0;padding:0;list-style:none;}\
.tilo-search-row{display:flex;align-items:baseline;gap:10px;padding:8px 10px;border-radius:8px;\
color:var(--tilo-text);text-decoration:none;font-size:14px;}\
.tilo-search-row:hover{background:var(--tilo-hover);}\
.tilo-search-row:active{background:var(--tilo-press);}\
.tilo-search-row:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-search-row-label{font-weight:500;}\
.tilo-search-row-kind{margin-left:auto;font-size:12px;color:var(--tilo-text-tertiary);}\
.tilo-search-note{margin:0;font-size:14px;color:var(--tilo-text-dim);}\
.tilo-search-form{display:flex;flex-direction:column;gap:12px;padding:16px;border-radius:12px;\
background:var(--tilo-raised);}\
.tilo-search-form h2{margin:0;font-size:15px;font-weight:600;}\
.tilo-search-form-field{display:flex;flex-direction:column;gap:4px;font-size:13px;font-weight:500;}\
.tilo-search-form-field input{box-sizing:border-box;padding:8px 10px;border:1px solid var(--tilo-border);\
border-radius:8px;background:var(--tilo-ground);color:var(--tilo-text);font:inherit;font-size:14px;}\
.tilo-search-form-field input:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-1px;}\
.tilo-search-form-field small{font-weight:400;color:var(--tilo-text-dim);}\
.tilo-search-form-actions{display:flex;justify-content:flex-end;gap:8px;}\
.tilo-search-form-actions button{padding:7px 14px;border:0;border-radius:8px;font:inherit;font-size:14px;\
background:var(--tilo-hover);color:var(--tilo-text);}\
.tilo-search-form-actions button[type=submit]{background:var(--tilo-accent);color:var(--tilo-on-accent);}";
