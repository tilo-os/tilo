//! A workspace's apps as web pages for its members: each page is the app's
//! view of the workspace's document, held for the person in a replica of
//! their own within bounds ([`bounded`]), in an app worker that also renders
//! it ([`ConfinedReplica::open_page`]). What the person does on the page is
//! acted on that view as a click or typing in a window is, and what it wrote
//! is checked against their access and saved as every bounded caller's write
//! is.

use std::fmt::Write as _;
use tilo_user_identity::DeviceCert;

use super::{
    ActionCallError, Caller, DocumentRoute, HashMap, Job, Needs, Worker, WorkspaceActions,
    WorkspaceId, bounded, catch_up, pending,
};
use tilo_actions::remote::{
    CatalogueQuery, Completer, FOUND_FILE, InvocationReceipt, InvokeRequest, Pending, PendingPoll,
    ReceiptStatus, RemoteActionTarget, Within,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_server_protocol::page::{
    COPY_INVITE_LINK, DEVICES_AND_ACCESS, OpenedPage, PAGE_ACTIONS_PROVIDER, PageAction,
    PageAddress, PagePatch, PageRefusal, WAITING_TO_JOIN, WaitingAddress, encode_part,
};

/// The app name a page address carries for the workspace's search
/// (`/w/<workspace>/search/<words>`): not an app of the catalogue but the
/// system's own search over what the workspace holds, which the server asks
/// and renders like a page.
const SEARCH_APP: &str = "search";

/// Whether `error` says the record is not ready yet, as a replica taking its
/// parts in does.
fn not_ready(error: &ActionCallError) -> bool {
    matches!(error, ActionCallError::NotReady)
}

/// What a page asks of the service's thread.
pub(super) enum PageJob {
    Open {
        workspace_id: WorkspaceId,
        caller: Box<Caller>,
        certificate: Option<Box<DeviceCert>>,
        address: PageAddress,
        done: Completer<Result<OpenedPage, PageRefusal>>,
    },
    Act {
        workspace_id: WorkspaceId,
        page: u64,
        act: Option<(u32, Option<String>, PageAction)>,
        done: Completer<Result<Vec<PagePatch>, PageRefusal>>,
    },
    Close {
        workspace_id: WorkspaceId,
        page: u64,
    },
}

impl PageJob {
    pub(super) const fn workspace(&self) -> WorkspaceId {
        match self {
            Self::Open { workspace_id, .. }
            | Self::Act { workspace_id, .. }
            | Self::Close { workspace_id, .. } => *workspace_id,
        }
    }

    pub(super) const fn caller(&self) -> Option<&Caller> {
        match self {
            Self::Open { caller, .. } => Some(&**caller),
            Self::Act { .. } | Self::Close { .. } => None,
        }
    }
}

/// A page the service holds: whose it is and what it shows.
#[allow(
    clippy::large_enum_variant,
    reason = "a service holds a page per open tab, few enough that boxing saves nothing"
)]
pub(super) enum HeldPage {
    /// An app's page: the record it shows, and who opened it.
    App {
        workspace_id: WorkspaceId,
        /// Boxed: who opened it is most of the page's size, and the front page holds none.
        who: Box<bounded::Bounded>,
        route: DocumentRoute,
        form_calls: Option<FormCalls>,
    },
    /// The workspace's front page: the apps a member opens here. Nothing is
    /// acted on it and it never changes.
    Home { workspace_id: WorkspaceId },
    /// The workspace's search: what its apps, their actions and their records
    /// hold for what the person typed. The caller is kept: each keystroke is
    /// a question asked as them, answered with what they may read.
    Search {
        workspace_id: WorkspaceId,
        caller: Caller,
        needle: String,
        /// The rows the page shows now, each naming what it opens or runs.
        rows: Vec<SearchRow>,
        /// What could not be searched here, when something could not.
        notes: Option<String>,
        /// The action a row ran and the page still waits on: its answer is
        /// what the page shows next.
        pending: Option<PendingRun>,
        /// What the last run answered, as the person reads it.
        notice: Option<String>,
        /// The row whose action asks for what it needs, and what the person
        /// has typed into its fields so far.
        asking: Option<Asking>,
        /// What one of the host's own actions opened on the page (the
        /// addresses waiting at the workspace's entry), and what the person
        /// does with it.
        card: Option<Card>,
    },
}

/// What one of the host's own actions opened on the search page, under the
/// rows: what the person does there, one press each.
pub(super) enum Card {
    /// The addresses waiting at the workspace's entry, each one an Invite
    /// button that mails it an invitation.
    Waiting {
        /// Whether the workspace's entry mails an invitation to an address.
        mails: bool,
        addresses: Vec<WaitingAddress>,
        /// The address whose invitation was mailed, as the page says it.
        sent: Option<String>,
        /// Why an invitation was not mailed, when the host said why.
        refused: Option<String>,
    },
    /// The person's own devices, and where one more is set up: the site's
    /// setup screen, which makes the code an agent on another computer runs.
    Devices {
        /// The page that sets one up.
        setup: String,
    },
}

/// A row's action that takes what the person types: its fields, shown under
/// the rows, and what each holds.
pub(super) struct Asking {
    label: String,
    run: RowRun,
    values: Vec<String>,
}

/// One row of a search page: what it says, the page it opens, or the action
/// it runs.
pub(super) struct SearchRow {
    /// The row's element number (`data-p`): what the page's act names it by.
    id: u32,
    label: String,
    secondary: String,
    /// The page the row opens.
    href: Option<String>,
    /// The action the row runs, when it runs one.
    run: Option<RowRun>,
    /// The link the row hands over on its press, made ahead of it: a
    /// clipboard write happens inside the press, and the host's answer comes
    /// after it.
    copy: Option<String>,
    /// Whether the row stands under the apps.
    app: bool,
}

/// The action a search row runs: the target a caller invokes, as the
/// catalogue named it.
#[derive(Clone)]
struct RowRun {
    provider: String,
    resource: String,
    action: String,
    /// The catalogue revision the row was listed at: what an action whose
    /// contract asks for confirmation is shown and refused at.
    revision: Option<String>,
    /// What the action needs from the person before it runs: one field each.
    fields: Vec<RowField>,
    /// Whether the host runs it itself (the link's `own_actions`)
    /// rather than a record of the workspace.
    host: bool,
}

/// One thing a row's action needs typed: the parameter it fills, as the
/// action describes it.
#[derive(Clone)]
struct RowField {
    name: String,
    hint: String,
    number: bool,
}

impl RowRun {
    /// The semantic target a search button sends back, even when another action
    /// now occupies its row. Each of the address parts has its own boundary.
    fn page_target(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            if self.host { "host" } else { "record" },
            encode_part(&self.provider),
            encode_part(&self.resource),
            encode_part(&self.action),
        )
    }

    /// The host's own page action this row runs, when it runs one
    /// (`tilo.page`): the two whose press opens something on the page.
    fn runs_page_action(&self, action: &str) -> bool {
        self.provider == PAGE_ACTIONS_PROVIDER && self.action == action
    }

    /// Whether `other` runs the same action as this row: what a row handing a
    /// link over finds the row it stands on again by.
    fn same_action(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.resource == other.resource
            && self.action == other.action
    }
}

impl RowField {
    /// The fields the action's required parameters ask for, when each is one
    /// a line of typed text fills; `None` when one is not (an object, a
    /// list), which the page has no field for.
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

