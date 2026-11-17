//! The workspace settings in a page: the session's own sheet, on its People and Agents tabs, over
//! the page. Who is in the workspace and what each may do is what the workspace's host answers, and
//! a role changed or a person removed is asked of it; the tab is built from the members as the
//! session's shell builds it from its own ([`tilo_session::workspace_people`]). The workspace's
//! agents are Chat's record of them, read and changed in the records the page's windows keep, as
//! the workspace lets this page's person ([`tilo_session::workspace_settings::PageAgents`]).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_desktop::here::Here;
use tilo_desktop::workspace_settings::people::{Member as Row, content_of};
use tilo_desktop_model::UserId;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::i18n::t;
use tilo_relay_protocol::workspaces::{
    Member, Role, Workspace, WorkspacesReply, WorkspacesRequest,
};
use tilo_session::workspace_people;
use tilo_session::workspace_settings::{AgentsHost, PageAgents};
use tilo_sheets::SheetState as _;
use tilo_sheets::workspace_settings::{self as sheet, WorkspaceSettingsEffect};

use ui::permissions::{LivePermissions, SharedPermissions};
use ui::resource::workspace::WorkspaceResources;

use crate::host_asks::{HostAsks, Reply, refused};
use crate::over_page::{OverSheet, PageSheet};

/// Why nothing was asked: a page with no workspace host has nobody to list.
const NO_HOST: &str = "this page is in no workspace";

/// Where the page reads the workspace's agents: the records of the workspace it shows, as its
/// windows open them, what the workspace lets its person do, and whether it lets them administer
/// it, as the workspace's host last answered.
#[derive(Clone)]
pub(crate) struct PageRecords {
    pub records: WorkspaceResources,
    pub permissions: SharedPermissions,
    pub administers: Arc<AtomicBool>,
    /// The shell the page shows, and its person.
    pub handle: DesktopHandle,
    pub person: UserId,
}

impl PageRecords {
    /// The records of `workspace` that `owners` keep beside the page's windows' (`shared`), opened
    /// as its person `author`, as `permissions` let them, over the shell `handle` shows.
    pub(crate) fn in_page(
        (owners, shared, permissions): (
            &ui::resource::ResourceOwners,
            &crate::desktop::Shared,
            &SharedPermissions,
        ),
        handle: &DesktopHandle,
        workspace: &str,
        author: String,
    ) -> Self {
        Self {
            records: crate::workspace_resources::in_page(
                owners,
                &shared.stores,
                workspace,
                author.clone(),
                true,
            ),
            permissions: permissions.clone(),
            administers: Arc::clone(&shared.administers),
            handle: handle.clone(),
            person: author,
        }
    }
}

impl AgentsHost for PageRecords {
    /// The page shows one workspace: its records are the ones it keeps.
    fn open(
        &self,
        _workspace: &tilo_desktop_model::Workspace,
        _person: &UserId,
    ) -> (WorkspaceResources, Option<LivePermissions>) {
        (
            self.records.clone(),
            Some(LivePermissions::shared(self.permissions.clone())),
        )
    }

    fn manages(&self, _workspace: &tilo_desktop_model::Workspace, _person: &UserId) -> bool {
        self.administers.load(Ordering::Relaxed)
    }

    /// The workspace's grants are its host's: a page asks it for changes.
    fn writes_grants(&self) -> bool {
        false
    }
}

/// The page's workspace settings, as the sheets' code makes it (`crate::over_page::SHEETS`).
pub(crate) fn make(
    shared: crate::desktop::Shared,
    here: Here,
    peer: Option<u32>,
    records: Option<PageRecords>,
) -> Box<dyn PageSheet> {
    Box::new(PageWorkspaceSettings::new(shared, here, peer, records))
}

struct PageWorkspaceSettings {
    state: sheet::WorkspaceSettingsState,
    peer: Option<u32>,
    shared: crate::desktop::Shared,
    /// Who is here, as the workspace's presence names them.
    here: Here,
    asks: HostAsks,
    /// The workspace as its host last answered, with its members.
    workspace: Rc<RefCell<Option<Workspace>>>,
    /// The sheet waits for the workspace's first answer before it goes up, rather than show
    /// nobody in it.
    opening: Rc<Cell<bool>>,
    /// Whether the host was asked who is in the workspace since the sheet was last down.
    asked: bool,
    /// The members the page followed when the host was last asked: asked again when they move.
    asked_for: Option<BTreeSet<String>>,
    /// What the built sheet shows, and the revision of what it keeps to itself.
    built: Option<(sheet::Settings, u64)>,
    /// The workspace's agents, on a page in a workspace.
    agents: Option<PageAgents>,
    /// The tab the person is on.
    tab: Cell<sheet::Tab>,
}

