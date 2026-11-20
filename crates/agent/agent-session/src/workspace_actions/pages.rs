//! A workspace's apps as web pages for its members: each page is the app's
//! view of the workspace's document, held for the person in a replica of
//! their own within bounds ([`bounded`]), in an app worker that also renders
//! it ([`ConfinedReplica::open_page`]). What the person does on the page is
//! acted on that view as a click or typing in a window is, and what it wrote
//! is checked against their access and saved as every bounded caller's write
//! is.

use std::fmt::Write as _;

use super::{
    ActionCallError, Caller, DocumentRoute, HashMap, Job, Needs, Worker, WorkspaceActions,
    WorkspaceId, bounded, catch_up, pending,
};
use tilo_actions::remote::{Completer, Pending};
use tilo_server_protocol::page::{OpenedPage, PageAction, PageAddress, PagePatch, PageRefusal};

/// What a page asks of the service's thread.
pub(super) enum PageJob {
    Open {
        workspace_id: WorkspaceId,
        caller: Box<Caller>,
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
    },
    /// The workspace's front page: the apps a member opens here. Nothing is
    /// acted on it and it never changes.
    Home { workspace_id: WorkspaceId },
}

/// The pages the service holds, by number.
#[derive(Default)]
pub(super) struct Pages {
    held: HashMap<u64, HeldPage>,
    next: u64,
}