    /// What `typed` gives this parameter: a number field's text as the number
    /// it says, `None` when it says none.
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

/// The action a search page ran and waits on.
pub(super) struct PendingRun {
    invocation: InvocationId,
    label: String,
}

/// The pages the service holds, by number.
#[derive(Default)]
pub(super) struct Pages {
    held: HashMap<u64, HeldPage>,
    next: u64,
}

/// Only the host holds the certificate proved by this page's session.
pub(super) struct FormCalls {
    form: crate::worker::proto::PageForm,
    certificate: Option<Box<DeviceCert>>,
    pending: Vec<(
        InvocationId,
        Pending<Result<serde_json::Value, ActionCallError>>,
    )>,
}

impl FormCalls {
    fn poll(
        &mut self,
        app: &crate::ConfinedReplica,
        workspace: WorkspaceId,
        caller: &Caller,
        link: Option<&dyn super::WorkspaceDocumentsLink>,
        mut activated: bool,
    ) -> Result<(), crate::apps::AppError> {
        let mut replies = Vec::new();
        self.pending.retain_mut(|(invocation, answer)| {
            let status = match answer.poll_ready() {
                PendingPoll::Waiting => return true,
                PendingPoll::Ready(Ok(value)) => ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        ..ActionOutput::default()
                    },
                },
                PendingPoll::Ready(Err(error)) => ReceiptStatus::Rejected { error },
                PendingPoll::Abandoned => ReceiptStatus::Rejected {
                    error: ActionCallError::NotReady,
                },
            };
            replies.push(InvocationReceipt {
                invocation: *invocation,
                status,
            });
            false
        });
        for request in app.page_form_calls(replies)? {
            let invocation = request.invocation;
            let answer = match (link, self.certificate.as_deref()) {
                (Some(link), Some(certificate))
                    if self.form.permits(&request.target, &mut activated) =>
                {
                    link.page_form_action(workspace, caller, certificate, app.app_id(), request)
                }
                _ => Pending::ready(Err(ActionCallError::NotPermitted)),
            };
            self.pending.push((invocation, answer));
        }
        Ok(())
    }
}

impl Pages {
    /// Who the page `job` is about was opened by: what an act on it, which names the page alone,
    /// is rendered for.
    pub(super) fn caller_of<'a>(&'a self, job: &'a PageJob) -> Option<&'a Caller> {
        match job {
            PageJob::Open { caller, .. } => Some(&**caller),
            PageJob::Act { page, .. } | PageJob::Close { page, .. } => match self.held.get(page) {
                Some(HeldPage::App { who, .. }) => Some(&who.caller),
                Some(HeldPage::Search { caller, .. }) => Some(caller),
                Some(HeldPage::Home { .. }) | None => None,
            },
        }
    }
}

fn unavailable(error: impl std::fmt::Display) -> PageRefusal {
    PageRefusal::Unavailable(error.to_string())
}

/// What a page's app answered with nothing: its document opened again past
/// a slow request ([`PageRefusal::Reopened`]), or otherwise not shown now.
fn not_shown(error: crate::apps::AppError) -> PageRefusal {
    match error {
        crate::apps::AppError::Reopened { request } => PageRefusal::Reopened { request },
        error => unavailable(error),
    }
}

fn refused(error: ActionCallError) -> PageRefusal {
    match error {
        ActionCallError::NotPermitted => PageRefusal::NotMember,
        error => unavailable(format!("{error:?}")),
    }
}

impl WorkspaceActions {
    /// Show `caller` the page `address` names in `workspace_id`: the app's view
    /// of the workspace's document, as the workspace lets them see it.
    #[must_use]
    pub fn open_page(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        address: PageAddress,
    ) -> Pending<Result<OpenedPage, PageRefusal>> {
        self.open_page_with_device(workspace_id, caller, None, address)
    }

    /// Open a page on the session that proved this device. Built-in forms use
    /// that session to reach their own services as the person.
    #[must_use]
    pub fn open_page_with_device(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        address: PageAddress,
    ) -> Pending<Result<OpenedPage, PageRefusal>> {
        let (done, answer) = pending();
        let job = PageJob::Open {
            workspace_id,
            caller: Box::new(caller.clone()),
            certificate: certificate.cloned().map(Box::new),
            address,
            done,
        };
        if let Err(Job::Page(job)) = self.submit(Job::Page(Box::new(job)))
            && let PageJob::Open { done, .. } = *job
        {
            done.complete(Err(unavailable("the workspace's pages are not served")));
        }
        answer
    }

    /// Act on `page` of `workspace_id` as its person did (`act`: the element's
    /// number, its target and the action), or with no act, answer what changed
    /// on it since: the elements that changed, outermost first.
    #[must_use]
    pub fn act_on_page(
        &self,
        workspace_id: WorkspaceId,
        page: u64,
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Pending<Result<Vec<PagePatch>, PageRefusal>> {
        let (done, answer) = pending();
        let job = PageJob::Act {
            workspace_id,
            page,
            act,
            done,
        };
        if let Err(Job::Page(job)) = self.submit(Job::Page(Box::new(job)))
            && let PageJob::Act { done, .. } = *job
        {
            done.complete(Err(unavailable("the workspace's pages are not served")));
        }
        answer
    }

    /// Let go of `page`: its person closed it.
    pub fn close_page(&self, workspace_id: WorkspaceId, page: u64) {
        let _ = self.submit(Job::Page(Box::new(PageJob::Close { workspace_id, page })));
    }
}

impl Worker {
    pub(super) fn page_job(&mut self, job: PageJob) {
        match job {
            PageJob::Open {
                workspace_id,
                caller,
                certificate,
                address,
                done,
            } => done.complete(self.open_page(workspace_id, &caller, certificate, &address)),
            PageJob::Act {
                workspace_id,
                page,
                act,
                done,
            } => match self.act_on_page(page, act.clone()) {
                Some(answer) => done.complete(answer),
                // Its record or its chains are still on their way: it waits off the thread,
                // and goes on once they arrive.
                None => self.await_part(
                    workspace_id,
                    Job::Page(Box::new(PageJob::Act {
                        workspace_id,
                        page,
                        act,
                        done,
                    })),
                ),
            },
            PageJob::Close { page, .. } => {
                if self.pages.held.remove(&page).is_some() {
                    self.bounded.retain(|key, _| key.3 != Some(page));
                }
            }
        }
    }

    /// What a page job waits for: the document its page shows, which the
    /// workspace is asked for first, and whose keys it is handed.
    pub(super) fn page_keys(&self, job: &PageJob) -> super::keys::Keys {
        let document = |app_id: &str, document: &str| super::keys::Keys::Record {
            app_id: app_id.to_owned(),
            record: document.to_owned(),
            act: true,
        };
        match job {
            PageJob::Open { address, .. } => {
                if address.is_home() || address.app == SEARCH_APP {
                    return super::keys::Keys::Joined;
                }
                self.page_app(&address.app)
                    .map_or(super::keys::Keys::Joined, |app| document(app.app_id(), ""))
            }
            PageJob::Act { page, .. } => match self.pages.held.get(page) {
                Some(HeldPage::App { route, .. }) => document(&route.app_id, &route.document),
                Some(HeldPage::Home { .. } | HeldPage::Search { .. }) | None => {
                    super::keys::Keys::Joined
                }
            },
            PageJob::Close { .. } => super::keys::Keys::Joined,
        }
    }

    /// The app of this catalogue `name` names: its id, or the last part of it
    /// (`notes` for `so.tilo.notes`).
    fn page_app(&self, name: &str) -> Option<&'static ui::application::ApplicationDefinition> {
        self.catalogue.get(name).or_else(|| {
            self.catalogue.iter().find(|app| {
                app.app_id()
                    .rsplit_once('.')
                    .is_some_and(|(_, last)| last == name)
            })
        })
    }