impl PageWorkspaceSettings {
    fn new(
        shared: crate::desktop::Shared,
        here: Here,
        peer: Option<u32>,
        records: Option<PageRecords>,
    ) -> Self {
        let agents = records.map(|records| {
            let woken = Arc::clone(&shared.woken);
            PageAgents::new(
                records.handle.clone(),
                records.person.clone(),
                Rc::new(records) as Rc<dyn AgentsHost>,
                Arc::new(move || {
                    woken.store(true, Ordering::Release);
                    crate::browser::wake_desktops();
                }),
            )
        });
        Self {
            agents,
            tab: Cell::new(sheet::Tab::People),
            state: sheet::WorkspaceSettingsState::default(),
            asks: HostAsks::new(peer, NO_HOST, Arc::clone(&shared.woken)),
            peer,
            shared,
            here,
            workspace: Rc::default(),
            opening: Rc::new(Cell::new(false)),
            asked: false,
            asked_for: None,
            built: None,
        }
    }

    /// The workspace this page is in, by its uuid.
    fn uuid(&self) -> Option<String> {
        self.peer.and_then(crate::replicated::workspace_uuid)
    }

    /// Ask the host who is in the workspace now.
    fn list(&mut self) {
        self.asked = true;
        self.asked_for.clone_from(&self.shared.members.borrow());
        let Some(uuid) = self.uuid() else {
            return self.answered(Err(NO_HOST.to_owned()));
        };
        let then = self.taking();
        self.asks.ask(
            |_| WorkspacesRequest::List,
            move |reply| {
                then(match reply {
                    Ok(WorkspacesReply::Workspaces(list)) => list
                        .into_iter()
                        .find(|held| held.workspace == uuid)
                        .map(WorkspacesReply::Workspace)
                        .ok_or_else(|| "the host does not govern this workspace".to_owned()),
                    other => other,
                });
            },
        );
    }

    /// Ask the host `request` about one member: its answer is the workspace after the change.
    fn change(&self, request: impl FnOnce(String, [u8; 32]) -> WorkspacesRequest + 'static) {
        let Some(uuid) = self.uuid() else {
            return self.answered(Err(NO_HOST.to_owned()));
        };
        let mut request_id = [0u8; 32];
        if let Err(error) = getrandom::fill(&mut request_id) {
            return self.answered(Err(error.to_string()));
        }
        self.asks
            .ask(move |_| request(uuid, request_id), self.taking());
    }