impl Pages {
    /// Who the page `job` is about was opened by: what an act on it, which names the page alone,
    /// is rendered for.
    pub(super) fn caller_of<'a>(&'a self, job: &'a PageJob) -> Option<&'a Caller> {
        match job {
            PageJob::Open { caller, .. } => Some(&**caller),
            PageJob::Act { page, .. } | PageJob::Close { page, .. } => match self.held.get(page) {
                Some(HeldPage::App { who, .. }) => Some(&who.caller),
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
        let (done, answer) = pending();
        let job = PageJob::Open {
            workspace_id,
            caller: Box::new(caller.clone()),
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
                address,
                done,
            } => done.complete(self.open_page(workspace_id, &caller, &address)),
            PageJob::Act {
                page, act, done, ..
            } => done.complete(self.act_on_page(page, act)),
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
            PageJob::Open { address, .. } => self
                .page_app(&address.app)
                .map_or(super::keys::Keys::Joined, |app| document(app.app_id(), "")),
            PageJob::Act { page, .. } => match self.pages.held.get(page) {
                Some(HeldPage::App { route, .. }) => document(&route.app_id, &route.document),
                Some(HeldPage::Home { .. }) | None => super::keys::Keys::Joined,
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
        address: &PageAddress,
    ) -> Result<OpenedPage, PageRefusal> {
        if !self.admits_member(workspace_id, caller) {
            return Err(PageRefusal::NotMember);
        }
        if address.is_home() {
            return self.open_home(workspace_id);
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
                crate::member::actions::Outcome::Rejected(error) => refused(error),
                crate::member::actions::Outcome::Failed(failure) => unavailable(failure.message),
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
        let shown = self
            .bounded
            .get(&key)
            .ok_or_else(|| unavailable("the page's document did not open"))?
            .held
            .app
            .page_show(address.show.as_deref())
            .map_err(not_shown)?;
        self.save_page(workspace_id, &key, (&who, &route, &state))?;
        self.pages.held.insert(
            page,
            HeldPage::App {
                workspace_id,
                who: Box::new(who),
                route,
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

    /// The workspace's front page: the apps a member opens here, each a link
    /// to its own page. It shows nothing the workspace holds, so it is the
    /// same for everyone in it, and nothing is acted on it.
    fn open_home(&mut self, workspace_id: WorkspaceId) -> Result<OpenedPage, PageRefusal> {
        // The workspace has to be readable here, as an app's page does.
        let state = self
            .refresh(workspace_id, Needs::Workspace)
            .map_err(refused)?;
        state.workspace.as_ref().ok_or(PageRefusal::Elsewhere)?;
        let mut apps: Vec<_> = self
            .catalogue
            .iter()
            .filter(|app| app.keeps_workspace_resource())
            .collect();
        apps.sort_by_key(|app| app.title().to_owned());
        let mut body = String::from(
            "<div class=\"tilo-view tilo-home\" data-p=\"0\">\
             <h1 class=\"tilo-home-title\">Tilo</h1><ul class=\"tilo-home-apps\">",
        );
        for app in apps {
            let name = app
                .app_id()
                .rsplit_once('.')
                .map_or_else(|| app.app_id(), |(_, last)| last);
            let icon = match &app.icon {
                ui::IconDescriptor::Named(icon) => {
                    ui::icons::Icon::from_name(icon).map_or_else(String::new, ui::icons::icon_svg)
                }
                ui::IconDescriptor::Glyph(_) => String::new(),
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

    fn act_on_page(
        &mut self,
        page: u64,
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        // What the page holds, taken by value so the workspace is read next.
        let (home, app) = match self
            .pages
            .held
            .get(&page)
            .ok_or_else(|| unavailable("the page was closed"))?
        {
            HeldPage::App {
                workspace_id,
                who,
                route,
            } => (
                None,
                Some((*workspace_id, bounded::Bounded::clone(who), route.clone())),
            ),
            HeldPage::Home { workspace_id } => (Some(*workspace_id), None),
        };
        // The workspace's front page holds no document and nothing acts on
        // it: a pass over it answers that nothing changed. It is read again
        // first, so a workspace no longer readable here is refused.
        if let Some(workspace_id) = home {
            self.refresh(workspace_id, Needs::Workspace)
                .map_err(refused)?;
            return Ok(Vec::new());
        }
        let (workspace_id, who, route) = app.ok_or_else(|| unavailable("the page was closed"))?;
        let started = std::time::Instant::now();
        // Others' writes since are merged before the person acts, as they
        // would have seen them in a window.
        let state = self
            .refresh(workspace_id, Needs::Workspace)
            .map_err(refused)?;
        let refreshed = started.elapsed();
        let key = self
            .bounded_replica(workspace_id, &who, &route, &state)
            .map_err(refused)?;
        let held_at = started.elapsed();
        let patches = self
            .bounded
            .get(&key)
            .ok_or_else(|| unavailable("the page's document did not open"))?
            .held
            .app
            .page_act(act)
            .map_err(not_shown)?;
        let acted = started.elapsed();
        self.save_page(workspace_id, &key, (&who, &route, &state))?;
        tracing::debug!(
            page,
            refresh_ms = refreshed.as_millis(),
            replica_ms = held_at.saturating_sub(refreshed).as_millis(),
            view_ms = acted.saturating_sub(held_at).as_millis(),
            save_ms = started.elapsed().saturating_sub(acted).as_millis(),
            "workspace actions: a page was acted on"
        );
        Ok(patches
            .into_iter()
            .map(|(id, html)| PagePatch { id, html })
            .collect())
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
                crate::member::actions::Outcome::Rejected(error) => refused(error),
                crate::member::actions::Outcome::Failed(failure) => unavailable(failure.message),
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

/// The workspace's front page: its apps as tiles, on the theme's own ground.
/// It is the page's own stylesheet, after the app pages' (`PAGE_CSS`).
const HOME_CSS: &str = "\
.tilo-home{height:100%;overflow-y:auto;display:flex;flex-direction:column;gap:20px;\
padding:clamp(24px,6vh,56px) clamp(20px,6vw,64px);background:var(--tilo-ground);}\
.tilo-home-title{margin:0;font-size:18px;font-weight:600;}\
.tilo-home-apps{display:grid;grid-template-columns:repeat(auto-fill,minmax(118px,1fr));\
gap:8px;margin:0;padding:0;list-style:none;max-width:880px;}\
.tilo-home-app{display:flex;flex-direction:column;align-items:center;gap:10px;padding:18px 10px;\
border-radius:14px;color:var(--tilo-text);text-decoration:none;font-size:13px;font-weight:500;}\
.tilo-home-app:hover{background:var(--tilo-hover);}\
.tilo-home-app:active{background:var(--tilo-press);}\
.tilo-home-app:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-home-app svg{width:52px;height:52px;border-radius:12px;}";

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