    fn open_page(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: Option<Box<DeviceCert>>,
        address: &PageAddress,
    ) -> Result<OpenedPage, PageRefusal> {
        if !self.admits_member(workspace_id, caller) {
            return Err(PageRefusal::NotMember);
        }
        if address.is_home() {
            return self.open_home(workspace_id);
        }
        if address.app == SEARCH_APP {
            return self.open_search(
                workspace_id,
                caller,
                address.show.as_deref().unwrap_or_default(),
            );
        }
        let app = self.page_app(&address.app).ok_or(PageRefusal::NoSuchApp)?;
        // A page shows an app's own document of the workspace; an app that
        // keeps one per thing it makes is shown from one of those, which no
        // address names yet.
        if !app.keeps_workspace_resource() {
            return Err(PageRefusal::NoSuchApp);
        }
        let cluster_id = self
            .refresh(workspace_id, Needs::Workspace)
            .map_err(refused)?
            .workspace
            .ok_or(PageRefusal::Elsewhere)?;
        let mut who = self.bounded(caller).unwrap_or_else(|| bounded::Bounded {
            caller: caller.clone(),
            person_caller: caller.clone(),
            person: self.member.clone(),
            agent: None,
            agent_name: None,
            page: None,
        });
        // The app's document of the workspace, founded by the person as a window
        // of it would be when the workspace holds none yet.
        self.open_new(workspace_id, &cluster_id, app.app_id(), Some(&who))
            .map_err(|outcome| match outcome {
                crate::record_actions::Outcome::Rejected(error) => refused(error),
                crate::record_actions::Outcome::Failed(failure) => unavailable(failure.message),
            })?;
        let state = self
            .refresh(workspace_id, Needs::Workspace)
            .map_err(refused)?;
        let route = DocumentRoute {
            app_id: app.app_id().to_owned(),
            cluster_id,
            document: String::new(),
        };
        self.pages.next = self.pages.next.wrapping_add(1);
        let page = self.pages.next;
        who.page = Some(page);
        let key = self
            .bounded_replica(workspace_id, &who, &route, &state)
            .map_err(refused)?;
        let replica = &self
            .bounded
            .get(&key)
            .ok_or_else(|| unavailable("the page's document did not open"))?
            .held
            .app;
        let shown = replica
            .page_show(address.show.as_deref())
            .map_err(not_shown)?;
        let mut form_calls =
            crate::worker::proto::PageForm::for_app(replica.app_id()).map(|form| FormCalls {
                certificate,
                pending: Vec::new(),
                form,
            });
        if let Some(calls) = &mut form_calls {
            calls
                .poll(
                    replica,
                    workspace_id,
                    &who.caller,
                    self.shared.link.get().map(AsRef::as_ref),
                    false,
                )
                .map_err(not_shown)?;
        }
        self.save_page(workspace_id, &key, (&who, &route, &state))?;
        self.pages.held.insert(
            page,
            HeldPage::App {
                workspace_id,
                who: Box::new(who),
                route,
                form_calls,
            },
        );
        let (title, css, body) = shown;
        Ok(OpenedPage {
            page,
            title,
            css,
            body,
        })
    }