    /// Who takes a host's answer: the workspace it answers with, or why it refused.
    fn taking(&self) -> impl FnOnce(Reply) + 'static {
        let (workspace, state, opening) = (
            Rc::clone(&self.workspace),
            self.state.clone(),
            Rc::clone(&self.opening),
        );
        move |reply| take(&workspace, &state, &opening, reply)
    }

    fn answered(&self, reply: Reply) {
        take(&self.workspace, &self.state, &self.opening, reply);
    }

    /// Carry out what the person pressed on the sheet.
    fn follow_asked(&self) -> bool {
        let asked = self.state.take_asked();
        let changed = !asked.is_empty();
        for effect in asked {
            if self
                .agents
                .as_ref()
                .is_some_and(|agents| agents.ask(&self.state, &effect))
            {
                continue;
            }
            match effect {
                WorkspaceSettingsEffect::ShowTab { tab } => {
                    if let Some(tab) = sheet::Tab::from_key(&tab) {
                        self.tab.set(tab);
                    }
                }
                WorkspaceSettingsEffect::SetRole { member, role } => {
                    let (Some(user_public_key), Some(role)) = (
                        person(&member),
                        Role::parse(&role).filter(|r| r.assignable()),
                    ) else {
                        continue;
                    };
                    self.change(move |workspace, request_id| WorkspacesRequest::SetRole {
                        workspace,
                        request_id,
                        user_public_key,
                        role,
                    });
                }
                WorkspaceSettingsEffect::RemoveMember { member } => {
                    let Some(user_public_key) = person(&member) else {
                        continue;
                    };
                    self.change(move |workspace, request_id| WorkspacesRequest::Remove {
                        workspace,
                        request_id,
                        user_public_key,
                    });
                }
                WorkspaceSettingsEffect::DismissNotice => self.state.dismiss_notice(),
                // The sheet closes itself; a page shows the People and Agents tabs alone, and
                // nothing else the sheet could ask is drawn on it.
                _ => {}
            }
        }
        changed
    }

    /// The sheet as the workspace stands for this page's person.
    fn settings(&self) -> sheet::Settings {
        let workspace = self.workspace.borrow();
        let members: Vec<&Member> = workspace
            .iter()
            .flat_map(|workspace| &workspace.members)
            // Software holds no role, and whoever was removed is no longer in it.
            .filter(|member| member.role.is_some() && !member.removed)
            .collect();
        let own = members.iter().find(|member| member.is_self);
        let viewer = own.map(|member| member.id.key()).unwrap_or_default();
        let role = own.and_then(|member| member.role);
        let here = self.here.borrow();
        let mut rows: Vec<Row> = members
            .iter()
            .filter_map(|member| {
                let role = member.role?;
                let user = member.id.key();
                Some(Row {
                    name: Some(member.name.trim())
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .or_else(|| here.person(&user).map(|(name, _)| name))
                        .unwrap_or_else(tilo_sheets::invite::unnamed_person),
                    owns: role == Role::Owner,
                    administrator: member.administrator,
                    role,
                    grants: usize::try_from(member.grants).unwrap_or(usize::MAX),
                    user,
                })
            })
            .collect();
        // The owner first, then everyone else by key, as the session's shell lists them.
        rows.sort_by(|a, b| b.owns.cmp(&a.owns).then_with(|| a.user.cmp(&b.user)));
        let mut people =
            workspace_people::tab(content_of(&rows, &viewer, role == Some(Role::Owner)));
        // What the workspace did is the session's to show; a page offers only the decision.
        people.remove_decision.review.clear();
        let mut tabs = vec![sheet::Tab::People];
        if self.agents.is_some() {
            tabs.push(sheet::Tab::Agents);
        }
        let tab = Some(self.tab.get())
            .filter(|tab| tabs.contains(tab))
            .unwrap_or(sheet::Tab::People);
        sheet::Settings {
            title: t("workspace-settings-title"),
            name: tilo_desktop_surface::names::workspace(
                workspace.as_ref().map_or("", |workspace| &workspace.name),
            ),
            role: workspace_people::role_line(role),
            close: t("workspace-settings-close"),
            tabs: tabs.into_iter().map(workspace_people::tab_label).collect(),
            tab,
            people,
            agents: self
                .agents
                .as_ref()
                .filter(|_| tab == sheet::Tab::Agents)
                .map(PageAgents::pane)
                .unwrap_or_default(),
            notice: self.state.notice(),
            ..sheet::Settings::default()
        }
    }
}

/// Take the host's answer: the workspace it answers with, or why it refused, over the sheet.
fn take(
    workspace: &RefCell<Option<Workspace>>,
    state: &sheet::WorkspaceSettingsState,
    opening: &Cell<bool>,
    reply: Reply,
) {
    match reply {
        Ok(WorkspacesReply::Workspace(answered)) => {
            *workspace.borrow_mut() = Some(answered);
        }
        other => state.refuse(workspace_people::refusal(&refused(other))),
    }
    if opening.replace(false) {
        state.presentation().open(false);
    }
}

/// The person a row names, by their root key.
fn person(key: &str) -> Option<[u8; 32]> {
    hex::decode(key).ok()?.try_into().ok()
}

impl PageSheet for PageWorkspaceSettings {
    fn open(&self) {
        self.state.dismiss_notice();
        self.state.reset();
        if self.state.presentation().is_open() {
            return;
        }
        self.opening.set(true);
        // `list` needs `&mut self` for what it last asked about; the next pass asks.
        self.shared.wake();
    }

    fn is_open(&self) -> bool {
        self.state.presentation().is_open()
    }
}

impl OverSheet for PageWorkspaceSettings {
    fn follow_sheet(
        &mut self,
        show: &mut dyn FnMut(Option<ui::widget::Element>),
        missing: bool,
    ) -> bool {
        let mut changed = self.asks.take_replies();
        let up = self.is_open() || self.opening.get();
        // Opening, or the workspace's people moved while the sheet is up: ask who is in it now.
        if up && (!self.asked || self.asked_for != *self.shared.members.borrow()) {
            self.list();
        }
        changed |= self.follow_asked();
        if !self.is_open() {
            if !self.opening.get() {
                self.asked = false;
            }
            if self.built.take().is_some() {
                changed = true;
            }
            show(None);
            return changed;
        }
        self.state.show(self.settings());
        let built = (self.state.settings(), self.state.drawn_revision());
        if missing || self.built.as_ref() != Some(&built) {
            self.built = Some(built);
            show(Some(self.state.sheet()));
            changed = true;
        }
        changed
    }

    fn drawn(&self) -> bool {
        self.state.presentation().is_drawn()
    }

    fn sheet_name(&self) -> &'static str {
        "PageWorkspaceSettings"
    }
}