    /// The workspace's front page: who is in it and who of them is here, and
    /// the apps a member opens, each a link to its own page.
    #[allow(
        clippy::too_many_lines,
        reason = "one page read top to bottom: the people, then the apps"
    )]
    fn open_home(&mut self, workspace_id: WorkspaceId) -> Result<OpenedPage, PageRefusal> {
        // The workspace has to be readable here, as an app's page does.
        let state = self
            .refresh(workspace_id, Needs::Workspace)
            .map_err(refused)?;
        state.workspace.as_ref().ok_or(PageRefusal::Elsewhere)?;
        // Who the workspace has is its own governance; who is here is what the
        // live session's devices say of themselves. Everyone the roster lists
        // is a row, those away included, and the ones here carry what they are
        // doing.
        let (roster, present) = match self.workspaces.get(&workspace_id) {
            Some(workspace) => (
                workspace.member.roster(workspace_id),
                workspace.member.present(workspace_id),
            ),
            None => (Vec::new(), Vec::new()),
        };
        let here = here(&present);
        let mut apps: Vec<_> = self
            .catalogue
            .iter()
            .filter(|app| app.keeps_workspace_resource())
            .collect();
        apps.sort_by_key(|app| app.title().to_owned());
        let mut body = String::from(
            "<div class=\"tilo-view tilo-home\" data-p=\"0\">\
             <h1 class=\"tilo-home-title\">Tilo</h1>",
        );
        // Search is the system's, over what the workspace holds: the front
        // page is its way in, and the field there is a link to its own page.
        let _ = write!(
            body,
            "<a class=\"tilo-home-search\" href=\"/w/{workspace_id}/{SEARCH_APP}\" \
             aria-label=\"Search the workspace\">Search apps, actions and records</a>\
             <button type=\"button\" data-pair-device>Pair a new device</button>"
        );
        // Who is here is said only when the live session says someone is: a
        // page that claims "0 here" of a workspace someone is reading is
        // wrong about the one person it can see.
        let count = if here == 0 {
            "People".to_owned()
        } else {
            format!("{here} here")
        };
        let _ = write!(
            body,
            "<section class=\"tilo-people\" aria-label=\"People\"><h2 class=\"tilo-people-count\">{count}</h2><ul class=\"tilo-people-list\">"
        );
        let mut rows = roster;
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        for member in rows.into_iter().filter(|member| !member.removed) {
            // A person is here by the name everyone calls them; an agent a
            // host speaks for is here by the member its presence names.
            let shown = present.iter().find(|shown| {
                shown
                    .member
                    .as_ref()
                    .is_some_and(|named| named.to_string() == member.id.to_string())
                    || shown.label == member.name
            });
            let role = if member.administrator {
                "owner"
            } else {
                match &member.role {
                    Some(role) => role.name(),
                    None => "agent",
                }
            };
            let _ = write!(
                body,
                "<li class=\"tilo-person\"{present}><span class=\"tilo-person-name\">{name}</span>\
                 <span class=\"tilo-person-kind\">{role}</span>{what}</li>",
                present = if shown.is_some() { " data-present" } else { "" },
                name = escape(&member.name),
                role = escape(role),
                what = shown
                    .and_then(|shown| shown.activity.as_deref())
                    .map_or_else(String::new, |what| format!(
                        "<span class=\"tilo-person-what\">{}</span>",
                        escape(what)
                    )),
            );
        }
        body.push_str("</ul></section><ul class=\"tilo-home-apps\">");
        for app in apps {
            let name = app
                .app_id()
                .rsplit_once('.')
                .map_or_else(|| app.app_id(), |(_, last)| last);
            // Every tile holds its icon's place, so a grid of apps reads as
            // rows: an app with no artwork of its own stands as its first
            // letter on the same tile.
            let icon = match &app.icon {
                ui::IconDescriptor::Named(icon) => ui::icons::Icon::from_name(icon)
                    .map_or_else(|| initial_tile(app.title()), ui::icons::icon_svg),
                ui::IconDescriptor::Glyph(_) => initial_tile(app.title()),
            };
            let _ = write!(
                body,
                "<li><a class=\"tilo-home-app\" href=\"/w/{workspace_id}/{name}\">{icon}\
                 <span>{}</span></a></li>",
                escape(app.title())
            );
        }
        body.push_str("</ul></div>");
        self.pages.next = self.pages.next.wrapping_add(1);
        let page = self.pages.next;
        self.pages
            .held
            .insert(page, HeldPage::Home { workspace_id });
        Ok(OpenedPage {
            page,
            title: "Tilo".to_owned(),
            css: ui::composition::page::theme_css(&ui::Theme::default())
                + ui::composition::page::PAGE_CSS
                + HOME_CSS,
            body,
        })
    }

    /// The workspace's search page: a field and what the system's search
    /// answers for what it holds. The page keeps no index of its own — the
    /// catalogue lists the apps and the actions of the workspace's records,
    /// and the workspace's search finds what their text says — so every
    /// answer is one the caller may read, decided where every other search
    /// and listing is.
    fn open_search(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        needle: &str,
    ) -> Result<OpenedPage, PageRefusal> {
        let state = self
            .refresh(workspace_id, Needs::Workspace)
            .map_err(refused)?;
        state.workspace.as_ref().ok_or(PageRefusal::Elsewhere)?;
        let (rows, notes) = self.search_rows(workspace_id, caller, needle);
        let body = search_body(needle, &results_html(&rows, None, notes.as_deref()));
        self.pages.next = self.pages.next.wrapping_add(1);
        let page = self.pages.next;
        self.pages.held.insert(
            page,
            HeldPage::Search {
                workspace_id,
                caller: caller.clone(),
                needle: needle.to_owned(),
                rows,
                notes,
                pending: None,
                notice: None,
                asking: None,
                card: None,
            },
        );
        Ok(OpenedPage {
            page,
            title: "Search".to_owned(),
            css: ui::composition::page::theme_css(&ui::Theme::default())
                + ui::composition::page::PAGE_CSS
                + HOME_CSS
                + SEARCH_CSS,
            body,
        })
    }

    /// What the workspace's search answers `needle` with, as the page's rows:
    /// the apps it names, the records whose text holds it, and the actions
    /// their catalogue offers. The second answer says what could not be
    /// searched here, when something could not.
    fn search_rows(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        needle: &str,
    ) -> (Vec<SearchRow>, Option<String>) {
        let typed = needle.trim();
        let query = typed.to_lowercase();
        let mut rows: Vec<SearchRow> = Vec::new();
        // The apps stand first and always: with nothing typed the page is the
        // way in to all of them.
        let mut apps: Vec<(String, String)> = self
            .catalogue
            .iter()
            .filter(|app| app.keeps_workspace_resource())
            .filter(|app| {
                query.is_empty()
                    || app.title().to_lowercase().contains(&query)
                    || app.app_id().to_lowercase().contains(&query)
            })
            .map(|app| {
                (
                    app.title().to_owned(),
                    page_app_name(app.app_id()).to_owned(),
                )
            })
            .collect();
        apps.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (title, name) in apps {
            let id = SEARCH_FIRST_ROW.saturating_add(u32::try_from(rows.len()).unwrap_or_default());
            rows.push(SearchRow {
                id,
                label: title,
                secondary: String::new(),
                href: Some(format!("/w/{workspace_id}/{name}")),
                run: None,
                copy: None,
                app: true,
            });
        }
        if typed.is_empty() {
            return (rows, None);
        }
        if caller.acting().person_id().is_some()
            && query
                .split_whitespace()
                .all(|word| "new create workspace".contains(word))
            && let Some(locator) = self
                .shared
                .link
                .get()
                .and_then(|link| link.personal_workspace_host(workspace_id))
            && let Some(href) = new_workspace_entry(workspace_id, &locator)
        {
            rows.push(SearchRow {
                id: SEARCH_FIRST_ROW.saturating_add(u32::try_from(rows.len()).unwrap_or_default()),
                label: "New workspace".to_owned(),
                secondary: "Workspaces".to_owned(),
                href: Some(href),
                run: None,
                copy: None,
                app: false,
            });
        }
        let mut notes: Vec<&str> = Vec::new();
        // What the workspace holds, by the words in it: the records the
        // search finds. A record is a page the row opens.
        match self.search(workspace_id, caller, (typed, &Within::default())) {
            Ok(found) => {
                for found in found.into_iter().filter(|found| found.kind != FOUND_FILE) {
                    let Some(app) = found.app.as_deref() else {
                        continue;
                    };
                    let Some(app) = self.page_app(app) else {
                        continue;
                    };
                    if !app.keeps_workspace_resource() {
                        continue;
                    }
                    let href = format!(
                        "/w/{workspace_id}/{}/{sub}",
                        page_app_name(app.app_id()),
                        sub = encode_part(&found.resource),
                    );
                    if rows
                        .iter()
                        .any(|row| row.href.as_deref() == Some(href.as_str()))
                    {
                        continue;
                    }
                    let id = SEARCH_FIRST_ROW
                        .saturating_add(u32::try_from(rows.len()).unwrap_or_default());
                    rows.push(SearchRow {
                        id,
                        label: found.label,
                        secondary: app.title().to_owned(),
                        href: Some(href),
                        run: None,
                        copy: None,
                        app: false,
                    });
                }
            }
            Err(_) => notes.push("The workspace's records could not be searched here."),
        }
        if !self.action_rows(workspace_id, caller, typed, &mut rows) {
            notes.push("The workspace's actions could not be searched here.");
        }
        if rows.iter().all(|row| row.app) && notes.is_empty() {
            notes.push("Nothing here matches.");
        }
        (rows, (!notes.is_empty()).then(|| notes.join(" ")))
    }

    /// The actions the workspace's records offer for `typed`, as rows after
    /// `rows`: whether the catalogue could be asked.
    fn action_rows(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        typed: &str,
        rows: &mut Vec<SearchRow>,
    ) -> bool {
        // The actions the workspace's records offer, which the row runs: the
        // same invocation an agent or a script makes, decided for this person
        // by the same admission, and answered as the action alone answers.
        let asked = CatalogueQuery {
            needle: typed.to_owned(),
            max_results: 40,
            ..CatalogueQuery::default()
        };
        match self.catalogue_page(workspace_id, caller, &asked) {
            Ok(page) => {
                for row in page.rows {
                    let Some(action) = row.action else { continue };
                    let fields = RowField::of(&action.params).unwrap_or_default();
                    let id = SEARCH_FIRST_ROW
                        .saturating_add(u32::try_from(rows.len()).unwrap_or_default());
                    rows.push(SearchRow {
                        id,
                        label: action.action_label,
                        secondary: row.resource.label,
                        href: None,
                        run: Some(RowRun {
                            provider: row.provider,
                            resource: row.resource.id,
                            action: action.address.action,
                            revision: Some(page.revision.clone()),
                            fields,
                            host: false,
                        }),
                        copy: None,
                        app: false,
                    });
                }
            }
            Err(_) => return false,
        }
        // The workspace's own business, which the host runs: giving yourself a
        // name, inviting someone.
        let Some(link) = self.shared.link.get().cloned() else {
            return true;
        };
        for row in link.own_actions(workspace_id, caller, typed) {
            let Some(action) = row.action else { continue };
            let Some(fields) = RowField::of(&action.params) else {
                continue;
            };
            // A row whose press hands a link over carries one made ahead of
            // it: the press copies it, and the row takes the next from the
            // host's answer.
            let copy = (row.provider == PAGE_ACTIONS_PROVIDER
                && action.address.action == COPY_INVITE_LINK)
                .then(|| link.ready_invite_link(workspace_id, caller))
                .flatten();
            let id = SEARCH_FIRST_ROW.saturating_add(u32::try_from(rows.len()).unwrap_or_default());
            rows.push(SearchRow {
                id,
                label: action.action_label,
                secondary: row.resource.label,
                href: None,
                run: Some(RowRun {
                    provider: row.provider,
                    resource: action.address.resource,
                    action: action.address.action,
                    revision: None,
                    fields,
                    host: true,
                }),
                copy,
                app: false,
            });
        }
        true
    }

    /// Act on the search page `page`: typing narrows it, and a row runs its
    /// action. A row naming a page is a link the browser follows, which asks
    /// nothing here.
    fn act_on_search(
        &mut self,
        page: u64,
        (workspace_id, caller, _needle): (WorkspaceId, Caller, String),
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let Some((id, target, action)) = act else {
            // A pass over the page: an action it ran that has settled says so.
            return Ok(self.settled_search(page));
        };
        match action {
            PageAction::Value(text) if id >= SEARCH_FIRST_FIELD => {
                // What the person types into a field of the action they chose
                // is kept until they run it.
                if let Some(HeldPage::Search {
                    asking: Some(asking),
                    ..
                }) = self.pages.held.get_mut(&page)
                    && let Some(value) = id
                        .checked_sub(SEARCH_FIRST_FIELD)
                        .and_then(|field| usize::try_from(field).ok())
                        .and_then(|field| asking.values.get_mut(field))
                {
                    *value = text;
                }
                Ok(Vec::new())
            }
            PageAction::Activate if id == SEARCH_FORM_CANCEL => {
                if let Some(HeldPage::Search { asking, .. }) = self.pages.held.get_mut(&page) {
                    *asking = None;
                }
                self.patch_search(page)
            }
            PageAction::Activate if id == SEARCH_FORM_RUN => {
                self.run_asked(page, workspace_id, &caller)
            }
            PageAction::Activate if id == SEARCH_CARD_CLOSE || id >= SEARCH_CARD_FIRST => {
                self.act_on_card(page, workspace_id, &caller, id)
            }
            PageAction::Value(text) => {
                // Typing narrows the page; what it was showing (a row's
                // answer still running, say) is kept until the answer is.
                if let Some(HeldPage::Search { needle, .. }) = self.pages.held.get_mut(&page) {
                    needle.clone_from(&text);
                }
                self.search_again(page, workspace_id, &caller, &text)
            }
            PageAction::Activate => {
                self.run_search_row(page, workspace_id, &caller, target.as_deref())
            }
            PageAction::Toggle | PageAction::Focus | PageAction::Select { .. } => Ok(Vec::new()),
        }
    }

    /// What the field now says, answered with the rows it narrows to.
    fn search_again(
        &mut self,
        page: u64,
        workspace_id: WorkspaceId,
        caller: &Caller,
        needle: &str,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let state = self
            .refresh(workspace_id, Needs::Workspace)
            .map_err(refused)?;
        state.workspace.as_ref().ok_or(PageRefusal::Elsewhere)?;
        let (rows, notes) = self.search_rows(workspace_id, caller, needle);
        if let Some(HeldPage::Search {
            rows: held,
            notes: held_notes,
            notice,
            ..
        }) = self.pages.held.get_mut(&page)
        {
            // An action's fields the person opened stay open while they type
            // here: they hold the action itself, not the row it stood on.
            *held = rows;
            *held_notes = notes;
            *notice = None;
        }
        self.patch_search(page)
    }

    /// Run the offered action the clicked row names, as a caller of the
    /// workspace runs one: logged for the person, admitted where every
    /// invocation is, and answered with what the action answers.
    fn run_search_row(
        &mut self,
        page: u64,
        workspace_id: WorkspaceId,
        caller: &Caller,
        target: Option<&str>,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let Some(HeldPage::Search {
            rows,
            pending,
            asking,
            ..
        }) = self.pages.held.get_mut(&page)
        else {
            return Err(unavailable("the page was closed"));
        };
        if pending.is_some() {
            return self.patch_search(page);
        }
        let Some((label, run)) = rows.iter().find_map(|row| {
            let run = row.run.as_ref()?;
            (target == Some(run.page_target().as_str())).then(|| (row.label.clone(), run.clone()))
        }) else {
            // A later query removed the action that the person pressed.
            return self.patch_search(page);
        };
        // An action that needs something typed asks for it first, in fields
        // of its own under the row.
        if !run.fields.is_empty() {
            *asking = Some(Asking {
                label,
                values: vec![String::new(); run.fields.len()],
                run,
            });
            return self.patch_search(page);
        }
        self.run_row(
            page,
            workspace_id,
            caller,
            (label, run),
            tilo_actions::remote::InvokeArgument::None,
        )
    }

    /// Run the action the page asked the person's fields for, with what they
    /// typed. A field left empty is said, and nothing runs.
    fn run_asked(
        &mut self,
        page: u64,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let Some(HeldPage::Search {
            asking: Some(asking),
            notice,
            ..
        }) = self.pages.held.get_mut(&page)
        else {
            return self.patch_search(page);
        };
        let run = &asking.run;
        let mut arguments = serde_json::Map::new();
        for (field, typed) in run.fields.iter().zip(&asking.values) {
            let Some(value) = field.value(typed) else {
                *notice = Some(format!(
                    "{}: {}",
                    field_title(&field.name),
                    if field.number && !typed.trim().is_empty() {
                        "write a number."
                    } else {
                        "write something first."
                    }
                ));
                return self.patch_search(page);
            };
            arguments.insert(field.name.clone(), value);
        }
        let asked = (asking.label.clone(), asking.run.clone());
        self.run_row(
            page,
            workspace_id,
            caller,
            asked,
            tilo_actions::remote::InvokeArgument::Json(arguments),
        )
    }

    /// Run the action `label` names, as `run` reaches it, with `argument`.
    fn run_row(
        &mut self,
        page: u64,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (label, run): (String, RowRun),
        argument: tilo_actions::remote::InvokeArgument,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let host = run.host;
        let opens_a_card =
            run.runs_page_action(WAITING_TO_JOIN) || run.runs_page_action(DEVICES_AND_ACCESS);
        let hands_a_link_over = run.runs_page_action(COPY_INVITE_LINK);
        let named = run.clone();
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: run.provider,
                address: ActionAddress::new(run.resource, run.action),
            },
            argument,
            context: None,
            confirmation: None,
            revision: run.revision,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        if host {
            // The host answers its own actions as it runs them.
            let Some(link) = self.shared.link.get().cloned() else {
                return Err(unavailable("the host is not here"));
            };
            let answered = link.run_own_action(workspace_id, caller, &request);
            // One of the host's own actions may open a card on the page
            // rather than saying a line: the addresses waiting at the entry,
            // or the person's own devices.
            if opens_a_card {
                self.answered_card(page, &label, answered);
                return self.patch_search(page);
            }
            let notice = match answered {
                Ok(value) => {
                    if hands_a_link_over
                        && let Some(next) = value.get("link").and_then(serde_json::Value::as_str)
                        && let Some(HeldPage::Search { rows, .. }) = self.pages.held.get_mut(&page)
                    {
                        for row in rows.iter_mut().filter(|row| {
                            row.run
                                .as_ref()
                                .is_some_and(|row_run| row_run.same_action(&named))
                        }) {
                            row.copy = Some(next.to_owned());
                        }
                    }
                    said_of(
                        &label,
                        &ActionOutput {
                            value: Some(value),
                            ..ActionOutput::default()
                        },
                    )
                }
                Err(error) => format!("{label}: {error}"),
            };
            if let Some(HeldPage::Search {
                notice: held,
                asking,
                ..
            }) = self.pages.held.get_mut(&page)
            {
                *held = Some(notice);
                *asking = None;
            }
            return self.patch_search(page);
        }
        let invocation = request.invocation;
        let logged = self.log().insert(caller, request);
        let (notice, pending) = if logged {
            // The invocation runs here as any caller's does; one that waits
            // for a part of what it changes ends on a later pass
            // (`settled_search`).
            let _ = self.invoke(workspace_id, invocation);
            (
                format!("Running {label}…"),
                Some(PendingRun { invocation, label }),
            )
        } else {
            (
                "The workspace is running as many actions as it keeps track of: ask again in a moment."
                    .to_owned(),
                None,
            )
        };
        if let Some(HeldPage::Search {
            notice: held,
            pending: held_pending,
            asking,
            ..
        }) = self.pages.held.get_mut(&page)
        {
            *held = Some(notice);
            *held_pending = pending;
            *asking = None;
        }
        self.patch_search(page)
    }

    /// What the host answered one of its own actions that opens a card on the
    /// page: what it opened, under the rows; or why it could not.
    fn answered_card(
        &mut self,
        page: u64,
        label: &str,
        answered: Result<serde_json::Value, ActionCallError>,
    ) {
        let Some(HeldPage::Search {
            card,
            notice,
            asking,
            ..
        }) = self.pages.held.get_mut(&page)
        else {
            return;
        };
        *asking = None;
        let answered = answered.map(|value| {
            serde_json::from_value::<tilo_server_protocol::page::PageActionAnswer>(value)
        });
        match answered {
            Ok(Ok(tilo_server_protocol::page::PageActionAnswer::WaitList { mails, entries })) => {
                *notice = None;
                *card = Some(Card::Waiting {
                    mails,
                    addresses: entries,
                    sent: None,
                    refused: None,
                });
            }
            Ok(Ok(tilo_server_protocol::page::PageActionAnswer::Devices { setup })) => {
                *notice = None;
                *card = Some(Card::Devices { setup });
            }
            Ok(Ok(tilo_server_protocol::page::PageActionAnswer::Link { .. }) | Err(_)) => {
                *notice = Some(format!("{label}: the host answered unexpectedly."));
            }
            Err(error) => *notice = Some(format!("{label}: {error}")),
        }
    }

    /// Press `id` on the card under the rows: Close puts it away, and an
    /// address's Invite issues that address's invitation and has it mailed,
    /// where the entry sends mail.
    fn act_on_card(
        &mut self,
        page: u64,
        workspace_id: WorkspaceId,
        caller: &Caller,
        id: u32,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        if id == SEARCH_CARD_CLOSE {
            if let Some(HeldPage::Search { card, .. }) = self.pages.held.get_mut(&page) {
                *card = None;
            }
            return self.patch_search(page);
        }
        let Some(address) = id
            .checked_sub(SEARCH_CARD_FIRST)
            .and_then(|at| usize::try_from(at).ok())
            .and_then(|at| match self.pages.held.get(&page) {
                Some(HeldPage::Search {
                    card: Some(Card::Waiting { addresses, .. }),
                    ..
                }) => addresses.get(at).cloned(),
                _ => None,
            })
        else {
            return self.patch_search(page);
        };
        let Some(link) = self.shared.link.get().cloned() else {
            return Err(unavailable("the host is not here"));
        };
        // The same invocation an agent makes inviting the address: the
        // workspace issues the invitation, and the entry mails it to the
        // address where it sends mail.
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: tilo_actions::remote::workspace_governance_address(
                    &workspace_id.to_string(),
                    tilo_actions::policy::roles::governance::INVITE,
                ),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([
                ("role".to_owned(), serde_json::Value::from("editor")),
                (
                    "wait_list_entry".to_owned(),
                    serde_json::Value::from(address.entry.clone()),
                ),
            ])),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let answered = link.run_own_action(workspace_id, caller, &request);
        if let Some(HeldPage::Search {
            card:
                Some(Card::Waiting {
                    addresses,
                    sent: card_sent,
                    refused: card_refused,
                    ..
                }),
            ..
        }) = self.pages.held.get_mut(&page)
        {
            match answered {
                Ok(value) => {
                    let mailed = value
                        .get("sent")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    if mailed {
                        *card_sent = Some(format!(
                            "The invitation was emailed to {}.",
                            address.contact
                        ));
                        *card_refused = None;
                    } else {
                        let next = value
                            .get("next")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(
                                "The entry sends no mail: hand the invitation over yourself.",
                            );
                        *card_refused = Some(format!(
                            "The invitation for {} was not mailed: {next}",
                            address.contact
                        ));
                    }
                    for waiting in addresses.iter_mut() {
                        if waiting.entry == address.entry {
                            waiting.invited = true;
                        }
                    }
                }
                Err(error) => {
                    *card_refused = Some(format!(
                        "The invitation for {} was not made: {error}",
                        address.contact
                    ));
                }
            }
        }
        self.patch_search(page)
    }

    /// A pass over the search page: what the action it ran answered, once it
    /// has. Nothing while it runs.
    fn settled_search(&mut self, page: u64) -> Vec<PagePatch> {
        let Some(HeldPage::Search { pending, .. }) = self.pages.held.get(&page) else {
            return Vec::new();
        };
        let Some(run) = pending else {
            return Vec::new();
        };
        let invocation = run.invocation;
        let label = run.label.clone();
        let settled = self
            .log()
            .entries
            .get(&invocation)
            .map(|entry| entry.update.clone());
        let notice = match settled {
            Some(InvocationUpdate::Succeeded { output, .. }) => Some(said_of(&label, &output)),
            Some(InvocationUpdate::Failed { error, .. }) => {
                Some(format!("{label}: {}", error.message))
            }
            Some(InvocationUpdate::Cancelled { message, .. }) => Some(match message {
                Some(message) => format!("{label}: {message}"),
                None => format!("{label}: stopped."),
            }),
            Some(
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. },
            )
            | None => None,
        };
        let Some(notice) = notice else {
            return Vec::new();
        };
        if let Some(HeldPage::Search {
            notice: held,
            pending,
            ..
        }) = self.pages.held.get_mut(&page)
        {
            *held = Some(notice);
            *pending = None;
        }
        self.patch_search(page).unwrap_or_default()
    }

    /// The search page `page` as it stands, as the patch that shows it.
    fn patch_search(&self, page: u64) -> Result<Vec<PagePatch>, PageRefusal> {
        let Some(HeldPage::Search {
            needle,
            rows,
            notes,
            notice,
            asking,
            card,
            ..
        }) = self.pages.held.get(&page)
        else {
            return Err(unavailable("the page was closed"));
        };
        let mut html = results_html(rows, notice.as_deref(), notes.as_deref());
        if let Some(card) = card {
            html = card_html(card) + &html;
        }
        if let Some(asking) = asking {
            html = asking_html(&asking.label, &asking.run.fields, &asking.values) + &html;
        }
        let html = results_div(needle, &html);
        Ok(vec![PagePatch {
            id: SEARCH_RESULTS,
            html,
        }])
    }

    /// Act on `page` as its person did, or answer what changed on it: what the page shows now.
    /// `None` while its record or the chains it reads are still on their way: the act is held
    /// off the thread ([`Worker::await_part`]) and applied once they arrive.
    fn act_on_page(
        &mut self,
        page: u64,
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Option<Result<Vec<PagePatch>, PageRefusal>> {
        // What the page holds, taken by value so the workspace is read next.
        let (home, app, search) = match self.pages.held.get(&page) {
            Some(HeldPage::App {
                workspace_id,
                who,
                route,
                ..
            }) => (
                None,
                Some((*workspace_id, bounded::Bounded::clone(who), route.clone())),
                None,
            ),
            Some(HeldPage::Home { workspace_id }) => (Some(*workspace_id), None, None),
            Some(HeldPage::Search {
                workspace_id,
                caller,
                needle,
                ..
            }) => (
                None,
                None,
                Some((*workspace_id, caller.clone(), needle.clone())),
            ),
            None => {
                tracing::warn!(page, "a page's act found no page held here");
                return Some(Err(unavailable("the page was closed")));
            }
        };
        // What the person typed in the search field is the question; its
        // answer is what the page shows next.
        if let Some(search) = search {
            return Some(self.act_on_search(page, search, act));
        }
        // The workspace's front page holds no document and nothing acts on
        // it: a pass over it answers that nothing changed. It is read again
        // first, so a workspace no longer readable here is refused.
        if let Some(workspace_id) = home {
            return Some(
                self.refresh(workspace_id, Needs::Workspace)
                    .map_err(refused)
                    .map(|_| Vec::new()),
            );
        }
        let Some((workspace_id, who, route)) = app else {
            return Some(Err(unavailable("the page was closed")));
        };
        let acting = act.is_some();
        let started = std::time::Instant::now();
        // Others' writes since are merged before the person acts, as they
        // would have seen them in a window.
        // A replica holding neither its document's parts nor the parts of its
        // chains answers that it is not ready while they are on their way off
        // this thread. What a person did is held until they are here: they see
        // the one click they made, and nothing they typed is lost to a read
        // that was still arriving.
        let state = match self.refresh(workspace_id, Needs::Workspace) {
            Ok(state) => state,
            Err(error) if acting && not_ready(&error) => {
                return self.park_page_act(page, &error);
            }
            Err(error) => return Some(Err(refused(error))),
        };
        let refreshed = started.elapsed();
        let key = match self.bounded_replica(workspace_id, &who, &route, &state) {
            Ok(key) => key,
            Err(error) if acting && not_ready(&error) => {
                return self.park_page_act(page, &error);
            }
            Err(error) => return Some(Err(refused(error))),
        };
        let held_at = started.elapsed();
        let Some(held) = self.bounded.get(&key) else {
            return Some(Err(unavailable("the page's document did not open")));
        };
        let replica = &held.held.app;
        // Deliver completed calls before rendering. Drain fresh submissions
        // before answering a click, even if the browser leaves immediately.
        let mut poll_form = |activated| {
            if let Some(HeldPage::App {
                form_calls: Some(calls),
                ..
            }) = self.pages.held.get_mut(&page)
            {
                calls.poll(
                    replica,
                    workspace_id,
                    &who.caller,
                    self.shared.link.get().map(AsRef::as_ref),
                    activated,
                )?;
            }
            Ok(())
        };
        let activated = matches!(act, Some((_, _, PageAction::Activate)));
        let patches = match poll_form(false).and_then(|()| replica.page_act(act)) {
            Ok(patches) => patches,
            Err(error) => return Some(Err(not_shown(error))),
        };
        if acting && let Err(error) = poll_form(activated) {
            return Some(Err(not_shown(error)));
        }
        let acted = started.elapsed();
        if let Err(error) = self.save_page(workspace_id, &key, (&who, &route, &state)) {
            return Some(Err(error));
        }
        let saved_at = started.elapsed();
        tracing::debug!(
            page,
            acted = acting,
            patches = patches.len(),
            refresh_ms = refreshed.as_millis(),
            replica_ms = held_at.saturating_sub(refreshed).as_millis(),
            view_ms = acted.saturating_sub(held_at).as_millis(),
            save_ms = saved_at.saturating_sub(acted).as_millis(),
            "workspace actions: a page was acted on"
        );
        Some(Ok(patches
            .into_iter()
            .map(|(id, html)| PagePatch { id, html })
            .collect()))
    }

    /// The act found its record or the chains it reads still on their way: hold it off the
    /// thread until they arrive ([`Worker::await_part`]), or refuse it once the wait is over.
    fn park_page_act(
        &mut self,
        page: u64,
        error: &ActionCallError,
    ) -> Option<Result<Vec<PagePatch>, PageRefusal>> {
        if self.hold_for_part(None) {
            tracing::warn!(page, error = ?error, "a page's act found its record not ready");
            return None;
        }
        tracing::warn!(page, error = ?error, "a page's act was refused before it was applied");
        Some(Err(unavailable(
            "the page's record has not arrived from the workspace in time".to_owned(),
        )))
    }

    /// Save what the page's person wrote through it: checked against their
    /// access as every bounded caller's write is, and handed to the session.
    fn save_page(
        &mut self,
        workspace_id: WorkspaceId,
        key: &bounded::BoundedKey,
        (who, route, state): (&bounded::Bounded, &DocumentRoute, &super::WorkspaceState),
    ) -> Result<(), PageRefusal> {
        let Some(mut held) = self.bounded.remove(key) else {
            return Ok(());
        };
        let holder = key.0.clone();
        let taken = super::take_all(&held.held.app, &holder).map_err(not_shown);
        let saved = taken.and_then(|taken| {
            let wrote: Vec<DocumentRoute> = taken.iter().map(|(route, _)| route.clone()).collect();
            if taken.is_empty() {
                return Ok(wrote);
            }
            self.save_bounded(
                workspace_id,
                (who, route, state),
                (&held.held, taken),
                (None, false),
            )
            .map(|()| wrote)
            .map_err(|outcome| match outcome {
                crate::record_actions::Outcome::Rejected(error) => refused(error),
                crate::record_actions::Outcome::Failed(failure) => unavailable(failure.message),
            })
        });
        match saved {
            Ok(wrote) => {
                catch_up::caught_up_with_own(&holder, &mut held.held, &wrote);
                if !wrote.is_empty() {
                    self.behind_all_but(&super::Writer::Bounded(key.clone()), &wrote);
                }
                self.bounded.insert(key.clone(), held);
                Ok(())
            }
            // A write refused leaves the replica behind: the page's next ask
            // opens it again from what the workspace holds.
            Err(refusal) => Err(refusal),
        }
    }
}

/// The element number the search results stand at: what a field's keystroke
/// patches in place, under the field the person keeps typing in.
const SEARCH_RESULTS: u32 = 2;

/// The most result rows one search page shows: the field is for finding a
/// thing, not for reading the workspace whole.
const MOST_ROWS: usize = 40;

/// The element number the first result row stands at; the field and the
/// results stand before it.
const SEARCH_FIRST_ROW: u32 = 10;

/// The element numbers of the button that runs the action a row asks fields
/// for, and of the one that puts it away.
const SEARCH_FORM_RUN: u32 = 3;
const SEARCH_FORM_CANCEL: u32 = 4;

/// The element number of the card's Close button, and of its first address's
/// Invite button: past every row and the form's own.
const SEARCH_CARD_CLOSE: u32 = 5;
const SEARCH_CARD_FIRST: u32 = 1 << 21;

/// The element number of that action's first field: past every row.
const SEARCH_FIRST_FIELD: u32 = 1 << 20;

/// A parameter's name as a field's title: `display_name` reads "Display name".
fn field_title(name: &str) -> String {
    let words = name.replace(['_', '-'], " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The site's naming form. The link only re-enters this workspace on its host;
/// the admitted browser's device creates and enters the named workspace.
fn new_workspace_entry(
    workspace: WorkspaceId,
    locator: &tilo_actions::remote::ResourceViewLocator,
) -> Option<String> {
    use ui::tilo_reference::link::{HostOrigin, Link, LinkTarget, Mode, Scope, WebEntry};

    let entry = WebEntry::new(locator.web_entry.as_deref()?).ok()?;
    let link = Link {
        mode: Mode::Desktop,
        target: LinkTarget::new(Scope::Workspace {
            host: HostOrigin::new(&locator.origin).ok()?,
            id: workspace.as_uuid(),
            slug: None,
        }),
        secrets: Default::default(),
    };
    let address = link.to_url(&entry).ok()?;
    Some(address.replacen('#', "?new-workspace=1#", 1))
}

/// The fields the action `label` asks for, holding what was typed: a form
/// over the rows, its first field taking the caret.
fn asking_html(label: &str, fields: &[RowField], values: &[String]) -> String {
    let mut out = format!(
        "<form class=\"tilo-search-form\" aria-label=\"{label}\"><h2>{label}</h2>",
        label = escape(label)
    );
    for (index, (field, value)) in fields.iter().zip(values).enumerate() {
        let id = SEARCH_FIRST_FIELD.saturating_add(u32::try_from(index).unwrap_or_default());
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

/// An app's page name: the last part of its id (`notes` for `so.tilo.notes`).
fn page_app_name(app_id: &str) -> &str {
    app_id.rsplit_once('.').map_or(app_id, |(_, last)| last)
}

/// The rows of the search page as HTML, grouped: the apps, then what the
/// workspace holds and what its actions offer. `notice` says what the last
/// action answered, and `notes` what could not be searched here.
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
            if let Some(href) = &row.href {
                let _ = write!(
                    out,
                    "<li><a class=\"tilo-search-row\" href=\"{}\">\
                     <span class=\"tilo-search-row-label\">{}</span>{secondary}</a></li>",
                    escape(href),
                    escape(&row.label)
                );
            } else if let Some(run) = &row.run {
                // A row that hands a link over carries it on the element: the
                // press copies it inside the gesture, and the handover beside
                // it holds it for a browser that would not.
                let copy = row
                    .copy
                    .as_ref()
                    .map(|link| format!(" data-copy=\"{}\"", escape(link)))
                    .unwrap_or_default();
                let _ = write!(
                    out,
                    "<li><button class=\"tilo-search-row\" type=\"button\" data-a=\"activate\" \
                     data-p=\"{}\" data-t=\"{}\"{copy}><span class=\"tilo-search-row-label\">{}</span>{secondary}\
                     </button>",
                    row.id,
                    run.page_target(),
                    escape(&row.label)
                );
                if let Some(link) = &row.copy {
                    let _ = write!(
                        out,
                        "<p class=\"tilo-search-handover\" hidden>Copy this link: \
                         <code>{}</code></p>",
                        escape(link)
                    );
                }
                out.push_str("</li>");
            }
        }
        out.push_str("</ul></section>");
    }
    out
}

/// The results element, holding `contents`: the one element a search page
/// patches.
fn results_div(needle: &str, contents: &str) -> String {
    format!(
        "<div class=\"tilo-search-results\" data-p=\"{SEARCH_RESULTS}\" data-query=\"{}\">{contents}</div>",
        escape(needle)
    )
}

/// The card one of the host's own actions opened, as the page shows it: the
/// addresses waiting at the workspace's entry, each one an Invite button, and
/// what the page says once one went out; or the person's own devices, and the
/// press that sets one more up.
fn card_html(card: &Card) -> String {
    match card {
        Card::Waiting {
            mails,
            addresses,
            sent,
            refused,
        } => {
            let mut out = String::from(
                "<section class=\"tilo-waiting\" aria-label=\"Waiting to join\">\
                 <h2>Waiting to join</h2>",
            );
            if addresses.is_empty() {
                out.push_str("<p class=\"tilo-waiting-none\">No one is waiting.</p>");
            } else {
                out.push_str("<ul>");
                for (at, address) in addresses.iter().enumerate() {
                    let id =
                        SEARCH_CARD_FIRST.saturating_add(u32::try_from(at).unwrap_or_default());
                    let _ = write!(
                        out,
                        "<li><span class=\"tilo-waiting-contact\">{contact}</span>",
                        contact = escape(&address.contact)
                    );
                    if address.invited {
                        let _ = write!(
                            out,
                            "<span class=\"tilo-waiting-invited\">Invited</span>\
                             <button type=\"button\" data-a=\"activate\" data-p=\"{id}\">\
                             Invite again</button>"
                        );
                    } else {
                        let _ = write!(
                            out,
                            "<button type=\"button\" data-a=\"activate\" data-p=\"{id}\">\
                             Invite</button>"
                        );
                    }
                    out.push_str("</li>");
                }
                out.push_str("</ul>");
            }
            if !mails {
                out.push_str(
                    "<p class=\"tilo-waiting-note\">This entry sends no mail: an invitation \
                     made here is handed over by you.</p>",
                );
            }
            let _ = write!(
                out,
                "<p class=\"tilo-waiting-sent\" role=\"status\"{hidden}>{said}</p>",
                hidden = if sent.is_some() { "" } else { " hidden" },
                said = escape(sent.as_deref().unwrap_or_default()),
            );
            let _ = write!(
                out,
                "<p class=\"tilo-entry-error\" role=\"alert\"{hidden}>{said}</p>",
                hidden = if refused.is_some() { "" } else { " hidden" },
                said = escape(refused.as_deref().unwrap_or_default()),
            );
            close_button(&mut out);
            out
        }
        Card::Devices { setup } => {
            let mut out = String::from(
                "<section class=\"tilo-devices\" aria-label=\"Devices and access\">\
                 <h2>Devices and access</h2>\
                 <p>This workspace is on every device you sign in on, and your agent reaches \
                 it from the computer you set up.</p>\
                 <button type=\"button\" data-pair-device>Pair a new device</button>",
            );
            let _ = write!(
                out,
                "<button class=\"tilo-devices-connect\" type=\"button\" data-setup=\"{setup}\">\
                 Connect an agent or computer</button>",
                setup = escape(setup),
            );
            close_button(&mut out);
            out
        }
    }
}

/// The Close button every card ends with: what puts it away.
fn close_button(out: &mut String) {
    let _ = write!(
        out,
        "<div class=\"tilo-card-actions\">\
         <button type=\"button\" data-a=\"activate\" data-p=\"{SEARCH_CARD_CLOSE}\">Close</button>\
         </div></section>"
    );
}

/// What the action `label` answered, as a person reads it: the value it
/// answered with, when it names something readable, or that it is done.
fn said_of(label: &str, output: &ActionOutput) -> String {
    let said = match &output.value {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(serde_json::Value::Object(fields)) => ["link", "url", "text", "message"]
            .iter()
            .find_map(|key| fields.get(*key).and_then(serde_json::Value::as_str))
            .map(str::to_owned),
        _ => None,
    };
    let said = said.unwrap_or_else(|| "Done.".to_owned());
    let said: String = said.chars().take(600).collect();
    format!("{label}: {said}")
}

/// The search page `needle` reached with and the results it answers with: a
/// field where the person keeps typing, over what the workspace's search
/// found.
fn search_body(needle: &str, results: &str) -> String {
    format!(
        "<div class=\"tilo-view tilo-search\" data-p=\"0\" data-focus=\"1\">\
         <div class=\"tilo-search-bar\">\
         <input class=\"tilo-search-field\" type=\"search\" data-a=\"value\" data-t=\"search\" \
         data-p=\"1\" value=\"{needle}\" placeholder=\"Search apps, actions and records\" \
         aria-label=\"Search\"></div>{results}</div>",
        needle = escape(needle),
        results = results_div(needle, results),
    )
}

/// The workspace's front page: its apps as tiles, on the theme's own ground.
/// It is the page's own stylesheet, after the app pages' (`PAGE_CSS`).
const HOME_CSS: &str = "\
.tilo-home{height:100%;overflow-y:auto;display:flex;flex-direction:column;gap:20px;\
padding:clamp(24px,6vh,56px) clamp(20px,6vw,64px);background:var(--tilo-ground);}\
.tilo-home-title{margin:0;font-size:18px;font-weight:600;}\
.tilo-home-search{display:block;box-sizing:border-box;max-width:880px;padding:12px 16px;\
border-radius:12px;background:var(--tilo-raised);color:var(--tilo-text-dim);\
text-decoration:none;font-size:16px;}\
.tilo-home-search:hover{color:var(--tilo-text);}\
.tilo-home-search:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-home-apps{display:grid;grid-template-columns:repeat(auto-fill,minmax(118px,1fr));\
gap:8px;margin:0;padding:0;list-style:none;max-width:880px;}\
.tilo-home-app{display:flex;flex-direction:column;align-items:center;gap:10px;padding:18px 10px;\
border-radius:14px;color:var(--tilo-text);text-decoration:none;font-size:13px;font-weight:500;}\
.tilo-home-app:hover{background:var(--tilo-hover);}\
.tilo-home-app:active{background:var(--tilo-press);}\
.tilo-home-app:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-home-app svg{width:52px;height:52px;border-radius:12px;}\
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

/// An app's first letter, on the tile an icon stands on.
/// How many are here of what `present` shows: a person's own device and
/// their page say they are here twice, and each person, agent or device
/// counts once.
fn here(present: &[collab_types::presence::Shown]) -> usize {
    let mut identities: Vec<String> = Vec::new();
    for shown in present {
        let identity = match (&shown.member, &shown.user) {
            (Some(member), _) => format!("member:{member}"),
            (None, Some(user)) => format!("user:{user}"),
            (None, None) => format!("device:{}", shown.publisher),
        };
        if !identities.contains(&identity) {
            identities.push(identity);
        }
    }
    identities.len()
}

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

/// The search page's own ground: the field on the front page's ground, over
/// the rows it finds.
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
.tilo-search-form-actions button[type=submit]{background:var(--tilo-accent);color:var(--tilo-on-accent);}\
.tilo-search-handover{margin:4px 0 8px;font-size:13px;color:var(--tilo-text-dim);}\
.tilo-search-handover code{font-size:13px;user-select:all;color:var(--tilo-text);}\
.tilo-waiting{display:flex;flex-direction:column;gap:10px;padding:16px;border-radius:12px;\
background:var(--tilo-raised);}\
.tilo-waiting h2{margin:0;font-size:15px;font-weight:600;}\
.tilo-waiting ul{display:flex;flex-direction:column;gap:6px;margin:0;padding:0;list-style:none;}\
.tilo-waiting li{display:flex;align-items:center;gap:10px;font-size:14px;}\
.tilo-waiting-contact{font-weight:500;}\
.tilo-waiting-invited{margin-left:auto;font-size:12px;color:var(--tilo-text-tertiary);}\
.tilo-waiting li button{margin-left:auto;padding:6px 12px;border:0;border-radius:8px;font:inherit;\
font-size:13px;background:var(--tilo-accent);color:var(--tilo-on-accent);cursor:pointer;}\
.tilo-waiting li button:hover{filter:brightness(1.08);}\
.tilo-waiting li button:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:2px;}\
.tilo-waiting p{margin:0;font-size:13px;color:var(--tilo-text-dim);}\
.tilo-waiting-sent,.tilo-entry-error{color:var(--tilo-text);}\
.tilo-card-actions{display:flex;justify-content:flex-end;}\
.tilo-card-actions button{padding:7px 14px;border:0;border-radius:8px;font:inherit;\
font-size:14px;background:var(--tilo-hover);color:var(--tilo-text);cursor:pointer;}\
.tilo-devices{display:flex;flex-direction:column;gap:10px;padding:16px;border-radius:12px;\
background:var(--tilo-raised);}\
.tilo-devices h2{margin:0;font-size:15px;font-weight:600;}\
.tilo-devices p{margin:0;font-size:13px;color:var(--tilo-text-dim);}\
.tilo-devices-connect{align-self:flex-start;padding:8px 14px;border:0;border-radius:8px;\
font:inherit;font-size:14px;font-weight:500;background:var(--tilo-accent);\
color:var(--tilo-on-accent);cursor:pointer;}\
.tilo-devices-connect:hover{filter:brightness(1.08);}\
.tilo-devices-connect:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:2px;}";
/// `text` as HTML shows it.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}
